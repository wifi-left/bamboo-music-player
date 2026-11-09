//! Compact always-on-top mini player.
//!
//! Like the desktop lyrics window this is an *immediate* egui viewport painted
//! inline with the root pass (deferred viewports starve each other on Windows,
//! see egui issues #8466/#7686), sharing state through a snapshot and queueing
//! button clicks for the main app.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use egui::text::{LayoutJob, TextFormat};
use egui::{
    Align2, Color32, CornerRadius, FontId, RichText, Sense, Stroke, Ui, ViewportClass,
    ViewportCommand, WindowLevel, vec2,
};

use crate::app::theme::ThemeColors;
use crate::config::MiniPlayerSettings;
use crate::lyrics::Lyrics;

pub fn viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("bamboo-mini-player")
}

pub fn window_level(always_on_top: bool) -> WindowLevel {
    if always_on_top {
        WindowLevel::AlwaysOnTop
    } else {
        WindowLevel::Normal
    }
}

/// Height needed for the current mini-player layout. The window grows by the
/// lyric row(s) so the track info, progress bar and buttons keep their
/// spacing instead of being squeezed.
pub fn window_height(settings: &MiniPlayerSettings) -> f32 {
    if settings.auto_height {
        auto_window_height(settings)
    } else {
        settings.height.clamp(120.0, 600.0)
    }
}

/// Height derived from the enabled lyric options, ignoring a manual override.
pub fn auto_window_height(settings: &MiniPlayerSettings) -> f32 {
    let mut height = 132.0;
    if settings.show_lyrics {
        let size = lyric_render_size(settings);
        let lines = settings.lyrics_lines.clamp(1, 2) as f32;
        let line_height = size * 1.35;
        let translation_height = if settings.show_translation {
            lyric_translation_size(size) * 1.35 + 2.0
        } else {
            0.0
        };
        height += lines * (line_height + translation_height) + (lines - 1.0) * 4.0 + 6.0;
    }
    height
}

fn lyric_render_size(settings: &MiniPlayerSettings) -> f32 {
    let size = settings.lyrics_font_size.clamp(10.0, 22.0);
    if settings.lyrics_bold { size * 1.03 } else { size }
}

/// Translation text is deliberately smaller than the lyric line.
pub fn lyric_translation_size(font: f32) -> f32 {
    (font * 0.66).max(9.0)
}

