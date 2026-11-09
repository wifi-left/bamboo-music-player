//! Main content pages.

pub mod desktop_lyrics;
mod library;
mod lyrics;
mod lyrics_editor;
pub mod lyrics_window;
pub mod mini_player;
mod playlists;
mod search;
mod settings;

pub use library::{LibraryTab, library, smart_playlists};
pub use lyrics::{LyricsAreaKind, fullscreen_lyrics, lyrics_area};
pub use lyrics_editor::lyrics_editor;
pub use playlists::playlists;
pub use search::{discover, search};
pub use settings::{SettingsTab, settings};

use egui::{Align2, Color32, CornerRadius, RichText, Ui, vec2};

use crate::app::theme::ThemeColors;
use crate::app::{App, widgets};
use crate::model::Track;
use crate::util::format_time;

/// Paint the sung part of a karaoke line over a galley laid out in the
/// inactive colour.
///
/// Single-row lines are clipped with one rectangle. Wrapped lines are handled
/// per row: each row is clipped at the exact character/fraction that has been
/// sung, so the fill sweeps through the sentence instead of switching whole
/// characters (which looked like "no gradient" on wrapped lines).
pub fn paint_karaoke_fill(
    painter: &egui::Painter,
    origin: egui::Pos2,
    galley: &std::sync::Arc<egui::Galley>,
    progress: &crate::lyrics::KaraokeProgress,
    color: Color32,
) {
    let mut split = progress.sung.chars().count() as f32;
    if progress.next.is_some() {
        split += progress.next_fraction.clamp(0.0, 1.0);
    }
    if split <= 0.0 {
        return;
    }
    for placed in &galley.rows {
        if split <= 0.0 {
            return;
        }
        let glyphs = &placed.row.glyphs;
        let row_len = glyphs.len() as f32;
        let row_rect =
            egui::Rect::from_min_size(origin + placed.pos.to_vec2(), placed.row.size);
        if split >= row_len {
            painter
                .with_clip_rect(row_rect)
                .galley_with_override_text_color(origin, galley.clone(), color);
            split -= row_len;
            continue;
        }
        let index = (split.floor() as usize).min(glyphs.len().saturating_sub(1));
        let fraction = (split - index as f32).clamp(0.0, 1.0);
        let glyph = &glyphs[index];
        let x = placed.pos.x + glyph.pos.x + glyph.advance_width * fraction;
        let clip = egui::Rect::from_min_max(
            row_rect.min,
            egui::pos2(origin.x + x, row_rect.max.y),
        );
        if clip.width() > 0.0 {
            painter
                .with_clip_rect(clip)
                .galley_with_override_text_color(origin, galley.clone(), color);
        }
        return;
    }
}

