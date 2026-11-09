//! Standalone fullscreen lyrics window.
//!
//! Unlike the immersive page (which reuses the main window), this viewport is
//! an independent OS window. It can be placed fullscreen on a separate display
//! while the main window stays usable elsewhere.
//!
//! The window is an *immediate* viewport: its UI is rendered inline during the
//! root app frame, so it is not affected by the Windows `WM_PAINT` starvation
//! between deferred viewports (egui issues #8466/#7686). The heartbeat thread
//! keeps the root pass running while it is open.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use egui::text::{LayoutJob, TextFormat};
use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Key, Layout, Modifiers, Rect, RichText, Sense,
    Stroke, StrokeKind, TextureHandle, Ui, ViewportClass, ViewportCommand, pos2, vec2,
};

use crate::app::theme::ThemeColors;
use crate::config::{LargeLyricsSettings, LyricsSettings};
use crate::lyrics::{LyricLine, Lyrics};

pub fn viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("bamboo-lyrics-window")
}

/// Everything needed to paint one frame (lyrics are passed separately).
#[derive(Clone)]
pub struct LyricsWindowSnapshot {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub lyrics_loading: bool,
    pub playing: bool,
    pub position_ms: u64,
    pub duration_ms: Option<u64>,
    pub offset_ms: i64,
    /// Normal resizable window instead of borderless fullscreen.
    pub windowed: bool,
    pub settings: LyricsSettings,
    pub large: LargeLyricsSettings,
    pub colors: ThemeColors,
    pub cover: Option<TextureHandle>,
    pub background: Option<TextureHandle>,
    /// Timestamp matching `position_ms` for smooth interpolation between app
    /// ticks.
    pub position_updated: Option<std::time::Instant>,
    pub frame_rate: u32,
    /// Shared lyric document so the deferred viewport can repaint even when
    /// the main window is hidden in the tray.
    pub lyrics: Option<Arc<Lyrics>>,
    pub lyrics_version: u64,
    /// Direct playback clock so the window keeps advancing even when the
    /// main window's per-frame logic is not running.
    pub clock: Option<crate::audio::PlaybackClock>,
    /// Last inner size reported by the window itself (windowed mode only).
    pub window_size: Option<(f32, f32)>,
    /// Last outer position reported by the window itself (windowed mode only).
    pub window_pos: Option<(f32, f32)>,
}

impl LyricsWindowSnapshot {
    fn live_position_ms(&self) -> u64 {
        if let Some(clock) = &self.clock
            && clock.state() == crate::audio::PlaybackState::Playing
        {
            return (clock.position_secs() * 1000.0).max(0.0) as u64;
        }
        let mut position = self.position_ms;
        if self.playing {
            let elapsed = self
                .position_updated
                .map(|at| at.elapsed().as_millis().min(2_000) as u64)
                .unwrap_or(0);
            position = position.saturating_add(elapsed);
        }
        position
    }

    fn live_playing(&self) -> bool {
        self.clock
            .as_ref()
            .map(|clock| clock.state() == crate::audio::PlaybackState::Playing)
            .unwrap_or(self.playing)
    }

