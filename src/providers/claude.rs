//! Claude subscription usage.
//!
//! Endpoint and response parsing follow CodeZeno/Claude-Code-Usage-Monitor
//! v2.23.0 (MIT, (C) 2026 Code Zeno Pty Ltd) `src/poller/claude.rs`.
//!
//! Deliberately removed from upstream, because each spends subscription quota
//! or touches the login:
//!   * the `/v1/messages` header probe (a real 1-token Haiku request) used as a
//!     fallback and to fill missing reset times;
//!   * token refresh by running `claude -p .` (a real model request).
//! If the token has expired we report it and wait for Claude itself to renew.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use super::claude_desktop;
use super::claude_limits;
use super::compat::parse_iso8601;
use crate::diag;
use crate::http;
use crate::model::{Failure, Pool, UsageWindow, Snapshot};

pub const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";

#[derive(Deserialize)]
struct UsageResponse {
    five_hour: Option<UsageBucket>,
    seven_day: Option<UsageBucket>,
    limits: Option<Vec<serde_json::Value>>,
    #[serde(flatten)]
    extra: std::collections::BTreeMap<String, serde_json::Value>,
}

#[derive(Deserialize)]
struct UsageBucket {
    utilization: f64,
    resets_at: Option<String>,
}

struct Token {
    access_token: String,
    expires_at_ms: Option<i64>,
    source: &'static str,
}

impl Token {
    fn expired(&self) -> bool {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as i64;
        self.expires_at_ms.is_some_and(|e| now >= e)
    }
}

fn cli_credentials_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(dir).join(".credentials.json"));
    }
    Some(crate::paths::home_dir()?.join(".claude").join(".credentials.json"))
}

