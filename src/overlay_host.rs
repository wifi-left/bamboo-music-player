//! Host side of the native overlay windows.
//!
//! Both overlays (`core::desktop_overlay`, `core::mini_overlay`) run on their
//! own thread and speak a small channel protocol. This module turns the player
//! `Snapshot` + settings into `OverlayState`/`MiniState`, sends cheap position
//! ticks so they can interpolate, and exposes the actions they post back.
//!
//! It owns no playback logic: the main window translates every action into a
//! `Command` (settings/playback) or a window action (show/hide main window).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, unbounded};

use crate::app::App;
use crate::config::{DesktopBackgroundMode, LyricsAlign, MiniPlayerSettings};
use crate::core::desktop_overlay;
use crate::core::mini_overlay;
use crate::lyrics::Lyrics;
use crate::overlay_protocol::{
    HostMessage, MiniAction, MiniColors, MiniHostMessage, MiniLabels, MiniState, OverlayAction,
    OverlayLabels, OverlayLine, OverlayLyrics, OverlaySettings, OverlayState, OverlayWord,
};

const STATE_INTERVAL: Duration = Duration::from_millis(500);
const POSITION_INTERVAL: Duration = Duration::from_millis(250);

/// Whether a full state must be pushed now.
///
/// `playing_changed` is load-bearing: pausing the player stops the main
/// window's animation timer, so there is no later heartbeat that could deliver
/// the new transport glyph. Without this the overlay kept showing "pause"
/// until the user dragged it (which woke the host through a geometry action).
fn state_due(
    sent: bool,
    force: bool,
    playing_changed: bool,
    lyrics_changed: bool,
    cover_changed: bool,
    since_state: Duration,
) -> bool {
    !sent
        || force
        || playing_changed
        || lyrics_changed
        || cover_changed
        || since_state >= STATE_INTERVAL
}

pub struct DesktopHost {
    messages: Sender<HostMessage>,
    actions: Receiver<OverlayAction>,
    alive: Arc<AtomicBool>,
    sent: bool,
    playing: bool,
    force_state: bool,
    lyrics_version: u64,
    cover_version: u64,
    last_state: Instant,
    last_position: Instant,
}

impl DesktopHost {
    pub fn new(wake: Arc<dyn Fn() + Send + Sync>) -> std::io::Result<Self> {
        let (messages, receive) = unbounded::<HostMessage>();
        let (send_actions, actions) = unbounded::<OverlayAction>();
        let alive = Arc::new(AtomicBool::new(false));
        let _ = desktop_overlay::spawn(receive, send_actions, alive.clone(), wake)?;
        Ok(Self {
            messages,
            actions,
            alive,
            sent: false,
            playing: false,
            force_state: false,
            lyrics_version: u64::MAX,
            cover_version: u64::MAX,
            last_state: Instant::now() - STATE_INTERVAL,
            last_position: Instant::now() - POSITION_INTERVAL,
        })
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }

    /// Make the next `sync` send a complete state even if the heartbeat
    /// interval has not elapsed. Used after an overlay action, because a
    /// paused player stops the main timer that would otherwise push the next
    /// tick.
    pub fn force_state(&mut self) {
        self.force_state = true;
    }

