pub mod antigravity;
pub mod claude;
mod claude_desktop;
mod claude_limits;
pub mod codex;
pub mod compat;
pub mod deepseek;
mod wincred;

use std::time::{Duration, SystemTime};

use crate::config::Config;
use crate::model::{Failure, Pool, ProviderId, UsageWindow, Snapshot};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Live,
    /// Fixed sample data; no file or network access at all.
    Demo,
    /// Sample data that walks through every error state.
    DemoErrors,
}

pub fn poll(id: ProviderId, mode: Mode, config: &Config) -> Result<Snapshot, Failure> {
    match mode {
        Mode::Live => match id {
            ProviderId::Claude => claude::poll(),
            ProviderId::Codex => codex::poll(),
            ProviderId::Antigravity => antigravity::poll(config.antigravity_memory_refresh),
            ProviderId::DeepSeek => deepseek::poll(),
        },
        Mode::Demo => {
            std::thread::sleep(Duration::from_millis(350 + 250 * id.index() as u64));
            Ok(demo(id))
        }
        Mode::DemoErrors => {
            std::thread::sleep(Duration::from_millis(300));
            match id {
                ProviderId::Claude => Err(Failure::TokenExpired),
                ProviderId::Codex => Err(Failure::Network("connection timed out".into())),
                ProviderId::Antigravity => Err(Failure::Unsupported("此账号暂不支持只读查询".into())),
                ProviderId::DeepSeek => Err(Failure::NoCredentials),
            }
        }
    }
}

fn hours(h: f64) -> Option<SystemTime> {
    Some(SystemTime::now() + Duration::from_secs_f64(h * 3600.0))
}

pub fn demo(id: ProviderId) -> Snapshot {
    let w = |label: &str, used: f64, h: f64| UsageWindow { label: label.into(), used, resets_at: hours(h), recent_rate: None };
    match id {
        ProviderId::Claude => Snapshot {
            pools: vec![Pool { name: String::new(), windows: vec![w("5 小时", 25.0, 3.55), w("本周", 23.0, 101.2)] }],
            source: "演示数据".into(),
            balance: None,
        },
        ProviderId::Codex => Snapshot {
            // ChatGPT Plus currently reports only a weekly window for Codex.
            pools: vec![Pool { name: String::new(), windows: vec![w("本周", 41.0, 125.0)] }],
            source: "演示数据".into(),
            balance: None,
        },
        ProviderId::Antigravity => Snapshot {
            pools: vec![
                Pool { name: "Gemini Models".into(), windows: vec![w("5 小时", 4.0, 4.98), w("本周", 100.0, 52.0)] },
                Pool { name: "Claude and GPT models".into(), windows: vec![w("5 小时", 0.0, 4.98), w("本周", 0.0, 120.0)] },
                Pool { name: "Gemini 4 Argon".into(), windows: vec![w("5 小时", 0.0, 4.98)] },
            ],
            source: "演示数据".into(),
            balance: None,
        },
        ProviderId::DeepSeek => Snapshot {
            pools: Vec::new(),
            source: "演示数据".into(),
            balance: Some(crate::model::Balance { currency: "CNY".into(), total: 86.4,
                granted: 0.0,
                topped_up: 86.4,
                available: true,
                spend: Some(crate::spend::Spend { today: 1.26, days_left: Some(23.4) }),
            }),
        },
    }
}
