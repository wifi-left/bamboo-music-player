//! Scrollable lyric list as a native child window (MFC style).
//!
//! The parent only draws the panel shell (background, "歌词" title, action
//! buttons). This control owns everything inside the panel: row layout via
//! [`crate::lyrics::view`], double-buffered GDI painting, hover/press states,
//! wheel and scrollbar scrolling, click-to-seek, karaoke fill and auto
//! centering of the current line.
//!
//! Auto centering pauses for three seconds after a manual scroll, so wheel
//! browsing is not immediately snapped back. Clicking a line posts
//! [`WM_APP_LYRIC_SEEK`] with the line's start time in milliseconds to the
//! parent window; the parent turns it into a `Command::SeekSecs`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CLEARTYPE_QUALITY,
    CLIP_DEFAULT_PRECIS, CreateCompatibleBitmap, CreateCompatibleDC, CreateFontW,
    CreateSolidBrush, DEFAULT_CHARSET, DIB_RGB_COLORS, DRAW_TEXT_FORMAT, DT_CENTER, DT_LEFT,
    DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE, DT_VCENTER, DeleteDC, DeleteObject, DrawTextW, Ellipse,
    EndPaint, FillRect, GRADIENT_FILL_RECT_V, GRADIENT_RECT, GetStockObject,
    GetTextExtentPoint32W, GradientFill, HALFTONE, HBRUSH, HBITMAP, HDC, HFONT, HGDIOBJ,
    IntersectClipRect, MapWindowPoints, NULL_PEN, OUT_DEFAULT_PRECIS, PAINTSTRUCT, RestoreDC,
    SRCCOPY, SaveDC, SelectObject, SetBkMode, SetStretchBltMode, SetTextColor, StretchDIBits,
    TRANSPARENT, TRIVERTEX,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GWLP_USERDATA, GetClientRect, GetParent, GetWindowLongPtrW,
    PostMessageW, RegisterClassW, SW_HIDE, SW_SHOWNA, SWP_NOACTIVATE, SWP_NOZORDER,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, WM_APP, WM_ERASEBKGND, WM_KEYDOWN,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCDESTROY, WM_PAINT,
    WM_RBUTTONUP, WM_SIZE, WNDCLASSW, WS_CHILD, WS_CLIPSIBLINGS,
};
use windows::core::{PCWSTR, Result, w};

use crate::app::{App, Page};
use crate::config::{LyricsAlign, LyricsPanelBackground};
use crate::layout::Rect;
use crate::lyrics::view::{self, LyricsView, ViewConfig};
use crate::util::image_util::RgbaImage;
use crate::lyrics::{Lyrics, karaoke_progress};
use crate::theme::Palette;

/// Posted to the parent when a lyric line is clicked; `wparam` is start_ms.
pub const WM_APP_LYRIC_SEEK: u32 = WM_APP + 2;
/// Posted to the parent on right click; `wparam` is `line_index + 1`
/// (0 = clicked outside any line). The parent builds the lyrics menu.
pub const WM_APP_LYRIC_MENU: u32 = WM_APP + 7;

const CLASS_NAME: PCWSTR = w!("BambooLyricsChild");
/// The standalone lyrics window is deliberately an immersive dark surface;
/// it does not follow the light theme, but its text/accent colors do.
const C_FLOATING_BG: COLORREF = rgb(13, 15, 17);
const C_FLOATING_BLOB: COLORREF = rgb(21, 31, 27);
/// Manual scrolling suspends auto centering for this long.
const AUTO_CENTER_PAUSE: Duration = Duration::from_secs(3);
const SCROLLBAR_WIDTH: f32 = 8.0;
const SIDE_PADDING: f32 = 12.0;

/// Appearance pulled from `Settings.lyrics` / `Settings.large_lyrics` plus the
/// window palette. Compared as a whole so any change triggers one full repaint
/// and a font-cache reset.
#[derive(Clone, PartialEq)]
struct Style {
    palette: Palette,
    normal_size: f32,
    current_size: f32,
    translation_size: f32,
    line_height: f32,
    translation_height: f32,
    gap: f32,
    align: LyricsAlign,
    karaoke: bool,
    words_only: bool,
    show_translation: bool,
    current_color: COLORREF,
    normal_color: COLORREF,
    translation_color: COLORREF,
    /// Karaoke/sung fill. Settings-driven so it can differ from the theme
    /// accent on each surface.
    highlight_color: COLORREF,
    /** Embedded panels keep a row highlight behind the current line. */
    current_bg: COLORREF,
    /// Embedded panel fill override (`None` = theme panel color).
    panel_bg: Option<COLORREF>,
    /// Embedded panel fill style. Floating surfaces always use `Theme`.
    panel_kind: LyricsPanelBackground,
    panel_color2: COLORREF,
    /// Source picture key for `Image`; `None` disables image lookup.
    panel_image_key: Option<PathBuf>,
    reduce_motion: bool,
    font_family: Option<String>,
    font_weight: u16,
    italic: bool,
    outline_width: f32,
    outline_color: COLORREF,
    shadow_offset: f32,
    shadow_color: COLORREF,
    translation_outline_width: f32,
    translation_outline_color: COLORREF,
    translation_shadow_offset: f32,
    translation_shadow_color: COLORREF,
    /// Extra user offset in milliseconds (on top of the file's `[offset]`).
    offset_ms: i64,
}

impl Style {
    fn placeholder() -> Self {
        let palette = Palette::new(&crate::config::ThemeSettings::default(), true);
        Self {
            palette,
            normal_size: 14.0,
            current_size: 15.0,
            translation_size: 11.5,
            line_height: 30.0,
            translation_height: 20.0,
            gap: 8.0,
            align: LyricsAlign::Center,
            karaoke: true,
            words_only: false,
            show_translation: true,
            current_color: rgb3([214, 220, 226]),
            normal_color: rgb3([165, 173, 184]),
            translation_color: rgb3([124, 133, 145]),
            highlight_color: rgb3([143, 224, 181]),
            current_bg: palette.row_selected,
            reduce_motion: false,
            font_family: None,
            font_weight: 400,
            italic: false,
            outline_width: 0.0,
            outline_color: rgb3([0, 0, 0]),
            shadow_offset: 0.0,
            shadow_color: rgb3([0, 0, 0]),
            translation_outline_width: 0.0,
            translation_outline_color: rgb3([0, 0, 0]),
            translation_shadow_offset: 0.0,
            translation_shadow_color: rgb3([0, 0, 0]),
            panel_bg: None,
            panel_kind: LyricsPanelBackground::Theme,
            panel_color2: rgb3([46, 52, 64]),
            panel_image_key: None,
            offset_ms: 0,
        }
    }

