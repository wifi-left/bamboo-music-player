//! Playback settings flyout (volume / speed / equalizer / pitch / sleep timer /
//! A-B loop / output device).
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
    DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE, DT_VCENTER, DeleteDC,
    DeleteObject, DrawTextW, EndPaint, FillRect, HBRUSH, HBITMAP, HDC, HFONT, HGDIOBJ,
    OUT_DEFAULT_PRECIS, PAINTSTRUCT, RestoreDC, SRCCOPY, SaveDC, SelectObject, SetBkMode,
    SetTextColor, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GWLP_USERDATA, GetClientRect, GetWindowLongPtrW, HWND_TOP,
    PostMessageW, RegisterClassW, SW_HIDE, SW_SHOWNA, SWP_NOACTIVATE, SetWindowLongPtrW,
    SetWindowPos, ShowWindow, WM_APP, WM_ERASEBKGND, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
    WM_NCDESTROY, WM_PAINT, WINDOW_EX_STYLE, WS_CHILD, WS_CLIPSIBLINGS,
};
use windows::core::{PCWSTR, Result, w};

use crate::app::App;
use crate::core::util::format_time;
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
/// `lparam` carries the preset length in minutes (0 = off).
pub const ACTION_SET_SLEEP: usize = 9;
/// `lparam` carries the custom-step delta in minutes (-5 / +5).
pub const ACTION_SLEEP_CUSTOM_DELTA: usize = 10;
pub const ACTION_AB_START: usize = 11;
pub const ACTION_AB_END: usize = 12;
pub const ACTION_AB_CLEAR: usize = 13;
/// `lparam` carries the device-list delta (-1 / +1).
pub const ACTION_DEVICE_DELTA: usize = 14;

/// Panel size in DIP. `layout::compute_with` uses these so the child window
/// and the painted rows cannot drift apart.
pub const PANEL_WIDTH: f32 = 300.0;
pub const PANEL_BASE_HEIGHT: f32 = 272.0;
/// Extra height for the desktop-lyrics/mini-player rows hidden by the player
/// bar on narrow windows.
pub const PANEL_EXTRAS_HEIGHT: f32 = 330.0;

const CLASS_NAME: PCWSTR = w!("BambooPlayerPanel");

// Panel-local DIP layout.
const LEFT: f32 = 14.0;
const RIGHT: f32 = PANEL_WIDTH - 14.0;
const LABEL_RIGHT: f32 = 100.0;
/// Left edge of the control column (steppers, segmented rows).
const CONTROL_X: f32 = 104.0;
const ROW_H: f32 = 26.0;
const CONTROL_H: f32 = 22.0;

const ROW_VOLUME: f32 = 32.0;
const ROW_SPEED: f32 = 58.0;
const ROW_EQ: f32 = 84.0;
const ROW_PITCH: f32 = 110.0;
const ROW_SLEEP: f32 = 136.0;
const ROW_CUSTOM: f32 = 162.0;
const ROW_AB: f32 = 188.0;
const ROW_DEVICE: f32 = 214.0;
const ROW_LINK: f32 = 244.0;
const LINK_H: f32 = 24.0;
const EXTRAS_TOP: f32 = 272.0;

const VOLUME_TRACK: Rect = Rect::new(104.0, 43.0, 220.0, 47.0);
const VOLUME_HIT: Rect = Rect::new(96.0, ROW_VOLUME, 230.0, ROW_VOLUME + ROW_H);

/// One interactive element in the flyout. Volume is tracked here too so the
/// drag and the plain buttons share the same hover/press bookkeeping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Control {
    Volume,
    SpeedDown,
    SpeedUp,
    Eq,
    Pitch,
    SleepOff,
    Sleep15,
    Sleep30,
    Sleep60,
    CustomDown,
    CustomUp,
    AbStart,
    AbEnd,
    AbClear,
    DevicePrev,
    DeviceNext,
    OpenSettings,
    DesktopLyrics,
    MiniPlayer,
}

const MAIN_CONTROLS: [Control; 16] = [
    Control::SpeedDown,
    Control::SpeedUp,
    Control::Eq,
    Control::Pitch,
    Control::SleepOff,
    Control::Sleep15,
    Control::Sleep30,
    Control::Sleep60,
    Control::CustomDown,
    Control::CustomUp,
    Control::AbStart,
    Control::AbEnd,
    Control::AbClear,
    Control::DevicePrev,
    Control::DeviceNext,
    Control::OpenSettings,
];
const EXTRA_CONTROLS: [Control; 2] = [Control::DesktopLyrics, Control::MiniPlayer];

