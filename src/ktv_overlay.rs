//! KTV lyric line as a layered child-window control.
//!
//! The main window is pure GDI. The KTV line needs Direct2D text + clipping,
//! so it lives in its own child window: a D2D DC render target draws into a
//! persistent DIB, and only the changed strip is published with
//! `UpdateLayeredWindowIndirect`. A manifest (`app.manifest`) enables layered
//! child windows; `WS_EX_TRANSPARENT` makes the control click-through.
//!
//! The parent drives updates (no timer here); the control only repaints when
//! the lyric line or fill position actually changes.

use std::collections::HashMap;
use std::mem::size_of;

use tracing::warn;
use windows::Win32::Foundation::{
    COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM,
};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_ALIASED, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_FACTORY_OPTIONS,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_TYPE_SOFTWARE, D2D1_RENDER_TARGET_USAGE_NONE,
    D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE, D2D1CreateFactory, ID2D1DCRenderTarget, ID2D1Factory,
    ID2D1RenderTarget, ID2D1SolidColorBrush,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_PARAGRAPH_ALIGNMENT_NEAR, DWRITE_TEXT_ALIGNMENT_CENTER,
    DWRITE_TEXT_METRICS, DWRITE_WORD_WRAPPING_NO_WRAP, DWriteCreateFactory, IDWriteFactory,
    IDWriteTextFormat, IDWriteTextLayout,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
    CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, HDC, HGDIOBJ,
    RGBQUAD, SelectObject,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GWL_EXSTYLE, GWLP_USERDATA, RegisterClassW, SW_HIDE,
    SW_SHOWNA, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, ULW_ALPHA, UPDATELAYEREDWINDOWINFO,
    UpdateLayeredWindow, UpdateLayeredWindowIndirect, WM_ERASEBKGND, WM_NCDESTROY, WNDCLASSW,
    WS_CHILD, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TRANSPARENT,
};
use windows::core::{PCWSTR, Result, w};
use windows_numerics::Vector2;

use crate::app::{App, Page};
use crate::layout::Rect;

const BASE: D2D1_COLOR_F = color(0.540, 0.570, 0.610, 1.0);
const ACCENT: D2D1_COLOR_F = color(0.560, 0.880, 0.710, 1.0);
const CLASS_NAME: PCWSTR = w!("BambooKtvChild");
const BAND_TOP: f32 = 2.0;
const BAND_HEIGHT: f32 = 58.0;
const LINE_TOP: f32 = 10.0;
const FONT_SIZE: f32 = 26.0;

