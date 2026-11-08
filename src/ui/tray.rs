//! Notification-area icon: one vertical bar per enabled service, filled to
//! its tightest remaining share. Rendered in software (no GPU, no files).

use windows::core::Result;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{
    CreateBitmap, CreateDIBSection, DeleteObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HGDIOBJ,
};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIM_SETVERSION,
    NOTIFYICONDATAW, NOTIFYICON_VERSION_4,
};
use windows::Win32::UI::WindowsAndMessaging::{CreateIconIndirect, DestroyIcon, HICON, ICONINFO};

pub const TRAY_ID: u32 = 1;

/// One bar per service: `value` is the tightest remaining share (0..=100),
/// `None` = no data (track only).
#[derive(Clone, Copy, Debug)]
pub struct Bar {
    pub id: crate::model::ProviderId,
    pub value: Option<f64>,
}

/// Brand colour of a service's bar. Codex is black on light backgrounds and
/// light grey on dark ones, where black would vanish.
pub fn brand_rgb(id: crate::model::ProviderId, dark_bg: bool) -> [f32; 3] {
    use crate::model::ProviderId::*;
    match (id, dark_bg) {
        (Claude, true) => [222.0, 124.0, 92.0],
        (Claude, false) => [201.0, 100.0, 66.0],
        (Codex, true) => [236.0, 236.0, 236.0],
        (Codex, false) => [26.0, 26.0, 26.0],
        // Google blue #4285F4 (Antigravity publishes no colour of its own).
        (Antigravity, true) => [102.0, 157.0, 246.0],
        (Antigravity, false) => [66.0, 133.0, 244.0],
        // DeepSeek blue #4D6BFE.
        (DeepSeek, true) => [112.0, 136.0, 255.0],
        (DeepSeek, false) => [77.0, 107.0, 254.0],
    }
}

/// Below this remaining share a bar turns red, whatever the brand.
pub const DANGER_BELOW: f64 = 10.0;

pub fn danger_rgb(dark_bg: bool) -> [f32; 3] {
    // The most conspicuous red: reserved for "almost out", never a brand colour.
    if dark_bg { [255.0, 69.0, 58.0] } else { [232.0, 17.0, 35.0] }
}

fn rgba(rgb: u32) -> [f32; 3] {
    [((rgb >> 16) & 255) as f32, ((rgb >> 8) & 255) as f32, (rgb & 255) as f32]
}

/// Pixels in BGRA, straight alpha, top-down.
pub fn render(size: i32, bars: &[Bar], taskbar_dark: bool) -> Vec<u8> {
    let s = size as f32;
    let n = bars.len().max(1) as f32;
    let fg = if taskbar_dark { rgba(0xFFFFFF) } else { rgba(0x1B1B1B) };
    let danger = danger_rgb(taskbar_dark);
    // Geometry scaled from a 16 px design: bars 3 px wide, 2 px apart;
    // four bars (all services on) get slimmer so they still fit.
    let (bar_w, gap) = if bars.len() >= 4 {
        ((s * 0.15).max(2.0), (s * 0.08).max(1.0))
    } else {
        ((s * 0.19).max(2.5), (s * 0.11).max(1.5))
    };
    let total = bar_w * n + gap * (n - 1.0);
    let x0 = (s - total) / 2.0;
    let top = s * 0.12;
    let bottom = s * 0.88;
    let radius = bar_w / 2.0;

    let mut px = vec![0u8; (size * size * 4) as usize];
    const SS: usize = 4;
    for y in 0..size {
        for x in 0..size {
            let mut color = [0f32; 3];
            let mut alpha = 0f32;
            for sy in 0..SS {
                for sx in 0..SS {
                    let fx = x as f32 + (sx as f32 + 0.5) / SS as f32;
                    let fy = y as f32 + (sy as f32 + 0.5) / SS as f32;
                    for (k, bar) in bars.iter().enumerate() {
                        let bx = x0 + k as f32 * (bar_w + gap);
                        if !inside_round(fx, fy, bx, top, bar_w, bottom - top, radius) {
                            continue;
                        }
                        let (c, a) = match bar.value {
                            Some(rem) => {
                                let fill_top = bottom - (bottom - top) * (rem as f32 / 100.0).clamp(0.0, 1.0);
                                let c = if rem < DANGER_BELOW { danger } else { brand_rgb(bar.id, taskbar_dark) };
                                if fy >= fill_top { (c, 1.0) } else { (fg, 0.28) }
                            }
                            None => (fg, 0.28),
                        };
                        color = [color[0] + c[0] * a, color[1] + c[1] * a, color[2] + c[2] * a];
                        alpha += a;
                    }
                }
            }
            let samples = (SS * SS) as f32;
            let a = alpha / samples;
            let i = ((y * size + x) * 4) as usize;
            if alpha > 0.0 {
                px[i] = (color[2] / alpha).round() as u8;
                px[i + 1] = (color[1] / alpha).round() as u8;
                px[i + 2] = (color[0] / alpha).round() as u8;
            }
            px[i + 3] = (a.clamp(0.0, 1.0) * 255.0).round() as u8;
        }
    }
    px
}

