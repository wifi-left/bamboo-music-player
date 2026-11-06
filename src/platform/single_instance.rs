//! Single-instance guard using a named Win32 mutex.

use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
use windows::Win32::System::Threading::CreateMutexW;
use windows::core::w;

/// Returns true when another instance already owns the mutex.
///
/// The mutex handle is intentionally leaked for the lifetime of the process:
/// closing it would release the single-instance lock.
pub fn already_running() -> bool {
    unsafe {
        match CreateMutexW(None, false, w!("Local\\BambooMusicPlayer.SingleInstance")) {
            Ok(_handle) => GetLastError() == ERROR_ALREADY_EXISTS,
            Err(err) => {
                tracing::warn!("CreateMutexW failed: {err}");
                false
            }
        }
    }
}
