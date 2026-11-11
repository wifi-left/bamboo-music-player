//! Embedded application icon (assets/favicon.ico) decoding.

use std::io::Cursor;

use image::{ImageBuffer, Rgba};

/// The user-provided application icon.
pub const APP_ICON_ICO: &[u8] = include_bytes!("../../../assets/favicon.ico");

/// Decode the largest frame of an ICO into RGBA.
pub fn decode_ico(bytes: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    let dir = ico::IconDir::read(Cursor::new(bytes)).ok()?;
    let entry = dir
        .entries()
        .iter()
        .max_by_key(|e| e.width().saturating_mul(e.height()))?;
    let image = entry.decode().ok()?;
    Some((image.rgba_data().to_vec(), image.width(), image.height()))
}

/// RGBA data resized to `size` (for the tray icon).
pub fn tray_icon_rgba(size: u32) -> Option<(Vec<u8>, u32, u32)> {
    let (rgba, w, h) = decode_ico(APP_ICON_ICO)?;
    if w == size && h == size {
        return Some((rgba, w, h));
    }
    let img: ImageBuffer<Rgba<u8>, Vec<u8>> = ImageBuffer::from_raw(w, h, rgba)?;
    let resized = image::imageops::resize(&img, size, size, image::imageops::FilterType::Lanczos3);
    Some((resized.into_raw(), size, size))
}
