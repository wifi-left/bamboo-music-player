//! GDI painter for all non-KTV UI.
//!
//! Backgrounds, sidebar, virtual list, player bar, progress bar, time labels
//! and the next-line lyric hint are drawn with GDI into the `BeginPaint` DC.
//! The KTV line itself lives in a separate layered child window
//! (`ktv_overlay.rs`) because Direct2D cannot safely share this GDI surface.
//!
//! `BeginPaint` clips GDI to the update region, and the app hands over the
//! exact dirty rectangles so untouched elements are skipped.

use std::collections::HashMap;

use rayon::prelude::*;

use windows::Win32::Foundation::{COLORREF, HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateFontW,
    CreateSolidBrush, DIB_RGB_COLORS, DEFAULT_CHARSET, DRAW_TEXT_FORMAT, DT_CENTER,
    DT_END_ELLIPSIS, DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE, DT_VCENTER, BitBlt,
    CreateCompatibleBitmap, CreateCompatibleDC, CreatePen, DeleteDC, DeleteObject, DrawTextW,
    Ellipse, FillRect, GRADIENT_FILL_RECT_V, GRADIENT_RECT, GetStockObject, GradientFill, HALFTONE,
    HBRUSH,
    HBITMAP, HDC, HFONT, HGDIOBJ, IntersectClipRect, LineTo, MoveToEx, NULL_BRUSH, NULL_PEN,
    OUT_DEFAULT_PRECIS, PS_DOT, RestoreDC,
    SRCCOPY, SaveDC, SelectObject, SetBkColor, SetBkMode, SetStretchBltMode,
    SetTextColor,
    StretchDIBits, TRANSPARENT, TRIVERTEX,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;
use windows::core::{PCWSTR, Result};

use crate::app::{App, Page};
use crate::core::util::image_util::RgbaImage;
use crate::layout::{self, Rect};
use crate::theme::Palette;
use crate::ui::{ButtonState, DownloadAction, Input, Target};

// GDI colors (COLORREF is 0x00BBGGRR).

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum FontKind {
    Title = 0,
    NowTitle = 1,
    Body = 2,
    Small = 3,
    Glyph = 5,
    GlyphLarge = 6,
    /// Standalone lyrics window: brand, cover placeholder, track text, icons.
    Brand = 7,
    CoverGlyph = 8,
    ScreenTitle = 9,
    ScreenBody = 10,
    ScreenSmall = 11,
    GlyphHuge = 12,
}

pub struct Painter {
    brushes: HashMap<u32, HBRUSH>,
    fonts: HashMap<u8, HFONT>,
    fonts_dpi: f32,
    /// UI face/base size from `theme.font_family` / `theme.font_size`.
    /// Changing either clears the font cache once.
    font_family: Option<String>,
    font_scale: f32,
    /// Converted BGRA pixels for decoded covers, keyed by the image address.
    /// A cover is decoded once and reused for every paint until the cache is
    /// trimmed, which keeps per-row thumbnails off the conversion path.
    images: HashMap<usize, CoverPixels>,
    /// Persistent double buffer: all GDI drawing goes here and only the dirty
    /// bounding box is blitted to the window, so partial repaints never show a
    /// half-drawn frame.
    back_dc: Option<HDC>,
    back_bitmap: Option<HBITMAP>,
    back_old: Option<HGDIOBJ>,
    back_size: (i32, i32),
    /// Colors for the current theme. Refreshed at the start of every paint;
    /// brushes are keyed by the resolved COLORREF so a theme switch only
    /// inserts the new brushes it actually uses.
    pal: Palette,
    /// Small pre-blurred backdrop derived from the current cover. Rebuilt only
    /// when the cover or light/dark choice changes, never per frame.
    background: Option<BackgroundPixels>,
    background_key: (usize, bool, [u8; 3], i32, i32, u8),
}

struct BackgroundPixels {
    bgra: Vec<u8>,
    width: i32,
    height: i32,
}

struct CoverPixels {
    bgra: Vec<u8>,
    width: i32,
    height: i32,
}

impl Painter {
    pub fn new() -> Result<Self> {
        Ok(Self {
            brushes: HashMap::new(),
            fonts: HashMap::new(),
            fonts_dpi: 0.0,
            font_family: None,
            font_scale: 1.0,
            images: HashMap::new(),
            back_dc: None,
            back_bitmap: None,
            back_old: None,
            back_size: (0, 0),
            pal: crate::theme::Palette::new(
                &crate::config::ThemeSettings::default(),
                true,
            ),
            background: None,
            background_key: (0, true, [0, 0, 0], 0, 0, 0),
        })
    }

    /// Install the palette for the next paint. The shell calls this before
    /// owner-draw callbacks, which do not go through [`Painter::paint`].
    pub fn set_palette(&mut self, pal: Palette) {
        self.pal = pal;
    }

    pub fn resize(&mut self, _width: u32, _height: u32, _dpi: f32) {}

    pub fn dpi_changed(&mut self) {
        self.clear_fonts();
    }

    fn clear_fonts(&mut self) {
        for (_, font) in self.fonts.drain() {
            unsafe {
                let _ = DeleteObject(HGDIOBJ(font.0));
            }
        }
    }

    pub fn paint(&mut self, hwnd: HWND, hdc: HDC, app: &App, rects: Vec<Rect>) -> Result<()> {
        self.paint_layers(hwnd, hdc, &rects, |this, back, dirty, scale, dpi| {
            this.draw_gdi(back, app, dirty, scale, dpi);
        })
    }

    /// Generic double-buffered paint pass: `draw` receives the back-buffer DC,
    /// the merged dirty rectangles and the DPI scale. Only the dirty bounding
    /// box is blitted, exactly like [`Painter::paint`]. Used by the standalone
    /// lyrics window, which has its own layout.
    pub fn paint_layers<F>(
        &mut self,
        hwnd: HWND,
        hdc: HDC,
        rects: &[Rect],
        mut draw: F,
    ) -> Result<()>
    where
        F: FnMut(&mut Self, HDC, &[Rect], f32, f32),
    {
        let mut client = RECT::default();
        let _ = unsafe { GetClientRect(hwnd, &mut client) };
        let width = (client.right - client.left).max(0) as u32;
        let height = (client.bottom - client.top).max(0) as u32;
        if width == 0 || height == 0 {
            return Ok(());
        }
        let dpi = unsafe { GetDpiForWindow(hwnd) } as f32;
        let dpi = if dpi > 0.0 { dpi } else { 96.0 };
        let scale = dpi / 96.0;
        let full = Rect::new(0.0, 0.0, width as f32 / scale, height as f32 / scale);
        let mut dirty = merge_rects(rects.to_vec(), full);
        if dirty.is_empty() {
            dirty.push(full);
        }

        // Union of the dirty rectangles in device pixels; only this box is
        // blitted back to the window.
        let mut bounds: Option<(i32, i32, i32, i32)> = None;
        for rect in &dirty {
            let left = (rect.left * scale).floor() as i32;
            let top = (rect.top * scale).floor() as i32;
            let right = (rect.right * scale).ceil() as i32;
            let bottom = (rect.bottom * scale).ceil() as i32;
            let left = left.max(0);
            let top = top.max(0);
            let right = right.min(width as i32);
            let bottom = bottom.min(height as i32);
            if right <= left || bottom <= top {
                continue;
            }
            bounds = Some(match bounds {
                Some((bl, bt, br, bb)) => {
                    (bl.min(left), bt.min(top), br.max(right), bb.max(bottom))
                }
                None => (left, top, right, bottom),
            });
        }
        let Some((left, top, right, bottom)) = bounds else {
            return Ok(());
        };

        self.ensure_back_buffer(hdc, width as i32, height as i32);
        let Some(back) = self.back_dc else {
            return Ok(());
        };

        let saved = unsafe { SaveDC(back) };
        unsafe {
            SetBkMode(back, TRANSPARENT);
        }
        draw(self, back, &dirty, scale, dpi);
        unsafe {
            let _ = RestoreDC(back, saved);
        }

        unsafe {
            BitBlt(
                hdc,
                left,
                top,
                right - left,
                bottom - top,
                Some(back),
                left,
                top,
                SRCCOPY,
            )?;
        }
        Ok(())
    }

    fn ensure_back_buffer(&mut self, reference: HDC, width: i32, height: i32) {
        if self.back_dc.is_some() && self.back_size == (width, height) {
            return;
        }
        self.release_back_buffer();
        unsafe {
            let dc = CreateCompatibleDC(Some(reference));
            if dc.is_invalid() {
                return;
            }
            let bitmap = CreateCompatibleBitmap(reference, width, height);
            if bitmap.is_invalid() {
                let _ = DeleteDC(dc);
                return;
            }
            let old = SelectObject(dc, HGDIOBJ(bitmap.0));
            self.back_dc = Some(dc);
            self.back_bitmap = Some(bitmap);
            self.back_old = Some(old);
            self.back_size = (width, height);
        }
    }

    fn release_back_buffer(&mut self) {
        unsafe {
            if let Some(dc) = self.back_dc.take() {
                if let Some(old) = self.back_old.take() {
                    let _ = SelectObject(dc, old);
                }
                let _ = DeleteDC(dc);
            }
            if let Some(bitmap) = self.back_bitmap.take() {
                let _ = DeleteObject(HGDIOBJ(bitmap.0));
            }
            self.back_size = (0, 0);
        }
    }

    fn draw_gdi(&mut self, hdc: HDC, app: &App, dirty: &[Rect], scale: f32, dpi: f32) {
        self.pal = app.palette();
        let theme = &app.core.settings.theme;
        let family = theme
            .font_family
            .clone()
            .filter(|name| !name.trim().is_empty());
        let base = (theme.font_size / 13.5).clamp(0.85, 1.4);
        if family != self.font_family || (base - self.font_scale).abs() > 0.001 {
            self.font_family = family;
            self.font_scale = base;
            self.clear_fonts();
        }
        let layout = app.layout;
        self.draw_background(hdc, app, dirty, scale);

        if hits(layout.sidebar, dirty) {
            self.fill(hdc, layout.sidebar, self.pal.sidebar, scale);
        }
        if hits(layout.player, dirty) {
            self.fill(hdc, layout.player, self.pal.player, scale);
            self.fill(
                hdc,
                Rect::new(
                    0.0,
                    layout.player.top,
                    layout.size.0,
                    layout.player.top + 1.0,
                ),
                self.pal.divider,
                scale,
            );
        }

        self.draw_sidebar(hdc, app, dirty, scale, dpi);
        self.draw_header(hdc, app, dirty, scale, dpi);
        match app.page {
            Page::NowPlaying => self.draw_now_playing(hdc, app, dirty, scale, dpi),
            Page::Search => {
                if app.online_wall_active() {
                    self.draw_cover_wall(hdc, app, dirty, scale, dpi);
                    if hits(app.layout.library_pager, dirty) {
                        self.draw_online_pager(hdc, app, scale, dpi);
                    }
                } else if app.row_count() == 0 {
                    self.draw_placeholder(hdc, app, dirty, scale, dpi);
                    // The pager must stay visible on an empty result page.
                    if hits(app.layout.library_pager, dirty) {
                        self.draw_online_pager(hdc, app, scale, dpi);
                    }
                } else {
                    self.draw_list(hdc, app, dirty, scale, dpi);
                }
            }
            Page::Statistics => self.draw_statistics(hdc, app, dirty, scale, dpi),
            Page::Settings => self.draw_settings(hdc, app, dirty, scale, dpi),
            Page::Downloads => self.draw_downloads(hdc, app, dirty, scale, dpi),
            Page::Queue | Page::Library | Page::Folders | Page::Smart | Page::Duplicates
            | Page::Discover => {
                self.draw_list(hdc, app, dirty, scale, dpi)
            }
        }
        if app.page == Page::NowPlaying && app.layout.visualizer.width() > 0.0 {
            self.draw_visualizer(hdc, app, dirty, scale, dpi);
        }
        self.draw_player(hdc, app, dirty, scale, dpi);
        self.draw_toast(hdc, app, dirty, scale, dpi);
        if let Some(area) = app.focus_rect()
            && hits(area, dirty)
        {
            self.focus_ring(hdc, area, scale);
        }
    }

    /// Dotted 1 px focus ring drawn in COPY mode.
    ///
    /// `DrawFocusRect` XORs into the DC. The window paints into a persistent
    /// back buffer and blits only dirty boxes, so an XOR ring toggled pixels
    /// that later partial repaints only half covered; moving the focus with
    /// Tab left dotted fragments behind. A dotted pen is idempotent and
    /// matches the native look closely enough.
    fn focus_ring(&mut self, hdc: HDC, area: Rect, scale: f32) {
        let rect = to_px_rect(area, scale);
        if rect.right - rect.left < 2 || rect.bottom - rect.top < 2 {
            return;
        }
        unsafe {
            let old_brush = SelectObject(hdc, GetStockObject(NULL_BRUSH));
            let pen = CreatePen(PS_DOT, 1, self.pal.text);
            let old_pen = SelectObject(hdc, HGDIOBJ(pen.0));
            let _ = MoveToEx(hdc, rect.left, rect.top, None);
            let _ = LineTo(hdc, rect.right - 1, rect.top);
            let _ = LineTo(hdc, rect.right - 1, rect.bottom - 1);
            let _ = LineTo(hdc, rect.left, rect.bottom - 1);
            let _ = LineTo(hdc, rect.left, rect.top);
            SelectObject(hdc, old_pen);
            let _ = DeleteObject(HGDIOBJ(pen.0));
            SelectObject(hdc, old_brush);
        }
    }

    /// Backdrop for the main window. Only a flat theme color or a dark
    /// blurred cover are supported; both are opaque so list text stays
    /// readable and no compositing is involved.
    fn draw_background(&mut self, hdc: HDC, app: &App, dirty: &[Rect], scale: f32) {
        use crate::core::config::BackgroundMode as Mode;
        let mode = app.background_mode();
        let width_px = (app.layout.size.0 * scale).round().max(1.0) as i32;
        let height_px = (app.layout.size.1 * scale).round().max(1.0) as i32;
        let art = match mode {
            Mode::Image => self.ensure_user_background(app, width_px, height_px),
            Mode::StaticBlur | Mode::GpuBlur => {
                self.ensure_cover_background(app, width_px, height_px)
            }
            _ => false,
        };
        if art {
            self.draw_background_image(hdc, app, dirty, scale);
            return;
        }
        for rect in dirty {
            self.fill(hdc, *rect, self.pal.background, scale);
        }
    }

    /// Build (once per cover/theme) a tiny blurred, darkened copy of the
    /// current cover. 40×24 pixels stretched with HALFTONE is cheap and hides
    /// the low resolution once it sits behind the UI.
    fn ensure_cover_background(&mut self, app: &App, width_px: i32, height_px: i32) -> bool {
        let Some((image, rev)) = app.current_cover() else {
            self.background = None;
            return false;
        };
        let accent = app.core.settings.theme.accent;
        let key = (
            image as *const RgbaImage as usize ^ rev as usize,
            self.pal.dark,
            accent,
            width_px,
            height_px,
            0,
        );
        if self.background.is_some() && self.background_key == key {
            return true;
        }
        let (src_w, src_h) = (image.size[0] as usize, image.size[1] as usize);
        if src_w == 0 || src_h == 0 || image.pixels.len() < src_w * src_h * 4 {
            return false;
        }
        let width = 96usize;
        let height = 54usize;
        let mut small = vec![0u8; width * height * 3];
        for y in 0..height {
            for x in 0..width {
                let sx = x * src_w / width;
                let sy = y * src_h / height;
                let index = (sy * src_w + sx) * 4;
                let out = (y * width + x) * 3;
                small[out] = image.pixels[index];
                small[out + 1] = image.pixels[index + 1];
                small[out + 2] = image.pixels[index + 2];
            }
        }
        // Two box-blur passes, then a strong dark/tint veil.
        let mut buffer = small.clone();
        for _ in 0..2 {
            box_blur_rgb(&small, &mut buffer, width, height);
            small.copy_from_slice(&buffer);
        }
        // Tint/darken the tiny blurred image, then upscale it once to the
        // client pixel size. Painting then blits it 1:1 instead of running a
        // HALFTONE stretch of the whole window on every frame.
        let mut tinted = vec![0u8; width * height * 3];
        for y in 0..height {
            for x in 0..width {
                let index = (y * width + x) * 3;
                let (r, g, b) = (small[index], small[index + 1], small[index + 2]);
                let (r, g, b) = if self.pal.dark {
                    (
                        (r as f32 * 0.34 + accent[0] as f32 * 0.05) as u8,
                        (g as f32 * 0.34 + accent[1] as f32 * 0.05) as u8,
                        (b as f32 * 0.34 + accent[2] as f32 * 0.05) as u8,
                    )
                } else {
                    (
                        (196.0 + r as f32 * 0.18).min(255.0) as u8,
                        (196.0 + g as f32 * 0.18).min(255.0) as u8,
                        (196.0 + b as f32 * 0.18).min(255.0) as u8,
                    )
                };
                // A gentle vertical fade keeps the header darker than the list.
                let fade = 0.82 + 0.18 * (y as f32 / height as f32);
                let out = (y * width + x) * 3;
                tinted[out] = (r as f32 * fade) as u8;
                tinted[out + 1] = (g as f32 * fade) as u8;
                tinted[out + 2] = (b as f32 * fade) as u8;
            }
        }
        let out_w = width_px.max(1) as u32;
        let out_h = height_px.max(1) as u32;
        let src_w = width as u32;
        let src_h = height as u32;
        let step_x = (src_w << 16) / out_w;
        let step_y = (src_h << 16) / out_h;
        let mut bgra = vec![0u8; (out_w * out_h * 4) as usize];
        // Full-window upscale once per cover/size change. It runs on the UI
        // paint thread, so parallel rows keep a 4K client from blocking for
        // seconds in debug builds.
        bgra.par_chunks_mut((out_w * 4) as usize)
            .enumerate()
            .for_each(|(y, row)| {
                let y = y as u32;
                let sy = y.saturating_mul(step_y);
                let y0 = ((sy >> 16) as usize).min(height - 1);
                let y1 = (y0 + 1).min(height - 1);
                let fy = (sy & 0xFFFF) as u64;
                let row0 = y0 * width;
                let row1 = y1 * width;
                for x in 0..out_w {
                    let sx = x.saturating_mul(step_x);
                    let x0 = ((sx >> 16) as usize).min(width - 1);
                    let x1 = (x0 + 1).min(width - 1);
                    let fx = (sx & 0xFFFF) as u64;
                    let w00 = (0x10000 - fx) * (0x10000 - fy);
                    let w10 = fx * (0x10000 - fy);
                    let w01 = (0x10000 - fx) * fy;
                    let w11 = fx * fy;
                    let sample = |channel: usize| -> u8 {
                        let a = tinted[(row0 + x0) * 3 + channel] as u64;
                        let b = tinted[(row0 + x1) * 3 + channel] as u64;
                        let c = tinted[(row1 + x0) * 3 + channel] as u64;
                        let d = tinted[(row1 + x1) * 3 + channel] as u64;
                        ((a * w00 + b * w10 + c * w01 + d * w11) >> 32) as u8
                    };
                    let index = (x * 4) as usize;
                    row[index] = sample(2);
                    row[index + 1] = sample(1);
                    row[index + 2] = sample(0);
                    row[index + 3] = 255;
                }
            });
        self.background = Some(BackgroundPixels {
            bgra,
            width: out_w as i32,
            height: out_h as i32,
        });
        self.background_key = key;
        true
    }

    /// User-selected picture: cover-fit it to the client with a dark veil and
    /// cache the full-size BGRA once. Bilinear sampling runs in parallel rows
    /// so a 4K client is prepared in milliseconds, not seconds.
    fn ensure_user_background(&mut self, app: &App, width_px: i32, height_px: i32) -> bool {
        let Some((image, rev)) = app.background_image() else {
            self.background = None;
            return false;
        };
        let accent = app.core.settings.theme.accent;
        let key = (
            image as *const RgbaImage as usize ^ rev as usize,
            self.pal.dark,
            accent,
            width_px,
            height_px,
            1,
        );
        if self.background.is_some() && self.background_key == key {
            return true;
        }
        let (src_w, src_h) = (image.size[0], image.size[1]);
        if src_w == 0 || src_h == 0 || image.pixels.len() < src_w * src_h * 4 {
            return false;
        }
        let out_w = width_px.max(1) as u32;
        let out_h = height_px.max(1) as u32;
        // Cover fit: scale so the smaller dimension fills, then center-crop.
        let scale = (out_w as f32 / src_w as f32).max(out_h as f32 / src_h as f32);
        let visible_w = out_w as f32 / scale;
        let visible_h = out_h as f32 / scale;
        let offset_x = (src_w as f32 - visible_w) * 0.5;
        let offset_y = (src_h as f32 - visible_h) * 0.5;
        let bg = self.pal.background.0;
        let (bg_r, bg_g, bg_b) = (
            (bg & 0xff) as f32,
            ((bg >> 8) & 0xff) as f32,
            ((bg >> 16) & 0xff) as f32,
        );
        let (dark, accent) = (self.pal.dark, accent);
        let mut bgra = vec![0u8; (out_w * out_h * 4) as usize];
        bgra.par_chunks_mut((out_w * 4) as usize)
            .enumerate()
            .for_each(|(y, row)| {
                let sy = offset_y + (y as f32 + 0.5) / scale - 0.5;
                let sy = sy.clamp(0.0, src_h as f32 - 1.0);
                let y0 = sy.floor() as usize;
                let y1 = (y0 + 1).min(src_h - 1);
                let fy = sy - y0 as f32;
                let row0 = y0 * src_w;
                let row1 = y1 * src_w;
                for x in 0..out_w {
                    let sx = offset_x + (x as f32 + 0.5) / scale - 0.5;
                    let sx = sx.clamp(0.0, src_w as f32 - 1.0);
                    let x0 = sx.floor() as usize;
                    let x1 = (x0 + 1).min(src_w - 1);
                    let fx = sx - x0 as f32;
                    let sample = |channel: usize| -> f32 {
                        let a = image.pixels[(row0 + x0) * 4 + channel] as f32;
                        let b = image.pixels[(row0 + x1) * 4 + channel] as f32;
                        let c = image.pixels[(row1 + x0) * 4 + channel] as f32;
                        let d = image.pixels[(row1 + x1) * 4 + channel] as f32;
                        (a * (1.0 - fx) + b * fx) * (1.0 - fy)
                            + (c * (1.0 - fx) + d * fx) * fy
                    };
                    // A dark veil keeps list text readable over arbitrary art;
                    // the light theme washes it toward the panel color.
                    let (r, g, b) = if dark {
                        (
                            sample(0) * 0.56 + bg_r * 0.28 + accent[0] as f32 * 0.03,
                            sample(1) * 0.56 + bg_g * 0.28 + accent[1] as f32 * 0.03,
                            sample(2) * 0.56 + bg_b * 0.28 + accent[2] as f32 * 0.03,
                        )
                    } else {
                        (
                            sample(0) * 0.42 + bg_r * 0.50,
                            sample(1) * 0.42 + bg_g * 0.50,
                            sample(2) * 0.42 + bg_b * 0.50,
                        )
                    };
                    let index = (x * 4) as usize;
                    row[index] = b.clamp(0.0, 255.0) as u8;
                    row[index + 1] = g.clamp(0.0, 255.0) as u8;
                    row[index + 2] = r.clamp(0.0, 255.0) as u8;
                    row[index + 3] = 255;
                }
            });
        self.background = Some(BackgroundPixels {
            bgra,
            width: out_w as i32,
            height: out_h as i32,
        });
        self.background_key = key;
        true
    }

    fn draw_background_image(&mut self, hdc: HDC, app: &App, dirty: &[Rect], scale: f32) {
        let Some(pixels) = self.background.as_ref() else {
            return;
        };
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: pixels.width,
                biHeight: -pixels.height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let (width, height) = app.layout.size;
        let dest = to_px_rect(Rect::new(0.0, 0.0, width, height), scale);
        for rect in dirty {
            let clip = to_px_rect(*rect, scale);
            unsafe {
                let saved = SaveDC(hdc);
                let _ = IntersectClipRect(hdc, clip.left, clip.top, clip.right, clip.bottom);
                let _ = StretchDIBits(
                    hdc,
                    dest.left,
                    dest.top,
                    dest.right - dest.left,
                    dest.bottom - dest.top,
                    0,
                    0,
                    pixels.width,
                    pixels.height,
                    Some(pixels.bgra.as_ptr() as *const core::ffi::c_void),
                    &info,
                    DIB_RGB_COLORS,
                    SRCCOPY,
                );
                let _ = RestoreDC(hdc, saved);
            }
        }
    }

    fn draw_visualizer(&mut self, hdc: HDC, app: &App, dirty: &[Rect], scale: f32, dpi: f32) {
        let area = app.layout.visualizer;
        if !hits(area.inflate(1.0), dirty) {
            return;
        }
        self.fill(hdc, area, self.pal.panel, scale);
        let inner = area.inset(14.0, 8.0);
        if inner.width() < 8.0 {
            return;
        }
        debug_assert!(dpi > 0.0);
        let mode = app.core.settings.visualizer.mode;
        let bars = &app.visual_bars;
        if bars.is_empty() {
            self.text(
                hdc,
                FontKind::Small,
                "音频可视化 · 播放时显示",
                inner,
                self.pal.muted,
                DT_CENTER.0,
                scale,
                dpi,
            );
            return;
        }
        if mode == crate::core::config::VisualizerMode::Waveform {
            let samples = &app.visual_samples;
            if samples.is_empty() {
                return;
            }
            let mid = (inner.top + inner.bottom) * 0.5;
            // 2048 raw samples would mean thousands of FillRect calls; a few
            // hundred columns are visually identical at this height.
            let points = samples.len().min(inner.width().max(32.0) as usize / 2).max(32);
            let step = inner.width() / points as f32;
            for (index, column) in (0..points).enumerate() {
                let source = index * samples.len() / points;
                let sample = samples[source];
                let x = inner.left + column as f32 * step;
                let amplitude = sample.clamp(-1.0, 1.0) * inner.height() * 0.45;
                let top = (mid - amplitude).min(mid);
                let bottom = (mid - amplitude).max(mid + 1.0);
                self.fill(
                    hdc,
                    Rect::new(x, top, x + step.max(1.0), bottom),
                    self.pal.accent,
                    scale,
                );
            }
            return;
        }
        let gap = 3.0;
        let bar_width = ((inner.width() - gap * (bars.len() - 1) as f32) / bars.len() as f32)
            .max(1.0);
        for (index, level) in bars.iter().enumerate() {
            let height = (inner.height() * level).max(1.5);
            let x = inner.left + index as f32 * (bar_width + gap);
            let top = if mode == crate::core::config::VisualizerMode::Ring {
                let middle = (inner.top + inner.bottom) * 0.5;
                middle - height * 0.5
            } else {
                inner.bottom - height
            };
            let bottom = if mode == crate::core::config::VisualizerMode::Ring {
                top + height
            } else {
                inner.bottom
            };
            self.fill(
                hdc,
                Rect::new(x, top, x + bar_width, bottom),
                self.pal.accent,
                scale,
            );
        }
    }

    fn draw_toast(&mut self, hdc: HDC, app: &App, dirty: &[Rect], scale: f32, dpi: f32) {
        let Some((text, error)) = app.toast_text() else {
            return;
        };
        let area = app.toast_rect();
        if !hits(area.inflate(2.0), dirty) {
            return;
        }
        self.fill(hdc, area, self.pal.card, scale);
        // COLORREF is 0x00BBGGRR.
        let error_color = COLORREF(0x005A5AF0);
        let edge = if error { error_color } else { self.pal.accent };
        self.fill(
            hdc,
            Rect::new(area.left, area.top, area.left + 3.0, area.bottom),
            edge,
            scale,
        );
        self.text(
            hdc,
            FontKind::Small,
            text,
            Rect::new(area.left + 12.0, area.top + 2.0, area.right - 8.0, area.bottom - 2.0),
            if error { edge } else { self.pal.text },
            0,
            scale,
            dpi,
        );
    }

    fn draw_sidebar(&mut self, hdc: HDC, app: &App, dirty: &[Rect], scale: f32, dpi: f32) {
        let layout = app.layout;
        if !hits(layout.sidebar, dirty) {
            return;
        }
        self.text(
            hdc,
            FontKind::Title,
            "Bamboo",
            layout.logo,
            self.pal.text,
            0,
            scale,
            dpi,
        );
        self.text(
            hdc,
            FontKind::Small,
            crate::i18n::tr("音乐播放器"),
            Rect::new(
                layout.logo.left,
                layout.logo.bottom + 2.0,
                layout.logo.right,
                layout.logo.bottom + 20.0,
            ),
            self.pal.muted,
            0,
            scale,
            dpi,
        );

        let viewport = layout.nav_viewport;
        let saved = unsafe { SaveDC(hdc) };
        let clip = to_px_rect(viewport, scale);
        unsafe {
            let _ = IntersectClipRect(hdc, clip.left, clip.top, clip.right, clip.bottom);
        }
        for index in 0..layout.nav.len() {
            let item = app.nav_rect(index);
            if !viewport.intersects(item) || !hits(item.inflate(2.0), dirty) {
                continue;
            }
            let page = Page::ALL[index];
            let selected = page == app.page;
            let drop_target = index == 1 && app.external_drag_highlight();
            if selected || drop_target {
                self.fill(hdc, item, self.pal.nav_active, scale);
                self.fill(
                    hdc,
                    Rect::new(item.left, item.top, item.left + 3.0, item.bottom),
                    self.pal.accent,
                    scale,
                );
            } else if app.ui.pressed(Target::Nav(index)) {
                self.fill(hdc, item, self.pal.nav_pressed, scale);
            } else if app.ui.hovered(Target::Nav(index)) {
                self.fill(hdc, item, self.pal.nav_hover, scale);
            }
            self.text(
                hdc,
                FontKind::Body,
                page.label(),
                item.inset(18.0, 4.0),
                if selected || drop_target {
                    self.pal.text
                } else {
                    self.pal.muted
                },
                0,
                scale,
                dpi,
            );
        }
        unsafe {
            let _ = RestoreDC(hdc, saved);
        }

        let bar = layout.nav_scrollbar;
        let max = app.sidebar_max_scroll();
        if bar.width() > 0.0 && max > 0.5 && hits(bar.inflate(2.0), dirty) {
            self.fill(hdc, bar, self.pal.scroll_track, scale);
            let view = viewport.height();
            let content = layout.nav_content_height;
            let thumb = (view * view / content).max(24.0).min(bar.height());
            let travel = (bar.height() - thumb).max(0.0);
            let ratio = (app.sidebar_scroll() / max).clamp(0.0, 1.0);
            let thumb_top = bar.top + travel * ratio;
            self.fill(
                hdc,
                Rect::new(bar.left, thumb_top, bar.right, thumb_top + thumb),
                if app.ui.pressed(Target::SidebarScrollbar)
                    || app.ui.hovered(Target::SidebarScrollbar)
                {
                    self.pal.scroll_thumb_hover
                } else {
                    self.pal.scroll_thumb
                },
                scale,
            );
        }

        let footer = Rect::new(
            layout.sidebar.left + 20.0,
            layout.sidebar.bottom - 30.0,
            layout.sidebar.right - 12.0,
            layout.sidebar.bottom - 10.0,
        );
        if hits(footer, dirty) {
            self.text(
                hdc,
                FontKind::Small,
                &format!("v{}", env!("CARGO_PKG_VERSION")),
                footer,
                self.pal.muted,
                0,
                scale,
                dpi,
            );
        }
    }

    fn draw_header(&mut self, hdc: HDC, app: &App, dirty: &[Rect], scale: f32, dpi: f32) {
        let layout = app.layout;
        // Toolbars below the header rect are painted here; without this second
        // test, a hover invalidation of one button erased it (the background
        // was repainted, the button itself was skipped).
        let online_toolbar = Rect::new(
            layout.online_search.left - 4.0,
            layout.online_search.top - 4.0,
            layout.header.right + 4.0,
            layout.online_search.bottom + 4.0,
        );
        let queue_toolbar = layout::queue_toolbar(&layout);
        let toolbar_dirty = (app.page == Page::Folders
            && hits(layout::folder_toolbar(&layout), dirty))
            || (app.page == Page::Library && hits(layout::library_controls(&layout), dirty))
            || (app.page == Page::Search && hits(online_toolbar, dirty))
            // The queue filter frame lives outside the header rect; without
            // this, focusing/hovering only the filter repaints the background
            // but skips the frame, leaving a flat black hole.
            || (app.page == Page::Queue && hits(queue_toolbar, dirty));
        if !hits(layout.header, dirty) && !toolbar_dirty {
            return;
        }
        let title = if app.page == Page::Search {
            app.online_header_title()
        } else {
            app.page.label().to_string()
        };
        self.text(
            hdc,
            FontKind::Title,
            &title,
            Rect::new(
                layout.header.left,
                layout.header.top + 10.0,
                layout.header.left + 360.0,
                layout.header.top + 42.0,
            ),
            self.pal.text,
            0,
            scale,
            dpi,
        );
        let subtitle = match app.page {
            Page::Queue => {
                if let Some(hint) = app.external_drag_hint() {
                    hint
                } else if app.queue_filter_text().trim().is_empty() {
                    if app.queue_shows_shuffle() {
                        format!("{} 首歌曲 · 乱序播放", app.queue_len())
                    } else {
                        crate::trf!("{} 首歌曲", app.queue_len())
                    }
                } else {
                    let filtered = format!(
                        "{} / {} 首 · 筛选：{}",
                        app.row_count(),
                        app.queue_len(),
                        app.queue_filter_text()
                    );
                    if app.queue_shows_shuffle() {
                        format!("{filtered} · 乱序")
                    } else {
                        filtered
                    }
                }
            }
            Page::Library => app.library_status(),
            Page::Folders => app.folder_status(),
            Page::NowPlaying => crate::i18n::tr("歌词可滚动，点击歌词可跳转").to_string(),
            Page::Search => app.online_status(),
            Page::Discover => app.discover_status(),
            Page::Statistics => app.statistics_status(),
            Page::Smart => app.smart_status(),
            Page::Duplicates => app.duplicate_status(),
            Page::Downloads => app.downloads_status(),
            Page::Settings => crate::i18n::tr("修改后自动保存").to_string(),
        };
        let subtitle_right = match app.page {
            // The four library actions live in the header's right corner.
            Page::Library => layout.add_folder.left - 12.0,
            Page::Search => layout.header.right,
            Page::Queue => layout.queue_import.left - 12.0,
            Page::Discover => layout.discover_refresh.left - 12.0,
            _ => layout.header.right,
        };
        let subtitle_rect = Rect::new(
            layout.header.left,
            layout.header.top + 38.0,
            subtitle_right,
            layout.header.bottom - 4.0,
        );
        self.text(
            hdc,
            FontKind::Small,
            &subtitle,
            subtitle_rect,
            self.pal.muted,
            0,
            scale,
            dpi,
        );

        let controls = layout::library_controls(&layout);
        if app.page == Page::Smart && hits(layout.sort.inflate(2.0), dirty) {
            let snapshot = app.core.snapshot();
            self.button(
                hdc,
                layout.sort,
                &format!("智能 · {} ▾", snapshot.library_smart_kind.label()),
                scale,
                dpi,
                app.button_state(Target::SmartKind, false),
            );
        }
        if app.page == Page::Duplicates && hits(layout.sort.inflate(2.0), dirty) {
            let snapshot = app.core.snapshot();
            let mode = match snapshot.library_duplicate_mode {
                crate::config::DedupeMode::Metadata => "元数据",
                crate::config::DedupeMode::Path => "路径",
                crate::config::DedupeMode::Fingerprint => "指纹",
            };
            self.button(
                hdc,
                layout.sort,
                &format!("查重 · {mode} ▾"),
                scale,
                dpi,
                app.button_state(Target::DuplicateMode, false),
            );
        }

        if app.page == Page::Search && hits(online_toolbar, dirty) {
            let border = if app.ui.focused(Input::Search) {
                self.pal.accent
            } else {
                self.pal.explicit_border
            };
            self.frame(hdc, layout.online_search, border, self.pal.search_bg, scale);
            let kind_label = if app.online_is_detail() {
                "返回".to_string()
            } else {
                format!("类型 · {} ▾", app.online_kind_label())
            };
            self.button(
                hdc,
                layout.online_kind,
                &kind_label,
                scale,
                dpi,
                app.button_state(
                    if app.online_is_detail() {
                        Target::OnlineBack
                    } else {
                        Target::OnlineKind
                    },
                    false,
                ),
            );
            let query = app.online_query_text();
            if query.trim().starts_with("http://") || query.trim().starts_with("https://") {
                self.button(
                    hdc,
                    layout.online_queue_all,
                    "打开链接",
                    scale,
                    dpi,
                    app.button_state(Target::OnlineQueueAll, false),
                );
            } else if app.online_can_queue_all() {
                self.button(
                    hdc,
                    layout.online_queue_all,
                    "全部入队",
                    scale,
                    dpi,
                    app.button_state(Target::OnlineQueueAll, false),
                );
            } else {
                self.button_disabled(hdc, layout.online_queue_all, "全部入队", scale, dpi);
            }
            self.button(
                hdc,
                layout.online_random,
                if app.online_is_detail() {
                    "加载更多"
                } else {
                    "随机推荐"
                },
                scale,
                dpi,
                app.button_state(Target::OnlineRandom, false),
            );
        }

        if app.page == Page::Library && hits(controls, dirty) {
            // Bare native EDIT sits on top; draw the frame and buttons here.
            let border = if app.ui.focused(Input::Search) {
                self.pal.accent
            } else {
                self.pal.explicit_border
            };
            self.frame(hdc, layout.search, border, self.pal.search_bg, scale);
            let sort = format!("排序 · {} ▾", app.library_sort_label());
            self.button(
                hdc,
                layout.sort,
                &sort,
                scale,
                dpi,
                app.button_state(Target::Sort, app.sort_menu_open),
            );
            self.button(
                hdc,
                layout.add_folder,
                "添加目录",
                scale,
                dpi,
                app.button_state(Target::AddFolder, false),
            );
            self.button(
                hdc,
                layout.rescan,
                if app.library_index_active() {
                    "取消索引"
                } else {
                    "重新扫描"
                },
                scale,
                dpi,
                app.button_state(Target::Rescan, app.library_index_active()),
            );
        }
        if app.page == Page::Library
            && (hits(layout.library_queue_all.inflate(2.0), dirty)
                || hits(layout.library_check_missing.inflate(2.0), dirty)
                || hits(layout.header, dirty))
        {
            if app.library_can_queue_all() {
                self.button(
                    hdc,
                    layout.library_queue_all,
                    "全部入队",
                    scale,
                    dpi,
                    app.button_state(Target::LibraryQueueAll, false),
                );
            } else {
                self.button_disabled(hdc, layout.library_queue_all, "全部入队", scale, dpi);
            }
            if app.library_can_check_missing() {
                self.button(
                    hdc,
                    layout.library_check_missing,
                    "检查缺失",
                    scale,
                    dpi,
                    app.button_state(Target::LibraryCheckMissing, false),
                );
            } else {
                self.button_disabled(
                    hdc,
                    layout.library_check_missing,
                    "检查缺失",
                    scale,
                    dpi,
                );
            }
        }
        let queue_controls = Rect::new(
            layout.queue_import.left - 4.0,
            layout.queue_import.top - 4.0,
            layout.header.right + 4.0,
            layout.queue_import.bottom + 4.0,
        );
        if app.page == Page::Queue && hits(layout.queue_filter.inflate(2.0), dirty) {
            let border = if app.ui.focused(Input::Search) {
                self.pal.accent
            } else {
                self.pal.explicit_border
            };
            self.frame(hdc, layout.queue_filter, border, self.pal.search_bg, scale);
        }
        if app.page == Page::Queue && hits(queue_controls, dirty) {
            self.button(
                hdc,
                layout.queue_import,
                "导入歌单",
                scale,
                dpi,
                app.button_state(Target::QueueImport, false),
            );
            if app.queue_len() == 0 {
                self.button_disabled(hdc, layout.queue_export, "导出歌单", scale, dpi);
                self.button_disabled(hdc, layout.queue_clear, "清空列表", scale, dpi);
            } else {
                self.button(
                    hdc,
                    layout.queue_export,
                    "导出歌单",
                    scale,
                    dpi,
                    app.button_state(Target::QueueExport, false),
                );
                self.button(
                    hdc,
                    layout.queue_clear,
                    "清空列表",
                    scale,
                    dpi,
                    app.button_state(Target::QueueClear, false),
                );
            }
        }
        if app.page == Page::Discover && hits(layout.discover_refresh.inflate(2.0), dirty) {
            self.button(
                hdc,
                layout.discover_refresh,
                "换一批",
                scale,
                dpi,
                app.button_state(Target::DiscoverRefresh, false),
            );
        }
        if app.page == Page::Folders {
            if hits(layout.folder_back.inflate(2.0), dirty) {
                if app.folder_can_go_back() {
                    self.button(
                        hdc,
                        layout.folder_back,
                        "后退",
                        scale,
                        dpi,
                        app.button_state(Target::FolderBack, false),
                    );
                } else {
                    self.button_disabled(hdc, layout.folder_back, "后退", scale, dpi);
                }
            }
            if hits(layout.folder_forward.inflate(2.0), dirty) {
                if app.folder_can_go_forward() {
                    self.button(
                        hdc,
                        layout.folder_forward,
                        "前进",
                        scale,
                        dpi,
                        app.button_state(Target::FolderForward, false),
                    );
                } else {
                    self.button_disabled(hdc, layout.folder_forward, "前进", scale, dpi);
                }
            }
            for (rect, label, target) in [
                (layout.folder_up, "上一级", Target::FolderUp),
                (layout.folder_refresh, "刷新", Target::FolderRefresh),
                (layout.folder_play_all, "播放全部", Target::FolderPlayAll),
                (layout.folder_queue_all, "加入队列", Target::FolderQueueAll),
            ] {
                if hits(rect.inflate(2.0), dirty) {
                    self.button(hdc, rect, label, scale, dpi, app.button_state(target, false));
                }
            }
            if hits(layout.folder_address.inflate(2.0), dirty) {
                let border = if app.ui.focused(Input::FolderPath) {
                    self.pal.accent
                } else {
                    self.pal.explicit_border
                };
                self.frame(hdc, layout.folder_address, border, self.pal.search_bg, scale);
            }
        }
        self.fill(
            hdc,
            Rect::new(
                layout.header.left,
                layout.header.bottom - 1.0,
                layout.header.right,
                layout.header.bottom,
            ),
            self.pal.divider,
            scale,
        );
    }

    fn draw_list(&mut self, hdc: HDC, app: &App, dirty: &[Rect], scale: f32, dpi: f32) {
        let layout = app.layout;
        let area = app.list_area();
        let list_box = if matches!(
            app.page,
            Page::Queue
                | Page::Folders
                | Page::Smart
                | Page::Duplicates
                | Page::Search
                | Page::Discover
        ) {
            Rect::new(
                area.left,
                area.top,
                app.scrollbar_rect().right,
                area.bottom,
            )
        } else {
            area
        };
        let paged = matches!(app.page, Page::Library | Page::Search);
        let folders = app.page == Page::Folders;
        if !hits(list_box, dirty) && !(paged && hits(layout.library_pager, dirty)) {
            return;
        }

        // Clip rows to the list area: a partially visible last row must not
        // paint over the player bar, pager or the header divider.
        let saved = unsafe { SaveDC(hdc) };
        unsafe {
            let _ = IntersectClipRect(
                hdc,
                (area.left * scale).floor() as i32,
                (area.top * scale).floor() as i32,
                (area.right * scale).ceil() as i32,
                (area.bottom * scale).ceil() as i32,
            );
        }
        let (first, last) = app.visible_rows();
        for index in first..last {
            let row = app.row_rect(index);
            if row.bottom <= area.top || row.top >= area.bottom {
                continue;
            }
            if !hits(row, dirty) {
                continue;
            }
            let Some(info) = app.row_info(index) else {
                continue;
            };
            if info.header {
                self.fill(hdc, row, self.pal.panel, scale);
                self.text(
                    hdc,
                    FontKind::Small,
                    info.title,
                    Rect::new(row.left + 10.0, row.top, row.right - 10.0, row.top + 20.0),
                    self.pal.accent_text,
                    0,
                    scale,
                    dpi,
                );
                self.text(
                    hdc,
                    FontKind::Small,
                    info.artist,
                    Rect::new(row.left + 10.0, row.top + 18.0, row.right - 10.0, row.bottom),
                    self.pal.muted,
                    0,
                    scale,
                    dpi,
                );
                continue;
            }
            if folders && app.is_folder_selected(index) {
                self.fill(hdc, row, self.pal.row_selected, scale);
            } else if info.current {
                self.fill(hdc, row, self.pal.row_selected, scale);
            } else if app.ui.pressed(Target::Row(index)) {
                self.fill(hdc, row, self.pal.row_pressed, scale);
            } else if app.ui.hovered(Target::Row(index)) {
                self.fill(hdc, row, self.pal.row_hover, scale);
            }
            let thumb = Rect::new(
                row.left + 8.0,
                row.top + 5.0,
                row.left + 38.0,
                (row.top + 35.0).min(row.bottom - 5.0),
            );
            if hits(thumb, dirty) {
                if folders {
                    let image = if info.artist == "音频" {
                        app.row_cover(index)
                    } else {
                        None
                    };
                    if let Some(image) = image {
                        self.fill(hdc, thumb, self.pal.cover, scale);
                        if !self.cover(hdc, thumb, image, scale) {
                            self.placeholder_thumb(hdc, thumb, scale, dpi);
                        }
                    } else {
                        self.fill(hdc, thumb, self.pal.button_bg, scale);
                        let glyph = if info.artist == "文件夹" {
                            "\u{E8B7}"
                        } else if info.artist == "播放列表" {
                            "\u{E8FD}"
                        } else {
                            "\u{E8D6}"
                        };
                        self.text(
                            hdc,
                            FontKind::Glyph,
                            glyph,
                            thumb,
                            if info.artist == "文件夹" {
                                self.pal.accent_text
                            } else {
                                self.pal.text
                            },
                            DT_CENTER.0,
                            scale,
                            dpi,
                        );
                    }
                } else {
                    self.fill(hdc, thumb, self.pal.cover, scale);
                    if let Some(image) = app.row_cover(index) {
                        if !self.cover(hdc, thumb, image, scale) {
                            self.placeholder_thumb(hdc, thumb, scale, dpi);
                        }
                    } else {
                        self.placeholder_thumb(hdc, thumb, scale, dpi);
                    }
                }
            }
            let text_left = thumb.right + 8.0;
            if folders {
                self.text(
                    hdc,
                    FontKind::Body,
                    info.title,
                    Rect::new(text_left, row.top + 2.0, row.right - 110.0, row.bottom - 2.0),
                    self.pal.text,
                    0,
                    scale,
                    dpi,
                );
                self.text(
                    hdc,
                    FontKind::Small,
                    info.artist,
                    Rect::new(row.right - 104.0, row.top, row.right - 14.0, row.bottom),
                    if info.artist == "文件夹" {
                        self.pal.accent_text
                    } else {
                        self.pal.muted
                    },
                    DT_RIGHT.0,
                    scale,
                    dpi,
                );
            } else if paged {
                // Library rows: title over artist; duration and play count on
                // the right (reference paged-list design).
                self.text(
                    hdc,
                    FontKind::Body,
                    info.title,
                    Rect::new(
                        text_left,
                        row.top + 3.0,
                        row.right - 170.0,
                        row.top + 21.0,
                    ),
                    self.pal.text,
                    0,
                    scale,
                    dpi,
                );
                let artist_text = if info.missing {
                    if info.artist.trim().is_empty() {
                        "文件缺失".to_string()
                    } else {
                        format!("{} · 文件缺失", info.artist.trim())
                    }
                } else {
                    info.artist.to_string()
                };
                self.text(
                    hdc,
                    FontKind::Small,
                    &artist_text,
                    Rect::new(
                        text_left,
                        row.top + 19.0,
                        row.right - 170.0,
                        row.bottom - 2.0,
                    ),
                    self.pal.muted,
                    0,
                    scale,
                    dpi,
                );
                if let Some(duration) = info.duration {
                    self.text(
                        hdc,
                        FontKind::Small,
                        &format_time(duration as f32),
                        Rect::new(row.right - 156.0, row.top, row.right - 92.0, row.bottom),
                        self.pal.muted,
                        DT_RIGHT.0,
                        scale,
                        dpi,
                    );
                }
                if let Some(count) = info.play_count.filter(|count| *count > 0) {
                    self.text(
                        hdc,
                        FontKind::Small,
                        &format!("{count} 次"),
                        Rect::new(row.right - 86.0, row.top, row.right - 14.0, row.bottom),
                        self.pal.accent,
                        DT_RIGHT.0,
                        scale,
                        dpi,
                    );
                }
            } else {
                let title_width = (area.width() * 0.46 - 56.0).max(60.0);
                self.text(
                    hdc,
                    FontKind::Body,
                    info.title,
                    Rect::new(
                        text_left,
                        row.top + 2.0,
                        text_left + title_width,
                        row.bottom - 2.0,
                    ),
                    self.pal.text,
                    0,
                    scale,
                    dpi,
                );
                let artist_left = area.left + area.width() * 0.52;
                self.text(
                    hdc,
                    FontKind::Body,
                    info.artist,
                    Rect::new(
                        artist_left,
                        row.top + 2.0,
                        artist_left + area.width() * 0.26,
                        row.bottom - 2.0,
                    ),
                    self.pal.muted,
                    0,
                    scale,
                    dpi,
                );
                let duration = format_time(info.duration.unwrap_or(0.0) as f32);
                self.text(
                    hdc,
                    FontKind::Body,
                    &duration,
                    Rect::new(
                        row.right - 78.0,
                        row.top + 2.0,
                        row.right - 12.0,
                        row.bottom - 2.0,
                    ),
                    self.pal.muted,
                    DT_RIGHT.0,
                    scale,
                    dpi,
                );
            }
        }
        if let Some(line) = app.queue_drag_indicator()
            && hits(line.inflate(2.0), dirty)
        {
            self.fill(hdc, line, self.pal.accent, scale);
        }
        unsafe {
            let _ = RestoreDC(hdc, saved);
        }

        if matches!(
            app.page,
            Page::Queue | Page::Folders | Page::Smart | Page::Duplicates | Page::Downloads
                | Page::Discover | Page::Search
        ) {
            let bar = app.scrollbar_rect();
            if hits(bar, dirty) {
                self.fill(hdc, bar, self.pal.scroll_track, scale);
                let view_height = app.list_area().height();
                let content_height = app.scroll_content_height();
                if content_height > view_height && content_height > 0.0 {
                    let thumb_height = (view_height * view_height / content_height).max(24.0);
                    let travel = (view_height - thumb_height).max(0.0);
                    let max_scroll = app.max_scroll();
                    let ratio = if max_scroll > 0.0 {
                        (app.scroll / max_scroll).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    let thumb_top = bar.top + travel * ratio;
                    self.fill(
                        hdc,
                        Rect::new(
                            bar.left + 2.0,
                            thumb_top,
                            bar.right - 2.0,
                            thumb_top + thumb_height,
                        ),
                        if app.ui.pressed(Target::Scrollbar) || app.ui.hovered(Target::Scrollbar) {
                            self.pal.scroll_thumb_hover
                        } else {
                            self.pal.scroll_thumb
                        },
                        scale,
                    );
                }
            }
        }
        if paged && hits(layout.library_pager, dirty) {
            if app.page == Page::Library {
                self.draw_library_pager(hdc, app, scale, dpi);
            } else {
                self.draw_online_pager(hdc, app, scale, dpi);
            }
        }
    }

    /// Album/artist detail cover wall: square covers with title/artist below.
    fn draw_cover_wall(&mut self, hdc: HDC, app: &App, dirty: &[Rect], scale: f32, dpi: f32) {
        let area = app.list_area();
        if !hits(area, dirty) {
            return;
        }
        let saved = unsafe { SaveDC(hdc) };
        unsafe {
            let _ = IntersectClipRect(
                hdc,
                (area.left * scale).floor() as i32,
                (area.top * scale).floor() as i32,
                (area.right * scale).ceil() as i32,
                (area.bottom * scale).ceil() as i32,
            );
        }
        let (first, last) = app.wall_visible_range();
        for index in first..last {
            let card = app.row_rect(index);
            if card.bottom + 42.0 <= area.top || card.top >= area.bottom {
                continue;
            }
            if !hits(Rect::new(card.left, card.top, card.right, card.bottom + 40.0), dirty) {
                continue;
            }
            self.fill(hdc, card, self.pal.cover, scale);
            let drawn = app
                .row_cover(index)
                .is_some_and(|image| self.cover(hdc, card, image, scale));
            if !drawn {
                self.fill(hdc, card, self.pal.button_bg, scale);
                self.text(
                    hdc,
                    FontKind::GlyphLarge,
                    "♪",
                    card,
                    self.pal.text_disabled,
                    DT_CENTER.0,
                    scale,
                    dpi,
                );
            }
            let Some(info) = app.row_info(index) else {
                continue;
            };
            self.text(
                hdc,
                FontKind::Body,
                info.title,
                Rect::new(card.left, card.bottom + 2.0, card.right, card.bottom + 21.0),
                self.pal.text,
                DT_END_ELLIPSIS.0,
                scale,
                dpi,
            );
            self.text(
                hdc,
                FontKind::Small,
                info.artist,
                Rect::new(card.left, card.bottom + 20.0, card.right, card.bottom + 38.0),
                self.pal.muted,
                DT_END_ELLIPSIS.0,
                scale,
                dpi,
            );
        }
        unsafe {
            let _ = RestoreDC(hdc, saved);
        }
        // The cover grid skips `draw_list`, so it owns its scrollbar.
        let bar = app.scrollbar_rect();
        if hits(bar, dirty) {
            self.fill(hdc, bar, self.pal.scroll_track, scale);
            let view_height = area.height();
            let content_height = app.scroll_content_height();
            if content_height > view_height && content_height > 0.0 {
                let thumb_height = (view_height * view_height / content_height).max(24.0);
                let travel = (view_height - thumb_height).max(0.0);
                let max_scroll = app.max_scroll();
                let ratio = if max_scroll > 0.0 {
                    (app.scroll / max_scroll).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let thumb_top = bar.top + travel * ratio;
                self.fill(
                    hdc,
                    Rect::new(
                        bar.left + 2.0,
                        thumb_top,
                        bar.right - 2.0,
                        thumb_top + thumb_height,
                    ),
                    if app.ui.pressed(Target::Scrollbar) || app.ui.hovered(Target::Scrollbar) {
                        self.pal.scroll_thumb_hover
                    } else {
                        self.pal.scroll_thumb
                    },
                    scale,
                );
            }
        }
    }

    fn draw_downloads(&mut self, hdc: HDC, app: &App, dirty: &[Rect], scale: f32, dpi: f32) {
        let layout = app.layout;
        if hits(layout.download_clean_part.inflate(2.0), dirty) {
            self.button(
                hdc,
                layout.download_clean_part,
                "清理 .part",
                scale,
                dpi,
                app.button_state(Target::DownloadsCleanPart, false),
            );
        }
        if hits(layout.download_clear_finished.inflate(2.0), dirty) {
            let finished = app
                .core
                .snapshot()
                .downloads
                .iter()
                .filter(|row| !row.state.is_active())
                .count();
            self.button(
                hdc,
                layout.download_clear_finished,
                &format!("清除已完成 ({finished})"),
                scale,
                dpi,
                app.button_state(Target::DownloadsClearFinished, false),
            );
        }

        let area = layout.download_list;
        if !hits(area, dirty) {
            return;
        }
        let snapshot = app.core.snapshot();
        if snapshot.downloads.is_empty() {
            self.text(
                hdc,
                FontKind::Title,
                "暂无下载任务",
                Rect::new(
                    area.left + 24.0,
                    area.top + 40.0,
                    area.right - 24.0,
                    area.top + 78.0,
                ),
                self.pal.text,
                0,
                scale,
                dpi,
            );
            self.text(
                hdc,
                FontKind::Body,
                "在搜索页右键歌曲选择“下载”。可在此重试失败任务、清理 .part，并调整每个任务的音质。",
                Rect::new(
                    area.left + 24.0,
                    area.top + 84.0,
                    area.right - 24.0,
                    area.top + 116.0,
                ),
                self.pal.muted,
                0,
                scale,
                dpi,
            );
            return;
        }

        let saved = unsafe { SaveDC(hdc) };
        unsafe {
            let _ = IntersectClipRect(
                hdc,
                (area.left * scale).floor() as i32,
                (area.top * scale).floor() as i32,
                (area.right * scale).ceil() as i32,
                (area.bottom * scale).ceil() as i32,
            );
        }
        let error_color = COLORREF(0x005A5AF0);
        let (first, last) = app.visible_rows();
        for index in first..last {
            let row = app.row_rect(index);
            if row.bottom <= area.top || row.top >= area.bottom || !hits(row, dirty) {
                continue;
            }
            let Some(entry) = snapshot.downloads.get(index) else {
                continue;
            };
            if app.ui.hovered(Target::Row(index)) {
                self.fill(hdc, row, self.pal.row_hover, scale);
            }
            if app.ui.pressed(Target::Row(index)) {
                self.fill(hdc, row, self.pal.row_pressed, scale);
            }

            let quality_rect = app.download_action_rect(index, DownloadAction::Quality);
            let text_right = (quality_rect.left - 10.0).max(row.left + 120.0);
            self.text(
                hdc,
                FontKind::Body,
                &entry.title,
                Rect::new(row.left + 14.0, row.top + 4.0, text_right, row.top + 22.0),
                self.pal.text,
                DT_END_ELLIPSIS.0,
                scale,
                dpi,
            );
            let progress = match (entry.state.is_active(), entry.total) {
                (true, Some(total)) if total > 0 => {
                    format!("{:.0}%", entry.received as f64 * 100.0 / total as f64)
                }
                (true, _) => format!("{} KB", entry.received / 1024),
                _ => String::new(),
            };
            let mut detail = format!("{} · {}", entry.artist, entry.state.label());
            if !progress.is_empty() {
                detail.push_str(" · ");
                detail.push_str(&progress);
            }
            if let Some(error) = entry.error.as_deref() {
                detail.push_str(" · ");
                detail.push_str(error);
            }
            self.text(
                hdc,
                FontKind::Small,
                &detail,
                Rect::new(
                    row.left + 14.0,
                    row.top + 20.0,
                    text_right,
                    row.bottom - 3.0,
                ),
                if entry.error.is_some() {
                    error_color
                } else if entry.state.is_active() {
                    self.pal.accent_text
                } else {
                    self.pal.muted
                },
                DT_END_ELLIPSIS.0,
                scale,
                dpi,
            );

            // Thin progress strip above the row's bottom edge.
            if entry.state.is_active()
                && let Some(total) = entry.total.filter(|total| *total > 0)
            {
                let track = Rect::new(row.left + 14.0, row.bottom - 2.0, text_right, row.bottom);
                self.fill(hdc, track, self.pal.scroll_track, scale);
                let ratio = (entry.received as f32 / total as f32).clamp(0.0, 1.0);
                let filled = Rect::new(
                    track.left,
                    track.top,
                    track.left + track.width() * ratio,
                    track.bottom,
                );
                self.fill(hdc, filled, self.pal.accent, scale);
            }

            if entry.state.is_active() {
                self.button_disabled(
                    hdc,
                    quality_rect,
                    &app.download_quality_label(index),
                    scale,
                    dpi,
                );
            } else {
                self.button(
                    hdc,
                    quality_rect,
                    &app.download_quality_label(index),
                    scale,
                    dpi,
                    app.button_state(
                        Target::DownloadAction(index, DownloadAction::Quality),
                        false,
                    ),
                );
            }

            let has_path = entry.path.as_ref().is_some_and(|path| path.exists());
            if entry.state.is_active() {
                let cancel =
                    app.download_action_rect(index, DownloadAction::Cancel);
                self.button(
                    hdc,
                    cancel,
                    "取消",
                    scale,
                    dpi,
                    app.button_state(
                        Target::DownloadAction(index, DownloadAction::Cancel),
                        false,
                    ),
                );
            } else {
                if has_path {
                    let open = app.download_action_rect(index, DownloadAction::Open);
                    self.button(
                        hdc,
                        open,
                        "打开",
                        scale,
                        dpi,
                        app.button_state(
                            Target::DownloadAction(index, DownloadAction::Open),
                            false,
                        ),
                    );
                }
                let retry = app.download_action_rect(index, DownloadAction::Retry);
                self.button(
                    hdc,
                    retry,
                    "重试",
                    scale,
                    dpi,
                    app.button_state(
                        Target::DownloadAction(index, DownloadAction::Retry),
                        false,
                    ),
                );
            }
        }
        unsafe {
            let _ = RestoreDC(hdc, saved);
        }
    }

    /// Footer pager for the paged library list: prev / page / next, with the
    /// dynamically computed rows-per-page on the right.
    fn draw_library_pager(&mut self, hdc: HDC, app: &App, scale: f32, dpi: f32) {
        let layout = app.layout;
        let pages = app.library_page_count();
        let page = app.library_page();
        self.fill(
            hdc,
            Rect::new(
                layout.library_pager.left,
                layout.library_pager.top - 8.0,
                layout.library_pager.right,
                layout.library_pager.top - 7.0,
            ),
            self.pal.divider,
            scale,
        );
        if page > 0 {
            self.button(
                hdc,
                layout.library_prev,
                "上一页",
                scale,
                dpi,
                app.button_state(Target::LibraryPrev, false),
            );
        } else {
            self.button_disabled(hdc, layout.library_prev, "上一页", scale, dpi);
        }
        if page + 1 < pages {
            self.button(
                hdc,
                layout.library_next,
                "下一页",
                scale,
                dpi,
                app.button_state(Target::LibraryNext, false),
            );
        } else {
            self.button_disabled(hdc, layout.library_next, "下一页", scale, dpi);
        }
        self.text(
            hdc,
            FontKind::Small,
            &format!("第 {} / {} 页", page + 1, pages),
            layout.library_page_label,
            self.pal.muted,
            DT_CENTER.0,
            scale,
            dpi,
        );
        if layout.library_page_size.width() > 0.0 {
            self.text(
                hdc,
                FontKind::Small,
                &format!("每页 {} 首", app.library_page_size()),
                layout.library_page_size,
                self.pal.muted,
                DT_RIGHT.0,
                scale,
                dpi,
            );
        }
    }

    fn draw_online_pager(&mut self, hdc: HDC, app: &App, scale: f32, dpi: f32) {
        let layout = app.layout;
        let page = app.online_page();
        self.fill(
            hdc,
            Rect::new(
                layout.library_pager.left,
                layout.library_pager.top - 8.0,
                layout.library_pager.right,
                layout.library_pager.top - 7.0,
            ),
            self.pal.divider,
            scale,
        );
        if page > 0 {
            self.button(
                hdc,
                layout.library_prev,
                "上一页",
                scale,
                dpi,
                app.button_state(Target::OnlinePrev, false),
            );
        } else {
            self.button_disabled(hdc, layout.library_prev, "上一页", scale, dpi);
        }
        if app.online_has_more() {
            self.button(
                hdc,
                layout.library_next,
                "下一页",
                scale,
                dpi,
                app.button_state(Target::OnlineNext, false),
            );
        } else {
            self.button_disabled(hdc, layout.library_next, "下一页", scale, dpi);
        }
        self.text(
            hdc,
            FontKind::Small,
            &format!("第 {} 页", page + 1),
            layout.library_page_label,
            self.pal.muted,
            DT_CENTER.0,
            scale,
            dpi,
        );
        if app.online_is_detail() {
            self.button(
                hdc,
                layout.online_wall,
                if app.online_wall_active() { "列表" } else { "封面墙" },
                scale,
                dpi,
                app.button_state(Target::OnlineWall, app.online_wall_active()),
            );
        }
        if layout.library_page_size.width() > 0.0 && !app.online_is_detail() {
            self.text(
                hdc,
                FontKind::Small,
                &format!("本页 {} 项 · 每页 {}", app.row_count(), app.online_page_size()),
                layout.library_page_size,
                self.pal.muted,
                DT_RIGHT.0,
                scale,
                dpi,
            );
        }
    }

    fn draw_now_playing(&mut self, hdc: HDC, app: &App, dirty: &[Rect], scale: f32, dpi: f32) {
        let layout = app.layout;
        let track = app.current_track();
        let title = track
            .map(|track| track.display_title())
            .unwrap_or_else(|| "未播放".to_string());

        if layout.card.width() > 0.0 && hits(layout.card, dirty) {
            self.fill(hdc, layout.card, self.pal.card, scale);
            self.frame(hdc, layout.card, self.pal.divider, self.pal.card, scale);
            if hits(layout.card_cover, dirty) {
                self.fill(hdc, layout.card_cover, self.pal.cover, scale);
                let drawn = app
                    .current_cover()
                    .is_some_and(|(image, _)| self.cover(hdc, layout.card_cover, image, scale));
                if !drawn {
                    self.text(
                        hdc,
                        FontKind::NowTitle,
                        "♪",
                        layout.card_cover,
                        self.pal.muted,
                        DT_CENTER.0,
                        scale,
                        dpi,
                    );
                }
            }
            if hits(layout.card_title, dirty) {
                self.text(
                    hdc,
                    FontKind::NowTitle,
                    &title,
                    layout.card_title,
                    self.pal.text,
                    0,
                    scale,
                    dpi,
                );
            }
            if hits(layout.card_meta, dirty) {
                let subtitle = match track {
                    Some(track) => track.subtitle(),
                    None => "拖放文件或点击“打开文件”开始播放".to_string(),
                };
                self.text(
                    hdc,
                    FontKind::Body,
                    &subtitle,
                    layout.card_meta,
                    self.pal.muted,
                    0,
                    scale,
                    dpi,
                );
            }
            if layout.card_badges.height() > 0.0 && hits(layout.card_badges, dirty) {
                let badges = card_badges(track);
                if !badges.is_empty() {
                    self.text(
                        hdc,
                        FontKind::Small,
                        &badges,
                        layout.card_badges,
                        self.pal.muted,
                        0,
                        scale,
                        dpi,
                    );
                }
            }
            if hits(layout.card_buttons[0].inflate(2.0), dirty) {
                let label = if app.is_playing() { "暂停" } else { "播放" };
                self.button(
                    hdc,
                    layout.card_buttons[0],
                    label,
                    scale,
                    dpi,
                    app.button_state(Target::CardPlay, app.is_playing()),
                );
            }
            if hits(layout.card_buttons[1].inflate(2.0), dirty) {
                self.button(
                    hdc,
                    layout.card_buttons[1],
                    "打开文件",
                    scale,
                    dpi,
                    app.button_state(Target::CardOpen, false),
                );
            }
        }

        if !hits(layout.lyric_panel, dirty) {
            return;
        }
        let lyrics_settings = &app.core.settings.lyrics;
        let border = lyrics_settings
            .panel_border_color
            .map(rgb3_color)
            .unwrap_or(self.pal.divider);
        let solid = lyrics_settings
            .panel_color
            .map(rgb3_color)
            .unwrap_or(self.pal.panel);
        self.fill(hdc, layout.lyric_panel, border, scale);
        let inner = layout.lyric_panel.inset(1.0, 1.0);
        match lyrics_settings.panel_background {
            crate::core::config::LyricsPanelBackground::Theme
            | crate::core::config::LyricsPanelBackground::Solid => {
                self.fill(hdc, inner, solid, scale);
            }
            crate::core::config::LyricsPanelBackground::Gradient => {
                let second = rgb3_color(lyrics_settings.panel_color2);
                self.fill_vertical_gradient(hdc, inner, solid, second, scale);
            }
            crate::core::config::LyricsPanelBackground::Image => {
                let drawn = app
                    .core
                    .snapshot()
                    .lyrics_background
                    .is_some_and(|image| self.fill_image(hdc, inner, image, scale));
                if !drawn {
                    // Missing/unreadable picture: keep the panel readable.
                    self.fill(hdc, inner, solid, scale);
                }
            }
        }
        if hits(layout.lyric_header, dirty) {
            let snapshot = app.core.snapshot();
            let source = if snapshot.lyrics_loading {
                crate::i18n::tr("歌词加载中…").to_string()
            } else if let Some(error) = snapshot.lyrics_error {
                error.to_string()
            } else if let Some(lyrics) = snapshot.lyrics {
                lyrics.source.label().to_string()
            } else {
                crate::i18n::tr("暂无歌词").to_string()
            };
            let source = if app.core.settings.lyrics.show_source {
                source.as_str()
            } else {
                ""
            };
            let label = if source.is_empty() || layout.lyric_header.width() <= 130.0 {
                "歌词".to_string()
            } else {
                format!("歌词 · {source}")
            };
            self.text(
                hdc,
                FontKind::Body,
                &label,
                layout.lyric_header,
                self.pal.text,
                0,
                scale,
                dpi,
            );
        }

        // 选择歌词 | 独立窗口 | 大歌词 | 编辑 | 刷新
        let labels = [
            "选择歌词",
            "独立窗口",
            if app.big_lyrics { "返回" } else { "大歌词" },
            "编辑",
            "刷新",
        ];
        let targets = [
            Some(Target::LyricsPick),
            Some(Target::LyricsWindow),
            Some(Target::LyricsBig),
            Some(Target::LyricsEdit),
            Some(Target::LyricsReload),
        ];
        for (index, label) in labels.iter().enumerate() {
            let area = layout.lyric_buttons[index];
            if !hits(area.inflate(2.0), dirty) {
                continue;
            }
            match targets[index] {
                Some(target) => {
                    let active = match target {
                        Target::LyricsWindow => app.lyrics_window_open,
                        Target::LyricsBig => app.big_lyrics,
                        _ => false,
                    };
                    self.button(
                        hdc,
                        area,
                        label,
                        scale,
                        dpi,
                        app.button_state(target, active),
                    )
                }
                None => self.button_disabled(hdc, area, label, scale, dpi),
            }
        }
    }

    fn draw_settings(&mut self, hdc: HDC, app: &App, dirty: &[Rect], scale: f32, dpi: f32) {
        let layout = app.layout;
        let settings = layout::settings(&layout);
        let area = Rect::new(
            layout.content.left,
            layout.header.bottom,
            layout.content.right,
            layout.player.top,
        );
        if !hits(area, dirty) {
            return;
        }

        // Union every tab: the flow layout wraps to as many rows as needed and
        // the tabs can move between rows/columns after a resize. A partial
        // bounding box made hovers over later tabs repaint the background
        // without redrawing the tab row ("buttons disappear under the mouse").
        let mut tabs_area = settings.tabs[0];
        for tab in settings.tabs.iter().skip(1) {
            tabs_area.left = tabs_area.left.min(tab.left);
            tabs_area.top = tabs_area.top.min(tab.top);
            tabs_area.right = tabs_area.right.max(tab.right);
            tabs_area.bottom = tabs_area.bottom.max(tab.bottom);
        }
        if hits(tabs_area.inflate(2.0), dirty) {
            for (index, tab) in crate::app::SettingsTab::ALL.iter().enumerate() {
                let rect = settings.tabs[index];
                self.button(
                    hdc,
                    rect,
                    tab.label(),
                    scale,
                    dpi,
                    app.button_state(
                        Target::SettingsTab(index),
                        *tab == app.settings_tab,
                    ),
                );
            }
        }
        if app.settings_tab != crate::app::SettingsTab::Library {
            self.draw_setting_rows(hdc, app, dirty, scale, dpi);
            return;
        }

        let rows = app.settings_rows();
        let toggles = [
            settings.scan_toggle,
            settings.watch_toggle,
            settings.follow_toggle,
        ];
        for (index, (label, value)) in rows.iter().enumerate() {
            let label_rect = layout::setting_label(&settings, index);
            if !hits(label_rect, dirty) && !hits(toggles[index], dirty) {
                continue;
            }
            self.text(
                hdc,
                FontKind::Body,
                label,
                label_rect,
                self.pal.text,
                0,
                scale,
                dpi,
            );
            self.button(
                hdc,
                toggles[index],
                value,
                scale,
                dpi,
                app.button_state(Target::SettingToggle(index), value == "开"),
            );
        }

        // Dedupe is a three-way choice, not an on/off toggle: use a
        // segmented control instead of a cycling button.
        let dedupe_label_rect = layout::setting_label(&settings, 3);
        let dedupe_options = layout::dedupe_options(&settings);
        let current_dedupe = crate::app::dedupe_index(app.dedupe_mode());
        if hits(dedupe_label_rect, dirty)
            || dedupe_options.iter().any(|rect| hits(*rect, dirty))
        {
            self.text(
                hdc,
                FontKind::Body,
                "查重方式",
                dedupe_label_rect,
                self.pal.text,
                0,
                scale,
                dpi,
            );
            for (index, mode) in crate::app::DEDUPE_ORDER.iter().enumerate() {
                self.button(
                    hdc,
                    dedupe_options[index],
                    crate::app::dedupe_label(*mode),
                    scale,
                    dpi,
                    app.button_state(Target::SettingDedupeOption(index), index == current_dedupe),
                );
            }
        }

        if hits(settings.ext_label, dirty) {
            self.text(
                hdc,
                FontKind::Body,
                "文件扩展名",
                settings.ext_label,
                self.pal.text,
                0,
                scale,
                dpi,
            );
        }
        if hits(settings.ext_edit.inflate(2.0), dirty) {
            let border = if app.ui.focused(Input::Extensions) {
                self.pal.accent
            } else {
                self.pal.explicit_border
            };
            self.frame(hdc, settings.ext_edit, border, self.pal.search_bg, scale);
        }
        if hits(settings.add_folder, dirty) {
            self.button(
                hdc,
                settings.add_folder,
                "添加目录",
                scale,
                dpi,
                app.button_state(Target::SettingAddFolder, false),
            );
        }

        let folders = app.settings_folders();
        let title = format!("音乐目录 · {}", folders.len());
        let title_rect = Rect::new(
            settings.folders.left,
            settings.folders.top - 26.0,
            settings.folders.right,
            settings.folders.top - 4.0,
        );
        if hits(title_rect, dirty) {
            self.text(
                hdc,
                FontKind::Small,
                &title,
                title_rect,
                self.pal.muted,
                0,
                scale,
                dpi,
            );
        }

        let first = app.settings_folders_scroll();
        let visible = app.settings_folder_rows();
        for offset in 0..visible {
            let Some(path) = folders.get(first + offset) else {
                break;
            };
            let row = layout::folder_row(&settings, offset);
            if !hits(row, dirty) {
                continue;
            }
            self.fill(hdc, row, self.pal.button_bg, scale);
            let text_rect = Rect::new(
                row.left + 8.0,
                row.top + 2.0,
                row.right - 84.0,
                row.bottom - 2.0,
            );
            self.text(
                hdc,
                FontKind::Small,
                &path.display().to_string(),
                text_rect,
                self.pal.muted,
                0,
                scale,
                dpi,
            );
            self.button(
                hdc,
                Rect::new(
                    row.right - 72.0,
                    row.top + 2.0,
                    row.right - 4.0,
                    row.bottom - 2.0,
                ),
                "移除",
                scale,
                dpi,
                app.button_state(Target::FolderRemove(offset), false),
            );
        }

        if folders.is_empty() && hits(title_rect.inflate(4.0), dirty) {
            self.text(
                hdc,
                FontKind::Small,
                "尚未添加目录；添加后会建立 SQLite 索引并自动监听。",
                Rect::new(
                    settings.folders.left,
                    settings.folders.top,
                    settings.folders.right,
                    settings.folders.top + 24.0,
                ),
                self.pal.muted,
                0,
                scale,
                dpi,
            );
        }
    }

    fn draw_setting_rows(
        &mut self,
        hdc: HDC,
        app: &App,
        dirty: &[Rect],
        scale: f32,
        dpi: f32,
    ) {
        let settings = layout::settings(&app.layout);
        let views = app.settings_views();
        let viewport = settings.generic;
        let scroll = app.settings_scroll;
        unsafe {
            SaveDC(hdc);
            IntersectClipRect(
                hdc,
                (viewport.left * scale).round() as i32,
                (viewport.top * scale).round() as i32,
                (viewport.right * scale).round() as i32,
                (viewport.bottom * scale).round() as i32,
            );
        }
        for (index, view) in views.iter().enumerate() {
            let mut row = layout::setting_row(&settings, index);
            row.top -= scroll;
            row.bottom -= scroll;
            if row.bottom <= viewport.top || row.top >= viewport.bottom {
                continue;
            }
            if !hits(row, dirty) {
                continue;
            }
            let label = Rect::new(
                row.left,
                row.top,
                row.left + (settings.label_width - 12.0).max(60.0),
                row.bottom,
            );
            self.text(hdc, FontKind::Body, view.label, label, self.pal.text, 0, scale, dpi);
            let mut control = layout::setting_control(&settings, index);
            control.top -= scroll;
            control.bottom -= scroll;
            match view.kind {
                crate::app::SettingKind::Toggle => {
                    self.button(
                        hdc,
                        control,
                        &view.value,
                        scale,
                        dpi,
                        app.button_state(Target::SettingsToggleRow(index), view.active),
                    );
                }
                crate::app::SettingKind::Choice => {
                    if app.settings_compact_choice() {
                        let label = view
                            .selected
                            .and_then(|selected| view.options.get(selected))
                            .copied()
                            .unwrap_or("");
                        self.button(
                            hdc,
                            control,
                            label,
                            scale,
                            dpi,
                            app.button_state(Target::SettingsChoiceCycle(index), false),
                        );
                        continue;
                    }
                    let segments = layout::segments(control, view.options.len());
                    for (option, (label, rect)) in
                        view.options.iter().zip(segments.into_iter()).enumerate()
                    {
                        self.button(
                            hdc,
                            rect,
                            label,
                            scale,
                            dpi,
                            app.button_state(
                                Target::SettingsChoice(index, option),
                                view.selected == Some(option),
                            ),
                        );
                    }
                }
                crate::app::SettingKind::Step => {
                    let (minus, value, plus) = layout::step_rects(control);
                    self.button(
                        hdc,
                        minus,
                        "−",
                        scale,
                        dpi,
                        app.button_state(Target::SettingsStep(index, -1), false),
                    );
                    self.button(
                        hdc,
                        plus,
                        "+",
                        scale,
                        dpi,
                        app.button_state(Target::SettingsStep(index, 1), false),
                    );
                    self.text(
                        hdc,
                        FontKind::Body,
                        &view.value,
                        value,
                        if view.active { self.pal.accent_text } else { self.pal.muted },
                        DT_CENTER.0,
                        scale,
                        dpi,
                    );
                    if app.is_number_edit_row(index) {
                        // Frame behind the native numeric EDIT.
                        self.frame(
                            hdc,
                            value.inflate(1.0),
                            self.pal.accent,
                            self.pal.search_bg,
                            scale,
                        );
                    }
                }
                crate::app::SettingKind::Action => {
                    self.button(
                        hdc,
                        control,
                        &view.value,
                        scale,
                        dpi,
                        app.button_state(Target::SettingsAction(index), view.active),
                    );
                }
                crate::app::SettingKind::Text => {
                    let editing = app.is_text_edit_row(index);
                    let state = app.button_state(Target::SettingsText(index), false);
                    if editing {
                        // Same treatment as the numeric editor: accent frame
                        // around the native EDIT, no filled button box.
                        self.frame(hdc, control, self.pal.accent, self.pal.search_bg, scale);
                    } else if matches!(
                        state,
                        ButtonState::Hover | ButtonState::HoverActive | ButtonState::Pressed
                    ) {
                        self.frame(hdc, control, self.pal.explicit_border, self.pal.panel, scale);
                    }
                    // The native EDIT covers this text while editing; keeping
                    // it painted avoids a one-frame hole when the editor hides.
                    self.text(
                        hdc,
                        FontKind::Body,
                        &view.value,
                        control.inset(8.0, 0.0),
                        if editing { self.pal.text } else { self.pal.accent_text },
                        0 | DT_VCENTER.0 | DT_END_ELLIPSIS.0,
                        scale,
                        dpi,
                    );
                }
                crate::app::SettingKind::Color => {
                    let Some(field) = app.settings_tab.fields().get(index).copied() else {
                        continue;
                    };
                    let rgba = app.setting_color(field).unwrap_or([70, 74, 80, 255]);
                    let swatch = COLORREF(
                        ((rgba[2] as u32) << 16) | ((rgba[1] as u32) << 8) | rgba[0] as u32,
                    );
                    let state = app.button_state(Target::SettingsColor(index), false);
                    let border = match state {
                        ButtonState::Hover | ButtonState::HoverActive | ButtonState::Pressed => {
                            self.pal.accent
                        }
                        _ => self.pal.explicit_border,
                    };
                    self.frame(hdc, control, border, swatch, scale);
                    let luminance =
                        0.299 * rgba[0] as f32 + 0.587 * rgba[1] as f32 + 0.114 * rgba[2] as f32;
                    let text_color = if luminance > 150.0 {
                        COLORREF(0x0010_1010)
                    } else {
                        self.pal.text
                    };
                    self.text(
                        hdc,
                        FontKind::Small,
                        &view.value,
                        control.inset(6.0, 0.0),
                        text_color,
                        DT_CENTER.0,
                        scale,
                        dpi,
                    );
                }
            }
        }
        unsafe {
            let _ = RestoreDC(hdc, -1);
        }
        self.draw_settings_scrollbar(hdc, app, dirty, scale);
    }

    /// Visible scrollbar for the generic settings rows. The wheel and drag
    /// share `App::settings_scroll`; this is the affordance the wheel lacked.
    fn draw_settings_scrollbar(
        &mut self,
        hdc: HDC,
        app: &App,
        dirty: &[Rect],
        scale: f32,
    ) {
        let settings = layout::settings(&app.layout);
        let bar = settings.scrollbar;
        let max = app.settings_max_scroll();
        if max <= 0.5 || !hits(bar.inflate(2.0), dirty) {
            return;
        }
        self.fill(hdc, bar, self.pal.scroll_track, scale);
        let view = settings.generic.height();
        let content = app.settings_tab.fields().len() as f32 * layout::SETTINGS_ROW_HEIGHT;
        if content <= view || view <= 0.0 {
            return;
        }
        let thumb_height = (view * view / content).max(24.0).min(bar.height());
        let travel = (bar.height() - thumb_height).max(0.0);
        let ratio = (app.settings_scroll / max).clamp(0.0, 1.0);
        let thumb_top = bar.top + travel * ratio;
        self.fill(
            hdc,
            Rect::new(
                bar.left + 2.0,
                thumb_top,
                bar.right - 2.0,
                thumb_top + thumb_height,
            ),
            if app.ui.pressed(Target::SettingsScrollbar)
                || app.ui.hovered(Target::SettingsScrollbar)
            {
                self.pal.scroll_thumb_hover
            } else {
                self.pal.scroll_thumb
            },
            scale,
        );
    }

    fn draw_statistics(&mut self, hdc: HDC, app: &App, dirty: &[Rect], scale: f32, dpi: f32) {
        let layout = app.layout;
        let stats_layout = layout::statistics(&layout);
        let area = Rect::new(
            layout.content.left,
            layout.header.bottom,
            layout.content.right,
            layout.player.top,
        );
        if !hits(area, dirty) {
            return;
        }
        let snapshot = app.core.snapshot();
        if hits(
            Rect::new(
                stats_layout.refresh.left,
                stats_layout.refresh.top,
                stats_layout.reset.right,
                stats_layout.reset.bottom,
            ),
            dirty,
        ) {
            self.button(
                hdc,
                stats_layout.refresh,
                "刷新",
                scale,
                dpi,
                app.button_state(Target::StatsRefresh, false),
            );
            self.button(
                hdc,
                stats_layout.reset,
                "重置统计",
                scale,
                dpi,
                app.button_state(Target::StatsReset, false),
            );
        }

        let stats = snapshot.statistics;
        let values: [(&str, String); 8] = match stats {
            Some(stats) => [
                ("曲目", stats.tracks.to_string()),
                ("歌手", stats.artists.to_string()),
                ("专辑", stats.albums.to_string()),
                (
                    "总时长",
                    if stats.total_secs > 0.0 {
                        crate::core::util::format_time(stats.total_secs)
                    } else {
                        "--".to_string()
                    },
                ),
                ("播放次数", stats.total_plays.to_string()),
                ("无损曲目", stats.lossless.to_string()),
                ("缺失文件", stats.missing.to_string()),
                ("曲库占用", format_bytes(stats.total_bytes)),
            ],
            None => [
                ("曲目", "—".to_string()),
                ("歌手", "—".to_string()),
                ("专辑", "—".to_string()),
                ("总时长", "—".to_string()),
                ("播放次数", "—".to_string()),
                ("无损曲目", "—".to_string()),
                ("缺失文件", "—".to_string()),
                ("曲库占用", "—".to_string()),
            ],
        };
        for (index, (label, value)) in values.iter().enumerate() {
            let card = stats_layout.summary[index];
            if !hits(card.inflate(1.0), dirty) {
                continue;
            }
            self.fill(hdc, card, self.pal.card, scale);
            self.text(
                hdc,
                FontKind::Small,
                label,
                Rect::new(card.left + 12.0, card.top + 4.0, card.right - 8.0, card.top + 22.0),
                self.pal.muted,
                0,
                scale,
                dpi,
            );
            self.text(
                hdc,
                FontKind::Body,
                value,
                Rect::new(card.left + 12.0, card.top + 20.0, card.right - 8.0, card.bottom - 2.0),
                self.pal.text,
                0,
                scale,
                dpi,
            );
        }

        // Recently played (clickable: plays the row).
        if hits(stats_layout.recent_header.inflate(2.0), dirty) {
            self.text(
                hdc,
                FontKind::Body,
                "最近播放",
                stats_layout.recent_header,
                self.pal.text,
                0,
                scale,
                dpi,
            );
        }
        self.fill(hdc, stats_layout.recent, self.pal.panel, scale);
        let recent: &[_] = stats
            .map(|stats| stats.recently_played.as_slice())
            .unwrap_or(&[]);
        if recent.is_empty() {
            self.text(
                hdc,
                FontKind::Small,
                "还没有播放记录",
                stats_layout.recent.inset(12.0, 8.0),
                self.pal.muted,
                0,
                scale,
                dpi,
            );
        }
        let row_height = layout::STATS_ROW_HEIGHT;
        let rows = ((stats_layout.recent.height() - 8.0) / row_height).max(0.0) as usize;
        for (index, entry) in recent.iter().take(rows).enumerate() {
            let top = stats_layout.recent.top + 4.0 + index as f32 * row_height;
            let row = Rect::new(
                stats_layout.recent.left + 4.0,
                top,
                stats_layout.recent.right - 4.0,
                top + row_height - 2.0,
            );
            if !hits(row.inflate(1.0), dirty) {
                continue;
            }
            if app.ui.hovered(Target::StatsRecent(index)) {
                self.fill(hdc, row, self.pal.row_hover, scale);
            }
            let title = if entry.title.trim().is_empty() {
                entry
                    .path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| entry.path.display().to_string())
            } else {
                entry.title.clone()
            };
            self.text(
                hdc,
                FontKind::Small,
                &title,
                Rect::new(row.left + 8.0, row.top, row.right - 70.0, row.top + 18.0),
                self.pal.text,
                0,
                scale,
                dpi,
            );
            let subtitle = if entry.artist.trim().is_empty() {
                entry.format.clone()
            } else {
                entry.artist.clone()
            };
            self.text(
                hdc,
                FontKind::Small,
                &subtitle,
                Rect::new(row.left + 8.0, row.top + 18.0, row.right - 70.0, row.bottom),
                self.pal.muted,
                0,
                scale,
                dpi,
            );
            if let Some(seconds) = entry.duration {
                self.text(
                    hdc,
                    FontKind::Small,
                    &crate::core::util::format_time(seconds),
                    Rect::new(row.right - 64.0, row.top, row.right - 6.0, row.bottom),
                    self.pal.muted,
                    DT_RIGHT.0,
                    scale,
                    dpi,
                );
            }
        }

        // Artist leaderboard.
        if hits(stats_layout.artists_header.inflate(2.0), dirty) {
            self.text(
                hdc,
                FontKind::Body,
                "歌手榜",
                stats_layout.artists_header,
                self.pal.text,
                0,
                scale,
                dpi,
            );
        }
        self.fill(hdc, stats_layout.artists, self.pal.panel, scale);
        let artists: &[_] = stats.map(|stats| stats.top_artists.as_slice()).unwrap_or(&[]);
        if artists.is_empty() {
            self.text(
                hdc,
                FontKind::Small,
                "暂无播放统计",
                stats_layout.artists.inset(12.0, 8.0),
                self.pal.muted,
                0,
                scale,
                dpi,
            );
        }
        let artist_rows = (stats_layout.artists.height() / 24.0).max(0.0) as usize;
        for (index, (artist, plays)) in artists.iter().take(artist_rows).enumerate() {
            let top = stats_layout.artists.top + index as f32 * 24.0;
            let row = Rect::new(
                stats_layout.artists.left + 8.0,
                top,
                stats_layout.artists.right - 8.0,
                top + 24.0,
            );
            if !hits(row, dirty) {
                continue;
            }
            self.text(
                hdc,
                FontKind::Small,
                &format!("{}. {}", index + 1, artist),
                Rect::new(row.left, row.top, row.right - 54.0, row.bottom),
                self.pal.text,
                0,
                scale,
                dpi,
            );
            self.text(
                hdc,
                FontKind::Small,
                &format!("{plays} 次"),
                Rect::new(row.right - 54.0, row.top, row.right, row.bottom),
                self.pal.muted,
                DT_RIGHT.0,
                scale,
                dpi,
            );
        }

        // Track leaderboard.
        if hits(stats_layout.tracks_header.inflate(2.0), dirty) {
            self.text(
                hdc,
                FontKind::Body,
                "曲目榜",
                stats_layout.tracks_header,
                self.pal.text,
                0,
                scale,
                dpi,
            );
        }
        self.fill(hdc, stats_layout.tracks, self.pal.panel, scale);
        let tracks: &[_] = stats.map(|stats| stats.top_tracks.as_slice()).unwrap_or(&[]);
        if tracks.is_empty() {
            self.text(
                hdc,
                FontKind::Small,
                "暂无播放统计",
                stats_layout.tracks.inset(12.0, 8.0),
                self.pal.muted,
                0,
                scale,
                dpi,
            );
        }
        let track_rows = (stats_layout.tracks.height() / 24.0).max(0.0) as usize;
        for (index, (title, artist, plays)) in tracks.iter().take(track_rows).enumerate() {
            let top = stats_layout.tracks.top + index as f32 * 24.0;
            let row = Rect::new(
                stats_layout.tracks.left + 8.0,
                top,
                stats_layout.tracks.right - 8.0,
                top + 24.0,
            );
            if !hits(row, dirty) {
                continue;
            }
            let title = if artist.trim().is_empty() {
                title.clone()
            } else {
                format!("{title} · {artist}")
            };
            self.text(
                hdc,
                FontKind::Small,
                &format!("{}. {title}", index + 1),
                Rect::new(row.left, row.top, row.right - 54.0, row.bottom),
                self.pal.text,
                0,
                scale,
                dpi,
            );
            self.text(
                hdc,
                FontKind::Small,
                &format!("{plays} 次"),
                Rect::new(row.right - 54.0, row.top, row.right, row.bottom),
                self.pal.muted,
                DT_RIGHT.0,
                scale,
                dpi,
            );
        }
    }

    fn draw_placeholder(&mut self, hdc: HDC, app: &App, dirty: &[Rect], scale: f32, dpi: f32) {
        let layout = app.layout;
        let area = Rect::new(
            layout.content.left + 48.0,
            layout.header.bottom + 48.0,
            layout.content.right - 48.0,
            layout.content.bottom - 48.0,
        );
        if !hits(area, dirty) {
            return;
        }
        let (title, hint) = if app.page == Page::Search {
            let snapshot = app.core.snapshot();
            let hint = if let Some(error) = snapshot.online.error() {
                error.to_string()
            } else if snapshot.online.loading() {
                "正在请求在线音源…".to_string()
            } else if snapshot.online.query.trim().is_empty() {
                "在上方输入歌名、歌手或专辑；也可以粘贴 http(s) 链接后回车直接播放。"
                    .to_string()
            } else {
                "没有找到匹配的曲目。换个关键词，或点“随机推荐”。".to_string()
            };
            ("在线搜索", hint)
        } else {
            ("提示", String::new())
        };
        self.text(
            hdc,
            FontKind::Title,
            title,
            Rect::new(area.left, area.top, area.right, area.top + 40.0),
            self.pal.text,
            0,
            scale,
            dpi,
        );
        self.text(
            hdc,
            FontKind::Body,
            &hint,
            Rect::new(area.left, area.top + 48.0, area.right, area.top + 90.0),
            self.pal.muted,
            0,
            scale,
            dpi,
        );
    }

    fn draw_player(&mut self, hdc: HDC, app: &App, dirty: &[Rect], scale: f32, dpi: f32) {
        let layout = app.layout;
        let snapshot = app.core.snapshot();
        let title = app
            .current_track()
            .map(|track| track.display_title())
            .unwrap_or_else(|| "未播放".to_string());
        let artist = app
            .current_track()
            .map(|track| track.display_artist())
            .unwrap_or_default();

        if hits(layout.cover, dirty) {
            self.fill(hdc, layout.cover, self.pal.cover, scale);
            let drawn = app
                .current_cover()
                .is_some_and(|(image, _)| self.cover(hdc, layout.cover, image, scale));
            if !drawn {
                self.text(
                    hdc,
                    FontKind::NowTitle,
                    "♪",
                    layout.cover,
                    self.pal.muted,
                    DT_CENTER.0,
                    scale,
                    dpi,
                );
            }
        }
        if hits(layout.track_text, dirty) {
            self.text(
                hdc,
                FontKind::Body,
                &title,
                Rect::new(
                    layout.track_text.left,
                    layout.track_text.top,
                    layout.track_text.right,
                    layout.track_text.top + 26.0,
                ),
                self.pal.text,
                0,
                scale,
                dpi,
            );
            self.text(
                hdc,
                FontKind::Small,
                &artist,
                Rect::new(
                    layout.track_text.left,
                    layout.track_text.top + 26.0,
                    layout.track_text.right,
                    layout.track_text.top + 46.0,
                ),
                self.pal.muted,
                0,
                scale,
                dpi,
            );
        }

        // Transport: shuffle / prev / play / next / repeat.
        let shuffle_on = snapshot.shuffle;
        self.icon_button(
            hdc,
            layout.shuffle,
            "\u{E8B1}",
            scale,
            dpi,
            app.button_state(Target::Shuffle, shuffle_on),
            shuffle_on,
        );
        self.icon_button(
            hdc,
            layout.prev,
            "\u{E892}",
            scale,
            dpi,
            app.button_state(Target::Prev, false),
            false,
        );
        if hits(layout.play.inflate(6.0), dirty) {
            let state = app.button_state(Target::Play, false);
            let background = match state {
                ButtonState::Pressed => self.pal.button_pressed,
                ButtonState::Hover | ButtonState::HoverActive => self.pal.play_hover,
                _ => self.pal.play_button,
            };
            self.fill(hdc, layout.play, background, scale);
            self.text(
                hdc,
                FontKind::GlyphLarge,
                if app.is_playing() {
                    "\u{E769}"
                } else {
                    "\u{E768}"
                },
                layout.play,
                self.pal.accent_text,
                DT_CENTER.0,
                scale,
                dpi,
            );
        }
        self.icon_button(
            hdc,
            layout.next,
            "\u{E893}",
            scale,
            dpi,
            app.button_state(Target::Next, false),
            false,
        );
        let repeat_on = snapshot.repeat != crate::core::model::RepeatMode::Off;
        self.icon_button(
            hdc,
            layout.repeat,
            if snapshot.repeat == crate::core::model::RepeatMode::One {
                "\u{E8ED}"
            } else {
                "\u{E8EE}"
            },
            scale,
            dpi,
            app.button_state(Target::Repeat, repeat_on),
            repeat_on,
        );

        // Right tools: volume / mute / settings / mini / desktop lyrics / list.
        if layout.volume_slider.width() > 0.0 && hits(layout.volume_hit, dirty) {
            let state = app.button_state(Target::Volume, false);
            let bar = layout.volume_slider;
            let center = bar.top + bar.height() * 0.5;
            let thickness = if state == ButtonState::Normal { 4.0 } else { 6.0 };
            let track = Rect::new(bar.left, center - thickness * 0.5, bar.right, center + thickness * 0.5);
            self.fill(
                hdc,
                track,
                if state == ButtonState::Normal {
                    self.pal.divider
                } else {
                    self.pal.button_hover
                },
                scale,
            );
            let ratio = snapshot.volume.clamp(0.0, 1.0);
            let fill_right = track.left + track.width() * ratio;
            let fill = if snapshot.muted {
                self.pal.muted
            } else if state == ButtonState::Normal {
                self.pal.accent
            } else {
                self.pal.accent_text
            };
            self.fill(
                hdc,
                Rect::new(track.left, track.top, fill_right, track.bottom),
                fill,
                scale,
            );
            let knob = if state == ButtonState::Normal { 6.0 } else { 8.0 };
            self.fill(
                hdc,
                Rect::new(
                    fill_right - knob * 0.5,
                    center - knob * 0.5,
                    fill_right + knob * 0.5,
                    center + knob * 0.5,
                ),
                if snapshot.muted { self.pal.muted } else { self.pal.accent_text },
                scale,
            );
        }
        self.icon_button(
            hdc,
            layout.mute,
            if snapshot.muted { "\u{E74F}" } else { "\u{E767}" },
            scale,
            dpi,
            app.button_state(Target::Mute, snapshot.muted),
            snapshot.muted,
        );
        self.icon_button(
            hdc,
            layout.player_settings,
            "\u{E713}",
            scale,
            dpi,
            app.button_state(Target::PlaySettings, app.player_panel_open),
            app.player_panel_open,
        );
        if layout.mini_player.width() > 0.0 {
            self.icon_button(
                hdc,
                layout.mini_player,
                "\u{E8A7}",
                scale,
                dpi,
                app.button_state(Target::MiniPlayer, app.mini_player_open()),
                app.mini_player_open(),
            );
        }
        if layout.desktop_lyrics.width() > 0.0 && hits(layout.desktop_lyrics.inflate(2.0), dirty) {
            let state = app.button_state(Target::DesktopLyrics, app.desktop_lyrics_open());
            if state != ButtonState::Normal {
                self.fill(
                    hdc,
                    layout.desktop_lyrics,
                    if state == ButtonState::Pressed {
                        self.pal.button_pressed
                    } else {
                        self.pal.button_hover
                    },
                    scale,
                );
            }
            let color = if app.desktop_lyrics_open() {
                self.pal.accent_text
            } else if state == ButtonState::Normal {
                self.pal.muted
            } else {
                self.pal.text
            };
            self.text(
                hdc,
                FontKind::Body,
                "词",
                layout.desktop_lyrics,
                color,
                DT_CENTER.0,
                scale,
                dpi,
            );
        }
        self.icon_button(
            hdc,
            layout.playlist,
            "\u{E8FD}",
            scale,
            dpi,
            app.button_state(Target::Playlist, false),
            false,
        );

        // Progress bar with normal / hover / pressed styling.
        if hits(layout.progress_hit, dirty) {
            let state = app.button_state(Target::Progress, false);
            let center = layout.progress.top + layout.progress.height() * 0.5;
            let (thickness, track, fill, knob_height, knob_color) = match state {
                ButtonState::Hover | ButtonState::HoverActive => {
                    (6.0, self.pal.button_hover, self.pal.accent_text, 14.0, self.pal.text)
                }
                ButtonState::Pressed => (8.0, self.pal.button_hover, self.pal.accent_text, 18.0, self.pal.text),
                ButtonState::Active => (4.0, self.pal.divider, self.pal.accent, 10.0, self.pal.accent_text),
                ButtonState::Normal => (4.0, self.pal.divider, self.pal.accent, 10.0, self.pal.accent_text),
            };
            let bar = Rect::new(
                layout.progress.left,
                center - thickness * 0.5,
                layout.progress.right,
                center + thickness * 0.5,
            );
            self.fill(hdc, bar, track, scale);
            let duration = app.duration().max(0.1);
            let ratio = (app.display_position() / duration).clamp(0.0, 1.0);
            let fill_right = bar.left + bar.width() * ratio;
            self.fill(
                hdc,
                Rect::new(bar.left, bar.top, fill_right, bar.bottom),
                fill,
                scale,
            );
            // A thin vertical playhead instead of a large square knob.
            self.fill(
                hdc,
                Rect::new(
                    fill_right - 1.5,
                    center - knob_height * 0.5,
                    fill_right + 1.5,
                    center + knob_height * 0.5,
                ),
                knob_color,
                scale,
            );
        }

        if hits(layout.time_left, dirty) {
            self.text(
                hdc,
                FontKind::Small,
                &format_time(app.display_position()),
                layout.time_left,
                self.pal.muted,
                0,
                scale,
                dpi,
            );
        }
        if hits(layout.time_right, dirty) {
            self.text(
                hdc,
                FontKind::Small,
                &format_time(app.duration()),
                layout.time_right,
                self.pal.muted,
                DT_RIGHT.0,
                scale,
                dpi,
            );
        }
    }

    fn icon_button(
        &mut self,
        hdc: HDC,
        area: Rect,
        glyph: &str,
        scale: f32,
        dpi: f32,
        state: ButtonState,
        active: bool,
    ) {
        if state != ButtonState::Normal {
            self.fill(
                hdc,
                area,
                if state == ButtonState::Pressed {
                    self.pal.button_pressed
                } else {
                    self.pal.button_hover
                },
                scale,
            );
        }
        let color = match state {
            ButtonState::Normal => {
                if active {
                    self.pal.accent_text
                } else {
                    self.pal.muted
                }
            }
            ButtonState::Pressed | ButtonState::HoverActive => self.pal.accent_text,
            _ => self.pal.text,
        };
        self.text(hdc, FontKind::Glyph, glyph, area, color, DT_CENTER.0, scale, dpi);
    }

    fn placeholder_thumb(&mut self, hdc: HDC, area: Rect, scale: f32, dpi: f32) {
        self.text(
            hdc,
            FontKind::Small,
            "♪",
            area,
            self.pal.text_disabled,
            DT_CENTER.0,
            scale,
            dpi,
        );
    }

    /// Owner-draw one row of the native sort LISTBOX. The shell calls this
    /// from `WM_DRAWITEM`; coordinates arrive in device pixels.
    pub fn draw_sort_item(
        &mut self,
        hdc: HDC,
        index: usize,
        selected: bool,
        hovered: bool,
        rect: RECT,
        dpi: f32,
    ) {
        let scale = dpi / 96.0;
        // Owner-draw DCs default to opaque text background; without this the
        // label gets a white box.
        unsafe {
            SetBkMode(hdc, TRANSPARENT);
            SetBkColor(hdc, self.pal.search_bg);
        }
        let area = Rect::new(
            rect.left as f32 / scale,
            rect.top as f32 / scale,
            rect.right as f32 / scale,
            rect.bottom as f32 / scale,
        );
        let background = if selected {
            self.pal.nav_active
        } else if hovered {
            self.pal.button_hover
        } else {
            self.pal.player
        };
        self.fill(hdc, area, background, scale);
        self.text(
            hdc,
            FontKind::Small,
            crate::app::App::sort_label_at(index),
            area.inset(10.0, 0.0),
            if selected || hovered { self.pal.text } else { self.pal.muted },
            DT_VCENTER.0 | DT_SINGLELINE.0,
            scale,
            dpi,
        );
    }

    fn frame(&mut self, hdc: HDC, area: Rect, border: COLORREF, background: COLORREF, scale: f32) {
        self.fill(hdc, area, border, scale);
        self.fill(hdc, area.inset(1.0, 1.0), background, scale);
    }

    fn button(
        &mut self,
        hdc: HDC,
        area: Rect,
        label: &str,
        scale: f32,
        dpi: f32,
        state: ButtonState,
    ) {
        let background = match state {
            ButtonState::Normal => self.pal.button_bg,
            ButtonState::Hover => self.pal.button_hover,
            ButtonState::Pressed => self.pal.button_pressed,
            ButtonState::Active => self.pal.button_active,
            ButtonState::HoverActive => self.pal.button_active_hover,
        };
        self.fill(hdc, area, background, scale);
        self.text(
            hdc,
            FontKind::Small,
            label,
            area,
            match state {
                ButtonState::Normal => self.pal.muted,
                ButtonState::HoverActive | ButtonState::Pressed => self.pal.accent_text,
                _ => self.pal.text,
            },
            DT_CENTER.0,
            scale,
            dpi,
        );
    }

    fn button_disabled(
        &mut self,
        hdc: HDC,
        area: Rect,
        label: &str,
        scale: f32,
        dpi: f32,
    ) {
        self.fill(hdc, area, self.pal.button_disabled, scale);
        self.text(
            hdc,
            FontKind::Small,
            label,
            area,
            self.pal.text_disabled,
            DT_CENTER.0,
            scale,
            dpi,
        );
    }

    pub(crate) fn fill(&mut self, hdc: HDC, area: Rect, color: COLORREF, scale: f32) {
        if area.width() <= 0.0 || area.height() <= 0.0 {
            return;
        }
        let rect = to_px_rect(area, scale);
        let brush = self.brush(color);
        unsafe {
            FillRect(hdc, &rect, brush);
        }
    }

    /// Return the cached BGRA pixels for a decoded cover, converting once.
    /// Covers arrive from the decode worker as RGBA; the conversion runs on
    /// the paint thread exactly once per image (about 1.6 MB for a 640²
    /// sticker), so keeping it out of `StretchDIBits` saves a full copy per
    /// frame.
    fn cached_cover(&mut self, image: &RgbaImage) -> Option<&CoverPixels> {
        let key = image as *const RgbaImage as usize;
        if !self.images.contains_key(&key) {
            let (w, h) = (image.size[0] as i32, image.size[1] as i32);
            if w <= 0 || h <= 0 || image.pixels.len() < (w * h * 4) as usize {
                return None;
            }
            let mut bgra = vec![0u8; image.pixels.len()];
            for (dst, px) in bgra.chunks_exact_mut(4).zip(image.pixels.chunks_exact(4)) {
                dst[0] = px[2];
                dst[1] = px[1];
                dst[2] = px[0];
                dst[3] = px[3];
            }
            if self.images.len() >= 384 {
                self.images.clear();
            }
            self.images.insert(
                key,
                CoverPixels {
                    bgra,
                    width: w,
                    height: h,
                },
            );
        }
        self.images.get(&key)
    }

    /// Fill a rectangle with an image stretched to fit (lyrics panel
    /// background). The converted BGRA buffer is cached by image pointer.
    pub(crate) fn fill_image(
        &mut self,
        hdc: HDC,
        area: Rect,
        image: &RgbaImage,
        scale: f32,
    ) -> bool {
        if area.width() <= 0.0 || area.height() <= 0.0 {
            return false;
        }
        let Some(pixels) = self.cached_cover(image) else {
            return false;
        };
        let dest = to_px_rect(area, scale);
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: pixels.width,
                biHeight: -pixels.height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        unsafe {
            SetStretchBltMode(hdc, HALFTONE);
            let _ = StretchDIBits(
                hdc,
                dest.left,
                dest.top,
                dest.right - dest.left,
                dest.bottom - dest.top,
                0,
                0,
                pixels.width,
                pixels.height,
                Some(pixels.bgra.as_ptr() as *const core::ffi::c_void),
                &info,
                DIB_RGB_COLORS,
                SRCCOPY,
            );
        }
        true
    }

    /// Vertical two-stop gradient inside `area`.
    fn fill_vertical_gradient(
        &mut self,
        hdc: HDC,
        area: Rect,
        top: COLORREF,
        bottom: COLORREF,
        scale: f32,
    ) {
        if area.width() <= 0.0 || area.height() <= 0.0 {
            return;
        }
        let rect = to_px_rect(area, scale);
        let split = |color: COLORREF| {
            let value = color.0;
            TRIVERTEX {
                x: 0,
                y: 0,
                Red: ((value & 0xff) << 8) as u16,
                Green: (((value >> 8) & 0xff) << 8) as u16,
                Blue: (((value >> 16) & 0xff) << 8) as u16,
                Alpha: 0xff00,
            }
        };
        let mut vertices = [split(top), split(bottom)];
        vertices[0].x = rect.left;
        vertices[0].y = rect.top;
        vertices[1].x = rect.right;
        vertices[1].y = rect.bottom;
        let gradient = [GRADIENT_RECT {
            UpperLeft: 0,
            LowerRight: 1,
        }];
        unsafe {
            let _ = GradientFill(
                hdc,
                &mut vertices,
                gradient.as_ptr() as *const core::ffi::c_void,
                1,
                GRADIENT_FILL_RECT_V,
            );
        }
    }

    /// Draw a decoded cover cropped to a square. Returns false when there is
    /// no image (the caller then paints its placeholder).
    pub(crate) fn cover(
        &mut self,
        hdc: HDC,
        area: Rect,
        image: &RgbaImage,
        scale: f32,
    ) -> bool {
        if area.width() <= 0.0 || area.height() <= 0.0 {
            return true;
        }
        let Some(pixels) = self.cached_cover(image) else {
            return false;
        };
        let side = pixels.width.min(pixels.height).max(1);
        let src_x = (pixels.width - side) / 2;
        let src_y = (pixels.height - side) / 2;
        let dest = to_px_rect(area, scale);
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: pixels.width,
                // Top-down DIB: rows arrive in image order.
                biHeight: -pixels.height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        unsafe {
            let saved = SaveDC(hdc);
            SetStretchBltMode(hdc, HALFTONE);
            let _ = StretchDIBits(
                hdc,
                dest.left,
                dest.top,
                dest.right - dest.left,
                dest.bottom - dest.top,
                src_x,
                src_y,
                side,
                side,
                Some(pixels.bgra.as_ptr() as *const core::ffi::c_void),
                &info,
                DIB_RGB_COLORS,
                SRCCOPY,
            );
            let _ = RestoreDC(hdc, saved);
        }
        true
    }

    /// Filled ellipse without an outline (decorative background blobs).
    pub(crate) fn ellipse(&mut self, hdc: HDC, area: Rect, color: COLORREF, scale: f32) {
        if area.width() <= 1.0 || area.height() <= 1.0 {
            return;
        }
        let rect = to_px_rect(area, scale);
        let brush = self.brush(color);
        unsafe {
            let old_brush = SelectObject(hdc, HGDIOBJ(brush.0));
            let old_pen = SelectObject(hdc, GetStockObject(NULL_PEN));
            let _ = Ellipse(hdc, rect.left, rect.top, rect.right, rect.bottom);
            let _ = SelectObject(hdc, old_pen);
            let _ = SelectObject(hdc, old_brush);
        }
    }

    pub(crate) fn brush(&mut self, color: COLORREF) -> HBRUSH {
        if let Some(brush) = self.brushes.get(&color.0) {
            return *brush;
        }
        let brush = unsafe { CreateSolidBrush(color) };
        self.brushes.insert(color.0, brush);
        brush
    }

    fn font(&mut self, kind: FontKind, dpi: f32) -> HFONT {
        if (self.fonts_dpi - dpi).abs() > 0.5 {
            self.clear_fonts();
            self.fonts_dpi = dpi;
        }
        let key = kind as u8;
        if let Some(font) = self.fonts.get(&key) {
            return *font;
        }
        let (size, weight, face) = match kind {
            FontKind::Title => (20.0, 700, "Microsoft YaHei UI"),
            FontKind::NowTitle => (30.0, 700, "Microsoft YaHei UI"),
            FontKind::Body => (13.5, 400, "Microsoft YaHei UI"),
            FontKind::Small => (11.5, 400, "Microsoft YaHei UI"),
            // Monochrome transport/tool icons (shuffle, repeat, volume, gear…).
            FontKind::Glyph => (15.0, 400, "Segoe MDL2 Assets"),
            FontKind::GlyphLarge => (18.0, 400, "Segoe MDL2 Assets"),
            FontKind::Brand => (12.0, 600, "Microsoft YaHei UI"),
            FontKind::CoverGlyph => (64.0, 400, "Microsoft YaHei UI"),
            FontKind::ScreenTitle => (22.0, 700, "Microsoft YaHei UI"),
            FontKind::ScreenBody => (15.0, 400, "Microsoft YaHei UI"),
            FontKind::ScreenSmall => (12.5, 400, "Microsoft YaHei UI"),
            FontKind::GlyphHuge => (24.0, 400, "Segoe MDL2 Assets"),
        };
        let glyph = matches!(
            kind,
            FontKind::Glyph | FontKind::GlyphLarge | FontKind::GlyphHuge
        );
        let size = if glyph { size } else { size * self.font_scale };
        let face = if glyph {
            face.to_string()
        } else {
            self.font_family.clone().unwrap_or_else(|| face.to_string())
        };
        let height = -((size * dpi / 96.0).round().max(1.0) as i32);
        let face: Vec<u16> = face.encode_utf16().chain([0]).collect();
        let font = unsafe {
            CreateFontW(
                height,
                0,
                0,
                0,
                weight,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                CLEARTYPE_QUALITY,
                0,
                PCWSTR(face.as_ptr()),
            )
        };
        self.fonts.insert(key, font);
        font
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn text(
        &mut self,
        hdc: HDC,
        kind: FontKind,
        text: &str,
        area: Rect,
        color: COLORREF,
        align: u32,
        scale: f32,
        dpi: f32,
    ) {
        if area.width() <= 1.0 || text.is_empty() {
            return;
        }
        let font = self.font(kind, dpi);
        let mut rect = to_px_rect(area, scale);
        let mut wide: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            SelectObject(hdc, HGDIOBJ(font.0));
            SetTextColor(hdc, color);
            let format = DRAW_TEXT_FORMAT(
                DT_SINGLELINE.0 | DT_VCENTER.0 | DT_NOPREFIX.0 | DT_END_ELLIPSIS.0 | align,
            );
            DrawTextW(hdc, &mut wide, &mut rect, format);
        }
    }
}

impl Drop for Painter {
    fn drop(&mut self) {
        self.release_back_buffer();
        for (_, brush) in self.brushes.drain() {
            unsafe {
                let _ = DeleteObject(HGDIOBJ(brush.0));
            }
        }
        self.clear_fonts();
    }
}

/// COLORREF is 0x00BBGGRR.
fn rgb3_color(color: [u8; 3]) -> COLORREF {
    COLORREF(
        color[0] as u32 | (color[1] as u32) << 8 | (color[2] as u32) << 16,
    )
}

fn box_blur_rgb(input: &[u8], output: &mut [u8], width: usize, height: usize) {    for y in 0..height {
        for x in 0..width {
            let mut sum = [0u32; 3];
            let mut count = 0u32;
            for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    let sx = (x as i32 + dx).clamp(0, width as i32 - 1) as usize;
                    let sy = (y as i32 + dy).clamp(0, height as i32 - 1) as usize;
                    let index = (sy * width + sx) * 3;
                    sum[0] += input[index] as u32;
                    sum[1] += input[index + 1] as u32;
                    sum[2] += input[index + 2] as u32;
                    count += 1;
                }
            }
            let index = (y * width + x) * 3;
            output[index] = (sum[0] / count) as u8;
            output[index + 1] = (sum[1] / count) as u8;
            output[index + 2] = (sum[2] / count) as u8;
        }
    }
}

fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    let value = bytes as f64;
    if value >= GIB {
        format!("{:.1} GB", value / GIB)
    } else if value >= MIB {
        format!("{:.0} MB", value / MIB)
    } else if value >= KIB {
        format!("{:.0} KB", value / KIB)
    } else {
        format!("{bytes} B")
    }
}

fn hits(area: Rect, dirty: &[Rect]) -> bool {
    dirty.iter().any(|rect| area.intersects(*rect))
}

fn merge_rects(rects: Vec<Rect>, full: Rect) -> Vec<Rect> {
    let full_area = full.width() * full.height();
    let mut merged: Vec<Rect> = Vec::new();
    let mut area = 0.0;
    for rect in rects {
        let clipped = intersect(rect, full);
        if clipped.width() <= 0.5 || clipped.height() <= 0.5 {
            continue;
        }
        area += clipped.width() * clipped.height();
        merged.push(clipped);
    }
    if merged.is_empty() {
        return merged;
    }
    if merged.len() > 16 || area > full_area * 0.55 {
        return vec![full];
    }
    let mut changed = true;
    while changed {
        changed = false;
        'outer: for i in 0..merged.len() {
            for j in (i + 1)..merged.len() {
                if merged[i].inflate(2.0).intersects(merged[j]) {
                    merged[i] = union(merged[i], merged[j]);
                    merged.remove(j);
                    changed = true;
                    break 'outer;
                }
            }
        }
    }
    merged
}