    pub fn sync(&mut self, app: &App) {
        if !self.is_alive() {
            return;
        }
        let now = Instant::now();
        let snapshot = app.core.snapshot();
        let playing = app.is_playing();
        let lyrics_changed = snapshot.lyrics_rev != self.lyrics_version;
        let cover_changed = snapshot.cover_rev != self.cover_version;
        // A play/pause transition must reach the overlay immediately: after a
        // pause the main window kills its timer, so no later tick is coming.
        let playing_changed = playing != self.playing;
        let state_due = state_due(
            self.sent,
            self.force_state,
            playing_changed,
            lyrics_changed,
            cover_changed,
            now.duration_since(self.last_state),
        );
        if state_due {
            let state = OverlayState {
                settings: desktop_settings(app),
                title: snapshot
                    .current
                    .map(|track| track.display_title())
                    .unwrap_or_default(),
                artist: snapshot
                    .current
                    .map(|track| track.display_artist())
                    .unwrap_or_default(),
                lyrics_version: snapshot.lyrics_rev,
                lyrics: (lyrics_changed || !self.sent)
                    .then(|| overlay_lyrics(snapshot.lyrics)),
                lyrics_loading: snapshot.lyrics_loading,
                playing,
                position_ms: (snapshot.position_secs * 1000.0).max(0.0) as u64,
                // The overlay subtracts this value; negate so a positive
                // settings offset advances lyrics like the embedded view.
                offset_ms: -app.core.settings.lyrics.offset_ms,
                cover_version: snapshot.cover_rev,
                cover_png: (cover_changed || !self.sent)
                    .then(|| snapshot.current_cover_png.map(|png| png.to_vec()))
                    .flatten(),
                labels: overlay_labels(),
            };
            let _ = self.messages.send(HostMessage::State(Box::new(state)));
            self.lyrics_version = snapshot.lyrics_rev;
            self.cover_version = snapshot.cover_rev;
            self.last_state = now;
            self.last_position = now;
            self.sent = true;
            self.playing = playing;
            self.force_state = false;
            return;
        }
        if now.duration_since(self.last_position) >= POSITION_INTERVAL {
            let _ = self.messages.send(HostMessage::Position {
                position_ms: (snapshot.position_secs * 1000.0).max(0.0) as u64,
                playing,
            });
            self.last_position = now;
            self.playing = playing;
        }
    }

    pub fn reset_position(&self) {
        let _ = self.messages.send(HostMessage::ResetPosition);
    }

    pub fn poll(&self) -> Vec<OverlayAction> {
        let mut out = Vec::new();
        while let Ok(action) = self.actions.try_recv() {
            out.push(action);
        }
        out
    }
}

impl Drop for DesktopHost {
    fn drop(&mut self) {
        let _ = self.messages.send(HostMessage::Shutdown);
    }
}

pub struct MiniHost {
    messages: Sender<MiniHostMessage>,
    actions: Receiver<MiniAction>,
    alive: Arc<AtomicBool>,
    sent: bool,
    playing: bool,
    force_state: bool,
    lyrics_version: u64,
    cover_version: u64,
    last_state: Instant,
    last_position: Instant,
}

impl MiniHost {
    pub fn new(wake: Arc<dyn Fn() + Send + Sync>) -> std::io::Result<Self> {
        let (messages, receive) = unbounded::<MiniHostMessage>();
        let (send_actions, actions) = unbounded::<MiniAction>();
        let alive = Arc::new(AtomicBool::new(false));
        let _ = mini_overlay::spawn(receive, send_actions, alive.clone(), wake)?;
        Ok(Self {
            messages,
            actions,
            alive,
            sent: false,
            playing: false,
            force_state: false,
            lyrics_version: u64::MAX,
            cover_version: u64::MAX,
            last_state: Instant::now() - STATE_INTERVAL,
            last_position: Instant::now() - POSITION_INTERVAL,
        })
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }

    pub fn force_state(&mut self) {
        self.force_state = true;
    }

