//! Reusable custom widgets for the modern UI.

use egui::{
    Align2, Color32, CornerRadius, FontId, Key, Margin, Modifiers, Response, RichText, Sense,
    Stroke, Ui, WidgetInfo, WidgetType, vec2,
};

use crate::app::theme::ThemeColors;
use crate::model::Track;
use crate::util::format_time;

pub fn icon_button(
    ui: &mut Ui,
    icon: &str,
    size: f32,
    tooltip: &str,
    active: bool,
    enabled: bool,
    colors: &ThemeColors,
) -> Response {
    let button_size = vec2(size + 16.0, size + 10.0);
    let (rect, response) = ui.allocate_exact_size(button_size, Sense::click());
    let hovered = response.hovered() && enabled;
    let clicked = response.is_pointer_button_down_on() && enabled;
    let bg = if clicked {
        colors.accent_soft
    } else if hovered {
        colors.card_hover
    } else {
        Color32::TRANSPARENT
    };
    if bg != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, CornerRadius::same(8), bg);
    }
    let color = if !enabled {
        colors.text_faint
    } else if active {
        colors.accent
    } else if hovered {
        colors.text
    } else {
        colors.text_weak
    };
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        icon,
        FontId::new(size, egui::FontFamily::Proportional),
        color,
    );
    let response = response.on_hover_text(tooltip);
    response.widget_info(|| {
        WidgetInfo::labeled(WidgetType::Button, enabled && ui.is_enabled(), tooltip)
    });
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect.shrink(1.0),
            CornerRadius::same(8),
            Stroke::new(1.5, colors.accent),
            egui::StrokeKind::Inside,
        );
    }
    response
}

pub fn nav_button(
    ui: &mut Ui,
    icon: &str,
    label: &str,
    selected: bool,
    colors: &ThemeColors,
    width: f32,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(width, 40.0), Sense::click());
    let hovered = response.hovered();
    let bg = if selected {
        colors.accent_soft
    } else if hovered {
        colors.card_hover
    } else {
        Color32::TRANSPARENT
    };
    if bg != Color32::TRANSPARENT {
        ui.painter()
            .rect_filled(rect.shrink2(vec2(4.0, 1.0)), CornerRadius::same(8), bg);
    }
    let color = if selected {
        colors.accent
    } else if hovered {
        colors.text
    } else {
        colors.text_weak
    };
    ui.painter().text(
        rect.left_center() + vec2(16.0, 0.0),
        Align2::LEFT_CENTER,
        icon,
        FontId::new(19.0, egui::FontFamily::Proportional),
        color,
    );
    ui.painter().text(
        rect.left_center() + vec2(44.0, 0.0),
        Align2::LEFT_CENTER,
        label,
        FontId::new(14.0, egui::FontFamily::Proportional),
        color,
    );
    response.widget_info(|| {
        WidgetInfo::selected(
            WidgetType::SelectableLabel,
            ui.is_enabled(),
            selected,
            label,
        )
    });
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect.shrink(2.0),
            CornerRadius::same(8),
            Stroke::new(1.5, colors.accent),
            egui::StrokeKind::Inside,
        );
    }
    response
}

/// Compact row used by the sidebar's “My playlists” section. Playlist names
/// come from files and can be very long, so the label is measured and
/// truncated with an ellipsis instead of overlapping the panel edge.
pub fn playlist_button(
    ui: &mut Ui,
    name: &str,
    selected: bool,
    missing: bool,
    colors: &ThemeColors,
    width: f32,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(width, 30.0), Sense::click());
    let hovered = response.hovered();
    let bg = if selected {
        colors.accent_soft
    } else if hovered {
        colors.card_hover
    } else {
        Color32::TRANSPARENT
    };
    if bg != Color32::TRANSPARENT {
        ui.painter()
            .rect_filled(rect.shrink2(vec2(4.0, 1.0)), CornerRadius::same(7), bg);
    }
    let icon_color = if missing {
        colors.danger
    } else if selected {
        colors.accent
    } else if hovered {
        colors.text
    } else {
        colors.text_weak
    };
    ui.painter().text(
        rect.left_center() + vec2(18.0, 0.0),
        Align2::LEFT_CENTER,
        egui_phosphor::regular::PLAYLIST,
        FontId::new(13.0, egui::FontFamily::Proportional),
        icon_color,
    );
    let text_color = if missing {
        colors.danger
    } else if selected || hovered {
        colors.text
    } else {
        colors.text_weak
    };
    let font = FontId::new(12.5, egui::FontFamily::Proportional);
    let label = truncate_to_width(ui, name, &font, (rect.width() - 42.0).max(24.0));
    ui.painter().text(
        rect.left_center() + vec2(40.0, 0.0),
        Align2::LEFT_CENTER,
        &label,
        font,
        text_color,
    );
    response.widget_info(|| {
        WidgetInfo::selected(WidgetType::SelectableLabel, ui.is_enabled(), selected, name)
    });
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect.shrink(2.0),
            CornerRadius::same(7),
            Stroke::new(1.5, colors.accent),
            egui::StrokeKind::Inside,
        );
    }
    response
}

