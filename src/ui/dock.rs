//! Edge-docked floating strip ("悬浮条").
//!
//! A per-pixel-alpha layered window. At rest only a thin sliver shows at the
//! screen edge; on hover it slides out into a capsule with one bar per
//! service. Rendering is pure software into a small premultiplied bitmap,
//! so it holds no GPU resources and costs nothing while idle.

use windows::Win32::Foundation::{COLORREF, HWND, POINT, RECT, SIZE};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, GetMonitorInfoW, MonitorFromPoint, ReleaseDC,
    SelectObject, AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS,
    HGDIOBJ, MONITORINFO, MONITOR_DEFAULTTOPRIMARY,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{UpdateLayeredWindow, ULW_ALPHA};

use super::tray::{brand_rgb, danger_rgb, Bar, DANGER_BELOW};

/// Capsule size in DIPs.
pub const CAP_W: f32 = 30.0;
pub const CAP_H: f32 = 78.0;
/// Gap between the expanded capsule and the screen edge.
pub const GAP: f32 = 5.0;
/// Width of the resting sliver.
pub const PEEK: f32 = 5.0;

pub struct Drag {
    pub start_cursor: POINT,
    pub start_pos: POINT,
    pub moved: bool,
}

pub struct Dock {
    pub hwnd: HWND,
    /// 0 = resting sliver, 1 = full capsule.
    pub progress: f32,
    pub target: f32,
    pub right: bool,
    pub y_frac: f32,
    pub hover: bool,
    pub drag: Option<Drag>,
    pub hidden_for_fullscreen: bool,
    pub dpi: u32,
    /// Window rectangle in screen pixels.
    pub rect: RECT,
}

fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Signed distance to a rounded rectangle (negative inside).
fn sd_round_rect(px: f32, py: f32, x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> f32 {
    let cx = (x0 + x1) / 2.0;
    let cy = (y0 + y1) / 2.0;
    let hx = (x1 - x0) / 2.0 - r;
    let hy = (y1 - y0) / 2.0 - r;
    let qx = (px - cx).abs() - hx;
    let qy = (py - cy).abs() - hy;
    let ox = qx.max(0.0);
    let oy = qy.max(0.0);
    (ox * ox + oy * oy).sqrt() + qx.max(qy).min(0.0) - r
}

/// The capsule rectangle (pixels, window-relative) at a given progress.
pub fn capsule_rect(w: f32, h: f32, scale: f32, progress: f32, right: bool) -> (f32, f32, f32, f32) {
    let t = ease(progress);
    let width = lerp(PEEK * scale, CAP_W * scale, t);
    let height = lerp(h * 0.62, h, t);
    let edge_gap = lerp(0.0, GAP * scale, t);
    let (x0, x1) = if right { (w - edge_gap - width, w - edge_gap) } else { (edge_gap, edge_gap + width) };
    let y0 = (h - height) / 2.0;
    (x0, y0, x1, y0 + height)
}

/// Premultiplied BGRA, top-down.
pub fn render(w: i32, h: i32, scale: f32, progress: f32, right: bool, bars: &[Bar], dark: bool) -> Vec<u8> {
    let (wf, hf) = (w as f32, h as f32);
    let (x0, y0, x1, y1) = capsule_rect(wf, hf, scale, progress, right);
    let radius = ((x1 - x0).min(y1 - y0)) / 2.0;
    let t = ease(progress);
    let bg: [f32; 3] = if dark { [38.0, 38.0, 38.0] } else { [250.0, 250.0, 250.0] };
    let bg_a = lerp(0.6, 0.95, t);
    // Resting sliver: a soft, translucent Claude orange (red when nearly out).
    let sliver: [f32; 3] = if dark { [226.0, 150.0, 118.0] } else { [232.0, 152.0, 120.0] };
    let border: [f32; 3] = if dark { [255.0, 255.0, 255.0] } else { [0.0, 0.0, 0.0] };
    let border_a = if dark { 0.16 } else { 0.12 };
    let fg: [f32; 3] = if dark { [242.0, 242.0, 242.0] } else { [30.0, 30.0, 30.0] };
    let danger = danger_rgb(dark);
    // At rest the sliver turns red only when some service is nearly out;
    // amber is avoided so it can't be mistaken for Claude's brand orange.
    let tight = bars.iter().filter_map(|b| b.value).fold(f64::INFINITY, f64::min);
    let sliver_tint = if tight < DANGER_BELOW { Some(danger) } else { None };

    let n = bars.len().max(1) as f32;
    let bar_w = 3.6 * scale;
    let bar_gap = 3.2 * scale;
    let total = bar_w * n + bar_gap * (n - 1.0);
    let bx0 = (x0 + x1) / 2.0 - total / 2.0;
    let by0 = y0 + 15.0 * scale;
    let by1 = y1 - 15.0 * scale;
    let bars_alpha = ((t - 0.45) / 0.55).clamp(0.0, 1.0);

    let mut px = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let d = sd_round_rect(fx, fy, x0, y0, x1, y1, radius);
            let cov = (0.5 - d).clamp(0.0, 1.0);
            if cov <= 0.0 {
                // Keep the gap between capsule and screen edge hit-testable
                // (alpha 1/255, invisible) so the cursor resting at the very
                // edge doesn't "leave" the strip and make it flicker.
                let in_gap = if right { fx >= x1 - radius } else { fx <= x0 + radius };
                if t > 0.02 && fy >= y0 && fy <= y1 && in_gap {
                    px[((y * w + x) * 4 + 3) as usize] = 1;
                }
                continue;
            }
            // Base fill with a 1px inner border.
            let mut c = bg;
            if t < 0.5 {
                let tint = sliver_tint.unwrap_or(sliver);
                let k = 1.0 - t * 2.0;
                c = [lerp(c[0], tint[0], k), lerp(c[1], tint[1], k), lerp(c[2], tint[2], k)];
            }
            let mut a = if sliver_tint.is_some() { lerp(0.85, bg_a, t.min(1.0)) } else { bg_a };
            let edge = (d + 1.2).clamp(0.0, 1.0);
            c = [lerp(c[0], border[0], edge * border_a), lerp(c[1], border[1], edge * border_a), lerp(c[2], border[2], edge * border_a)];
            a = lerp(a, 1.0, edge * 0.3);
            // Bars.
            if bars_alpha > 0.0 {
                for (k, bar) in bars.iter().enumerate() {
                    let bx = bx0 + k as f32 * (bar_w + bar_gap);
                    let bd = sd_round_rect(fx, fy, bx, by0, bx + bar_w, by1, bar_w / 2.0);
                    let bcov = (0.5 - bd).clamp(0.0, 1.0);
                    if bcov <= 0.0 {
                        continue;
                    }
                    let (col, ba) = match bar.value {
                        Some(rem) => {
                            let fill_top = by1 - (by1 - by0) * (rem as f32 / 100.0).clamp(0.0, 1.0);
                            let col = if rem < DANGER_BELOW { danger } else { brand_rgb(bar.id, dark) };
                            let inside = ((fy - fill_top) + 0.5).clamp(0.0, 1.0);
                            (col, lerp(0.22, 1.0, inside))
                        }
                        None => (fg, 0.22),
                    };
                    let k = bcov * ba * bars_alpha;
                    c = [lerp(c[0], col[0], k), lerp(c[1], col[1], k), lerp(c[2], col[2], k)];
                    a = lerp(a, 1.0, k);
                }
            }
            let alpha = (a * cov).clamp(0.0, 1.0);
            let i = ((y * w + x) * 4) as usize;
            px[i] = (c[2] * alpha).round() as u8;
            px[i + 1] = (c[1] * alpha).round() as u8;
            px[i + 2] = (c[0] * alpha).round() as u8;
            px[i + 3] = (alpha * 255.0).round() as u8;
        }
    }
    px
}

impl Dock {
    pub fn scale(&self) -> f32 {
        self.dpi as f32 / 96.0
    }

    pub fn size_px(&self) -> (i32, i32) {
        let s = self.scale();
        (((CAP_W + GAP + 1.0) * s).ceil() as i32, (CAP_H * s).ceil() as i32)
    }

