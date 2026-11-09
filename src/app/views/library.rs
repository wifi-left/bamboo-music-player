//! Library page: folder browsing, the SQLite index, statistics and the
//! duplicate/missing-file tools.

use egui::{Align, Align2, Layout, RichText, Sense, Ui, vec2};

use crate::app::{App, ToastKind, widgets};
use crate::config::DedupeMode;
use crate::library::{BrowserEntry, LibraryEntry, LibrarySort, SmartKind};
use crate::util::format_time;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LibraryTab {
    #[default]
    Browse,
    Index,
    Stats,
    Tools,
    Downloads,
}

impl LibraryTab {
    pub fn label(self) -> &'static str {
        match self {
            Self::Browse => crate::i18n::tr("文件夹"),
            Self::Index => crate::i18n::tr("曲库索引"),
            Self::Stats => crate::i18n::tr("统计"),
            Self::Tools => crate::i18n::tr("查重与缺失"),
            Self::Downloads => crate::i18n::tr("下载"),
        }
    }
}

pub fn library(app: &mut App, ui: &mut Ui) {
    let colors = app.colors();
    let tab = app.library_tab;

    widgets::page_header(
        ui,
        app.t("library.title"),
        crate::i18n::tr("浏览文件夹，或建立可搜索的曲库索引"),
        &colors,
        |ui| match tab {
            LibraryTab::Browse => {
                if widgets::secondary_button(ui, crate::i18n::tr("扫描全部目录"), &colors).clicked()
                {
                    let roots = app.settings.library.folders.clone();
                    if roots.is_empty() {
                        app.toast(crate::i18n::tr("请先添加音乐目录"), ToastKind::Info);
                    } else {
                        app.start_scan(roots, false);
                    }
                }
                if widgets::primary_button(ui, app.t("library.add_folder"), &colors).clicked() {
                    app.browse_folder_dialog();
                }
            }
            LibraryTab::Index => {
                if widgets::secondary_button(ui, crate::i18n::tr("更新索引"), &colors).clicked()
                {
                    app.start_library_index();
                }
                if widgets::primary_button(ui, app.t("library.add_folder"), &colors).clicked() {
                    app.browse_folder_dialog();
                }
            }
            LibraryTab::Stats => {
                if widgets::secondary_button(ui, crate::i18n::tr("刷新"), &colors).clicked() {
                    app.request_library_stats();
                }
                if widgets::secondary_button(ui, crate::i18n::tr("清空播放统计"), &colors).clicked()
                {
                    app.reset_play_statistics();
                }
            }
            LibraryTab::Tools => {
                if app.db_tools_busy {
                    ui.spinner();
                } else {
                    if widgets::secondary_button(ui, crate::i18n::tr("检查缺失"), &colors).clicked()
                    {
                        app.check_missing();
                    }
                    if widgets::secondary_button(ui, crate::i18n::tr("查找重复"), &colors).clicked()
                    {
                        app.find_duplicates();
                    }
                }
            }
            LibraryTab::Downloads => {
                if widgets::secondary_button(ui, crate::i18n::tr("下载目录"), &colors).clicked()
                {
                    app.open_download_dir();
                }
                if widgets::secondary_button(ui, crate::i18n::tr("清除已完成"), &colors).clicked()
                {
                    app.clear_finished_downloads();
                }
                if widgets::secondary_button(ui, crate::i18n::tr("清理未完成分片"), &colors).clicked()
                {
                    app.clear_download_partials();
                }
            }
        },
    );

    // Category tabs.
    let mut selected = app.library_tab;
    ui.horizontal_wrapped(|ui| {
        for candidate in LibraryTab::ALL {
            if candidate == LibraryTab::Stats && !app.settings.features.statistics {
                continue;
            }
            if candidate == LibraryTab::Tools && !app.settings.features.duplicate_finder {
                continue;
            }
            if candidate == LibraryTab::Downloads && !app.settings.features.downloads {
                continue;
            }
            let active = selected == candidate;
            let text = RichText::new(candidate.label())
                .size(13.0)
                .color(if active {
                    colors.accent
                } else {
                    colors.text_weak
                });
            if ui.selectable_label(active, text).clicked() {
                selected = candidate;
            }
        }
    });
    ui.add_space(2.0);
    ui.separator();
    ui.add_space(10.0);

    if selected != app.library_tab {
        app.library_tab = selected;
        app.scroll_to_top = true;
        match selected {
            LibraryTab::Index => {
                app.search_library(false);
                if app.db_stats.is_none() {
                    app.request_library_stats();
                }
            }
            LibraryTab::Stats => app.request_library_stats(),
            LibraryTab::Tools => app.db.missing(),
            LibraryTab::Downloads => {}
            LibraryTab::Browse => {}
        }
    }

    match app.library_tab {
        LibraryTab::Browse => browse_tab(app, ui, &colors),
        LibraryTab::Index => index_tab(app, ui, &colors),
        LibraryTab::Stats => stats_tab(app, ui, &colors),
        LibraryTab::Tools => tools_tab(app, ui, &colors),
        LibraryTab::Downloads => downloads_tab(app, ui, &colors),
    }
}