fn truncate_to_width(ui: &Ui, text: &str, font: &FontId, max_width: f32) -> String {
    let measure = |candidate: &str| {
        ui.painter()
            .layout_no_wrap(candidate.to_string(), font.clone(), Color32::WHITE)
            .size()
            .x
    };
    if measure(text) <= max_width {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut keep = chars.len();
    while keep > 0 {
        keep -= 1;
        let mut candidate: String = chars[..keep].iter().collect();
        candidate.push('…');
        if measure(&candidate) <= max_width {
            return candidate;
        }
    }
    "…".to_string()
}

pub fn primary_button(ui: &mut Ui, label: &str, colors: &ThemeColors) -> Response {
    ui.add(
        egui::Button::new(RichText::new(label).color(Color32::from_rgb(12, 16, 24)))
            .fill(colors.accent)
            .corner_radius(CornerRadius::same(8))
            .min_size(vec2(0.0, 34.0)),
    )
}

pub fn secondary_button(ui: &mut Ui, label: &str, colors: &ThemeColors) -> Response {
    ui.add(
        egui::Button::new(RichText::new(label).color(colors.text))
            .fill(colors.bg2)
            .stroke(Stroke::new(1.0, colors.line))
            .corner_radius(CornerRadius::same(8))
            .min_size(vec2(0.0, 34.0)),
    )
}

/// A primary + secondary button pair centered inside a top-down centered
/// empty state. A fixed-width child shifts the pair whenever the labels are
/// wider than that child (the row overflows to one side), so measure the real
/// row and add the exact left padding instead.
pub fn centered_button_pair(
    ui: &mut Ui,
    primary: &str,
    secondary: &str,
    colors: &ThemeColors,
) -> (Response, Response) {
    let font = egui::TextStyle::Button.resolve(ui.style());
    let text_width = |text: &str| {
        ui.painter()
            .layout_no_wrap(text.to_owned(), font.clone(), Color32::WHITE)
            .size()
            .x
    };
    let padding = ui.spacing().button_padding.x * 2.0;
    let gap = ui.spacing().item_spacing.x;
    let available = ui.available_width();
    let row_width =
        (text_width(primary) + text_width(secondary) + padding * 2.0 + gap).min(available);
    let left = ((available - row_width) * 0.5).max(0.0);
    ui.horizontal(|ui| {
        ui.add_space(left);
        let first = primary_button(ui, primary, colors);
        let second = secondary_button(ui, secondary, colors);
        (first, second)
    })
    .inner
}

pub fn chip(ui: &mut Ui, text: &str, fg: Color32, bg: Color32) -> Response {
    let galley = ui.painter().layout_no_wrap(
        text.to_string(),
        FontId::new(11.0, egui::FontFamily::Proportional),
        fg,
    );
    let size = galley.size() + vec2(14.0, 6.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(6), bg);
    ui.painter().galley(
        rect.left_center() + vec2(7.0, -galley.size().y / 2.0),
        galley,
        fg,
    );
    response
}

pub struct SeekResult {
    pub value: Option<f32>,
    pub commit: bool,
}

pub fn seek_bar(
    ui: &mut Ui,
    width: f32,
    position: f32,
    duration: f32,
    colors: &ThemeColors,
    enabled: bool,
) -> Option<SeekResult> {
    let (rect, response) = ui.allocate_exact_size(vec2(width, 18.0), Sense::click_and_drag());
    response.widget_info(|| {
        WidgetInfo::slider(
            enabled && duration > 0.0,
            position.clamp(0.0, duration.max(0.0)) as f64,
            crate::i18n::tr("播放进度"),
        )
    });
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect.shrink(3.0),
            CornerRadius::same(5),
            Stroke::new(1.0, colors.accent),
            egui::StrokeKind::Inside,
        );
    }
    let hovered = response.hovered();
    let t = if duration > 0.0 {
        (position / duration).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let rail = egui::Rect::from_center_size(
        rect.center(),
        vec2(
            rect.width(),
            if hovered || response.dragged() {
                5.0
            } else {
                4.0
            },
        ),
    );
    ui.painter()
        .rect_filled(rail, CornerRadius::same(3), colors.line);
    if t > 0.0 {
        let fill = egui::Rect::from_min_max(
            rail.min,
            egui::pos2(rail.min.x + rail.width() * t, rail.max.y),
        );
        ui.painter()
            .rect_filled(fill, CornerRadius::same(3), colors.accent);
    }
    if hovered || response.dragged() {
        let x = rail.min.x + rail.width() * t;
        ui.painter()
            .circle_filled(egui::pos2(x, rail.center().y), 5.5, colors.text);
    }
    if !enabled || duration <= 0.0 {
        return None;
    }
    // Keyboard control while focused (Tab/click gives the bar focus).
    if response.has_focus() {
        let step = (duration * 0.02).clamp(1.0, 30.0);
        let page = (duration * 0.10).clamp(5.0, 120.0);
        let (delta, absolute) = ui.input_mut(|input| {
            let mut delta = 0.0;
            if input.consume_key(Modifiers::NONE, Key::ArrowLeft)
                || input.consume_key(Modifiers::NONE, Key::ArrowDown)
            {
                delta -= step;
            }
            if input.consume_key(Modifiers::NONE, Key::ArrowRight)
                || input.consume_key(Modifiers::NONE, Key::ArrowUp)
            {
                delta += step;
            }
            if input.consume_key(Modifiers::NONE, Key::PageDown) {
                delta -= page;
            }
            if input.consume_key(Modifiers::NONE, Key::PageUp) {
                delta += page;
            }
            let absolute = if input.consume_key(Modifiers::NONE, Key::Home) {
                Some(0.0)
            } else if input.consume_key(Modifiers::NONE, Key::End) {
                Some(duration)
            } else {
                None
            };
            (delta, absolute)
        });
        if let Some(value) = absolute {
            return Some(SeekResult {
                value: Some(value),
                commit: true,
            });
        }
        if delta != 0.0 {
            return Some(SeekResult {
                value: Some((position + delta).clamp(0.0, duration)),
                commit: true,
            });
        }
    }
    let mut value = None;
    if response.dragged() || response.clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            let x = ((pos.x - rail.min.x) / rail.width().max(1.0)).clamp(0.0, 1.0);
            value = Some(x * duration);
        }
    }
    let commit = response.clicked() || response.drag_stopped();
    Some(SeekResult { value, commit })
}

