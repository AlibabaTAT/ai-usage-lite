//! Open the desktop client for a service.
//!
//! Clients are looked up by display name in the Start menu's app list
//! (shell:AppsFolder), so Store/MSIX and classic installs both work and no
//! install path is hard-coded.

use std::sync::OnceLock;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::GENERIC_READ;
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICImagingFactory, WICBitmapInterpolationModeFant, WICConvertBitmapSource,
    WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
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
        // DeepSeek Harness desktop (productName in its electron-builder config).
        ProviderId::DeepSeek => &["DeepSeek Harness", "DSH"],
    }
}

pub fn client_label(id: ProviderId) -> &'static str {
    match id {
        ProviderId::Claude => "Claude",
        ProviderId::Codex => "ChatGPT",
        ProviderId::Antigravity => "Antigravity",
        ProviderId::DeepSeek => "DeepSeek Harness（DSH）",
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

/// A service icon: premultiplied BGRA, square, `px` wide.
pub struct AppIcon {
    pub px: u32,
    pub bgra: Vec<u8>,
}

static CUSTOM_ICONS: OnceLock<[Option<AppIcon>; crate::model::PROVIDER_COUNT]> = OnceLock::new();

/// File name of an optional user-supplied icon in
/// `%APPDATA%\AI-Usage-Lite\icons\` (PNG, ICO, JPG or BMP; square works best).
pub fn icon_file(id: ProviderId) -> &'static str {
    match id {
        ProviderId::Claude => "claude.png",
        ProviderId::Codex => "codex.png",
        ProviderId::Antigravity => "antigravity.png",
        ProviderId::DeepSeek => "deepseek.png",
    }
}

/// The user's own icon for a service, once loaded. None: use the built-in tile.
pub fn custom_icon(id: ProviderId) -> Option<&'static AppIcon> {
    CUSTOM_ICONS.get().and_then(|a| a[id.index()].as_ref())
}

/// Decodes any user-supplied icons once, on a short-lived thread, then
/// calls `done` so the UI can repaint. No files: nothing is decoded.
pub fn preload_icons(done: impl FnOnce() + Send + 'static) {
    if CUSTOM_ICONS.get().is_some() {
        return;
    }
    let Some(dir) = crate::paths::config_dir().map(|d| d.join("icons")) else { return };
    if !dir.is_dir() {
        let _ = CUSTOM_ICONS.set(Default::default());
        return;
    }
    std::thread::spawn(move || {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        let icons = ProviderId::ALL.map(|id| {
            let path = dir.join(icon_file(id));
            if path.is_file() { unsafe { decode_icon(&path, 96) } } else { None }
        });
        let _ = CUSTOM_ICONS.set(icons);
        done();
    });
}

/// Decodes an image file with Windows Imaging Component and scales it to
/// `px` × `px` premultiplied BGRA.
unsafe fn decode_icon(path: &std::path::Path, px: u32) -> Option<AppIcon> {
    let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
    let wide: Vec<u16> = path.as_os_str().to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
    let decoder = wic
        .CreateDecoderFromFilename(PCWSTR(wide.as_ptr()), None, GENERIC_READ, WICDecodeMetadataCacheOnDemand)
        .ok()?;
    let frame = decoder.GetFrame(0).ok()?;
    let scaler = wic.CreateBitmapScaler().ok()?;
    scaler.Initialize(&frame, px, px, WICBitmapInterpolationModeFant).ok()?;
    let converted = WICConvertBitmapSource(&GUID_WICPixelFormat32bppPBGRA, &scaler).ok()?;
    let mut bgra = vec![0u8; (px * px * 4) as usize];
    converted.CopyPixels(std::ptr::null(), px * 4, &mut bgra).ok()?;
    Some(AppIcon { px, bgra })
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
        let dsh = vec![("DeepSeek Harness".to_string(), "com.deepseek.harness".to_string())];
        assert_eq!(best_match(&dsh, client_names(ProviderId::DeepSeek)), Some("com.deepseek.harness"));
    }
    #[test]
    fn prefix_fallback_skips_uninstallers() {
        let apps = vec![("Uninstall ChatGPT".to_string(), "u".to_string()), ("ChatGPT (Beta)".to_string(), "b".to_string())];
        assert_eq!(best_match(&apps, &["ChatGPT"]), Some("b"));
    }
}
