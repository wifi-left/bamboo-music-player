//! Image loading and texture helpers.
//!
//! Cover decoding runs on a worker thread (see `util::cover`); this module
//! produces CPU-side `ColorImage`s and the placeholder texture.

use std::io::Cursor;

use egui::{ColorImage, TextureHandle, TextureOptions};

/// Fallback artwork shown when a track has no cover or cover loading is off.
pub const DEFAULT_COVER_PNG: &[u8] = include_bytes!("../../assets/default_cd.png");

fn decode_limited(bytes: &[u8]) -> Option<image::DynamicImage> {
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    // A single oversized cover must not balloon private memory; 96 MiB is
    // enough for a 6000x6000 RGB image while rejecting decompression bombs.
    limits.max_alloc = Some(96 * 1024 * 1024);
    reader.limits(limits);
    reader.decode().ok()
}

pub struct DecodedCover {
    pub cover: ColorImage,
    pub background: Option<ColorImage>,
    /// Small, unblurred thumbnail for the GPU background pipeline.
    pub source: Option<ColorImage>,
}

/// Decode cover bytes into CPU-side images. This is intentionally free of
/// egui context calls so it can run on a worker thread; the UI thread only
/// uploads the finished `ColorImage`s as textures.
pub fn decode_cover_images(
    bytes: &[u8],
    accent: [u8; 3],
    max_side: u32,
    want_background: bool,
    blur_strength: u32,
) -> Option<DecodedCover> {
    let img = decode_limited(bytes)?;

    let background = if want_background {
        let small = img.thumbnail(96, 96).to_rgba8();
        let (bw, bh) = (small.width() as usize, small.height() as usize);
        let mut bg_data = box_blur(
            &small.into_raw(),
            bw,
            bh,
            blur_strength.clamp(1, 8) as usize,
        );
        for px in bg_data.chunks_exact_mut(4) {
            for i in 0..3 {
                let accent_v = accent[i] as f32;
                px[i] = ((px[i] as f32 * 0.72) + accent_v * 0.10).clamp(0.0, 255.0) as u8;
            }
            px[3] = 255;
        }
        Some(ColorImage::from_rgba_unmultiplied([bw, bh], &bg_data))
    } else {
        None
    };

    let source = if want_background {
        // Square crop: the GPU shader samples in normalized coordinates, so a
        // square thumbnail keeps the blur circular instead of elliptical.
        let small = img.resize_to_fill(160, 160, image::imageops::FilterType::Triangle);
        let rgba = small.to_rgba8();
        Some(ColorImage::from_rgba_unmultiplied(
            [rgba.width() as usize, rgba.height() as usize],
            &rgba.into_raw(),
        ))
    } else {
        None
    };

    let cover_img = if max_side > 0 && (img.width() > max_side || img.height() > max_side) {
        img.thumbnail(max_side, max_side)
    } else {
        img
    };
    let rgba = cover_img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    let pixels = rgba.into_raw();
    let cover = ColorImage::from_rgba_unmultiplied(size, &pixels);

    Some(DecodedCover {
        cover,
        background,
        source,
    })
}

pub fn placeholder_texture(
    ctx: &egui::Context,
    name: impl Into<String>,
    size: usize,
    _accent: [u8; 3],
) -> TextureHandle {
    // Transparent canvas: only the bundled music-note artwork is drawn, so
    // the card/player-bar colour shows through instead of a gradient tile.
    let mut pixels = vec![0u8; size * size * 4];
    if let Some(icon) = decode_limited(DEFAULT_COVER_PNG) {
        let side = ((size as f32) * 0.64).round().max(24.0) as u32;
        let icon = icon.thumbnail(side, side).to_rgba8();
        let (iw, ih) = icon.dimensions();
        let ox = (size as i32 - iw as i32) / 2;
        let oy = (size as i32 - ih as i32) / 2;
        for iy in 0..ih as i32 {
            for ix in 0..iw as i32 {
                let dx = ox + ix;
                let dy = oy + iy;
                if dx < 0 || dy < 0 || dx >= size as i32 || dy >= size as i32 {
                    continue;
                }
                let src = icon.get_pixel(ix as u32, iy as u32).0;
                if src[3] == 0 {
                    continue;
                }
                let idx = ((dy as usize) * size + dx as usize) * 4;
                pixels[idx..idx + 4].copy_from_slice(&src);
            }
        }
    }

    let color = ColorImage::from_rgba_unmultiplied([size, size], &pixels);
    ctx.load_texture(name, color, TextureOptions::LINEAR)
}

fn box_blur(data: &[u8], w: usize, h: usize, radius: usize) -> Vec<u8> {
    let mut out = vec![0u8; data.len()];
    for y in 0..h {
        for x in 0..w {
            let mut acc = [0u32; 4];
            let mut count = 0u32;
            let x0 = x.saturating_sub(radius);
            let x1 = (x + radius).min(w - 1);
            let y0 = y.saturating_sub(radius);
            let y1 = (y + radius).min(h - 1);
            for yy in y0..=y1 {
                for xx in x0..=x1 {
                    let idx = (yy * w + xx) * 4;
                    for c in 0..4 {
                        acc[c] += data[idx + c] as u32;
                    }
                    count += 1;
                }
            }
            let idx = (y * w + x) * 4;
            for c in 0..4 {
                out[idx + c] = (acc[c] / count.max(1)) as u8;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_default_cover_decodes() {
        let image = decode_limited(DEFAULT_COVER_PNG).expect("default_cd.png must decode");
        assert_eq!((image.width(), image.height()), (320, 320));
    }
}
