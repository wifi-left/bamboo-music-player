//! P10 lyrics editor page.
//!
//! The document lives in `App::lyrics_editor` so closing the page does not
//! discard unsaved work. All edits are applied to that model immediately; the
//! page only performs app-level actions (save/reload/seek) after the model has
//! been put back.

use egui::{Align, Layout, RichText, Ui, vec2};

use crate::app::{App, widgets};
use crate::lyrics::LyricsEditor;
use crate::lyrics::editor::{format_tag, parse_tag};
use crate::util::format_time;

#[derive(Clone, Copy)]
enum Action {
    None,
    Close,
    Save,
    SaveAs,
    ShareImage,
    SaveAndClose,
    Reload,
    Seek(f64, bool),
}

pub fn lyrics_editor(app: &mut App, ui: &mut Ui) {
    let Some(mut editor) = app.lyrics_editor.take() else {
        app.lyrics_editor_open = false;
        return;
    };
    let colors = app.colors();
    let position = app.position_secs();
    let mut action = Action::None;

    // Keyboard: Ctrl+S / Ctrl+Shift+S / Esc / Ctrl+Z / Ctrl+Y. Text edits
    // keep egui's own undo while a text field has focus, so the document-level
    // undo only triggers otherwise.
    let text_focused = ui.memory(|memory| memory.focused().is_some());
    let (save, save_as, escape, undo, redo) = ui.input(|input| {
        let command = input.modifiers.command;
        let shift = input.modifiers.shift;
        let save = command && input.key_pressed(egui::Key::S);
        (
            save,
            save && shift,
            input.key_pressed(egui::Key::Escape),
            command && !shift && input.key_pressed(egui::Key::Z),
            command
                && ((shift && input.key_pressed(egui::Key::Z))
                    || (!shift && input.key_pressed(egui::Key::Y))),
        )
    });
    if !text_focused {
        if undo {
            editor.undo();
        } else if redo {
            editor.redo();
        }
    }
    if save_as {
        action = Action::SaveAs;
    } else if save {
        action = Action::Save;
    } else if escape && !editor.confirm_close {
        action = Action::Close;
    }

    header(ui, &mut editor, &colors, &mut action);

    let width = ui.available_width();
    let height = ui.available_height().max(200.0);
    if width >= 880.0 {
        let left_w = (width * 0.52).clamp(380.0, 720.0);
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(vec2(left_w, height), Layout::top_down(Align::LEFT), |ui| {
                line_list(&mut editor, ui, position, &colors, &mut action, None)
            });
            ui.add_space(12.0);
            ui.allocate_ui_with_layout(
                vec2(ui.available_width().max(280.0), height),
                Layout::top_down(Align::LEFT),
                |ui| details(&mut editor, ui, height, position, &colors, &mut action),
            );
        });
    } else {
        egui::ScrollArea::vertical()
            .id_salt("bamboo-lyrics-editor-narrow")
            .show(ui, |ui| {
                let list_h = (height * 0.42).clamp(180.0, 360.0);
                line_list(
                    &mut editor,
                    ui,
                    position,
                    &colors,
                    &mut action,
                    Some(list_h),
                );
                ui.add_space(12.0);
                details(&mut editor, ui, height, position, &colors, &mut action);
            });
    }

    if editor.confirm_close {
        confirm_dialog(ui, &mut editor, &colors, &mut action);
    }

    app.lyrics_editor = Some(editor);
    match action {
        Action::None => {}
        Action::Close => app.request_close_lyrics_editor(),
        Action::Save => {
            app.save_lyrics_editor(false);
        }
        Action::SaveAs => {
            app.save_lyrics_editor(true);
        }
        Action::ShareImage => app.save_lyrics_share_image(),
        Action::SaveAndClose => {
            if let Some(editor) = app.lyrics_editor.as_ref() {
                if !editor.dirty {
                    app.force_close_lyrics_editor();
                } else if app.save_lyrics_editor(false) {
                    app.force_close_lyrics_editor();
                }
            }
        }
        Action::Reload => app.reload_lyrics_editor(),
        Action::Seek(secs, play) => {
            app.seek_to(secs);
            if play && !app.is_playing() {
                app.toggle_play();
            }
        }
    }
}

