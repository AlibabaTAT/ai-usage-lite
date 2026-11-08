//! Settings in %APPDATA%\AI-Usage-Lite\config.json. Never contains secrets.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    /// Claude, Codex, Antigravity, DeepSeek. A list so older files with
    /// fewer services still load.
    pub enabled: Vec<bool>,
    /// DeepSeek balance below this many yuan shows in red.
    pub deepseek_low_balance: f64,
    pub refresh_minutes: u32,
    /// Bars and numbers show used share instead of remaining share.
    pub show_used: bool,
    pub compact: bool,
    /// Let Antigravity exchange its refresh token for an access token in
    /// memory when the stored one has expired. Off until the user opts in.
    pub antigravity_memory_refresh: bool,
    pub antigravity_hidden_pools: Vec<String>,
    /// Pools seen at least once, so first-seen defaults apply only once.
    pub antigravity_seen_pools: Vec<String>,
    /// The user accepted the read scope shown on first launch.
    pub consented: bool,
    /// Show the edge-docked floating strip (the tray icon always stays).
    pub dock: bool,
    /// Strip docked to the right (true) or left screen edge.
    pub dock_right: bool,
    /// Vertical position as a fraction of the work area height.
    pub dock_y: f32,
    /// Last strip centre in screen pixels, to pick the monitor.
    pub dock_anchor: Option<(i32, i32)>,
    /// Show the burn-rate forecast under each window.
    pub forecast: bool,
    /// Append changed readings to the long-term usage log.
    pub usage_log: bool,
    /// Folder of the usage log; empty = %LOCALAPPDATA%\AI-Usage-Lite\usage-log.
    pub usage_log_dir: Option<String>,
    /// Monthly subscription fees, copied into the log for later reports.
    pub subscriptions: Vec<Subscription>,
    /// Exchange rate the user wants reports to use (not fetched online).
    pub usd_to_cny: Option<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Subscription {
    pub service: String,
    pub plan: String,
    pub monthly_fee: f64,
    pub currency: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            // Padded to every service by `load` (DeepSeek on only with a key).
            enabled: vec![true, true, true],
            deepseek_low_balance: 10.0,
            refresh_minutes: 5,
            show_used: false,
            compact: false,
            antigravity_memory_refresh: false,
            antigravity_hidden_pools: Vec::new(),
            antigravity_seen_pools: Vec::new(),
            consented: false,
            dock: true,
            dock_right: true,
            dock_y: 0.62,
            dock_anchor: None,
            forecast: true,
            usage_log: true,
            usage_log_dir: None,
            subscriptions: Vec::new(),
            usd_to_cny: None,
        }
    }
}

pub const REFRESH_CHOICES: [u32; 5] = [2, 5, 10, 15, 30];

impl Config {
    pub fn is_enabled(&self, id: crate::model::ProviderId) -> bool {
        self.enabled.get(id.index()).copied().unwrap_or(false)
    }
    pub fn load() -> Config {
        let Some(path) = crate::paths::config_dir().map(|d| d.join("config.json")) else {
            return Config::default();
        };
        let legacy = crate::paths::legacy_config_dir().map(|d| d.join("config.json"));
        let mut cfg: Config = std::fs::read_to_string(&path)
            .ok()
            .or_else(|| legacy.and_then(|l| std::fs::read_to_string(l).ok()))
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        // Services added after the file was written: Claude/Codex/Antigravity
        // default on; DeepSeek only if an API key is already configured.
        while cfg.enabled.len() < crate::model::PROVIDER_COUNT {
            let idx = cfg.enabled.len();
            cfg.enabled.push(if idx == crate::model::ProviderId::DeepSeek.index() { crate::providers::deepseek::key_present() } else { true });
        }
        if !REFRESH_CHOICES.contains(&cfg.refresh_minutes) {
            cfg.refresh_minutes = 5;
        }
        cfg
    }

    pub fn save(&self) {
        let Some(dir) = crate::paths::config_dir() else { return };
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        let tmp = dir.join("config.json.tmp");
        if let Ok(json) = serde_json::to_string_pretty(self) {
            if std::fs::write(&tmp, json).is_ok() {
                let _ = std::fs::rename(&tmp, dir.join("config.json"));
            }
        }
    }

    pub fn pool_visible(&self, name: &str) -> bool {
        !self.antigravity_hidden_pools.iter().any(|p| p == name)
    }

    /// First sighting of an Antigravity pool. Pools for model families that
    /// are often unavailable on promotional plans (Gemini 4 Argon, Claude and
    /// GPT models) start hidden; they can be shown again in settings.
    /// Returns true if the config changed.
    pub fn note_pools(&mut self, names: &[String]) -> bool {
        let mut changed = false;
        for name in names {
            if self.antigravity_seen_pools.iter().any(|p| p == name) {
                continue;
            }
            self.antigravity_seen_pools.push(name.clone());
            let lower = name.to_ascii_lowercase();
            if lower.contains("argon") || lower.contains("claude") || lower.contains("gpt") {
                self.antigravity_hidden_pools.push(name.clone());
            }
            changed = true;
        }
        changed
    }
}