/// Derived window width (kept in one place so the viewport and the renderer
/// agree on the layout bounds).
pub fn window_width(settings: &MiniPlayerSettings) -> f32 {
    settings.width.clamp(280.0, 900.0)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MiniPlayerAction {
    PlayPause,
    Prev,
    Next,
    ShowMain,
    Hide,
    ToggleMute,
    SetVolume(f32),
    Seek(f64),
}

#[derive(Clone)]
pub struct MiniPlayerSnapshot {
    pub title: String,
    pub artist: String,
    pub playing: bool,
    pub position: f32,
    pub duration: f32,
    pub volume: f32,
    pub muted: bool,
    pub cover: Option<egui::TextureHandle>,
    pub colors: ThemeColors,
    pub opacity: f32,
    pub window_pos: Option<(f32, f32)>,
    pub frame_rate: u32,
    /// Direct playback clock so the progress line keeps moving even when the
    /// main window's logic is not ticking.
    pub clock: Option<crate::audio::PlaybackClock>,
    pub show_lyrics: bool,
    pub karaoke: bool,
    pub karaoke_words_only: bool,
    pub lyrics_lines: u8,
    pub lyrics_bold: bool,
    pub show_translation: bool,
    pub lyrics_font_size: f32,
    pub lyrics: Option<Arc<Lyrics>>,
    pub lyrics_version: u64,
    pub lyrics_loading: bool,
    pub position_ms: u64,
    pub position_updated: Option<Instant>,
    pub offset_ms: i64,
    /// Last inner size requested from the main app; used to resize the window
    /// only when the lyric/layout options actually change.
    pub requested_size: Option<(f32, f32)>,
    /// Last inner size reported by the window itself (points).
    pub window_size: Option<(f32, f32)>,
}

impl MiniPlayerSnapshot {
    fn live_playing(&self) -> bool {
        self.clock
            .as_ref()
            .map(|clock| clock.state() == crate::audio::PlaybackState::Playing)
            .unwrap_or(self.playing)
    }

    fn live_position(&self) -> f32 {
        if let Some(clock) = &self.clock
            && clock.state() == crate::audio::PlaybackState::Playing
        {
            return clock.position_secs().max(0.0) as f32;
        }
        self.position
    }

    /// Millisecond position with interpolation since the last app snapshot,
    /// so the karaoke fill advances between the main window's slower ticks.
    fn live_position_ms(&self) -> u64 {
        if let Some(clock) = &self.clock
            && clock.state() == crate::audio::PlaybackState::Playing
        {
            return (clock.position_secs() * 1000.0).max(0.0) as u64;
        }
        let base = self.position_ms;
        if !self.live_playing() {
            return base;
        }
        let elapsed = self
            .position_updated
            .map(|at| at.elapsed().as_millis().min(2_000) as u64)
            .unwrap_or(0);
        base.saturating_add(elapsed)
    }

    /// Offset lyric position (a positive user offset shows lyrics earlier).
    fn offset_position_ms(&self) -> u64 {
        let position = self.live_position_ms() as i64;
        let user = self.offset_ms;
        let effective = if user >= 0 {
            position.saturating_sub(user)
        } else {
            position.saturating_add(-user)
        };
        effective.max(0) as u64
    }
}

pub fn mini_player_ui(
    ui: &mut Ui,
    _class: ViewportClass,
    shared: &Arc<Mutex<MiniPlayerSnapshot>>,
    actions: &Arc<Mutex<Vec<MiniPlayerAction>>>,
    close_flag: &Arc<AtomicBool>,
) {
    let ctx = ui.ctx().clone();
    let snap = match shared.lock() {
        Ok(state) => state.clone(),
        Err(_) => return,
    };
    let colors = &snap.colors;
    let playing = snap.live_playing();
    let position = snap.live_position();

    if ctx.input(|input| input.viewport().close_requested()) {
        ctx.send_viewport_cmd(ViewportCommand::CancelClose);
        close_flag.store(true, Ordering::Relaxed);
    }
    if let Some(rect) = ctx.input(|input| input.viewport().outer_rect)
        && rect.min.x.is_finite()
        && rect.min.y.is_finite()
        && let Ok(mut state) = shared.lock()
    {
        state.window_pos = Some((rect.min.x, rect.min.y));
    }
    if let Some(inner) = ctx.input(|input| input.viewport().inner_rect)
        && inner.width().is_finite()
        && inner.height().is_finite()
        && let Ok(mut state) = shared.lock()
    {
        state.window_size = Some((inner.width(), inner.height()));
    }

    let rect = ui.max_rect();
    let opacity = snap.opacity.clamp(0.3, 1.0);
    let alpha = (opacity * 255.0) as u8;
    let panel = Color32::from_rgba_unmultiplied(
        colors.panel.r(),
        colors.panel.g(),
        colors.panel.b(),
        alpha,
    );
    ui.painter()
        .rect_filled(rect, CornerRadius::same(12), panel);
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(12),
        Stroke::new(1.0, colors.line),
        egui::StrokeKind::Inside,
    );

    // Dragging anywhere that is not a button; widgets added later sit on top.
    let drag = ui.interact(rect, ui.id().with("mini-drag"), Sense::click_and_drag());
    let drag_label = if snap.title.is_empty() {
        crate::i18n::tr("迷你播放器").to_string()
    } else if snap.artist.is_empty() {
        crate::trf!("迷你播放器：{}", snap.title)
    } else {
        crate::trf!("迷你播放器：{} - {}", snap.title, snap.artist)
    };
    drag.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, &drag_label));

    let padding = 14.0;
    let cover_side = (rect.height() - padding * 2.0).clamp(56.0, 96.0);
    let cover_rect = egui::Rect::from_min_size(
        egui::pos2(rect.left() + padding, rect.center().y - cover_side * 0.5),
        vec2(cover_side, cover_side),
    );
    if let Some(cover) = snap.cover.as_ref() {
        ui.painter().image(
            cover.id(),
            cover_rect,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            Color32::WHITE,
        );
    } else {
        ui.painter()
            .rect_filled(cover_rect, CornerRadius::same(8), colors.card);
        ui.painter().text(
            cover_rect.center(),
            Align2::CENTER_CENTER,
            egui_phosphor::regular::MUSIC_NOTES,
            FontId::proportional(24.0),
            colors.text_faint,
        );
    }

    let close_rect = egui::Rect::from_center_size(
        egui::pos2(rect.right() - 16.0, rect.top() + 16.0),
        vec2(22.0, 22.0),
    );
    let close_response = ui.interact(close_rect, ui.id().with("mini-close"), Sense::click());
    close_response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            true,
            crate::i18n::tr("关闭迷你播放器"),
        )
    });
    if close_response.has_focus() {
        ui.painter()
            .circle_stroke(close_rect.center(), 10.0, Stroke::new(1.5, colors.text));
    }
    let close_hover = close_response.hovered();
    if close_hover {
        ui.painter()
            .circle_filled(close_rect.center(), 10.0, colors.card_hover);
    }
    ui.painter().text(
        close_rect.center(),
        Align2::CENTER_CENTER,
        egui_phosphor::regular::X,
        FontId::proportional(13.0),
        if close_hover {
            colors.text
        } else {
            colors.text_faint
        },
    );
    if close_response.clicked() {
        actions.lock().unwrap().push(MiniPlayerAction::Hide);
    }

    // Volume: the speaker icon toggles mute, the wheel over the window adjusts
    // the level. A thin bar under the icon shows the current level.
    let mute_rect = egui::Rect::from_center_size(
        egui::pos2(rect.right() - 42.0, rect.top() + 16.0),
        vec2(22.0, 22.0),
    );
    let mute_response = ui
        .interact(mute_rect, ui.id().with("mini-mute"), Sense::click())
        .on_hover_text(crate::i18n::tr("静音/取消静音（滚轮调节音量）"));
    mute_response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            true,
            if snap.muted {
                crate::i18n::tr("取消静音")
            } else {
                crate::i18n::tr("静音")
            },
        )
    });
    if mute_response.hovered() {
        ui.painter()
            .circle_filled(mute_rect.center(), 10.0, colors.card_hover);
    }
    if mute_response.has_focus() {
        ui.painter()
            .circle_stroke(mute_rect.center(), 10.0, Stroke::new(1.5, colors.text));
    }
    ui.painter().text(
        mute_rect.center(),
        Align2::CENTER_CENTER,
        if snap.muted {
            egui_phosphor::regular::SPEAKER_SLASH
        } else {
            egui_phosphor::regular::SPEAKER_HIGH
        },
        FontId::proportional(13.0),
        if mute_response.hovered() {
            colors.text
        } else {
            colors.text_faint
        },
    );
    if mute_response.clicked() {
        actions.lock().unwrap().push(MiniPlayerAction::ToggleMute);
    }
    let level_rect = egui::Rect::from_min_max(
        egui::pos2(mute_rect.left() + 3.0, mute_rect.bottom() + 1.0),
        egui::pos2(mute_rect.right() - 3.0, mute_rect.bottom() + 3.0),
    );
    ui.painter()
        .rect_filled(level_rect, CornerRadius::same(1), colors.card_hover);
    let level = if snap.muted {
        0.0
    } else {
        snap.volume.clamp(0.0, 1.0)
    };
    if level > 0.0 {
        ui.painter().rect_filled(
            egui::Rect::from_min_max(
                level_rect.min,
                egui::pos2(
                    level_rect.left() + level_rect.width() * level,
                    level_rect.bottom(),
                ),
            ),
            CornerRadius::same(1),
            colors.accent,
        );
    }

    let pointer_over = ctx
        .pointer_hover_pos()
        .is_some_and(|position| rect.contains(position));
    if pointer_over {
        let scroll = ctx.input(|input| input.smooth_scroll_delta.y);
        if scroll.abs() > 0.1 {
            let next = (snap.volume + scroll * 0.0015).clamp(0.0, 1.0);
            if (next - snap.volume).abs() > 1e-4 {
                actions
                    .lock()
                    .unwrap()
                    .push(MiniPlayerAction::SetVolume(next));
                ctx.request_repaint();
            }
        }
    }

    let buttons_size = 30.0;
    let next_rect = egui::Rect::from_center_size(
        egui::pos2(
            rect.right() - padding - buttons_size * 0.5,
            rect.bottom() - 40.0,
        ),
        vec2(buttons_size, buttons_size),
    );
    let play_rect = next_rect.translate(vec2(-(buttons_size + 6.0), 0.0));
    let prev_rect = play_rect.translate(vec2(-(buttons_size + 6.0), 0.0));
    if button(
        ui,
        prev_rect,
        egui_phosphor::regular::SKIP_BACK,
        false,
        crate::i18n::tr("上一首"),
        colors,
    ) {
        actions.lock().unwrap().push(MiniPlayerAction::Prev);
    }
    if button(
        ui,
        play_rect,
        if playing {
            egui_phosphor::regular::PAUSE
        } else {
            egui_phosphor::regular::PLAY
        },
        true,
        if snap.playing {
            crate::i18n::tr("暂停")
        } else {
            crate::i18n::tr("播放")
        },
        colors,
    ) {
        actions.lock().unwrap().push(MiniPlayerAction::PlayPause);
    }
    if button(
        ui,
        next_rect,
        egui_phosphor::regular::SKIP_FORWARD,
        false,
        crate::i18n::tr("下一首"),
        colors,
    ) {
        actions.lock().unwrap().push(MiniPlayerAction::Next);
    }

    let text_left = cover_rect.right() + 12.0;
    // Keep the track info clear of the close/mute buttons in the top-right
    // corner; the transport buttons sit lower, so the lyric row below can use
    // the full width. Clamp to the window so a very narrow window never lays
    // out content past its right edge.
    let text_right = (rect.right() - padding - 44.0)
        .min(rect.right() - padding)
        .max(text_left);
    let text_width = (text_right - text_left).max(0.0);
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(egui::Rect::from_min_size(
                egui::pos2(text_left, rect.top() + padding + 4.0),
                vec2(text_width, 42.0),
            ))
            .layout(egui::Layout::top_down(egui::Align::LEFT)),
        |ui| {
            ui.set_width(text_width);
            let title = if snap.title.trim().is_empty() {
                crate::i18n::tr("未在播放")
            } else {
                snap.title.as_str()
            };
            ui.add(
                egui::Label::new(RichText::new(title).size(13.5).strong().color(colors.text))
                    .truncate(),
            );
            ui.add(
                egui::Label::new(
                    RichText::new(&snap.artist)
                        .size(10.5)
                        .color(colors.text_weak),
                )
                .truncate(),
            );
        },
    );

    let progress_rect = egui::Rect::from_min_max(
        egui::pos2(text_left, rect.bottom() - padding - 12.0),
        egui::pos2(
            (prev_rect.left() - 10.0).max(text_left + 10.0),
            rect.bottom() - padding - 8.0,
        ),
    );
    let hit_rect = egui::Rect::from_min_max(
        egui::pos2(progress_rect.left(), progress_rect.top() - 6.0),
        egui::pos2(progress_rect.right(), progress_rect.bottom() + 6.0),
    );
    let progress_response = ui
        .interact(
            hit_rect,
            ui.id().with("mini-progress"),
            Sense::click_and_drag(),
        )
        .on_hover_text(crate::i18n::tr("拖动跳转"));
    progress_response.widget_info(|| {
        egui::WidgetInfo::slider(
            snap.duration > 0.0,
            position as f64,
            crate::i18n::tr("播放进度"),
        )
    });
    if progress_response.has_focus() {
        ui.painter().rect_stroke(
            hit_rect.shrink(1.0),
            CornerRadius::same(4),
            Stroke::new(1.0, colors.accent),
            egui::StrokeKind::Inside,
        );
    }
    let mut ratio = if snap.duration > 0.0 {
        (position / snap.duration).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let mut seek = None;
    if snap.duration > 0.0 {
        if progress_response.dragged() || progress_response.clicked() {
            if let Some(pos) = progress_response.interact_pointer_pos() {
                let value = ((pos.x - progress_rect.left()) / progress_rect.width().max(1.0))
                    .clamp(0.0, 1.0);
                ratio = value;
                seek = Some(value as f64 * snap.duration as f64);
            }
        }
        if progress_response.has_focus() {
            let step = 5.0f64;
            let page = (snap.duration as f64 * 0.1).clamp(5.0, 120.0);
            let (delta, absolute) = ui.input_mut(|input| {
                let mut delta = 0.0;
                if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft)
                    || input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown)
                {
                    delta -= step;
                }
                if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight)
                    || input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp)
                {
                    delta += step;
                }
                if input.consume_key(egui::Modifiers::NONE, egui::Key::PageDown) {
                    delta -= page;
                }
                if input.consume_key(egui::Modifiers::NONE, egui::Key::PageUp) {
                    delta += page;
                }
                let absolute = if input.consume_key(egui::Modifiers::NONE, egui::Key::Home) {
                    Some(0.0)
                } else if input.consume_key(egui::Modifiers::NONE, egui::Key::End) {
                    Some(snap.duration as f64)
                } else {
                    None
                };
                (delta, absolute)
            });
            if let Some(value) = absolute {
                seek = Some(value);
            } else if delta != 0.0 {
                seek = Some((position as f64 + delta).clamp(0.0, snap.duration as f64));
            }
        }
    }
    if let Some(value) = seek {
        actions.lock().unwrap().push(MiniPlayerAction::Seek(value));
        ctx.request_repaint();
    }
    ui.painter()
        .rect_filled(progress_rect, CornerRadius::same(2), colors.card_hover);
    if ratio > 0.0 {
        let filled = egui::Rect::from_min_max(
            progress_rect.min,
            egui::pos2(
                progress_rect.left() + progress_rect.width() * ratio,
                progress_rect.bottom(),
            ),
        );
        ui.painter()
            .rect_filled(filled, CornerRadius::same(2), colors.accent);
    }
    if progress_response.hovered() || progress_response.dragged() {
        let x = progress_rect.left() + progress_rect.width() * ratio;
        ui.painter()
            .circle_filled(egui::pos2(x, progress_rect.center().y), 3.5, colors.text);
    }
    ui.painter().text(
        egui::pos2(progress_rect.left(), progress_rect.top() - 7.0),
        Align2::LEFT_CENTER,
        format_time(position),
        FontId::proportional(9.0),
        colors.text_faint,
    );
    ui.painter().text(
        egui::pos2(progress_rect.right(), progress_rect.top() - 7.0),
        Align2::RIGHT_CENTER,
        format_time(snap.duration),
        FontId::proportional(9.0),
        colors.text_faint,
    );

    if snap.show_lyrics {
        let lyric_top = rect.top() + padding + 4.0 + 44.0;
        // Keep the lyric block above both the time labels and the transport
        // buttons. The buttons are centred 40 px above the bottom with a 30 px
        // circle, so their top edge is bottom-55; the old bottom-34 bound let
        // the translation line draw over them.
        let controls_top = prev_rect.top().min(progress_rect.top() - 18.0);
        let lyric_bottom = (controls_top - 6.0).max(lyric_top + 1.0);
        let lyric_area = egui::Rect::from_min_max(
            egui::pos2(text_left, lyric_top),
            egui::pos2((rect.right() - padding).max(text_left + 10.0), lyric_bottom),
        );
        paint_lyrics(ui, &snap, colors, lyric_area);
    }

    let over_controls = ctx.pointer_interact_pos().is_some_and(|pointer| {
        [
            close_rect, mute_rect, prev_rect, play_rect, next_rect, hit_rect,
        ]
        .iter()
        .any(|control| control.contains(pointer))
    });
    // Borderless window: a drag near an edge resizes, otherwise it moves the
    // window. Starting the resize on the press frame (instead of waiting for
    // `drag_started`) keeps egui's drag threshold out of the cursor/edge gap.
    if let Some(direction) = ctx
        .input(|input| input.pointer.hover_pos())
        .and_then(|pointer| super::desktop_lyrics::edge_resize_direction(rect, pointer))
        && !over_controls
    {
        ctx.set_cursor_icon(super::desktop_lyrics::resize_cursor(direction));
    }
    let press_edge = ctx
        .input(|input| input.pointer.press_origin())
        .and_then(|origin| super::desktop_lyrics::edge_resize_direction(rect, origin));
    if ctx.input(|input| input.pointer.primary_pressed())
        && !over_controls
        && let Some(direction) = press_edge
    {
        ctx.send_viewport_cmd(ViewportCommand::BeginResize(direction));
    }
    if drag.drag_started() && !over_controls && press_edge.is_none() {
        ctx.send_viewport_cmd(ViewportCommand::StartDrag);
    }
    if drag.double_clicked() && !over_controls && press_edge.is_none() {
        actions.lock().unwrap().push(MiniPlayerAction::ShowMain);
    }

    // Deferred viewports never run `App::logic`; queued button actions are
    // applied there, so wake the root viewport instead of waiting for the main
    // window to be focused.
    let pending_root = close_flag.load(Ordering::Relaxed)
        || actions
            .lock()
            .map(|queue| !queue.is_empty())
            .unwrap_or(false);
    if pending_root {
        ctx.request_repaint_of(egui::ViewportId::ROOT);
    }

    // Keep repainting while playing so the progress line follows the engine;
    // when paused, a slow tick still picks up snapshot changes (track, time).
    let delay = if playing {
        let fps = if snap.frame_rate == 0 {
            60
        } else {
            snap.frame_rate.clamp(15, 120)
        };
        (1000 / fps).max(8) as u64
    } else {
        500
    };
    // Compensate for egui subtracting the predicted frame time from the
    // requested delay; otherwise a 30 fps cap still runs at 60 fps.
    let lead = std::time::Duration::from_secs_f32(
        ctx.input(|input| input.predicted_dt).clamp(0.0, 0.1),
    );
    ctx.request_repaint_after(std::time::Duration::from_millis(delay) + lead);
}