    pub fn sync(&mut self, app: &App) {
        if !self.is_alive() {
            return;
        }
        let now = Instant::now();
        let snapshot = app.core.snapshot();
        let playing = app.is_playing();
        let lyrics_changed = snapshot.lyrics_rev != self.lyrics_version;
        let cover_changed = snapshot.cover_rev != self.cover_version;
        // See `DesktopHost::sync`: a pause stops the main timer, so the
        // transition has to be pushed with this call.
        let playing_changed = playing != self.playing;
        let state_due = state_due(
            self.sent,
            self.force_state,
            playing_changed,
            lyrics_changed,
            cover_changed,
            now.duration_since(self.last_state),
        );
        if state_due {
            let settings = &app.core.settings.mini;
            let height = if settings.auto_height {
                if settings.show_lyrics { 172.0 } else { 128.0 }
            } else {
                settings.height
            };
            let state = MiniState {
                title: snapshot
                    .current
                    .map(|track| track.display_title())
                    .unwrap_or_default(),
                artist: snapshot
                    .current
                    .map(|track| track.display_artist())
                    .unwrap_or_default(),
                playing,
                position_ms: (snapshot.position_secs * 1000.0).max(0.0) as u64,
                duration_ms: (snapshot.duration_secs * 1000.0).max(0.0) as u64,
                volume: snapshot.volume,
                muted: snapshot.muted,
                cover_version: snapshot.cover_rev,
                cover_png: (cover_changed || !self.sent)
                    .then(|| snapshot.current_cover_png.map(|png| png.to_vec()))
                    .flatten(),
                lyrics_version: snapshot.lyrics_rev,
                lyrics: (lyrics_changed || !self.sent)
                    .then(|| overlay_lyrics(snapshot.lyrics)),
                lyrics_loading: snapshot.lyrics_loading,
                offset_ms: -app.core.settings.lyrics.offset_ms,
                show_lyrics: settings.show_lyrics,
                karaoke: settings.karaoke,
                karaoke_words_only: settings.karaoke_words_only,
                lyrics_lines: settings.lyrics_lines.clamp(1, 2),
                lyrics_bold: settings.lyrics_bold,
                show_translation: settings.show_translation,
                lyrics_font_size: settings.lyrics_font_size,
                width: settings.width,
                height,
                opacity: settings.opacity,
                always_on_top: settings.always_on_top,
                show_in_taskbar: settings.show_in_taskbar,
                frame_rate: settings.frame_rate.clamp(15, 120),
                d2d_hardware: app.core.settings.performance.d2d_hardware,
                font_family: app.core.settings.theme.font_family.clone(),
                colors: mini_colors(settings),
                position: settings.position,
                labels: mini_labels(),
            };
            let _ = self.messages.send(MiniHostMessage::State(Box::new(state)));
            self.lyrics_version = snapshot.lyrics_rev;
            self.cover_version = snapshot.cover_rev;
            self.last_state = now;
            self.last_position = now;
            self.sent = true;
            self.playing = playing;
            self.force_state = false;
            return;
        }
        if now.duration_since(self.last_position) >= POSITION_INTERVAL {
            let _ = self.messages.send(MiniHostMessage::Position {
                position_ms: (snapshot.position_secs * 1000.0).max(0.0) as u64,
                playing,
            });
            self.last_position = now;
            self.playing = playing;
        }
    }

    pub fn poll(&self) -> Vec<MiniAction> {
        let mut out = Vec::new();
        while let Ok(action) = self.actions.try_recv() {
            out.push(action);
        }
        out
    }
}

impl Drop for MiniHost {
    fn drop(&mut self) {
        let _ = self.messages.send(MiniHostMessage::Shutdown);
    }
}

fn overlay_lyrics(lyrics: Option<&Lyrics>) -> OverlayLyrics {
    let Some(lyrics) = lyrics else {
        return OverlayLyrics::default();
    };
    OverlayLyrics {
        lines: lyrics
            .lines
            .iter()
            .map(|line| OverlayLine {
                start_ms: line.start_ms,
                end_ms: line.end_ms,
                text: line.text.clone(),
                translation: line.translation.clone(),
                words: line
                    .words
                    .iter()
                    .map(|word| OverlayWord {
                        start_ms: word.start_ms,
                        end_ms: word.end_ms,
                        text: word.text.clone(),
                    })
                    .collect(),
            })
            .collect(),
        offset_ms: lyrics.offset_ms,
        timed: lyrics.timed,
    }
}

