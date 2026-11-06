#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod audio;
mod config;
mod i18n;
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
    let accent = settings.theme.accent;

    let (width, height) = session.window_size.unwrap_or((1180.0, 760.0));
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Bamboo Music Player")
        .with_app_id("bamboo-player")
        .with_inner_size([width.max(900.0), height.max(600.0)])
        .with_min_inner_size([900.0, 600.0])
        .with_icon(Arc::new(app::app_icon(accent)));
    if let Some((x, y)) = session.window_pos {
        viewport = viewport.with_position([x, y]);
    }

    let mut wgpu_options = eframe::WgpuConfiguration::default();
    wgpu_options.surface = eframe::SurfaceConfig::LOW_LATENCY;
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(mut setup) = wgpu_options.wgpu_setup {
        setup.instance_descriptor.backends = eframe::wgpu::Backends::DX12
            | eframe::wgpu::Backends::VULKAN
            | eframe::wgpu::Backends::GL;
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
        centered: session.window_pos.is_none(),
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