fn header(
    ui: &mut Ui,
    editor: &mut LyricsEditor,
    colors: &crate::app::theme::ThemeColors,
    action: &mut Action,
) {
    ui.horizontal(|ui| {
        if widgets::icon_button(
            ui,
            egui_phosphor::regular::ARROW_LEFT,
            18.0,
            crate::i18n::tr("返回 (Esc)"),
            false,
            true,
            colors,
        )
        .clicked()
        {
            *action = Action::Close;
        }
        ui.add_space(4.0);
        ui.label(
            RichText::new(crate::i18n::tr("歌词编辑器"))
                .size(22.0)
                .strong()
                .color(colors.text),
        );
        widgets::chip(ui, &editor.track_label, colors.text_weak, colors.card);
        if editor.dirty {
            widgets::chip(ui, crate::i18n::tr("未保存"), colors.danger, colors.card);
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if widgets::primary_button(ui, crate::i18n::tr("保存"), colors).clicked() {
                *action = Action::Save;
            }
            if widgets::secondary_button(ui, crate::i18n::tr("分享图"), colors).clicked() {
                *action = Action::ShareImage;
            }
            if widgets::secondary_button(ui, crate::i18n::tr("另存为"), colors).clicked() {
                *action = Action::SaveAs;
            }
            ui.add_enabled_ui(editor.can_redo(), |ui| {
                if widgets::secondary_button(ui, crate::i18n::tr("重做"), colors)
                    .on_hover_text(crate::i18n::tr("重做 (Ctrl+Y)"))
                    .clicked()
                {
                    editor.redo();
                }
            });
            ui.add_enabled_ui(editor.can_undo(), |ui| {
                if widgets::secondary_button(ui, crate::i18n::tr("撤销"), colors)
                    .on_hover_text(crate::i18n::tr("撤销 (Ctrl+Z)"))
                    .clicked()
                {
                    editor.undo();
                }
            });
            if widgets::secondary_button(ui, crate::i18n::tr("重新载入"), colors).clicked() {
                *action = Action::Reload;
            }
            ui.label(
                RichText::new(if editor.timed {
                    "LRC"
                } else {
                    crate::i18n::tr("纯文本")
                })
                .size(10.5)
                .color(colors.text_faint),
            );
        });
    });
    ui.add_space(6.0);
}

