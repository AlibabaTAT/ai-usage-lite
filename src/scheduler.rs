//! One worker thread per service. Workers sleep on a channel, so an idle app
//! uses no CPU between polls, and a slow service never blocks the others.

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime};

use crate::config::Config;
use crate::model::{Failure, ProviderId, ProviderState};
use crate::providers::{self, Mode};

pub type States = Arc<Mutex<[ProviderState; crate::model::PROVIDER_COUNT]>>;
pub type SharedConfig = Arc<RwLock<Config>>;

pub enum Cmd {
    /// Poll now (manual refresh, resume from sleep).
    Refresh,
    /// Settings changed; re-evaluate the schedule without forcing a request.
    Reconfigure,
}

pub struct Scheduler {
    senders: Vec<Sender<Cmd>>,
}

/// Manual refreshes closer together than this are coalesced.
const MIN_GAP: Duration = Duration::from_secs(8);

fn interval_of(cfg: &Config) -> Duration {
    Duration::from_secs(u64::from(cfg.refresh_minutes.max(1)) * 60)
}

/// How long to wait after a result before polling again.
fn next_wait(result: &Result<(), Failure>, interval: Duration, failures: u32) -> Duration {
    match result {
        Ok(()) => interval,
        Err(Failure::RateLimited(after)) => {
            Duration::from_secs(after.unwrap_or(0).max(interval.as_secs() * 2).clamp(60, 3600))
        }
        Err(e) if e.is_transient() => {
            let backoff = 60u64 << failures.clamp(1, 5).saturating_sub(1);
            Duration::from_secs(backoff).min(interval)
        }
        Err(_) => interval,
    }
}

impl Scheduler {
    pub fn start(mode: Mode, states: States, config: SharedConfig, notify: Arc<dyn Fn() + Send + Sync>) -> Scheduler {
        let mut senders = Vec::new();
        for id in ProviderId::ALL {
            let (tx, rx) = channel::<Cmd>();
            senders.push(tx);
            let states = states.clone();
            let config = config.clone();
            let notify = notify.clone();
            std::thread::Builder::new()
                .name(format!("poll-{}", id.name()))
                .stack_size(512 * 1024)
                .spawn(move || worker(id, mode, rx, states, config, notify))
                .expect("spawn poll thread");
        }
        Scheduler { senders }
    }

    pub fn refresh_all(&self) {
        for s in &self.senders {
            let _ = s.send(Cmd::Refresh);
        }
    }

    pub fn reconfigure(&self) {
        for s in &self.senders {
            let _ = s.send(Cmd::Reconfigure);
        }
    }
}

fn worker(
    id: ProviderId,
    mode: Mode,
    rx: std::sync::mpsc::Receiver<Cmd>,
    states: States,
    config: SharedConfig,
    notify: Arc<dyn Fn() + Send + Sync>,
) {
    let i = id.index();
    let mut failures: u32 = 0;
    let mut was_enabled = false;
    let mut next_due = Instant::now();
    let mut last_attempt: Option<Instant> = None;
    let mut last_ok = true;
    let mut cooldown_until: Option<Instant> = None;
    // In-memory readings per window, for the recent-rate forecast.
    let mut history: HashMap<String, crate::forecast::History> = HashMap::new();
    // Balance readings (DeepSeek), loaded from disk on first use.
    let mut spend_history: Option<crate::spend::History> = None;
    loop {
        let cfg = config.read().map(|c| c.clone()).unwrap_or_default();
        let enabled = cfg.enabled.get(i).copied().unwrap_or(false);
        if enabled && !was_enabled {
            next_due = Instant::now();
        }
        was_enabled = enabled;

        if enabled && Instant::now() >= next_due {
            last_attempt = Some(Instant::now());
            if let Ok(mut s) = states.lock() {
                s[i].in_flight = true;
            }
            notify();
            let mut result = std::panic::catch_unwind(AssertUnwindSafe(|| providers::poll(id, mode, &cfg)))
                .unwrap_or_else(|_| Err(Failure::BadResponse));
            let done = SystemTime::now();
            if let Ok(snap) = result.as_mut() {
                for pool in &mut snap.pools {
                    for w in &mut pool.windows {
                        let key = format!("{}|{}", pool.name, w.label);
                        let h = history.entry(key).or_default();
                        w.recent_rate = crate::forecast::record(h, done, w.used, crate::forecast::period_of(&w.label));
                    }
                }
                // DeepSeek: infer spending from the stored balance readings.
                // Demo modes never touch the file.
                if mode == Mode::Live {
                    if let Some(b) = snap.balance.as_mut() {
                        let h = spend_history.get_or_insert_with(crate::spend::load);
                        let now = crate::spend::unix(done);
                        h.record(now, &b.currency, b.total);
                        b.spend = Some(h.spend(now, crate::spend::local_midnight(done), b.total));
                        crate::spend::save(h);
                    }
                    crate::usagelog::record(&cfg, id, snap, done);
                }
            }
            let outcome = result.as_ref().map(|_| ()).map_err(Clone::clone);
            failures = if outcome.is_ok() { 0 } else { failures + 1 };
            last_ok = outcome.is_ok();
            let wait = next_wait(&outcome, interval_of(&cfg), failures);
            next_due = Instant::now() + wait;
            cooldown_until = matches!(outcome, Err(Failure::RateLimited(_))).then_some(next_due);
            if let Ok(mut s) = states.lock() {
                let st = &mut s[i];
                st.in_flight = false;
                st.cooldown_until = cooldown_until.map(|_| done + wait);
                match result {
                    Ok(snap) => st.last_ok = Some((done, snap)),
                    Err(e) => st.last_error = Some((done, e)),
                }
            }
            notify();
        }

        let wait = if enabled { next_due.saturating_duration_since(Instant::now()) } else { Duration::from_secs(3600) };
        match rx.recv_timeout(wait) {
            Ok(Cmd::Refresh) if enabled => {
                let earliest = last_attempt.map(|t| t + MIN_GAP).unwrap_or_else(Instant::now);
                let earliest = cooldown_until.map_or(earliest, |c| c.max(earliest));
                next_due = next_due.min(earliest.max(Instant::now()));
            }
            Ok(Cmd::Reconfigure) if enabled && last_ok => {
                // Keep the cadence anchored to the last successful poll.
                let cfg = config.read().map(|c| c.clone()).unwrap_or_default();
                if let Some(t) = last_attempt {
                    next_due = t + interval_of(&cfg);
                }
            }
            Ok(_) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}
