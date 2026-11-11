//! Wire protocol between the player process and the native desktop-lyrics
//! overlay process.
//!
//! The overlay deliberately does not link egui/eframe: it is a tiny Win32 +
//! Direct2D process whose only job is painting a handful of text rows. The
//! player owns playback, settings and lyric parsing, then sends a full state
//! snapshot whenever something visible changes plus cheap position ticks.

use serde::{Deserialize, Serialize};

/// One row of lyrics, flattened from [`crate::lyrics::LyricLine`]. The overlay
/// has no parser, so only the rendered fields cross the process boundary.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct OverlayWord {
    pub start_ms: u64,
    pub end_ms: Option<u64>,
    pub text: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct OverlayLine {
    pub start_ms: u64,
    pub end_ms: Option<u64>,
    pub text: String,
    pub translation: Option<String>,
    pub words: Vec<OverlayWord>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct OverlayLyrics {
    pub lines: Vec<OverlayLine>,
    pub offset_ms: i64,
    pub timed: bool,
}

/// Visual settings copied from `DesktopLyricsSettings`. Colors stay in the
/// source RGBA order used by the settings file.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct OverlaySettings {
    pub locked: bool,
    pub click_through: bool,
    pub always_on_top: bool,
    pub lines: u8,
    pub show_translation: bool,
    pub show_footer: bool,
    pub font_family: Option<String>,
    pub font_size: f32,
    pub next_font_size: f32,
    pub prev_font_size: f32,
    pub footer_font_size: f32,
    /// Per-row font families; `None` follows `font_family`.
    pub translation_font_family: Option<String>,
    pub footer_font_family: Option<String>,
    pub bold: bool,
    pub italic: bool,
    pub translation_bold: bool,
    pub footer_bold: bool,
    /// 0 = left, 1 = center, 2 = right.
    pub align: u8,
    pub current_color: [u8; 4],
    pub next_color: [u8; 4],
    pub prev_color: [u8; 4],
    pub footer_color: [u8; 4],
    pub translation_color: [u8; 4],
    /// 0 = transparent, 1 = solid, 2 = gradient, 3 = image, 4 = blurred cover.
    pub background: u8,
    pub bg_color: [u8; 4],
    pub bg_color2: [u8; 4],
    pub bg_image: Option<String>,
    pub opacity: f32,
    pub radius: u8,
    pub padding: f32,
    pub line_spacing: f32,
    pub shadow: bool,
    pub outline: bool,
    pub translation_outline: bool,
    pub translation_shadow: bool,
    pub footer_outline: bool,
    pub footer_shadow: bool,
    pub border: bool,
    pub window_shadow: bool,
    pub outline_color: [u8; 4],
    pub outline_width: f32,
    pub shadow_color: [u8; 4],
    pub shadow_offset: f32,
    pub translation_outline_width: f32,
    pub translation_outline_color: [u8; 4],
    pub translation_shadow_offset: f32,
    pub translation_shadow_color: [u8; 4],
    pub footer_outline_width: f32,
    pub footer_outline_color: [u8; 4],
    pub footer_shadow_offset: f32,
    pub footer_shadow_color: [u8; 4],
    pub blur_radius: f32,
    pub capture_protection: bool,
    pub karaoke: bool,
    pub karaoke_words_only: bool,
    pub width: f32,
    pub height: f32,
    pub auto_size: bool,
    /// Logical (egui-point) position. Applied on the first state and after an
    /// explicit reset; normal moves are owned by the overlay window itself.
    pub position: Option<(f32, f32)>,
    pub frame_rate: u32,
    /// Hardware vs software Direct2D DC target (global performance setting).
    pub d2d_hardware: bool,
}

impl Default for OverlaySettings {
    fn default() -> Self {
        Self {
            locked: false,
            click_through: false,
            always_on_top: true,
            lines: 3,
            show_translation: true,
            show_footer: true,
            font_family: None,
            font_size: 34.0,
            next_font_size: 26.0,
            prev_font_size: 24.0,
            footer_font_size: 13.0,
            translation_font_family: None,
            footer_font_family: None,
            bold: true,
            italic: false,
            translation_bold: false,
            footer_bold: false,
            align: 1,
            current_color: [255, 255, 255, 255],
            next_color: [170, 185, 215, 235],
            prev_color: [150, 162, 190, 220],
            footer_color: [200, 210, 230, 220],
            translation_color: [145, 158, 185, 210],
            background: 1,
            bg_color: [14, 17, 26, 185],
            bg_color2: [32, 40, 60, 185],
            bg_image: None,
            opacity: 1.0,
            radius: 18,
            padding: 24.0,
            line_spacing: 6.0,
            shadow: true,
            outline: false,
            translation_outline: false,
            translation_shadow: false,
            footer_outline: false,
            footer_shadow: false,
            border: true,
            window_shadow: true,
            outline_color: [0, 0, 0, 230],
            outline_width: 1.0,
            shadow_color: [0, 0, 0, 150],
            shadow_offset: 2.0,
            translation_outline_width: 1.0,
            translation_outline_color: [0, 0, 0, 230],
            translation_shadow_offset: 2.0,
            translation_shadow_color: [0, 0, 0, 150],
            footer_outline_width: 1.0,
            footer_outline_color: [0, 0, 0, 230],
            footer_shadow_offset: 1.0,
            footer_shadow_color: [0, 0, 0, 150],
            blur_radius: 14.0,
            capture_protection: false,
            karaoke: true,
            karaoke_words_only: false,
            width: 900.0,
            height: 190.0,
            auto_size: true,
            position: None,
            frame_rate: 60,
            d2d_hardware: false,
        }
    }
}

