//! Immersive layout, painting and input for the standalone lyrics window.
//!
//! The window is borderless: this module draws the whole shell (brand,
//! fullscreen/close icons, cover + track info on the left, progress and
//! transport at the bottom) and positions the lyric child in the right
//! column. Everything is GDI into the window's own persistent back buffer;
//! only the rectangles that changed are invalidated.
//!
//! The window procedure owns the input plumbing (see `lyrics_window`); this
//! module only maps points to [`Action`]s so the window can post playback
//! commands to the player.

use std::sync::Arc;

use windows::Win32::Foundation::{COLORREF, HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    DT_CENTER, DT_LEFT, DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE, DT_VCENTER, HDC, InvalidateRect,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    GetClientRect, GetWindowRect, SWP_NOACTIVATE, SWP_NOZORDER, SetWindowPos,
};

use crate::app::App;
use crate::core::util::image_util::RgbaImage;
use crate::layout::Rect;
use crate::paint::{FontKind, Painter};

const C_BG: COLORREF = rgb(13, 15, 17);
const C_BLOB: COLORREF = rgb(21, 31, 27);
const C_BRAND: COLORREF = rgb(112, 121, 133);
const C_COVER: COLORREF = rgb(37, 43, 49);
const C_COVER_GLYPH: COLORREF = rgb(74, 84, 94);
const C_TRACK_LINE: COLORREF = rgb(49, 54, 60);
const C_TIME: COLORREF = rgb(138, 147, 160);

/// Colors that follow the large-lyrics settings and the accent color. The
/// immersive background stays dark by design.
#[derive(Clone, Copy, PartialEq)]
struct ScreenStyle {
    title: COLORREF,
    artist: COLORREF,
    album: COLORREF,
    accent: COLORREF,
    icon: COLORREF,
    icon_hover: COLORREF,
    icon_bg: COLORREF,
    icon_press: COLORREF,
    show_cover: bool,
    show_title: bool,
    show_footer: bool,
}

impl Default for ScreenStyle {
    fn default() -> Self {
        Self {
            title: rgb(235, 238, 242),
            artist: rgb(167, 175, 186),
            album: rgb(124, 133, 145),
            accent: rgb(127, 211, 168),
            icon: rgb(198, 206, 216),
            icon_hover: rgb(240, 244, 248),
            icon_bg: rgb(31, 35, 40),
            icon_press: rgb(43, 58, 51),
            show_cover: true,
            show_title: true,
            show_footer: true,
        }
    }
}

impl ScreenStyle {
    fn from_app(app: &App) -> Self {
        let large = &app.core.settings.large_lyrics;
        let palette = app.palette();
        Self {
            title: rgb3(large.current_color),
            artist: rgb3(large.normal_color),
            album: rgb3(large.translation_color),
            accent: palette.accent_text,
            icon: rgb(198, 206, 216),
            icon_hover: rgb(240, 244, 248),
            icon_bg: rgb(31, 35, 40),
            icon_press: palette.accent,
            show_cover: large.show_cover,
            show_title: large.show_title,
            show_footer: large.show_footer,
        }
    }
}

const fn rgb3(color: [u8; 3]) -> COLORREF {
    COLORREF(((color[2] as u32) << 16) | ((color[1] as u32) << 8) | color[0] as u32)
}

const TOP_BAR: f32 = 64.0;
const FOOTER: f32 = 150.0;
const ICON_SIZE: f32 = 28.0;

#[derive(Clone, Copy)]
pub struct ScreenLayout {
    pub size: (f32, f32),
    pub brand: Rect,
    pub fullscreen: Rect,
    pub close: Rect,
    pub cover: Rect,
    pub title: Rect,
    pub artist: Rect,
    pub album: Rect,
    pub lyrics: Rect,
    pub time_left: Rect,
    pub time_right: Rect,
    pub progress: Rect,
    pub progress_hit: Rect,
    pub prev: Rect,
    pub play: Rect,
    pub next: Rect,
}