fn inside_round(px: f32, py: f32, x: f32, y: f32, w: f32, h: f32, r: f32) -> bool {
    if px < x || px > x + w || py < y || py > y + h {
        return false;
    }
    // max/min instead of clamp: float rounding can make lo > hi.
    let cx = px.max(x + r).min((x + w - r).max(x + r));
    let cy = py.max(y + r).min((y + h - r).max(y + r));
    (px - cx).powi(2) + (py - cy).powi(2) <= r * r
}

pub fn make_icon(size: i32, bars: &[Bar], taskbar_dark: bool) -> Result<HICON> {
    let pixels = render(size, bars, taskbar_dark);
    unsafe {
        let mut info = BITMAPINFO::default();
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: size,
            biHeight: -size,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let color = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0)?;
        std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits as *mut u8, pixels.len());
        let mask_bits = vec![0u8; ((size + 15) / 16 * 2 * size) as usize];
        let mask = CreateBitmap(size, size, 1, 1, Some(mask_bits.as_ptr() as *const _));
        let ii = ICONINFO { fIcon: true.into(), xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
        let icon = CreateIconIndirect(&ii);
        let _ = DeleteObject(HGDIOBJ(color.0));
        let _ = DeleteObject(HGDIOBJ(mask.0));
        icon
    }
}

pub struct Tray {
    hwnd: HWND,
    callback: u32,
    icon: Option<HICON>,
}

fn tip_buf(text: &str) -> [u16; 128] {
    let mut buf = [0u16; 128];
    for (k, u) in text.encode_utf16().take(127).enumerate() {
        buf[k] = u;
    }
    buf
}

impl Tray {
    pub fn new(hwnd: HWND, callback: u32) -> Tray {
        Tray { hwnd, callback, icon: None }
    }

    fn data(&self) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: TRAY_ID,
            uCallbackMessage: self.callback,
            ..Default::default()
        }
    }

    /// Adds (or re-adds after an Explorer restart) the icon.
    pub fn add(&mut self, icon: HICON, tip: &str) {
        let mut nid = self.data();
        nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
        nid.hIcon = icon;
        nid.szTip = tip_buf(tip);
        unsafe {
            let _ = Shell_NotifyIconW(NIM_ADD, &nid);
            nid.Anonymous.uVersion = NOTIFYICON_VERSION_4;
            let _ = Shell_NotifyIconW(NIM_SETVERSION, &nid);
        }
        self.replace_icon(icon);
    }

    pub fn update(&mut self, icon: HICON, tip: &str) {
        let mut nid = self.data();
        nid.uFlags = NIF_ICON | NIF_TIP | NIF_SHOWTIP;
        nid.hIcon = icon;
        nid.szTip = tip_buf(tip);
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
        }
        self.replace_icon(icon);
    }

    fn replace_icon(&mut self, icon: HICON) {
        if let Some(old) = self.icon.replace(icon) {
            if old != icon {
                unsafe {
                    let _ = DestroyIcon(old);
                }
            }
        }
    }

    pub fn remove(&mut self) {
        let nid = self.data();
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
        }
        if let Some(old) = self.icon.take() {
            unsafe {
                let _ = DestroyIcon(old);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn full_bar_is_opaque_at_bottom_and_track_at_top() {
        let size = 16;
        let px = render(size, &[Bar { id: crate::model::ProviderId::Claude, value: Some(50.0) }], true);
        let at = |x: i32, y: i32| px[((y * size + x) * 4 + 3) as usize];
        assert_eq!(at(7, 11), 255);
        assert!(at(7, 4) > 30 && at(7, 4) < 120);
        assert_eq!(at(0, 0), 0);
    }

    #[test]
    fn four_bars_fit_inside_the_icon() {
        use crate::model::ProviderId::*;
        for size in [16, 20, 24, 32] {
            let bars: Vec<Bar> = [Claude, Codex, Antigravity, DeepSeek].into_iter().map(|id| Bar { id, value: Some(100.0) }).collect();
            let px = render(size, &bars, true);
            let at = |x: i32, y: i32| px[((y * size + x) * 4 + 3) as usize];
            let mid = size / 2;
            assert_eq!(at(0, mid), 0, "left edge clear at {size}");
            assert_eq!(at(size - 1, mid), 0, "right edge clear at {size}");
        }
    }
}

#[cfg(test)]
mod dump {
    /// `cargo test -- --ignored dump_tray` writes raw BGRA previews.
    #[test]
    #[ignore]
    fn dump_tray() {
        for (dark, tag) in [(true, "dark"), (false, "light")] {
            for size in [16, 20, 24, 32] {
                use crate::model::ProviderId::*;
                let bars = [super::Bar { id: Claude, value: Some(75.0) }, super::Bar { id: Codex, value: Some(14.0) }, super::Bar { id: Antigravity, value: Some(6.0) }];
                let px = super::render(size, &bars, dark);
                std::fs::write(format!("tray-{tag}-{size}.bgra"), px).unwrap();
            }
        }
    }
}
