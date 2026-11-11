//! Playback settings flyout (speed / equalizer / pitch).
//!
//! A small native child window anchored above the player bar's gear button.
//! It is a sibling of the lyric child and is raised with `HWND_TOP` while
//! open, so it can float over the lyric list. It owns hover/press painting and
//! posts [`WM_APP_PLAYER_ACTION`] to the parent, which turns each action into
//! a `Command`; the parent owns the open/closed state and the actual settings.

use std::collections::HashMap;

use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateCompatibleBitmap,
    CreateCompatibleDC, CreateFontW, CreateSolidBrush, DEFAULT_CHARSET, DRAW_TEXT_FORMAT, DT_CENTER,
    DT_LEFT, DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE, DT_VCENTER, DeleteDC, DeleteObject, DrawTextW,
    EndPaint, FillRect, HBRUSH, HBITMAP, HDC, HFONT, HGDIOBJ, OUT_DEFAULT_PRECIS,
    PAINTSTRUCT, RestoreDC, SRCCOPY, SaveDC, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GWLP_USERDATA, GetClientRect, HWND_TOP, SW_HIDE, SW_SHOWNA, SWP_NOACTIVATE,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, WM_APP, WM_ERASEBKGND, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MOUSEMOVE, WM_NCDESTROY, WM_PAINT, WINDOW_EX_STYLE, WS_CHILD,
    WS_CLIPSIBLINGS,
    GetWindowLongPtrW, PostMessageW, RegisterClassW,
};
use windows::core::{PCWSTR, Result, w};

use crate::app::App;
use crate::layout::Rect;
use crate::theme::Palette;
use crate::ui::ButtonState;

/// Posted to the parent when a flyout control is activated; `wparam` is one of
/// the `ACTION_*` constants below.
pub const WM_APP_PLAYER_ACTION: u32 = WM_APP + 3;

pub const ACTION_SPEED_DOWN: usize = 1;
pub const ACTION_SPEED_UP: usize = 2;
pub const ACTION_TOGGLE_EQ: usize = 3;
pub const ACTION_TOGGLE_PITCH: usize = 4;
pub const ACTION_OPEN_SETTINGS: usize = 5;
/// `lparam` carries volume × 1000 (0–1000).
pub const ACTION_SET_VOLUME: usize = 6;
/// Narrow-window overlay rows.
pub const ACTION_TOGGLE_DESKTOP_LYRICS: usize = 7;
pub const ACTION_TOGGLE_MINI_PLAYER: usize = 8;

const CLASS_NAME: PCWSTR = w!("BambooPlayerPanel");
const CONTROL_COUNT: usize = 5;
/// Hover index for the volume slider (kept separate from `controls`).
const VOLUME_CONTROL: usize = 5;

// Panel-local DIP layout. Must match `layout::compute_with`'s
// `player_panel` size (260 wide; 208 tall normally, 264 when narrow).
const VOLUME_TRACK: Rect = Rect::new(90.0, 52.0, 210.0, 56.0);
const VOLUME_HIT: Rect = Rect::new(84.0, 42.0, 216.0, 66.0);


struct State {
    parent: HWND,
    dpi: f32,
    visible: bool,
    speed: f32,
    eq: bool,
    pitch: bool,
    volume: f32,
    muted: bool,
    /// Narrow window: show the functions hidden from the player bar.
    narrow: bool,
    desktop_open: bool,
    mini_open: bool,
    hover: Option<usize>,
    pressed: Option<usize>,
    dragging_volume: bool,
    back_dc: Option<HDC>,
    back_bitmap: Option<HBITMAP>,
    back_old: Option<HGDIOBJ>,
    back_size: (i32, i32),
    fonts: HashMap<(u8, u16), HFONT>,
    font_dpi: f32,
    brushes: HashMap<u32, HBRUSH>,
    palette: Palette,
}

impl State {
    fn new(parent: HWND) -> Self {
        Self {
            parent,
            dpi: 96.0,
            visible: false,
            speed: 1.0,
            eq: false,
            pitch: true,
            volume: 1.0,
            muted: false,
            narrow: false,
            desktop_open: false,
            mini_open: false,
            hover: None,
            pressed: None,
            dragging_volume: false,
            back_dc: None,
            back_bitmap: None,
            back_old: None,
            back_size: (0, 0),
            fonts: HashMap::new(),
            font_dpi: 0.0,
            brushes: HashMap::new(),
            palette: Palette::new(&crate::config::ThemeSettings::default(), true),
        }
    }

