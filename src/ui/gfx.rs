//! Thin Direct2D / DirectWrite layer. All coordinates are DIPs.
//!
//! The render target exists only while the panel is visible; hiding the
//! panel releases it so an idle app holds no GPU surfaces.

use std::cell::RefCell;
use std::collections::HashMap;

use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM};
use windows_numerics::Vector2;

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.right() && py >= self.y && py < self.bottom()
    }
    fn d2d(&self) -> D2D_RECT_F {
        D2D_RECT_F { left: self.x, top: self.y, right: self.right(), bottom: self.bottom() }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color(pub f32, pub f32, pub f32, pub f32);

impl Color {
    pub const fn hex(rgb: u32) -> Color {
        Color(((rgb >> 16) & 0xff) as f32 / 255.0, ((rgb >> 8) & 0xff) as f32 / 255.0, (rgb & 0xff) as f32 / 255.0, 1.0)
    }
    pub const fn hexa(rgb: u32, a: f32) -> Color {
        let c = Color::hex(rgb);
        Color(c.0, c.1, c.2, a)
    }
    pub fn alpha(self, a: f32) -> Color {
        Color(self.0, self.1, self.2, self.3 * a)
    }
    fn d2d(self) -> D2D1_COLOR_F {
        D2D1_COLOR_F { r: self.0, g: self.1, b: self.2, a: self.3 }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Align {
    Left,
    Center,
    Right,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Font {
    /// Size in tenths of a DIP, so the key stays hashable.
    pub size10: u32,
    pub weight: u16,
    pub align: Align,
    pub wrap: bool,
    pub icon: bool,
}

pub const REGULAR: u16 = 400;
pub const SEMIBOLD: u16 = 600;

pub fn font(size: f32, weight: u16, align: Align) -> Font {
    Font { size10: (size * 10.0) as u32, weight, align, wrap: false, icon: false }
}

pub fn icon_font(size: f32) -> Font {
    Font { size10: (size * 10.0) as u32, weight: REGULAR, align: Align::Center, wrap: false, icon: true }
}

pub struct Gfx {
    pub d2d: ID2D1Factory,
    dwrite: IDWriteFactory,
    formats: RefCell<HashMap<Font, IDWriteTextFormat>>,
    text_family: Vec<u16>,
    icon_family: Vec<u16>,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

impl Gfx {
    pub fn new() -> Result<Gfx> {
        unsafe {
            let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let has = |name: &str| -> bool {
                let mut coll: Option<IDWriteFontCollection> = None;
                if dwrite.GetSystemFontCollection(&mut coll, false).is_err() {
                    return false;
                }
                let Some(coll) = coll else { return false };
                let mut index = 0u32;
                let mut exists = windows::core::BOOL(0);
                let n = wide(name);
                coll.FindFamilyName(PCWSTR(n.as_ptr()), &mut index, &mut exists).is_ok() && exists.as_bool()
            };
            let text_family = if has("Segoe UI Variable Text") { "Segoe UI Variable Text" } else { "Segoe UI" };
            let icon_family = if has("Segoe Fluent Icons") { "Segoe Fluent Icons" } else { "Segoe MDL2 Assets" };
            Ok(Gfx { d2d, dwrite, formats: RefCell::new(HashMap::new()), text_family: wide(text_family), icon_family: wide(icon_family) })
        }
    }

    fn format(&self, f: Font) -> IDWriteTextFormat {
        if let Some(fmt) = self.formats.borrow().get(&f) {
            return fmt.clone();
        }
        let fmt = unsafe {
            let family = if f.icon { &self.icon_family } else { &self.text_family };
            let fmt = self
                .dwrite
                .CreateTextFormat(
                    PCWSTR(family.as_ptr()),
                    None,
                    DWRITE_FONT_WEIGHT(f.weight as i32),
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    f.size10 as f32 / 10.0,
                    w!("zh-CN"),
                )
                .expect("CreateTextFormat");
            let _ = fmt.SetTextAlignment(match f.align {
                Align::Left => DWRITE_TEXT_ALIGNMENT_LEADING,
                Align::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
                Align::Right => DWRITE_TEXT_ALIGNMENT_TRAILING,
            });
            let _ = fmt.SetParagraphAlignment(if f.wrap { DWRITE_PARAGRAPH_ALIGNMENT_NEAR } else { DWRITE_PARAGRAPH_ALIGNMENT_CENTER });
            let _ = fmt.SetWordWrapping(if f.wrap { DWRITE_WORD_WRAPPING_WRAP } else { DWRITE_WORD_WRAPPING_NO_WRAP });
            if !f.wrap {
                if let Ok(sign) = self.dwrite.CreateEllipsisTrimmingSign(&fmt) {
                    let trimming = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
                    let _ = fmt.SetTrimming(&trimming, &sign);
                }
            }
            fmt
        };
        self.formats.borrow_mut().insert(f, fmt.clone());
        fmt
    }

    /// Width and height of `text` laid out in at most `max_w`.
    pub fn measure(&self, text: &str, f: Font, max_w: f32) -> (f32, f32) {
        let t: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            let Ok(layout) = self.dwrite.CreateTextLayout(&t, &self.format(f), max_w, 10_000.0) else {
                return (0.0, 0.0);
            };
            let mut m = DWRITE_TEXT_METRICS::default();
            if layout.GetMetrics(&mut m).is_err() {
                return (0.0, 0.0);
            }
            (m.widthIncludingTrailingWhitespace, m.height)
        }
    }

    pub fn create_target(&self, hwnd: HWND, px_w: u32, px_h: u32, dpi: f32) -> Result<Target> {
        unsafe {
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_IGNORE },
                dpiX: dpi,
                dpiY: dpi,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let hwnd_props = D2D1_HWND_RENDER_TARGET_PROPERTIES {
                hwnd,
                pixelSize: D2D_SIZE_U { width: px_w.max(1), height: px_h.max(1) },
                presentOptions: D2D1_PRESENT_OPTIONS_NONE,
            };
            let rt = self.d2d.CreateHwndRenderTarget(&props, &hwnd_props)?;
            rt.SetDpi(dpi, dpi);
            let brush = rt.CreateSolidColorBrush(&Color(0.0, 0.0, 0.0, 1.0).d2d(), None)?;
            Ok(Target { rt, brush, images: RefCell::new(HashMap::new()) })
        }
    }
}

pub struct Target {
    pub rt: ID2D1HwndRenderTarget,
    brush: ID2D1SolidColorBrush,
    /// Device bitmaps for app icons, keyed by the source's address.
    images: RefCell<HashMap<usize, ID2D1Bitmap>>,
}

impl Target {
    pub fn resize(&self, px_w: u32, px_h: u32, dpi: f32) {
        unsafe {
            let _ = self.rt.Resize(&D2D_SIZE_U { width: px_w.max(1), height: px_h.max(1) });
            self.rt.SetDpi(dpi, dpi);
        }
    }
}

/// Drawing context. With `target == None` it only lays out (for sizing and
/// hit testing), so layout and painting can never disagree.
pub struct Painter<'a> {
    pub gfx: &'a Gfx,
    pub target: Option<&'a Target>,
}

impl Painter<'_> {
    fn brush(&self, c: Color) -> Option<&ID2D1SolidColorBrush> {
        let t = self.target?;
        unsafe { t.brush.SetColor(&c.d2d()) };
        Some(&t.brush)
    }

    /// Draws a premultiplied BGRA square image scaled into `r`.
    pub fn image(&self, img: &crate::ui::launch::AppIcon, r: Rect) {
        let Some(t) = self.target else { return };
        let key = img as *const _ as usize;
        let mut cache = t.images.borrow_mut();
        if !cache.contains_key(&key) {
            let props = D2D1_BITMAP_PROPERTIES {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
            };
            let size = D2D_SIZE_U { width: img.px, height: img.px };
            let Ok(bmp) = (unsafe { t.rt.CreateBitmap(size, Some(img.bgra.as_ptr() as *const _), img.px * 4, &props) }) else { return };
            cache.insert(key, bmp);
        }
        if let Some(bmp) = cache.get(&key) {
            unsafe { t.rt.DrawBitmap(bmp, Some(&r.d2d()), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None) };
        }
    }

    /// Shifts everything drawn afterwards up by `dy` (scrolling).
    pub fn set_offset(&self, dy: f32) {
        if let Some(t) = self.target {
            let m = windows_numerics::Matrix3x2::translation(0.0, -dy);
            unsafe { t.rt.SetTransform(&m) };
        }
    }

    pub fn clear(&self, c: Color) {
        if let Some(t) = self.target {
            unsafe { t.rt.Clear(Some(&c.d2d())) };
        }
    }

    pub fn fill_round(&self, r: Rect, radius: f32, c: Color) {
        if c.3 <= 0.0 {
            return;
        }
        if let (Some(t), Some(b)) = (self.target, self.brush(c)) {
            let rr = D2D1_ROUNDED_RECT { rect: r.d2d(), radiusX: radius, radiusY: radius };
            unsafe { t.rt.FillRoundedRectangle(&rr, b) };
        }
    }

    pub fn stroke_round(&self, r: Rect, radius: f32, c: Color, width: f32) {
        if let (Some(t), Some(b)) = (self.target, self.brush(c)) {
            let h = width / 2.0;
            let rr = D2D1_ROUNDED_RECT {
                rect: Rect::new(r.x + h, r.y + h, r.w - width, r.h - width).d2d(),
                radiusX: radius,
                radiusY: radius,
            };
            unsafe { t.rt.DrawRoundedRectangle(&rr, b, width, None) };
        }
    }

    pub fn fill_circle(&self, cx: f32, cy: f32, r: f32, c: Color) {
        if let (Some(t), Some(b)) = (self.target, self.brush(c)) {
            let e = D2D1_ELLIPSE { point: Vector2 { X: cx, Y: cy }, radiusX: r, radiusY: r };
            unsafe { t.rt.FillEllipse(&e, b) };
        }
    }

    pub fn line(&self, x0: f32, y0: f32, x1: f32, y1: f32, c: Color, width: f32) {
        if let (Some(t), Some(b)) = (self.target, self.brush(c)) {
            unsafe { t.rt.DrawLine(Vector2 { X: x0, Y: y0 }, Vector2 { X: x1, Y: y1 }, b, width, None) };
        }
    }

    pub fn text(&self, s: &str, r: Rect, f: Font, c: Color) {
        if let (Some(t), Some(b)) = (self.target, self.brush(c)) {
            let s: Vec<u16> = s.encode_utf16().collect();
            unsafe {
                t.rt.DrawText(
                    &s,
                    &self.gfx.format(f),
                    &r.d2d(),
                    b,
                    D2D1_DRAW_TEXT_OPTIONS_CLIP,
                    DWRITE_MEASURING_MODE_NATURAL,
                )
            };
        }
    }

    pub fn measure(&self, s: &str, f: Font) -> f32 {
        self.gfx.measure(s, f, 10_000.0).0
    }

    pub fn text_height(&self, s: &str, f: Font, max_w: f32) -> f32 {
        let f = Font { wrap: true, ..f };
        self.gfx.measure(s, f, max_w).1
    }

    /// Filled polygon/bezier path, points in DIPs. `curves` are cubic
    /// segments (c1, c2, end) following `start`.
    pub fn fill_path(&self, start: (f32, f32), curves: &[[(f32, f32); 3]], c: Color) {
        let (Some(t), Some(b)) = (self.target, self.brush(c)) else { return };
        unsafe {
            let Ok(path) = self.gfx.d2d.CreatePathGeometry() else { return };
            let Ok(sink) = path.Open() else { return };
            sink.BeginFigure(Vector2 { X: start.0, Y: start.1 }, D2D1_FIGURE_BEGIN_FILLED);
            for seg in curves {
                let bz = D2D1_BEZIER_SEGMENT {
                    point1: Vector2 { X: seg[0].0, Y: seg[0].1 },
                    point2: Vector2 { X: seg[1].0, Y: seg[1].1 },
                    point3: Vector2 { X: seg[2].0, Y: seg[2].1 },
                };
                sink.AddBezier(&bz);
            }
            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
            let _ = sink.Close();
            t.rt.FillGeometry(&path, b, None);
        }
    }

    /// Open polyline stroke.
    pub fn stroke_poly(&self, pts: &[(f32, f32)], c: Color, width: f32) {
        let (Some(t), Some(b)) = (self.target, self.brush(c)) else { return };
        if pts.len() < 2 {
            return;
        }
        unsafe {
            let Ok(path) = self.gfx.d2d.CreatePathGeometry() else { return };
            let Ok(sink) = path.Open() else { return };
            sink.BeginFigure(Vector2 { X: pts[0].0, Y: pts[0].1 }, D2D1_FIGURE_BEGIN_HOLLOW);
            for p in &pts[1..] {
                sink.AddLine(Vector2 { X: p.0, Y: p.1 });
            }
            sink.EndFigure(D2D1_FIGURE_END_OPEN);
            let _ = sink.Close();
            let props = D2D1_STROKE_STYLE_PROPERTIES {
                startCap: D2D1_CAP_STYLE_ROUND,
                endCap: D2D1_CAP_STYLE_ROUND,
                dashCap: D2D1_CAP_STYLE_ROUND,
                lineJoin: D2D1_LINE_JOIN_ROUND,
                miterLimit: 10.0,
                dashStyle: D2D1_DASH_STYLE_SOLID,
                dashOffset: 0.0,
            };
            let style = self.gfx.d2d.CreateStrokeStyle(&props, None).ok();
            t.rt.DrawGeometry(&path, b, width, style.as_ref());
        }
    }
}
