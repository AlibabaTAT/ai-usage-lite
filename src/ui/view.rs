//! Panel layout and painting (immediate mode). The same code path runs
//! without a render target to compute height and hit regions.

use std::time::{Duration, SystemTime};

use super::fmt;
use super::gfx::{font, icon_font, Align, Color, Font, Painter, Rect, REGULAR, SEMIBOLD};
use super::theme::Palette;
use crate::config::{Config, REFRESH_CHOICES};
use crate::model::{Balance, Failure, ProviderId, ProviderState, UsageWindow};

pub const WIDTH: f32 = 360.0;
pub const MINI_WIDTH: f32 = 252.0;

pub fn page_width(page: Page) -> f32 {
    if page == Page::Mini { MINI_WIDTH } else { WIDTH }
}
const PAD: f32 = 12.0;

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Refresh,
    SetCompact(bool),
    OpenSettings,
    Back,
    ToggleProvider(usize),
    SetInterval(u32),
    SetShowUsed(bool),
    ToggleAgRefresh,
    TogglePool(String),
    Consent,
    DemoOnly,
    /// Mini card → full panel.
    OpenFull,
    /// Open the desktop client of a service.
    Launch(usize),
    SetDock(bool),
    ToggleForecast,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Page {
    Main,
    Settings,
    Consent,
    /// Small card shown on the first click of the floating strip.
    Mini,
}

pub struct Input<'a> {
    pub pal: &'a Palette,
    pub cfg: &'a Config,
    pub states: &'a [ProviderState; crate::model::PROVIDER_COUNT],
    pub now: SystemTime,
    pub demo: bool,
    pub page: Page,
    pub hover: Option<&'a Action>,
    /// Scroll position of the body and visible height, in DIPs.
    pub scroll: f32,
    pub viewport: f32,
    pub bar: Bar,
}

/// Overlay scroll bar state (WinUI style): hidden, a thin panning
/// indicator, or the full thumb while the cursor is over it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Bar {
    Hidden,
    Indicator,
    Expanded,
}

/// Pinned header height of the scrollable pages.
pub const HEADER_H: f32 = 48.0;
/// Width of the scroll bar band at the right edge.
pub const BAR_BAND: f32 = 14.0;

pub fn scrolls(page: Page) -> bool {
    page != Page::Mini
}

/// Scroll bar track (top, height) and thumb (top, height).
pub fn thumb(content: f32, viewport: f32, scroll: f32) -> ((f32, f32), (f32, f32)) {
    let top = HEADER_H + 2.0;
    let track = (viewport - top - 6.0).max(1.0);
    let max = (content - viewport).max(1.0);
    let h = (track * viewport / content.max(1.0)).clamp(28.0f32.min(track), track);
    let y = top + (track - h) * (scroll / max).clamp(0.0, 1.0);
    ((top, track), (y, h))
}

pub struct Output {
    pub height: f32,
    pub hits: Vec<(Rect, Action)>,
}

struct Ctx<'a, 'b> {
    p: &'a Painter<'b>,
    i: &'a Input<'a>,
    hits: Vec<(Rect, Action)>,
}

// Glyphs from Segoe Fluent Icons / MDL2 Assets.
const ICON_REFRESH: &str = "\u{E72C}";
const ICON_SETTINGS: &str = "\u{E713}";
const ICON_BACK: &str = "\u{E72B}";

impl Ctx<'_, '_> {
    fn pal(&self) -> &Palette {
        self.i.pal
    }

    fn hit(&mut self, r: Rect, a: Action) -> bool {
        let hovered = self.i.hover == Some(&a);
        self.hits.push((r, a));
        hovered
    }

    fn icon_button(&mut self, r: Rect, glyph: &str, a: Action, color: Color) {
        let hovered = self.hit(r, a);
        if hovered {
            self.p.fill_round(r, 6.0, self.pal().hover);
        }
        self.p.text(glyph, r, icon_font(14.0), color);
    }

    fn segmented(&mut self, r: Rect, labels: &[&str], selected: usize, actions: Vec<Action>) {
        let pal = *self.pal();
        self.p.fill_round(r, 7.0, pal.seg_bg);
        let n = labels.len() as f32;
        let w = (r.w - 4.0) / n;
        for (k, (label, action)) in labels.iter().zip(actions).enumerate() {
            let cell = Rect::new(r.x + 2.0 + w * k as f32, r.y + 2.0, w, r.h - 4.0);
            let hovered = self.hit(cell, action);
            if k == selected {
                self.p.fill_round(cell, 5.0, pal.seg_sel);
                if !pal.dark {
                    self.p.stroke_round(cell, 5.0, pal.card_border, 1.0);
                }
            } else if hovered {
                self.p.fill_round(cell, 5.0, pal.hover);
            }
            let (weight, color) = if k == selected { (SEMIBOLD, pal.text) } else { (REGULAR, pal.text2) };
            self.p.text(label, cell, font(13.0, weight, Align::Center), color);
        }
    }

    fn toggle(&self, cx_right: f32, cy: f32, on: bool) {
        let pal = self.pal();
        let r = Rect::new(cx_right - 40.0, cy - 10.0, 40.0, 20.0);
        if on {
            self.p.fill_round(r, 10.0, pal.accent);
            self.p.fill_circle(r.right() - 10.0, cy, 6.0, if pal.dark { Color::hex(0x1B1B1B) } else { Color::hex(0xFFFFFF) });
        } else {
            self.p.stroke_round(r, 10.0, pal.toggle_off, 1.0);
            self.p.fill_circle(r.x + 10.0, cy, 5.0, pal.toggle_off);
        }
    }

    fn card_bg(&self, r: Rect) {
        let pal = self.pal();
        self.p.fill_round(r, 8.0, pal.card);
        self.p.stroke_round(r, 8.0, pal.card_border, 1.0);
    }

