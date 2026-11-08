//! Long-term usage log, for monthly / yearly reports made later by other
//! tools. The app itself computes no statistics: after each successful poll
//! it appends one JSON line per usage window or balance whose value changed
//! (plus one per window after each start, as an anchor). An append is a few
//! hundred bytes, so the cost is negligible.
//!
//! Files: `<dir>\usage-YYYY-MM-<computer>.jsonl`, one per month and device
//! (two PCs syncing the same folder never write the same file). The first
//! line of each file is a `meta` record with the app version and the monthly
//! subscription fees from the settings. Nothing is ever deleted.
//!
//! `<dir>` is `usage_log_dir` in config.json, or by default
//! %LOCALAPPDATA%\AI-Usage-Lite\usage-log. The log holds no credentials.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::SystemTime;

use serde_json::{json, Value};

use crate::config::Config;
use crate::model::{ProviderId, Snapshot};
use crate::ui::fmt;

/// Last value written per key ("Claude||5 小时" → (used, reset unix)).
static LAST: Mutex<Option<HashMap<String, (f64, i64)>>> = Mutex::new(None);

pub fn dir(cfg: &Config) -> Option<PathBuf> {
    match cfg.usage_log_dir.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(d) => Some(PathBuf::from(d)),
        None => crate::paths::report_dir().map(|d| d.join("usage-log")),
    }
}

fn device() -> String {
    let name = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "PC".into());
    let clean: String = name.chars().filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_').collect();
    if clean.is_empty() { "PC".into() } else { clean }
}

fn unix(t: SystemTime) -> i64 {
    crate::spend::unix(t) as i64
}

/// Lines to append for this snapshot, given what was written before.
/// Pure, so it can be tested; updates `last`.
pub fn lines_for(id: ProviderId, snap: &Snapshot, at: SystemTime, last: &mut HashMap<String, (f64, i64)>) -> Vec<Value> {
    let t = fmt::iso(at);
    let mut out = Vec::new();
    for pool in &snap.pools {
        for w in &pool.windows {
            let key = format!("{}|{}|{}", id.name(), pool.name, w.label);
            let reset = w.resets_at.map(unix).unwrap_or(0);
            let changed = match last.get(&key) {
                Some(&(used, r)) => (used - w.used).abs() >= 0.05 || (r - reset).abs() > 120,
                None => true,
            };
            if !changed {
                continue;
            }
            last.insert(key, (w.used, reset));
            let mut v = json!({
                "t": t,
                "service": id.name(),
                "window": w.label,
                "used_pct": (w.used * 10.0).round() / 10.0,
                "resets_at": w.resets_at.map(fmt::iso),
            });
            if !pool.name.is_empty() {
                v["pool"] = json!(pool.name);
            }
            out.push(v);
        }
    }
    if let Some(b) = &snap.balance {
        let key = format!("{}|balance", id.name());
        let flag = if b.available { 1 } else { 0 };
        let changed = match last.get(&key) {
            Some(&(total, f)) => (total - b.total).abs() >= 0.005 || f != flag,
            None => true,
        };
        if changed {
            last.insert(key, (b.total, flag));
            out.push(json!({
                "t": t,
                "service": id.name(),
                "balance": (b.total * 100.0).round() / 100.0,
                "currency": b.currency,
                "available": b.available,
            }));
        }
    }
    out
}

fn meta(cfg: &Config) -> Value {
    json!({
        "type": "meta",
        "app": "AI Usage Lite",
        "version": env!("CARGO_PKG_VERSION"),
        "device": device(),
        "subscriptions": cfg.subscriptions,
        "usd_to_cny": cfg.usd_to_cny,
        "note": "每行一条读数，仅在数值变化或程序启动后首次读取时记录。used_pct=已用百分比（0-100），resets_at=该窗口下次重置时间；balance=账户余额。时间为本地时间（含时区）。",
    })
}

/// Appends the changed readings of a successful live poll.
pub fn record(cfg: &Config, id: ProviderId, snap: &Snapshot, at: SystemTime) {
    if !cfg.usage_log {
        return;
    }
    let Some(dir) = dir(cfg) else { return };
    let Ok(mut guard) = LAST.lock() else { return };
    let last = guard.get_or_insert_with(HashMap::new);
    let lines = lines_for(id, snap, at, last);
    if lines.is_empty() {
        return;
    }
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join(format!("usage-{}-{}.jsonl", fmt::year_month(at), device()));
    let fresh = !path.exists();
    let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) else { return };
    let mut buf = String::new();
    if fresh {
        buf.push_str(&meta(cfg).to_string());
        buf.push('\n');
    }
    for l in lines {
        buf.push_str(&l.to_string());
        buf.push('\n');
    }
    let _ = f.write_all(buf.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Balance, Pool, UsageWindow};
    use std::time::Duration;

    #[test]
    fn writes_on_start_and_on_change_only() {
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let reset = Some(t + Duration::from_secs(3600));
        let snap = |used: f64| Snapshot {
            pools: vec![Pool { name: String::new(), windows: vec![UsageWindow { label: "本周".into(), used, resets_at: reset, recent_rate: None }] }],
            source: String::new(),
            balance: None,
        };
        let mut last = HashMap::new();
        let first = lines_for(ProviderId::Codex, &snap(76.0), t, &mut last);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0]["used_pct"], 76.0);
        assert_eq!(first[0]["window"], "本周");
        assert!(first[0].get("pool").is_none());
        assert!(lines_for(ProviderId::Codex, &snap(76.0), t, &mut last).is_empty());
        assert_eq!(lines_for(ProviderId::Codex, &snap(77.0), t, &mut last).len(), 1);
    }

    #[test]
    fn balance_lines() {
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let snap = |total: f64| Snapshot {
            pools: Vec::new(),
            source: String::new(),
            balance: Some(Balance { currency: "CNY".into(), total, granted: 0.0, topped_up: total, available: true, spend: None }),
        };
        let mut last = HashMap::new();
        let l = lines_for(ProviderId::DeepSeek, &snap(70.8), t, &mut last);
        assert_eq!(l[0]["balance"], 70.8);
        assert_eq!(l[0]["currency"], "CNY");
        assert!(lines_for(ProviderId::DeepSeek, &snap(70.8), t, &mut last).is_empty());
        assert_eq!(lines_for(ProviderId::DeepSeek, &snap(70.5), t, &mut last).len(), 1);
    }

    #[test]
    fn iso_has_offset() {
        // Non-Windows test builds use UTC+8; on Windows the machine zone.
        let s = fmt::iso(SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000));
        assert_eq!(s.len(), 25, "{s}");
        assert!(s.as_bytes()[19] == b'+' || s.as_bytes()[19] == b'-');
    }
}
