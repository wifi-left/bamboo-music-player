//! Reusable custom widgets for the modern UI.

use egui::{
    Align2, Color32, CornerRadius, FontId, Margin, Response, RichText, Sense, Stroke, Ui, vec2,
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
        ui.painter()
            .rect_filled(rect, CornerRadius::same(9), bg);
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
    if !enabled {
        response
    } else {
        response
    }
}

pub fn nav_button(
    ui: &mut Ui,
    icon: &str,
    label: &str,
    selected: bool,
    colors: &ThemeColors,
) -> Response {
    let width = ui.available_width();
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
            .rect_filled(rect.shrink2(vec2(4.0, 1.0)), CornerRadius::same(10), bg);
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
    response
}

pub fn primary_button(ui: &mut Ui, label: &str, colors: &ThemeColors) -> Response {
    ui.add(
        egui::Button::new(RichText::new(label).color(Color32::from_rgb(12, 16, 24)))
            .fill(colors.accent)
            .corner_radius(CornerRadius::same(10))
            .min_size(vec2(0.0, 34.0)),
    )
}

pub fn secondary_button(ui: &mut Ui, label: &str, colors: &ThemeColors) -> Response {
    ui.add(
        egui::Button::new(RichText::new(label).color(colors.text))
            .fill(colors.card)
            .stroke(Stroke::new(1.0, colors.line))
            .corner_radius(CornerRadius::same(10))
            .min_size(vec2(0.0, 34.0)),
    )
}

pub fn chip(ui: &mut Ui, text: &str, fg: Color32, bg: Color32) -> Response {
    let galley = ui.painter().layout_no_wrap(
        text.to_string(),
        FontId::new(11.0, egui::FontFamily::Proportional),
        fg,
    );
    let size = galley.size() + vec2(14.0, 6.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(7), bg);
    ui.painter()
        .galley(rect.left_center() + vec2(7.0, -galley.size().y / 2.0), galley, fg);
    response
}

pub struct SeekResult {
    pub value: Option<f32>,
    pub commit: bool,
}

pub fn seek_bar(
    ui: &mut Ui,
    position: f32,
    duration: f32,
    colors: &ThemeColors,
    enabled: bool,
) -> Option<SeekResult> {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(vec2(width, 18.0), Sense::click_and_drag());
    let hovered = response.hovered();
    let t = if duration > 0.0 {
        (position / duration).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let rail = egui::Rect::from_center_size(
        rect.center(),
        vec2(rect.width(), if hovered || response.dragged() { 5.0 } else { 4.0 }),
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
        ui.painter().circle_filled(
            egui::pos2(x, rail.center().y),
            5.5,
            colors.text,
        );
    }
    if !enabled || duration <= 0.0 {
        return None;
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
        .corner_radius(CornerRadius::same(14))
        .inner_margin(Margin::same(16))
        .show(ui, add)
        .inner
}

pub fn page_header(
    ui: &mut Ui,
    title: &str,
    subtitle: &str,
    colors: &ThemeColors,
    right: impl FnOnce(&mut Ui),
) {
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.label(
                RichText::new(title)
                    .size(26.0)
                    .strong()
                    .color(colors.text),
            );
            if !subtitle.is_empty() {
                ui.add_space(2.0);
                ui.label(RichText::new(subtitle).size(12.5).color(colors.text_weak));
            }
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            right(ui);
        });
    });
    ui.add_space(14.0);
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
            ui.label(
                RichText::new(icon)
                    .size(52.0)
                    .color(colors.text_faint),
            );
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
    let (rect, response) = ui.allocate_exact_size(vec2(width, 52.0), Sense::click());
    let hovered = response.hovered();
    if current || hovered {
        let bg = if current {
            colors.accent_soft
        } else {
            colors.card_hover
        };
        ui.painter()
            .rect_filled(rect.shrink2(vec2(0.0, 1.0)), CornerRadius::same(9), bg);
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
    let text_width = (rect.width() - 50.0 - 120.0).max(80.0);
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
    // Keep the layout honest even though we paint manually.
    ui.allocate_space(vec2(0.0, 0.0));
    let _ = text_width;
    if let Some(dur) = track.duration {
        ui.painter().text(
            egui::pos2(rect.right() - 22.0, rect.center().y),
            Align2::RIGHT_CENTER,
            format_time(dur),
            FontId::new(12.0, egui::FontFamily::Proportional),
            colors.text_faint,
        );
    }
    response
}

pub fn section_title(ui: &mut Ui, title: &str, colors: &ThemeColors) {
    ui.label(RichText::new(title).size(15.0).strong().color(colors.text));
    ui.add_space(6.0);
}

pub fn key_value(ui: &mut Ui, key: &str, value: &str, colors: &ThemeColors) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(key).size(12.5).color(colors.text_weak));
        ui.label(RichText::new(value).size(12.5).color(colors.text));
    });
}
