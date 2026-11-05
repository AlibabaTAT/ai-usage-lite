//! Codex (ChatGPT plan) usage.
//!
//! Endpoint, headers and window mapping follow CodeZeno/Claude-Code-Usage-Monitor
//! v2.23.0 (MIT, (C) 2026 Code Zeno Pty Ltd) `src/poller/codex.rs`.
//!
//! Removed from upstream: the token "refresh" that runs `codex exec .`, which
//! starts a real model turn and spends quota. On 401 we report and wait for
//! Codex itself to renew `auth.json`.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use super::compat::unix_to_system_time;
use crate::diag;
use crate::http;
use crate::model::{Failure, Pool, UsageWindow, Snapshot};

pub const USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const WEEKLY_WINDOW_THRESHOLD_SECONDS: i64 = 86_400;

#[derive(Deserialize)]
struct AuthFile {
    tokens: Option<TokenData>,
}

#[derive(Deserialize)]
struct TokenData {
    access_token: String,
    account_id: Option<String>,
}

#[derive(Deserialize)]
struct UsageResponse {
    rate_limit: Option<Option<Box<RateLimitDetails>>>,
    spend_control: Option<SpendControl>,
    plan_type: Option<String>,
}

#[derive(Deserialize)]
struct SpendControl {
    individual_limit: Option<IndividualLimit>,
}

#[derive(Deserialize)]
struct IndividualLimit {
    used_percent: Option<f64>,
    reset_at: Option<i64>,
}

#[derive(Deserialize)]
struct RateLimitDetails {
    primary_window: Option<Option<Box<RateLimitWindow>>>,
    secondary_window: Option<Option<Box<RateLimitWindow>>>,
}

#[derive(Deserialize)]
struct RateLimitWindow {
    used_percent: f64,
    reset_at: i64,
    limit_window_seconds: Option<i64>,
}

pub fn auth_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CODEX_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(dir).join("auth.json"));
    }
    Some(crate::paths::home_dir()?.join(".codex").join("auth.json"))
}

pub fn poll() -> Result<Snapshot, Failure> {
    let path = auth_path().ok_or(Failure::NoCredentials)?;
    let content = std::fs::read_to_string(&path).map_err(|_| {
        diag::log(format!("Codex: 未找到 {}", path.display()));
        Failure::NoCredentials
    })?;
    let auth: AuthFile = serde_json::from_str(&content).map_err(|_| Failure::NoCredentials)?;
    let tokens = auth.tokens.filter(|t| !t.access_token.trim().is_empty()).ok_or_else(|| {
        diag::log("Codex: auth.json 中没有 ChatGPT 登录令牌（可能是 API Key 模式）");
        Failure::NoCredentials
    })?;
    let exp = jwt_exp(&tokens.access_token);
    diag::log(format!(
        "Codex: 找到 {}（令牌{}）",
        path.display(),
        match exp {
            Some(e) if e <= now_secs() => "已过期",
            Some(_) => "有效",
            None => "有效期未知",
        }
    ));
    if exp.is_some_and(|e| e <= now_secs()) {
        return Err(Failure::TokenExpired);
    }

    let auth_header = format!("Bearer {}", tokens.access_token);
    let mut headers = vec![("Authorization", auth_header.as_str()), ("User-Agent", "codex-cli")];
    let account = tokens.account_id.filter(|a| !a.is_empty());
    if let Some(a) = account.as_deref() {
        headers.push(("ChatGPT-Account-Id", a));
    }
    let reply = http::get(USAGE_URL, &headers)?;
    diag::log(format!("Codex: GET /backend-api/wham/usage -> {}", reply.status));
    if !reply.ok() {
        return Err(reply.failure());
    }
    let response: UsageResponse = reply.json()?;
    if let Some(plan) = &response.plan_type {
        diag::log(format!("Codex: plan_type = {plan}"));
    }
    snapshot_from(response).ok_or(Failure::BadResponse)
}

fn snapshot_from(response: UsageResponse) -> Option<Snapshot> {
    let details = response.rate_limit.flatten();
    let monthly = response.spend_control.and_then(|s| s.individual_limit).and_then(|l| {
        let used = l.used_percent?.clamp(0.0, 100.0);
        used.is_finite().then(|| UsageWindow { label: "30 天".into(), used, resets_at: unix_to_system_time(l.reset_at), recent_rate: None })
    });
    if details.is_none() && monthly.is_none() {
        return None;
    }
    let mut session = None;
    let mut weekly = None;
    // Assign by window length, not by slot (upstream finding: Codex has sent
    // the weekly window in `primary_window` with the 5h window switched off).
    if let Some(d) = details {
        for (window, default_weekly) in [(d.primary_window.flatten(), false), (d.secondary_window.flatten(), true)] {
            let Some(w) = window else { continue };
            let is_weekly = w.limit_window_seconds.map(|s| s >= WEEKLY_WINDOW_THRESHOLD_SECONDS).unwrap_or(default_weekly);
            let q = UsageWindow {
                label: if is_weekly { "本周".into() } else { "5 小时".into() },
                used: w.used_percent,
                resets_at: unix_to_system_time(Some(w.reset_at)),
                recent_rate: None,
            };
            if is_weekly {
                weekly = Some(q);
            } else {
                session = Some(q);
            }
        }
    }
    let mut windows: Vec<UsageWindow> = [session, weekly].into_iter().flatten().collect();
    if windows.iter().all(|w| w.label != "本周") {
        windows.extend(monthly);
    }
    Some(Snapshot { pools: vec![Pool { name: String::new(), windows }], source: "Codex 登录".into() })
}

fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64
}

/// Reads only the `exp` claim of the access token. No signature check is
/// needed: this only decides whether a request is worth sending.
fn jwt_exp(token: &str) -> Option<i64> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64url_decode(payload)?;
    let json: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    json.get("exp")?.as_i64()
}

fn base64url_decode(input: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0;
    for c in input.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            b'=' => break,
            _ => return None,
        } as u32;
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn windows_assigned_by_length() {
        let r: UsageResponse = serde_json::from_str(
            r#"{"rate_limit":{"primary_window":{"used_percent":3,"reset_at":1793800000,"limit_window_seconds":604800},
                "secondary_window":null}}"#,
        )
        .unwrap();
        let s = snapshot_from(r).unwrap();
        assert_eq!(s.pools[0].windows.len(), 1);
        assert_eq!(s.pools[0].windows[0].label, "本周");
    }
    #[test]
    fn both_windows() {
        let r: UsageResponse = serde_json::from_str(
            r#"{"plan_type":"plus","rate_limit":{"primary_window":{"used_percent":0,"reset_at":1793800000,"limit_window_seconds":18000},
                "secondary_window":{"used_percent":3,"reset_at":1794300000,"limit_window_seconds":604800}}}"#,
        )
        .unwrap();
        let w = &snapshot_from(r).unwrap().pools[0].windows;
        assert_eq!((w[0].label.as_str(), w[1].label.as_str()), ("5 小时", "本周"));
    }
    #[test]
    fn jwt_exp_is_read() {
        // {"exp":1700000000}
        assert_eq!(jwt_exp("x.eyJleHAiOjE3MDAwMDAwMDB9.y"), Some(1_700_000_000));
    }
}
