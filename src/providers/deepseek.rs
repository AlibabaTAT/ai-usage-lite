//! DeepSeek API account balance (人民币余额).
//!
//! Official endpoint: `GET https://api.deepseek.com/user/balance` with
//! `Authorization: Bearer <API key>`. It only reads the balance and costs
//! nothing.
//!
//! The API key is read, in order, from:
//!   1. the `DEEPSEEK_API_KEY` environment variable;
//!   2. the Windows Credential Manager generic entry `AI-Usage-Lite/DeepSeek`
//!      (encrypted per Windows user; `设置DeepSeek密钥.cmd` creates it with
//!      `cmdkey`). The key is never written to the settings file or logged.

use serde::Deserialize;

use super::wincred;
use crate::diag;
use crate::http;
use crate::model::{Balance, Failure, Snapshot};

pub const BALANCE_URL: &str = "https://api.deepseek.com/user/balance";
pub const CREDENTIAL_TARGET: &str = "AI-Usage-Lite/DeepSeek";

#[derive(Deserialize)]
struct BalanceResponse {
    is_available: Option<bool>,
    balance_infos: Option<Vec<BalanceInfo>>,
}

#[derive(Deserialize)]
struct BalanceInfo {
    currency: Option<String>,
    total_balance: Option<String>,
    granted_balance: Option<String>,
    topped_up_balance: Option<String>,
}

fn api_key() -> Option<(String, &'static str)> {
    if let Ok(k) = std::env::var("DEEPSEEK_API_KEY") {
        if !k.trim().is_empty() {
            return Some((k.trim().to_string(), "环境变量 DEEPSEEK_API_KEY"));
        }
    }
    wincred::read_generic(CREDENTIAL_TARGET)
        .map(|k| k.trim().trim_matches('\0').to_string())
        .filter(|k| !k.is_empty())
        .map(|k| (k, "Windows 凭据管理器"))
}

/// Whether a key is configured (used to decide the default visibility).
pub fn key_present() -> bool {
    api_key().is_some()
}

pub fn poll() -> Result<Snapshot, Failure> {
    let (key, source) = api_key().ok_or_else(|| {
        diag::log(format!("DeepSeek: 未找到 API Key（环境变量 DEEPSEEK_API_KEY 或凭据 {CREDENTIAL_TARGET}）"));
        Failure::NoCredentials
    })?;
    diag::log(format!("DeepSeek: 使用{source}中的 API Key"));
    let auth = format!("Bearer {key}");
    let reply = http::get(BALANCE_URL, &[("Authorization", &auth), ("Accept", "application/json")])?;
    diag::log(format!("DeepSeek: GET /user/balance -> {}", reply.status));
    if !reply.ok() {
        return Err(reply.failure());
    }
    let r: BalanceResponse = reply.json()?;
    let balance = pick(r).ok_or(Failure::BadResponse)?;
    diag::log(format!("DeepSeek: 余额 {} {:.2}（可用：{}）", balance.currency, balance.total, balance.available));
    Ok(Snapshot { pools: Vec::new(), source: format!("DeepSeek API Key（{source}）"), balance: Some(balance) })
}

/// Prefers the CNY entry; falls back to the first one reported.
fn pick(r: BalanceResponse) -> Option<Balance> {
    let available = r.is_available.unwrap_or(true);
    let infos = r.balance_infos?;
    let info = infos
        .iter()
        .find(|b| b.currency.as_deref() == Some("CNY"))
        .or_else(|| infos.first())?;
    let num = |s: &Option<String>| s.as_deref().and_then(|v| v.trim().parse::<f64>().ok()).unwrap_or(0.0);
    Some(Balance {
        currency: info.currency.clone().unwrap_or_else(|| "CNY".into()),
        total: num(&info.total_balance),
        granted: num(&info.granted_balance),
        topped_up: num(&info.topped_up_balance),
        available,
        spend: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prefers_cny_and_parses_strings() {
        let r: BalanceResponse = serde_json::from_str(
            r#"{"is_available":true,"balance_infos":[
                {"currency":"USD","total_balance":"3.00","granted_balance":"0.00","topped_up_balance":"3.00"},
                {"currency":"CNY","total_balance":"110.00","granted_balance":"10.00","topped_up_balance":"100.00"}]}"#,
        )
        .unwrap();
        let b = pick(r).unwrap();
        assert_eq!(b.currency, "CNY");
        assert_eq!((b.total, b.granted, b.topped_up), (110.0, 10.0, 100.0));
        assert!(b.available);
    }
    #[test]
    fn empty_list_is_bad_response() {
        let r: BalanceResponse = serde_json::from_str(r#"{"is_available":false,"balance_infos":[]}"#).unwrap();
        assert!(pick(r).is_none());
    }
}