impl LibraryTab {
    pub const ALL: [LibraryTab; 5] = [
        LibraryTab::Browse,
        LibraryTab::Index,
        LibraryTab::Stats,
        LibraryTab::Tools,
        LibraryTab::Downloads,
    ];
}

// ---------------------------------------------------------------------------
// Downloads (P8)
// ---------------------------------------------------------------------------

fn downloads_tab(app: &mut App, ui: &mut Ui, colors: &crate::app::theme::ThemeColors) {
    if !app.settings.features.downloads {
        widgets::empty_state(
            ui,
            egui_phosphor::regular::DOWNLOAD_SIMPLE,
            crate::i18n::tr("下载功能已关闭"),
            crate::i18n::tr("可在 设置 → 功能开关 中重新启用"),
            colors,
            |ui| {
                if widgets::secondary_button(ui, crate::i18n::tr("打开功能开关"), colors).clicked()
                {
                    app.settings_tab = crate::app::views::SettingsTab::Features;
                    app.set_page(crate::model::Page::Settings);
                }
            },
        );
        return;
    }

    widgets::card(ui, colors, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(crate::i18n::tr("下载目录"))
                    .size(12.0)
                    .color(colors.text_weak),
            );
            ui.add(
                egui::Label::new(
                    RichText::new(crate::util::truncate_chars(
                        &app.download_dir().display().to_string(),
                        72,
                    ))
                    .size(11.0)
                    .color(colors.text),
                )
                .truncate(),
            );
            if ui.button(crate::i18n::tr("选择")).clicked() {
                app.choose_download_dir();
            }
            if ui.button(crate::i18n::tr("打开")).clicked() {
                app.open_download_dir();
            }
        });
        let active = app.unfinished_downloads();
        let completed = app
            .download_items
            .iter()
            .filter(|item| item.state == crate::download::DownloadState::Completed)
            .count();
        let conflict = match app.settings.download.conflict {
            crate::config::ConflictPolicy::Rename => crate::i18n::tr("重命名"),
            crate::config::ConflictPolicy::Skip => crate::i18n::tr("跳过"),
            crate::config::ConflictPolicy::Overwrite => crate::i18n::tr("覆盖"),
        };
        ui.label(
            RichText::new(crate::trf!(
                "{active} 个进行中 · {completed} 个已完成 · 并发 {} · 同名文件：{conflict}",
                active,
                completed,
                app.settings.download.concurrency,
                conflict
            ))
            .size(10.5)
            .color(colors.text_faint),
        );
        if app.settings.network.offline {
            ui.label(
                RichText::new(crate::i18n::tr(
                    "离线模式已开启，下载会立即失败；请先在设置中关闭离线模式",
                ))
                .size(10.5)
                .color(colors.warning),
            );
        }
        ui.horizontal(|ui| {
            let has_active = active > 0;
            if ui
                .add_enabled(has_active, egui::Button::new(crate::i18n::tr("全部取消")))
                .clicked()
            {
                app.cancel_all_downloads();
            }
            if widgets::secondary_button(ui, crate::i18n::tr("打开下载目录"), colors).clicked()
            {
                app.open_download_dir();
            }
            if widgets::secondary_button(ui, crate::i18n::tr("下载设置"), colors).clicked() {
                app.settings_tab = crate::app::views::SettingsTab::Online;
                app.set_page(crate::model::Page::Settings);
            }
        });
    });
    ui.add_space(10.0);

    let mut items = app.download_items.clone();
    items.sort_by(|a, b| {
        b.state
            .is_active()
            .cmp(&a.state.is_active())
            .then_with(|| b.created.cmp(&a.created))
    });
    if items.is_empty() {
        widgets::empty_state(
            ui,
            egui_phosphor::regular::DOWNLOAD_SIMPLE,
            crate::i18n::tr("下载队列为空"),
            crate::i18n::tr("在在线搜索或专辑曲目中点击下载按钮即可加入队列"),
            colors,
            |ui| {
                if widgets::primary_button(ui, crate::i18n::tr("去在线搜索"), colors).clicked()
                {
                    app.set_page(crate::model::Page::Search);
                }
            },
        );
        return;
    }

    let mut cancel: Option<u64> = None;
    let mut retry: Option<u64> = None;
    let mut remove: Option<u64> = None;
    let mut reveal: Option<std::path::PathBuf> = None;
    for item in &items {
        widgets::card(ui, colors, |ui| {
            ui.set_width(ui.available_width());
            let state_color = match item.state {
                crate::download::DownloadState::Completed => colors.success,
                crate::download::DownloadState::Failed => colors.danger,
                crate::download::DownloadState::Canceled
                | crate::download::DownloadState::Skipped => colors.text_faint,
                _ => colors.accent,
            };
            ui.horizontal(|ui| {
                let text_width = (ui.available_width() - 190.0).max(160.0);
                ui.allocate_ui_with_layout(
                    egui::vec2(text_width, 0.0),
                    egui::Layout::top_down(egui::Align::LEFT),
                    |ui| {
                        ui.set_width(text_width);
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(item.track.display_title())
                                        .size(13.5)
                                        .strong()
                                        .color(colors.text),
                                )
                                .truncate(),
                            );
                            widgets::chip(ui, item.state.label(), state_color, colors.card_hover);
                        });
                        let mut subtitle = item.track.subtitle();
                        if let Some(quality) = item.quality {
                            if !subtitle.is_empty() {
                                subtitle.push_str(" · ");
                            }
                            subtitle.push_str(quality.label());
                        }
                        ui.add(
                            egui::Label::new(
                                RichText::new(subtitle).size(10.5).color(colors.text_weak),
                            )
                            .truncate(),
                        );
                    },
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(crate::i18n::tr("移除")).clicked() {
                        remove = Some(item.id);
                    }
                    if item.state.is_active() {
                        if ui.button(crate::i18n::tr("取消")).clicked() {
                            cancel = Some(item.id);
                        }
                    } else if ui.button(crate::i18n::tr("重试")).clicked() {
                        retry = Some(item.id);
                    }
                    if let Some(path) = item.path.as_ref()
                        && ui.button(crate::i18n::tr("定位文件")).clicked()
                    {
                        reveal = Some(path.clone());
                    }
                });
            });

            if item.state.is_active() || item.state == crate::download::DownloadState::Completed {
                let fraction = match item.total.filter(|total| *total > 0) {
                    Some(total) => (item.received as f32 / total as f32).clamp(0.0, 1.0),
                    None if item.state == crate::download::DownloadState::Completed => 1.0,
                    None => 0.0,
                };
                let size_text = match item.total {
                    Some(total) => {
                        format!("{} / {}", format_bytes(item.received), format_bytes(total))
                    }
                    None => format_bytes(item.received),
                };
                ui.add(
                    egui::ProgressBar::new(fraction)
                        .desired_width(ui.available_width())
                        .text(RichText::new(size_text).size(10.0)),
                );
            }
            if let Some(error) = item.error.as_ref() {
                ui.add(
                    egui::Label::new(
                        RichText::new(crate::util::truncate_chars(error, 140))
                            .size(10.0)
                            .color(colors.danger),
                    )
                    .wrap(),
                );
            }
            if let Some(path) = item.path.as_ref() {
                ui.add(
                    egui::Label::new(
                        RichText::new(crate::util::truncate_chars(&path.display().to_string(), 90))
                            .size(9.8)
                            .color(colors.text_faint),
                    )
                    .truncate(),
                );
            }
        });
        ui.add_space(6.0);
    }

    if let Some(id) = cancel {
        app.cancel_download(id);
    }
    if let Some(id) = retry {
        app.retry_download(id);
    }
    if let Some(id) = remove {
        app.remove_download(id);
    }
    if let Some(path) = reveal {
        crate::util::open_in_explorer(&path);
    }
}

