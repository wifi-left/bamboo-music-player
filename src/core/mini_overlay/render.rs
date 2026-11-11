//! Direct2D/DirectWrite renderer for the native mini player.
//!
//! Mirrors the layout of the egui mini player (`views::mini_player`) but draws
//! into a 32-bit top-down DIB that `UpdateLayeredWindow` composites. There is
//! no egui, wgpu or swapchain involved, so the main window does not have to
//! repaint to keep this window animated.

use std::ffi::c_void;
use std::mem::ManuallyDrop;

use image::RgbaImage;
use windows::Win32::Foundation::{COLORREF, HWND, POINT, RECT, SIZE};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_F,
    D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
    D2D1_BITMAP_PROPERTIES, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_FACTORY_OPTIONS,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT, D2D1_LAYER_OPTIONS_NONE,
    D2D1_LAYER_PARAMETERS, D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_HARDWARE,
    D2D1_RENDER_TARGET_TYPE_SOFTWARE,
    D2D1_RENDER_TARGET_USAGE_NONE, D2D1_ROUNDED_RECT, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
    D2D1CreateFactory, ID2D1Brush, ID2D1DCRenderTarget, ID2D1Factory, ID2D1RenderTarget,
    ID2D1SolidColorBrush,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT_BOLD, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_LINE_METRICS,
    DWRITE_PARAGRAPH_ALIGNMENT_NEAR, DWRITE_TEXT_ALIGNMENT, DWRITE_TEXT_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_TRIMMING,
    DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWRITE_WORD_WRAPPING_NO_WRAP, DWriteCreateFactory,
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

use crate::overlay_protocol::{MiniState, OverlayLine, OverlayLyrics};

const DEFAULT_FAMILY: &str = "Microsoft YaHei UI";
const ICON_FAMILY: &str = "Segoe Fluent Icons";
const MAX_LAYOUT_HEIGHT: f32 = 200.0;

