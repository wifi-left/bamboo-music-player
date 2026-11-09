//! Render a shareable lyrics card with GDI+.
//!
//! GDI+ is used instead of a font rasterizer crate: it is available on every
//! supported Windows version, handles CJK text with anti-aliasing, and lets us
//! draw directly into a BGRA buffer that the `image` crate then encodes as PNG.

use std::path::Path;
use std::ptr::null_mut;

use windows::Win32::Graphics::GdiPlus::{
    GdipCreateBitmapFromScan0, GdipCreateFont, GdipCreateFontFamilyFromName, GdipCreatePen1,
    GdipCreateSolidFill, GdipCreateStringFormat, GdipDeleteBrush, GdipDeleteFont,
    GdipDeleteFontFamily, GdipDeleteGraphics, GdipDeletePen, GdipDeleteStringFormat,
    GdipDisposeImage, GdipDrawImageRectI, GdipDrawRectangleI, GdipDrawString, GdipFillEllipseI,
    GdipFillRectangleI, GdipGetImageGraphicsContext, GdipSetSmoothingMode,
    GdipSetStringFormatAlign, GdipSetStringFormatLineAlign, GdipSetStringFormatTrimming,
    GdipSetTextRenderingHint, GdiplusShutdown, GdiplusStartup, GdiplusStartupInput, GpBrush,
    GpFont, GpFontFamily, GpGraphics, GpImage, GpPen, GpStringFormat, RectF,
    SmoothingModeAntiAlias, StringAlignmentCenter, StringTrimmingEllipsisCharacter,
    TextRenderingHintAntiAliasGridFit, UnitPixel,
};
use windows::core::PCWSTR;

const WIDTH: i32 = 1200;
const HEIGHT: i32 = 1600;
/// `PixelFormat32bppARGB`; the windows-rs bindings do not export this constant.
const PIXEL_FORMAT_ARGB: i32 = 0x0026_200A;

/// One lyric entry on the card, optionally with its translation line.
#[derive(Clone, Debug)]
pub struct ShareLine {
    pub text: String,
    pub translation: Option<String>,
}

pub struct ShareCard<'a> {
    pub title: &'a str,
    pub subtitle: &'a str,
    pub lines: &'a [ShareLine],
    pub highlight: Option<usize>,
    pub cover: Option<&'a [u8]>,
    pub accent: [u8; 3],
}

/// Draw a 1200×1600 lyrics card and write it as PNG (or the format inferred
/// from the path extension).
pub fn render_lyrics_card(path: &Path, card: &ShareCard<'_>) -> Result<(), String> {
    let mut token = 0usize;
    let input = GdiplusStartupInput {
        GdiplusVersion: 1,
        ..Default::default()
    };
    let status = unsafe { GdiplusStartup(&mut token, &input, null_mut()) };
    if status.0 != 0 {
        return Err(crate::i18n::tr("GDI+ 初始化失败").to_string());
    }
    let result = unsafe { draw_card(path, card) };
    unsafe { GdiplusShutdown(token) };
    result
}

#[derive(Default)]
struct GdiCleanup {
    fonts: Vec<*mut GpFont>,
    brushes: Vec<*mut GpBrush>,
    format: *mut GpStringFormat,
    family: *mut GpFontFamily,
}

impl Drop for GdiCleanup {
    fn drop(&mut self) {
        unsafe {
            for font in self.fonts.drain(..) {
                let _ = GdipDeleteFont(font);
            }
            for brush in self.brushes.drain(..) {
                let _ = GdipDeleteBrush(brush);
            }
            if !self.format.is_null() {
                let _ = GdipDeleteStringFormat(self.format);
            }
            if !self.family.is_null() {
                let _ = GdipDeleteFontFamily(self.family);
            }
        }
    }
}

