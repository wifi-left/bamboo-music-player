//! Online search and discover placeholders (implemented with the Bamboo
//! source stage).

use egui::{RichText, Ui};

use crate::app::{App, ToastKind, widgets};

pub fn search(app: &mut App, ui: &mut Ui) {
    let colors = app.colors();
    widgets::page_header(
        ui,
        app.t("search.title"),
        app.t("search.subtitle"),
        &colors,
        |_ui| {},
    );

    ui.horizontal(|ui| {
        let width = (ui.available_width() - 130.0).max(220.0);
        let mut query = String::new();
        ui.add_sized(
            [width, 34.0],
            egui::TextEdit::singleline(&mut query)
                .hint_text(app.t("search.placeholder"))
                .margin(egui::Margin::symmetric(10, 8)),
        );
        if widgets::primary_button(ui, app.t("search.button"), &colors).clicked() {
            app.toast("在线搜索将在在线音源阶段启用", ToastKind::Info);
        }
    });
    ui.add_space(12.0);

    widgets::empty_state(
        ui,
        egui_phosphor::regular::MAGNIFYING_GLASS,
        app.t("search.empty"),
        "Bamboo 本地曲库与酷我/网易/QQ 音源将在下一阶段接入",
        &colors,
        |ui| {
            if widgets::secondary_button(ui, app.t("search.test_connection"), &colors).clicked() {
                app.toast("连接测试将在在线音源阶段启用", ToastKind::Info);
            }
        },
    );
}

pub fn discover(app: &mut App, ui: &mut Ui) {
    let colors = app.colors();
    widgets::page_header(
        ui,
        app.t("discover.title"),
        app.t("discover.subtitle"),
        &colors,
        |ui| {
            if widgets::secondary_button(ui, app.t("discover.refresh"), &colors).clicked() {
                app.toast("随机漫游将在发现阶段启用", ToastKind::Info);
            }
        },
    );

    ui.horizontal(|ui| {
        let mut link = String::new();
        ui.add_sized(
            [(ui.available_width() - 130.0).max(220.0), 34.0],
            egui::TextEdit::singleline(&mut link)
                .hint_text(app.t("discover.link_hint"))
                .margin(egui::Margin::symmetric(10, 8)),
        );
        if widgets::primary_button(ui, app.t("discover.open_link"), &colors).clicked() {
            app.toast("分享链接解析将在在线音源阶段启用", ToastKind::Info);
        }
    });

    ui.add_space(20.0);
    widgets::empty_state(
        ui,
        egui_phosphor::regular::COMPASS,
        "发现页即将上线",
        "随机漫游、专辑/歌手浏览与分享链接打开",
        &colors,
        |ui| {
            if widgets::secondary_button(ui, "打开本地文件", &colors).clicked() {
                app.open_files_dialog();
            }
        },
    );
    let _ = RichText::new("");
}
