use std::path::PathBuf;

fn env_dir(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from)
}

pub fn home_dir() -> Option<PathBuf> {
    env_dir("USERPROFILE").or_else(|| env_dir("HOME"))
}

pub fn roaming_app_data_dir() -> Option<PathBuf> {
    env_dir("APPDATA")
}

pub fn local_app_data_dir() -> Option<PathBuf> {
    env_dir("LOCALAPPDATA")
}

/// %APPDATA%\AI-Usage-Lite — outside any sync folder. Holds only settings.
pub fn config_dir() -> Option<PathBuf> {
    roaming_app_data_dir().map(|p| p.join("AI-Usage-Lite"))
}

/// Settings folder of pre-release builds named "AI Quota Lite"; read once
/// as a fallback so an earlier consent and settings carry over.
pub fn legacy_config_dir() -> Option<PathBuf> {
    roaming_app_data_dir().map(|p| p.join("AI-Quota-Lite"))
}

/// %LOCALAPPDATA%\AI-Usage-Lite — diagnose reports, written only on demand.
pub fn report_dir() -> Option<PathBuf> {
    local_app_data_dir().map(|p| p.join("AI-Usage-Lite"))
}