pub fn compute(width: f32, height: f32) -> ScreenLayout {
    let pad = (width * 0.045).clamp(20.0, 72.0);
    let content_top = TOP_BAR + 24.0;
    let content_bottom = (height - FOOTER).max(content_top + 96.0);

    let gap = 10.0;
    let close = Rect::new(
        width - pad - ICON_SIZE,
        (TOP_BAR - ICON_SIZE) * 0.5,
        width - pad,
        (TOP_BAR + ICON_SIZE) * 0.5,
    );
    let fullscreen = Rect::new(
        close.left - gap - ICON_SIZE,
        close.top,
        close.left - gap,
        close.bottom,
    );
    let brand = Rect::new(pad, 0.0, fullscreen.left - 24.0, TOP_BAR);

    // Left column: cover, then track title / artist / album.
    let column_width = (width * 0.32).clamp(180.0, 420.0);
    let cover_size = column_width
        .min((content_bottom - content_top) * 0.62)
        .max(64.0);
    let cover = Rect::new(pad, content_top, pad + cover_size, content_top + cover_size);
    let title = Rect::new(
        cover.left,
        cover.bottom + 22.0,
        cover.right,
        cover.bottom + 52.0,
    );
    let artist = Rect::new(
        cover.left,
        title.bottom + 4.0,
        cover.right,
        title.bottom + 26.0,
    );
    let album = Rect::new(
        cover.left,
        artist.bottom + 2.0,
        cover.right,
        artist.bottom + 22.0,
    );

    // Lyrics column: everything right of the cover column.
    let lyrics = Rect::new(
        (cover.right + 48.0).min(width - pad - 160.0),
        content_top,
        width - pad,
        content_bottom - 4.0,
    );

    // Footer: times, progress, transport.
    let footer_top = height - FOOTER + 6.0;
    let time_left = Rect::new(pad, footer_top, pad + 90.0, footer_top + 18.0);
    let time_right = Rect::new(
        width - pad - 90.0,
        footer_top,
        width - pad,
        footer_top + 18.0,
    );
    let progress = Rect::new(pad, footer_top + 26.0, width - pad, footer_top + 30.0);
    let progress_hit = progress.inflate(10.0);

    let row_center = footer_top + 66.0;
    let center = width * 0.5;
    let play = Rect::new(center - 22.0, row_center - 22.0, center + 22.0, row_center + 22.0);
    let prev = Rect::new(
        play.left - 30.0 - 30.0,
        row_center - 15.0,
        play.left - 30.0,
        row_center + 15.0,
    );
    let next = Rect::new(
        play.right + 30.0,
        row_center - 15.0,
        play.right + 30.0 + 30.0,
        row_center + 15.0,
    );

    ScreenLayout {
        size: (width, height),
        brand,
        fullscreen,
        close,
        cover,
        title,
        artist,
        album,
        lyrics,
        time_left,
        time_right,
        progress,
        progress_hit,
        prev,
        play,
        next,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Close,
    Fullscreen,
    Prev,
    Play,
    Next,
    Progress,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Close,
    Fullscreen,
    Prev,
    PlayPause,
    Next,
    Seek(u64),
    /// A click on the empty top bar: hand the move loop to the system.
    Caption,
}

#[derive(Clone, Default)]
struct ScreenData {
    title: String,
    artist: String,
    album: String,
    position: f64,
    duration: f64,
    playing: bool,
    has_track: bool,
    windowed: bool,
    cover: Option<Arc<RgbaImage>>,
    cover_rev: u64,
}

pub struct Screen {
    painter: Painter,
    layout: ScreenLayout,
    data: ScreenData,
    style: ScreenStyle,
    hover: Option<Button>,
    pressed: Option<Button>,
    dragging_progress: bool,
    /// Preview position (seconds) while dragging the footer progress bar.
    /// The player is seeked once on release, never per mouse move.
    drag_preview: Option<f64>,
    needs_full: bool,
    last_client: (i32, i32),
    dpi: f32,
}

impl Screen {
    pub fn new() -> windows::core::Result<Self> {
        Ok(Self {
            painter: Painter::new()?,
            layout: compute(900.0, 620.0),
            data: ScreenData::default(),
            style: ScreenStyle::default(),
            hover: None,
            pressed: None,
            dragging_progress: false,
            drag_preview: None,
            needs_full: true,
            last_client: (0, 0),
            dpi: 96.0,
        })
    }

    fn scale(&self) -> f32 {
        if self.dpi > 0.0 { self.dpi / 96.0 } else { 1.0 }
    }

    /// UIA nodes for the standalone lyrics shell: track text, transport,
    /// progress and the top-bar window buttons.
    pub fn uia_elements(&self, hwnd: HWND) -> Vec<crate::accessibility::UiElement> {
        use windows::Win32::UI::Accessibility::UiaRect;
        let mut window = RECT::default();
        let _ = unsafe { GetWindowRect(hwnd, &mut window) };
        let scale = self.scale();
        let to_uia = |rect: Rect| UiaRect {
            left: window.left as f64 + rect.left as f64 * scale as f64,
            top: window.top as f64 + rect.top as f64 * scale as f64,
            width: rect.width() as f64 * scale as f64,
            height: rect.height() as f64 * scale as f64,
        };
        let packed = |rect: Rect| {
            let x = ((rect.left + rect.right) * 0.5 * scale).round() as i32;
            let y = ((rect.top + rect.bottom) * 0.5 * scale).round() as i32;
            (((y as u16 as u32) << 16) | (x as u16 as u32)) as isize
        };
        let mut elements = Vec::new();
        let heading = if self.data.artist.trim().is_empty() {
            self.data.title.clone()
        } else {
            format!("{} - {}", self.data.title, self.data.artist)
        };
        if !heading.trim().is_empty() {
            elements.push(crate::accessibility::UiElement::text(
                heading,
                to_uia(self.layout.title),
            ));
        }
        for (name, rect) in [
            (crate::i18n::tr("上一首"), self.layout.prev),
            (
                crate::i18n::tr(if self.data.playing { "暂停" } else { "播放" }),
                self.layout.play,
            ),
            (crate::i18n::tr("下一首"), self.layout.next),
            (crate::i18n::tr("全屏 (F11)"), self.layout.fullscreen),
            (crate::i18n::tr("关闭"), self.layout.close),
        ] {
            elements.push(crate::accessibility::UiElement::button(
                name,
                to_uia(rect),
                packed(rect),
            ));
        }
        let duration = self.data.duration.max(0.0);
        elements.push(crate::accessibility::UiElement::slider(
            crate::i18n::tr("播放进度"),
            to_uia(self.layout.progress_hit),
            packed(self.layout.progress_hit),
            self.data.position.max(0.0),
            0.0,
            duration.max(1.0),
            5.0,
            10.0,
            duration <= 0.5 || !self.data.has_track,
            format!("{}:{:02}", self.data.position.max(0.0) as u64 / 60, self.data.position.max(0.0) as u64 % 60),
        ));
        elements
    }

    fn invalidate(&self, hwnd: HWND, area: Rect) {
        let scale = self.scale();
        let rect = RECT {
            left: (area.left * scale).floor() as i32 - 1,
            top: (area.top * scale).floor() as i32 - 1,
            right: (area.right * scale).ceil() as i32 + 1,
            bottom: (area.bottom * scale).ceil() as i32 + 1,
        };
        let _ = unsafe { InvalidateRect(Some(hwnd), Some(&rect), false) };
    }

    fn invalidate_all(&self, hwnd: HWND) {
        let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
    }

    /// Recompute layout for the current client size, position the lyric child
    /// and repaint the whole shell. Called from `WM_SIZE` (before the player's
    /// data sync arrives).
    pub fn resize(&mut self, hwnd: HWND, child: HWND) {
        let dpi = unsafe { GetDpiForWindow(hwnd) } as f32;
        let dpi = if dpi > 0.0 { dpi } else { 96.0 };
        if (dpi - self.dpi).abs() > 0.5 {
            self.dpi = dpi;
            self.painter.dpi_changed();
        }
        let mut client = RECT::default();
        let _ = unsafe { GetClientRect(hwnd, &mut client) };
        let width = client.right.max(1);
        let height = client.bottom.max(1);
        if self.last_client == (width, height) {
            return;
        }
        self.last_client = (width, height);
        let scale = self.scale();
        self.layout = compute(width as f32 / scale, height as f32 / scale);
        self.needs_full = true;
        position_child(child, &self.layout, scale);
        self.invalidate_all(hwnd);
    }

    /// Sync layout, track metadata and playback state. Positions the lyric
    /// child in the right column; the caller then calls its `update_floating`.
    pub fn update(&mut self, hwnd: HWND, child: HWND, app: &App, windowed: bool) {
        self.resize(hwnd, child);

        let style = ScreenStyle::from_app(app);
        if self.style != style {
            self.style = style;
            self.needs_full = true;
            self.invalidate_all(hwnd);
        }

        let snapshot = app.core.snapshot();
        let placeholder = crate::i18n::tr("未在播放");
        let title = snapshot
            .current
            .map(|track| track.title.as_str())
            .unwrap_or(placeholder);
        let artist = snapshot
            .current
            .map(|track| track.artist.as_str())
            .unwrap_or("");
        let album = snapshot
            .current
            .map(|track| track.album.as_str())
            .unwrap_or("");
        let has_track = snapshot.current.is_some();
        let position = if self.dragging_progress {
            self.drag_preview
                .unwrap_or_else(|| snapshot.position_secs.max(0.0))
        } else {
            snapshot.position_secs.max(0.0)
        };
        let duration = snapshot.duration_secs.max(0.0);
        let playing = app.is_playing();

        if let Some((cover, rev)) = app.core.current_cover_owned()
            && rev != self.data.cover_rev
        {
            self.data.cover = Some(cover);
            self.data.cover_rev = rev;
            self.invalidate(hwnd, self.layout.cover);
        }

        let meta_changed = title != self.data.title
            || artist != self.data.artist
            || album != self.data.album
            || has_track != self.data.has_track;
        if meta_changed {
            self.data.title.clear();
            self.data.title.push_str(title);
            self.data.artist.clear();
            self.data.artist.push_str(artist);
            self.data.album.clear();
            self.data.album.push_str(album);
            self.data.has_track = has_track;
            let left = Rect::new(
                self.layout.cover.left,
                self.layout.cover.top,
                self.layout.lyrics.left - 16.0,
                self.layout.album.bottom + 4.0,
            );
            self.invalidate(hwnd, left);
        }
        if playing != self.data.playing {
            self.invalidate(hwnd, self.layout.play.inflate(8.0));
        }
        if windowed != self.data.windowed {
            self.invalidate(hwnd, self.layout.fullscreen.inflate(8.0));
        }
        if (duration - self.data.duration).abs() > 0.01 {
            self.invalidate(hwnd, self.layout.time_right);
            self.invalidate(hwnd, self.layout.progress_hit);
        }
        let old_ratio = ratio(self.data.position, self.data.duration);
        let new_ratio = ratio(position, duration);
        let bar = self.layout.progress;
        let old_x = bar.left + bar.width() * old_ratio;
        let new_x = bar.left + bar.width() * new_ratio;
        // Compare in DIPs, not ratios: one 16 ms tick advances a 2:30 track by
        // ~0.0001 of the bar, which a ratio threshold would swallow forever.
        if (new_x - old_x).abs() > 0.05 {
            self.invalidate(
                hwnd,
                Rect::new(
                    old_x.min(new_x) - 5.0,
                    bar.top - 8.0,
                    old_x.max(new_x) + 5.0,
                    bar.bottom + 8.0,
                ),
            );
        }
        if position.floor() as i64 != self.data.position.floor() as i64 {
            self.invalidate(hwnd, self.layout.time_left);
        }

        self.data.position = position;
        self.data.duration = duration;
        self.data.playing = playing;
        self.data.windowed = windowed;
        if self.needs_full {
            self.needs_full = false;
            self.invalidate_all(hwnd);
        }
    }

    pub fn paint(&mut self, hwnd: HWND, hdc: HDC, paint: RECT) {
        let scale = self.scale();
        let paint_area = Rect::new(
            paint.left as f32 / scale,
            paint.top as f32 / scale,
            paint.right as f32 / scale,
            paint.bottom as f32 / scale,
        );
        let layout = self.layout;
        let data = &self.data;
        let hover = self.hover;
        let pressed = self.pressed;
        let dragging = self.dragging_progress;
        let style = self.style;
        let _ = self.painter.paint_layers(hwnd, hdc, &[paint_area], |p, back, dirty, scale, dpi| {
            for rect in dirty {
                p.fill(back, *rect, C_BG, scale);
            }
            // Two very large, very dark green circles like the reference.
            let w = layout.size.0;
            let h = layout.size.1;
            let top_blob = Rect::new(w * 0.72, -h * 0.32, w * 1.34, h * 0.52);
            if top_blob.intersects(paint_area) {
                p.ellipse(back, top_blob, C_BLOB, scale);
            }
            let bottom_blob = Rect::new(-w * 0.18, h * 0.62, w * 0.42, h * 1.18);
            if bottom_blob.intersects(paint_area) {
                p.ellipse(back, bottom_blob, C_BLOB, scale);
            }

            if hits(layout.brand, dirty) {
                p.text(
                    back,
                    FontKind::Brand,
                    "BAMBOO  MUSIC",
                    layout.brand,
                    C_BRAND,
                    DT_LEFT.0,
                    scale,
                    dpi,
                );
            }
            if hits(layout.fullscreen, dirty) {
                draw_icon(
                    p,
                    back,
                    layout.fullscreen,
                    if data.windowed { "\u{E740}" } else { "\u{E73F}" },
                    hover == Some(Button::Fullscreen),
                    pressed == Some(Button::Fullscreen),
                    scale,
                    dpi,
                    &style,
                );
            }
            if hits(layout.close, dirty) {
                draw_icon(
                    p,
                    back,
                    layout.close,
                    "\u{E8BB}",
                    hover == Some(Button::Close),
                    pressed == Some(Button::Close),
                    scale,
                    dpi,
                    &style,
                );
            }

            if style.show_cover && hits(layout.cover, dirty) {
                p.fill(back, layout.cover, C_COVER, scale);
                let drawn = data
                    .cover
                    .as_ref()
                    .is_some_and(|image| p.cover(back, layout.cover, image, scale));
                if !drawn {
                    p.text(
                        back,
                        FontKind::CoverGlyph,
                        "\u{266A}",
                        layout.cover,
                        C_COVER_GLYPH,
                        DT_CENTER.0,
                        scale,
                        dpi,
                    );
                }
            }
            if style.show_title && hits(layout.title, dirty) {
                p.text(
                    back,
                    FontKind::ScreenTitle,
                    &data.title,
                    layout.title,
                    style.title,
                    DT_LEFT.0,
                    scale,
                    dpi,
                );
            }
            if style.show_title && hits(layout.artist, dirty) && !data.artist.is_empty() {
                p.text(
                    back,
                    FontKind::ScreenBody,
                    &data.artist,
                    layout.artist,
                    style.artist,
                    DT_LEFT.0,
                    scale,
                    dpi,
                );
            }
            if style.show_title && hits(layout.album, dirty) && !data.album.is_empty() {
                p.text(
                    back,
                    FontKind::ScreenSmall,
                    &data.album,
                    layout.album,
                    style.album,
                    DT_LEFT.0,
                    scale,
                    dpi,
                );
            }

            if style.show_footer {
            if hits(layout.time_left, dirty) {
                p.text(
                    back,
                    FontKind::ScreenSmall,
                    &format_time(data.position),
                    layout.time_left,
                    C_TIME,
                    DT_LEFT.0,
                    scale,
                    dpi,
                );
            }
            if hits(layout.time_right, dirty) {
                p.text(
                    back,
                    FontKind::ScreenSmall,
                    &format_time(data.duration),
                    layout.time_right,
                    C_TIME,
                    DT_RIGHT.0,
                    scale,
                    dpi,
                );
            }
            if hits(layout.progress_hit, dirty) {
                let bar = layout.progress;
                let center_y = bar.top + bar.height() * 0.5;
                let thickness = if hover == Some(Button::Progress) || dragging {
                    6.0
                } else {
                    4.0
                };
                let track = Rect::new(
                    bar.left,
                    center_y - thickness * 0.5,
                    bar.right,
                    center_y + thickness * 0.5,
                );
                p.fill(back, track, C_TRACK_LINE, scale);
                let filled = track.left + track.width() * ratio(data.position, data.duration);
                p.fill(
                    back,
                    Rect::new(track.left, track.top, filled, track.bottom),
                    style.accent,
                    scale,
                );
            }

            if hits(layout.prev, dirty) {
                draw_icon(
                    p,
                    back,
                    layout.prev,
                    "\u{E892}",
                    hover == Some(Button::Prev),
                    pressed == Some(Button::Prev),
                    scale,
                    dpi,
                    &style,
                );
            }
            if hits(layout.play, dirty) {
                if hover == Some(Button::Play) || pressed == Some(Button::Play) {
                    let bg = if pressed == Some(Button::Play) {
                        style.icon_press
                    } else {
                        style.icon_bg
                    };
                    p.fill(back, layout.play, bg, scale);
                }
                p.text(
                    back,
                    FontKind::GlyphHuge,
                    if data.playing { "\u{E769}" } else { "\u{E768}" },
                    layout.play,
                    if hover == Some(Button::Play) {
                        style.icon_hover
                    } else {
                        style.icon
                    },
                    DT_CENTER.0,
                    scale,
                    dpi,
                );
            }
            if hits(layout.next, dirty) {
                draw_icon(
                    p,
                    back,
                    layout.next,
                    "\u{E893}",
                    hover == Some(Button::Next),
                    pressed == Some(Button::Next),
                    scale,
                    dpi,
                    &style,
                );
            }
            }
        });
    }

    fn button_rect(&self, button: Button) -> Rect {
        match button {
            Button::Close => self.layout.close,
            Button::Fullscreen => self.layout.fullscreen,
            Button::Prev => self.layout.prev,
            Button::Play => self.layout.play,
            Button::Next => self.layout.next,
            Button::Progress => self.layout.progress_hit,
        }
    }

    fn hit(&self, x: f32, y: f32) -> Option<Button> {
        if self.layout.close.inflate(6.0).contains(x, y) {
            return Some(Button::Close);
        }
        if self.layout.fullscreen.inflate(6.0).contains(x, y) {
            return Some(Button::Fullscreen);
        }
        if !self.style.show_footer {
            return None;
        }
        if self.layout.prev.inflate(6.0).contains(x, y) {
            return Some(Button::Prev);
        }
        if self.layout.play.inflate(6.0).contains(x, y) {
            return Some(Button::Play);
        }
        if self.layout.next.inflate(6.0).contains(x, y) {
            return Some(Button::Next);
        }
        if self.layout.progress_hit.contains(x, y) {
            return Some(Button::Progress);
        }
        None
    }

    fn set_hover(&mut self, hwnd: HWND, hover: Option<Button>) {
        if self.hover == hover {
            return;
        }
        if let Some(old) = self.hover {
            self.invalidate(hwnd, self.button_rect(old).inflate(8.0));
        }
        self.hover = hover;
        if let Some(new) = hover {
            self.invalidate(hwnd, self.button_rect(new).inflate(8.0));
        }
    }

    fn set_pressed(&mut self, hwnd: HWND, pressed: Option<Button>) {
        if self.pressed == pressed {
            return;
        }
        if let Some(old) = self.pressed {
            self.invalidate(hwnd, self.button_rect(old).inflate(8.0));
        }
        self.pressed = pressed;
        if let Some(new) = pressed {
            self.invalidate(hwnd, self.button_rect(new).inflate(8.0));
        }
    }

    fn seek_secs_at(&self, x: f32) -> Option<f64> {
        let bar = self.layout.progress;
        if bar.width() <= 0.0 || self.data.duration <= 0.0 {
            return None;
        }
        let ratio = ((x - bar.left) / bar.width()).clamp(0.0, 1.0);
        Some(ratio as f64 * self.data.duration)
    }

    fn seek_at(&self, x: f32) -> Option<Action> {
        self.seek_secs_at(x)
            .map(|secs| Action::Seek((secs * 1000.0) as u64))
    }

    pub fn on_move(&mut self, hwnd: HWND, x: f32, y: f32) -> Option<Action> {
        if self.dragging_progress {
            if let Some(secs) = self.seek_secs_at(x) {
                let old = self.drag_preview.unwrap_or(self.data.position);
                if (old - secs).abs() > 0.01 {
                    self.drag_preview = Some(secs);
                    let bar = self.layout.progress;
                    let duration = self.data.duration.max(0.1);
                    let old_x = bar.left + bar.width() * (old / duration).clamp(0.0, 1.0) as f32;
                    let new_x = bar.left + bar.width() * (secs / duration).clamp(0.0, 1.0) as f32;
                    self.invalidate(
                        hwnd,
                        Rect::new(
                            old_x.min(new_x) - 5.0,
                            bar.top - 8.0,
                            old_x.max(new_x) + 5.0,
                            bar.bottom + 8.0,
                        ),
                    );
                    if old.floor() != secs.floor() {
                        self.invalidate(hwnd, self.layout.time_left);
                        self.invalidate(hwnd, self.layout.time_right);
                    }
                }
            }
            return None;
        }
        let hover = self.hit(x, y);
        self.set_hover(hwnd, hover);
        None
    }

    pub fn on_press(&mut self, hwnd: HWND, x: f32, y: f32) -> Option<Action> {
        let button = self.hit(x, y);
        self.set_pressed(hwnd, button);
        match button {
            Some(Button::Progress) => {
                // Only the 4 DIP bar starts a drag; the padded area above and
                // below is hover-only, so a press there cannot seek-spam.
                if self.layout.progress.inflate(6.0).contains(x, y) {
                    self.dragging_progress = true;
                    self.drag_preview = self.seek_secs_at(x);
                    self.invalidate(hwnd, self.layout.progress_hit);
                    self.invalidate(hwnd, self.layout.time_left);
                    self.invalidate(hwnd, self.layout.time_right);
                }
                None
            }
            Some(_) => None,
            None => {
                // The native title bar already moves the window; the custom
                // top bar is a second drag handle in windowed mode only.
                if y < TOP_BAR && self.data.windowed {
                    Some(Action::Caption)
                } else {
                    None
                }
            }
        }
    }

    pub fn on_release(&mut self, hwnd: HWND, x: f32, y: f32) -> Option<Action> {
        if self.dragging_progress {
            self.dragging_progress = false;
            self.drag_preview = None;
            self.set_pressed(hwnd, None);
            // One seek for the whole drag (seek on mouse up).
            return self.seek_at(x);
        }
        let pressed = self.pressed.take();
        self.set_pressed(hwnd, None);
        let hit = self.hit(x, y);
        if pressed != hit {
            return None;
        }
        match pressed {
            Some(Button::Close) => Some(Action::Close),
            Some(Button::Fullscreen) => Some(Action::Fullscreen),
            Some(Button::Prev) => Some(Action::Prev),
            Some(Button::Play) => Some(Action::PlayPause),
            Some(Button::Next) => Some(Action::Next),
            Some(Button::Progress) | None => None,
        }
    }

    pub fn clear_hover(&mut self, hwnd: HWND) {
        self.set_hover(hwnd, None);
    }

    pub fn stop_drag(&mut self) {
        self.dragging_progress = false;
        self.drag_preview = None;
    }
}

fn position_child(child: HWND, layout: &ScreenLayout, scale: f32) {
    let area = layout.lyrics;
    let x = (area.left * scale).round() as i32;
    let y = (area.top * scale).round() as i32;
    let width = ((area.width() * scale).round() as i32).max(10);
    let height = ((area.height() * scale).round() as i32).max(10);
    let _ = unsafe {
        SetWindowPos(
            child,
            None,
            x,
            y,
            width,
            height,
            SWP_NOZORDER | SWP_NOACTIVATE,
        )
    };
}

fn draw_icon(
    painter: &mut Painter,
    hdc: HDC,
    area: Rect,
    glyph: &str,
    hover: bool,
    pressed: bool,
    scale: f32,
    dpi: f32,
    style: &ScreenStyle,
) {
    if hover || pressed {
        painter.fill(
            hdc,
            area.inflate(7.0),
            if pressed { style.icon_press } else { style.icon_bg },
            scale,
        );
    }
    painter.text(
        hdc,
        FontKind::GlyphLarge,
        glyph,
        area,
        if hover || pressed { style.icon_hover } else { style.icon },
        DT_CENTER.0 | DT_SINGLELINE.0 | DT_VCENTER.0 | DT_NOPREFIX.0,
        scale,
        dpi,
    );
}

fn ratio(position: f64, duration: f64) -> f32 {
    if duration > 0.0 {
        (position / duration).clamp(0.0, 1.0) as f32
    } else {
        0.0
    }
}

fn format_time(seconds: f64) -> String {
    let seconds = seconds.max(0.0) as u64;
    let (hours, minutes, secs) = (seconds / 3600, (seconds % 3600) / 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{secs:02}")
    } else {
        format!("{minutes:02}:{secs:02}")
    }
}

fn hits(area: Rect, dirty: &[Rect]) -> bool {
    dirty.iter().any(|rect| area.intersects(*rect))
}

const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF(((b as u32) << 16) | ((g as u32) << 8) | r as u32)
}