// ---------------------------------------------------------------------------
// Folder browser (existing behaviour)
// ---------------------------------------------------------------------------

fn browse_tab(app: &mut App, ui: &mut Ui, colors: &crate::app::theme::ThemeColors) {
    if app.scan_active {
        widgets::card(ui, colors, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(
                    RichText::new(crate::trf!("正在后台扫描… 已发现 {} 首", app.scan_found))
                        .size(12.5)
                        .color(colors.text),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if widgets::secondary_button(ui, crate::i18n::tr("取消扫描"), colors).clicked()
                    {
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
                RichText::new(crate::i18n::tr("音乐目录"))
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
            crate::i18n::tr("选择一个音乐目录后可按文件夹浏览；大目录不会一次性扫描"),
            colors,
            |ui| {
                if widgets::primary_button(ui, app.t("library.add_folder"), colors).clicked() {
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
            crate::i18n::tr("上一级"),
            false,
            current.parent().is_some(),
            colors,
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
                crate::i18n::tr("刷新"),
                false,
                true,
                colors,
            )
            .clicked()
            {
                app.open_browser(current.clone());
            }
            if widgets::secondary_button(ui, crate::i18n::tr("播放全部"), colors).clicked() {
                app.start_scan(vec![current.clone()], true);
            }
            if widgets::secondary_button(ui, crate::i18n::tr("递归加入队列"), colors).clicked()
            {
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
                RichText::new(crate::i18n::tr("正在读取文件夹…"))
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
                RichText::new(crate::i18n::tr("这个文件夹里没有音频文件"))
                    .size(13.0)
                    .color(colors.text_weak),
            );
            ui.add_space(8.0);
            if widgets::secondary_button(ui, crate::i18n::tr("递归加入队列"), colors).clicked()
            {
                app.start_scan(vec![current.clone()], false);
            }
        });
        return;
    }

    ui.horizontal(|ui| {
        widgets::chip(
            ui,
            &crate::trf!("{} 项", entries.len()),
            colors.text_weak,
            colors.card,
        );
        if app.browser.truncated {
            widgets::chip(
                ui,
                crate::i18n::tr("仅显示前 20000 项"),
                colors.warning,
                colors.card,
            );
        }
    });
    ui.add_space(4.0);

    egui::ScrollArea::vertical()
        .id_salt("bamboo-library-entries")
        .auto_shrink([false, false])
        .show_rows(ui, 40.0, entries.len(), |ui, range| {
            for index in range {
                let entry = entries[index].clone();
                let response = browser_row(ui, &entry, colors);
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
                        if ui.button(crate::i18n::tr("打开文件夹")).clicked() {
                            app.open_browser(entry.path.clone());
                            ui.close();
                        }
                        if ui.button(crate::i18n::tr("递归加入队列")).clicked() {
                            app.start_scan(vec![entry.path.clone()], false);
                            ui.close();
                        }
                        if ui.button(crate::i18n::tr("播放全部")).clicked() {
                            app.start_scan(vec![entry.path.clone()], true);
                            ui.close();
                        }
                    } else {
                        if entry.is_playlist && ui.button(crate::i18n::tr("导入歌单")).clicked()
                        {
                            app.import_playlist(entry.path.clone(), true);
                            ui.close();
                        }
                        if !entry.is_playlist && ui.button(crate::i18n::tr("播放")).clicked() {
                            app.play_browser_file(entry.path.clone());
                            ui.close();
                        }
                        if ui.button(crate::i18n::tr("加入队列")).clicked() {
                            app.add_paths(vec![entry.path.clone()], false);
                            ui.close();
                        }
                        if ui.button(crate::i18n::tr("打开所在文件夹")).clicked() {
                            if let Some(parent) = entry.path.parent() {
                                crate::util::open_in_explorer(parent);
                            }
                            ui.close();
                        }
                    }
                    ui.separator();
                    if !entry.is_dir {
                        if let Some(name) = entry.path.file_name() {
                            if ui.button(crate::i18n::tr("复制文件名")).clicked() {
                                ui.ctx().copy_text(name.to_string_lossy().into_owned());
                                ui.close();
                            }
                        }
                        if ui.button(crate::i18n::tr("复制完整路径")).clicked() {
                            ui.ctx().copy_text(entry.path.display().to_string());
                            ui.close();
                        }
                    }
                    if ui.button(crate::i18n::tr("在资源管理器中显示")).clicked() {
                        crate::util::reveal_in_explorer(&entry.path);
                        ui.close();
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
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 38.0), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &entry.name));
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect.shrink(1.0),
            egui::CornerRadius::same(8),
            egui::Stroke::new(1.5, colors.accent),
            egui::StrokeKind::Inside,
        );
    }
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

// ---------------------------------------------------------------------------
// Index tab
// ---------------------------------------------------------------------------

fn index_tab(app: &mut App, ui: &mut Ui, colors: &crate::app::theme::ThemeColors) {
    if app.db_index_active {
        widgets::card(ui, colors, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(
                    RichText::new(crate::trf!(
                        "正在建立索引… 已写入 {} 首",
                        app.db_index_progress
                    ))
                    .size(12.5)
                    .color(colors.text),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if widgets::secondary_button(ui, crate::i18n::tr("取消索引"), colors).clicked()
                    {
                        app.cancel_library_index();
                    }
                });
            });
            if !app.db_index_current.is_empty() {
                ui.add(
                    egui::Label::new(
                        RichText::new(&app.db_index_current)
                            .size(10.5)
                            .color(colors.text_faint),
                    )
                    .wrap(),
                );
            }
        });
        ui.add_space(10.0);
    }

    ui.horizontal(|ui| {
        let response = ui.add(
            egui::TextEdit::singleline(&mut app.library_search)
                .hint_text(crate::i18n::tr("搜索标题、歌手、专辑"))
                .desired_width(260.0),
        );
        if response.changed() {
            app.search_library(false);
        }
        ui.add_space(8.0);
        let mut sort = app.library_sort;
        egui::ComboBox::from_id_salt("library-sort")
            .selected_text(sort.label())
            .show_ui(ui, |ui| {
                for candidate in [
                    LibrarySort::Title,
                    LibrarySort::Artist,
                    LibrarySort::Album,
                    LibrarySort::Added,
                    LibrarySort::Duration,
                    LibrarySort::PlayCount,
                    LibrarySort::LastPlayed,
                    LibrarySort::Path,
                ] {
                    ui.selectable_value(&mut sort, candidate, candidate.label());
                }
            });
        if sort != app.library_sort {
            app.library_sort = sort;
            app.search_library(false);
        }
        if app.library_loading {
            ui.spinner();
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if widgets::secondary_button(ui, crate::i18n::tr("更新索引"), colors).clicked() {
                app.start_library_index();
            }
        });
    });
    ui.add_space(6.0);

    ui.horizontal(|ui| {
        widgets::chip(
            ui,
            &crate::trf!("曲库 {} 首", app.library_total),
            colors.text_weak,
            colors.card,
        );
        if app.library_rows.len() < app.library_total {
            widgets::chip(
                ui,
                &crate::trf!("已显示 {}", app.library_rows.len()),
                colors.text_faint,
                colors.card,
            );
        }
        if let Some(stats) = &app.db_stats
            && stats.missing > 0
        {
            widgets::chip(
                ui,
                &crate::trf!("缺失 {}", stats.missing),
                colors.warning,
                colors.card,
            );
        }
    });
    ui.add_space(4.0);

    if app.library_rows.is_empty() && !app.library_loading {
        widgets::empty_state(
            ui,
            egui_phosphor::regular::DATABASE,
            crate::i18n::tr("曲库还没有索引"),
            crate::i18n::tr(
                "「更新索引」会读取音乐目录中的标签并写入本地 SQLite 数据库；之后即可搜索、统计和生成智能歌单",
            ),
            colors,
            |ui| {
                if widgets::primary_button(ui, crate::i18n::tr("更新索引"), colors).clicked() {
                    app.start_library_index();
                }
            },
        );
        return;
    }

    // Page the index instead of scrolling a long list: each row lays out text
    // (two galleys), so rendering hundreds per frame was the main cost. The
    // page size follows the visible height, with a floor of five rows.
    const ROW_HEIGHT: f32 = 44.0;
    const PAGER_HEIGHT: f32 = 38.0;
    let viewport_height = ui.available_height().max(0.0);
    let per_page = (((viewport_height - PAGER_HEIGHT) / ROW_HEIGHT).floor() as usize).max(5);
    let total = app.library_total.max(app.library_rows.len());
    let total_pages = total.div_ceil(per_page).max(1);
    app.library_page = app.library_page.min(total_pages - 1);
    let page = app.library_page;
    let start = page * per_page;
    let end = (start + per_page).min(app.library_rows.len());

    // Moving past the rows loaded from SQLite fetches the next chunk (the
    // database returns 300 at a time).
    if start >= app.library_rows.len()
        && app.library_rows.len() < app.library_total
        && !app.library_loading
    {
        app.search_library(true);
    }

    if start < end {
        for index in start..end {
            let entry = app.library_rows[index].clone();
            let response = library_row(ui, &entry, colors, false);
            if response.clicked() {
                app.play_library_entry(&entry);
            }
            context_menu(app, &response, &entry);
        }
    } else if app.library_loading || app.library_rows.len() < app.library_total {
        ui.add_space(16.0);
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(
                RichText::new(crate::i18n::tr("正在加载…"))
                    .size(11.5)
                    .color(colors.text_weak),
            );
        });
    }

    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if ui
            .add_enabled(page > 0, egui::Button::new(crate::i18n::tr("上一页")))
            .clicked()
        {
            app.library_page = page - 1;
        }
        ui.label(
            RichText::new(crate::trf!("第 {} / {} 页", page + 1, total_pages))
                .size(11.5)
                .color(colors.text_weak),
        );
        if ui
            .add_enabled(
                page + 1 < total_pages,
                egui::Button::new(crate::i18n::tr("下一页")),
            )
            .clicked()
        {
            app.library_page = page + 1;
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                RichText::new(crate::trf!("每页 {} 首", per_page))
                    .size(10.5)
                    .color(colors.text_faint),
            );
        });
    });
}

