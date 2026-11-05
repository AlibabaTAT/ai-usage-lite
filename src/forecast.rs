//! Usage forecast for the current window.
//!
//! The services report only "percent used" and "resets at". Two rates are
//! combined:
//!   * window average: the window started `period` before its reset, so the
//!     average so far is `used / elapsed` (stable, but slow to react);
//!   * recent rate: from the readings this app took over the last part of
//!     the window — the last hour of a 5-hour window, the last day of a week
//!     (reacts to the current pace, but noisy because usage is reported in
//!     whole percents).
//! With enough recent readings the forecast uses 70% recent + 30% average,
//! otherwise the average alone. Readings live in memory only.

use std::collections::VecDeque;
use std::time::{Duration, SystemTime};

use crate::model::UsageWindow;

#[derive(Clone, Debug, PartialEq)]
pub struct Forecast {
    /// Average consumption so far, percent per hour.
    pub rate_per_hour: f64,
    /// Time until the window resets.
    pub reset_in: Duration,
    /// Time until it runs out at this rate, only if that happens before the reset.
    pub exhaust_in: Option<Duration>,
    /// Projected percent used at the reset (capped at 100).
    pub used_at_reset: f64,
    /// Window length, for choosing per-hour vs per-day wording.
    pub period: Duration,
}

/// Window length implied by the label the providers assign.
pub fn period_of(label: &str) -> Option<Duration> {
    let hours = match label {
        "5 小时" => 5,
        "今日" => 24,
        "30 天" => 24 * 30,
        l if l.starts_with("本周") => 24 * 7,
        _ => return None,
    };
    Some(Duration::from_secs(hours * 3600))
}

pub fn forecast(w: &UsageWindow, now: SystemTime) -> Option<Forecast> {
    let period = period_of(&w.label)?;
    let reset = w.resets_at?;
    let reset_in = reset.duration_since(now).ok()?;
    if reset_in > period || w.exhausted() || w.used < 1.0 {
        return None;
    }
    let elapsed = period - reset_in;
    // Too early in the window for an average to mean anything:
    // at least 1/20 of the window and 10 minutes.
    let min_elapsed = (period / 20).max(Duration::from_secs(600));
    if elapsed < min_elapsed {
        return None;
    }
    let average = w.used / elapsed.as_secs_f64();
    let rate_per_sec = match w.recent_rate {
        Some(recent) => RECENT_WEIGHT * recent + (1.0 - RECENT_WEIGHT) * average,
        None => average,
    };
    if rate_per_sec <= 0.0 {
        return Some(Forecast { rate_per_hour: 0.0, reset_in, exhaust_in: None, used_at_reset: w.used, period });
    }
    let left = (100.0 - w.used).max(0.0);
    let to_empty = Duration::from_secs_f64(left / rate_per_sec);
    let used_at_reset = (w.used + rate_per_sec * reset_in.as_secs_f64()).min(100.0);
    Some(Forecast {
        rate_per_hour: rate_per_sec * 3600.0,
        reset_in,
        exhaust_in: (to_empty < reset_in).then_some(to_empty),
        used_at_reset,
        period,
    })
}

const RECENT_WEIGHT: f64 = 0.7;

/// How far back the recent rate looks: a fifth of the window, at most a day
/// (5-hour window → 1 hour; week → 24 hours).
pub fn lookback(period: Duration) -> Duration {
    (period / 5).min(Duration::from_secs(24 * 3600))
}

/// Readings of one window: (time, percent used).
pub type History = VecDeque<(SystemTime, f64)>;

