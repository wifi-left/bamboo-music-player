//! Settings page: appearance, playback, lyrics, desktop lyrics, sources,
//! window behaviour, feature switches and about.

use egui::{RichText, Ui};

use crate::app::{App, ToastKind, widgets};
use crate::config::{
    BackgroundMode, CloseBehavior, ConflictPolicy, DesktopBackgroundMode, EqPreset, Language,
    LyricsEncoding, MetadataMode, ProxyMode, Quality, ReplayGainMode, SourceKind, ThemeMode,
    VisualizerMode, WgpuBackend, GpuPowerPreference,
};
use crate::platform::window;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SettingsTab {
    #[default]
    Appearance,
    Playback,
    Performance,
    Cache,
    Lyrics,
    DesktopLyrics,
    MiniPlayer,
    Library,
    Online,
    Playlist,
    Window,
    Features,
    About,
}

impl SettingsTab {
    pub const ALL: [SettingsTab; 13] = [
        SettingsTab::Appearance,
        SettingsTab::Playback,
        SettingsTab::Performance,
        SettingsTab::Cache,
        SettingsTab::Lyrics,
        SettingsTab::DesktopLyrics,
        SettingsTab::MiniPlayer,
        SettingsTab::Library,
        SettingsTab::Online,
        SettingsTab::Playlist,
        SettingsTab::Window,
        SettingsTab::Features,
        SettingsTab::About,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SettingsTab::Appearance => crate::i18n::tr("外观"),
            SettingsTab::Playback => crate::i18n::tr("播放"),
            SettingsTab::Performance => crate::i18n::tr("性能"),
            SettingsTab::Cache => crate::i18n::tr("缓存"),
            SettingsTab::Lyrics => crate::i18n::tr("歌词"),
            SettingsTab::DesktopLyrics => crate::i18n::tr("桌面歌词"),
            SettingsTab::MiniPlayer => crate::i18n::tr("迷你播放器"),
            SettingsTab::Library => crate::i18n::tr("曲库"),
            SettingsTab::Online => crate::i18n::tr("在线与网络"),
            SettingsTab::Playlist => crate::i18n::tr("歌单"),
            SettingsTab::Window => crate::i18n::tr("窗口与系统"),
            SettingsTab::Features => crate::i18n::tr("功能开关"),
            SettingsTab::About => crate::i18n::tr("关于"),
        }
    }
}

/// Category tabs at the top of the settings page.
fn settings_nav(app: &mut App, ui: &mut Ui, colors: &crate::app::theme::ThemeColors) {
    ui.horizontal_wrapped(|ui| {
        for tab in SettingsTab::ALL {
            let selected = app.settings_tab == tab;
            let text = RichText::new(tab.label()).size(13.0).color(if selected {
                colors.accent
            } else {
                colors.text_weak
            });
            if ui.selectable_label(selected, text).clicked() {
                app.settings_tab = tab;
                app.scroll_to_top = true;
            }
        }
    });
    ui.add_space(2.0);
    ui.separator();
    ui.add_space(8.0);
}