fn library_row(
    ui: &mut Ui,
    entry: &LibraryEntry,
    colors: &crate::app::theme::ThemeColors,
    missing: bool,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 44.0), Sense::click());
    let row_label = if entry.artist.trim().is_empty() {
        entry.title.clone()
    } else {
        format!("{} · {}", entry.title, entry.artist)
    };
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &row_label));
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect.shrink(1.0),
            egui::CornerRadius::same(8),
            egui::Stroke::new(1.5, colors.accent),
            egui::StrokeKind::Inside,
        );
    }
    if response.hovered() {
        ui.painter()
            .rect_filled(rect, egui::CornerRadius::same(8), colors.card_hover);
    }
    let title_color = colors.text;
    let text_width = (rect.width() - 150.0).max(120.0);
    paint_elided(
        ui,
        egui::pos2(rect.left() + 12.0, rect.top() + 16.0),
        &entry.title,
        egui::FontId::proportional(13.0),
        title_color,
        text_width,
    );
    let subtitle = if missing {
        crate::trf!("缺失 · {}", entry.path.display())
    } else {
        entry.subtitle()
    };
    paint_elided(
        ui,
        egui::pos2(rect.left() + 12.0, rect.top() + 32.0),
        &subtitle,
        egui::FontId::proportional(10.5),
        colors.text_faint,
        text_width,
    );
    let mut right = rect.right_center() - vec2(14.0, 0.0);
    if entry.play_count > 0 {
        let text = crate::trf!("{} 次", entry.play_count);
        ui.painter().text(
            right,
            Align2::RIGHT_CENTER,
            &text,
            egui::FontId::proportional(10.0),
            colors.accent,
        );
        right.x -= 58.0;
    }
    if let Some(duration) = entry.duration {
        ui.painter().text(
            right,
            Align2::RIGHT_CENTER,
            format_time(duration),
            egui::FontId::proportional(10.5),
            colors.text_faint,
        );
    }
    let mut info = format!(
        "{}\n{} · {}",
        entry.path.display(),
        if entry.format.is_empty() {
            crate::i18n::tr("未知格式")
        } else {
            entry.format.as_str()
        },
        format_bytes(entry.size)
    );
    if let Some(timestamp) = entry.last_played_at
        && let Some(time) = chrono::DateTime::from_timestamp(timestamp, 0)
    {
        info.push_str(&crate::trf!(
            "\n上次播放：{}",
            time.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M")
        ));
    }
    response.on_hover_text(info)
}

