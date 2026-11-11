//! Standalone "immersive" lyrics window.
//!
//! Custom-drawn borderless top-level window: brand and window buttons in the
//! top bar, cover/track info on the left, the shared scrollable lyric control
//! in the right column, and progress + transport in the footer (see
//! [`crate::lyrics_screen`] for layout and painting). F11 toggles borderless
//! fullscreen; in windowed mode the top bar drags the window and the thin
//! border resizes it.
//!
//! The player keeps syncing it from the shared [`crate::app::App`] snapshot.
//! Clicking a line or the progress bar posts seek requests to the player; the
//! transport buttons post [`WM_APP_LYRICS_ACTION`]. Closing the window posts
//! [`WM_APP_LYRICS_CLOSED`] so the player clears its handle and remembers the
//! mode/geometry.

use std::mem::size_of;

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, DestroyWindow, GWLP_USERDATA, GWL_STYLE,
    GetWindowLongPtrW, GetWindowRect, HTCAPTION, HWND_NOTOPMOST, HWND_TOPMOST,
    IDC_ARROW, IsIconic, IsWindowVisible, LoadCursorW, LoadIconW, MINMAXINFO, PostMessageW,
    RegisterClassW, SW_HIDE, SW_SHOW, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOZORDER,
    SendMessageW, SetForegroundWindow, SetWindowDisplayAffinity, SetWindowLongPtrW, SetWindowPos,
    ShowWindow, WDA_EXCLUDEFROMCAPTURE, WDA_NONE, WM_APP, WM_CAPTURECHANGED,
    WM_CLOSE, WM_DPICHANGED, WM_ERASEBKGND, WM_GETMINMAXINFO, WM_KEYDOWN, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MOUSEMOVE, WM_NCDESTROY, WM_PAINT, WM_SIZE, WNDCLASSW, WS_CLIPCHILDREN,
    WS_EX_APPWINDOW, WS_OVERLAPPEDWINDOW, WS_POPUP, WS_VISIBLE,
};
use windows::core::{PCWSTR, Result, w};

use crate::app::App;
use crate::lyrics_screen::{Action, Screen};
use crate::lyrics_view::{LyricsChild, WM_APP_LYRIC_MENU, WM_APP_LYRIC_SEEK};

/// Posted to the player window when the user closes the standalone window.
pub const WM_APP_LYRICS_CLOSED: u32 = WM_APP + 4;
/// Posted to the player window after a resize/DPI/mode change so it syncs the
/// floating child immediately (no full tick needed).
pub const WM_APP_LYRICS_SYNC: u32 = WM_APP + 5;
/// Posted to the player window when the footer transport is used.
pub const WM_APP_LYRICS_ACTION: u32 = WM_APP + 6;
pub const LYRICS_ACTION_PREV: usize = 1;
pub const LYRICS_ACTION_PLAY_PAUSE: usize = 2;
pub const LYRICS_ACTION_NEXT: usize = 3;

const CLASS_NAME: PCWSTR = w!("BambooLyricsWindow");
const TITLE: PCWSTR = w!("Bamboo 歌词");
const DEFAULT_WIDTH: f32 = 900.0;
const DEFAULT_HEIGHT: f32 = 620.0;
const MIN_WIDTH: f32 = 640.0;
const MIN_HEIGHT: f32 = 420.0;
const VK_ESCAPE: u16 = 0x1B;
const VK_F11: u16 = 0x7A;

/// Window-level state, stored in the top-level window's user data.
struct WindowState {
    main: HWND,
    child: HWND,
    windowed: bool,
    /// Normal placement restored when leaving fullscreen.
    saved: RECT,
    screen: Screen,
}

pub struct LyricsWindow {
    hwnd: HWND,
    child: LyricsChild,
}