fn line_list(
    editor: &mut LyricsEditor,
    ui: &mut Ui,
    position: f64,
    colors: &crate::app::theme::ThemeColors,
    action: &mut Action,
    list_height_override: Option<f32>,
) {
    widgets::card(ui, colors, |ui| {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(crate::i18n::tr("歌词行"))
                    .size(14.0)
                    .strong()
                    .color(colors.text),
            );
            widgets::chip(
                ui,
                &crate::trf!("{} 行", editor.lines.len()),
                colors.text_faint,
                colors.card_hover,
            );
            if editor.out_of_order() {
                ui.label(
                    RichText::new(crate::i18n::tr("时间顺序有误"))
                        .size(10.5)
                        .color(colors.danger),
                );
            }
        });
        ui.add_space(4.0);
        let row_height = 30.0;
        // The card owns the full page height. Reserve the footer and the item
        // spacing, then let the list take exactly what is left: a height
        // derived from the window instead of the card's real content pushed
        // the last rows and the buttons below the visible area. The narrow
        // layout scrolls the page, so its content height is unbounded and the
        // caller supplies an explicit list height instead.
        let footer_height = 34.0;
        let list_height = match list_height_override {
            Some(height) => height.max(120.0),
            None => {
                (ui.available_height() - footer_height - ui.spacing().item_spacing.y).max(120.0)
            }
        };
        let mut select = None;
        egui::ScrollArea::vertical()
            .id_salt("bamboo-lyrics-editor-lines")
            .max_height(list_height)
            .auto_shrink([false, false])
            .show_rows(ui, row_height, editor.lines.len(), |ui, range| {
                for index in range {
                    let line = &editor.lines[index];
                    let previous = index
                        .checked_sub(1)
                        .and_then(|prev| editor.lines.get(prev))
                        .map(|line| line.start_ms);
                    let bad_order = previous.is_some_and(|prev| prev > line.start_ms);
                    let time = if editor.timed {
                        format_tag(line.start_ms)
                    } else {
                        "--:--.--".to_string()
                    };
                    let text = if line.text.trim().is_empty() {
                        crate::i18n::tr("(空行)").to_string()
                    } else {
                        crate::util::truncate_chars(&line.text, 90)
                    };
                    let label = format!("{index:>3}  {time}  {text}");
                    let color = if bad_order {
                        colors.danger
                    } else if line.text.trim().is_empty() {
                        colors.text_faint
                    } else {
                        colors.text
                    };
                    let selected = editor.selected == index;
                    // The whole strip is the click target and highlight, not
                    // just the glyphs: selecting a line in a long list is much
                    // easier when the full row reacts.
                    let row_width = ui.available_width();
                    let (rect, response) =
                        ui.allocate_exact_size(vec2(row_width, row_height), egui::Sense::click());
                    response.widget_info(|| {
                        egui::WidgetInfo::selected(
                            egui::WidgetType::SelectableLabel,
                            true,
                            selected,
                            &label,
                        )
                    });
                    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
                    let hovered = response.hovered() && !selected;
                    if selected || hovered {
                        ui.painter().rect_filled(
                            rect.shrink2(vec2(2.0, 1.0)),
                            egui::CornerRadius::same(5),
                            if selected {
                                colors.accent_soft
                            } else {
                                colors.card_hover
                            },
                        );
                    }
                    if response.has_focus() {
                        ui.painter().rect_stroke(
                            rect.shrink2(vec2(2.0, 1.0)),
                            egui::CornerRadius::same(5),
                            egui::Stroke::new(1.5, colors.accent),
                            egui::StrokeKind::Inside,
                        );
                    }
                    paint_elided(
                        ui,
                        rect.left_center() + vec2(8.0, 0.0),
                        &label,
                        egui::FontId::proportional(12.5),
                        color,
                        (row_width - 16.0).max(40.0),
                    );
                    if response.clicked() {
                        select = Some(index);
                    }
                    if response.double_clicked() {
                        select = Some(index);
                        *action = Action::Seek(line.start_ms as f64 / 1000.0, true);
                    }
                }
            });
        if let Some(index) = select {
            editor.select(index);
        }
        ui.horizontal(|ui| {
            if widgets::secondary_button(ui, crate::i18n::tr("添加行"), colors).clicked() {
                editor.add_after_selected(position);
            }
            if widgets::secondary_button(ui, crate::i18n::tr("删除行"), colors).clicked() {
                editor.remove_selected();
            }
            if widgets::secondary_button(ui, crate::i18n::tr("上移"), colors).clicked() {
                editor.move_selected(true);
            }
            if widgets::secondary_button(ui, crate::i18n::tr("下移"), colors).clicked() {
                editor.move_selected(false);
            }
        });
    });
}

fn translation_region_label(region: &str) -> String {
    match region {
        "zh" => crate::i18n::tr("中文 (zh)").to_string(),
        "en" => "English (en)".to_string(),
        "jp" => crate::i18n::tr("日本語 (jp)").to_string(),
        other => other.to_string(),
    }
}

fn paint_elided(
    ui: &Ui,
    left_center: egui::Pos2,
    text: &str,
    font: egui::FontId,
    color: egui::Color32,
    max_width: f32,
) {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_owned(), font, color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(max_width);
    let galley = ui.painter().layout_job(job);
    let top_left = egui::pos2(left_center.x, left_center.y - galley.size().y * 0.5);
    ui.painter().galley(top_left, galley, color);
}