    fn tile(&self, x: f32, y: f32, size: f32, id: ProviderId) {
        // An icon the user put in %APPDATA%\AI-Usage-Lite\icons replaces the tile.
        if let Some(icon) = crate::ui::launch::custom_icon(id) {
            self.p.image(icon, Rect::new(x, y, size, size));
            return;
        }
        let pal = self.pal();
        let white = Color::hex(0xFFFFFF);
        let bg = match id {
            ProviderId::Claude => Color::hex(0xC8694A),
            ProviderId::Codex => if pal.dark { Color::hex(0x3B3B3B) } else { Color::hex(0x2A2A2A) },
            ProviderId::Antigravity => Color::hex(0x4874D6),
            ProviderId::DeepSeek => Color::hex(0x4D6BFE),
        };
        self.p.fill_round(Rect::new(x, y, size, size), size * 0.26, bg);
        let (cx, cy) = (x + size / 2.0, y + size / 2.0);
        let s = size / 30.0;
        match id {
            // A plain four-point spark (generic glyph, not a brand logo).
            ProviderId::Claude => {
                let r = 8.5 * s;
                let k = 1.6 * s;
                self.p.fill_path(
                    (cx, cy - r),
                    &[
                        [(cx + k, cy - k), (cx + k, cy - k), (cx + r, cy)],
                        [(cx + k, cy + k), (cx + k, cy + k), (cx, cy + r)],
                        [(cx - k, cy + k), (cx - k, cy + k), (cx - r, cy)],
                        [(cx - k, cy - k), (cx - k, cy - k), (cx, cy - r)],
                    ],
                    white,
                );
            }
            // Terminal prompt ">_".
            ProviderId::Codex => {
                let w = 1.9 * s;
                self.p.stroke_poly(&[(cx - 7.0 * s, cy - 5.0 * s), (cx - 2.0 * s, cy), (cx - 7.0 * s, cy + 5.0 * s)], white, w);
                self.p.line(cx + 1.0 * s, cy + 5.5 * s, cx + 7.5 * s, cy + 5.5 * s, white, w);
            }
            // Rising arch.
            ProviderId::Antigravity => {
                self.p.stroke_poly(&[(cx - 7.0 * s, cy + 6.0 * s), (cx, cy - 7.0 * s), (cx + 7.0 * s, cy + 6.0 * s)], white, 2.2 * s);
                self.p.line(cx - 3.2 * s, cy + 1.5 * s, cx + 3.2 * s, cy + 1.5 * s, white, 1.6 * s);
            }
            // DeepSeek blue tile with initials (generic, not the logo).
            ProviderId::DeepSeek => {
                self.p.text("DS", Rect::new(x, y, size, size), font(12.0 * s, SEMIBOLD, Align::Center), white);
            }
        }
    }

    /// Service icon that opens its desktop client; returns hover state.
    fn launch_tile(&mut self, r: Rect, id: ProviderId) -> bool {
        let hovered = self.hit(r, Action::Launch(id.index()));
        if hovered {
            let ring = Rect::new(r.x - 3.0, r.y - 3.0, r.w + 6.0, r.h + 6.0);
            self.p.fill_round(ring, r.w * 0.3 + 3.0, self.pal().hover);
            self.p.stroke_round(ring, r.w * 0.3 + 3.0, self.pal().accent.alpha(0.55), 1.0);
        }
        self.tile(r.x, r.y, r.w, id);
        hovered
    }

    fn chip(&self, right: f32, cy: f32, text: &str, color: Color) -> f32 {
        let f = font(11.5, SEMIBOLD, Align::Center);
        let w = self.p.measure(text, f) + 14.0;
        let r = Rect::new(right - w, cy - 10.0, w, 20.0);
        self.p.fill_round(r, 10.0, color.alpha(0.16));
        self.p.text(text, r, f, color);
        w
    }
}

/// Forecast for a window, if enabled and the data is live.
fn forecast_of(i: &Input, win: &UsageWindow, dim: bool) -> Option<crate::forecast::Forecast> {
    if !i.cfg.forecast || dim {
        return None;
    }
    crate::forecast::forecast(win, i.now)
}

/// Soonest projected run-out among a service's windows.
fn soonest_exhaust<'w>(i: &Input, wins: impl Iterator<Item = &'w UsageWindow>, dim: bool) -> Option<Duration> {
    wins.filter_map(|w| forecast_of(i, w, dim)?.exhaust_in).min()
}

/// One quiet line: does the balance last until the reset, and if not, when
/// it runs out.
fn forecast_line(f: &crate::forecast::Forecast, _show_used: bool) -> (String, bool) {
    match f.exhaust_in {
        Some(d) => (exhaust_text(d), true),
        None => ("可撑到重置".into(), false),
    }
}

/// Short form for the mini card and compact list.
fn exhaust_text(d: Duration) -> String {
    format!("约 {}后用完", fmt::span(d))
}

const FORECAST_LINE_H: f32 = 17.0;

fn money(b: &Balance, v: f64) -> String {
    format!("{}{:.2}", b.symbol(), v)
}

/// Low balance (or the service says calls would fail) shows in red.
fn balance_low(cfg: &Config, b: &Balance) -> bool {
    !b.available || b.total < cfg.deepseek_low_balance
}

const UNAVAILABLE: &str = "余额不足，API 暂不可用";
/// The DeepSeek balance bar is full at ¥100.
const BALANCE_FULL: f64 = 100.0;

/// "约 23天10小时后用完" / "可用 3 个月以上", from the projected days left.
fn days_text(days: f64) -> String {
    if days > crate::spend::MAX_DAYS {
        "可用 3 个月以上".into()
    } else {
        exhaust_text(Duration::from_secs_f64((days * 86_400.0).max(60.0)))
    }
}

/// Days-left projection, when forecasts are on and the data is live.
fn balance_days(i: &Input, b: &Balance, dim: bool) -> Option<f64> {
    if !i.cfg.forecast || dim {
        return None;
    }
    b.spend?.days_left
}

/// "今日已用 ¥1.26", shown on the balance line where the reset time would be.
fn today_text(b: &Balance, dim: bool) -> Option<String> {
    if dim {
        return None;
    }
    Some(format!("今日已用 {}", money(b, b.spend?.today)))
}

/// Line under the balance, like the forecast under a usage window.
/// Returns the text and whether it should stand out.
fn balance_line(i: &Input, b: &Balance, dim: bool) -> Option<(String, bool)> {
    if !b.available {
        return Some((UNAVAILABLE.into(), true));
    }
    if !i.cfg.forecast || dim {
        return None;
    }
    b.spend?;
    Some(match balance_days(i, b, dim) {
        Some(d) => (days_text(d), d < 7.0),
        None => ("近期无消耗，暂不预估".into(), false),
    })
}

fn balance_of(st: &Status) -> Option<&Balance> {
    st.rows.iter().find_map(|r| if let Row::Balance(b) = r { Some(b) } else { None })
}

fn stale_after(cfg: &Config) -> Duration {
    Duration::from_secs((u64::from(cfg.refresh_minutes) * 60 * 3).max(20 * 60))
}