pub fn volume_bar(ui: &mut Ui, volume: f32, colors: &ThemeColors, width: f32) -> Option<f32> {
    let (rect, response) = ui.allocate_exact_size(vec2(width, 18.0), Sense::click_and_drag());
    response.widget_info(|| {
        WidgetInfo::slider(
            ui.is_enabled(),
            volume.clamp(0.0, 1.0) as f64,
            crate::i18n::tr("音量"),
        )
    });
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect.shrink(3.0),
            CornerRadius::same(5),
            Stroke::new(1.0, colors.accent),
            egui::StrokeKind::Inside,
        );
    }
    let t = volume.clamp(0.0, 1.0);
    let rail = egui::Rect::from_center_size(rect.center(), vec2(rect.width(), 4.0));
    ui.painter()
        .rect_filled(rail, CornerRadius::same(3), colors.line);
    let fill = egui::Rect::from_min_max(
        rail.min,
        egui::pos2(rail.min.x + rail.width() * t, rail.max.y),
    );
    ui.painter()
        .rect_filled(fill, CornerRadius::same(3), colors.accent);
    if response.hovered() || response.dragged() {
        let x = rail.min.x + rail.width() * t;
        ui.painter()
            .circle_filled(egui::pos2(x, rail.center().y), 5.0, colors.text);
    }
    if response.has_focus() {
        let (delta, absolute) = ui.input_mut(|input| {
            let mut delta = 0.0;
            if input.consume_key(Modifiers::NONE, Key::ArrowLeft)
                || input.consume_key(Modifiers::NONE, Key::ArrowDown)
            {
                delta -= 0.05;
            }
            if input.consume_key(Modifiers::NONE, Key::ArrowRight)
                || input.consume_key(Modifiers::NONE, Key::ArrowUp)
            {
                delta += 0.05;
            }
            let absolute = if input.consume_key(Modifiers::NONE, Key::Home) {
                Some(0.0)
            } else if input.consume_key(Modifiers::NONE, Key::End) {
                Some(1.0)
            } else {
                None
            };
            (delta, absolute)
        });
        if let Some(value) = absolute {
            return Some(value);
        }
        if delta != 0.0 {
            return Some((volume + delta).clamp(0.0, 1.0));
        }
    }
    if response.dragged() || response.clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            let x = ((pos.x - rail.min.x) / rail.width().max(1.0)).clamp(0.0, 1.0);
            return Some(x);
        }
    }
    None
}

