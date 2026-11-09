//! Lyric rendering shared by the now-playing card, the persistent sidebar and
//! the immersive full-screen view.

use std::time::{Duration, Instant};

use egui::{Align, Color32, CornerRadius, Layout, RichText, Sense, Stroke, Ui, vec2};

use crate::app::theme::ThemeColors;
use crate::app::{App, widgets};
use crate::config::LyricsAlign;
use crate::lyrics::Lyrics;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LyricsAreaKind {
    Panel,
    Sidebar,
    Fullscreen,
}

#[derive(Clone)]
struct LineStyle {
    font_size: f32,
    font_family: egui::FontFamily,
    line_spacing: f32,
    align: Align,
    show_translation: bool,
    current_color: Color32,
    normal_color: Color32,
    translation_color: Color32,
}

pub fn lyrics_area(app: &mut App, ui: &mut Ui, height: f32, kind: LyricsAreaKind) {
    let colors = app.colors();
    let settings = app.settings.lyrics.clone();
    let large = app.settings.large_lyrics.clone();
    let scale = match kind {
        LyricsAreaKind::Sidebar => 0.86,
        LyricsAreaKind::Fullscreen => 1.0,
        LyricsAreaKind::Panel => 1.0,
    };
    let fullscreen = kind == LyricsAreaKind::Fullscreen;
    let (
        font_size,
        line_spacing,
        align,
        show_translation,
        current_color,
        normal_color,
        translation_color,
    ) = if fullscreen {
        (
            large.font_size,
            large.line_spacing,
            large.align,
            large.show_translation,
            large.current_color,
            large.normal_color,
            large.translation_color,
        )
    } else {
        (
            settings.font_size * scale,
            settings.line_spacing * scale,
            settings.align,
            settings.show_translation,
            settings.current_color,
            settings.normal_color,
            settings.translation_color,
        )
    };
    let karaoke = if fullscreen {
        large.karaoke
    } else {
        settings.karaoke
    };
    let karaoke_words_only = if fullscreen {
        large.karaoke_words_only
    } else {
        settings.karaoke_words_only
    };
    let style = LineStyle {
        font_size: font_size.clamp(11.0, 96.0),
        font_family: if fullscreen {
            crate::app::fonts::large_lyrics_font_family()
        } else {
            egui::FontFamily::Proportional
        },
        line_spacing: line_spacing.clamp(0.0, 80.0),
        align: match align {
            LyricsAlign::Left => Align::LEFT,
            LyricsAlign::Center => Align::Center,
            LyricsAlign::Right => Align::RIGHT,
        },
        show_translation,
        current_color: Color32::from_rgb(current_color[0], current_color[1], current_color[2]),
        normal_color: Color32::from_rgb(normal_color[0], normal_color[1], normal_color[2]),
        translation_color: Color32::from_rgb(
            translation_color[0],
            translation_color[1],
            translation_color[2],
        ),
    };

    if app.current_track.is_none() {
        lyrics_empty(
            app,
            ui,
            height,
            kind,
            &colors,
            crate::i18n::tr("播放歌曲后显示歌词"),
            "",
        );
        return;
    }

    if app.lyrics_loading && app.lyrics.is_none() {
        let width = ui.available_width().max(80.0);
        ui.allocate_ui_with_layout(vec2(width, height), Layout::top_down(Align::Center), |ui| {
            ui.set_min_size(vec2(width, height));
            ui.add_space((height * 0.30).max(12.0));
            ui.spinner();
            ui.add_space(6.0);
            ui.label(
                RichText::new(crate::i18n::tr("正在查找歌词…"))
                    .size(12.0)
                    .color(colors.text_weak),
            );
        });
        return;
    }

    if app.lyrics.is_none() {
        let hint = if app.settings.lyrics.auto_load {
            app.lyrics_error.clone().unwrap_or_else(|| {
                crate::i18n::tr("没有找到歌词，可手动选择 .lrc/.srt").to_string()
            })
        } else {
            crate::i18n::tr("自动查找歌词已关闭").to_string()
        };
        lyrics_empty(app, ui, height, kind, &colors, &hint, "");
        return;
    }

    if app
        .lyrics
        .as_ref()
        .map(|lyrics| lyrics.lines.is_empty())
        .unwrap_or(true)
    {
        let message = app
            .lyrics_error
            .clone()
            .unwrap_or_else(|| crate::i18n::tr("歌词文件为空或格式无法识别").to_string());
        lyrics_empty(app, ui, height, kind, &colors, &message, "");
        return;
    }

    let lyrics = app.lyrics.as_ref().expect("lyrics checked above");

    let position_ms = (app.position_secs() * 1000.0).max(0.0) as u64;
    let user_offset = settings.offset_ms;
    let effective_ms = if user_offset >= 0 {
        position_ms.saturating_sub(user_offset as u64)
    } else {
        position_ms.saturating_add((-user_offset) as u64)
    };
    let active = lyrics.index_at(effective_ms);
    let now = Instant::now();
    let base_follow = app
        .lyrics_manual_scroll_until
        .map(|until| now >= until)
        .unwrap_or(true);
    let pointer_over = ui.rect_contains_pointer(ui.max_rect());
    let wheel = ui
        .ctx()
        .input(|input| input.smooth_scroll_delta.y.abs() > 0.1);
    let manual_scroll = pointer_over && wheel && lyrics.timed;
    let follow = base_follow && !manual_scroll && lyrics.timed;
    let animate = settings.scroll_animation
        && app.settings.theme.animations
        && !app.settings.theme.reduce_motion;
    let scroll_animation = if animate {
        egui::style::ScrollAnimation::default()
    } else {
        egui::style::ScrollAnimation::none()
    };

    let top_space = (height * 0.45).max(20.0);
    let bottom_space = (height * 0.55).max(40.0);

    let width = ui.available_width().max(80.0);
    let area_id = match kind {
        LyricsAreaKind::Panel => "bamboo-lyrics-panel",
        LyricsAreaKind::Sidebar => "bamboo-lyrics-sidebar",
        LyricsAreaKind::Fullscreen => "bamboo-lyrics-fullscreen",
    };
    let mut clicked = None;
    let follow_id = egui::Id::new(("bamboo-lyrics-follow", area_id));
    let last_follow: Option<usize> = ui
        .ctx()
        .data_mut(|data| data.get_persisted::<usize>(follow_id));
    let scroll = egui::ScrollArea::vertical()
        .id_salt(area_id)
        .auto_shrink([false, false])
        .max_height(height);
    ui.allocate_ui_with_layout(vec2(width, height), Layout::top_down(Align::LEFT), |ui| {
        ui.set_min_size(vec2(width, height));
        scroll.show(ui, |ui| {
            // Use the viewport width the scroll area actually gives us: forcing
            // the outer width here makes the content wider than the viewport by
            // the scrollbar gutter, which in turn widens the whole lyrics card
            // past the hero card above it.
            let width = ui.available_width();
            ui.set_width(width);
            ui.add_space(top_space);
            for (index, line) in lyrics.lines.iter().enumerate() {
                let is_active = active == Some(index);
                let row = ui.allocate_ui_with_layout(
                    vec2(width, 0.0),
                    Layout::top_down(style.align),
                    |ui| {
                        ui.set_width(width);
                        if is_active
                            && karaoke
                            && lyrics.timed
                            && (!karaoke_words_only || !line.words.is_empty())
                        {
                            let line_end = line_end_ms(line, index, lyrics);
                            let font = egui::FontId::new(
                                style.font_size * 1.12,
                                style.font_family.clone(),
                            );
                            let progress =
                                crate::lyrics::karaoke_progress(line, effective_ms, line_end);
                            let mut job = egui::text::LayoutJob::default();
                            job.wrap = egui::text::TextWrapping::wrap_at_width(width);
                            job.halign = style.align;
                            job.append(
                                &line.text,
                                0.0,
                                egui::text::TextFormat {
                                    font_id: font.clone(),
                                    color: style.normal_color,
                                    ..Default::default()
                                },
                            );
                            let galley = ui.painter().layout_job(job);
                            let (rect, _) = ui.allocate_exact_size(
                                vec2(width, galley.size().y),
                                Sense::hover(),
                            );
                            let anchor = match style.align {
                                Align::Min => rect.left(),
                                Align::Max => rect.right(),
                                _ => rect.center().x,
                            };
                            let origin = egui::pos2(anchor, rect.top());
                            // Paint the line in the inactive colour, then
                            // overlay the sung prefix. The helper clips each
                            // wrapped row at the exact character fraction, so
                            // multi-row lines get the same continuous fill as
                            // single-row ones.
                            ui.painter()
                                .galley(origin, galley.clone(), style.normal_color);
                            super::paint_karaoke_fill(
                                ui.painter(),
                                origin,
                                &galley,
                                &progress,
                                style.current_color,
                            );
                        } else {
                            let main = line_text(line, is_active, &style);
                            ui.add(
                                egui::Label::new(main)
                                    .wrap()
                                    .selectable(false)
                                    .sense(Sense::hover()),
                            );
                        }
                        if style.show_translation {
                            if let Some(translation) = &line.translation {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(translation)
                                            .size(style.font_size * 0.78)
                                            .font(egui::FontId::new(
                                                style.font_size * 0.78,
                                                style.font_family.clone(),
                                            ))
                                            .color(style.translation_color),
                                    )
                                    .wrap()
                                    .selectable(false),
                                );
                            }
                        }
                    },
                );
                let rect = row.response.rect;
                let response = ui
                    .interact(
                        rect,
                        ui.id().with(("lyric-row", index)),
                        if lyrics.timed {
                            Sense::click()
                        } else {
                            Sense::hover()
                        },
                    )
                    .on_hover_cursor(if lyrics.timed {
                        egui::CursorIcon::PointingHand
                    } else {
                        egui::CursorIcon::Default
                    });
                if lyrics.timed {
                    let mut row_label = line.text.clone();
                    if style.show_translation
                        && let Some(translation) = &line.translation
                        && !translation.trim().is_empty()
                    {
                        row_label.push(' ');
                        row_label.push_str(translation);
                    }
                    response.widget_info(|| {
                        egui::WidgetInfo::selected(
                            egui::WidgetType::SelectableLabel,
                            true,
                            is_active,
                            &row_label,
                        )
                    });
                    if response.has_focus() {
                        ui.painter().rect_stroke(
                            rect.shrink(1.0),
                            CornerRadius::same(4),
                            Stroke::new(1.5, colors.accent),
                            egui::StrokeKind::Inside,
                        );
                    }
                }
                // Starting a scroll animation on every frame keeps egui in a
                // permanent 60 fps repaint loop (and a permanently high GPU
                // load). Follow only when the active line actually changes.
                if is_active
                    && follow
                    && !manual_scroll
                    && last_follow != Some(index)
                {
                    ui.scroll_to_rect_animation(rect, Some(Align::Center), scroll_animation);
                }
                if is_active && style.align == Align::LEFT {
                    ui.painter().rect_filled(
                        egui::Rect::from_min_size(
                            egui::pos2(rect.left(), rect.top() + 3.0),
                            vec2(3.0, (rect.height() - 6.0).max(4.0)),
                        ),
                        CornerRadius::same(2),
                        style.current_color,
                    );
                }
                if response.clicked() && lyrics.timed {
                    clicked = Some(line.start_ms);
                }
                ui.add_space(style.line_spacing);
            }
            ui.add_space(bottom_space);
        });
    });

    let source_line = if settings.show_source {
        lyrics_source_line(lyrics, &settings)
    } else {
        None
    };

    if let Some(source) = source_line {
        ui.add_space(4.0);
        ui.label(RichText::new(source).size(10.0).color(colors.text_faint));
    }
    if manual_scroll {
        app.lyrics_manual_scroll_until = Some(now + Duration::from_secs(4));
        ui.ctx()
            .data_mut(|data| data.insert_persisted(follow_id, usize::MAX));
    } else if follow && let Some(index) = active {
        ui.ctx()
            .data_mut(|data| data.insert_persisted(follow_id, index));
    }
    if let Some(start_ms) = clicked {
        app.lyrics_manual_scroll_until = None;
        app.seek_to(start_ms as f64 / 1000.0);
    }
}

