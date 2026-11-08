//! DeepSeek spending, inferred from balance readings.
//!
//! DeepSeek's API reports only the current balance (no usage or billing
//! records), so spending is the sum of drops between consecutive readings;
//! a rise is taken as a top-up and counts as nothing. Money spent while the
//! app was closed still shows up as the drop to the next reading. An interval
//! that straddles a boundary (e.g. local midnight) is split pro rata by time.
//!
//! Days left = balance / daily rate, where the daily rate blends the last
//! 3 days (70%) with the last 14 days (30%), counted in calendar time, idle
//! days included. While fewer than 24 hours have been recorded, the money
//! spent so far counts as one day's worth (spent / max(span, 1 day)), so a
//! first estimate appears early, errs on the long side, and converges as
//! readings accumulate instead of jumping at the 24-hour mark.
//!
//! Readings persist in %LOCALAPPDATA%\AI-Usage-Lite\deepseek-history.json:
//! timestamps and amounts only, never the API key, kept for 30 days. Balances
//! in this app's own diagnose reports (same folder) are merged in as extra
//! readings, so history from before the first run is not lost.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

const DAY: u64 = 86_400;
const KEEP_DAYS: u64 = 30;
/// Readings older than this are thinned to one per hour.
const FINE_DAYS: u64 = 3;
const RECENT_DAYS: u64 = 3;
const LONG_DAYS: u64 = 14;
const RECENT_WEIGHT: f64 = 0.7;
/// A projection needs at least an hour of readings.
const MIN_COVER: u64 = 3600;
/// Beyond this the projection is shown as "3 个月以上".
pub const MAX_DAYS: f64 = 90.0;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct History {
    #[serde(default)]
    pub currency: String,
    /// (unix seconds, total balance), oldest first.
    #[serde(default)]
    pub readings: Vec<(u64, f64)>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spend {
    /// Spent since local midnight.
    pub today: f64,
    /// Days until the balance runs out; None without a day of readings or
    /// when nothing is being spent.
    pub days_left: Option<f64>,
}

pub fn unix(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Start of the local day containing `t`, in unix seconds.
pub fn local_midnight(t: SystemTime) -> u64 {
    let l = crate::ui::fmt::local(t);
    let s = unix(t);
    s - s % 60 - u64::from(l.hour) * 3600 - u64::from(l.minute) * 60
}

fn same(a: f64, b: f64) -> bool {
    (a - b).abs() < 0.005
}

impl History {
    pub fn record(&mut self, now: u64, currency: &str, total: f64) {
        if self.currency != currency {
            self.currency = currency.to_string();
            self.readings.clear();
        }
        // The clock went back: readings "from the future" can't be ordered.
        self.readings.retain(|&(t, _)| t <= now);
        let n = self.readings.len();
        // A flat run only needs its two ends.
        if n >= 2 && same(self.readings[n - 1].1, total) && same(self.readings[n - 2].1, total) {
            self.readings[n - 1].0 = now;
        } else {
            self.readings.push((now, total));
        }
        self.compact(now);
    }

    fn compact(&mut self, now: u64) {
        let cutoff = now.saturating_sub(KEEP_DAYS * DAY);
        let fine = now.saturating_sub(FINE_DAYS * DAY);
        let old = std::mem::take(&mut self.readings);
        let len = old.len();
        for (k, &(t, v)) in old.iter().enumerate() {
            // Keep the newest reading before the cutoff as the window's anchor.
            if k + 1 < len && old[k + 1].0 < cutoff {
                continue;
            }
            // Older than a few days: keep the last reading of each hour.
            if t < fine && k + 1 < len && old[k + 1].0 < fine && old[k + 1].0 / 3600 == t / 3600 && k > 0 {
                continue;
            }
            self.readings.push((t, v));
        }
    }

    /// Money spent within [a, b] (unix seconds).
    pub fn spent_between(&self, a: u64, b: u64) -> f64 {
        self.readings
            .windows(2)
            .map(|w| {
                let ((t0, v0), (t1, v1)) = (w[0], w[1]);
                let drop = v0 - v1;
                let (lo, hi) = (t0.max(a), t1.min(b));
                if drop <= 0.0 || t1 <= t0 || hi <= lo {
                    0.0
                } else {
                    drop * (hi - lo) as f64 / (t1 - t0) as f64
                }
            })
            .sum()
    }

    /// Average spend per day over the last `days` (or since the first
    /// reading); under a day of readings counts as one day.
    fn daily_avg(&self, now: u64, days: u64) -> Option<f64> {
        let first = self.readings.first()?.0;
        let start = now.saturating_sub(days * DAY).max(first);
        let span = now.checked_sub(start).filter(|&s| s > 0)?;
        Some(self.spent_between(start, now) * DAY as f64 / span.max(DAY) as f64)
    }

    /// Adds readings taken elsewhere (diagnose reports), keeping time order.
    pub fn merge(&mut self, now: u64, extra: &[(u64, String, f64)]) {
        let mut added = false;
        for (t, cur, v) in extra {
            if *t > now || now - t > KEEP_DAYS * DAY {
                continue;
            }
            if self.currency.is_empty() {
                self.currency = cur.clone();
            }
            if *cur != self.currency || self.readings.iter().any(|&(rt, _)| rt == *t) {
                continue;
            }
            self.readings.push((*t, *v));
            added = true;
        }
        if added {
            self.readings.sort_by_key(|&(t, _)| t);
            self.compact(now);
        }
    }

    /// Call right after `record(now, ..)` so the last reading is current.
    pub fn spend(&self, now: u64, midnight: u64, total: f64) -> Spend {
        let today = self.spent_between(midnight, now);
        let cover = self.readings.first().map_or(0, |&(t, _)| now.saturating_sub(t));
        let days_left = if cover < MIN_COVER {
            None
        } else {
            match (self.daily_avg(now, RECENT_DAYS), self.daily_avg(now, LONG_DAYS)) {
                (Some(r), Some(l)) => {
                    let rate = RECENT_WEIGHT * r + (1.0 - RECENT_WEIGHT) * l;
                    (rate > 1e-6).then(|| total.max(0.0) / rate)
                }
                _ => None,
            }
        };
        Spend { today, days_left }
    }
}

pub fn path() -> Option<PathBuf> {
    crate::paths::report_dir().map(|d| d.join("deepseek-history.json"))
}

/// Stored readings plus any balances found in diagnose reports.
pub fn load() -> History {
    let mut h: History = path()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    h.merge(unix(SystemTime::now()), &diagnose_readings());
    h
}

/// Balances in this app's diagnose reports (`diagnose-<unix time>.txt`).
fn diagnose_readings() -> Vec<(u64, String, f64)> {
    let Some(dir) = crate::paths::report_dir() else { return Vec::new() };
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let t: u64 = name.strip_prefix("diagnose-")?.strip_suffix(".txt")?.parse().ok()?;
            let text = std::fs::read_to_string(e.path()).ok()?;
            let (cur, v) = parse_report_balance(&text)?;
            Some((t, cur, v))
        })
        .collect()
}

