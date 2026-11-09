//! Online search, album browsing and the discover placeholder.

use egui::{Align2, Color32, CornerRadius, FontId, RichText, Sense, Stroke, Ui, vec2};

use crate::api::{ApiSong, SearchKind};
use crate::app::{App, widgets};
use crate::config::Quality;

pub fn search(app: &mut App, ui: &mut Ui) {
    let colors = app.colors();
    widgets::page_header(
        ui,
        app.t("search.title"),
        app.t("search.subtitle"),
        &colors,
        |ui| {
            if widgets::secondary_button(ui, app.t("search.test_connection"), &colors).clicked() {
                if let Some(source) = app.search_source_config() {
                    app.test_source(&source.id);
                }
            }
            if widgets::secondary_button(ui, crate::i18n::tr("打开设置"), &colors).clicked() {
                app.set_page(crate::model::Page::Settings);
            }
        },
    );

    if let Some(view) = app.artist_view.clone() {
        artist_view(app, ui, &view);
        return;
    }
    if let Some(view) = app.album_view.clone() {
        album_view(app, ui, &view);
        return;
    }

    search_controls(app, ui);
    ui.add_space(14.0);

    if app.search_loading {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.add_space(8.0);
            ui.label(
                RichText::new(crate::i18n::tr("正在搜索…"))
                    .size(13.0)
                    .color(colors.text_weak),
            );
        });
        return;
    }
    if let Some(err) = app.search_error.clone() {
        error_card(ui, &err, &colors);
        return;
    }
    if app.search_results.is_empty() {
        widgets::empty_state(
            ui,
            egui_phosphor::regular::MAGNIFYING_GLASS,
            if app.search_query.trim().is_empty() {
                app.t("search.empty")
            } else {
                crate::i18n::tr("没有找到匹配结果")
            },
            crate::i18n::tr("支持 Bamboo 本地曲库及兼容 Bamboo 协议的自建在线音源"),
            &colors,
            |ui| {
                if widgets::secondary_button(ui, app.t("search.test_connection"), &colors).clicked()
                {
                    if let Some(source) = app.search_source_config() {
                        app.test_source(&source.id);
                    }
                }
            },
        );
        return;
    }

    let header_songs = app.search_results.clone();
    let total = app.search_total;
    results_header(app, ui, &header_songs, crate::i18n::tr("首"), total);
    ui.add_space(6.0);
    let songs = app.search_results.clone();
    list_songs(app, ui, &songs, app.search_kind == SearchKind::Album);
    pagination(
        app,
        ui,
        app.search_page,
        app.search_total,
        30,
        |app, page| app.search_to_page(page),
    );
}