pub fn card<R>(ui: &mut Ui, colors: &ThemeColors, add: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::new()
        .fill(colors.card)
        .stroke(Stroke::new(1.0, colors.line))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(20, 18))
        // Keep the 1 px border inside the page scroll clip rect; at the
        // exact content edge the left stroke can be clipped away.
        .outer_margin(Margin {
            left: 2,
            right: 2,
            top: 0,
            bottom: 0,
        })
        .show(ui, |ui| {
            // `ui.available_width()` here is already the width inside the
            // frame's margins. Using the outer width made every card 40 px
            // wider than the content area and clipped it at the window edge.
            // Never let a card grow past the frame's content bounds; a
            // previous overflowing row could otherwise widen the card and
            // clip its right side behind the lyrics panel.
            let inner_width = ui.available_width().min(ui.max_rect().width()).max(80.0);
            ui.set_width(inner_width);
            add(ui)
        })
        .inner
}

/// Searchable system-font dropdown. Uses egui's popup with
/// `CloseOnClickOutside` so focusing/typing in the search box does not close
/// the list, while still behaving like a normal ComboBox.
pub fn font_picker(ui: &mut Ui, id: &str, current: &mut Option<String>, none_label: &str) -> bool {
    let mut changed = false;
    let label = current
        .as_deref()
        .map(short_font_name)
        .unwrap_or_else(|| none_label.to_string());
    let filter_id = ui.id().with((id, "filter"));

    let button = ui.add(
        egui::Button::new(egui::RichText::new(label).size(13.0))
            .min_size(egui::vec2(240.0, ui.spacing().interact_size.y)),
    );
    if button.clicked() {
        // Start every dropdown session with an empty font filter.
        ui.memory_mut(|memory| memory.data.insert_temp(filter_id, String::new()));
    }

    // Pick a stable side before showing the popup: put the search box on the
    // side closest to the trigger (top when opening below, bottom when
    // opening above) instead of letting the popup height change the order.
    let content_rect = ui.ctx().content_rect();
    let estimated_height = 310.0;
    let space_below = content_rect.bottom() - button.rect.bottom();
    let space_above = button.rect.top() - content_rect.top();
    let opens_above = space_below < estimated_height && space_above > space_below;
    let align = if opens_above {
        egui::RectAlign::TOP_START
    } else {
        egui::RectAlign::BOTTOM_START
    };

    egui::Popup::menu(&button)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .align(align)
        .align_alternatives(&[])
        .gap(2.0)
        .width(400.0)
        .show(|ui| {
            // A popup area starts out screen-sized, so `available_width()`
            // here reports the whole monitor on the first frame. Feeding that
            // back into the row width made the popup grow every frame until it
            // hit the screen edge; use a fixed content width instead.
            const CONTENT_WIDTH: f32 = 380.0;
            ui.set_width(CONTENT_WIDTH);
            let item_width = CONTENT_WIDTH;
            let row_height = ui.spacing().interact_size.y.max(22.0);
            let mut filter = ui
                .memory(|memory| memory.data.get_temp::<String>(filter_id))
                .unwrap_or_default();
            let draw_search = |ui: &mut Ui, filter: &mut String| {
                if ui
                    .add(
                        egui::TextEdit::singleline(filter)
                            .hint_text(crate::i18n::tr("搜索字体…"))
                            .desired_width(item_width),
                    )
                    .changed()
                {
                    ui.memory_mut(|memory| {
                        memory.data.insert_temp(filter_id, filter.clone());
                    });
                }
            };
            let draw_file = |ui: &mut Ui| {
                ui.add_sized(
                    [item_width, row_height],
                    egui::Button::new(crate::i18n::tr("选择字体文件…")),
                )
                .clicked()
            };

            if !opens_above {
                draw_search(ui, &mut filter);
                ui.separator();
            }
            let mut file_clicked = false;
            if opens_above {
                file_clicked = draw_file(ui);
                ui.separator();
            }

            if selectable_row(ui, item_width, row_height, none_label, current.is_none()) {
                *current = None;
                changed = true;
                egui::Popup::close_all(ui.ctx());
            }
            egui::ScrollArea::vertical()
                .max_height(220.0)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let needle = filter.trim().to_lowercase();
                    for name in crate::app::fonts::system_font_families() {
                        if !needle.is_empty() && !name.to_lowercase().contains(&needle) {
                            continue;
                        }
                        if selectable_row(
                            ui,
                            item_width,
                            row_height,
                            name,
                            current.as_deref() == Some(name.as_str()),
                        ) {
                            *current = Some(name.clone());
                            changed = true;
                            egui::Popup::close_all(ui.ctx());
                        }
                    }
                });
            ui.separator();
            if opens_above {
                draw_search(ui, &mut filter);
            } else if draw_file(ui) {
                file_clicked = true;
            }

            if file_clicked
                && let Some(path) = rfd::FileDialog::new()
                    .add_filter("Font", &["ttf", "otf", "ttc"])
                    .pick_file()
            {
                *current = Some(path.to_string_lossy().into_owned());
                changed = true;
                egui::Popup::close_all(ui.ctx());
            }
        });

    changed
}