/// Glyph code points from Segoe Fluent Icons / Segoe MDL2 Assets.
const ICON_CLOSE: char = '\u{E8BB}';
const ICON_MUTE: char = '\u{E74F}';
const ICON_VOLUME: char = '\u{E767}';
const ICON_PREV: char = '\u{E892}';
const ICON_NEXT: char = '\u{E893}';
const ICON_PLAY: char = '\u{E768}';
const ICON_PAUSE: char = '\u{E769}';
const ICON_MUSIC: char = '\u{E8D6}';

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hover {
    None,
    Close,
    Mute,
    Prev,
    Play,
    Next,
    Progress,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct BoxRect {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl BoxRect {
    fn new(left: f32, top: f32, right: f32, bottom: f32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.left && x <= self.right && y >= self.top && y <= self.bottom
    }

    pub fn union(self, other: Self) -> Self {
        Self {
            left: self.left.min(other.left),
            top: self.top.min(other.top),
            right: self.right.max(other.right),
            bottom: self.bottom.max(other.bottom),
        }
    }

    fn center(&self) -> (f32, f32) {
        ((self.left + self.right) * 0.5, (self.top + self.bottom) * 0.5)
    }
}

/// Logical layout shared by the renderer and hit-testing.
pub struct Layout {
    pub close: BoxRect,
    pub mute: BoxRect,
    pub prev: BoxRect,
    pub play: BoxRect,
    pub next: BoxRect,
    /// Expanded hit area around the thin progress bar.
    pub progress: BoxRect,
    /// Visible progress bar.
    pub progress_bar: BoxRect,
    pub cover: BoxRect,
    pub lyric: BoxRect,
    pub text_left: f32,
    pub text_right: f32,
    pub padding: f32,
}

pub fn layout(width: f32, height: f32) -> Layout {
    let padding = 14.0;
    let cover_side = (height - padding * 2.0).clamp(56.0, 96.0);
    let cover = BoxRect::new(
        padding,
        (height - cover_side) * 0.5,
        padding + cover_side,
        (height + cover_side) * 0.5,
    );
    let close = BoxRect::new(width - 27.0, 5.0, width - 5.0, 27.0);
    let mute = BoxRect::new(width - 53.0, 5.0, width - 31.0, 27.0);
    let button_size = 30.0;
    let next_cx = width - padding - button_size * 0.5;
    let next_cy = height - 40.0;
    let next = BoxRect::new(
        next_cx - button_size * 0.5,
        next_cy - button_size * 0.5,
        next_cx + button_size * 0.5,
        next_cy + button_size * 0.5,
    );
    let play = BoxRect::new(
        next.left - button_size - 6.0,
        next.top,
        next.left - 6.0,
        next.bottom,
    );
    let prev = BoxRect::new(
        play.left - button_size - 6.0,
        next.top,
        play.left - 6.0,
        next.bottom,
    );

    let text_left = cover.right + 12.0;
    let text_right = (width - padding - 44.0).min(width - padding).max(text_left);
    let progress_bar = BoxRect::new(
        text_left,
        height - padding - 12.0,
        (prev.left - 10.0).max(text_left + 10.0),
        height - padding - 8.0,
    );
    let progress = BoxRect::new(
        progress_bar.left,
        progress_bar.top - 8.0,
        progress_bar.right,
        progress_bar.bottom + 8.0,
    );

    let lyric_top = padding + 4.0 + 44.0;
    let controls_top = prev.top.min(progress_bar.top - 18.0);
    let lyric_bottom = (controls_top - 6.0).max(lyric_top + 1.0);
    let lyric = BoxRect::new(
        text_left,
        lyric_top,
        (width - padding).max(text_left + 10.0),
        lyric_bottom,
    );

    Layout {
        close,
        mute,
        prev,
        play,
        next,
        progress,
        progress_bar,
        cover,
        lyric,
        text_left,
        text_right,
        padding,
    }
}

impl Layout {
    pub fn bar_zone(&self) -> BoxRect {
        BoxRect::new(
            self.progress_bar.left - 4.0,
            self.progress_bar.top - 4.0,
            self.progress_bar.right + 4.0,
            self.progress_bar.bottom + 4.0,
        )
    }

    pub fn time_zone(&self) -> BoxRect {
        BoxRect::new(
            self.progress_bar.left - 4.0,
            self.progress_bar.top - 18.0,
            self.progress_bar.right + 4.0,
            self.progress_bar.top - 2.0,
        )
    }

    pub fn progress_zone(&self) -> BoxRect {
        self.bar_zone().union(self.time_zone())
    }

    pub fn mute_zone(&self) -> BoxRect {
        BoxRect::new(
            self.mute.left - 2.0,
            self.mute.top - 4.0,
            self.mute.right + 2.0,
            self.mute.bottom + 8.0,
        )
    }
}

pub struct Renderer {
    factory: ID2D1Factory,
    dwrite: IDWriteFactory,
    surface: Option<Surface>,
    cover_version: u64,
    cover: Option<RgbaImage>,
    cover_bitmap: Option<windows::Win32::Graphics::Direct2D::ID2D1Bitmap>,
    font_family: Option<String>,
    /// True after the first frame has been published (diagnostics only).
    first_frame: bool,
}

struct Surface {
    rt: ID2D1DCRenderTarget,
    mem_dc: HDC,
    dib: HGDIOBJ,
    old: HGDIOBJ,
    width: u32,
    height: u32,
    dpi: f32,
    /// What the settings asked for; a failed hardware request must not retry
    /// and rebuild the surface every frame.
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
            font_family: None,
            first_frame: false,
        })
    }

    pub fn set_cover(&mut self, version: u64, png: Option<&[u8]>) {
        if version == self.cover_version {
            return;
        }
        self.cover_version = version;
        self.cover_bitmap = None;
        self.cover = png.and_then(|bytes| {
            image::load_from_memory(bytes)
                .ok()
                .map(|img| img.to_rgba8())
        });
    }

    /// Draw one frame.
    ///
    /// `dirty` is either the region that changed (only the clock-driven parts
    /// are repainted over a pristine copy of the last full frame), or `None`
    /// to repaint and cache the whole window.
    pub fn render(
        &mut self,
        hwnd: HWND,
        state: &MiniState,
        lyrics: Option<&OverlayLyrics>,
        position_ms: u64,
        width_px: u32,
        height_px: u32,
        dpi: f32,
        seek_preview: Option<f32>,
        hover: Hover,
        dirty: Option<BoxRect>,
    ) {
        self.font_family = state
            .font_family
            .as_deref()
            .map(str::trim)
            .filter(|family| !family.is_empty() && !looks_like_path(family))
            .map(str::to_string);
        self.ensure_target(
            width_px.max(1),
            height_px.max(1),
            dpi,
            state.d2d_hardware,
        );
        let scale = (dpi / 96.0).max(0.5);
        let width = width_px.max(1) as f32 / scale;
        let height = height_px.max(1) as f32 / scale;
        let ui = layout(width, height);

        let (rt, surface_width, surface_height, mem_dc) = {
            let Some(surface) = self.surface.as_ref() else {
                tracing::warn!(
                    "native mini player: no D2D surface for {width_px}x{height_px} (dpi {dpi})"
                );
                return;
            };
            (
                surface.rt.clone(),
                surface.width,
                surface.height,
                surface.mem_dc,
            )
        };
        let rt: &ID2D1RenderTarget = &rt;

        // D2D always repaints the whole (small) surface. Only the dirty
        // rectangle is published to the layered window; blitting a cached
        // 32-bit frame back with GDI is not reliable for premultiplied alpha
        // and caused ghosted text.
        unsafe {
            let _ = rt.BeginDraw();
            let transparent = D2D1_COLOR_F {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0,
            };
            let _ = rt.Clear(Some(&transparent));
            self.paint_panel(rt, state, width, height);
            self.paint_cover(rt, state, &ui);
            self.paint_header(rt, state, &ui, hover);
            self.paint_controls(rt, state, &ui, hover);
            self.paint_progress(rt, state, &ui, position_ms, seek_preview, hover);
            self.paint_lyrics(rt, state, lyrics, &ui, position_ms);
            if let Err(err) = rt.EndDraw(None, None) {
                tracing::warn!("native mini player: EndDraw failed: {err}");
            }
        }
        let dirty_px = dirty.map(|region| pixel_rect(region, scale, surface_width, surface_height));
        present(hwnd, mem_dc, surface_width, surface_height, dirty_px);
        if !self.first_frame {
            self.first_frame = true;
            tracing::info!(
                "native mini player: first frame presented {surface_width}x{surface_height}"
            );
        }
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
            tracing::warn!("native mini player: CreateCompatibleDC failed");
            return;
        }
        let dib = match unsafe {
            CreateDIBSection(None, &bmi, DIB_RGB_COLORS, &mut bits, None, 0)
        } {
            Ok(dib) => dib,
            Err(err) => {
                tracing::warn!("native mini player: CreateDIBSection failed: {err}");
                unsafe {
                    let _ = DeleteDC(mem_dc);
                }
                return;
            }
        };
        let old = unsafe { SelectObject(mem_dc, HGDIOBJ::from(dib)) };
        let base = D2D1_RENDER_TARGET_PROPERTIES {
            // See the desktop overlay: software by default, hardware only
            // when the performance setting asks for it (with a fallback).
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
                    tracing::warn!(
                        "native mini player: hardware target failed ({err}), using software"
                    );
                    match unsafe { self.factory.CreateDCRenderTarget(&base) } {
                        Ok(rt) => rt,
                        Err(err) => {
                            tracing::warn!(
                                "native mini player: CreateDCRenderTarget failed: {err}"
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
                    tracing::warn!("native mini player: CreateDCRenderTarget failed: {err}");
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
            "native mini player: D2D {} target {}x{}",
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
    }

    fn paint_panel(&self, rt: &ID2D1RenderTarget, state: &MiniState, width: f32, height: f32) {
        let opacity = state.opacity.clamp(0.2, 1.0);
        let panel = color(state.colors.panel, opacity);
        if let Ok(brush) = unsafe { rt.CreateSolidColorBrush(&panel, None) } {
            let rounded = D2D1_ROUNDED_RECT {
                rect: rect_f(0.0, 0.0, width, height),
                radiusX: 12.0,
                radiusY: 12.0,
            };
            unsafe {
                rt.FillRoundedRectangle(&rounded, &brush);
            }
        }
        if let Ok(brush) = self.solid(rt, state.colors.line, 1.0) {
            let rounded = D2D1_ROUNDED_RECT {
                rect: rect_f(0.5, 0.5, (width - 0.5).max(0.5), (height - 0.5).max(0.5)),
                radiusX: 11.5,
                radiusY: 11.5,
            };
            unsafe {
                rt.DrawRoundedRectangle(&rounded, &brush, 1.0, None);
            }
        }
    }

    fn paint_cover(&mut self, rt: &ID2D1RenderTarget, state: &MiniState, ui: &Layout) {
        let rounded = D2D1_ROUNDED_RECT {
            rect: rect_f(
                ui.cover.left,
                ui.cover.top,
                ui.cover.right,
                ui.cover.bottom,
            ),
            radiusX: 8.0,
            radiusY: 8.0,
        };
        if let Some(bitmap) = self.bitmap_for(rt) {
            let size = unsafe { bitmap.GetSize() };
            let dest = fit_cover(size, ui.cover.right - ui.cover.left, ui.cover.bottom - ui.cover.top);
            let dest = D2D_RECT_F {
                left: dest.left + ui.cover.left,
                top: dest.top + ui.cover.top,
                right: dest.right + ui.cover.left,
                bottom: dest.bottom + ui.cover.top,
            };
            self.with_rounded_clip(rt, &rounded, |rt| unsafe {
                let _ = rt.DrawBitmap(
                    &bitmap,
                    Some(&dest as *const _),
                    1.0,
                    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                    None,
                );
            });
        } else {
            if let Ok(brush) = self.solid(rt, state.colors.card, 1.0) {
                unsafe {
                    rt.FillRoundedRectangle(&rounded, &brush);
                }
            }
            if let Ok(brush) = self.solid(rt, state.colors.text_faint, 1.0) {
                let (cx, cy) = ui.cover.center();
                self.icon(
                    rt,
                    ICON_MUSIC,
                    24.0,
                    &brush,
                    cx - 20.0,
                    cy - 20.0,
                    40.0,
                    40.0,
                );
            }
        }
    }

    fn paint_header(
        &self,
        rt: &ID2D1RenderTarget,
        state: &MiniState,
        ui: &Layout,
        hover: Hover,
    ) {
        let title = if state.title.trim().is_empty() {
            state.labels.not_playing.as_str()
        } else {
            state.title.as_str()
        };
        if let Ok(brush) = self.solid(rt, state.colors.text, 1.0) {
            let width = (ui.text_right - ui.text_left).max(1.0);
            self.text(
                rt,
                title,
                13.5,
                true,
                &brush,
                ui.text_left,
                ui.padding + 4.0,
                width,
            );
        }
        if !state.artist.trim().is_empty()
            && let Ok(brush) = self.solid(rt, state.colors.text_weak, 1.0)
        {
            let width = (ui.text_right - ui.text_left).max(1.0);
            self.text(
                rt,
                &state.artist,
                10.5,
                false,
                &brush,
                ui.text_left,
                ui.padding + 4.0 + 19.0,
                width,
            );
        }

        // Close button.
        let (cx, cy) = ui.close.center();
        if hover == Hover::Close {
            if let Ok(brush) = self.solid(rt, state.colors.card_hover, 1.0) {
                let circle = d2d_ellipse(cx, cy, 10.0, 10.0);
                unsafe {
                    rt.FillEllipse(&circle, &brush);
                }
            }
        }
        let icon_color = if hover == Hover::Close {
            state.colors.text
        } else {
            state.colors.text_faint
        };
        if let Ok(brush) = self.solid(rt, icon_color, 1.0) {
            self.icon(rt, ICON_CLOSE, 13.0, &brush, cx - 11.0, cy - 11.0, 22.0, 22.0);
        }

        // Mute button plus the thin volume level underneath.
        let (mx, my) = ui.mute.center();
        if hover == Hover::Mute {
            if let Ok(brush) = self.solid(rt, state.colors.card_hover, 1.0) {
                let circle = d2d_ellipse(mx, my, 10.0, 10.0);
                unsafe {
                    rt.FillEllipse(&circle, &brush);
                }
            }
        }
        let mute_color = if hover == Hover::Mute {
            state.colors.text
        } else {
            state.colors.text_faint
        };
        if let Ok(brush) = self.solid(rt, mute_color, 1.0) {
            let glyph = if state.muted { ICON_MUTE } else { ICON_VOLUME };
            self.icon(rt, glyph, 13.0, &brush, mx - 11.0, my - 11.0, 22.0, 22.0);
        }
        if let Ok(brush) = self.solid(rt, state.colors.card_hover, 1.0) {
            let bar = rect_f(ui.mute.left + 3.0, ui.mute.bottom + 1.0, ui.mute.right - 3.0, ui.mute.bottom + 3.0);
            unsafe {
                let _ = rt.FillRectangle(&bar, &brush);
            }
        }
        let level = if state.muted {
            0.0
        } else {
            state.volume.clamp(0.0, 1.0)
        };
        if level > 0.0
            && let Ok(brush) = self.solid(rt, state.colors.accent, 1.0)
        {
            let bar = rect_f(
                ui.mute.left + 3.0,
                ui.mute.bottom + 1.0,
                ui.mute.left + 3.0 + (ui.mute.right - ui.mute.left - 6.0) * level,
                ui.mute.bottom + 3.0,
            );
            unsafe {
                let _ = rt.FillRectangle(&bar, &brush);
            }
        }
    }

    fn paint_controls(
        &self,
        rt: &ID2D1RenderTarget,
        state: &MiniState,
        ui: &Layout,
        hover: Hover,
    ) {
        self.control(rt, state, &ui.prev, ICON_PREV, false, hover == Hover::Prev);
        self.control(
            rt,
            state,
            &ui.play,
            if state.playing { ICON_PAUSE } else { ICON_PLAY },
            true,
            hover == Hover::Play,
        );
        self.control(rt, state, &ui.next, ICON_NEXT, false, hover == Hover::Next);
    }

    fn control(
        &self,
        rt: &ID2D1RenderTarget,
        state: &MiniState,
        area: &BoxRect,
        glyph: char,
        primary: bool,
        hovered: bool,
    ) {
        let (cx, cy) = area.center();
        let radius = (area.right - area.left) * 0.5;
        if primary || hovered {
            let rgba = if primary {
                state.colors.accent
            } else {
                state.colors.card_hover
            };
            if let Ok(brush) = self.solid(rt, rgba, 1.0) {
                let circle = d2d_ellipse(cx, cy, radius, radius);
                unsafe {
                    rt.FillEllipse(&circle, &brush);
                }
            }
        }
        let icon_color = if primary {
            [12, 16, 24, 255]
        } else if hovered {
            state.colors.text
        } else {
            state.colors.text_weak
        };
        if let Ok(brush) = self.solid(rt, icon_color, 1.0) {
            self.icon(
                rt,
                glyph,
                15.0,
                &brush,
                area.left,
                area.top,
                area.right - area.left,
                area.bottom - area.top,
            );
        }
    }

    fn paint_progress(
        &self,
        rt: &ID2D1RenderTarget,
        state: &MiniState,
        ui: &Layout,
        position_ms: u64,
        seek_preview: Option<f32>,
        hover: Hover,
    ) {
        let duration = state.duration_ms.max(1) as f32;
        let ratio = if state.duration_ms == 0 {
            0.0
        } else {
            seek_preview.unwrap_or((position_ms as f32 / duration).clamp(0.0, 1.0))
        };
        let bar = ui.progress_bar;
        let track = D2D1_ROUNDED_RECT {
            rect: rect_f(bar.left, bar.top, bar.right, bar.bottom),
            radiusX: 2.0,
            radiusY: 2.0,
        };
        if let Ok(brush) = self.solid(rt, state.colors.card_hover, 1.0) {
            unsafe {
                rt.FillRoundedRectangle(&track, &brush);
            }
        }
        if ratio > 0.0
            && let Ok(brush) = self.solid(rt, state.colors.accent, 1.0)
        {
            let filled = D2D1_ROUNDED_RECT {
                rect: rect_f(
                    bar.left,
                    bar.top,
                    bar.left + (bar.right - bar.left) * ratio,
                    bar.bottom,
                ),
                radiusX: 2.0,
                radiusY: 2.0,
            };
            unsafe {
                rt.FillRoundedRectangle(&filled, &brush);
            }
        }
        if (hover == Hover::Progress || seek_preview.is_some())
            && let Ok(brush) = self.solid(rt, state.colors.text, 1.0)
        {
            let x = bar.left + (bar.right - bar.left) * ratio;
            let circle = d2d_ellipse(x, (bar.top + bar.bottom) * 0.5, 3.5, 3.5);
            unsafe {
                rt.FillEllipse(&circle, &brush);
            }
        }

        let left = format_time(position_ms / 1000);
        let right = format_time(state.duration_ms / 1000);
        if let Ok(brush) = self.solid(rt, state.colors.text_faint, 1.0) {
            self.text(
                rt,
                &left,
                9.0,
                false,
                &brush,
                bar.left,
                bar.top - 15.0,
                120.0,
            );
        }
        if let Ok(brush) = self.solid(rt, state.colors.text_faint, 1.0) {
            let width = 80.0;
            if let Ok(format) = self.format(9.0, false, DWRITE_TEXT_ALIGNMENT_TRAILING)
                && let Ok(layout) = self.layout_with(&format, &right, width)
            {
                unsafe {
                    let _ = rt.DrawTextLayout(
                        Vector2 {
                            X: bar.right - width,
                            Y: bar.top - 15.0,
                        },
                        &layout,
                        &brush,
                        D2D1_DRAW_TEXT_OPTIONS_NONE,
                    );
                }
            }
        }
    }

    fn paint_lyrics(
        &self,
        rt: &ID2D1RenderTarget,
        state: &MiniState,
        lyrics: Option<&OverlayLyrics>,
        ui: &Layout,
        position_ms: u64,
    ) {
        if !state.show_lyrics {
            return;
        }
        let area = ui.lyric;
        if area.right - area.left <= 8.0 || area.bottom - area.top <= 8.0 {
            return;
        }
        // Manual window heights can be shorter than the lyric content; without
        // this clip the lyric/translation would paint over the time labels and
        // transport buttons below.
        let clip = D2D_RECT_F {
            left: area.left,
            top: area.top,
            right: area.right,
            bottom: area.bottom,
        };
        unsafe {
            let _ = rt.PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        }
        let width = area.right - area.left;
        let size = state.lyrics_font_size.clamp(8.0, 64.0)
            * if state.lyrics_bold { 1.03 } else { 1.0 };
        let effective = effective_position(position_ms, state.offset_ms);
        let Some(lyrics) = lyrics.filter(|lyrics| !lyrics.lines.is_empty()) else {
            let status = if state.lyrics_loading {
                Some(state.labels.loading.as_str())
            } else if state.title.is_empty() {
                None
            } else {
                Some(state.labels.no_lyrics.as_str())
            };
            if let Some(status) = status
                && let Ok(brush) = self.solid(rt, state.colors.text_faint, 1.0)
                && let Ok(format) = self.format(size, false, DWRITE_TEXT_ALIGNMENT_CENTER)
                && let Ok(layout) = self.layout_with(&format, status, width)
            {
                let h = metrics(&layout).height.max(10.0);
                let y = area.top + ((area.bottom - area.top - h) * 0.5).max(0.0);
                unsafe {
                    let _ = rt.DrawTextLayout(
                        Vector2 { X: area.left, Y: y },
                        &layout,
                        &brush,
                        D2D1_DRAW_TEXT_OPTIONS_NONE,
                    );
                }
            }
            unsafe {
                rt.PopAxisAlignedClip();
            }
            return;
        };

        let active = index_at(lyrics, effective);
        let start = active
            .unwrap_or(0)
            .min(lyrics.lines.len().saturating_sub(1));
        let count = state.lyrics_lines.clamp(1, 2) as usize;
        let end = (start + count).min(lyrics.lines.len());
        let line_gap = 4.0;

        struct Row {
            layout: IDWriteTextLayout,
            text_width: f32,
            height: f32,
            fill: Option<KaraokeProgress>,
            line: OverlayLine,
            translation: Option<IDWriteTextLayout>,
            translation_height: f32,
            current: bool,
        }

        let mut rows = Vec::with_capacity(end - start);
        for index in start..end {
            let line = &lyrics.lines[index];
            let current = active == Some(index);
            let karaoke = current
                && lyrics.timed
                && state.karaoke
                && (!state.karaoke_words_only || !line.words.is_empty());
            let Ok(format) = self.format_lyric(size, state.lyrics_bold) else {
                continue;
            };
            // A generous width lets the layout report the full text width even
            // though only the lyric area is visible.
            let Ok(layout) = self.layout_with(&format, &line.text, 10_000.0) else {
                continue;
            };
            let text_metrics = metrics(&layout);
            let text_width = text_metrics.width;
            let height = text_metrics.height.max(size);
            let fill = if karaoke {
                let line_end = line.end_ms.unwrap_or_else(|| {
                    lyrics
                        .lines
                        .get(index + 1)
                        .map(|next| next.start_ms)
                        .unwrap_or(line.start_ms + 5000)
                });
                Some(karaoke_progress(line, effective, line_end))
            } else {
                None
            };
            let translation = state
                .show_translation
                .then_some(line.translation.as_deref())
                .flatten()
                .filter(|text| !text.trim().is_empty())
                .and_then(|text| {
                    let size = (size * 0.66).max(9.0);
                    let format = self
                        .format(size, false, DWRITE_TEXT_ALIGNMENT_LEADING)
                        .ok()?;
                    self.layout_with(&format, text, width).ok()
                });
            let translation_height = translation
                .as_ref()
                .map(|layout| metrics(layout).height)
                .unwrap_or(0.0);

            // Paint immediately; rows only need the prepared layouts for
            // vertical centering, so compute heights first in a pass below.
            rows.push(Row {
                layout,
                text_width,
                height,
                fill,
                line: line.clone(),
                translation,
                translation_height,
                current,
            });
        }

        let mut total = 0.0;
        for (index, row) in rows.iter().enumerate() {
            if index > 0 {
                total += line_gap;
            }
            total += row.height;
            if row.translation.is_some() {
                total += 1.0 + row.translation_height;
            }
        }
        let mut y = area.top + ((area.bottom - area.top - total) * 0.5).max(0.0);
        for (index, row) in rows.iter().enumerate() {
            if index > 0 {
                y += line_gap;
            }
            let (base, fill_color) = if row.current {
                (state.colors.text_weak, state.colors.accent)
            } else {
                (state.colors.text_faint, state.colors.accent)
            };
            let base_color = if row.current && !row.fill.is_some() {
                state.colors.text
            } else {
                base
            };
            // Follow the karaoke edge on long lines instead of truncating the
            // tail: once the sung position reaches the right edge, scroll the
            // single-row text left so the next characters become visible.
            let scroll = match &row.fill {
                Some(progress) if row.text_width > width => {
                    let edge = karaoke_edge_x(&row.layout, progress, &row.line.text);
                    (edge + 20.0 - width).clamp(0.0, row.text_width - width)
                }
                _ => 0.0,
            };
            let x = area.left - scroll;
            if let Ok(brush) = self.solid(rt, base_color, 1.0) {
                unsafe {
                    let _ = rt.DrawTextLayout(
                        Vector2 { X: x, Y: y },
                        &row.layout,
                        &brush,
                        D2D1_DRAW_TEXT_OPTIONS_NONE,
                    );
                }
            }
            if let Some(progress) = &row.fill
                && let Ok(brush) = self.solid(rt, fill_color, 1.0)
            {
                draw_karaoke(
                    rt,
                    &row.layout,
                    &brush,
                    progress,
                    &row.line.text,
                    x,
                    y,
                    width,
                );
            }
            y += row.height;
            if let Some(translation) = &row.translation {
                y += 1.0;
                let rgba = if row.current {
                    state.colors.text_weak
                } else {
                    state.colors.text_faint
                };
                if let Ok(brush) = self.solid(rt, rgba, 1.0) {
                    unsafe {
                        let _ = rt.DrawTextLayout(
                            Vector2 { X: area.left, Y: y },
                            translation,
                            &brush,
                            D2D1_DRAW_TEXT_OPTIONS_NONE,
                        );
                    }
                }
                y += row.translation_height;
            }
        }
        unsafe {
            rt.PopAxisAlignedClip();
        }
    }

    fn text(
        &self,
        rt: &ID2D1RenderTarget,
        text: &str,
        size: f32,
        bold: bool,
        brush: &ID2D1SolidColorBrush,
        x: f32,
        y: f32,
        width: f32,
    ) {
        let Ok(format) = self.format(size, bold, DWRITE_TEXT_ALIGNMENT_LEADING) else {
            return;
        };
        let Ok(layout) = self.layout_with(&format, text, width) else {
            return;
        };
        unsafe {
            let _ = rt.DrawTextLayout(
                Vector2 { X: x, Y: y },
                &layout,
                brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
        }
    }

    fn icon(
        &self,
        rt: &ID2D1RenderTarget,
        glyph: char,
        size: f32,
        brush: &ID2D1SolidColorBrush,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    ) {
        let Ok(format) = self.icon_format(size) else {
            return;
        };
        let text = glyph.to_string();
        let Ok(layout) = self.layout_with(&format, &text, width) else {
            return;
        };
        let metrics = metrics(&layout);
        let y = y + ((height - metrics.height) * 0.5).max(0.0);
        unsafe {
            let _ = rt.DrawTextLayout(
                Vector2 { X: x, Y: y },
                &layout,
                brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
        }
    }

    fn layout_with(
        &self,
        format: &IDWriteTextFormat,
        text: &str,
        width: f32,
    ) -> windows::core::Result<IDWriteTextLayout> {
        let wide: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            self.dwrite
                .CreateTextLayout(&wide, format, width.max(1.0), MAX_LAYOUT_HEIGHT)
        }
    }

    /// Single-row lyric layout without ellipsis trimming. Long text keeps its
    /// full width so the caller can scroll it under the karaoke edge.
    fn format_lyric(
        &self,
        size: f32,
        bold: bool,
    ) -> windows::core::Result<IDWriteTextFormat> {
        let format = self.format(size, bold, DWRITE_TEXT_ALIGNMENT_LEADING)?;
        let trimming = DWRITE_TRIMMING::default();
        unsafe {
            let _ = format.SetTrimming(&trimming, None);
        }
        Ok(format)
    }

    fn format(
        &self,
        size: f32,
        bold: bool,
        align: DWRITE_TEXT_ALIGNMENT,
    ) -> windows::core::Result<IDWriteTextFormat> {
        let family = wide(
            self.font_family
                .as_deref()
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
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size.max(1.0),
                PCWSTR(locale.as_ptr()),
            )
        }?;
        unsafe {
            let _ = format.SetTextAlignment(align);
            let _ = format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR);
            let _ = format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
            let trimming = DWRITE_TRIMMING {
                granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                delimiter: 0,
                delimiterCount: 0,
            };
            if let Ok(sign) = self.dwrite.CreateEllipsisTrimmingSign(&format) {
                let _ = format.SetTrimming(&trimming, &sign);
            }
        }
        Ok(format)
    }

    fn icon_format(&self, size: f32) -> windows::core::Result<IDWriteTextFormat> {
        let family = wide(ICON_FAMILY);
        let locale = wide("en-US");
        let format = unsafe {
            self.dwrite.CreateTextFormat(
                PCWSTR(family.as_ptr()),
                None,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size.max(1.0),
                PCWSTR(locale.as_ptr()),
            )
        }?;
        unsafe {
            let _ = format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
            let _ = format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR);
            let _ = format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
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
    ) -> Option<windows::Win32::Graphics::Direct2D::ID2D1Bitmap> {
        if let Some(bitmap) = &self.cover_bitmap {
            return Some(bitmap.clone());
        }
        let image = self.cover.as_ref()?;
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
        self.cover_bitmap = Some(bitmap.clone());
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
}

#[derive(Clone, Copy, Debug)]
struct KaraokeProgress {
    sung_chars: usize,
    next: bool,
    fraction: f32,
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

fn karaoke_progress(line: &OverlayLine, position_ms: u64, line_end_ms: u64) -> KaraokeProgress {    let total = line.text.chars().count();
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
        None => return,
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
    let mut lines = vec![DWRITE_LINE_METRICS::default(); 16];
    let mut actual = 0u32;
    unsafe { layout.GetLineMetrics(Some(&mut lines), &mut actual) }.ok()?;
    lines.truncate((actual as usize).min(lines.len()));
    Some(lines)
}

fn utf16_len(text: &str, chars: usize) -> u32 {
    text.chars()
        .take(chars)
        .map(char::len_utf16)
        .sum::<usize>() as u32
}

fn metrics(
    layout: &IDWriteTextLayout,
) -> windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_METRICS {
    let mut metrics = windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_METRICS::default();
    unsafe {
        let _ = layout.GetMetrics(&mut metrics);
    }
    metrics
}

fn fit_cover(size: D2D_SIZE_F, width: f32, height: f32) -> D2D_RECT_F {
    if size.width <= 0.0 || size.height <= 0.0 {
        return rect_f(0.0, 0.0, width, height);
    }
    let source_ratio = size.width / size.height;
    let target_ratio = width / height.max(1.0);
    if source_ratio > target_ratio {
        let scale = height / size.height;
        let draw_w = size.width * scale;
        let left = (width - draw_w) * 0.5;
        rect_f(left, 0.0, left + draw_w, height)
    } else {
        let scale = width / size.width;
        let draw_h = size.height * scale;
        let top = (height - draw_h) * 0.5;
        rect_f(0.0, top, width, top + draw_h)
    }
}

fn rect_f(left: f32, top: f32, right: f32, bottom: f32) -> D2D_RECT_F {
    D2D_RECT_F {
        left,
        top,
        right,
        bottom,
    }
}

fn d2d_ellipse(
    x: f32,
    y: f32,
    rx: f32,
    ry: f32,
) -> windows::Win32::Graphics::Direct2D::D2D1_ELLIPSE {
    windows::Win32::Graphics::Direct2D::D2D1_ELLIPSE {
        point: Vector2 { X: x, Y: y },
        radiusX: rx,
        radiusY: ry,
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

pub fn index_at(lyrics: &OverlayLyrics, position_ms: u64) -> Option<usize> {
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

fn effective_position(position_ms: u64, user_offset_ms: i64) -> u64 {
    let position = position_ms as i64;
    if user_offset_ms >= 0 {
        (position - user_offset_ms).max(0) as u64
    } else {
        (position + -user_offset_ms).max(0) as u64
    }
}

fn format_time(seconds: u64) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

fn pixel_rect(region: BoxRect, scale: f32, width: u32, height: u32) -> RECT {
    let left = (region.left * scale).floor() as i32 - 1;
    let top = (region.top * scale).floor() as i32 - 1;
    let right = (region.right * scale).ceil() as i32 + 1;
    let bottom = (region.bottom * scale).ceil() as i32 + 1;
    RECT {
        left: left.max(0),
        top: top.max(0),
        right: right.min(width as i32).max(0),
        bottom: bottom.min(height as i32).max(0),
    }
}

/// Publish the DIB. A dirty rect uses `UpdateLayeredWindowIndirect`, which
/// only invalidates that part of the layered surface; the full call is kept
/// as a fallback for drivers that reject partial updates.
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
        if let Err(err) = UpdateLayeredWindow(
            hwnd,
            None,
            None,
            Some(&size),
            Some(mem_dc),
            Some(&src),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        ) {
            tracing::warn!("native mini player: UpdateLayeredWindow failed: {err}");
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_keeps_controls_inside_a_small_window() {
        let ui = layout(280.0, 120.0);
        for area in [
            ui.close, ui.mute, ui.prev, ui.play, ui.next, ui.progress,
        ] {
            assert!(area.left >= 0.0, "control starts before the window");
            assert!(area.right <= 280.0, "control ends past the window");
            assert!(area.top >= 0.0, "control starts above the window");
            assert!(area.bottom <= 120.0, "control ends below the window");
        }
        assert!(ui.prev.right < ui.play.left);
        assert!(ui.play.right < ui.next.left);
        assert!(ui.progress.right > ui.progress.left);
        assert!(ui.lyric.bottom <= ui.progress.top);
        assert!(ui.cover.right < ui.text_left);
    }

    #[test]
    fn layout_scales_with_the_window() {
        let small = layout(380.0, 170.0);
        let large = layout(760.0, 340.0);
        assert!(large.next.right > small.next.right);
        assert!(large.lyric.right > small.lyric.right);
        assert!(large.progress_bar.right > small.progress_bar.right);
    }
}
