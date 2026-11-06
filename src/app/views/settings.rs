//! Settings page: appearance, playback, lyrics, desktop lyrics, sources,
//! window behaviour, feature switches and about.

use egui::{RichText, Ui};

use crate::app::{App, ToastKind, widgets};
use crate::config::{
    BackgroundMode, CloseBehavior, DesktopBackgroundMode, EqPreset, Language, LyricsEncoding,
    ProxyMode, Quality, ReplayGainMode, SourceKind, ThemeMode, WgpuBackend,
};
use crate::platform::window;

pub fn settings(app: &mut App, ui: &mut Ui) {
    let colors = app.colors();
    widgets::page_header(
        ui,
        app.t("settings.title"),
        "所有选项即时保存；增强功能可逐项关闭",
        &colors,
        |_ui| {},
    );

    let mut appearance_changed = false;
    let mut settings_changed = false;

    // ---------------------------------------------------------------- Appearance
    cards(ui, &colors, app.t("settings.appearance"), |ui| {
        row(ui, app.t("settings.theme"), &colors, |ui| {
            let mut mode = app.settings.theme.mode;
            egui::ComboBox::from_id_salt("theme-mode")
                .selected_text(theme_label(mode))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut mode, ThemeMode::System, "跟随系统");
                    ui.selectable_value(&mut mode, ThemeMode::Dark, "深色");
                    ui.selectable_value(&mut mode, ThemeMode::Light, "浅色");
                });
            if mode != app.settings.theme.mode {
                app.settings.theme.mode = mode;
                appearance_changed = true;
            }
        });
        row(ui, app.t("settings.accent"), &colors, |ui| {
            if ui
                .color_edit_button_srgb(&mut app.settings.theme.accent)
                .changed()
            {
                appearance_changed = true;
            }
            ui.label(
                RichText::new(format!(
                    "#{:02X}{:02X}{:02X}",
                    app.settings.theme.accent[0],
                    app.settings.theme.accent[1],
                    app.settings.theme.accent[2]
                ))
                .size(11.0)
                .color(colors.text_faint),
            );
        });
        row(ui, app.t("settings.background"), &colors, |ui| {
            let mut bg = app.settings.theme.background;
            egui::ComboBox::from_id_salt("background-mode")
                .selected_text(background_label(bg))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut bg, BackgroundMode::StaticBlur, "静态模糊封面（省电）");
                    ui.selectable_value(&mut bg, BackgroundMode::GpuBlur, "GPU 模糊封面");
                    ui.selectable_value(&mut bg, BackgroundMode::Flow, "流动渐变（30fps）");
                    ui.selectable_value(&mut bg, BackgroundMode::Solid, "纯色/渐变");
                    ui.selectable_value(&mut bg, BackgroundMode::Image, "自定义图片");
                    ui.selectable_value(&mut bg, BackgroundMode::Mica, "Mica 云母（Win11）");
                });
            if bg != app.settings.theme.background {
                app.settings.theme.background = bg;
                appearance_changed = true;
            }
        });
        if app.settings.theme.background == BackgroundMode::Image {
            row(ui, "背景图片", &colors, |ui| {
                let label = app
                    .settings
                    .theme
                    .background_image
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "未选择".to_string());
                ui.label(RichText::new(crate::util::truncate_chars(&label, 46)).size(11.0).color(colors.text_weak));
                if ui.button(app.t("common.browse")).clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("图片", &["png", "jpg", "jpeg", "webp", "bmp"])
                        .pick_file()
                    {
                        app.settings.theme.background_image = Some(path);
                        appearance_changed = true;
                    }
                }
            });
        }
        row(ui, app.t("settings.font"), &colors, |ui| {
            let mut family = app
                .settings
                .theme
                .font_family
                .clone()
                .unwrap_or_else(|| "Microsoft YaHei UI".to_string());
            if ui
                .add(
                    egui::TextEdit::singleline(&mut family)
                        .desired_width(ui.available_width().min(240.0).max(120.0)),
                )
                .changed()
            {
                app.settings.theme.font_family = if family.trim().is_empty() {
                    None
                } else {
                    Some(family)
                };
                appearance_changed = true;
            }
            if ui.button("选择字体文件").clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("字体", &["ttf", "otf", "ttc"])
                    .pick_file()
                {
                    app.settings.theme.font_family = Some(path.display().to_string());
                    appearance_changed = true;
                }
            }
        });
        row(ui, app.t("settings.font_size"), &colors, |ui| {
            if ui
                .add(egui::Slider::new(&mut app.settings.theme.font_size, 10.0..=22.0).suffix(" px"))
                .changed()
            {
                appearance_changed = true;
            }
        });
        row(ui, app.t("settings.roundness"), &colors, |ui| {
            let mut value = app.settings.theme.roundness as f32;
            if ui
                .add(egui::Slider::new(&mut value, 0.0..=20.0).integer())
                .changed()
            {
                app.settings.theme.roundness = value as u8;
                appearance_changed = true;
            }
        });
        row(ui, app.t("settings.animations"), &colors, |ui| {
            if ui
                .checkbox(&mut app.settings.theme.animations, "启用界面动效")
                .changed()
            {
                settings_changed = true;
            }
            if ui
                .checkbox(&mut app.settings.theme.reduce_motion, "减少动效")
                .changed()
            {
                settings_changed = true;
            }
        });
        row(ui, "渲染后端", &colors, |ui| {
            let mut backend = app.settings.theme.backend;
            egui::ComboBox::from_id_salt("wgpu-backend")
                .selected_text(backend_label(backend))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut backend, WgpuBackend::Auto, "自动（DX12 优先）");
                    ui.selectable_value(&mut backend, WgpuBackend::Dx12, "DX12");
                    ui.selectable_value(&mut backend, WgpuBackend::Vulkan, "Vulkan");
                    ui.selectable_value(&mut backend, WgpuBackend::Gl, "OpenGL（低内存）");
                });
            if backend != app.settings.theme.backend {
                app.settings.theme.backend = backend;
                settings_changed = true;
                app.toast("渲染后端将在下次启动时生效", ToastKind::Info);
            }
            if ui
                .button("低内存")
                .on_hover_text("切换到 OpenGL 后端，私有内存可降到约 60 MB；需重启生效")
                .clicked()
            {
                app.settings.theme.backend = WgpuBackend::Gl;
                settings_changed = true;
                app.toast("已选择 OpenGL 低内存后端，请重启应用", ToastKind::Info);
            }
            ui.add(
                egui::Label::new(
                    RichText::new(&app.gpu_info)
                        .size(10.0)
                        .color(colors.text_faint),
                )
                .wrap(),
            );
        });
        row(ui, app.t("settings.language"), &colors, |ui| {
            let mut lang = app.settings.language;
            egui::ComboBox::from_id_salt("language")
                .selected_text(match lang {
                    Language::ZhCn => "简体中文",
                    Language::EnUs => "English",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut lang, Language::ZhCn, "简体中文");
                    ui.selectable_value(&mut lang, Language::EnUs, "English");
                });
            if lang != app.settings.language {
                app.settings.language = lang;
                app.lang = lang;
                app.tray = None; // rebuild labels next frame
                settings_changed = true;
            }
        });
    });
    ui.add_space(12.0);

    // ---------------------------------------------------------------- Playback
    cards(ui, &colors, app.t("settings.playback"), |ui| {
        ui.label(
            RichText::new(format!(
                "{}: {} · {} Hz · {} ch · {}",
                app.t("settings.device"),
                app.device_info.device_name,
                app.device_info.sample_rate,
                app.device_info.channels,
                app.device_info.sample_format
            ))
            .size(11.0)
            .color(colors.text_weak),
        );
        row(ui, app.t("settings.volume"), &colors, |ui| {
            let mut volume = app.volume();
            if ui
                .add(egui::Slider::new(&mut volume, 0.0..=1.0).show_value(false))
                .changed()
            {
                app.set_volume(volume);
            }
            ui.label(
                RichText::new(format!("{:.0}%", app.volume() * 100.0))
                    .size(11.0)
                    .color(colors.text_faint),
            );
        });
        row(ui, app.t("settings.auto_advance"), &colors, |ui| {
            if ui
                .checkbox(&mut app.settings.playback.auto_advance, "播放结束后自动下一首")
                .changed()
            {
                settings_changed = true;
            }
        });
        row(ui, app.t("settings.resume"), &colors, |ui| {
            if ui
                .checkbox(&mut app.settings.playback.resume_on_start, "启动时继续播放")
                .changed()
            {
                settings_changed = true;
            }
        });
        row(ui, app.t("settings.gapless"), &colors, |ui| {
            if ui
                .checkbox(&mut app.settings.playback.gapless, "无缝播放（阶段 7 生效）")
                .changed()
            {
                settings_changed = true;
            }
        });
        row(ui, app.t("settings.crossfade"), &colors, |ui| {
            let mut value = app.settings.playback.crossfade_secs;
            if ui
                .add_enabled(
                    app.settings.features.crossfade,
                    egui::Slider::new(&mut value, 0.0..=12.0).suffix(" s"),
                )
                .changed()
            {
                app.settings.playback.crossfade_secs = value;
                settings_changed = true;
            }
        });
        row(ui, app.t("settings.seek_step"), &colors, |ui| {
            let mut value = app.settings.playback.seek_step_secs;
            if ui
                .add(egui::Slider::new(&mut value, 1.0..=30.0).suffix(" s"))
                .changed()
            {
                app.settings.playback.seek_step_secs = value;
                settings_changed = true;
            }
        });
        row(ui, app.t("settings.replaygain"), &colors, |ui| {
            let mut mode = app.settings.playback.replaygain;
            egui::ComboBox::from_id_salt("replaygain")
                .selected_text(replaygain_label(mode))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut mode, ReplayGainMode::Off, "关闭");
                    ui.selectable_value(&mut mode, ReplayGainMode::Track, "按曲目");
                    ui.selectable_value(&mut mode, ReplayGainMode::Album, "按专辑");
                });
            if mode != app.settings.playback.replaygain {
                app.settings.playback.replaygain = mode;
                settings_changed = true;
            }
            ui.checkbox(&mut app.settings.features.replaygain, "启用模块");
        });
        row(ui, app.t("settings.eq"), &colors, |ui| {
            let mut enabled = app.settings.playback.eq_enabled;
            if ui.checkbox(&mut enabled, "启用 10 段均衡器").changed() {
                app.settings.playback.eq_enabled = enabled;
                settings_changed = true;
            }
            let mut preset = app.settings.playback.eq_preset;
            egui::ComboBox::from_id_salt("eq-preset")
                .selected_text(eq_label(preset))
                .show_ui(ui, |ui| {
                    for p in [
                        EqPreset::Flat,
                        EqPreset::Rock,
                        EqPreset::Pop,
                        EqPreset::Jazz,
                        EqPreset::Classical,
                        EqPreset::Vocal,
                        EqPreset::Bass,
                    ] {
                        ui.selectable_value(&mut preset, p, eq_label(p));
                    }
                });
            if preset != app.settings.playback.eq_preset {
                app.settings.playback.eq_preset = preset;
                settings_changed = true;
            }
        });
        row(ui, app.t("settings.speed"), &colors, |ui| {
            let mut speed = app.settings.playback.speed;
            if ui
                .add(
                    egui::Slider::new(&mut speed, 0.5..=2.0)
                        .suffix("x")
                        .fixed_decimals(2),
                )
                .changed()
            {
                app.settings.playback.speed = speed;
                settings_changed = true;
            }
            ui.checkbox(
                &mut app.settings.playback.pitch_preserve,
                "保持音调（阶段 7 生效）",
            );
        });
    });
    ui.add_space(12.0);

    // ---------------------------------------------------------------- Lyrics
    cards(ui, &colors, app.t("settings.lyrics"), |ui| {
        row(ui, app.t("settings.encoding"), &colors, |ui| {
            let mut enc = app.settings.lyrics.encoding;
            egui::ComboBox::from_id_salt("lyrics-encoding")
                .selected_text(encoding_label(enc))
                .show_ui(ui, |ui| {
                    for e in [
                        LyricsEncoding::Auto,
                        LyricsEncoding::Utf8,
                        LyricsEncoding::Gbk,
                        LyricsEncoding::Gb18030,
                        LyricsEncoding::Utf16Le,
                        LyricsEncoding::Utf16Be,
                        LyricsEncoding::Big5,
                    ] {
                        ui.selectable_value(&mut enc, e, encoding_label(e));
                    }
                });
            if enc != app.settings.lyrics.encoding {
                app.settings.lyrics.encoding = enc;
                settings_changed = true;
            }
        });
        row(ui, app.t("settings.offset"), &colors, |ui| {
            let mut offset = app.settings.lyrics.offset_ms;
            if ui
                .add(
                    egui::DragValue::new(&mut offset)
                        .speed(50.0)
                        .suffix(" ms")
                        .range(-30_000..=30_000),
                )
                .changed()
            {
                app.settings.lyrics.offset_ms = offset;
                settings_changed = true;
            }
        });
        row(ui, app.t("settings.font_size"), &colors, |ui| {
            let mut size = app.settings.lyrics.font_size;
            if ui
                .add(egui::Slider::new(&mut size, 12.0..=40.0).suffix(" px"))
                .changed()
            {
                app.settings.lyrics.font_size = size;
                settings_changed = true;
            }
        });
        row(ui, app.t("settings.show_translation"), &colors, |ui| {
            if ui
                .checkbox(&mut app.settings.lyrics.show_translation, "双语歌词")
                .changed()
            {
                settings_changed = true;
            }
            if ui
                .checkbox(&mut app.settings.lyrics.karaoke, crate::i18n::t(app.lang, "settings.karaoke"))
                .changed()
            {
                settings_changed = true;
            }
        });
        row(ui, app.t("settings.sidebar"), &colors, |ui| {
            if ui
                .checkbox(
                    &mut app.settings.lyrics.sidebar_visible,
                    "在右侧常驻歌词栏",
                )
                .changed()
            {
                app.show_lyrics_sidebar = app.settings.lyrics.sidebar_visible;
                settings_changed = true;
            }
            if ui
                .checkbox(&mut app.settings.lyrics.auto_load, "自动查找歌词")
                .changed()
            {
                settings_changed = true;
            }
        });
        row(ui, "歌词颜色", &colors, |ui| {
            ui.label(RichText::new("当前行").size(11.0).color(colors.text_weak));
            ui.color_edit_button_srgb(&mut app.settings.lyrics.current_color);
            ui.label(RichText::new("普通行").size(11.0).color(colors.text_weak));
            ui.color_edit_button_srgb(&mut app.settings.lyrics.normal_color);
        });
        ui.label(
            RichText::new("LRC/SRT 解析、逐字高亮与偏移将在歌词阶段接入")
                .size(10.5)
                .color(colors.text_faint),
        );
    });
    ui.add_space(12.0);

    // ---------------------------------------------------------------- Desktop lyrics
    cards(ui, &colors, app.t("settings.desktop_lyrics"), |ui| {
        row(ui, "行为", &colors, |ui| {
            if ui
                .checkbox(
                    &mut app.settings.desktop_lyrics.visible_at_start,
                    "显示桌面歌词",
                )
                .changed()
            {
                settings_changed = true;
            }
            if ui
                .checkbox(&mut app.settings.desktop_lyrics.always_on_top, crate::i18n::t(app.lang, "settings.always_on_top"))
                .changed()
            {
                settings_changed = true;
            }
            if ui
                .checkbox(&mut app.settings.desktop_lyrics.locked, crate::i18n::t(app.lang, "settings.locked"))
                .changed()
            {
                settings_changed = true;
            }
            if ui
                .checkbox(&mut app.settings.desktop_lyrics.click_through, crate::i18n::t(app.lang, "settings.click_through"))
                .changed()
            {
                settings_changed = true;
            }
        });
        row(ui, "外观", &colors, |ui| {
            let mut lines = app.settings.desktop_lyrics.lines as f32;
            if ui
                .add(egui::Slider::new(&mut lines, 1.0..=5.0).integer().prefix("行数 "))
                .changed()
            {
                app.settings.desktop_lyrics.lines = lines as u8;
                settings_changed = true;
            }
            let mut size = app.settings.desktop_lyrics.font_size;
            if ui
                .add(egui::Slider::new(&mut size, 16.0..=72.0).suffix(" px"))
                .changed()
            {
                app.settings.desktop_lyrics.font_size = size;
                settings_changed = true;
            }
            if ui
                .checkbox(&mut app.settings.desktop_lyrics.show_footer, crate::i18n::t(app.lang, "settings.show_footer"))
                .changed()
            {
                settings_changed = true;
            }
            if ui
                .checkbox(&mut app.settings.desktop_lyrics.show_translation, crate::i18n::t(app.lang, "settings.show_translation"))
                .changed()
            {
                settings_changed = true;
            }
        });
        row(ui, "背景", &colors, |ui| {
            let mut bg = app.settings.desktop_lyrics.background;
            egui::ComboBox::from_id_salt("desktop-bg")
                .selected_text(desktop_bg_label(bg))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut bg, DesktopBackgroundMode::Transparent, app.t("settings.bg_transparent"));
                    ui.selectable_value(&mut bg, DesktopBackgroundMode::Solid, app.t("settings.bg_solid"));
                    ui.selectable_value(&mut bg, DesktopBackgroundMode::Gradient, app.t("settings.bg_gradient"));
                    ui.selectable_value(&mut bg, DesktopBackgroundMode::Image, app.t("settings.bg_image"));
                    ui.selectable_value(&mut bg, DesktopBackgroundMode::BlurCover, app.t("settings.bg_blur"));
                });
            if bg != app.settings.desktop_lyrics.background {
                app.settings.desktop_lyrics.background = bg;
                settings_changed = true;
            }
            let mut bg_color = egui::Color32::from_rgba_unmultiplied(
                app.settings.desktop_lyrics.bg_color[0],
                app.settings.desktop_lyrics.bg_color[1],
                app.settings.desktop_lyrics.bg_color[2],
                app.settings.desktop_lyrics.bg_color[3],
            );
            if ui.color_edit_button_srgba(&mut bg_color).changed() {
                app.settings.desktop_lyrics.bg_color = bg_color.to_array();
                settings_changed = true;
            }
        });
        row(ui, "文字颜色", &colors, |ui| {
            ui.label(RichText::new("当前").size(11.0).color(colors.text_weak));
            let mut current = [
                app.settings.desktop_lyrics.current_color[0],
                app.settings.desktop_lyrics.current_color[1],
                app.settings.desktop_lyrics.current_color[2],
            ];
            if ui.color_edit_button_srgb(&mut current).changed() {
                app.settings.desktop_lyrics.current_color[..3].copy_from_slice(&current);
                settings_changed = true;
            }
            ui.label(RichText::new("下一行").size(11.0).color(colors.text_weak));
            let mut next = [
                app.settings.desktop_lyrics.next_color[0],
                app.settings.desktop_lyrics.next_color[1],
                app.settings.desktop_lyrics.next_color[2],
            ];
            if ui.color_edit_button_srgb(&mut next).changed() {
                app.settings.desktop_lyrics.next_color[..3].copy_from_slice(&next);
                settings_changed = true;
            }
        });
        row(ui, "捕获保护", &colors, |ui| {
            if ui
                .checkbox(
                    &mut app.settings.desktop_lyrics.capture_protection,
                    crate::i18n::t(app.lang, "settings.capture_protection"),
                )
                .changed()
            {
                settings_changed = true;
            }
            ui.label(
                RichText::new(app.t("settings.capture_hint"))
                    .size(10.0)
                    .color(colors.text_faint),
            );
        });
        ui.label(
            RichText::new("桌面歌词窗口、拖动/锁定/穿透与逐字高亮将在阶段 5 接入")
                .size(10.5)
                .color(colors.text_faint),
        );
    });
    ui.add_space(12.0);

    // ---------------------------------------------------------------- Online sources
    cards(ui, &colors, app.t("settings.online"), |ui| {
        row(ui, app.t("settings.timeout"), &colors, |ui| {
            let mut t = app.settings.network.timeout_secs;
            if ui.add(egui::DragValue::new(&mut t).range(2..=120).suffix(" s")).changed() {
                app.settings.network.timeout_secs = t;
                settings_changed = true;
            }
            let mut retries = app.settings.network.retries;
            if ui.add(egui::DragValue::new(&mut retries).range(0..=10)).changed() {
                app.settings.network.retries = retries;
                settings_changed = true;
            }
        });
        row(ui, app.t("settings.proxy"), &colors, |ui| {
            let mut mode = app.settings.network.proxy_mode;
            egui::ComboBox::from_id_salt("proxy-mode")
                .selected_text(proxy_label(mode))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut mode, ProxyMode::System, "系统代理");
                    ui.selectable_value(&mut mode, ProxyMode::None, "不使用代理");
                    ui.selectable_value(&mut mode, ProxyMode::Manual, "手动");
                });
            if mode != app.settings.network.proxy_mode {
                app.settings.network.proxy_mode = mode;
                settings_changed = true;
            }
            if app.settings.network.proxy_mode == ProxyMode::Manual {
                ui.add(
                    egui::TextEdit::singleline(&mut app.settings.network.proxy_url)
                        .hint_text("http://127.0.0.1:7890")
                        .desired_width(220.0),
                );
            }
        });
        row(ui, "离线模式", &colors, |ui| {
            if ui
                .checkbox(&mut app.settings.network.offline, "仅播放本地/已缓存内容")
                .changed()
            {
                settings_changed = true;
            }
            let mut limit = app.settings.network.cache_limit_mb;
            if ui
                .add(
                    egui::DragValue::new(&mut limit)
                        .range(64..=102_400)
                        .suffix(" MB"),
                )
                .changed()
            {
                app.settings.network.cache_limit_mb = limit;
                settings_changed = true;
            }
        });

        ui.add_space(8.0);
        let mut test_source: Option<String> = None;
        let count = app.settings.sources.len();
        let label_test = app.t("settings.test").to_string();
        let label_base = app.t("settings.base_url").to_string();
        let label_quality = app.t("settings.quality").to_string();
        let label_session = app.t("settings.session_key").to_string();
        let label_insecure = app.t("settings.insecure_tls").to_string();
        for i in 0..count {
            let id = app.settings.sources[i].id.clone();
            let name = app.settings.sources[i].name.clone();
            let kind = app.settings.sources[i].kind;
            ui.push_id(("source", i), |ui| {
                let source = &mut app.settings.sources[i];
                egui::Frame::new()
                    .fill(colors.bg2)
                    .corner_radius(egui::CornerRadius::same(10))
                    .inner_margin(egui::Margin::symmetric(12, 10))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            if ui.checkbox(&mut source.enabled, "").changed() {
                                settings_changed = true;
                            }
                            ui.label(
                                RichText::new(format!("{name}  ·  {}", source_kind_label(kind)))
                                    .size(13.0)
                                    .strong()
                                    .color(colors.text),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.button(&label_test).clicked() {
                                        test_source = Some(id.clone());
                                    }
                                },
                            );
                        });
                        row(ui, &label_base, &colors, |ui| {
                            if ui
                                .add(
                                    egui::TextEdit::singleline(&mut source.base_url)
                                        .desired_width(ui.available_width().max(140.0))
                                        .hint_text("填写 Bamboo API 地址"),
                                )
                                .changed()
                            {
                                settings_changed = true;
                            }
                        });
                        row(ui, "脚本", &colors, |ui| {
                            if ui
                                .add(
                                    egui::TextEdit::singleline(&mut source.script)
                                        .desired_width(160.0),
                                )
                                .changed()
                            {
                                settings_changed = true;
                            }
                            ui.add(
                                egui::Label::new(
                                    RichText::new("基址以 .php 结尾时直接使用")
                                        .size(10.0)
                                        .color(colors.text_faint),
                                )
                                .wrap(),
                            );
                        });
                        row(ui, &label_quality, &colors, |ui| {
                            let mut quality = source.quality;
                            egui::ComboBox::from_id_salt("quality")
                                .selected_text(quality.label())
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(&mut quality, Quality::K128, "128 kbps");
                                    ui.selectable_value(&mut quality, Quality::K320, "320 kbps");
                                    ui.selectable_value(&mut quality, Quality::Flac, "无损 FLAC");
                                });
                            if quality != source.quality {
                                source.quality = quality;
                                settings_changed = true;
                            }
                            if matches!(kind, SourceKind::Online) {
                                ui.add(
                                    egui::TextEdit::singleline(&mut source.session_key)
                                        .hint_text(&label_session)
                                        .password(true)
                                        .desired_width(150.0),
                                );
                            }
                        });
                        ui.horizontal(|ui| {
                            if ui
                                .checkbox(&mut source.insecure_tls, &label_insecure)
                                .changed()
                            {
                                settings_changed = true;
                            }
                            ui.label(
                                RichText::new("仅建议在本地测试环境使用")
                                    .size(10.0)
                                    .color(colors.text_faint),
                            );
                        });
                    });
            });
            ui.add_space(6.0);
        }
        if let Some(id) = test_source {
            app.toast(
                format!("连接测试（{id}）将在在线音源阶段启用"),
                ToastKind::Info,
            );
        }
        ui.label(
            RichText::new("在线音源不会预填地址；填写 Bamboo API 地址并启用后生效。")
                .size(10.5)
                .color(colors.text_faint),
        );
    });
    ui.add_space(12.0);

    // ---------------------------------------------------------------- Playlist
    cards(ui, &colors, "歌单", |ui| {
        row(ui, "导入行为", &colors, |ui| {
            if ui
                .checkbox(
                    &mut app.settings.playlist.auto_refresh_on_startup,
                    "启动时重新展开当前歌单",
                )
                .changed()
            {
                settings_changed = true;
            }
            if ui
                .checkbox(
                    &mut app.settings.playlist.export_utf8_bom,
                    "导出 UTF-8 BOM",
                )
                .changed()
            {
                settings_changed = true;
            }
        });
        if let Some(playlist) = app.current_playlist.clone() {
            long_value(ui, "当前歌单", &playlist.display().to_string(), &colors);
            ui.horizontal(|ui| {
                if widgets::secondary_button(ui, "重新扫描歌单", &colors).clicked() {
                    app.rescan_current_playlist();
                }
                if widgets::secondary_button(ui, "导出到文件", &colors).clicked() {
                    app.export_playlist_dialog();
                }
            });
        } else {
            ui.label(
                RichText::new("尚未打开 .mp3list 歌单")
                    .size(11.0)
                    .color(colors.text_faint),
            );
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if widgets::primary_button(ui, "导入歌单", &colors).clicked() {
                app.import_playlist_dialog();
            }
            if widgets::secondary_button(ui, "导出当前队列", &colors).clicked() {
                app.export_playlist_dialog();
            }
        });
        ui.add(
            egui::Label::new(
                RichText::new(
                    "每次导入都会重新扫描 #SEARCH# 目录；当前曲目按文件路径匹配，并用上次保存的播放位置恢复。",
                )
                .size(10.5)
                .color(colors.text_faint),
            )
            .wrap(),
        );
    });
    ui.add_space(12.0);

    // ---------------------------------------------------------------- Window & tray
    cards(ui, &colors, app.t("settings.window"), |ui| {
        row(ui, app.t("settings.close_behavior"), &colors, |ui| {
            let mut behavior = app.settings.window.close_behavior;
            egui::ComboBox::from_id_salt("close-behavior")
                .selected_text(close_label(behavior))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut behavior, CloseBehavior::Tray, app.t("settings.close_tray"));
                    ui.selectable_value(&mut behavior, CloseBehavior::Exit, app.t("settings.close_exit"));
                    ui.selectable_value(&mut behavior, CloseBehavior::Ask, app.t("settings.close_ask"));
                });
            if behavior != app.settings.window.close_behavior {
                app.settings.window.close_behavior = behavior;
                settings_changed = true;
            }
        });
        row(ui, "托盘", &colors, |ui| {
            if ui
                .checkbox(&mut app.settings.features.tray, "启用托盘图标")
                .changed()
            {
                app.tray = None;
                app.tray_failed = false;
                settings_changed = true;
            }
            if ui
                .checkbox(&mut app.settings.window.start_minimized, crate::i18n::t(app.lang, "settings.start_minimized"))
                .changed()
            {
                settings_changed = true;
            }
            if ui
                .checkbox(&mut app.settings.window.remember_position, "记住窗口位置")
                .changed()
            {
                settings_changed = true;
            }
            if ui
                .checkbox(
                    &mut app.settings.window.trim_working_set_on_idle,
                    "空闲时自动释放内存",
                )
                .changed()
            {
                settings_changed = true;
            }
        });
        row(ui, app.t("settings.capture_protection"), &colors, |ui| {
            if ui
                .checkbox(
                    &mut app.settings.window.capture_protection,
                    "主窗口不被录屏/截图捕获",
                )
                .changed()
            {
                if let Some(hwnd) = app.hwnd {
                    window::set_capture_protection(
                        hwnd,
                        app.settings.window.capture_protection,
                    );
                }
                settings_changed = true;
            }
            ui.label(
                RichText::new(app.t("settings.capture_hint"))
                    .size(10.0)
                    .color(colors.text_faint),
            );
        });
    });
    ui.add_space(12.0);

    // ---------------------------------------------------------------- Features
    cards(ui, &colors, app.t("settings.features"), |ui| {
        ui.label(
            RichText::new(app.t("settings.features_hint"))
                .size(11.0)
                .color(colors.text_weak),
        );
        ui.add_space(6.0);
        let f = &mut app.settings.features;
        let mut changed = false;
        macro_rules! toggle {
            ($ui:expr, $field:ident, $label:expr) => {
                if $ui.checkbox(&mut f.$field, $label).changed() {
                    changed = true;
                }
            };
        }
        egui::Grid::new("bamboo-feature-grid")
            .num_columns(2)
            .spacing([28.0, 8.0])
            .show(ui, |ui| {
                toggle!(ui, crossfade, "交叉淡化");
                toggle!(ui, discover, "发现页");
                ui.end_row();
                toggle!(ui, gapless, "无缝播放");
                toggle!(ui, advanced_network, "高级网络");
                ui.end_row();
                toggle!(ui, replaygain, "响度均衡");
                toggle!(ui, mini_player, "迷你播放器");
                ui.end_row();
                toggle!(ui, equalizer, "均衡器");
                toggle!(ui, fullscreen_lyrics, "全屏歌词");
                ui.end_row();
                toggle!(ui, speed_control, "变速播放");
                toggle!(ui, visualizer, "音频可视化");
                ui.end_row();
                toggle!(ui, ab_repeat, "A-B 循环");
                toggle!(ui, notifications, "系统通知");
                ui.end_row();
                toggle!(ui, sleep_timer, "睡眠定时");
                toggle!(ui, jump_list, "任务栏跳转列表");
                ui.end_row();
                toggle!(ui, internet_radio, "网络电台");
                toggle!(ui, start_with_windows, "开机启动");
                ui.end_row();
                toggle!(ui, smart_playlists, "智能歌单");
                toggle!(ui, file_associations, "文件关联");
                ui.end_row();
                toggle!(ui, statistics, "播放统计");
                toggle!(ui, command_palette, "命令面板");
                ui.end_row();
                toggle!(ui, duplicate_finder, "查重与缺失");
                toggle!(ui, accessibility, "无障碍");
                ui.end_row();
                toggle!(ui, lyric_editor, "歌词编辑器");
                toggle!(ui, desktop_lyrics, "桌面歌词");
                ui.end_row();
                toggle!(ui, downloads, "在线下载");
                ui.end_row();
                toggle!(ui, stream_cache, "流缓存");
                ui.end_row();
            });
        if changed {
            settings_changed = true;
        }
    });
    ui.add_space(12.0);

    // ---------------------------------------------------------------- About
    cards(ui, &colors, app.t("settings.about"), |ui| {
        widgets::key_value(ui, app.t("settings.version"), env!("CARGO_PKG_VERSION"), &colors);
        widgets::key_value(
            ui,
            app.t("settings.portable"),
            if app.paths.portable { "是" } else { "否" },
            &colors,
        );
        long_value(ui, "数据目录", &app.paths.root.display().to_string(), &colors);
        if let Some(mem) = app.memory_info {
            long_value(
                ui,
                "内存占用",
                &format!(
                    "{} 工作集 · {} 私有",
                    crate::platform::memory::format_mb(mem.working_set),
                    crate::platform::memory::format_mb(mem.private_usage)
                ),
                &colors,
            );
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button(app.t("settings.open_data_dir")).clicked() {
                app.open_data_dir();
            }
            if ui.button(app.t("settings.log_dir")).clicked() {
                app.open_log_dir();
            }
            if ui.button("释放内存").clicked() {
                crate::platform::memory::trim_working_set();
                app.memory_info = crate::platform::memory::process_memory();
                app.toast("已尝试释放工作集内存", ToastKind::Success);
            }
            if ui.button(app.t("settings.reset")).clicked() {
                app.reset_settings();
            }
        });
        ui.add_space(6.0);
        ui.label(
            RichText::new("Bamboo Music Player · 原生 Rimor 播放器 · WGPU/DX12")
                .size(10.0)
                .color(colors.text_faint),
        );
    });

    if appearance_changed {
        app.settings_dirty = true;
        app.apply_appearance();
    }
    if settings_changed {
        app.settings_dirty = true;
    }
}

