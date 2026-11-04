//! Windows 11 flyout-like palettes. Only the remaining-usage level uses
//! saturated colour; everything else stays neutral.

use super::gfx::Color;

#[derive(Clone, Copy)]
pub struct Palette {
    pub dark: bool,
    pub bg: Color,
    pub card: Color,
    pub card_border: Color,
    pub hover: Color,
    pub pressed: Color,
    pub divider: Color,
    pub text: Color,
    pub text2: Color,
    pub text3: Color,
    pub track: Color,
    pub normal: Color,
    pub warn: Color,
    pub danger: Color,
    pub accent: Color,
    pub seg_bg: Color,
    pub seg_sel: Color,
    pub toggle_off: Color,
}

pub const DARK: Palette = Palette {
    dark: true,
    bg: Color::hex(0x202020),
    card: Color::hex(0x2B2B2B),
    card_border: Color::hexa(0xFFFFFF, 0.055),
    hover: Color::hexa(0xFFFFFF, 0.06),
    pressed: Color::hexa(0xFFFFFF, 0.04),
    divider: Color::hexa(0xFFFFFF, 0.07),
    text: Color::hex(0xF2F2F2),
    text2: Color::hexa(0xFFFFFF, 0.64),
    text3: Color::hexa(0xFFFFFF, 0.44),
    track: Color::hexa(0xFFFFFF, 0.10),
    normal: Color::hex(0x8DB1DE),
    warn: Color::hex(0xE6AC55),
    danger: Color::hex(0xFF453A),
    accent: Color::hex(0x8DB1DE),
    seg_bg: Color::hexa(0xFFFFFF, 0.045),
    seg_sel: Color::hexa(0xFFFFFF, 0.10),
    toggle_off: Color::hexa(0xFFFFFF, 0.62),
};

pub const LIGHT: Palette = Palette {
    dark: false,
    bg: Color::hex(0xF3F3F3),
    card: Color::hex(0xFFFFFF),
    card_border: Color::hexa(0x000000, 0.06),
    hover: Color::hexa(0x000000, 0.04),
    pressed: Color::hexa(0x000000, 0.06),
    divider: Color::hexa(0x000000, 0.07),
    text: Color::hex(0x1B1B1B),
    text2: Color::hexa(0x000000, 0.62),
    text3: Color::hexa(0x000000, 0.45),
    track: Color::hexa(0x000000, 0.09),
    normal: Color::hex(0x3E6CAE),
    warn: Color::hex(0xB8730A),
    danger: Color::hex(0xE81123),
    accent: Color::hex(0x3E6CAE),
    seg_bg: Color::hexa(0x000000, 0.045),
    seg_sel: Color::hex(0xFFFFFF),
    toggle_off: Color::hexa(0x000000, 0.55),
};

impl Palette {
    /// Brand colour of a service, shared by the strip, tray, mini card and
    /// panel so a bar means the same thing everywhere.
    pub fn brand(&self, id: crate::model::ProviderId) -> Color {
        let [r, g, b] = super::tray::brand_rgb(id, self.dark);
        Color(r / 255.0, g / 255.0, b / 255.0, 1.0)
    }

    /// Bar and percentage colour: the brand colour, red when nearly out.
    pub fn usage(&self, id: crate::model::ProviderId, remaining: f64) -> Color {
        if remaining < super::tray::DANGER_BELOW { self.danger } else { self.brand(id) }
    }
}

#[cfg(windows)]
fn read_dword(name: windows::core::PCWSTR) -> Option<u32> {
    use windows::core::w;
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
    let mut v = 0u32;
    let mut size = 4u32;
    let rc = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            name,
            RRF_RT_REG_DWORD,
            None,
            Some(&mut v as *mut u32 as *mut _),
            Some(&mut size),
        )
    };
    rc.is_ok().then_some(v)
}

/// Apps theme (read-only).
#[cfg(windows)]
pub fn apps_dark() -> bool {
    read_dword(windows::core::w!("AppsUseLightTheme")) == Some(0)
}

/// Taskbar theme, which decides the tray glyph colour.
#[cfg(windows)]
pub fn taskbar_dark() -> bool {
    read_dword(windows::core::w!("SystemUsesLightTheme")) != Some(1)
}