fn button_top(row: f32) -> f32 {
    row + (ROW_H - CONTROL_H) * 0.5
}

fn button_rect(row: f32, left: f32, right: f32) -> Rect {
    let top = button_top(row);
    Rect::new(left, top, right, top + CONTROL_H)
}

fn segmented_rect(row: f32, index: usize) -> Rect {
    let gap = 3.0;
    let width = (RIGHT - CONTROL_X - gap * 3.0) / 4.0;
    let left = CONTROL_X + index as f32 * (width + gap);
    button_rect(row, left, left + width)
}

fn control_rect(control: Control) -> Rect {
    match control {
        Control::Volume => VOLUME_HIT,
        Control::SpeedDown => button_rect(ROW_SPEED, 196.0, 224.0),
        Control::SpeedUp => button_rect(ROW_SPEED, 258.0, RIGHT),
        Control::Eq => button_rect(ROW_EQ, 200.0, RIGHT),
        Control::Pitch => button_rect(ROW_PITCH, 200.0, RIGHT),
        Control::SleepOff => segmented_rect(ROW_SLEEP, 0),
        Control::Sleep15 => segmented_rect(ROW_SLEEP, 1),
        Control::Sleep30 => segmented_rect(ROW_SLEEP, 2),
        Control::Sleep60 => segmented_rect(ROW_SLEEP, 3),
        Control::CustomDown => button_rect(ROW_CUSTOM, CONTROL_X, 132.0),
        Control::CustomUp => button_rect(ROW_CUSTOM, 258.0, RIGHT),
        Control::AbStart
        | Control::AbEnd
        | Control::AbClear => {
            let gap = 3.0;
            let width = (RIGHT - CONTROL_X - gap * 2.0) / 3.0;
            let slot = match control {
                Control::AbStart => 0,
                Control::AbEnd => 1,
                _ => 2,
            };
            let left = CONTROL_X + slot as f32 * (width + gap);
            button_rect(ROW_AB, left, left + width)
        }
        Control::DevicePrev => button_rect(ROW_DEVICE, CONTROL_X, 132.0),
        Control::DeviceNext => button_rect(ROW_DEVICE, 258.0, RIGHT),
        Control::OpenSettings => Rect::new(LEFT, ROW_LINK, RIGHT, ROW_LINK + LINK_H),
        Control::DesktopLyrics => {
            let top = EXTRAS_TOP;
            Rect::new(LEFT, top, RIGHT, top + CONTROL_H)
        }
        Control::MiniPlayer => {
            let top = EXTRAS_TOP + ROW_H;
            Rect::new(LEFT, top, RIGHT, top + CONTROL_H)
        }
    }
}

struct State {
    parent: HWND,
    dpi: f32,
    visible: bool,
    speed: f32,
    eq: bool,
    pitch: bool,
    volume: f32,
    muted: bool,
    sleep_minutes: Option<u32>,
    ab_start: Option<f64>,
    ab_end: Option<f64>,
    device: Option<String>,
    /// Narrow window: show the functions hidden from the player bar.
    narrow: bool,
    desktop_open: bool,
    mini_open: bool,
    hover: Option<Control>,
    pressed: Option<Control>,
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
            sleep_minutes: None,
            ab_start: None,
            ab_end: None,
            device: None,
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

    fn invalidate(&self, hwnd: HWND) {
        let _ = unsafe { windows::Win32::Graphics::Gdi::InvalidateRect(Some(hwnd), None, false) };
    }

    fn control_active(&self, control: Control) -> bool {
        match control {
            Control::Eq => self.eq,
            Control::Pitch => self.pitch,
            Control::SleepOff => self.sleep_minutes.is_none(),
            Control::Sleep15 => self.sleep_minutes == Some(15),
            Control::Sleep30 => self.sleep_minutes == Some(30),
            Control::Sleep60 => self.sleep_minutes == Some(60),
            Control::AbStart => self.ab_start.is_some(),
            Control::AbEnd => self.ab_end.is_some(),
            Control::AbClear => self.ab_start.is_some() || self.ab_end.is_some(),
            Control::DesktopLyrics => self.desktop_open,
            Control::MiniPlayer => self.mini_open,
            _ => false,
        }
    }