impl LyricsWindow {
    /// Create the window (hidden) and its lyric child. `pos`/`size` are in
    /// DIPs, matching the values remembered in the session. `windowed = false`
    /// starts borderless fullscreen on the window's monitor.
    pub fn new(
        main: HWND,
        pos: Option<(f32, f32)>,
        size: Option<(f32, f32)>,
        dpi: f32,
        windowed: bool,
    ) -> Result<Self> {
        unsafe {
            let instance = GetModuleHandleW(None)?;
            let icon = LoadIconW(Some(HINSTANCE(instance.0)), PCWSTR(1 as *const u16))
                .unwrap_or_default();
            let class = WNDCLASSW {
                hInstance: HINSTANCE(instance.0),
                hIcon: icon,
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                hbrBackground: windows::Win32::Graphics::Gdi::HBRUSH(std::ptr::null_mut()),
                lpszClassName: CLASS_NAME,
                lpfnWndProc: Some(window_proc),
                ..Default::default()
            };
            let _ = RegisterClassW(&class);

            let scale = if dpi > 0.0 { dpi / 96.0 } else { 1.0 };
            let width = (size.map(|value| value.0).unwrap_or(DEFAULT_WIDTH) * scale)
                .round()
                .max(MIN_WIDTH * scale) as i32;
            let height = (size.map(|value| value.1).unwrap_or(DEFAULT_HEIGHT) * scale)
                .round()
                .max(MIN_HEIGHT * scale) as i32;
            let (x, y) = match pos {
                Some((left, top)) => (
                    (left * scale).round() as i32,
                    (top * scale).round() as i32,
                ),
                None => (CW_USEDEFAULT, CW_USEDEFAULT),
            };
            let hwnd = CreateWindowExW(
                // Independent taskbar entry and no owner: the window must stay
                // visible when the player is minimized or hidden to the tray.
                WS_EX_APPWINDOW,
                CLASS_NAME,
                TITLE,
                WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
                x,
                y,
                width,
                height,
                None,
                None,
                Some(HINSTANCE(instance.0)),
                None,
            )?;
            let child = LyricsChild::new(hwnd)?;
            let mut saved = RECT::default();
            let _ = GetWindowRect(hwnd, &mut saved);
            let state = Box::new(WindowState {
                main,
                child: child.hwnd(),
                windowed,
                saved,
                screen: Screen::new()?,
            });
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(state) as isize);
            if !windowed {
                enter_fullscreen(hwnd);
            }
            Ok(Self { hwnd, child })
        }
    }

    pub fn is_visible(&self) -> bool {
        unsafe { IsWindowVisible(self.hwnd).as_bool() && !IsIconic(self.hwnd).as_bool() }
    }

    /// Whether the window currently uses the windowed frame instead of
    /// borderless fullscreen. Stored in the session so the mode survives a
    /// restart.
    pub fn is_windowed(&self) -> bool {
        window_state(self.hwnd)
            .map(|state| state.windowed)
            .unwrap_or(true)
    }

    /// Push the current snapshot into the shell and the lyric child.
    pub fn sync(&mut self, app: &App) {
        let Some(state) = window_state_mut(self.hwnd) else {
            return;
        };
        let windowed = state.windowed;
        state
            .screen
            .update(self.hwnd, state.child, app, windowed);
        self.child.update_floating(app);
    }

    /// Show the window and do a first sync.
    pub fn show(&mut self, app: &App) {
        self.sync(app);
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOW);
            let _ = SetForegroundWindow(self.hwnd);
        }
        self.sync(app);
    }

    pub fn hide(&self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }

    /// Exclude the standalone window from screen capture when the setting is
    /// on. Applied live because the user can toggle it from the settings page.
    pub fn apply_capture_protection(&self, enabled: bool) {
        let _ = unsafe {
            SetWindowDisplayAffinity(
                self.hwnd,
                if enabled { WDA_EXCLUDEFROMCAPTURE } else { WDA_NONE },
            )
        };
    }

    /// Outer position and size in DIPs, stored in the session on close. In
    /// fullscreen this returns the remembered windowed placement.
    pub fn geometry(&self, dpi: f32) -> (Option<(f32, f32)>, Option<(f32, f32)>) {
        let mut current = RECT::default();
        let rect = match window_state(self.hwnd) {
            Some(state) if !state.windowed => state.saved,
            _ => {
                if unsafe { GetWindowRect(self.hwnd, &mut current) }.is_err() {
                    return (None, None);
                }
                current
            }
        };
        let scale = if dpi > 0.0 { dpi / 96.0 } else { 1.0 };
        (
            Some((rect.left as f32 / scale, rect.top as f32 / scale)),
            Some((
                (rect.right - rect.left) as f32 / scale,
                (rect.bottom - rect.top) as f32 / scale,
            )),
        )
    }
}

