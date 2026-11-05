//! Google Antigravity quota.
//!
//! Credential location, endpoints, request bodies and the quota-summary
//! parsing follow CodeZeno/Claude-Code-Usage-Monitor v2.23.0
//! (MIT, (C) 2026 Code Zeno Pty Ltd) `src/poller/antigravity.rs`.
//!
//! Differences from upstream:
//!   * The OAuth access-token refresh is OFF unless the user enables it in
//!     settings. When on, the new access token lives only in memory and is
//!     never written back to the Windows credential store.
//!   * The installed-binary scan for the OAuth client is streamed in chunks
//!     (the language server binary is large) and cached for the session.
//!   * Every reported model group is kept as its own pool; which pools are
//!     shown is a user setting, nothing is hard-coded as available.

use std::fs::File;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use serde::Deserialize;

use super::compat::parse_iso8601;
use super::wincred;
use crate::diag;
use crate::http;
use crate::model::{Failure, Pool, UsageWindow, Snapshot};

const CREDENTIAL_TARGET: &str = "gemini:antigravity";
const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const EXPIRY_SKEW: Duration = Duration::from_secs(60);
const USER_AGENT: &str = "antigravity/cli/1.4.0";
pub const ENDPOINTS: &[&str] = &[
    "https://daily-cloudcode-pa.googleapis.com",
    "https://cloudcode-pa.googleapis.com",
];

#[derive(Deserialize)]
struct AuthFile {
    token: TokenData,
}

#[derive(Deserialize)]
struct TokenData {
    access_token: String,
    refresh_token: Option<String>,
    expiry: Option<String>,
}

#[derive(Deserialize)]
struct LoadResponse {
    #[serde(rename = "cloudaicompanionProject")]
    project: Option<String>,
}

#[derive(Deserialize)]
struct SummaryResponse {
    groups: Option<Vec<SummaryGroup>>,
}

#[derive(Deserialize)]
struct SummaryGroup {
    #[serde(rename = "displayName")]
    display_name: Option<String>,
    description: Option<String>,
    buckets: Option<Vec<SummaryBucket>>,
}

#[derive(Deserialize)]
struct SummaryBucket {
    #[serde(rename = "bucketId")]
    bucket_id: Option<String>,
    #[serde(rename = "displayName")]
    display_name: Option<String>,
    window: Option<String>,
    #[serde(rename = "remainingFraction")]
    remaining_fraction: Option<f64>,
    #[serde(rename = "resetTime")]
    reset_time: Option<String>,
}

#[derive(Deserialize)]
struct ModelsResponse {
    models: std::collections::HashMap<String, ModelInfo>,
}

#[derive(Deserialize)]
struct ModelInfo {
    #[serde(rename = "quotaInfo")]
    quota_info: Option<QuotaInfo>,
}

#[derive(Deserialize)]
struct QuotaInfo {
    #[serde(rename = "remainingFraction")]
    remaining_fraction: Option<f64>,
    #[serde(rename = "resetTime")]
    reset_time: Option<String>,
}

/// In-memory only: the refreshed access token and its expiry.
static MEMORY_TOKEN: Mutex<Option<(String, SystemTime)>> = Mutex::new(None);
static OAUTH_CLIENTS: Mutex<Option<Vec<(String, String)>>> = Mutex::new(None);

pub fn poll(allow_refresh: bool) -> Result<Snapshot, Failure> {
    let content = wincred::read_generic(CREDENTIAL_TARGET).ok_or(Failure::NoCredentials)?;
    let auth: AuthFile = serde_json::from_str(&content).map_err(|_| {
        diag::log("Antigravity: 凭据格式无法识别");
        Failure::NoCredentials
    })?;
    let token = auth.token;
    let expiry = parse_iso8601(token.expiry.as_deref());
    let stored_fresh = !token.access_token.is_empty() && expiry.is_none_or(|e| e > SystemTime::now() + EXPIRY_SKEW);
    diag::log(format!(
        "Antigravity: 找到 Windows 凭据 {CREDENTIAL_TARGET}（访问令牌{}，刷新令牌{}）",
        if stored_fresh { "有效" } else { "已过期" },
        if token.refresh_token.as_deref().is_some_and(|r| !r.is_empty()) { "存在" } else { "不存在" }
    ));

    let mut access = if stored_fresh { Some(token.access_token.clone()) } else { None };
    if access.is_none() {
        if let Ok(guard) = MEMORY_TOKEN.lock() {
            if let Some((t, exp)) = guard.as_ref() {
                if *exp > SystemTime::now() + EXPIRY_SKEW {
                    access = Some(t.clone());
                }
            }
        }
    }
    let refresh_token = token.refresh_token.as_deref().filter(|r| !r.is_empty());
    let access = match access {
        Some(a) => a,
        None if allow_refresh && refresh_token.is_some() => refresh(refresh_token.unwrap())?,
        None => return Err(Failure::TokenExpired),
    };

    match fetch(&access) {
        Err(Failure::AuthRejected(_)) if allow_refresh && refresh_token.is_some() && stored_fresh => {
            let access = refresh(refresh_token.unwrap())?;
            fetch(&access)
        }
        other => other,
    }
}

