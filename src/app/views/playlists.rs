//! Queue / playlist page.

use egui::{RichText, Ui};

use crate::app::{App, ToastKind, widgets};
use crate::model::RepeatMode;
use crate::util::format_time;

pub fn playlists(app: &mut App, ui: &mut Ui) {
    let colors = app.colors();
    widgets::page_header(
        ui,
        app.t("playlists.title"),
        app.t("playlists.subtitle"),
        &colors,
        |ui| {
            if widgets::secondary_button(ui, "导出歌单", &colors).clicked() {
                app.export_playlist_dialog();
            }
            if widgets::primary_button(ui, "导入歌单", &colors).clicked() {
                app.import_playlist_dialog();
            }
        },
    );

    if app.scan_active {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(
                RichText::new(format!("正在后台扫描… 已发现 {} 首", app.scan_found))
                    .size(11.5)
                    .color(colors.text_weak),
            );
            if widgets::secondary_button(ui, "取消", &colors).clicked() {
                app.cancel_scan();
            }
        });
        ui.add_space(4.0);
    }

    if app.queue.is_empty() {
        widgets::empty_state(
            ui,
            egui_phosphor::regular::QUEUE,
            app.t("playlists.empty"),
            app.t("playlists.empty_hint"),
            &colors,
            |ui| {
                if widgets::primary_button(ui, "导入歌单", &colors).clicked() {
                    app.import_playlist_dialog();
                }
                if widgets::secondary_button(ui, app.t("player.open_files"), &colors).clicked() {
                    app.open_files_dialog();
                }
            },
        );
        return;
    }

    if let Some(playlist) = app.current_playlist.clone() {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("当前歌单")
                    .size(10.5)
                    .color(colors.text_faint),
            );
            ui.add(
                egui::Label::new(
                    RichText::new(playlist.display().to_string())
                        .size(10.5)
                        .color(colors.text_weak),
                )
                .wrap(),
            );
            if widgets::secondary_button(ui, "重新扫描", &colors).clicked() {
                app.rescan_current_playlist();
            }
        });
        ui.add_space(4.0);
    }

    // Toolbar.
    ui.horizontal(|ui| {
        widgets::chip(
            ui,
            &format!("{} 首", app.queue.len()),
            colors.text_weak,
            colors.card,
        );
        widgets::chip(
            ui,
            &format_time(app.total_duration()),
            colors.text_weak,
            colors.card,
        );
        ui.add_space(8.0);
        if widgets::icon_button(
            ui,
            egui_phosphor::regular::SHUFFLE,
            17.0,
            app.t("player.shuffle"),
            app.queue.shuffle,
            true,
            &colors,
        )
        .clicked()
        {
            app.toggle_shuffle();
        }
        let repeat_icon = match app.queue.repeat {
            RepeatMode::One => egui_phosphor::regular::REPEAT_ONCE,
            _ => egui_phosphor::regular::REPEAT,
        };
        if widgets::icon_button(
            ui,
            repeat_icon,
            17.0,
            app.queue.repeat.label(),
            app.queue.repeat != RepeatMode::Off,
            true,
            &colors,
        )
        .clicked()
        {
            app.cycle_repeat();
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if widgets::secondary_button(ui, app.t("player.clear_queue"), &colors).clicked() {
                app.clear_queue();
            }
            if widgets::secondary_button(ui, app.t("player.open_files"), &colors).clicked() {
                app.open_files_dialog();
            }
        });
    });
    ui.add_space(10.0);

    let order = app.queue.display_order();
    let current = app.queue.current;
    let playing = app.is_playing();
    egui::ScrollArea::vertical()
        .id_salt("bamboo-queue-scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (display_pos, actual) in order.iter().copied().enumerate() {
                let track = app.queue.items[actual].clone();
                let is_current = current == Some(actual);
                let response =
                    widgets::song_row(ui, display_pos, &track, is_current, playing, &colors);
                if response.clicked() {
                    app.play_index(actual);
                }
                response.context_menu(|ui| {
                    if ui.button(app.t("player.play_now")).clicked() {
                        app.play_index(actual);
                        ui.close();
                    }
                    if ui.button(app.t("player.add_to_queue")).clicked() {
                        let key = track.dedupe_key();
                        if app.queue.items.iter().any(|t| t.dedupe_key() == key) {
                            app.toast("歌曲已在队列中", ToastKind::Info);
                        } else {
                            app.queue.add(track.clone());
                            app.session_dirty = true;
                        }
                        ui.close();
                    }
                    if ui.button(app.t("player.remove")).clicked() {
                        app.remove_index(actual);
                        ui.close();
                    }
                });
                let _ = display_pos;
            }
            ui.add_space(12.0);
        });
}