    fn from_app(app: &App, floating: bool) -> Self {
        let palette = app.palette();
        let lyrics = &app.core.settings.lyrics;
        let large = &app.core.settings.large_lyrics;
        if floating {
            let current = (large.font_size - 3.0).clamp(10.0, 190.0);
            let normal = (current - 2.0).max(10.0);
            Self {
                palette,
                normal_size: normal,
                current_size: current,
                translation_size: (current * 0.675).clamp(9.0, 24.0),
                line_height: current + 26.0,
                translation_height: 26.0,
                gap: 12.0 + (large.line_spacing - 14.0).max(-8.0) * 0.5,
                align: large.align,
                karaoke: large.karaoke,
                words_only: large.karaoke_words_only,
                show_translation: large.show_translation,
                current_color: rgb3(large.current_color),
                normal_color: rgb3(large.normal_color),
                translation_color: rgb3(large.translation_color),
                highlight_color: rgb3(large.highlight_color),
                current_bg: palette.panel,
                panel_bg: None,
                panel_kind: LyricsPanelBackground::Theme,
                panel_color2: rgb3([46, 52, 64]),
                panel_image_key: None,
                reduce_motion: app.core.settings.theme.reduce_motion,
                font_family: large.font_family.clone(),
                font_weight: large.font_weight,
                italic: large.italic,
                outline_width: large.outline_width,
                outline_color: rgb3(large.outline_color),
                shadow_offset: large.shadow_offset,
                shadow_color: rgb3(large.shadow_color),
                translation_outline_width: large.translation_outline_width,
                translation_outline_color: rgb3(large.translation_outline_color),
                translation_shadow_offset: large.translation_shadow_offset,
                translation_shadow_color: rgb3(large.translation_shadow_color),
                offset_ms: lyrics.offset_ms,
            }
        } else {
            let current = (lyrics.font_size - 2.0).clamp(8.0, 116.0);
            let normal = (current - 1.0).max(10.0);
            // The stored defaults were tuned for the dark surface. In the
            // light theme those exact values would be unreadable, so map only
            // the untouched defaults to dark counterparts; custom colors are
            // always used verbatim.
            let adapt = |color: [u8; 3], dark_default: [u8; 3], light_default: [u8; 3]| {
                if !palette.dark && color == dark_default {
                    light_default
                } else {
                    color
                }
            };
            Self {
                palette,
                normal_size: normal,
                current_size: current,
                translation_size: (current * 0.77).clamp(9.0, 24.0),
                line_height: current + 15.0,
                translation_height: translation_size_c(lyrics) + 8.5,
                gap: (lyrics.line_spacing - 2.0).max(0.0),
                align: lyrics.align,
                karaoke: lyrics.karaoke,
                words_only: lyrics.karaoke_words_only,
                show_translation: lyrics.show_translation,
                current_color: rgb3(adapt(lyrics.current_color, [255, 255, 255], [28, 31, 36])),
                normal_color: rgb3(adapt(lyrics.normal_color, [150, 160, 180], [88, 96, 108])),
                translation_color: rgb3(adapt(
                    lyrics.translation_color,
                    [130, 142, 165],
                    [122, 131, 145],
                )),
                highlight_color: rgb3(lyrics.highlight_color),
                current_bg: palette.row_selected,
                panel_bg: lyrics.panel_color.map(rgb3),
                panel_kind: lyrics.panel_background,
                panel_color2: rgb3(lyrics.panel_color2),
                panel_image_key: (lyrics.panel_background == LyricsPanelBackground::Image)
                    .then(|| lyrics.panel_image.clone())
                    .flatten(),
                reduce_motion: app.core.settings.theme.reduce_motion,
                font_family: lyrics.font_family.clone(),
                font_weight: lyrics.font_weight,
                italic: lyrics.italic,
                outline_width: lyrics.outline_width,
                outline_color: rgb3(lyrics.outline_color),
                shadow_offset: lyrics.shadow_offset,
                shadow_color: rgb3(lyrics.shadow_color),
                translation_outline_width: lyrics.translation_outline_width,
                translation_outline_color: rgb3(lyrics.translation_outline_color),
                translation_shadow_offset: lyrics.translation_shadow_offset,
                translation_shadow_color: rgb3(lyrics.translation_shadow_color),
                offset_ms: lyrics.offset_ms,
            }
        }
    }
}

fn translation_size_c(lyrics: &crate::config::LyricsSettings) -> f32 {
    ((lyrics.font_size - 2.0) * 0.77).clamp(7.0, 90.0)
}

const fn rgb3(color: [u8; 3]) -> COLORREF {
    COLORREF(((color[2] as u32) << 16) | ((color[1] as u32) << 8) | color[0] as u32)
}

