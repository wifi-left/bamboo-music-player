//! Image loading and texture helpers.
//!
//! Cover decoding runs on a worker thread (see `util::cover`); this module
//! produces CPU-side RGBA images that the UI layer uploads.

use std::io::Cursor;
use std::sync::atomic::{AtomicU64, Ordering};

use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppRGBA, IWICImagingFactory,
    WICBitmapInterpolationModeFant, IWICPalette, WICBitmapDitherTypeNone,
    WICBitmapPaletteTypeCustom, WICDecodeMetadataCacheOnDemand, WICRect,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
};

/// Fallback artwork shown when a track has no cover or cover loading is off.
pub const DEFAULT_COVER_PNG: &[u8] = include_bytes!("../../../assets/default_cd.png");

/// Decode with WIC, center-cropping to a square and scaling during decode.
///
/// Large embedded covers (4000-6000 px JPEGs) spend most of their time in the
/// decoder; WIC's native codec plus its scaler path is considerably faster
/// than decoding the full image and resizing it in Rust. Returns `None` for
/// formats/codecs WIC cannot handle, so callers can fall back to `image`.
fn decode_wic_square(bytes: &[u8], max_side: u32) -> Option<RgbaImage> {
    if max_side == 0 || bytes.is_empty() {
        return None;
    }
    unsafe {
        // WIC needs COM on this thread. S_FALSE (already initialized) is fine.
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
        let stream = factory.CreateStream().ok()?;
        stream.InitializeFromMemory(bytes).ok()?;
        let decoder = factory
            .CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)
            .ok()?;
        let frame = decoder.GetFrame(0).ok()?;
        let mut width = 0u32;
        let mut height = 0u32;
        frame.GetSize(&mut width, &mut height).ok()?;
        if width == 0 || height == 0 {
            return None;
        }
        let side = width.min(height);
        let crop = WICRect {
            X: ((width - side) / 2) as i32,
            Y: ((height - side) / 2) as i32,
            Width: side as i32,
            Height: side as i32,
        };
        let clipper = factory.CreateBitmapClipper().ok()?;
        clipper.Initialize(&frame, &crop).ok()?;
        let target = side.min(max_side).max(1);
        let scaler = factory.CreateBitmapScaler().ok()?;
        scaler
            .Initialize(&clipper, target, target, WICBitmapInterpolationModeFant)
            .ok()?;
        let converter = factory.CreateFormatConverter().ok()?;
        converter
            .Initialize(
                &scaler,
                &GUID_WICPixelFormat32bppRGBA,
                WICBitmapDitherTypeNone,
                None::<&IWICPalette>,
                0.0,
                WICBitmapPaletteTypeCustom,
            )
            .ok()?;
        let stride = target * 4;
        let mut pixels = vec![0u8; (stride * target) as usize];
        converter
            .CopyPixels(std::ptr::null(), stride, &mut pixels)
            .ok()?;
        Some(RgbaImage {
            size: [target as usize, target as usize],
            pixels,
            id: next_image_id(),
        })
    }
}

