//! Windows platform integration: window styling, capture protection, tray,
//! and single-instance handoff.

pub mod ipc;
pub mod jump_list;
pub mod memory;
pub mod notify;
pub mod native_mini;
pub mod native_overlay;
pub mod registry;
pub mod single_instance;
pub mod smtc;
pub mod tray;
pub mod window;

/// AppUserModelID shared by the shell, jump list and taskbar.
pub const APP_USER_MODEL_ID: &str = "Bamboo.Music.Player";
