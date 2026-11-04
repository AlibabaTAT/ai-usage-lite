//! Minimal stand-ins for the upstream `models` / `poller` helpers that the
//! vendored modules expect. Parsing helpers are vendored from
//! CodeZeno/Claude-Code-Usage-Monitor v2.23.0 (MIT, (C) 2026 Code Zeno Pty Ltd).

use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct UsageSection {
    pub available: bool,
    pub percentage: f64,
    pub resets_at: Option<SystemTime>,
}

#[derive(Clone, Debug, PartialEq)]
#[allow(dead_code)]
pub struct UsageLimit {
    pub key: String,
    pub kind: String,
    pub label: String,
    pub model: Option<String>,
    pub model_id: Option<String>,
    pub scope: Option<serde_json::Value>,
    pub is_active: bool,
    pub usage: UsageSection,
}

pub fn limit_slug(value: &str) -> String {
    let mut result = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            result.push(ch.to_ascii_lowercase());
        } else if !result.is_empty() && !result.ends_with('_') {
            result.push('_');
        }
    }
    let result = result.trim_end_matches('_');
    if result.is_empty() {
        "unnamed".to_string()
    } else {
        result.to_string()
    }
}

pub fn unix_to_system_time(unix_secs: Option<i64>) -> Option<SystemTime> {
    let secs = unix_secs?;
    if secs <= 0 {
        return None;
    }
    Some(UNIX_EPOCH + Duration::from_secs(secs as u64))
}

/// Parse an ISO 8601 timestamp string into a SystemTime.
pub fn parse_iso8601(s: Option<&str>) -> Option<SystemTime> {
    let unix_secs = parse_datetime_to_unix(s?)?;
    UNIX_EPOCH.checked_add(Duration::from_secs(unix_secs))
}

fn parse_datetime_to_unix(s: &str) -> Option<u64> {
    let (datetime, offset_seconds) = split_timezone(s)?;
    let datetime = match datetime.split_once('.') {
        Some((base, fraction))
            if !fraction.is_empty() && fraction.bytes().all(|b| b.is_ascii_digit()) =>
        {
            base
        }
        Some(_) => return None,
        None => datetime,
    };
    let bytes = datetime.as_bytes();
    if bytes.len() != 19
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return None;
    }
    let year = parse_digits(&bytes[0..4])?;
    let month = parse_digits(&bytes[5..7])?;
    let day = parse_digits(&bytes[8..10])?;
    let hour = parse_digits(&bytes[11..13])?;
    let minute = parse_digits(&bytes[14..16])?;
    let second = parse_digits(&bytes[17..19])?;
    if !(1970..=9999).contains(&year)
        || !(1..=12).contains(&month)
        || day == 0
        || day > days_in_month(year, month)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    let mut days: u64 = 0;
    for y in 1970..year {
        days += if is_leap(y) { 366 } else { 365 };
    }
    for m in 1..month {
        days += days_in_month(year, m);
    }
    days += day - 1;
    let local_seconds = days
        .checked_mul(86_400)?
        .checked_add(hour * 3_600 + minute * 60 + second)?;
    u64::try_from(i64::try_from(local_seconds).ok()?.checked_sub(offset_seconds)?).ok()
}

fn split_timezone(s: &str) -> Option<(&str, i64)> {
    if let Some(datetime) = s.strip_suffix('Z') {
        return Some((datetime, 0));
    }
    if s.len() >= 25 {
        let offset_start = s.len() - 6;
        let offset = &s.as_bytes()[offset_start..];
        if matches!(offset[0], b'+' | b'-') && offset[3] == b':' {
            let hours = parse_digits(&offset[1..3])?;
            let minutes = parse_digits(&offset[4..6])?;
            if hours > 23 || minutes > 59 {
                return None;
            }
            let seconds = i64::try_from(hours * 3_600 + minutes * 60).ok()?;
            return Some((
                &s[..offset_start],
                if offset[0] == b'+' { seconds } else { -seconds },
            ));
        }
    }
    Some((s, 0))
}

fn parse_digits(bytes: &[u8]) -> Option<u64> {
    let mut value = 0u64;
    for b in bytes {
        if !b.is_ascii_digit() {
            return None;
        }
        value = value * 10 + u64::from(b - b'0');
    }
    Some(value)
}

fn is_leap(y: u64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn days_in_month(y: u64, m: u64) -> u64 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(y) => 29,
        2 => 28,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_offsets_and_fractions() {
        let a = parse_iso8601(Some("2026-11-03T10:00:00Z")).unwrap();
        let b = parse_iso8601(Some("2026-11-03T18:00:00.123456+08:00")).unwrap();
        assert_eq!(a, b);
        assert!(parse_iso8601(Some("garbage")).is_none());
    }
}