pub fn now_playing(app: &mut App, ui: &mut Ui) {
    let colors = app.colors();
    let header_width = ui.available_width();
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(app.t("now.title"))
                .size(22.0)
                .strong()
                .color(colors.text),
        );
        if header_width > 460.0 {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(track) = app.current_track.clone() {
                    if track.lossless {
                        widgets::chip(ui, "LOSSLESS", colors.success, colors.card);
                    }
                    let format = track_format(&track);
                    if !format.is_empty() {
                        widgets::chip(ui, &format, colors.text_weak, colors.card);
                    }
                }
            });
        }
    });
    ui.add_space(10.0);

    let Some(track) = app.current_track.clone() else {
        widgets::empty_state(
            ui,
            egui_phosphor::regular::MUSIC_NOTES,
            app.t("player.no_track"),
            app.t("player.no_track_hint"),
            &colors,
            |ui| {
                let (open_files, open_folder) = widgets::centered_button_pair(
                    ui,
                    app.t("player.open_files"),
                    app.t("player.open_folder"),
                    &colors,
                );
                if open_files.clicked() {
                    app.open_files_dialog();
                }
                if open_folder.clicked() {
                    app.open_folder_dialog();
                }
            },
        );
        return;
    };

    let avail = ui.available_rect_before_wrap();
    let compact = avail.height() < 430.0;
    // With the lyrics sidebar open on a 1024-wide window the main column is
    // barely 450 px. The cover then steals the space the title and actions
    // need, so hide it at that size instead of letting content overflow.
    let hide_cover = avail.width() < 620.0;
    let cover_side = if hide_cover {
        0.0
    } else if compact {
        112.0
    } else {
        144.0
    };
    let hero_h = if hide_cover {
        // Title + artist + source chip + one action row, plus card padding.
        if compact { 150.0 } else { 174.0 }
    } else {
        cover_side + 28.0
    };
    let gap = 12.0;
    let show_visualizer = app.visualizer_visible() && avail.height() >= 520.0;
    let visualizer_h = if show_visualizer { 92.0 } else { 0.0 };
    let visualizer_gap = if show_visualizer { gap } else { 0.0 };
    let lyrics_h = (avail.height() - hero_h - gap - visualizer_h - visualizer_gap).max(120.0);

    let cover = app.cover.clone();
    let hero_rect = egui::Rect::from_min_size(avail.min, vec2(avail.width(), hero_h));
    card_in_rect(ui, hero_rect, &colors, |ui| {
        if hide_cover {
            hero_details(app, ui, &track, compact, &colors);
        } else {
            ui.horizontal(|ui| {
                let (rect, _) =
                    ui.allocate_exact_size(vec2(cover_side, cover_side), egui::Sense::hover());
                ui.painter()
                    .rect_filled(rect, CornerRadius::same(12), colors.card_hover);
                if let Some(texture) = &cover {
                    ui.painter().image(
                        texture.id(),
                        rect,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        Color32::WHITE,
                    );
                } else {
                    ui.painter().text(
                        rect.center(),
                        Align2::CENTER_CENTER,
                        egui_phosphor::regular::MUSIC_NOTES,
                        egui::FontId::proportional(42.0),
                        colors.text_faint,
                    );
                }

                ui.add_space(18.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 3.0;
                    let text_width = (ui.available_width() - 4.0).max(120.0);
                    ui.allocate_ui_with_layout(
                        vec2(text_width, 0.0),
                        egui::Layout::top_down(egui::Align::LEFT),
                        |ui| {
                            ui.set_width(text_width);
                            hero_details(app, ui, &track, compact, &colors);
                        },
                    );
                });
            });
        }
    });

    let lyrics_source = app
        .lyrics
        .as_ref()
        .map(|lyrics| lyrics.source.label())
        .unwrap_or("");
    if show_visualizer {
        let visualizer_rect = egui::Rect::from_min_size(
            egui::pos2(avail.left(), avail.top() + hero_h + gap),
            vec2(avail.width(), visualizer_h),
        );
        card_in_rect(ui, visualizer_rect, &colors, |ui| visualizer(app, ui));
    }
    let lyrics_rect = egui::Rect::from_min_size(
        egui::pos2(
            avail.left(),
            avail.top() + hero_h + gap + visualizer_h + visualizer_gap,
        ),
        vec2(avail.width(), lyrics_h),
    );
    card_in_rect(ui, lyrics_rect, &colors, |ui| {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(app.t("player.lyrics"))
                    .size(14.5)
                    .strong()
                    .color(colors.text),
            );
            if !lyrics_source.is_empty() && ui.available_width() > 150.0 {
                widgets::chip(ui, lyrics_source, colors.text_faint, colors.card_hover);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.available_width() > 400.0 {
                    if widgets::secondary_button(ui, crate::i18n::tr("刷新"), &colors)
                        .on_hover_text(crate::i18n::tr(
                            "重新读取当前曲目的标签/在线元信息与歌词",
                        ))
                        .clicked()
                    {
                        app.refresh_current_track();
                    }
                    if app.settings.features.lyric_editor
                        && widgets::secondary_button(ui, crate::i18n::tr("编辑"), &colors).clicked()
                    {
                        app.open_lyrics_editor();
                    }
                    if app.settings.features.fullscreen_lyrics
                        && widgets::secondary_button(ui, crate::i18n::tr("大歌词"), &colors)
                            .clicked()
                    {
                        app.toggle_fullscreen_lyrics();
                    }
                    if app.settings.features.fullscreen_lyrics
                        && widgets::secondary_button(ui, crate::i18n::tr("独立窗口"), &colors)
                            .clicked()
                    {
                        app.open_lyrics_window();
                    }
                    if widgets::secondary_button(ui, crate::i18n::tr("选择歌词文件"), &colors)
                        .clicked()
                    {
                        app.pick_lyrics_file();
                    }
                } else {
                    ui.menu_button(crate::i18n::tr("更多"), |ui| {
                        if ui
                            .button(crate::i18n::tr("刷新元信息与歌词"))
                            .clicked()
                        {
                            app.refresh_current_track();
                            ui.close();
                        }
                        if ui.button(crate::i18n::tr("选择歌词文件")).clicked() {
                            app.pick_lyrics_file();
                            ui.close();
                        }
                        if app.settings.features.lyric_editor
                            && ui.button(crate::i18n::tr("编辑歌词")).clicked()
                        {
                            app.open_lyrics_editor();
                            ui.close();
                        }
                        if app.settings.features.fullscreen_lyrics
                            && ui.button(crate::i18n::tr("大歌词")).clicked()
                        {
                            app.toggle_fullscreen_lyrics();
                            ui.close();
                        }
                        if app.settings.features.fullscreen_lyrics
                            && ui.button(crate::i18n::tr("独立歌词窗口")).clicked()
                        {
                            app.open_lyrics_window();
                            ui.close();
                        }
                    });
                }
            });
        });
        ui.add_space(4.0);
        // Reserve room for the source line that `lyrics_area` adds below the
        // scroll area; otherwise the card grows past its rect and its border
        // bleeds into the player bar.
        let area_h = (ui.available_height() - 26.0).max(80.0);
        lyrics_area(app, ui, area_h, LyricsAreaKind::Panel);
    });
}

