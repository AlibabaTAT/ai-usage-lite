//! Popup window, tray integration and message loop.

use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use windows::core::{w, Result, PCSTR, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows::Win32::System::ProcessStatus::EmptyWorkingSet;
use windows::Win32::System::Threading::GetCurrentProcess;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, GetDpiForSystem, GetSystemMetricsForDpi, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_ESCAPE, VK_F5};
use windows::Win32::UI::Shell::{Shell_NotifyIconGetRect, NOTIFYICONIDENTIFIER};
use windows::Win32::UI::WindowsAndMessaging::*;

use super::dock::{Dock, Drag};
use super::gfx::{Gfx, Painter, Rect, Target};
use super::theme::{self, Palette, DARK, LIGHT};
use super::tray::{self, Bar, Tray};
use super::view::{self, Action, Page};
use crate::config::Config;
use crate::model::{ProviderId, ProviderState};
use crate::providers::Mode;
use crate::scheduler::{Scheduler, SharedConfig, States};

pub const CLASS_NAME: PCWSTR = w!("AIUsageLite.Panel");
const DOCK_CLASS: PCWSTR = w!("AIUsageLite.Dock");
const TIMER_DOCK_ANIM: usize = 11;
const TIMER_DOCK_COLLAPSE: usize = 12;
const DRAG_THRESHOLD: i32 = 4;
static APP: AtomicPtr<App> = AtomicPtr::new(std::ptr::null_mut());
pub const WM_APP_TRAY: u32 = WM_APP + 1;
pub const WM_APP_UPDATE: u32 = WM_APP + 2;
pub const WM_APP_SHOW: u32 = WM_APP + 3;
const TIMER_TICK: usize = 1;
const TIMER_RESUME: usize = 2;
const MENU_REFRESH: usize = 1;
const MENU_SETTINGS: usize = 2;
const MENU_QUIT: usize = 3;
const MARGIN_DIP: f32 = 12.0;
const WM_MOUSELEAVE: u32 = 0x02A3;

struct App {
    hwnd: HWND,
    mode: Mode,
    states: States,
    config: SharedConfig,
    scheduler: Option<Scheduler>,
    gfx: Gfx,
    target: Option<Target>,
    page: Page,
    hits: Vec<(Rect, Action)>,
    hover: Option<Action>,
    visible: bool,
    last_hide: Instant,
    dpi: u32,
    /// Screen edge the panel hangs from, in pixels, for re-anchoring on resize.
    anchor: Anchor,
    size_px: (i32, i32),
    dark: bool,
    taskbar_dark: bool,
    tray: Tray,
    taskbar_created: u32,
    tracking: bool,
    dock: Option<Dock>,
    /// The panel was opened from the floating strip (positions it beside it).
    from_dock: bool,
}

#[derive(Clone, Copy)]
enum Anchor {
    Bottom(i32),
    Top(i32),
}