pub fn settings(app: &mut App, ui: &mut Ui) {
    let colors = app.colors();
    widgets::page_header(
        ui,
        app.t("settings.title"),
        crate::i18n::tr("所有选项即时保存；增强功能可逐项关闭"),
        &colors,
        |_ui| {},
    );
    settings_nav(app, ui, &colors);
    let tab = app.settings_tab;

    let mut appearance_changed = false;
    let mut settings_changed = false;

    // ---------------------------------------------------------------- Appearance
    if tab == SettingsTab::Appearance {
        cards(ui, &colors, app.t("settings.appearance"), |ui| {
            row(ui, app.t("settings.theme"), &colors, |ui| {
                let mut mode = app.settings.theme.mode;
                egui::ComboBox::from_id_salt("theme-mode")
                    .selected_text(theme_label(mode))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut mode,
                            ThemeMode::System,
                            crate::i18n::tr("跟随系统"),
                        );
                        ui.selectable_value(&mut mode, ThemeMode::Dark, crate::i18n::tr("深色"));
                        ui.selectable_value(&mut mode, ThemeMode::Light, crate::i18n::tr("浅色"));
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
                        ui.selectable_value(
                            &mut bg,
                            BackgroundMode::StaticBlur,
                            crate::i18n::tr("静态模糊封面（省电）"),
                        );
                        ui.selectable_value(
                            &mut bg,
                            BackgroundMode::GpuBlur,
                            crate::i18n::tr("GPU 模糊封面"),
                        );
                        ui.selectable_value(
                            &mut bg,
                            BackgroundMode::Flow,
                            crate::i18n::tr("流动渐变（跟随帧率）"),
                        );
                        ui.selectable_value(
                            &mut bg,
                            BackgroundMode::Solid,
                            crate::i18n::tr("纯色/渐变"),
                        );
                        ui.selectable_value(
                            &mut bg,
                            BackgroundMode::Image,
                            crate::i18n::tr("自定义图片"),
                        );
                        ui.selectable_value(
                            &mut bg,
                            BackgroundMode::Mica,
                            crate::i18n::tr("Mica 云母（Win11）"),
                        );
                    });
                if bg != app.settings.theme.background {
                    app.settings.theme.background = bg;
                    appearance_changed = true;
                }
            });
            if matches!(
                app.settings.theme.background,
                BackgroundMode::GpuBlur | BackgroundMode::Flow
            ) {
                row(ui, crate::i18n::tr("模糊半径"), &colors, |ui| {
                    if ui
                        .add(
                            egui::Slider::new(&mut app.settings.theme.blur_radius, 4.0..=80.0)
                                .suffix(" px"),
                        )
                        .changed()
                    {
                        settings_changed = true;
                    }
                });
            }
            if matches!(
                app.settings.theme.background,
                BackgroundMode::StaticBlur
                    | BackgroundMode::GpuBlur
                    | BackgroundMode::Flow
                    | BackgroundMode::Mica
            ) {
                row(ui, crate::i18n::tr("背景遮罩"), &colors, |ui| {
                    if ui
                        .add(
                            egui::Slider::new(
                                &mut app.settings.theme.background_opacity,
                                0.0..=0.9,
                            )
                            .show_value(true),
                        )
                        .changed()
                    {
                        settings_changed = true;
                    }
                    ui.label(
                        RichText::new(crate::i18n::tr("数值越高，封面越淡、文字越清晰"))
                            .size(10.0)
                            .color(colors.text_faint),
                    );
                });
            }
            if app.settings.theme.background == BackgroundMode::Mica && !app.mica_active {
                ui.horizontal(|ui| {
                    ui.add_space(158.0);
                    ui.add(
                        egui::Label::new(
                            RichText::new(crate::i18n::tr(
                                "当前系统或渲染后端不支持 Mica，已自动回退为静态模糊封面",
                            ))
                            .size(10.0)
                            .color(colors.warning),
                        )
                        .wrap(),
                    );
                });
            }
            if app.settings.theme.background == BackgroundMode::Image {
                row(ui, crate::i18n::tr("背景图片"), &colors, |ui| {
                    let label = app
                        .settings
                        .theme
                        .background_image
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| crate::i18n::tr("未选择").to_string());
                    ui.label(
                        RichText::new(crate::util::truncate_chars(&label, 46))
                            .size(11.0)
                            .color(colors.text_weak),
                    );
                    if ui.button(app.t("common.browse")).clicked() {
                        if let Some(path) = rfd::FileDialog::new()
                            .add_filter(
                                crate::i18n::tr("图片"),
                                &["png", "jpg", "jpeg", "webp", "bmp"],
                            )
                            .pick_file()
                        {
                            app.settings.theme.background_image = Some(path);
                            appearance_changed = true;
                        }
                    }
                });
            }
            row(ui, app.t("settings.font"), &colors, |ui| {
                let mut family = app.settings.theme.font_family.clone();
                if widgets::font_picker(
                    ui,
                    "theme-font",
                    &mut family,
                    crate::i18n::tr("默认（微软雅黑）"),
                ) {
                    app.settings.theme.font_family = family;
                    appearance_changed = true;
                }
            });
            row(ui, app.t("settings.font_size"), &colors, |ui| {
                if ui
                    .add(
                        egui::Slider::new(&mut app.settings.theme.font_size, 10.0..=22.0)
                            .suffix(" px"),
                    )
                    .changed()
                {
                    appearance_changed = true;
                }
            });
            row(ui, crate::i18n::tr("界面缩放"), &colors, |ui| {
                ui.add_enabled_ui(app.settings.features.accessibility, |ui| {
                    let mut scale = app.settings.theme.ui_scale;
                    if ui
                        .add(
                            egui::Slider::new(&mut scale, 0.75..=1.60)
                                .suffix("x")
                                .fixed_decimals(2)
                                .text(""),
                        )
                        .changed()
                    {
                        app.settings.theme.ui_scale = scale;
                        appearance_changed = true;
                    }
                });
                ui.label(
                    RichText::new(crate::i18n::tr("在系统 DPI 基础上整体缩放界面（无障碍）"))
                        .size(10.0)
                        .color(colors.text_faint),
                );
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
                    .checkbox(
                        &mut app.settings.theme.animations,
                        crate::i18n::tr("启用界面动效"),
                    )
                    .changed()
                {
                    settings_changed = true;
                }
                if ui
                    .checkbox(
                        &mut app.settings.theme.reduce_motion,
                        crate::i18n::tr("减少动效"),
                    )
                    .changed()
                {
                    settings_changed = true;
                }
            });
            row(ui, crate::i18n::tr("渲染后端"), &colors, |ui| {
                let mut backend = app.settings.theme.backend;
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("wgpu-backend")
                        .selected_text(backend_label(backend))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut backend,
                                WgpuBackend::Auto,
                                crate::i18n::tr("自动（DX12 优先）"),
                            );
                            ui.selectable_value(&mut backend, WgpuBackend::Dx12, "DX12");
                            ui.selectable_value(&mut backend, WgpuBackend::Vulkan, "Vulkan");
                            ui.selectable_value(
                                &mut backend,
                                WgpuBackend::Gl,
                                crate::i18n::tr("OpenGL（低内存）"),
                            );
                        });
                    if ui
                        .button(crate::i18n::tr("低内存"))
                        .on_hover_text(crate::i18n::tr(
                            "切换到 OpenGL 后端，私有内存可降到约 60 MB；需重启生效",
                        ))
                        .clicked()
                    {
                        backend = WgpuBackend::Gl;
                    }
                });
                if backend != app.settings.theme.backend {
                    app.settings.theme.backend = backend;
                    settings_changed = true;
                    app.toast(
                        crate::i18n::tr("渲染后端将在下次启动时生效"),
                        ToastKind::Info,
                    );
                }
            });
            row(ui, crate::i18n::tr("显卡偏好"), &colors, |ui| {
                let mut power = app.settings.theme.gpu_power;
                egui::ComboBox::from_id_salt("wgpu-power-preference")
                    .selected_text(power.label())
                    .show_ui(ui, |ui| {
                        for option in [
                            GpuPowerPreference::Auto,
                            GpuPowerPreference::LowPower,
                            GpuPowerPreference::HighPerformance,
                        ] {
                            ui.selectable_value(&mut power, option, option.label());
                        }
                    });
                if power != app.settings.theme.gpu_power {
                    app.settings.theme.gpu_power = power;
                    settings_changed = true;
                    app.toast(
                        crate::i18n::tr("显卡偏好将在下次启动时生效"),
                        ToastKind::Info,
                    );
                }
            });
            // GPU information belongs on its own line, aligned with the
            // control column, instead of being squeezed next to the buttons.
            ui.horizontal(|ui| {
                ui.add_space(158.0);
                ui.add(
                    egui::Label::new(
                        RichText::new(&app.gpu_info)
                            .size(10.0)
                            .color(colors.text_faint),
                    )
                    .wrap(),
                );
            });
            if app.gpu_info.contains("IntegratedGpu") {
                ui.horizontal(|ui| {
                    ui.add_space(158.0);
                    ui.add(
                        egui::Label::new(
                            RichText::new(crate::i18n::tr(
                                "轻负载界面通常核显更省电；强制独显可能因跨显卡合成反而提高占用",
                            ))
                            .size(10.0)
                            .color(colors.text_faint),
                        )
                        .wrap(),
                    );
                });
            }
            ui.add_space(2.0);
            row(ui, app.t("settings.language"), &colors, |ui| {
                let mut lang = app.settings.language;
                egui::ComboBox::from_id_salt("language")
                    .selected_text(match lang {
                        Language::ZhCn => crate::i18n::tr("简体中文"),
                        Language::EnUs => "English",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut lang, Language::ZhCn, crate::i18n::tr("简体中文"));
                        ui.selectable_value(&mut lang, Language::EnUs, "English");
                    });
                if lang != app.settings.language {
                    app.settings.language = lang;
                    app.lang = lang;
                    crate::i18n::set_language(lang);
                    app.tray = None; // rebuild labels next frame
                    settings_changed = true;
                }
            });
        });
        ui.add_space(12.0);
    }

    if tab == SettingsTab::Appearance {
        cards(ui, &colors, crate::i18n::tr("音频可视化"), |ui| {
            if !app.settings.features.visualizer {
                ui.label(
                    RichText::new(crate::i18n::tr("可视化已在 功能开关 中关闭"))
                        .size(10.5)
                        .color(colors.warning),
                );
                return;
            }
            row(ui, crate::i18n::tr("启用"), &colors, |ui| {
                if ui
                    .checkbox(
                        &mut app.settings.visualizer.enabled,
                        crate::i18n::tr("在正在播放页显示"),
                    )
                    .changed()
                {
                    settings_changed = true;
                }
                ui.label(
                    RichText::new(crate::i18n::tr("窗口高度不足或暂停时可按设置自动隐藏"))
                        .size(10.0)
                        .color(colors.text_faint),
                );
            });
            row(ui, crate::i18n::tr("样式"), &colors, |ui| {
                let mut mode = app.settings.visualizer.mode;
                egui::ComboBox::from_id_salt("visualizer-mode")
                    .selected_text(match mode {
                        VisualizerMode::Off => crate::i18n::tr("关闭"),
                        VisualizerMode::Bars => crate::i18n::tr("频谱条形"),
                        VisualizerMode::Waveform => crate::i18n::tr("波形"),
                        VisualizerMode::Ring => crate::i18n::tr("环形频谱"),
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut mode,
                            VisualizerMode::Off,
                            crate::i18n::tr("关闭"),
                        );
                        ui.selectable_value(
                            &mut mode,
                            VisualizerMode::Bars,
                            crate::i18n::tr("频谱条形"),
                        );
                        ui.selectable_value(
                            &mut mode,
                            VisualizerMode::Waveform,
                            crate::i18n::tr("波形"),
                        );
                        ui.selectable_value(
                            &mut mode,
                            VisualizerMode::Ring,
                            crate::i18n::tr("环形频谱"),
                        );
                    });
                if mode != app.settings.visualizer.mode {
                    app.settings.visualizer.mode = mode;
                    settings_changed = true;
                }
                if ui
                    .color_edit_button_srgb(&mut app.settings.visualizer.color)
                    .changed()
                {
                    settings_changed = true;
                }
            });
            row(ui, crate::i18n::tr("透明度与灵敏度"), &colors, |ui| {
                let mut opacity = app.settings.visualizer.opacity;
                if ui
                    .add(egui::Slider::new(&mut opacity, 0.1..=1.0).text(crate::i18n::tr("透明度")))
                    .changed()
                {
                    app.settings.visualizer.opacity = opacity;
                    settings_changed = true;
                }
                let mut sensitivity = app.settings.visualizer.sensitivity;
                if ui
                    .add(
                        egui::Slider::new(&mut sensitivity, 0.2..=3.0)
                            .text(crate::i18n::tr("灵敏度")),
                    )
                    .changed()
                {
                    app.settings.visualizer.sensitivity = sensitivity;
                    settings_changed = true;
                }
            });
            row(ui, crate::i18n::tr("刷新率"), &colors, |ui| {
                let mut fps = app.settings.visualizer.fps;
                if ui
                    .add(
                        egui::DragValue::new(&mut fps)
                            .range(5..=60)
                            .speed(1.0)
                            .suffix(" fps"),
                    )
                    .changed()
                {
                    app.settings.visualizer.fps = fps.clamp(5, 60);
                    settings_changed = true;
                }
                ui.label(
                    RichText::new(crate::i18n::tr(
                        "频谱/波形的更新频率；30 fps 已足够流畅，降低可减少 CPU 占用",
                    ))
                    .size(10.0)
                    .color(colors.text_faint),
                );
            });
            row(ui, crate::i18n::tr("暂停时隐藏"), &colors, |ui| {
                if ui
                    .checkbox(
                        &mut app.settings.visualizer.auto_hide,
                        crate::i18n::tr("仅在播放时显示可视化"),
                    )
                    .changed()
                {
                    settings_changed = true;
                }
            });
        });
        ui.add_space(12.0);
    }

    // ---------------------------------------------------------------- Playback
    if tab == SettingsTab::Playback {
        cards(ui, &colors, app.t("settings.playback"), |ui| {
            let device_text = if app.device_info.ready {
                format!(
                    "{}: {} · {} Hz · {} ch · {}",
                    app.t("settings.device"),
                    app.device_info.device_name,
                    app.device_info.sample_rate,
                    app.device_info.channels,
                    app.device_info.sample_format
                )
            } else {
                crate::trf!(
                    "{}: 尚未初始化（首次播放时创建，可在性能设置中立即初始化）",
                    app.t("settings.device")
                )
            };
            ui.label(
                RichText::new(device_text)
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
                    .checkbox(
                        &mut app.settings.playback.auto_advance,
                        crate::i18n::tr("播放结束后自动下一首"),
                    )
                    .changed()
                {
                    settings_changed = true;
                }
            });
            row(ui, app.t("settings.resume"), &colors, |ui| {
                if ui
                    .checkbox(
                        &mut app.settings.playback.resume_on_start,
                        crate::i18n::tr("启动时继续播放"),
                    )
                    .changed()
                {
                    settings_changed = true;
                }
            });
            row(ui, app.t("settings.gapless"), &colors, |ui| {
                if ui
                    .add_enabled(
                        app.settings.features.gapless,
                        egui::Checkbox::new(
                            &mut app.settings.playback.gapless,
                            crate::i18n::tr("无缝播放（下一首提前解码，曲间无静音）"),
                        ),
                    )
                    .changed()
                {
                    settings_changed = true;
                }
                if !app.settings.features.gapless {
                    ui.label(
                        RichText::new(crate::i18n::tr("功能开关中已关闭无缝播放"))
                            .size(10.0)
                            .color(colors.text_faint),
                    );
                }
            });
            row(ui, app.t("settings.crossfade"), &colors, |ui| {
                let mut value = app.settings.playback.crossfade_secs;
                if ui
                    .add_enabled(
                        app.settings.features.crossfade,
                        egui::Slider::new(&mut value, 0.0..=12.0)
                            .suffix(" s")
                            .text(""),
                    )
                    .changed()
                {
                    app.settings.playback.crossfade_secs = value;
                    settings_changed = true;
                }
                ui.label(
                    RichText::new(crate::i18n::tr(
                        "0 秒表示只做无缝切换；在线音源与需要解析地址的曲目仍走普通切歌",
                    ))
                    .size(10.0)
                    .color(colors.text_faint),
                );
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
                let enabled = app.settings.features.replaygain;
                let mut mode = app.settings.playback.replaygain;
                ui.add_enabled_ui(enabled, |ui| {
                    egui::ComboBox::from_id_salt("replaygain")
                        .selected_text(replaygain_label(mode))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut mode,
                                ReplayGainMode::Off,
                                crate::i18n::tr("关闭"),
                            );
                            ui.selectable_value(
                                &mut mode,
                                ReplayGainMode::Track,
                                crate::i18n::tr("按曲目"),
                            );
                            ui.selectable_value(
                                &mut mode,
                                ReplayGainMode::Album,
                                crate::i18n::tr("按专辑"),
                            );
                        });
                    let mut preamp = app.settings.playback.replaygain_preamp_db;
                    if ui
                        .add(
                            egui::Slider::new(&mut preamp, -12.0..=12.0)
                                .suffix(" dB")
                                .text(crate::i18n::tr("预增益")),
                        )
                        .changed()
                    {
                        app.settings.playback.replaygain_preamp_db = preamp;
                        settings_changed = true;
                    }
                    let mut scan = app.settings.playback.replaygain_scan;
                    if ui
                        .checkbox(
                            &mut scan,
                            crate::i18n::tr("无标签时后台分析响度（EBU R128）"),
                        )
                        .changed()
                    {
                        app.settings.playback.replaygain_scan = scan;
                        settings_changed = true;
                        if scan && let Some(track) = app.current_track.clone() {
                            app.maybe_scan_replaygain(&track);
                        }
                    }
                });
                if mode != app.settings.playback.replaygain {
                    app.settings.playback.replaygain = mode;
                    settings_changed = true;
                    if mode != ReplayGainMode::Off
                        && let Some(track) = app.current_track.clone()
                    {
                        app.maybe_scan_replaygain(&track);
                    }
                }
                ui.label(
                    RichText::new(crate::i18n::tr(
                        "优先读取 REPLAYGAIN_TRACK/ALBUM_GAIN；无标签时可在后台分析并缓存，首次播放后生效",
                    ))
                    .size(10.0)
                    .color(colors.text_faint),
                );
            });
            row(ui, app.t("settings.eq"), &colors, |ui| {
                let enabled = app.settings.features.equalizer;
                ui.add_enabled_ui(enabled, |ui| {
                    if ui
                        .checkbox(
                            &mut app.settings.playback.eq_enabled,
                            crate::i18n::tr("启用 10 段均衡器"),
                        )
                        .changed()
                    {
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
                        if preset != EqPreset::Custom {
                            app.settings.playback.eq_bands = preset.bands();
                        }
                        settings_changed = true;
                    }
                    let mut preamp = app.settings.playback.preamp_db;
                    if ui
                        .add(
                            egui::Slider::new(&mut preamp, -12.0..=12.0)
                                .suffix(" dB")
                                .text(crate::i18n::tr("前级")),
                        )
                        .changed()
                    {
                        app.settings.playback.preamp_db = preamp;
                        settings_changed = true;
                    }
                    let mut bands = app.settings.playback.eq_bands;
                    let mut bands_changed = false;
                    egui::Grid::new("eq-bands")
                        .num_columns(2)
                        .spacing(egui::vec2(10.0, 4.0))
                        .show(ui, |ui| {
                            for (index, band) in bands.iter_mut().enumerate() {
                                let label = if crate::audio::EQ_BANDS_HZ[index] >= 1000.0 {
                                    format!("{:.0} kHz", crate::audio::EQ_BANDS_HZ[index] / 1000.0)
                                } else {
                                    format!("{:.0} Hz", crate::audio::EQ_BANDS_HZ[index])
                                };
                                ui.label(RichText::new(label).size(10.5).color(colors.text_weak));
                                if ui
                                    .add(
                                        egui::Slider::new(band, -15.0..=15.0)
                                            .suffix(" dB")
                                            .fixed_decimals(1),
                                    )
                                    .changed()
                                {
                                    bands_changed = true;
                                }
                                ui.end_row();
                            }
                        });
                    if bands_changed {
                        app.settings.playback.eq_bands = bands;
                        app.settings.playback.eq_preset = EqPreset::Custom;
                        settings_changed = true;
                    }
                    if ui.button(crate::i18n::tr("全部归零")).clicked() {
                        app.settings.playback.eq_bands = [0.0; 10];
                        app.settings.playback.eq_preset = EqPreset::Flat;
                        settings_changed = true;
                    }
                });
                if !enabled {
                    ui.label(
                        RichText::new(crate::i18n::tr("功能开关中已关闭均衡器"))
                            .size(10.0)
                            .color(colors.text_faint),
                    );
                }
            });
            row(ui, app.t("settings.speed"), &colors, |ui| {
                let enabled = app.settings.features.speed_control;
                ui.add_enabled_ui(enabled, |ui| {
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
                    let mut preserve = app.settings.playback.pitch_preserve;
                    if ui
                        .checkbox(&mut preserve, crate::i18n::tr("保持音调"))
                        .on_hover_text(crate::i18n::tr(
                            "使用 WSOLA 时间伸缩改变速度，音调不变；关闭后为磁带式变速",
                        ))
                        .changed()
                    {
                        app.settings.playback.pitch_preserve = preserve;
                        settings_changed = true;
                    }
                });
                let hint = if app.settings.playback.pitch_preserve {
                    crate::i18n::tr("变速使用 WSOLA 时间伸缩，保持音调不变（CPU 占用略高）")
                } else {
                    crate::i18n::tr("变速通过重采样实现，改变速度的同时会改变音调（磁带式）")
                };
                ui.label(RichText::new(hint).size(10.0).color(colors.text_faint));
            });
        });
        ui.add_space(12.0);
    }

    // ---------------------------------------------------------------- Performance
    if tab == SettingsTab::Performance {
        cards(ui, &colors, crate::i18n::tr("性能与加载"), |ui| {
            ui.label(
                RichText::new(crate::i18n::tr(
                    "关闭不需要的元信息/封面读取，可显著加快大目录导入与切歌。",
                ))
                .size(10.5)
                .color(colors.text_faint),
            );
            ui.add_space(4.0);
            row(ui, crate::i18n::tr("歌曲元信息"), &colors, |ui| {
                let mut mode = app.settings.performance.metadata_mode;
                egui::ComboBox::from_id_salt("metadata-mode")
                    .selected_text(mode.label())
                    .show_ui(ui, |ui| {
                        for option in [
                            MetadataMode::Off,
                            MetadataMode::CurrentOnly,
                            MetadataMode::All,
                        ] {
                            ui.selectable_value(&mut mode, option, option.label());
                        }
                    });
                if mode != app.settings.performance.metadata_mode {
                    app.set_metadata_mode(mode);
                    settings_changed = true;
                }
                if ui
                    .button(crate::i18n::tr("补齐当前队列"))
                    .on_hover_text(crate::i18n::tr("为缺少标签的队列曲目排队读取元信息"))
                    .clicked()
                {
                    app.fill_queue_metadata();
                }
            });
            row(ui, crate::i18n::tr("封面图片"), &colors, |ui| {
                if ui
                    .checkbox(
                        &mut app.settings.performance.load_covers,
                        crate::i18n::tr("加载并后台解码封面"),
                    )
                    .changed()
                {
                    app.reload_cover();
                    settings_changed = true;
                }
                ui.label(
                    RichText::new(crate::i18n::tr("关闭后使用纯色占位封面"))
                        .size(10.0)
                        .color(colors.text_faint),
                );
            });
            row(ui, crate::i18n::tr("模糊背景"), &colors, |ui| {
                let covers_enabled = app.settings.performance.load_covers;
                if ui
                    .add_enabled(
                        covers_enabled,
                        egui::Checkbox::new(
                            &mut app.settings.performance.cover_background,
                            crate::i18n::tr("用封面生成模糊背景"),
                        ),
                    )
                    .changed()
                {
                    app.reload_cover();
                    settings_changed = true;
                }
                ui.label(
                    RichText::new(crate::i18n::tr("关闭后不生成背景纹理，减少内存与 GPU 占用"))
                        .size(10.0)
                        .color(colors.text_faint),
                );
            });
            row(ui, crate::i18n::tr("动画/界面帧率"), &colors, |ui| {
                let mut fps = app.settings.performance.frame_rate;
                if ui
                    .add(
                        egui::DragValue::new(&mut fps)
                            .range(15..=120)
                            .speed(1.0)
                            .suffix(" fps"),
                    )
                    .changed()
                {
                    app.settings.performance.frame_rate = fps.clamp(15, 120);
                    settings_changed = true;
                }
                ui.label(
                    RichText::new(crate::i18n::tr(
                        "影响逐字高亮、歌词滚动、桌面/独立歌词与背景动画；低帧率更省电",
                    ))
                    .size(10.0)
                    .color(colors.text_faint),
                );
            });
            row(ui, crate::i18n::tr("静态刷新率"), &colors, |ui| {
                let mut fps = app.settings.performance.idle_frame_rate;
                if ui
                    .add(
                        egui::DragValue::new(&mut fps)
                            .range(2..=60)
                            .speed(1.0)
                            .suffix(" fps"),
                    )
                    .changed()
                {
                    app.settings.performance.idle_frame_rate = fps.clamp(2, 60);
                    settings_changed = true;
                }
                ui.label(
                    RichText::new(crate::i18n::tr(
                        "没有动画时（仅播放进度与时间）的刷新频率；降低可显著减少前台 CPU/GPU 唤醒",
                    ))
                    .size(10.0)
                    .color(colors.text_faint),
                );
            });
            row(ui, crate::i18n::tr("背景动效"), &colors, |ui| {
                if ui
                    .checkbox(
                        &mut app.settings.performance.background_animation,
                        crate::i18n::tr("流动背景持续动画"),
                    )
                    .on_hover_text(crate::i18n::tr("关闭后背景图保持不变，不再为动画重绘"))
                    .changed()
                {
                    settings_changed = true;
                }
                let mut fps = app.settings.performance.background_frame_rate;
                if ui
                    .add_enabled(
                        app.settings.performance.background_animation,
                        egui::DragValue::new(&mut fps)
                            .range(15..=120)
                            .speed(1.0)
                            .suffix(" fps"),
                    )
                    .changed()
                {
                    app.settings.performance.background_frame_rate = fps.clamp(15, 120);
                    settings_changed = true;
                }
                ui.label(
                    RichText::new(crate::i18n::tr(
                        "动效帧率；30 fps 已足够平滑，降低可减少 GPU 占用",
                    ))
                    .size(10.0)
                    .color(colors.text_faint),
                );
            });
            row(ui, crate::i18n::tr("后台动效"), &colors, |ui| {
                if ui
                    .checkbox(
                        &mut app.settings.performance.pause_animations_unfocused,
                        crate::i18n::tr("窗口失焦/最小化时暂停动效"),
                    )
                    .on_hover_text(crate::i18n::tr(
                        "暂停背景动画与可视化刷新；回到前台自动恢复",
                    ))
                    .changed()
                {
                    settings_changed = true;
                }
                ui.label(
                    RichText::new(crate::i18n::tr(
                        "后台时仅保留进度条的低频刷新，大幅降低 GPU/CPU 占用",
                    ))
                    .size(10.0)
                    .color(colors.text_faint),
                );
            });
            row(ui, crate::i18n::tr("缩略图缓存"), &colors, |ui| {
                let mut cached = app.settings.performance.thumbnail_cache;
                if ui
                    .add(
                        egui::DragValue::new(&mut cached)
                            .range(0..=2048)
                            .speed(8.0)
                            .suffix(crate::i18n::tr(" 张")),
                    )
                    .on_hover_text(crate::i18n::tr("0 表示不限制"))
                    .changed()
                {
                    app.settings.performance.thumbnail_cache = cached.min(2048);
                    app.trim_thumbnail_cache();
                    settings_changed = true;
                }
                ui.label(
                    RichText::new(crate::i18n::tr(
                        "在线搜索封面在内存中的保留数量；超出后释放最旧的纹理",
                    ))
                    .size(10.0)
                    .color(colors.text_faint),
                );
            });
            row(ui, crate::i18n::tr("音频设备"), &colors, |ui| {
                let text = if app.device_info.ready {
                    format!(
                        "{} · {} Hz · {} ch · {}",
                        app.device_info.device_name,
                        app.device_info.sample_rate,
                        app.device_info.channels,
                        app.device_info.sample_format
                    )
                } else {
                    crate::i18n::tr("未初始化（首次播放时创建）").to_string()
                };
                ui.add(
                    egui::Label::new(RichText::new(text).size(10.5).color(colors.text_weak)).wrap(),
                );
                if ui.button(crate::i18n::tr("立即初始化")).clicked() {
                    app.engine.warm_up();
                    app.toast(crate::i18n::tr("正在初始化音频设备…"), ToastKind::Info);
                }
            });
            row(ui, crate::i18n::tr("启动与空闲"), &colors, |ui| {
                if ui
                    .checkbox(
                        &mut app.settings.performance.lazy_audio,
                        crate::i18n::tr("延迟初始化音频设备"),
                    )
                    .on_hover_text(crate::i18n::tr(
                        "启动时不创建 WASAPI 输出，首次播放才初始化",
                    ))
                    .changed()
                {
                    settings_changed = true;
                }
                if ui
                    .checkbox(
                        &mut app.settings.performance.defer_paused_restore,
                        crate::i18n::tr("恢复暂停会话时不预加载音频"),
                    )
                    .on_hover_text(crate::i18n::tr(
                        "启动时只恢复曲目与位置，按播放键才打开文件",
                    ))
                    .changed()
                {
                    settings_changed = true;
                }
                if ui
                    .checkbox(
                        &mut app.settings.performance.release_audio_output,
                        crate::i18n::tr("暂停/停止时暂停输出流"),
                    )
                    .on_hover_text(crate::i18n::tr("降低空闲功耗；恢复播放可能有极短延迟"))
                    .changed()
                {
                    app.engine
                        .set_release_when_idle(app.settings.performance.release_audio_output);
                    settings_changed = true;
                }
            });
        });
        ui.add_space(12.0);
    }

    // ---------------------------------------------------------------- Cache
    if tab == SettingsTab::Cache {
        cards(ui, &colors, crate::i18n::tr("缓存"), |ui| {
            ui.label(
                RichText::new(crate::i18n::tr(
                    "在线音频、HLS 点播、响度扫描与音频指纹都会写入本地缓存；可在这里统一启用、查看占用和清理。",
                ))
                .size(10.5)
                .color(colors.text_faint),
            );
            ui.add_space(4.0);
            row(ui, crate::i18n::tr("启用缓存"), &colors, |ui| {
                let mut enabled = app.settings.features.stream_cache;
                if ui
                    .checkbox(&mut enabled, crate::i18n::tr("缓存在线/点播音频"))
                    .changed()
                {
                    app.settings.features.stream_cache = enabled;
                    settings_changed = true;
                }
                ui.label(
                    RichText::new(crate::i18n::tr(
                        "关闭后不再读写 Bamboo 与 HLS 点播缓存；已缓存文件保留到手动清理",
                    ))
                    .size(10.0)
                    .color(colors.text_faint),
                );
            });
            row(ui, crate::i18n::tr("缓存上限"), &colors, |ui| {
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
                ui.label(
                    RichText::new(crate::i18n::tr("超过上限时按最后修改时间删除最旧的流缓存"))
                        .size(10.0)
                        .color(colors.text_faint),
                );
            });
            row(ui, crate::i18n::tr("流缓存目录"), &colors, |ui| {
                let dir = app
                    .settings
                    .network
                    .stream_cache_dir
                    .clone()
                    .unwrap_or_else(|| app.paths.stream_cache.clone());
                ui.add(
                    egui::Label::new(
                        RichText::new(crate::util::truncate_chars(&dir.display().to_string(), 56))
                            .size(11.0)
                            .color(colors.text_weak),
                    )
                    .wrap(),
                );
                if ui.button(crate::i18n::tr("选择")).clicked() {
                    app.choose_stream_cache_dir();
                }
                if ui.button(crate::i18n::tr("打开")).clicked() {
                    app.open_stream_cache_dir();
                }
                if widgets::secondary_button(ui, crate::i18n::tr("清理到上限"), &colors)
                    .on_hover_text(crate::i18n::tr(
                        "按最后修改时间删除最旧的缓存文件，直到低于缓存上限",
                    ))
                    .clicked()
                {
                    app.trim_stream_cache(true);
                }
            });
            row(ui, crate::i18n::tr("缓存占用"), &colors, |ui| {
                app.refresh_cache_stats(false);
                ui.label(
                    RichText::new(crate::trf!(
                        "总计 {} 个文件 · {:.1} MB（流缓存 {} 个 · {:.1} MB）",
                        app.cache_total_files,
                        format!("{:.1}", app.cache_total_bytes as f64 / 1024.0 / 1024.0),
                        app.cache_files,
                        format!("{:.1}", app.cache_bytes as f64 / 1024.0 / 1024.0)
                    ))
                    .size(10.5)
                    .color(colors.text_weak),
                );
            });
            row(ui, crate::i18n::tr("清理所有缓存"), &colors, |ui| {
                let button = egui::Button::new(
                    RichText::new(crate::i18n::tr("一键清理所有缓存")).color(egui::Color32::WHITE),
                )
                .fill(colors.danger)
                .corner_radius(egui::CornerRadius::same(8))
                .min_size(egui::vec2(0.0, 30.0));
                if ui.add(button).clicked() {
                    app.clear_all_caches();
                }
                ui.label(
                    RichText::new(crate::i18n::tr(
                        "清理流缓存、封面/歌词缓存、响度扫描与音频指纹；不会删除已下载的音频文件",
                    ))
                    .size(10.0)
                    .color(colors.text_faint),
                );
            });
            row(ui, crate::i18n::tr("分类清理"), &colors, |ui| {
                if widgets::secondary_button(ui, crate::i18n::tr("流缓存"), &colors).clicked() {
                    app.clear_stream_cache();
                }
                if widgets::secondary_button(ui, crate::i18n::tr("封面缓存"), &colors).clicked()
                {
                    app.clear_cover_cache();
                }
                if widgets::secondary_button(ui, crate::i18n::tr("歌词缓存"), &colors).clicked()
                {
                    app.clear_lyrics_cache();
                }
                if widgets::secondary_button(ui, crate::i18n::tr("响度缓存"), &colors).clicked()
                {
                    app.clear_replaygain_cache();
                }
                if widgets::secondary_button(ui, crate::i18n::tr("指纹缓存"), &colors).clicked()
                {
                    app.clear_fingerprint_cache();
                }
            });
        });
        ui.add_space(12.0);
    }

    // ---------------------------------------------------------------- Lyrics
    if tab == SettingsTab::Lyrics {
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
                    app.reload_lyrics();
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
                    .checkbox(
                        &mut app.settings.lyrics.show_translation,
                        crate::i18n::tr("双语歌词"),
                    )
                    .changed()
                {
                    settings_changed = true;
                }
                if ui
                    .checkbox(
                        &mut app.settings.lyrics.karaoke,
                        crate::i18n::t(app.lang, "settings.karaoke"),
                    )
                    .changed()
                {
                    settings_changed = true;
                }
                if ui
                    .checkbox(
                        &mut app.settings.lyrics.karaoke_words_only,
                        crate::i18n::tr("仅增强型 LRC"),
                    )
                    .on_hover_text(crate::i18n::tr(
                        "仅当歌词包含 <mm:ss.xx> 逐字时间时启用逐字高亮；普通 LRC 保持整行高亮",
                    ))
                    .changed()
                {
                    settings_changed = true;
                }
            });
            row(ui, crate::i18n::tr("译文语言"), &colors, |ui| {
                let mut language = app.settings.lyrics.translation_language.clone();
                let selected = match language.as_str() {
                    "zh" => crate::i18n::tr("中文 (zh)").to_string(),
                    "en" => "English (en)".to_string(),
                    "jp" => crate::i18n::tr("日本語 (jp)").to_string(),
                    _ => crate::i18n::tr("自动（跟随界面语言）").to_string(),
                };
                egui::ComboBox::from_id_salt("lyrics-translation-language")
                    .selected_text(selected)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut language,
                            "auto".to_string(),
                            crate::i18n::tr("自动（跟随界面语言）"),
                        );
                        ui.selectable_value(
                            &mut language,
                            "zh".to_string(),
                            crate::i18n::tr("中文 (zh)"),
                        );
                        ui.selectable_value(&mut language, "en".to_string(), "English (en)");
                        ui.selectable_value(
                            &mut language,
                            "jp".to_string(),
                            crate::i18n::tr("日本語 (jp)"),
                        );
                    });
                if language != app.settings.lyrics.translation_language {
                    app.settings.lyrics.translation_language = language;
                    settings_changed = true;
                    // Re-read the current track so a different
                    // `Song.<region>.lrc` sidecar takes effect immediately.
                    app.reload_lyrics();
                }
                ui.label(
                    RichText::new(crate::i18n::tr(
                        "优先读取 同名.<语言>.lrc/.srt（如 A.zh.lrc、A.en.lrc）；缺失时回退到其他语言",
                    ))
                    .size(10.0)
                    .color(colors.text_faint),
                );
            });
            row(ui, app.t("settings.sidebar"), &colors, |ui| {
                if ui
                    .checkbox(
                        &mut app.settings.lyrics.sidebar_visible,
                        crate::i18n::tr("在右侧常驻歌词栏"),
                    )
                    .changed()
                {
                    app.show_lyrics_sidebar = app.settings.lyrics.sidebar_visible;
                    settings_changed = true;
                }
                if ui
                    .checkbox(
                        &mut app.settings.lyrics.auto_load,
                        crate::i18n::tr("自动查找歌词"),
                    )
                    .changed()
                {
                    app.reload_lyrics();
                    settings_changed = true;
                }
                if ui
                    .checkbox(
                        &mut app.settings.lyrics.scroll_animation,
                        crate::i18n::tr("歌词滚动动画"),
                    )
                    .on_hover_text(crate::i18n::tr("受“启用界面动效/减少动效”总开关控制"))
                    .changed()
                {
                    settings_changed = true;
                }
                if ui
                    .checkbox(
                        &mut app.settings.lyrics.prefetch_next,
                        crate::i18n::tr("预取下一首歌词"),
                    )
                    .on_hover_text(crate::i18n::tr(
                        "在当前曲目播放时提前解析下一首的本地歌词，切歌后立即显示",
                    ))
                    .changed()
                {
                    if !app.settings.lyrics.prefetch_next {
                        app.clear_lyrics_prefetch();
                    }
                    settings_changed = true;
                }
                if ui.button(crate::i18n::tr("重新加载歌词")).clicked() {
                    app.reload_lyrics();
                }
            });
            row(ui, crate::i18n::tr("歌词状态"), &colors, |ui| {
                if ui
                    .checkbox(
                        &mut app.settings.lyrics.enabled,
                        crate::i18n::tr("启用歌词"),
                    )
                    .changed()
                {
                    app.reload_lyrics();
                    settings_changed = true;
                }
                if ui.button(crate::i18n::tr("选择歌词文件")).clicked() {
                    app.pick_lyrics_file();
                }
                if app.settings.features.fullscreen_lyrics
                    && ui.button(crate::i18n::tr("打开大歌词页")).clicked()
                {
                    app.toggle_fullscreen_lyrics();
                }
            });
            row(ui, crate::i18n::tr("歌词颜色"), &colors, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(crate::i18n::tr("当前行"))
                            .size(11.0)
                            .color(colors.text_weak),
                    );
                    ui.color_edit_button_srgb(&mut app.settings.lyrics.current_color);
                    ui.label(
                        RichText::new(crate::i18n::tr("普通行"))
                            .size(11.0)
                            .color(colors.text_weak),
                    );
                    ui.color_edit_button_srgb(&mut app.settings.lyrics.normal_color);
                });
            });
            ui.label(
            RichText::new(
                crate::i18n::tr("支持同名 .lrc/.srt、标签内嵌歌词；自动识别 UTF-8/GBK/GB18030 等编码。全屏歌词可用 F11 或 Esc 切换。"),
            )
            .size(10.5)
            .color(colors.text_faint),
        );
        });
        ui.add_space(12.0);

        cards(
            ui,
            &colors,
            crate::i18n::tr("大屏 / 独立歌词窗口"),
            |ui| {
                ui.label(
                    RichText::new(crate::i18n::tr(
                        "同时用于沉浸式全屏歌词与独立歌词窗口；小歌词面板/侧栏仍使用上面的设置。",
                    ))
                    .size(10.5)
                    .color(colors.text_faint),
                );
                ui.add_space(4.0);
                row(ui, crate::i18n::tr("字体"), &colors, |ui| {
                    let mut family = app.settings.large_lyrics.font_family.clone();
                    if widgets::font_picker(
                        ui,
                        "large-lyrics-font",
                        &mut family,
                        crate::i18n::tr("跟随界面字体"),
                    ) {
                        app.settings.large_lyrics.font_family = family;
                        app.reload_fonts();
                        settings_changed = true;
                    }
                });
                row(ui, crate::i18n::tr("字号 / 行距"), &colors, |ui| {
                    let mut size = app.settings.large_lyrics.font_size;
                    if ui
                        .add(
                            egui::Slider::new(&mut size, 16.0..=96.0)
                                .integer()
                                .suffix(" px"),
                        )
                        .changed()
                    {
                        app.settings.large_lyrics.font_size = size;
                        settings_changed = true;
                    }
                    let mut spacing = app.settings.large_lyrics.line_spacing;
                    if ui
                        .add(
                            egui::Slider::new(&mut spacing, 0.0..=80.0)
                                .integer()
                                .suffix(" px"),
                        )
                        .changed()
                    {
                        app.settings.large_lyrics.line_spacing = spacing;
                        settings_changed = true;
                    }
                });
                row(ui, crate::i18n::tr("对齐与显示"), &colors, |ui| {
                    let mut align = app.settings.large_lyrics.align;
                    egui::ComboBox::from_id_salt("large-lyrics-align")
                        .selected_text(crate::app::views::desktop_lyrics::align_label(align))
                        .show_ui(ui, |ui| {
                            use crate::config::LyricsAlign;
                            ui.selectable_value(
                                &mut align,
                                LyricsAlign::Left,
                                crate::i18n::tr("左对齐"),
                            );
                            ui.selectable_value(
                                &mut align,
                                LyricsAlign::Center,
                                crate::i18n::tr("居中"),
                            );
                            ui.selectable_value(
                                &mut align,
                                LyricsAlign::Right,
                                crate::i18n::tr("右对齐"),
                            );
                        });
                    if align != app.settings.large_lyrics.align {
                        app.settings.large_lyrics.align = align;
                        settings_changed = true;
                    }
                    for (value, label) in [
                        (
                            &mut app.settings.large_lyrics.show_translation,
                            crate::i18n::tr("显示译文"),
                        ),
                        (
                            &mut app.settings.large_lyrics.show_cover,
                            crate::i18n::tr("显示封面"),
                        ),
                        (
                            &mut app.settings.large_lyrics.show_title,
                            crate::i18n::tr("显示曲目信息"),
                        ),
                        (
                            &mut app.settings.large_lyrics.show_footer,
                            crate::i18n::tr("显示进度与控制"),
                        ),
                        (
                            &mut app.settings.large_lyrics.karaoke,
                            crate::i18n::tr("逐字高亮"),
                        ),
                    ] {
                        if ui.checkbox(value, label).changed() {
                            settings_changed = true;
                        }
                    }
                    if ui
                        .checkbox(
                            &mut app.settings.large_lyrics.karaoke_words_only,
                            crate::i18n::tr("仅增强型 LRC"),
                        )
                        .changed()
                    {
                        settings_changed = true;
                    }
                });
                row(ui, crate::i18n::tr("歌词颜色"), &colors, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(crate::i18n::tr("当前行"))
                                .size(11.0)
                                .color(colors.text_weak),
                        );
                        ui.color_edit_button_srgb(&mut app.settings.large_lyrics.current_color);
                        ui.label(
                            RichText::new(crate::i18n::tr("普通行"))
                                .size(11.0)
                                .color(colors.text_weak),
                        );
                        ui.color_edit_button_srgb(&mut app.settings.large_lyrics.normal_color);
                        ui.label(
                            RichText::new(crate::i18n::tr("译文"))
                                .size(11.0)
                                .color(colors.text_weak),
                        );
                        ui.color_edit_button_srgb(&mut app.settings.large_lyrics.translation_color);
                    });
                });
            },
        );
        ui.add_space(12.0);
    }
    if tab == SettingsTab::DesktopLyrics {
        cards(ui, &colors, app.t("settings.desktop_lyrics"), |ui| {
            row(ui, crate::i18n::tr("行为"), &colors, |ui| {
                if ui
                    .checkbox(
                        &mut app.settings.desktop_lyrics.visible_at_start,
                        crate::i18n::tr("显示桌面歌词"),
                    )
                    .changed()
                {
                    settings_changed = true;
                }
                if ui
                    .checkbox(
                        &mut app.settings.desktop_lyrics.always_on_top,
                        crate::i18n::t(app.lang, "settings.always_on_top"),
                    )
                    .changed()
                {
                    settings_changed = true;
                }
                if ui
                    .checkbox(
                        &mut app.settings.desktop_lyrics.locked,
                        crate::i18n::t(app.lang, "settings.locked"),
                    )
                    .changed()
                {
                    settings_changed = true;
                }
                if ui
                    .checkbox(
                        &mut app.settings.desktop_lyrics.click_through,
                        crate::i18n::t(app.lang, "settings.click_through"),
                    )
                    .changed()
                {
                    settings_changed = true;
                }
            });
            row(ui, crate::i18n::tr("外观"), &colors, |ui| {
                let mut lines = app.settings.desktop_lyrics.lines as f32;
                if ui
                    .add(
                        egui::Slider::new(&mut lines, 1.0..=3.0)
                            .integer()
                            .prefix(crate::i18n::tr("行数 ")),
                    )
                    .changed()
                {
                    app.settings.desktop_lyrics.lines = lines as u8;
                    settings_changed = true;
                }
                let mut size = app.settings.desktop_lyrics.font_size;
                if ui
                    .add(
                        egui::Slider::new(&mut size, 14.0..=96.0)
                            .integer()
                            .prefix(crate::i18n::tr("当前字号 ")),
                    )
                    .changed()
                {
                    app.settings.desktop_lyrics.font_size = size;
                    settings_changed = true;
                }
                if ui
                    .checkbox(
                        &mut app.settings.desktop_lyrics.show_footer,
                        crate::i18n::t(app.lang, "settings.show_footer"),
                    )
                    .changed()
                {
                    settings_changed = true;
                }
                if ui
                    .checkbox(
                        &mut app.settings.desktop_lyrics.show_translation,
                        crate::i18n::t(app.lang, "settings.show_translation"),
                    )
                    .changed()
                {
                    settings_changed = true;
                }
            });
            row(ui, crate::i18n::tr("样式"), &colors, |ui| {
                if ui
                    .checkbox(
                        &mut app.settings.desktop_lyrics.bold,
                        crate::i18n::tr("粗体"),
                    )
                    .changed()
                {
                    settings_changed = true;
                }
                if ui
                    .checkbox(
                        &mut app.settings.desktop_lyrics.karaoke,
                        crate::i18n::tr("逐字高亮"),
                    )
                    .changed()
                {
                    settings_changed = true;
                }
                if ui
                    .checkbox(
                        &mut app.settings.desktop_lyrics.karaoke_words_only,
                        crate::i18n::tr("仅增强型 LRC"),
                    )
                    .on_hover_text(crate::i18n::tr(
                        "仅当歌词包含 <mm:ss.xx> 逐字时间时启用逐字填充",
                    ))
                    .changed()
                {
                    settings_changed = true;
                }
                if ui
                    .checkbox(
                        &mut app.settings.desktop_lyrics.shadow,
                        crate::i18n::tr("文字阴影"),
                    )
                    .changed()
                {
                    settings_changed = true;
                }
                if ui
                    .checkbox(
                        &mut app.settings.desktop_lyrics.outline,
                        crate::i18n::tr("文字描边"),
                    )
                    .changed()
                {
                    settings_changed = true;
                }
                if ui
                    .checkbox(
                        &mut app.settings.desktop_lyrics.translation_outline,
                        crate::i18n::tr("译文描边"),
                    )
                    .on_hover_text(crate::i18n::tr("使用与当前行相同的描边颜色/厚度"))
                    .changed()
                {
                    settings_changed = true;
                }
                if ui
                    .checkbox(
                        &mut app.settings.desktop_lyrics.translation_shadow,
                        crate::i18n::tr("译文阴影"),
                    )
                    .on_hover_text(crate::i18n::tr("使用与当前行相同的阴影颜色/距离"))
                    .changed()
                {
                    settings_changed = true;
                }
                if ui
                    .checkbox(
                        &mut app.settings.desktop_lyrics.footer_outline,
                        crate::i18n::tr("页脚描边"),
                    )
                    .on_hover_text(crate::i18n::tr("歌名/歌手页脚使用相同的描边颜色/厚度"))
                    .changed()
                {
                    settings_changed = true;
                }
                if ui
                    .checkbox(
                        &mut app.settings.desktop_lyrics.footer_shadow,
                        crate::i18n::tr("页脚阴影"),
                    )
                    .on_hover_text(crate::i18n::tr("歌名/歌手页脚使用相同的阴影颜色/距离"))
                    .changed()
                {
                    settings_changed = true;
                }
                if ui
                    .checkbox(
                        &mut app.settings.desktop_lyrics.border,
                        crate::i18n::tr("显示边框"),
                    )
                    .changed()
                {
                    settings_changed = true;
                }
                if ui
                    .checkbox(
                        &mut app.settings.desktop_lyrics.window_shadow,
                        crate::i18n::tr("窗口阴影"),
                    )
                    .on_hover_text(crate::i18n::tr(
                        "透明背景下建议关闭，否则窗口矩形仍会投下一圈系统阴影",
                    ))
                    .changed()
                {
                    settings_changed = true;
                }
                if app.settings.desktop_lyrics.outline
                    || app.settings.desktop_lyrics.translation_outline
                    || app.settings.desktop_lyrics.footer_outline
                {
                    ui.label(
                        RichText::new(crate::i18n::tr("描边颜色"))
                            .size(11.0)
                            .color(colors.text_weak),
                    );
                    let mut color = egui::Color32::from_rgba_unmultiplied(
                        app.settings.desktop_lyrics.outline_color[0],
                        app.settings.desktop_lyrics.outline_color[1],
                        app.settings.desktop_lyrics.outline_color[2],
                        app.settings.desktop_lyrics.outline_color[3],
                    );
                    if ui.color_edit_button_srgba(&mut color).changed() {
                        app.settings.desktop_lyrics.outline_color = color.to_array();
                        settings_changed = true;
                    }
                    let mut width = app.settings.desktop_lyrics.outline_width;
                    ui.label(
                        RichText::new(crate::i18n::tr("描边厚度"))
                            .size(11.0)
                            .color(colors.text_weak),
                    );
                    if ui
                        .add(
                            egui::DragValue::new(&mut width)
                                .range(0.5..=6.0)
                                .speed(0.1)
                                .suffix(" px"),
                        )
                        .changed()
                    {
                        app.settings.desktop_lyrics.outline_width = width;
                        settings_changed = true;
                    }
                }
                if app.settings.desktop_lyrics.shadow
                    || app.settings.desktop_lyrics.translation_shadow
                    || app.settings.desktop_lyrics.footer_shadow
                {
                    ui.label(
                        RichText::new(crate::i18n::tr("阴影颜色"))
                            .size(11.0)
                            .color(colors.text_weak),
                    );
                    let mut color = egui::Color32::from_rgba_unmultiplied(
                        app.settings.desktop_lyrics.shadow_color[0],
                        app.settings.desktop_lyrics.shadow_color[1],
                        app.settings.desktop_lyrics.shadow_color[2],
                        app.settings.desktop_lyrics.shadow_color[3],
                    );
                    if ui.color_edit_button_srgba(&mut color).changed() {
                        app.settings.desktop_lyrics.shadow_color = color.to_array();
                        settings_changed = true;
                    }
                    let mut offset = app.settings.desktop_lyrics.shadow_offset;
                    ui.label(
                        RichText::new(crate::i18n::tr("阴影距离"))
                            .size(11.0)
                            .color(colors.text_weak),
                    );
                    if ui
                        .add(
                            egui::DragValue::new(&mut offset)
                                .range(0.0..=24.0)
                                .speed(0.2)
                                .suffix(" px"),
                        )
                        .changed()
                    {
                        app.settings.desktop_lyrics.shadow_offset = offset;
                        settings_changed = true;
                    }
                }
                let mut align = app.settings.desktop_lyrics.align;
                egui::ComboBox::from_id_salt("desktop-align")
                    .selected_text(crate::app::views::desktop_lyrics::align_label(align))
                    .show_ui(ui, |ui| {
                        use crate::config::LyricsAlign;
                        ui.selectable_value(
                            &mut align,
                            LyricsAlign::Left,
                            crate::i18n::tr("左对齐"),
                        );
                        ui.selectable_value(
                            &mut align,
                            LyricsAlign::Center,
                            crate::i18n::tr("居中"),
                        );
                        ui.selectable_value(
                            &mut align,
                            LyricsAlign::Right,
                            crate::i18n::tr("右对齐"),
                        );
                    });
                if align != app.settings.desktop_lyrics.align {
                    app.settings.desktop_lyrics.align = align;
                    settings_changed = true;
                }
            });
            row(ui, crate::i18n::tr("字体"), &colors, |ui| {
                let mut family = app.settings.desktop_lyrics.font_family.clone();
                if widgets::font_picker(
                    ui,
                    "desktop-font",
                    &mut family,
                    crate::i18n::tr("跟随界面字体"),
                ) {
                    app.settings.desktop_lyrics.font_family = family;
                    app.reload_fonts();
                    settings_changed = true;
                }
            });
            row(ui, crate::i18n::tr("字号"), &colors, |ui| {
                let mut current = app.settings.desktop_lyrics.font_size;
                if ui
                    .add(
                        egui::Slider::new(&mut current, 14.0..=96.0)
                            .integer()
                            .prefix(crate::i18n::tr("当前 ")),
                    )
                    .changed()
                {
                    app.settings.desktop_lyrics.font_size = current;
                    settings_changed = true;
                }
                let mut next = app.settings.desktop_lyrics.next_font_size;
                if ui
                    .add(
                        egui::Slider::new(&mut next, 12.0..=80.0)
                            .integer()
                            .prefix(crate::i18n::tr("下一句 ")),
                    )
                    .changed()
                {
                    app.settings.desktop_lyrics.next_font_size = next;
                    settings_changed = true;
                }
                let mut prev = app.settings.desktop_lyrics.prev_font_size;
                if ui
                    .add(
                        egui::Slider::new(&mut prev, 12.0..=80.0)
                            .integer()
                            .prefix(crate::i18n::tr("上一句 ")),
                    )
                    .changed()
                {
                    app.settings.desktop_lyrics.prev_font_size = prev;
                    settings_changed = true;
                }
                let mut footer = app.settings.desktop_lyrics.footer_font_size;
                if ui
                    .add(
                        egui::Slider::new(&mut footer, 8.0..=40.0)
                            .integer()
                            .prefix(crate::i18n::tr("歌名 ")),
                    )
                    .changed()
                {
                    app.settings.desktop_lyrics.footer_font_size = footer;
                    settings_changed = true;
                }
            });
            row(ui, crate::i18n::tr("窗口大小"), &colors, |ui| {
                if ui
                    .checkbox(
                        &mut app.settings.desktop_lyrics.auto_size,
                        crate::i18n::tr("自动高度"),
                    )
                    .changed()
                {
                    settings_changed = true;
                }
                let mut width = app.settings.desktop_lyrics.width;
                if ui
                    .add(
                        egui::Slider::new(&mut width, 320.0..=1600.0)
                            .integer()
                            .prefix(crate::i18n::tr("宽度 ")),
                    )
                    .changed()
                {
                    app.settings.desktop_lyrics.width = width;
                    settings_changed = true;
                }
                let mut height = app.settings.desktop_lyrics.height;
                if ui
                    .add(
                        egui::Slider::new(&mut height, 60.0..=500.0)
                            .integer()
                            .prefix(crate::i18n::tr("高度 ")),
                    )
                    .changed()
                {
                    app.settings.desktop_lyrics.height = height;
                    // Dragging the height slider is an explicit request for a
                    // fixed window size, so auto-height is switched off.
                    app.settings.desktop_lyrics.auto_size = false;
                    settings_changed = true;
                }
                if app.settings.desktop_lyrics.auto_size {
                    ui.label(
                        RichText::new(crate::i18n::tr("高度随行数自动调整"))
                            .size(10.5)
                            .color(colors.text_faint),
                    );
                }
                if ui
                    .checkbox(
                        &mut app.settings.desktop_lyrics.hide_when_paused,
                        crate::i18n::tr("暂停时隐藏"),
                    )
                    .changed()
                {
                    settings_changed = true;
                }
                if ui
                    .checkbox(
                        &mut app.settings.desktop_lyrics.hide_when_fullscreen,
                        crate::i18n::tr("全屏应用时隐藏"),
                    )
                    .changed()
                {
                    settings_changed = true;
                }
                if ui.button(crate::i18n::tr("重置位置")).clicked() {
                    app.session.desktop_lyrics_pos = None;
                    app.settings.desktop_lyrics.position = None;
                    app.desktop_lyrics_reset_pos = true;
                    settings_changed = true;
                }
            });
            row(ui, crate::i18n::tr("透明度/圆角"), &colors, |ui| {
                let mut opacity = app.settings.desktop_lyrics.opacity;
                if ui
                    .add(
                        egui::Slider::new(&mut opacity, 0.0..=1.0)
                            .text(crate::i18n::tr("不透明度")),
                    )
                    .changed()
                {
                    app.settings.desktop_lyrics.opacity = opacity;
                    settings_changed = true;
                }
                let mut radius = app.settings.desktop_lyrics.radius as f32;
                if ui
                    .add(
                        egui::Slider::new(&mut radius, 0.0..=48.0)
                            .integer()
                            .text(crate::i18n::tr("圆角")),
                    )
                    .changed()
                {
                    app.settings.desktop_lyrics.radius = radius as u8;
                    settings_changed = true;
                }
                let mut spacing = app.settings.desktop_lyrics.line_spacing;
                if ui
                    .add(egui::Slider::new(&mut spacing, 0.0..=40.0).text(crate::i18n::tr("行距")))
                    .changed()
                {
                    app.settings.desktop_lyrics.line_spacing = spacing;
                    settings_changed = true;
                }
                let mut padding = app.settings.desktop_lyrics.padding;
                if ui
                    .add(egui::Slider::new(&mut padding, 0.0..=60.0).text(crate::i18n::tr("边距")))
                    .changed()
                {
                    app.settings.desktop_lyrics.padding = padding;
                    settings_changed = true;
                }
            });
            row(ui, crate::i18n::tr("背景"), &colors, |ui| {
                let mut bg = app.settings.desktop_lyrics.background;
                egui::ComboBox::from_id_salt("desktop-bg")
                    .selected_text(desktop_bg_label(bg))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut bg,
                            DesktopBackgroundMode::Transparent,
                            app.t("settings.bg_transparent"),
                        );
                        ui.selectable_value(
                            &mut bg,
                            DesktopBackgroundMode::Solid,
                            app.t("settings.bg_solid"),
                        );
                        ui.selectable_value(
                            &mut bg,
                            DesktopBackgroundMode::Gradient,
                            app.t("settings.bg_gradient"),
                        );
                        ui.selectable_value(
                            &mut bg,
                            DesktopBackgroundMode::Image,
                            app.t("settings.bg_image"),
                        );
                        ui.selectable_value(
                            &mut bg,
                            DesktopBackgroundMode::BlurCover,
                            app.t("settings.bg_blur"),
                        );
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
                if matches!(
                    app.settings.desktop_lyrics.background,
                    DesktopBackgroundMode::Gradient
                ) {
                    let mut bg_color2 = egui::Color32::from_rgba_unmultiplied(
                        app.settings.desktop_lyrics.bg_color2[0],
                        app.settings.desktop_lyrics.bg_color2[1],
                        app.settings.desktop_lyrics.bg_color2[2],
                        app.settings.desktop_lyrics.bg_color2[3],
                    );
                    if ui.color_edit_button_srgba(&mut bg_color2).changed() {
                        app.settings.desktop_lyrics.bg_color2 = bg_color2.to_array();
                        settings_changed = true;
                    }
                }
                if matches!(
                    app.settings.desktop_lyrics.background,
                    DesktopBackgroundMode::Image
                ) {
                    if ui.button(crate::i18n::tr("选择图片")).clicked()
                        && let Some(path) = rfd::FileDialog::new()
                            .add_filter("Image", &["png", "jpg", "jpeg", "webp", "bmp"])
                            .pick_file()
                    {
                        app.settings.desktop_lyrics.bg_image = Some(path);
                        settings_changed = true;
                    }
                    if ui.button(crate::i18n::tr("清除")).clicked() {
                        app.settings.desktop_lyrics.bg_image = None;
                        settings_changed = true;
                    }
                }
            });
            row(ui, crate::i18n::tr("文字颜色"), &colors, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(crate::i18n::tr("当前"))
                            .size(11.0)
                            .color(colors.text_weak),
                    );
                    let mut current = [
                        app.settings.desktop_lyrics.current_color[0],
                        app.settings.desktop_lyrics.current_color[1],
                        app.settings.desktop_lyrics.current_color[2],
                    ];
                    if ui.color_edit_button_srgb(&mut current).changed() {
                        app.settings.desktop_lyrics.current_color[..3].copy_from_slice(&current);
                        settings_changed = true;
                    }
                    ui.label(
                        RichText::new(crate::i18n::tr("下一句"))
                            .size(11.0)
                            .color(colors.text_weak),
                    );
                    let mut next = [
                        app.settings.desktop_lyrics.next_color[0],
                        app.settings.desktop_lyrics.next_color[1],
                        app.settings.desktop_lyrics.next_color[2],
                    ];
                    if ui.color_edit_button_srgb(&mut next).changed() {
                        app.settings.desktop_lyrics.next_color[..3].copy_from_slice(&next);
                        settings_changed = true;
                    }
                    ui.label(
                        RichText::new(crate::i18n::tr("上一句"))
                            .size(11.0)
                            .color(colors.text_weak),
                    );
                    let mut prev = [
                        app.settings.desktop_lyrics.prev_color[0],
                        app.settings.desktop_lyrics.prev_color[1],
                        app.settings.desktop_lyrics.prev_color[2],
                    ];
                    if ui.color_edit_button_srgb(&mut prev).changed() {
                        app.settings.desktop_lyrics.prev_color[..3].copy_from_slice(&prev);
                        settings_changed = true;
                    }
                    ui.label(
                        RichText::new(crate::i18n::tr("歌名"))
                            .size(11.0)
                            .color(colors.text_weak),
                    );
                    let mut footer = [
                        app.settings.desktop_lyrics.footer_color[0],
                        app.settings.desktop_lyrics.footer_color[1],
                        app.settings.desktop_lyrics.footer_color[2],
                    ];
                    if ui.color_edit_button_srgb(&mut footer).changed() {
                        app.settings.desktop_lyrics.footer_color[..3].copy_from_slice(&footer);
                        settings_changed = true;
                    }
                });
            });
            row(ui, crate::i18n::tr("捕获保护"), &colors, |ui| {
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
            RichText::new(
                crate::i18n::tr("桌面歌词是独立透明置顶窗口：未锁定时可直接拖动，右键可切换锁定/穿透/字号；鼠标穿透后请回此页关闭。"),
            )
            .size(10.5)
            .color(colors.text_faint),
        );
        });
        ui.add_space(12.0);
    }

    // ---------------------------------------------------------------- Mini player
    if tab == SettingsTab::MiniPlayer {
        cards(ui, &colors, crate::i18n::tr("迷你播放器"), |ui| {
            row(ui, crate::i18n::tr("行为"), &colors, |ui| {
                let mut visible = app.settings.mini.visible;
                if ui
                    .add_enabled(
                        app.settings.features.mini_player,
                        egui::Checkbox::new(&mut visible, crate::i18n::tr("显示迷你播放器")),
                    )
                    .changed()
                {
                    app.toggle_mini_player();
                    settings_changed = true;
                }
                if ui
                    .checkbox(
                        &mut app.settings.mini.always_on_top,
                        crate::i18n::tr("窗口置顶"),
                    )
                    .changed()
                {
                    settings_changed = true;
                }
                if ui
                    .checkbox(
                        &mut app.settings.mini.show_in_taskbar,
                        crate::i18n::tr("在任务栏显示"),
                    )
                    .on_hover_text(crate::i18n::tr(
                        "关闭时迷你播放器只保留在桌面/置顶层，不占任务栏按钮",
                    ))
                    .changed()
                {
                    settings_changed = true;
                }
                if ui
                    .checkbox(
                        &mut app.settings.mini.native_window,
                        crate::i18n::tr("原生窗口渲染（推荐）"),
                    )
                    .on_hover_text(crate::i18n::tr(
                        "使用 Direct2D 原生窗口绘制迷你播放器，主窗口隐藏时不再为它重绘；关闭则使用 egui 版本",
                    ))
                    .changed()
                {
                    settings_changed = true;
                }
                let mut opacity = app.settings.mini.opacity;
                if ui
                    .add(
                        egui::Slider::new(&mut opacity, 0.5..=1.0)
                            .text(crate::i18n::tr("不透明度")),
                    )
                    .changed()
                {
                    app.settings.mini.opacity = opacity;
                    settings_changed = true;
                }
            });
            row(ui, crate::i18n::tr("歌词"), &colors, |ui| {
                if ui
                    .checkbox(
                        &mut app.settings.mini.show_lyrics,
                        crate::i18n::tr("显示当前歌词"),
                    )
                    .changed()
                {
                    settings_changed = true;
                }
                let lyrics_off = !app.settings.mini.show_lyrics;
                let mut lyrics_lines = app.settings.mini.lyrics_lines as f32;
                if ui
                    .add_enabled(
                        !lyrics_off,
                        egui::Slider::new(&mut lyrics_lines, 1.0..=2.0)
                            .integer()
                            .text(crate::i18n::tr("歌词行数")),
                    )
                    .changed()
                {
                    app.settings.mini.lyrics_lines = lyrics_lines as u8;
                    settings_changed = true;
                }
                if ui
                    .add_enabled(
                        !lyrics_off,
                        egui::Checkbox::new(
                            &mut app.settings.mini.lyrics_bold,
                            crate::i18n::tr("粗体歌词"),
                        ),
                    )
                    .changed()
                {
                    settings_changed = true;
                }
                if ui
                    .add_enabled(
                        !lyrics_off,
                        egui::Checkbox::new(
                            &mut app.settings.mini.karaoke,
                            crate::i18n::tr("逐字高亮"),
                        ),
                    )
                    .on_hover_text(crate::i18n::tr(
                        "普通 LRC 也会按行时间平滑渐变；开启“仅增强型 LRC”后只对带逐字时间轴的歌词生效",
                    ))
                    .changed()
                {
                    settings_changed = true;
                }
                if ui
                    .add_enabled(
                        !lyrics_off && app.settings.mini.karaoke,
                        egui::Checkbox::new(
                            &mut app.settings.mini.karaoke_words_only,
                            crate::i18n::tr("仅增强型 LRC"),
                        ),
                    )
                    .on_hover_text(crate::i18n::tr(
                        "仅当歌词包含 <mm:ss.xx> 逐字时间时启用逐字填充；关闭时普通 LRC 也按时间比例渐变",
                    ))
                    .changed()
                {
                    settings_changed = true;
                }
                if ui
                    .add_enabled(
                        !lyrics_off,
                        egui::Checkbox::new(
                            &mut app.settings.mini.show_translation,
                            crate::i18n::tr("显示翻译歌词"),
                        ),
                    )
                    .changed()
                {
                    settings_changed = true;
                }
                let mut lyrics_size = app.settings.mini.lyrics_font_size;
                if ui
                    .add_enabled(
                        !lyrics_off,
                        egui::Slider::new(&mut lyrics_size, 10.0..=22.0)
                            .text(crate::i18n::tr("歌词字号")),
                    )
                    .changed()
                {
                    app.settings.mini.lyrics_font_size = lyrics_size;
                    settings_changed = true;
                }
            });
            row(ui, crate::i18n::tr("窗口大小"), &colors, |ui| {
                let mut width = app.settings.mini.width;
                if ui
                    .add(
                        egui::Slider::new(&mut width, 280.0..=900.0)
                            .text(crate::i18n::tr("窗口宽度")),
                    )
                    .changed()
                {
                    app.settings.mini.width = width;
                    settings_changed = true;
                }
                if ui
                    .checkbox(
                        &mut app.settings.mini.auto_height,
                        crate::i18n::tr("自动高度"),
                    )
                    .on_hover_text(crate::i18n::tr(
                        "根据歌词行数/字号自动调整窗口高度；拖动窗口边缘会切换为手动高度",
                    ))
                    .changed()
                {
                    if app.settings.mini.auto_height {
                        app.settings.mini.height =
                            crate::app::views::mini_player::auto_window_height(&app.settings.mini);
                    }
                    settings_changed = true;
                }
                let mut height = app.settings.mini.height;
                if ui
                    .add_enabled(
                        !app.settings.mini.auto_height,
                        egui::Slider::new(&mut height, 120.0..=400.0)
                            .text(crate::i18n::tr("窗口高度")),
                    )
                    .on_hover_text(crate::i18n::tr(
                        "也可以直接拖动迷你播放器窗口边缘调整大小",
                    ))
                    .changed()
                {
                    app.settings.mini.height = height;
                    settings_changed = true;
                }
                ui.label(
                    RichText::new(crate::i18n::tr(
                        "歌词显示当前/后续行；普通 LRC 也会按时渐变，逐字高亮需要带时间轴的歌词；窗口高度自动调整",
                    ))
                    .size(10.0)
                    .color(colors.text_faint),
                );
            });
            if !app.settings.features.mini_player {
                ui.label(
                    RichText::new(crate::i18n::tr("迷你播放器已在功能开关中关闭"))
                        .size(10.0)
                        .color(colors.text_faint),
                );
            }
        });
        ui.add_space(12.0);
    }

    // ---------------------------------------------------------------- Library
    if tab == SettingsTab::Library {
        cards(ui, &colors, crate::i18n::tr("音乐目录"), |ui| {
            let roots = app.settings.library.folders.clone();
            if roots.is_empty() {
                ui.label(
                    RichText::new(crate::i18n::tr("还没有添加音乐目录"))
                        .size(11.0)
                        .color(colors.text_weak),
                );
            }
            for root in roots {
                let name = root
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| root.display().to_string());
                let path_text = root.display().to_string();
                egui::Frame::new()
                    .fill(colors.bg2)
                    .stroke(egui::Stroke::new(1.0, colors.line))
                    .corner_radius(egui::CornerRadius::same(8))
                    .inner_margin(egui::Margin::symmetric(12, 9))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            let button_w = 64.0;
                            let spacing = ui.spacing().item_spacing.x;
                            let text_w =
                                (ui.available_width() - button_w * 2.0 - spacing * 2.0)
                                    .max(80.0);
                            ui.allocate_ui_with_layout(
                                egui::vec2(text_w, 0.0),
                                egui::Layout::top_down(egui::Align::LEFT),
                                |ui| {
                                    ui.set_width(text_w);
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(&name)
                                                .size(12.5)
                                                .strong()
                                                .color(colors.text),
                                        )
                                        .truncate(),
                                    );
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(&path_text)
                                                .size(10.5)
                                                .color(colors.text_weak),
                                        )
                                        .truncate(),
                                    )
                                    .on_hover_text(&path_text);
                                },
                            );
                            if ui
                                .add_sized(
                                    [button_w, 30.0],
                                    egui::Button::new(crate::i18n::tr("打开")),
                                )
                                .clicked()
                            {
                                app.open_browser(root.clone());
                                app.set_page(crate::model::Page::Library);
                            }
                            if ui
                                .add_sized(
                                    [button_w, 30.0],
                                    egui::Button::new(crate::i18n::tr("移除")),
                                )
                                .clicked()
                            {
                                app.remove_library_folder(&root);
                                settings_changed = true;
                            }
                        });
                    });
                ui.add_space(6.0);
            }
            if widgets::secondary_button(ui, crate::i18n::tr("添加音乐目录"), &colors).clicked() {
                app.browse_folder_dialog();
                settings_changed = true;
            }
        });
        ui.add_space(12.0);
        cards(ui, &colors, crate::i18n::tr("索引与监听"), |ui| {
            row(ui, crate::i18n::tr("启动时扫描"), &colors, |ui| {
                if ui
                    .checkbox(
                        &mut app.settings.library.scan_on_start,
                        crate::i18n::tr("启动后自动更新曲库索引"),
                    )
                    .on_hover_text(crate::i18n::tr(
                        "只重新解析 size/mtime 变化的文件；首次全量索引仍需要等待",
                    ))
                    .changed()
                {
                    settings_changed = true;
                }
            });
            row(ui, crate::i18n::tr("自动监听"), &colors, |ui| {
                if ui
                    .checkbox(
                        &mut app.settings.library.auto_watch,
                        crate::i18n::tr("目录增删改时自动更新索引"),
                    )
                    .on_hover_text(crate::i18n::tr(
                        "使用系统文件监听，只解析变化的文件；无需重启播放器",
                    ))
                    .changed()
                {
                    app.sync_library_watch();
                    settings_changed = true;
                }
            });
            row(ui, crate::i18n::tr("符号链接"), &colors, |ui| {
                if ui
                    .checkbox(
                        &mut app.settings.library.follow_symlinks,
                        crate::i18n::tr("扫描时跟随符号链接"),
                    )
                    .changed()
                {
                    settings_changed = true;
                }
            });
            row(ui, crate::i18n::tr("音频扩展名"), &colors, |ui| {
                let response = ui.add(
                    egui::TextEdit::singleline(&mut app.library_extensions_text)
                        .desired_width(360.0)
                        .hint_text("mp3, flac, wav, ogg …"),
                );
                if response.changed() {
                    let extensions: Vec<String> = app
                        .library_extensions_text
                        .split([' ', ',', ';', '\n', '\t'])
                        .map(|ext| ext.trim().trim_start_matches('.').to_ascii_lowercase())
                        .filter(|ext| !ext.is_empty())
                        .collect();
                    if !extensions.is_empty() {
                        app.settings.library.extensions = extensions;
                        settings_changed = true;
                    }
                }
                // Restarting the OS watcher on every keystroke is wasteful; wait
                // until the field loses focus. An empty list keeps the previous
                // extensions, so restore the canonical text as well.
                if response.lost_focus() {
                    app.library_extensions_text =
                        app.settings.library.extensions.join(", ");
                    app.sync_library_watch();
                }
            });
            row(ui, crate::i18n::tr("索引操作"), &colors, |ui| {
                if app.db_index_active {
                    ui.spinner();
                    if ui.button(crate::i18n::tr("取消索引")).clicked() {
                        app.cancel_library_index();
                    }
                    ui.label(
                        RichText::new(crate::trf!(
                            "已写入 {} 首",
                            app.db_index_progress
                        ))
                        .size(10.5)
                        .color(colors.text_weak),
                    );
                } else if widgets::secondary_button(
                    ui,
                    crate::i18n::tr("立即更新索引"),
                    &colors,
                )
                .clicked()
                {
                    app.start_library_index();
                }
            });
            ui.label(
                RichText::new(crate::i18n::tr(
                    "自动监听只更新变化的文件；首次使用请先点击“立即更新索引”。",
                ))
                .size(10.0)
                .color(colors.text_faint),
            );
        });
        ui.add_space(12.0);
    }

    if tab == SettingsTab::Online {
        cards(ui, &colors, app.t("settings.online"), |ui| {
            row(ui, app.t("settings.timeout"), &colors, |ui| {
                let mut t = app.settings.network.timeout_secs;
                if ui
                    .add(egui::DragValue::new(&mut t).range(2..=120).suffix(" s"))
                    .changed()
                {
                    app.settings.network.timeout_secs = t;
                    settings_changed = true;
                }
                let mut retries = app.settings.network.retries;
                if ui
                    .add(egui::DragValue::new(&mut retries).range(0..=10))
                    .changed()
                {
                    app.settings.network.retries = retries;
                    settings_changed = true;
                }
            });
            row(ui, crate::i18n::tr("连接超时"), &colors, |ui| {
                let mut connect = app.settings.network.connect_timeout_secs;
                if ui
                    .add(
                        egui::DragValue::new(&mut connect)
                            .range(1..=60)
                            .suffix(" s"),
                    )
                    .changed()
                {
                    app.settings.network.connect_timeout_secs = connect;
                    settings_changed = true;
                }
            });
            row(ui, crate::i18n::tr("API 请求间隔"), &colors, |ui| {
                let mut interval = app.settings.network.api_min_interval_ms;
                if ui
                    .add(
                        egui::DragValue::new(&mut interval)
                            .range(0..=3000)
                            .speed(10.0)
                            .suffix(" ms"),
                    )
                    .changed()
                {
                    app.settings.network.api_min_interval_ms = interval;
                    app.api.set_min_interval_ms(interval);
                    settings_changed = true;
                }
                ui.label(
                    RichText::new(crate::i18n::tr(
                        "0 表示不限速；只限制搜索/元信息等 API 请求，不影响音频流和下载",
                    ))
                    .size(10.0)
                    .color(colors.text_faint),
                );
            });
            row(ui, crate::i18n::tr("专辑/歌手每页"), &colors, |ui| {
                let mut page_size = app.settings.network.album_page_size;
                if ui
                    .add(
                        egui::DragValue::new(&mut page_size)
                            .range(10..=200)
                            .speed(5.0)
                            .suffix(" 条"),
                    )
                    .changed()
                {
                    app.settings.network.album_page_size = page_size;
                    settings_changed = true;
                }
                if ui.button(crate::i18n::tr("刷新当前页")).clicked() {
                    if app.album_view.is_some() {
                        let page = app.album_view.as_ref().map(|view| view.page).unwrap_or(1);
                        app.album_to_page(page);
                    } else if app.artist_view.is_some() {
                        let page = app.artist_view.as_ref().map(|view| view.page).unwrap_or(1);
                        app.artist_to_page(page);
                    }
                }
            });
            row(ui, "User-Agent", &colors, |ui| {
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut app.settings.network.user_agent)
                            .desired_width(360.0),
                    )
                    .changed()
                {
                    settings_changed = true;
                }
            });
            row(ui, app.t("settings.proxy"), &colors, |ui| {
                let mut mode = app.settings.network.proxy_mode;
                egui::ComboBox::from_id_salt("proxy-mode")
                    .selected_text(proxy_label(mode))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut mode,
                            ProxyMode::System,
                            crate::i18n::tr("系统代理"),
                        );
                        ui.selectable_value(
                            &mut mode,
                            ProxyMode::None,
                            crate::i18n::tr("不使用代理"),
                        );
                        ui.selectable_value(&mut mode, ProxyMode::Manual, crate::i18n::tr("手动"));
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
            row(ui, crate::i18n::tr("离线模式"), &colors, |ui| {
                if ui
                    .checkbox(
                        &mut app.settings.network.offline,
                        crate::i18n::tr("仅播放本地/已缓存内容"),
                    )
                    .changed()
                {
                    settings_changed = true;
                }
            });

            ui.add_space(10.0);
            ui.label(
                RichText::new(crate::i18n::tr("下载"))
                    .size(14.0)
                    .strong()
                    .color(colors.text),
            );
            ui.label(
                RichText::new(crate::i18n::tr(
                    "下载使用在线音源的当前音质；任务在后台线程执行，可在 曲库 → 下载 查看进度",
                ))
                .size(10.0)
                .color(colors.text_faint),
            );
            ui.add_space(4.0);
            row(ui, app.t("settings.download_dir"), &colors, |ui| {
                ui.add(
                    egui::Label::new(
                        RichText::new(crate::util::truncate_chars(
                            &app.download_dir().display().to_string(),
                            64,
                        ))
                        .size(11.0)
                        .color(colors.text_weak),
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
            row(ui, crate::i18n::tr("文件名模板"), &colors, |ui| {
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut app.settings.download.template)
                            .desired_width(360.0)
                            .hint_text("{artist}/{album}/{title}"),
                    )
                    .changed()
                {
                    settings_changed = true;
                }
                ui.label(
                    RichText::new(crate::i18n::tr(
                        "可用：{artist} {album} {title} {genre} {year} {track} {source}",
                    ))
                    .size(10.0)
                    .color(colors.text_faint),
                );
            });
            row(ui, crate::i18n::tr("并发与冲突"), &colors, |ui| {
                let mut concurrency = app.settings.download.concurrency;
                if ui
                    .add(
                        egui::DragValue::new(&mut concurrency)
                            .range(1..=4)
                            .suffix(crate::i18n::tr(" 个线程")),
                    )
                    .changed()
                {
                    app.settings.download.concurrency = concurrency;
                    app.downloads.set_concurrency(concurrency as usize);
                    settings_changed = true;
                }
                ui.label(
                    RichText::new(crate::i18n::tr("修改立即生效；正在下载的任务不会被中断"))
                        .size(10.0)
                        .color(colors.text_faint),
                );
                let mut conflict = app.settings.download.conflict;
                egui::ComboBox::from_id_salt("download-conflict")
                    .selected_text(match conflict {
                        ConflictPolicy::Rename => crate::i18n::tr("重命名"),
                        ConflictPolicy::Skip => crate::i18n::tr("跳过"),
                        ConflictPolicy::Overwrite => crate::i18n::tr("覆盖"),
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut conflict,
                            ConflictPolicy::Rename,
                            crate::i18n::tr("重命名"),
                        );
                        ui.selectable_value(
                            &mut conflict,
                            ConflictPolicy::Skip,
                            crate::i18n::tr("跳过"),
                        );
                        ui.selectable_value(
                            &mut conflict,
                            ConflictPolicy::Overwrite,
                            crate::i18n::tr("覆盖"),
                        );
                    });
                if conflict != app.settings.download.conflict {
                    app.settings.download.conflict = conflict;
                    settings_changed = true;
                }
                ui.label(
                    RichText::new(crate::i18n::tr("并发线程数在下次启动时生效"))
                        .size(10.0)
                        .color(colors.text_faint),
                );
            });
            row(ui, crate::i18n::tr("下载后处理"), &colors, |ui| {
                if ui
                    .checkbox(
                        &mut app.settings.download.embed_tags,
                        crate::i18n::tr("写入标题/歌手/专辑标签"),
                    )
                    .changed()
                {
                    settings_changed = true;
                }
                if ui
                    .checkbox(
                        &mut app.settings.download.embed_cover,
                        crate::i18n::tr("嵌入封面图片"),
                    )
                    .changed()
                {
                    settings_changed = true;
                }
                if ui
                    .checkbox(
                        &mut app.settings.download.save_lyrics,
                        crate::i18n::tr("保存同名 .lrc 歌词"),
                    )
                    .changed()
                {
                    settings_changed = true;
                }
                if ui
                    .checkbox(
                        &mut app.settings.download.add_to_library,
                        crate::i18n::tr("下载完成后加入曲库索引"),
                    )
                    .changed()
                {
                    settings_changed = true;
                }
            });
            if !app.settings.features.downloads {
                ui.label(
                    RichText::new(crate::i18n::tr("下载功能已在 功能开关 中关闭"))
                        .size(10.0)
                        .color(colors.warning),
                );
            } else if widgets::secondary_button(ui, crate::i18n::tr("打开下载管理"), &colors)
                .clicked()
            {
                app.library_tab = crate::app::views::LibraryTab::Downloads;
                app.set_page(crate::model::Page::Library);
            }

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
                        .corner_radius(egui::CornerRadius::same(8))
                        .inner_margin(egui::Margin::symmetric(12, 10))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                if ui.checkbox(&mut source.enabled, "").changed() {
                                    settings_changed = true;
                                }
                                ui.label(
                                    RichText::new(format!(
                                        "{name}  ·  {}",
                                        source_kind_label(kind)
                                    ))
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
                                            .desired_width(300.0)
                                            .hint_text(crate::i18n::tr("填写 Bamboo API 地址")),
                                    )
                                    .changed()
                                {
                                    settings_changed = true;
                                }
                            });
                            row(ui, crate::i18n::tr("脚本"), &colors, |ui| {
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
                                        RichText::new(crate::i18n::tr(
                                            "基址以 .php 结尾时直接使用",
                                        ))
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
                                        ui.selectable_value(
                                            &mut quality,
                                            Quality::K128,
                                            "128 kbps",
                                        );
                                        ui.selectable_value(
                                            &mut quality,
                                            Quality::K320,
                                            "320 kbps",
                                        );
                                        ui.selectable_value(
                                            &mut quality,
                                            Quality::Flac,
                                            crate::i18n::tr("无损 FLAC"),
                                        );
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
                                    RichText::new(crate::i18n::tr("仅建议在本地测试环境使用"))
                                        .size(10.0)
                                        .color(colors.text_faint),
                                );
                            });
                            ui.collapsing(crate::i18n::tr("请求头 / Cookie（高级）"), |ui| {
                                row(ui, "Cookie", &colors, |ui| {
                                    if ui
                                        .add(
                                            egui::TextEdit::singleline(&mut source.cookie)
                                                .hint_text("name=value; name2=value2")
                                                .desired_width(320.0),
                                        )
                                        .changed()
                                    {
                                        settings_changed = true;
                                    }
                                });
                                let mut remove_header: Option<usize> = None;
                                for (index, header) in source.headers.iter_mut().enumerate() {
                                    ui.horizontal(|ui| {
                                        if ui
                                            .add(
                                                egui::TextEdit::singleline(&mut header.name)
                                                    .hint_text(crate::i18n::tr("名称"))
                                                    .desired_width(120.0),
                                            )
                                            .changed()
                                        {
                                            settings_changed = true;
                                        }
                                        if ui
                                            .add(
                                                egui::TextEdit::singleline(&mut header.value)
                                                    .hint_text(crate::i18n::tr("值"))
                                                    .desired_width(210.0),
                                            )
                                            .changed()
                                        {
                                            settings_changed = true;
                                        }
                                        if ui.small_button(crate::i18n::tr("删除")).clicked() {
                                            remove_header = Some(index);
                                        }
                                    });
                                }
                                if let Some(index) = remove_header {
                                    source.headers.remove(index);
                                    settings_changed = true;
                                }
                                if ui.button(crate::i18n::tr("添加请求头")).clicked() {
                                    source.headers.push(crate::config::HeaderPair::default());
                                    settings_changed = true;
                                }
                                ui.label(
                                    RichText::new(crate::i18n::tr(
                                        "同时用于 API 与音频流；X-Session-Key 不会转发给流地址",
                                    ))
                                    .size(10.0)
                                    .color(colors.text_faint),
                                );
                            });
                        });
                });
                ui.add_space(6.0);
            }
            if let Some(id) = test_source {
                app.test_source(&id);
            }
            if let Some((message, ok)) = app.source_test_message.clone() {
                ui.add_space(4.0);
                ui.label(RichText::new(message).size(11.0).color(if ok {
                    colors.success
                } else {
                    colors.danger
                }));
            }
            ui.label(
            RichText::new(crate::i18n::tr("填写 Bamboo API 地址（例如 http://127.0.0.1/musicnew/apis/local）并启用后即可搜索/播放。"))
                .size(10.5)
                .color(colors.text_faint),
        );
        });
        ui.add_space(12.0);
    }

    // ---------------------------------------------------------------- Playlist
    if tab == SettingsTab::Playlist {
        cards(ui, &colors, crate::i18n::tr("歌单"), |ui| {
            row(ui, crate::i18n::tr("导入行为"), &colors, |ui| {
                if ui
                    .checkbox(
                        &mut app.settings.playlist.auto_refresh_on_startup,
                        crate::i18n::tr("启动时重新展开当前歌单"),
                    )
                    .changed()
                {
                    settings_changed = true;
                }
                if ui
                    .checkbox(
                        &mut app.settings.playlist.export_utf8_bom,
                        crate::i18n::tr("导出 UTF-8 BOM"),
                    )
                    .changed()
                {
                    settings_changed = true;
                }
            });
            if let Some(playlist) = app.current_playlist.clone() {
                long_value(
                    ui,
                    crate::i18n::tr("当前歌单"),
                    &playlist.display().to_string(),
                    &colors,
                );
                ui.horizontal(|ui| {
                    if widgets::secondary_button(ui, crate::i18n::tr("重新扫描歌单"), &colors)
                        .clicked()
                    {
                        app.rescan_current_playlist();
                    }
                    if widgets::secondary_button(ui, crate::i18n::tr("导出到文件"), &colors)
                        .clicked()
                    {
                        app.export_playlist_dialog();
                    }
                });
            } else {
                ui.label(
                    RichText::new(crate::i18n::tr("尚未打开 .mp3list 歌单"))
                        .size(11.0)
                        .color(colors.text_faint),
                );
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if widgets::primary_button(ui, crate::i18n::tr("导入歌单"), &colors).clicked() {
                    app.import_playlist_dialog();
                }
                if widgets::secondary_button(ui, crate::i18n::tr("导出当前队列"), &colors).clicked()
                {
                    app.export_playlist_dialog();
                }
            });
            ui.add(
            egui::Label::new(
                RichText::new(
                    crate::i18n::tr("每次导入都会重新扫描 #SEARCH# 目录；当前曲目按文件路径匹配，并用上次保存的播放位置恢复。"),
                )
                .size(10.5)
                .color(colors.text_faint),
            )
            .wrap(),
        );
        });
        ui.add_space(12.0);
    }

    // ---------------------------------------------------------------- Window & tray
    if tab == SettingsTab::Window {
        cards(ui, &colors, app.t("settings.window"), |ui| {
            row(ui, app.t("settings.close_behavior"), &colors, |ui| {
                let mut behavior = app.settings.window.close_behavior;
                egui::ComboBox::from_id_salt("close-behavior")
                    .selected_text(close_label(behavior))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut behavior,
                            CloseBehavior::Tray,
                            app.t("settings.close_tray"),
                        );
                        ui.selectable_value(
                            &mut behavior,
                            CloseBehavior::Exit,
                            app.t("settings.close_exit"),
                        );
                        ui.selectable_value(
                            &mut behavior,
                            CloseBehavior::Ask,
                            app.t("settings.close_ask"),
                        );
                    });
                if behavior != app.settings.window.close_behavior {
                    app.settings.window.close_behavior = behavior;
                    settings_changed = true;
                }
            });
            row(ui, crate::i18n::tr("多实例"), &colors, |ui| {
                if ui
                    .checkbox(
                        &mut app.settings.window.allow_multiple_instances,
                        crate::i18n::tr("允许多个实例同时运行"),
                    )
                    .changed()
                {
                    settings_changed = true;
                }
                ui.label(
                    RichText::new(crate::i18n::tr(
                        "重启后生效；多实例共享设置、曲库与会话文件",
                    ))
                    .size(10.0)
                    .color(colors.text_faint),
                );
            });
            row(ui, crate::i18n::tr("托盘"), &colors, |ui| {
                if ui
                    .checkbox(
                        &mut app.settings.features.tray,
                        crate::i18n::tr("启用托盘图标"),
                    )
                    .changed()
                {
                    app.tray = None;
                    app.tray_failed = false;
                    settings_changed = true;
                }
                if ui
                    .checkbox(
                        &mut app.settings.window.start_minimized,
                        crate::i18n::t(app.lang, "settings.start_minimized"),
                    )
                    .changed()
                {
                    settings_changed = true;
                }
                if ui
                    .checkbox(
                        &mut app.settings.window.remember_position,
                        crate::i18n::tr("记住窗口位置"),
                    )
                    .changed()
                {
                    settings_changed = true;
                }
                if ui
                    .checkbox(
                        &mut app.settings.window.trim_working_set_on_idle,
                        crate::i18n::tr("空闲时自动释放内存"),
                    )
                    .changed()
                {
                    settings_changed = true;
                }
            });
            row(ui, crate::i18n::tr("歌词全屏"), &colors, |ui| {
                let mut mode = app.settings.window.lyrics_fullscreen;
                egui::ComboBox::from_id_salt("lyrics-fullscreen-mode")
                    .selected_text(fullscreen_mode_label(mode))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut mode,
                            crate::config::FullscreenMode::Borderless,
                            crate::i18n::tr("无边框全屏（默认，不切换显示模式）"),
                        );
                        ui.selectable_value(
                            &mut mode,
                            crate::config::FullscreenMode::Maximized,
                            crate::i18n::tr("最大化窗口（保留任务栏）"),
                        );
                    });
                if mode != app.settings.window.lyrics_fullscreen {
                    app.settings.window.lyrics_fullscreen = mode;
                    settings_changed = true;
                }
            });
            row(ui, crate::i18n::tr("歌词全屏显示器"), &colors, |ui| {
                let monitors = window::monitors();
                let selected = app
                    .settings
                    .window
                    .lyrics_monitor
                    .filter(|index| *index < monitors.len());
                let monitor_label = |index: usize, monitor: &window::MonitorInfo| {
                    crate::trf!(
                        "显示器 {}{}（{}×{}）",
                        index + 1,
                        if monitor.primary {
                            crate::i18n::tr(" · 主")
                        } else {
                            ""
                        },
                        monitor.width,
                        monitor.height
                    )
                };
                let current = selected
                    .map(|index| monitor_label(index, &monitors[index]))
                    .unwrap_or_else(|| crate::i18n::tr("跟随当前窗口").to_string());
                egui::ComboBox::from_id_salt("lyrics-monitor")
                    .selected_text(current)
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_label(selected.is_none(), crate::i18n::tr("跟随当前窗口"))
                            .clicked()
                        {
                            app.settings.window.lyrics_monitor = None;
                            settings_changed = true;
                        }
                        for (index, monitor) in monitors.iter().enumerate() {
                            if ui
                                .selectable_label(
                                    selected == Some(index),
                                    monitor_label(index, monitor),
                                )
                                .clicked()
                            {
                                app.settings.window.lyrics_monitor = Some(index);
                                settings_changed = true;
                            }
                        }
                    });
                ui.label(
                    RichText::new(
                        crate::i18n::tr("大歌词页全屏会移动主窗口到该显示器（退出后恢复）；独立歌词窗口直接全屏在该显示器"),
                    )
                    .size(10.0)
                    .color(colors.text_faint),
                );
            });
            row(ui, crate::i18n::tr("独立歌词窗口"), &colors, |ui| {
                let label = if app.lyrics_window_open {
                    crate::i18n::tr("关闭独立歌词窗口")
                } else {
                    crate::i18n::tr("打开独立歌词窗口")
                };
                if widgets::secondary_button(ui, label, &colors).clicked() {
                    app.toggle_lyrics_window();
                }
                let mut windowed = app.lyrics_window_windowed;
                if ui
                    .checkbox(
                        &mut windowed,
                        crate::i18n::tr("以窗口模式打开（可调整大小）"),
                    )
                    .changed()
                {
                    app.set_lyrics_window_windowed(windowed);
                }
                ui.label(
                    RichText::new(crate::i18n::tr(
                        "独立窗口可放在另一台显示器；窗口模式下可拖动边缘调整大小，尺寸和位置会记住；F11 切换全屏/窗口",
                    ))
                    .size(10.0)
                    .color(colors.text_faint),
                );
            });
            row(ui, app.t("settings.capture_protection"), &colors, |ui| {
                if ui
                    .checkbox(
                        &mut app.settings.window.capture_protection,
                        crate::i18n::tr("主窗口不被录屏/截图捕获"),
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
            row(
                ui,
                crate::i18n::tr("歌词窗口捕获保护"),
                &colors,
                |ui| {
                    if ui
                        .checkbox(
                            &mut app.settings.window.lyrics_capture_protection,
                            crate::i18n::tr("独立歌词窗口不被录屏/截图捕获"),
                        )
                        .changed()
                    {
                        settings_changed = true;
                        app.ctx.request_repaint();
                    }
                    ui.label(
                        RichText::new(crate::i18n::tr(
                            "全屏/窗口化歌词窗口都不会出现在录屏与截图中",
                        ))
                        .size(10.0)
                        .color(colors.text_faint),
                    );
                },
            );
        });
        ui.add_space(12.0);

        cards(ui, &colors, crate::i18n::tr("系统集成"), |ui| {
            row(ui, crate::i18n::tr("开机启动"), &colors, |ui| {
                let mut enabled = app.autostart_enabled();
                if ui
                    .checkbox(
                        &mut enabled,
                        crate::i18n::tr("登录 Windows 后自动启动 Bamboo Music Player"),
                    )
                    .changed()
                {
                    app.set_autostart(enabled);
                    settings_changed = true;
                }
                ui.label(
                    RichText::new(crate::i18n::tr(
                        "写入 HKCU\\...\\CurrentVersion\\Run，无需管理员权限",
                    ))
                    .size(10.0)
                    .color(colors.text_faint),
                );
            });
            row(ui, crate::i18n::tr("文件关联"), &colors, |ui| {
                let mut enabled = app.associations_enabled();
                if ui
                    .checkbox(
                        &mut enabled,
                        crate::i18n::tr("把 mp3/flac/wav/ogg/m4a/aac/opus 等关联到本程序"),
                    )
                    .changed()
                {
                    app.set_file_associations(enabled);
                    settings_changed = true;
                }
                ui.label(
                    RichText::new(crate::i18n::tr(
                        "仅写入当前用户；Windows 10/11 可能仍需在“默认应用”中确认打开方式",
                    ))
                    .size(10.0)
                    .color(colors.text_faint),
                );
            });
            row(ui, crate::i18n::tr(".mp3list 歌单关联"), &colors, |ui| {
                let mut enabled = app.playlist_association_enabled();
                if ui
                    .checkbox(
                        &mut enabled,
                        crate::i18n::tr("把 .mp3list 歌单文件关联到本程序"),
                    )
                    .changed()
                {
                    app.set_playlist_association(enabled);
                    settings_changed = true;
                }
                ui.label(
                    RichText::new(crate::i18n::tr(
                        "与上面的音频关联相互独立；双击 .mp3list 直接打开歌单。写入当前用户，一般无需管理员权限",
                    ))
                    .size(10.0)
                    .color(colors.text_faint),
                );
            });
            row(ui, crate::i18n::tr("系统通知"), &colors, |ui| {
                if ui
                    .checkbox(
                        &mut app.settings.notifications.enabled,
                        crate::i18n::tr("切换曲目时显示 Windows 通知"),
                    )
                    .changed()
                {
                    settings_changed = true;
                }
                if ui
                    .checkbox(
                        &mut app.settings.notifications.only_when_unfocused,
                        crate::i18n::tr("仅在窗口未聚焦时"),
                    )
                    .changed()
                {
                    settings_changed = true;
                }
                if ui
                    .checkbox(
                        &mut app.settings.notifications.on_track_change,
                        crate::i18n::tr("自动切歌也通知"),
                    )
                    .changed()
                {
                    settings_changed = true;
                }
                ui.label(
                    RichText::new(crate::i18n::tr(
                        "未打包应用的通知可能需要在系统中允许；失败只记录日志",
                    ))
                    .size(10.0)
                    .color(colors.text_faint),
                );
            });
            row(ui, crate::i18n::tr("命令面板"), &colors, |ui| {
                if widgets::secondary_button(ui, crate::i18n::tr("打开命令面板 (Ctrl+K)"), &colors)
                    .clicked()
                {
                    app.toggle_command_palette();
                }
                ui.label(
                    RichText::new(crate::i18n::tr("搜索页面、播放控制、智能歌单与系统功能"))
                        .size(10.0)
                        .color(colors.text_faint),
                );
            });
        });
        ui.add_space(12.0);
    }
    if tab == SettingsTab::Features {
        cards(ui, &colors, app.t("settings.features"), |ui| {
            ui.label(
                RichText::new(app.t("settings.features_hint"))
                    .size(11.0)
                    .color(colors.text_weak),
            );
            ui.add_space(6.0);
            let f = &mut app.settings.features;
            let mut changed = false;
            let width = ui.available_width().max(240.0);
            let columns = ((width / 170.0).floor() as usize).clamp(1, 4);
            let mut column = 0usize;
            macro_rules! toggle {
                ($ui:expr, $field:ident, $label:expr) => {{
                    if $ui.checkbox(&mut f.$field, $label).changed() {
                        changed = true;
                    }
                    column += 1;
                    if column >= columns {
                        $ui.end_row();
                        column = 0;
                    }
                }};
            }
            let spacing = 16.0;
            let cell_width = ((width - spacing * (columns.saturating_sub(1)) as f32)
                / columns as f32)
                .max(120.0);
            egui::Grid::new("bamboo-feature-grid")
                .num_columns(columns)
                .min_col_width(cell_width)
                .max_col_width(cell_width)
                .spacing([spacing, 8.0])
                .show(ui, |ui| {
                    toggle!(ui, crossfade, crate::i18n::tr("交叉淡化"));
                    toggle!(ui, gapless, crate::i18n::tr("无缝播放"));
                    toggle!(ui, replaygain, crate::i18n::tr("响度均衡"));
                    toggle!(ui, mini_player, crate::i18n::tr("迷你播放器"));
                    toggle!(ui, equalizer, crate::i18n::tr("均衡器"));
                    toggle!(ui, fullscreen_lyrics, crate::i18n::tr("全屏歌词"));
                    toggle!(ui, lyric_editor, crate::i18n::tr("歌词编辑器"));
                    toggle!(ui, speed_control, crate::i18n::tr("变速播放"));
                    toggle!(ui, visualizer, crate::i18n::tr("音频可视化"));
                    toggle!(ui, ab_repeat, crate::i18n::tr("A-B 循环"));
                    toggle!(ui, notifications, crate::i18n::tr("系统通知"));
                    toggle!(ui, sleep_timer, crate::i18n::tr("睡眠定时"));
                    toggle!(ui, internet_radio, crate::i18n::tr("网络电台 / 随机电台"));
                    toggle!(ui, discover, crate::i18n::tr("随机漫游推荐"));
                    toggle!(ui, smart_playlists, crate::i18n::tr("智能歌单"));
                    toggle!(ui, statistics, crate::i18n::tr("播放统计"));
                    toggle!(ui, command_palette, crate::i18n::tr("命令面板"));
                    toggle!(ui, duplicate_finder, crate::i18n::tr("查重与缺失"));
                    toggle!(ui, accessibility, crate::i18n::tr("无障碍"));
                    toggle!(ui, desktop_lyrics, crate::i18n::tr("桌面歌词"));
                    toggle!(ui, downloads, crate::i18n::tr("在线下载"));
                    toggle!(ui, media_controls, crate::i18n::tr("系统媒体控制"));
                    toggle!(ui, jump_list, crate::i18n::tr("任务栏跳转列表"));
                    if column != 0 {
                        ui.end_row();
                    }
                });
            if changed {
                settings_changed = true;
                app.jump_list_dirty = true;
            }
        });
        ui.add_space(12.0);
    }

    // ---------------------------------------------------------------- About
    if tab == SettingsTab::About {
        cards(ui, &colors, app.t("settings.about"), |ui| {
            widgets::key_value(
                ui,
                app.t("settings.version"),
                env!("CARGO_PKG_VERSION"),
                &colors,
            );
            widgets::key_value(ui, crate::i18n::tr("作者"), "wifi-left", &colors);
            long_value(
                ui,
                crate::i18n::tr("许可"),
                crate::i18n::tr(
                    "Bamboo Music Player Non-Commercial License 1.0 · 非商业使用 · 需署名并标注原作者 wifi-left",
                ),
                &colors,
            );
            widgets::key_value(
                ui,
                app.t("settings.portable"),
                if app.paths.portable {
                    crate::i18n::tr("是")
                } else {
                    crate::i18n::tr("否")
                },
                &colors,
            );
            long_value(
                ui,
                crate::i18n::tr("数据目录"),
                &app.paths.root.display().to_string(),
                &colors,
            );
            if let Some(mem) = app.memory_info {
                long_value(
                    ui,
                    crate::i18n::tr("内存占用"),
                    &crate::trf!(
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
                if ui.button(crate::i18n::tr("释放内存")).clicked() {
                    crate::platform::memory::trim_working_set();
                    app.memory_info = crate::platform::memory::process_memory();
                    app.toast(crate::i18n::tr("已尝试释放工作集内存"), ToastKind::Success);
                }
                if ui
                    .add(
                        egui::Button::new(
                            RichText::new(crate::i18n::tr("重置设置")).color(egui::Color32::WHITE),
                        )
                        .fill(colors.danger),
                    )
                    .clicked()
                {
                    app.reset_confirm_open = true;
                }
            });
            ui.add_space(6.0);
            ui.label(
                RichText::new(crate::i18n::tr(
                    "Bamboo Music Player · 原生 Rimor 播放器 · WGPU/DX12 · Copyright © 2026 wifi-left",
                ))
                .size(10.0)
                .color(colors.text_faint),
            );
        });
    }

    if app.reset_confirm_open {
        let mut open = app.reset_confirm_open;
        let mut confirm = false;
        let mut cancel = false;
        egui::Window::new(crate::i18n::tr("重置设置"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                ui.label(crate::i18n::tr("确定要重置所有设置吗？此操作不可撤销。"));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(crate::i18n::tr("取消")).clicked() {
                        cancel = true;
                    }
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new(crate::i18n::tr("重置")).color(egui::Color32::WHITE),
                            )
                            .fill(colors.danger),
                        )
                        .clicked()
                    {
                        confirm = true;
                    }
                });
            });
        if confirm {
            app.reset_settings();
            app.reset_confirm_open = false;
        } else if cancel {
            app.reset_confirm_open = false;
        } else {
            app.reset_confirm_open = open;
        }
    }

    if appearance_changed {
        app.settings_dirty = true;
        app.apply_appearance();
    }
    if settings_changed {
        app.settings_dirty = true;
        app.apply_dsp();
        app.arm_next();
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
        ui.add_space(10.0);
        add(ui)
    })
}