    fn scale(&self) -> f32 {
        if self.dpi > 0.0 { self.dpi / 96.0 } else { 1.0 }
    }

    fn hide(&mut self, hwnd: HWND) {
        if self.visible {
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
            self.visible = false;
        }
        if self.dragging_volume {
            self.dragging_volume = false;
            unsafe {
                let _ = ReleaseCapture();
            }
        }
        self.hover = None;
        self.pressed = None;
    }

    fn controls() -> [Rect; CONTROL_COUNT] {
        [
            Rect::new(160.0, 74.0, 188.0, 102.0),  // speed -
            Rect::new(218.0, 74.0, 246.0, 102.0),  // speed +
            Rect::new(178.0, 108.0, 246.0, 136.0), // equalizer
            Rect::new(178.0, 142.0, 246.0, 170.0), // pitch preserve
            Rect::new(14.0, 176.0, 246.0, 200.0),  // open settings page
        ]
    }

    /// Rows that only exist when the player bar hides these functions
    /// (narrow window). They are still disabled until the overlays are wired.
    fn extras() -> [Rect; 2] {
        [
            Rect::new(14.0, 206.0, 246.0, 230.0),
            Rect::new(14.0, 234.0, 246.0, 258.0),
        ]
    }

    fn invalidate(&self, hwnd: HWND) {
        let _ = unsafe { windows::Win32::Graphics::Gdi::InvalidateRect(Some(hwnd), None, false) };
    }