    fn control_label(&self, control: Control) -> String {
        match control {
            Control::SpeedDown | Control::CustomDown | Control::DevicePrev => "−".to_string(),
            Control::SpeedUp | Control::CustomUp | Control::DeviceNext => "+".to_string(),
            Control::Eq => toggle_label(self.eq),
            Control::Pitch => toggle_label(self.pitch),
            Control::SleepOff => "关闭".to_string(),
            Control::Sleep15 => "15 分".to_string(),
            Control::Sleep30 => "30 分".to_string(),
            Control::Sleep60 => "60 分".to_string(),
            Control::AbStart => match self.ab_start {
                Some(secs) => format!("A {}", format_time(secs)),
                None => "设 A".to_string(),
            },
            Control::AbEnd => match self.ab_end {
                Some(secs) => format!("B {}", format_time(secs)),
                None => "设 B".to_string(),
            },
            Control::AbClear => "清除".to_string(),
            Control::OpenSettings => "打开设置页 ›".to_string(),
            Control::DesktopLyrics => "桌面歌词".to_string(),
            Control::MiniPlayer => "迷你播放器".to_string(),
            Control::Volume => String::new(),
        }
    }

    fn control_at(&self, x: f32, y: f32) -> Option<Control> {
        if VOLUME_HIT.contains(x, y) {
            return Some(Control::Volume);
        }
        if let Some(control) = MAIN_CONTROLS
            .iter()
            .copied()
            .find(|control| control_rect(*control).contains(x, y))
        {
            return Some(control);
        }
        if self.narrow {
            return EXTRA_CONTROLS
                .iter()
                .copied()
                .find(|control| control_rect(*control).contains(x, y));
        }
        None
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
        self.fill(
            back,
            Rect::new(0.0, 0.0, width as f32 / scale, height as f32 / scale),
            self.palette.panel,
            scale,
        );
        // 1 DIP border.
        self.fill(
            back,
            Rect::new(0.0, 0.0, width as f32 / scale, 1.0),
            self.palette.divider,
            scale,
        );
        self.fill(
            back,
            Rect::new(
                0.0,
                height as f32 / scale - 1.0,
                width as f32 / scale,
                height as f32 / scale,
            ),
            self.palette.divider,
            scale,
        );
        self.fill(
            back,
            Rect::new(0.0, 0.0, 1.0, height as f32 / scale),
            self.palette.divider,
            scale,
        );
        self.fill(
            back,
            Rect::new(
                width as f32 / scale - 1.0,
                0.0,
                width as f32 / scale,
                height as f32 / scale,
            ),
            self.palette.divider,
            scale,
        );

        let rect = |left: f32, top: f32, right: f32, bottom: f32| Rect::new(left, top, right, bottom);
        let row_rect = |row: f32| rect(LEFT, row, RIGHT, row + ROW_H);

        self.text(
            back,
            13.0,
            true,
            "播放设置",
            rect(LEFT, 6.0, RIGHT, 32.0),
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
            row_rect(ROW_VOLUME),
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
            if self.muted {
                self.palette.muted
            } else {
                self.palette.accent_text
            },
            scale,
        );
        let knob_color = if self.dragging_volume || self.hover == Some(Control::Volume) {
            self.palette.text
        } else {
            self.palette.accent_text
        };
        self.fill(
            back,
            Rect::new(fill_right - 1.5, track.top - 3.0, fill_right + 1.5, track.bottom + 3.0),
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
            rect(230.0, ROW_VOLUME, RIGHT, ROW_VOLUME + ROW_H),
            if self.muted {
                self.palette.muted
            } else {
                self.palette.accent_text
            },
            DT_RIGHT.0 | DT_SINGLELINE.0 | DT_VCENTER.0 | DT_NOPREFIX.0,
            scale,
        );

        // Playback speed.
        self.row_label(back, "播放速度", ROW_SPEED, scale);
        self.button(back, Control::SpeedDown, scale);
        self.text(
            back,
            11.5,
            false,
            &format!("{:.2}×", self.speed),
            rect(224.0, ROW_SPEED, 258.0, ROW_SPEED + ROW_H),
            self.palette.accent_text,
            DT_CENTER.0 | DT_SINGLELINE.0 | DT_VCENTER.0 | DT_NOPREFIX.0,
            scale,
        );
        self.button(back, Control::SpeedUp, scale);

        self.row_label(back, "均衡器", ROW_EQ, scale);
        self.button(back, Control::Eq, scale);

        self.row_label(back, "保持音高", ROW_PITCH, scale);
        self.button(back, Control::Pitch, scale);

        self.row_label(back, "睡眠定时", ROW_SLEEP, scale);
        self.button(back, Control::SleepOff, scale);
        self.button(back, Control::Sleep15, scale);
        self.button(back, Control::Sleep30, scale);
        self.button(back, Control::Sleep60, scale);

        self.row_label(back, "自定义睡眠", ROW_CUSTOM, scale);
        self.button(back, Control::CustomDown, scale);
        self.text(
            back,
            11.5,
            false,
            &match self.sleep_minutes {
                Some(minutes) => format!("{minutes} 分钟"),
                None => "关闭".to_string(),
            },
            rect(132.0, ROW_CUSTOM, 258.0, ROW_CUSTOM + ROW_H),
            if self.sleep_minutes.is_some() {
                self.palette.accent_text
            } else {
                self.palette.muted
            },
            DT_CENTER.0 | DT_SINGLELINE.0 | DT_VCENTER.0 | DT_NOPREFIX.0,
            scale,
        );
        self.button(back, Control::CustomUp, scale);

        self.row_label(back, "A-B 循环", ROW_AB, scale);
        self.button(back, Control::AbStart, scale);
        self.button(back, Control::AbEnd, scale);
        self.button(back, Control::AbClear, scale);

        self.row_label(back, "输出设备", ROW_DEVICE, scale);
        self.button(back, Control::DevicePrev, scale);
        let device_text = self
            .device
            .clone()
            .unwrap_or_else(|| "系统默认".to_string());
        let device_active = self.device.is_some();
        self.text(
            back,
            11.5,
            false,
            &device_text,
            rect(132.0, ROW_DEVICE, 258.0, ROW_DEVICE + ROW_H),
            if device_active {
                self.palette.accent_text
            } else {
                self.palette.muted
            },
            DT_CENTER.0
                | DT_SINGLELINE.0
                | DT_VCENTER.0
                | DT_NOPREFIX.0
                | DT_END_ELLIPSIS.0,
            scale,
        );
        self.button(back, Control::DeviceNext, scale);

        let hovered = self.hover == Some(Control::OpenSettings);
        self.text(
            back,
            11.5,
            false,
            "打开设置页 ›",
            control_rect(Control::OpenSettings),
            if hovered {
                self.palette.accent_text
            } else {
                self.palette.muted
            },
            DT_RIGHT.0 | DT_SINGLELINE.0 | DT_VCENTER.0 | DT_NOPREFIX.0,
            scale,
        );

        // Narrow window: the player bar hides these buttons; show the same
        // toggles here so they stay reachable.
        if self.narrow {
            for control in EXTRA_CONTROLS {
                self.extra_button(back, control, scale);
            }
        }

        unsafe {
            let _ = RestoreDC(back, saved);
            let _ = BitBlt(hdc, 0, 0, width, height, Some(back), 0, 0, SRCCOPY);
        }
    }