fn row(
    ui: &mut Ui,
    label: &str,
    colors: &crate::app::theme::ThemeColors,
    add: impl FnOnce(&mut Ui),
) {
    // [text] [controls]: a fixed label column and a vertically stacked control
    // column. The control column has a bounded width, so long text wraps
    // inside its own row and the row grows instead of overlapping the next.
    // Available width must be read before entering the horizontal layout:
    // inside a non-wrapping horizontal ui it can be infinite, which made
    // `horizontal_wrapped` grow the card instead of wrapping.
    let total_width = ui.available_width();
    let label_width = 150.0_f32.min((total_width * 0.35).max(72.0));
    let control_width = (total_width - label_width - ui.spacing().item_spacing.x).max(120.0);
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            egui::vec2(label_width, 0.0),
            egui::Layout::left_to_right(egui::Align::TOP),
            |ui| {
                ui.set_min_width(label_width);
                ui.set_max_width(label_width);
                ui.add(
                    egui::Label::new(RichText::new(label).size(12.0).color(colors.text_weak))
                        .wrap(),
                );
            },
        );
        ui.allocate_ui_with_layout(
            egui::vec2(control_width, 0.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.set_width(control_width);
                ui.spacing_mut().slider_width = 150.0;
                add(ui);
            },
        );
    });
    ui.add_space(6.0);
}