fn details(
    editor: &mut LyricsEditor,
    ui: &mut Ui,
    height: f32,
    position: f64,
    colors: &crate::app::theme::ThemeColors,
    action: &mut Action,
) {
    widgets::card(ui, colors, |ui| {
        ui.set_min_height((height - 40.0).max(120.0));
        let Some(line) = editor.lines.get(editor.selected) else {
            ui.label(RichText::new(crate::i18n::tr("没有可编辑的歌词行")).color(colors.text_weak));
            return;
        };
        let line_start = line.start_ms;
        let index = editor.selected + 1;
        let count = editor.lines.len();
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(crate::trf!("第 {index} / {count} 行", index, count))
                    .size(14.0)
                    .strong()
                    .color(colors.text),
            );
            ui.label(
                RichText::new(crate::trf!("播放位置 {}", format_time(position)))
                    .size(10.5)
                    .color(colors.text_faint),
            );
        });
        ui.add_space(8.0);

        ui.horizontal(|ui| {
            ui.label(
                RichText::new(crate::i18n::tr("时间"))
                    .size(12.0)
                    .color(colors.text_weak),
            );
            let response = ui.add(
                egui::TextEdit::singleline(&mut editor.time_text)
                    .desired_width(96.0)
                    .char_limit(14)
                    .hint_text("mm:ss.xx"),
            );
            let mut invalid = false;
            if response.changed() && !editor.apply_time_text() {
                invalid = true;
            }
            if response.lost_focus() && !editor.time_text.trim().is_empty() {
                if parse_tag(&editor.time_text).is_none() {
                    editor.sync_buffers();
                }
            }
            if widgets::secondary_button(ui, crate::i18n::tr("打点"), colors).clicked() {
                editor.stamp(position);
            }
            if widgets::secondary_button(ui, crate::i18n::tr("跳到此句"), colors).clicked() {
                *action = Action::Seek(line_start as f64 / 1000.0, false);
            }
            if widgets::secondary_button(ui, crate::i18n::tr("从此播放"), colors).clicked() {
                *action = Action::Seek(line_start as f64 / 1000.0, true);
            }
            if invalid {
                ui.label(
                    RichText::new(crate::i18n::tr("格式无效"))
                        .size(10.5)
                        .color(colors.danger),
                );
            }
        });
        ui.add_space(6.0);

        ui.label(
            RichText::new(crate::i18n::tr("歌词文本"))
                .size(12.0)
                .color(colors.text_weak),
        );
        let response = ui.add(
            egui::TextEdit::singleline(&mut editor.text_text)
                .desired_width(f32::INFINITY)
                .hint_text(crate::i18n::tr("输入这一句歌词")),
        );
        if response.changed() {
            editor.apply_text_text();
        }
        if let Some(line) = editor.lines.get(editor.selected)
            && !line.words.is_empty()
        {
            let preserved = line.words_match_text();
            ui.add_space(4.0);
            ui.label(
                RichText::new(if preserved {
                    crate::trf!("逐字时间：{} 段（保存时保留）", line.words.len())
                } else {
                    crate::i18n::tr("歌词文本已修改，保存时将不再写入逐字时间").to_string()
                })
                .size(10.5)
                .color(if preserved {
                    colors.text_faint
                } else {
                    colors.warning
                }),
            );
        }
        ui.add_space(6.0);
        ui.label(
            RichText::new(crate::i18n::tr("翻译"))
                .size(12.0)
                .color(colors.text_weak),
        );
        let response = ui.add(
            egui::TextEdit::singleline(&mut editor.translation_text)
                .desired_width(f32::INFINITY)
                .hint_text(crate::i18n::tr("这一句的译文（可选）")),
        );
        if response.changed() {
            editor.apply_translation_text();
        }
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(crate::i18n::tr("译文语言"))
                    .size(11.0)
                    .color(colors.text_faint),
            );
            let mut region = editor.translation_region.clone();
            egui::ComboBox::from_id_salt("bamboo-editor-translation-region")
                .selected_text(translation_region_label(&region))
                .show_ui(ui, |ui| {
                    for option in ["zh", "en", "jp"] {
                        ui.selectable_value(
                            &mut region,
                            option.to_string(),
                            translation_region_label(option),
                        );
                    }
                });
            if region != editor.translation_region {
                editor.translation_region = region;
                // The old sidecar is left untouched; the next save writes the
                // translation under the newly selected region.
                editor.translation_path = None;
            }
            ui.label(
                RichText::new(if editor.has_translations() {
                    crate::trf!(
                        "保存为 {}",
                        editor
                            .translation_sidecar_path(
                                editor
                                    .default_save_path()
                                    .as_deref()
                                    .unwrap_or_else(|| std::path::Path::new("lyrics.lrc"))
                            )
                            .file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .unwrap_or_default()
                    )
                } else {
                    crate::i18n::tr("译文随原文保存到同名语言文件").to_string()
                })
                .size(10.0)
                .color(colors.text_faint),
            );
        });
        ui.add_space(10.0);
        ui.separator();
        ui.add_space(8.0);

        ui.horizontal(|ui| {
            if widgets::secondary_button(ui, crate::i18n::tr("按时间排序"), colors).clicked() {
                editor.sort_by_time();
            }
            if !editor.timed {
                ui.add(
                    egui::DragValue::new(&mut editor.interval_ms)
                        .range(500..=30_000)
                        .speed(100.0)
                        .suffix(" ms"),
                );
                if widgets::secondary_button(ui, crate::i18n::tr("启用时间轴"), colors).clicked()
                {
                    let interval = editor.interval_ms;
                    editor.enable_timeline(interval);
                }
            }
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(crate::i18n::tr("整体偏移"))
                    .size(12.0)
                    .color(colors.text_weak),
            );
            ui.add(
                egui::DragValue::new(&mut editor.shift_ms)
                    .range(-60_000..=60_000)
                    .speed(50.0)
                    .suffix(" ms"),
            );
            if widgets::secondary_button(ui, crate::i18n::tr("应用"), colors).clicked() {
                let delta = editor.shift_ms;
                editor.shift_all(delta);
            }
            ui.label(
                RichText::new(crate::i18n::tr("正数延后，负数提前"))
                    .size(10.0)
                    .color(colors.text_faint),
            );
        });

        ui.add_space(10.0);
        if let Some(path) = editor.default_save_path() {
            ui.label(
                RichText::new(crate::trf!("保存到 {}", path.display()))
                    .size(10.5)
                    .color(colors.text_faint),
            );
        } else {
            ui.label(
                RichText::new(crate::i18n::tr("在线歌词首次保存时会询问保存位置"))
                    .size(10.5)
                    .color(colors.text_faint),
            );
        }
        if let Some(status) = editor.status.as_ref() {
            ui.add_space(4.0);
            ui.label(RichText::new(status).size(11.0).color(colors.text_weak));
        }
    });
}

fn confirm_dialog(
    ui: &mut Ui,
    editor: &mut LyricsEditor,
    colors: &crate::app::theme::ThemeColors,
    action: &mut Action,
) {
    let mut open = true;
    egui::Window::new(crate::i18n::tr("未保存的修改"))
        .id(egui::Id::new("bamboo-lyrics-editor-confirm"))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, vec2(0.0, 0.0))
        .open(&mut open)
        .show(ui.ctx(), |ui| {
            ui.label(
                RichText::new(crate::trf!("「{}」还有未保存的修改。", editor.track_label))
                    .size(12.5)
                    .color(colors.text),
            );
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if widgets::primary_button(ui, crate::i18n::tr("保存并关闭"), colors).clicked()
                {
                    *action = Action::SaveAndClose;
                }
                if widgets::secondary_button(ui, crate::i18n::tr("放弃修改"), colors).clicked()
                {
                    editor.dirty = false;
                    *action = Action::Close;
                }
                if widgets::secondary_button(ui, crate::i18n::tr("取消"), colors).clicked() {
                    editor.confirm_close = false;
                }
            });
        });
    if !open {
        editor.confirm_close = false;
    }
}
