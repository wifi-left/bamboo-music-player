#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod audio;
mod config;
mod i18n;
mod library;
mod lyrics;
mod model;
mod platform;
mod playlist;
mod util;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::OnceLock;

use config::Paths;
use tracing_subscriber::util::SubscriberInitExt;

fn main() -> eframe::Result {
    let paths = Paths::discover();
    init_logging(&paths);
    tracing::info!(
        "Bamboo Music Player {} starting (portable={}, data={})",
        env!("CARGO_PKG_VERSION"),
        paths.portable,
        paths.root.display()
    );

    if platform::single_instance::already_running() {
        tracing::info!("another instance is running; focusing it");
        platform::window::focus_existing_instance("Bamboo Music Player");
        return Ok(());
    }

    let (settings, session) = app::default_settings_and_session(&paths);
    let startup_args: Vec<PathBuf> = std::env::args_os()
        .skip(1)
        .map(PathBuf::from)
        .filter(|p| p.exists())
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
        setup.power_preference = eframe::wgpu::PowerPreference::LowPower;
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
            )))
        }),
    )
}

fn init_logging(paths: &Paths) {
    static GUARD: OnceLock<tracing_appender::non_blocking::WorkerGuard> = OnceLock::new();
    let file_appender = tracing_appender::rolling::daily(&paths.logs, "bamboo.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
    let _ = GUARD.set(guard);

    let level = if cfg!(debug_assertions) {
        tracing::Level::DEBUG
    } else {
        tracing::Level::INFO
    };
    let filter = tracing_subscriber::EnvFilter::new(format!(
        "warn,bamboo_player={}",
        if cfg!(debug_assertions) { "debug" } else { "info" }
    ));
    tracing_subscriber::fmt()
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_target(true)
        .with_env_filter(filter)
        .finish()
        .init();
    let _ = level;

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!("panic: {info}");
        default_hook(info);
    }));
}
