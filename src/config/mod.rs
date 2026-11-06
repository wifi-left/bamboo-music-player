//! Persistent settings and playback session (exit cache).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{de::DeserializeOwned, Deserialize, Serialize};

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

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ListDensity {
    Compact,
    #[default]
    Comfortable,
    Spacious,
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

impl SourceKind {
    pub fn default_script(self) -> &'static str {
        match self {
            Self::Local | Self::Online => "local.php",
        }
    }

    pub fn default_prefix(self) -> &'static str {
        ""
    }
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
            Self::Flac => "无损 FLAC",
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
}

impl Default for ThemeSettings {
    fn default() -> Self {
        Self {
            mode: ThemeMode::Dark,
            accent: [104, 140, 255],
            background: BackgroundMode::StaticBlur,
            background_image: None,
            blur_radius: 24.0,
            background_opacity: 0.45,
            font_family: Some("Microsoft YaHei UI".to_string()),
            font_size: 14.0,
            roundness: 12,
            animations: true,
            reduce_motion: false,
            list_density: ListDensity::Comfortable,
            sidebar_width: 220.0,
            backend: WgpuBackend::Auto,
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
    pub minimize_to_tray: bool,
    pub capture_protection: bool,
    pub remember_size: bool,
    pub trim_working_set_on_idle: bool,
    pub trim_idle_secs: u64,
}

impl Default for WindowSettings {
    fn default() -> Self {
        Self {
            remember_position: true,
            close_behavior: CloseBehavior::Tray,
            start_minimized: false,
            minimize_to_tray: true,
            capture_protection: false,
            remember_size: true,
            trim_working_set_on_idle: true,
            trim_idle_secs: 30,
        }
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
    pub advanced_network: bool,
    pub mini_player: bool,
    pub fullscreen_lyrics: bool,
    pub visualizer: bool,
    pub notifications: bool,
    pub jump_list: bool,
    pub start_with_windows: bool,
    pub file_associations: bool,
    pub command_palette: bool,
    pub accessibility: bool,
    pub tray: bool,
    pub desktop_lyrics: bool,
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
            advanced_network: true,
            mini_player: true,
            fullscreen_lyrics: true,
            visualizer: true,
            notifications: true,
            jump_list: true,
            start_with_windows: false,
            file_associations: false,
            command_palette: true,
            accessibility: true,
            tray: true,
            desktop_lyrics: true,
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

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
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
    pub karaoke: bool,
    pub font_size: f32,
    pub font_family: Option<String>,
    pub current_color: [u8; 3],
    pub normal_color: [u8; 3],
    pub translation_color: [u8; 3],
    pub align: LyricsAlign,
    pub line_spacing: f32,
    pub show_source: bool,
    pub prefetch_next: bool,
    pub sidebar_visible: bool,
    pub offset_step_ms: i64,
}

impl Default for LyricsSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            encoding: LyricsEncoding::Auto,
            offset_ms: 0,
            auto_load: true,
            show_translation: true,
            karaoke: true,
            font_size: 17.0,
            font_family: None,
            current_color: [255, 255, 255],
            normal_color: [150, 160, 180],
            translation_color: [130, 142, 165],
            align: LyricsAlign::Center,
            line_spacing: 10.0,
            show_source: true,
            prefetch_next: true,
            sidebar_visible: false,
            offset_step_ms: 500,
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
    pub font_size: f32,
    pub bold: bool,
    pub align: LyricsAlign,
    pub current_color: [u8; 4],
    pub next_color: [u8; 4],
    pub translation_color: [u8; 4],
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
    pub capture_protection: bool,
    pub monitor: Option<usize>,
    pub width: f32,
    pub height: f32,
    pub auto_size: bool,
    pub hide_when_paused: bool,
    pub hide_when_fullscreen: bool,
    pub karaoke: bool,
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
            bold: true,
            align: LyricsAlign::Center,
            current_color: [255, 255, 255, 255],
            next_color: [170, 185, 215, 235],
            translation_color: [145, 158, 185, 210],
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
            capture_protection: false,
            monitor: None,
            width: 900.0,
            height: 190.0,
            auto_size: true,
            hide_when_paused: false,
            hide_when_fullscreen: true,
            karaoke: true,
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
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct DownloadSettings {
    pub dir: Option<PathBuf>,
    pub template: String,
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
}

impl Default for ShortcutSettings {
    fn default() -> Self {
        Self {
            play_pause: "Ctrl+Alt+Space".to_string(),
            next: "Ctrl+Alt+Right".to_string(),
            prev: "Ctrl+Alt+Left".to_string(),
            volume_up: "Ctrl+Alt+Up".to_string(),
            volume_down: "Ctrl+Alt+Down".to_string(),
            toggle_desktop_lyrics: "Ctrl+Alt+L".to_string(),
            show_hide: "Ctrl+Alt+M".to_string(),
            enabled: true,
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
    pub network: NetworkSettings,
    pub lyrics: LyricsSettings,
    pub desktop_lyrics: DesktopLyricsSettings,
    pub playlist: PlaylistSettings,
    pub library: LibrarySettings,
    pub download: DownloadSettings,
    pub notifications: NotificationSettings,
    pub visualizer: VisualizerSettings,
    pub shortcuts: ShortcutSettings,
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
            network: NetworkSettings::default(),
            lyrics: LyricsSettings::default(),
            desktop_lyrics: DesktopLyricsSettings::default(),
            playlist: PlaylistSettings::default(),
            library: LibrarySettings::default(),
            download: DownloadSettings::default(),
            notifications: NotificationSettings::default(),
            visualizer: VisualizerSettings::default(),
            shortcuts: ShortcutSettings::default(),
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

/// Playback state persisted on exit and periodically while playing.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Session {
    pub queue: Vec<Track>,
    pub current_index: Option<usize>,
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
    pub playlist_path: Option<PathBuf>,
    pub saved_at: Option<String>,
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

pub fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_vec_pretty(value)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let tmp = path.with_extension("json.tmp");
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

pub fn save_session(path: &Path, session: &Session) -> io::Result<()> {
    write_json_atomic(path, session)
}