fn fetch(token: &str) -> Result<Snapshot, Failure> {
    let auth = format!("Bearer {token}");
    let headers = [("Authorization", auth.as_str()), ("Content-Type", "application/json"), ("User-Agent", USER_AGENT)];
    let mut last = Failure::BadResponse;
    let mut fallback: Option<Snapshot> = None;
    for base in ENDPOINTS {
        let reply = match http::post_json(&format!("{base}/v1internal:loadCodeAssist"), &headers, &serde_json::json!({"metadata": {"ideType": "ANTIGRAVITY"}})) {
            Ok(r) => r,
            Err(e) => {
                last = e;
                continue;
            }
        };
        diag::log(format!("Antigravity: {base} loadCodeAssist -> {}", reply.status));
        if !reply.ok() {
            let f = reply.failure();
            if matches!(f, Failure::AuthRejected(_)) {
                return Err(f);
            }
            last = f;
            continue;
        }
        let project = reply.json::<LoadResponse>()?.project.filter(|p| !p.is_empty());
        if let Some(project) = project.as_deref() {
            match http::post_json(&format!("{base}/v1internal:retrieveUserQuotaSummary"), &headers, &serde_json::json!({"project": project})) {
                Ok(r) if r.ok() => {
                    diag::log(format!("Antigravity: retrieveUserQuotaSummary -> {}", r.status));
                    let summary: SummaryResponse = r.json()?;
                    if let Some(snap) = snapshot_from_summary(summary) {
                        return Ok(snap);
                    }
                }
                Ok(r) => {
                    diag::log(format!("Antigravity: retrieveUserQuotaSummary -> {}", r.status));
                    if matches!(r.failure(), Failure::AuthRejected(_)) {
                        return Err(r.failure());
                    }
                }
                Err(e) => last = e,
            }
        } else {
            diag::log("Antigravity: 账号没有返回 Code Assist 项目");
        }
        // Fallback: per-model quota of Gemini models (window length unknown).
        let body = match project.as_deref() {
            Some(p) => serde_json::json!({ "project": p }),
            None => serde_json::json!({}),
        };
        if let Ok(r) = http::post_json(&format!("{base}/v1internal:fetchAvailableModels"), &headers, &body) {
            diag::log(format!("Antigravity: fetchAvailableModels -> {}", r.status));
            if r.ok() {
                if let Ok(models) = r.json::<ModelsResponse>() {
                    let names: Vec<_> = models.models.keys().cloned().collect();
                    diag::log(format!("Antigravity: 可用模型 {}", names.join(", ")));
                    if fallback.is_none() {
                        fallback = snapshot_from_models(&models);
                    }
                }
            }
        }
    }
    fallback.ok_or(last)
}

fn window_label(window: Option<&str>) -> Option<String> {
    let w = window?.trim();
    Some(match w.to_ascii_lowercase().as_str() {
        "5h" => "5 小时".into(),
        "weekly" | "7d" => "本周".into(),
        "daily" | "24h" | "1d" => "今日".into(),
        _ => w.to_string(),
    })
}