fn selectable_row(ui: &mut Ui, width: f32, height: f32, text: &str, selected: bool) -> bool {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click());
    response.widget_info(|| {
        WidgetInfo::selected(WidgetType::SelectableLabel, ui.is_enabled(), selected, text)
    });
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect.shrink(1.0),
            egui::CornerRadius::same(4),
            Stroke::new(1.5, ui.visuals().selection.stroke.color),
            egui::StrokeKind::Inside,
        );
    }
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact_selectable(&response, selected);
        if selected || response.hovered() {
            ui.painter()
                .rect_filled(rect, egui::CornerRadius::same(4), visuals.bg_fill);
        }
        ui.painter().text(
            rect.left_center() + egui::vec2(8.0, 0.0),
            egui::Align2::LEFT_CENTER,
            text,
            egui::FontId::proportional(13.0),
            visuals.text_color(),
        );
    }
    response.clicked()
}

fn short_font_name(family: &str) -> String {
    std::path::Path::new(family)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| family.to_string())
}

pub fn page_header(
    ui: &mut Ui,
    title: &str,
    subtitle: &str,
    colors: &ThemeColors,
    right: impl FnOnce(&mut Ui),
) {
    // Lay out the actions first so the title gets exactly the width that is
    // left. The previous center-aligned horizontal row let the buttons cover
    // the subtitle on narrow windows.
    ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
        right(ui);
        ui.add_space(16.0);
        let left_width = ui.available_width();
        ui.allocate_ui_with_layout(
            egui::vec2(left_width, 0.0),
            egui::Layout::top_down(egui::Align::LEFT),
            |ui| {
                ui.set_width(left_width);
                ui.add(
                    egui::Label::new(RichText::new(title).size(24.0).strong().color(colors.text))
                        .truncate(),
                );
                if !subtitle.is_empty() {
                    ui.add_space(2.0);
                    ui.add(
                        egui::Label::new(
                            RichText::new(subtitle).size(12.5).color(colors.text_weak),
                        )
                        .wrap(),
                    );
                }
            },
        );
    });
    ui.add_space(18.0);
}

pub fn empty_state(
    ui: &mut Ui,
    icon: &str,
    title: &str,
    hint: &str,
    colors: &ThemeColors,
    action: impl FnOnce(&mut Ui),
) {
    let height = (ui.available_height() - 40.0).max(220.0);
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), height),
        egui::Layout::top_down(egui::Align::Center),
        |ui| {
            ui.add_space((height * 0.22).min(160.0));
            ui.label(RichText::new(icon).size(52.0).color(colors.text_faint));
            ui.add_space(12.0);
            ui.label(RichText::new(title).size(19.0).strong().color(colors.text));
            ui.add_space(6.0);
            ui.label(RichText::new(hint).size(13.0).color(colors.text_weak));
            ui.add_space(18.0);
            action(ui);
        },
    );
}