unsafe fn draw_card(path: &Path, card: &ShareCard<'_>) -> Result<(), String> {
    let mut canvas = vec![0u8; (WIDTH * HEIGHT * 4) as usize];
    fill_background(&mut canvas, card.accent);

    let mut bitmap = null_mut();
    if unsafe {
        GdipCreateBitmapFromScan0(
            WIDTH,
            HEIGHT,
            WIDTH * 4,
            PIXEL_FORMAT_ARGB,
            Some(canvas.as_ptr()),
            &mut bitmap,
        )
    }
    .0 != 0
    {
        return Err(crate::i18n::tr("无法创建分享图画布").to_string());
    }
    let mut graphics: *mut GpGraphics = null_mut();
    if unsafe { GdipGetImageGraphicsContext(bitmap as *mut GpImage, &mut graphics) }.0 != 0 {
        unsafe {
            let _ = GdipDisposeImage(bitmap as *mut GpImage);
        }
        return Err(crate::i18n::tr("无法创建分享图绘图上下文").to_string());
    }

    unsafe {
        let _ = GdipSetSmoothingMode(graphics, SmoothingModeAntiAlias);
        let _ = GdipSetTextRenderingHint(graphics, TextRenderingHintAntiAliasGridFit);
    }

    let mut cleanup = GdiCleanup::default();

    if let Some(cover) = card.cover {
        unsafe { draw_cover(graphics, cover) };
    } else {
        unsafe { draw_placeholder(graphics, card.accent) };
    }

    let family = unsafe { create_family() };
    if family.is_null() {
        unsafe {
            let _ = GdipDeleteGraphics(graphics);
            let _ = GdipDisposeImage(bitmap as *mut GpImage);
        }
        return Err(crate::i18n::tr("未找到可用字体").to_string());
    }
    cleanup.family = family;

    let mut format = null_mut();
    if unsafe { GdipCreateStringFormat(0, 0, &mut format) }.0 == 0 {
        unsafe {
            let _ = GdipSetStringFormatAlign(format, StringAlignmentCenter);
            let _ = GdipSetStringFormatLineAlign(format, StringAlignmentCenter);
            let _ = GdipSetStringFormatTrimming(format, StringTrimmingEllipsisCharacter);
        }
        cleanup.format = format;
    }

    let title_font = unsafe { create_font(family, 54.0, true) };
    let subtitle_font = unsafe { create_font(family, 30.0, false) };
    let lyric_font = unsafe { create_font(family, 36.0, false) };
    let lyric_highlight_font = unsafe { create_font(family, 40.0, true) };
    let translation_font = unsafe { create_font(family, 21.0, false) };
    let footer_font = unsafe { create_font(family, 22.0, false) };
    for font in [
        title_font,
        subtitle_font,
        lyric_font,
        lyric_highlight_font,
        translation_font,
        footer_font,
    ] {
        if !font.is_null() {
            cleanup.fonts.push(font);
        }
    }

    let title_brush = add_brush(&mut cleanup, 0xFFF2_F5F8);
    let subtitle_brush = add_brush(&mut cleanup, 0xFF9A_A6B2);
    let lyric_brush = add_brush(&mut cleanup, 0xFFD6_DE_E7);
    let accent_brush = add_brush(&mut cleanup, argb(card.accent, 255));
    let footer_brush = add_brush(&mut cleanup, 0xFF73_8190);

    unsafe {
        if !accent_brush.is_null() {
            let _ = GdipFillRectangleI(graphics, accent_brush, 540, 1042, 120, 5);
        }
    }

    unsafe {
        draw_text(
            graphics,
            card.title,
            title_font,
            RectF {
                X: 80.0,
                Y: 900.0,
                Width: 1040.0,
                Height: 88.0,
            },
            title_brush,
            format,
        );
        draw_text(
            graphics,
            card.subtitle,
            subtitle_font,
            RectF {
                X: 80.0,
                Y: 978.0,
                Width: 1040.0,
                Height: 56.0,
            },
            subtitle_brush,
            format,
        );
    }

    let count = card.lines.len().min(5);
    if count > 0 {
        let start_y = 1105.0 + (5 - count) as f32 * 41.0;
        for (index, line) in card.lines.iter().take(5).enumerate() {
            let highlighted = card.highlight == Some(index);
            let translation = line
                .translation
                .as_deref()
                .map(str::trim)
                .filter(|text| !text.is_empty());
            let top = start_y + index as f32 * 82.0;
            unsafe {
                draw_text(
                    graphics,
                    &line.text,
                    if highlighted {
                        lyric_highlight_font
                    } else {
                        lyric_font
                    },
                    RectF {
                        X: 100.0,
                        Y: top,
                        Width: 1000.0,
                        // Leave room for the translation below when present.
                        Height: if translation.is_some() { 58.0 } else { 70.0 },
                    },
                    if highlighted {
                        accent_brush
                    } else {
                        lyric_brush
                    },
                    format,
                );
                if let Some(translation) = translation {
                    draw_text(
                        graphics,
                        translation,
                        translation_font,
                        RectF {
                            X: 100.0,
                            Y: top + 56.0,
                            Width: 1000.0,
                            Height: 24.0,
                        },
                        subtitle_brush,
                        format,
                    );
                }
            }
        }
    }

    unsafe {
        draw_text(
            graphics,
            crate::i18n::tr("Bamboo Music Player · 歌词分享"),
            footer_font,
            RectF {
                X: 80.0,
                Y: 1532.0,
                Width: 1040.0,
                Height: 40.0,
            },
            footer_brush,
            format,
        );
        let _ = GdipDeleteGraphics(graphics);
        let _ = GdipDisposeImage(bitmap as *mut GpImage);
    }
    drop(cleanup);

    let mut rgba = Vec::with_capacity(canvas.len());
    for pixel in canvas.chunks_exact(4) {
        rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], 255]);
    }
    image::save_buffer(
        path,
        &rgba,
        WIDTH as u32,
        HEIGHT as u32,
        image::ExtendedColorType::Rgba8,
    )
    .map_err(|err| crate::trf!("保存分享图失败: {}", err))
}

