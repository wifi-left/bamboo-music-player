//! Native layered mini-player window.
//!
//! Runs on its own thread inside the player process. It deliberately uses no
//! egui/wgpu: a `WS_EX_LAYERED` popup owns the window, Direct2D paints into a
//! DIB and `UpdateLayeredWindow` composites it. The UI thread feeds state over
//! a channel and receives button/seek/geometry actions back, so the main egui
//! window does not have to repaint to keep the mini player animated.

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
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::PCWSTR;

use crate::accessibility::{self, UiElement};
use crate::overlay_protocol::{MiniAction, MiniHostMessage, MiniState, OverlayLyrics};
use crate::platform::overlay_inbox::{self, Inbox, WM_APP_OVERLAY_WAKE};
use crate::platform::timer_resolution::TimerResolution;

const CLASS_NAME: &str = "BambooNativeMiniPlayer";
const WINDOW_TITLE: &str = "Bamboo Mini Player";
const TIMER_RENDER: usize = 1;
/// Occlusion poll; never publishes, only detects the overlay becoming visible.
const TIMER_OCCLUSION: usize = 2;
const EDGE: i32 = 6;

pub fn run(
    messages: Receiver<MiniHostMessage>,
    actions: Sender<MiniAction>,
    alive: Arc<AtomicBool>,
    wake: Arc<dyn Fn() + Send + Sync>,
) -> windows::core::Result<()> {
    unsafe {
        // Inherit the process's per-monitor-v2 awareness explicitly. Without
        // it the window is DPI-virtualized: `GetWindowRect`, cursor and mouse
        // message coordinates use different scales, which breaks both the
        // layered-surface sizing and button hit-testing.
        let _ = windows::Win32::UI::HiDpi::SetThreadDpiAwarenessContext(
            windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        );
        let instance = GetModuleHandleW(None)?;
        let class = wide(CLASS_NAME);
        let cursor = LoadCursorW(None, IDC_ARROW).unwrap_or_default();
        let window_class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW | CS_DBLCLKS,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            hCursor: cursor,
            lpszClassName: PCWSTR(class.as_ptr()),
            ..Default::default()
        };
        RegisterClassExW(&window_class);

        let mut app = MiniApp::new(messages, actions, alive.clone(), wake);
        let title = wide(WINDOW_TITLE);
        let ex_style = WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
        let style = WS_POPUP | WS_THICKFRAME;
        let hwnd = CreateWindowExW(
            ex_style,
            PCWSTR(class.as_ptr()),
            PCWSTR(title.as_ptr()),
            style,
            0,
            0,
            380,
            172,
            None,
            None,
            Some(HINSTANCE(instance.0)),
            Some(&mut app as *mut MiniApp as *const core::ffi::c_void),
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
        tracing::info!("native mini player message loop ended");
        app.shutdown();
        alive.store(false, Ordering::Relaxed);
    }
    Ok(())
}

struct MiniApp {
    hwnd: HWND,
    /// Consumed once to start [`Inbox`]; the forwarder thread owns it after.
    messages: Option<Receiver<MiniHostMessage>>,
    inbox: Option<Inbox<MiniHostMessage>>,
    actions: Sender<MiniAction>,
    alive: Arc<AtomicBool>,
    /// Wakes the hidden main window so it drains the action queue promptly
    /// instead of relying on a periodic heartbeat.
    wake: Arc<dyn Fn() + Send + Sync>,
    renderer: Option<render::Renderer>,
    state: MiniState,
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
    /// Repaint everything (state/layout/text changed).
    full_dirty: bool,
    /// Region that changed since the last frame (clock-driven content).
    pending: Option<render::BoxRect>,
    /// Active lyric line painted by the last frame.
    last_active: Option<usize>,
    /// Last published time labels; the progress bar advances every frame but
    /// the text only needs a new publish when the second changes.
    last_time_left: String,
    last_time_right: String,
    /// Current render-timer interval; `None` = idle, no timer at all.
    timer_interval: Option<u32>,
    /// 1 ms system timer resolution, held only while fast frames are needed.
    resolution: TimerResolution,
    /// An opaque window covers the mini player: keep state but publish nothing.
    occluded: bool,
    last_occlusion_check: Instant,
    hover: render::Hover,
    seek_preview: Option<f32>,
    /// Target of a seek sent to the host. Stale position ticks are ignored
    /// until the engine position reaches it, otherwise the bar snaps back.
    seek_pending: Option<(u64, Instant)>,
    applied_topmost: Option<bool>,
    applied_taskbar: Option<bool>,
}