impl Drop for LyricsWindow {
    fn drop(&mut self) {
        // Owned by the player window; destroying it also destroys the child.
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

fn window_state(hwnd: HWND) -> Option<&'static WindowState> {
    let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const WindowState;
    unsafe { pointer.as_ref() }
}

fn window_state_mut(hwnd: HWND) -> Option<&'static mut WindowState> {
    let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut WindowState;
    unsafe { pointer.as_mut() }
}

/// Borderless fullscreen on the monitor that currently contains the window.
fn enter_fullscreen(hwnd: HWND) {
    unsafe {
        let mut rect = RECT::default();
        if GetWindowRect(hwnd, &mut rect).is_ok()
            && let Some(state) = window_state_mut(hwnd)
        {
            state.saved = rect;
        }
        SetWindowLongPtrW(hwnd, GWL_STYLE, (WS_POPUP | WS_VISIBLE).0 as isize);
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                info.rcMonitor.left,
                info.rcMonitor.top,
                info.rcMonitor.right - info.rcMonitor.left,
                info.rcMonitor.bottom - info.rcMonitor.top,
                SWP_FRAMECHANGED | SWP_NOACTIVATE,
            );
        }
        if let Some(state) = window_state_mut(hwnd) {
            state.windowed = false;
        }
    }
}

/// Restore the native frame and remembered placement.
fn exit_fullscreen(hwnd: HWND) {
    unsafe {
        let saved = window_state(hwnd)
            .map(|state| state.saved)
            .unwrap_or_default();
        SetWindowLongPtrW(
            hwnd,
            GWL_STYLE,
            (WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN | WS_VISIBLE).0 as isize,
        );
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_NOTOPMOST),
            saved.left,
            saved.top,
            (saved.right - saved.left).max(1),
            (saved.bottom - saved.top).max(1),
            SWP_FRAMECHANGED | SWP_NOACTIVATE,
        );
        if let Some(state) = window_state_mut(hwnd) {
            state.windowed = true;
        }
    }
}

fn toggle_fullscreen(hwnd: HWND) {
    let windowed = window_state(hwnd)
        .map(|state| state.windowed)
        .unwrap_or(true);
    if windowed {
        enter_fullscreen(hwnd);
    } else {
        exit_fullscreen(hwnd);
    }
    // Re-layout, refresh the fullscreen glyph and let the child resize.
    notify_main(hwnd);
}

/// Ask the player for a sync (data, DPI or mode changes).
fn notify_main(hwnd: HWND) {
    if let Some(state) = window_state(hwnd) {
        unsafe {
            let _ = PostMessageW(Some(state.main), WM_APP_LYRICS_SYNC, WPARAM(0), LPARAM(0));
        }
    }
}

fn post_action(hwnd: HWND, action: usize) {
    if let Some(state) = window_state(hwnd) {
        unsafe {
            let _ = PostMessageW(
                Some(state.main),
                WM_APP_LYRICS_ACTION,
                WPARAM(action),
                LPARAM(0),
            );
        }
    }
}

fn perform(hwnd: HWND, action: Action) {
    match action {
        Action::Close => {
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
            if let Some(state) = window_state(hwnd) {
                unsafe {
                    let _ =
                        PostMessageW(Some(state.main), WM_APP_LYRICS_CLOSED, WPARAM(0), LPARAM(0));
                }
            }
        }
        Action::Fullscreen => toggle_fullscreen(hwnd),
        Action::Prev => post_action(hwnd, LYRICS_ACTION_PREV),
        Action::PlayPause => post_action(hwnd, LYRICS_ACTION_PLAY_PAUSE),
        Action::Next => post_action(hwnd, LYRICS_ACTION_NEXT),
        Action::Seek(ms) => {
            if let Some(state) = window_state(hwnd) {
                unsafe {
                    let _ = PostMessageW(
                        Some(state.main),
                        WM_APP_LYRIC_SEEK,
                        WPARAM(ms as usize),
                        LPARAM(0),
                    );
                }
            }
        }
        Action::Caption => unsafe {
            let _ = ReleaseCapture();
            SendMessageW(
                hwnd,
                windows::Win32::UI::WindowsAndMessaging::WM_NCLBUTTONDOWN,
                Some(WPARAM(HTCAPTION as usize)),
                Some(LPARAM(0)),
            );
        },
    }
}

