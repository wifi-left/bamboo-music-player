#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! Bamboo Music Player
//!
//! Copyright (c) 2026 wifi-left. Licensed under the Bamboo Music Player
//! Non-Commercial License 1.0 (see LICENSE): non-commercial use only, with
//! attribution to the original author required.

mod api;
mod app;
mod audio;
mod config;
mod desktop_overlay;
mod download;
mod i18n;
mod library;
mod lyrics;
mod mini_overlay;
mod model;
mod overlay_protocol;
mod platform;
mod playlist;
mod radio;
mod util;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::OnceLock;

use config::Paths;
use tracing_subscriber::util::SubscriberInitExt;

fn main() -> eframe::Result {
    // Give the media flyout / taskbar a stable identity instead of "未知应用".
    #[cfg(windows)]
    unsafe {
        use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
        use windows::core::PCWSTR;
        let aumid: Vec<u16> = platform::APP_USER_MODEL_ID
            .encode_utf16()
            .chain([0])
            .collect();
        let _ = SetCurrentProcessExplicitAppUserModelID(PCWSTR(aumid.as_ptr()));
    }

    let paths = Paths::discover();
    // Settings must be read before the single-instance check: the user can
    // opt into running multiple windows from the window settings.
    let (settings, session) = app::default_settings_and_session(&paths);
    init_logging(&paths, settings.window.allow_multiple_instances);
    tracing::info!(
        "Bamboo Music Player {} starting (portable={}, data={}, multi_instance={})",
        env!("CARGO_PKG_VERSION"),
        paths.portable,
        paths.root.display(),
        settings.window.allow_multiple_instances
    );

    let raw_args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    if !settings.window.allow_multiple_instances && platform::single_instance::already_running() {
        // With no file/command, ask the running instance to show itself. The
        // pipe request also brings the window forward when it opens a file.
        let mut forwarded_args = raw_args.clone();
        if forwarded_args.is_empty() {
            forwarded_args.push(std::ffi::OsString::from("--show"));
        }
        let forwarded = platform::ipc::send_to_existing(&forwarded_args);
        tracing::info!("another instance is running; forwarded_args={forwarded}");
        if !forwarded {
            platform::window::focus_existing_instance("Bamboo Music Player");
        }
        return Ok(());
    }
    let startup_args: Vec<PathBuf> = std::env::args_os()
        .skip(1)
        .map(PathBuf::from)
        .filter(|p| p.exists())
        .collect();
    // Commands (--play-pause, --next, …) also apply when this process is the
    // one that starts the player, e.g. from a jump-list task.
    let startup_commands: Vec<String> = raw_args
        .iter()
        .filter_map(|arg| arg.to_str())
        .filter(|arg| arg.starts_with("--"))
        .map(str::to_string)
        .collect();
    tracing::info!("startup args: {:?}", startup_args);
    let accent = settings.theme.accent;

    // Restore a sane inner size. Older builds persisted the outer rect, which
    // could grow every launch and overlap the taskbar; clamp it now.
    let default_size = (1024.0f32, 680.0f32);
    let (mut width, mut height) = match session.window_size {
        Some((w, h))
            if w.is_finite()
                && h.is_finite()
                && w >= 900.0
                && h >= 560.0
                && w <= 1280.0
                && h <= 820.0 =>
        {
            (w, h)
        }
        _ => default_size,
    };

    let mut position = session.window_pos;
    if let Some((wx, wy, ww, wh)) = platform::window::work_area_logical() {
        let max_w = (ww - 40.0).max(900.0);
        let max_h = (wh - 80.0).max(560.0);
        width = width.min(max_w);
        height = height.min(max_h);
        if let Some((mut x, mut y)) = position {
            x = x.clamp(wx + 8.0, (wx + ww - width - 8.0).max(wx + 8.0));
            y = y.clamp(wy + 8.0, (wy + wh - height - 8.0).max(wy + 8.0));
            position = Some((x, y));
        }
    }

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Bamboo Music Player")
        .with_app_id("bamboo-player")
        .with_inner_size([width, height])
        .with_min_inner_size([900.0, 560.0])
        // Only Mica needs a transparent surface. Requesting it for every
        // background made DWM composite a full-size transparent window even
        // when the app painted an opaque background, which was a large fixed
        // GPU cost on laptops.
        .with_transparent(settings.theme.background == config::BackgroundMode::Mica)
        .with_icon(Arc::new(app::app_icon(accent)));
    let has_position = position.is_some();
    if let Some((x, y)) = position {
        viewport = viewport.with_position([x, y]);
    }

    let mut wgpu_options = eframe::WgpuConfiguration::default();
    wgpu_options.surface = eframe::SurfaceConfig::LOW_LATENCY;
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(mut setup) = wgpu_options.wgpu_setup {
        setup.instance_descriptor.backends =
            eframe::wgpu::Backends::from_env().unwrap_or_else(|| match settings.theme.backend {
                config::WgpuBackend::Auto => {
                    eframe::wgpu::Backends::DX12
                        | eframe::wgpu::Backends::VULKAN
                        | eframe::wgpu::Backends::GL
                }
                config::WgpuBackend::Dx12 => eframe::wgpu::Backends::DX12,
                config::WgpuBackend::Vulkan => eframe::wgpu::Backends::VULKAN,
                config::WgpuBackend::Gl => eframe::wgpu::Backends::GL,
            });
        setup.power_preference = match settings.theme.gpu_power {
            config::GpuPowerPreference::Auto => eframe::wgpu::PowerPreference::None,
            config::GpuPowerPreference::LowPower => eframe::wgpu::PowerPreference::LowPower,
            config::GpuPowerPreference::HighPerformance => {
                eframe::wgpu::PowerPreference::HighPerformance
            }
        };
        tracing::info!(
            "wgpu preference: backend={:?} power={:?}",
            settings.theme.backend,
            setup.power_preference
        );
        setup.device_descriptor = Arc::new(|adapter| {
            let base_limits = if adapter.get_info().backend == eframe::wgpu::Backend::Gl {
                eframe::wgpu::Limits::downlevel_webgl2_defaults()
            } else {
                eframe::wgpu::Limits::default()
            };
            eframe::wgpu::DeviceDescriptor {
                label: Some("Bamboo UI device"),
                required_limits: eframe::wgpu::Limits {
                    max_texture_dimension_2d: 8192,
                    ..base_limits
                },
                memory_hints: eframe::wgpu::MemoryHints::MemoryUsage,
                ..Default::default()
            }
        });
        wgpu_options.wgpu_setup = eframe::egui_wgpu::WgpuSetup::CreateNew(setup);
    }

    let native_options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Wgpu,
        wgpu_options,
        persist_window: false,
        centered: !has_position,
        ..Default::default()
    };

    eframe::run_native(
        "Bamboo Music Player",
        native_options,
        Box::new(move |cc| {
            Ok(Box::new(app::App::new(
                cc,
                settings,
                session,
                paths,
                startup_args,
                startup_commands,
            )))
        }),
    )
}

