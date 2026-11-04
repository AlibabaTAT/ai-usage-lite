//! In-memory diagnostic log. Nothing is written to disk unless the user runs
//! `--diagnose`. Callers must never pass tokens, cookies or keys here.

use std::collections::VecDeque;
use std::sync::Mutex;

static LOG: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());
const CAP: usize = 300;

pub fn log(message: impl Into<String>) {
    if let Ok(mut log) = LOG.lock() {
        if log.len() >= CAP {
            log.pop_front();
        }
        log.push_back(redact(&message.into()));
    }
}

pub fn log_error(context: &str, error: impl std::fmt::Display) {
    log(format!("{context}: {error}"));
}

pub fn is_enabled() -> bool {
    true
}

pub fn take_all() -> Vec<String> {
    LOG.lock().map(|mut l| l.drain(..).collect()).unwrap_or_default()
}

/// Belt and braces: mask anything that looks like a bearer token or key.
fn redact(s: &str) -> String {
    s.split(' ')
        .map(|word| {
            let w = word.trim_matches(|c: char| !c.is_ascii_alphanumeric());
            let looks_secret = w.len() >= 40
                && w.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
                && w.chars().any(|c| c.is_ascii_digit());
            if looks_secret || w.starts_with("sk-ant") || w.starts_with("ya29.") || w.starts_with("GOCSPX-") {
                "[已隐藏]".to_string()
            } else {
                word.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