    fn effective_ms(&self) -> u64 {
        let mut position = self.live_position_ms();
        if let Some(duration) = self.duration_ms {
            position = position.min(duration);
        }
        let position = position as i64;
        let user = self.offset_ms;
        let effective = if user >= 0 {
            position.saturating_sub(user)
        } else {
            position.saturating_add(-user)
        };
        effective.max(0) as u64
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LyricsWindowAction {
    PlayPause,
    Next,
    Prev,
    Seek(u64),
    ToggleFullscreen,
}

pub fn lyrics_window_ui(
    ui: &mut Ui,
    _class: ViewportClass,
    shared: &Arc<Mutex<LyricsWindowSnapshot>>,
    snap: &LyricsWindowSnapshot,
    lyrics: Option<&Lyrics>,
    actions: &mut Vec<LyricsWindowAction>,
    close: &mut bool,
) {
    let ctx = ui.ctx().clone();

    if ctx.input(|input| input.viewport().close_requested()) {
        ctx.send_viewport_cmd(ViewportCommand::CancelClose);
        *close = true;
    }
    // Windowed mode is user-resizable; remember the size/position so the next
    // open restores it. Fullscreen geometry is not interesting to persist.
    if snap.windowed {
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
    }
    // Borderless fullscreen is normally requested by the viewport builder, but
    // re-assert it if the backend created the window windowed. Never do this in
    // windowed mode: that would immediately undo the user's choice.
    if !snap.windowed && ctx.input(|input| input.viewport().fullscreen) != Some(true) {
        ctx.send_viewport_cmd(ViewportCommand::Fullscreen(true));
    }

    let rect = ui.max_rect();
    // Borderless windowed mode has no native resize frame: thin edge bands
    // turn an edge/corner drag into a window resize. Starting on the press
    // frame avoids adding egui's drag threshold to the cursor/edge gap.
    if snap.windowed {
        const EDGE: f32 = 4.0;
        let bands = [
            Rect::from_min_max(rect.left_top(), pos2(rect.right(), rect.top() + EDGE)),
            Rect::from_min_max(pos2(rect.left(), rect.bottom() - EDGE), rect.right_bottom()),
            Rect::from_min_max(rect.left_top(), pos2(rect.left() + EDGE, rect.bottom())),
            Rect::from_min_max(pos2(rect.right() - EDGE, rect.top()), rect.right_bottom()),
        ];
        let press_edge = ctx
            .input(|input| input.pointer.press_origin())
            .and_then(|origin| super::desktop_lyrics::edge_resize_direction(rect, origin));
        let pressed = ctx.input(|input| input.pointer.primary_pressed());
        for (index, band) in bands.iter().enumerate() {
            let response = ui.interact(
                *band,
                ui.id().with(("lyrics-edge", index)),
                Sense::click_and_drag(),
            );
            if response.hovered()
                && let Some(pointer) = ctx.input(|input| input.pointer.hover_pos())
                && let Some(direction) =
                    super::desktop_lyrics::edge_resize_direction(rect, pointer)
            {
                ctx.set_cursor_icon(super::desktop_lyrics::resize_cursor(direction));
            }
        }
        if pressed && let Some(direction) = press_edge {
            ctx.send_viewport_cmd(ViewportCommand::BeginResize(direction));
        }
    }
    let colors = &snap.colors;
    paint_background(ui, rect, snap, colors);
    handle_keyboard(&ctx, snap, actions, close);

    let padding_x = (rect.width() * 0.045).clamp(28.0, 96.0);
    let padding_y = (rect.height() * 0.055).clamp(20.0, 72.0);
    let content = rect.shrink2(vec2(padding_x, padding_y));
    if content.width() < 80.0 || content.height() < 80.0 {
        request_next_frame(&ctx, snap);
        return;
    }

    let header_h = 52.0;
    let footer_h = if snap.large.show_footer { 92.0 } else { 0.0 };
    let body = Rect::from_min_max(
        pos2(content.left(), content.top() + header_h),
        pos2(content.right(), content.bottom() - footer_h),
    );

    // Header: small brand label, drag surface (windowed mode), fullscreen
    // toggle and close button.
    ui.painter().text(
        pos2(content.left(), content.top() + 4.0),
        Align2::LEFT_TOP,
        "BAMBOO MUSIC",
        FontId::proportional(13.0),
        colors.text_faint,
    );
    if snap.windowed {
        let drag_rect = Rect::from_min_max(
            pos2(content.left(), content.top() - 8.0),
            pos2(content.right() - 92.0, content.top() + 38.0),
        );
        let drag = ui
            .interact(
                drag_rect,
                ui.id().with("lyrics-window-drag"),
                Sense::click_and_drag(),
            )
            .on_hover_cursor(egui::CursorIcon::Grab);
        if drag.drag_started_by(egui::PointerButton::Primary) {
            ctx.send_viewport_cmd(ViewportCommand::StartDrag);
        }
        if drag.double_clicked() {
            actions.push(LyricsWindowAction::ToggleFullscreen);
        }
    }
    let toggle_rect = Rect::from_min_size(
        pos2(content.right() - 78.0, content.top() - 2.0),
        vec2(36.0, 36.0),
    );
    let (toggle_glyph, toggle_label) = if snap.windowed {
        (
            egui_phosphor::regular::ARROWS_OUT,
            crate::i18n::tr("全屏 (F11)"),
        )
    } else {
        (
            egui_phosphor::regular::ARROWS_IN,
            crate::i18n::tr("窗口模式 (F11)"),
        )
    };
    let toggle_button = ui.put(
        toggle_rect,
        egui::Button::new(RichText::new(toggle_glyph).size(18.0))
            .frame(false)
            .fill(Color32::TRANSPARENT),
    );
    toggle_button
        .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, toggle_label));
    if toggle_button.clicked() {
        actions.push(LyricsWindowAction::ToggleFullscreen);
    }
    toggle_button.on_hover_text(toggle_label);