fn search_controls(app: &mut App, ui: &mut Ui) {
    let colors = app.colors();
    egui::Frame::new()
        .fill(colors.card)
        .stroke(Stroke::new(1.0, colors.line))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(egui::Margin::symmetric(16, 14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                let sources: Vec<(String, String, bool)> = app
                    .settings
                    .sources
                    .iter()
                    .map(|source| (source.id.clone(), source.name.clone(), source.enabled))
                    .collect();
                egui::ComboBox::from_id_salt("search-source")
                    .selected_text(
                        sources
                            .iter()
                            .find(|(id, _, _)| id == &app.search_source)
                            .map(|(_, name, _)| name.clone())
                            .unwrap_or_else(|| crate::i18n::tr("选择音源").to_string()),
                    )
                    .width(140.0)
                    .show_ui(ui, |ui| {
                        for (id, name, enabled) in &sources {
                            let label = if *enabled {
                                name.clone()
                            } else {
                                crate::trf!("{name}（未启用）", name)
                            };
                            if ui
                                .selectable_value(&mut app.search_source, id.clone(), label)
                                .changed()
                            {
                                app.search_results.clear();
                                app.search_total = 0;
                                app.search_error = None;
                                app.album_view = None;
                                app.clear_search_suggestions();
                            }
                        }
                    });

                let mode_song = app.search_kind == SearchKind::Song;
                if ui
                    .selectable_label(mode_song, RichText::new(crate::i18n::tr("歌曲")).size(12.5))
                    .clicked()
                    && !mode_song
                {
                    app.search_kind = SearchKind::Song;
                    app.search_results.clear();
                    app.search_total = 0;
                    app.clear_search_suggestions();
                }
                if ui
                    .selectable_label(
                        !mode_song,
                        RichText::new(crate::i18n::tr("专辑")).size(12.5),
                    )
                    .clicked()
                    && mode_song
                {
                    app.search_kind = SearchKind::Album;
                    app.search_results.clear();
                    app.search_total = 0;
                    app.clear_search_suggestions();
                }
            });
            ui.add_space(8.0);
            let placeholder = app.t("search.placeholder").to_string();
            let mut query_changed = false;
            let mut entered = false;
            ui.horizontal(|ui| {
                let button_w = 84.0;
                let width = (ui.available_width() - button_w - 8.0).max(160.0);
                let response = ui.add_sized(
                    [width, 34.0],
                    egui::TextEdit::singleline(&mut app.search_query)
                        .hint_text(placeholder)
                        .margin(egui::Margin::symmetric(10, 8)),
                );
                query_changed = response.changed();
                if response.has_focus() && !app.search_suggestions.is_empty() {
                    let len = app.search_suggestions.len();
                    let down = ui.input(|i| i.key_pressed(egui::Key::ArrowDown));
                    let up = ui.input(|i| i.key_pressed(egui::Key::ArrowUp));
                    if down {
                        app.search_suggest_index = Some(match app.search_suggest_index {
                            Some(index) if index + 1 < len => index + 1,
                            _ => 0,
                        });
                    } else if up {
                        app.search_suggest_index = Some(match app.search_suggest_index {
                            Some(0) | None => len.saturating_sub(1),
                            Some(index) => index - 1,
                        });
                    }
                }
                entered = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if !entered
                    && widgets::primary_button(ui, app.t("search.button"), &colors).clicked()
                {
                    app.submit_search();
                }
            });
            if query_changed {
                app.note_search_input_changed();
            }
            if entered {
                if let Some(suggestion) = app
                    .search_suggest_index
                    .and_then(|index| app.search_suggestions.get(index).cloned())
                {
                    app.search_query = suggestion;
                }
                app.clear_search_suggestions();
                app.submit_search();
            }
            if !app.search_suggestions.is_empty() {
                ui.add_space(4.0);
                let suggestions = app.search_suggestions.clone();
                let mut picked: Option<String> = None;
                egui::Frame::new()
                    .fill(colors.card_hover)
                    .stroke(Stroke::new(1.0, colors.line))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(egui::Margin::symmetric(8, 4))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        for (index, suggestion) in suggestions.into_iter().enumerate() {
                            // A menu-like full-width row: the whole strip is the
                            // click target and the highlight, not just the glyphs.
                            let selected = app.search_suggest_index == Some(index);
                            let row_width = ui.available_width();
                            let (rect, response) =
                                ui.allocate_exact_size(vec2(row_width, 26.0), Sense::click());
                            response.widget_info(|| {
                                egui::WidgetInfo::selected(
                                    egui::WidgetType::SelectableLabel,
                                    true,
                                    selected,
                                    &suggestion,
                                )
                            });
                            if response.has_focus() {
                                ui.painter().rect_stroke(
                                    rect.shrink(2.0),
                                    CornerRadius::same(6),
                                    Stroke::new(1.5, colors.accent),
                                    egui::StrokeKind::Inside,
                                );
                            }
                            let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
                            let hovered = response.hovered();
                            if selected || hovered {
                                ui.painter().rect_filled(
                                    rect.shrink2(vec2(4.0, 1.0)),
                                    CornerRadius::same(6),
                                    if selected {
                                        colors.accent_soft
                                    } else {
                                        colors.card_hover
                                    },
                                );
                            }
                            paint_elided(
                                ui,
                                rect.left_center() + vec2(12.0, 0.0),
                                &suggestion,
                                FontId::proportional(12.5),
                                if selected || hovered {
                                    colors.accent
                                } else {
                                    colors.text
                                },
                                (row_width - 24.0).max(40.0),
                            );
                            if response.clicked() {
                                picked = Some(suggestion);
                            }
                        }
                    });
                if let Some(suggestion) = picked {
                    app.search_query = suggestion;
                    app.clear_search_suggestions();
                    app.submit_search();
                }
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    app.clear_search_suggestions();
                }
            }
            if let Some((message, ok)) = app.source_test_message.clone() {
                ui.add_space(6.0);
                ui.label(RichText::new(message).size(11.0).color(if ok {
                    colors.success
                } else {
                    colors.danger
                }));
            }
            if app
                .search_source_config()
                .map(|source| source.base_url.trim().is_empty())
                .unwrap_or(true)
            {
                ui.add_space(6.0);
                ui.label(
                    RichText::new(crate::i18n::tr(
                        "当前音源没有填写 API 地址；在“设置 → 在线音源”中配置后即可搜索。",
                    ))
                    .size(11.0)
                    .color(colors.warning),
                );
            }
        });
}