unsafe fn draw_cover(graphics: *mut GpGraphics, bytes: &[u8]) {
    let Some(decoded) =
        crate::util::image_util::decode_cover_images(bytes, [0, 0, 0], 640, false, 1)
    else {
        return;
    };
    let image = decoded.cover;
    let width = image.size[0] as i32;
    let height = image.size[1] as i32;
    if width <= 0 || height <= 0 {
        return;
    }
    let mut bgra = Vec::with_capacity(image.pixels.len() * 4);
    for pixel in &image.pixels {
        bgra.extend_from_slice(&[pixel.b(), pixel.g(), pixel.r(), 255]);
    }
    let mut bitmap = null_mut();
    if unsafe {
        GdipCreateBitmapFromScan0(
            width,
            height,
            width * 4,
            PIXEL_FORMAT_ARGB,
            Some(bgra.as_ptr()),
            &mut bitmap,
        )
    }
    .0 != 0
    {
        return;
    }
    // Fit inside the 600 px frame without stretching non-square artwork.
    let frame = 600;
    let scale = frame as f32 / width.max(height) as f32;
    let draw_width = (width as f32 * scale).round().max(1.0) as i32;
    let draw_height = (height as f32 * scale).round().max(1.0) as i32;
    let x = (WIDTH - draw_width) / 2;
    let y = 180 + (frame - draw_height) / 2;
    unsafe {
        let _ = GdipDrawImageRectI(
            graphics,
            bitmap as *mut GpImage,
            x,
            y,
            draw_width,
            draw_height,
        );
        let mut pen: *mut GpPen = null_mut();
        if GdipCreatePen1(0x33FF_FFFF, 2.0, UnitPixel, &mut pen).0 == 0 {
            let _ = GdipDrawRectangleI(graphics, pen, x, y, draw_width, draw_height);
            let _ = GdipDeletePen(pen);
        }
        let _ = GdipDisposeImage(bitmap as *mut GpImage);
    }
}

/// A vinyl-style placeholder for tracks without embedded artwork.
unsafe fn draw_placeholder(graphics: *mut GpGraphics, accent: [u8; 3]) {
    let mut brush = null_mut();
    if unsafe { GdipCreateSolidFill(argb(accent, 34), &mut brush) }.0 == 0 {
        let brush = brush as *mut GpBrush;
        unsafe {
            let _ = GdipFillEllipseI(graphics, brush, 410, 290, 380, 380);
            let _ = GdipDeleteBrush(brush);
        }
    }
    let mut inner = null_mut();
    if unsafe { GdipCreateSolidFill(argb(accent, 70), &mut inner) }.0 == 0 {
        let inner = inner as *mut GpBrush;
        unsafe {
            let _ = GdipFillEllipseI(graphics, inner, 545, 425, 110, 110);
            let _ = GdipDeleteBrush(inner);
        }
    }
}

unsafe fn create_family() -> *mut GpFontFamily {
    for name in ["Microsoft YaHei UI", "Microsoft YaHei", "Segoe UI"] {
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        let mut family = null_mut();
        let status =
            unsafe { GdipCreateFontFamilyFromName(PCWSTR(wide.as_ptr()), null_mut(), &mut family) };
        if status.0 == 0 {
            return family;
        }
    }
    null_mut()
}

unsafe fn create_font(family: *mut GpFontFamily, size: f32, bold: bool) -> *mut GpFont {
    let mut font = null_mut();
    let style = if bold { 1 } else { 0 };
    let status = unsafe { GdipCreateFont(family, size, style, UnitPixel, &mut font) };
    if status.0 == 0 { font } else { null_mut() }
}

fn add_brush(cleanup: &mut GdiCleanup, color: u32) -> *mut GpBrush {
    let mut brush = null_mut();
    if unsafe { GdipCreateSolidFill(color, &mut brush) }.0 == 0 {
        let brush = brush as *mut GpBrush;
        cleanup.brushes.push(brush);
        brush
    } else {
        null_mut()
    }
}

unsafe fn draw_text(
    graphics: *mut GpGraphics,
    text: &str,
    font: *mut GpFont,
    rect: RectF,
    brush: *mut GpBrush,
    format: *mut GpStringFormat,
) {
    if font.is_null() || brush.is_null() || format.is_null() || text.trim().is_empty() {
        return;
    }
    let wide: Vec<u16> = text.encode_utf16().collect();
    unsafe {
        let _ = GdipDrawString(
            graphics,
            PCWSTR(wide.as_ptr()),
            wide.len() as i32,
            font,
            &rect,
            format,
            brush,
        );
    }
}