struct Surface {
    rt: ID2D1DCRenderTarget,
    mem_dc: HDC,
    dib: HGDIOBJ,
    old: HGDIOBJ,
    width: u32,
    height: u32,
    dpi: f32,
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

pub struct KtvOverlay {
    hwnd: HWND,
    factory: ID2D1Factory,
    dwrite: IDWriteFactory,
    format: IDWriteTextFormat,
    surface: Option<Surface>,
    layout: Option<(usize, u32, IDWriteTextLayout)>,
    brushes: HashMap<u32, ID2D1SolidColorBrush>,
    /// Text span in band-local DIPs (left, width) from the last draw.
    span: Option<(f32, f32)>,
    line: Option<usize>,
    progress: f32,
    visible: bool,
}

impl KtvOverlay {
    pub fn new(parent: HWND) -> Result<Self> {
        unsafe {
            let instance = GetModuleHandleW(None)?;
            let class = WNDCLASSW {
                hInstance: HINSTANCE(instance.0),
                lpszClassName: CLASS_NAME,
                lpfnWndProc: Some(child_proc),
                ..Default::default()
            };
            let _ = RegisterClassW(&class);
            let hwnd = CreateWindowExW(
                windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0),
                CLASS_NAME,
                w!(""),
                WS_CHILD,
                0,
                0,
                1,
                1,
                Some(parent),
                None,
                Some(HINSTANCE(instance.0)),
                None,
            )?;
            // Layered child windows need the app manifest (see app.manifest);
            // CreateWindowExW itself rejects WS_EX_LAYERED for children.
            SetWindowLongPtrW(
                hwnd,
                GWL_EXSTYLE,
                (WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE).0 as isize,
            );
            let _ = SetWindowPos(
                hwnd,
                None,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
            );
            let factory: ID2D1Factory = D2D1CreateFactory(
                D2D1_FACTORY_TYPE_SINGLE_THREADED,
                Some(&D2D1_FACTORY_OPTIONS::default()),
            )?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let family: Vec<u16> = "Microsoft YaHei UI\0".encode_utf16().collect();
            let locale: Vec<u16> = "zh-CN\0".encode_utf16().collect();
            let format = dwrite.CreateTextFormat(
                PCWSTR(family.as_ptr()),
                None,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                FONT_SIZE,
                PCWSTR(locale.as_ptr()),
            )?;
            format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
            format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR)?;
            format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            Ok(Self {
                hwnd,
                factory,
                dwrite,
                format,
                surface: None,
                layout: None,
                brushes: HashMap::new(),
                span: None,
                line: None,
                progress: 0.0,
                visible: false,
            })
        }
    }

    pub fn update(&mut self, parent: HWND, app: &App) {
        let area = app.layout.ktv;
        let Some((index, progress)) = app.current_line() else {
            self.hide();
            return;
        };
        let Some(text) = app.lyric_line(index) else {
            self.hide();
            return;
        };
        if app.page != Page::NowPlaying || area.height() < 30.0 {
            self.hide();
            return;
        }
        // While the window is fully covered, skip all publishing: DWM
        // composition of covered layers costs more than idling. Focus is not
        // considered; visible-but-unfocused windows keep updating.
        if !app.window_visible {
            return;
        }

        let dpi = unsafe { GetDpiForWindow(parent) } as f32;
        let dpi = if dpi > 0.0 { dpi } else { 96.0 };
        let scale = dpi / 96.0;
        let width = ((area.width() - 4.0) * scale).round().max(1.0) as u32;
        let height = ((BAND_HEIGHT - 4.0) * scale).round().max(1.0) as u32;
        let recreated = self.surface.as_ref().is_none_or(|surface| {
            surface.width != width || surface.height != height || (surface.dpi - dpi).abs() > 0.5
        });
        if recreated {
            self.surface = match self.create_surface(width, height, dpi) {
                Ok(surface) => Some(surface),
                Err(err) => {
                    warn!("creating KTV surface failed: {err}");
                    self.hide();
                    return;
                }
            };
            self.layout = None;
            self.line = None;
        }

        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                None,
                (area.left * scale).round() as i32,
                ((area.top + BAND_TOP) * scale).round() as i32,
                width as i32,
                height as i32,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            if !self.visible {
                let _ = ShowWindow(self.hwnd, SW_SHOWNA);
                self.visible = true;
                self.line = None;
            }
        }

        let line_changed = self.line != Some(index);
        let full = recreated || line_changed || progress < self.progress;
        let old_progress = self.progress;
        self.line = Some(index);
        self.progress = progress;

        let dirty = if full {
            None
        } else if let Some((left, span_width)) = self.span {
            let old_x = left + span_width * old_progress;
            let new_x = left + span_width * progress;
            if (new_x - old_x).abs() < 0.3 {
                return;
            }
            Some(RECT {
                left: ((old_x.min(new_x) - 3.0) * scale).floor() as i32,
                top: 0,
                right: ((old_x.max(new_x) + 3.0) * scale).ceil() as i32,
                bottom: height as i32,
            })
        } else {
            None
        };

        let pieces = visible_pieces(app, dirty, width, height, scale);
        if pieces.is_empty() {
            return;
        }
        if let Err(err) = self.draw(index, text, progress) {
            warn!("KTV draw failed: {err}");
            self.surface = None;
            self.layout = None;
            self.line = None;
            return;
        }
        if let Some(surface) = self.surface.as_ref() {
            let mem_dc = surface.mem_dc;
            let surface_width = surface.width;
            let surface_height = surface.height;
            for piece in pieces {
                self.publish(mem_dc, surface_width, surface_height, Some(piece));
            }
        }
    }

    /// Force the next update to redraw/publish the whole band (used after the
    /// parent window becomes visible again).
    pub fn mark_dirty(&mut self) {
        self.line = None;
    }

    pub fn hide(&mut self) {
        if self.visible {
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
            self.visible = false;
        }
        self.line = None;
    }

    fn create_surface(&self, width: u32, height: u32, dpi: f32) -> Result<Surface> {
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                biHeight: -(height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            bmiColors: [RGBQUAD::default()],
        };
        let mut bits = std::ptr::null_mut();
        let dib = unsafe { CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0)? };
        let mem_dc = unsafe { CreateCompatibleDC(None) };
        if mem_dc.0.is_null() {
            unsafe {
                let _ = DeleteObject(HGDIOBJ(dib.0));
            }
            return Err(windows::core::Error::from_thread());
        }
        let old = unsafe { SelectObject(mem_dc, HGDIOBJ(dib.0)) };
        let properties = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 0.0,
            dpiY: 0.0,
            usage: D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
        };
        let rt: ID2D1DCRenderTarget = unsafe { self.factory.CreateDCRenderTarget(&properties)? };
        let surface = Surface {
            rt,
            mem_dc,
            dib: HGDIOBJ(dib.0),
            old,
            width,
            height,
            dpi,
        };
        let bounds = RECT {
            left: 0,
            top: 0,
            right: width as i32,
            bottom: height as i32,
        };
        unsafe {
            surface.rt.BindDC(mem_dc, &bounds)?;
            surface.rt.SetDpi(dpi, dpi);
            surface
                .rt
                .SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
        }
        Ok(surface)
    }

    fn draw(&mut self, index: usize, text: &str, progress: f32) -> Result<()> {
        let Some(surface) = self.surface.as_ref() else {
            return Ok(());
        };
        let rt = surface.rt.clone();
        let surface_dpi = surface.dpi;
        let surface_width = surface.width;
        let width_dip = surface_width as f32 / (surface_dpi / 96.0);
        
        let layout = match &self.layout {
            Some((line, layout_width, layout))
                if *line == index && *layout_width == width_dip.round() as u32 =>
            {
                layout.clone()
            }
            _ => {
                let wide: Vec<u16> = text.encode_utf16().collect();
                let layout = unsafe {
                    self.dwrite
                        .CreateTextLayout(&wide, &self.format, width_dip, 200.0)?
                };
                self.layout = Some((index, width_dip.round() as u32, layout.clone()));
                layout
            }
        };
        let mut metrics = DWRITE_TEXT_METRICS::default();
        unsafe {
            layout.GetMetrics(&mut metrics)?;
        }
        self.span = Some((metrics.left, metrics.width));

        let target: &ID2D1RenderTarget = &rt;
        unsafe {
            target.BeginDraw();
            let clear = D2D1_COLOR_F {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0,
            };
            target.Clear(Some(&clear));
            let base = self.solid(target, BASE)?;
            target.DrawTextLayout(
                Vector2::new(0.0, LINE_TOP),
                &layout,
                &base,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
            let right = metrics.left + metrics.width * progress;
            if right > metrics.left {
                let clip = D2D_RECT_F {
                    left: metrics.left,
                    top: LINE_TOP,
                    right,
                    bottom: LINE_TOP + metrics.height.max(40.0),
                };
                let hot = self.solid(target, ACCENT)?;
                let _ = target.PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_ALIASED);
                target.DrawTextLayout(
                    Vector2::new(0.0, LINE_TOP),
                    &layout,
                    &hot,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                );
                let _ = target.PopAxisAlignedClip();
            }
            target.EndDraw(None, None)?;
        }

        Ok(())
    }

    fn publish(&self, mem_dc: HDC, width: u32, height: u32, dirty: Option<RECT>) {
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
                    cbSize: size_of::<UPDATELAYEREDWINDOWINFO>() as u32,
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
                if UpdateLayeredWindowIndirect(self.hwnd, &info).as_bool() {
                    return;
                }
            }
            if let Err(err) = UpdateLayeredWindow(
                self.hwnd,
                None,
                None,
                Some(&size),
                Some(mem_dc),
                Some(&src),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            ) {
                warn!("UpdateLayeredWindow failed: {err}");
            }
        }
    }

    fn solid(
        &mut self,
        target: &ID2D1RenderTarget,
        color: D2D1_COLOR_F,
    ) -> Result<ID2D1SolidColorBrush> {
        let key = color.r.to_bits()
            ^ color.g.to_bits().rotate_left(7)
            ^ color.b.to_bits().rotate_left(13)
            ^ color.a.to_bits().rotate_left(19);
        if let Some(brush) = self.brushes.get(&key) {
            return Ok(brush.clone());
        }
        let brush = unsafe { target.CreateSolidColorBrush(&color, None)? };
        self.brushes.insert(key, brush.clone());
        Ok(brush)
    }
}

