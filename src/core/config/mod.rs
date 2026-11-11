//! Persistent settings and playback session (exit cache).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::model::{Page, RepeatMode, Track};

pub const CONFIG_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    #[default]
    ZhCn,
    EnUs,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode {
    System,
    #[default]
    Dark,
    Light,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundMode {
    #[default]
    StaticBlur,
    GpuBlur,
    Flow,
    Solid,
    Image,
    Mica,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WgpuBackend {
    #[default]
    Auto,
    Dx12,
    Vulkan,
    Gl,
}

/// Which physical GPU wgpu should prefer.
///
/// Laptops with a discrete GPU often pick the integrated one when the app
/// asks for low power. For an immediate-mode UI that redraws the whole window
/// every frame the integrated GPU is frequently the slower, hotter choice, so
/// this is configurable instead of hard-coded.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GpuPowerPreference {
    /// Let wgpu/driver decide (usually the best available GPU when on AC).
    #[default]
    Auto,
    /// Prefer the integrated/low-power GPU.
    LowPower,
    /// Prefer the discrete/highest-performance GPU.
    HighPerformance,
}

impl GpuPowerPreference {
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => crate::i18n::tr("自动（推荐）"),
            Self::LowPower => crate::i18n::tr("省电（核显）"),
            Self::HighPerformance => crate::i18n::tr("高性能（独显）"),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ListDensity {
    Compact,
    #[default]
    Comfortable,
    Spacious,
}

/// How much tag metadata is read from local files.
///
/// Reading every file's tags during a large scan is the most expensive part
/// of importing a folder, so the default only loads the current track.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MetadataMode {
    /// Never read tags; titles come from file names.
    Off,
    /// Read tags when a track is played or restored (recommended).
    #[default]
    CurrentOnly,
    /// Read tags for every queued file in the background.
    All,
}

impl MetadataMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => crate::i18n::tr("关闭（最快）"),
            Self::CurrentOnly => crate::i18n::tr("仅当前曲目（推荐）"),
            Self::All => crate::i18n::tr("全部曲目（后台）"),
        }
    }

    pub fn enabled(self) -> bool {
        !matches!(self, Self::Off)
    }

    pub fn background_all(self) -> bool {
        matches!(self, Self::All)
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FullscreenMode {
    /// Borderless window covering the monitor (default, no display-mode switch).
    #[default]
    Borderless,
    /// Borderless maximized window; taskbar remains visible.
    Maximized,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CloseBehavior {
    #[default]
    Tray,
    Exit,
    Ask,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReplayGainMode {
    #[default]
    Off,
    Track,
    Album,
}

/// How the library duplicate finder decides two rows describe the same audio.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DedupeMode {
    /// Title + artist, with durations within 1.5 seconds of each other.
    #[default]
    Metadata,
    /// Rows that resolve to the same file (case differences, symlinks, hard links).
    Path,
    /// Decoded-audio fingerprint; tolerates different tags and most re-encodes.
    Fingerprint,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EqPreset {
    #[default]
    Flat,
    Rock,
    Pop,
    Jazz,
    Classical,
    Vocal,
    Bass,
    Custom,
}

impl EqPreset {
    /// Band gains in dB for [`crate::audio::EQ_BANDS_HZ`].
    pub fn bands(self) -> [f32; 10] {
        match self {
            Self::Flat => [0.0; 10],
            Self::Rock => [5.0, 4.0, 2.5, 0.5, -1.0, -1.0, 0.0, 2.0, 3.5, 4.5],
            Self::Pop => [-1.0, 0.5, 2.0, 4.0, 4.0, 2.0, 0.0, -0.5, -1.0, -1.0],
            Self::Jazz => [3.5, 2.5, 1.0, 2.0, -0.5, -0.5, 0.0, 1.0, 2.0, 3.0],
            Self::Classical => [3.0, 2.0, 1.0, 0.0, -0.5, -0.5, 0.0, 1.0, 2.0, 3.0],
            Self::Vocal => [-2.0, -1.5, 0.0, 2.5, 4.5, 4.0, 3.0, 1.0, 0.0, -1.0],
            Self::Bass => [7.0, 6.0, 5.0, 3.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            Self::Custom => [0.0; 10],
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LyricsEncoding {
    #[default]
    Auto,
    Utf8,
    Gbk,
    Gb18030,
    Utf16Le,
    Utf16Be,
    Big5,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LyricsAlign {
    Left,
    #[default]
    Center,
    Right,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LyricsPanelBackground {
    /// Follow the theme panel color.
    #[default]
    Theme,
    Solid,
    Gradient,
    /// A user-selected picture, stretched over the panel.
    Image,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DesktopBackgroundMode {
    Transparent,
    #[default]
    Solid,
    Gradient,
    Image,
    BlurCover,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProxyMode {
    #[default]
    System,
    None,
    Manual,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    #[default]
    Local,
    /// Generic Bamboo-compatible endpoint. The address is intentionally blank
    /// until the user configures it.
    Online,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Quality {
    K128,
    #[default]
    K320,
    Flac,
}

impl Quality {
    pub fn api_value(self) -> &'static str {
        match self {
            Self::K128 => "128kmp3",
            Self::K320 => "320kmp3",
            Self::Flac => "2000kflac",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::K128 => "128 kbps",
            Self::K320 => "320 kbps",
            Self::Flac => crate::i18n::tr("无损 FLAC"),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConflictPolicy {
    #[default]
    Rename,
    Skip,
    Overwrite,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VisualizerMode {
    Off,
    #[default]
    Bars,
    Waveform,
    Ring,
}

// ---------------------------------------------------------------------------
// Settings sections
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeSettings {
    pub mode: ThemeMode,
    pub accent: [u8; 3],
    pub background: BackgroundMode,
    pub background_image: Option<PathBuf>,
    pub blur_radius: f32,
    pub background_opacity: f32,
    pub font_family: Option<String>,
    pub font_size: f32,
    pub roundness: u8,
    pub animations: bool,
    pub reduce_motion: bool,
    pub list_density: ListDensity,
    pub sidebar_width: f32,
    pub backend: WgpuBackend,
    /// Preferred physical GPU; takes effect on the next start.
    pub gpu_power: GpuPowerPreference,
}

impl Default for ThemeSettings {
    fn default() -> Self {
        Self {
            mode: ThemeMode::Dark,
            // Bamboo green. Older builds shipped an unused blue default; the
            // migration below upgrades it so existing settings keep the look.
            accent: [127, 211, 168],
            background: BackgroundMode::StaticBlur,
            background_image: None,
            blur_radius: 24.0,
            background_opacity: 0.45,
            font_family: Some("Microsoft YaHei UI".to_string()),
            font_size: 14.0,
            roundness: 8,
            animations: true,
            reduce_motion: false,
            list_density: ListDensity::Comfortable,
            sidebar_width: 220.0,
            backend: WgpuBackend::Auto,
            gpu_power: GpuPowerPreference::Auto,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct PlaybackSettings {
    pub volume: f32,
    pub muted: bool,
    pub device: Option<String>,
    pub seek_step_secs: f64,
    pub auto_advance: bool,
    pub resume_on_start: bool,
    pub crossfade_secs: f32,
    pub gapless: bool,
    pub replaygain: ReplayGainMode,
    pub replaygain_preamp_db: f32,
    /// Analyze files that carry no ReplayGain tags (EBU R128). Results are
    /// cached, so each file is only measured once.
    pub replaygain_scan: bool,
    pub eq_enabled: bool,
    pub eq_preset: EqPreset,
    pub eq_bands: [f32; 10],
    pub preamp_db: f32,
    pub speed: f32,
    pub pitch_preserve: bool,
}

impl Default for PlaybackSettings {
    fn default() -> Self {
        Self {
            volume: 0.8,
            muted: false,
            device: None,
            seek_step_secs: 5.0,
            auto_advance: true,
            resume_on_start: false,
            crossfade_secs: 0.0,
            gapless: true,
            replaygain: ReplayGainMode::Off,
            replaygain_preamp_db: 0.0,
            replaygain_scan: true,
            eq_enabled: false,
            eq_preset: EqPreset::Flat,
            eq_bands: [0.0; 10],
            preamp_db: 0.0,
            speed: 1.0,
            pitch_preserve: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowSettings {
    pub remember_position: bool,
    pub close_behavior: CloseBehavior,
    pub start_minimized: bool,
    pub capture_protection: bool,
    /// Exclude the standalone lyrics window from screen capture.
    pub lyrics_capture_protection: bool,
    pub remember_size: bool,
    pub trim_working_set_on_idle: bool,
    pub trim_idle_secs: u64,
    pub lyrics_fullscreen: FullscreenMode,
    /// Display used by the fullscreen lyrics page (index into the monitor
    /// list, left to right). `None` follows the window's current display.
    pub lyrics_monitor: Option<usize>,
    /// Launch a new window even when another instance is running. Requires a
    /// restart because the single-instance guard runs at startup.
    pub allow_multiple_instances: bool,
}

impl Default for WindowSettings {
    fn default() -> Self {
        Self {
            remember_position: true,
            close_behavior: CloseBehavior::Tray,
            start_minimized: false,
            capture_protection: false,
            lyrics_capture_protection: false,
            remember_size: true,
            trim_working_set_on_idle: true,
            trim_idle_secs: 30,
            lyrics_fullscreen: FullscreenMode::Borderless,
            lyrics_monitor: None,
            allow_multiple_instances: false,
        }
    }
}

/// Startup, loading and memory trade-offs. Every switch is optional so users
/// can pick between fastest startup and the richest list metadata.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct PerformanceSettings {
    pub metadata_mode: MetadataMode,
    /// Decode embedded/sidecar cover art for the current track.
    pub load_covers: bool,
    /// Create the audio output device on first playback instead of at launch.
    pub lazy_audio: bool,
    /// Pause the audio output stream while stopped/paused to save power.
    pub release_audio_output: bool,
    /// Restore a paused session without opening the decoder until Play.
    pub defer_paused_restore: bool,
    /// UI/animation refresh rate while the player is visible. Lower values
    /// save power; higher values keep karaoke highlighting and scrolling
    /// smooth.
    pub frame_rate: u32,
    /// Refresh rate used when nothing on screen is animating (a playing
    /// progress bar and the time labels). Lower values save power without
    /// changing the rendered result.
    pub idle_frame_rate: u32,
    /// Stop background/visualizer animation while the window is unfocused or
    /// minimized. The UI still updates at `idle_frame_rate`.
    pub pause_animations_unfocused: bool,
    /// Maximum number of decoded online cover thumbnails kept in memory.
    /// Zero means unlimited.
    pub thumbnail_cache: u32,
    /// Render the Direct2D overlay surfaces (desktop lyrics, mini player)
    /// with a hardware DC render target instead of the software rasterizer.
    /// Software is the default: the surfaces are small DIBs, and a hardware
    /// target uploads/reads back every frame, which shows up as GPU time.
    pub d2d_hardware: bool,
}

impl Default for PerformanceSettings {
    fn default() -> Self {
        Self {
            metadata_mode: MetadataMode::CurrentOnly,
            load_covers: true,
            lazy_audio: true,
            release_audio_output: true,
            defer_paused_restore: true,
            frame_rate: 60,
            idle_frame_rate: 15,
            pause_animations_unfocused: true,
            thumbnail_cache: 256,
            d2d_hardware: false,
        }
    }
}

impl PerformanceSettings {
    /// Repaint interval used by lyrics/animation surfaces.
    pub fn frame_millis(&self) -> u64 {
        (1000 / self.frame_rate.clamp(15, 120)).max(8) as u64
    }

    /// Repaint interval for a playing window with no active animation.
    pub fn idle_millis(&self) -> u64 {
        (1000 / self.idle_frame_rate.clamp(2, 120)) as u64
    }
}

/// Master switches for optional features. Everything can be turned off.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct FeatureToggles {
    pub crossfade: bool,
    pub gapless: bool,
    pub replaygain: bool,
    pub equalizer: bool,
    pub speed_control: bool,
    pub ab_repeat: bool,
    pub sleep_timer: bool,
    pub internet_radio: bool,
    pub smart_playlists: bool,
    pub statistics: bool,
    pub duplicate_finder: bool,
    pub lyric_editor: bool,
    pub downloads: bool,
    pub stream_cache: bool,
    pub discover: bool,
    pub mini_player: bool,
    pub fullscreen_lyrics: bool,
    pub visualizer: bool,
    pub notifications: bool,
    pub jump_list: bool,
    pub start_with_windows: bool,
    pub file_associations: bool,
    /// `.mp3list` playlist association (independent from audio files).
    pub playlist_associations: bool,
    pub command_palette: bool,
    pub accessibility: bool,
    pub tray: bool,
    pub desktop_lyrics: bool,
    pub media_controls: bool,
}

impl Default for FeatureToggles {
    fn default() -> Self {
        Self {
            crossfade: true,
            gapless: true,
            replaygain: true,
            equalizer: true,
            speed_control: true,
            ab_repeat: true,
            sleep_timer: true,
            internet_radio: true,
            smart_playlists: true,
            statistics: true,
            duplicate_finder: true,
            lyric_editor: true,
            downloads: true,
            stream_cache: true,
            discover: true,
            mini_player: true,
            fullscreen_lyrics: true,
            visualizer: true,
            notifications: true,
            jump_list: true,
            start_with_windows: false,
            file_associations: false,
            playlist_associations: false,
            command_palette: true,
            accessibility: true,
            tray: true,
            desktop_lyrics: true,
            media_controls: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct HeaderPair {
    pub name: String,
    pub value: String,
}

impl Default for HeaderPair {
    fn default() -> Self {
        Self {
            name: String::new(),
            value: String::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(default)]
pub struct EndpointOverrides {
    pub search: Option<String>,
    pub suggest: Option<String>,
    pub info: Option<String>,
    pub url: Option<String>,
    pub cover: Option<String>,
    pub album: Option<String>,
    pub playlist: Option<String>,
    pub singer: Option<String>,
    pub album_search: Option<String>,
}
impl EndpointOverrides {
    pub fn get(&self, action: &str) -> Option<&String> {
        match action {
            "search" => self.search.as_ref(),
            "suggestKey" => self.suggest.as_ref(),
            "info" => self.info.as_ref(),
            "url" => self.url.as_ref(),
            "cover" => self.cover.as_ref(),
            "album" => self.album.as_ref(),
            "playlist" => self.playlist.as_ref(),
            "singer" => self.singer.as_ref(),
            "searchAlbum" => self.album_search.as_ref(),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct SourceConfig {
    pub id: String,
    pub name: String,
    pub kind: SourceKind,
    /// API directory URL (e.g. http://127.0.0.1/musicnew/apis/local) or a full .php URL.
    pub base_url: String,
    pub script: String,
    pub prefix: String,
    pub enabled: bool,
    pub quality: Quality,
    pub session_key: String,
    pub cookie: String,
    pub headers: Vec<HeaderPair>,
    pub endpoints: EndpointOverrides,
    pub insecure_tls: bool,
}

impl Default for SourceConfig {
    fn default() -> Self {
        Self {
            id: "local".to_string(),
            name: "本地曲库".to_string(),
            kind: SourceKind::Local,
            base_url: "http://127.0.0.1/musicnew/apis/local".to_string(),
            script: "local.php".to_string(),
            prefix: String::new(),
            enabled: true,
            quality: Quality::K320,
            session_key: String::new(),
            cookie: String::new(),
            headers: Vec::new(),
            endpoints: EndpointOverrides::default(),
            insecure_tls: false,
        }
    }
}

impl SourceConfig {
    pub fn preset(kind: SourceKind) -> Self {
        match kind {
            SourceKind::Local => Self {
                id: "local".to_string(),
                name: "本地曲库".to_string(),
                kind,
                base_url: "http://127.0.0.1/musicnew/apis/local".to_string(),
                script: "local.php".to_string(),
                prefix: String::new(),
                enabled: true,
                quality: Quality::K320,
                ..Default::default()
            },
            SourceKind::Online => Self {
                id: "online".to_string(),
                name: "在线音源".to_string(),
                kind,
                base_url: String::new(),
                script: "local.php".to_string(),
                prefix: String::new(),
                enabled: false,
                quality: Quality::K320,
                ..Default::default()
            },
        }
    }

    /// Resolve the full endpoint URL for an API action. `path_override` wins,
    /// then the source-specific endpoint override, then the default script.
    pub fn endpoint_url(&self, action: &str, path_override: Option<&str>) -> String {
        if let Some(path) = path_override {
            return join_url(&self.base_url, path);
        }
        if let Some(path) = self.endpoints.get(action) {
            return join_url(&self.base_url, path);
        }
        if self.base_url.contains(".php") {
            self.base_url.clone()
        } else {
            join_url(&self.base_url, &self.script)
        }
    }
}

fn join_url(base: &str, path: &str) -> String {
    let base = base.trim_end_matches('/');
    if path.starts_with("http://") || path.starts_with("https://") {
        return path.to_string();
    }
    if path.starts_with('/') {
        // Absolute path on the same host: strip the API directory's path.
        if let Some(idx) = base.find("://") {
            let after_scheme = &base[idx + 3..];
            if let Some(slash) = after_scheme.find('/') {
                let host = &base[..idx + 3 + slash];
                return format!("{}{}", host, path);
            }
        }
    }
    format!("{}/{}", base, path.trim_start_matches('/'))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct NetworkSettings {
    pub timeout_secs: u64,
    pub connect_timeout_secs: u64,
    pub retries: u32,
    pub proxy_mode: ProxyMode,
    pub proxy_url: String,
    pub user_agent: String,
    pub offline: bool,
    /// Minimum delay between consecutive API requests (0 = unlimited).
    /// Audio streaming and downloads are not throttled.
    pub api_min_interval_ms: u64,
    /// Page size for album/artist detail requests.
    pub album_page_size: u32,
    pub cache_limit_mb: u64,
    pub stream_cache_dir: Option<PathBuf>,
}

impl Default for NetworkSettings {
    fn default() -> Self {
        Self {
            timeout_secs: 10,
            connect_timeout_secs: 5,
            retries: 2,
            proxy_mode: ProxyMode::System,
            proxy_url: String::new(),
            user_agent: format!("BambooPlayer/{}", env!("CARGO_PKG_VERSION")),
            offline: false,
            api_min_interval_ms: 0,
            album_page_size: 100,
            cache_limit_mb: 1024,
            stream_cache_dir: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct LyricsSettings {
    pub enabled: bool,
    pub encoding: LyricsEncoding,
    pub offset_ms: i64,
    pub auto_load: bool,
    pub show_translation: bool,
    /// Fill style of the embedded lyrics panel.
    pub panel_background: LyricsPanelBackground,
    /// Solid fill / first gradient stop; `None` follows the theme panel color.
    pub panel_color: Option<[u8; 3]>,
    /// Second gradient stop.
    pub panel_color2: [u8; 3],
    /// Picture used by [`LyricsPanelBackground::Image`].
    pub panel_image: Option<PathBuf>,
    /// Panel border color; `None` follows the theme divider.
    pub panel_border_color: Option<[u8; 3]>,
    /// Preferred translation sidecar region (`auto`, `zh`, `en`, `jp`, …).
    /// Sidecars use the `A.<region>.lrc` / `A.<region>.srt` naming scheme.
    pub translation_language: String,
    pub karaoke: bool,
    /// Only run karaoke highlighting when the line carries word timings
    /// (enhanced LRC); plain timed LRC keeps whole-line highlighting.
    pub karaoke_words_only: bool,
    pub font_size: f32,
    pub font_family: Option<String>,
    /// 100–900, applied to the main row; the current row is 200 heavier (capped).
    pub font_weight: u16,
    pub italic: bool,
    /// Text decoration for the main row and (separately) the translation.
    pub outline_width: f32,
    pub outline_color: [u8; 3],
    pub shadow_offset: f32,
    pub shadow_color: [u8; 3],
    pub translation_outline_width: f32,
    pub translation_outline_color: [u8; 3],
    pub translation_shadow_offset: f32,
    pub translation_shadow_color: [u8; 3],
    /// Color of the karaoke/sung fill. Independent from the theme accent so
    /// each lyrics surface can match its own background.
    pub highlight_color: [u8; 3],
    pub current_color: [u8; 3],
    pub normal_color: [u8; 3],
    pub translation_color: [u8; 3],
    pub align: LyricsAlign,
    pub line_spacing: f32,
    pub show_source: bool,
    pub prefetch_next: bool,
    pub sidebar_visible: bool,
    pub offset_step_ms: i64,
    /// Smoothly animate the lyric auto-scroll (also requires global animations
    /// and not "reduce motion").
    pub scroll_animation: bool,
}

impl Default for LyricsSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            encoding: LyricsEncoding::Auto,
            offset_ms: 0,
            auto_load: true,
            show_translation: true,
            panel_background: LyricsPanelBackground::Theme,
            panel_color: None,
            panel_color2: [46, 52, 64],
            panel_image: None,
            panel_border_color: None,
            translation_language: "auto".to_string(),
            karaoke: true,
            karaoke_words_only: false,
            font_size: 17.0,
            font_family: None,
            font_weight: 400,
            italic: false,
            outline_width: 0.0,
            outline_color: [0, 0, 0],
            shadow_offset: 0.0,
            shadow_color: [0, 0, 0],
            translation_outline_width: 0.0,
            translation_outline_color: [0, 0, 0],
            translation_shadow_offset: 0.0,
            translation_shadow_color: [0, 0, 0],
            highlight_color: [143, 224, 181],
            current_color: [255, 255, 255],
            normal_color: [150, 160, 180],
            translation_color: [130, 142, 165],
            align: LyricsAlign::Center,
            line_spacing: 10.0,
            show_source: true,
            prefetch_next: true,
            sidebar_visible: false,
            offset_step_ms: 500,
            scroll_animation: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
/// Appearance of the immersive full-screen lyrics page and the standalone
/// lyrics window. Kept separate from the small panel/sidebar so the large
/// layouts can use their own type size and colors.
pub struct LargeLyricsSettings {
    pub font_size: f32,
    /// Optional system font family for the large/full-screen and standalone
    /// lyrics surfaces. `None` follows the interface font.
    pub font_family: Option<String>,
    pub font_weight: u16,
    pub italic: bool,
    pub outline_width: f32,
    pub outline_color: [u8; 3],
    pub shadow_offset: f32,
    pub shadow_color: [u8; 3],
    pub translation_outline_width: f32,
    pub translation_outline_color: [u8; 3],
    pub translation_shadow_offset: f32,
    pub translation_shadow_color: [u8; 3],
    pub line_spacing: f32,
    pub align: LyricsAlign,
    pub show_translation: bool,
    pub karaoke: bool,
    pub karaoke_words_only: bool,
    /// Karaoke/sung fill color for the standalone/full-screen lyric surfaces.
    pub highlight_color: [u8; 3],
    pub current_color: [u8; 3],
    pub normal_color: [u8; 3],
    pub translation_color: [u8; 3],
    pub show_cover: bool,
    pub show_title: bool,
    pub show_footer: bool,
}

impl Default for LargeLyricsSettings {
    fn default() -> Self {
        Self {
            font_size: 23.0,
            font_family: None,
            font_weight: 400,
            italic: false,
            outline_width: 0.0,
            outline_color: [0, 0, 0],
            shadow_offset: 0.0,
            shadow_color: [0, 0, 0],
            translation_outline_width: 0.0,
            translation_outline_color: [0, 0, 0],
            translation_shadow_offset: 0.0,
            translation_shadow_color: [0, 0, 0],
            line_spacing: 14.0,
            align: LyricsAlign::Center,
            show_translation: true,
            karaoke: true,
            karaoke_words_only: false,
            highlight_color: [143, 224, 181],
            current_color: [255, 255, 255],
            normal_color: [150, 160, 180],
            translation_color: [130, 142, 165],
            show_cover: true,
            show_title: true,
            show_footer: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct DesktopLyricsSettings {
    pub enabled: bool,
    pub visible_at_start: bool,
    pub locked: bool,
    pub click_through: bool,
    pub always_on_top: bool,
    pub lines: u8,
    pub show_translation: bool,
    pub show_footer: bool,
    pub font_family: Option<String>,
    /// Current line font size.
    pub font_size: f32,
    pub next_font_size: f32,
    pub prev_font_size: f32,
    pub footer_font_size: f32,
    /// Optional per-row font families; `None` follows `font_family`.
    pub translation_font_family: Option<String>,
    pub footer_font_family: Option<String>,
    pub bold: bool,
    pub italic: bool,
    /// Per-row weight overrides (translation / footer).
    pub translation_bold: bool,
    pub footer_bold: bool,
    pub align: LyricsAlign,
    pub current_color: [u8; 4],
    pub next_color: [u8; 4],
    pub prev_color: [u8; 4],
    pub footer_color: [u8; 4],
    pub translation_color: [u8; 4],
    /// Karaoke fill refresh rate; line changes still poll at 10 Hz.
    pub frame_rate: u32,
    pub background: DesktopBackgroundMode,
    pub bg_color: [u8; 4],
    pub bg_color2: [u8; 4],
    pub bg_image: Option<PathBuf>,
    pub opacity: f32,
    pub radius: u8,
    pub padding: f32,
    pub line_spacing: f32,
    pub shadow: bool,
    pub outline: bool,
    /// Apply the same outline/shadow treatment to the translation line.
    pub translation_outline: bool,
    pub translation_shadow: bool,
    /// Apply the same outline/shadow treatment to the footer (title/artist).
    pub footer_outline: bool,
    pub footer_shadow: bool,
    /// Draw the rounded panel border. Independent from the text shadow.
    pub border: bool,
    /// Native window drop shadow. Turn off for a fully transparent overlay
    /// so the system does not draw a shadow around an invisible rectangle.
    pub window_shadow: bool,
    pub outline_color: [u8; 4],
    pub outline_width: f32,
    pub shadow_color: [u8; 4],
    pub shadow_offset: f32,
    /// Per-surface decoration for the translation and footer rows.
    pub translation_outline_width: f32,
    pub translation_outline_color: [u8; 4],
    pub translation_shadow_offset: f32,
    pub translation_shadow_color: [u8; 4],
    pub footer_outline_width: f32,
    pub footer_outline_color: [u8; 4],
    pub footer_shadow_offset: f32,
    pub footer_shadow_color: [u8; 4],
    /// Blur radius (logical pixels) for the `BlurCover` background.
    pub blur_radius: f32,
    pub capture_protection: bool,
    pub monitor: Option<usize>,
    pub width: f32,
    pub height: f32,
    pub auto_size: bool,
    pub hide_when_paused: bool,
    pub hide_when_fullscreen: bool,
    pub karaoke: bool,
    /// Only run karaoke highlighting for enhanced LRC (word timings).
    pub karaoke_words_only: bool,
    pub position: Option<(f32, f32)>,
}

impl Default for DesktopLyricsSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            visible_at_start: false,
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
            align: LyricsAlign::Center,
            current_color: [255, 255, 255, 255],
            next_color: [170, 185, 215, 235],
            prev_color: [150, 162, 190, 220],
            footer_color: [200, 210, 230, 220],
            translation_color: [145, 158, 185, 210],
            frame_rate: 60,
            background: DesktopBackgroundMode::Solid,
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
            monitor: None,
            width: 900.0,
            height: 190.0,
            auto_size: true,
            hide_when_paused: false,
            hide_when_fullscreen: true,
            karaoke: true,
            karaoke_words_only: false,
            position: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct PlaylistSettings {
    pub auto_refresh_on_startup: bool,
    pub export_utf8_bom: bool,
}

impl Default for PlaylistSettings {
    fn default() -> Self {
        Self {
            auto_refresh_on_startup: true,
            export_utf8_bom: false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct LibrarySettings {
    pub folders: Vec<PathBuf>,
    pub scan_on_start: bool,
    pub extensions: Vec<String>,
    pub auto_watch: bool,
    pub follow_symlinks: bool,
    pub dedupe_mode: DedupeMode,
}

impl Default for LibrarySettings {
    fn default() -> Self {
        Self {
            folders: Vec::new(),
            scan_on_start: true,
            extensions: vec![
                "mp3".into(),
                "ogg".into(),
                "wav".into(),
                "flac".into(),
                "m4a".into(),
                "aac".into(),
                "opus".into(),
            ],
            auto_watch: true,
            follow_symlinks: false,
            dedupe_mode: DedupeMode::Metadata,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct DownloadSettings {
    pub dir: Option<PathBuf>,
    pub template: String,
    /// `None` follows the online source's quality; a value overrides it for
    /// every new download and is remembered per task so a retry keeps it.
    pub quality: Option<Quality>,
    pub concurrency: u8,
    pub conflict: ConflictPolicy,
    pub embed_tags: bool,
    pub embed_cover: bool,
    pub save_lyrics: bool,
    pub add_to_library: bool,
}

impl Default for DownloadSettings {
    fn default() -> Self {
        Self {
            dir: None,
            template: "{artist}/{album}/{title}".to_string(),
            quality: None,
            concurrency: 2,
            conflict: ConflictPolicy::Rename,
            embed_tags: true,
            embed_cover: true,
            save_lyrics: true,
            add_to_library: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct NotificationSettings {
    pub enabled: bool,
    pub only_when_unfocused: bool,
    pub show_cover: bool,
    pub on_track_change: bool,
}

impl Default for NotificationSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            only_when_unfocused: true,
            show_cover: true,
            on_track_change: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct VisualizerSettings {
    pub enabled: bool,
    pub mode: VisualizerMode,
    pub fps: u8,
    pub auto_hide: bool,
    pub color: [u8; 3],
    pub opacity: f32,
    pub sensitivity: f32,
}

impl Default for VisualizerSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: VisualizerMode::Bars,
            fps: 30,
            auto_hide: true,
            color: [104, 140, 255],
            opacity: 0.8,
            sensitivity: 1.0,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ShortcutSettings {
    pub play_pause: String,
    pub next: String,
    pub prev: String,
    pub volume_up: String,
    pub volume_down: String,
    pub toggle_desktop_lyrics: String,
    pub show_hide: String,
    pub enabled: bool,
    /// Map XInput button presses to playback commands.
    pub gamepad: bool,
}

/// One configurable global hotkey. Kept in the config module so the core and
/// the native shell agree on the settings field without sharing UI types.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ShortcutSlot {
    PlayPause,
    Next,
    Prev,
    VolumeUp,
    VolumeDown,
    ToggleDesktopLyrics,
    ShowHide,
}

impl ShortcutSlot {
    pub const ALL: [ShortcutSlot; 7] = [
        ShortcutSlot::PlayPause,
        ShortcutSlot::Next,
        ShortcutSlot::Prev,
        ShortcutSlot::VolumeUp,
        ShortcutSlot::VolumeDown,
        ShortcutSlot::ToggleDesktopLyrics,
        ShortcutSlot::ShowHide,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ShortcutSlot::PlayPause => "播放 / 暂停",
            ShortcutSlot::Next => "下一首",
            ShortcutSlot::Prev => "上一首",
            ShortcutSlot::VolumeUp => "音量 +5%",
            ShortcutSlot::VolumeDown => "音量 -5%",
            ShortcutSlot::ToggleDesktopLyrics => "桌面歌词",
            ShortcutSlot::ShowHide => "显示 / 隐藏窗口",
        }
    }
}

impl ShortcutSettings {
    pub fn get(&self, slot: ShortcutSlot) -> &str {
        match slot {
            ShortcutSlot::PlayPause => &self.play_pause,
            ShortcutSlot::Next => &self.next,
            ShortcutSlot::Prev => &self.prev,
            ShortcutSlot::VolumeUp => &self.volume_up,
            ShortcutSlot::VolumeDown => &self.volume_down,
            ShortcutSlot::ToggleDesktopLyrics => &self.toggle_desktop_lyrics,
            ShortcutSlot::ShowHide => &self.show_hide,
        }
    }

    pub fn set(&mut self, slot: ShortcutSlot, value: String) {
        match slot {
            ShortcutSlot::PlayPause => self.play_pause = value,
            ShortcutSlot::Next => self.next = value,
            ShortcutSlot::Prev => self.prev = value,
            ShortcutSlot::VolumeUp => self.volume_up = value,
            ShortcutSlot::VolumeDown => self.volume_down = value,
            ShortcutSlot::ToggleDesktopLyrics => self.toggle_desktop_lyrics = value,
            ShortcutSlot::ShowHide => self.show_hide = value,
        }
    }
}

impl Default for ShortcutSettings {
    fn default() -> Self {
        Self {
            play_pause: "Ctrl+Alt+P".to_string(),
            next: "Ctrl+Alt+Right".to_string(),
            prev: "Ctrl+Alt+Left".to_string(),
            volume_up: "Ctrl+Alt+Up".to_string(),
            volume_down: "Ctrl+Alt+Down".to_string(),
            toggle_desktop_lyrics: "Ctrl+Alt+M".to_string(),
            show_hide: "Ctrl+Alt+Q".to_string(),
            enabled: true,
            gamepad: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RadioStation {
    pub name: String,
    pub url: String,
    pub genre: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct RadioSettings {
    pub stations: Vec<RadioStation>,
    /// URL of the station played most recently (for a quick resume).
    pub last_station: Option<String>,
}

impl Default for RadioSettings {
    fn default() -> Self {
        // Discover now only exposes the local BambooMusic random station; the
        // old SomaFM presets are intentionally not seeded anymore.
        Self {
            stations: Vec::new(),
            last_station: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct MiniPlayerSettings {
    pub visible: bool,
    pub always_on_top: bool,
    pub opacity: f32,
    pub position: Option<(f32, f32)>,
    /// Show the current lyric line between the track info and the progress bar.
    pub show_lyrics: bool,
    /// Fill the syllable being sung instead of highlighting the whole line.
    pub karaoke: bool,
    /// Apply karaoke fill only to enhanced LRC lines that carry word timings.
    /// When off, plain timed LRC lines get a proportional gradient fill too.
    pub karaoke_words_only: bool,
    /// Number of lyric lines to show (1 or 2).
    pub lyrics_lines: u8,
    pub lyrics_bold: bool,
    /// Show the translation of the current line (if the lyrics have one).
    pub show_translation: bool,
    pub lyrics_font_size: f32,
    /// Progress/lyric animation refresh rate for the native mini player.
    pub frame_rate: u32,
    /// Window width; drag-resizing the window updates it.
    pub width: f32,
    /// Derive the window height from the lyric options (desktop lyrics style).
    pub auto_height: bool,
    /// Manual window height, used when `auto_height` is off.
    pub height: f32,
    /// Show the mini-player button in the Windows taskbar.
    pub show_in_taskbar: bool,
    /// Draw the mini player with a native Direct2D layered window. This keeps
    /// the main egui window out of the mini player's animation loop, so a
    /// hidden main window no longer has to repaint just to advance the mini
    /// player. Turn it off to fall back to the egui viewport.
    pub native_window: bool,
    /// Panel/card background color (RGB). Follows the dark look by default;
    /// card, hover, divider and weak text colors are derived from it.
    pub panel_color: [u8; 3],
    /// Primary text color (RGB); weak/faint variants are derived.
    pub text_color: [u8; 3],
    /// Accent color used for the playing progress and active buttons.
    pub accent_color: [u8; 3],
}

impl Default for MiniPlayerSettings {
    fn default() -> Self {
        Self {
            visible: false,
            always_on_top: true,
            opacity: 0.94,
            position: None,
            show_lyrics: true,
            karaoke: true,
            karaoke_words_only: false,
            lyrics_lines: 1,
            lyrics_bold: false,
            show_translation: true,
            lyrics_font_size: 13.0,
            frame_rate: 60,
            width: 380.0,
            auto_height: true,
            height: 172.0,
            show_in_taskbar: false,
            native_window: true,
            panel_color: [37, 39, 43],
            text_color: [232, 235, 238],
            accent_color: [127, 211, 168],
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub version: u32,
    pub language: Language,
    pub theme: ThemeSettings,
    pub playback: PlaybackSettings,
    pub window: WindowSettings,
    pub features: FeatureToggles,
    pub sources: Vec<SourceConfig>,
    /// Source id selected in the settings UI and used by search/playback.
    /// `None` falls back to the first enabled source.
    pub active_source: Option<String>,
    pub network: NetworkSettings,
    pub lyrics: LyricsSettings,
    pub large_lyrics: LargeLyricsSettings,
    pub desktop_lyrics: DesktopLyricsSettings,
    pub playlist: PlaylistSettings,
    pub library: LibrarySettings,
    pub performance: PerformanceSettings,
    pub download: DownloadSettings,
    pub notifications: NotificationSettings,
    pub visualizer: VisualizerSettings,
    pub shortcuts: ShortcutSettings,
    pub radio: RadioSettings,
    pub mini: MiniPlayerSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            language: Language::ZhCn,
            theme: ThemeSettings::default(),
            playback: PlaybackSettings::default(),
            window: WindowSettings::default(),
            features: FeatureToggles::default(),
            sources: vec![
                SourceConfig::preset(SourceKind::Local),
                SourceConfig::preset(SourceKind::Online),
            ],
            active_source: None,
            network: NetworkSettings::default(),
            lyrics: LyricsSettings::default(),
            large_lyrics: LargeLyricsSettings::default(),
            desktop_lyrics: DesktopLyricsSettings::default(),
            playlist: PlaylistSettings::default(),
            library: LibrarySettings::default(),
            performance: PerformanceSettings::default(),
            download: DownloadSettings::default(),
            notifications: NotificationSettings::default(),
            visualizer: VisualizerSettings::default(),
            shortcuts: ShortcutSettings::default(),
            radio: RadioSettings::default(),
            mini: MiniPlayerSettings::default(),
        }
    }
}

impl Settings {
    fn migrate(&mut self) {
        if self.version == 0 {
            self.version = CONFIG_VERSION;
        }
        // Future migrations go here, keyed on self.version.
        if self.playback.speed <= 0.0 {
            self.playback.speed = 1.0;
        }
        self.playback.volume = self.playback.volume.clamp(0.0, 1.0);
        // 2026-11-11 global-control defaults. Only rewrite the old default
        // combinations, never a user-customized binding.
        let shortcuts = &mut self.shortcuts;
        if shortcuts.play_pause == "Ctrl+Alt+Space" {
            shortcuts.play_pause = "Ctrl+Alt+P".to_string();
        }
        if shortcuts.toggle_desktop_lyrics == "Ctrl+Alt+L" {
            shortcuts.toggle_desktop_lyrics = "Ctrl+Alt+M".to_string();
        }
        if shortcuts.show_hide == "Ctrl+Alt+M" {
            shortcuts.show_hide = "Ctrl+Alt+Q".to_string();
        }
        // Solid, blurred cover and a user picture are supported. Mica and flow
        // gradients produced stale pixels on GDI surfaces; map old values onto
        // the supported looks. A picture mode without a path falls back to
        // solid instead of leaving an empty background.
        self.theme.background = match self.theme.background {
            BackgroundMode::Solid => BackgroundMode::Solid,
            BackgroundMode::StaticBlur | BackgroundMode::GpuBlur => {
                BackgroundMode::StaticBlur
            }
            BackgroundMode::Image => {
                if self.theme.background_image.is_some() {
                    BackgroundMode::Image
                } else {
                    BackgroundMode::Solid
                }
            }
            BackgroundMode::Flow | BackgroundMode::Mica => BackgroundMode::Solid,
        };
        // The accent color used to be stored but never rendered. Treat the old
        // default as "unset" and adopt the Bamboo green so an upgrade does not
        // silently repaint the whole UI blue.
        if self.theme.accent == [104, 140, 255] {
            self.theme.accent = [127, 211, 168];
        }
        // Discover is now the local BambooMusic random station only. Drop the
        // old preset/custom stream list so it cannot reappear in settings.
        self.radio.stations.clear();
        // Older builds shipped Kuwo/Netease/QQ presets. Keep only the local
        // source and one generic, user-configured online source.
        self.sources
            .retain(|s| !matches!(s.id.as_str(), "kuwo" | "netease" | "qq"));
        if !self.sources.iter().any(|s| s.kind == SourceKind::Local) {
            self.sources
                .insert(0, SourceConfig::preset(SourceKind::Local));
        }
        if !self.sources.iter().any(|s| s.id == "online") {
            self.sources.push(SourceConfig::preset(SourceKind::Online));
        }
        if self.sources.is_empty() {
            self.sources = Settings::default().sources;
        }
    }

    pub fn source(&self, id: &str) -> Option<&SourceConfig> {
        self.sources.iter().find(|s| s.id == id)
    }

    pub fn enabled_sources(&self) -> impl Iterator<Item = &SourceConfig> {
        self.sources.iter().filter(|s| s.enabled)
    }
}

/// A local file shown in the taskbar jump list's "recent" category.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RecentFile {
    pub path: PathBuf,
    pub title: String,
}

/// Shuffled order remembered for one `.mp3list`, keyed by its path.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PlaylistShuffle {
    pub path: PathBuf,
    /// Track keys in shuffled playback order.
    pub keys: Vec<String>,
    /// Position of the last played entry in `keys` (fallback when the track
    /// key itself is no longer in the list).
    pub cursor: usize,
    /// Key of the last played track, preferred over `cursor`.
    pub cursor_key: Option<String>,
}

/// A `.mp3list` / `.m3u` the user has opened. The file itself is the source of
/// truth; this only remembers where it is and how recently it was used.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SavedPlaylist {
    pub path: PathBuf,
    /// Cached display name (the file stem when first imported).
    pub name: String,
    /// Unix seconds of the last import/open, used to keep the list ordered.
    pub last_opened: u64,
}

impl SavedPlaylist {
    pub fn new(path: PathBuf, last_opened: u64) -> Self {
        let name = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().trim().to_string())
            .filter(|stem| !stem.is_empty())
            .unwrap_or_else(|| path.display().to_string());
        Self {
            path,
            name,
            last_opened,
        }
    }

    /// Cached name with a fallback for hand-edited or older session files.
    pub fn display_name(&self) -> String {
        let name = self.name.trim();
        if !name.is_empty() {
            return name.to_string();
        }
        self.path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .filter(|stem| !stem.trim().is_empty())
            .unwrap_or_else(|| self.path.display().to_string())
    }
}

/// Keep the sidebar usable even after the user has opened many playlists.
pub const MAX_SAVED_PLAYLISTS: usize = 24;

/// Playback state persisted on exit and periodically while playing.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Session {
    pub queue: Vec<Track>,
    pub current_index: Option<usize>,
    /// Legacy per-track second. No longer written: only the playlist position
    /// (`current_key` / `current_index`) is restored, always at 0:00. Kept so
    /// older session files still deserialize.
    pub position_secs: f64,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    pub volume: f32,
    pub page: Page,
    pub playlist_scroll: f32,
    pub window_pos: Option<(f32, f32)>,
    pub window_size: Option<(f32, f32)>,
    pub window_maximized: bool,
    pub desktop_lyrics_pos: Option<(f32, f32)>,
    pub desktop_lyrics_monitor: Option<usize>,
    pub current_key: Option<String>,
    /// Whether the immersive lyrics page was open at exit.
    pub fullscreen_lyrics: bool,
    /// Whether the independent fullscreen lyrics window was open at exit.
    pub lyrics_window: bool,
    /// Whether that window was used as a normal resizable window instead of
    /// borderless fullscreen.
    pub lyrics_window_windowed: bool,
    /// Remembered size of the windowed standalone lyrics window.
    pub lyrics_window_size: Option<(f32, f32)>,
    /// Remembered position of the windowed standalone lyrics window.
    pub lyrics_window_pos: Option<(f32, f32)>,
    /// Display order of the shuffle cycle, stored as stable track keys so the
    /// order survives playlist re-imports and file additions.
    pub shuffle_keys: Vec<String>,
    /// Index into `shuffle_keys` of the track that was playing. Used as a
    /// fallback when the track itself disappeared (renamed/removed file).
    pub shuffle_cursor: Option<usize>,
    /// Per-playlist shuffle cache: reopening the same `.mp3list` keeps its
    /// previous shuffled order and progress, appending newly discovered files
    /// to the end of the cycle instead of reshuffling everything.
    pub playlist_shuffle: Vec<PlaylistShuffle>,
    pub playlist_path: Option<PathBuf>,
    /// Recently opened playlists (newest first). Kept for future playlist
    /// management; the sidebar no longer lists them.
    pub playlists: Vec<SavedPlaylist>,
    /// Whether the local random station was active at exit.
    pub station: bool,
    /// Most recent station tracks (newest first), for the Discover history.
    pub station_history: Vec<Track>,
    /// Most recent local files (newest first), mirrored to the jump list.
    pub recent_files: Vec<RecentFile>,
    /// Last completed index summary. The startup "索引完成" toast is only
    /// shown when these numbers change, so a routine launch stays quiet.
    pub last_indexed: Option<usize>,
    pub last_index_missing: Option<u64>,
    pub saved_at: Option<String>,
}

impl Session {
    /// Move `path` to the front of the saved-playlist list, creating it if
    /// needed. Paths compare case-insensitively on Windows.
    pub fn remember_playlist(&mut self, path: PathBuf, now: u64) {
        let key = crate::model::normalize_path(&path);
        self.playlists
            .retain(|entry| crate::model::normalize_path(&entry.path) != key);
        self.playlists.insert(0, SavedPlaylist::new(path, now));
        self.playlists.truncate(MAX_SAVED_PLAYLISTS);
    }

    /// Remove one entry from “My playlists”. Returns true when it was present.
    /// The file on disk and the current queue are not touched.
    pub fn forget_playlist(&mut self, path: &Path) -> bool {
        let key = crate::model::normalize_path(path);
        let before = self.playlists.len();
        self.playlists
            .retain(|entry| crate::model::normalize_path(&entry.path) != key);
        self.playlists.len() != before
    }

    #[cfg(test)]
    pub fn has_playlist(&self, path: &Path) -> bool {
        let key = crate::model::normalize_path(path);
        self.playlists
            .iter()
            .any(|entry| crate::model::normalize_path(&entry.path) == key)
    }
}

/// Current Unix time in whole seconds, used for saved-playlist ordering.
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Paths and file IO
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Paths {
    pub root: PathBuf,
    pub cache: PathBuf,
    pub covers: PathBuf,
    pub lyrics_cache: PathBuf,
    pub stream_cache: PathBuf,
    pub logs: PathBuf,
    pub settings_file: PathBuf,
    pub session_file: PathBuf,
    pub library_db: PathBuf,
    pub portable: bool,
}

impl Paths {
    pub fn discover() -> Self {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf));
        let portable = exe_dir
            .as_ref()
            .map(|d| d.join("portable.txt").exists())
            .unwrap_or(false);

        let root = if portable {
            exe_dir.clone().unwrap_or_else(|| PathBuf::from("."))
        } else {
            dirs::data_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("BambooMusicPlayer")
        };

        let cache = root.join("cache");
        let paths = Self {
            covers: cache.join("covers"),
            lyrics_cache: cache.join("lyrics"),
            stream_cache: cache.join("stream"),
            cache: cache.clone(),
            logs: root.join("logs"),
            settings_file: root.join("settings.json"),
            session_file: root.join("session.json"),
            library_db: root.join("library.db"),
            portable,
            root,
        };
        for dir in [
            &paths.root,
            &paths.cache,
            &paths.covers,
            &paths.lyrics_cache,
            &paths.stream_cache,
            &paths.logs,
        ] {
            let _ = fs::create_dir_all(dir);
        }
        paths
    }

    pub fn default_download_dir(&self) -> PathBuf {
        dirs::audio_dir()
            .unwrap_or_else(|| self.root.clone())
            .join("BambooPlayer")
    }
}

pub fn load_json<T: DeserializeOwned + Default>(path: &Path) -> T {
    match fs::read(path) {
        Ok(bytes) => match serde_json::from_slice::<T>(&bytes) {
            Ok(value) => value,
            Err(err) => {
                tracing::warn!("failed to parse {}: {err}", path.display());
                T::default()
            }
        },
        Err(err) if err.kind() == io::ErrorKind::NotFound => T::default(),
        Err(err) => {
            tracing::warn!("failed to read {}: {err}", path.display());
            T::default()
        }
    }
}

pub fn load_settings(path: &Path) -> Settings {
    let mut settings = load_json::<Settings>(path);
    settings.migrate();
    settings
}

/// Import one settings file without the lenient fallback used at startup.
/// A malformed file must be reported to the user instead of silently
/// replacing their configuration with defaults.
pub fn load_settings_strict(path: &Path) -> Result<Settings, String> {
    let text = std::fs::read_to_string(path).map_err(|err| err.to_string())?;
    let mut settings: Settings = serde_json::from_str(&text).map_err(|err| err.to_string())?;
    settings.migrate();
    Ok(settings)
}

pub fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    write_json_atomic_with(path, value, true)
}

fn write_json_atomic_with<T: Serialize>(path: &Path, value: &T, pretty: bool) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let data = if pretty {
        serde_json::to_vec_pretty(value)
    } else {
        serde_json::to_vec(value)
    }
    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    // Include the PID so two instances writing the same settings/session file
    // never contend for the same temporary name.
    let extension = path
        .extension()
        .map(|extension| extension.to_string_lossy().into_owned())
        .unwrap_or_else(|| "json".to_string());
    let tmp = path.with_extension(format!("{extension}.{}.tmp", std::process::id()));
    fs::write(&tmp, data)?;
    match fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(_) => {
            // Windows can refuse to replace an open file; retry after removing.
            let _ = fs::remove_file(path);
            fs::rename(&tmp, path)
        }
    }
}

pub fn save_settings(path: &Path, settings: &Settings) -> io::Result<()> {
    write_json_atomic(path, settings)
}

/// Sessions contain the whole queue, so they are written compactly; pretty
/// printing a 10k-track queue wastes CPU and disk on every autosave.
pub fn save_session(path: &Path, session: &Session) -> io::Result<()> {
    write_json_atomic_with(path, session, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_rate_is_clamped_to_safe_range() {
        let mut performance = PerformanceSettings::default();
        assert_eq!(performance.frame_rate, 60);
        assert_eq!(performance.frame_millis(), 16);
        performance.frame_rate = 15;
        assert_eq!(performance.frame_millis(), 66);
        performance.frame_rate = 240;
        assert_eq!(performance.frame_millis(), 8);
        performance.frame_rate = 0;
        assert_eq!(performance.frame_millis(), 66);
    }

    #[test]
    fn idle_and_background_intervals_are_clamped() {
        let mut performance = PerformanceSettings::default();
        assert_eq!(performance.idle_millis(), 66);
        performance.idle_frame_rate = 1;
        assert_eq!(performance.idle_millis(), 500);
        performance.idle_frame_rate = 0;
        assert_eq!(performance.idle_millis(), 500);
        performance.idle_frame_rate = 1000;
        assert_eq!(performance.idle_millis(), 8);    }

    #[test]
    fn large_lyrics_defaults_are_independent_from_the_small_panel() {
        let lyrics = LyricsSettings::default();
        let large = LargeLyricsSettings::default();
        assert!(large.font_size > lyrics.font_size);
        assert!(large.show_cover);
        assert!(large.show_title);
        assert!(large.show_footer);
    }

    #[test]
    fn settings_round_trip_preserves_new_fields() {
        let mut settings = Settings::default();
        settings.theme.gpu_power = GpuPowerPreference::HighPerformance;
        settings.playback.crossfade_secs = 3.5;
        settings.playback.speed = 1.25;
        settings.mini.visible = true;
        settings.mini.native_window = false;
        settings.window.allow_multiple_instances = true;
        settings.window.lyrics_monitor = Some(1);
        settings.visualizer.enabled = true;
        settings.visualizer.mode = VisualizerMode::Bars;

        let text = serde_json::to_string(&settings).expect("serialize settings");
        let restored: Settings = serde_json::from_str(&text).expect("deserialize settings");
        assert!((restored.playback.crossfade_secs - 3.5).abs() < 1e-6);
        assert!((restored.playback.speed - 1.25).abs() < 1e-6);
        assert!(restored.mini.visible);
        assert!(!restored.mini.native_window);
        assert_eq!(
            restored.theme.gpu_power,
            GpuPowerPreference::HighPerformance
        );
        assert!(restored.window.allow_multiple_instances);
        assert_eq!(restored.window.lyrics_monitor, Some(1));
        assert!(restored.visualizer.enabled);
        assert_eq!(restored.visualizer.mode, VisualizerMode::Bars);
    }

    #[test]
    fn migrate_clamps_values_and_removes_legacy_sources() {
        let mut settings = Settings::default();
        settings.version = 0;
        settings.playback.speed = 0.0;
        settings.playback.volume = 2.0;
        settings.radio.stations.push(RadioStation {
            name: "Old".to_string(),
            url: "https://example.invalid/stream".to_string(),
            genre: String::new(),
        });
        let mut legacy = SourceConfig::preset(SourceKind::Online);
        legacy.id = "kuwo".to_string();
        settings.sources.push(legacy);

        settings.migrate();

        assert_eq!(settings.version, CONFIG_VERSION);
        assert!((settings.playback.speed - 1.0).abs() < 1e-6);
        assert!((settings.playback.volume - 1.0).abs() < 1e-6);
        assert!(settings.radio.stations.is_empty());
        assert!(!settings.sources.iter().any(|source| source.id == "kuwo"));
        assert!(
            settings
                .sources
                .iter()
                .any(|source| source.kind == SourceKind::Local)
        );
        assert!(settings.sources.iter().any(|source| source.id == "online"));
    }

    #[test]
    fn load_settings_returns_defaults_for_invalid_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "{ this is not json").unwrap();

        let settings = load_settings(&path);
        assert_eq!(settings.version, CONFIG_VERSION);
        assert!(!settings.sources.is_empty());
    }

    #[test]
    fn migrate_upgrades_the_old_unused_accent_but_keeps_custom_colors() {
        let mut settings = Settings::default();
        settings.theme.accent = [104, 140, 255];
        settings.migrate();
        assert_eq!(settings.theme.accent, ThemeSettings::default().accent);

        settings.theme.accent = [200, 40, 90];
        settings.migrate();
        assert_eq!(settings.theme.accent, [200, 40, 90]);
    }

    #[test]
    fn atomic_writes_replace_existing_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let mut settings = Settings::default();
        settings.playback.volume = 0.25;
        save_settings(&path, &settings).unwrap();
        settings.playback.volume = 0.75;
        save_settings(&path, &settings).unwrap();

        let restored = load_settings(&path);
        assert!((restored.playback.volume - 0.75).abs() < 1e-6);
        assert!(
            !path
                .with_extension(format!("json.{}.tmp", std::process::id()))
                .exists()
        );
    }

    #[test]
    fn session_round_trip_keeps_station_history() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.json");
        let mut first = Track::local("C:/music/first.mp3");
        first.title = "First".to_string();
        let mut second = Track::local("C:/music/second.mp3");
        second.title = "Second".to_string();
        let session = Session {
            station: true,
            lyrics_window: true,
            lyrics_window_windowed: true,
            station_history: vec![second.clone(), first.clone()],
            recent_files: vec![RecentFile {
                path: first.source.path().unwrap().to_path_buf(),
                title: "First".to_string(),
            }],
            current_key: Some(second.dedupe_key()),
            ..Session::default()
        };
        save_session(&path, &session).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        let restored: Session = serde_json::from_str(&raw).unwrap();
        assert!(restored.station);
        assert!(restored.lyrics_window);
        assert!(restored.lyrics_window_windowed);
        assert_eq!(restored.station_history.len(), 2);
        assert_eq!(restored.station_history[0].title, "Second");
        assert_eq!(restored.recent_files.len(), 1);
        assert_eq!(restored.recent_files[0].title, "First");
        assert_eq!(
            restored.current_key.as_deref(),
            Some(second.dedupe_key().as_str())
        );
    }

    #[test]
    fn saved_playlists_dedupe_and_keep_newest_first() {
        let mut session = Session::default();
        session.remember_playlist(PathBuf::from("C:/music/one.mp3list"), 10);
        session.remember_playlist(PathBuf::from("C:/music/two.m3u"), 20);
        session.remember_playlist(PathBuf::from("C:/Music/ONE.mp3list"), 30);
        assert_eq!(
            session.playlists.len(),
            2,
            "path comparison is case-insensitive"
        );
        assert_eq!(
            session.playlists[0].path,
            PathBuf::from("C:/Music/ONE.mp3list")
        );
        assert_eq!(session.playlists[0].name, "ONE");
        assert_eq!(session.playlists[0].last_opened, 30);
        assert!(!session.forget_playlist(Path::new("C:/music/missing.mp3list")));
        assert!(session.forget_playlist(Path::new("C:/music/two.m3u")));
        assert_eq!(session.playlists.len(), 1);
        assert!(session.has_playlist(Path::new("C:/music/one.mp3list")));
    }

    #[test]
    fn saved_playlists_are_capped_and_persisted() {
        let mut session = Session::default();
        for index in 0..(MAX_SAVED_PLAYLISTS + 5) {
            session.remember_playlist(
                PathBuf::from(format!("C:/music/list-{index}.mp3list")),
                index as u64,
            );
        }
        assert_eq!(session.playlists.len(), MAX_SAVED_PLAYLISTS);
        assert_eq!(
            session.playlists[0].name,
            format!("list-{}", MAX_SAVED_PLAYLISTS + 4)
        );

        let raw = serde_json::to_string(&session).unwrap();
        let restored: Session = serde_json::from_str(&raw).unwrap();
        assert_eq!(restored.playlists.len(), MAX_SAVED_PLAYLISTS);
        let newest = MAX_SAVED_PLAYLISTS + 4;
        assert_eq!(
            restored.playlists[0].display_name(),
            format!("list-{newest}")
        );
        assert_eq!(restored.playlists[0].last_opened, newest as u64);
    }
}

#[cfg(test)]
mod overlay_settings_tests {
    use super::*;

    #[test]
    fn overlay_positions_deserialize_from_array() {
        let json = br#"{
          "desktop_lyrics": { "enabled": true, "visible_at_start": true, "position": [80, 80], "auto_size": false, "width": 760, "height": 200 },
          "mini": { "visible": true, "position": [80, 380] }
        }"#;
        let settings: Settings = serde_json::from_slice(json).unwrap();
        assert_eq!(settings.desktop_lyrics.position, Some((80.0, 80.0)));
        assert_eq!(settings.mini.position, Some((80.0, 380.0)));
    }
}