fn intersect(a: Rect, b: Rect) -> Rect {
    Rect::new(
        a.left.max(b.left),
        a.top.max(b.top),
        a.right.min(b.right),
        a.bottom.min(b.bottom),
    )
}

fn union(a: Rect, b: Rect) -> Rect {
    Rect::new(
        a.left.min(b.left),
        a.top.min(b.top),
        a.right.max(b.right),
        a.bottom.max(b.bottom),
    )
}

fn to_px_rect(rect: Rect, scale: f32) -> RECT {
    RECT {
        left: (rect.left * scale).floor() as i32,
        top: (rect.top * scale).floor() as i32,
        right: (rect.right * scale).ceil() as i32,
        bottom: (rect.bottom * scale).ceil() as i32,
    }
}

pub(crate) fn format_time(seconds: f32) -> String {
    let total = seconds.max(0.0) as u64;
    format!("{:02}:{:02}", total / 60, total % 60)
}

/// Source · lossless · duration · format badges for the Now Playing card.
fn card_badges(track: Option<&crate::core::model::Track>) -> String {
    let Some(track) = track else {
        return String::new();
    };
    let mut parts: Vec<String> = Vec::new();
    let source = if track.source_name.trim().is_empty() {
        match track.source.kind_label() {
            "local" => crate::i18n::tr("本地"),
            "url" => crate::i18n::tr("在线"),
            "radio" => crate::i18n::tr("电台"),
            "bamboo" => "Bamboo",
            _ => "",
        }
    } else {
        track.source_name.as_str()
    };
    if !source.is_empty() {
        parts.push(source.to_string());
    }
    if track.lossless {
        parts.push(crate::i18n::tr("无损").to_string());
    }
    if let Some(duration) = track.duration {
        parts.push(format_time(duration as f32));
    }
    if let Some(path) = track.source.path()
        && let Some(extension) = path.extension()
    {
        parts.push(extension.to_string_lossy().to_uppercase());
    }
    parts.join(" · ")
}

/// Background used by the native search EDIT so it blends with the frame.
pub fn edit_background(pal: &Palette) -> COLORREF {
    pal.search_bg
}

/// Text color used by native child controls.
pub fn edit_text_color(pal: &Palette) -> COLORREF {
    pal.text
}

/// Background for the native sort LISTBOX (matches the panel, not the
/// search field, so the unused strip at the bottom is invisible).
pub fn list_background(pal: &Palette) -> COLORREF {
    pal.player
}
