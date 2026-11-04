//! HTTP plumbing: one ureq agent per proxy setting, Windows system proxy
//! detection (read-only), and status mapping.

use std::sync::Mutex;
use std::time::Duration;

use crate::diag;
use crate::model::Failure;

pub struct Reply {
    pub status: u16,
    pub body: String,
    pub retry_after: Option<u64>,
}

impl Reply {
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
    pub fn json<T: serde::de::DeserializeOwned>(&self) -> Result<T, Failure> {
        serde_json::from_str(&self.body).map_err(|e| {
            diag::log(format!("JSON 解析失败: {e}"));
            Failure::BadResponse
        })
    }
    /// Generic mapping for a non-2xx reply.
    pub fn failure(&self) -> Failure {
        match self.status {
            401 | 403 => Failure::AuthRejected(self.status),
            429 => Failure::RateLimited(self.retry_after),
            s => Failure::Http(s),
        }
    }
}

static AGENT: Mutex<Option<(Option<String>, ureq::Agent)>> = Mutex::new(None);

fn agent() -> ureq::Agent {
    let proxy = proxy_url();
    let mut guard = AGENT.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((cached_proxy, agent)) = guard.as_ref() {
        if *cached_proxy == proxy {
            return agent.clone();
        }
    }
    let tls = ureq::tls::TlsConfig::builder()
        .provider(ureq::tls::TlsProvider::NativeTls)
        .root_certs(ureq::tls::RootCerts::PlatformVerifier)
        .build();
    let ureq_proxy = proxy.as_deref().and_then(|p| match ureq::Proxy::new(p) {
        Ok(p) => Some(p),
        Err(e) => {
            diag::log(format!("代理地址无法使用 ({p}): {e}"));
            None
        }
    });
    diag::log(match &proxy {
        Some(p) => format!("使用代理 {p}"),
        None => "未使用代理（直连）".to_string(),
    });
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(25)))
        .tls_config(tls)
        .proxy(ureq_proxy)
        .http_status_as_error(false)
        .user_agent("AI-Usage-Lite/0.1")
        .build()
        .into();
    *guard = Some((proxy, agent.clone()));
    agent
}

fn finish(result: Result<ureq::http::Response<ureq::Body>, ureq::Error>) -> Result<Reply, Failure> {
    let mut resp = result.map_err(|e| {
        diag::log(format!("网络请求失败: {e}"));
        Failure::Network(e.to_string())
    })?;
    let status = resp.status().as_u16();
    let retry_after = resp
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok());
    let body = resp
        .body_mut()
        .with_config()
        .limit(2 * 1024 * 1024)
        .read_to_string()
        .unwrap_or_default();
    Ok(Reply { status, body, retry_after })
}

pub fn get(url: &str, headers: &[(&str, &str)]) -> Result<Reply, Failure> {
    let mut req = agent().get(url);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    finish(req.call())
}

pub fn post_json(url: &str, headers: &[(&str, &str)], body: &serde_json::Value) -> Result<Reply, Failure> {
    let mut req = agent().post(url);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    finish(req.send_json(body))
}

pub fn post_form(url: &str, form: &[(&str, &str)]) -> Result<Reply, Failure> {
    finish(agent().post(url).send_form(form.iter().copied()))
}

/// Proxy precedence: HTTPS_PROXY / ALL_PROXY env, then the Windows user
/// proxy (what Clash Verge sets when "系统代理" is on). Read-only.
pub fn proxy_url() -> Option<String> {
    for name in ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"] {
        if let Ok(v) = std::env::var(name) {
            if !v.trim().is_empty() {
                return Some(v.trim().to_string());
            }
        }
    }
    system_proxy()
}

#[cfg(windows)]
fn system_proxy() -> Option<String> {
    use windows::core::w;
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RRF_RT_REG_SZ};

    let key = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings");
    let mut enabled: u32 = 0;
    let mut size = 4u32;
    let rc = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key,
            w!("ProxyEnable"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut enabled as *mut u32 as *mut _),
            Some(&mut size),
        )
    };
    if rc.is_err() || enabled == 0 {
        return None;
    }
    let mut buf = vec![0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    let rc = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key,
            w!("ProxyServer"),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut size),
        )
    };
    if rc.is_err() {
        return None;
    }
    let len = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    parse_wininet_proxy(&String::from_utf16_lossy(&buf[..len]))
}

#[cfg(not(windows))]
fn system_proxy() -> Option<String> {
    None
}

/// "127.0.0.1:7897" or "http=127.0.0.1:7897;https=127.0.0.1:7897;socks=..."
pub fn parse_wininet_proxy(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let pick = if value.contains('=') {
        let parts: Vec<(&str, &str)> = value
            .split(';')
            .filter_map(|p| p.split_once('='))
            .map(|(k, v)| (k.trim(), v.trim()))
            .collect();
        let find = |scheme: &str| parts.iter().find(|(k, _)| k.eq_ignore_ascii_case(scheme)).map(|(_, v)| *v);
        if let Some(v) = find("https").or_else(|| find("http")) {
            v.to_string()
        } else if let Some(v) = find("socks") {
            return Some(if v.contains("://") { v.to_string() } else { format!("socks5://{v}") });
        } else {
            return None;
        }
    } else {
        value.split(';').next()?.trim().to_string()
    };
    if pick.is_empty() {
        None
    } else if pick.contains("://") {
        Some(pick)
    } else {
        Some(format!("http://{pick}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wininet_formats() {
        assert_eq!(parse_wininet_proxy("127.0.0.1:7897").as_deref(), Some("http://127.0.0.1:7897"));
        assert_eq!(
            parse_wininet_proxy("http=127.0.0.1:1;https=127.0.0.1:2").as_deref(),
            Some("http://127.0.0.1:2")
        );
        assert_eq!(parse_wininet_proxy("socks=127.0.0.1:9").as_deref(), Some("socks5://127.0.0.1:9"));
        assert_eq!(parse_wininet_proxy(""), None);
    }
}
