//! Native layered desktop-lyrics window.
//!
//! Runs on its own thread inside the player process. It deliberately uses no
//! egui/wgpu: a `WS_EX_LAYERED` popup owns the window, Direct2D paints into a
//! DIB and `UpdateLayeredWindow` composites it. The UI thread feeds state over
//! a channel, so the overlay never blocks a frame.

mod render;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWMNCRP_DISABLED, DWMNCRP_ENABLED, DWMWA_BORDER_COLOR, DWMWA_NCRENDERING_POLICY,
    DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND,
    DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow, UpdateWindow,
    ValidateRect,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::PCWSTR;

use crate::overlay_protocol::{HostMessage, OverlayAction, OverlayLyrics, OverlayState};

const CLASS_NAME: &str = "BambooNativeDesktopLyrics";
const WINDOW_TITLE: &str = "Bamboo Desktop Lyrics";
const TIMER_RENDER: usize = 1;
const EDGE: i32 = 6;

pub fn run(
    messages: Receiver<HostMessage>,
    actions: Sender<OverlayAction>,
    alive: Arc<AtomicBool>,
    wake: Arc<dyn Fn() + Send + Sync>,
) -> windows::core::Result<()> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = wide(CLASS_NAME);
        let cursor = LoadCursorW(None, IDC_ARROW).unwrap_or_default();
        let window_class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            hCursor: cursor,
            lpszClassName: PCWSTR(class.as_ptr()),
            ..Default::default()
        };
        RegisterClassExW(&window_class);

        let mut app = OverlayApp::new(messages, actions, alive.clone(), wake);
        let title = wide(WINDOW_TITLE);
        let ex_style = WS_EX_LAYERED
            | WS_EX_TOOLWINDOW
            | WS_EX_NOACTIVATE
            | WS_EX_TOPMOST;
        let style = WS_POPUP | WS_THICKFRAME;
        let hwnd = CreateWindowExW(
            ex_style,
            PCWSTR(class.as_ptr()),
            PCWSTR(title.as_ptr()),
            style,
            0,
            0,
            640,
            160,
            None,
            None,
            Some(HINSTANCE(instance.0)),
            Some(&mut app as *mut OverlayApp as *const core::ffi::c_void),
        )?;
        if hwnd.is_invalid() {
            alive.store(false, Ordering::Relaxed);
            return Err(windows::core::Error::from_thread());
        }
        alive.store(true, Ordering::Relaxed);
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        let _ = UpdateWindow(hwnd);

        let mut message = MSG::default();
        loop {
            let result = GetMessageW(&mut message, None, 0, 0);
            if result.0 == 0 || result.0 == -1 {
                break;
            }
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        app.shutdown();
        alive.store(false, Ordering::Relaxed);
    }
    Ok(())
}

struct OverlayApp {
    hwnd: HWND,
    messages: Receiver<HostMessage>,
    actions: Sender<OverlayAction>,
    alive: Arc<AtomicBool>,
    /// Wakes the hidden main window after a menu action.
    wake: Arc<dyn Fn() + Send + Sync>,
    renderer: Option<render::Renderer>,
    state: OverlayState,
    lyrics: Option<OverlayLyrics>,
    lyrics_version: u64,
    cover_version: u64,
    initialized: bool,
    playing: bool,
    position_ms: u64,
    position_at: Instant,
    dpi: f32,
    interaction: bool,
    last_move_rect: RECT,
    dirty: bool,
    /// A clock tick arrived; only the karaoke row may need repainting.
    position_dirty: bool,
    /// Active lyric line painted by the last full frame.
    last_active: Option<usize>,
    last_render: Instant,
    applied_click_through: Option<bool>,
    applied_topmost: Option<bool>,
    applied_capture: Option<bool>,
    applied_shadow: Option<bool>,
}

impl OverlayApp {
    fn new(
        messages: Receiver<HostMessage>,
        actions: Sender<OverlayAction>,
        alive: Arc<AtomicBool>,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        Self {
            hwnd: HWND::default(),
            messages,
            actions,
            alive,
            wake,
            renderer: None,
            state: OverlayState::default(),
            lyrics: None,
            lyrics_version: 0,
            cover_version: 0,
            initialized: false,
            playing: false,
            position_ms: 0,
            position_at: Instant::now(),
            dpi: 1.0,
            interaction: false,
            last_move_rect: RECT::default(),
            dirty: true,
            position_dirty: false,
            last_active: None,
            last_render: Instant::now() - Duration::from_secs(5),
            applied_click_through: None,
            applied_topmost: None,
            applied_capture: None,
            applied_shadow: None,
        }
    }