    let close_rect = Rect::from_min_size(
        pos2(content.right() - 36.0, content.top() - 2.0),
        vec2(36.0, 36.0),
    );
    let close_button = ui.put(
        close_rect,
        egui::Button::new(RichText::new(egui_phosphor::regular::X).size(20.0))
            .frame(false)
            .fill(Color32::TRANSPARENT),
    );
    close_button.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            true,
            crate::i18n::tr("关闭歌词窗口"),
        )
    });
    if close_button.clicked() {
        *close = true;
    }
    close_button.on_hover_text(crate::i18n::tr("关闭 (Esc)"));

    let effective = snap.effective_ms();
    let active = lyrics.and_then(|lyrics| lyrics.index_at(effective));

    let wide = snap.large.show_cover && body.width() >= 860.0;
    let cover_side = (body.height() * 0.52)
        .min(body.width() * 0.30)
        .clamp(150.0, 340.0);
    let (cover_rect, lyrics_rect) = if wide {
        let cover_rect = Rect::from_min_size(
            pos2(body.left(), body.center().y - cover_side * 0.56),
            vec2(cover_side, cover_side),
        );
        let lyrics_rect = Rect::from_min_max(
            pos2(
                cover_rect.right() + (content.width() * 0.055).clamp(28.0, 72.0),
                body.top(),
            ),
            body.right_bottom(),
        );
        (Some(cover_rect), lyrics_rect)
    } else {
        (None, body)
    };

    if let Some(cover_rect) = cover_rect {
        paint_cover(ui, cover_rect, snap, colors);
        if snap.large.show_title {
            paint_track_info(ui, cover_rect, snap, colors);
        }
    }

    paint_lyrics(
        ui,
        lyrics_rect,
        snap,
        lyrics,
        colors,
        active,
        effective,
        actions,
    );

    if snap.large.show_footer {
        paint_footer(ui, content, snap, colors, actions);
    }

    // Deferred viewports never run `App::logic`; the queued transport actions
    // are only applied there. Wake the root viewport explicitly, otherwise a
    // click would sit in the queue until the main window happens to repaint
    // (which only happens when it gains focus). `raw_input_hook` also drains
    // the queue on the next frame of any viewport.
    if *close || !actions.is_empty() {
        ctx.request_repaint_of(egui::ViewportId::ROOT);
    }

    request_next_frame(&ctx, snap);
}