fn desktop_settings(app: &App) -> OverlaySettings {
    let settings = &app.core.settings.desktop_lyrics;
    let align = match settings.align {
        LyricsAlign::Left => 0,
        LyricsAlign::Center => 1,
        LyricsAlign::Right => 2,
    };
    let background = match settings.background {
        DesktopBackgroundMode::Transparent => 0,
        DesktopBackgroundMode::Solid => 1,
        DesktopBackgroundMode::Gradient => 2,
        DesktopBackgroundMode::Image => 3,
        DesktopBackgroundMode::BlurCover => 4,
    };
    OverlaySettings {
        locked: settings.locked,
        click_through: settings.click_through,
        always_on_top: settings.always_on_top,
        lines: settings.lines.clamp(1, 3),
        show_translation: settings.show_translation,
        show_footer: settings.show_footer,
        font_family: settings.font_family.clone(),
        font_size: settings.font_size,
        next_font_size: settings.next_font_size,
        prev_font_size: settings.prev_font_size,
        footer_font_size: settings.footer_font_size,
        translation_font_family: settings.translation_font_family.clone(),
        footer_font_family: settings.footer_font_family.clone(),
        bold: settings.bold,
        italic: settings.italic,
        translation_bold: settings.translation_bold,
        footer_bold: settings.footer_bold,
        align,
        current_color: settings.current_color,
        next_color: settings.next_color,
        prev_color: settings.prev_color,
        footer_color: settings.footer_color,
        translation_color: settings.translation_color,
        background,
        bg_color: settings.bg_color,
        bg_color2: settings.bg_color2,
        bg_image: settings
            .bg_image
            .as_ref()
            .map(|path| path.display().to_string()),
        opacity: settings.opacity,
        radius: settings.radius,
        padding: settings.padding,
        line_spacing: settings.line_spacing,
        shadow: settings.shadow,
        outline: settings.outline,
        translation_outline: settings.translation_outline,
        translation_shadow: settings.translation_shadow,
        footer_outline: settings.footer_outline,
        footer_shadow: settings.footer_shadow,
        border: settings.border,
        window_shadow: settings.window_shadow,
        outline_color: settings.outline_color,
        outline_width: settings.outline_width,
        shadow_color: settings.shadow_color,
        shadow_offset: settings.shadow_offset,
        translation_outline_width: settings.translation_outline_width,
        translation_outline_color: settings.translation_outline_color,
        translation_shadow_offset: settings.translation_shadow_offset,
        translation_shadow_color: settings.translation_shadow_color,
        footer_outline_width: settings.footer_outline_width,
        footer_outline_color: settings.footer_outline_color,
        footer_shadow_offset: settings.footer_shadow_offset,
        footer_shadow_color: settings.footer_shadow_color,
        blur_radius: settings.blur_radius,
        capture_protection: settings.capture_protection,
        karaoke: settings.karaoke,
        karaoke_words_only: settings.karaoke_words_only,
        width: settings.width,
        height: settings.height,
        auto_size: settings.auto_size,
        position: settings.position.or(app.core.session.desktop_lyrics_pos),
        frame_rate: settings.frame_rate.clamp(15, 120),
        d2d_hardware: app.core.settings.performance.d2d_hardware,
    }
}

fn overlay_labels() -> OverlayLabels {
    OverlayLabels {
        open_main: crate::i18n::tr("显示主窗口").to_string(),
        open_settings: crate::i18n::tr("打开设置").to_string(),
        lock: crate::i18n::tr("锁定").to_string(),
        click_through: crate::i18n::tr("鼠标穿透").to_string(),
        always_on_top: crate::i18n::tr("置顶").to_string(),
        lines: crate::i18n::tr("歌词行数").to_string(),
        line_1: "1 行".to_string(),
        line_2: "2 行".to_string(),
        line_3: "3 行".to_string(),
        align: crate::i18n::tr("对齐").to_string(),
        align_left: crate::i18n::tr("左对齐").to_string(),
        align_center: crate::i18n::tr("居中").to_string(),
        align_right: crate::i18n::tr("右对齐").to_string(),
        auto_size: crate::i18n::tr("自动宽度").to_string(),
        window_size: crate::i18n::tr("窗口大小").to_string(),
        size_small: crate::i18n::tr("小").to_string(),
        size_medium: crate::i18n::tr("中").to_string(),
        size_large: crate::i18n::tr("大").to_string(),
        font_increase: crate::i18n::tr("增大字号").to_string(),
        font_decrease: crate::i18n::tr("减小字号").to_string(),
        reset_position: crate::i18n::tr("重置位置").to_string(),
        hide: crate::i18n::tr("关闭桌面歌词").to_string(),
        loading: crate::i18n::tr("歌词加载中…").to_string(),
        no_lyrics: crate::i18n::tr("暂无歌词").to_string(),
        app_name: "Bamboo Music Player".to_string(),
    }
}