/// Paint the current lyric line(s) inside `area`.
///
/// Each displayed line is truncated to a single row, so the window height
/// stays predictable. The active line gets a smooth fill: with word timings
/// the edge follows the sung syllable, and a plain timed LRC line advances
/// proportionally through the line. `karaoke_words_only` restricts the fill to
/// enhanced LRC (the desktop lyrics window has the same option).
fn paint_lyrics(ui: &Ui, snap: &MiniPlayerSnapshot, colors: &ThemeColors, area: egui::Rect) {
    if area.width() <= 8.0 || area.height() <= 8.0 {
        return;
    }
    // Manual window heights can be smaller than the lyric content; without
    // this clip a translation row would paint through the controls below.
    let painter = ui.painter().with_clip_rect(area);
    let settings_size = snap.lyrics_font_size.clamp(10.0, 22.0);
    let size = if snap.lyrics_bold {
        settings_size * 1.03
    } else {
        settings_size
    };
    let font = FontId::proportional(size);
    let position = snap.offset_position_ms();
    let line_count = snap.lyrics_lines.clamp(1, 2) as usize;

    let Some(lyrics) = snap.lyrics.as_deref().filter(|lyrics| !lyrics.lines.is_empty()) else {
        let status = if snap.lyrics_loading {
            Some(crate::i18n::tr("正在查找歌词…"))
        } else if snap.title.is_empty() {
            None
        } else {
            Some(crate::i18n::tr("暂无歌词"))
        };
        if let Some(status) = status {
            let galley = painter.layout_no_wrap(status.to_string(), font, colors.text_faint);
            let pos = egui::pos2(
                area.left() + ((area.width() - galley.size().x) * 0.5).max(0.0),
                area.top() + ((area.height() - galley.size().y) * 0.5).max(0.0),
            );
            painter.galley(pos, galley, colors.text_faint);
        }
        return;
    };

    let active = lyrics.index_at(position);
    let start = active.unwrap_or(0).min(lyrics.lines.len() - 1);
    let end = (start + line_count).min(lyrics.lines.len());

    struct Row {
        main: Arc<egui::Galley>,
        fill: Option<(Arc<egui::Galley>, f32)>,
        translation: Option<Arc<egui::Galley>>,
    }

    let mut rows = Vec::with_capacity(end - start);
    for index in start..end {
        let line = &lyrics.lines[index];
        let current = active == Some(index);
        let line_end = line.end_ms.unwrap_or_else(|| {
            lyrics
                .lines
                .get(index + 1)
                .map(|next| next.start_ms)
                .unwrap_or(line.start_ms + 5000)
        });
        let can_fill = current
            && lyrics.timed
            && snap.karaoke
            && (!snap.karaoke_words_only || !line.words.is_empty());
        let pending = if current {
            colors.text_weak
        } else {
            colors.text_faint
        };

        let mut job = LayoutJob::default();
        wrap_lyric_job(&mut job, area.width());
        if can_fill {
            for (text, sung) in crate::lyrics::karaoke_runs(line, position, line_end) {
                job.append(
                    &text,
                    0.0,
                    TextFormat {
                        font_id: font.clone(),
                        color: if sung { colors.accent } else { pending },
                        ..Default::default()
                    },
                );
            }
        } else {
            job.append(
                &line.text,
                0.0,
                TextFormat {
                    font_id: font.clone(),
                    color: if current && !snap.karaoke {
                        colors.text
                    } else {
                        pending
                    },
                    ..Default::default()
                },
            );
        }
        let main = painter.layout_job(job);

        // Same text, same font, same wrap: the overlay only differs in colour,
        // so its glyphs line up with the base row.
        let fill = can_fill.then(|| {
            let mut job = LayoutJob::default();
            wrap_lyric_job(&mut job, area.width());
            job.append(
                &line.text,
                0.0,
                TextFormat {
                    font_id: font.clone(),
                    color: colors.accent,
                    ..Default::default()
                },
            );
            let galley = painter.layout_job(job);
            let progress = crate::lyrics::karaoke_progress(line, position, line_end);
            let prefix = painter
                .layout_no_wrap(progress.sung, font.clone(), colors.accent)
                .size()
                .x;
            let next = progress
                .next
                .map(|ch| {
                    painter
                        .layout_no_wrap(ch.to_string(), font.clone(), colors.accent)
                        .size()
                        .x
                })
                .unwrap_or(0.0);
            (galley, prefix + next * progress.next_fraction)
        });

        let translation = snap
            .show_translation
            .then_some(line.translation.as_deref())
            .flatten()
            .filter(|text| !text.trim().is_empty())
            .map(|text| {
                let mut job = LayoutJob::default();
                wrap_lyric_job(&mut job, area.width());
                job.append(
                    text,
                    0.0,
                    TextFormat {
                        font_id: FontId::proportional(lyric_translation_size(size)),
                        color: if current {
                            colors.text_weak
                        } else {
                            colors.text_faint
                        },
                        ..Default::default()
                    },
                );
                painter.layout_job(job)
            });

        rows.push(Row {
            main,
            fill,
            translation,
        });
    }

    let row_gap = 4.0;
    let mut total = 0.0;
    for (index, row) in rows.iter().enumerate() {
        if index > 0 {
            total += row_gap;
        }
        total += row.main.size().y;
        if let Some(translation) = &row.translation {
            total += 1.0 + translation.size().y;
        }
    }

    let mut y = area.top() + ((area.height() - total) * 0.5).max(0.0);
    for row in &rows {
        let pos = egui::pos2(area.left(), y);
        painter.galley(pos, row.main.clone(), colors.text);
        if let Some((fill_galley, fill_width)) = &row.fill {
            let clip = egui::Rect::from_min_size(
                pos,
                vec2(
                    fill_width.clamp(0.0, area.width()),
                    row.main.size().y,
                ),
            );
            painter
                .with_clip_rect(clip)
                .galley(pos, fill_galley.clone(), colors.accent);
        }
        y += row.main.size().y;
        if let Some(translation) = &row.translation {
            y += 1.0;
            painter.galley(
                egui::pos2(area.left(), y),
                translation.clone(),
                colors.text_faint,
            );
            y += translation.size().y;
        }
        y += row_gap;
    }
}