    fn init(&mut self) -> bool {
        self.dpi = dpi_for_window(self.hwnd);
        let Some(renderer) = render::Renderer::new().ok() else {
            return false;
        };
        self.renderer = Some(renderer);
        unsafe {
            let _ = SetTimer(Some(self.hwnd), TIMER_RENDER, 16, None);
        }
        self.apply_window_flags();
        self.apply_initial_geometry();
        true
    }

    fn shutdown(&mut self) {
        self.alive.store(false, Ordering::Relaxed);
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_RENDER);
        }
        self.renderer = None;
    }

    fn drain_messages(&mut self) {
        let mut received = Vec::new();
        while let Ok(message) = self.messages.try_recv() {
            received.push(message);
        }
        for message in received {
            match message {
                HostMessage::State(state) => self.apply_state(*state),
                HostMessage::Position {
                    position_ms,
                    playing,
                } => {
                    self.position_ms = position_ms;
                    self.position_at = Instant::now();
                    self.playing = playing;
                    // Never force a full repaint for a clock tick: the timer
                    // decides whether the active line changed or only the
                    // karaoke fill advanced.
                    self.position_dirty = true;
                }
                HostMessage::ResetPosition => {
                    self.reset_position();
                    self.dirty = true;
                }
                HostMessage::Shutdown => {
                    unsafe {
                        let _ = DestroyWindow(self.hwnd);
                    }
                    return;
                }
            }
        }
    }

    fn apply_state(&mut self, mut state: OverlayState) {
        let first = !self.initialized;
        let cover_png = state.cover_png.take();
        if state.cover_version != self.cover_version {
            if let Some(renderer) = self.renderer.as_mut() {
                renderer.set_cover(state.cover_version, cover_png.as_deref());
            }
            self.cover_version = state.cover_version;
        }
        if state.lyrics_version != self.lyrics_version {
            self.lyrics = state.lyrics.take();
            self.lyrics_version = state.lyrics_version;
        } else if let Some(lyrics) = state.lyrics.take() {
            self.lyrics = Some(lyrics);
        }
        let old_auto = self.state.settings.auto_size;
        let old_width = self.state.settings.width;
        let old_height = self.state.settings.height;
        self.state = state;
        self.position_at = Instant::now();
        self.playing = self.state.playing;
        self.position_ms = self.state.position_ms;
        self.initialized = true;
        if first {
            self.apply_initial_geometry();
        } else if self.state.settings.width != old_width
            || old_auto != self.state.settings.auto_size
            || (!self.state.settings.auto_size
                && (self.state.settings.height - old_height).abs() > 0.5)
        {
            self.apply_size();
        }
        self.apply_window_flags();
        self.dirty = true;
    }

    fn apply_window_flags(&mut self) {
        let settings = &self.state.settings;
        if self.applied_click_through != Some(settings.click_through) {
            self.applied_click_through = Some(settings.click_through);
            unsafe {
                let mut style = GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE);
                if settings.click_through {
                    style |= WS_EX_TRANSPARENT.0 as isize;
                } else {
                    style &= !(WS_EX_TRANSPARENT.0 as isize);
                }
                SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, style);
            }
        }
        if self.applied_topmost != Some(settings.always_on_top) {
            self.applied_topmost = Some(settings.always_on_top);
            unsafe {
                let _ = SetWindowPos(
                    self.hwnd,
                    Some(if settings.always_on_top {
                        HWND_TOPMOST
                    } else {
                        HWND_NOTOPMOST
                    }),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
            }
        }
        if self.applied_capture != Some(settings.capture_protection) {
            self.applied_capture = Some(settings.capture_protection);
            unsafe {
                let _ = SetWindowDisplayAffinity(
                    self.hwnd,
                    if settings.capture_protection {
                        WDA_EXCLUDEFROMCAPTURE
                    } else {
                        WDA_NONE
                    },
                );
            }
        }
        if self.applied_shadow != Some(settings.window_shadow) {
            self.applied_shadow = Some(settings.window_shadow);
            apply_shadow(self.hwnd, settings.window_shadow);
        }
        apply_overlay_attributes(self.hwnd);
    }

    fn apply_initial_geometry(&mut self) {
        let settings = &self.state.settings;
        let width = settings.width.clamp(240.0, 3000.0);
        let height = if settings.auto_size {
            render::auto_height(settings, &self.state.title, &self.state.artist)
        } else {
            settings.height.clamp(48.0, 900.0)
        };
        let (x, y) = settings.position.map_or_else(
            || default_position(self.hwnd, width, height, self.scale()),
            |(x, y)| (x, y),
        );
        self.set_logical_rect(x, y, width, height);
    }

    fn apply_size(&mut self) {
        if self.interaction {
            return;
        }
        let settings = &self.state.settings;
        let width = settings.width.clamp(240.0, 3000.0);
        let height = if settings.auto_size {
            render::auto_height(settings, &self.state.title, &self.state.artist)
        } else {
            settings.height.clamp(48.0, 900.0)
        };
        let (x, y) = self.window_logical_position();
        self.set_logical_size(width, height, x, y);
    }

    fn reset_position(&mut self) {
        let (width, height) = self.window_logical_size();
        let (x, y) = default_position(self.hwnd, width, height, self.scale());
        self.set_logical_rect(x, y, width, height);
        self.send_action(OverlayAction::Geometry {
            x,
            y,
            width,
            height,
            resized: false,
        });
    }

    fn scale(&self) -> f32 {
        (self.dpi / 96.0).max(0.5)
    }

    fn set_logical_rect(&mut self, x: f32, y: f32, width: f32, height: f32) {
        let scale = self.scale();
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                None,
                (x * scale).round() as i32,
                (y * scale).round() as i32,
                (width * scale).round().max(1.0) as i32,
                (height * scale).round().max(1.0) as i32,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }

    fn set_logical_size(&mut self, width: f32, height: f32, x: f32, y: f32) {
        let scale = self.scale();
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                None,
                (x * scale).round() as i32,
                (y * scale).round() as i32,
                (width * scale).round().max(1.0) as i32,
                (height * scale).round().max(1.0) as i32,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        self.dirty = true;
    }

    fn window_logical_size(&self) -> (f32, f32) {
        let mut rect = RECT::default();
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut rect);
        }
        let scale = self.scale();
        (
            (rect.right - rect.left) as f32 / scale,
            (rect.bottom - rect.top) as f32 / scale,
        )
    }

    fn window_logical_position(&self) -> (f32, f32) {
        let mut rect = RECT::default();
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut rect);
        }
        let scale = self.scale();
        (rect.left as f32 / scale, rect.top as f32 / scale)
    }

    fn live_position_ms(&self) -> u64 {
        if self.playing {
            self.position_ms
                .saturating_add(self.position_at.elapsed().as_millis() as u64)
        } else {
            self.position_ms
        }
    }

    fn render(&mut self) {
        if !self.initialized {
            return;
        }
        let mut rect = RECT::default();
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut rect);
        }
        let width_px = (rect.right - rect.left).max(1) as u32;
        let height_px = (rect.bottom - rect.top).max(1) as u32;
        let position = self.live_position_ms();
        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };
        let desired = renderer.render(
            self.hwnd,
            &self.state,
            self.lyrics.as_ref(),
            position,
            width_px,
            height_px,
            self.dpi,
        );
        self.last_render = Instant::now();
        self.dirty = false;
        self.position_dirty = false;
        self.last_active = self.active_line();

        if self.state.settings.auto_size && !self.interaction {
            let current = height_px as f32 / self.scale();
            if (desired - current).abs() > 3.0 {
                let (x, y) = self.window_logical_position();
                let width = self.state.settings.width.clamp(240.0, 3000.0);
                self.set_logical_size(width, desired, x, y);
                if self.renderer.is_some() {
                    self.render();
                }
            }
        }
    }

    fn active_line(&self) -> Option<usize> {
        let lyrics = self.lyrics.as_ref()?;
        if !lyrics.timed || lyrics.lines.is_empty() {
            return None;
        }
        let adjusted =
            render::effective_position(self.live_position_ms(), self.state.offset_ms);
        render::index_at(lyrics, adjusted)
    }

    /// True when the active row changes with the clock (karaoke fill).
    fn karaoke_active(&self, line: Option<usize>) -> bool {
        if !self.state.settings.karaoke {
            return false;
        }
        let Some(index) = line else {
            return false;
        };
        let Some(lyrics) = self.lyrics.as_ref() else {
            return false;
        };
        let Some(line) = lyrics.lines.get(index) else {
            return false;
        };
        !self.state.settings.karaoke_words_only || !line.words.is_empty()
    }

    /// Repaint only the current lyric row over the cached static frame.
    fn render_partial(&mut self, line: usize) -> bool {
        let Some(lyrics) = self.lyrics.as_ref() else {
            return false;
        };
        let mut rect = RECT::default();
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut rect);
        }
        let width_px = (rect.right - rect.left).max(1) as u32;
        let height_px = (rect.bottom - rect.top).max(1) as u32;
        let position = self.live_position_ms();
        let Some(renderer) = self.renderer.as_mut() else {
            return false;
        };
        if renderer.render_line(
            self.hwnd,
            &self.state,
            lyrics,
            position,
            width_px,
            height_px,
            self.dpi,
            line,
        ) {
            self.last_render = Instant::now();
            self.position_dirty = false;
            true
        } else {
            false
        }
    }

    fn send_action(&self, action: OverlayAction) {
        let _ = self.actions.send(action);
        (self.wake)();
    }

    fn context_menu(&mut self) {
        let settings = &self.state.settings;
        unsafe {
            let menu = match CreatePopupMenu() {
                Ok(menu) => menu,
                Err(_) => return,
            };
            let labels = &self.state.labels;
            append(menu, MF_STRING, ID_OPEN_MAIN, &labels.open_main);
            append(menu, MF_STRING, ID_OPEN_SETTINGS, &labels.open_settings);
            separator(menu);
            append_checked(menu, ID_LOCK, &labels.lock, settings.locked);
            append_checked(menu, ID_CLICK_THROUGH, &labels.click_through, settings.click_through);
            append_checked(menu, ID_ALWAYS_ON_TOP, &labels.always_on_top, settings.always_on_top);
            separator(menu);

            if let Ok(lines) = CreatePopupMenu() {
                for (count, label) in [
                    (1u8, &labels.line_1),
                    (2u8, &labels.line_2),
                    (3u8, &labels.line_3),
                ] {
                    append_checked(
                        lines,
                        ID_LINES_BASE + count as usize,
                        label,
                        settings.lines == count,
                    );
                }
                let label = wide(&labels.lines);
                let _ = AppendMenuW(menu, MF_POPUP, lines.0 as usize, PCWSTR(label.as_ptr()));
            }
            if let Ok(align) = CreatePopupMenu() {
                for (index, label) in [
                    (0u8, &labels.align_left),
                    (1u8, &labels.align_center),
                    (2u8, &labels.align_right),
                ] {
                    append_checked(
                        align,
                        ID_ALIGN_BASE + index as usize,
                        label,
                        settings.align == index,
                    );
                }
                let label = wide(&labels.align);
                let _ = AppendMenuW(menu, MF_POPUP, align.0 as usize, PCWSTR(label.as_ptr()));
            }
            if let Ok(size) = CreatePopupMenu() {
                append_checked(size, ID_AUTO_SIZE, &labels.auto_size, settings.auto_size);
                separator(size);
                append(size, MF_STRING, ID_SIZE_SMALL, &labels.size_small);
                append(size, MF_STRING, ID_SIZE_MEDIUM, &labels.size_medium);
                append(size, MF_STRING, ID_SIZE_LARGE, &labels.size_large);
                let label = wide(&labels.window_size);
                let _ = AppendMenuW(menu, MF_POPUP, size.0 as usize, PCWSTR(label.as_ptr()));
            }
            append(menu, MF_STRING, ID_FONT_PLUS, &labels.font_increase);
            append(menu, MF_STRING, ID_FONT_MINUS, &labels.font_decrease);
            separator(menu);
            append(menu, MF_STRING, ID_RESET_POSITION, &labels.reset_position);
            append(menu, MF_STRING, ID_HIDE, &labels.hide);

            let mut point = POINT::default();
            if GetCursorPos(&mut point).is_err() {
                let _ = DestroyMenu(menu);
                return;
            }
            let _ = SetForegroundWindow(self.hwnd);
            let command = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_LEFTALIGN | TPM_TOPALIGN,
                point.x,
                point.y,
                None,
                self.hwnd,
                None,
            )
            .0 as usize;
            let _ = DestroyMenu(menu);
            let _ = PostMessageW(Some(self.hwnd), WM_NULL, WPARAM(0), LPARAM(0));

            let action = match command {
                ID_OPEN_MAIN => Some(OverlayAction::ShowMainWindow),
                ID_OPEN_SETTINGS => Some(OverlayAction::OpenSettings),
                ID_LOCK => Some(OverlayAction::ToggleLock),
                ID_CLICK_THROUGH => Some(OverlayAction::ToggleClickThrough),
                ID_ALWAYS_ON_TOP => Some(OverlayAction::ToggleAlwaysOnTop),
                ID_FONT_PLUS => Some(OverlayAction::FontDelta { delta: 2 }),
                ID_FONT_MINUS => Some(OverlayAction::FontDelta { delta: -2 }),
                ID_AUTO_SIZE => Some(OverlayAction::ToggleAutoSize),
                ID_SIZE_SMALL => Some(OverlayAction::SetSize {
                    width: 600.0,
                    height: 120.0,
                }),
                ID_SIZE_MEDIUM => Some(OverlayAction::SetSize {
                    width: 900.0,
                    height: 190.0,
                }),
                ID_SIZE_LARGE => Some(OverlayAction::SetSize {
                    width: 1200.0,
                    height: 280.0,
                }),
                ID_RESET_POSITION => Some(OverlayAction::ResetPosition),
                ID_HIDE => Some(OverlayAction::Hide),
                id if (ID_LINES_BASE + 1..=ID_LINES_BASE + 3).contains(&id) => {
                    Some(OverlayAction::SetLines {
                        lines: (id - ID_LINES_BASE) as u8,
                    })
                }
                id if (ID_ALIGN_BASE..ID_ALIGN_BASE + 3).contains(&id) => {
                    Some(OverlayAction::SetAlign {
                        align: (id - ID_ALIGN_BASE) as u8,
                    })
                }
                _ => None,
            };
            if let Some(action) = action {
                self.send_action(action);
            }
        }
    }

    fn report_geometry(&mut self, previous: RECT) {
        let mut rect = RECT::default();
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut rect);
        }
        let scale = self.scale();
        let width = (rect.right - rect.left) as f32 / scale;
        let height = (rect.bottom - rect.top) as f32 / scale;
        let x = rect.left as f32 / scale;
        let y = rect.top as f32 / scale;
        let resized = (rect.right - rect.left) != (previous.right - previous.left)
            || (rect.bottom - rect.top) != (previous.bottom - previous.top);
        if resized {
            self.state.settings.auto_size = false;
            self.state.settings.width = width;
            self.state.settings.height = height;
        }
        self.state.settings.position = Some((x, y));
        self.send_action(OverlayAction::Geometry {
            x,
            y,
            width,
            height,
            resized,
        });
        self.dirty = true;
    }
}