/// The "    余额 CNY 75.97（充值 …" line of a diagnose report.
fn parse_report_balance(text: &str) -> Option<(String, f64)> {
    let rest = text.lines().find_map(|l| l.trim_start().strip_prefix("余额 "))?;
    let mut parts = rest.splitn(2, ' ');
    let cur = parts.next()?.to_string();
    let num: String = parts.next()?.chars().take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-').collect();
    Some((cur, num.parse().ok()?))
}

/// Written to a temp file and renamed, so a crash never leaves half a file.
pub fn save(h: &History) {
    let Some(p) = path() else { return };
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = p.with_extension("json.tmp");
    if let Ok(bytes) = serde_json::to_vec(h) {
        if std::fs::write(&tmp, bytes).is_ok() {
            let _ = std::fs::rename(&tmp, &p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: u64 = 1_800_000_000;
    const H: u64 = 3600;

    #[test]
    fn drops_count_and_top_ups_do_not() {
        let mut h = History::default();
        h.record(T0, "CNY", 50.0);
        h.record(T0 + H, "CNY", 49.0);
        h.record(T0 + 2 * H, "CNY", 99.0); // top-up
        h.record(T0 + 3 * H, "CNY", 98.5);
        assert!((h.spent_between(0, u64::MAX) - 1.5).abs() < 1e-9);
    }

    #[test]
    fn gap_across_midnight_is_split_by_time() {
        let mut h = History::default();
        let midnight = T0 + 10 * H;
        h.record(midnight - 2 * H, "CNY", 10.0);
        h.record(midnight + 2 * H, "CNY", 8.0);
        let s = h.spend(midnight + 2 * H, midnight, 8.0);
        assert!((s.today - 1.0).abs() < 1e-9, "{}", s.today);
        // Under a day of readings: ¥2 so far counts as ¥2/day → 4 days.
        assert!((s.days_left.unwrap() - 4.0).abs() < 1e-9);
    }

    #[test]
    fn needs_an_hour_of_readings() {
        let mut h = History::default();
        h.record(T0, "CNY", 10.0);
        h.record(T0 + 1800, "CNY", 9.0);
        assert_eq!(h.spend(T0 + 1800, T0, 9.0).days_left, None);
    }

    #[test]
    fn diagnose_reports_merge_in_order() {
        let report = "==== DeepSeek (300 ms) ====\n  DeepSeek: 余额 CNY 75.97（可用：true）\n  结果：成功\n    余额 CNY 75.59（充值 75.59，赠金 0.00，可用：true）\n";
        assert_eq!(parse_report_balance(report), Some(("CNY".into(), 75.59)));
        let mut h = History::default();
        h.record(T0 + 2 * H, "CNY", 70.0);
        h.merge(T0 + 2 * H, &[(T0, "CNY".into(), 75.0), (T0 + H, "USD".into(), 1.0), (T0 + 2 * H, "CNY".into(), 70.0)]);
        assert_eq!(h.readings, vec![(T0, 75.0), (T0 + 2 * H, 70.0)]);
    }

    #[test]
    fn days_left_blends_recent_and_long_rates() {
        let mut h = History::default();
        // 14 days: ¥1/day for the first 11 days, ¥3/day for the last 3.
        let mut bal = 100.0;
        for d in 0..=14u64 {
            h.record(T0 + d * DAY, "CNY", bal);
            bal -= if d >= 11 { 3.0 } else { 1.0 };
        }
        let now = T0 + 14 * DAY;
        let total = h.readings.last().unwrap().1;
        let s = h.spend(now, now, total);
        // recent 3/day, 14-day avg (11+9)/14; rate = 0.7*3 + 0.3*20/14
        let rate = 0.7 * 3.0 + 0.3 * 20.0 / 14.0;
        assert!((s.days_left.unwrap() - total / rate).abs() < 1e-6);
    }

    #[test]
    fn idle_days_lengthen_the_projection_and_nothing_spent_gives_none() {
        let mut h = History::default();
        h.record(T0, "CNY", 20.0);
        h.record(T0 + H, "CNY", 18.0);
        h.record(T0 + 3 * DAY, "CNY", 18.0);
        let s = h.spend(T0 + 3 * DAY, T0 + 3 * DAY - H, 18.0);
        // ¥2 over 3 days → both windows ≈ 0.667/day → 27 days.
        assert!((s.days_left.unwrap() - 27.0).abs() < 1e-6);
        assert_eq!(s.today, 0.0);
        let mut flat = History::default();
        flat.record(T0, "CNY", 5.0);
        flat.record(T0 + 2 * DAY, "CNY", 5.0);
        assert_eq!(flat.spend(T0 + 2 * DAY, T0, 5.0).days_left, None);
    }

    #[test]
    fn flat_runs_collapse_and_old_readings_thin_out() {
        let mut h = History::default();
        for k in 0..10 {
            h.record(T0 + k * 600, "CNY", 7.0);
        }
        assert_eq!(h.readings.len(), 2);
        assert_eq!(h.readings[1].0, T0 + 9 * 600);
        // Spending every 10 minutes for 40 days stays bounded.
        let mut h = History::default();
        for k in 0..(40 * 144u64) {
            h.record(T0 + k * 600, "CNY", 10_000.0 - k as f64 * 0.01);
        }
        assert!(h.readings.len() < 30 * 24 + 3 * 144 + 10, "{}", h.readings.len());
        let first = h.readings[0].0;
        let now = h.readings.last().unwrap().0;
        assert!(now - first <= (KEEP_DAYS + 1) * DAY);
        // Thinning keeps the total spent intact.
        let spent = h.spent_between(first, now);
        let expect = h.readings[0].1 - h.readings.last().unwrap().1;
        assert!((spent - expect).abs() < 1e-6);
    }

    #[test]
    fn currency_change_starts_over() {
        let mut h = History::default();
        h.record(T0, "CNY", 5.0);
        h.record(T0 + H, "USD", 3.0);
        assert_eq!(h.readings, vec![(T0 + H, 3.0)]);
    }
}