fn align_flags(align: LyricsAlign) -> u32 {
    match align {
        LyricsAlign::Left => DT_LEFT.0,
        LyricsAlign::Right => DT_RIGHT.0,
        LyricsAlign::Center => DT_CENTER.0,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum FontKind {
    Normal = 0,
    Current = 1,
    Translation = 2,
    Hint = 3,
}

struct State {
    parent: HWND,
    view: LyricsView,
    lyrics: Option<Lyrics>,
    revision: u64,
    current: Option<usize>,
    progress: f32,
    position_ms: u64,
    scroll: f32,
    hover: Option<usize>,
    pressed: Option<usize>,
    bar_hover: bool,
    dragging: bool,
    manual_until: Option<Instant>,
    empty_hint: String,
    empty_sub_hint: String,
    style: Style,
    /// Decoded picture for the embedded panel, with a cached BGRA copy.
    panel_image: Option<RgbaImage>,
    panel_image_ptr: usize,
    panel_image_bgra: Option<(Vec<u8>, i32, i32)>,
    /// Set once a style has been applied; the first sync must repaint.
    styled: bool,
    visible: bool,
    /// Standalone window mode: larger rows, no scrollbar.
    floating: bool,
    needs_full: bool,
    dpi: f32,
    font_dpi: f32,
    fonts: HashMap<(u8, u16), HFONT>,
    brushes: HashMap<u32, HBRUSH>,
    back_dc: Option<HDC>,
    back_bitmap: Option<HBITMAP>,
    back_old: Option<HGDIOBJ>,
    back_size: (i32, i32),
    last_size: (i32, i32),
    /// Client size in device pixels, captured at paint time.
    client: (i32, i32),
}

impl State {
    fn new(parent: HWND) -> Self {
        Self {
            parent,
            view: LyricsView::new(ViewConfig::default()),
            lyrics: None,
            revision: u64::MAX,
            current: None,
            progress: 0.0,
            position_ms: 0,
            scroll: 0.0,
            hover: None,
            pressed: None,
            bar_hover: false,
            dragging: false,
            manual_until: None,
            empty_hint: crate::i18n::tr("暂无歌词").to_string(),
            empty_sub_hint: crate::i18n::tr("拖放或选择 .lrc/.srt 文件").to_string(),
            style: Style::placeholder(),
            panel_image: None,
            panel_image_ptr: 0,
            panel_image_bgra: None,
            styled: false,
            visible: false,
            floating: false,
            needs_full: true,
            dpi: 96.0,
            font_dpi: 0.0,
            fonts: HashMap::new(),
            brushes: HashMap::new(),
            back_dc: None,
            back_bitmap: None,
            back_old: None,
            back_size: (0, 0),
            last_size: (0, 0),
            client: (1, 1),
        }
    }

    fn hide(&mut self, hwnd: HWND) {
        if self.visible {
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
            self.visible = false;
        }
        self.hover = None;
        self.pressed = None;
        self.bar_hover = false;
        self.dragging = false;
        self.needs_full = true;
    }

    fn scale(&self) -> f32 {
        if self.dpi > 0.0 { self.dpi / 96.0 } else { 1.0 }
    }

    // ------------------------------------------------------------------
    // Painting
    // ------------------------------------------------------------------

    fn invalidate(&self, hwnd: HWND) {
        let _ = unsafe { windows::Win32::Graphics::Gdi::InvalidateRect(Some(hwnd), None, false) };
    }

    fn invalidate_line(&self, hwnd: HWND, index: usize) {
        let Some(metrics) = self.view.line(index) else {
            return;
        };
        let scale = self.scale();
        let top = ((metrics.top - self.scroll) * scale).floor() as i32 - 1;
        let bottom = ((metrics.top - self.scroll + metrics.height) * scale).ceil() as i32 + 1;
        let mut client = RECT::default();
        let _ = unsafe { GetClientRect(hwnd, &mut client) };
        if bottom <= 0 || top >= client.bottom {
            return;
        }
        let rect = RECT {
            left: 0,
            top: top.max(0),
            right: client.right,
            bottom: bottom.min(client.bottom),
        };
        let _ = unsafe {
            windows::Win32::Graphics::Gdi::InvalidateRect(Some(hwnd), Some(&rect), false)
        };
    }

    fn paint(&mut self, hwnd: HWND, hdc: HDC, paint: RECT) {
        let mut client = RECT::default();
        let _ = unsafe { GetClientRect(hwnd, &mut client) };
        let width = client.right.max(1);
        let height = client.bottom.max(1);
        self.client = (width, height);
        self.ensure_back_buffer(hdc, width, height);
        let Some(back) = self.back_dc else {
            return;
        };
        // Only the invalidated box is cleared, redrawn and blitted: karaoke
        // progress invalidates a single line, scrolling invalidates everything.
        let bounds = if paint.right > paint.left
            && paint.bottom > paint.top
            && paint.left >= 0
            && paint.top >= 0
            && paint.right <= width
            && paint.bottom <= height
        {
            paint
        } else {
            RECT {
                left: 0,
                top: 0,
                right: width,
                bottom: height,
            }
        };
        let scale = self.scale();
        let saved = unsafe { SaveDC(back) };
        unsafe {
            SetBkMode(back, TRANSPARENT);
        }
        let bounds_dip = Rect::new(
            bounds.left as f32 / scale,
            bounds.top as f32 / scale,
            bounds.right as f32 / scale,
            bounds.bottom as f32 / scale,
        );
        let full_dip = Rect::new(
            0.0,
            0.0,
            width as f32 / scale,
            height as f32 / scale,
        );
        if self.floating {
            // Blend into the immersive window: same base color plus the two
            // decorative ellipses, translated by this child's offset inside
            // the parent, so the gradient continues across the seam.
            self.fill(back, bounds_dip, C_FLOATING_BG);
            let (w, h, ox, oy) = self.floating_blobs(hwnd);
            let top_blob = Rect::new(w * 0.72 - ox, -h * 0.32 - oy, w * 1.34 - ox, h * 0.52 - oy);
            if top_blob.intersects(bounds_dip) {
                self.ellipse(back, top_blob, C_FLOATING_BLOB);
            }
            let bottom_blob = Rect::new(-w * 0.18 - ox, h * 0.62 - oy, w * 0.42 - ox, h * 1.18 - oy);
            if bottom_blob.intersects(bounds_dip) {
                self.ellipse(back, bottom_blob, C_FLOATING_BLOB);
            }
        } else {
            let solid = self.style.panel_bg.unwrap_or(self.style.palette.panel);
            match self.style.panel_kind {
                LyricsPanelBackground::Theme | LyricsPanelBackground::Solid => {
                    self.fill(back, full_dip, solid);
                }
                LyricsPanelBackground::Gradient => {
                    self.fill_gradient(
                        back,
                        full_dip,
                        solid,
                        self.style.panel_color2,
                    );
                }
                LyricsPanelBackground::Image => {
                    if !self.draw_panel_image(back, full_dip) {
                        self.fill(back, full_dip, solid);
                    }
                }
            }
        }

        let viewport = height as f32 / scale;
        self.scroll = self
            .scroll
            .clamp(self.view.min_scroll(viewport), self.view.max_scroll(viewport));

        if self.lyrics.is_none() || self.view.is_empty() {
            self.draw_empty(back, width, height, scale);
        } else {
            let range = self.view.visible_range(self.scroll, viewport);
            for index in range {
                let Some(metrics) = self.view.line(index) else {
                    continue;
                };
                let top = ((metrics.top - self.scroll) * scale).floor() as i32;
                let bottom = ((metrics.top - self.scroll + metrics.height) * scale).ceil() as i32;
                if bottom <= bounds.top || top >= bounds.bottom {
                    continue;
                }
                self.draw_line(back, index, scale);
            }
            self.draw_scrollbar(back, viewport, scale);
        }

        unsafe {
            let _ = RestoreDC(back, saved);
            let _ = BitBlt(
                hdc,
                bounds.left,
                bounds.top,
                bounds.right - bounds.left,
                bounds.bottom - bounds.top,
                Some(back),
                bounds.left,
                bounds.top,
                SRCCOPY,
            );
        }
    }

    fn draw_empty(&mut self, hdc: HDC, width: i32, height: i32, scale: f32) {
        let area = Rect::new(0.0, 0.0, width as f32 / scale, height as f32 / scale);
        let top = (area.height() * 0.5 - 26.0).max(8.0);
        let hint = self.empty_hint.clone();
        let sub_hint = self.empty_sub_hint.clone();
        let (hint_size, sub_size) = if self.floating { (15.0, 12.5) } else { (13.0, 11.5) };
        self.text(
            hdc,
            FontKind::Hint,
            hint_size,
            &hint,
            Rect::new(area.left, top, area.right, top + 24.0),
            self.style.palette.muted,
            DT_CENTER.0 | DT_SINGLELINE.0 | DT_VCENTER.0 | DT_NOPREFIX.0,
        );
        self.text(
            hdc,
            FontKind::Translation,
            sub_size,
            &sub_hint,
            Rect::new(area.left, top + 26.0, area.right, top + 48.0),
            self.style.translation_color,
            DT_CENTER.0 | DT_SINGLELINE.0 | DT_VCENTER.0 | DT_NOPREFIX.0,
        );
    }

    fn draw_line(&mut self, hdc: HDC, index: usize, scale: f32) {
        let Some(metrics) = self.view.line(index) else {
            return;
        };
        // Clone the small per-line payloads so the immutable borrow of
        // `self.lyrics` ends before the mutable GDI helpers run.
        let (text, translation, karaoke) = {
            let Some(lyrics) = self.lyrics.as_ref() else {
                return;
            };
            let Some(line) = lyrics.lines.get(index) else {
                return;
            };
            let karaoke = if self.style.karaoke && self.current == Some(index) && lyrics.timed {
                // `words_only` means "do not guess with a proportional fill
                // for plain timed LRC"; the line is still highlighted as the
                // current line, it just has no karaoke sweep.
                if self.style.words_only && line.words.is_empty() {
                    None
                } else {
                    let position =
                        (view::adjusted_position(lyrics, self.position_ms) as i64
                            + self.style.offset_ms)
                            .max(0) as u64;
                    let end = view::line_end_ms(lyrics, index);
                    Some(karaoke_progress(line, position, end))
                }
            } else {
                None
            };
            (
                view::line_text(line).to_string(),
                line.translation.clone(),
                karaoke,
            )
        };
        if text.is_empty() {
            return;
        }

        let (client_width, _) = self.client;
        let top = (metrics.top - self.scroll) * scale;
        let width_dip = client_width as f32 / scale;
        let content_right = (width_dip - SCROLLBAR_WIDTH).max(0.0);
        let row_dip = Rect::new(
            0.0,
            metrics.top - self.scroll,
            width_dip,
            metrics.top - self.scroll + metrics.height,
        );
        if self.pressed == Some(index) {
            self.fill(hdc, row_dip, self.style.palette.row_pressed);
        } else if self.hover == Some(index) {
            self.fill(hdc, row_dip, self.style.palette.row_hover);
        } else if self.current == Some(index) && !self.floating {
            // The immersive window keeps the current line clean (accent fill
            // only); the embedded panel keeps its subtle row highlight.
            self.fill(hdc, row_dip, self.style.current_bg);
        }

        let is_current = self.current == Some(index);
        let (kind, size) = if is_current {
            (FontKind::Current, self.style.current_size)
        } else {
            (FontKind::Normal, self.style.normal_size)
        };
        let left = (SIDE_PADDING * scale).round() as i32;
        let right = ((content_right - SIDE_PADDING) * scale).round() as i32;
        if right <= left {
            return;
        }
        let text_height = metrics.text_height * scale;
        let text_rect = RECT {
            left,
            top: top.round() as i32,
            right,
            bottom: (top + text_height).round() as i32,
        };
        let available = (right - left) as f32;
        // Fixed size per surface: long lines are handled by the horizontal
        // follow below or by an ellipsis, never by shrinking the font.
        let text_width = self.measure(hdc, kind, size, &text);

        // Absolute sung width in device pixels; it also drives the follow.
        let sung_width = karaoke
            .as_ref()
            .map(|karaoke| {
                let sung = self.measure(hdc, kind, size, &karaoke.sung);
                let next = karaoke
                    .next
                    .map(|ch| {
                        let mut buffer = [0u16; 2];
                        let encoded = ch.encode_utf16(&mut buffer);
                        let len = encoded.len();
                        self.measure(hdc, kind, size, &String::from_utf16_lossy(&buffer[..len]))
                            * karaoke.next_fraction
                    })
                    .unwrap_or(0.0);
                sung + next
            })
            .unwrap_or(0.0);

        // Follow the sung edge when it reaches 7/8 of the row width, keeping a
        // 1/8 margin instead of starting only once the text is already cut.
        // Right alignment opts out; "reduce motion" keeps the line still too.
        let align = self.style.align;
        let follow = if align == LyricsAlign::Right || self.style.reduce_motion {
            0.0
        } else {
            horizontal_follow(text_width, sung_width, available, is_current)
        };
        let centered = follow <= 0.5 && text_width <= available;
        let text_left = if align == LyricsAlign::Right {
            (right as f32 - text_width).round() as i32
        } else if centered && align == LyricsAlign::Center {
            left + ((available - text_width) * 0.5).round() as i32
        } else {
            (left as f32 - follow).round() as i32
        };
        let text_dip = Rect::new(
            text_left as f32 / scale,
            text_rect.top as f32 / scale,
            right as f32 / scale,
            text_rect.bottom as f32 / scale,
        );
        // Long non-current lines are truncated; the current line is clipped
        // while the follow offset moves it (an ellipsis would flicker).
        let ellipsis = !centered && (!is_current || karaoke.is_none());
        let format = if ellipsis {
            DT_LEFT.0
                | DT_SINGLELINE.0
                | DT_VCENTER.0
                | DT_NOPREFIX.0
                | windows::Win32::Graphics::Gdi::DT_END_ELLIPSIS.0
        } else {
            DT_LEFT.0 | DT_SINGLELINE.0 | DT_VCENTER.0 | DT_NOPREFIX.0
        };
        // Clip all text to the row: the follow offset can start left of the
        // side padding while the tail stays visible.
        let text_clip = unsafe { SaveDC(hdc) };
        unsafe {
            let _ = IntersectClipRect(hdc, left, text_rect.top, right, text_rect.bottom);
        }
        if self.style.shadow_offset > 0.0 {
            let shadow = Rect::new(
                text_dip.left + 1.0,
                text_dip.top + self.style.shadow_offset,
                text_dip.right + 1.0,
                text_dip.bottom + self.style.shadow_offset,
            );
            self.text(hdc, kind, size, &text, shadow, self.style.shadow_color, format);
        }
        if self.style.outline_width > 0.0 {
            let radius = self.style.outline_width.ceil().clamp(1.0, 3.0) as i32;
            for dy in -radius..=radius {
                for dx in -radius..=radius {
                    if dx == 0 && dy == 0 {
                        continue;
                    }
                    let outlined = Rect::new(
                        text_dip.left + dx as f32,
                        text_dip.top + dy as f32,
                        text_dip.right + dx as f32,
                        text_dip.bottom + dy as f32,
                    );
                    self.text(hdc, kind, size, &text, outlined, self.style.outline_color, format);
                }
            }
        }
        self.text(
            hdc,
            kind,
            size,
            &text,
            text_dip,
            if is_current { self.style.current_color } else { self.style.normal_color },
            format,
        );

        // Karaoke fill: redraw the same text in the accent color, clipped to
        // the sung prefix and shifted by the same follow offset. Translation
        // rows never participate.
        if sung_width > 0.5 {
            let clip_left = (text_left as f32).max(left as f32);
            let clip_right = (text_left as f32 + sung_width).min(right as f32);
            if clip_right > clip_left {
                let saved = unsafe { SaveDC(hdc) };
                unsafe {
                    let _ = IntersectClipRect(
                        hdc,
                        clip_left.floor() as i32,
                        text_rect.top,
                        clip_right.ceil() as i32,
                        text_rect.bottom,
                    );
                }
                self.text(
                    hdc,
                    kind,
                    size,
                    &text,
                    text_dip,
                    self.style.highlight_color,
                    DT_LEFT.0 | DT_SINGLELINE.0 | DT_VCENTER.0 | DT_NOPREFIX.0,
                );
                unsafe {
                    let _ = RestoreDC(hdc, saved);
                }
            }
        }
        unsafe {
            let _ = RestoreDC(hdc, text_clip);
        }

        if metrics.translation_height > 0.0
            && let Some(translation) = translation
        {
            let translation = translation.replace('\n', " / ");
            if !translation.trim().is_empty() {
                let translation_top = top + text_height;
                let translation_rect = Rect::new(
                    SIDE_PADDING,
                    translation_top / scale,
                    content_right - SIDE_PADDING,
                    (translation_top + metrics.translation_height * scale) / scale,
                );
                let format = align_flags(self.style.align)
                    | DT_SINGLELINE.0
                    | DT_VCENTER.0
                    | DT_NOPREFIX.0
                    | windows::Win32::Graphics::Gdi::DT_END_ELLIPSIS.0;
                if self.style.translation_shadow_offset > 0.0 {
                    let shadow = Rect::new(
                        translation_rect.left + 1.0,
                        translation_rect.top + self.style.translation_shadow_offset,
                        translation_rect.right + 1.0,
                        translation_rect.bottom + self.style.translation_shadow_offset,
                    );
                    self.text(
                        hdc,
                        FontKind::Translation,
                        self.style.translation_size,
                        translation.trim(),
                        shadow,
                        self.style.translation_shadow_color,
                        format,
                    );
                }
                if self.style.translation_outline_width > 0.0 {
                    let radius = self.style.translation_outline_width.ceil().clamp(1.0, 2.0) as i32;
                    for dy in -radius..=radius {
                        for dx in -radius..=radius {
                            if dx == 0 && dy == 0 {
                                continue;
                            }
                            let outlined = Rect::new(
                                translation_rect.left + dx as f32,
                                translation_rect.top + dy as f32,
                                translation_rect.right + dx as f32,
                                translation_rect.bottom + dy as f32,
                            );
                            self.text(
                                hdc,
                                FontKind::Translation,
                                self.style.translation_size,
                                translation.trim(),
                                outlined,
                                self.style.translation_outline_color,
                                format,
                            );
                        }
                    }
                }
                self.text(
                    hdc,
                    FontKind::Translation,
                    self.style.translation_size,
                    translation.trim(),
                    translation_rect,
                    self.style.translation_color,
                    format,
                );
            }
        }
    }

    fn draw_scrollbar(&mut self, hdc: HDC, viewport: f32, scale: f32) {
        // The standalone window scrolls by wheel only; a bar would break the
        // immersive layout.
        if self.floating {
            return;
        }
        let content = self.view.total_height();
        if content <= viewport || viewport <= 0.0 {
            return;
        }
        let width = self.client.0 as f32 / scale;
        let track = Rect::new(width - SCROLLBAR_WIDTH, 0.0, width, viewport);
        self.fill(hdc, track, self.style.palette.scroll_track);
        let thumb_height = (viewport * viewport / content)
            .max(24.0)
            .min(viewport);
        let travel = (viewport - thumb_height).max(0.0);
        let min_scroll = self.view.min_scroll(viewport);
        let max_scroll = self.view.max_scroll(viewport);
        let span = (max_scroll - min_scroll).max(0.0);
        let ratio = if span > 0.0 {
            ((self.scroll - min_scroll) / span).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let thumb_top = travel * ratio;
        let color = if self.dragging || self.bar_hover {
            self.style.palette.scroll_thumb_hover
        } else {
            self.style.palette.scroll_thumb
        };
        self.fill(
            hdc,
            Rect::new(
                width - SCROLLBAR_WIDTH + 2.0,
                thumb_top,
                width - 2.0,
                thumb_top + thumb_height,
            ),
            color,
        );
    }

    // ------------------------------------------------------------------
    // Input
    // ------------------------------------------------------------------

    fn scroll_to(&mut self, hwnd: HWND, scroll: f32) {
        let mut client = RECT::default();
        let _ = unsafe { GetClientRect(hwnd, &mut client) };
        let viewport = client.bottom as f32 / self.scale();
        let min_scroll = self.view.min_scroll(viewport);
        let max_scroll = self.view.max_scroll(viewport);
        let next = scroll.clamp(min_scroll, max_scroll);
        if (next - self.scroll).abs() < 0.5 {
            return;
        }
        self.scroll = next;
        self.invalidate(hwnd);
    }

    fn drag_scrollbar(&mut self, hwnd: HWND, y: f32) {
        let mut client = RECT::default();
        let _ = unsafe { GetClientRect(hwnd, &mut client) };
        let viewport = client.bottom as f32 / self.scale();
        let content = self.view.total_height();
        if content <= viewport || viewport <= 0.0 {
            return;
        }
        let thumb = (viewport * viewport / content).max(24.0).min(viewport);
        let travel = (viewport - thumb).max(1.0);
        let ratio = ((y - thumb * 0.5) / travel).clamp(0.0, 1.0);
        let min_scroll = self.view.min_scroll(viewport);
        let max_scroll = self.view.max_scroll(viewport);
        let target = min_scroll + ratio * (max_scroll - min_scroll);
        if (target - self.scroll).abs() < 0.5 {
            return;
        }
        self.scroll = target;
        self.manual_until = Some(Instant::now());
        self.invalidate(hwnd);
    }

    fn on_move(&mut self, hwnd: HWND, x: f32, y: f32) {
        if self.dragging {
            self.drag_scrollbar(hwnd, y);
            return;
        }
        let mut client = RECT::default();
        let _ = unsafe { GetClientRect(hwnd, &mut client) };
        let width = client.right as f32 / self.scale();
        let viewport = client.bottom as f32 / self.scale();
        let over_bar = !self.floating
            && x >= width - SCROLLBAR_WIDTH - 2.0
            && self.view.total_height() > viewport;
        if over_bar {
            if !self.bar_hover {
                self.bar_hover = true;
                self.invalidate(hwnd);
            }
            if self.hover.take().is_some() {
                self.invalidate(hwnd);
            }
            return;
        }
        if self.bar_hover {
            self.bar_hover = false;
            self.invalidate(hwnd);
        }
        let hover = self.view.hit_test(self.scroll, viewport, y);
        if hover != self.hover {
            let old = self.hover;
            self.hover = hover;
            if let Some(old) = old {
                self.invalidate_line(hwnd, old);
            }
            if let Some(new) = hover {
                self.invalidate_line(hwnd, new);
            }
        }
    }

    fn on_press(&mut self, hwnd: HWND, x: f32, y: f32) {
        let mut client = RECT::default();
        let _ = unsafe { GetClientRect(hwnd, &mut client) };
        let width = client.right as f32 / self.scale();
        let viewport = client.bottom as f32 / self.scale();
        let over_bar = !self.floating
            && x >= width - SCROLLBAR_WIDTH - 2.0
            && self.view.total_height() > viewport;
        if over_bar {
            self.dragging = true;
            self.manual_until = Some(Instant::now());
            unsafe {
                SetCapture(hwnd);
            }
            self.drag_scrollbar(hwnd, y);
            self.invalidate(hwnd);
            return;
        }
        let index = self.view.hit_test(self.scroll, viewport, y);
        if index != self.pressed {
            let old = self.pressed;
            self.pressed = index;
            if let Some(old) = old {
                self.invalidate_line(hwnd, old);
            }
            if let Some(new) = index {
                self.invalidate_line(hwnd, new);
            }
        }
    }

    fn on_release(&mut self, hwnd: HWND, x: f32, y: f32) {
        if self.dragging {
            self.dragging = false;
            unsafe {
                let _ = ReleaseCapture();
            }
            self.invalidate(hwnd);
            return;
        }
        let mut client = RECT::default();
        let _ = unsafe { GetClientRect(hwnd, &mut client) };
        let viewport = client.bottom as f32 / self.scale();
        let pressed = self.pressed.take();
        let hit = self.view.hit_test(self.scroll, viewport, y);
        let _ = x;
        if let Some(index) = pressed {
            self.invalidate_line(hwnd, index);
            if hit == Some(index)
                && let Some(lyrics) = self.lyrics.as_ref()
                && lyrics.timed
                && let Some(line) = lyrics.lines.get(index)
            {
                self.manual_until = None;
                unsafe {
                    let _ = PostMessageW(
                        Some(self.parent),
                        WM_APP_LYRIC_SEEK,
                        WPARAM(line.start_ms as usize),
                        LPARAM(0),
                    );
                }
            }
        }
    }

    fn on_wheel(&mut self, hwnd: HWND, delta: f32, _x: f32, _y: f32) {
        let rows = -(delta / 120.0) * 3.0;
        let step = rows * (view::DEFAULT_LINE_HEIGHT + view::DEFAULT_LINE_GAP);
        let target = self.scroll + step;
        let before = self.scroll;
        self.scroll_to(hwnd, target);
        if (self.scroll - before).abs() > 0.5 {
            self.manual_until = Some(Instant::now());
        }
    }

    // ------------------------------------------------------------------
    // GDI helpers
    // ------------------------------------------------------------------

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

    fn fill(&mut self, hdc: HDC, area: Rect, color: COLORREF) {
        if area.width() <= 0.0 || area.height() <= 0.0 {
            return;
        }
        let scale = self.scale();
        let rect = RECT {
            left: (area.left * scale).floor() as i32,
            top: (area.top * scale).floor() as i32,
            right: (area.right * scale).ceil() as i32,
            bottom: (area.bottom * scale).ceil() as i32,
        };
        let brush = self.brush(color);
        unsafe {
            FillRect(hdc, &rect, brush);
        }
    }

    fn fill_gradient(
        &mut self,
        hdc: HDC,
        area: Rect,
        top: COLORREF,
        bottom: COLORREF,
    ) {
        if area.width() <= 0.0 || area.height() <= 0.0 {
            return;
        }
        let scale = self.scale();
        let rect = RECT {
            left: (area.left * scale).floor() as i32,
            top: (area.top * scale).floor() as i32,
            right: (area.right * scale).ceil() as i32,
            bottom: (area.bottom * scale).ceil() as i32,
        };
        let vertex = |color: COLORREF, x: i32, y: i32| {
            let value = color.0;
            TRIVERTEX {
                x,
                y,
                Red: ((value & 0xff) << 8) as u16,
                Green: (((value >> 8) & 0xff) << 8) as u16,
                Blue: (((value >> 16) & 0xff) << 8) as u16,
                Alpha: u16::MAX,
            }
        };
        let mut vertices = [
            vertex(top, rect.left, rect.top),
            vertex(bottom, rect.right, rect.bottom),
        ];
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

    fn draw_panel_image(&mut self, hdc: HDC, area: Rect) -> bool {
        let Some(image) = self.panel_image.as_ref() else {
            return false;
        };
        if self.panel_image_bgra.is_none() {
            let (w, h) = (image.size[0] as i32, image.size[1] as i32);
            if w <= 0 || h <= 0 || image.pixels.len() < (w * h * 4) as usize {
                return false;
            }
            let mut bgra = Vec::with_capacity(image.pixels.len());
            for px in image.pixels.chunks_exact(4) {
                bgra.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
            }
            self.panel_image_bgra = Some((bgra, w, h));
        }
        let Some((bgra, width, height)) = self.panel_image_bgra.as_ref() else {
            return false;
        };
        let scale = self.scale();
        let dest = RECT {
            left: (area.left * scale).floor() as i32,
            top: (area.top * scale).floor() as i32,
            right: (area.right * scale).ceil() as i32,
            bottom: (area.bottom * scale).ceil() as i32,
        };
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: *width,
                biHeight: -*height,
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
                *width,
                *height,
                Some(bgra.as_ptr() as *const core::ffi::c_void),
                &info,
                DIB_RGB_COLORS,
                SRCCOPY,
            );
        }
        true
    }

    fn brush(&mut self, color: COLORREF) -> HBRUSH {
        if let Some(brush) = self.brushes.get(&color.0) {
            return *brush;
        }
        let brush = unsafe { CreateSolidBrush(color) };
        self.brushes.insert(color.0, brush);
        brush
    }

    /// Filled ellipse with no outline, used for the immersive background.
    fn ellipse(&mut self, hdc: HDC, area: Rect, color: COLORREF) {
        if area.width() <= 1.0 || area.height() <= 1.0 {
            return;
        }
        let scale = self.scale();
        let rect = RECT {
            left: (area.left * scale).floor() as i32,
            top: (area.top * scale).floor() as i32,
            right: (area.right * scale).ceil() as i32,
            bottom: (area.bottom * scale).ceil() as i32,
        };
        let brush = self.brush(color);
        unsafe {
            let old_brush = SelectObject(hdc, HGDIOBJ(brush.0));
            let old_pen = SelectObject(hdc, GetStockObject(NULL_PEN));
            let _ = Ellipse(hdc, rect.left, rect.top, rect.right, rect.bottom);
            let _ = SelectObject(hdc, old_pen);
            let _ = SelectObject(hdc, old_brush);
        }
    }

    /// Parent client size (DIPs) and this child's offset inside it, matching
    /// `lyrics_screen::compute` so the decorative blobs line up.
    fn floating_blobs(&self, hwnd: HWND) -> (f32, f32, f32, f32) {
        let scale = self.scale();
        let parent = unsafe { GetParent(hwnd) }.unwrap_or_default();
        let mut client = RECT::default();
        if parent.is_invalid() {
            return (self.client.0 as f32 / scale, self.client.1 as f32 / scale, 0.0, 0.0);
        }
        let _ = unsafe { GetClientRect(parent, &mut client) };
        let mut points = [POINT::default()];
        unsafe {
            MapWindowPoints(Some(hwnd), Some(parent), &mut points);
        }
        (
            client.right as f32 / scale,
            client.bottom as f32 / scale,
            points[0].x as f32 / scale,
            points[0].y as f32 / scale,
        )
    }

    fn font(&mut self, kind: FontKind, size: f32) -> HFONT {
        if (self.font_dpi - self.dpi).abs() > 0.5 {
            self.clear_fonts();
            self.font_dpi = self.dpi;
        }
        let size = size.max(8.0);
        let key = (kind as u8, (size * 10.0).round() as u16);
        if let Some(font) = self.fonts.get(&key) {
            return *font;
        }
        let weight = if kind == FontKind::Current {
            (self.style.font_weight as i32 + 200).min(900) as i32
        } else {
            self.style.font_weight as i32
        };
        let height = -((size * self.dpi / 96.0).round().max(1.0) as i32);
        let face_name = self
            .style
            .font_family
            .as_deref()
            .map(str::trim)
            .filter(|family| !family.is_empty())
            .unwrap_or("Microsoft YaHei UI");
        let face: Vec<u16> = face_name.encode_utf16().chain([0]).collect();
        let font = unsafe {
            CreateFontW(
                height,
                0,
                0,
                0,
                weight,
                if self.style.italic { 1 } else { 0 },
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

    fn clear_fonts(&mut self) {
        for (_, font) in self.fonts.drain() {
            unsafe {
                let _ = DeleteObject(HGDIOBJ(font.0));
            }
        }
    }

    fn measure(&mut self, hdc: HDC, kind: FontKind, size: f32, text: &str) -> f32 {
        if text.is_empty() {
            return 0.0;
        }
        let font = self.font(kind, size);
        let wide: Vec<u16> = text.encode_utf16().collect();
        let mut size = SIZE::default();
        unsafe {
            SelectObject(hdc, HGDIOBJ(font.0));
            let _ = GetTextExtentPoint32W(hdc, &wide, &mut size);
        }
        size.cx as f32
    }

    #[allow(clippy::too_many_arguments)]
    fn text(
        &mut self,
        hdc: HDC,
        kind: FontKind,
        size: f32,
        text: &str,
        area: Rect,
        color: COLORREF,
        format: u32,
    ) {
        if text.is_empty() || area.width() <= 1.0 {
            return;
        }
        let scale = self.scale();
        let font = self.font(kind, size);
        let mut rect = RECT {
            left: (area.left * scale).floor() as i32,
            top: (area.top * scale).floor() as i32,
            right: (area.right * scale).ceil() as i32,
            bottom: (area.bottom * scale).ceil() as i32,
        };
        let mut wide: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            SelectObject(hdc, HGDIOBJ(font.0));
            SetTextColor(hdc, color);
            SetBkMode(hdc, TRANSPARENT);
            DrawTextW(hdc, &mut wide, &mut rect, DRAW_TEXT_FORMAT(format));
        }
    }
}

impl Drop for State {
    fn drop(&mut self) {
        self.release_back_buffer();
        self.clear_fonts();
        for (_, brush) in self.brushes.drain() {
            unsafe {
                let _ = DeleteObject(HGDIOBJ(brush.0));
            }
        }
    }
}

/// The lyric list control. The real state lives in the child window's user
/// data so the window procedure never borrows the parent's `State`.
pub struct LyricsChild {
    hwnd: HWND,
}

impl LyricsChild {
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
                WS_CHILD | WS_CLIPSIBLINGS,
                0,
                0,
                10,
                10,
                Some(parent),
                None,
                Some(HINSTANCE(instance.0)),
                None,
            )?;
            let state = Box::new(State::new(parent));
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(state) as isize);
            Ok(Self { hwnd })
        }
    }

    /// Repaint everything on the next update (parent became visible again).
    pub fn mark_dirty(&mut self) {
        if let Some(state) = state_mut(self.hwnd) {
            state.needs_full = true;
            if state.visible {
                state.invalidate(self.hwnd);
            }
        }
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// Sync geometry, document, current line and karaoke progress.
    pub fn update(&mut self, _parent: HWND, app: &App) {
        let Some(state) = state_mut(self.hwnd) else {
            return;
        };
        state.floating = false;
        state.dpi = app.dpi;
        let area = app.layout.lyrics;
        let show = app.page == Page::NowPlaying && area.width() > 40.0 && area.height() > 40.0;
        if !show {
            state.hide(self.hwnd);
            return;
        }
        if !app.window_visible {
            return;
        }

        let scale = app.dpi / 96.0;
        let x = (area.left * scale).round() as i32;
        let y = (area.top * scale).round() as i32;
        let width = ((area.width() * scale).round() as i32).max(10);
        let height = ((area.height() * scale).round() as i32).max(10);
        let resized = state.last_size != (width, height);
        state.last_size = (width, height);
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                None,
                x,
                y,
                width,
                height,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            if !state.visible {
                let _ = ShowWindow(self.hwnd, SW_SHOWNA);
                state.visible = true;
                state.needs_full = true;
            }
        }
        if resized {
            state.needs_full = true;
            state.invalidate(self.hwnd);
        }

        let viewport = area.height();
        state.sync_content(self.hwnd, app, viewport);
    }

    /// Sync a lyrics child that fills a standalone top-level window.
    ///
    /// Unlike [`LyricsChild::update`] this ignores the main window's page and
    /// visibility: the window may live on another display while the player is
    /// hidden or minimized.
    pub fn update_floating(&mut self, app: &App) {
        let Some(state) = state_mut(self.hwnd) else {
            return;
        };
        state.floating = true;
        let dpi = unsafe { GetDpiForWindow(self.hwnd) };
        state.dpi = if dpi > 0 { dpi as f32 } else { 96.0 };
        let mut client = RECT::default();
        let _ = unsafe { GetClientRect(self.hwnd, &mut client) };
        if client.right < 40 || client.bottom < 40 {
            return;
        }
        if !state.visible {
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_SHOWNA);
            }
            state.visible = true;
            state.needs_full = true;
        }
        let viewport = client.bottom as f32 / state.scale();
        state.sync_content(self.hwnd, app, viewport);
    }
}