const ID_OPEN_MAIN: usize = 1001;
const ID_OPEN_SETTINGS: usize = 1002;
const ID_LOCK: usize = 1010;
const ID_CLICK_THROUGH: usize = 1011;
const ID_ALWAYS_ON_TOP: usize = 1012;
const ID_FONT_PLUS: usize = 1020;
const ID_FONT_MINUS: usize = 1021;
const ID_RESET_POSITION: usize = 1030;
const ID_HIDE: usize = 1031;
const ID_LINES_BASE: usize = 1100;
const ID_ALIGN_BASE: usize = 1200;
const ID_AUTO_SIZE: usize = 1300;
const ID_SIZE_SMALL: usize = 1301;
const ID_SIZE_MEDIUM: usize = 1302;
const ID_SIZE_LARGE: usize = 1303;

unsafe extern "system" fn wndproc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_NCCREATE {
        let create = unsafe { &*(lparam.0 as *const CREATESTRUCTW) };
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
        }
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    }
    let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut OverlayApp;
    if pointer.is_null() {
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    }
    let app = unsafe { &mut *pointer };
    match message {
        WM_CREATE => {
            app.hwnd = hwnd;
            if app.init() {
                LRESULT(0)
            } else {
                LRESULT(-1)
            }
        }
        WM_NCCALCSIZE => {
            if wparam.0 == 0 {
                let rect = unsafe { &mut *(lparam.0 as *mut RECT) };
                let mut window = RECT::default();
                unsafe {
                    let _ = GetWindowRect(hwnd, &mut window);
                }
                *rect = window;
            } else {
                let params = unsafe { &mut *(lparam.0 as *mut NCCALCSIZE_PARAMS) };
                params.rgrc[0] = params.rgrc[1];
            }
            LRESULT(0)
        }
        WM_NCHITTEST => {
            if app.state.settings.click_through {
                return LRESULT(HTTRANSPARENT as isize);
            }
            if app.state.settings.locked {
                return LRESULT(HTCLIENT as isize);
            }
            LRESULT(hit_test(hwnd).unwrap_or(HTCLIENT) as isize)
        }
        WM_SETCURSOR => {
            let hit = (lparam.0 as u32) & 0xFFFF;
            if app.state.settings.click_through {
                return LRESULT(1);
            }
            let cursor = unsafe {
                match hit {
                    HTLEFT | HTRIGHT => LoadCursorW(None, IDC_SIZEWE).ok(),
                    HTTOP | HTBOTTOM => LoadCursorW(None, IDC_SIZENS).ok(),
                    HTTOPLEFT | HTBOTTOMRIGHT => LoadCursorW(None, IDC_SIZENWSE).ok(),
                    HTTOPRIGHT | HTBOTTOMLEFT => LoadCursorW(None, IDC_SIZENESW).ok(),
                    HTCAPTION => LoadCursorW(None, IDC_SIZEALL).ok(),
                    _ => LoadCursorW(None, IDC_ARROW).ok(),
                }
            };
            if let Some(cursor) = cursor {
                unsafe {
                    SetCursor(Some(cursor));
                }
                return LRESULT(1);
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            if !app.state.settings.locked && !app.state.settings.click_through {
                unsafe {
                    let _ = ReleaseCapture();
                    let _ = SendMessageW(
                        hwnd,
                        WM_NCLBUTTONDOWN,
                        Some(WPARAM(HTCAPTION as usize)),
                        Some(LPARAM(0)),
                    );
                }
            }
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            if !app.state.settings.click_through {
                app.context_menu();
            }
            LRESULT(0)
        }
        WM_ENTERSIZEMOVE => {
            app.interaction = true;
            unsafe {
                let _ = GetWindowRect(hwnd, &mut app.last_move_rect);
            }
            LRESULT(0)
        }
        WM_EXITSIZEMOVE => {
            app.interaction = false;
            let previous = app.last_move_rect;
            app.report_geometry(previous);
            LRESULT(0)
        }
        WM_MOVING => {
            let rect = unsafe { &*(lparam.0 as *const RECT) };
            app.last_move_rect = *rect;
            LRESULT(1)
        }
        WM_TIMER if wparam.0 == TIMER_RENDER => {
            app.drain_messages();
            if app.dirty {
                app.render();
                return LRESULT(0);
            }
            let fps = app.state.settings.frame_rate.clamp(15, 120);
            let interval = Duration::from_millis((1000 / fps).max(8) as u64);
            let due = app.position_dirty
                || (app.playing && app.last_render.elapsed() >= interval);
            if !due {
                return LRESULT(0);
            }
            app.position_dirty = false;
            let active = app.active_line();
            if active != app.last_active {
                // A new line shifts the rows/colors: full frame.
                app.render();
                return LRESULT(0);
            }
            if !app.karaoke_active(active) {
                // Nothing on screen moves (paused, untimed lyrics, karaoke
                // off): skip the frame entirely.
                return LRESULT(0);
            }
            let Some(line) = active else {
                return LRESULT(0);
            };
            if !app.render_partial(line) {
                app.render();
            }
            LRESULT(0)
        }
        WM_DPICHANGED => {
            app.dpi = dpi_for_window(hwnd);
            let suggested = unsafe { &*(lparam.0 as *const RECT) };
            let scale = app.scale();
            let (x, y) = (suggested.left as f32 / scale, suggested.top as f32 / scale);
            let width = app.state.settings.width.clamp(240.0, 3000.0);
            let height = if app.state.settings.auto_size {
                render::auto_height(&app.state.settings, &app.state.title, &app.state.artist)
            } else {
                app.state.settings.height.clamp(48.0, 900.0)
            };
            app.set_logical_rect(x, y, width, height);
            app.dirty = true;
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            unsafe {
                let _ = ValidateRect(Some(hwnd), None);
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            app.shutdown();
            unsafe {
                PostQuitMessage(0);
            }
            LRESULT(0)
        }
        WM_NCDESTROY => {
            unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            }
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

fn hit_test(hwnd: HWND) -> Option<u32> {
    let mut point = POINT::default();
    unsafe {
        GetCursorPos(&mut point).ok()?;
    }
    let mut rect = RECT::default();
    unsafe {
        GetWindowRect(hwnd, &mut rect).ok()?;
    }
    if point.x < rect.left || point.x > rect.right || point.y < rect.top || point.y > rect.bottom {
        return Some(HTCLIENT);
    }
    let left = point.x <= rect.left + EDGE;
    let right = point.x >= rect.right - EDGE;
    let top = point.y <= rect.top + EDGE;
    let bottom = point.y >= rect.bottom - EDGE;
    Some(match (left, right, top, bottom) {
        (true, _, true, _) => HTTOPLEFT,
        (_, true, true, _) => HTTOPRIGHT,
        (true, _, _, true) => HTBOTTOMLEFT,
        (_, true, _, true) => HTBOTTOMRIGHT,
        (true, _, _, _) => HTLEFT,
        (_, true, _, _) => HTRIGHT,
        (_, _, true, _) => HTTOP,
        (_, _, _, true) => HTBOTTOM,
        _ => HTCLIENT,
    })
}

fn apply_overlay_attributes(hwnd: HWND) {
    unsafe {
        let dark: i32 = 1;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark as *const i32 as *const core::ffi::c_void,
            std::mem::size_of::<i32>() as u32,
        );
        let corner = DWMWCP_DONOTROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &corner as *const _ as *const core::ffi::c_void,
            std::mem::size_of_val(&corner) as u32,
        );
        let none: u32 = 0xFFFF_FFFE;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR,
            &none as *const u32 as *const core::ffi::c_void,
            std::mem::size_of::<u32>() as u32,
        );
    }
}

