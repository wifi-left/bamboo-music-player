//! Image loading and texture helpers.
//!
//! Cover art is decoded once and both the foreground cover and the blurred
//! background are derived from the same decoded image, keeping peak memory low.

use std::io::Cursor;

use egui::{Color32, ColorImage, TextureHandle, TextureOptions};

pub struct CoverTextures {
    pub cover: TextureHandle,
    pub background: Option<TextureHandle>,
}

fn decode_limited(bytes: &[u8]) -> Option<image::DynamicImage> {
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(192 * 1024 * 1024);
    reader.limits(limits);
    reader.decode().ok()
}

pub fn texture_from_bytes(
    ctx: &egui::Context,
    name: impl Into<String>,
    bytes: &[u8],
    max_side: u32,
) -> Option<TextureHandle> {
    let img = decode_limited(bytes)?;
    let img = if max_side > 0 && (img.width() > max_side || img.height() > max_side) {
        img.thumbnail(max_side, max_side)
    } else {
        img
    };
    let rgba = img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    let pixels = rgba.into_raw();
    let color = ColorImage::from_rgba_unmultiplied(size, &pixels);
    Some(ctx.load_texture(name, color, TextureOptions::LINEAR))
}

/// Decode cover art once and produce both textures. The full-size decoder
/// buffer is dropped as soon as the small JPEG/PNG-sized surfaces are created.
pub fn cover_textures(
    ctx: &egui::Context,
    bytes: &[u8],
    accent: [u8; 3],
    max_side: u32,
    blur_strength: u32,
) -> Option<CoverTextures> {
    let img = decode_limited(bytes)?;
    let hash = hash_bytes(bytes);

    // Background: small thumbnail + two box-blur passes.
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
    let bg_color = ColorImage::from_rgba_unmultiplied([bw, bh], &bg_data);
    let background = ctx.load_texture(
        format!("bg-{hash}"),
        bg_color,
        TextureOptions::LINEAR,
    );

    // Foreground cover, capped to the maximum display size.
    let cover_img = if max_side > 0 && (img.width() > max_side || img.height() > max_side) {
        img.thumbnail(max_side, max_side)
    } else {
        img
    };
    let rgba = cover_img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    let pixels = rgba.into_raw();
    let color = ColorImage::from_rgba_unmultiplied(size, &pixels);
    let cover = ctx.load_texture(
        format!("cover-{hash}-{max_side}"),
        color,
        TextureOptions::LINEAR,
    );

    Some(CoverTextures {
        cover,
        background: Some(background),
    })
}

pub fn placeholder_texture(
    ctx: &egui::Context,
    name: impl Into<String>,
    size: usize,
    accent: [u8; 3],
) -> TextureHandle {
    let mut pixels = vec![0u8; size * size * 4];
    let (ar, ag, ab) = (accent[0] as f32, accent[1] as f32, accent[2] as f32);
    for y in 0..size {
        for x in 0..size {
            let fx = x as f32 / size as f32;
            let fy = y as f32 / size as f32;
            let t = (fx * 0.55 + fy * 0.45).clamp(0.0, 1.0);
            let vignette = 1.0 - (((fx - 0.5).powi(2) + (fy - 0.5).powi(2)) * 0.9).min(0.6);
            let r = (18.0 + (ar - 18.0) * t) * vignette;
            let g = (22.0 + (ag - 22.0) * t) * vignette;
            let b = (32.0 + (ab - 32.0) * t) * vignette;
            let idx = (y * size + x) * 4;
            pixels[idx] = r.clamp(0.0, 255.0) as u8;
            pixels[idx + 1] = g.clamp(0.0, 255.0) as u8;
            pixels[idx + 2] = b.clamp(0.0, 255.0) as u8;
            pixels[idx + 3] = 255;
        }
    }
    let color = ColorImage::from_rgba_unmultiplied([size, size], &pixels);
    ctx.load_texture(name, color, TextureOptions::LINEAR)
}

/// A low-resolution, box-blurred version suitable as a background layer.
pub fn blurred_background(
    ctx: &egui::Context,
    bytes: &[u8],
    accent: [u8; 3],
    strength: u32,
) -> Option<TextureHandle> {
    let img = decode_limited(bytes)?;
    let small = img.thumbnail(96, 96).to_rgba8();
    let (w, h) = (small.width() as usize, small.height() as usize);
    let mut data = box_blur(&small.into_raw(), w, h, strength.clamp(1, 8) as usize);
    for px in data.chunks_exact_mut(4) {
        for i in 0..3 {
            let accent_v = accent[i] as f32;
            px[i] = ((px[i] as f32 * 0.72) + accent_v * 0.10).clamp(0.0, 255.0) as u8;
        }
        px[3] = 255;
    }
    let color = ColorImage::from_rgba_unmultiplied([w, h], &data);
    Some(ctx.load_texture(
        format!("bg-{}", hash_bytes(bytes)),
        color,
        TextureOptions::LINEAR,
    ))
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

pub fn hash_bytes(bytes: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

pub fn accent_color(accent: [u8; 3]) -> Color32 {
    Color32::from_rgb(accent[0], accent[1], accent[2])
}
