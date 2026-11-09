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
}

impl Default for ThemeColors {
    fn default() -> Self {
        Self {
            bg: Color32::from_rgb(10, 13, 20),
            bg2: Color32::from_rgb(15, 20, 30),
            panel: Color32::from_rgb(20, 26, 38),
            card: Color32::from_rgb(27, 34, 49),
            card_hover: Color32::from_rgb(36, 46, 65),
            line: Color32::from_rgb(42, 52, 70),
            text: Color32::from_rgb(232, 237, 247),
            text_weak: Color32::from_rgb(154, 164, 184),
            text_faint: Color32::from_rgb(108, 118, 138),
            accent: Color32::from_rgb(96, 165, 250),
            accent_soft: Color32::from_rgba_unmultiplied(96, 165, 250, 46),
            danger: Color32::from_rgb(255, 106, 118),
            warning: Color32::from_rgb(255, 193, 94),
            success: Color32::from_rgb(90, 214, 160),
        }
    }
}

pub fn is_dark(ctx: &egui::Context, settings: &Settings) -> bool {
    match settings.theme.mode {
        ThemeMode::Dark => true,
        ThemeMode::Light => false,
        ThemeMode::System => ctx.system_theme().map(|t| t == Theme::Dark).unwrap_or(true),
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
        }
    }
}