fn apply_shadow(hwnd: HWND, enabled: bool) {
    let policy = if enabled {
        DWMNCRP_ENABLED
    } else {
        DWMNCRP_DISABLED
    };
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_NCRENDERING_POLICY,
            &policy as *const _ as *const core::ffi::c_void,
            std::mem::size_of_val(&policy) as u32,
        );
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
    }
    apply_overlay_attributes(hwnd);
}

fn default_position(hwnd: HWND, width: f32, height: f32, scale: f32) -> (f32, f32) {
    let Some((wx, wy, ww, wh)) = work_area(hwnd, scale) else {
        return (120.0, 120.0);
    };
    (
        wx + ((ww - width) * 0.5).max(0.0),
        wy + (wh - height - 72.0).max(0.0),
    )
}

fn work_area(hwnd: HWND, scale: f32) -> Option<(f32, f32, f32, f32)> {
    let mut rect = RECT::default();
    unsafe {
        SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(&mut rect as *mut _ as *mut core::ffi::c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
        .ok()?;
    }
    if rect.right <= rect.left || rect.bottom <= rect.top {
        let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
            return None;
        }
        rect = info.rcWork;
    }
    Some((
        rect.left as f32 / scale,
        rect.top as f32 / scale,
        (rect.right - rect.left) as f32 / scale,
        (rect.bottom - rect.top) as f32 / scale,
    ))
}

