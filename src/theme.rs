//! Runtime color palette shared by every GDI surface.
//!
//! `paint.rs` used to hard-code one dark palette. Themes need the same colors
//! in several places (main window, lyric child, playback flyout, native EDIT
//! controls), so the values now live in [`Palette`], derived from
//! [`crate::config::ThemeSettings`] once per frame. Brushes are still cached by
//! the painter, so switching themes only costs one repaint.

use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::{ThemeMode, ThemeSettings};
use windows::Win32::Foundation::COLORREF;

/// Accent presets offered by the settings page. The first entry matches the
/// historical Bamboo green so existing users keep the same look.
pub const ACCENT_PRESETS: [(&str, [u8; 3]); 6] = [
    ("竹绿", [127, 211, 168]),
    ("海蓝", [104, 140, 255]),
    ("紫罗兰", [167, 139, 250]),
    ("珊瑚", [251, 146, 120]),
    ("琥珀", [245, 190, 90]),
    ("青碧", [120, 200, 210]),
];

/// Labels for the segmented accent picker (same order as [`ACCENT_PRESETS`]).
pub const ACCENT_NAMES: [&str; 6] = ["竹绿", "海蓝", "紫罗兰", "珊瑚", "琥珀", "青碧"];

/// Number of list rows per viewport at each density.
pub fn density_row_height(settings: &crate::config::ThemeSettings) -> f32 {
    match settings.list_density {
        crate::config::ListDensity::Compact => 34.0,
        crate::config::ListDensity::Comfortable => crate::layout::ROW_HEIGHT,
        crate::config::ListDensity::Spacious => 48.0,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub dark: bool,
    pub background: COLORREF,
    pub sidebar: COLORREF,
    pub player: COLORREF,
    pub cover: COLORREF,
    pub nav_active: COLORREF,
    pub accent: COLORREF,
    pub text: COLORREF,
    pub muted: COLORREF,
    pub divider: COLORREF,
    pub row_selected: COLORREF,
    pub scroll_track: COLORREF,
    pub scroll_thumb: COLORREF,
    pub accent_text: COLORREF,
    pub search_bg: COLORREF,
    pub button_bg: COLORREF,
    pub button_hover: COLORREF,
    pub button_pressed: COLORREF,
    pub button_active: COLORREF,
    pub button_active_hover: COLORREF,
    pub explicit_border: COLORREF,
    pub nav_hover: COLORREF,
    pub row_hover: COLORREF,
    pub nav_pressed: COLORREF,
    pub row_pressed: COLORREF,
    pub scroll_thumb_hover: COLORREF,
    pub card: COLORREF,
    pub panel: COLORREF,
    pub button_disabled: COLORREF,
    pub text_disabled: COLORREF,
    pub play_button: COLORREF,
    pub play_hover: COLORREF,
}

impl Palette {
    /// Build the palette for the current settings. `dark` is the resolved
    /// light/dark decision (`ThemeMode::System` is resolved by the caller).
    pub fn new(theme: &ThemeSettings, dark: bool) -> Self {
        let accent = theme.accent;
        if dark {
            Self {
                dark,
                background: rgb(27, 28, 31),
                sidebar: rgb(34, 36, 40),
                player: rgb(37, 39, 43),
                cover: rgb(48, 51, 56),
                nav_active: mix(rgb(34, 36, 40), accent, 0.28),
                accent: rgb3(accent),
                text: rgb(232, 235, 238),
                muted: rgb(158, 166, 176),
                divider: rgb(51, 54, 59),
                row_selected: mix(rgb(27, 28, 31), accent, 0.17),
                scroll_track: rgb(41, 43, 48),
                scroll_thumb: rgb(92, 98, 106),
                accent_text: lighten(accent, 0.22),
                search_bg: rgb(22, 23, 26),
                button_bg: rgb(45, 48, 53),
                button_hover: rgb(58, 62, 69),
                button_pressed: mix(rgb(27, 28, 31), accent, 0.33),
                button_active: mix(rgb(27, 28, 31), accent, 0.24),
                button_active_hover: mix(rgb(27, 28, 31), accent, 0.38),
                explicit_border: rgb(70, 75, 83),
                nav_hover: rgb(40, 43, 48),
                row_hover: rgb(37, 40, 45),
                nav_pressed: mix(rgb(27, 28, 31), accent, 0.30),
                row_pressed: mix(rgb(27, 28, 31), accent, 0.26),
                scroll_thumb_hover: rgb(126, 134, 144),
                card: rgb(33, 35, 40),
                panel: rgb(29, 31, 35),
                button_disabled: rgb(38, 40, 44),
                text_disabled: rgb(108, 114, 122),
                play_button: mix(rgb(27, 28, 31), accent, 0.21),
                play_hover: mix(rgb(27, 28, 31), accent, 0.33),
            }
        } else {
            Self {
                dark,
                background: rgb(244, 245, 247),
                sidebar: rgb(235, 237, 240),
                player: rgb(250, 250, 252),
                cover: rgb(224, 227, 232),
                nav_active: mix(rgb(235, 237, 240), accent, 0.26),
                accent: rgb3(accent),
                text: rgb(32, 35, 40),
                muted: rgb(98, 106, 116),
                divider: rgb(206, 210, 217),
                row_selected: mix(rgb(255, 255, 255), accent, 0.18),
                scroll_track: rgb(229, 231, 236),
                scroll_thumb: rgb(168, 175, 184),
                accent_text: darken(accent, 0.35),
                search_bg: rgb(255, 255, 255),
                button_bg: rgb(232, 235, 239),
                button_hover: rgb(220, 224, 230),
                button_pressed: mix(rgb(255, 255, 255), accent, 0.34),
                button_active: mix(rgb(255, 255, 255), accent, 0.24),
                button_active_hover: mix(rgb(255, 255, 255), accent, 0.36),
                explicit_border: rgb(196, 201, 209),
                nav_hover: rgb(226, 230, 235),
                row_hover: rgb(233, 236, 240),
                nav_pressed: mix(rgb(255, 255, 255), accent, 0.30),
                row_pressed: mix(rgb(255, 255, 255), accent, 0.26),
                scroll_thumb_hover: rgb(120, 128, 138),
                card: rgb(255, 255, 255),
                panel: rgb(248, 249, 251),
                button_disabled: rgb(238, 240, 243),
                text_disabled: rgb(158, 164, 173),
                play_button: mix(rgb(255, 255, 255), accent, 0.22),
                play_hover: mix(rgb(255, 255, 255), accent, 0.34),
            }
        }
    }
}

/// Resolve `ThemeMode` (including "follow system") to a dark/light decision.
///
/// The registry is only read for `System`, at most once every two seconds, so
/// painting can call this freely.
pub fn resolve_dark(mode: ThemeMode) -> bool {
    match mode {
        ThemeMode::Dark => true,
        ThemeMode::Light => false,
        ThemeMode::System => system_prefers_dark(),
    }
}

fn system_prefers_dark() -> bool {
    static CACHE: AtomicI32 = AtomicI32::new(-1);
    static STAMP: AtomicU64 = AtomicU64::new(0);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let cached = CACHE.load(Ordering::Relaxed);
    let stamp = STAMP.load(Ordering::Relaxed);
    if cached >= 0 && now.saturating_sub(stamp) < 2 {
        return cached == 1;
    }
    let dark = read_apps_use_dark().unwrap_or(true);
    CACHE.store(dark as i32, Ordering::Relaxed);
    STAMP.store(now, Ordering::Relaxed);
    dark
}

/// `HKCU\...\Themes\Personalize\AppsUseLightTheme` is 1 for the light theme.
fn read_apps_use_dark() -> Option<bool> {
    use windows::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, RRF_RT_REG_DWORD, RegCloseKey, RegGetValueW,
        RegOpenKeyExW,
    };
    use windows::core::w;

    let mut key = HKEY::default();
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            Some(0),
            KEY_QUERY_VALUE,
            &mut key,
        )
    };
    if status.is_err() {
        return None;
    }
    let mut value = 1u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    let status = unsafe {
        RegGetValueW(
            key,
            None,
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut _),
            Some(&mut size),
        )
    };
    let _ = unsafe { RegCloseKey(key) };
    status.ok().ok().map(|_| value == 0)
}