fn failure_color(pal: &Palette, f: &Failure) -> Color {
    match f {
        Failure::NoCredentials | Failure::TokenExpired | Failure::AuthRejected(_) | Failure::NoPermission => pal.danger,
        Failure::Unsupported(_) | Failure::BadResponse => pal.text2,
        _ => pal.warn,
    }
}

enum Row {
    PoolTitle(String),
    Balance(Balance),
    Win(UsageWindow),
    Message(String, Color),
}

struct Status {
    rows: Vec<Row>,
    chip: Option<(String, Color)>,
    sub: String,
    dim: bool,
}

fn status_of(i: &Input, id: ProviderId) -> Status {
    let pal = i.pal;
    let st = &i.states[id.index()];
    let err = st.current_error();
    let mut rows = Vec::new();
    let mut chip = None;
    let mut dim = false;
    let mut sub = String::new();

    if let Some((at, snap)) = &st.last_ok {
        let stale = i.now.duration_since(*at).is_ok_and(|age| age > stale_after(i.cfg));
        let pools: Vec<_> = snap
            .pools
            .iter()
            .filter(|p| id != ProviderId::Antigravity || i.cfg.pool_visible(&p.name))
            .collect();
        let titled = id == ProviderId::Antigravity;
        for pool in &pools {
            if titled {
                rows.push(Row::PoolTitle(pool.name.clone()));
            }
            rows.extend(pool.windows.iter().cloned().map(Row::Win));
        }
        if let Some(b) = &snap.balance {
            rows.push(Row::Balance(b.clone()));
        } else if snap.pools.is_empty() {
            rows.push(Row::Message("账号没有返回额度数据".into(), pal.text2));
        } else if pools.is_empty() {
            rows.push(Row::Message("额度池均已隐藏，可在设置中开启".into(), pal.text2));
        }
        sub = format!("{} 更新", fmt::updated(*at, i.now));
        if let Some(e) = err {
            chip = Some((e.title(), failure_color(pal, e)));
            dim = true;
            rows.push(Row::Message(format!("{}；以上为 {} 的数据", e.hint(id), fmt::clock(*at)), pal.text3));
        } else if stale {
            chip = Some(("数据过期".into(), pal.warn));
            dim = true;
        }
    } else if let Some(e) = err {
        chip = Some((e.title(), failure_color(pal, e)));
        rows.push(Row::Message(e.hint(id), pal.text2));
        sub = "无数据".into();
    } else {
        rows.push(Row::Message(if st.in_flight { "正在查询…".into() } else { "等待查询".into() }, pal.text3));
        sub = "—".into();
    }
    if st.in_flight {
        sub = "正在刷新…".into();
    }
    Status { rows, chip, sub, dim }
}

pub fn draw(p: &Painter, i: &Input) -> Output {
    let mut c = Ctx { p, i, hits: Vec::new() };
    p.clear(i.pal.bg);
    let scroll = if scrolls(i.page) { i.scroll.max(0.0) } else { 0.0 };
    p.set_offset(scroll);
    let height = match i.page {
        Page::Main => main_page(&mut c),
        Page::Settings => settings_page(&mut c),
        Page::Consent => consent_page(&mut c),
        Page::Mini => mini_page(&mut c),
    };
    p.set_offset(0.0);
    if !scrolls(i.page) || height <= i.viewport + 0.5 {
        return Output { height, hits: c.hits };
    }
    let pal = *c.pal();
    // Hits move with the content; whatever slid under the header is gone.
    let mut hits: Vec<(Rect, Action)> = Vec::new();
    for (r, a) in c.hits.drain(..) {
        let top = (r.y - scroll).max(if scroll > 0.0 { HEADER_H } else { 0.0 });
        let bottom = (r.bottom() - scroll).min(i.viewport);
        if bottom > top {
            hits.push((Rect::new(r.x, top, r.w, bottom - top), a));
        }
    }
    let w = page_width(i.page);
    // More below: a soft fade into the background.
    if scroll < height - i.viewport - 0.5 {
        for k in 0..8 {
            let y = i.viewport - 28.0 + 3.5 * k as f32;
            p.fill_round(Rect::new(0.0, y, w, 3.5), 0.0, pal.bg.alpha(0.1 + 0.11 * k as f32));
        }
    }
    // Pinned header with a hairline once content runs under it.
    if scroll > 0.0 {
        p.fill_round(Rect::new(0.0, 0.0, w, HEADER_H), 0.0, pal.bg);
        match i.page {
            Page::Main => main_header(&mut c),
            Page::Settings => settings_header(&mut c),
            Page::Consent => consent_header(&mut c),
            Page::Mini => {}
        }
        p.line(0.0, HEADER_H - 0.5, w, HEADER_H - 0.5, pal.divider, 1.0);
        hits.extend(c.hits.drain(..));
    }
    let ((ty, th), (y, h)) = thumb(height, i.viewport, scroll);
    match i.bar {
        Bar::Hidden => {}
        Bar::Indicator => p.fill_round(Rect::new(w - 5.0, y, 2.0, h), 1.0, pal.text3.alpha(0.7)),
        Bar::Expanded => {
            p.fill_round(Rect::new(w - BAR_BAND + 2.0, ty - 2.0, BAR_BAND - 4.0, th + 4.0), 5.0, pal.track.alpha(0.9));
            p.fill_round(Rect::new(w - 10.0, y, 6.0, h), 3.0, pal.text2);
        }
    }
    Output { height, hits }
}

fn main_header(c: &mut Ctx) {
    let pal = *c.pal();
    let i = c.i;
    c.p.text("AI 额度", Rect::new(20.0, 12.0, 160.0, 28.0), font(15.0, SEMIBOLD, Align::Left), pal.text);
    let busy = i.states.iter().any(|s| s.in_flight);
    let btn = Rect::new(WIDTH - 16.0 - 32.0, 10.0, 32.0, 32.0);
    c.icon_button(btn, ICON_REFRESH, Action::Refresh, if busy { pal.accent } else { pal.text2 });
    let latest = i.states.iter().filter_map(|s| s.last_ok.as_ref().map(|(t, _)| *t)).max();
    let note = if busy {
        "正在刷新…".to_string()
    } else if let Some(t) = latest {
        format!("{} 更新 · {}", fmt::updated(t, i.now), if i.cfg.show_used { "已用" } else { "剩余" })
    } else {
        String::new()
    };
    c.p.text(&note, Rect::new(btn.x - 200.0, 10.0, 196.0, 32.0), font(12.0, REGULAR, Align::Right), pal.text3);
}