    fn paint(&mut self, hwnd: HWND, hdc: HDC) {
        let mut client = RECT::default();
        let _ = unsafe { GetClientRect(hwnd, &mut client) };
        let width = client.right.max(1);
        let height = client.bottom.max(1);
        self.ensure_back_buffer(hdc, width, height);
        let Some(back) = self.back_dc else {
            return;
        };
        let scale = self.scale();
        let saved = unsafe { SaveDC(back) };
        unsafe {
            SetBkMode(back, TRANSPARENT);
        }
        self.fill(back, Rect::new(0.0, 0.0, width as f32 / scale, height as f32 / scale), self.palette.panel, scale);
        // 1 DIP border.
        self.fill(back, Rect::new(0.0, 0.0, width as f32 / scale, 1.0), self.palette.divider, scale);
        self.fill(
            back,
            Rect::new(0.0, height as f32 / scale - 1.0, width as f32 / scale, height as f32 / scale),
            self.palette.divider,
            scale,
        );
        self.fill(back, Rect::new(0.0, 0.0, 1.0, height as f32 / scale), self.palette.divider, scale);
        self.fill(
            back,
            Rect::new(width as f32 / scale - 1.0, 0.0, width as f32 / scale, height as f32 / scale),
            self.palette.divider,
            scale,
        );

        self.text(
            back,
            13.0,
            true,
            "播放设置",
            Rect::new(14.0, 10.0, 246.0, 34.0),
            self.palette.text,
            DT_LEFT.0 | DT_SINGLELINE.0 | DT_VCENTER.0 | DT_NOPREFIX.0,
            scale,
        );

        // Volume: the player bar hides its slider below 820 DIP, so the flyout
        // always carries one.
        self.text(
            back,
            11.5,
            false,
            "音量",
            Rect::new(14.0, 40.0, 86.0, 68.0),
            self.palette.text,
            DT_LEFT.0 | DT_SINGLELINE.0 | DT_VCENTER.0 | DT_NOPREFIX.0,
            scale,
        );
        let track = VOLUME_TRACK;
        let volume = self.volume.clamp(0.0, 1.0);
        self.fill(back, track, self.palette.button_bg, scale);
        let fill_right = track.left + track.width() * volume;
        self.fill(
            back,
            Rect::new(track.left, track.top, fill_right, track.bottom),
            if self.muted { self.palette.muted } else { self.palette.accent_text },
            scale,
        );
        let knob_color = if self.dragging_volume || self.hover == Some(VOLUME_CONTROL) {
            self.palette.text
        } else {
            self.palette.accent_text
        };
        self.fill(
            back,
            Rect::new(fill_right - 1.5, 46.0, fill_right + 1.5, 62.0),
            knob_color,
            scale,
        );
        let volume_text = if self.muted {
            "静音".to_string()
        } else {
            format!("{:.0}%", volume * 100.0)
        };
        self.text(
            back,
            11.5,
            false,
            &volume_text,
            Rect::new(214.0, 40.0, 246.0, 68.0),
            if self.muted { self.palette.muted } else { self.palette.accent_text },
            DT_RIGHT.0 | DT_SINGLELINE.0 | DT_VCENTER.0 | DT_NOPREFIX.0,
            scale,
        );

        let controls = Self::controls();
        self.text(
            back,
            11.5,
            false,
            "播放速度",
            Rect::new(14.0, 74.0, 120.0, 102.0),
            self.palette.text,
            DT_LEFT.0 | DT_SINGLELINE.0 | DT_VCENTER.0 | DT_NOPREFIX.0,
            scale,
        );
        self.button(back, 0, controls[0], "−", false, scale);
        self.text(
            back,
            11.5,
            false,
            &format!("{:.2}×", self.speed),
            Rect::new(188.0, 74.0, 218.0, 102.0),
            self.palette.accent_text,
            DT_CENTER.0 | DT_SINGLELINE.0 | DT_VCENTER.0 | DT_NOPREFIX.0,
            scale,
        );
        self.button(back, 1, controls[1], "+", false, scale);

        self.text(
            back,
            11.5,
            false,
            "均衡器",
            Rect::new(14.0, 108.0, 120.0, 136.0),
            self.palette.text,
            DT_LEFT.0 | DT_SINGLELINE.0 | DT_VCENTER.0 | DT_NOPREFIX.0,
            scale,
        );
        self.button(
            back,
            2,
            controls[2],
            if self.eq { "开" } else { "关" },
            self.eq,
            scale,
        );

        self.text(
            back,
            11.5,
            false,
            "保持音高",
            Rect::new(14.0, 142.0, 120.0, 170.0),
            self.palette.text,
            DT_LEFT.0 | DT_SINGLELINE.0 | DT_VCENTER.0 | DT_NOPREFIX.0,
            scale,
        );
        self.button(
            back,
            3,
            controls[3],
            if self.pitch { "开" } else { "关" },
            self.pitch,
            scale,
        );

        let hovered = self.hover == Some(4);
        self.text(
            back,
            11.5,
            false,
            "打开设置页 ›",
            controls[4],
            if hovered { self.palette.accent_text } else { self.palette.muted },
            DT_RIGHT.0 | DT_SINGLELINE.0 | DT_VCENTER.0 | DT_NOPREFIX.0,
            scale,
        );

        // Narrow window: the player bar hides these buttons; show the same
        // toggles here so they stay reachable at 640 DIP.
        if self.narrow {
            let rows = [
                ("桌面歌词", self.desktop_open, 6usize),
                ("迷你播放器", self.mini_open, 7usize),
            ];
            for (row, (label, active, index)) in Self::extras().into_iter().zip(rows) {
                self.button(back, index, row, label, active, scale);
                self.text(
                    back,
                    10.5,
                    false,
                    if active { "开" } else { "关" },
                    row.inset(8.0, 0.0),
                    if active { self.palette.accent_text } else { self.palette.muted },
                    DT_RIGHT.0 | DT_SINGLELINE.0 | DT_VCENTER.0 | DT_NOPREFIX.0,
                    scale,
                );
            }
        }

        unsafe {
            let _ = RestoreDC(back, saved);
            let _ = BitBlt(hdc, 0, 0, width, height, Some(back), 0, 0, SRCCOPY);
        }
    }

    fn button(
        &mut self,
        hdc: HDC,
        index: usize,
        area: Rect,
        label: &str,
        active: bool,
        scale: f32,
    ) {
        let state = if self.pressed == Some(index) {
            ButtonState::Pressed
        } else if self.hover == Some(index) {
            if active {
                ButtonState::HoverActive
            } else {
                ButtonState::Hover
            }
        } else if active {
            ButtonState::Active
        } else {
            ButtonState::Normal
        };
        let background = match state {
            ButtonState::Normal => self.palette.button_bg,
            ButtonState::Hover => self.palette.button_hover,
            ButtonState::HoverActive => self.palette.button_active,
            ButtonState::Pressed => self.palette.button_pressed,
            ButtonState::Active => self.palette.button_active,
        };
        self.fill(hdc, area, background, scale);
        self.text(
            hdc,
            11.5,
            false,
            label,
            area,
            match state {
                ButtonState::Normal => self.palette.muted,
                ButtonState::Pressed | ButtonState::HoverActive => self.palette.accent_text,
                _ => self.palette.text,
            },
            DT_CENTER.0 | DT_SINGLELINE.0 | DT_VCENTER.0 | DT_NOPREFIX.0,
            scale,
        );
    }