const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF(((b as u32) << 16) | ((g as u32) << 8) | r as u32)
}

const fn rgb3(color: [u8; 3]) -> COLORREF {
    rgb(color[0], color[1], color[2])
}

/// Tint `base` toward `accent`; `t` in 0..=1.
fn mix(base: COLORREF, accent: [u8; 3], t: f32) -> COLORREF {
    let base = base.0;
    let r = (base & 0xff) as f32;
    let g = ((base >> 8) & 0xff) as f32;
    let b = ((base >> 16) & 0xff) as f32;
    let t = t.clamp(0.0, 1.0);
    rgb(
        (r + (accent[0] as f32 - r) * t).round().clamp(0.0, 255.0) as u8,
        (g + (accent[1] as f32 - g) * t).round().clamp(0.0, 255.0) as u8,
        (b + (accent[2] as f32 - b) * t).round().clamp(0.0, 255.0) as u8,
    )
}

fn lighten(color: [u8; 3], t: f32) -> COLORREF {
    rgb(
        (color[0] as f32 + (255.0 - color[0] as f32) * t).round() as u8,
        (color[1] as f32 + (255.0 - color[1] as f32) * t).round() as u8,
        (color[2] as f32 + (255.0 - color[2] as f32) * t).round() as u8,
    )
}

fn darken(color: [u8; 3], t: f32) -> COLORREF {
    rgb(
        (color[0] as f32 * (1.0 - t)).round() as u8,
        (color[1] as f32 * (1.0 - t)).round() as u8,
        (color[2] as f32 * (1.0 - t)).round() as u8,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accent_presets_include_the_default_green() {
        assert_eq!(ACCENT_PRESETS[0].1, ThemeSettings::default().accent);
    }

    #[test]
    fn light_palette_has_dark_text_and_light_background() {
        let theme = ThemeSettings {
            accent: ACCENT_PRESETS[1].1,
            ..ThemeSettings::default()
        };
        let light = Palette::new(&theme, false);
        let dark = Palette::new(&theme, true);
        assert!(light.background.0 & 0xff > 0xc0);
        assert!(light.text.0 & 0xff < 0x60);
        assert!(dark.background.0 & 0xff < 0x40);
        assert!(dark.text.0 & 0xff > 0xc0);
        assert_ne!(light.accent_text, dark.accent_text);
    }

    #[test]
    fn mix_moves_towards_the_accent() {
        let tinted = mix(rgb(0, 0, 0), [255, 255, 255], 0.5).0;
        assert_eq!(tinted & 0xff, 128);
        assert_eq!((tinted >> 8) & 0xff, 128);
    }
}