fn main_page(c: &mut Ctx) -> f32 {
    let pal = *c.pal();
    let i = c.i;
    main_header(c);
    let mut y = 52.0;
    let compact = i.cfg.compact;
    c.segmented(
        Rect::new(16.0, y, WIDTH - 32.0, 32.0),
        &["详细", "精简"],
        compact as usize,
        vec![Action::SetCompact(false), Action::SetCompact(true)],
    );
    y += 32.0 + 12.0;

    let enabled: Vec<ProviderId> = ProviderId::ALL.into_iter().filter(|id| i.cfg.is_enabled(*id)).collect();
    if enabled.is_empty() {
        c.p.text("没有启用的服务，请在设置中开启", Rect::new(20.0, y, WIDTH - 40.0, 40.0), font(12.5, REGULAR, Align::Center), pal.text2);
        y += 48.0;
    } else if compact {
        y = compact_list(c, &enabled, y) + 8.0;
    } else {
        for id in enabled {
            y = detail_card(c, id, y) + 8.0;
        }
    }

    // Footer
    let btn = Rect::new(10.0, y + 2.0, 76.0, 32.0);
    let hovered = c.hit(btn, Action::OpenSettings);
    if hovered {
        c.p.fill_round(btn, 6.0, pal.hover);
    }
    c.p.text(ICON_SETTINGS, Rect::new(btn.x + 8.0, btn.y, 18.0, 32.0), icon_font(13.0), pal.text2);
    c.p.text("设置", Rect::new(btn.x + 30.0, btn.y, 40.0, 32.0), font(12.5, REGULAR, Align::Left), pal.text2);
    let right_note = if i.demo {
        "演示数据 · 未读取任何账号".to_string()
    } else {
        format!("每 {} 分钟自动刷新", i.cfg.refresh_minutes)
    };
    c.p.text(&right_note, Rect::new(100.0, y + 2.0, WIDTH - 120.0, 32.0), font(11.5, REGULAR, Align::Right), if i.demo { pal.warn } else { pal.text3 });
    y + 42.0
}

fn detail_card(c: &mut Ctx, id: ProviderId, y0: f32) -> f32 {
    let pal = *c.pal();
    let i = c.i;
    let st = status_of(i, id);
    let x0 = PAD;
    let w = WIDTH - 2.0 * PAD;
    let inner_x = x0 + 14.0;
    let inner_w = w - 28.0;
    let msg_font = Font { wrap: true, ..font(12.0, REGULAR, Align::Left) };

    // Measure rows.
    let mut h = 12.0 + 32.0 + 8.0;
    for row in &st.rows {
        h += match row {
            Row::PoolTitle(_) => 24.0,
            Row::Balance(b) => 26.0 + if balance_line(i, b, st.dim).is_some() { FORECAST_LINE_H } else { 0.0 },
            Row::Win(w) => 26.0 + if forecast_of(i, w, st.dim).is_some() { FORECAST_LINE_H } else { 0.0 },
            Row::Message(m, _) => c.p.text_height(m, msg_font, inner_w) + 6.0,
        };
    }
    h += 8.0;
    let card = Rect::new(x0, y0, w, h);
    c.card_bg(card);

    // Header. The icon opens the service's desktop client.
    let tile = Rect::new(inner_x, y0 + 12.0, 32.0, 32.0);
    let tile_hover = c.launch_tile(tile, id);
    let tx = inner_x + 32.0 + 12.0;
    c.p.text(id.name(), Rect::new(tx, y0 + 11.0, 170.0, 19.0), font(14.0, SEMIBOLD, Align::Left), pal.text);
    if tile_hover {
        let hint = format!("打开 {} 客户端", crate::ui::launch::client_label(id));
        c.p.text(&hint, Rect::new(tx, y0 + 29.0, 170.0, 16.0), font(11.5, REGULAR, Align::Left), pal.accent);
    } else {
        c.p.text(&st.sub, Rect::new(tx, y0 + 29.0, 170.0, 16.0), font(11.5, REGULAR, Align::Left), pal.text3);
    }
    if let Some((text, color)) = &st.chip {
        c.chip(card.right() - 14.0, y0 + 28.0, text, *color);
    } else if !st.dim && balance_of(&st).is_some_and(|b| b.spend.is_some()) {
        let head = Rect::new(inner_x + inner_w - 82.0, y0 + 20.0, 82.0, 16.0);
        c.p.text("今日已用", head, font(11.5, REGULAR, Align::Right), pal.text3);
    } else if st.rows.iter().any(|r| matches!(r, Row::Win(w) if w.resets_at.is_some())) {
        // Column heading, so the times on the right read as "when the quota
        // resets" and not "how long it lasts". Empty space reads larger than
        // the same space filled by a forecast line, so the heading sits a bit
        // closer to its column than the row pitch: between the name line and
        // the "更新" line, about 36 px above the first time.
        let head = Rect::new(inner_x + inner_w - 82.0, y0 + 20.0, 82.0, 16.0);
        c.p.text("重置时间", head, font(11.5, REGULAR, Align::Right), pal.text3);
    }

    let mut y = y0 + 12.0 + 32.0 + 8.0;
    let mut first_title = true;
    for row in &st.rows {
        match row {
            Row::PoolTitle(name) => {
                if !first_title {
                    c.p.line(inner_x, y + 2.0, inner_x + inner_w, y + 2.0, pal.divider, 1.0);
                }
                first_title = false;
                c.p.text(name, Rect::new(inner_x, y + 4.0, inner_w, 20.0), font(12.0, SEMIBOLD, Align::Left), pal.text2);
                y += 24.0;
            }
            Row::Balance(b) => {
                let low = balance_low(i.cfg, b) && !st.dim;
                let num = if st.dim { pal.text2 } else if low { pal.danger } else { pal.text };
                // Same rhythm as a usage window: label · amount · today's
                // spend in the right column, then one quiet forecast line.
                // Laid out exactly like a usage window: label · bar (¥100 =
                // full, DeepSeek blue) · amount · today's spend in the right
                // column, whose heading reads "今日已用".
                let r = Rect::new(inner_x, y, inner_w, 26.0);
                let today_col = Rect::new(r.right() - 70.0, r.y, 70.0, r.h);
                let amount = Rect::new(today_col.x - 62.0, r.y, 58.0, r.h);
                let bar_x = r.x + 56.0;
                let bar = Rect::new(bar_x, r.y + r.h / 2.0 - 3.0, (amount.x - 8.0) - bar_x, 6.0);
                c.p.text("余额", Rect::new(r.x, r.y, 52.0, r.h), font(12.0, REGULAR, Align::Left), pal.text2);
                c.p.fill_round(bar, 3.0, pal.track);
                let frac = (b.total / BALANCE_FULL).clamp(0.0, 1.0) as f32;
                if frac > 0.0 {
                    let fill_color = if st.dim { pal.text3 } else if low { pal.danger } else { pal.brand(id) };
                    c.p.fill_round(Rect::new(bar.x, bar.y, (bar.w * frac).max(bar.h), bar.h), 3.0, fill_color);
                }
                c.p.text(&money(b, b.total), amount, font(13.0, SEMIBOLD, Align::Right), num);
                if let Some(t) = b.spend.filter(|_| !st.dim) {
                    c.p.text(&money(b, t.today), today_col, font(12.0, REGULAR, Align::Right), pal.text3);
                }
                y += 26.0;
                if let Some((text, alarm)) = balance_line(i, b, st.dim) {
                    let color = if !b.available { pal.danger } else if alarm { pal.text2 } else { pal.text3 };
                    c.p.text(&text, Rect::new(inner_x + 56.0, y - 4.0, inner_w - 56.0, FORECAST_LINE_H), font(11.5, REGULAR, Align::Left), color);
                    y += FORECAST_LINE_H;
                }
            }
            Row::Win(win) => {
                let fc = forecast_of(i, win, st.dim);
                window_row(c, id, win, Rect::new(inner_x, y, inner_w, 26.0), st.dim, fc.as_ref());
                y += 26.0;
                if let Some(f) = &fc {
                    let (text, alarm) = forecast_line(f, i.cfg.show_used);
                    let color = if alarm { pal.text2 } else { pal.text3 };
                    c.p.text(&text, Rect::new(inner_x + 56.0, y - 4.0, inner_w - 56.0, FORECAST_LINE_H), font(11.5, REGULAR, Align::Left), color);
                    y += FORECAST_LINE_H;
                }
            }
            Row::Message(m, color) => {
                let mh = c.p.text_height(m, msg_font, inner_w);
                c.p.text(m, Rect::new(inner_x, y + 2.0, inner_w, mh + 2.0), msg_font, *color);
                y += mh + 6.0;
            }
        }
    }
    y0 + h
}

