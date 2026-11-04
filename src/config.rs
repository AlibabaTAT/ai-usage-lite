//! Settings in %APPDATA%\AI-Usage-Lite\config.json. Never contains secrets.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    /// Claude, Codex, Antigravity.
    pub enabled: [bool; 3],
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
}

impl Default for Config {
    fn default() -> Self {
        Config {
            enabled: [true, true, true],
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
        }
    }
}

pub const REFRESH_CHOICES: [u32; 5] = [2, 5, 10, 15, 30];

impl Config {
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