fn snapshot_from_summary(summary: SummaryResponse) -> Option<Snapshot> {
    let groups = summary.groups?;
    let mut pools = Vec::new();
    for group in groups {
        let name = group
            .display_name
            .clone()
            .filter(|n| !n.trim().is_empty())
            .or_else(|| group.description.clone())
            .unwrap_or_else(|| "未命名额度池".into());
        let mut windows = Vec::new();
        for b in group.buckets.unwrap_or_default() {
            let Some(remaining) = b.remaining_fraction.filter(|f| f.is_finite()) else { continue };
            let Some(label) = window_label(b.window.as_deref()) else { continue };
            diag::log(format!(
                "Antigravity: 池「{name}」窗口 {label} 剩余 {:.0}% bucket={} {}",
                remaining * 100.0,
                b.bucket_id.as_deref().unwrap_or("-"),
                b.display_name.as_deref().unwrap_or("")
            ));
            windows.push(UsageWindow { label, used: (1.0 - remaining.clamp(0.0, 1.0)) * 100.0, resets_at: parse_iso8601(b.reset_time.as_deref()), recent_rate: None });
        }
        // 5 小时 first, then weekly, then anything else.
        windows.sort_by_key(|w| match w.label.as_str() {
            "5 小时" => 0,
            "今日" => 1,
            "本周" => 2,
            _ => 3,
        });
        if !windows.is_empty() {
            pools.push(Pool { name, windows });
        }
    }
    Some(Snapshot { pools, source: "Antigravity 登录".into() })
}

fn snapshot_from_models(models: &ModelsResponse) -> Option<Snapshot> {
    let best = models
        .models
        .iter()
        .filter(|(name, _)| name.starts_with("gemini"))
        .filter_map(|(_, info)| {
            let q = info.quota_info.as_ref()?;
            let rem = q.remaining_fraction?.clamp(0.0, 1.0);
            Some(UsageWindow { label: "当前".into(), used: (1.0 - rem) * 100.0, resets_at: parse_iso8601(q.reset_time.as_deref()), recent_rate: None })
        })
        .min_by(|a, b| b.used.total_cmp(&a.used))?;
    Some(Snapshot { pools: vec![Pool { name: "Gemini".into(), windows: vec![best] }], source: "Antigravity 登录（模型额度）".into() })
}

fn refresh(refresh_token: &str) -> Result<String, Failure> {
    let clients = {
        let mut guard = OAUTH_CLIENTS.lock().unwrap_or_else(|e| e.into_inner());
        guard.get_or_insert_with(installed_oauth_clients).clone()
    };
    if clients.is_empty() {
        diag::log("Antigravity: 未在本机 Antigravity 安装目录找到 OAuth 客户端信息，无法续期");
        return Err(Failure::TokenExpired);
    }
    for (id, secret) in clients {
        let reply = http::post_form(
            GOOGLE_TOKEN_URL,
            &[("client_id", &id), ("client_secret", &secret), ("refresh_token", refresh_token), ("grant_type", "refresh_token")],
        )?;
        diag::log(format!("Antigravity: 内存续期 oauth2/token -> {}", reply.status));
        match reply.status {
            200..=299 => {
                #[derive(Deserialize)]
                struct R {
                    access_token: String,
                    expires_in: Option<u64>,
                }
                let r: R = reply.json()?;
                if r.access_token.is_empty() {
                    return Err(Failure::BadResponse);
                }
                let exp = SystemTime::now() + Duration::from_secs(r.expires_in.unwrap_or(3000));
                if let Ok(mut g) = MEMORY_TOKEN.lock() {
                    *g = Some((r.access_token.clone(), exp));
                }
                return Ok(r.access_token);
            }
            400 | 401 | 403 => continue, // this client pair did not issue the token
            429 => return Err(Failure::RateLimited(reply.retry_after)),
            s => return Err(Failure::Http(s)),
        }
    }
    Err(Failure::AuthRejected(400))
}

fn installed_oauth_clients() -> Vec<(String, String)> {
    let mut paths: Vec<PathBuf> = Vec::new();
    if let Some(local) = crate::paths::local_app_data_dir() {
        paths.push(local.join("Programs/Antigravity/resources/bin/language_server.exe"));
        paths.push(local.join("agy/bin/agy.exe"));
    }
    if let Some(pf) = std::env::var_os("ProgramFiles") {
        paths.push(PathBuf::from(pf).join("Antigravity/resources/bin/language_server.exe"));
    }
    for path in paths {
        if let Ok(file) = File::open(&path) {
            let clients = scan_oauth_clients(file);
            if !clients.is_empty() {
                return clients;
            }
        }
    }
    Vec::new()
}