fn hero_details(app: &mut App, ui: &mut Ui, track: &Track, compact: bool, colors: &ThemeColors) {
    ui.add(
        egui::Label::new(
            RichText::new(track.display_title())
                .size(if compact { 19.0 } else { 23.0 })
                .strong()
                .color(colors.text),
        )
        .truncate(),
    );
    let mut subtitle = track.display_artist();
    if !track.album.is_empty() {
        subtitle.push_str(" · ");
        subtitle.push_str(&track.album);
    }
    ui.add(
        egui::Label::new(
            RichText::new(subtitle)
                .size(if compact { 12.0 } else { 13.5 })
                .color(colors.text_weak),
        )
        .truncate(),
    );
    if !compact {
        ui.add_space(3.0);
        ui.horizontal(|ui| {
            widgets::chip(
                ui,
                if track.source_name.is_empty() {
                    track.source.kind_label()
                } else {
                    crate::i18n::tr_str(&track.source_name)
                },
                colors.accent,
                colors.accent_soft,
            );
            if let Some(duration) = track.duration {
                widgets::chip(
                    ui,
                    &format_time(duration),
                    colors.text_weak,
                    colors.card_hover,
                );
            }
            if let Some(year) = track.year {
                widgets::chip(ui, &year.to_string(), colors.text_weak, colors.card_hover);
            }
        });
    }
    ui.add_space(6.0);
    // Fold secondary actions into a menu when the row no longer fits, so the
    // hero card height never changes with window width.
    let wide = ui.available_width() >= 340.0;
    let playing = app.is_playing();
    ui.horizontal(|ui| {
        if widgets::primary_button(
            ui,
            if playing {
                app.t("player.pause")
            } else {
                app.t("player.play")
            },
            colors,
        )
        .clicked()
        {
            app.toggle_play();
        }
        if wide {
            if widgets::secondary_button(ui, app.t("player.open_files"), colors).clicked() {
                app.open_files_dialog();
            }
            if widgets::secondary_button(ui, app.t("player.desktop_lyrics"), colors).clicked() {
                app.toggle_desktop_lyrics();
            }
            if app.settings.features.fullscreen_lyrics
                && widgets::secondary_button(ui, crate::i18n::tr("大歌词"), colors).clicked()
            {
                app.toggle_fullscreen_lyrics();
            }
        } else {
            ui.menu_button(crate::i18n::tr("更多"), |ui| {
                if ui.button(app.t("player.open_files")).clicked() {
                    app.open_files_dialog();
                    ui.close();
                }
                if ui.button(app.t("player.desktop_lyrics")).clicked() {
                    app.toggle_desktop_lyrics();
                    ui.close();
                }
                if app.settings.features.fullscreen_lyrics
                    && ui.button(crate::i18n::tr("大歌词")).clicked()
                {
                    app.toggle_fullscreen_lyrics();
                    ui.close();
                }
            });
        }
    });
}

fn card_in_rect<R>(
    ui: &mut Ui,
    rect: egui::Rect,
    colors: &ThemeColors,
    add: impl FnOnce(&mut Ui) -> R,
) -> R {
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::top_down(egui::Align::LEFT)),
        |ui| {
            egui::Frame::new()
                .fill(colors.card)
                .stroke(egui::Stroke::new(1.0, colors.line))
                .corner_radius(CornerRadius::same(10))
                .inner_margin(egui::Margin::symmetric(18, 14))
                .show(ui, |ui| {
                    // Clamp the content to the card's rect. Without this the
                    // frame grows to fit overflowing children and its border
                    // can be painted over the player bar.
                    ui.set_clip_rect(ui.clip_rect().intersect(rect));
                    ui.set_width((rect.width() - 36.0).max(40.0));
                    ui.set_height((rect.height() - 28.0).max(20.0));
                    add(ui)
                })
                .inner
        },
    )
    .inner
}