fn init_logging(paths: &Paths, per_process: bool) {
    static GUARD: OnceLock<tracing_appender::non_blocking::WorkerGuard> = OnceLock::new();
    // Multiple instances cannot share one rolling file safely; give each
    // process its own prefix when the user enables that mode.
    let prefix = if per_process {
        format!("bamboo.{}.log", std::process::id())
    } else {
        "bamboo.log".to_string()
    };
    let file_appender = tracing_appender::rolling::daily(&paths.logs, prefix);
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
    let _ = GUARD.set(guard);

    let level = if cfg!(debug_assertions) {
        tracing::Level::DEBUG
    } else {
        tracing::Level::INFO
    };
    let filter = tracing_subscriber::EnvFilter::new(format!(
        "warn,bamboo_player={}",
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "info"
        }
    ));
    tracing_subscriber::fmt()
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_target(true)
        .with_env_filter(filter)
        .finish()
        .init();
    let _ = level;

    // eframe/egui-wgpu log through the `log` crate. Forward warnings/errors and
    // notice when the swapchain rejects transparency, which is the signal that
    // the DWM Mica backdrop cannot be shown.
    let _ = log::set_logger(&LOG_BRIDGE);
    log::set_max_level(log::LevelFilter::Info);

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!("panic: {info}");
        default_hook(info);
    }));
}

struct LogBridge;

static LOG_BRIDGE: LogBridge = LogBridge;

impl log::Log for LogBridge {
    fn enabled(&self, _metadata: &log::Metadata<'_>) -> bool {
        true
    }

    fn log(&self, record: &log::Record<'_>) {
        let message = record.args().to_string();
        if record.level() > log::Level::Warn {
            return;
        }
        if message.contains("Transparent window was requested") {
            platform::window::note_transparency_unsupported();
        }
        match record.level() {
            log::Level::Error => tracing::error!("[{}] {message}", record.target()),
            log::Level::Warn => tracing::warn!("[{}] {message}", record.target()),
            _ => {}
        }
    }

    fn flush(&self) {}
}