    fn set_volume_from_x(&mut self, x: f32) {
        let ratio = ((x - VOLUME_TRACK.left) / VOLUME_TRACK.width().max(1.0)).clamp(0.0, 1.0);
        if (self.volume - ratio).abs() < 0.001 {
            return;
        }
        self.volume = ratio;
        self.muted = false;
        unsafe {
            let _ = PostMessageW(
                Some(self.parent),
                WM_APP_PLAYER_ACTION,
                WPARAM(ACTION_SET_VOLUME),
                LPARAM((ratio * 1000.0).round() as isize),
            );
        }
    }

    fn on_move(&mut self, hwnd: HWND, x: f32, y: f32) {
        if self.dragging_volume {
            self.set_volume_from_x(x);
            self.invalidate(hwnd);
            return;
        }
        let hover = if VOLUME_HIT.contains(x, y) {
            Some(VOLUME_CONTROL)
        } else {
            let controls = Self::controls();
            controls
                .iter()
                .position(|rect| rect.contains(x, y))
                .or_else(|| {
                    if !self.narrow {
                        return None;
                    }
                    Self::extras()
                        .iter()
                        .position(|rect| rect.contains(x, y))
                        .map(|index| index + 6)
                })
        };
        if hover != self.hover {
            self.hover = hover;
            self.invalidate(hwnd);
        }
    }

    fn on_press(&mut self, hwnd: HWND, x: f32, y: f32) {
        if VOLUME_HIT.contains(x, y) {
            self.dragging_volume = true;
            self.hover = Some(VOLUME_CONTROL);
            unsafe {
                SetCapture(hwnd);
            }
            self.set_volume_from_x(x);
            self.invalidate(hwnd);
            return;
        }
        let controls = Self::controls();
        let pressed = controls.iter().position(|rect| rect.contains(x, y)).or_else(|| {
            if !self.narrow {
                return None;
            }
            Self::extras()
                .iter()
                .position(|rect| rect.contains(x, y))
                .map(|index| index + 6)
        });
        if pressed != self.pressed {
            self.pressed = pressed;
            self.invalidate(hwnd);
        }
    }

    fn on_release(&mut self, hwnd: HWND, x: f32, y: f32) {
        if self.dragging_volume {
            self.dragging_volume = false;
            unsafe {
                let _ = ReleaseCapture();
            }
            self.invalidate(hwnd);
            return;
        }
        let controls = Self::controls();
        let hit = controls.iter().position(|rect| rect.contains(x, y)).or_else(|| {
            if !self.narrow {
                return None;
            }
            Self::extras()
                .iter()
                .position(|rect| rect.contains(x, y))
                .map(|index| index + 6)
        });
        let pressed = self.pressed.take();
        self.invalidate(hwnd);
        let Some(pressed) = pressed else {
            return;
        };
        if hit != Some(pressed) {
            return;
        }
        let action = match pressed {
            0 => ACTION_SPEED_DOWN,
            1 => ACTION_SPEED_UP,
            2 => ACTION_TOGGLE_EQ,
            3 => ACTION_TOGGLE_PITCH,
            4 => ACTION_OPEN_SETTINGS,
            6 => ACTION_TOGGLE_DESKTOP_LYRICS,
            7 => ACTION_TOGGLE_MINI_PLAYER,
            _ => return,
        };
        unsafe {
            let _ = PostMessageW(
                Some(self.parent),
                WM_APP_PLAYER_ACTION,
                WPARAM(action),
                LPARAM(0),
            );
        }
    }

    fn ensure_back_buffer(&mut self, reference: HDC, width: i32, height: i32) {
        if self.back_dc.is_some() && self.back_size == (width, height) {
            return;
        }
        self.release_back_buffer();
        unsafe {
            let dc = CreateCompatibleDC(Some(reference));
            if dc.is_invalid() {
                return;
            }
            let bitmap = CreateCompatibleBitmap(reference, width, height);
            if bitmap.is_invalid() {
                let _ = DeleteDC(dc);
                return;
            }
            let old = SelectObject(dc, HGDIOBJ(bitmap.0));
            self.back_dc = Some(dc);
            self.back_bitmap = Some(bitmap);
            self.back_old = Some(old);
            self.back_size = (width, height);
        }
    }