pub fn lyrics_sidebar(app: &mut App, ui: &mut Ui) {
    let colors = app.colors();
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(app.t("player.lyrics"))
                .size(15.0)
                .strong()
                .color(colors.text),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if widgets::icon_button(
                ui,
                egui_phosphor::regular::X,
                15.0,
                app.t("common.close"),
                false,
                true,
                &colors,
            )
            .clicked()
            {
                app.toggle_lyrics_sidebar();
            }
            if app.settings.features.fullscreen_lyrics
                && widgets::icon_button(
                    ui,
                    egui_phosphor::regular::ARROWS_OUT,
                    15.0,
                    crate::i18n::tr("大歌词页"),
                    false,
                    true,
                    &colors,
                )
                .clicked()
            {
                app.toggle_fullscreen_lyrics();
            }
        });
    });
    ui.add_space(8.0);
    if let Some(track) = app.current_track.clone() {
        ui.label(
            RichText::new(track.display_title())
                .size(13.0)
                .strong()
                .color(colors.text),
        );
        ui.label(
            RichText::new(track.display_artist())
                .size(11.0)
                .color(colors.text_weak),
        );
    }
    ui.add_space(8.0);
    let height = (ui.available_height() - 28.0).max(120.0);
    lyrics_area(app, ui, height, LyricsAreaKind::Sidebar);
}

fn visualizer(app: &App, ui: &mut Ui) {
    use egui::Stroke;

    let settings = &app.settings.visualizer;
    let rect = ui.available_rect_before_wrap();
    if rect.width() < 24.0 || rect.height() < 16.0 {
        return;
    }
    let painter = ui.painter();
    let base = Color32::from_rgb(settings.color[0], settings.color[1], settings.color[2]);
    let alpha = (settings.opacity.clamp(0.0, 1.0) * 255.0) as u8;
    let color = Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), alpha);
    let faint = Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), alpha / 3);

    match settings.mode {
        crate::config::VisualizerMode::Off => {}
        crate::config::VisualizerMode::Bars => {
            if app.visual_levels.is_empty() {
                painter.text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    crate::i18n::tr("等待音频…"),
                    egui::FontId::proportional(11.0),
                    faint,
                );
                return;
            }
            let count = app.visual_levels.len();
            let gap = 2.0;
            let bar_width =
                ((rect.width() - gap * (count.saturating_sub(1)) as f32) / count as f32).max(1.0);
            let bottom = rect.bottom() - 2.0;
            for (index, level) in app.visual_levels.iter().enumerate() {
                let x = rect.left() + index as f32 * (bar_width + gap);
                let height = (level * (rect.height() - 8.0)).max(1.0);
                painter.rect_filled(
                    egui::Rect::from_min_max(
                        egui::pos2(x, bottom - height),
                        egui::pos2(x + bar_width, bottom),
                    ),
                    egui::CornerRadius::same(2),
                    color,
                );
                let peak = app.visual_peaks.get(index).copied().unwrap_or(0.0);
                let peak_y = bottom - (peak * (rect.height() - 8.0)).max(1.0);
                painter.line_segment(
                    [egui::pos2(x, peak_y), egui::pos2(x + bar_width, peak_y)],
                    Stroke::new(1.0, faint),
                );
            }
        }
        crate::config::VisualizerMode::Waveform => {
            if app.visual_wave.is_empty() {
                return;
            }
            let samples = &app.visual_wave;
            let stride = (samples.len() / 512).max(1);
            let points: Vec<egui::Pos2> = samples
                .iter()
                .step_by(stride)
                .enumerate()
                .map(|(index, sample)| {
                    let t = index as f32 / (samples.len() / stride).max(1) as f32;
                    egui::pos2(
                        rect.left() + t * rect.width(),
                        rect.center().y - sample.clamp(-1.0, 1.0) * rect.height() * 0.42,
                    )
                })
                .collect();
            if points.len() > 1 {
                painter.add(egui::Shape::line(points, Stroke::new(1.6, color)));
            }
        }
        crate::config::VisualizerMode::Ring => {
            let count = app.visual_levels.len().max(48);
            let center = rect.center();
            let radius = rect.height().min(rect.width()) * 0.30;
            painter.circle_stroke(center, radius, Stroke::new(1.0, faint));
            for index in 0..count {
                let level = app
                    .visual_levels
                    .get(index % app.visual_levels.len().max(1))
                    .copied()
                    .unwrap_or(0.0);
                let angle = index as f32 / count as f32 * std::f32::consts::TAU
                    - std::f32::consts::FRAC_PI_2;
                let direction = egui::vec2(angle.cos(), angle.sin());
                let inner = center + direction * radius;
                let outer = center + direction * (radius + level * radius * 0.9 + 2.0);
                painter.line_segment([inner, outer], Stroke::new(2.0, color));
            }
        }
    }
}

fn track_format(track: &Track) -> String {
    match &track.source {
        crate::model::TrackSource::Local { path } => path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_uppercase())
            .unwrap_or_default(),
        crate::model::TrackSource::Url { .. } => "URL".to_string(),
        crate::model::TrackSource::Radio { .. } => "RADIO".to_string(),
        crate::model::TrackSource::Bamboo { .. } => "ONLINE".to_string(),
    }
}