fn paint_background(ui: &Ui, rect: Rect, snap: &LyricsWindowSnapshot, colors: &ThemeColors) {
    let painter = ui.painter();
    painter.rect_filled(rect, 0, colors.bg);
    if let Some(background) = &snap.background {
        painter.image(
            background.id(),
            rect,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::from_white_alpha(110),
        );
    }
    // Vertical darkening gradient: keeps lyrics readable over a blurred cover.
    const STRIPS: usize = 28;
    let strip_h = rect.height() / STRIPS as f32;
    for index in 0..STRIPS {
        let t = index as f32 / (STRIPS - 1) as f32;
        let alpha = (70.0 + 165.0 * t) as u8;
        let color =
            Color32::from_rgba_unmultiplied(colors.bg.r(), colors.bg.g(), colors.bg.b(), alpha);
        painter.rect_filled(
            Rect::from_min_size(
                pos2(rect.left(), rect.top() + strip_h * index as f32),
                vec2(rect.width(), strip_h + 1.0),
            ),
            0,
            color,
        );
    }
    // Soft accent glows in opposite corners.
    painter.circle_filled(
        pos2(
            rect.right() - rect.width() * 0.12,
            rect.top() + rect.height() * 0.10,
        ),
        rect.width() * 0.22,
        Color32::from_rgba_unmultiplied(
            colors.accent.r(),
            colors.accent.g(),
            colors.accent.b(),
            22,
        ),
    );
    painter.circle_filled(
        pos2(
            rect.left() + rect.width() * 0.08,
            rect.bottom() - rect.height() * 0.05,
        ),
        rect.width() * 0.18,
        Color32::from_rgba_unmultiplied(
            colors.accent.r(),
            colors.accent.g(),
            colors.accent.b(),
            16,
        ),
    );
}

fn paint_cover(ui: &Ui, rect: Rect, snap: &LyricsWindowSnapshot, colors: &ThemeColors) {
    let painter = ui.painter();
    if let Some(cover) = &snap.cover {
        painter.image(
            cover.id(),
            rect,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );
    } else {
        painter.rect_filled(rect, CornerRadius::same(10), colors.card);
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            egui_phosphor::regular::MUSIC_NOTES,
            FontId::proportional((rect.width() * 0.22).clamp(36.0, 90.0)),
            colors.text_faint,
        );
    }
    painter.rect_stroke(
        rect,
        CornerRadius::same(10),
        Stroke::new(1.0, Color32::from_white_alpha(36)),
        StrokeKind::Inside,
    );
}

