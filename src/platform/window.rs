//! Native window helpers (DWM dark title bar, rounded corners, capture
//! protection, foreground handoff).

use std::sync::atomic::Ordering;

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWMSBT_MAINWINDOW, DWMSBT_NONE, DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR,
    DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_TEXT_COLOR,
    DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND,
    DWMWCP_ROUND, DwmExtendFrameIntoClientArea, DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO,
    MONITORINFOEXW, MonitorFromWindow,
};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, GWL_EXSTYLE, GetForegroundWindow, GetWindowLongPtrW, GetWindowRect,
    GetWindowThreadProcessId, IsZoomed, PostMessageW, SPI_GETWORKAREA, SW_HIDE, SW_RESTORE,
    SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SetForegroundWindow, SetWindowDisplayAffinity,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, SystemParametersInfoW, WDA_EXCLUDEFROMCAPTURE,
    WDA_NONE, WM_CLOSE, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
};
use windows::core::BOOL;

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

/// Hide the native window immediately (used by the fast quit path).
pub fn hide_window(raw: isize) {
    unsafe {
        let _ = ShowWindow(hwnd(raw), SW_HIDE);
    }
}

/// Ensure a borderless tool window either owns a taskbar button or stays out
/// of the taskbar/Alt-Tab list. winit normally applies this at creation; doing
/// it again after the HWND is available also covers viewports that are patched
/// at runtime.
pub fn set_taskbar_visible(raw: isize, visible: bool) {
    if raw == 0 {
        return;
    }
    let hwnd = hwnd(raw);
    unsafe {
        let mut style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let tool = WS_EX_TOOLWINDOW.0 as isize;
        let app = WS_EX_APPWINDOW.0 as isize;
        if visible {
            style = (style & !tool) | app;
        } else {
            style = (style | tool) & !app;
        }
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style);
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
    }
}

/// Wake the main event loop with a normal close request. Used by the tray
/// "Quit" item so exiting does not wait for the hidden window to be focused.
pub fn post_close(raw: isize) {
    if raw == 0 {
        return;
    }
    unsafe {
        let _ = PostMessageW(Some(hwnd(raw)), WM_CLOSE, WPARAM(0), LPARAM(0));
    }
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
        // Win11 22000+: match the native caption to the app's dark content
        // instead of the system accent color. Older builds reject these.
        let (caption, text, border): (u32, u32, u32) = if dark {
            (0x0026_1A14, 0x00FF_FFFF, 0x0046_342A)
        } else {
            (0x00FB_F7F5, 0x0030_211A, 0x00EE_E3DC)
        };
        for (attr, value) in [
            (DWMWA_CAPTION_COLOR, caption),
            (DWMWA_TEXT_COLOR, text),
            (DWMWA_BORDER_COLOR, border),
        ] {
            let _ = DwmSetWindowAttribute(
                hwnd,
                attr,
                &value as *const u32 as *const core::ffi::c_void,
                std::mem::size_of::<u32>() as u32,
            );
        }
        let _ = radius;
    }
    set_capture_protection(raw, capture_protection);
}

/// Set when egui-wgpu reports that the swapchain cannot composite with alpha
/// (DX12 HWND swapchains are typically opaque). Mica needs a transparent
/// surface, so the app falls back to the painted cover background instead.
static TRANSPARENCY_UNSUPPORTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

pub fn note_transparency_unsupported() {
    TRANSPARENCY_UNSUPPORTED.store(true, Ordering::Relaxed);
}

pub fn transparency_supported() -> bool {
    !TRANSPARENCY_UNSUPPORTED.load(Ordering::Relaxed)
}