fn long_value(ui: &mut Ui, label: &str, value: &str, colors: &crate::app::theme::ThemeColors) {
    ui.label(RichText::new(label).size(10.5).color(colors.text_weak));
    ui.add(egui::Label::new(RichText::new(value).size(11.0).color(colors.text)).wrap());
    ui.add_space(4.0);
}

fn theme_label(mode: ThemeMode) -> &'static str {
    match mode {
        ThemeMode::System => crate::i18n::tr("跟随系统"),
        ThemeMode::Dark => crate::i18n::tr("深色"),
        ThemeMode::Light => crate::i18n::tr("浅色"),
    }
}

fn backend_label(backend: WgpuBackend) -> &'static str {
    match backend {
        WgpuBackend::Auto => crate::i18n::tr("自动（DX12 优先）"),
        WgpuBackend::Dx12 => "DX12",
        WgpuBackend::Vulkan => "Vulkan",
        WgpuBackend::Gl => crate::i18n::tr("OpenGL（低内存）"),
    }
}

fn background_label(mode: BackgroundMode) -> &'static str {
    match mode {
        BackgroundMode::StaticBlur => crate::i18n::tr("静态模糊封面"),
        BackgroundMode::GpuBlur => crate::i18n::tr("GPU 模糊封面"),
        BackgroundMode::Flow => crate::i18n::tr("流动渐变"),
        BackgroundMode::Solid => crate::i18n::tr("纯色/渐变"),
        BackgroundMode::Image => crate::i18n::tr("自定义图片"),
        BackgroundMode::Mica => crate::i18n::tr("Mica 云母"),
    }
}