impl State {
    /// Document, current line and invalidation pass shared by the embedded
    /// panel and the standalone window. The caller owns geometry and viewport.
    fn sync_content(&mut self, hwnd: HWND, app: &App, viewport: f32) {
        let snapshot = app.core.snapshot();
        let mut repaint_all = false;

        if (self.font_dpi - self.dpi).abs() > 0.5 {
            self.clear_fonts();
            self.font_dpi = self.dpi;
            repaint_all = true;
        }

        // Appearance settings feed the view config, the fonts and every color.
        // A single comparison catches all of them and triggers one full
        // repaint plus a font-cache reset.
        let style = Style::from_app(app, self.floating);
        if !self.styled || self.style != style {
            self.style = style.clone();
            self.styled = true;
            self.clear_fonts();
            self.font_dpi = self.dpi;
            repaint_all = true;
        }
        if self.style.panel_image_key.is_some() {
            if let Some(image) = snapshot.lyrics_background {
                let pointer = image as *const RgbaImage as usize;
                if self.panel_image.is_none() || pointer != self.panel_image_ptr {
                    self.panel_image = Some(image.clone());
                    self.panel_image_ptr = pointer;
                    self.panel_image_bgra = None;
                    repaint_all = true;
                }
            } else if self.panel_image.is_some() {
                self.panel_image = None;
                self.panel_image_ptr = 0;
                self.panel_image_bgra = None;
                repaint_all = true;
            }
        } else if self.panel_image.is_some() {
            self.panel_image = None;
            self.panel_image_ptr = 0;
            self.panel_image_bgra = None;
            repaint_all = true;
        }
        let config = ViewConfig {
            line_height: style.line_height,
            translation_height: style.translation_height,
            gap: style.gap,
            show_translation: style.show_translation,
        };
        if self.view.set_config(config) {
            self.view.rebuild(self.lyrics.as_ref());
            repaint_all = true;
        }

        if self.revision != snapshot.lyrics_rev {
            self.revision = snapshot.lyrics_rev;
            self.lyrics = snapshot.lyrics.cloned();
            self.view.rebuild(self.lyrics.as_ref());
            self.current = None;
            self.progress = 0.0;
            self.hover = None;
            self.pressed = None;
            self.scroll = 0.0;
            self.manual_until = None;
            self.empty_hint = if snapshot.lyrics_loading {
                crate::i18n::tr("歌词加载中…").to_string()
            } else if let Some(error) = snapshot.lyrics_error {
                error.to_string()
            } else {
                crate::i18n::tr("暂无歌词").to_string()
            };
            self.empty_sub_hint = if snapshot.lyrics_loading {
                String::new()
            } else {
                crate::i18n::tr("拖放或选择 .lrc/.srt 文件").to_string()
            };
            repaint_all = true;
        }

        let min_scroll = self.view.min_scroll(viewport);
        let max_scroll = self.view.max_scroll(viewport);
        self.scroll = self.scroll.clamp(min_scroll, max_scroll);

        let position_ms = (snapshot.position_secs * 1000.0).max(0.0) as u64;
        let lyric_position_ms =
            (position_ms as i64 + self.style.offset_ms).max(0) as u64;
        let (index, progress) = match self
            .lyrics
            .as_ref()
            .and_then(|lyrics| view::current_line(lyrics, lyric_position_ms))
        {
            Some((index, progress)) => (Some(index), progress),
            None => (None, 0.0),
        };
        let index_changed = index != self.current;
        let progress_changed = index == self.current && (progress - self.progress).abs() > 0.0005;
        self.current = index;
        self.progress = progress;
        self.position_ms = position_ms;

        // Manual scrolling suspends auto centering; once it expires the list
        // snaps back to the current line even if the line did not change.
        if self
            .manual_until
            .is_some_and(|until| until.elapsed() >= AUTO_CENTER_PAUSE)
        {
            self.manual_until = None;
            if let Some(index) = index {
                self.scroll = self.view.centered_scroll(index, viewport);
            }
            repaint_all = true;
        }

        if index_changed {
            let manual = self
                .manual_until
                .is_some_and(|until| until.elapsed() < AUTO_CENTER_PAUSE);
            if !manual
                && let Some(index) = index
            {
                self.scroll = self.view.centered_scroll(index, viewport);
            }
            repaint_all = true;
        }

        if repaint_all || self.needs_full {
            self.needs_full = false;
            self.invalidate(hwnd);
        } else if progress_changed && let Some(index) = index {
            self.invalidate_line(hwnd, index);
        }
    }
}

