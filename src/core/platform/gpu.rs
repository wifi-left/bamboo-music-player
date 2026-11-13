//! Display adapter discovery for the settings debug page.

use std::sync::OnceLock;

use windows::Win32::Graphics::Gdi::{
    DISPLAY_DEVICEW, DISPLAY_DEVICE_ATTACHED_TO_DESKTOP, EnumDisplayDevicesW,
};

/// Names of the display adapters Windows reports (integrated and discrete).
/// Cached for the process lifetime: adapter hot-plug is rare and a debug row
/// must not enumerate devices on every 2 s repaint.
pub fn adapter_names() -> &'static [String] {
    static NAMES: OnceLock<Vec<String>> = OnceLock::new();
    NAMES.get_or_init(|| {
        let mut names: Vec<String> = Vec::new();
        for index in 0..16u32 {
            let mut device = DISPLAY_DEVICEW::default();
            device.cb = std::mem::size_of::<DISPLAY_DEVICEW>() as u32;
            let ok = unsafe { EnumDisplayDevicesW(None, index, &mut device, 0) };
            if !ok.as_bool() {
                break;
            }
            if device.StateFlags.0 & DISPLAY_DEVICE_ATTACHED_TO_DESKTOP.0 == 0 {
                continue;
            }
            let name = wide_to_string(&device.DeviceString);
            if name.is_empty() || names.iter().any(|existing| existing == &name) {
                continue;
            }
            names.push(name);
        }
        names
    })
}

fn wide_to_string(buffer: &[u16]) -> String {
    let len = buffer
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..len]).trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapters_are_enumerated_without_panicking() {
        for name in adapter_names() {
            assert!(!name.trim().is_empty());
        }
    }
}