fn desktop_bg_label(mode: DesktopBackgroundMode) -> &'static str {
    match mode {
        DesktopBackgroundMode::Transparent => crate::i18n::tr("透明"),
        DesktopBackgroundMode::Solid => crate::i18n::tr("纯色"),
        DesktopBackgroundMode::Gradient => crate::i18n::tr("渐变"),
        DesktopBackgroundMode::Image => crate::i18n::tr("图片"),
        DesktopBackgroundMode::BlurCover => crate::i18n::tr("模糊封面"),
    }
}

fn encoding_label(enc: LyricsEncoding) -> &'static str {
    match enc {
        LyricsEncoding::Auto => crate::i18n::tr("自动识别（BOM/UTF-8/GBK）"),
        LyricsEncoding::Utf8 => "UTF-8",
        LyricsEncoding::Gbk => "GBK",
        LyricsEncoding::Gb18030 => "GB18030",
        LyricsEncoding::Utf16Le => "UTF-16 LE",
        LyricsEncoding::Utf16Be => "UTF-16 BE",
        LyricsEncoding::Big5 => "Big5",
    }
}

fn fullscreen_mode_label(mode: crate::config::FullscreenMode) -> &'static str {
    match mode {
        crate::config::FullscreenMode::Borderless => crate::i18n::tr("无边框全屏（默认）"),
        crate::config::FullscreenMode::Maximized => crate::i18n::tr("最大化窗口"),
    }
}

