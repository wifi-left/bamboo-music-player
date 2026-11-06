//! Native window helpers (DWM dark title bar, rounded corners, capture
//! protection, foreground handoff).

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Dwm::{
    DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    DwmSetWindowAttribute,
};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, SW_RESTORE, SetForegroundWindow, SetWindowDisplayAffinity, ShowWindow,
    WDA_EXCLUDEFROMCAPTURE, WDA_NONE,
};

pub fn hwnd_from_creation(cc: &eframe::CreationContext<'_>) -> Option<isize> {
    let handle = cc.window_handle().ok()?;
    match handle.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
        _ => None,
    }
}

pub fn hwnd(raw: isize) -> HWND {
    HWND(raw as *mut core::ffi::c_void)
}

pub fn apply_style(raw: isize, dark: bool, radius: u8, capture_protection: bool) {
    let hwnd = hwnd(raw);
    unsafe {
        let dark_value: i32 = i32::from(dark);
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark_value as *const i32 as *const core::ffi::c_void,
            std::mem::size_of::<i32>() as u32,
        );
        let corner = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &corner as *const _ as *const core::ffi::c_void,
            std::mem::size_of_val(&corner) as u32,
        );
        let _ = radius;
    }
    set_capture_protection(raw, capture_protection);
}

pub fn set_capture_protection(raw: isize, enabled: bool) {
    let hwnd = hwnd(raw);
    let affinity = if enabled {
        WDA_EXCLUDEFROMCAPTURE
    } else {
        WDA_NONE
    };
    unsafe {
        if let Err(err) = SetWindowDisplayAffinity(hwnd, affinity) {
            tracing::warn!("SetWindowDisplayAffinity failed: {err}");
        }
    }
}

/// Bring an existing instance to the foreground (used by single-instance).
pub fn focus_existing_instance(title: &str) -> bool {
    let wide: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        match FindWindowW(None, windows::core::PCWSTR(wide.as_ptr())) {
            Ok(hwnd) if !hwnd.is_invalid() => {
                let _ = ShowWindow(hwnd, SW_RESTORE);
                let _ = SetForegroundWindow(hwnd);
                true
            }
            _ => false,
        }
    }
}

#[allow(dead_code)]
pub fn set_click_through(raw: isize, enabled: bool) {
    use windows::Win32::UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GetWindowLongPtrW, SetWindowLongPtrW, WS_EX_LAYERED, WS_EX_TRANSPARENT,
    };
    let hwnd = hwnd(raw);
    unsafe {
        let current = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let layered = WS_EX_LAYERED.0 as isize;
        let transparent = WS_EX_TRANSPARENT.0 as isize;
        let next = if enabled {
            current | layered | transparent
        } else {
            (current & !transparent) | layered
        };
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, next);
    }
}
