//! Usage forecast from the average burn rate of the current window.
//!
//! The services report only "percent used" and "resets at". The window
//! started `period` before its reset, so the average rate so far is
//! `used / elapsed`. Projecting it forward tells whether the allowance runs
//! out before the reset, and when. No history or extra requests are needed.

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
    let rate_per_sec = w.used / elapsed.as_secs_f64();
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
        UsageWindow { label: label.into(), used, resets_at: Some(now + Duration::from_secs_f64(reset_in_h * 3600.0)) }
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
    fn no_forecast_when_unreliable() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        assert!(forecast(&win("5 小时", 5.0, 4.95, now), now).is_none(), "3 minutes in");
        assert!(forecast(&win("5 小时", 0.0, 2.0, now), now).is_none(), "nothing used");
        assert!(forecast(&win("5 小时", 100.0, 2.0, now), now).is_none(), "already out");
        assert!(forecast(&win("当前", 50.0, 2.0, now), now).is_none(), "unknown length");
    }
}