fn paint_track_info(ui: &Ui, cover_rect: Rect, snap: &LyricsWindowSnapshot, colors: &ThemeColors) {
    let painter = ui.painter();
    let x = cover_rect.left();
    let mut y = cover_rect.bottom() + 22.0;
    let title = if snap.title.is_empty() {
        "Bamboo Music Player"
    } else {
        &snap.title
    };
    painter.text(
        pos2(x, y),
        Align2::LEFT_TOP,
        title,
        FontId::proportional(22.0),
        colors.text,
    );
    y += 32.0;
    if !snap.artist.is_empty() {
        painter.text(
            pos2(x, y),
            Align2::LEFT_TOP,
            &snap.artist,
            FontId::proportional(15.0),
            colors.text_weak,
        );
        y += 22.0;
    }
    if !snap.album.is_empty() {
        painter.text(
            pos2(x, y),
            Align2::LEFT_TOP,
            &snap.album,
            FontId::proportional(13.0),
            colors.text_faint,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_lyrics(
    ui: &mut Ui,
    rect: Rect,
    snap: &LyricsWindowSnapshot,
    lyrics: Option<&Lyrics>,
    colors: &ThemeColors,
    active: Option<usize>,
    effective_ms: u64,
    actions: &mut Vec<LyricsWindowAction>,
) {
    let settings = &snap.settings;
    let large = &snap.large;
    let align = match large.align {
        crate::config::LyricsAlign::Left => Align::LEFT,
        crate::config::LyricsAlign::Center => Align::Center,
        crate::config::LyricsAlign::Right => Align::RIGHT,
    };
    let base = large.font_size.clamp(16.0, 96.0);
    let font_family = crate::app::fonts::large_lyrics_font_family();
    let width = rect.width().max(80.0);
    let height = rect.height().max(80.0);
    let current_color = rgb(large.current_color);
    let normal_color = rgb(large.normal_color);
    let translation_color = rgb(large.translation_color);

    let Some(lyrics) = lyrics.filter(|lyrics| !lyrics.lines.is_empty()) else {
        let message = if snap.lyrics_loading {
            crate::i18n::tr("正在查找歌词…")
        } else if snap.title.is_empty() {
            "Bamboo Music Player"
        } else {
            crate::i18n::tr("暂无歌词")
        };
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            message,
            FontId::new(22.0, font_family.clone()),
            colors.text_weak,
        );
        return;
    };

    let scroll_animation = if settings.scroll_animation {
        egui::style::ScrollAnimation::default()
    } else {
        egui::style::ScrollAnimation::none()
    };
    let scroll = egui::ScrollArea::vertical()
        .id_salt("bamboo-lyrics-window")
        .auto_shrink([false, false])
        .max_height(height);
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::top_down(Align::LEFT)),
        |ui| {
            ui.set_min_size(vec2(width, height));
            scroll.show(ui, |ui| {
                let width = ui.available_width();
                ui.set_width(width);
                ui.add_space(height * 0.42);
                for (index, line) in lyrics.lines.iter().enumerate() {
                    let is_active = active == Some(index);
                    let row = ui.allocate_ui_with_layout(
                        vec2(width, 0.0),
                        Layout::top_down(align),
                        |ui| {
                            ui.set_width(width);
                            if is_active
                                && large.karaoke
                                && lyrics.timed
                                && (!large.karaoke_words_only || !line.words.is_empty())
                            {
                                let line_end = line_end_ms(line, index, lyrics);
                                let font = FontId::new(base * 1.12, font_family.clone());
                                let progress = crate::lyrics::karaoke_progress(
                                    line,
                                    effective_ms,
                                    line_end,
                                );
                                let mut job = LayoutJob::default();
                                job.wrap = egui::text::TextWrapping::wrap_at_width(width);
                                job.halign = align;
                                job.append(
                                    &line.text,
                                    0.0,
                                    TextFormat {
                                        font_id: font.clone(),
                                        color: normal_color,
                                        ..Default::default()
                                    },
                                );
                                let galley = ui.painter().layout_job(job);
                                let (rect, _) = ui.allocate_exact_size(
                                    vec2(width, galley.size().y),
                                    Sense::hover(),
                                );
                                let anchor = match align {
                                    Align::Min => rect.left(),
                                    Align::Max => rect.right(),
                                    _ => rect.center().x,
                                };
                                let origin = pos2(anchor, rect.top());
                                // Same continuous fill as the immersive page:
                                // the helper handles wrapped rows row by row.
                                ui.painter().galley(origin, galley.clone(), normal_color);
                                super::paint_karaoke_fill(
                                    ui.painter(),
                                    origin,
                                    &galley,
                                    &progress,
                                    current_color,
                                );
                            } else {
                                let size = if is_active { base * 1.12 } else { base };
                                let color = if is_active {
                                    current_color
                                } else {
                                    normal_color
                                };
                                let mut text = RichText::new(&line.text)
                                    .size(size)
                                    .font(FontId::new(size, font_family.clone()))
                                    .color(color);
                                if is_active {
                                    text = text.strong();
                                }
                                ui.add(egui::Label::new(text).wrap().selectable(false));
                            }
                            if large.show_translation
                                && let Some(translation) = &line.translation
                                && !translation.trim().is_empty()
                            {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(translation)
                                            .size((base * 0.62).max(13.0))
                                            .font(FontId::new(
                                                (base * 0.62).max(13.0),
                                                font_family.clone(),
                                            ))
                                            .color(translation_color),
                                    )
                                    .wrap()
                                    .selectable(false),
                                );
                            }
                        },
                    );
                    let row_rect = row.response.rect;
                    let response = ui
                        .interact(
                            row_rect,
                            ui.id().with(("lyrics-window-row", index)),
                            Sense::click(),
                        )
                        .on_hover_cursor(egui::CursorIcon::PointingHand);
                    let mut label = line.text.clone();
                    if large.show_translation
                        && let Some(translation) = &line.translation
                        && !translation.trim().is_empty()
                    {
                        label.push(' ');
                        label.push_str(translation);
                    }
                    response.widget_info(|| {
                        egui::WidgetInfo::selected(
                            egui::WidgetType::SelectableLabel,
                            true,
                            is_active,
                            &label,
                        )
                    });
                    if response.has_focus() {
                        ui.painter().rect_stroke(
                            row_rect.shrink(1.0),
                            CornerRadius::same(5),
                            Stroke::new(1.5, colors.accent),
                            StrokeKind::Inside,
                        );
                    }
                    if response.clicked() {
                        actions.push(LyricsWindowAction::Seek(line.start_ms));
                    }
                    if is_active {
                        ui.scroll_to_rect_animation(
                            row_rect,
                            Some(Align::Center),
                            scroll_animation,
                        );
                    }
                    ui.add_space(large.line_spacing.clamp(0.0, 80.0));
                }
                ui.add_space(height * 0.55);
            });
        },
    );
}