    fn row_label(&mut self, hdc: HDC, label: &str, row: f32, scale: f32) {
        self.text(
            hdc,
            11.5,
            false,
            label,
            Rect::new(LEFT, row, LABEL_RIGHT, row + ROW_H),
            self.palette.text,
            DT_LEFT.0 | DT_SINGLELINE.0 | DT_VCENTER.0 | DT_NOPREFIX.0,
            scale,
        );
    }

    fn button(&mut self, hdc: HDC, control: Control, scale: f32) {
        let area = control_rect(control);
        let active = self.control_active(control);
        let state = if self.pressed == Some(control) {
            ButtonState::Pressed
        } else if self.hover == Some(control) {
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
        let label = self.control_label(control);
        self.text(
            hdc,
            11.5,
            false,
            &label,
            area.inset(4.0, 0.0),
            match state {
                ButtonState::Normal => self.palette.muted,
                ButtonState::Pressed | ButtonState::HoverActive => self.palette.accent_text,
                _ => self.palette.text,
            },
            DT_CENTER.0
                | DT_SINGLELINE.0
                | DT_VCENTER.0
                | DT_NOPREFIX.0
                | DT_END_ELLIPSIS.0,
            scale,
        );
    }

    fn extra_button(&mut self, hdc: HDC, control: Control, scale: f32) {
        let area = control_rect(control);
        let active = self.control_active(control);
        let state = if self.pressed == Some(control) {
            ButtonState::Pressed
        } else if self.hover == Some(control) {
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
        let color = match state {
            ButtonState::Normal => self.palette.muted,
            ButtonState::Pressed | ButtonState::HoverActive => self.palette.accent_text,
            _ => self.palette.text,
        };
        self.text(
            hdc,
            11.5,
            false,
            &self.control_label(control),
            area.inset(10.0, 0.0),
            color,
            DT_LEFT.0 | DT_SINGLELINE.0 | DT_VCENTER.0 | DT_NOPREFIX.0,
            scale,
        );
        self.text(
            hdc,
            10.5,
            false,
            if active { "开" } else { "关" },
            area.inset(10.0, 0.0),
            if active {
                self.palette.accent_text
            } else {
                self.palette.muted
            },
            DT_RIGHT.0 | DT_SINGLELINE.0 | DT_VCENTER.0 | DT_NOPREFIX.0,
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
        let hover = self.control_at(x, y);
        if hover != self.hover {
            self.hover = hover;
            self.invalidate(hwnd);
        }
    }

    fn on_press(&mut self, hwnd: HWND, x: f32, y: f32) {
        if VOLUME_HIT.contains(x, y) {
            self.dragging_volume = true;
            self.hover = Some(Control::Volume);
            unsafe {
                SetCapture(hwnd);
            }
            self.set_volume_from_x(x);
            self.invalidate(hwnd);
            return;
        }
        let pressed = self.control_at(x, y);
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
        let hit = self.control_at(x, y);
        let pressed = self.pressed.take();
        self.invalidate(hwnd);
        let Some(pressed) = pressed else {
            return;
        };
        if hit != Some(pressed) {
            return;
        }
        let Some((action, data)) = action_for(pressed) else {
            return;
        };
        unsafe {
            let _ = PostMessageW(
                Some(self.parent),
                WM_APP_PLAYER_ACTION,
                WPARAM(action),
                LPARAM(data),
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

fn toggle_label(on: bool) -> String {
    if on { "开" } else { "关" }.to_string()
}

fn action_for(control: Control) -> Option<(usize, isize)> {
    match control {
        Control::SpeedDown => Some((ACTION_SPEED_DOWN, 0)),
        Control::SpeedUp => Some((ACTION_SPEED_UP, 0)),
        Control::Eq => Some((ACTION_TOGGLE_EQ, 0)),
        Control::Pitch => Some((ACTION_TOGGLE_PITCH, 0)),
        Control::SleepOff => Some((ACTION_SET_SLEEP, 0)),
        Control::Sleep15 => Some((ACTION_SET_SLEEP, 15)),
        Control::Sleep30 => Some((ACTION_SET_SLEEP, 30)),
        Control::Sleep60 => Some((ACTION_SET_SLEEP, 60)),
        Control::CustomDown => Some((ACTION_SLEEP_CUSTOM_DELTA, -5)),
        Control::CustomUp => Some((ACTION_SLEEP_CUSTOM_DELTA, 5)),
        Control::AbStart => Some((ACTION_AB_START, 0)),
        Control::AbEnd => Some((ACTION_AB_END, 0)),
        Control::AbClear => Some((ACTION_AB_CLEAR, 0)),
        Control::DevicePrev => Some((ACTION_DEVICE_DELTA, -1)),
        Control::DeviceNext => Some((ACTION_DEVICE_DELTA, 1)),
        Control::OpenSettings => Some((ACTION_OPEN_SETTINGS, 0)),
        Control::DesktopLyrics => Some((ACTION_TOGGLE_DESKTOP_LYRICS, 0)),
        Control::MiniPlayer => Some((ACTION_TOGGLE_MINI_PLAYER, 0)),
        Control::Volume => None,
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
        if state.device != playback.device {
            state.device = playback.device.clone();
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
        if state.sleep_minutes != snapshot.sleep_minutes {
            state.sleep_minutes = snapshot.sleep_minutes;
            changed = true;
        }
        if state.ab_start != snapshot.ab_start || state.ab_end != snapshot.ab_end {
            state.ab_start = snapshot.ab_start;
            state.ab_end = snapshot.ab_end;
            changed = true;
        }
        // The extra rows only exist when the layout actually reserved space
        // for them (narrow windows with enough vertical room).
        let narrow = app.layout.player_panel.height() > PANEL_BASE_HEIGHT + 1.0;
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