pub fn apply(ctx: &egui::Context, settings: &Settings) {
    // Accessibility zoom multiplies the OS DPI scale; egui applies it on top
    // of the native pixels-per-point.
    ctx.set_zoom_factor(settings.theme.ui_scale.clamp(0.75, 1.6));
    let dark = is_dark(ctx, settings);
    // Select the theme *before* reading/applying its style. Doing it the other
    // way round leaves the custom style on the previous theme, so the first
    // dark/light switch renders with egui's default colors (and can alter
    // control metrics) until the next switch.
    ctx.set_theme(if dark {
        egui::ThemePreference::Dark
    } else {
        egui::ThemePreference::Light
    });
    let c = colors(ctx, settings);
    let radius = settings.theme.roundness;
    let mut style = (*ctx.global_style()).clone();
    let mut v = if dark {
        Visuals::dark()
    } else {
        Visuals::light()
    };

    v.dark_mode = dark;
    v.panel_fill = c.bg;
    v.window_fill = c.panel;
    v.extreme_bg_color = if dark {
        Color32::from_rgb(20, 25, 36)
    } else {
        Color32::from_rgb(245, 247, 252)
    };
    v.faint_bg_color = c.card;
    v.code_bg_color = c.card;
    v.hyperlink_color = c.accent;
    v.window_corner_radius = CornerRadius::same(radius);
    v.menu_corner_radius = CornerRadius::same(radius.min(3));
    v.selection = egui::style::Selection {
        bg_fill: c.accent_soft,
        stroke: Stroke::new(1.0, c.text),
    };
    v.override_text_color = Some(c.text);

    v.widgets.noninteractive.bg_fill = c.panel;
    v.widgets.noninteractive.weak_bg_fill = c.panel;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, c.line);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, c.text);
    v.widgets.noninteractive.corner_radius = CornerRadius::same(radius.min(3));

    // The inactive `bg_fill` is used by slider rails and progress bars;
    // keeping it identical to the card color made the track invisible.
    v.widgets.inactive.bg_fill = if dark {
        Color32::from_rgb(50, 62, 84)
    } else {
        Color32::from_rgb(203, 212, 228)
    };
    v.widgets.inactive.weak_bg_fill = if dark {
        Color32::from_rgb(21, 27, 40)
    } else {
        Color32::from_rgb(247, 249, 253)
    };
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, c.line);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, c.text);
    v.widgets.inactive.corner_radius = CornerRadius::same(radius.min(3));

    v.widgets.hovered.bg_fill = c.card_hover;
    v.widgets.hovered.weak_bg_fill = c.card_hover;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, c.accent);
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, c.text);
    v.widgets.hovered.corner_radius = CornerRadius::same(radius.min(3));

    v.widgets.active.bg_fill = c.accent_soft;
    v.widgets.active.weak_bg_fill = c.accent_soft;
    v.widgets.active.bg_stroke = Stroke::new(1.0, c.accent);
    v.widgets.active.fg_stroke = Stroke::new(1.0, c.text);
    v.widgets.active.corner_radius = CornerRadius::same(radius.min(3));

    v.widgets.open.bg_fill = c.card;
    v.widgets.open.weak_bg_fill = c.card;
    v.widgets.open.bg_stroke = Stroke::new(1.0, c.accent);
    v.widgets.open.fg_stroke = Stroke::new(1.0, c.text);
    v.widgets.open.corner_radius = CornerRadius::same(radius.min(3));

    // Keyboard focus uses the `active` visuals (see `Widgets::style`). Keep
    // every state's geometry identical so focusing a button or combo box can
    // never change its size and trigger a relayout.
    //
    // `Frame::outer_rect = content + inner_margin + stroke + outer_margin`
    // and `inner_margin = button_padding + expansion - stroke`. With
    // `expansion = stroke = 1`, every state occupies `content + padding` and
    // the border is a virtual 1 px drawn outside the layout box. Hovering
    // therefore shows an outline without changing width or moving the text.
    for widget in [
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        widget.expansion = 1.0;
        widget.bg_stroke.width = 1.0;
        widget.fg_stroke.width = 1.0;
        widget.corner_radius = CornerRadius::same(radius.min(3));
    }
    v.widgets.noninteractive.expansion = 0.0;
    v.widgets.noninteractive.fg_stroke.width = 1.0;
    v.widgets.noninteractive.corner_radius = CornerRadius::same(radius.min(3));

    style.visuals = v;
    style.spacing.item_spacing = vec2(8.0, 8.0);
    style.spacing.button_padding = vec2(14.0, 7.0);
    style.spacing.interact_size = vec2(34.0, 32.0);
    style.spacing.window_margin = Margin::symmetric(12, 10);
    style.spacing.menu_margin = Margin::symmetric(10, 8);
    // Reserve room for scroll bars so popup items are not painted under
    // them, and add a small inset so the virtual hover border on the first
    // and last item is not clipped by the scroll viewport.
    style.spacing.scroll.floating = true;
    // Reserve space for the floating bar so it never covers popup items,
    // while keeping the bar hidden until the user scrolls/hovers.
    style.spacing.scroll.floating_allocated_width = 10.0;
    style.spacing.scroll.bar_width = 8.0;
    style.spacing.scroll.bar_inner_margin = 4.0;
    style.spacing.scroll.content_margin = Margin::symmetric(4, 4);
    style.spacing.slider_width = 140.0;
    // Fixed combo width: selecting a longer item must not resize the row.
    style.spacing.combo_width = 170.0;

    let base = settings.theme.font_size.clamp(10.0, 26.0);
    let mut text_styles = std::collections::BTreeMap::new();
    text_styles.insert(
        TextStyle::Heading,
        FontId::new(base * 1.65, FontFamily::Proportional),
    );
    text_styles.insert(TextStyle::Body, FontId::new(base, FontFamily::Proportional));
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

    // Store the fully-built style for the target theme itself, then make it
    // the active style. Applying only the "global" style leaves the other
    // theme with egui's defaults, which is why the first dark/light switch
    // used to show inverted colors and different control metrics.
    let theme = if dark {
        egui::Theme::Dark
    } else {
        egui::Theme::Light
    };
    ctx.set_style_of(theme, style.clone());
    ctx.set_global_style(style);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ThemeMode;

    #[test]
    fn switching_theme_keeps_custom_metrics_and_colors() {
        let ctx = egui::Context::default();
        let mut settings = Settings::default();
        settings.theme.mode = ThemeMode::Dark;
        apply(&ctx, &settings);
        assert!(ctx.global_style().visuals.dark_mode);
        assert_eq!(ctx.global_style().spacing.interact_size, vec2(34.0, 32.0));

        settings.theme.mode = ThemeMode::Light;
        apply(&ctx, &settings);
        assert!(!ctx.global_style().visuals.dark_mode);
        assert_eq!(ctx.global_style().spacing.interact_size, vec2(34.0, 32.0));
        let light = ctx.style_of(egui::Theme::Light);
        assert_eq!(light.spacing.interact_size, vec2(34.0, 32.0));
        assert!(!light.visuals.dark_mode);

        settings.theme.mode = ThemeMode::Dark;
        apply(&ctx, &settings);
        assert!(ctx.global_style().visuals.dark_mode);
        let dark = ctx.style_of(egui::Theme::Dark);
        assert_eq!(dark.spacing.interact_size, vec2(34.0, 32.0));
        assert!(dark.visuals.dark_mode);
    }

    #[test]
    fn focus_does_not_change_control_geometry() {
        let ctx = egui::Context::default();
        let settings = Settings::default();
        apply(&ctx, &settings);
        let widgets = &ctx.global_style().visuals.widgets;
        let base = widgets.inactive.corner_radius;
        let padding = ctx.global_style().spacing.button_padding;
        for state in [
            &widgets.inactive,
            &widgets.hovered,
            &widgets.active,
            &widgets.open,
        ] {
            assert_eq!(state.expansion, 1.0);
            assert_eq!(state.corner_radius, base);
            assert_eq!(state.bg_stroke.width, 1.0);
            assert_eq!(state.fg_stroke.width, 1.0);
            // Virtual border: inner + stroke - expansion == button padding.
            let outer = vec2(
                padding.x + state.expansion - state.bg_stroke.width + state.bg_stroke.width
                    - state.expansion,
                padding.y + state.expansion - state.bg_stroke.width + state.bg_stroke.width
                    - state.expansion,
            );
            assert!((outer - padding).length() < 1e-4);
        }
        assert_eq!(widgets.noninteractive.bg_stroke.width, 1.0);
    }
}