/// Build the blurred background + small source thumbnail from a square cover.
fn derivative_images(
    cover: &RgbaImage,
    accent: [u8; 3],
    want_background: bool,
    blur_strength: u32,
) -> (Option<RgbaImage>, Option<RgbaImage>) {
    if !want_background {
        return (None, None);
    }
    let (w, h) = (cover.size[0], cover.size[1]);
    let Some(source) = image::RgbaImage::from_raw(w as u32, h as u32, cover.pixels.clone()) else {
        return (None, None);
    };
    let small = image::imageops::thumbnail(&source, 96, 96);
    let (bw, bh) = (small.width() as usize, small.height() as usize);
    let mut bg_data = box_blur(
        small.as_raw(),
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
    let background = RgbaImage::from_rgba_unmultiplied([bw, bh], &bg_data);
    let thumb = image::imageops::resize(
        &source,
        160,
        160,
        image::imageops::FilterType::Triangle,
    );
    let source_image = RgbaImage::from_rgba_unmultiplied(
        [thumb.width() as usize, thumb.height() as usize],
        thumb.as_raw(),
    );
    (Some(background), Some(source_image))
}

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

/// UI-independent RGBA8 image (top-left origin, unmultiplied alpha).
///
/// The UI layer uploads this to whatever surface it uses (Direct2D bitmap,
/// Windows bitmap, ...). The core never touches a UI texture type.
#[derive(Clone, Debug)]
pub struct RgbaImage {
    pub size: [usize; 2],
    pub pixels: Vec<u8>,
    /// Process-unique identity for UI-side caches. `Clone` keeps the id
    /// because the clone carries the same pixels; a freshly decoded image gets
    /// a new id, so caches keyed by it can never reuse a freed buffer's entry
    /// (the old pointer-based keys did, showing another song's cover).
    pub id: u64,
}

static NEXT_IMAGE_ID: AtomicU64 = AtomicU64::new(1);

fn next_image_id() -> u64 {
    NEXT_IMAGE_ID.fetch_add(1, Ordering::Relaxed)
}

impl RgbaImage {
    pub fn from_rgba_unmultiplied(size: [usize; 2], pixels: &[u8]) -> Self {
        Self {
            size,
            pixels: pixels.to_vec(),
            id: next_image_id(),
        }
    }
}

pub struct DecodedCover {
    pub cover: RgbaImage,
    pub background: Option<RgbaImage>,
    /// Small, unblurred thumbnail for the background pipeline.
    pub source: Option<RgbaImage>,
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
    // Fast path: native WIC decode + center crop + scale. Large JPEG album
    // art is the common slow case and this avoids materializing the original
    // 20-30 MB RGBA frame before thumbnailing it.
    if let Some(cover) = decode_wic_square(bytes, max_side) {
        let (background, source) =
            derivative_images(&cover, accent, want_background, blur_strength);
        return Some(DecodedCover {
            cover,
            background,
            source,
        });
    }

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
        Some(RgbaImage::from_rgba_unmultiplied([bw, bh], &bg_data))
    } else {
        None
    };

    let source = if want_background {
        // Square crop: the GPU shader samples in normalized coordinates, so a
        // square thumbnail keeps the blur circular instead of elliptical.
        let small = img.resize_to_fill(160, 160, image::imageops::FilterType::Triangle);
        let rgba = small.to_rgba8();
        Some(RgbaImage::from_rgba_unmultiplied(
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
    let cover = RgbaImage::from_rgba_unmultiplied(size, &pixels);

    Some(DecodedCover {
        cover,
        background,
        source,
    })
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

    #[test]
    fn large_jpeg_is_scaled_and_center_cropped() {
        // A non-square 3000x2000 cover: the old Rust thumbnail path would
        // produce 640x427; the WIC fast path crops to a 640x640 square first.
        let source = image::RgbaImage::from_fn(3000, 2000, |x, y| {
            image::Rgba([(x % 255) as u8, (y % 255) as u8, 90, 255])
        });
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgba8(source)
            .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Jpeg)
            .unwrap();
        let decoded = decode_cover_images(&bytes, [0, 0, 0], 640, false, 1)
            .expect("large JPEG must decode");
        assert_eq!(decoded.cover.size, [640, 640]);
    }

    #[test]
    #[ignore]
    fn large_cover_decode_benchmark() {
        let source = image::RgbaImage::from_fn(4000, 3000, |x, y| {
            image::Rgba([(x % 255) as u8, (y % 255) as u8, 90, 255])
        });
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgba8(source)
            .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Jpeg)
            .unwrap();
        let start = std::time::Instant::now();
        let wic = decode_wic_square(&bytes, 640).unwrap();
        let wic_time = start.elapsed();
        let start = std::time::Instant::now();
        let img = decode_limited(&bytes).unwrap().thumbnail(640, 640);
        let rust_time = start.elapsed();
        println!(
            "wic={:?} ({}x{}) rust={:?} ({}x{})",
            wic_time,
            wic.size[0],
            wic.size[1],
            rust_time,
            img.width(),
            img.height()
        );
    }
}