/// Localized strings for the native context menu and status placeholders.
/// The overlay does not embed the i18n table (it would drag in the player's
/// config module), so the host sends the labels it should display.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct OverlayLabels {
    pub open_main: String,
    pub open_settings: String,
    pub lock: String,
    pub click_through: String,
    pub always_on_top: String,
    pub lines: String,
    pub line_1: String,
    pub line_2: String,
    pub line_3: String,
    pub align: String,
    pub align_left: String,
    pub align_center: String,
    pub align_right: String,
    pub auto_size: String,
    pub window_size: String,
    pub size_small: String,
    pub size_medium: String,
    pub size_large: String,
    pub font_increase: String,
    pub font_decrease: String,
    pub reset_position: String,
    pub hide: String,
    pub loading: String,
    pub no_lyrics: String,
    pub app_name: String,
}

/// Complete visible state. Sent on connect and whenever anything other than
/// the playback clock changes.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct OverlayState {
    pub settings: OverlaySettings,
    pub title: String,
    pub artist: String,
    pub lyrics_version: u64,
    /// Included only when `lyrics_version` advanced; the overlay keeps the
    /// previous lyrics otherwise.
    pub lyrics: Option<OverlayLyrics>,
    pub lyrics_loading: bool,
    pub playing: bool,
    pub position_ms: u64,
    pub offset_ms: i64,
    pub cover_version: u64,
    /// PNG bytes (base64 on the wire) for the blurred-cover background.
    #[serde(with = "cover_bytes")]
    pub cover_png: Option<Vec<u8>>,
    pub labels: OverlayLabels,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum HostMessage {
    State(Box<OverlayState>),
    /// Cheap clock tick; the overlay interpolates between ticks.
    Position { position_ms: u64, playing: bool },
    /// Move back to the default placement (bottom center).
    ResetPosition,
    Shutdown,
}

/// Commands the native overlay asks the player to perform.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum OverlayAction {
    ToggleLock,
    ToggleClickThrough,
    ToggleAlwaysOnTop,
    Hide,
    ShowMainWindow,
    OpenSettings,
    ResetPosition,
    FontDelta { delta: i32 },
    SetLines { lines: u8 },
    SetAlign { align: u8 },
    ToggleAutoSize,
    SetAutoSize { enabled: bool },
    SetSize { width: f32, height: f32 },
    /// Reported after a native move/resize. Coordinates and sizes are logical
    /// points, matching the values stored in settings/session.
    Geometry {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        resized: bool,
    },
}

// ---------------------------------------------------------------------------
// Native mini player
// ---------------------------------------------------------------------------

/// Theme colors copied from the egui palette so the native window looks like
/// the rest of the app. Values are straight-alpha RGBA; the renderer applies
/// the panel opacity itself.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct MiniColors {
    pub panel: [u8; 4],
    pub card: [u8; 4],
    pub card_hover: [u8; 4],
    pub line: [u8; 4],
    pub text: [u8; 4],
    pub text_weak: [u8; 4],
    pub text_faint: [u8; 4],
    pub accent: [u8; 4],
}

/// Localized text for the native window (status placeholders and tooltips).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct MiniLabels {
    pub loading: String,
    pub no_lyrics: String,
    pub not_playing: String,
    pub mini_player: String,
    pub play: String,
    pub pause: String,
    pub prev: String,
    pub next: String,
    pub mute: String,
    pub unmute: String,
    pub close: String,
}

/// Complete visible state of the native mini player. Position is interpolated
/// locally between cheap `Position` ticks so the progress bar stays smooth
/// even when the main window's logic runs at a slower cadence.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct MiniState {
    pub title: String,
    pub artist: String,
    pub playing: bool,
    pub position_ms: u64,
    pub duration_ms: u64,
    pub volume: f32,
    pub muted: bool,
    pub cover_version: u64,
    #[serde(with = "cover_bytes")]
    pub cover_png: Option<Vec<u8>>,
    pub lyrics_version: u64,
    /// Included only when `lyrics_version` advanced.
    pub lyrics: Option<OverlayLyrics>,
    pub lyrics_loading: bool,
    pub offset_ms: i64,
    pub show_lyrics: bool,
    pub karaoke: bool,
    pub karaoke_words_only: bool,
    pub lyrics_lines: u8,
    pub lyrics_bold: bool,
    pub show_translation: bool,
    pub lyrics_font_size: f32,
    pub width: f32,
    pub height: f32,
    pub opacity: f32,
    pub always_on_top: bool,
    pub show_in_taskbar: bool,
    pub frame_rate: u32,
    /// Hardware vs software Direct2D DC target (global performance setting).
    pub d2d_hardware: bool,
    pub font_family: Option<String>,
    pub colors: MiniColors,
    /// Applied on the first state and after an explicit reset; moves are owned
    /// by the native window afterwards.
    pub position: Option<(f32, f32)>,
    pub labels: MiniLabels,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum MiniHostMessage {
    State(Box<MiniState>),
    Position {
        position_ms: u64,
        playing: bool,
    },
    ResetPosition,
    Shutdown,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum MiniAction {
    PlayPause,
    Prev,
    Next,
    ShowMain,
    Hide,
    ToggleMute,
    SetVolume {
        volume: f32,
    },
    Seek {
        position_ms: u64,
    },
    /// Reported after a native move/resize. Logical points.
    Geometry {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        resized: bool,
    },
}

mod cover_bytes {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(value: &Option<Vec<u8>>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match value {
            Some(bytes) => serializer.serialize_some(&STANDARD.encode(bytes)),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<Vec<u8>>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = Option::<String>::deserialize(deserializer)?;
        match encoded {
            Some(encoded) => STANDARD
                .decode(encoded.as_bytes())
                .map(Some)
                .map_err(serde::de::Error::custom),
            None => Ok(None),
        }
    }
}