    fn release_back_buffer(&mut self) {
        unsafe {
            if let Some(dc) = self.back_dc.take() {
                if let Some(old) = self.back_old.take() {
                    let _ = SelectObject(dc, old);
                }
                let _ = DeleteDC(dc);
            }
            if let Some(bitmap) = self.back_bitmap.take() {
                let _ = DeleteObject(HGDIOBJ(bitmap.0));
            }
            self.back_size = (0, 0);
        }
    }

    fn fill(&mut self, hdc: HDC, area: Rect, color: COLORREF, scale: f32) {
        if area.width() <= 0.0 || area.height() <= 0.0 {
            return;
        }
        let rect = RECT {
            left: (area.left * scale).floor() as i32,
            top: (area.top * scale).floor() as i32,
            right: (area.right * scale).ceil() as i32,
            bottom: (area.bottom * scale).ceil() as i32,
        };
        let brush = self.brush(color);
        unsafe {
            FillRect(hdc, &rect, brush);
        }
    }

    fn brush(&mut self, color: COLORREF) -> HBRUSH {
        if let Some(brush) = self.brushes.get(&color.0) {
            return *brush;
        }
        let brush = unsafe { CreateSolidBrush(color) };
        self.brushes.insert(color.0, brush);
        brush
    }

    fn font(&mut self, size: f32, bold: bool) -> HFONT {
        if (self.font_dpi - self.dpi).abs() > 0.5 {
            self.clear_fonts();
            self.font_dpi = self.dpi;
        }
        let key = (u8::from(bold), (size * 10.0).round() as u16);
        if let Some(font) = self.fonts.get(&key) {
            return *font;
        }
        let height = -((size * self.dpi / 96.0).round().max(1.0) as i32);
        let face: Vec<u16> = "Microsoft YaHei UI\0".encode_utf16().collect();
        let font = unsafe {
            CreateFontW(
                height,
                0,
                0,
                0,
                if bold { 600 } else { 400 },
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                CLEARTYPE_QUALITY,
                0,
                PCWSTR(face.as_ptr()),
            )
        };
        self.fonts.insert(key, font);
        font
    }

    fn clear_fonts(&mut self) {
        for (_, font) in self.fonts.drain() {
            unsafe {
                let _ = DeleteObject(HGDIOBJ(font.0));
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn text(
        &mut self,
        hdc: HDC,
        size: f32,
        bold: bool,
        text: &str,
        area: Rect,
        color: COLORREF,
        format: u32,
        scale: f32,
    ) {
        if text.is_empty() || area.width() <= 1.0 {
            return;
        }
        let font = self.font(size, bold);
        let mut rect = RECT {
            left: (area.left * scale).floor() as i32,
            top: (area.top * scale).floor() as i32,
            right: (area.right * scale).ceil() as i32,
            bottom: (area.bottom * scale).ceil() as i32,
        };
        let mut wide: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            SelectObject(hdc, HGDIOBJ(font.0));
            SetTextColor(hdc, color);
            SetBkMode(hdc, TRANSPARENT);
            DrawTextW(hdc, &mut wide, &mut rect, DRAW_TEXT_FORMAT(format));
        }
    }
}

impl Drop for State {
    fn drop(&mut self) {
        self.release_back_buffer();
        self.clear_fonts();
        for (_, brush) in self.brushes.drain() {
            unsafe {
                let _ = DeleteObject(HGDIOBJ(brush.0));
            }
        }
    }
}

pub struct PlayerPanel {
    hwnd: HWND,
}

impl PlayerPanel {
    pub fn new(parent: HWND) -> Result<Self> {
        unsafe {
            let instance = GetModuleHandleW(None)?;
            let class = windows::Win32::UI::WindowsAndMessaging::WNDCLASSW {
                hInstance: HINSTANCE(instance.0),
                lpszClassName: CLASS_NAME,
                lpfnWndProc: Some(child_proc),
                ..Default::default()
            };
            let _ = RegisterClassW(&class);
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                CLASS_NAME,
                w!(""),
                WS_CHILD | WS_CLIPSIBLINGS,
                0,
                0,
                10,
                10,
                Some(parent),
                None,
                Some(HINSTANCE(instance.0)),
                None,
            )?;
            let state = Box::new(State::new(parent));
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(state) as isize);
            Ok(Self { hwnd })
        }
    }

