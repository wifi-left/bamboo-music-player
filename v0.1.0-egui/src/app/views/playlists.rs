//! Queue / playlist page.

use egui::{RichText, Ui};

use crate::app::{App, ToastKind, widgets};
use crate::model::{RepeatMode, TrackSource};
use crate::util::format_time;

pub fn playlists(app: &mut App, ui: &mut Ui) {
    let colors = app.colors();
    widgets::page_header(
        ui,
        app.t("playlists.title"),
        app.t("playlists.subtitle"),
        &colors,
        |ui| {
            if widgets::secondary_button(ui, crate::i18n::tr("导出歌单"), &colors).clicked() {
                app.export_playlist_dialog();
            }
            if widgets::primary_button(ui, crate::i18n::tr("导入歌单"), &colors).clicked() {
                app.import_playlist_dialog();
            }
        },
    );

    // Smart playlists live on the Discover page now.

    if app.scan_active {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(
                RichText::new(crate::trf!("正在后台扫描… 已发现 {} 首", app.scan_found))
                    .size(11.5)
                    .color(colors.text_weak),
            );
            if widgets::secondary_button(ui, crate::i18n::tr("取消"), &colors).clicked() {
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
                let (import, open_files) = widgets::centered_button_pair(
                    ui,
                    crate::i18n::tr("导入歌单"),
                    app.t("player.open_files"),
                    &colors,
                );
                if import.clicked() {
                    app.import_playlist_dialog();
                }
                if open_files.clicked() {
                    app.open_files_dialog();
                }
            },
        );
        return;
    }

    if let Some(playlist) = app.current_playlist.clone() {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(crate::i18n::tr("当前歌单"))
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
            if widgets::secondary_button(ui, crate::i18n::tr("重新扫描"), &colors).clicked() {
                app.rescan_current_playlist();
            }
        });
        ui.add_space(4.0);
    }

    // Toolbar. On narrow windows (e.g. the lyrics sidebar is open) the
    // right-aligned file actions used to overlap "重新随机"; split them onto a
    // second row instead.
    let narrow = ui.available_width() < 700.0;
    ui.horizontal(|ui| {
        widgets::chip(
            ui,
            &crate::trf!("{} 首", app.queue.len()),
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
        ui.add_space(6.0);
        if widgets::secondary_button(ui, crate::i18n::tr("重新随机"), &colors)
            .on_hover_text(crate::i18n::tr(
                "重新打乱当前列表的播放顺序（当前曲目保持播放）",
            ))
            .clicked()
        {
            app.reroll_shuffle();
        }
        if !narrow {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if widgets::secondary_button(ui, app.t("player.clear_queue"), &colors).clicked() {
                    app.clear_queue();
                }
                if widgets::secondary_button(ui, crate::i18n::tr("打开 URL"), &colors)
                    .on_hover_text(crate::i18n::tr(
                        "Bamboo 分享链接 / 音频直链 / .mp3list .m3u 歌单",
                    ))
                    .clicked()
                {
                    app.open_url_dialog();
                }
                if widgets::secondary_button(ui, app.t("player.open_files"), &colors).clicked() {
                    app.open_files_dialog();
                }
            });
        }
    });
    if narrow {
        ui.horizontal(|ui| {
            if widgets::secondary_button(ui, app.t("player.open_files"), &colors).clicked() {
                app.open_files_dialog();
            }
            if widgets::secondary_button(ui, crate::i18n::tr("打开 URL"), &colors).clicked() {
                app.open_url_dialog();
            }
            if widgets::secondary_button(ui, app.t("player.clear_queue"), &colors).clicked() {
                app.clear_queue();
            }
        });
    }
    ui.add_space(10.0);

    let order = app.queue.display_order();
    let total = order.len();
    let query = app.queue_filter.trim().to_lowercase();
    let rows: Vec<(usize, usize)> = order
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, actual)| {
            app.queue
                .items
                .get(*actual)
                .is_some_and(|track| track.matches_query(&query))
        })
        .map(|(display_pos, actual)| (display_pos, actual))
        .collect();
    ui.horizontal(|ui| {
        let label = ui.label(
            RichText::new(crate::i18n::tr("筛选队列"))
                .size(11.0)
                .color(colors.text_weak),
        );
        ui.add(
            egui::TextEdit::singleline(&mut app.queue_filter)
                .hint_text(crate::i18n::tr("筛选队列…"))
                .desired_width(220.0),
        )
        .labelled_by(label.id);
        if !app.queue_filter.trim().is_empty() {
            if widgets::icon_button(
                ui,
                egui_phosphor::regular::X,
                13.0,
                crate::i18n::tr("清除筛选"),
                false,
                true,
                &colors,
            )
            .clicked()
            {
                app.queue_filter.clear();
            }
            widgets::chip(
                ui,
                &crate::trf!("{} / {} 首", rows.len(), total),
                colors.text_weak,
                colors.card,
            );
        }
    });
    ui.add_space(6.0);

    let current = app.queue.current;
    let playing = app.is_playing();
    let mut drop_move = None;
    let mut move_by = None;
    if rows.is_empty() {
        ui.add_space(16.0);
        ui.vertical_centered(|ui| {
            ui.label(
                RichText::new(crate::i18n::tr("没有匹配的曲目"))
                    .size(12.5)
                    .color(colors.text_weak),
            );
        });
    } else {
        egui::ScrollArea::vertical()
            .id_salt("bamboo-queue-scroll")
            .auto_shrink([false, false])
            // The queue can hold thousands of tracks; laying out every row on
            // every frame was a large part of the immediate-mode CPU cost
            // while a song was playing.
            .show_rows(ui, 52.0, rows.len(), |ui, range| {
                for &(display_pos, actual) in &rows[range] {
                    let track = app.queue.items[actual].clone();
                    let is_current = current == Some(actual);
                    let response =
                        widgets::song_row(ui, display_pos, &track, is_current, playing, &colors);
                    response.dnd_set_drag_payload(display_pos);
                    if let Some(payload) = response.dnd_hover_payload::<usize>() {
                        if *payload != display_pos {
                            let y = ui
                                .ctx()
                                .pointer_latest_pos()
                                .map(|pointer| {
                                    if pointer.y < response.rect.center().y {
                                        response.rect.top()
                                    } else {
                                        response.rect.bottom()
                                    }
                                })
                                .unwrap_or(response.rect.center().y);
                            ui.painter().line_segment(
                                [
                                    egui::pos2(response.rect.left() + 8.0, y),
                                    egui::pos2(response.rect.right() - 8.0, y),
                                ],
                                egui::Stroke::new(2.0, colors.accent),
                            );
                        }
                    }
                    if let Some(payload) = response.dnd_release_payload::<usize>() {
                        drop_move = Some((*payload, display_pos));
                    }
                    if response.clicked() {
                        app.play_index(actual);
                    }
                    let visible_pos = rows
                        .iter()
                        .position(|(position, _)| *position == display_pos)
                        .unwrap_or(0);
                    response.context_menu(|ui| {
                        ui.add_enabled_ui(visible_pos > 0, |ui| {
                            if ui.button(crate::i18n::tr("上移")).clicked() {
                                move_by = Some((display_pos, rows[visible_pos - 1].0));
                                ui.close();
                            }
                        });
                        ui.add_enabled_ui(visible_pos + 1 < rows.len(), |ui| {
                            if ui.button(crate::i18n::tr("下移")).clicked() {
                                move_by = Some((display_pos, rows[visible_pos + 1].0));
                                ui.close();
                            }
                        });
                        ui.add_enabled_ui(display_pos > 0, |ui| {
                            if ui.button(crate::i18n::tr("移到顶部")).clicked() {
                                move_by = Some((display_pos, 0));
                                ui.close();
                            }
                        });
                        ui.add_enabled_ui(display_pos + 1 < total, |ui| {
                            if ui.button(crate::i18n::tr("移到底部")).clicked() {
                                move_by = Some((display_pos, total - 1));
                                ui.close();
                            }
                        });
                        ui.separator();
                        if ui.button(app.t("player.play_now")).clicked() {
                            app.play_index(actual);
                            ui.close();
                        }
                        if ui.button(app.t("player.add_to_queue")).clicked() {
                            let key = track.dedupe_key();
                            if app.queue.items.iter().any(|t| t.dedupe_key() == key) {
                                app.toast(crate::i18n::tr("歌曲已在队列中"), ToastKind::Info);
                            } else {
                                app.queue.add(track.clone());
                                app.session_dirty = true;
                            }
                            ui.close();
                        }
                        if !matches!(track.source, TrackSource::Local { .. })
                            && ui.button(crate::i18n::tr("下载")).clicked()
                        {
                            app.start_download_track(&track, None);
                            ui.close();
                        }
                        if ui.button(app.t("player.remove")).clicked() {
                            app.remove_index(actual);
                            ui.close();
                        }
                        widgets::track_clipboard_menu(ui, &track);
                    });
                }
                ui.add_space(12.0);
            });
    }

    let moved = drop_move
        .or(move_by)
        .is_some_and(|(from, to)| app.queue.move_in_order(from, to));
    if moved {
        app.session_dirty = true;
        app.ctx.request_repaint();
    }

    if app.url_dialog_open {
        let mut open = app.url_dialog_open;
        let mut submit = false;
        let mut cancel = false;
        let sources: Vec<(String, String)> = app
            .settings
            .enabled_sources()
            .map(|source| (source.id.clone(), source.name.clone()))
            .collect();
        egui::Window::new(crate::i18n::tr("打开 URL"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(540.0)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                ui.label(
                    RichText::new(crate::i18n::tr(
                        "Bamboo 分享链接 / 音频直链 / .mp3list .m3u 歌单",
                    ))
                    .size(11.0)
                    .color(colors.text_weak),
                );
                ui.add_space(4.0);
                let response = ui.add(
                    egui::TextEdit::singleline(&mut app.url_input)
                        .hint_text(crate::i18n::tr("https://... 或 #musicid=..."))
                        .desired_width(500.0),
                );
                if response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
                    submit = true;
                }
                if !sources.is_empty() {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(crate::i18n::tr("Bamboo 音源"))
                                .size(11.0)
                                .color(colors.text_weak),
                        );
                        let selected = sources
                            .iter()
                            .find(|(id, _)| id == &app.url_source)
                            .map(|(_, name)| name.clone())
                            .unwrap_or_else(|| sources[0].1.clone());
                        egui::ComboBox::from_id_salt("url-source")
                            .selected_text(selected)
                            .show_ui(ui, |ui| {
                                for (id, name) in &sources {
                                    ui.selectable_value(&mut app.url_source, id.clone(), name);
                                }
                            });
                    });
                }
                if let Some(error) = &app.url_error {
                    ui.add_space(4.0);
                    ui.colored_label(colors.danger, error);
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(crate::i18n::tr("打开并播放")).clicked() {
                        submit = true;
                    }
                    if ui.button(crate::i18n::tr("取消")).clicked() {
                        cancel = true;
                    }
                });
            });
        if submit {
            app.submit_url();
        } else if cancel {
            app.url_dialog_open = false;
        } else {
            app.url_dialog_open = open;
        }
    }
}