fn cards<R>(
    ui: &mut Ui,
    colors: &crate::app::theme::ThemeColors,
    title: &str,
    add: impl FnOnce(&mut Ui) -> R,
) -> R {
    widgets::card(ui, colors, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new(title).size(15.0).strong().color(colors.text));
        ui.add_space(8.0);
        add(ui)
    })
}

fn row(
    ui: &mut Ui,
    label: &str,
    colors: &crate::app::theme::ThemeColors,
    add: impl FnOnce(&mut Ui),
) {
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(
            egui::vec2(150.0, 22.0),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.add(
                    egui::Label::new(
                        RichText::new(label).size(12.0).color(colors.text_weak),
                    )
                    .wrap(),
                );
            },
        );
        ui.horizontal_wrapped(|ui| add(ui));
    });
}

fn long_value(
    ui: &mut Ui,
    label: &str,
    value: &str,
    colors: &crate::app::theme::ThemeColors,
) {
    ui.label(RichText::new(label).size(10.5).color(colors.text_weak));
    ui.add(
        egui::Label::new(RichText::new(value).size(11.0).color(colors.text))
            .wrap(),
    );
    ui.add_space(4.0);
}

fn theme_label(mode: ThemeMode) -> &'static str {
    match mode {
        ThemeMode::System => "跟随系统",
        ThemeMode::Dark => "深色",
        ThemeMode::Light => "浅色",
    }
}