fn context_menu(app: &mut App, response: &egui::Response, entry: &LibraryEntry) {
    response.context_menu(|ui| {
        if ui.button(crate::i18n::tr("播放")).clicked() {
            app.play_library_entry(entry);
            ui.close();
        }
        if ui.button(crate::i18n::tr("加入队列")).clicked() {
            app.queue_library_entries(std::slice::from_ref(entry));
            ui.close();
        }
        if ui.button(crate::i18n::tr("在资源管理器中显示")).clicked() {
            crate::util::reveal_in_explorer(&entry.path);
            ui.close();
        }
        ui.separator();
        if ui
            .button(crate::i18n::tr("从曲库移除（不删除文件）"))
            .clicked()
        {
            app.remove_library_keys(vec![entry.key.clone()]);
            ui.close();
        }
        if let Some(name) = entry.path.file_name() {
            if ui.button(crate::i18n::tr("复制文件名")).clicked() {
                ui.ctx().copy_text(name.to_string_lossy().into_owned());
                ui.close();
            }
        }
        if ui.button(crate::i18n::tr("复制完整路径")).clicked() {
            ui.ctx().copy_text(entry.path.display().to_string());
            ui.close();
        }
    });
}

// ---------------------------------------------------------------------------
// Statistics tab
// ---------------------------------------------------------------------------

