//! Chinese time formatting for countdowns and clock times.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub struct Local {
    pub month: u16,
    pub day: u16,
    pub hour: u16,
    pub minute: u16,
    /// 0 = Sunday.
    pub weekday: u16,
    /// Days since 1970 in local time, for "today / tomorrow" comparisons.
    pub day_index: i64,
}

#[cfg(windows)]
pub fn local(t: SystemTime) -> Local {
    use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
    let since_1601 = t.duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos() as u64 / 100 + 116_444_736_000_000_000;
    let ft = FILETIME { dwLowDateTime: since_1601 as u32, dwHighDateTime: (since_1601 >> 32) as u32 };
    let mut utc = SYSTEMTIME::default();
    let mut loc = SYSTEMTIME::default();
    unsafe {
        let _ = FileTimeToSystemTime(&ft, &mut utc);
        if SystemTimeToTzSpecificLocalTime(None, &utc, &mut loc).is_err() {
            loc = utc;
        }
    }
    let day_index = days_from_civil(loc.wYear as i64, loc.wMonth as i64, loc.wDay as i64);
    Local { month: loc.wMonth, day: loc.wDay, hour: loc.wHour, minute: loc.wMinute, weekday: loc.wDayOfWeek, day_index }
}

/// Non-Windows builds (unit tests only): fixed UTC+8 so results are deterministic.
#[cfg(not(windows))]
pub fn local(t: SystemTime) -> Local {
    let secs = t.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64 + 8 * 3600;
    let day_index = secs.div_euclid(86_400);
    let (_, m, d) = civil_from_days(day_index);
    let rem = secs.rem_euclid(86_400);
    Local {
        month: m as u16,
        day: d as u16,
        hour: (rem / 3600) as u16,
        minute: ((rem % 3600) / 60) as u16,
        weekday: ((day_index + 4).rem_euclid(7)) as u16,
        day_index,
    }
}

/// Local time with offset, e.g. "2026-11-08T09:05:30+08:00" (for the usage log).
pub fn iso(t: SystemTime) -> String {
    let l = local(t);
    let unix = t.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
    let sec = unix.rem_euclid(60);
    let local_secs = l.day_index * 86_400 + i64::from(l.hour) * 3600 + i64::from(l.minute) * 60 + sec;
    let off_min = (local_secs - unix) / 60;
    let (y, _, _) = civil_from_days(l.day_index);
    let sign = if off_min < 0 { '-' } else { '+' };
    format!(
        "{y:04}-{:02}-{:02}T{:02}:{:02}:{sec:02}{sign}{:02}:{:02}",
        l.month,
        l.day,
        l.hour,
        l.minute,
        off_min.abs() / 60,
        off_min.abs() % 60
    )
}

/// "2026-11" for the month containing `t`, local time.
pub fn year_month(t: SystemTime) -> String {
    let l = local(t);
    let (y, m, _) = civil_from_days(l.day_index);
    format!("{y:04}-{m:02}")
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[allow(dead_code)]
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

const WEEKDAYS: [&str; 7] = ["周日", "周一", "周二", "周三", "周四", "周五", "周六"];

pub fn clock(t: SystemTime) -> String {
    let l = local(t);
    format!("{:02}:{:02}", l.hour, l.minute)
}

/// "2小时13分" / "45分钟" / "1天5小时".
pub fn span(d: Duration) -> String {
    let mins = (d.as_secs() + 59) / 60;
    if mins < 60 {
        format!("{}分钟", mins.max(1))
    } else if mins < 24 * 60 {
        let (h, m) = (mins / 60, mins % 60);
        if m == 0 { format!("{h}小时") } else { format!("{h}小时{m}分") }
    } else {
        let hours = mins / 60;
        let (d, h) = (hours / 24, hours % 24);
        if h == 0 { format!("{d}天") } else { format!("{d}天{h}小时") }
    }
}

/// When a window resets, phrased for the reset column.
/// < 24 h: countdown ("3小时33分后"); later: weekday + clock ("周六 08:00").
pub fn reset(at: Option<SystemTime>, now: SystemTime) -> String {
    let Some(at) = at else { return String::new() };
    let Ok(left) = at.duration_since(now) else { return "即将恢复".into() };
    if left < Duration::from_secs(60) {
        return "即将恢复".into();
    }
    if left < Duration::from_secs(24 * 3600) {
        return format!("{}后", span(left));
    }
    let l = local(at);
    let n = local(now);
    if l.day_index - n.day_index < 7 {
        format!("{} {:02}:{:02}", WEEKDAYS[l.weekday as usize % 7], l.hour, l.minute)
    } else {
        format!("{}月{}日", l.month, l.day)
    }
}

/// "刚刚" / "13:20".
pub fn updated(at: SystemTime, now: SystemTime) -> String {
    match now.duration_since(at) {
        Ok(d) if d < Duration::from_secs(60) => "刚刚".into(),
        _ => clock(at),
    }
}

pub fn percent(v: f64) -> String {
    if v > 0.0 && v < 1.0 {
        "<1%".into()
    } else if v < 100.0 && v > 99.0 {
        ">99%".into()
    } else {
        format!("{:.0}%", v.clamp(0.0, 100.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn spans() {
        assert_eq!(span(Duration::from_secs(45 * 60)), "45分钟");
        assert_eq!(span(Duration::from_secs(3 * 3600 + 33 * 60)), "3小时33分");
        assert_eq!(span(Duration::from_secs(29 * 3600)), "1天5小时");
    }
    #[test]
    fn resets() {
        let now = UNIX_EPOCH + Duration::from_secs(1_793_700_000);
        assert_eq!(reset(Some(now + Duration::from_secs(2 * 3600)), now), "2小时后");
        assert!(reset(Some(now + Duration::from_secs(3 * 86400)), now).starts_with('周'));
        assert!(reset(Some(now + Duration::from_secs(20 * 86400)), now).contains('月'));
        assert_eq!(reset(Some(now - Duration::from_secs(5)), now), "即将恢复");
    }
    #[test]
    fn percents() {
        assert_eq!(percent(0.4), "<1%");
        assert_eq!(percent(75.2), "75%");
        assert_eq!(percent(0.0), "0%");
    }
}
