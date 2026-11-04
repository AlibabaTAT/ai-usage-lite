//! Open the desktop client for a service.
//!
//! Clients are looked up by display name in the Start menu's app list
//! (shell:AppsFolder), so Store/MSIX and classic installs both work and no
//! install path is hard-coded.

use windows::core::{w, PCWSTR};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{
    BHID_EnumItems, IEnumShellItems, IShellItem, SHCreateItemFromParsingName, ShellExecuteW, SIGDN_NORMALDISPLAY,
    SIGDN_PARENTRELATIVEPARSING,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use crate::model::ProviderId;

/// Start-menu display names, most specific first.
pub fn client_names(id: ProviderId) -> &'static [&'static str] {
    match id {
        ProviderId::Claude => &["Claude"],
        ProviderId::Codex => &["ChatGPT"],
        ProviderId::Antigravity => &["Antigravity"],
    }
}

pub fn client_label(id: ProviderId) -> &'static str {
    match id {
        ProviderId::Claude => "Claude",
        ProviderId::Codex => "ChatGPT",
        ProviderId::Antigravity => "Antigravity",
    }
}

/// Picks the best match: exact name first, then a name starting with the
/// wanted word (e.g. "ChatGPT (Beta)").
pub fn best_match<'a>(apps: &'a [(String, String)], names: &[&str]) -> Option<&'a str> {
    for want in names {
        if let Some((_, id)) = apps.iter().find(|(n, _)| n.eq_ignore_ascii_case(want)) {
            return Some(id);
        }
    }
    for want in names {
        let lw = want.to_lowercase();
        if let Some((_, id)) = apps.iter().find(|(n, _)| {
            let ln = n.to_lowercase();
            ln.starts_with(&lw) && !ln.contains("uninstall") && !ln.contains("卸载")
        }) {
            return Some(id);
        }
    }
    None
}

unsafe fn take_string(p: windows::core::PWSTR) -> String {
    let s = p.to_string().unwrap_or_default();
    CoTaskMemFree(Some(p.0 as *const _));
    s
}

/// (display name, AppUserModelID or parsing name) of every Start-menu app.
fn start_apps() -> Vec<(String, String)> {
    let mut out = Vec::new();
    unsafe {
        let Ok(folder) = SHCreateItemFromParsingName::<_, _, IShellItem>(w!("shell:AppsFolder"), None) else {
            return out;
        };
        let Ok(items) = folder.BindToHandler::<_, IEnumShellItems>(None, &BHID_EnumItems) else {
            return out;
        };
        loop {
            let mut batch: [Option<IShellItem>; 1] = [None];
            let mut fetched = 0u32;
            if items.Next(&mut batch, Some(&mut fetched)).is_err() || fetched == 0 {
                break;
            }
            let Some(item) = batch[0].take() else { break };
            let (Ok(name), Ok(id)) = (item.GetDisplayName(SIGDN_NORMALDISPLAY), item.GetDisplayName(SIGDN_PARENTRELATIVEPARSING)) else {
                continue;
            };
            out.push((take_string(name), take_string(id)));
        }
    }
    out
}

/// Launches the client. Err carries a user-facing message.
pub fn open(id: ProviderId) -> Result<(), String> {
    let apps = start_apps();
    let Some(app_id) = best_match(&apps, client_names(id)) else {
        return Err(format!("未在开始菜单中找到 {} 客户端", client_label(id)));
    };
    let target: Vec<u16> = format!("shell:AppsFolder\\{app_id}").encode_utf16().chain(std::iter::once(0)).collect();
    let r = unsafe { ShellExecuteW(None, w!("open"), PCWSTR(target.as_ptr()), None, None, SW_SHOWNORMAL) };
    if r.0 as isize > 32 {
        Ok(())
    } else {
        Err(format!("无法启动 {} 客户端（错误 {}）", client_label(id), r.0 as isize))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_names_win_over_prefixes() {
        let apps = vec![
            ("Claude Code Helper".to_string(), "x.helper".to_string()),
            ("Claude".to_string(), "Claude_abc!Claude".to_string()),
            ("ChatGPT".to_string(), "OpenAI.ChatGPT!App".to_string()),
        ];
        assert_eq!(best_match(&apps, client_names(ProviderId::Claude)), Some("Claude_abc!Claude"));
        assert_eq!(best_match(&apps, client_names(ProviderId::Codex)), Some("OpenAI.ChatGPT!App"));
        assert_eq!(best_match(&apps, client_names(ProviderId::Antigravity)), None);
    }
    #[test]
    fn prefix_fallback_skips_uninstallers() {
        let apps = vec![("Uninstall ChatGPT".to_string(), "u".to_string()), ("ChatGPT (Beta)".to_string(), "b".to_string())];
        assert_eq!(best_match(&apps, &["ChatGPT"]), Some("b"));
    }
}