fn paint_footer(
    ui: &mut Ui,
    content: Rect,
    snap: &LyricsWindowSnapshot,
    colors: &ThemeColors,
    actions: &mut Vec<LyricsWindowAction>,
) {
    let progress_rect = Rect::from_min_max(
        pos2(content.left(), content.bottom() - 52.0),
        pos2(content.right(), content.bottom() - 46.0),
    );
    paint_progress(ui, progress_rect, snap, colors, actions);
    if let Some(duration) = snap.duration_ms {
        ui.painter().text(
            pos2(content.left(), progress_rect.top() - 20.0),
            Align2::LEFT_BOTTOM,
            format_time(snap.live_position_ms()),
            FontId::proportional(12.0),
            colors.text_faint,
        );
        ui.painter().text(
            pos2(content.right(), progress_rect.top() - 20.0),
            Align2::RIGHT_BOTTOM,
            format_time(duration),
            FontId::proportional(12.0),
            colors.text_faint,
        );
    }

    let center = pos2(content.center().x, content.bottom() - 18.0);
    let play_rect = Rect::from_center_size(center, vec2(52.0, 44.0));
    let prev_rect = Rect::from_center_size(pos2(center.x - 82.0, center.y), vec2(44.0, 44.0));
    let next_rect = Rect::from_center_size(pos2(center.x + 82.0, center.y), vec2(44.0, 44.0));

    if button(
        ui,
        prev_rect,
        crate::i18n::tr("上一首"),
        egui_phosphor::regular::SKIP_BACK,
        22.0,
    )
    .clicked()
    {
        actions.push(LyricsWindowAction::Prev);
    }
    let (glyph, label) = if snap.live_playing() {
        (egui_phosphor::regular::PAUSE, crate::i18n::tr("暂停"))
    } else {
        (egui_phosphor::regular::PLAY, crate::i18n::tr("播放"))
    };
    if button(ui, play_rect, label, glyph, 26.0).clicked() {
        actions.push(LyricsWindowAction::PlayPause);
    }
    if button(
        ui,
        next_rect,
        crate::i18n::tr("下一首"),
        egui_phosphor::regular::SKIP_FORWARD,
        22.0,
    )
    .clicked()
    {
        actions.push(LyricsWindowAction::Next);
    }
}

fn button(ui: &mut Ui, rect: Rect, label: &str, glyph: &str, size: f32) -> egui::Response {
    let response = ui.put(
        rect,
        egui::Button::new(RichText::new(glyph).size(size))
            .frame(false)
            .fill(Color32::TRANSPARENT),
    );
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
    response.on_hover_text(label)
}