fn backend_label(backend: WgpuBackend) -> &'static str {
    match backend {
        WgpuBackend::Auto => "自动（DX12 优先）",
        WgpuBackend::Dx12 => "DX12",
        WgpuBackend::Vulkan => "Vulkan",
        WgpuBackend::Gl => "OpenGL（低内存）",
    }
}

fn background_label(mode: BackgroundMode) -> &'static str {
    match mode {
        BackgroundMode::StaticBlur => "静态模糊封面",
        BackgroundMode::GpuBlur => "GPU 模糊封面",
        BackgroundMode::Flow => "流动渐变",
        BackgroundMode::Solid => "纯色/渐变",
        BackgroundMode::Image => "自定义图片",
        BackgroundMode::Mica => "Mica 云母",
    }
}

fn desktop_bg_label(mode: DesktopBackgroundMode) -> &'static str {
    match mode {
        DesktopBackgroundMode::Transparent => "透明",
        DesktopBackgroundMode::Solid => "纯色",
        DesktopBackgroundMode::Gradient => "渐变",
        DesktopBackgroundMode::Image => "图片",
        DesktopBackgroundMode::BlurCover => "模糊封面",
    }
}

fn encoding_label(enc: LyricsEncoding) -> &'static str {
    match enc {
        LyricsEncoding::Auto => "自动识别（BOM/UTF-8/GBK）",
        LyricsEncoding::Utf8 => "UTF-8",
        LyricsEncoding::Gbk => "GBK",
        LyricsEncoding::Gb18030 => "GB18030",
        LyricsEncoding::Utf16Le => "UTF-16 LE",
        LyricsEncoding::Utf16Be => "UTF-16 BE",
        LyricsEncoding::Big5 => "Big5",
    }
}