fn dpi_for_window(hwnd: HWND) -> f32 {
    unsafe { GetDpiForWindow(hwnd) }.max(96) as f32
}

pub fn spawn(
    messages: Receiver<HostMessage>,
    actions: Sender<OverlayAction>,
    alive: Arc<AtomicBool>,
    wake: Arc<dyn Fn() + Send + Sync>,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("bamboo-desktop-lyrics".to_string())
        .spawn(move || {
            if let Err(err) = run(messages, actions, alive.clone(), wake) {
                tracing::error!("desktop lyrics overlay stopped: {err}");
            }
            alive.store(false, Ordering::Relaxed);
        })
}

unsafe fn append(menu: HMENU, flags: MENU_ITEM_FLAGS, id: usize, text: &str) {
    let text = wide(text);
    unsafe {
        let _ = AppendMenuW(menu, flags, id, PCWSTR(text.as_ptr()));
    }
}

unsafe fn append_checked(menu: HMENU, id: usize, text: &str, checked: bool) {
    let flags = MF_STRING
        | if checked {
            MF_CHECKED
        } else {
            MENU_ITEM_FLAGS(0)
        };
    unsafe {
        append(menu, flags, id, text);
    }
}

unsafe fn separator(menu: HMENU) {
    unsafe {
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}
