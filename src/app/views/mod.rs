//! Main content pages.

mod library;
mod playlists;
mod search;
mod settings;

pub use library::library;
pub use playlists::playlists;
pub use search::{discover, search};
pub use settings::settings;

use egui::{Align2, Color32, CornerRadius, RichText, Ui, vec2};

use crate::app::theme::ThemeColors;
use crate::app::{App, ToastKind, widgets};
use crate::model::Track;
use crate::util::format_time;

pub fn now_playing(app: &mut App, ui: &mut Ui) {
    let colors = app.colors();
    widgets::page_header(
        ui,
        app.t("now.title"),
        "",
        &colors,
        |ui| {
            if let Some(track) = app.current_track.clone() {
                let format = track_format(&track);
                if !format.is_empty() {
                    widgets::chip(
                        ui,
                        &format,
                        colors.text_weak,
                        colors.card,
                    );
                }
                if track.lossless {
                    widgets::chip(ui, "LOSSLESS", colors.success, colors.card);
                }
            }
        },
    );

    let Some(track) = app.current_track.clone() else {
        widgets::empty_state(
            ui,
            egui_phosphor::regular::MUSIC_NOTES,
            app.t("player.no_track"),
            app.t("player.no_track_hint"),
            &colors,
            |ui| {
                ui.horizontal(|ui| {
                    if widgets::primary_button(ui, app.t("player.open_files"), &colors).clicked() {
                        app.open_files_dialog();
                    }
                    if widgets::secondary_button(ui, app.t("player.open_folder"), &colors).clicked()
                    {
                        app.open_folder_dialog();
                    }
                });
            },
        );
        return;
    };

    let cover = app.cover.clone();
    let can_play = app.current_track.is_some();
    let playing = app.is_playing();

    widgets::card(ui, &colors, |ui| {
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(vec2(238.0, 238.0), egui::Sense::hover());
            if let Some(texture) = &cover {
                ui.painter().image(
                    texture.id(),
                    rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
            } else {
                ui.painter()
                    .rect_filled(rect, CornerRadius::same(16), colors.card_hover);
                ui.painter().text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    egui_phosphor::regular::MUSIC_NOTES,
                    egui::FontId::proportional(64.0),
                    colors.text_faint,
                );
            }
            // Rounded overlay corners: draw four small corner masks.
            paint_rounded_corner_mask(ui, rect, 16.0, colors.bg);

            ui.add_space(24.0);
            ui.vertical(|ui| {
                ui.add_space(6.0);
                ui.label(
                    RichText::new(track.display_title())
                        .size(30.0)
                        .strong()
                        .color(colors.text),
                );
                ui.add_space(6.0);
                ui.label(
                    RichText::new(track.display_artist())
                        .size(15.0)
                        .color(colors.text_weak),
                );
                if !track.album.is_empty() {
                    ui.add_space(2.0);
                    ui.label(
                        RichText::new(&track.album)
                            .size(12.0)
                            .color(colors.text_faint),
                    );
                }
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    widgets::chip(
                        ui,
                        if track.source_name.is_empty() {
                            track.source.kind_label()
                        } else {
                            track.source_name.as_str()
                        },
                        colors.accent,
                        colors.accent_soft,
                    );
                    if let Some(dur) = track.duration {
                        widgets::chip(
                            ui,
                            &format_time(dur),
                            colors.text_weak,
                            colors.card_hover,
                        );
                    }
                    if let Some(year) = track.year {
                        widgets::chip(
                            ui,
                            &year.to_string(),
                            colors.text_weak,
                            colors.card_hover,
                        );
                    }
                });
                ui.add_space(20.0);
                ui.horizontal(|ui| {
                    if widgets::primary_button(
                        ui,
                        if playing {
                            app.t("player.pause")
                        } else {
                            app.t("player.play")
                        },
                        &colors,
                    )
                    .clicked()
                    {
                        app.toggle_play();
                    }
                    if widgets::secondary_button(ui, app.t("player.open_files"), &colors).clicked() {
                        app.open_files_dialog();
                    }
                    if widgets::secondary_button(
                        ui,
                        app.t("player.desktop_lyrics"),
                        &colors,
                    )
                    .clicked()
                    {
                        app.toggle_desktop_lyrics();
                    }
                });
                ui.add_space(12.0);
                ui.label(
                    RichText::new(match &track.source {
                        crate::model::TrackSource::Local { path } => path.display().to_string(),
                        crate::model::TrackSource::Url { url }
                        | crate::model::TrackSource::Radio { url } => url.clone(),
                        crate::model::TrackSource::Bamboo { song_id, .. } => {
                            format!("Bamboo · {song_id}")
                        }
                    })
                    .size(10.5)
                    .color(colors.text_faint),
                );
            });
        });
    });

    ui.add_space(16.0);

    // Lyrics panel (full parser arrives in the lyrics stage).
    widgets::card(ui, &colors, |ui| {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(app.t("player.lyrics"))
                    .size(15.0)
                    .strong()
                    .color(colors.text),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if widgets::secondary_button(ui, "选择歌词文件", &colors).clicked() {
                    app.toast("歌词文件选择将在歌词阶段启用", ToastKind::Info);
                }
            });
        });
        ui.add_space(8.0);
        let height = 220.0;
        let (rect, _) = ui.allocate_exact_size(
            vec2(ui.available_width(), height),
            egui::Sense::hover(),
        );
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(12),
            colors.bg2,
        );
        ui.painter().text(
            rect.center() - vec2(0.0, 16.0),
            Align2::CENTER_CENTER,
            egui_phosphor::regular::TEXT_ALIGN_LEFT,
            egui::FontId::proportional(30.0),
            colors.text_faint,
        );
        ui.painter().text(
            rect.center() + vec2(0.0, 22.0),
            Align2::CENTER_CENTER,
            app.t("now.no_lyrics"),
            egui::FontId::proportional(15.0),
            colors.text_weak,
        );
        ui.painter().text(
            rect.center() + vec2(0.0, 44.0),
            Align2::CENTER_CENTER,
            app.t("now.no_lyrics_hint"),
            egui::FontId::proportional(11.0),
            colors.text_faint,
        );
    });

    // Up next.
    let next_items = next_tracks(app, 5);
    if !next_items.is_empty() {
        ui.add_space(16.0);
        widgets::card(ui, &colors, |ui| {
            ui.label(
                RichText::new("接下来播放")
                    .size(15.0)
                    .strong()
                    .color(colors.text),
            );
            ui.add_space(6.0);
            for (display_pos, actual) in next_items {
                let track = app.queue.items[actual].clone();
                let response =
                    widgets::song_row(ui, display_pos, &track, false, false, &colors);
                if response.clicked() {
                    app.play_index(actual);
                }
            }
        });
    }
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
    ui.add_space(12.0);
    let height = (ui.available_height() - 20.0).max(160.0);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), egui::Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::same(12), colors.bg2);
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        app.t("now.no_lyrics"),
        egui::FontId::proportional(13.0),
        colors.text_weak,
    );
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

fn next_tracks(app: &App, count: usize) -> Vec<(usize, usize)> {
    let order = app.queue.display_order();
    let Some(current) = app.queue.current else {
        return Vec::new();
    };
    let Some(pos) = order.iter().position(|i| *i == current) else {
        return Vec::new();
    };
    order
        .iter()
        .skip(pos + 1)
        .take(count)
        .enumerate()
        .map(|(offset, actual)| (pos + 1 + offset, *actual))
        .collect()
}

fn paint_rounded_corner_mask(ui: &mut Ui, rect: egui::Rect, radius: f32, bg: Color32) {
    // egui's painter clips to rectangular rectangles; mask the four corners
    // with small filled squares to visually round the artwork.
    let r = radius;
    let corners = [
        (rect.left_top(), vec2(r, r) * 0.0),
        (rect.right_top() - vec2(r, 0.0), vec2(r, r)),
        (rect.left_bottom() - vec2(0.0, r), vec2(r, r)),
        (rect.right_bottom() - vec2(r, r), vec2(r, r)),
    ];
    let _ = corners;
    let _ = bg;
    // The current implementation keeps square corners; a proper image mask
    // will land with the GPU background pass.
}