fn stats_tab(app: &mut App, ui: &mut Ui, colors: &crate::app::theme::ThemeColors) {
    if app.db_stats.is_none() && !app.db_stats_loading {
        app.request_library_stats();
    }
    let Some(stats) = app.db_stats.clone() else {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(
                RichText::new(crate::i18n::tr("正在统计…"))
                    .size(12.0)
                    .color(colors.text_weak),
            );
        });
        return;
    };
    if app.db_stats_loading {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(
                RichText::new(crate::i18n::tr("正在刷新…"))
                    .size(11.0)
                    .color(colors.text_faint),
            );
        });
    }

    widgets::card(ui, colors, |ui| {
        ui.set_width(ui.available_width());
        ui.label(
            RichText::new(crate::i18n::tr("曲库概览"))
                .size(15.0)
                .strong()
                .color(colors.text),
        );
        ui.add_space(10.0);
        ui.horizontal_wrapped(|ui| {
            widgets::chip(
                ui,
                &crate::trf!("{} 首", stats.tracks),
                colors.text,
                colors.card_hover,
            );
            widgets::chip(
                ui,
                &crate::trf!("{} 位歌手", stats.artists),
                colors.text_weak,
                colors.card_hover,
            );
            widgets::chip(
                ui,
                &crate::trf!("{} 张专辑", stats.albums),
                colors.text_weak,
                colors.card_hover,
            );
            widgets::chip(
                ui,
                &crate::trf!("总时长 {}", format_hours(stats.total_secs)),
                colors.text_weak,
                colors.card_hover,
            );
            widgets::chip(
                ui,
                &crate::trf!("总播放 {} 次", stats.total_plays),
                colors.accent,
                colors.card_hover,
            );
            if stats.lossless > 0 {
                widgets::chip(
                    ui,
                    &crate::trf!("无损 {}", stats.lossless),
                    colors.success,
                    colors.card_hover,
                );
            }
            if stats.missing > 0 {
                widgets::chip(
                    ui,
                    &crate::trf!("缺失 {}", stats.missing),
                    colors.warning,
                    colors.card_hover,
                );
            }
            widgets::chip(
                ui,
                &crate::trf!("占用 {}", format_bytes(stats.total_bytes)),
                colors.text_faint,
                colors.card_hover,
            );
        });
    });
    ui.add_space(12.0);

    if !stats.formats.is_empty() {
        widgets::card(ui, colors, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                RichText::new(crate::i18n::tr("格式分布"))
                    .size(15.0)
                    .strong()
                    .color(colors.text),
            );
            ui.add_space(8.0);
            let max = stats
                .formats
                .iter()
                .map(|(_, n)| *n)
                .max()
                .unwrap_or(1)
                .max(1);
            for (format, count) in &stats.formats {
                let fraction = *count as f32 / max as f32;
                ui.horizontal(|ui| {
                    ui.allocate_ui_with_layout(
                        vec2(70.0, 18.0),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            ui.label(
                                RichText::new(if format.is_empty() {
                                    crate::i18n::tr("未知")
                                } else {
                                    format.as_str()
                                })
                                .size(11.5)
                                .color(colors.text_weak),
                            );
                        },
                    );
                    let (rect, _) = ui.allocate_exact_size(
                        vec2((ui.available_width() - 70.0).max(40.0), 10.0),
                        Sense::hover(),
                    );
                    ui.painter()
                        .rect_filled(rect, egui::CornerRadius::same(5), colors.card_hover);
                    let filled = egui::Rect::from_min_size(
                        rect.min,
                        vec2(rect.width() * fraction, rect.height()),
                    );
                    ui.painter()
                        .rect_filled(filled, egui::CornerRadius::same(5), colors.accent);
                    ui.label(
                        RichText::new(format!("{count}"))
                            .size(10.5)
                            .color(colors.text_faint),
                    );
                });
                ui.add_space(3.0);
            }
        });
        ui.add_space(12.0);
    }

    if !stats.top_artists.is_empty() {
        widgets::card(ui, colors, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                RichText::new(crate::i18n::tr("最常播放歌手"))
                    .size(15.0)
                    .strong()
                    .color(colors.text),
            );
            ui.add_space(8.0);
            for (index, (artist, plays)) in stats.top_artists.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("{:02}", index + 1))
                            .size(11.0)
                            .color(colors.text_faint),
                    );
                    ui.label(RichText::new(artist).size(12.5).color(colors.text));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(crate::trf!("{plays} 次", plays))
                                .size(10.5)
                                .color(colors.accent),
                        );
                    });
                });
                ui.add_space(3.0);
            }
        });
        ui.add_space(12.0);
    }

    if !stats.top_tracks.is_empty() {
        widgets::card(ui, colors, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                RichText::new(crate::i18n::tr("最常播放曲目"))
                    .size(15.0)
                    .strong()
                    .color(colors.text),
            );
            ui.add_space(8.0);
            for (title, artist, plays) in &stats.top_tracks {
                ui.horizontal(|ui| {
                    ui.add(
                        egui::Label::new(
                            RichText::new(crate::util::truncate_chars(title, 46))
                                .size(12.5)
                                .color(colors.text),
                        )
                        .truncate(),
                    );
                    ui.label(
                        RichText::new(crate::util::truncate_chars(artist, 22))
                            .size(10.5)
                            .color(colors.text_faint),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(crate::trf!("{plays} 次", plays))
                                .size(10.5)
                                .color(colors.accent),
                        );
                    });
                });
                ui.add_space(3.0);
            }
        });
        ui.add_space(12.0);
    }

    if !stats.recently_played.is_empty() {
        widgets::card(ui, colors, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                RichText::new(crate::i18n::tr("最近播放"))
                    .size(15.0)
                    .strong()
                    .color(colors.text),
            );
            ui.add_space(8.0);
            let entries = stats.recently_played.clone();
            for entry in &entries {
                let response = library_row(ui, entry, colors, false);
                if response.clicked() {
                    app.play_library_entry(entry);
                }
                context_menu(app, &response, entry);
            }
        });
    }
}