fn point(lparam: LPARAM, scale: f32) -> (f32, f32) {
    let x = (lparam.0 & 0xFFFF) as i16 as f32;
    let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32;
    (x / scale, y / scale)
}

fn track_mouse_leave(hwnd: HWND) {
    let mut event = TRACKMOUSEEVENT {
        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
        dwFlags: TME_LEAVE,
        hwndTrack: hwnd,
        dwHoverTime: 0,
    };
    let _ = unsafe { TrackMouseEvent(&mut event) };
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_PAINT => {
            let mut ps = windows::Win32::Graphics::Gdi::PAINTSTRUCT::default();
            let _ = unsafe { windows::Win32::Graphics::Gdi::BeginPaint(hwnd, &mut ps) };
            if let Some(state) = window_state_mut(hwnd) {
                state.screen.paint(hwnd, ps.hdc, ps.rcPaint);
            }
            let _ = unsafe { windows::Win32::Graphics::Gdi::EndPaint(hwnd, &ps) };
            LRESULT(0)
        }
        windows::Win32::UI::WindowsAndMessaging::WM_GETOBJECT => {
            if lparam.0 as i32 == crate::accessibility::root_object_id() {
                let mut rect = RECT::default();
                let _ = unsafe { GetWindowRect(hwnd, &mut rect) };
                let full = windows::Win32::UI::Accessibility::UiaRect {
                    left: rect.left as f64,
                    top: rect.top as f64,
                    width: (rect.right - rect.left) as f64,
                    height: (rect.bottom - rect.top) as f64,
                };
                let mut elements = Vec::new();
                let mut main = None;
                if let Some(state) = window_state(hwnd) {
                    elements = state.screen.uia_elements(hwnd);
                    main = Some(state.main);
                }
                if let Some(main) = main
                    && let Some(app) = unsafe { crate::state_mut(main) }
                {
                    let snapshot = app.app.core.snapshot();
                    let mut current = String::new();
                    if let Some(lyrics) = snapshot.lyrics {
                        let position_ms = (snapshot.position_secs * 1000.0).max(0.0) as u64;
                        for line in &lyrics.lines {
                            if line.start_ms <= position_ms {
                                current = line.text.clone();
                                if let Some(translation) = line.translation.as_deref()
                                    && !translation.trim().is_empty()
                                {
                                    current.push('\n');
                                    current.push_str(translation);
                                }
                            } else {
                                break;
                            }
                        }
                    }
                    if !current.trim().is_empty() {
                        elements.push(crate::accessibility::UiElement::text(current, full));
                    }
                }
                let provider = crate::accessibility::publish(hwnd, elements);
                return crate::accessibility::return_published(hwnd, wparam, lparam, &provider);
            }
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        crate::accessibility::WM_APP_UIA_RANGE => {
            if wparam.0 == 2
                && let Some(state) = window_state(hwnd)
            {
                let seconds = f64::from_bits(lparam.0 as u64).max(0.0);
                let milliseconds = (seconds * 1000.0).round() as usize;
                let _ = unsafe {
                    PostMessageW(
                        Some(state.main),
                        WM_APP_LYRIC_SEEK,
                        WPARAM(milliseconds),
                        LPARAM(0),
                    )
                };
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_SIZE => {
            if let Some(state) = window_state_mut(hwnd) {
                state.screen.resize(hwnd, state.child);
                notify_main(hwnd);
            }
            LRESULT(0)
        }
        WM_GETMINMAXINFO => {
            if lparam.0 != 0 {
                let dpi = unsafe { GetDpiForWindow(hwnd) } as f32;
                let scale = if dpi > 0.0 { dpi / 96.0 } else { 1.0 };
                let info = unsafe { &mut *(lparam.0 as *mut MINMAXINFO) };
                info.ptMinTrackSize.x = (MIN_WIDTH * scale).round() as i32;
                info.ptMinTrackSize.y = (MIN_HEIGHT * scale).round() as i32;
            }
            LRESULT(0)
        }
        WM_DPICHANGED => {
            if lparam.0 != 0 {
                let suggested = unsafe { &*(lparam.0 as *const RECT) };
                let _ = unsafe {
                    SetWindowPos(
                        hwnd,
                        None,
                        suggested.left,
                        suggested.top,
                        suggested.right - suggested.left,
                        suggested.bottom - suggested.top,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    )
                };
            }
            notify_main(hwnd);
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let key = wparam.0 as u16;
            if key == VK_F11 {
                toggle_fullscreen(hwnd);
            } else if key == VK_ESCAPE {
                if window_state(hwnd).is_some_and(|state| !state.windowed) {
                    exit_fullscreen(hwnd);
                    notify_main(hwnd);
                }
            } else if let Some(state) = window_state(hwnd) {
                // Space/arrows belong to the player.
                unsafe {
                    let _ = PostMessageW(Some(state.main), WM_KEYDOWN, wparam, lparam);
                }
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            track_mouse_leave(hwnd);
            let dpi = unsafe { GetDpiForWindow(hwnd) } as f32;
            let scale = if dpi > 0.0 { dpi / 96.0 } else { 1.0 };
            let (x, y) = point(lparam, scale);
            if let Some(state) = window_state_mut(hwnd)
                && let Some(action) = state.screen.on_move(hwnd, x, y)
            {
                perform(hwnd, action);
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            if let Some(state) = window_state_mut(hwnd) {
                state.screen.clear_hover(hwnd);
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let dpi = unsafe { GetDpiForWindow(hwnd) } as f32;
            let scale = if dpi > 0.0 { dpi / 96.0 } else { 1.0 };
            let (x, y) = point(lparam, scale);
            let action = window_state_mut(hwnd).and_then(|state| {
                let action = state.screen.on_press(hwnd, x, y);
                if matches!(action, Some(Action::Seek(_))) {
                    unsafe {
                        SetCapture(hwnd);
                    }
                }
                action
            });
            if let Some(action) = action {
                perform(hwnd, action);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let dpi = unsafe { GetDpiForWindow(hwnd) } as f32;
            let scale = if dpi > 0.0 { dpi / 96.0 } else { 1.0 };
            let (x, y) = point(lparam, scale);
            let action = window_state_mut(hwnd)
                .and_then(|state| state.screen.on_release(hwnd, x, y));
            unsafe {
                let _ = ReleaseCapture();
            }
            if let Some(action) = action {
                perform(hwnd, action);
            }
            LRESULT(0)
        }
        WM_CAPTURECHANGED => {
            if let Some(state) = window_state_mut(hwnd) {
                state.screen.stop_drag();
            }
            LRESULT(0)
        }
        WM_APP_LYRIC_SEEK => {
            if let Some(state) = window_state(hwnd) {
                unsafe {
                    let _ = PostMessageW(Some(state.main), WM_APP_LYRIC_SEEK, wparam, lparam);
                }
            }
            LRESULT(0)
        }
        WM_APP_LYRIC_MENU => {
            // The shared lyric child reports right clicks; the player window
            // owns the menu and the lyrics document.
            if let Some(state) = window_state(hwnd) {
                unsafe {
                    let _ = PostMessageW(Some(state.main), WM_APP_LYRIC_MENU, wparam, lparam);
                }
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = unsafe { ShowWindow(hwnd, SW_HIDE) };
            if let Some(state) = window_state(hwnd) {
                unsafe {
                    let _ =
                        PostMessageW(Some(state.main), WM_APP_LYRICS_CLOSED, WPARAM(0), LPARAM(0));
                }
            }
            LRESULT(0)
        }
        WM_NCDESTROY => {
            let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut WindowState;
            if !pointer.is_null() {
                unsafe {
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                    drop(Box::from_raw(pointer));
                }
            }
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}