fn results_header(app: &mut App, ui: &mut Ui, songs: &[ApiSong], unit: &str, total: u64) {
    let colors = app.colors();
    let label = if total > 0 {
        crate::trf!("找到约 {total} {unit}", total, unit)
    } else {
        crate::trf!("{} {unit}", songs.len(), unit)
    };
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).size(13.0).color(colors.text_weak));
        if app.search_cached {
            widgets::chip(
                ui,
                crate::i18n::tr("缓存结果"),
                colors.warning,
                colors.card_hover,
            );
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if app.settings.features.downloads
                && widgets::secondary_button(ui, crate::i18n::tr("下载本页"), &colors).clicked()
            {
                let songs = songs.to_vec();
                app.download_search_songs(&songs);
            }
            if widgets::secondary_button(ui, crate::i18n::tr("全部加入队列"), &colors).clicked()
            {
                let songs = songs.to_vec();
                app.queue_search_songs(&songs);
            }
            if widgets::primary_button(ui, crate::i18n::tr("播放全部"), &colors).clicked() {
                let songs = songs.to_vec();
                app.play_search_songs(&songs);
            }
        });
    });
}

fn list_songs(app: &mut App, ui: &mut Ui, songs: &[ApiSong], album_mode: bool) {
    let colors = app.colors();
    let mut play: Option<ApiSong> = None;
    let mut queue: Option<ApiSong> = None;
    let mut download: Option<ApiSong> = None;
    let mut download_quality: Option<Quality> = None;
    let mut open_album: Option<(String, String)> = None;
    let mut open_artist: Option<(String, String)> = None;
    for (index, song) in songs.iter().enumerate() {
        let row = song_row(app, ui, index, song, &colors, album_mode);
        let album_row = album_mode && song.artist.trim().is_empty();
        // Album search entries carry their id in `id`, not `albumid`.
        let album_id = song
            .album_id
            .clone()
            .or_else(|| album_row.then(|| song.id.clone()));
        if let Some((id, name)) = row.artist_clicked {
            open_artist = Some((id, name));
        } else if row.clicked || row.play_clicked || (album_row && row.add_clicked) {
            if album_row {
                if let Some(album_id) = album_id.clone() {
                    open_album = Some((album_id, song.name.clone()));
                }
            } else {
                play = Some(song.clone());
            }
        }
        if row.secondary_clicked {
            if let Some(album_id) = album_id.clone() {
                open_album = Some((
                    album_id,
                    if song.album.is_empty() {
                        song.name.clone()
                    } else {
                        song.album.clone()
                    },
                ));
            }
        }
        if row.add_clicked && !album_row {
            queue = Some(song.clone());
        }
        if let Some(quality) = row.download_quality {
            download = Some(song.clone());
            download_quality = Some(quality);
        } else if row.download_clicked {
            download = Some(song.clone());
        }
    }
    if let Some(song) = play {
        app.play_api_song(&song);
    }
    if let Some(song) = queue {
        app.queue_api_song(&song);
    }
    if let Some(song) = download {
        app.start_download_song_with_quality(&song, download_quality);
    }
    if let Some((id, name)) = open_album {
        app.open_album(id, name);
    }
    if let Some((id, name)) = open_artist {
        app.open_artist(id, name);
    }

    // Album search results are playlists, not songs.
    if songs.iter().all(|song| song.artist.trim().is_empty()) {
        ui.add_space(4.0);
        ui.label(
            RichText::new(crate::i18n::tr("提示：点击专辑行或右键可查看曲目"))
                .size(10.5)
                .color(colors.text_faint),
        );
    }
}

struct SongRowResponse {
    clicked: bool,
    secondary_clicked: bool,
    play_clicked: bool,
    add_clicked: bool,
    download_clicked: bool,
    download_quality: Option<Quality>,
    artist_clicked: Option<(String, String)>,
}