// ---------------------------------------------------------------------------
// Duplicates / missing tools
// ---------------------------------------------------------------------------

fn tools_tab(app: &mut App, ui: &mut Ui, colors: &crate::app::theme::ThemeColors) {
    ui.horizontal_wrapped(|ui| {
        ui.label(
            RichText::new(crate::i18n::tr("查重方式"))
                .size(11.5)
                .color(colors.text_weak),
        );
        let mut mode = app.settings.library.dedupe_mode;
        egui::ComboBox::from_id_salt("dedupe-mode")
            .selected_text(dedupe_label(mode))
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut mode,
                    DedupeMode::Metadata,
                    crate::i18n::tr("标题 + 歌手 + 时长"),
                );
                ui.selectable_value(&mut mode, DedupeMode::Path, crate::i18n::tr("同一文件路径"));
                ui.selectable_value(
                    &mut mode,
                    DedupeMode::Fingerprint,
                    crate::i18n::tr("音频指纹"),
                );
            });
        if mode != app.settings.library.dedupe_mode {
            app.settings.library.dedupe_mode = mode;
            app.settings_dirty = true;
            app.db_duplicates.clear();
        }
        if widgets::secondary_button(ui, crate::i18n::tr("查找重复"), colors).clicked() {
            app.find_duplicates();
        }
        if widgets::secondary_button(ui, crate::i18n::tr("检查缺失文件"), colors).clicked() {
            app.check_missing();
        }
        if widgets::secondary_button(ui, crate::i18n::tr("重新检查曲库"), colors).clicked() {
            app.start_library_index();
        }
        if app.db_tools_busy {
            ui.spinner();
        }
    });
    ui.add_space(10.0);

    if !app.db_duplicates.is_empty() {
        widgets::card(ui, colors, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(crate::trf!("重复曲目（{} 组）", app.db_duplicates.len()))
                        .size(15.0)
                        .strong()
                        .color(colors.text),
                );
            });
            ui.add_space(4.0);
            let note = match app.settings.library.dedupe_mode {
                DedupeMode::Metadata => crate::i18n::tr(
                    "同名同歌手的文件按 1.5 秒内时长归为一组；移除只影响曲库索引，不会删除文件",
                ),
                DedupeMode::Path => crate::i18n::tr(
                    "按文件真实路径归组（含符号链接/大小写差异）；移除只影响曲库索引，不会删除文件",
                ),
                DedupeMode::Fingerprint => crate::i18n::tr(
                    "解码音频生成指纹后比对，可识别标签不同或重新编码的同一首歌；首次扫描较慢，结果会缓存",
                ),
            };
            ui.label(RichText::new(note).size(10.5).color(colors.text_faint));
            ui.add_space(8.0);
            let groups = app.db_duplicates.clone();
            for (group_index, group) in groups.iter().enumerate() {
                ui.add_space(6.0);
                ui.label(
                    RichText::new(crate::trf!(
                        "{} — {}（{} 个）",
                        group.title,
                        group.artist,
                        group.entries.len()
                    ))
                    .size(12.5)
                    .strong()
                    .color(colors.text),
                );
                for entry in &group.entries {
                    let response = library_row(ui, entry, colors, false);
                    if response.clicked() {
                        app.play_library_entry(entry);
                    }
                    context_menu(app, &response, entry);
                }
                if ui
                    .button(crate::i18n::tr("保留第一个，从曲库移除其余"))
                    .on_hover_text(crate::i18n::tr("不会删除磁盘上的文件"))
                    .clicked()
                {
                    let keys: Vec<String> = group
                        .entries
                        .iter()
                        .skip(1)
                        .map(|e| e.key.clone())
                        .collect();
                    app.remove_library_keys(keys);
                    if let Some(remove) = app.db_duplicates.get_mut(group_index) {
                        remove.entries.truncate(1);
                    }
                }
            }
        });
        ui.add_space(12.0);
    }

    widgets::card(ui, colors, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(crate::trf!("缺失文件（{}）", app.db_missing.len()))
                    .size(15.0)
                    .strong()
                    .color(colors.text),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if !app.db_missing.is_empty()
                    && widgets::secondary_button(ui, crate::i18n::tr("移除全部缺失"), colors)
                        .clicked()
                {
                    app.clear_missing_entries();
                }
                if widgets::secondary_button(ui, crate::i18n::tr("刷新列表"), colors).clicked()
                {
                    app.db.missing();
                }
            });
        });
        ui.add_space(6.0);
        if app.db_missing.is_empty() {
            ui.label(
                RichText::new(crate::i18n::tr(
                    "没有缺失文件。点击「检查缺失文件」可重新核对磁盘路径",
                ))
                .size(11.5)
                .color(colors.text_faint),
            );
        } else {
            let entries = app.db_missing.clone();
            for entry in &entries {
                let response = library_row(ui, entry, colors, true);
                response.context_menu(|ui| {
                    if ui.button(crate::i18n::tr("从曲库移除")).clicked() {
                        app.remove_library_keys(vec![entry.key.clone()]);
                        ui.close();
                    }
                    if ui.button(crate::i18n::tr("复制路径")).clicked() {
                        ui.ctx().copy_text(entry.path.display().to_string());
                        ui.close();
                    }
                });
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Single line text clipped with `…` at `max_width`. Character-count limits
/// are too coarse for CJK text next to right-aligned duration columns.
fn paint_elided(
    ui: &Ui,
    center_left: egui::Pos2,
    text: &str,
    font: egui::FontId,
    color: egui::Color32,
    max_width: f32,
) {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_owned(), font, color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(max_width);
    let galley = ui.painter().layout_job(job);
    let top_left = egui::pos2(center_left.x, center_left.y - galley.size().y * 0.5);
    ui.painter().galley(top_left, galley, color);
}

fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{b} B")
    }
}

