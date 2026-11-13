//! Windows platform integration: native overlay threads, single-instance
//! handoff, taskbar/notification helpers and process memory reporting.
//!
//! `window` from the archived egui build is intentionally not migrated: the
//! native shell owns its window. The tray is migrated without `egui::Context`
//! and wakes the native shell through a host-supplied callback.

pub mod gpu;
pub mod ipc;
pub mod jump_list;
pub mod memory;
pub mod native_mini;
pub mod native_overlay;
pub mod notify;
pub mod overlay_inbox;
pub mod registry;
pub mod single_instance;
pub mod smtc;
pub mod text_inject;
pub mod timer_resolution;
pub mod tray;

/// AppUserModelID shared by the shell, jump list and taskbar.
pub const APP_USER_MODEL_ID: &str = "Bamboo.Music.Player";
