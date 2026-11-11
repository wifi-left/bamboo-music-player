//! UI-independent business core (M0).
//!
//! Everything here is moved from the archived egui build (`ref/archived/
//! bamboo-egui`) with minimal adaptation: no `egui`/`eframe`/`winit` types are
//! allowed. The native UI reads snapshots and sends commands; the core owns
//! playback, queue, library, lyrics, downloads, settings and the native
//! overlay threads.

pub mod api;
pub mod app;
pub mod audio;
pub mod config;
pub mod desktop_overlay;
pub mod download;
pub mod i18n;
pub mod library;
pub mod lyrics;
pub mod mini_overlay;
pub mod model;
pub mod overlay_protocol;
pub mod platform;
pub mod playlist;
pub mod radio;
pub mod util;