/// Enable or disable the Windows 11 Mica system backdrop.
///
/// Returns `true` when the backdrop was applied. Older Windows versions reject
/// `DWMWA_SYSTEMBACKDROP_TYPE`, so the caller can fall back to painting the
/// cover background itself. The window must have been created with
/// `transparent: true` for the backdrop to show through the client area.
pub fn set_mica(raw: isize, enabled: bool, dark: bool) -> bool {
    let hwnd = hwnd(raw);
    unsafe {
        let backdrop = if enabled {
            DWMSBT_MAINWINDOW
        } else {
            DWMSBT_NONE
        };
        let applied = DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE,
            &backdrop as *const _ as *const core::ffi::c_void,
            std::mem::size_of_val(&backdrop) as u32,
        )
        .is_ok();

        // Extend the frame over the whole client area so DWM composites the
        // backdrop behind it. A zero margin restores the normal frame.
        let margins = if enabled && applied {
            MARGINS {
                cxLeftWidth: -1,
                cxRightWidth: -1,
                cyTopHeight: -1,
                cyBottomHeight: -1,
            }
        } else {
            MARGINS::default()
        };
        let _ = DwmExtendFrameIntoClientArea(hwnd, &margins);

        if enabled && applied {
            // Let the system draw the caption over the backdrop; the opaque
            // caption colour used by the normal style would cover it.
            let none: u32 = 0xFFFF_FFFE;
            for attr in [DWMWA_CAPTION_COLOR, DWMWA_BORDER_COLOR, DWMWA_TEXT_COLOR] {
                let _ = DwmSetWindowAttribute(
                    hwnd,
                    attr,
                    &none as *const u32 as *const core::ffi::c_void,
                    std::mem::size_of::<u32>() as u32,
                );
            }
        }
        if enabled {
            tracing::info!("mica backdrop applied={applied} dark={dark}");
        }
        enabled && applied
    }
}

/// Style a borderless overlay: no DWM accent border and no DWM corner
/// rounding (the app paints its own rounded background).
pub fn apply_overlay_style(raw: isize, capture_protection: bool) {
    let hwnd = hwnd(raw);
    unsafe {
        let dark: i32 = 1;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark as *const i32 as *const core::ffi::c_void,
            std::mem::size_of::<i32>() as u32,
        );
        let corner = DWMWCP_DONOTROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &corner as *const _ as *const core::ffi::c_void,
            std::mem::size_of_val(&corner) as u32,
        );
        // DWMWA_COLOR_NONE removes the window border entirely.
        let none: u32 = 0xFFFF_FFFE;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR,
            &none as *const u32 as *const core::ffi::c_void,
            std::mem::size_of::<u32>() as u32,
        );
    }
    set_capture_protection(raw, capture_protection);
}