fn format_hours(secs: f64) -> String {
    let hours = secs / 3600.0;
    if hours >= 1.0 {
        crate::trf!("{hours:.1} 小时", format!("{hours:.1}"))
    } else {
        crate::trf!("{:.0} 分钟", format!("{:.0}", secs / 60.0))
    }
}

/// Smart playlist chips are shown above the queue on the playlists page.
pub fn smart_playlists(app: &mut App, ui: &mut Ui) {
    if !app.settings.features.smart_playlists {
        return;
    }
    let colors = app.colors();
    widgets::card(ui, &colors, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(crate::i18n::tr("智能歌单"))
                    .size(13.0)
                    .strong()
                    .color(colors.text),
            );
            let mut selected: Option<SmartKind> = None;
            egui::ComboBox::from_id_salt("smart-playlist-menu")
                .selected_text(crate::i18n::tr("选择智能歌单"))
                .width(170.0)
                .show_ui(ui, |ui| {
                    for kind in SmartKind::ALL {
                        if ui.selectable_label(false, kind.label()).clicked() {
                            selected = Some(kind);
                            ui.close();
                        }
                    }
                });
            if app.smart_pending.is_some() {
                ui.spinner();
                ui.label(
                    RichText::new(crate::i18n::tr("正在查询曲库…"))
                        .size(10.0)
                        .color(colors.text_faint),
                );
            } else {
                ui.label(
                    RichText::new(crate::i18n::tr("选择后加入队列并播放"))
                        .size(10.0)
                        .color(colors.text_faint),
                );
            }
            if let Some(kind) = selected {
                app.load_smart_playlist(kind, true);
            }
        });
    });
    ui.add_space(10.0);
}

fn dedupe_label(mode: DedupeMode) -> &'static str {
    match mode {
        DedupeMode::Metadata => crate::i18n::tr("标题 + 歌手 + 时长"),
        DedupeMode::Path => crate::i18n::tr("同一文件路径"),
        DedupeMode::Fingerprint => crate::i18n::tr("音频指纹"),
    }
}