fn paint_progress(
    ui: &mut Ui,
    rect: Rect,
    snap: &LyricsWindowSnapshot,
    colors: &ThemeColors,
    actions: &mut Vec<LyricsWindowAction>,
) {
    let hit = rect.expand2(vec2(0.0, 10.0));
    let response = ui.interact(
        hit,
        ui.id().with("lyrics-window-progress"),
        Sense::click_and_drag(),
    );
    response.widget_info(|| {
        egui::WidgetInfo::slider(
            snap.duration_ms.is_some(),
            snap.duration_ms
                .map(|duration| snap.live_position_ms() as f64 / duration.max(1) as f64)
                .unwrap_or(0.0),
            crate::i18n::tr("播放进度"),
        )
    });
    let fraction = snap
        .duration_ms
        .map(|duration| (snap.live_position_ms() as f32 / duration.max(1) as f32).clamp(0.0, 1.0))
        .unwrap_or(0.0);

    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(3), Color32::from_white_alpha(28));
    let filled = Rect::from_min_max(
        rect.left_top(),
        pos2(rect.left() + rect.width() * fraction, rect.bottom()),
    );
    painter.rect_filled(filled, CornerRadius::same(3), colors.accent);

    if let (Some(duration), Some(pointer)) = (
        snap.duration_ms,
        response
            .interact_pointer_pos()
            .filter(|_| response.clicked() || response.dragged()),
    ) {
        let fraction = ((pointer.x - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0);
        let target = (fraction as f64 * duration as f64) as u64;
        actions.push(LyricsWindowAction::Seek(target));
    }
}

fn line_end_ms(line: &LyricLine, index: usize, lyrics: &Lyrics) -> u64 {
    line.end_ms
        .or_else(|| lyrics.lines.get(index + 1).map(|next| next.start_ms))
        .unwrap_or(line.start_ms.saturating_add(5000))
}

fn handle_keyboard(
    ctx: &egui::Context,
    snap: &LyricsWindowSnapshot,
    actions: &mut Vec<LyricsWindowAction>,
    close: &mut bool,
) {
    if ctx.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Escape)) {
        *close = true;
    }
    if ctx.input_mut(|input| input.consume_key(Modifiers::NONE, Key::F11)) {
        actions.push(LyricsWindowAction::ToggleFullscreen);
    }
    if ctx.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Space)) {
        actions.push(LyricsWindowAction::PlayPause);
    }
    if ctx.input_mut(|input| input.consume_key(Modifiers::CTRL, Key::ArrowRight)) {
        actions.push(LyricsWindowAction::Next);
    }
    if ctx.input_mut(|input| input.consume_key(Modifiers::CTRL, Key::ArrowLeft)) {
        actions.push(LyricsWindowAction::Prev);
    }
    let seek_delta = if ctx.input_mut(|input| input.consume_key(Modifiers::NONE, Key::ArrowRight)) {
        Some(5_000i64)
    } else if ctx.input_mut(|input| input.consume_key(Modifiers::NONE, Key::ArrowLeft)) {
        Some(-5_000)
    } else {
        None
    };
    if let Some(delta) = seek_delta
        && let Some(duration) = snap.duration_ms
    {
        let target = (snap.live_position_ms() as i64 + delta).clamp(0, duration as i64) as u64;
        actions.push(LyricsWindowAction::Seek(target));
    }
}

fn request_next_frame(ctx: &egui::Context, snap: &LyricsWindowSnapshot) {
    let fps = if snap.frame_rate == 0 {
        60
    } else {
        snap.frame_rate.clamp(15, 120)
    };
    let frame_ms = (1000 / fps).max(8) as u64;
    let delay = if snap.live_playing() { frame_ms } else { 500 };
    ctx.request_repaint_after(Duration::from_millis(delay));
}

fn rgb(values: [u8; 3]) -> Color32 {
    Color32::from_rgb(values[0], values[1], values[2])
}

fn format_time(ms: u64) -> String {
    let seconds = ms / 1000;
    format!("{:02}:{:02}", seconds / 60, seconds % 60)
}