/// Find a top-level window by exact title (used for the desktop lyrics
/// viewport, whose HWND is not exposed through egui).
pub fn find_window_by_title(title: &str) -> Option<isize> {
    let wide: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        FindWindowW(None, windows::core::PCWSTR(wide.as_ptr()))
            .ok()
            .filter(|hwnd| !hwnd.is_invalid())
            .map(|hwnd| hwnd.0 as isize)
    }
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
/// Logical work area (excludes the taskbar) as `(x, y, width, height)`.
pub fn work_area_logical() -> Option<(f32, f32, f32, f32)> {
    let mut rect = windows::Win32::Foundation::RECT::default();
    unsafe {
        SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(&mut rect as *mut _ as *mut core::ffi::c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
        .ok()?;
        let dpi = GetDpiForSystem().max(96) as f32;
        let scale = dpi / 96.0;
        let w = (rect.right - rect.left) as f32 / scale;
        let h = (rect.bottom - rect.top) as f32 / scale;
        if w <= 100.0 || h <= 100.0 {
            None
        } else {
            Some((rect.left as f32 / scale, rect.top as f32 / scale, w, h))
        }
    }
}

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

/// True when the foreground window is a borderless/fullscreen window owned by
/// another process. Our own fullscreen lyrics window is excluded so the
/// desktop lyrics do not hide themselves while the user is on our UI.
pub fn foreground_is_fullscreen_excluding_self() -> bool {
    unsafe {
        let foreground = GetForegroundWindow();
        if foreground.is_invalid() {
            return false;
        }
        let mut pid = 0u32;
        let _ = GetWindowThreadProcessId(foreground, Some(&mut pid));
        if pid == GetCurrentProcessId() {
            return false;
        }
        // A maximized window's GetWindowRect extends past the monitor (the
        // invisible resize borders), which used to look exactly like a
        // fullscreen window and hid desktop lyrics whenever the user switched
        // to any maximized app.
        if IsZoomed(foreground).as_bool() {
            return false;
        }
        let mut rect = windows::Win32::Foundation::RECT::default();
        if GetWindowRect(foreground, &mut rect).is_err() {
            return false;
        }
        let monitor = MonitorFromWindow(foreground, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(monitor, &mut info).as_bool() {
            return false;
        }
        let target = info.rcMonitor;
        let width = (target.right - target.left) as i64;
        let height = (target.bottom - target.top) as i64;
        let window_width = (rect.right - rect.left) as i64;
        let window_height = (rect.bottom - rect.top) as i64;
        window_width >= width - 2
            && window_height >= height - 2
            && rect.left <= target.left + 1
            && rect.top <= target.top + 1
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

/// Physical display bounds, in virtual-screen coordinates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MonitorInfo {
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub primary: bool,
    /// Index in `EnumDisplayMonitors` order, which matches winit's
    /// `available_monitors()` order used by `ViewportBuilder::with_monitor`.
    pub native_index: usize,
}

/// Enumerate displays in a stable left-to-right, top-to-bottom order.
pub fn monitors() -> Vec<MonitorInfo> {
    unsafe extern "system" fn collect(
        monitor: HMONITOR,
        _hdc: HDC,
        _rect: *mut RECT,
        data: LPARAM,
    ) -> BOOL {
        let list = unsafe { &mut *(data.0 as *mut Vec<MonitorInfo>) };
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if unsafe { GetMonitorInfoW(monitor, &mut info.monitorInfo) }.as_bool() {
            let rect = info.monitorInfo.rcMonitor;
            let name_len = info
                .szDevice
                .iter()
                .position(|unit| *unit == 0)
                .unwrap_or(info.szDevice.len());
            list.push(MonitorInfo {
                name: String::from_utf16_lossy(&info.szDevice[..name_len]),
                x: rect.left,
                y: rect.top,
                width: rect.right - rect.left,
                height: rect.bottom - rect.top,
                primary: info.monitorInfo.dwFlags & 1 != 0,
                native_index: list.len(),
            });
        }
        BOOL(1)
    }

    let mut list: Vec<MonitorInfo> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(collect),
            LPARAM(&mut list as *mut _ as isize),
        );
    }
    list.sort_by_key(|monitor| (monitor.x, monitor.y));
    list
}

/// Map an index into the sorted [`monitors`] list to winit's enumeration order.
pub fn native_monitor_index(index: usize) -> Option<usize> {
    monitors().get(index).map(|monitor| monitor.native_index)
}

pub fn window_rect(raw: isize) -> Option<(i32, i32, i32, i32)> {
    let mut rect = RECT::default();
    unsafe {
        GetWindowRect(hwnd(raw), &mut rect).ok()?;
    }
    Some((rect.left, rect.top, rect.right, rect.bottom))
}

/// Move (not resize) the window so it sits on the requested display. The
/// fullscreen command afterwards then targets that monitor.
pub fn move_window_to_monitor(raw: isize, index: usize) -> bool {
    let Some(monitor) = monitors().get(index).cloned() else {
        return false;
    };
    let Some((left, top, right, bottom)) = window_rect(raw) else {
        return false;
    };
    let width = (right - left).max(320);
    let height = (bottom - top).max(240);
    unsafe {
        SetWindowPos(
            hwnd(raw),
            None,
            monitor.x,
            monitor.y,
            width,
            height,
            SWP_NOZORDER | SWP_NOACTIVATE,
        )
        .is_ok()
    }
}

pub fn restore_window_rect(raw: isize, rect: (i32, i32, i32, i32)) -> bool {
    let (left, top, right, bottom) = rect;
    let width = (right - left).max(200);
    let height = (bottom - top).max(150);
    unsafe {
        SetWindowPos(
            hwnd(raw),
            None,
            left,
            top,
            width,
            height,
            SWP_NOZORDER | SWP_NOACTIVATE,
        )
        .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enumerates_displays_in_a_stable_order() {
        let monitors = monitors();
        assert!(!monitors.is_empty(), "at least one display");
        assert!(
            monitors
                .iter()
                .all(|monitor| monitor.width > 0 && monitor.height > 0)
        );
        assert_eq!(
            monitors.iter().filter(|monitor| monitor.primary).count(),
            1,
            "exactly one primary display"
        );
        assert!(
            monitors
                .windows(2)
                .all(|pair| (pair[0].x, pair[0].y) <= (pair[1].x, pair[1].y)),
            "sorted left-to-right, top-to-bottom"
        );
    }
}