impl MiniApp {
    fn new(
        messages: Receiver<MiniHostMessage>,
        actions: Sender<MiniAction>,
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
            state: MiniState::default(),
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
            full_dirty: true,
            pending: None,
            last_active: None,
            last_time_left: String::new(),
            last_time_right: String::new(),
            timer_interval: None,
            resolution: TimerResolution::new(),
            occluded: false,
            last_occlusion_check: Instant::now() - Duration::from_secs(1),
            hover: render::Hover::None,
            seek_preview: None,
            seek_pending: None,
            applied_topmost: None,
            applied_taskbar: None,
        }
    }

    fn invalidate_full(&mut self) {
        self.full_dirty = true;
        self.pending = None;
    }

    fn invalidate(&mut self, region: render::BoxRect) {
        if self.full_dirty {
            return;
        }
        self.pending = Some(match self.pending.take() {
            Some(current) => current.union(region),
            None => region,
        });
    }

    fn current_layout(&self) -> render::Layout {
        let (width, height) = self.window_logical_size();
        render::layout(width, height)
    }

    fn dynamic_region(&self) -> render::BoxRect {
        let ui = self.current_layout();
        let mut region = ui.progress_zone();
        if self.state.show_lyrics {
            region = region.union(ui.lyric);
        }
        region
    }

    /// UIA nodes for the mini player: text, transport buttons and the
    /// progress slider. Screen rectangles are physical pixels.
    fn uia_elements(&self) -> Vec<UiElement> {
        let mut window = RECT::default();
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut window);
        }
        let scale = self.scale();
        let to_uia = |rect: render::BoxRect| windows::Win32::UI::Accessibility::UiaRect {
            left: window.left as f64 + rect.left as f64 * scale as f64,
            top: window.top as f64 + rect.top as f64 * scale as f64,
            width: (rect.right - rect.left) as f64 * scale as f64,
            height: (rect.bottom - rect.top) as f64 * scale as f64,
        };
        let packed = |rect: render::BoxRect| {
            let x = ((rect.left + rect.right) * 0.5 * scale).round() as i32;
            let y = ((rect.top + rect.bottom) * 0.5 * scale).round() as i32;
            (((y as u16 as u32) << 16) | (x as u16 as u32)) as isize
        };
        let ui = self.current_layout();
        let mut elements = Vec::new();
        let title = if self.state.artist.trim().is_empty() {
            self.state.title.clone()
        } else {
            format!("{} - {}", self.state.title, self.state.artist)
        };
        if !title.trim().is_empty() {
            elements.push(UiElement::text(
                title,
                windows::Win32::UI::Accessibility::UiaRect {
                    left: window.left as f64 + ui.text_left as f64 * scale as f64,
                    top: window.top as f64 + 14.0 * scale as f64,
                    width: (ui.text_right - ui.text_left).max(1.0) as f64 * scale as f64,
                    height: 30.0 * scale as f64,
                },
            ));
        }
        if self.state.show_lyrics {
            let text = self
                .lyrics
                .as_ref()
                .and_then(|lyrics| {
                    let index = self.active_line()?;
                    lyrics.lines.get(index).map(|line| line.text.clone())
                })
                .unwrap_or_else(|| {
                    if self.state.lyrics_loading {
                        self.state.labels.loading.clone()
                    } else {
                        self.state.labels.no_lyrics.clone()
                    }
                });
            elements.push(UiElement::text(text, to_uia(ui.lyric)));
        }
        for (name, rect) in [
            (&self.state.labels.prev, ui.prev),
            (
                if self.playing {
                    &self.state.labels.pause
                } else {
                    &self.state.labels.play
                },
                ui.play,
            ),
            (&self.state.labels.next, ui.next),
            (
                if self.state.muted {
                    &self.state.labels.unmute
                } else {
                    &self.state.labels.mute
                },
                ui.mute,
            ),
            (&self.state.labels.close, ui.close),
        ] {
            elements.push(UiElement::button(name.clone(), to_uia(rect), packed(rect)));
        }
        let duration = self.state.duration_ms;
        elements.push(UiElement::slider(
            crate::i18n::tr("播放进度"),
            to_uia(ui.progress),
            packed(ui.progress),
            self.visible_position_ms() as f64,
            0.0,
            duration.max(1) as f64,
            5000.0,
            10_000.0,
            duration == 0,
            format_time(self.visible_position_ms() / 1000),
        ));
        elements
    }

    fn active_line(&self) -> Option<usize> {
        let lyrics = self.lyrics.as_ref()?;
        if !lyrics.timed || lyrics.lines.is_empty() {
            return None;
        }
        let position = self.live_position_ms() as i64;
        let user = self.state.offset_ms;
        let adjusted = if user >= 0 {
            position.saturating_sub(user)
        } else {
            position.saturating_add(-user)
        }
        .max(0) as u64;
        crate::mini_overlay::render::index_at(lyrics, adjusted)
    }
    /// True when the karaoke fill advances within the active line.
    fn karaoke_active(&self, line: Option<usize>) -> bool {
        if !self.state.show_lyrics || !self.state.karaoke {
            return false;
        }
        let (Some(index), Some(lyrics)) = (line, self.lyrics.as_ref()) else {
            return false;
        };
        let Some(line) = lyrics.lines.get(index) else {
            return false;
        };
        !self.state.karaoke_words_only || !line.words.is_empty()
    }

    fn hover_zone(&self, hover: render::Hover) -> Option<render::BoxRect> {
        let ui = self.current_layout();
        Some(match hover {
            render::Hover::None => return None,
            render::Hover::Close => ui.close,
            render::Hover::Mute => ui.mute_zone(),
            render::Hover::Prev => ui.prev,
            render::Hover::Play => ui.play,
            render::Hover::Next => ui.next,
            render::Hover::Progress => ui.progress_zone(),
        })
    }

    fn init(&mut self) -> bool {
        self.dpi = dpi_for_window(self.hwnd);
        let Some(renderer) = render::Renderer::new().ok() else {
            return false;
        };
        self.renderer = Some(renderer);
        // The inbox forwarder wakes the window for the first state; a render
        // timer is only needed while the progress bar/lyrics actually move.
        if let Some(messages) = self.messages.take() {
            self.inbox = Some(Inbox::start(messages, self.hwnd));
        }
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
                MiniHostMessage::State(state) => self.apply_state(*state),
                MiniHostMessage::Position {
                    position_ms,
                    playing,
                } => {
                    let playing_changed = playing != self.playing;
                    self.playing = playing;
                    // The renderer paints the transport icon from the cached
                    // state; keep it in sync with the cheap position ticks.
                    self.state.playing = playing;
                    self.position_at = Instant::now();
                    let mut settled = true;
                    if let Some((target, at)) = self.seek_pending {
                        if position_ms.abs_diff(target) <= 1_500
                            || at.elapsed() >= Duration::from_millis(1_500)
                        {
                            self.seek_pending = None;
                            self.seek_preview = None;
                        } else {
                            // Tick emitted before the seek landed: keep the
                            // dragged position on screen instead of bouncing.
                            settled = false;
                        }
                    }
                    if settled {
                        self.position_ms = position_ms;
                    }
                    if playing_changed {
                        // Play/pause swaps the transport glyph and the volume
                        // tick, which lives outside the clock-driven region.
                        self.invalidate_full();
                    } else {
                        self.invalidate(self.dynamic_region());
                    }
                }
                MiniHostMessage::ResetPosition => {
                    self.reset_position();
                    self.invalidate_full();
                }
                MiniHostMessage::Shutdown => {
                    tracing::info!("native mini player got shutdown");
                    unsafe {
                        let _ = DestroyWindow(self.hwnd);
                    }
                    return;
                }
            }
        }
    }

    fn apply_state(&mut self, mut state: MiniState) {
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
        let old_width = self.state.width;
        let old_height = self.state.height;
        self.state = state;
        self.position_at = Instant::now();
        self.playing = self.state.playing;
        self.position_ms = self.state.position_ms;
        if first {
            tracing::info!(
                "native mini player first state: {}x{} opacity={} colors.panel={:?}",
                self.state.width,
                self.state.height,
                self.state.opacity,
                self.state.colors.panel
            );
        }
        self.initialized = true;
        if first {
            self.apply_initial_geometry();
        } else if !self.interaction
            && ((self.state.width - old_width).abs() > 0.5
                || (self.state.height - old_height).abs() > 0.5)
        {
            self.apply_size();
        }
        self.apply_window_flags();
        self.invalidate_full();
        self.refresh_timer();
    }

    fn apply_window_flags(&mut self) {
        let topmost = self.state.always_on_top;
        if self.applied_topmost != Some(topmost) {
            self.applied_topmost = Some(topmost);
            unsafe {
                let _ = SetWindowPos(
                    self.hwnd,
                    Some(if topmost {
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
        let taskbar = self.state.show_in_taskbar;
        if self.applied_taskbar != Some(taskbar) {
            self.applied_taskbar = Some(taskbar);
            unsafe {
                // The shell only picks up a tool-window change while the
                // window is hidden, so hide/show around the ex-style update.
                let _ = ShowWindow(self.hwnd, SW_HIDE);
                let mut style = GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE);
                style &= !((WS_EX_TOOLWINDOW.0 | WS_EX_APPWINDOW.0) as isize);
                style |= if taskbar {
                    WS_EX_APPWINDOW.0 as isize
                } else {
                    WS_EX_TOOLWINDOW.0 as isize
                };
                SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, style);
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            }
        }
        apply_overlay_attributes(self.hwnd);
    }

    fn apply_initial_geometry(&mut self) {
        let width = self.state.width.clamp(280.0, 900.0);
        let height = self.state.height.clamp(120.0, 600.0);
        let (x, y) = self.state.position.map_or_else(
            || default_position(self.hwnd, width, height, self.scale()),
            |(x, y)| (x, y),
        );
        self.set_logical_rect(x, y, width, height);
    }

    fn apply_size(&mut self) {
        let width = self.state.width.clamp(280.0, 900.0);
        let height = self.state.height.clamp(120.0, 600.0);
        let (x, y) = self.window_logical_position();
        self.set_logical_size(width, height, x, y);
    }

    fn reset_position(&mut self) {
        let (width, height) = self.window_logical_size();
        let (x, y) = default_position(self.hwnd, width, height, self.scale());
        self.set_logical_rect(x, y, width, height);
        self.send_action(MiniAction::Geometry {
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
        self.invalidate_full();
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

    fn visible_position_ms(&self) -> u64 {
        if self.state.duration_ms > 0 {
            self.live_position_ms().min(self.state.duration_ms)
        } else {
            self.live_position_ms()
        }
    }

    fn time_labels(&self) -> (String, String) {
        (
            format_time(self.visible_position_ms() / 1000),
            format_time(self.state.duration_ms / 1000),
        )
    }

    fn render(&mut self, dirty: Option<render::BoxRect>) {
        if !self.initialized || self.occluded {
            return;
        }
        let mut rect = RECT::default();
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut rect);
        }
        let width_px = (rect.right - rect.left).max(1) as u32;
        let height_px = (rect.bottom - rect.top).max(1) as u32;
        let position = self.visible_position_ms();
        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };
        renderer.render(
            self.hwnd,
            &self.state,
            self.lyrics.as_ref(),
            position,
            width_px,
            height_px,
            self.dpi,
            self.seek_preview,
            self.hover,
            dirty,
        );
        self.full_dirty = false;
        self.pending = None;
        self.last_active = self.active_line();
        let (left, right) = self.time_labels();
        self.last_time_left = left;
        self.last_time_right = right;
    }

    /// The mini player animates the progress bar and its time labels whenever
    /// playback advances; paused/occluded needs no periodic timer at all.
    fn desired_interval(&self) -> Option<u32> {
        if !self.initialized || self.occluded || !self.playing {
            return None;
        }
        let fps = if self.state.frame_rate == 0 {
            60
        } else {
            self.state.frame_rate.clamp(15, 120)
        };
        Some((1000 / fps).max(8))
    }

    fn refresh_timer(&mut self) {
        let desired = self.desired_interval();
        // Sub-90 ms animation intervals are distorted by the default 15.6 ms
        // system tick (60 fps becomes ~31 fps); hold 1 ms resolution only
        // while the animation needs it and release it on pause/hide.
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

    fn sync_occlusion(&mut self) {
        if self.last_occlusion_check.elapsed() < Duration::from_millis(250) {
            return;
        }
        self.last_occlusion_check = Instant::now();
        let occluded = overlay_inbox::fully_occluded(self.hwnd);
        if occluded != self.occluded {
            self.occluded = occluded;
            if !occluded {
                // The clock advanced while covered; repaint everything.
                self.invalidate_full();
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

    /// Publish any pending region now. Used after host messages and mouse
    /// input, so a paused overlay still reacts without a render timer.
    fn flush(&mut self) {
        self.sync_occlusion();
        if !self.occluded {
            if self.full_dirty {
                self.render(None);
            } else if let Some(region) = self.pending {
                self.render(Some(region));
            }
        }
        self.refresh_timer();
    }

    fn send_action(&self, action: MiniAction) {
        let _ = self.actions.send(action);
        // The main window may be hidden and not painting; run the host's
        // action handler now instead of waiting for the next heartbeat.
        (self.wake)();
    }

    /// Cursor position in logical client coordinates.
    ///
    /// Do not trust mouse-message `lparam` here: on a per-monitor-DPI layered
    /// window those coordinates arrive in screen space (and `ScreenToClient`
    /// applies the same virtualization), which made every button hit-test
    /// miss on 175% displays. `GetCursorPos` and `GetWindowRect` are both in
    /// physical screen pixels for this thread, so subtract and scale.
    fn cursor_client(&self) -> Option<(f32, f32)> {
        let mut point = POINT::default();
        if unsafe { GetCursorPos(&mut point) }.is_err() {
            return None;
        }
        let mut rect = RECT::default();
        if unsafe { GetWindowRect(self.hwnd, &mut rect) }.is_err() {
            return None;
        }
        let scale = self.scale();
        Some((
            (point.x - rect.left) as f32 / scale,
            (point.y - rect.top) as f32 / scale,
        ))
    }

    fn layout(&self) -> (render::Layout, f32) {
        let (width, height) = self.window_logical_size();
        (render::layout(width, height), width)
    }

    fn hit_test(&self, x: f32, y: f32) -> render::Hover {
        let (ui, _) = self.layout();
        if ui.close.contains(x, y) {
            render::Hover::Close
        } else if ui.mute.contains(x, y) {
            render::Hover::Mute
        } else if ui.prev.contains(x, y) {
            render::Hover::Prev
        } else if ui.play.contains(x, y) {
            render::Hover::Play
        } else if ui.next.contains(x, y) {
            render::Hover::Next
        } else if ui.progress.contains(x, y) {
            render::Hover::Progress
        } else {
            render::Hover::None
        }
    }

    fn update_hover(&mut self) {
        let hover = self
            .cursor_client()
            .map(|(x, y)| self.hit_test(x, y))
            .unwrap_or(render::Hover::None);
        if hover != self.hover {
            let old = self.hover_zone(self.hover);
            let new = self.hover_zone(hover);
            self.hover = hover;
            match (old, new) {
                (Some(a), Some(b)) => self.invalidate(a.union(b)),
                (Some(a), None) | (None, Some(a)) => self.invalidate(a),
                (None, None) => {}
            }
        }
    }

    fn seek_ratio(&self, x: f32) -> Option<f32> {
        let (ui, _) = self.layout();
        let width = ui.progress_bar.right - ui.progress_bar.left;
        if width <= 1.0 {
            return None;
        }
        Some(((x - ui.progress_bar.left) / width).clamp(0.0, 1.0))
    }

    fn button_down(&mut self) {
        let Some((x, y)) = self.cursor_client() else {
            return;
        };
        let hover = self.hit_test(x, y);
        match hover {
            render::Hover::Close => self.send_action(MiniAction::Hide),
            render::Hover::Mute => self.send_action(MiniAction::ToggleMute),
            render::Hover::Prev => self.send_action(MiniAction::Prev),
            render::Hover::Play => self.send_action(MiniAction::PlayPause),
            render::Hover::Next => self.send_action(MiniAction::Next),
            render::Hover::Progress => {
                if let Some(ratio) = self.seek_ratio(x) {
                    self.seek_preview = Some(ratio);
                    unsafe {
                        let _ = SetCapture(self.hwnd);
                    }
                    self.invalidate(self.current_layout().progress_zone());
                }
            }
            render::Hover::None => unsafe {
                // Dragging empty space moves the borderless window.
                let _ = ReleaseCapture();
                let _ = SendMessageW(
                    self.hwnd,
                    WM_NCLBUTTONDOWN,
                    Some(WPARAM(HTCAPTION as usize)),
                    Some(LPARAM(0)),
                );
            },
        }
    }

    fn button_up(&mut self) {
        if let Some(ratio) = self.seek_preview.take() {
            unsafe {
                let _ = ReleaseCapture();
            }
            let position_ms = (ratio as f64 * self.state.duration_ms as f64).round() as u64;
            self.send_action(MiniAction::Seek { position_ms });
            self.position_ms = position_ms;
            self.position_at = Instant::now();
            // Keep showing the target until the host confirms it; otherwise
            // the next stale tick would make the bar snap back.
            self.seek_preview = Some(ratio);
            self.seek_pending = Some((position_ms, Instant::now()));
            self.invalidate(self.current_layout().progress_zone());
        }
    }

    fn wheel(&mut self, delta: i16) {
        let Some((x, y)) = self.cursor_client() else {
            return;
        };
        let (ui, _) = self.layout();
        // Volume is adjusted over the mute/level indicator, not the whole
        // window: scrolling anywhere else should not change the volume by
        // accident.
        let mute_zone = render::BoxRect {
            left: ui.mute.left - 2.0,
            top: ui.mute.top - 4.0,
            right: ui.mute.right + 2.0,
            bottom: ui.mute.bottom + 8.0,
        };
        if !mute_zone.contains(x, y) {
            return;
        }
        let volume = (self.state.volume + delta as f32 / 120.0 * 0.05).clamp(0.0, 1.0);
        if (volume - self.state.volume).abs() > f32::EPSILON {
            self.state.volume = volume;
            self.state.muted = false;
            self.send_action(MiniAction::SetVolume { volume });
            self.invalidate(self.current_layout().mute_zone());
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
            self.state.width = width;
            self.state.height = height;
            self.invalidate_full();
        }
        self.send_action(MiniAction::Geometry {
            x,
            y,
            width,
            height,
            resized,
        });
    }
}

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
    let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut MiniApp;
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
        WM_NCHITTEST => LRESULT(hit_test(hwnd).unwrap_or(HTCLIENT) as isize),
        WM_SETCURSOR => {
            let hit = (lparam.0 as u32) & 0xFFFF;
            let cursor = unsafe {
                match hit {
                    HTLEFT | HTRIGHT => LoadCursorW(None, IDC_SIZEWE).ok(),
                    HTTOP | HTBOTTOM => LoadCursorW(None, IDC_SIZENS).ok(),
                    HTTOPLEFT | HTBOTTOMRIGHT => LoadCursorW(None, IDC_SIZENWSE).ok(),
                    HTTOPRIGHT | HTBOTTOMLEFT => LoadCursorW(None, IDC_SIZENESW).ok(),
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
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_LBUTTONDBLCLK => {
            // Double-clicking empty space (cover/title area) returns to the
            // main window; controls keep their normal click behaviour.
            if let Some((x, y)) = app.cursor_client()
                && app.hit_test(x, y) == render::Hover::None
            {
                app.send_action(MiniAction::ShowMain);
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            app.button_down();
            app.flush();
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            app.button_up();
            app.flush();
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            if app.seek_preview.is_some() {
                if let Some((x, _)) = app.cursor_client()
                    && let Some(ratio) = app.seek_ratio(x)
                {
                    app.seek_preview = Some(ratio);
                    app.invalidate(app.current_layout().progress_zone());
                }
            } else {
                app.update_hover();
            }
            app.flush();
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta = ((wparam.0 >> 16) & 0xFFFF) as u16 as i16;
            app.wheel(delta);
            app.flush();
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
            app.flush();
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
            if app.occluded {
                app.refresh_timer();
                return LRESULT(0);
            }
            if app.full_dirty {
                app.render(None);
            } else if let Some(region) = app.pending {
                app.render(Some(region));
            } else if app.playing {
                // The clock advances every frame. Publish only the bar most of
                // the time; the time labels get a new publish when their
                // displayed second changes, and lyrics only while they move.
                let (left, right) = app.time_labels();
                let time_changed =
                    left != app.last_time_left || right != app.last_time_right;
                let active = app.active_line();
                let lyric_changed = app.state.show_lyrics
                    && (active != app.last_active || app.karaoke_active(active));
                let ui = app.current_layout();
                let mut region = ui.bar_zone();
                if time_changed {
                    region = region.union(ui.time_zone());
                }
                if lyric_changed {
                    region = region.union(ui.lyric);
                }
                app.render(Some(region));
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
                app.invalidate_full();
                app.flush();
            }
            LRESULT(0)
        }
        WM_APP_OVERLAY_WAKE => {
            app.drain_messages();
            app.flush();
            LRESULT(0)
        }
        WM_DPICHANGED => {
            app.dpi = dpi_for_window(hwnd);
            let suggested = unsafe { &*(lparam.0 as *const RECT) };
            let scale = app.scale();
            let (x, y) = (suggested.left as f32 / scale, suggested.top as f32 / scale);
            let width = app.state.width.clamp(280.0, 900.0);
            let height = app.state.height.clamp(120.0, 600.0);
            app.set_logical_rect(x, y, width, height);
            app.invalidate_full();
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
        crate::accessibility::WM_APP_UIA_ACTIVATE => {
            let packed = wparam.0 as isize;
            unsafe {
                let _ = PostMessageW(Some(hwnd), WM_LBUTTONDOWN, WPARAM(1), LPARAM(packed));
                let _ = PostMessageW(Some(hwnd), WM_LBUTTONUP, WPARAM(0), LPARAM(packed));
            }
            LRESULT(0)
        }
        crate::accessibility::WM_APP_UIA_RANGE => {
            if wparam.0 == 2 {
                let value = f64::from_bits(lparam.0 as u64).max(0.0) as u64;
                app.position_ms = value;
                app.position_at = Instant::now();
                app.send_action(MiniAction::Seek { position_ms: value });
                app.invalidate(app.current_layout().progress_zone());
                app.flush();
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

#[allow(dead_code)]
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
        wx + (ww - width - 32.0).max(0.0),
        wy + (wh - height - 64.0).max(0.0),
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
    messages: Receiver<MiniHostMessage>,
    actions: Sender<MiniAction>,
    alive: Arc<AtomicBool>,
    wake: Arc<dyn Fn() + Send + Sync>,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("bamboo-mini-player".to_string())
        .spawn(move || {
            if let Err(err) = run(messages, actions, alive.clone(), wake) {
                tracing::error!("native mini player stopped: {err}");
            }
            alive.store(false, Ordering::Relaxed);
        })
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn format_time(seconds: u64) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}
