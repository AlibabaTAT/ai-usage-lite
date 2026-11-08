#![windows_subsystem = "windows"]
//! AI Usage Lite — tray monitor for Claude / Codex / Antigravity usage limits.
//!
//! Usage:
//!   AI-Usage-Lite.exe              normal (live) mode
//!   AI-Usage-Lite.exe --demo       sample data, no credential or network access
//!   AI-Usage-Lite.exe --demo-errors  sample error states
//!   AI-Usage-Lite.exe --diagnose   one read-only query per service, writes a
//!                                  redacted report and exits

mod config;
mod diag;
mod forecast;
mod http;
mod model;
mod paths;
mod providers;
mod scheduler;
mod spend;
mod ui;
mod usagelog;

use providers::Mode;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, LPARAM, WAIT_ABANDONED, WAIT_OBJECT_0, WPARAM};
use windows::Win32::System::Threading::{CreateMutexW, WaitForSingleObject};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, FindWindowW, GetWindowThreadProcessId, MessageBoxW, PostMessageW, SendMessageTimeoutW, IDOK,
    MB_ICONINFORMATION, MB_OK, MB_OKCANCEL, SMTO_ABORTIFHUNG, WM_CLOSE,
};

fn message(text: &str, flags: windows::Win32::UI::WindowsAndMessaging::MESSAGEBOX_STYLE) -> bool {
    let t: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe { MessageBoxW(None, PCWSTR(t.as_ptr()), w!("AI Usage Lite"), flags) == IDOK }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let has = |flag: &str| args.iter().any(|a| a == flag);

    if has("--diagnose") {
        run_diagnose();
        return;
    }

    let mode = if has("--demo-errors") {
        Mode::DemoErrors
    } else if has("--demo") {
        Mode::Demo
    } else {
        Mode::Live
    };

    // Single instance. Starting a newer version replaces the running copy
    // (so an update takes effect without quitting from the tray first);
    // otherwise a second launch just opens the existing panel.
    unsafe {
        let Ok(_mutex) = CreateMutexW(None, false, w!("Local\\AIUsageLite.Instance")) else { return };
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let Ok(hwnd) = FindWindowW(ui::window::CLASS_NAME, None) else { return };
            let mut reply = 0usize;
            let ok = SendMessageTimeoutW(
                hwnd,
                ui::window::WM_APP_HANDOFF,
                WPARAM(ui::window::version_code()),
                LPARAM(0),
                SMTO_ABORTIFHUNG,
                2000,
                Some(&mut reply),
            );
            let replace = match reply {
                1 => true,
                2 => false,
                // Builds before 0.4.1 don't know the message: they are older.
                _ if ok.0 != 0 => {
                    let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                    true
                }
                _ => false,
            };
            if !replace {
                let mut pid = 0u32;
                GetWindowThreadProcessId(hwnd, Some(&mut pid));
                let _ = AllowSetForegroundWindow(pid);
                let _ = PostMessageW(Some(hwnd), ui::window::WM_APP_SHOW, WPARAM(0), LPARAM(0));
                return;
            }
            // Wait for the old copy to exit and release the mutex.
            let r = WaitForSingleObject(_mutex, 5000);
            if r != WAIT_OBJECT_0 && r != WAIT_ABANDONED {
                message("旧版本未能退出，请右键托盘图标选择「退出」后再启动新版本。", MB_OK | MB_ICONINFORMATION);
                return;
            }
        } else if WaitForSingleObject(_mutex, 0) != WAIT_OBJECT_0 {
            return;
        }
        if let Err(e) = ui::window::run(mode) {
            message(&format!("启动失败：{e}"), MB_OK);
        }
        std::mem::forget(_mutex);
    }
}

fn run_diagnose() {
    let ok = message(
        "诊断将对 Claude、Codex、Antigravity 各做一次只读额度查询，\n\
         并在已设置 API Key 时查询一次 DeepSeek 余额：\n\n\
         · 读取本机已有登录（不修改、不刷新、不写回）\n\
         · 只访问官方额度接口，不调用任何模型\n\
         · 报告不含令牌，保存到 %LOCALAPPDATA%\\AI-Usage-Lite\n\n继续吗？",
        MB_OKCANCEL | MB_ICONINFORMATION,
    );
    if !ok {
        return;
    }
    let cfg = config::Config::load();
    let mut report = String::new();
    report.push_str("AI Usage Lite 诊断报告（不含令牌）\n");
    report.push_str(&format!("代理：{}\n\n", http::proxy_url().unwrap_or_else(|| "无（直连）".into())));
    let mut summary = Vec::new();
    for id in model::ProviderId::ALL {
        let start = std::time::Instant::now();
        let result = providers::poll(id, Mode::Live, &config::Config { antigravity_memory_refresh: cfg.antigravity_memory_refresh, ..Default::default() });
        report.push_str(&format!("==== {} ({} ms) ====\n", id.name(), start.elapsed().as_millis()));
        for line in diag::take_all() {
            report.push_str(&format!("  {line}\n"));
        }
        match &result {
            Ok(snap) => {
                report.push_str(&format!("  结果：成功（来源：{}）\n", snap.source));
                for pool in &snap.pools {
                    let name = if pool.name.is_empty() { "-" } else { &pool.name };
                    for w in &pool.windows {
                        report.push_str(&format!(
                            "    [{name}] {}：已用 {:.1}% / 剩余 {:.1}%，重置 {}\n",
                            w.label,
                            w.used,
                            w.remaining(),
                            w.resets_at.map(|t| format!("{}（{}）", ui::fmt::clock(t), ui::fmt::reset(Some(t), std::time::SystemTime::now()))).unwrap_or_else(|| "未提供".into())
                        ));
                    }
                }
                if let Some(b) = &snap.balance {
                    report.push_str(&format!(
                        "    余额 {} {:.2}（充值 {:.2}，赠金 {:.2}，可用：{}）\n",
                        b.currency, b.total, b.topped_up, b.granted, b.available
                    ));
                    // Read-only: the stored readings plus this one, not saved.
                    let mut h = spend::load();
                    let now = std::time::SystemTime::now();
                    h.record(spend::unix(now), &b.currency, b.total);
                    let s = h.spend(spend::unix(now), spend::local_midnight(now), b.total);
                    let days = match s.days_left {
                        Some(d) => format!("约 {d:.1} 天后用完"),
                        None => "读数不足一天或近期无消耗，暂不预估".into(),
                    };
                    report.push_str(&format!("    今日已用 {:.2}，{days}（本地读数 {} 条）\n", s.today, h.readings.len()));
                }
                summary.push(format!("{}：成功", id.name()));
            }
            Err(e) => {
                report.push_str(&format!("  结果：{}（{:?}）\n", e.title(), e));
                summary.push(format!("{}：{}", id.name(), e.title()));
            }
        }
        report.push('\n');
    }
    let path = paths::report_dir().map(|d| {
        let _ = std::fs::create_dir_all(&d);
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
        d.join(format!("diagnose-{t}.txt"))
    });
    let saved = path.as_ref().map(|p| std::fs::write(p, report.as_bytes()).is_ok()).unwrap_or(false);
    message(
        &format!(
            "{}\n\n{}",
            summary.join("\n"),
            if saved { format!("报告：{}", path.unwrap().display()) } else { "报告保存失败".into() }
        ),
        MB_OK | MB_ICONINFORMATION,
    );
}