fn song_row(
    app: &mut App,
    ui: &mut Ui,
    index: usize,
    song: &ApiSong,
    colors: &crate::app::theme::ThemeColors,
    album_mode: bool,
) -> SongRowResponse {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(vec2(width, 58.0), Sense::click());
    let row_label = if song.artist.trim().is_empty() {
        song.name.clone()
    } else {
        format!("{} · {}", song.name, song.artist)
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
        ui.painter().rect_filled(
            rect.shrink2(vec2(0.0, 1.0)),
            CornerRadius::same(8),
            colors.card_hover,
        );
    }
    let cover_side = 42.0;
    let cover_rect = egui::Rect::from_min_size(
        rect.left_center() + vec2(8.0, -cover_side / 2.0),
        vec2(cover_side, cover_side),
    );
    let thumb = song
        .pic
        .as_ref()
        .and_then(|url| app.thumbnails.get(url).cloned());
    if thumb.is_none()
        && ui.is_rect_visible(rect)
        && let Some(url) = song.pic.as_ref()
    {
        // Only visible rows ask for artwork; a 100-row album page used to
        // enqueue 100 image requests before the first row was even seen.
        app.request_thumbnail(url);
    }
    if let Some(thumb) = thumb {
        ui.painter().image(
            thumb.id(),
            cover_rect,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            Color32::WHITE,
        );
    } else {
        ui.painter()
            .rect_filled(cover_rect, CornerRadius::same(7), colors.bg2);
        ui.painter().text(
            cover_rect.center(),
            Align2::CENTER_CENTER,
            egui_phosphor::regular::MUSIC_NOTES,
            FontId::proportional(18.0),
            colors.text_faint,
        );
    }

    let buttons_w = 96.0;
    let text_left = cover_rect.right() + 12.0;
    let is_album_row = album_mode && song.artist.trim().is_empty();
    let cached = !is_album_row && app.song_cached(song);
    let text_width =
        (rect.right() - buttons_w - text_left - if cached { 24.0 } else { 10.0 }).max(80.0);
    paint_elided(
        ui,
        egui::pos2(text_left, rect.center().y - 9.0),
        &song.name,
        FontId::proportional(13.8),
        colors.text,
        text_width,
    );
    let subtitle = song.subtitle();
    paint_elided(
        ui,
        egui::pos2(text_left, rect.center().y + 10.0),
        &subtitle,
        FontId::proportional(11.3),
        colors.text_weak,
        text_width,
    );

    if cached {
        ui.painter().text(
            egui::pos2(rect.right() - buttons_w - 16.0, rect.center().y),
            Align2::CENTER_CENTER,
            egui_phosphor::regular::CHECK_CIRCLE,
            FontId::proportional(14.0),
            colors.success,
        );
    }

    // The artist prefix of the subtitle opens the artist page. Hit-testing via
    // the row response avoids adding a second widget that would compete with
    // the row's play-on-click behavior.
    let artist_target = if song.artist.trim().is_empty() {
        None
    } else {
        song.artist_id
            .as_ref()
            .filter(|id| !id.trim().is_empty())
            .map(|_| {
                let galley = ui.painter().layout_no_wrap(
                    song.artist.clone(),
                    FontId::proportional(11.3),
                    colors.text_weak,
                );
                let height = galley.size().y;
                let width = galley.size().x.min(text_width);
                egui::Rect::from_min_size(
                    egui::pos2(text_left, rect.center().y + 10.0 - height * 0.5),
                    vec2(width, height),
                )
            })
    };
    let artist_hovered = artist_target
        .is_some_and(|target| response.hover_pos().is_some_and(|pos| target.contains(pos)));
    if artist_hovered {
        paint_elided(
            ui,
            egui::pos2(text_left, rect.center().y + 10.0),
            &song.artist,
            FontId::proportional(11.3),
            colors.accent,
            text_width,
        );
    }
    let artist_clicked = if response.clicked() && artist_hovered {
        song.artist_id
            .as_ref()
            .map(|id| (id.clone(), song.artist.clone()))
    } else {
        None
    };

    let play_rect = egui::Rect::from_center_size(
        egui::pos2(rect.right() - 86.0, rect.center().y),
        vec2(26.0, 26.0),
    );
    let add_rect = egui::Rect::from_center_size(
        egui::pos2(rect.right() - 56.0, rect.center().y),
        vec2(26.0, 26.0),
    );
    let download_rect = egui::Rect::from_center_size(
        egui::pos2(rect.right() - 26.0, rect.center().y),
        vec2(26.0, 26.0),
    );
    let play_resp = ui.interact(play_rect, ui.id().with(("api-play", index)), Sense::click());
    let add_resp = ui.interact(add_rect, ui.id().with(("api-add", index)), Sense::click());
    let download_resp = ui.interact(
        download_rect,
        ui.id().with(("api-download", index)),
        Sense::click(),
    );
    let actions: [(egui::Rect, &str, &egui::Response); 3] = if is_album_row {
        [
            (add_rect, egui_phosphor::regular::CARET_RIGHT, &add_resp),
            (play_rect, "", &play_resp),
            (download_rect, "", &download_resp),
        ]
    } else {
        [
            (play_rect, egui_phosphor::regular::PLAY, &play_resp),
            (add_rect, egui_phosphor::regular::PLUS, &add_resp),
            (
                download_rect,
                egui_phosphor::regular::DOWNLOAD_SIMPLE,
                &download_resp,
            ),
        ]
    };
    for (action_rect, icon, action_response) in actions {
        let hover = action_response.hovered();
        if hover {
            ui.painter()
                .circle_filled(action_rect.center(), 12.0, colors.accent_soft);
        }
        if action_response.has_focus() {
            ui.painter()
                .circle_stroke(action_rect.center(), 12.5, Stroke::new(1.5, colors.accent));
        }
        ui.painter().text(
            action_rect.center(),
            Align2::CENTER_CENTER,
            icon,
            FontId::proportional(15.0),
            if hover {
                colors.accent
            } else {
                colors.text_weak
            },
        );
    }
    let play_resp = play_resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    let add_resp = add_resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    let download_resp = download_resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);

    if !is_album_row {
        play_resp.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, true, crate::i18n::tr("播放"))
        });
        add_resp.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, true, crate::i18n::tr("加入队列"))
        });
        download_resp.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, true, crate::i18n::tr("下载"))
        });
    }

    // Right-click the download icon to pick a quality for this track only.
    let mut download_quality = None;
    if !is_album_row && app.settings.features.downloads {
        download_resp
            .clone()
            .on_hover_text(crate::i18n::tr("下载（右键选择音质）"));
        download_resp.context_menu(|ui| {
            ui.label(
                RichText::new(crate::i18n::tr("下载音质"))
                    .size(11.0)
                    .color(colors.text_faint),
            );
            for quality in [Quality::K128, Quality::K320, Quality::Flac] {
                if ui.button(quality.label()).clicked() {
                    download_quality = Some(quality);
                    ui.close();
                }
            }
        });
    }

    let has_album_target = song.album_id.is_some() || is_album_row;
    let secondary_clicked = response.secondary_clicked() && has_album_target;
    SongRowResponse {
        clicked: response.clicked(),
        secondary_clicked,
        play_clicked: if is_album_row {
            false
        } else {
            play_resp.clicked()
        },
        add_clicked: add_resp.clicked(),
        download_clicked: !is_album_row && download_resp.clicked(),
        download_quality,
        artist_clicked,
    }
}

