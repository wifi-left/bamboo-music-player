//! Library page: lazy folder browsing plus background recursive scanning.

use egui::{Align2, RichText, Sense, Ui, vec2};

use crate::app::{App, ToastKind, widgets};
use crate::library::BrowserEntry;

pub fn library(app: &mut App, ui: &mut Ui) {
    let colors = app.colors();
    widgets::page_header(
        ui,
        app.t("library.title"),
        app.t("library.subtitle"),
        &colors,
        |ui| {
            if widgets::secondary_button(ui, "扫描全部目录", &colors).clicked() {
                let roots = app.settings.library.folders.clone();
                if roots.is_empty() {
                    app.toast("请先添加音乐目录", ToastKind::Info);
                } else {
                    app.start_scan(roots, false);
                }
            }
            if widgets::primary_button(ui, app.t("library.add_folder"), &colors).clicked() {
                app.browse_folder_dialog();
            }
        },
    );

    // Background scan progress.
    if app.scan_active {
        widgets::card(ui, &colors, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(
                    RichText::new(format!("正在后台扫描… 已发现 {} 首", app.scan_found))
                        .size(12.5)
                        .color(colors.text),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if widgets::secondary_button(ui, "取消扫描", &colors).clicked() {
                        app.cancel_scan();
                    }
                });
            });
            if !app.scan_current.is_empty() {
                ui.add(
                    egui::Label::new(
                        RichText::new(&app.scan_current)
                            .size(10.5)
                            .color(colors.text_faint),
                    )
                    .wrap(),
                );
            }
        });
        ui.add_space(10.0);
    }

    let roots = app.settings.library.folders.clone();
    if !roots.is_empty() {
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new("音乐目录")
                    .size(10.5)
                    .color(colors.text_faint),
            );
            for root in &roots {
                let label = root
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| root.display().to_string());
                if ui
                    .button(format!("{}  {}", egui_phosphor::regular::FOLDER, label))
                    .clicked()
                {
                    app.open_browser(root.clone());
                }
            }
        });
        ui.add_space(8.0);
    }

    let Some(current) = app.browser.current.clone() else {
        widgets::empty_state(
            ui,
            egui_phosphor::regular::FOLDERS,
            app.t("library.empty"),
            "选择一个音乐目录后可按文件夹浏览；大目录不会一次性扫描",
            &colors,
            |ui| {
                if widgets::primary_button(ui, app.t("library.add_folder"), &colors).clicked() {
                    app.browse_folder_dialog();
                }
            },
        );
        return;
    };

    // Breadcrumb / actions.
    ui.horizontal(|ui| {
        if widgets::icon_button(
            ui,
            egui_phosphor::regular::ARROW_LEFT,
            16.0,
            "上一级",
            false,
            current.parent().is_some(),
            &colors,
        )
        .clicked()
        {
            app.browser_up();
        }
        ui.add(
            egui::Label::new(
                RichText::new(current.display().to_string())
                    .size(11.5)
                    .color(colors.text_weak),
            )
            .wrap(),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if widgets::icon_button(
                ui,
                egui_phosphor::regular::ARROW_CLOCKWISE,
                15.0,
                "刷新",
                false,
                true,
                &colors,
            )
            .clicked()
            {
                app.open_browser(current.clone());
            }
            if widgets::secondary_button(ui, "播放全部", &colors).clicked() {
                app.start_scan(vec![current.clone()], true);
            }
            if widgets::secondary_button(ui, "递归加入队列", &colors).clicked() {
                app.start_scan(vec![current.clone()], false);
            }
        });
    });
    ui.add_space(6.0);

    if let Some(error) = app.browser.error.clone() {
        ui.label(RichText::new(error).size(11.0).color(colors.danger));
        return;
    }
    if app.browser.loading {
        ui.add_space(20.0);
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(
                RichText::new("正在读取文件夹…")
                    .size(12.0)
                    .color(colors.text_weak),
            );
        });
        return;
    }

    let entries = app.browser.entries.clone();
    if entries.is_empty() {
        ui.add_space(24.0);
        ui.vertical_centered(|ui| {
            ui.label(
                RichText::new("这个文件夹里没有音频文件")
                    .size(13.0)
                    .color(colors.text_weak),
            );
            ui.add_space(8.0);
            if widgets::secondary_button(ui, "递归加入队列", &colors).clicked() {
                app.start_scan(vec![current.clone()], false);
            }
        });
        return;
    }

    ui.horizontal(|ui| {
        widgets::chip(
            ui,
            &format!("{} 项", entries.len()),
            colors.text_weak,
            colors.card,
        );
        if app.browser.truncated {
            widgets::chip(ui, "仅显示前 20000 项", colors.warning, colors.card);
        }
    });
    ui.add_space(4.0);

    egui::ScrollArea::vertical()
        .id_salt("bamboo-library-entries")
        .auto_shrink([false, false])
        .show_rows(ui, 40.0, entries.len(), |ui, range| {
            for index in range {
                let entry = entries[index].clone();
                let response = browser_row(ui, &entry, &colors);
                if response.clicked() {
                    if entry.is_dir {
                        app.open_browser(entry.path.clone());
                    } else if entry.is_playlist {
                        app.import_playlist(entry.path.clone(), true);
                    } else {
                        app.play_browser_file(entry.path.clone());
                    }
                }
                response.context_menu(|ui| {
                    if entry.is_dir {
                        if ui.button("打开文件夹").clicked() {
                            app.open_browser(entry.path.clone());
                            ui.close();
                        }
                        if ui.button("递归加入队列").clicked() {
                            app.start_scan(vec![entry.path.clone()], false);
                            ui.close();
                        }
                        if ui.button("播放全部").clicked() {
                            app.start_scan(vec![entry.path.clone()], true);
                            ui.close();
                        }
                    } else {
                        if entry.is_playlist && ui.button("导入歌单").clicked() {
                            app.import_playlist(entry.path.clone(), true);
                            ui.close();
                        }
                        if !entry.is_playlist && ui.button("播放").clicked() {
                            app.play_browser_file(entry.path.clone());
                            ui.close();
                        }
                        if ui.button("加入队列").clicked() {
                            app.add_paths(vec![entry.path.clone()], false);
                            ui.close();
                        }
                        if ui.button("打开所在文件夹").clicked() {
                            if let Some(parent) = entry.path.parent() {
                                crate::util::open_in_explorer(parent);
                            }
                            ui.close();
                        }
                    }
                });
            }
        });
}