fn lyrics_source_line(lyrics: &Lyrics, settings: &crate::config::LyricsSettings) -> Option<String> {
    let mut parts = Vec::new();
    let source = lyrics.source.label();
    if !source.is_empty() {
        parts.push(source.to_string());
    }
    if let Some(encoding) = &lyrics.encoding {
        parts.push(encoding.clone());
    }
    if !lyrics.timed {
        parts.push(crate::i18n::tr("无时间轴").to_string());
    }
    if settings.offset_ms != 0 {
        parts.push(crate::trf!("偏移 {} ms", settings.offset_ms));
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

fn line_text<'a>(line: &'a crate::lyrics::LyricLine, active: bool, style: &LineStyle) -> RichText {
    let size = if active {
        style.font_size * 1.12
    } else {
        style.font_size
    };
    let color = if active {
        style.current_color
    } else {
        style.normal_color
    };
    let mut text = RichText::new(&line.text)
        .size(size)
        .font(egui::FontId::new(size, style.font_family.clone()))
        .color(color);
    if active {
        text = text.strong();
    }
    text
}

/// End of a line for karaoke timing: the parsed end, the next line's start, or
/// a five-second fallback for the final line.
fn line_end_ms(line: &crate::lyrics::LyricLine, index: usize, lyrics: &Lyrics) -> u64 {
    line.end_ms
        .or_else(|| lyrics.lines.get(index + 1).map(|next| next.start_ms))
        .unwrap_or(line.start_ms.saturating_add(5000))
}

fn lyrics_empty(
    app: &mut App,
    ui: &mut Ui,
    height: f32,
    kind: LyricsAreaKind,
    colors: &ThemeColors,
    message: &str,
    detail: &str,
) {
    let width = ui.available_width().max(80.0);
    ui.allocate_ui_with_layout(vec2(width, height), Layout::top_down(Align::Center), |ui| {
        ui.set_min_size(vec2(width, height));
        ui.add_space((height * 0.24).max(12.0));
        ui.label(
            RichText::new(crate::i18n::tr("词"))
                .size(34.0)
                .color(colors.text_faint),
        );
        ui.add_space(6.0);
        ui.label(
            RichText::new(message)
                .size(if kind == LyricsAreaKind::Fullscreen {
                    16.0
                } else {
                    12.5
                })
                .color(colors.text_weak),
        );
        if !detail.is_empty() {
            ui.label(RichText::new(detail).size(10.5).color(colors.text_faint));
        }
        if kind != LyricsAreaKind::Sidebar {
            ui.add_space(12.0);
            let font = egui::TextStyle::Button.resolve(ui.style());
            let button_text = [crate::i18n::tr("选择歌词文件"), crate::i18n::tr("重新加载")];
            let text_width: f32 = button_text
                .iter()
                .map(|text| {
                    ui.painter()
                        .layout_no_wrap((*text).to_owned(), font.clone(), Color32::WHITE)
                        .size()
                        .x
                })
                .sum();
            let button_padding = ui.spacing().button_padding.x * 2.0;
            let gap = ui.spacing().item_spacing.x;
            let row_width = (text_width + button_padding * 2.0 + gap).min(ui.available_width());
            let left = ((ui.available_width() - row_width) * 0.5).max(0.0);
            ui.horizontal(|ui| {
                ui.add_space(left);
                if widgets::secondary_button(ui, crate::i18n::tr("选择歌词文件"), colors).clicked()
                {
                    app.pick_lyrics_file();
                }
                if widgets::secondary_button(ui, crate::i18n::tr("重新加载"), colors).clicked()
                {
                    app.reload_lyrics();
                }
            });
        }
    });
}

/// Immersive full-screen lyrics: a large right-hand lyric column (the focus),
/// with cover/title on the left. The lyric area intentionally uses most of the
/// available width and height, matching the requested reference layout.
pub fn fullscreen_lyrics(app: &mut App, ui: &mut Ui) {
    let colors = app.colors();
    let large = app.settings.large_lyrics.clone();
    let full = ui.max_rect();
    let pad = 28.0;
    let header_h = 50.0;

    let header_rect = egui::Rect::from_min_size(
        egui::pos2(full.left() + pad, full.top() + pad * 0.6),
        vec2(full.width() - pad * 2.0, header_h),
    );
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(header_rect)
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            if widgets::icon_button(
                ui,
                egui_phosphor::regular::CARET_DOWN,
                20.0,
                crate::i18n::tr("返回 (Esc)"),
                false,
                true,
                &colors,
            )
            .clicked()
            {
                app.toggle_fullscreen_lyrics();
            }
            ui.add_space(6.0);
            ui.label(
                RichText::new(crate::i18n::tr("歌词"))
                    .size(13.0)
                    .color(colors.text_weak),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let icon = if app.lyrics_os_fullscreen {
                    egui_phosphor::regular::CORNERS_IN
                } else {
                    egui_phosphor::regular::CORNERS_OUT
                };
                if widgets::icon_button(
                    ui,
                    icon,
                    18.0,
                    if app.lyrics_os_fullscreen {
                        crate::i18n::tr("退出全屏 (F11)")
                    } else {
                        crate::i18n::tr("全屏 (F11)")
                    },
                    app.lyrics_os_fullscreen,
                    true,
                    &colors,
                )
                .clicked()
                {
                    app.toggle_lyrics_os_fullscreen();
                }
                if app.settings.features.fullscreen_lyrics
                    && widgets::secondary_button(ui, crate::i18n::tr("独立窗口"), &colors).clicked()
                {
                    app.open_lyrics_window();
                }
                if app.settings.features.lyric_editor
                    && widgets::secondary_button(ui, crate::i18n::tr("编辑歌词"), &colors).clicked()
                {
                    app.open_lyrics_editor();
                }
                ui.label(
                    RichText::new(if app.lyrics_os_fullscreen {
                        crate::i18n::tr("Esc 退出全屏")
                    } else {
                        crate::i18n::tr("Esc 返回")
                    })
                    .size(11.0)
                    .color(colors.text_faint),
                );
            });
        },
    );

    let content_top = header_rect.bottom() + 12.0;
    let content_bottom = full.bottom() - pad;
    let content_h = (content_bottom - content_top).max(120.0);
    let wide = large.show_cover && full.width() >= 980.0;
    let left_w = if wide {
        (full.width() * 0.32).clamp(260.0, 500.0)
    } else {
        0.0
    };

    if wide {
        let center = egui::pos2(
            full.left() + pad + left_w * 0.5,
            content_top + content_h * 0.46,
        );
        let side = (left_w.min(content_h * 0.72)).max(180.0);
        ui.painter()
            .circle_filled(center, side * 0.58, Color32::from_rgb(12, 15, 22));
        ui.painter()
            .circle_stroke(center, side * 0.58, Stroke::new(1.0, colors.line));
        ui.painter()
            .circle_stroke(center, side * 0.20, Stroke::new(1.0, colors.line));
        if let Some(cover) = app.cover.clone() {
            let cover_rect = egui::Rect::from_center_size(center, vec2(side * 0.70, side * 0.70));
            ui.painter().image(
                cover.id(),
                cover_rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        }
    }

    let right_x = if wide {
        full.left() + pad + left_w + 34.0
    } else {
        full.left() + pad
    };
    let right_w = (full.right() - pad - right_x).max(200.0);
    let right_rect =
        egui::Rect::from_min_size(egui::pos2(right_x, content_top), vec2(right_w, content_h));

    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(right_rect)
            .layout(Layout::top_down(Align::Center)),
        |ui| {
            ui.set_width(right_w);
            if large.show_title {
                if let Some(track) = app.current_track.clone() {
                    ui.label(
                        RichText::new(track.display_title())
                            .size(30.0)
                            .strong()
                            .color(colors.text),
                    );
                    ui.add_space(4.0);
                    let mut subtitle = track.display_artist();
                    if !track.album.is_empty() {
                        subtitle.push_str(" · ");
                        subtitle.push_str(&track.album);
                    }
                    ui.label(RichText::new(subtitle).size(14.0).color(colors.text_weak));
                    if let Some(lyrics) = app.lyrics.as_ref() {
                        if !lyrics
                            .path
                            .as_ref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_default()
                            .is_empty()
                        {
                            if let Some(path) = &lyrics.path {
                                ui.label(
                                    RichText::new(crate::util::truncate_chars(
                                        &path.display().to_string(),
                                        110,
                                    ))
                                    .size(10.5)
                                    .color(colors.text_faint),
                                );
                            }
                        }
                    }
                } else {
                    ui.label(
                        RichText::new(crate::i18n::tr("未在播放"))
                            .size(26.0)
                            .color(colors.text_weak),
                    );
                }
            }
            ui.add_space(6.0);
            widgets::chip(
                ui,
                crate::i18n::tr("歌词"),
                colors.accent,
                colors.accent_soft,
            );
            ui.add_space(8.0);
            let lyrics_height = ui.available_height().max(120.0);
            lyrics_area(app, ui, lyrics_height, LyricsAreaKind::Fullscreen);
        },
    );
}