/// Paints a single line of text that is cut off with `…` when it is wider than
/// `max_width`. A character-count limit is not enough for CJK text: 70 hanzi
/// are several times wider than the space left next to the row actions.
fn paint_elided(
    ui: &Ui,
    center_left: egui::Pos2,
    text: &str,
    font: FontId,
    color: Color32,
    max_width: f32,
) {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_owned(), font, color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(max_width);
    let galley = ui.painter().layout_job(job);
    let top_left = egui::pos2(center_left.x, center_left.y - galley.size().y * 0.5);
    ui.painter().galley(top_left, galley, color);
}

fn pagination(
    app: &mut App,
    ui: &mut Ui,
    page: u32,
    total: u64,
    page_size: u32,
    goto: impl Fn(&mut App, u32),
) {
    let colors = app.colors();
    let has_next = total > (page as u64) * page_size as u64;
    if page <= 1 && !has_next {
        return;
    }
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        if page > 1 && widgets::secondary_button(ui, crate::i18n::tr("上一页"), &colors).clicked()
        {
            goto(app, page - 1);
        }
        ui.label(
            RichText::new(crate::trf!("第 {page} 页", page))
                .size(12.0)
                .color(colors.text_weak),
        );
        if has_next && widgets::secondary_button(ui, crate::i18n::tr("下一页"), &colors).clicked()
        {
            goto(app, page + 1);
        }
    });
}