fn window_row(c: &mut Ctx, id: ProviderId, win: &UsageWindow, r: Rect, dim: bool, fc: Option<&crate::forecast::Forecast>) {
    let pal = *c.pal();
    let show_used = c.i.cfg.show_used;
    let remaining = win.remaining();
    let value = if show_used { win.used.clamp(0.0, 100.0) } else { remaining };
    let level = pal.usage(id, remaining);

    // Long labels (e.g. "本周·Opus") get a wider column.
    let label_w = if win.label.chars().count() > 4 { 76.0 } else { 52.0 };
    let label = Rect::new(r.x, r.y, label_w, r.h);
    let reset = Rect::new(r.right() - 82.0, r.y, 82.0, r.h);
    let pct = Rect::new(reset.x - 50.0, r.y, 46.0, r.h);
    let bar_x = r.x + 56.0f32.max(label_w + 4.0);
    let bar = Rect::new(bar_x, r.y + r.h / 2.0 - 3.0, (pct.x - 8.0) - bar_x, 6.0);

    let label_color = if win.exhausted() && !dim { pal.danger } else { pal.text2 };
    c.p.text(&win.label, label, font(12.0, REGULAR, Align::Left), label_color);
    c.p.fill_round(bar, 3.0, pal.track);
    let frac = (value / 100.0).clamp(0.0, 1.0) as f32;
    let color = if dim { pal.text3 } else { level };
    let _ = fc;
    if frac > 0.0 {
        let fill = Rect::new(bar.x, bar.y, (bar.w * frac).max(bar.h), bar.h);
        c.p.fill_round(fill, 3.0, color);
    }
    // Numbers stay neutral; only the bar carries the brand colour. Red when nearly out.
    let pct_color = if dim { pal.text2 } else if remaining < crate::ui::tray::DANGER_BELOW { pal.danger } else { pal.text };
    c.p.text(&fmt::percent(value), pct, font(13.0, SEMIBOLD, Align::Right), pct_color);
    c.p.text(&fmt::reset(win.resets_at, c.i.now), reset, font(12.0, REGULAR, Align::Right), pal.text3);
}

fn short_label(label: &str) -> String {
    match label {
        "5 小时" => "5h".into(),
        "本周" => "周".into(),
        "30 天" => "月".into(),
        "今日" => "日".into(),
        other => other.chars().take(2).collect(),
    }
}