    pub fn update(&mut self, app: &App) {
        let Some(state) = state_mut(self.hwnd) else {
            return;
        };
        state.dpi = app.dpi;
        let palette = app.palette();
        if state.palette != palette {
            state.palette = palette;
            state.invalidate(self.hwnd);
        }
        if !app.player_panel_open {
            state.hide(self.hwnd);
            return;
        }
        let area = app.layout.player_panel;
        let scale = state.scale();
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOP),
                (area.left * scale).round() as i32,
                (area.top * scale).round() as i32,
                ((area.width().max(1.0)) * scale).round() as i32,
                ((area.height().max(1.0)) * scale).round() as i32,
                SWP_NOACTIVATE,
            );
            if !state.visible {
                let _ = ShowWindow(self.hwnd, SW_SHOWNA);
                state.visible = true;
                state.invalidate(self.hwnd);
            }
        }
        let playback = &app.core.settings.playback;
        let mut changed = false;
        if (state.speed - playback.speed).abs() > 0.0005 {
            state.speed = playback.speed;
            changed = true;
        }
        if state.eq != playback.eq_enabled {
            state.eq = playback.eq_enabled;
            changed = true;
        }
        if state.pitch != playback.pitch_preserve {
            state.pitch = playback.pitch_preserve;
            changed = true;
        }
        let snapshot = app.core.snapshot();
        if !state.dragging_volume && (state.volume - snapshot.volume).abs() > 0.001 {
            state.volume = snapshot.volume;
            changed = true;
        }
        if state.muted != snapshot.muted {
            state.muted = snapshot.muted;
            changed = true;
        }
        let narrow = app.layout.desktop_lyrics.width() <= 0.0;
        if state.narrow != narrow {
            state.narrow = narrow;
            changed = true;
        }
        if state.desktop_open != app.desktop_lyrics_open() {
            state.desktop_open = app.desktop_lyrics_open();
            changed = true;
        }
        if state.mini_open != app.mini_player_open() {
            state.mini_open = app.mini_player_open();
            changed = true;
        }
        if changed {
            state.invalidate(self.hwnd);
        }
    }
}

fn state_mut(hwnd: HWND) -> Option<&'static mut State> {
    let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut State;
    unsafe { pointer.as_mut() }
}

fn point_from_lparam(lparam: LPARAM) -> (f32, f32) {
    (
        (lparam.0 & 0xFFFF) as i16 as f32,
        ((lparam.0 >> 16) & 0xFFFF) as i16 as f32,
    )
}

fn track_mouse_leave(hwnd: HWND) {
    let mut event = TRACKMOUSEEVENT {
        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
        dwFlags: TME_LEAVE,
        hwndTrack: hwnd,
        dwHoverTime: 0,
    };
    let _ = unsafe { TrackMouseEvent(&mut event) };
}

unsafe extern "system" fn child_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let _ = unsafe { BeginPaint(hwnd, &mut ps) };
            if let Some(state) = state_mut(hwnd) {
                state.paint(hwnd, ps.hdc);
            }
            let _ = unsafe { EndPaint(hwnd, &ps) };
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_MOUSEMOVE => {
            track_mouse_leave(hwnd);
            if let Some(state) = state_mut(hwnd) {
                let scale = state.scale();
                let (x, y) = point_from_lparam(lparam);
                state.on_move(hwnd, x / scale, y / scale);
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            if let Some(state) = state_mut(hwnd)
                && state.hover.take().is_some()
            {
                state.invalidate(hwnd);
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            if let Some(state) = state_mut(hwnd) {
                let _ = unsafe { SetFocus(Some(state.parent)) };
                let scale = state.scale();
                let (x, y) = point_from_lparam(lparam);
                state.on_press(hwnd, x / scale, y / scale);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            if let Some(state) = state_mut(hwnd) {
                let scale = state.scale();
                let (x, y) = point_from_lparam(lparam);
                state.on_release(hwnd, x / scale, y / scale);
            }
            LRESULT(0)
        }
        WM_NCDESTROY => {
            let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut State;
            if !pointer.is_null() {
                unsafe {
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                    drop(Box::from_raw(pointer));
                }
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