fn album_view(app: &mut App, ui: &mut Ui, view: &crate::app::AlbumView) {
    let colors = app.colors();
    ui.horizontal(|ui| {
        if widgets::icon_button(
            ui,
            egui_phosphor::regular::CARET_LEFT,
            17.0,
            crate::i18n::tr("返回搜索结果"),
            false,
            true,
            &colors,
        )
        .clicked()
        {
            app.close_album();
        }
        ui.add_space(4.0);
        ui.label(
            RichText::new(if view.name.trim().is_empty() {
                crate::i18n::tr("专辑")
            } else {
                &view.name
            })
            .size(19.0)
            .strong()
            .color(colors.text),
        );
        if view.total > 0 {
            ui.label(
                RichText::new(crate::trf!("约 {} 首", view.total))
                    .size(11.5)
                    .color(colors.text_faint),
            );
        }
        if view.cached {
            widgets::chip(
                ui,
                crate::i18n::tr("缓存结果"),
                colors.warning,
                colors.card_hover,
            );
        }
    });
    ui.add_space(10.0);

    if view.loading {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.add_space(8.0);
            ui.label(
                RichText::new(crate::i18n::tr("正在加载专辑…"))
                    .size(13.0)
                    .color(colors.text_weak),
            );
        });
        return;
    }
    if let Some(err) = view.error.clone() {
        error_card(ui, &err, &colors);
        return;
    }
    if view.songs.is_empty() {
        ui.label(
            RichText::new(crate::i18n::tr("专辑中没有曲目"))
                .size(13.0)
                .color(colors.text_weak),
        );
        return;
    }
    results_header(app, ui, &view.songs, crate::i18n::tr("首"), view.total);
    ui.add_space(6.0);
    let songs = view.songs.clone();
    list_songs(app, ui, &songs, false);
    let page_size = app.settings.network.album_page_size.clamp(10, 200);
    pagination(app, ui, view.page, view.total, page_size, |app, page| {
        app.album_to_page(page)
    });
}

fn artist_view(app: &mut App, ui: &mut Ui, view: &crate::app::ArtistView) {
    let colors = app.colors();
    ui.horizontal(|ui| {
        if widgets::icon_button(
            ui,
            egui_phosphor::regular::CARET_LEFT,
            17.0,
            crate::i18n::tr("返回搜索结果"),
            false,
            true,
            &colors,
        )
        .clicked()
        {
            app.close_artist();
        }
        ui.add_space(4.0);
        ui.label(
            RichText::new(if view.name.trim().is_empty() {
                crate::i18n::tr("歌手")
            } else {
                &view.name
            })
            .size(19.0)
            .strong()
            .color(colors.text),
        );
        if view.total > 0 {
            ui.label(
                RichText::new(crate::trf!("约 {} 首", view.total))
                    .size(11.5)
                    .color(colors.text_faint),
            );
        }
        if view.cached {
            widgets::chip(
                ui,
                crate::i18n::tr("缓存结果"),
                colors.warning,
                colors.card_hover,
            );
        }
    });
    ui.add_space(10.0);

    if view.loading {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.add_space(8.0);
            ui.label(
                RichText::new(crate::i18n::tr("正在加载歌手曲目…"))
                    .size(13.0)
                    .color(colors.text_weak),
            );
        });
        return;
    }
    if let Some(err) = view.error.clone() {
        error_card(ui, &err, &colors);
        return;
    }
    if view.songs.is_empty() {
        ui.label(
            RichText::new(crate::i18n::tr("没有找到这位歌手的曲目"))
                .size(13.0)
                .color(colors.text_weak),
        );
        return;
    }
    results_header(app, ui, &view.songs, crate::i18n::tr("首"), view.total);
    ui.add_space(6.0);
    let songs = view.songs.clone();
    list_songs(app, ui, &songs, false);
    let page_size = app.settings.network.album_page_size.clamp(10, 200);
    pagination(app, ui, view.page, view.total, page_size, |app, page| {
        app.artist_to_page(page)
    });
}

fn error_card(ui: &mut Ui, message: &str, colors: &crate::app::theme::ThemeColors) {
    egui::Frame::new()
        .fill(colors.card)
        .stroke(Stroke::new(1.0, colors.danger))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(egui::Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(message).size(12.5).color(colors.danger));
        });
}