/// Clip the KTV dirty region (child-local px) to the parent's visible rects and
/// convert each exposed piece back to child-local pixels. Empty means the band
/// is fully covered, so nothing should be drawn or published.
fn visible_pieces(
    app: &App,
    dirty: Option<RECT>,
    width: u32,
    height: u32,
    scale: f32,
) -> Vec<RECT> {
    let area = app.layout.ktv;
    let band_left = area.left;
    let band_top = area.top + BAND_TOP;
    let band_width = width as f32 / scale;
    let band_height = height as f32 / scale;
    let target = if let Some(dirty) = dirty {
        Rect::new(
            band_left + dirty.left as f32 / scale,
            band_top + dirty.top as f32 / scale,
            band_left + dirty.right as f32 / scale,
            band_top + dirty.bottom as f32 / scale,
        )
    } else {
        Rect::new(
            band_left,
            band_top,
            band_left + band_width,
            band_top + band_height,
        )
    };
    let targets: Vec<Rect> = if app.visible.is_empty() {
        vec![target]
    } else {
        app.visible
            .iter()
            .filter_map(|visible| {
                let piece = intersect(target, *visible);
                (piece.width() > 0.5 && piece.height() > 0.5).then_some(piece)
            })
            .collect()
    };
    let mut pieces = Vec::new();
    for target in targets {
        let left = ((target.left - band_left) * scale).floor().max(0.0) as i32;
        let top = ((target.top - band_top) * scale).floor().max(0.0) as i32;
        let right = ((target.right - band_left) * scale)
            .ceil()
            .min(width as f32) as i32;
        let bottom = ((target.bottom - band_top) * scale)
            .ceil()
            .min(height as f32) as i32;
        if right > left && bottom > top {
            pieces.push(RECT {
                left,
                top,
                right,
                bottom,
            });
        }
    }
    pieces
}

fn intersect(a: Rect, b: Rect) -> Rect {
    Rect::new(
        a.left.max(b.left),
        a.top.max(b.top),
        a.right.min(b.right),
        a.bottom.min(b.bottom),
    )
}

unsafe extern "system" fn child_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_ERASEBKGND => LRESULT(1),
        WM_NCDESTROY => {
            unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

const fn color(r: f32, g: f32, b: f32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r, g, b, a }
}