fn read_cli_token(path: &Path) -> Option<Token> {
    let content = std::fs::read_to_string(path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;
    let oauth = json.get("claudeAiOauth")?;
    Some(Token {
        access_token: oauth.get("accessToken")?.as_str().filter(|t| !t.trim().is_empty())?.to_string(),
        expires_at_ms: oauth.get("expiresAt").and_then(|v| v.as_i64()),
        source: "Claude Code 登录",
    })
}

fn cli_token() -> Option<Token> {
    let path = cli_credentials_path()?;
    match read_cli_token(&path) {
        Some(t) => {
            diag::log(format!("Claude: 找到 Claude Code 凭据 {}（{}）", path.display(), if t.expired() { "已过期" } else { "有效" }));
            Some(t)
        }
        None => {
            diag::log(format!("Claude: 未找到可用的 {}", path.display()));
            None
        }
    }
}

/// The desktop app's encrypted token cache. Only consulted when the
/// Claude Code login is missing, expired or rejected. Read-only.
fn desktop_tokens() -> Vec<Token> {
    if std::env::var_os("CLAUDE_CONFIG_DIR").is_some() {
        return Vec::new();
    }
    let mut tokens = Vec::new();
    for config in claude_desktop::config_paths() {
        if !config.exists() {
            continue;
        }
        match claude_desktop::read_token(&config) {
            Some(t) => {
                let t = Token { access_token: t.access_token, expires_at_ms: t.expires_at, source: "Claude 桌面版登录" };
                diag::log(format!("Claude: 找到桌面版登录 {}（{}）", config.display(), if t.expired() { "已过期" } else { "有效" }));
                tokens.push(t);
            }
            None => diag::log(format!("Claude: 桌面版配置 {} 中没有可用令牌", config.display())),
        }
    }
    tokens
}

pub fn poll() -> Result<Snapshot, Failure> {
    let cli = cli_token();
    let mut found_any = cli.is_some();
    let mut last = Failure::TokenExpired;
    if let Some(t) = cli.filter(|t| !t.expired()) {
        match fetch(&t.access_token) {
            Ok(mut snap) => {
                snap.source = t.source.to_string();
                return Ok(snap);
            }
            // A rejected token may be stale; fall through to the desktop login.
            Err(e @ Failure::AuthRejected(_)) => last = e,
            Err(e) => return Err(e),
        }
    }
    let desktop = desktop_tokens();
    found_any |= !desktop.is_empty();
    for t in desktop.iter().filter(|t| !t.expired()) {
        match fetch(&t.access_token) {
            Ok(mut snap) => {
                snap.source = t.source.to_string();
                return Ok(snap);
            }
            Err(e @ Failure::AuthRejected(_)) => last = e,
            Err(e) => return Err(e),
        }
    }
    if found_any { Err(last) } else { Err(Failure::NoCredentials) }
}

fn fetch(token: &str) -> Result<Snapshot, Failure> {
    let auth = format!("Bearer {token}");
    let reply = http::get(USAGE_URL, &[("Authorization", &auth), ("anthropic-beta", "oauth-2025-04-20")])?;
    diag::log(format!("Claude: GET /api/oauth/usage -> {}", reply.status));
    if !reply.ok() {
        return Err(match reply.status {
            401 | 403 | 429 => reply.failure(),
            s if s >= 500 => Failure::Http(s),
            s => Failure::Unsupported(format!("用量接口返回 {s}，此账号暂无法只读查询")),
        });
    }
    let response: UsageResponse = reply.json()?;
    let snap = snapshot_from(response);
    if snap.pools.iter().all(|p| p.windows.is_empty()) {
        return Err(Failure::BadResponse);
    }
    Ok(snap)
}

fn snapshot_from(response: UsageResponse) -> Snapshot {
    let mut windows = Vec::new();
    let mut have_session = false;
    let mut have_weekly = false;
    if let Some(b) = &response.five_hour {
        windows.push(UsageWindow { label: "5 小时".into(), used: b.utilization, resets_at: parse_iso8601(b.resets_at.as_deref()) });
        have_session = true;
    }
    if let Some(b) = &response.seven_day {
        windows.push(UsageWindow { label: "本周".into(), used: b.utilization, resets_at: parse_iso8601(b.resets_at.as_deref()) });
        have_weekly = true;
    }
    let limits = claude_limits::parse(response.limits.as_deref().unwrap_or_default(), &response.extra);
    for limit in &limits {
        let w = |label: String| UsageWindow { label, used: limit.usage.percentage, resets_at: limit.usage.resets_at };
        match (limit.kind.as_str(), limit.model.as_deref()) {
            ("session", None) if !have_session => {
                windows.insert(0, w("5 小时".into()));
                have_session = true;
            }
            ("weekly_all", None) if !have_weekly => {
                windows.push(w("本周".into()));
                have_weekly = true;
            }
            // Per-model weekly caps (e.g. Opus) that the plan actually reports.
            ("weekly_scoped" | "seven_day_opus" | "seven_day_sonnet", Some(model)) => {
                windows.push(w(format!("本周·{model}")));
            }
            (kind, _) => diag::log(format!("Claude: 忽略额度项 {kind}")),
        }
    }
    Snapshot { pools: vec![Pool { name: String::new(), windows }], source: String::new() }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_legacy_and_scoped_windows() {
        let r: UsageResponse = serde_json::from_str(
            r#"{"five_hour":{"utilization":25.0,"resets_at":"2026-11-03T15:00:00Z"},
                "seven_day":{"utilization":23.0,"resets_at":"2026-11-08T00:00:00+00:00"},
                "seven_day_opus":null,
                "seven_day_oauth_apps":{"utilization":5,"resets_at":null}}"#,
        )
        .unwrap();
        let s = snapshot_from(r);
        let w = &s.pools[0].windows;
        assert_eq!(w.len(), 2);
        assert_eq!(w[0].label, "5 小时");
        assert_eq!(w[0].used, 25.0);
        assert!(w[1].resets_at.is_some());
    }

    #[test]
    fn array_only_response_fills_standard_windows() {
        let r: UsageResponse = serde_json::from_str(
            r#"{"limits":[{"kind":"session","percent":40,"resets_at":"2026-11-03T15:00:00Z"},
                          {"kind":"weekly_all","percent":10,"resets_at":"2026-11-08T00:00:00Z"},
                          {"kind":"weekly_scoped","percent":70,"scope":{"model":{"display_name":"Opus"}}}]}"#,
        )
        .unwrap();
        let s = snapshot_from(r);
        let labels: Vec<_> = s.pools[0].windows.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(labels, ["5 小时", "本周", "本周·Opus"]);
    }
}