fn mini_labels() -> MiniLabels {
    MiniLabels {
        loading: crate::i18n::tr("歌词加载中…").to_string(),
        no_lyrics: crate::i18n::tr("暂无歌词").to_string(),
        not_playing: crate::i18n::tr("未在播放").to_string(),
        mini_player: crate::i18n::tr("迷你播放器").to_string(),
        play: crate::i18n::tr("播放").to_string(),
        pause: crate::i18n::tr("暂停").to_string(),
        prev: crate::i18n::tr("上一首").to_string(),
        next: crate::i18n::tr("下一首").to_string(),
        mute: crate::i18n::tr("静音").to_string(),
        unmute: crate::i18n::tr("取消静音").to_string(),
        close: crate::i18n::tr("关闭").to_string(),
    }
}

/// Blend `from` toward `to` (`t` in 0..=1).
fn blend_rgb(from: [u8; 3], to: [u8; 3], t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    let channel = |a: u8, b: u8| {
        (a as f32 + (b as f32 - a as f32) * t)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    [
        channel(from[0], to[0]),
        channel(from[1], to[1]),
        channel(from[2], to[2]),
    ]
}

fn rgba(rgb: [u8; 3], alpha: u8) -> [u8; 4] {
    [rgb[0], rgb[1], rgb[2], alpha]
}

/// Derive the whole mini-player palette from the three configurable colors.
/// Card, hover, divider and weak text shades stay consistent for any panel
/// color, so a custom theme does not need a dozen extra settings.
fn mini_colors(settings: &MiniPlayerSettings) -> MiniColors {
    let panel = settings.panel_color;
    let text = settings.text_color;
    MiniColors {
        // Fully opaque base panel: the opacity slider is the only
        // transparency control, so 100% really is 100%.
        panel: rgba(panel, 255),
        card: rgba(blend_rgb(panel, [255, 255, 255], 0.05), 255),
        card_hover: rgba(blend_rgb(panel, [255, 255, 255], 0.12), 255),
        line: rgba(blend_rgb(panel, [255, 255, 255], 0.07), 255),
        text: rgba(text, 255),
        text_weak: rgba(blend_rgb(text, panel, 0.63), 255),
        text_faint: rgba(blend_rgb(text, panel, 0.43), 255),
        accent: rgba(settings.accent_color, 255),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pause_transition_is_pushed_even_before_the_heartbeat() {
        // The main timer stops on pause, so waiting for STATE_INTERVAL would
        // never deliver the new icon.
        assert!(state_due(
            true,
            false,
            true,
            false,
            false,
            Duration::ZERO
        ));
        assert!(!state_due(
            true,
            false,
            false,
            false,
            false,
            Duration::from_millis(10)
        ));
    }

    #[test]
    fn custom_mini_colors_drive_the_derived_palette() {
        let mut settings = crate::config::MiniPlayerSettings::default();
        assert_eq!(mini_colors(&settings).accent, [127, 211, 168, 255]);

        settings.panel_color = [10, 20, 30];
        settings.text_color = [200, 210, 220];
        settings.accent_color = [255, 0, 0];
        let colors = mini_colors(&settings);
        assert_eq!(colors.panel, [10, 20, 30, 255]);
        assert_eq!(colors.text, [200, 210, 220, 255]);
        assert_eq!(colors.accent, [255, 0, 0, 255]);
        // Card/hover stay lighter than the panel; weak text sits between the
        // primary text and the panel background.
        assert!(colors.card[0] > colors.panel[0]);
        assert!(colors.card_hover[0] > colors.card[0]);
        assert!(colors.text_weak[0] < colors.text[0]);
        assert!(colors.text_weak[0] > colors.panel[0]);
    }

    #[test]
    fn force_state_and_revisions_bypass_the_interval() {
        let idle = Duration::from_millis(1);
        assert!(state_due(true, true, false, false, false, idle));
        assert!(state_due(true, false, false, true, false, idle));
        assert!(state_due(true, false, false, false, true, idle));
        assert!(state_due(false, false, false, false, false, idle));
        assert!(state_due(
            true,
            false,
            false,
            false,
            false,
            STATE_INTERVAL
        ));
    }
}
