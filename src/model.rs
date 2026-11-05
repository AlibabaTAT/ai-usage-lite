//! Display-oriented data model shared by providers, scheduler and UI.

use std::time::SystemTime;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderId {
    Claude,
    Codex,
    Antigravity,
}

impl ProviderId {
    pub const ALL: [ProviderId; 3] = [ProviderId::Claude, ProviderId::Codex, ProviderId::Antigravity];

    pub fn index(self) -> usize {
        self as usize
    }

    pub fn name(self) -> &'static str {
        match self {
            ProviderId::Claude => "Claude",
            ProviderId::Codex => "Codex",
            ProviderId::Antigravity => "Antigravity",
        }
    }
}

/// One usage window, e.g. "5 小时" or "本周".
#[derive(Clone, Debug, PartialEq)]
pub struct UsageWindow {
    pub label: String,
    /// Percentage already used, 0..=100.
    pub used: f64,
    pub resets_at: Option<SystemTime>,
    /// Recent consumption rate in percent per second, from readings this app
    /// took over the last part of the window. Filled in by the scheduler.
    pub recent_rate: Option<f64>,
}

impl UsageWindow {
    pub fn remaining(&self) -> f64 {
        (100.0 - self.used).clamp(0.0, 100.0)
    }
    pub fn exhausted(&self) -> bool {
        self.used >= 99.95
    }
}

/// A named pool of windows. Claude and Codex have a single unnamed pool;
/// Antigravity reports one pool per model group.
#[derive(Clone, Debug, PartialEq)]
pub struct Pool {
    pub name: String,
    pub windows: Vec<UsageWindow>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub pools: Vec<Pool>,
    /// Where the login came from, e.g. "Claude 桌面版登录". Never a secret.
    pub source: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Failure {
    /// No local login was found for this service.
    NoCredentials,
    /// A local login exists but its access token has expired.
    TokenExpired,
    /// The service rejected the token (401/403).
    AuthRejected(u16),
    /// The account is not entitled to this usage limit.
    NoPermission,
    Network(String),
    RateLimited(Option<u64>),
    Http(u16),
    /// The endpoint cannot be read safely for this account.
    Unsupported(String),
    BadResponse,
}

impl Failure {
    pub fn title(&self) -> String {
        match self {
            Failure::NoCredentials => "未找到登录".into(),
            Failure::TokenExpired => "登录已过期".into(),
            Failure::AuthRejected(_) => "登录失效".into(),
            Failure::NoPermission => "没有权限".into(),
            Failure::Network(_) => "网络错误".into(),
            Failure::RateLimited(_) => "请求受限".into(),
            Failure::Http(code) => format!("服务异常 {code}"),
            Failure::Unsupported(_) => "暂不支持".into(),
            Failure::BadResponse => "数据无法识别".into(),
        }
    }

    pub fn hint(&self, provider: ProviderId) -> String {
        let app = match provider {
            ProviderId::Claude => "Claude 桌面版或 Claude Code",
            ProviderId::Codex => "Codex",
            ProviderId::Antigravity => "Antigravity",
        };
        match self {
            Failure::NoCredentials => format!("请先在 {app} 中登录"),
            Failure::TokenExpired => format!("打开 {app} 用一下即可自动续期"),
            Failure::AuthRejected(_) => format!("请在 {app} 中重新登录"),
            Failure::NoPermission => "当前订阅无此额度".into(),
            Failure::Network(_) => "检查网络或代理，稍后自动重试".into(),
            Failure::RateLimited(Some(s)) => format!("服务端限流，约 {} 分钟后重试", (s + 59) / 60),
            Failure::RateLimited(None) => "服务端限流，稍后自动重试".into(),
            Failure::Http(_) => "服务端暂时不可用，稍后自动重试".into(),
            Failure::Unsupported(why) => why.clone(),
            Failure::BadResponse => "接口格式可能已变化".into(),
        }
    }

    /// Errors that a later poll may fix on its own.
    pub fn is_transient(&self) -> bool {
        matches!(self, Failure::Network(_) | Failure::RateLimited(_) | Failure::Http(_))
    }
}

#[derive(Clone, Debug, Default)]
pub struct ProviderState {
    pub last_ok: Option<(SystemTime, Snapshot)>,
    pub last_error: Option<(SystemTime, Failure)>,
    pub in_flight: bool,
    /// Do not poll before this instant (server cooldown).
    pub cooldown_until: Option<SystemTime>,
}

impl ProviderState {
    /// The error that should currently be shown, if it is newer than the data.
    pub fn current_error(&self) -> Option<&Failure> {
        match (&self.last_ok, &self.last_error) {
            (_, None) => None,
            (None, Some((_, e))) => Some(e),
            (Some((ok, _)), Some((err, e))) if err >= ok => Some(e),
            _ => None,
        }
    }
}
