//! Single-instance guard using a named Win32 mutex.

use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
use windows::Win32::System::Threading::CreateMutexW;
use windows::core::PCWSTR;

/// Returns true when another instance already owns the mutex.
///
/// The mutex handle is intentionally leaked for the lifetime of the process:
/// closing it would release the single-instance lock. `BAMBOO_INSTANCE` can
/// select a separate lock name, which is useful for portable test profiles.
pub fn already_running() -> bool {
    let name = instance_name();
    let mut wide: Vec<u16> = format!("Local\\{name}").encode_utf16().collect();
    wide.push(0);
    unsafe {
        match CreateMutexW(None, false, PCWSTR(wide.as_ptr())) {
            Ok(_handle) => GetLastError() == ERROR_ALREADY_EXISTS,
            Err(err) => {
                tracing::warn!("CreateMutexW failed: {err}");
                false
            }
        }
    }
}

/// Mutex/pipe name for this instance. `BAMBOO_INSTANCE` selects independent
/// profiles for testing or side-by-side portable installs.
pub fn instance_name() -> String {
    std::env::var("BAMBOO_INSTANCE")
        .ok()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "BambooMusicPlayer.SingleInstance".to_string())
}