fn close_label(b: CloseBehavior) -> &'static str {
    match b {
        CloseBehavior::Tray => "最小化到托盘",
        CloseBehavior::Exit => "退出程序",
        CloseBehavior::Ask => "每次询问",
    }
}

fn proxy_label(mode: ProxyMode) -> &'static str {
    match mode {
        ProxyMode::System => "系统代理",
        ProxyMode::None => "不使用代理",
        ProxyMode::Manual => "手动",
    }
}

fn replaygain_label(mode: ReplayGainMode) -> &'static str {
    match mode {
        ReplayGainMode::Off => "关闭",
        ReplayGainMode::Track => "按曲目",
        ReplayGainMode::Album => "按专辑",
    }
}

fn eq_label(preset: EqPreset) -> &'static str {
    match preset {
        EqPreset::Flat => "平直",
        EqPreset::Rock => "摇滚",
        EqPreset::Pop => "流行",
        EqPreset::Jazz => "爵士",
        EqPreset::Classical => "古典",
        EqPreset::Vocal => "人声",
        EqPreset::Bass => "低音增强",
        EqPreset::Custom => "自定义",
    }
}

fn source_kind_label(kind: SourceKind) -> &'static str {
    match kind {
        SourceKind::Local => "本地",
        SourceKind::Online => "在线",
    }
}