/// Streams the binary in 1 MiB chunks. Token runs are short (<100 bytes), so
/// carrying the tail of each chunk over is enough to catch boundary hits.
fn scan_oauth_clients(mut reader: impl Read) -> Vec<(String, String)> {
    const CHUNK: usize = 1 << 20;
    const CARRY: usize = 256;
    let mut ids: Vec<String> = Vec::new();
    let mut secrets: Vec<String> = Vec::new();
    let mut buf = vec![0u8; CHUNK + CARRY];
    let mut carried = 0usize;
    loop {
        let n = match reader.read(&mut buf[carried..]) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        let filled = carried + n;
        scan_slice(&buf[..filled], &mut ids, &mut secrets);
        carried = CARRY.min(filled);
        buf.copy_within(filled - carried..filled, 0);
    }
    ids.into_iter().flat_map(|id| secrets.iter().cloned().map(move |s| (id.clone(), s))).collect()
}

fn scan_slice(bytes: &[u8], ids: &mut Vec<String>, secrets: &mut Vec<String>) {
    const ID_SUFFIX: &[u8] = b".apps.googleusercontent.com";
    const SECRET_PREFIX: &[u8] = b"GOCSPX-";
    for run in bytes.split(|b| !b.is_ascii_alphanumeric() && !matches!(*b, b'.' | b'_' | b'-')) {
        if run.len() < SECRET_PREFIX.len() {
            continue;
        }
        if let Some(at) = find(run, ID_SUFFIX) {
            // "<digits>-<hash>.apps.googleusercontent.com"
            let head = &run[..at];
            if let Some(hyphen) = head.iter().rposition(|b| *b == b'-') {
                let mut start = hyphen;
                while start > 0 && run[start - 1].is_ascii_digit() {
                    start -= 1;
                }
                let hash = &run[hyphen + 1..at];
                if hyphen - start >= 10 && (20..=80).contains(&hash.len()) {
                    if let Ok(id) = std::str::from_utf8(&run[start..at + ID_SUFFIX.len()]) {
                        if !ids.iter().any(|e| e == id) {
                            ids.push(id.to_string());
                        }
                    }
                }
            }
        }
        if let Some(at) = find(run, SECRET_PREFIX) {
            if let Some(c) = run.get(at..at + 35) {
                if c.iter().all(|b| b.is_ascii_alphanumeric() || matches!(*b, b'_' | b'-')) {
                    if let Ok(s) = std::str::from_utf8(c) {
                        if !secrets.iter().any(|e| e == s) {
                            secrets.push(s.to_string());
                        }
                    }
                }
            }
        }
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn summary_groups_become_pools() {
        let s: SummaryResponse = serde_json::from_str(
            r#"{"groups":[
              {"displayName":"Gemini Models","buckets":[
                 {"window":"weekly","remainingFraction":0.8,"resetTime":"2026-11-08T00:00:00Z"},
                 {"window":"5h","remainingFraction":0.5,"resetTime":"2026-11-03T18:00:00Z"}]},
              {"displayName":"Gemini 4 Argon","buckets":[{"window":"5h","remainingFraction":1.0}]},
              {"displayName":"Empty","buckets":[]}]}"#,
        )
        .unwrap();
        let snap = snapshot_from_summary(s).unwrap();
        assert_eq!(snap.pools.len(), 2);
        assert_eq!(snap.pools[0].windows[0].label, "5 小时");
        assert!((snap.pools[0].windows[0].used - 50.0).abs() < 1e-9);
    }

    #[test]
    fn streamed_scan_finds_pairs_across_chunk_boundaries() {
        let mut data = vec![0u8; (1 << 20) - 20];
        data.extend_from_slice(b"\x001234567890123-abcdefghijklmnopqrstuvwxyz012345.apps.googleusercontent.com\x00");
        data.extend_from_slice(b"GOCSPX-abcdefghijklmnopqrstuvwxyz0123\x00");
        let found = scan_oauth_clients(&data[..]);
        assert_eq!(found.len(), 1);
        assert!(found[0].0.ends_with(".apps.googleusercontent.com"));
        assert!(found[0].1.starts_with("GOCSPX-"));
    }
}