pub fn song_row(
    ui: &mut Ui,
    index: usize,
    track: &Track,
    current: bool,
    playing: bool,
    colors: &ThemeColors,
) -> Response {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(vec2(width, 52.0), Sense::click_and_drag());
    let label = if track.artist.trim().is_empty() {
        track.display_title().to_string()
    } else {
        format!("{} · {}", track.display_title(), track.subtitle())
    };
    response
        .widget_info(|| WidgetInfo::selected(WidgetType::SelectableLabel, true, current, &label));
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect.shrink(1.0),
            CornerRadius::same(8),
            Stroke::new(1.5, colors.accent),
            egui::StrokeKind::Inside,
        );
    }
    let hovered = response.hovered();
    if current || hovered {
        let bg = if current {
            colors.accent_soft
        } else {
            colors.card_hover
        };
        ui.painter()
            .rect_filled(rect.shrink2(vec2(0.0, 1.0)), CornerRadius::same(8), bg);
    }
    let icon = if current {
        if playing {
            egui_phosphor::regular::SPEAKER_HIGH
        } else {
            egui_phosphor::regular::PAUSE
        }
    } else {
        egui_phosphor::regular::PLAY
    };
    let index_area = egui::Rect::from_min_size(rect.min, vec2(44.0, rect.height()));
    if hovered || current {
        ui.painter().text(
            index_area.center(),
            Align2::CENTER_CENTER,
            icon,
            FontId::new(16.0, egui::FontFamily::Proportional),
            if current { colors.accent } else { colors.text },
        );
    } else {
        ui.painter().text(
            index_area.center(),
            Align2::CENTER_CENTER,
            format!("{}", index + 1),
            FontId::new(12.0, egui::FontFamily::Proportional),
            colors.text_faint,
        );
    }

    let text_left = rect.left() + 50.0;
    let text_width = (rect.width() - 50.0 - 200.0).max(80.0);
    ui.painter().text(
        egui::pos2(text_left, rect.center().y - 9.0),
        Align2::LEFT_CENTER,
        crate::util::truncate_chars(&track.display_title(), 80),
        FontId::new(14.0, egui::FontFamily::Proportional),
        if current { colors.accent } else { colors.text },
    );
    let artist = track.subtitle();
    ui.painter().text(
        egui::pos2(text_left, rect.center().y + 10.0),
        Align2::LEFT_CENTER,
        crate::util::truncate_chars(&artist, 70),
        FontId::new(11.5, egui::FontFamily::Proportional),
        colors.text_weak,
    );
    // Source badge: local files, direct URLs and online Bamboo sources are
    // visually distinguishable in the queue.
    let source = if track.source_name.trim().is_empty() {
        track.source.kind_label().to_string()
    } else {
        crate::i18n::tr_str(&track.source_name).to_string()
    };
    ui.painter().text(
        egui::pos2(rect.right() - 78.0, rect.center().y),
        Align2::RIGHT_CENTER,
        crate::util::truncate_chars(&source, 10),
        FontId::new(10.5, egui::FontFamily::Proportional),
        colors.text_faint,
    );
    // Keep the layout honest even though we paint manually.
    ui.allocate_space(vec2(0.0, 0.0));
    let _ = text_width;
    if let Some(dur) = track.duration {
        ui.painter().text(
            egui::pos2(rect.right() - 12.0, rect.center().y),
            Align2::RIGHT_CENTER,
            format_time(dur),
            FontId::new(12.0, egui::FontFamily::Proportional),
            colors.text_faint,
        );
    }
    response
}

/// Shared right-click actions for song names in the queue, library and
/// now-playing views.
pub fn track_clipboard_menu(ui: &mut Ui, track: &Track) {
    ui.separator();
    if ui.button(crate::i18n::tr("复制歌名")).clicked() {
        ui.ctx().copy_text(track.display_title());
        ui.close();
    }
    if let Some(path) = track.source.path() {
        if let Some(name) = path.file_name() {
            if ui.button(crate::i18n::tr("复制文件名")).clicked() {
                ui.ctx().copy_text(name.to_string_lossy().into_owned());
                ui.close();
            }
        }
        if ui.button(crate::i18n::tr("复制文件路径")).clicked() {
            ui.ctx().copy_text(path.display().to_string());
            ui.close();
        }
        if ui.button(crate::i18n::tr("在资源管理器中显示")).clicked() {
            crate::util::reveal_in_explorer(path);
            ui.close();
        }
    } else if let crate::model::TrackSource::Url { url }
    | crate::model::TrackSource::Radio { url } = &track.source
    {
        if ui.button(crate::i18n::tr("复制链接")).clicked() {
            ui.ctx().copy_text(url.clone());
            ui.close();
        }
    }
}

pub fn key_value(ui: &mut Ui, key: &str, value: &str, colors: &ThemeColors) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(key).size(12.5).color(colors.text_weak));
        ui.label(RichText::new(value).size(12.5).color(colors.text));
    });
}