fn compact_list(c: &mut Ctx, ids: &[ProviderId], y0: f32) -> f32 {
    let pal = *c.pal();
    let i = c.i;
    let row_h = 52.0;
    let card = Rect::new(PAD, y0, WIDTH - 2.0 * PAD, row_h * ids.len() as f32);
    c.card_bg(card);
    for (k, id) in ids.iter().enumerate() {
        let y = y0 + row_h * k as f32;
        if k > 0 {
            c.p.line(card.x + 14.0, y, card.right() - 14.0, y, pal.divider, 1.0);
        }
        let st = status_of(i, *id);
        let x = card.x + 14.0;
        c.launch_tile(Rect::new(x, y + 14.0, 26.0, 26.0), *id);
        let tx = x + 26.0 + 10.0;
        c.p.text(id.name(), Rect::new(tx, y + 9.0, 96.0, 19.0), font(13.0, SEMIBOLD, Align::Left), pal.text);

        let wins: Vec<&UsageWindow> = st
            .rows
            .iter()
            .filter_map(|r| if let Row::Win(w) = r { Some(w) } else { None })
            .collect();
        // Sub line: an error, or when the tightest window comes back.
        let sub_rect = Rect::new(tx, y + 28.0, card.right() - 14.0 - tx, 16.0);
        let bal = balance_of(&st);
        if let Some((text, color)) = &st.chip {
            c.p.text(text, sub_rect, font(11.5, SEMIBOLD, Align::Left), *color);
        } else if let Some(b) = bal {
            let (text, color) = match balance_line(i, b, st.dim) {
                Some((t, _)) if !b.available => (t, pal.danger),
                Some((t, alarm)) if balance_days(i, b, st.dim).is_some() => (t, if alarm { pal.text2 } else { pal.text3 }),
                _ => (today_text(b, st.dim).unwrap_or_else(|| st.sub.clone()), pal.text3),
            };
            c.p.text(&text, sub_rect, font(11.5, REGULAR, Align::Left), color);
        } else if let Some(d) = soonest_exhaust(i, wins.iter().copied(), st.dim) {
            c.p.text(&exhaust_text(d), sub_rect, font(11.5, REGULAR, Align::Left), pal.text2);
        } else if let Some(tight) = wins.iter().min_by(|a, b| a.remaining().total_cmp(&b.remaining())) {
            let when = fmt::reset(tight.resets_at, i.now);
            let s = if when.is_empty() {
                st.sub.clone()
            } else if when.ends_with('后') {
                format!("{when}恢复")
            } else {
                format!("{when} 恢复")
            };
            c.p.text(&s, sub_rect, font(11.5, REGULAR, Align::Left), pal.text3);
        } else {
            c.p.text(&st.sub, sub_rect, font(11.5, REGULAR, Align::Left), pal.text3);
        }

        // Right side: up to two windows of the first visible pool.
        let first_pool: Vec<&UsageWindow> = {
            let mut out = Vec::new();
            let mut started = false;
            for r in &st.rows {
                match r {
                    Row::PoolTitle(_) if started => break,
                    Row::PoolTitle(_) => started = true,
                    Row::Win(w) => {
                        started = true;
                        out.push(w);
                    }
                    _ => {}
                }
            }
            out
        };
        if let Some(b) = bal {
            let low = balance_low(i.cfg, b) && !st.dim;
            let color = if st.dim { pal.text2 } else if low { pal.danger } else { pal.text };
            c.p.text(&money(b, b.total), Rect::new(card.right() - 14.0 - 140.0, y + 8.0, 140.0, 21.0), font(14.0, SEMIBOLD, Align::Right), color);
        }
        let mut right = card.right() - 14.0;
        for win in first_pool.iter().take(2).rev() {
            // Meters sit on the name line so the sub line has the full width.
            let line_y = y + 9.0;
            let line_h = 19.0;
            let block = Rect::new(right - 88.0, line_y, 88.0, line_h);
            let remaining = win.remaining();
            let value = if i.cfg.show_used { win.used.clamp(0.0, 100.0) } else { remaining };
            let level = if st.dim { pal.text3 } else { pal.usage(*id, remaining) };
            c.p.text(&short_label(&win.label), Rect::new(block.x, line_y, 20.0, line_h), font(11.5, REGULAR, Align::Left), pal.text3);
            let bar = Rect::new(block.x + 21.0, line_y + line_h / 2.0 - 2.0, 28.0, 4.0);
            c.p.fill_round(bar, 2.0, pal.track);
            let frac = (value / 100.0).clamp(0.0, 1.0) as f32;
            if frac > 0.0 {
                c.p.fill_round(Rect::new(bar.x, bar.y, (bar.w * frac).max(4.0), 4.0), 2.0, level);
            }
            let color = if st.dim { pal.text2 } else if remaining < crate::ui::tray::DANGER_BELOW { pal.danger } else { pal.text };
            c.p.text(&fmt::percent(value), Rect::new(bar.right() + 2.0, line_y, 37.0, line_h), font(12.5, SEMIBOLD, Align::Right), color);
            right = block.x - 4.0;
        }
    }
    card.bottom()
}

fn section_label(c: &Ctx, text: &str, y: f32) -> f32 {
    c.p.text(text, Rect::new(20.0, y, WIDTH - 40.0, 20.0), font(12.0, SEMIBOLD, Align::Left), c.pal().text2);
    y + 24.0
}

fn settings_header(c: &mut Ctx) {
    let pal = *c.pal();
    c.icon_button(Rect::new(12.0, 10.0, 32.0, 32.0), ICON_BACK, Action::Back, pal.text2);
    c.p.text("设置", Rect::new(52.0, 12.0, 160.0, 28.0), font(15.0, SEMIBOLD, Align::Left), pal.text);
}