impl App {
    fn palette(&self) -> &'static Palette {
        if self.dark { &DARK } else { &LIGHT }
    }

    fn scale(&self) -> f32 {
        self.dpi as f32 / 96.0
    }

    fn cfg(&self) -> Config {
        self.config.read().map(|c| c.clone()).unwrap_or_default()
    }

    fn snapshot_states(&self) -> [ProviderState; 3] {
        self.states.lock().map(|s| s.clone()).unwrap_or_default()
    }

    fn start_scheduler(&mut self) {
        if self.scheduler.is_some() {
            return;
        }
        let hwnd = self.hwnd.0 as isize;
        let notify: Arc<dyn Fn() + Send + Sync> = Arc::new(move || unsafe {
            let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_APP_UPDATE, WPARAM(0), LPARAM(0));
        });
        self.scheduler = Some(Scheduler::start(self.mode, self.states.clone(), self.config.clone(), notify));
    }

    fn layout_height(&self) -> f32 {
        let cfg = self.cfg();
        let states = self.snapshot_states();
        let input = view::Input {
            pal: self.palette(),
            cfg: &cfg,
            states: &states,
            now: SystemTime::now(),
            demo: self.mode != Mode::Live,
            page: self.page,
            hover: None,
        };
        view::draw(&Painter { gfx: &self.gfx, target: None }, &input).height
    }

    fn paint(&mut self) {
        let Some(target) = self.target.as_ref() else { return };
        let cfg = self.cfg();
        let states = self.snapshot_states();
        let input = view::Input {
            pal: self.palette(),
            cfg: &cfg,
            states: &states,
            now: SystemTime::now(),
            demo: self.mode != Mode::Live,
            page: self.page,
            hover: self.hover.as_ref(),
        };
        unsafe { target.rt.BeginDraw() };
        let out = view::draw(&Painter { gfx: &self.gfx, target: Some(target) }, &input);
        let result = unsafe { target.rt.EndDraw(None, None) };
        self.hits = out.hits;
        if let Err(e) = result {
            if e.code() == D2DERR_RECREATE_TARGET {
                self.target = None;
                self.ensure_target();
                unsafe {
                    let _ = InvalidateRect(Some(self.hwnd), None, false);
                }
            }
        }
    }

    fn ensure_target(&mut self) {
        if self.target.is_none() {
            self.target = self.gfx.create_target(self.hwnd, self.size_px.0 as u32, self.size_px.1 as u32, self.dpi as f32).ok();
        }
    }

    fn work_area(&self) -> (RECT, RECT, u32, Option<RECT>) {
        unsafe {
            let id = NOTIFYICONIDENTIFIER {
                cbSize: std::mem::size_of::<NOTIFYICONIDENTIFIER>() as u32,
                hWnd: self.hwnd,
                uID: tray::TRAY_ID,
                ..Default::default()
            };
            let icon = Shell_NotifyIconGetRect(&id).ok();
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            if let Some(r) = icon {
                pt = POINT { x: (r.left + r.right) / 2, y: (r.top + r.bottom) / 2 };
            }
            let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
            let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            let _ = GetMonitorInfoW(mon, &mut info);
            let (mut dx, mut dy) = (96u32, 96u32);
            let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
            (info.rcWork, info.rcMonitor, dx.max(96), icon)
        }
    }

    fn show(&mut self, page: Page) {
        let (work, mon, dpi, icon) = self.work_area();
        self.dpi = dpi;
        self.page = page;
        self.hover = None;
        self.from_dock = false;
        let s = self.scale();
        let margin = (MARGIN_DIP * s).round() as i32;
        let w = (view::page_width(page) * s).round() as i32;
        let max_h = (work.bottom - work.top) - 2 * margin;
        let h = ((self.layout_height() * s).round() as i32).min(max_h);
        // Taskbar edge: wherever the work area is inset from the monitor.
        let x = match icon {
            Some(r) => ((r.left + r.right) / 2 - w / 2).min(work.right - w - margin).max(work.left + margin),
            None => work.right - w - margin,
        };
        let (x, y, anchor) = if work.top > mon.top {
            (x, work.top + margin, Anchor::Top(work.top + margin))
        } else if work.left > mon.left {
            (work.left + margin, work.bottom - h - margin, Anchor::Bottom(work.bottom - margin))
        } else if work.right < mon.right {
            (work.right - w - margin, work.bottom - h - margin, Anchor::Bottom(work.bottom - margin))
        } else {
            (x, work.bottom - h - margin, Anchor::Bottom(work.bottom - margin))
        };
        self.present(x, y, w, h, anchor);
    }

    /// Opens the panel beside the floating strip.
    fn show_from_dock(&mut self, page: Page) {
        let Some(dock) = self.dock.as_ref() else { return self.show(page) };
        let cap = dock.expanded_screen_rect();
        let right = dock.right;
        let center = POINT { x: (cap.left + cap.right) / 2, y: (cap.top + cap.bottom) / 2 };
        let (work, dpi) = Dock::monitor_at(center);
        self.dpi = dpi;
        self.page = page;
        self.hover = None;
        self.from_dock = true;
        let s = self.scale();
        let margin = (MARGIN_DIP * s).round() as i32;
        let gap = (8.0 * s).round() as i32;
        let w = (view::page_width(page) * s).round() as i32;
        let max_h = (work.bottom - work.top) - 2 * margin;
        let h = ((self.layout_height() * s).round() as i32).min(max_h);
        let x = if right { cap.left - gap - w } else { cap.right + gap };
        let y = (center.y - h / 2).min(work.bottom - margin - h).max(work.top + margin);
        self.present(x, y, w, h, Anchor::Top(y));
    }

    fn present(&mut self, x: i32, y: i32, w: i32, h: i32, anchor: Anchor) {
        self.anchor = anchor;
        self.size_px = (w, h);
        unsafe {
            let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_SHOWWINDOW);
            let _ = SetForegroundWindow(self.hwnd);
        }
        self.target = None;
        self.ensure_target();
        self.visible = true;
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_TICK, 30_000, None);
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn hide(&mut self) {
        if !self.visible {
            return;
        }
        self.visible = false;
        self.last_hide = Instant::now();
        self.target = None;
        self.hover = None;
        if self.page == Page::Settings {
            self.page = Page::Main;
        }
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
            let _ = KillTimer(Some(self.hwnd), TIMER_TICK);
        }
        if self.page == Page::Mini {
            self.page = Page::Main;
        }
        self.schedule_dock_collapse();
        trim_memory();
    }

    fn toggle(&mut self) {
        if self.visible {
            self.hide();
        } else if self.last_hide.elapsed().as_millis() > 250 {
            let page = if self.needs_consent() { Page::Consent } else { Page::Main };
            self.show(page);
        }
    }

    fn needs_consent(&self) -> bool {
        self.mode == Mode::Live && !self.cfg().consented
    }

    /// Content height may change after data or setting updates.
    fn relayout(&mut self) {
        if !self.visible {
            return;
        }
        let s = self.scale();
        let (work, _, _, _) = self.work_area();
        let max_h = (work.bottom - work.top) - 2 * (MARGIN_DIP * s).round() as i32;
        let h = ((self.layout_height() * s).round() as i32).min(max_h);
        if h != self.size_px.1 {
            let mut rc = RECT::default();
            unsafe {
                let _ = GetWindowRect(self.hwnd, &mut rc);
            }
            let margin = (MARGIN_DIP * s).round() as i32;
            let y = match self.anchor {
                Anchor::Bottom(b) => b - h,
                Anchor::Top(t) => t.min(work.bottom - margin - h).max(work.top + margin),
            };
            self.size_px.1 = h;
            unsafe {
                let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), rc.left, y, self.size_px.0, h, SWP_NOACTIVATE);
            }
            if let Some(t) = &self.target {
                t.resize(self.size_px.0 as u32, h as u32, self.dpi as f32);
            }
        }
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn update_config(&mut self, f: impl FnOnce(&mut Config)) {
        let snapshot = if let Ok(mut c) = self.config.write() {
            f(&mut c);
            c.clone()
        } else {
            return;
        };
        snapshot.save();
    }

    fn handle(&mut self, action: Action) {
        match action {
            Action::Refresh => {
                if let Some(s) = &self.scheduler {
                    s.refresh_all();
                }
            }
            Action::SetCompact(v) => self.update_config(|c| c.compact = v),
            Action::OpenSettings => self.page = Page::Settings,
            Action::Back => self.page = Page::Main,
            Action::ToggleProvider(i) => {
                self.update_config(|c| c.enabled[i] = !c.enabled[i]);
                if let Some(s) = &self.scheduler {
                    s.reconfigure();
                }
            }
            Action::SetInterval(m) => {
                self.update_config(|c| c.refresh_minutes = m);
                if let Some(s) = &self.scheduler {
                    s.reconfigure();
                }
            }
            Action::SetShowUsed(v) => self.update_config(|c| c.show_used = v),
            Action::ToggleAgRefresh => {
                self.update_config(|c| c.antigravity_memory_refresh = !c.antigravity_memory_refresh);
                if let Some(s) = &self.scheduler {
                    s.reconfigure();
                }
            }
            Action::TogglePool(name) => self.update_config(|c| {
                if let Some(pos) = c.antigravity_hidden_pools.iter().position(|p| *p == name) {
                    c.antigravity_hidden_pools.remove(pos);
                } else {
                    c.antigravity_hidden_pools.push(name);
                }
            }),
            Action::Consent => {
                self.update_config(|c| c.consented = true);
                self.page = Page::Main;
                self.start_scheduler();
            }
            Action::DemoOnly => {
                self.mode = Mode::Demo;
                self.page = Page::Main;
                self.start_scheduler();
            }
            Action::OpenFull => {
                self.show_from_dock(Page::Main);
                return;
            }
            Action::Launch(i) => {
                self.hide();
                if let Err(msg) = super::launch::open(ProviderId::ALL[i]) {
                    let t: Vec<u16> = msg.encode_utf16().chain(std::iter::once(0)).collect();
                    unsafe {
                        MessageBoxW(None, PCWSTR(t.as_ptr()), w!("AI Usage Lite"), MB_OK | MB_ICONINFORMATION);
                    }
                }
                return;
            }
            Action::ToggleForecast => self.update_config(|c| c.forecast = !c.forecast),
            Action::SetDock(on) => {
                self.update_config(|c| c.dock = on);
                if on { self.create_dock() } else { self.destroy_dock() }
            }
        }
        self.hover = None;
        self.relayout();
    }

    fn tray_state(&self) -> (Vec<Bar>, String) {
        let cfg = self.cfg();
        let states = self.snapshot_states();
        let mut bars = Vec::new();
        let mut lines = Vec::new();
        for id in ProviderId::ALL {
            if !cfg.enabled[id.index()] {
                continue;
            }
            let st = &states[id.index()];
            let windows: Vec<_> = st
                .last_ok
                .iter()
                .flat_map(|(_, s)| s.pools.iter())
                .filter(|p| id != ProviderId::Antigravity || cfg.pool_visible(&p.name))
                .flat_map(|p| p.windows.iter())
                .collect();
            let tight = windows.iter().map(|w| w.remaining()).min_by(f64::total_cmp);
            let err = st.current_error();
            let value = if err.is_some() && !err.is_some_and(|e| e.is_transient()) { None } else { tight };
            bars.push(Bar { id, value });
            let line = match (err, tight) {
                (Some(e), _) if !e.is_transient() || tight.is_none() => format!("{} {}", id.name(), e.title()),
                (_, Some(_)) => {
                    let parts: Vec<String> = windows
                        .iter()
                        .take(2)
                        .map(|w| format!("{} {:.0}%", w.label.replace(' ', ""), if cfg.show_used { w.used } else { w.remaining() }))
                        .collect();
                    format!("{} {}", id.name(), parts.join(" · "))
                }
                _ => format!("{} 等待查询", id.name()),
            };
            lines.push(line);
        }
        let head = if self.mode == Mode::Live { "AI 额度" } else { "AI 额度（演示）" };
        (bars, format!("{head}\n{}", lines.join("\n")))
    }

    fn refresh_tray(&mut self, add: bool) {
        let (bars, tip) = self.tray_state();
        let size = unsafe { GetSystemMetricsForDpi(SM_CXSMICON, GetDpiForSystem()) };
        let Ok(icon) = tray::make_icon(size.max(16), &bars, self.taskbar_dark) else { return };
        if add {
            self.tray.add(icon, &tip);
        } else {
            self.tray.update(icon, &tip);
        }
    }

    fn on_data(&mut self) {
        // Remember Antigravity pools so settings can list them.
        let names: Vec<String> = self
            .states
            .lock()
            .ok()
            .and_then(|s| s[ProviderId::Antigravity.index()].last_ok.as_ref().map(|(_, snap)| snap.pools.iter().map(|p| p.name.clone()).collect()))
            .unwrap_or_default();
        if !names.is_empty() && self.mode == Mode::Live {
            let changed = self.config.write().map(|mut c| c.note_pools(&names)).unwrap_or(false);
            if changed {
                self.cfg().save();
            }
        }
        self.refresh_tray(false);
        self.paint_dock();
        if self.visible {
            self.relayout();
        } else if !self.states.lock().map(|s| s.iter().any(|p| p.in_flight)).unwrap_or(false) {
            trim_memory();
        }
    }

    fn apply_theme(&mut self) {
        self.dark = theme::apps_dark();
        self.taskbar_dark = theme::taskbar_dark();
        apply_dwm(self.hwnd, self.dark);
    }

    fn show_menu(&mut self, x: i32, y: i32) {
        unsafe {
            let Ok(menu) = CreatePopupMenu() else { return };
            let _ = AppendMenuW(menu, MF_STRING, MENU_REFRESH, w!("刷新额度"));
            let _ = AppendMenuW(menu, MF_STRING, MENU_SETTINGS, w!("设置"));
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
            let _ = AppendMenuW(menu, MF_STRING, MENU_QUIT, w!("退出"));
            let _ = SetForegroundWindow(self.hwnd);
            let cmd = TrackPopupMenuEx(menu, (TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY).0, x, y, self.hwnd, None);
            let _ = PostMessageW(Some(self.hwnd), WM_NULL, WPARAM(0), LPARAM(0));
            let _ = DestroyMenu(menu);
            match cmd.0 as usize {
                MENU_REFRESH => self.handle(Action::Refresh),
                MENU_SETTINGS => {
                    let page = if self.needs_consent() { Page::Consent } else { Page::Settings };
                    self.last_hide = Instant::now() - std::time::Duration::from_secs(1);
                    self.show(page)
                }
                MENU_QUIT => {
                    let _ = DestroyWindow(self.hwnd);
                }
                _ => {}
            }
        }
    }

    fn hit_at(&self, lparam: LPARAM) -> Option<Action> {
        let x = (lparam.0 & 0xffff) as i16 as f32 / self.scale();
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as f32 / self.scale();
        self.hits.iter().rev().find(|(r, _)| r.contains(x, y)).map(|(_, a)| a.clone())
    }
}