pub fn discover(app: &mut App, ui: &mut Ui) {
    let colors = app.colors();
    widgets::page_header(
        ui,
        crate::i18n::tr("发现"),
        crate::i18n::tr("本地 BambooMusic 随机电台、智能歌单与最近播放"),
        &colors,
        |ui| {
            if app.settings.features.smart_playlists
                && widgets::secondary_button(ui, crate::i18n::tr("曲库统计"), &colors).clicked()
            {
                app.library_tab = crate::app::views::LibraryTab::Stats;
                app.set_page(crate::model::Page::Library);
            }
        },
    );

    // ---- Local BambooMusic random station ----
    if !app.settings.features.internet_radio {
        widgets::card(ui, &colors, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                RichText::new(crate::i18n::tr("没有可用的电台"))
                    .size(14.0)
                    .strong()
                    .color(colors.text),
            );
            ui.label(
                RichText::new(crate::i18n::tr("随机电台已在 设置 → 功能开关 中关闭"))
                    .size(10.5)
                    .color(colors.text_faint),
            );
        });
    } else if !app.station_available() {
        widgets::card(ui, &colors, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(crate::i18n::tr("没有可用的电台"))
                        .size(14.0)
                        .strong()
                        .color(colors.warning),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if widgets::secondary_button(ui, crate::i18n::tr("打开在线设置"), &colors)
                        .clicked()
                    {
                        app.settings_tab = crate::app::views::SettingsTab::Online;
                        app.set_page(crate::model::Page::Settings);
                    }
                });
            });
            ui.label(
                RichText::new(crate::i18n::tr(
                    "请先在 设置 → 在线与网络 配置并启用本地 BambooMusic 音源地址",
                ))
                .size(10.5)
                .color(colors.text_faint),
            );
        });
    } else {
        widgets::card(ui, &colors, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(crate::i18n::tr("随机电台"))
                        .size(14.0)
                        .strong()
                        .color(colors.text),
                );
                widgets::chip(
                    ui,
                    crate::i18n::tr("本地 BambooMusic"),
                    colors.accent,
                    colors.accent_soft,
                );
                if app.station_active {
                    widgets::chip(
                        ui,
                        crate::i18n::tr("播放中"),
                        colors.success,
                        colors.card_hover,
                    );
                }
                if app.station_fetching {
                    ui.spinner();
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if app.station_active {
                        if widgets::secondary_button(ui, crate::i18n::tr("停止随机电台"), &colors)
                            .clicked()
                        {
                            app.stop_station();
                        }
                    } else if widgets::primary_button(ui, crate::i18n::tr("开始随机电台"), &colors)
                        .clicked()
                    {
                        app.start_station();
                    }
                });
            });
            let source = app
                .station_source_name()
                .unwrap_or_else(|| crate::i18n::tr("本地音源").to_string());
            let mut detail = crate::trf!(
                "音源：{source} · 连续随机播放，队列不足时自动补充；停止后已加入的歌曲保留",
                source
            );
            if let Some(error) = app.station_error.as_ref() {
                detail.push_str(crate::i18n::tr(" · 最近错误："));
                detail.push_str(error);
            }
            ui.add(
                egui::Label::new(RichText::new(detail).size(10.5).color(
                    if app.station_error.is_some() {
                        colors.warning
                    } else {
                        colors.text_faint
                    },
                ))
                .wrap(),
            );
        });
    }
    ui.add_space(12.0);

    // ---- Random roam / recommendation feed ----
    if app.settings.features.internet_radio && app.settings.features.discover {
        if !app.discover_loading
            && app.discover_tracks.is_empty()
            && app.discover_error.is_none()
            && app.discover_available()
        {
            app.refresh_discover_feed();
        }
        let feed = app.discover_tracks.clone();
        let current_key = app.current_track.as_ref().map(|track| track.dedupe_key());
        let source_name = app.discover_source_name();
        let mut refresh = false;
        let mut play_all = false;
        let mut enqueue_all = false;
        let mut play_one: Option<crate::model::Track> = None;
        let mut add_one: Option<crate::model::Track> = None;
        widgets::card(ui, &colors, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(crate::i18n::tr("随机漫游"))
                        .size(14.0)
                        .strong()
                        .color(colors.text),
                );
                widgets::chip(
                    ui,
                    crate::i18n::tr("在线推荐"),
                    colors.accent,
                    colors.accent_soft,
                );
                if let Some(source) = source_name.as_ref() {
                    widgets::chip(ui, source, colors.text_weak, colors.card_hover);
                }
                if app.discover_loading {
                    ui.spinner();
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if widgets::secondary_button(ui, crate::i18n::tr("换一批"), &colors).clicked()
                    {
                        refresh = true;
                    }
                    if !feed.is_empty() {
                        if widgets::primary_button(ui, crate::i18n::tr("播放全部"), &colors)
                            .clicked()
                        {
                            play_all = true;
                        }
                        if widgets::secondary_button(ui, crate::i18n::tr("加入队列"), &colors)
                            .clicked()
                        {
                            enqueue_all = true;
                        }
                    }
                });
            });
            ui.add(
                egui::Label::new(
                    RichText::new(crate::i18n::tr(
                        "从当前音源随机挑选歌曲，点一首即可播放；不会中断正在播放的随机电台",
                    ))
                    .size(10.5)
                    .color(colors.text_faint),
                )
                .wrap(),
            );
            if let Some(error) = app.discover_error.clone() {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(error).size(11.0).color(colors.warning));
                    if ui
                        .link(RichText::new(crate::i18n::tr("打开在线设置")).size(10.5))
                        .clicked()
                    {
                        refresh = false;
                        app.settings_tab = crate::app::views::SettingsTab::Online;
                        app.set_page(crate::model::Page::Settings);
                    }
                });
            }
            if feed.is_empty() {
                if app.discover_loading {
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(
                            RichText::new(crate::i18n::tr("正在获取推荐…"))
                                .size(11.0)
                                .color(colors.text_weak),
                        );
                    });
                }
                return;
            }
            ui.add_space(4.0);
            egui::ScrollArea::vertical()
                .id_salt("bamboo-discover-feed")
                .max_height(322.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for (index, track) in feed.iter().enumerate() {
                        let current = current_key
                            .as_deref()
                            .is_some_and(|key| key == track.dedupe_key());
                        let response = widgets::song_row(ui, index, track, current, false, &colors);
                        if response.clicked() {
                            play_one = Some(track.clone());
                        }
                        response.context_menu(|ui| {
                            if ui.button(crate::i18n::tr("播放")).clicked() {
                                play_one = Some(track.clone());
                                ui.close();
                            }
                            if ui.button(app.t("player.add_to_queue")).clicked() {
                                add_one = Some(track.clone());
                                ui.close();
                            }
                            widgets::track_clipboard_menu(ui, track);
                        });
                        ui.add_space(2.0);
                    }
                });
        });
        if refresh {
            app.refresh_discover_feed();
        } else if play_all {
            app.play_discover_feed();
        } else if enqueue_all {
            app.enqueue_discover_feed();
        } else if let Some(track) = add_one {
            let key = track.dedupe_key();
            if app.queue.items.iter().any(|item| item.dedupe_key() == key) {
                app.toast(
                    crate::i18n::tr("歌曲已在队列中"),
                    crate::app::ToastKind::Info,
                );
            } else if app.queue.add(track) {
                app.session_dirty = true;
                app.toast(
                    crate::i18n::tr("已加入队列"),
                    crate::app::ToastKind::Success,
                );
            }
        } else if let Some(track) = play_one {
            app.play_track_now(&track);
        }
        ui.add_space(12.0);
    }

    // ---- Smart playlists (moved here from the playlists page) ----
    crate::app::views::smart_playlists(app, ui);

    // ---- Recent station history ----
    widgets::card(ui, &colors, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(crate::i18n::tr("最近播放"))
                    .size(13.0)
                    .strong()
                    .color(colors.text),
            );
            widgets::chip(
                ui,
                crate::i18n::tr("随机电台"),
                colors.text_faint,
                colors.card_hover,
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if !app.station_history.is_empty()
                    && widgets::secondary_button(ui, crate::i18n::tr("清空"), &colors).clicked()
                {
                    app.clear_station_history();
                }
            });
        });
        ui.add_space(4.0);
        if app.station_history.is_empty() {
            ui.label(
                RichText::new(crate::i18n::tr("还没有随机电台播放记录"))
                    .size(10.5)
                    .color(colors.text_faint),
            );
            return;
        }
        let history = app.station_history.clone();
        let mut play: Option<crate::model::Track> = None;
        for track in &history {
            ui.horizontal(|ui| {
                let width = (ui.available_width() - 90.0).max(140.0);
                ui.allocate_ui_with_layout(
                    egui::vec2(width, 0.0),
                    egui::Layout::top_down(egui::Align::LEFT),
                    |ui| {
                        ui.set_width(width);
                        ui.add(
                            egui::Label::new(
                                RichText::new(track.display_title())
                                    .size(12.5)
                                    .color(colors.text),
                            )
                            .truncate(),
                        );
                        ui.add(
                            egui::Label::new(
                                RichText::new(track.subtitle())
                                    .size(10.0)
                                    .color(colors.text_weak),
                            )
                            .truncate(),
                        );
                    },
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if widgets::secondary_button(ui, crate::i18n::tr("播放"), &colors).clicked() {
                        play = Some(track.clone());
                    }
                });
            });
            ui.add_space(4.0);
        }
        if let Some(track) = play {
            app.play_track_now(&track);
        }
    });
}
