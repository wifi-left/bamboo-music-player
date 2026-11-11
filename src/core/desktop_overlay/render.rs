//! Direct2D/DirectWrite renderer for the layered lyrics window.
//!
//! The window surface is a 32-bit top-down DIB bound to a Direct2D DC render
//! target. After drawing, `UpdateLayeredWindow` publishes it with per-pixel
//! alpha. No GPU swapchain, no egui, no wgpu.

use std::ffi::c_void;
use std::mem::ManuallyDrop;

use image::RgbaImage;
use windows::Win32::Foundation::{COLORREF, HWND, POINT, RECT, SIZE};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_GRADIENT_STOP, D2D1_PIXEL_FORMAT, D2D_RECT_F,
    D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
    D2D1_BITMAP_PROPERTIES, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_EXTEND_MODE_CLAMP, D2D1_FACTORY_OPTIONS,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT, D2D1_GAMMA_2_2,
    D2D1_LAYER_OPTIONS_NONE, D2D1_LAYER_PARAMETERS, D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES,
    D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_HARDWARE,
    D2D1_RENDER_TARGET_TYPE_SOFTWARE, D2D1_RENDER_TARGET_USAGE_NONE,
    D2D1_ROUNDED_RECT, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE, D2D1CreateFactory, ID2D1Brush,
    ID2D1DCRenderTarget, ID2D1Factory, ID2D1RenderTarget, ID2D1SolidColorBrush,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_ITALIC,
    DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT_BOLD, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_LINE_METRICS,
    DWRITE_PARAGRAPH_ALIGNMENT_NEAR, DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING,
    DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_WORD_WRAPPING_NO_WRAP, DWRITE_WORD_WRAPPING_WRAP,
    DWriteCreateFactory,
    IDWriteFactory, IDWriteTextFormat, IDWriteTextLayout,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
    CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, HDC, HGDIOBJ,
    SelectObject,
};
use windows::Win32::UI::WindowsAndMessaging::{
    ULW_ALPHA, UPDATELAYEREDWINDOWINFO, UpdateLayeredWindow, UpdateLayeredWindowIndirect,
};
use windows::core::{Interface, PCWSTR};
use windows_numerics::{Matrix3x2, Vector2};

use crate::overlay_protocol::{OverlayLine, OverlayLyrics, OverlaySettings, OverlayState};

const DEFAULT_FAMILY: &str = "Microsoft YaHei UI";
const MAX_LAYOUT_HEIGHT: f32 = 10_000.0;

pub struct Renderer {
    factory: ID2D1Factory,
    dwrite: IDWriteFactory,
    surface: Option<Surface>,
    cover_version: u64,
    cover: Option<RgbaImage>,
    cover_bitmap: Option<windows::Win32::Graphics::Direct2D::ID2D1Bitmap>,
    background_path: Option<String>,
    background: Option<RgbaImage>,
    background_bitmap: Option<windows::Win32::Graphics::Direct2D::ID2D1Bitmap>,
    font_family: Option<String>,
    italic: bool,
    blur_radius: f32,
    /// The active row painted by the last full frame. Karaoke updates repaint
    /// only this rectangle and reuse the rest from the static copy.
    current_row: Option<CurrentRow>,
}

#[derive(Clone, Copy)]
struct CurrentRow {
    line: usize,
    /// Clip/publish rectangle, expanded a little for shadows and outlines.
    region: D2D_RECT_F,
    width: u32,
    height: u32,
}

struct Surface {
    rt: ID2D1DCRenderTarget,
    mem_dc: HDC,
    dib: HGDIOBJ,
    old: HGDIOBJ,
    width: u32,
    height: u32,
    dpi: f32,
    /// What the settings asked for; keeps a failed hardware request from
    /// retrying and rebuilding the surface on every frame.
    requested_hardware: bool,
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            let _ = SelectObject(self.mem_dc, self.old);
            let _ = DeleteObject(self.dib);
            let _ = DeleteDC(self.mem_dc);
        }
    }
}