/// Private bytes of this process in MB (what Task Manager calls 内存).
pub fn private_mb() -> Option<f64> {
    use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX};
    let mut pmc = PROCESS_MEMORY_COUNTERS_EX { cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32, ..Default::default() };
    unsafe {
        GetProcessMemoryInfo(GetCurrentProcess(), &mut pmc as *mut _ as *mut PROCESS_MEMORY_COUNTERS, pmc.cb).ok()?;
    }
    let bytes = if pmc.PrivateUsage > 0 { pmc.PrivateUsage } else { pmc.WorkingSetSize };
    (bytes > 0).then(|| bytes as f64 / 1_048_576.0)
}

// ---------------------------------------------------------------- floating strip

impl App {
    fn create_dock(&mut self) {
        if self.dock.is_some() {
            return;
        }
        let cfg = self.cfg();
        unsafe {
            let Ok(hinstance) = GetModuleHandleW(None) else { return };
            let Ok(hwnd) = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
                DOCK_CLASS,
                w!("AI Usage Lite Dock"),
                WS_POPUP,
                0,
                0,
                10,
                10,
                None,
                None,
                Some(hinstance.into()),
                None,
            ) else {
                return;
            };
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, self as *mut App as isize);
            let mut dock = Dock {
                hwnd,
                progress: 0.0,
                target: 0.0,
                right: cfg.dock_right,
                y_frac: cfg.dock_y,
                hover: false,
                drag: None,
                hidden_for_fullscreen: false,
                dpi: 96,
                rect: RECT::default(),
            };
            let anchor = cfg.dock_anchor.map(|(x, y)| POINT { x, y }).unwrap_or_default();
            dock.place(anchor);
            let (w, h) = dock.size_px();
            let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), dock.rect.left, dock.rect.top, w, h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
            self.dock = Some(dock);
        }
        self.paint_dock();
    }

    fn destroy_dock(&mut self) {
        if let Some(d) = self.dock.take() {
            unsafe {
                let _ = DestroyWindow(d.hwnd);
            }
        }
    }

    /// Monitor or DPI changed: snap again on the same monitor.
    fn replace_dock(&mut self) {
        let Some(d) = self.dock.as_mut() else { return };
        let center = POINT { x: (d.rect.left + d.rect.right) / 2, y: (d.rect.top + d.rect.bottom) / 2 };
        d.place(center);
        let (w, h) = d.size_px();
        unsafe {
            let _ = SetWindowPos(d.hwnd, Some(HWND_TOPMOST), d.rect.left, d.rect.top, w, h, SWP_NOACTIVATE);
        }
        self.paint_dock();
    }

    fn paint_dock(&self) {
        let Some(d) = self.dock.as_ref() else { return };
        if d.hidden_for_fullscreen {
            return;
        }
        let (bars, _) = self.tray_state();
        d.paint(&bars, self.dark);
    }

    fn dock_animate(&mut self, target: f32) {
        let Some(d) = self.dock.as_mut() else { return };
        d.target = target;
        unsafe {
            SetTimer(Some(d.hwnd), TIMER_DOCK_ANIM, 15, None);
        }
    }

    fn schedule_dock_collapse(&self) {
        if let Some(d) = self.dock.as_ref() {
            unsafe {
                SetTimer(Some(d.hwnd), TIMER_DOCK_COLLAPSE, 650, None);
            }
        }
    }

    /// First click: mini card. Second: full panel. Third: close.
    fn dock_click(&mut self) {
        if !self.visible {
            let page = if self.needs_consent() { Page::Consent } else { Page::Mini };
            self.show_from_dock(page);
        } else if self.page == Page::Mini {
            self.show_from_dock(Page::Main);
        } else {
            self.hide();
        }
    }

    fn finish_drag(&mut self, cursor: POINT) {
        let Some(d) = self.dock.as_mut() else { return };
        let (work, _) = Dock::monitor_at(cursor);
        let (_, h) = d.size_px();
        d.right = cursor.x >= (work.left + work.right) / 2;
        let span = ((work.bottom - work.top) - h).max(1) as f32;
        d.y_frac = ((d.rect.top - work.top) as f32 / span).clamp(0.0, 1.0);
        d.place(cursor);
        let (w, h) = d.size_px();
        let (right, y_frac, rect) = (d.right, d.y_frac, d.rect);
        unsafe {
            let _ = SetWindowPos(d.hwnd, Some(HWND_TOPMOST), rect.left, rect.top, w, h, SWP_NOACTIVATE);
        }
        self.update_config(|c| {
            c.dock_right = right;
            c.dock_y = y_frac;
            c.dock_anchor = Some((cursor.x, cursor.y));
        });
        self.paint_dock();
    }

    /// Hide the strip while a fullscreen app (video, slides, game) is in front.
    fn check_fullscreen(&mut self, fg: HWND) {
        let own = fg == self.hwnd || self.dock.as_ref().is_some_and(|d| d.hwnd == fg);
        if own {
            return;
        }
        let full = is_fullscreen(fg);
        let Some(d) = self.dock.as_mut() else { return };
        if full != d.hidden_for_fullscreen {
            d.hidden_for_fullscreen = full;
            unsafe {
                let _ = ShowWindow(d.hwnd, if full { SW_HIDE } else { SW_SHOWNOACTIVATE });
            }
            if !full {
                self.paint_dock();
            }
        }
    }
}

