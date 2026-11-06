//! Library page (scanning lands in the library stage; folders are managed
//! here already).

use egui::{RichText, Ui};

use crate::app::{App, widgets};

pub fn library(app: &mut App, ui: &mut Ui) {
    let colors = app.colors();
    widgets::page_header(
        ui,
        app.t("library.title"),
        app.t("library.subtitle"),
        &colors,
        |ui| {
            if widgets::secondary_button(ui, app.t("library.scan"), &colors).clicked() {
                app.toast("曲库扫描将在曲库阶段启用", crate::app::ToastKind::Info);
            }
            if widgets::primary_button(ui, app.t("library.add_folder"), &colors).clicked() {
                app.open_folder_dialog();
            }
        },
    );

    let folders = app.settings.library.folders.clone();
    if folders.is_empty() {
        widgets::empty_state(
            ui,
            egui_phosphor::regular::FOLDERS,
            app.t("library.empty"),
            app.t("library.empty_hint"),
            &colors,
            |ui| {
                if widgets::primary_button(ui, app.t("library.add_folder"), &colors).clicked() {
                    app.open_folder_dialog();
                }
            },
        );
        return;
    }

    widgets::section_title(ui, "音乐目录", &colors);
    for folder in folders {
        widgets::card(ui, &colors, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(egui_phosphor::regular::FOLDER_OPEN)
                        .size(20.0)
                        .color(colors.accent),
                );
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new(
                            folder
                                .file_name()
                                .map(|s| s.to_string_lossy().into_owned())
                                .unwrap_or_else(|| folder.display().to_string()),
                        )
                        .size(13.5)
                        .strong()
                        .color(colors.text),
                    );
                    ui.label(
                        RichText::new(folder.display().to_string())
                            .size(10.0)
                            .color(colors.text_faint),
                    );
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if widgets::secondary_button(ui, "移除", &colors).clicked() {
                        app.settings
                            .library
                            .folders
                            .retain(|f| f != &folder);
                        app.settings_dirty = true;
                    }
                });
            });
        });
        ui.add_space(8.0);
    }

    ui.add_space(16.0);
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 160.0),
        egui::Sense::hover(),
    );
    ui.painter()
        .rect_filled(rect, egui::CornerRadius::same(14), colors.card);
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        "曲库索引、专辑/歌手/文件夹视图将在曲库阶段启用",
        egui::FontId::proportional(12.0),
        colors.text_faint,
    );
}