fn browser_row(
    ui: &mut Ui,
    entry: &BrowserEntry,
    colors: &crate::app::theme::ThemeColors,
) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), 38.0), Sense::click());
    if response.hovered() {
        ui.painter()
            .rect_filled(rect, egui::CornerRadius::same(8), colors.card_hover);
    }
    let icon = if entry.is_dir {
        egui_phosphor::regular::FOLDER
    } else if entry.is_playlist {
        egui_phosphor::regular::QUEUE
    } else {
        egui_phosphor::regular::MUSIC_NOTES
    };
    let icon_color = if entry.is_dir {
        colors.warning
    } else {
        colors.accent
    };
    ui.painter().text(
        rect.left_center() + vec2(16.0, 0.0),
        Align2::LEFT_CENTER,
        icon,
        egui::FontId::proportional(18.0),
        icon_color,
    );
    ui.painter().text(
        rect.left_center() + vec2(46.0, 0.0),
        Align2::LEFT_CENTER,
        crate::util::truncate_chars(&entry.name, 90),
        egui::FontId::proportional(13.0),
        if entry.is_dir {
            colors.text
        } else {
            colors.text_weak
        },
    );
    if let Some(size) = entry.size {
        ui.painter().text(
            rect.right_center() - vec2(16.0, 0.0),
            Align2::RIGHT_CENTER,
            format_bytes(size),
            egui::FontId::proportional(10.5),
            colors.text_faint,
        );
    }
    response
}

fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    let b = bytes as f64;
    if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{b} B")
    }
}