fn settings_page(c: &mut Ctx) -> f32 {
    let pal = *c.pal();
    let i = c.i;
    settings_header(c);

    let x0 = PAD;
    let w = WIDTH - 2.0 * PAD;
    let mut y = section_label(c, "显示的服务", 54.0);
    let row_h = 44.0;
    let card = Rect::new(x0, y, w, row_h * ProviderId::ALL.len() as f32);
    c.card_bg(card);
    for id in ProviderId::ALL {
        let ry = y + row_h * id.index() as f32;
        let row = Rect::new(x0, ry, w, row_h);
        if id.index() > 0 {
            c.p.line(x0 + 14.0, ry, x0 + w - 14.0, ry, pal.divider, 1.0);
        }
        let hovered = c.hit(row, Action::ToggleProvider(id.index()));
        if hovered {
            c.p.fill_round(Rect::new(row.x + 4.0, row.y + 4.0, row.w - 8.0, row.h - 8.0), 6.0, pal.hover);
        }
        c.tile(x0 + 14.0, ry + 11.0, 22.0, id);
        c.p.text(id.name(), Rect::new(x0 + 46.0, ry, 200.0, row_h), font(13.0, REGULAR, Align::Left), pal.text);
        c.toggle(row.right() - 14.0, ry + row_h / 2.0, i.cfg.is_enabled(id));
    }
    y = card.bottom() + 14.0;

    y = section_label(c, "显示方式", y);
    c.segmented(
        Rect::new(16.0, y, WIDTH - 32.0, 32.0),
        &["屏幕边缘悬浮条", "仅托盘图标"],
        (!i.cfg.dock) as usize,
        vec![Action::SetDock(true), Action::SetDock(false)],
    );
    y += 32.0 + 14.0;

    let row = Rect::new(PAD, y, WIDTH - 2.0 * PAD, 44.0);
    c.card_bg(row);
    if c.hit(row, Action::ToggleForecast) {
        c.p.fill_round(Rect::new(row.x + 4.0, row.y + 4.0, row.w - 8.0, row.h - 8.0), 6.0, pal.hover);
    }
    c.p.text("显示消耗速度与用完预估", Rect::new(PAD + 14.0, y, 240.0, 44.0), font(13.0, REGULAR, Align::Left), pal.text);
    c.toggle(row.right() - 14.0, y + 22.0, i.cfg.forecast);
    y += 44.0 + 14.0;

    y = section_label(c, "自动刷新间隔", y);
    let labels: Vec<String> = REFRESH_CHOICES.iter().map(|m| format!("{m} 分钟")).collect();
    let refs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();
    let sel = REFRESH_CHOICES.iter().position(|m| *m == i.cfg.refresh_minutes).unwrap_or(1);
    c.segmented(Rect::new(16.0, y, WIDTH - 32.0, 32.0), &refs, sel, REFRESH_CHOICES.iter().map(|m| Action::SetInterval(*m)).collect());
    y += 32.0 + 14.0;

    y = section_label(c, "数字与进度条表示", y);
    c.segmented(
        Rect::new(16.0, y, WIDTH - 32.0, 32.0),
        &["剩余额度", "已用额度"],
        i.cfg.show_used as usize,
        vec![Action::SetShowUsed(false), Action::SetShowUsed(true)],
    );
    y += 32.0 + 14.0;

    // Antigravity options only matter while the service is shown.
    if !i.cfg.is_enabled(ProviderId::Antigravity) {
        return settings_footer(c, y - 4.0);
    }
    y = section_label(c, "Antigravity", y);
    let desc = "访问令牌过期时，用本机已保存的刷新令牌向 Google 换取临时令牌，只保存在内存，不写回、不改动登录。";
    let desc_font = Font { wrap: true, ..font(11.5, REGULAR, Align::Left) };
    let desc_h = c.p.text_height(desc, desc_font, w - 28.0 - 52.0);
    let pools = &i.cfg.antigravity_seen_pools;
    let pool_h = 36.0;
    let pools_h = 30.0 + if pools.is_empty() { 26.0 } else { pool_h * pools.len() as f32 } + 6.0;
    let top_h = 12.0 + 20.0 + desc_h + 12.0;
    let card = Rect::new(x0, y, w, top_h + pools_h);
    c.card_bg(card);
    let top = Rect::new(x0, y, w, top_h);
    let hovered = c.hit(top, Action::ToggleAgRefresh);
    if hovered {
        c.p.fill_round(Rect::new(top.x + 4.0, top.y + 4.0, top.w - 8.0, top.h - 8.0), 6.0, pal.hover);
    }
    c.p.text("令牌过期时在内存中续期", Rect::new(x0 + 14.0, y + 12.0, w - 80.0, 20.0), font(13.0, REGULAR, Align::Left), pal.text);
    c.p.text(desc, Rect::new(x0 + 14.0, y + 34.0, w - 28.0 - 52.0, desc_h + 2.0), desc_font, pal.text3);
    c.toggle(top.right() - 14.0, y + top_h / 2.0, i.cfg.antigravity_memory_refresh);
    let mut py = y + top_h;
    c.p.line(x0 + 14.0, py, x0 + w - 14.0, py, pal.divider, 1.0);
    c.p.text("显示的额度池", Rect::new(x0 + 14.0, py + 6.0, w - 28.0, 22.0), font(12.0, SEMIBOLD, Align::Left), pal.text2);
    py += 30.0;
    if pools.is_empty() {
        c.p.text("首次成功查询后在这里列出", Rect::new(x0 + 14.0, py, w - 28.0, 22.0), font(12.0, REGULAR, Align::Left), pal.text3);
    } else {
        for name in pools {
            let row = Rect::new(x0, py, w, pool_h);
            let hovered = c.hit(row, Action::TogglePool(name.clone()));
            if hovered {
                c.p.fill_round(Rect::new(row.x + 4.0, row.y + 2.0, row.w - 8.0, row.h - 4.0), 6.0, pal.hover);
            }
            c.p.text(name, Rect::new(x0 + 14.0, py, w - 80.0, pool_h), font(12.5, REGULAR, Align::Left), pal.text);
            c.toggle(row.right() - 14.0, py + pool_h / 2.0, i.cfg.pool_visible(name));
            py += pool_h;
        }
    }
    settings_footer(c, card.bottom() + 10.0)
}

fn settings_footer(c: &mut Ctx, y: f32) -> f32 {
    let pal = *c.pal();
    let mem = crate::ui::window::private_mb().map(|m| format!(" · 内存 {m:.1} MB")).unwrap_or_default();
    c.p.text(
        &format!("v{}{mem} · 配置 %APPDATA%\\AI-Usage-Lite", env!("CARGO_PKG_VERSION")),
        Rect::new(20.0, y, WIDTH - 40.0, 22.0),
        font(11.0, REGULAR, Align::Left),
        pal.text3,
    );
    y + 30.0
}

fn consent_header(c: &mut Ctx) {
    let pal = *c.pal();
    c.p.text("首次使用确认", Rect::new(20.0, 12.0, 240.0, 28.0), font(15.0, SEMIBOLD, Align::Left), pal.text);
}

fn consent_page(c: &mut Ctx) -> f32 {
    let pal = *c.pal();
    consent_header(c);
    let x0 = PAD;
    let w = WIDTH - 2.0 * PAD;
    let body = Font { wrap: true, ..font(12.0, REGULAR, Align::Left) };
    let items: [(&str, &str); 4] = [
        ("只读取本机已有登录", "Claude：~\\.claude\\.credentials.json 或 Claude 桌面版加密令牌缓存；Codex：~\\.codex\\auth.json；Antigravity：Windows 凭据 gemini:antigravity；DeepSeek：你用脚本保存的 Windows 凭据 AI-Usage-Lite/DeepSeek"),
        ("只访问官方额度接口", "api.anthropic.com/api/oauth/usage、chatgpt.com/backend-api/wham/usage、cloudcode-pa.googleapis.com、api.deepseek.com/user/balance（经系统代理）"),
        ("不消耗额度、不改登录", "不调用任何模型，不刷新或写回令牌，不上传到第三方，不记录令牌"),
        ("已知风险", "这些是各客户端内部使用的非公开接口，可能变更或限流；查询频率与官方客户端相当"),
    ];
    let mut y = 52.0;
    let inner = w - 28.0;
    let mut heights = Vec::new();
    let mut total = 12.0;
    for (_, text) in &items {
        let h = c.p.text_height(text, body, inner);
        heights.push(h);
        total += 20.0 + h + 12.0;
    }
    let card = Rect::new(x0, y, w, total);
    c.card_bg(card);
    y += 12.0;
    for ((title, text), h) in items.iter().zip(heights) {
        c.p.text(title, Rect::new(x0 + 14.0, y, inner, 20.0), font(13.0, SEMIBOLD, Align::Left), pal.text);
        c.p.text(text, Rect::new(x0 + 14.0, y + 20.0, inner, h + 2.0), body, pal.text2);
        y += 20.0 + h + 12.0;
    }
    y = card.bottom() + 14.0;
    let bw = (WIDTH - 32.0 - 8.0) / 2.0;
    let demo = Rect::new(16.0, y, bw, 34.0);
    let ok = Rect::new(16.0 + bw + 8.0, y, bw, 34.0);
    let hd = c.hit(demo, Action::DemoOnly);
    c.p.fill_round(demo, 6.0, if hd { pal.seg_sel } else { pal.seg_bg });
    c.p.text("仅看演示", demo, font(13.0, REGULAR, Align::Center), pal.text);
    let ho = c.hit(ok, Action::Consent);
    c.p.fill_round(ok, 6.0, if ho { pal.accent.alpha(0.85) } else { pal.accent });
    let on_accent = if pal.dark { Color::hex(0x101010) } else { Color::hex(0xFFFFFF) };
    c.p.text("开始查询", ok, font(13.0, SEMIBOLD, Align::Center), on_accent);
    y + 34.0 + 16.0
}

