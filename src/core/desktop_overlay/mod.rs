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

use crate::accessibility::{self, UiElement};
use crate::overlay_protocol::{HostMessage, OverlayAction, OverlayLyrics, OverlayState};
use crate::platform::overlay_inbox::{self, Inbox, WM_APP_OVERLAY_WAKE};
use crate::platform::timer_resolution::TimerResolution;

const CLASS_NAME: &str = "BambooNativeDesktopLyrics";
const WINDOW_TITLE: &str = "Bamboo Desktop Lyrics";
const TIMER_RENDER: usize = 1;
/// Only runs while an opaque window covers the overlay; it is the cheap way
/// back to rendering when the overlay becomes visible again.
const TIMER_OCCLUSION: usize = 2;
const EDGE: i32 = 6;

pub fn run(
    messages: Receiver<HostMessage>,
    actions: Sender<OverlayAction>,
    alive: Arc<AtomicBool>,
    wake: Arc<dyn Fn() + Send + Sync>,
) -> windows::core::Result<()> {
    unsafe {
        // Without an explicit per-monitor-v2 context the thread reports 96 DPI
        // while Direct2D renders the layered surface at the monitor DPI: the
        // window opens at half size on a 175% display.
        let _ = windows::Win32::UI::HiDpi::SetThreadDpiAwarenessContext(
            windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        );
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
    /// Consumed once to start [`Inbox`]; the forwarder thread owns it after.
    messages: Option<Receiver<HostMessage>>,
    inbox: Option<Inbox<HostMessage>>,
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
    /// Active lyric line painted by the last full frame.
    last_active: Option<usize>,
    applied_click_through: Option<bool>,
    applied_topmost: Option<bool>,
    applied_capture: Option<bool>,
    applied_shadow: Option<bool>,
    /// Current render-timer interval; `None` = idle, no timer at all.
    timer_interval: Option<u32>,
    /// 1 ms system timer resolution, held only while a fast animation needs
    /// sub-16 ms pacing (see [`TimerResolution`]).
    resolution: TimerResolution,
    /// An opaque window covers the overlay: keep state but publish nothing.
    occluded: bool,
    last_occlusion_check: Instant,
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
            messages: Some(messages),
            inbox: None,
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
            last_active: None,
            applied_click_through: None,
            applied_topmost: None,
            applied_capture: None,
            applied_shadow: None,
            timer_interval: None,
            resolution: TimerResolution::new(),
            occluded: false,
            last_occlusion_check: Instant::now() - Duration::from_secs(1),
        }
    }

    fn init(&mut self) -> bool {
        self.dpi = dpi_for_window(self.hwnd);
        let Some(renderer) = render::Renderer::new().ok() else {
            return false;
        };
        self.renderer = Some(renderer);
        // No timer yet: the inbox forwarder posts WM_APP_OVERLAY_WAKE for the
        // initial state, and the render timer only starts once playback (or a
        // karaoke fill) actually needs continuous frames.
        if let Some(messages) = self.messages.take() {
            self.inbox = Some(Inbox::start(messages, self.hwnd));
        }
        self.apply_window_flags();
        self.apply_initial_geometry();
        true
    }

    fn shutdown(&mut self) {
        self.alive.store(false, Ordering::Relaxed);
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_RENDER);
            let _ = KillTimer(Some(self.hwnd), TIMER_OCCLUSION);
        }
        self.timer_interval = None;
        self.resolution.release();
        if let Some(inbox) = &self.inbox {
            inbox.close();
        }
        self.renderer = None;
    }

    fn drain_messages(&mut self) {
        let Some(inbox) = self.inbox.as_ref() else {
            return;
        };
        let received = inbox.drain();
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
                    // No repaint here: the render timer drives the karaoke
                    // fill from the interpolated clock, so a clock tick only
                    // re-anchors it.
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
        self.refresh_timer();
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
        if !self.initialized || self.occluded {
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
        self.dirty = false;
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

    /// UIA nodes: the current lyric text and, when enabled, the track footer.
    fn uia_elements(&self) -> Vec<UiElement> {
        let mut rect = RECT::default();
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut rect);
        }
        let full = windows::Win32::UI::Accessibility::UiaRect {
            left: rect.left as f64,
            top: rect.top as f64,
            width: (rect.right - rect.left) as f64,
            height: (rect.bottom - rect.top) as f64,
        };
        let mut text = String::new();
        if let Some(lyrics) = self.lyrics.as_ref() {
            let index = self.active_line().unwrap_or(0);
            if let Some(line) = lyrics.lines.get(index) {
                text.push_str(&line.text);
                if let Some(translation) = line.translation.as_deref()
                    && !translation.trim().is_empty()
                {
                    text.push('\n');
                    text.push_str(translation);
                }
            }
        }
        if text.trim().is_empty() {
            text = if self.state.lyrics_loading {
                self.state.labels.loading.clone()
            } else {
                self.state.labels.no_lyrics.clone()
            };
        }
        let mut elements = vec![UiElement::text(text, full)];
        if self.state.settings.show_footer && !self.state.title.trim().is_empty() {
            let footer = if self.state.artist.trim().is_empty() {
                self.state.title.clone()
            } else {
                format!("{} - {}", self.state.title, self.state.artist)
            };
            elements.push(UiElement::text(footer, full));
        }
        elements
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

    /// Render at the frame rate only when something actually moves. A paused
    /// or untimed overlay has no timer at all; a timed line without a karaoke
    /// fill only changes at line boundaries (checked at 10 Hz).
    fn desired_interval(&self) -> Option<u32> {
        if !self.initialized || self.occluded || !self.playing {
            return None;
        }
        let fps = self.state.settings.frame_rate.clamp(15, 120);
        let active = self.active_line();
        Some(if self.karaoke_active(active) {
            (1000 / fps).max(8)
        } else {
            100
        })
    }

    fn refresh_timer(&mut self) {
        let desired = self.desired_interval();
        // The default 15.6 ms system tick turns a 16 ms (60 fps) timer into
        // ~31 fps, a 33 ms (30 fps) one into ~47 ms and a 66 ms one slower
        // still. Hold 1 ms resolution for the whole animated range and drop
        // it on pause/hide; the 100 ms line-change poll does not need it.
        self.resolution
            .set(desired.is_some_and(|interval| interval < 90));
        if desired == self.timer_interval {
            return;
        }
        self.timer_interval = desired;
        unsafe {
            match desired {
                Some(interval) => {
                    SetTimer(Some(self.hwnd), TIMER_RENDER, interval.max(8), None);
                }
                None => {
                    let _ = KillTimer(Some(self.hwnd), TIMER_RENDER);
                }
            }
        }
    }

    /// Detect a fully covering opaque window. The check itself runs at most
    /// four times per second and never publishes a frame.
    fn sync_occlusion(&mut self) {
        if self.last_occlusion_check.elapsed() < Duration::from_millis(250) {
            return;
        }
        self.last_occlusion_check = Instant::now();
        let occluded = overlay_inbox::fully_occluded(self.hwnd);
        if occluded != self.occluded {
            self.occluded = occluded;
            if !occluded {
                // Nothing was published while covered; the last frame can be
                // arbitrarily old.
                self.dirty = true;
            }
            self.refresh_timer();
        }
        unsafe {
            if self.occluded {
                SetTimer(Some(self.hwnd), TIMER_OCCLUSION, 500, None);
            } else {
                let _ = KillTimer(Some(self.hwnd), TIMER_OCCLUSION);
            }
        }
    }

    /// Publish only when the visible content changed. Clock ticks do not
    /// force a frame; the karaoke row is the only continuously moving part.
    fn render_if_needed(&mut self) {
        if !self.initialized || self.occluded {
            return;
        }
        if self.dirty {
            self.render();
            return;
        }
        // The active line is derived from the live clock, so a non-karaoke
        // line boundary is noticed on the next timer tick instead of waiting
        // for the 250 ms host position message.
        let active = self.active_line();
        if active != self.last_active {
            self.render();
            return;
        }
        // The karaoke fill is derived from the live clock, which is
        // interpolated from the last 250 ms host tick. Only repainting when a
        // host position message arrived capped the fill at 4 fps no matter
        // what frame rate the settings asked for; every render tick must
        // advance the row while it is playing.
        if self.playing
            && self.karaoke_active(active)
            && let Some(line) = active
            && !self.render_partial(line)
        {
            self.render();
        }
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
            app.sync_occlusion();
            if !app.occluded {
                app.render_if_needed();
            }
            app.refresh_timer();
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == TIMER_OCCLUSION => {
            app.drain_messages();
            let was_occluded = app.occluded;
            app.occluded = overlay_inbox::fully_occluded(hwnd);
            if was_occluded && !app.occluded {
                unsafe {
                    let _ = KillTimer(Some(hwnd), TIMER_OCCLUSION);
                }
                // Repaint the whole surface: playback advanced while covered.
                app.dirty = true;
                app.render_if_needed();
            }
            app.refresh_timer();
            LRESULT(0)
        }
        WM_APP_OVERLAY_WAKE => {
            app.drain_messages();
            app.sync_occlusion();
            if !app.occluded {
                app.render_if_needed();
            }
            app.refresh_timer();
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
        WM_GETOBJECT => {
            if lparam.0 as i32 == crate::accessibility::root_object_id() {
                let provider = accessibility::publish(hwnd, app.uia_elements());
                return accessibility::return_published(hwnd, wparam, lparam, &provider);
            }
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
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