fn state_mut(hwnd: HWND) -> Option<&'static mut State> {
    let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut State;
    unsafe { pointer.as_mut() }
}

fn point_from_lparam(lparam: LPARAM) -> (f32, f32) {
    let x = (lparam.0 & 0xFFFF) as i16 as f32;
    let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32;
    (x, y)
}

fn track_mouse_leave(hwnd: HWND) {
    let mut event = TRACKMOUSEEVENT {
        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
        dwFlags: TME_LEAVE,
        hwndTrack: hwnd,
        dwHoverTime: 0,
    };
    let _ = unsafe { TrackMouseEvent(&mut event) };
}

unsafe extern "system" fn child_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let _ = unsafe { BeginPaint(hwnd, &mut ps) };
            if let Some(state) = state_mut(hwnd) {
                state.paint(hwnd, ps.hdc, ps.rcPaint);
            }
            let _ = unsafe { EndPaint(hwnd, &ps) };
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_SIZE => {
            if let Some(state) = state_mut(hwnd) {
                state.needs_full = true;
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            track_mouse_leave(hwnd);
            if let Some(state) = state_mut(hwnd) {
                let scale = state.scale();
                let (x, y) = point_from_lparam(lparam);
                state.on_move(hwnd, x / scale, y / scale);
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            if let Some(state) = state_mut(hwnd) {
                if state.hover.take().is_some() || state.bar_hover {
                    state.bar_hover = false;
                    state.invalidate(hwnd);
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            if let Some(state) = state_mut(hwnd) {
                // Keep keyboard shortcuts (space/arrows) on the player.
                let _ = unsafe { SetFocus(Some(state.parent)) };
                let scale = state.scale();
                let (x, y) = point_from_lparam(lparam);
                state.on_press(hwnd, x / scale, y / scale);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            if let Some(state) = state_mut(hwnd) {
                let scale = state.scale();
                let (x, y) = point_from_lparam(lparam);
                state.on_release(hwnd, x / scale, y / scale);
            }
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            // Report the line under the cursor; the parent owns the menu.
            if let Some(state) = state_mut(hwnd) {
                let mut client = RECT::default();
                let _ = unsafe { GetClientRect(hwnd, &mut client) };
                let scale = state.scale();
                let viewport = client.bottom as f32 / scale;
                let (_, y) = point_from_lparam(lparam);
                let line = state.view.hit_test(state.scroll, viewport, y / scale);
                let payload = line.map(|index| index + 1).unwrap_or(0);
                let _ = unsafe {
                    PostMessageW(
                        Some(state.parent),
                        WM_APP_LYRIC_MENU,
                        WPARAM(payload),
                        LPARAM(0),
                    )
                };
            }
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            if let Some(state) = state_mut(hwnd) {
                let delta = ((wparam.0 >> 16) & 0xFFFF) as u16 as i16 as f32;
                let scale = state.scale();
                let (x, y) = point_from_lparam(lparam);
                state.on_wheel(hwnd, delta, x / scale, y / scale);
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            // Keyboard shortcuts stay with the window that owns the lyrics:
            // the player for the embedded panel, the standalone window (which
            // handles F11 and forwards the rest) for the floating one.
            if let Some(state) = state_mut(hwnd) {
                let _ = unsafe { PostMessageW(Some(state.parent), WM_KEYDOWN, wparam, lparam) };
            }
            LRESULT(0)
        }
        WM_NCDESTROY => {
            let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut State;
            if !pointer.is_null() {
                unsafe {
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                    drop(Box::from_raw(pointer));
                }
            }
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF(((b as u32) << 16) | ((g as u32) << 8) | r as u32)
}

/// Horizontal follow offset (device pixels) for a long current lyric line.
///
/// The line starts moving left when the sung edge reaches 7/8 of the visible
/// width, keeping a 1/8 margin on the right instead of waiting until the text
/// is already clipped at the edge. Non-current lines and lines that fit never
/// move.
fn horizontal_follow(text_width: f32, sung_width: f32, available: f32, current: bool) -> f32 {
    if !current || available <= 0.0 || text_width <= available {
        return 0.0;
    }
    let threshold = available * 7.0 / 8.0;
    (sung_width - threshold).clamp(0.0, text_width - available + available / 8.0)
}

#[cfg(test)]
mod tests {
    use super::horizontal_follow;

    #[test]
    fn follow_starts_at_seven_eighths() {
        let available = 800.0;
        // 690 px sung: still before the 7/8 trigger (700 px).
        assert_eq!(horizontal_follow(1600.0, 690.0, available, true), 0.0);
        // 710 px sung: 10 px past the trigger, the text follows by 10 px.
        assert!((horizontal_follow(1600.0, 710.0, available, true) - 10.0).abs() < 0.01);
    }

    #[test]
    fn short_lines_and_other_rows_never_follow() {
        assert_eq!(horizontal_follow(700.0, 700.0, 800.0, true), 0.0);
        assert_eq!(horizontal_follow(1600.0, 1600.0, 800.0, false), 0.0);
    }

    #[test]
    fn follow_keeps_a_trailing_margin() {
        let available = 800.0;
        // At the end of the line the tail stops 1/8 before the right edge.
        let max = horizontal_follow(1600.0, 1600.0, available, true);
        assert!((max - (1600.0 - available + available / 8.0)).abs() < 0.01);
    }
}