/// The tightest window of a service: lowest remaining share.
fn tightest(st: &Status) -> Option<&UsageWindow> {
    st.rows
        .iter()
        .filter_map(|r| if let Row::Win(w) = r { Some(w) } else { None })
        .min_by(|a, b| a.remaining().total_cmp(&b.remaining()))
}

fn mini_page(c: &mut Ctx) -> f32 {
    let pal = *c.pal();
    let i = c.i;
    let w = MINI_WIDTH;
    let ids: Vec<ProviderId> = ProviderId::ALL.into_iter().filter(|id| i.cfg.is_enabled(*id)).collect();
    let row_h = 44.0;
    let top = 8.0;
    let footer_h = 32.0;
    let height = top + row_h * ids.len().max(1) as f32 + footer_h;
    // The whole card opens the full panel; pushed first so rows can't shadow it.
    let hovered = c.hit(Rect::new(0.0, 0.0, w, height), Action::OpenFull);
    if ids.is_empty() {
        c.p.text("没有启用的服务", Rect::new(14.0, top, w - 28.0, row_h), font(12.5, REGULAR, Align::Left), pal.text2);
    }
    for (k, id) in ids.iter().enumerate() {
        let y = top + row_h * k as f32;
        if k > 0 {
            c.p.line(14.0, y, w - 14.0, y, pal.divider, 1.0);
        }
        let st = status_of(i, *id);
        // The icon opens the client; the rest of the card opens the full panel.
        c.launch_tile(Rect::new(14.0, y + 11.0, 22.0, 22.0), *id);
        c.p.text(id.name(), Rect::new(46.0, y + 5.0, 100.0, 18.0), font(13.0, SEMIBOLD, Align::Left), pal.text);
        let right = Rect::new(w - 14.0 - 90.0, y + 5.0, 90.0, 20.0);
        let sub = Rect::new(46.0, y + 23.0, w - 60.0, 16.0);
        if let Some(b) = balance_of(&st) {
            let low = balance_low(i.cfg, b);
            let color = if st.dim { pal.text2 } else if low { pal.danger } else { pal.brand(*id) };
            let wide = Rect::new(w - 14.0 - 120.0, right.y, 120.0, right.h);
            c.p.text(&format!("余额 {}", money(b, b.total)), wide, font(13.0, SEMIBOLD, Align::Right), color);
            let (line, color) = match &st.chip {
                Some((text, col)) => (text.clone(), *col),
                None if !b.available => (UNAVAILABLE.into(), pal.danger),
                None => match balance_days(i, b, st.dim) {
                    Some(d) => (days_text(d), pal.text2),
                    None => (st.sub.clone(), pal.text3),
                },
            };
            c.p.text(&line, sub, font(11.5, REGULAR, Align::Left), color);
            continue;
        }
        match (tightest(&st), &st.chip) {
            (Some(win), chip) => {
                let remaining = win.remaining();
                let value = if i.cfg.show_used { win.used.clamp(0.0, 100.0) } else { remaining };
                let color = if st.dim { pal.text2 } else { pal.usage(*id, remaining) };
                let label = format!("{} {}", super::view::short_label_cn(&win.label), fmt::percent(value));
                c.p.text(&label, right, font(13.0, SEMIBOLD, Align::Right), color);
                let when = fmt::reset(win.resets_at, i.now);
                let all: Vec<&UsageWindow> = st.rows.iter().filter_map(|r| if let Row::Win(w) = r { Some(w) } else { None }).collect();
                let exhaust = soonest_exhaust(i, all.iter().copied(), st.dim);
                let line = match chip {
                    Some((text, _)) => text.clone(),
                    None if exhaust.is_some() => exhaust_text(exhaust.unwrap()),
                    None if when.is_empty() => st.sub.clone(),
                    None if when.ends_with('后') => format!("{when}恢复"),
                    None => format!("{when} 恢复"),
                };
                let (color, weight) = match chip {
                    Some((_, c)) => (*c, REGULAR),
                    None if exhaust.is_some() => (pal.text2, REGULAR),
                    None => (pal.text3, REGULAR),
                };
                c.p.text(&line, sub, font(11.5, weight, Align::Left), color);
            }
            (None, Some((text, color))) => {
                c.p.text(text, right, font(12.0, SEMIBOLD, Align::Right), *color);
                c.p.text(&st.sub, sub, font(11.5, REGULAR, Align::Left), pal.text3);
            }
            (None, None) => c.p.text(&st.sub, sub, font(11.5, REGULAR, Align::Left), pal.text3),
        }
    }
    let fy = height - footer_h;
    c.p.line(14.0, fy, w - 14.0, fy, pal.divider, 1.0);
    let label = if i.demo { "演示数据 · 点击查看详情" } else { "点击查看详情" };
    c.p.text(label, Rect::new(0.0, fy, w, footer_h), font(11.5, REGULAR, Align::Center), if hovered { pal.accent } else { pal.text3 });
    height
}

/// Labels for the mini card: "5h" / "周" etc. keep the row short.
pub fn short_label_cn(label: &str) -> String {
    match label {
        "5 小时" => "5h".into(),
        "本周" => "本周".into(),
        "30 天" => "30天".into(),
        "今日" => "今日".into(),
        other => other.to_string(),
    }
}