fn is_fullscreen(hwnd: HWND) -> bool {
    if hwnd.0.is_null() {
        return false;
    }
    unsafe {
        let mut class = [0u16; 64];
        let n = GetClassNameW(hwnd, &mut class) as usize;
        let class = String::from_utf16_lossy(&class[..n]);
        if matches!(class.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd") {
            return false;
        }
        let mut r = RECT::default();
        if GetWindowRect(hwnd, &mut r).is_err() {
            return false;
        }
        let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(mon, &mut info);
        let m = info.rcMonitor;
        r.left <= m.left && r.top <= m.top && r.right >= m.right && r.bottom >= m.bottom
    }
}

unsafe extern "system" fn on_foreground(
    _hook: windows::Win32::UI::Accessibility::HWINEVENTHOOK,
    _event: u32,
    hwnd: HWND,
    _object: i32,
    _child: i32,
    _thread: u32,
    _time: u32,
) {
    let p = APP.load(Ordering::Relaxed);
    if let Some(app) = p.as_mut() {
        app.check_fullscreen(hwnd);
    }
}

fn cursor_pos() -> POINT {
    let mut pt = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut pt);
    }
    pt
}

extern "system" fn dock_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        let Some(app) = app_of(hwnd) else { return DefWindowProcW(hwnd, msg, wparam, lparam) };
        match msg {
            // Never take focus from the app the user is typing in.
            WM_MOUSEACTIVATE => LRESULT(3), // MA_NOACTIVATE
            WM_MOUSEMOVE => {
                let mut expand = false;
                if let Some(d) = app.dock.as_mut() {
                    if !d.hover {
                        d.hover = true;
                        let mut tme = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                        let _ = TrackMouseEvent(&mut tme);
                        let _ = KillTimer(Some(hwnd), TIMER_DOCK_COLLAPSE);
                        expand = true;
                    }
                    let (w, h) = d.size_px();
                    if let Some(drag) = d.drag.as_mut() {
                        let c = cursor_pos();
                        let (dx, dy) = (c.x - drag.start_cursor.x, c.y - drag.start_cursor.y);
                        if !drag.moved && (dx.abs() > DRAG_THRESHOLD || dy.abs() > DRAG_THRESHOLD) {
                            drag.moved = true;
                        }
                        if drag.moved {
                            d.rect = RECT { left: drag.start_pos.x + dx, top: drag.start_pos.y + dy, right: drag.start_pos.x + dx + w, bottom: drag.start_pos.y + dy + h };
                            let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), d.rect.left, d.rect.top, 0, 0, SWP_NOSIZE | SWP_NOACTIVATE);
                            app.paint_dock();
                        }
                    }
                }
                if expand {
                    app.dock_animate(1.0);
                }
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                let dragging = app.dock.as_ref().is_some_and(|d| d.drag.is_some());
                if let Some(d) = app.dock.as_mut() {
                    d.hover = false;
                }
                if !dragging && !app.visible {
                    app.schedule_dock_collapse();
                }
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                if let Some(d) = app.dock.as_mut() {
                    SetCapture(hwnd);
                    d.drag = Some(Drag { start_cursor: cursor_pos(), start_pos: POINT { x: d.rect.left, y: d.rect.top }, moved: false });
                }
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let drag = app.dock.as_mut().and_then(|d| d.drag.take());
                let _ = ReleaseCapture();
                match drag {
                    Some(d) if d.moved => app.finish_drag(cursor_pos()),
                    Some(_) => app.dock_click(),
                    None => {}
                }
                LRESULT(0)
            }
            WM_RBUTTONUP => {
                let c = cursor_pos();
                app.hide();
                app.show_menu(c.x, c.y);
                LRESULT(0)
            }
            WM_TIMER => {
                match wparam.0 {
                    TIMER_DOCK_ANIM => {
                        let animating = app.dock.as_mut().map(|d| d.step()).unwrap_or(false);
                        app.paint_dock();
                        if !animating {
                            let _ = KillTimer(Some(hwnd), TIMER_DOCK_ANIM);
                        }
                    }
                    TIMER_DOCK_COLLAPSE => {
                        let _ = KillTimer(Some(hwnd), TIMER_DOCK_COLLAPSE);
                        let idle = app.dock.as_ref().is_some_and(|d| !d.hover && d.drag.is_none());
                        if idle && !app.visible {
                            app.dock_animate(0.0);
                        }
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

fn trim_memory() {
    unsafe {
        let _ = EmptyWorkingSet(GetCurrentProcess());
    }
}

fn apply_dwm(hwnd: HWND, dark: bool) {
    unsafe {
        let corner = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &corner as *const _ as *const _, 4);
        let flag: windows::core::BOOL = dark.into();
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &flag as *const _ as *const _, 4);
        let border: u32 = if dark { 0x003A3A3A } else { 0x00DADADA };
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR, &border as *const _ as *const _, 4);
    }
}