    /// Work area and DPI of the monitor containing `pt`.
    pub fn monitor_at(pt: POINT) -> (RECT, u32) {
        unsafe {
            let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTOPRIMARY);
            let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            let _ = GetMonitorInfoW(mon, &mut info);
            let (mut dx, mut dy) = (96u32, 96u32);
            let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
            (info.rcWork, dx.max(96))
        }
    }

    /// Snap to the left/right edge of the monitor around `anchor`.
    pub fn place(&mut self, anchor: POINT) {
        let (work, dpi) = Dock::monitor_at(anchor);
        self.dpi = dpi;
        let (w, h) = self.size_px();
        let span = ((work.bottom - work.top) - h).max(1);
        let y = work.top + (self.y_frac.clamp(0.0, 1.0) * span as f32).round() as i32;
        let x = if self.right { work.right - w } else { work.left };
        self.rect = RECT { left: x, top: y, right: x + w, bottom: y + h };
    }

    /// The capsule's on-screen rectangle when fully expanded.
    pub fn expanded_screen_rect(&self) -> RECT {
        let (w, h) = self.size_px();
        let (x0, y0, x1, y1) = capsule_rect(w as f32, h as f32, self.scale(), 1.0, self.right);
        RECT {
            left: self.rect.left + x0 as i32,
            top: self.rect.top + y0 as i32,
            right: self.rect.left + x1 as i32,
            bottom: self.rect.top + y1 as i32,
        }
    }

    pub fn paint(&self, bars: &[Bar], dark: bool) {
        let (w, h) = self.size_px();
        let progress = if self.drag.as_ref().is_some_and(|d| d.moved) { 1.0 } else { self.progress };
        let pixels = render(w, h, self.scale(), progress, self.right, bars, dark);
        unsafe {
            let screen = GetDC(None);
            let mem = CreateCompatibleDC(Some(screen));
            let mut info = BITMAPINFO::default();
            info.bmiHeader = BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            };
            let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
            if let Ok(bmp) = CreateDIBSection(Some(mem), &info, DIB_RGB_COLORS, &mut bits, None, 0) {
                std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits as *mut u8, pixels.len());
                let old = SelectObject(mem, HGDIOBJ(bmp.0));
                let pos = POINT { x: self.rect.left, y: self.rect.top };
                let size = SIZE { cx: w, cy: h };
                let src = POINT { x: 0, y: 0 };
                let blend = BLENDFUNCTION {
                    BlendOp: AC_SRC_OVER as u8,
                    BlendFlags: 0,
                    SourceConstantAlpha: 255,
                    AlphaFormat: AC_SRC_ALPHA as u8,
                };
                let _ = UpdateLayeredWindow(self.hwnd, Some(screen), Some(&pos), Some(&size), Some(mem), Some(&src), COLORREF(0), Some(&blend), ULW_ALPHA);
                SelectObject(mem, old);
                let _ = DeleteObject(HGDIOBJ(bmp.0));
            }
            let _ = DeleteDC(mem);
            ReleaseDC(None, screen);
        }
    }

    /// Advance the slide animation one frame. Returns true while animating.
    pub fn step(&mut self) -> bool {
        let delta = 0.18;
        if (self.progress - self.target).abs() <= delta {
            self.progress = self.target;
            false
        } else {
            self.progress += if self.target > self.progress { delta } else { -delta };
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ProviderId::*;
    fn b(id: crate::model::ProviderId, v: Option<f64>) -> Bar {
        Bar { id, value: v }
    }
    #[test]
    fn resting_sliver_is_thin_and_flush_to_edge() {
        let (w, h) = (36, 78);
        let px = render(w, h, 1.0, 0.0, true, &[b(Claude, Some(80.0)), b(Codex, Some(50.0))], true);
        let a = |x: i32, y: i32| px[((y * w + x) * 4 + 3) as usize];
        assert!(a(w - 2, h / 2) > 110, "translucent sliver still visible at the right edge");
        assert_eq!(a(w - 12, h / 2), 0, "rest of the window is transparent (click-through)");
    }
    #[test]
    fn expanded_capsule_shows_bars() {
        let (w, h) = (36, 78);
        let px = render(w, h, 1.0, 1.0, false, &[b(Claude, Some(100.0))], true);
        let a = |x: i32, y: i32| px[((y * w + x) * 4 + 3) as usize];
        assert!(a(GAP as i32 + 15, h / 2) > 240, "capsule body opaque");
        assert!(a(1, h / 2) <= 1, "gap to the edge is invisible");
        assert_eq!(a(1, h / 2), 1, "but still hit-testable");
    }
    #[test]
    fn rendering_never_panics_at_any_scale() {
        for s in [1.0f32, 1.25, 1.5, 1.75, 2.0, 2.5] {
            let w = ((CAP_W + GAP + 1.0) * s).ceil() as i32;
            let h = (CAP_H * s).ceil() as i32;
            for p in [0.0, 0.3, 0.7, 1.0] {
                let _ = render(w, h, s, p, true, &[b(Claude, Some(5.0)), b(Codex, None), b(Antigravity, Some(25.0))], false);
            }
        }
    }
}