/// Single-row lyric layout with ellipsis. The fill overlay must use identical
/// wrapping settings or its glyphs would not line up with the base row.
fn wrap_lyric_job(job: &mut LayoutJob, width: f32) {
    job.wrap.max_width = width;
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job.wrap.overflow_character = Some('…');
}

fn button(
    ui: &mut Ui,
    rect: egui::Rect,
    icon: &str,
    primary: bool,
    label: &str,
    colors: &ThemeColors,
) -> bool {
    let response = ui.interact(
        rect,
        ui.id().with(("mini-button", rect.left().to_bits())),
        Sense::click(),
    );
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
    let hovered = response.hovered();
    if response.has_focus() {
        ui.painter().circle_stroke(
            rect.center(),
            rect.width() * 0.5,
            Stroke::new(1.5, colors.text),
        );
    }
    let background = if primary {
        colors.accent
    } else if hovered {
        colors.card_hover
    } else {
        Color32::TRANSPARENT
    };
    if background != Color32::TRANSPARENT {
        ui.painter()
            .circle_filled(rect.center(), rect.width() * 0.5, background);
    }
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        icon,
        FontId::proportional(15.0),
        if primary {
            Color32::from_rgb(12, 16, 24)
        } else if hovered {
            colors.text
        } else {
            colors.text_weak
        },
    );
    response.clicked()
}

fn format_time(seconds: f32) -> String {
    let seconds = seconds.max(0.0) as u64;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}