/// Lets the tray context menu follow dark mode (uxtheme ordinal 135/136;
/// undocumented but stable since 1809). Silently skipped if unavailable.
fn allow_dark_menus() {
    unsafe {
        let Ok(lib) = LoadLibraryW(w!("uxtheme.dll")) else { return };
        if let Some(set_mode) = GetProcAddress(lib, PCSTR(135 as *const u8)) {
            let f: extern "system" fn(i32) -> i32 = std::mem::transmute(set_mode);
            f(1);
        }
        if let Some(flush) = GetProcAddress(lib, PCSTR(136 as *const u8)) {
            let f: extern "system" fn() = std::mem::transmute(flush);
            f();
        }
    }
}

unsafe fn app_of(hwnd: HWND) -> Option<&'static mut App> {
    let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App;
    p.as_mut()
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        let Some(app) = app_of(hwnd) else { return DefWindowProcW(hwnd, msg, wparam, lparam) };
        if msg == app.taskbar_created && msg != 0 {
            app.refresh_tray(true);
            return LRESULT(0);
        }
        match msg {
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                BeginPaint(hwnd, &mut ps);
                app.ensure_target();
                app.paint();
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_SETCURSOR if matches!(app.hover, Some(Action::Launch(_))) => {
                if let Ok(c) = LoadCursorW(None, IDC_HAND) {
                    SetCursor(Some(c));
                }
                LRESULT(1)
            }
            WM_ACTIVATE => {
                if (wparam.0 & 0xffff) as u32 == WA_INACTIVE {
                    app.hide();
                }
                LRESULT(0)
            }
            WM_KEYDOWN => {
                if wparam.0 as u16 == VK_ESCAPE.0 {
                    app.hide();
                } else if wparam.0 as u16 == VK_F5.0 {
                    app.handle(Action::Refresh);
                }
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                if !app.tracking {
                    let mut tme = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                    let _ = TrackMouseEvent(&mut tme);
                    app.tracking = true;
                }
                let hover = app.hit_at(lparam);
                if hover != app.hover {
                    app.hover = hover;
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                app.tracking = false;
                if app.hover.take().is_some() {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                if let Some(a) = app.hit_at(lparam) {
                    app.handle(a);
                }
                LRESULT(0)
            }
            WM_TIMER => {
                match wparam.0 {
                    TIMER_TICK => {
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
                    TIMER_RESUME => {
                        let _ = KillTimer(Some(hwnd), TIMER_RESUME);
                        if let Some(s) = &app.scheduler {
                            s.refresh_all();
                        }
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            WM_APP_TRAY => {
                let event = (lparam.0 & 0xffff) as u32;
                let x = (wparam.0 & 0xffff) as i16 as i32;
                let y = ((wparam.0 >> 16) & 0xffff) as i16 as i32;
                match event {
                    // NIN_SELECT / NIN_KEYSELECT
                    0x400 | 0x401 => app.toggle(),
                    WM_CONTEXTMENU => {
                        app.hide();
                        app.show_menu(x, y);
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            WM_APP_UPDATE => {
                app.on_data();
                LRESULT(0)
            }
            WM_APP_SHOW => {
                if !app.visible {
                    app.last_hide = Instant::now() - std::time::Duration::from_secs(1);
                    app.toggle();
                }
                LRESULT(0)
            }
            WM_SETTINGCHANGE => {
                let changed = !PCWSTR(lparam.0 as *const u16).is_null()
                    && PCWSTR(lparam.0 as *const u16).to_string().map(|s| s == "ImmersiveColorSet").unwrap_or(false);
                if changed {
                    app.apply_theme();
                    app.refresh_tray(false);
                    app.paint_dock();
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            WM_DPICHANGED | WM_DISPLAYCHANGE => {
                app.hide();
                app.refresh_tray(false);
                app.replace_dock();
                LRESULT(0)
            }
            WM_POWERBROADCAST => {
                // PBT_APMRESUMEAUTOMATIC: give the network a moment, then refresh.
                if wparam.0 == 0x12 {
                    SetTimer(Some(hwnd), TIMER_RESUME, 15_000, None);
                }
                LRESULT(1)
            }
            WM_DESTROY => {
                app.destroy_dock();
                app.tray.remove();
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

pub fn run(mode: Mode) -> Result<()> {
    unsafe {
        let hinstance = GetModuleHandleW(None)?;
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_DROPSHADOW,
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            hIcon: LoadIconW(Some(hinstance.into()), PCWSTR(1 as *const u16)).unwrap_or_default(),
            lpszClassName: CLASS_NAME,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
        let dock_wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(dock_proc),
            hInstance: hinstance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: DOCK_CLASS,
            ..Default::default()
        };
        RegisterClassExW(&dock_wc);
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            CLASS_NAME,
            w!("AI Usage Lite"),
            WS_POPUP,
            0,
            0,
            360,
            400,
            None,
            None,
            Some(hinstance.into()),
            None,
        )?;
        allow_dark_menus();

        let config = Config::load();
        let mut app = Box::new(App {
            hwnd,
            mode,
            states: Default::default(),
            config: Arc::new(std::sync::RwLock::new(config)),
            scheduler: None,
            gfx: Gfx::new()?,
            target: None,
            page: Page::Main,
            hits: Vec::new(),
            hover: None,
            visible: false,
            last_hide: Instant::now() - std::time::Duration::from_secs(5),
            dpi: 96,
            anchor: Anchor::Bottom(0),
            size_px: (360, 400),
            dark: true,
            taskbar_dark: true,
            tray: Tray::new(hwnd, WM_APP_TRAY),
            taskbar_created: RegisterWindowMessageW(w!("TaskbarCreated")),
            tracking: false,
            dock: None,
            from_dock: false,
        });
        app.apply_theme();
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, app.as_mut() as *mut App as isize);
        APP.store(app.as_mut() as *mut App, Ordering::Relaxed);
        app.refresh_tray(true);
        if app.cfg().dock {
            app.create_dock();
        }
        let hook = windows::Win32::UI::Accessibility::SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            None,
            Some(on_foreground),
            0,
            0,
            WINEVENT_OUTOFCONTEXT,
        );
        if app.needs_consent() {
            app.show(Page::Consent);
        } else {
            app.start_scheduler();
        }
        if std::env::args().any(|a| a == "--open-mini") && app.dock.is_some() {
            if let Some(d) = app.dock.as_mut() {
                d.progress = 1.0;
                d.target = 1.0;
                d.hover = true;
            }
            app.paint_dock();
            app.show_from_dock(Page::Mini);
        }
        if std::env::args().any(|a| a == "--open-dock") && app.dock.is_some() {
            if let Some(d) = app.dock.as_mut() {
                d.progress = 1.0;
                d.target = 1.0;
                d.hover = true;
            }
            app.paint_dock();
            app.show_from_dock(Page::Main);
        }
        if std::env::args().any(|a| a == "--open") {
            app.show(Page::Main);
        }
        trim_memory();

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        if !hook.is_invalid() {
            let _ = windows::Win32::UI::Accessibility::UnhookWinEvent(hook);
        }
        APP.store(std::ptr::null_mut(), Ordering::Relaxed);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        drop(app);
        Ok(())
    }
}