impl Renderer {
    pub fn new() -> windows::core::Result<Self> {
        let factory: ID2D1Factory = unsafe {
            D2D1CreateFactory(
                D2D1_FACTORY_TYPE_SINGLE_THREADED,
                Some(&D2D1_FACTORY_OPTIONS::default()),
            )?
        };
        let dwrite: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };
        Ok(Self {
            factory,
            dwrite,
            surface: None,
            cover_version: 0,
            cover: None,
            cover_bitmap: None,
            background_path: None,
            background: None,
            background_bitmap: None,
            font_family: None,
            italic: false,
            blur_radius: 0.0,
            current_row: None,
        })
    }

    pub fn set_cover(&mut self, version: u64, png: Option<&[u8]>) {
        if version == self.cover_version {
            return;
        }
        self.cover_version = version;
        self.cover_bitmap = None;
        self.cover = png.and_then(|bytes| image::load_from_memory(bytes).ok().map(|img| img.to_rgba8()));
    }

    fn ensure_background(&mut self, path: Option<&str>) {
        let path = path.map(str::trim).filter(|path| !path.is_empty());
        if path == self.background_path.as_deref() {
            return;
        }
        self.background_path = path.map(str::to_string);
        self.background_bitmap = None;
        self.background = path
            .and_then(|path| image::open(path).ok())
            .map(|image| image.to_rgba8());
    }

    /// Draw one frame and publish it to the layered window. Returns the
    /// logical height the window should use when auto-height is enabled.
    pub fn render(
        &mut self,
        hwnd: HWND,
        state: &OverlayState,
        lyrics: Option<&OverlayLyrics>,
        position_ms: u64,
        width_px: u32,
        height_px: u32,
        dpi: f32,
    ) -> f32 {
        let settings = &state.settings;
        self.font_family = settings
            .font_family
            .as_deref()
            .map(str::trim)
            .filter(|family| !family.is_empty() && !looks_like_path(family))
            .map(str::to_string);
        self.italic = settings.italic;
        if (self.blur_radius - settings.blur_radius).abs() > 0.01 {
            self.blur_radius = settings.blur_radius;
            self.cover_bitmap = None;
        }
        self.ensure_target(
            width_px.max(1),
            height_px.max(1),
            dpi,
            settings.d2d_hardware,
        );
        self.ensure_background(settings.bg_image.as_deref());

        let scale = (dpi / 96.0).max(0.5);
        let width = width_px.max(1) as f32 / scale;
        let height = height_px.max(1) as f32 / scale;
        let auto_height = auto_height(settings, &state.title, &state.artist);

        let (rt, surface_width, surface_height, mem_dc) = {
            let Some(surface) = self.surface.as_ref() else {
                return auto_height;
            };
            (
                surface.rt.clone(),
                surface.width,
                surface.height,
                surface.mem_dc,
            )
        };
        let rt: &ID2D1RenderTarget = &rt;
        unsafe {
            let _ = rt.BeginDraw();
            let transparent = D2D1_COLOR_F {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0,
            };
            let _ = rt.Clear(Some(&transparent));
            self.paint_background(rt, settings, width, height);
            let current = self.paint_body(rt, state, lyrics, position_ms, width, height);
            self.current_row = current.map(|(line, region)| CurrentRow {
                line,
                region,
                width: surface_width,
                height: surface_height,
            });
            if let Err(err) = rt.EndDraw(None, None) {
                eprintln!("desktop lyrics overlay: EndDraw failed: {err}");
            }
        }
        present(hwnd, mem_dc, surface_width, surface_height, None);
        auto_height
    }

    /// Repaint the frame and publish only the active lyric row. Returns false
    /// when the cached row is stale (line changed, size changed or layout
    /// failed), in which case the caller must run a full render.
    pub fn render_line(
        &mut self,
        hwnd: HWND,
        state: &OverlayState,
        lyrics: &OverlayLyrics,
        position_ms: u64,
        width_px: u32,
        height_px: u32,
        dpi: f32,
        line_index: usize,
    ) -> bool {
        let Some(cached) = self.current_row else {
            return false;
        };
        if cached.line != line_index
            || cached.width != width_px
            || cached.height != height_px
        {
            return false;
        }
        let settings = &state.settings;
        self.font_family = settings
            .font_family
            .as_deref()
            .map(str::trim)
            .filter(|family| !family.is_empty() && !looks_like_path(family))
            .map(str::to_string);
        self.italic = settings.italic;
        if (self.blur_radius - settings.blur_radius).abs() > 0.01 {
            self.blur_radius = settings.blur_radius;
            self.cover_bitmap = None;
        }
        self.ensure_target(
            width_px.max(1),
            height_px.max(1),
            dpi,
            settings.d2d_hardware,
        );
        self.ensure_background(settings.bg_image.as_deref());
        let scale = (dpi / 96.0).max(0.5);
        let width = width_px.max(1) as f32 / scale;
        let height = height_px.max(1) as f32 / scale;
        let (rt, mem_dc) = {
            let Some(surface) = self.surface.as_ref() else {
                return false;
            };
            if surface.width != width_px || surface.height != height_px {
                return false;
            }
            (surface.rt.clone(), surface.mem_dc)
        };
        let rt: &ID2D1RenderTarget = &rt;
        let current = unsafe {
            let _ = rt.BeginDraw();
            let transparent = D2D1_COLOR_F {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0,
            };
            let _ = rt.Clear(Some(&transparent));
            self.paint_background(rt, settings, width, height);
            let current = self.paint_body(rt, state, Some(lyrics), position_ms, width, height);
            if let Err(err) = rt.EndDraw(None, None) {
                eprintln!("desktop lyrics overlay: partial EndDraw failed: {err}");
            }
            current
        };
        let Some((line, new_region)) = current else {
            return false;
        };
        if line != line_index {
            return false;
        }
        self.current_row = Some(CurrentRow {
            line,
            region: new_region,
            width: width_px,
            height: height_px,
        });
        // Publish the union of the old and new row rectangles so a row that
        // moved by a fraction of a pixel leaves no trail.
        let region = rect_union(cached.region, new_region);
        let px = pixel_rect(
            region.left,
            region.top,
            region.right,
            region.bottom,
            scale,
            width_px,
            height_px,
        );
        present(hwnd, mem_dc, width_px, height_px, Some(px));
        true
    }

    fn ensure_target(&mut self, width: u32, height: u32, dpi: f32, hardware: bool) {
        if let Some(surface) = &self.surface
            && surface.width == width
            && surface.height == height
            && (surface.dpi - dpi).abs() < 0.5
            && surface.requested_hardware == hardware
        {
            return;
        }
        self.surface = None;
        self.cover_bitmap = None;
        self.background_bitmap = None;
        let dpi = dpi.max(96.0);
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                biHeight: -(height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let mem_dc = unsafe { CreateCompatibleDC(None) };
        if mem_dc.is_invalid() {
            return;
        }
        let dib = match unsafe {
            CreateDIBSection(None, &bmi, DIB_RGB_COLORS, &mut bits, None, 0)
        } {
            Ok(dib) => dib,
            Err(err) => {
                eprintln!("desktop lyrics overlay: CreateDIBSection failed: {err}");
                unsafe {
                    let _ = DeleteDC(mem_dc);
                }
                return;
            }
        };
        let old = unsafe { SelectObject(mem_dc, HGDIOBJ::from(dib)) };
        let base = D2D1_RENDER_TARGET_PROPERTIES {
            // Software is the default (small DIB surfaces gain nothing from a
            // hardware DC target); the performance setting can request the
            // hardware path, which falls back to software when D3D is
            // unavailable.
            r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: dpi,
            dpiY: dpi,
            usage: D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
        };
        let mut actual_hardware = false;
        let rt = if hardware {
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_HARDWARE,
                ..base
            };
            match unsafe { self.factory.CreateDCRenderTarget(&props) } {
                Ok(rt) => {
                    actual_hardware = true;
                    rt
                }
                Err(err) => {
                    eprintln!("desktop lyrics overlay: hardware target failed ({err}), using software");
                    match unsafe { self.factory.CreateDCRenderTarget(&base) } {
                        Ok(rt) => rt,
                        Err(err) => {
                            eprintln!(
                                "desktop lyrics overlay: CreateDCRenderTarget failed: {err}"
                            );
                            unsafe {
                                let _ = SelectObject(mem_dc, old);
                                let _ = DeleteObject(HGDIOBJ::from(dib));
                                let _ = DeleteDC(mem_dc);
                            }
                            return;
                        }
                    }
                }
            }
        } else {
            match unsafe { self.factory.CreateDCRenderTarget(&base) } {
                Ok(rt) => rt,
                Err(err) => {
                    eprintln!("desktop lyrics overlay: CreateDCRenderTarget failed: {err}");
                    unsafe {
                        let _ = SelectObject(mem_dc, old);
                        let _ = DeleteObject(HGDIOBJ::from(dib));
                        let _ = DeleteDC(mem_dc);
                    }
                    return;
                }
            }
        };
        let rect = RECT {
            left: 0,
            top: 0,
            right: width as i32,
            bottom: height as i32,
        };
        unsafe {
            let _ = rt.BindDC(mem_dc, &rect);
            rt.SetDpi(dpi, dpi);
            let _ = rt.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            let _ = rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            let _ = rt.SetTransform(&Matrix3x2::identity());
        }
        tracing::info!(
            "desktop lyrics overlay: D2D {} target {}x{}",
            if actual_hardware { "hardware" } else { "software" },
            width,
            height
        );
        self.surface = Some(Surface {
            rt,
            mem_dc,
            dib: HGDIOBJ::from(dib),
            old,
            width,
            height,
            dpi,
            requested_hardware: hardware,
        });
        self.current_row = None;
    }

    fn paint_background(&mut self, rt: &ID2D1RenderTarget, settings: &OverlaySettings, width: f32, height: f32) {
        let opacity = settings.opacity.clamp(0.0, 1.0);
        if settings.background == 0 {
            return;
        }
        let radius = settings.radius.min(48) as f32;
        let rect = D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F {
                left: 0.0,
                top: 0.0,
                right: width,
                bottom: height,
            },
            radiusX: radius,
            radiusY: radius,
        };

        match settings.background {
            1 => {
                if let Ok(brush) = self.solid(rt, opaque(settings.bg_color), opacity) {
                    unsafe { rt.FillRoundedRectangle(&rect, &brush) };
                }
            }
            2 => {
                if let Ok(collection) = unsafe {
                    rt.CreateGradientStopCollection(
                        &[
                            D2D1_GRADIENT_STOP {
                                position: 0.0,
                                color: color(opaque(settings.bg_color), opacity),
                            },
                            D2D1_GRADIENT_STOP {
                                position: 1.0,
                                color: color(opaque(settings.bg_color2), opacity),
                            },
                        ],
                        D2D1_GAMMA_2_2,
                        D2D1_EXTEND_MODE_CLAMP,
                    )
                } {
                    let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
                        startPoint: Vector2 { X: 0.0, Y: 0.0 },
                        endPoint: Vector2 { X: 0.0, Y: height },
                    };
                    if let Ok(brush) =
                        unsafe { rt.CreateLinearGradientBrush(&props, None, &collection) }
                    {
                        unsafe { rt.FillRoundedRectangle(&rect, &brush) };
                    }
                }
            }
            3 => {
                let bitmap = self
                    .bitmap_for(rt, false)
                    .filter(|_| self.background.is_some());
                if let Some(bitmap) = bitmap {
                    let dest = fit_cover(unsafe { bitmap.GetSize() }, width, height);
                    self.with_rounded_clip(rt, &rect, |rt| unsafe {
                        let _ = rt.DrawBitmap(
                            &bitmap,
                            Some(&dest),
                            opacity,
                            D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                            None,
                        );
                    });
                } else if let Ok(brush) = self.solid(rt, opaque(settings.bg_color), opacity) {
                    unsafe { rt.FillRoundedRectangle(&rect, &brush) };
                }
            }
            4 => {
                let bitmap = self.bitmap_for(rt, true).filter(|_| self.cover.is_some());
                if let Some(bitmap) = bitmap {
                    let dest = fit_cover(unsafe { bitmap.GetSize() }, width, height);
                    self.with_rounded_clip(rt, &rect, |rt| unsafe {
                        let _ = rt.DrawBitmap(
                            &bitmap,
                            Some(&dest),
                            opacity,
                            D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                            None,
                        );
                        if let Ok(brush) = self.solid(rt, [0, 0, 0, 70], opacity) {
                            rt.FillRoundedRectangle(&rect, &brush);
                        }
                    });
                } else if let Ok(brush) = self.solid(rt, opaque(settings.bg_color), opacity) {
                    unsafe { rt.FillRoundedRectangle(&rect, &brush) };
                }
            }
            _ => {}
        }

        if settings.border
            && let Ok(brush) = self.solid(rt, [255, 255, 255, 18], opacity)
        {
            let border = D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F {
                    left: 1.0,
                    top: 1.0,
                    right: width - 1.0,
                    bottom: height - 1.0,
                },
                radiusX: (radius - 1.0).max(0.0),
                radiusY: (radius - 1.0).max(0.0),
            };
            unsafe {
                rt.DrawRoundedRectangle(&border, &brush, 1.0, None);
            }
        }
    }

    fn paint_body(
        &self,
        rt: &ID2D1RenderTarget,
        state: &OverlayState,
        lyrics: Option<&OverlayLyrics>,
        position_ms: u64,
        width: f32,
        height: f32,
    ) -> Option<(usize, D2D_RECT_F)> {
        let settings = &state.settings;
        let scale = if settings.bold { 1.03 } else { 1.0 };
        let padding = settings.padding.clamp(0.0, 80.0);
        let content_w = (width - padding * 2.0).max(1.0);
        let content_y = padding * 0.7;
        let content_h = (height - padding * 1.4).max(1.0);
        let count = settings.lines.clamp(1, 3) as usize;
        let adjusted = effective_position(position_ms, state.offset_ms);

        let active = lyrics.and_then(|lyrics| index_at(lyrics, adjusted));
        let mut rows = Vec::new();
        let mut status = None;
        match lyrics {
            Some(lyrics) if !lyrics.lines.is_empty() => {
                let (start, end) = visible_range(lyrics, active, count);
                for index in start..end {
                    let kind = match active {
                        Some(current) if index < current => RowKind::Previous,
                        Some(current) if index > current => RowKind::Next,
                        Some(_) => RowKind::Current,
                        None => RowKind::Next,
                    };
                    if let Some(row) = self.prepare_row(
                        &lyrics.lines[index],
                        index,
                        kind,
                        settings,
                        lyrics,
                        content_w,
                        adjusted,
                    ) {
                        rows.push(row);
                    }
                }
            }
            _ => {
                status = Some(if state.lyrics_loading {
                    state.labels.loading.clone()
                } else if state.title.is_empty() {
                    state.labels.app_name.clone()
                } else {
                    state.labels.no_lyrics.clone()
                });
            }
        }

        let footer = settings.show_footer.then(|| {
            let mut text = String::new();
            if !state.title.is_empty() {
                text.push_str(&state.title);
            }
            if !state.artist.is_empty() {
                if !text.is_empty() {
                    text.push_str(" - ");
                }
                text.push_str(&state.artist);
            }
            text
        });
        let footer_size = settings.footer_font_size.clamp(8.0, 48.0) * scale;
        let footer_h = if footer.as_deref().is_some_and(|text| !text.is_empty()) {
            footer_size + 10.0
        } else {
            0.0
        };

        let spacing = settings.line_spacing.clamp(0.0, 60.0);
        let mut total_h = footer_h;
        for (index, row) in rows.iter().enumerate() {
            if index > 0 {
                total_h += spacing;
            }
            total_h += row.height();
        }
        if status.is_some() {
            total_h += (settings.next_font_size * scale).max(12.0) + 6.0;
        }
        let mut y = content_y + ((content_h - total_h) * 0.5).max(0.0);
        let mut current = None;

        for (index, row) in rows.iter().enumerate() {
            if index > 0 {
                y += spacing;
            }
            if row.kind == RowKind::Current {
                current = Some((
                    row.line_index,
                    D2D_RECT_F {
                        left: (padding - 6.0).max(0.0),
                        top: (y - 4.0).max(0.0),
                        right: (padding + content_w + 6.0).min(width),
                        bottom: (y + row.height() + 6.0).min(height),
                    },
                ));
            }
            row.paint(self, rt, settings, content_w, padding, y);
            y += row.height();
        }

        if let Some(status) = status {
            let size = (settings.next_font_size * scale).max(12.0);
            if let Ok(layout) = self.layout(&status, size, false, settings.align, content_w)
                && let Ok(brush) = self.solid(rt, settings.next_color, 1.0)
            {
                unsafe {
                    let _ = rt.DrawTextLayout(
                        Vector2 { X: padding, Y: y },
                        &layout,
                        &brush,
                        D2D1_DRAW_TEXT_OPTIONS_NONE,
                    );
                }
            }
        }

        if let Some(footer) = footer.filter(|text| !text.is_empty()) {
            if let Ok(layout) = self.layout_with(
                &footer,
                footer_size,
                settings.footer_bold,
                settings.align,
                content_w,
                settings.footer_font_family.as_deref(),
            )
                && let Ok(brush) = self.solid(rt, settings.footer_color, 1.0)
            {
                let metrics = metrics(&layout);
                let y = height - padding * 0.7 - metrics.height;
                self.decorated(
                    rt,
                    layout,
                    brush,
                    true,
                    padding,
                    y,
                    settings.footer_outline,
                    settings.footer_outline_width,
                    settings.footer_outline_color,
                    settings.footer_shadow,
                    settings.footer_shadow_offset,
                    settings.footer_shadow_color,
                );
            }
        }
        current
    }

    fn prepare_row(
        &self,
        line: &OverlayLine,
        index: usize,
        kind: RowKind,
        settings: &OverlaySettings,
        lyrics: &OverlayLyrics,
        width: f32,
        position_ms: u64,
    ) -> Option<PreparedRow> {
        let scale = if settings.bold { 1.03 } else { 1.0 };
        let (size, base_color) = row_style(settings, kind, scale);
        // Lyrics are laid out as a single row so long lines can scroll with
        // the karaoke edge instead of wrapping. Alignment is applied manually
        // when the text fits.
        let main = self
            .layout_line(&line.text, size, settings.bold)
            .ok()?;
        let main_metrics = metrics(&main);
        let main_w = main_metrics.width;
        let main_h = main_metrics.height;
        let translation_size = (size * 0.60).max(10.0);
        let translation = if settings.show_translation {
            line.translation
                .as_deref()
                .filter(|text| !text.trim().is_empty())
                .and_then(|text| {
                    self.layout_with(
                        text,
                        translation_size,
                        settings.translation_bold,
                        settings.align,
                        width,
                        settings.translation_font_family.as_deref(),
                    )
                    .ok()
                })
        } else {
            None
        };
        let translation_h = translation.as_ref().map(metrics).map(|m| m.height).unwrap_or(0.0);
        let active = kind == RowKind::Current;
        let karaoke = settings.karaoke && (!settings.karaoke_words_only || !line.words.is_empty());
        let progress = if active && karaoke {
            let line_end = line.end_ms.unwrap_or_else(|| {
                lyrics
                    .lines
                    .get(index + 1)
                    .map(|next| next.start_ms)
                    .unwrap_or(line.start_ms + 5000)
            });
            Some(karaoke_progress(line, position_ms, line_end))
        } else {
            None
        };
        let scroll = match &progress {
            Some(progress) if main_w > width => {
                // Keep a margin so the sung edge is never glued to the edge.
                let edge = karaoke_edge_x(&main, progress, &line.text);
                (edge + 24.0 - width).clamp(0.0, main_w - width)
            }
            _ => 0.0,
        };
        Some(PreparedRow {
            line_index: index,
            main,
            main_w,
            main_h,
            scroll,
            translation,
            translation_h,
            kind,
            color: base_color,
            active,
            progress,
            line_text: line.text.clone(),
        })
    }

    fn layout_line(
        &self,
        text: &str,
        size: f32,
        bold: bool,
    ) -> windows::core::Result<IDWriteTextLayout> {
        self.layout_line_with(text, size, bold, None)
    }

    fn layout_line_with(
        &self,
        text: &str,
        size: f32,
        bold: bool,
        family: Option<&str>,
    ) -> windows::core::Result<IDWriteTextLayout> {
        let format = self.format_with(size, bold, 0, family)?;
        unsafe {
            let _ = format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
        }
        let wide: Vec<u16> = text.encode_utf16().collect();
        // A large width means the layout reports the full text width even
        // though only `content_w` is visible.
        unsafe { self.dwrite.CreateTextLayout(&wide, &format, 10_000.0, MAX_LAYOUT_HEIGHT) }
    }

    fn layout(
        &self,
        text: &str,
        size: f32,
        bold: bool,
        align: u8,
        width: f32,
    ) -> windows::core::Result<IDWriteTextLayout> {
        self.layout_with(text, size, bold, align, width, None)
    }

    #[allow(clippy::too_many_arguments)]
    fn layout_with(
        &self,
        text: &str,
        size: f32,
        bold: bool,
        align: u8,
        width: f32,
        family: Option<&str>,
    ) -> windows::core::Result<IDWriteTextLayout> {
        let format = self.format_with(size, bold, align, family)?;
        let wide: Vec<u16> = text.encode_utf16().collect();
        let max_width = width.max(1.0);
        unsafe { self.dwrite.CreateTextLayout(&wide, &format, max_width, MAX_LAYOUT_HEIGHT) }
    }

    fn format_with(
        &self,
        size: f32,
        bold: bool,
        align: u8,
        family: Option<&str>,
    ) -> windows::core::Result<IDWriteTextFormat> {
        let family = wide(
            family
                .or(self.font_family.as_deref())
                .filter(|family| !looks_like_path(family))
                .unwrap_or(DEFAULT_FAMILY),
        );
        let locale = wide("zh-CN");
        let format = unsafe {
            self.dwrite.CreateTextFormat(
                PCWSTR(family.as_ptr()),
                None,
                if bold {
                    DWRITE_FONT_WEIGHT_BOLD
                } else {
                    DWRITE_FONT_WEIGHT_NORMAL
                },
                if self.italic {
                    DWRITE_FONT_STYLE_ITALIC
                } else {
                    DWRITE_FONT_STYLE_NORMAL
                },
                DWRITE_FONT_STRETCH_NORMAL,
                size.max(1.0),
                PCWSTR(locale.as_ptr()),
            )
        }?;
        unsafe {
            let alignment = match align {
                0 => DWRITE_TEXT_ALIGNMENT_LEADING,
                2 => DWRITE_TEXT_ALIGNMENT_TRAILING,
                _ => DWRITE_TEXT_ALIGNMENT_CENTER,
            };
            let _ = format.SetTextAlignment(alignment);
            let _ = format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR);
            let _ = format.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP);
        }
        Ok(format)
    }

    fn solid(
        &self,
        rt: &ID2D1RenderTarget,
        rgba: [u8; 4],
        opacity: f32,
    ) -> windows::core::Result<ID2D1SolidColorBrush> {
        unsafe { rt.CreateSolidColorBrush(&color(rgba, opacity), None) }
    }

    fn bitmap_for(
        &mut self,
        rt: &ID2D1RenderTarget,
        cover: bool,
    ) -> Option<windows::Win32::Graphics::Direct2D::ID2D1Bitmap> {
        let cached = if cover {
            &mut self.cover_bitmap
        } else {
            &mut self.background_bitmap
        };
        if let Some(bitmap) = cached {
            return Some(bitmap.clone());
        }
        let source = if cover {
            self.cover.as_ref()
        } else {
            self.background.as_ref()
        }?;
        // The blur is baked once when the bitmap is created; a blurred cover
        // is only used for the `BlurCover` background mode.
        let blurred;
        let image = if cover && self.blur_radius > 0.01 {
            blurred = blur_rgba(source, self.blur_radius);
            &blurred
        } else {
            source
        };
        let (width, height) = image.dimensions();
        let mut bgra = Vec::with_capacity((width * height * 4) as usize);
        for pixel in image.pixels() {
            let alpha = pixel[3] as u32;
            bgra.push(((pixel[2] as u32 * alpha) / 255) as u8);
            bgra.push(((pixel[1] as u32 * alpha) / 255) as u8);
            bgra.push(((pixel[0] as u32 * alpha) / 255) as u8);
            bgra.push(pixel[3]);
        }
        let props = D2D1_BITMAP_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
        };
        let bitmap = unsafe {
            rt.CreateBitmap(
                D2D_SIZE_U { width, height },
                Some(bgra.as_ptr() as *const c_void),
                width * 4,
                &props,
            )
        }
        .ok()?;
        *cached = Some(bitmap.clone());
        Some(bitmap)
    }

    fn with_rounded_clip(
        &self,
        rt: &ID2D1RenderTarget,
        rounded: &D2D1_ROUNDED_RECT,
        draw: impl FnOnce(&ID2D1RenderTarget),
    ) {
        let Ok(geometry) = (unsafe { self.factory.CreateRoundedRectangleGeometry(rounded) }) else {
            draw(rt);
            return;
        };
        let geometry: windows::core::Result<windows::Win32::Graphics::Direct2D::ID2D1Geometry> =
            geometry.cast();
        let Ok(geometry) = geometry else {
            draw(rt);
            return;
        };
        let layer = unsafe { rt.CreateLayer(None) };
        let Ok(layer) = layer else {
            draw(rt);
            return;
        };
        let params = D2D1_LAYER_PARAMETERS {
            contentBounds: rounded.rect,
            geometricMask: ManuallyDrop::new(Some(geometry)),
            maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
            maskTransform: Matrix3x2::identity(),
            opacity: 1.0,
            opacityBrush: ManuallyDrop::new(None::<ID2D1Brush>),
            layerOptions: D2D1_LAYER_OPTIONS_NONE,
        };
        unsafe {
            rt.PushLayer(&params, &layer);
            draw(rt);
            rt.PopLayer();
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn decorated(
        &self,
        rt: &ID2D1RenderTarget,
        layout: IDWriteTextLayout,
        fill: ID2D1SolidColorBrush,
        active: bool,
        x: f32,
        y: f32,
        outline_enabled: bool,
        outline_width: f32,
        outline_color: [u8; 4],
        shadow_enabled: bool,
        shadow_offset: f32,
        shadow_color: [u8; 4],
    ) {
        if active && shadow_enabled {
            if let Ok(shadow) = self.solid(rt, shadow_color, 1.0) {
                unsafe {
                    let _ = rt.DrawTextLayout(
                        Vector2 {
                            X: x,
                            Y: y + shadow_offset.clamp(0.0, 24.0),
                        },
                        &layout,
                        &shadow,
                        D2D1_DRAW_TEXT_OPTIONS_NONE,
                    );
                }
            }
        }
        if active && outline_enabled && outline_width > 0.0 {
            if let Ok(outline) = self.solid(rt, outline_color, 1.0) {
                let radius = outline_width.clamp(0.5, 6.0).ceil().max(1.0) as i32;
                let limit = radius * radius;
                for dy in -radius..=radius {
                    for dx in -radius..=radius {
                        if dx == 0 && dy == 0 {
                            continue;
                        }
                        if dx * dx + dy * dy > limit {
                            continue;
                        }
                        unsafe {
                            let _ = rt.DrawTextLayout(
                                Vector2 {
                                    X: x + dx as f32,
                                    Y: y + dy as f32,
                                },
                                &layout,
                                &outline,
                                D2D1_DRAW_TEXT_OPTIONS_NONE,
                            );
                        }
                    }
                }
            }
        }
        unsafe {
            let _ = rt.DrawTextLayout(
                Vector2 { X: x, Y: y },
                &layout,
                &fill,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
        }
    }
}

struct PreparedRow {
    line_index: usize,
    main: IDWriteTextLayout,
    main_w: f32,
    main_h: f32,
    /// Horizontal offset so a long karaoke line follows the sung position.
    scroll: f32,
    translation: Option<IDWriteTextLayout>,
    translation_h: f32,
    kind: RowKind,
    color: D2D1_COLOR_F,
    active: bool,
    progress: Option<KaraokeProgress>,
    line_text: String,
}

impl PreparedRow {
    fn height(&self) -> f32 {
        self.main_h + if self.translation.is_some() { 2.0 + self.translation_h } else { 0.0 }
    }

    fn paint(
        &self,
        renderer: &Renderer,
        rt: &ID2D1RenderTarget,
        settings: &OverlaySettings,
        width: f32,
        x: f32,
        y: f32,
    ) {
        let current = color(settings.current_color, 1.0);
        let is_active = self.active;
        let fill_color = if is_active { current } else { self.color };
        let Ok(fill) = (unsafe { rt.CreateSolidColorBrush(&fill_color, None) }) else {
            return;
        };
        let _ = self.kind;
        // Long lines are single-row and horizontally clipped. The current
        // line scrolls so the karaoke edge stays visible.
        let align_offset = if self.main_w <= width {
            match settings.align {
                0 => 0.0,
                2 => width - self.main_w,
                _ => (width - self.main_w) * 0.5,
            }
        } else {
            0.0
        };
        let text_x = x + align_offset - self.scroll;
        let clip = D2D_RECT_F {
            left: x,
            top: (y - 4.0).max(0.0),
            right: x + width,
            bottom: y + self.main_h + 6.0,
        };
        unsafe {
            let _ = rt.PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        }
        renderer.decorated(
            rt,
            self.main.clone(),
            fill.clone(),
            // Outline/shadow are surface-wide settings: the previous/next
            // rows must look like the current line, not lose their treatment.
            true,
            text_x,
            y,
            settings.outline,
            settings.outline_width,
            settings.outline_color,
            settings.shadow,
            settings.shadow_offset,
            settings.shadow_color,
        );

        if let Some(progress) = &self.progress {
            if let Ok(pending) = renderer.solid(rt, settings.next_color, 1.0) {
                unsafe {
                    let _ = rt.DrawTextLayout(
                        Vector2 { X: text_x, Y: y },
                        &self.main,
                        &pending,
                        D2D1_DRAW_TEXT_OPTIONS_NONE,
                    );
                }
            }
            if let Ok(active) = renderer.solid(rt, settings.current_color, 1.0) {
                draw_karaoke(
                    rt,
                    &self.main,
                    &active,
                    progress,
                    &self.line_text,
                    text_x,
                    y,
                    width,
                );
            }
        }
        unsafe {
            rt.PopAxisAlignedClip();
        }

        if let Some(translation) = &self.translation {
            let alpha = if is_active { 0.9 } else { 0.67 };
            let mut color = settings.translation_color;
            color[3] = (color[3] as f32 * alpha) as u8;
            if let Ok(brush) = renderer.solid(rt, color, 1.0) {
                let ty = y + self.main_h + 2.0;
                renderer.decorated(
                    rt,
                    translation.clone(),
                    brush,
                    // Same rule as the main row: the translation of a
                    // previous/next line keeps its outline/shadow.
                    true,
                    x,
                    ty,
                    settings.translation_outline,
                    settings.translation_outline_width,
                    settings.translation_outline_color,
                    settings.translation_shadow,
                    settings.translation_shadow_offset,
                    settings.translation_shadow_color,
                );
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowKind {
    Previous,
    Current,
    Next,
}

fn row_style(settings: &OverlaySettings, kind: RowKind, scale: f32) -> (f32, D2D1_COLOR_F) {
    match kind {
        RowKind::Previous => (
            settings.prev_font_size.clamp(8.0, 120.0) * scale,
            color(settings.prev_color, 1.0),
        ),
        RowKind::Current => (
            settings.font_size.clamp(8.0, 140.0) * scale,
            color(settings.current_color, 1.0),
        ),
        RowKind::Next => (
            settings.next_font_size.clamp(8.0, 120.0) * scale,
            color(settings.next_color, 1.0),
        ),
    }
}

fn visible_range(lyrics: &OverlayLyrics, active: Option<usize>, count: usize) -> (usize, usize) {
    let len = lyrics.lines.len();
    let count = count.min(len).max(1);
    let anchor = active.unwrap_or(0);
    let before = usize::from(count >= 3);
    let mut start = anchor.saturating_sub(before);
    if start + count > len {
        start = len.saturating_sub(count);
    }
    (start, (start + count).min(len))
}

pub(crate) fn index_at(lyrics: &OverlayLyrics, position_ms: u64) -> Option<usize> {
    if lyrics.lines.is_empty() || !lyrics.timed {
        return None;
    }
    let adjusted = (position_ms as i64 + lyrics.offset_ms).max(0) as u64;
    let mut lo = 0usize;
    let mut hi = lyrics.lines.len();
    while lo < hi {
        let mid = (lo + hi) / 2;
        if lyrics.lines[mid].start_ms <= adjusted {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    if lo == 0 { None } else { Some(lo - 1) }
}

pub(crate) fn effective_position(position_ms: u64, user_offset_ms: i64) -> u64 {
    let position = position_ms as i64;
    if user_offset_ms >= 0 {
        (position - user_offset_ms).max(0) as u64
    } else {
        (position + -user_offset_ms).max(0) as u64
    }
}

pub struct KaraokeProgress {
    pub sung_chars: usize,
    pub next: bool,
    pub fraction: f32,
}

/// X advance of the karaoke edge inside a single-row text layout.
fn karaoke_edge_x(layout: &IDWriteTextLayout, progress: &KaraokeProgress, text: &str) -> f32 {
    let split = utf16_len(text, progress.sung_chars);
    let mut px = 0.0f32;
    let mut py = 0.0f32;
    let mut hit = windows::Win32::Graphics::DirectWrite::DWRITE_HIT_TEST_METRICS::default();
    let ok = unsafe { layout.HitTestTextPosition(split, false, &mut px, &mut py, &mut hit) };
    if ok.is_ok() {
        if progress.next {
            px + hit.width * progress.fraction
        } else {
            px
        }
    } else {
        0.0
    }
}

fn karaoke_progress(line: &OverlayLine, position_ms: u64, line_end_ms: u64) -> KaraokeProgress {
    let total = line.text.chars().count();
    let (done, fraction) = if !line.words.is_empty() {
        let mut done = 0usize;
        let mut fraction = 0.0f32;
        for (index, word) in line.words.iter().enumerate() {
            let word_chars = word.text.chars().count();
            let word_end = word
                .end_ms
                .unwrap_or_else(|| {
                    line.words
                        .get(index + 1)
                        .map(|next| next.start_ms)
                        .unwrap_or(line_end_ms)
                })
                .max(word.start_ms);
            if position_ms >= word_end {
                done += word_chars;
                continue;
            }
            if position_ms > word.start_ms {
                let span = word_end.saturating_sub(word.start_ms).max(1);
                let exact = word_chars as f64 * (position_ms - word.start_ms) as f64 / span as f64;
                let whole = exact.floor() as usize;
                done += whole.min(word_chars);
                fraction = (exact - whole as f64) as f32;
            }
            break;
        }
        (done, fraction)
    } else {
        let span = line_end_ms.saturating_sub(line.start_ms).max(1);
        let elapsed = position_ms.saturating_sub(line.start_ms).min(span);
        let exact = total as f64 * elapsed as f64 / span as f64;
        let whole = exact.floor() as usize;
        (whole, (exact - whole as f64) as f32)
    };
    let done = done.min(total);
    KaraokeProgress {
        sung_chars: done,
        next: done < total,
        fraction: fraction.clamp(0.0, 1.0),
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_karaoke(
    rt: &ID2D1RenderTarget,
    layout: &IDWriteTextLayout,
    brush: &ID2D1SolidColorBrush,
    progress: &KaraokeProgress,
    text: &str,
    x: f32,
    y: f32,
    width: f32,
) {
    let split = utf16_len(text, progress.sung_chars);
    if split == 0 && !progress.next {
        return;
    }
    let lines = match layout_lines(layout) {
        Some(lines) => lines,
        None => {
            tracing::warn!("karaoke: GetLineMetrics failed text={text:?} split={split}");
            return;
        }
    };
    let mut offset = 0u32;
    let mut top = 0.0f32;
    for line in &lines {
        let end = offset + line.length;
        let line_top = y + top;
        if split >= end {
            let clip = D2D_RECT_F {
                left: x,
                top: line_top,
                right: x + width,
                bottom: line_top + line.height,
            };
            unsafe {
                let _ = rt.PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
                let _ = rt.DrawTextLayout(
                    Vector2 { X: x, Y: y },
                    layout,
                    brush,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                );
                rt.PopAxisAlignedClip();
            }
        } else if split > offset {
            let mut px = 0.0f32;
            let mut py = 0.0f32;
            let mut hit = windows::Win32::Graphics::DirectWrite::DWRITE_HIT_TEST_METRICS::default();
            let ok = unsafe { layout.HitTestTextPosition(split, false, &mut px, &mut py, &mut hit) };
            if ok.is_ok() {
                let advance = if progress.next {
                    px + hit.width * progress.fraction
                } else {
                    px
                };
                let clip = D2D_RECT_F {
                    left: x,
                    top: line_top,
                    right: x + advance,
                    bottom: line_top + line.height,
                };
                if clip.right > clip.left {
                    unsafe {
                        let _ = rt.PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
                        let _ = rt.DrawTextLayout(
                            Vector2 { X: x, Y: y },
                            layout,
                            brush,
                            D2D1_DRAW_TEXT_OPTIONS_NONE,
                        );
                        rt.PopAxisAlignedClip();
                    }
                }
            }
            break;
        } else {
            break;
        }
        offset = end;
        top += line.height;
    }
}

fn layout_lines(layout: &IDWriteTextLayout) -> Option<Vec<DWRITE_LINE_METRICS>> {
    // DirectWrite rejects a zero-sized buffer even for the "query the count"
    // form, so ask for a generous fixed capacity in one call. Lyric rows wrap
    // to a handful of visual lines.
    let mut lines = vec![DWRITE_LINE_METRICS::default(); 64];
    let mut actual = 0u32;
    unsafe { layout.GetLineMetrics(Some(&mut lines), &mut actual) }.ok()?;
    lines.truncate((actual as usize).min(lines.len()));
    Some(lines)
}

fn utf16_len(text: &str, chars: usize) -> u32 {
    text.chars().take(chars).map(char::len_utf16).sum::<usize>() as u32
}

fn metrics(layout: &IDWriteTextLayout) -> windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_METRICS {
    let mut metrics =
        windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_METRICS::default();
    unsafe {
        let _ = layout.GetMetrics(&mut metrics);
    }
    metrics
}

fn fit_cover(size: windows::Win32::Graphics::Direct2D::Common::D2D_SIZE_F, width: f32, height: f32) -> D2D_RECT_F {
    if size.width <= 0.0 || size.height <= 0.0 {
        return D2D_RECT_F {
            left: 0.0,
            top: 0.0,
            right: width,
            bottom: height,
        };
    }
    let source_ratio = size.width as f32 / size.height as f32;
    let target_ratio = width / height.max(1.0);
    if source_ratio > target_ratio {
        let scale = height / size.height as f32;
        let draw_w = size.width as f32 * scale;
        let left = (width - draw_w) * 0.5;
        D2D_RECT_F {
            left,
            top: 0.0,
            right: left + draw_w,
            bottom: height,
        }
    } else {
        let scale = width / size.width as f32;
        let draw_h = size.height as f32 * scale;
        let top = (height - draw_h) * 0.5;
        D2D_RECT_F {
            left: 0.0,
            top,
            right: width,
            bottom: top + draw_h,
        }
    }
}

fn color(rgba: [u8; 4], opacity: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: rgba[0] as f32 / 255.0,
        g: rgba[1] as f32 / 255.0,
        b: rgba[2] as f32 / 255.0,
        a: (rgba[3] as f32 / 255.0 * opacity.clamp(0.0, 1.0)).clamp(0.0, 1.0),
    }
}

/// Panel background colors are stored with a decorative alpha (185/255), but
/// the opacity setting is the only transparency control: at 100% the panel
/// must be fully opaque instead of keeping a 28% see-through tint.
fn opaque(rgba: [u8; 4]) -> [u8; 4] {
    [rgba[0], rgba[1], rgba[2], 255]
}

fn rect_union(a: D2D_RECT_F, b: D2D_RECT_F) -> D2D_RECT_F {
    D2D_RECT_F {
        left: a.left.min(b.left),
        top: a.top.min(b.top),
        right: a.right.max(b.right),
        bottom: a.bottom.max(b.bottom),
    }
}

fn pixel_rect(
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
    scale: f32,
    width: u32,
    height: u32,
) -> RECT {    RECT {
        left: ((left * scale).floor() as i32 - 1).max(0),
        top: ((top * scale).floor() as i32 - 1).max(0),
        right: ((right * scale).ceil() as i32 + 1).min(width as i32).max(0),
        bottom: ((bottom * scale).ceil() as i32 + 1).min(height as i32).max(0),
    }
}

fn present(hwnd: HWND, mem_dc: HDC, width: u32, height: u32, dirty: Option<RECT>) {
    let size = SIZE {
        cx: width as i32,
        cy: height as i32,
    };
    let src = POINT { x: 0, y: 0 };
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    unsafe {
        if let Some(dirty) = dirty {
            let info = UPDATELAYEREDWINDOWINFO {
                cbSize: std::mem::size_of::<UPDATELAYEREDWINDOWINFO>() as u32,
                hdcDst: HDC::default(),
                pptDst: std::ptr::null(),
                psize: &size,
                hdcSrc: mem_dc,
                pptSrc: &src,
                crKey: COLORREF(0),
                pblend: &blend,
                dwFlags: ULW_ALPHA,
                prcDirty: &dirty,
            };
            if UpdateLayeredWindowIndirect(hwnd, &info).as_bool() {
                return;
            }
        }
        let _ = UpdateLayeredWindow(
            hwnd,
            None,
            None,
            Some(&size),
            Some(mem_dc),
            Some(&src),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        );
    }
}

pub fn auto_height(settings: &OverlaySettings, title: &str, artist: &str) -> f32 {
    let scale = if settings.bold { 1.03 } else { 1.0 };
    let line_box = |size: f32| {
        size * 1.24
            + if settings.show_translation {
                (size * 0.60).max(10.0) * 1.24 + 2.0
            } else {
                0.0
            }
    };
    let current = line_box(settings.font_size * scale);
    let next = line_box(settings.next_font_size * scale);
    let previous = line_box(settings.prev_font_size * scale);
    let spacing = settings.line_spacing.clamp(0.0, 60.0);
    let lines = settings.lines.clamp(1, 3);
    let footer_h = if settings.show_footer && (!title.is_empty() || !artist.is_empty()) {
        settings.footer_font_size.clamp(8.0, 48.0) * scale + 10.0
    } else {
        0.0
    };
    let baseline = match lines {
        1 => current,
        2 => current + spacing + next,
        _ => previous + spacing + current + spacing + next,
    } + footer_h;
    (baseline + settings.padding.clamp(0.0, 80.0) * 1.4 + 8.0).clamp(48.0, 700.0)
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn looks_like_path(family: &str) -> bool {
    let lower = family.to_ascii_lowercase();
    family.contains('\\')
        || family.contains('/')
        || lower.ends_with(".ttf")
        || lower.ends_with(".ttc")
        || lower.ends_with(".otf")
        || lower.ends_with(".woff")
        || lower.ends_with(".woff2")
}

/// Small, allocation-free blurred copy for the BlurCover background. The
/// radius is expressed in logical pixels and scaled to the bitmap.
fn blur_rgba(source: &image::RgbaImage, radius: f32) -> image::RgbaImage {
    let (width, height) = source.dimensions();
    let radius = ((radius * width as f32 / 900.0).round() as i32).clamp(1, 48) as usize;
    let mut pixels = source.pixels().map(|p| p.0).collect::<Vec<_>>();
    let mut scratch = vec![[0u8; 4]; pixels.len()];
    for _ in 0..2 {
        // Horizontal pass.
        for y in 0..height as usize {
            for x in 0..width as usize {
                let mut sum = [0u32; 4];
                let mut count = 0u32;
                let start = x.saturating_sub(radius);
                let end = (x + radius).min(width as usize - 1);
                for sx in start..=end {
                    let pixel = pixels[y * width as usize + sx];
                    for channel in 0..4 {
                        sum[channel] += pixel[channel] as u32;
                    }
                    count += 1;
                }
                let out = &mut scratch[y * width as usize + x];
                for channel in 0..4 {
                    out[channel] = (sum[channel] / count) as u8;
                }
            }
        }
        // Vertical pass.
        for y in 0..height as usize {
            for x in 0..width as usize {
                let mut sum = [0u32; 4];
                let mut count = 0u32;
                let start = y.saturating_sub(radius);
                let end = (y + radius).min(height as usize - 1);
                for sy in start..=end {
                    let pixel = scratch[sy * width as usize + x];
                    for channel in 0..4 {
                        sum[channel] += pixel[channel] as u32;
                    }
                    count += 1;
                }
                let out = &mut pixels[y * width as usize + x];
                for channel in 0..4 {
                    out[channel] = (sum[channel] / count) as u8;
                }
            }
        }
    }
    image::RgbaImage::from_raw(width, height, pixels.into_iter().flatten().collect())
        .unwrap_or_else(|| source.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_opacity_background_is_fully_opaque() {
        // The stored background color keeps a decorative 185/255 alpha; the
        // opacity slider alone decides the panel's transparency.
        let stored = [14, 17, 26, 185];
        assert_eq!(color(opaque(stored), 1.0).a, 1.0);
        let half = color(opaque(stored), 0.5).a;
        assert!((half - 0.5).abs() < 0.01, "50% opacity -> {half}");
    }
}