/// Records a reading and returns the recent rate (percent per second), if
/// the readings span at least a third of the look-back period.
pub fn record(history: &mut History, now: SystemTime, used: f64, period: Option<Duration>) -> Option<f64> {
    // A drop means the window reset; old readings no longer apply.
    if history.back().is_some_and(|&(_, last)| used + 0.5 < last) {
        history.clear();
    }
    history.push_back((now, used));
    let look = lookback(period?);
    while history.len() > 2 && history.front().is_some_and(|&(t, _)| now.duration_since(t).unwrap_or_default() > look) {
        history.pop_front();
    }
    let &(t0, u0) = history.front()?;
    let span = now.duration_since(t0).ok()?;
    if span < look / 3 {
        return None;
    }
    Some(((used - u0).max(0.0)) / span.as_secs_f64())
}

impl Forecast {
    /// "约 12%/小时" or, for long windows, "约 9%/天".
    pub fn rate_text(&self) -> String {
        if self.period > Duration::from_secs(24 * 3600) {
            let per_day = self.rate_per_hour * 24.0;
            if per_day < 1.0 { "约 <1%/天".into() } else { format!("约 {:.0}%/天", per_day) }
        } else if self.rate_per_hour < 1.0 {
            "约 <1%/小时".into()
        } else {
            format!("约 {:.0}%/小时", self.rate_per_hour)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win(label: &str, used: f64, reset_in_h: f64, now: SystemTime) -> UsageWindow {
        UsageWindow { label: label.into(), used, resets_at: Some(now + Duration::from_secs_f64(reset_in_h * 3600.0)), recent_rate: None }
    }

    #[test]
    fn five_hour_window_running_out_early() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        // 2 h into the window (3 h left), 60% used → 30%/h → empty in 1h20m.
        let f = forecast(&win("5 小时", 60.0, 3.0, now), now).unwrap();
        assert!((f.rate_per_hour - 30.0).abs() < 1e-6);
        let e = f.exhaust_in.unwrap().as_secs();
        assert!((4790..=4810).contains(&e), "{e}");
        assert_eq!(f.used_at_reset, 100.0);
    }

    #[test]
    fn weekly_window_that_lasts() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        // 3.5 days in, 20% used → about 5.7%/day → ~40% at reset.
        let f = forecast(&win("本周", 20.0, 84.0, now), now).unwrap();
        assert!(f.exhaust_in.is_none());
        assert!((f.used_at_reset - 40.0).abs() < 0.01);
        assert_eq!(f.rate_text(), "约 6%/天");
    }

    #[test]
    fn recent_pace_outweighs_early_burst() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        // 60% used in the first 2 h (avg 30%/h → would run out), but nothing
        // used in the last hour: blended rate is 9%/h → lasts to the reset.
        let mut w = win("5 小时", 60.0, 3.0, now);
        w.recent_rate = Some(0.0);
        let f = forecast(&w, now).unwrap();
        assert!((f.rate_per_hour - 9.0).abs() < 1e-6);
        assert!(f.exhaust_in.is_none());
    }

    #[test]
    fn recent_rate_needs_a_third_of_the_lookback_and_resets_clear_history() {
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let p = period_of("5 小时");
        let mut h = History::new();
        assert_eq!(record(&mut h, t0, 10.0, p), None);
        assert_eq!(record(&mut h, t0 + Duration::from_secs(600), 12.0, p), None, "10 min < 20 min");
        let r = record(&mut h, t0 + Duration::from_secs(1800), 16.0, p).unwrap();
        assert!((r * 3600.0 - 12.0).abs() < 1e-6, "6% in 30 min = 12%/h");
        // Window reset: usage drops, history starts over.
        assert_eq!(record(&mut h, t0 + Duration::from_secs(2400), 1.0, p), None);
        assert_eq!(h.len(), 1);
    }

    #[test]
    fn no_forecast_when_unreliable() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        assert!(forecast(&win("5 小时", 5.0, 4.95, now), now).is_none(), "3 minutes in");
        assert!(forecast(&win("5 小时", 0.0, 2.0, now), now).is_none(), "nothing used");
        assert!(forecast(&win("5 小时", 100.0, 2.0, now), now).is_none(), "already out");
        assert!(forecast(&win("当前", 50.0, 2.0, now), now).is_none(), "unknown length");
    }
}