fn argb(color: [u8; 3], alpha: u8) -> u32 {
    ((alpha as u32) << 24) | ((color[0] as u32) << 16) | ((color[1] as u32) << 8) | color[2] as u32
}

fn fill_background(buffer: &mut [u8], accent: [u8; 3]) {
    let top = [15.0f32, 20.0, 28.0];
    let bottom = [27.0f32, 36.0, 48.0];
    for y in 0..HEIGHT {
        let t = y as f32 / (HEIGHT - 1) as f32;
        let base = [
            top[0] + (bottom[0] - top[0]) * t,
            top[1] + (bottom[1] - top[1]) * t,
            top[2] + (bottom[2] - top[2]) * t,
        ];
        for x in 0..WIDTH {
            let dx = (x as f32 - 250.0) / 760.0;
            let dy = (y as f32 - 200.0) / 760.0;
            let distance = (dx * dx + dy * dy).sqrt().min(1.0);
            let glow = (1.0 - distance).powi(2) * 0.22;
            let r = base[0] * (1.0 - glow) + accent[0] as f32 * glow;
            let g = base[1] * (1.0 - glow) + accent[1] as f32 * glow;
            let b = base[2] * (1.0 - glow) + accent[2] as f32 * glow;
            let offset = ((y * WIDTH + x) * 4) as usize;
            buffer[offset] = b.clamp(0.0, 255.0) as u8;
            buffer[offset + 1] = g.clamp(0.0, 255.0) as u8;
            buffer[offset + 2] = r.clamp(0.0, 255.0) as u8;
            buffer[offset + 3] = 255;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argb_and_background_are_valid() {
        assert_eq!(argb([0x12, 0x34, 0x56], 0xFF), 0xFF12_3456);
        let mut buffer = vec![0u8; (WIDTH * HEIGHT * 4) as usize];
        fill_background(&mut buffer, [56, 178, 118]);
        // Corners are opaque and the middle is not all black.
        assert_eq!(buffer[3], 255);
        let middle = ((HEIGHT / 2 * WIDTH + WIDTH / 2) * 4) as usize;
        assert!(buffer[middle] > 10 || buffer[middle + 1] > 10 || buffer[middle + 2] > 10);
    }

    /// Manual visual check: writes target/share-card-preview.png.
    #[test]
    #[ignore]
    fn render_preview() {
        let path = std::path::PathBuf::from("target/share-card-preview.png");
        let lines = vec![
            ShareLine {
                text: "远处传来熟悉的声音".to_string(),
                translation: Some("A familiar voice drifts from afar".to_string()),
            },
            ShareLine {
                text: "在风里轻轻回应".to_string(),
                translation: Some("answering softly in the wind".to_string()),
            },
            ShareLine {
                text: "我把思念写进歌声".to_string(),
                translation: Some("I write my longing into this song".to_string()),
            },
            ShareLine {
                text: "沿着星光的方向".to_string(),
                translation: None,
            },
            ShareLine {
                text: "直到再次遇见你".to_string(),
                translation: Some("until we meet again".to_string()),
            },
        ];
        let card = ShareCard {
            title: "夜空のメロディ",
            subtitle: "Bamboo Singer · Demo Album",
            lines: &lines,
            highlight: Some(2),
            cover: None,
            accent: [56, 178, 118],
        };
        render_lyrics_card(&path, &card).expect("render preview");
    }

    #[test]
    fn renders_a_png_card() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("card.png");
        let lines = vec![
            ShareLine {
                text: "第一句歌词".to_string(),
                translation: Some("First line".to_string()),
            },
            ShareLine {
                text: "第二句歌词".to_string(),
                translation: Some("Second line".to_string()),
            },
            ShareLine {
                text: "第三句歌词".to_string(),
                translation: None,
            },
            ShareLine {
                text: "第四句歌词".to_string(),
                translation: None,
            },
            ShareLine {
                text: "第五句歌词".to_string(),
                translation: Some("Fifth line".to_string()),
            },
        ];
        let card = ShareCard {
            title: "测试歌曲",
            subtitle: "测试歌手 · 测试专辑",
            lines: &lines,
            highlight: Some(2),
            cover: None,
            accent: [56, 178, 118],
        };
        render_lyrics_card(&path, &card).expect("render card");
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.starts_with(&[0x89, b'P', b'N', b'G']));
        let image = image::load_from_memory(&bytes).expect("decode card");
        assert_eq!(image.width(), WIDTH as u32);
        assert_eq!(image.height(), HEIGHT as u32);
    }
}