fn close_label(b: CloseBehavior) -> &'static str {
    match b {
        CloseBehavior::Tray => crate::i18n::tr("最小化到托盘"),
        CloseBehavior::Exit => crate::i18n::tr("退出程序"),
        CloseBehavior::Ask => crate::i18n::tr("每次询问"),
    }
}

fn proxy_label(mode: ProxyMode) -> &'static str {
    match mode {
        ProxyMode::System => crate::i18n::tr("系统代理"),
        ProxyMode::None => crate::i18n::tr("不使用代理"),
        ProxyMode::Manual => crate::i18n::tr("手动"),
    }
}

fn replaygain_label(mode: ReplayGainMode) -> &'static str {
    match mode {
        ReplayGainMode::Off => crate::i18n::tr("关闭"),
        ReplayGainMode::Track => crate::i18n::tr("按曲目"),
        ReplayGainMode::Album => crate::i18n::tr("按专辑"),
    }
}

fn eq_label(preset: EqPreset) -> &'static str {
    match preset {
        EqPreset::Flat => crate::i18n::tr("平直"),
        EqPreset::Rock => crate::i18n::tr("摇滚"),
        EqPreset::Pop => crate::i18n::tr("流行"),
        EqPreset::Jazz => crate::i18n::tr("爵士"),
        EqPreset::Classical => crate::i18n::tr("古典"),
        EqPreset::Vocal => crate::i18n::tr("人声"),
        EqPreset::Bass => crate::i18n::tr("低音增强"),
        EqPreset::Custom => crate::i18n::tr("自定义"),
    }
}

fn source_kind_label(kind: SourceKind) -> &'static str {
    match kind {
        SourceKind::Local => crate::i18n::tr("本地"),
        SourceKind::Online => crate::i18n::tr("在线"),
    }
}
