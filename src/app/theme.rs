//! Color palette and egui style application.

use egui::{
    Color32, CornerRadius, FontFamily, FontId, Margin, Stroke, TextStyle, Theme, Visuals, vec2,
};

use crate::config::{Settings, ThemeMode};

#[derive(Clone, Debug)]
pub struct ThemeColors {
    pub bg: Color32,
    pub bg2: Color32,
    pub panel: Color32,
    pub card: Color32,
    pub card_hover: Color32,
    pub line: Color32,
    pub text: Color32,
    pub text_weak: Color32,
    pub text_faint: Color32,
    pub accent: Color32,
    pub accent_soft: Color32,
    pub danger: Color32,
    pub warning: Color32,
    pub success: Color32,
    pub shadow: Color32,
}

pub fn is_dark(ctx: &egui::Context, settings: &Settings) -> bool {
    match settings.theme.mode {
        ThemeMode::Dark => true,
        ThemeMode::Light => false,
        ThemeMode::System => ctx
            .system_theme()
            .map(|t| t == Theme::Dark)
            .unwrap_or(true),
    }
}

pub fn colors(ctx: &egui::Context, settings: &Settings) -> ThemeColors {
    let accent = Color32::from_rgb(
        settings.theme.accent[0],
        settings.theme.accent[1],
        settings.theme.accent[2],
    );
    if is_dark(ctx, settings) {
        ThemeColors {
            bg: Color32::from_rgb(10, 13, 20),
            bg2: Color32::from_rgb(15, 20, 30),
            panel: Color32::from_rgb(20, 26, 38),
            card: Color32::from_rgb(27, 34, 49),
            card_hover: Color32::from_rgb(36, 46, 65),
            line: Color32::from_rgb(42, 52, 70),
            text: Color32::from_rgb(232, 237, 247),
            text_weak: Color32::from_rgb(154, 164, 184),
            text_faint: Color32::from_rgb(108, 118, 138),
            accent,
            accent_soft: Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 46),
            danger: Color32::from_rgb(255, 106, 118),
            warning: Color32::from_rgb(255, 193, 94),
            success: Color32::from_rgb(90, 214, 160),
            shadow: Color32::from_black_alpha(90),
        }
    } else {
        ThemeColors {
            bg: Color32::from_rgb(242, 245, 251),
            bg2: Color32::from_rgb(235, 239, 247),
            panel: Color32::from_rgb(252, 253, 255),
            card: Color32::from_rgb(255, 255, 255),
            card_hover: Color32::from_rgb(238, 243, 252),
            line: Color32::from_rgb(220, 226, 238),
            text: Color32::from_rgb(26, 33, 48),
            text_weak: Color32::from_rgb(91, 103, 124),
            text_faint: Color32::from_rgb(132, 143, 162),
            accent,
            accent_soft: Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 38),
            danger: Color32::from_rgb(214, 63, 78),
            warning: Color32::from_rgb(190, 128, 19),
            success: Color32::from_rgb(30, 150, 105),
            shadow: Color32::from_black_alpha(30),
        }
    }
}

pub fn apply(ctx: &egui::Context, settings: &Settings) {
    let dark = is_dark(ctx, settings);
    let c = colors(ctx, settings);
    let radius = settings.theme.roundness;
    let mut style = (*ctx.global_style()).clone();
    let mut v = if dark { Visuals::dark() } else { Visuals::light() };

    v.dark_mode = dark;
    v.panel_fill = c.bg;
    v.window_fill = c.panel;
    v.extreme_bg_color = if dark {
        Color32::from_rgb(13, 17, 26)
    } else {
        Color32::from_rgb(240, 243, 249)
    };
    v.faint_bg_color = c.card;
    v.code_bg_color = c.card;
    v.hyperlink_color = c.accent;
    v.window_corner_radius = CornerRadius::same(radius);
    v.menu_corner_radius = CornerRadius::same(radius.min(12));
    v.selection = egui::style::Selection {
        bg_fill: c.accent_soft,
        stroke: Stroke::new(1.0, c.text),
    };
    v.override_text_color = Some(c.text);

    v.widgets.noninteractive.bg_fill = c.panel;
    v.widgets.noninteractive.weak_bg_fill = c.panel;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, c.line);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, c.text);
    v.widgets.noninteractive.corner_radius = CornerRadius::same(radius.min(10));

    v.widgets.inactive.bg_fill = c.card;
    v.widgets.inactive.weak_bg_fill = c.card;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, c.line);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, c.text);
    v.widgets.inactive.corner_radius = CornerRadius::same(radius.min(10));

    v.widgets.hovered.bg_fill = c.card_hover;
    v.widgets.hovered.weak_bg_fill = c.card_hover;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, c.accent);
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, c.text);
    v.widgets.hovered.corner_radius = CornerRadius::same(radius.min(10));

    v.widgets.active.bg_fill = c.accent_soft;
    v.widgets.active.weak_bg_fill = c.accent_soft;
    v.widgets.active.bg_stroke = Stroke::new(1.0, c.accent);
    v.widgets.active.fg_stroke = Stroke::new(1.0, c.text);
    v.widgets.active.corner_radius = CornerRadius::same(radius.min(10));

    v.widgets.open.bg_fill = c.card;
    v.widgets.open.weak_bg_fill = c.card;
    v.widgets.open.bg_stroke = Stroke::new(1.0, c.accent);
    v.widgets.open.fg_stroke = Stroke::new(1.0, c.text);
    v.widgets.open.corner_radius = CornerRadius::same(radius.min(10));

    style.visuals = v;
    style.spacing.item_spacing = vec2(8.0, 8.0);
    style.spacing.button_padding = vec2(12.0, 6.0);
    style.spacing.interact_size = vec2(32.0, 30.0);
    style.spacing.window_margin = Margin::symmetric(12, 10);
    style.spacing.menu_margin = Margin::symmetric(8, 6);
    style.spacing.slider_width = 120.0;

    let base = settings.theme.font_size.clamp(10.0, 26.0);
    let mut text_styles = std::collections::BTreeMap::new();
    text_styles.insert(
        TextStyle::Heading,
        FontId::new(base * 1.65, FontFamily::Proportional),
    );
    text_styles.insert(
        TextStyle::Body,
        FontId::new(base, FontFamily::Proportional),
    );
    text_styles.insert(
        TextStyle::Button,
        FontId::new(base, FontFamily::Proportional),
    );
    text_styles.insert(
        TextStyle::Small,
        FontId::new((base * 0.82).max(9.0), FontFamily::Proportional),
    );
    text_styles.insert(
        TextStyle::Monospace,
        FontId::new(base * 0.92, FontFamily::Monospace),
    );
    style.text_styles = text_styles;

    ctx.set_global_style(style);
    ctx.set_theme(if dark {
        egui::ThemePreference::Dark
    } else {
        egui::ThemePreference::Light
    });
}
