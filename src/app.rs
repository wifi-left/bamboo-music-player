//! UI state, input handling and dirty-rect invalidation.
//!
//! The app never repaints on its own: state changes mark the affected widget
//! rectangles with `InvalidateRect`, and `WM_PAINT` clips drawing to the union
//! of those rectangles. Page switches, track changes and resizes are treated
//! as large state changes and redraw the whole window once.
//!
//! Business state (playback, queue, lyrics, settings) lives in
//! [`crate::core::app::AppCore`]; this type only owns layout, scroll, selection
//! and the dirty-rect bookkeeping.

use std::sync::Arc;
use std::time::Instant;

use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{InvalidateRect, ScreenToClient};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GW_HWNDPREV, GetClientRect, GetWindow, GetWindowLongPtrW, GetWindowRect,
    IsIconic, IsWindowVisible, PostMessageW, WM_LBUTTONDOWN, WM_LBUTTONUP, WS_EX_LAYERED,
    WS_EX_TRANSPARENT,
};

use crate::core::app::{AppCore, Command};
use crate::core::config::{
    CloseBehavior, DedupeMode, EqPreset, ListDensity, LyricsAlign, LyricsPanelBackground,
    Paths, ReplayGainMode, ShortcutSlot, ThemeMode,
};
use crate::core::library::LibrarySort;
use crate::core::model::{Page as CorePage, Track, TrackSource};
use crate::layout::{self, Layout, Rect};
use crate::ui::{ButtonState, DownloadAction, Input, Interaction, Target};

/// Snapshot delta used by `App::tick` to pick the smallest repaint regions.
struct TickState {
    index: Option<usize>,
    playing: bool,
    state: crate::core::audio::PlaybackState,
    duration: f32,
    volume: f32,
    muted: bool,
    shuffle: bool,
    repeat: crate::core::model::RepeatMode,
    lyrics_rev: u64,
    lyrics_background_rev: u64,
    background_image_rev: u64,
    lyrics_line: Option<usize>,
    library_rev: u64,
    cover_rev: u64,
    row_cover_rev: u64,
    sleep_remaining: Option<u64>,
    queue_rev: u64,
    browse_rev: u64,
    online_rev: u64,
    discover_rev: u64,
    download_rev: u64,
    cache_rev: u64,
}

/// One visible list row, normalized across the queue and the library.
pub struct RowInfo<'a> {
    pub title: &'a str,
    pub artist: &'a str,
    pub duration: Option<f64>,
    pub current: bool,
    pub missing: bool,
    /// Play count from the library index; `None` for queue rows.
    pub play_count: Option<i64>,
    /// Duplicate-page group header (not playable).
    pub header: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    NowPlaying,
    Queue,
    Library,
    Folders,
    Search,
    Discover,
    Statistics,
    Smart,
    Duplicates,
    Downloads,
    Settings,
}

impl Page {
    pub const ALL: [Page; 11] = [
        Page::NowPlaying,
        Page::Queue,
        Page::Library,
        Page::Folders,
        Page::Search,
        Page::Discover,
        Page::Statistics,
        Page::Smart,
        Page::Duplicates,
        Page::Downloads,
        Page::Settings,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Page::NowPlaying => "正在播放",
            Page::Queue => "播放队列",
            Page::Library => "曲库",
            Page::Folders => "文件夹",
            Page::Search => "搜索",
            Page::Discover => "发现",
            Page::Statistics => "播放统计",
            Page::Smart => "智能歌单",
            Page::Duplicates => "重复文件",
            Page::Downloads => "下载管理",
            Page::Settings => "设置",
        }
    }

    fn to_core(self) -> CorePage {
        match self {
            Page::NowPlaying | Page::Queue => CorePage::NowPlaying,
            Page::Library => CorePage::Library,
            Page::Folders => CorePage::Folders,
            Page::Search => CorePage::Search,
            Page::Discover => CorePage::Discover,
            Page::Statistics => CorePage::Statistics,
            Page::Smart => CorePage::Smart,
            Page::Duplicates => CorePage::Duplicates,
            Page::Downloads => CorePage::Downloads,
            Page::Settings => CorePage::Settings,
        }
    }

    fn from_core(page: CorePage) -> Self {
        match page {
            CorePage::NowPlaying | CorePage::Playlists => Page::NowPlaying,
            CorePage::Library => Page::Library,
            CorePage::Folders => Page::Folders,
            CorePage::Search => Page::Search,
            CorePage::Discover => Page::Discover,
            CorePage::Statistics => Page::Statistics,
            CorePage::Smart => Page::Smart,
            CorePage::Duplicates => Page::Duplicates,
            CorePage::Downloads => Page::Downloads,
            CorePage::Settings => Page::Settings,
        }
    }
}

/// Deferred settings-file action. The shell opens the dialog / confirmation
/// after the click handler returns, then calls the matching core method.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsAction {
    Export,
    Import,
    Reset,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsTab {
    Library,
    Playback,
    Lyrics,
    Large,
    Desktop,
    Mini,
    Window,
    Appearance,
    Performance,
    System,
    Network,
    Storage,
    Shortcuts,
}

impl SettingsTab {
    pub const ALL: [SettingsTab; 13] = [
        SettingsTab::Library,
        SettingsTab::Playback,
        SettingsTab::Lyrics,
        SettingsTab::Large,
        SettingsTab::Desktop,
        SettingsTab::Mini,
        SettingsTab::Window,
        SettingsTab::Appearance,
        SettingsTab::Performance,
        SettingsTab::System,
        SettingsTab::Network,
        SettingsTab::Storage,
        SettingsTab::Shortcuts,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SettingsTab::Library => "曲库",
            SettingsTab::Playback => "播放",
            SettingsTab::Lyrics => "歌词",
            SettingsTab::Large => "独立歌词",
            SettingsTab::Desktop => "桌面歌词",
            SettingsTab::Mini => "迷你",
            SettingsTab::Window => "窗口",
            SettingsTab::Appearance => "外观",
            SettingsTab::Performance => "性能",
            SettingsTab::System => "系统",
            SettingsTab::Network => "网络",
            SettingsTab::Storage => "存储",
            SettingsTab::Shortcuts => "快捷键",
        }
    }

    pub fn fields(self) -> &'static [SettingField] {
        match self {
            SettingsTab::Library => &[],
            SettingsTab::Playback => &PLAYBACK_FIELDS,
            SettingsTab::Lyrics => &LYRICS_FIELDS,
            SettingsTab::Large => &LARGE_FIELDS,
            SettingsTab::Desktop => &DESKTOP_FIELDS,
            SettingsTab::Mini => &MINI_FIELDS,
            SettingsTab::Window => &WINDOW_FIELDS,
            SettingsTab::Appearance => &APPEARANCE_FIELDS,
            SettingsTab::Performance => &PERFORMANCE_FIELDS,
            SettingsTab::System => &SYSTEM_FIELDS,
            SettingsTab::Network => &NETWORK_FIELDS,
            SettingsTab::Storage => &STORAGE_FIELDS,
            SettingsTab::Shortcuts => &SHORTCUTS_FIELDS,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingField {
    Gapless,
    AutoAdvance,
    Crossfade,
    ReplayGain,
    ReplayGainPreamp,
    EqEnabled,
    EqPreset,
    /// One of the ten equalizer bands (`index` into `playback.eq_bands`).
    EqBand(u8),
    ReplayGainScan,
    SleepTimer,
    SleepTimerCustom,
    ABStart,
    ABEnd,
    ABClear,
    OutputDevice,
    LyricsKaraoke,
    LyricsWordsOnly,
    LyricsTranslation,
    LyricsFontSize,
    LyricsLineSpacing,
    LyricsAlign,
    LyricsFontFamily,
    LyricsFontWeight,
    LyricsItalic,
    LyricsEncoding,
    LyricsOffset,
    LyricsLanguage,
    LyricsPrefetch,
    LyricsShowSource,
    LyricsPanelColor,
    LyricsPanelBackground,
    LyricsPanelColor2,
    LyricsPanelBorderColor,
    LyricsPanelImage,
    LyricsPanelReset,
    LyricsOutlineWidth,
    LyricsOutlineColor,
    LyricsShadowOffset,
    LyricsShadowColor,
    LyricsTransOutlineWidth,
    LyricsTransOutlineColor,
    LyricsTransShadowOffset,
    LyricsTransShadowColor,
    LargeLyricsFontSize,
    LargeFontFamily,
    LargeFontWeight,
    LargeItalic,
    LargeOutlineWidth,
    LargeOutlineColor,
    LargeShadowOffset,
    LargeShadowColor,
    LargeTransOutlineWidth,
    LargeTransOutlineColor,
    LargeTransShadowOffset,
    LargeTransShadowColor,
    LyricsHighlightColor,
    LyricsCurrentColor,
    LyricsNormalColor,
    LyricsTranslationColor,
    LargeLineSpacing,
    LargeAlign,
    LargeTranslation,
    LargeKaraoke,
    LargeWordsOnly,
    LargeShowCover,
    LargeShowTitle,
    LargeShowFooter,
    LargeHighlightColor,
    LargeCurrentColor,
    LargeNormalColor,
    LargeTranslationColor,
    DesktopCurrentColor,
    DesktopNextColor,
    DesktopPrevColor,
    DesktopFooterColor,
    DesktopTranslationColor,
    DesktopOutlineColor,
    DesktopShadowColor,
    DesktopBackgroundImage,
    DesktopClearBackgroundImage,
    DesktopBgColor,
    DesktopBgColor2,
    DesktopFontFamily,
    DesktopTransFontFamily,
    DesktopFooterFontFamily,
    DesktopTransBold,
    DesktopFooterBold,
    DesktopOutlineWidth,
    DesktopShadowOffset,
    DesktopItalic,
    DesktopTransOutlineWidth,
    DesktopTransShadowOffset,
    DesktopFooterOutlineWidth,
    DesktopFooterShadowOffset,
    DesktopTransOutlineColor,
    DesktopTransShadowColor,
    DesktopFooterOutlineColor,
    DesktopFooterShadowColor,
    DesktopBlurRadius,
    DesktopTranslationOutline,
    DesktopTranslationShadow,
    DesktopFooterOutline,
    DesktopFooterShadow,
    DesktopHideWhenFullscreen,
    DesktopShow,
    DesktopLock,
    DesktopClickThrough,
    DesktopAlwaysOnTop,
    DesktopLines,
    DesktopTranslation,
    DesktopFooter,
    DesktopFontSize,
    DesktopNextFontSize,
    DesktopPrevFontSize,
    DesktopFooterFontSize,
    DesktopFrameRate,
    DesktopBold,
    DesktopAlign,
    DesktopBackground,
    DesktopOpacity,
    DesktopRadius,
    DesktopPadding,
    DesktopLineSpacing,
    DesktopShadow,
    DesktopOutline,
    DesktopBorder,
    DesktopWindowShadow,
    DesktopAutoSize,
    DesktopWidth,
    DesktopHeight,
    DesktopHideWhenPaused,
    DesktopCaptureProtection,
    DesktopKaraoke,
    DesktopWordsOnly,
    DesktopResetPosition,
    MiniShowLyrics,
    MiniKaraoke,
    MiniWordsOnly,
    MiniLines,
    MiniBold,
    MiniTranslation,
    MiniFontSize,
    MiniFrameRate,
    MiniAutoHeight,
    MiniHeight,
    MiniAlwaysOnTop,
    MiniOpacity,
    MiniTaskbar,
    MiniWidth,
    MiniPanelColor,
    MiniTextColor,
    MiniAccentColor,
    ThemeMode,
    Accent,
    ThemeFontFamily,
    ThemeFontSize,
    Density,
    ReduceMotion,
    FrameRate,
    /// Background style and audio visualizer (appearance).
    BackgroundMode,
    BackgroundImage,
    ClearBackgroundImage,
    VisualizerEnabled,
    VisualizerMode,
    /// Performance / memory trade-offs.
    MetadataMode,
    LoadCovers,
    LazyAudio,
    ReleaseAudioOutput,
    DeferPausedRestore,
    IdleFrameRate,
    PauseAnimationsUnfocused,
    ThumbnailCache,
    D2DHardware,
    TrimWorkingSet,
    TrimIdleSecs,
    OptimizeIndex,
    CloseBehavior,
    StartMinimized,
    RememberPosition,
    CaptureProtection,
    LyricsCaptureProtection,
    /// Desktop-lyrics overlay protection, surfaced in the Window tab next to
    /// the other two capture-protection switches.
    DesktopLyricsCaptureProtection,
    AllowMultipleInstances,
    StartWithWindows,
    FileAssociations,
    PlaylistAssociations,
    MediaControls,
    Notifications,
    JumpList,
    /// Global hotkeys.
    ShortcutsEnabled,
    GamepadEnabled,
    Shortcut(ShortcutSlot),
    ResetShortcuts,
    ExportSettings,
    ImportSettings,
    ResetSettings,
    /// Primary online source address / credentials.
    SourceSelect,
    SourceEnabled,
    SourceName,
    SourceBaseUrl,
    SourceQuality,
    SourceHeaders,
    SourceEndpoints,
    SourceAdd,
    SourceRemove,
    /// Network settings.
    NetworkOffline,
    ApiMinInterval,
    AlbumPageSize,
    NetworkTimeout,
    NetworkRetries,
    ProxyMode,
    ProxyUrl,
    CacheLimitMb,
    StreamCacheDir,
    ClearCache,
    OnlineTest,
    /// Download settings and manager.
    DownloadDir,
    DownloadConcurrency,
    DownloadConflict,
    DownloadQuality,
    DownloadTemplate,
    DownloadEmbedTags,
    DownloadEmbedCover,
    DownloadSaveLyrics,
    DownloadAddToLibrary,
    ClearFinishedDownloads,
}

const PLAYBACK_FIELDS: [SettingField; 24] = [
    SettingField::Gapless,
    SettingField::AutoAdvance,
    SettingField::Crossfade,
    SettingField::ReplayGain,
    SettingField::ReplayGainPreamp,
    SettingField::ReplayGainScan,
    SettingField::EqEnabled,
    SettingField::EqPreset,
    SettingField::EqBand(0),
    SettingField::EqBand(1),
    SettingField::EqBand(2),
    SettingField::EqBand(3),
    SettingField::EqBand(4),
    SettingField::EqBand(5),
    SettingField::EqBand(6),
    SettingField::EqBand(7),
    SettingField::EqBand(8),
    SettingField::EqBand(9),
    SettingField::SleepTimer,
    SettingField::SleepTimerCustom,
    SettingField::ABStart,
    SettingField::ABEnd,
    SettingField::ABClear,
    SettingField::OutputDevice,
];

const EQ_BAND_LABELS: [&str; 10] = [
    "32 Hz", "64 Hz", "125 Hz", "250 Hz", "500 Hz", "1 kHz", "2 kHz", "4 kHz", "8 kHz", "16 kHz",
];

const WINDOW_FIELDS: [SettingField; 6] = [
    SettingField::CloseBehavior,
    SettingField::StartMinimized,
    SettingField::RememberPosition,
    SettingField::CaptureProtection,
    SettingField::LyricsCaptureProtection,
    SettingField::DesktopLyricsCaptureProtection,
];

const SYSTEM_FIELDS: [SettingField; 10] = [
    SettingField::AllowMultipleInstances,
    SettingField::StartWithWindows,
    SettingField::FileAssociations,
    SettingField::PlaylistAssociations,
    SettingField::MediaControls,
    SettingField::Notifications,
    SettingField::JumpList,
    SettingField::ExportSettings,
    SettingField::ImportSettings,
    SettingField::ResetSettings,
];

const SHORTCUTS_FIELDS: [SettingField; 10] = [
    SettingField::ShortcutsEnabled,
    SettingField::GamepadEnabled,
    SettingField::Shortcut(ShortcutSlot::PlayPause),
    SettingField::Shortcut(ShortcutSlot::Next),
    SettingField::Shortcut(ShortcutSlot::Prev),
    SettingField::Shortcut(ShortcutSlot::VolumeUp),
    SettingField::Shortcut(ShortcutSlot::VolumeDown),
    SettingField::Shortcut(ShortcutSlot::ToggleDesktopLyrics),
    SettingField::Shortcut(ShortcutSlot::ShowHide),
    SettingField::ResetShortcuts,
];

const NETWORK_FIELDS: [SettingField; 20] = [
    SettingField::SourceSelect,
    SettingField::SourceEnabled,
    SettingField::SourceName,
    SettingField::SourceBaseUrl,
    SettingField::SourceQuality,
    SettingField::SourceHeaders,
    SettingField::SourceEndpoints,
    SettingField::SourceAdd,
    SettingField::SourceRemove,
    SettingField::OnlineTest,
    SettingField::NetworkOffline,
    SettingField::ApiMinInterval,
    SettingField::AlbumPageSize,
    SettingField::NetworkTimeout,
    SettingField::NetworkRetries,
    SettingField::ProxyMode,
    SettingField::ProxyUrl,
    SettingField::CacheLimitMb,
    SettingField::StreamCacheDir,
    SettingField::ClearCache,
];

const STORAGE_FIELDS: [SettingField; 10] = [
    SettingField::DownloadDir,
    SettingField::DownloadConcurrency,
    SettingField::DownloadConflict,
    SettingField::DownloadQuality,
    SettingField::DownloadTemplate,
    SettingField::DownloadEmbedTags,
    SettingField::DownloadEmbedCover,
    SettingField::DownloadSaveLyrics,
    SettingField::DownloadAddToLibrary,
    SettingField::ClearFinishedDownloads,
];

const LYRICS_FIELDS: [SettingField; 33] = [
    SettingField::LyricsKaraoke,
    SettingField::LyricsWordsOnly,
    SettingField::LyricsTranslation,
    SettingField::LyricsFontSize,
    SettingField::LyricsFontFamily,
    SettingField::LyricsFontWeight,
    SettingField::LyricsItalic,
    SettingField::LyricsLineSpacing,
    SettingField::LyricsAlign,
    SettingField::LyricsEncoding,
    SettingField::LyricsOffset,
    SettingField::LyricsLanguage,
    SettingField::LyricsPrefetch,
    SettingField::LyricsShowSource,
    SettingField::LyricsPanelColor,
    SettingField::LyricsPanelBackground,
    SettingField::LyricsPanelColor2,
    SettingField::LyricsPanelBorderColor,
    SettingField::LyricsPanelImage,
    SettingField::LyricsPanelReset,
    SettingField::LyricsOutlineWidth,
    SettingField::LyricsOutlineColor,
    SettingField::LyricsShadowOffset,
    SettingField::LyricsShadowColor,
    SettingField::LyricsTransOutlineWidth,
    SettingField::LyricsTransOutlineColor,
    SettingField::LyricsTransShadowOffset,
    SettingField::LyricsTransShadowColor,
    SettingField::LyricsHighlightColor,
    SettingField::LyricsCurrentColor,
    SettingField::LyricsNormalColor,
    SettingField::LyricsTranslationColor,
    SettingField::LargeLyricsFontSize,
];

const LARGE_FIELDS: [SettingField; 24] = [
    SettingField::LargeLyricsFontSize,
    SettingField::LargeFontFamily,
    SettingField::LargeFontWeight,
    SettingField::LargeItalic,
    SettingField::LargeOutlineWidth,
    SettingField::LargeOutlineColor,
    SettingField::LargeShadowOffset,
    SettingField::LargeShadowColor,
    SettingField::LargeTransOutlineWidth,
    SettingField::LargeTransOutlineColor,
    SettingField::LargeTransShadowOffset,
    SettingField::LargeTransShadowColor,
    SettingField::LargeLineSpacing,
    SettingField::LargeAlign,
    SettingField::LargeTranslation,
    SettingField::LargeKaraoke,
    SettingField::LargeWordsOnly,
    SettingField::LargeShowCover,
    SettingField::LargeShowTitle,
    SettingField::LargeShowFooter,
    SettingField::LargeHighlightColor,
    SettingField::LargeCurrentColor,
    SettingField::LargeNormalColor,
    SettingField::LargeTranslationColor,
];

const DESKTOP_FIELDS: [SettingField; 64] = [
    SettingField::DesktopShow,
    SettingField::DesktopLock,
    SettingField::DesktopClickThrough,
    SettingField::DesktopAlwaysOnTop,
    SettingField::DesktopLines,
    SettingField::DesktopTranslation,
    SettingField::DesktopFooter,
    SettingField::DesktopFontSize,
    SettingField::DesktopNextFontSize,
    SettingField::DesktopPrevFontSize,
    SettingField::DesktopFooterFontSize,
    SettingField::DesktopFrameRate,
    SettingField::DesktopBold,
    SettingField::DesktopAlign,
    SettingField::DesktopBackground,
    SettingField::DesktopOpacity,
    SettingField::DesktopRadius,
    SettingField::DesktopPadding,
    SettingField::DesktopLineSpacing,
    SettingField::DesktopShadow,
    SettingField::DesktopOutline,
    SettingField::DesktopBorder,
    SettingField::DesktopWindowShadow,
    SettingField::DesktopAutoSize,
    SettingField::DesktopWidth,
    SettingField::DesktopHeight,
    SettingField::DesktopHideWhenPaused,
    SettingField::DesktopCaptureProtection,
    SettingField::DesktopKaraoke,
    SettingField::DesktopWordsOnly,
    SettingField::DesktopResetPosition,
    SettingField::DesktopCurrentColor,
    SettingField::DesktopNextColor,
    SettingField::DesktopPrevColor,
    SettingField::DesktopFooterColor,
    SettingField::DesktopTranslationColor,
    SettingField::DesktopOutlineColor,
    SettingField::DesktopShadowColor,
    SettingField::DesktopBackgroundImage,
    SettingField::DesktopClearBackgroundImage,
    SettingField::DesktopBgColor,
    SettingField::DesktopBgColor2,
    SettingField::DesktopFontFamily,
    SettingField::DesktopTransFontFamily,
    SettingField::DesktopFooterFontFamily,
    SettingField::DesktopTransBold,
    SettingField::DesktopFooterBold,
    SettingField::DesktopOutlineWidth,
    SettingField::DesktopShadowOffset,
    SettingField::DesktopTranslationOutline,
    SettingField::DesktopTranslationShadow,
    SettingField::DesktopFooterOutline,
    SettingField::DesktopFooterShadow,
    SettingField::DesktopHideWhenFullscreen,
    SettingField::DesktopItalic,
    SettingField::DesktopTransOutlineWidth,
    SettingField::DesktopTransShadowOffset,
    SettingField::DesktopFooterOutlineWidth,
    SettingField::DesktopFooterShadowOffset,
    SettingField::DesktopTransOutlineColor,
    SettingField::DesktopTransShadowColor,
    SettingField::DesktopFooterOutlineColor,
    SettingField::DesktopFooterShadowColor,
    SettingField::DesktopBlurRadius,
];

const MINI_FIELDS: [SettingField; 17] = [
    SettingField::MiniShowLyrics,
    SettingField::MiniKaraoke,
    SettingField::MiniWordsOnly,
    SettingField::MiniLines,
    SettingField::MiniBold,
    SettingField::MiniTranslation,
    SettingField::MiniFontSize,
    SettingField::MiniFrameRate,
    SettingField::MiniAutoHeight,
    SettingField::MiniHeight,
    SettingField::MiniAlwaysOnTop,
    SettingField::MiniOpacity,
    SettingField::MiniWidth,
    SettingField::MiniTaskbar,
    SettingField::MiniPanelColor,
    SettingField::MiniTextColor,
    SettingField::MiniAccentColor,
];

const APPEARANCE_FIELDS: [SettingField; 12] = [
    SettingField::ThemeMode,
    SettingField::Accent,
    SettingField::ThemeFontFamily,
    SettingField::ThemeFontSize,
    SettingField::Density,
    SettingField::ReduceMotion,
    SettingField::FrameRate,
    SettingField::BackgroundMode,
    SettingField::BackgroundImage,
    SettingField::ClearBackgroundImage,
    SettingField::VisualizerEnabled,
    SettingField::VisualizerMode,
];

const PERFORMANCE_FIELDS: [SettingField; 13] = [
    SettingField::MetadataMode,
    SettingField::LoadCovers,
    SettingField::LazyAudio,
    SettingField::ReleaseAudioOutput,
    SettingField::DeferPausedRestore,
    SettingField::IdleFrameRate,
    SettingField::PauseAnimationsUnfocused,
    SettingField::ThumbnailCache,
    SettingField::D2DHardware,
    SettingField::TrimWorkingSet,
    SettingField::TrimIdleSecs,
    SettingField::OptimizeIndex,
    SettingField::FrameRate,
];

const THEME_OPTIONS: [&str; 3] = ["暗色", "亮色", "跟随"];
const BACKGROUND_OPTIONS: [&str; 3] = ["纯色", "模糊封面", "自定义图片"];
const VISUALIZER_MODE_OPTIONS: [&str; 3] = ["条形", "波形", "环形"];
const METADATA_OPTIONS: [&str; 3] = ["关闭", "仅当前曲目", "全部曲目"];
const WEIGHT_OPTIONS: [&str; 3] = ["常规", "中等", "粗体"];
const WEIGHT_VALUES: [u16; 3] = [400, 600, 800];
const ENCODING_OPTIONS: [&str; 7] = ["自动", "UTF-8", "GBK", "GB18030", "UTF-16LE", "UTF-16BE", "Big5"];
const LANGUAGE_OPTIONS: [&str; 5] = ["自动", "中文", "英文", "日文", "韩文"];
const LANGUAGE_VALUES: [&str; 5] = ["auto", "zh", "en", "jp", "ko"];

fn weight_index(weight: u16) -> usize {
    WEIGHT_VALUES
        .iter()
        .position(|value| *value == weight)
        .unwrap_or(0)
}

fn lyrics_encoding_index(encoding: crate::config::LyricsEncoding) -> usize {
    use crate::config::LyricsEncoding as E;
    match encoding {
        E::Auto => 0,
        E::Utf8 => 1,
        E::Gbk => 2,
        E::Gb18030 => 3,
        E::Utf16Le => 4,
        E::Utf16Be => 5,
        E::Big5 => 6,
    }
}
const ALIGN_OPTIONS: [&str; 3] = ["左", "中", "右"];
const DENSITY_OPTIONS: [&str; 3] = ["紧凑", "舒适", "宽松"];
const MINI_LINE_OPTIONS: [&str; 2] = ["1 行", "2 行"];
const DESKTOP_LINE_OPTIONS: [&str; 3] = ["1 行", "2 行", "3 行"];
const DESKTOP_BG_OPTIONS: [&str; 5] = ["透明", "纯色", "渐变", "图片", "模糊"];
const PROXY_OPTIONS: [&str; 3] = ["系统", "直连", "手动"];
const CONFLICT_OPTIONS: [&str; 3] = ["重命名", "跳过", "覆盖"];
const DOWNLOAD_QUALITY_OPTIONS: [&str; 4] = ["跟随音源", "320 kbps", "FLAC", "128 kbps"];
const SOURCE_QUALITY_OPTIONS: [&str; 3] = ["128 kbps", "320 kbps", "无损 FLAC"];
const LYRICS_PANEL_BG_OPTIONS: [&str; 4] = ["跟随主题", "纯色", "渐变", "图片"];

fn desktop_background_index(mode: crate::config::DesktopBackgroundMode) -> i32 {
    use crate::config::DesktopBackgroundMode as Mode;
    match mode {
        Mode::Transparent => 0,
        Mode::Solid => 1,
        Mode::Gradient => 2,
        Mode::Image => 3,
        Mode::BlurCover => 4,
    }
}

/// Index of the closest accent preset, so a custom stored color still shows a
/// sensible selection in the segmented picker.
fn accent_index(color: [u8; 3]) -> usize {
    crate::theme::ACCENT_PRESETS
        .iter()
        .enumerate()
        .min_by_key(|(_, (_, preset))| {
            let dr = color[0] as i32 - preset[0] as i32;
            let dg = color[1] as i32 - preset[1] as i32;
            let db = color[2] as i32 - preset[2] as i32;
            dr * dr + dg * dg + db * db
        })
        .map(|(index, _)| index)
        .unwrap_or(0)
}

const REPLAYGAIN_OPTIONS: [&str; 3] = ["关闭", "曲目", "专辑"];
const EQ_OPTIONS: [&str; 8] = [
    "平坦", "摇滚", "流行", "爵士", "古典", "人声", "低音", "自定义",
];
const EQ_PRESETS: [EqPreset; 8] = [
    EqPreset::Flat,
    EqPreset::Rock,
    EqPreset::Pop,
    EqPreset::Jazz,
    EqPreset::Classical,
    EqPreset::Vocal,
    EqPreset::Bass,
    EqPreset::Custom,
];
const SLEEP_OPTIONS: [&str; 4] = ["关闭", "15 分钟", "30 分钟", "60 分钟"];
const CLOSE_OPTIONS: [&str; 3] = ["托盘", "退出", "询问"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingKind {
    Toggle,
    Step,
    Choice,
    Color,
    /// Button that opens a dialog (file/font picker) or performs an action.
    Action,
    /// Inline native text editor over the value area.
    Text,
}

#[derive(Clone)]
pub struct SettingView {
    pub label: &'static str,
    pub kind: SettingKind,
    pub value: String,
    pub active: bool,
    pub options: &'static [&'static str],
    pub selected: Option<usize>,
}

/// Transient status message drawn above the player bar.
pub struct Toast {
    pub text: String,
    pub error: bool,
    pub expires: Instant,
}

/// How long a toast stays on screen. Long enough to read a file error,
/// short enough not to pile up during rapid track changes.
const TOAST_DURATION: std::time::Duration = std::time::Duration::from_millis(3600);

pub struct App {    pub dpi: f32,
    pub layout: Layout,
    pub page: Page,
    pub scroll: f32,
    pub dirty: Vec<Rect>,
    pub dragging_progress: bool,
    /// Position preview while dragging the progress bar; the engine is only
    /// seeked once on release, so a drag cannot flood the audio thread.
    drag_preview: Option<f32>,
    dragging_scrollbar: bool,
    /// Dragging the settings-page scrollbar (generic rows).
    dragging_settings_scrollbar: bool,
    dragging_volume: bool,
    /// Paint statistics for `--bench`; not used by the UI itself.
    pub window_visible: bool,
    /// Visible parts of the client area in DIPs. Occluders are subtracted so
    /// animation only repaints exposed pixels.
    pub visible: Vec<Rect>,
    /// Set when the window becomes visible again; the KTV overlay must repaint
    /// its whole band because it stopped publishing while covered.
    pub visibility_changed: bool,
    visibility_poll: u8,
    pub frames: u64,
    pub ticks: u64,
    pub paint_nanos: u128,
    last_tick: Option<Instant>,
    /// Position at the previous tick. The engine clock advances on the audio
    /// thread, so sampling "before/after core.tick()" would compare two values
    /// microseconds apart and never move the progress edge.
    last_position: Option<f32>,
    /// First visible folder row on the settings page.
    settings_folders_scroll: usize,
    /// Settings category shown on the settings page.
    pub settings_tab: SettingsTab,
    /// Scroll offset of the generic settings rows (tabs with many rows).
    pub settings_scroll: f32,
    /// Main-window background picture picker requested by the settings page.
    background_image_pick_requested: bool,
    /// Settings row currently waiting for a key combination.
    pub shortcut_capture: Option<ShortcutSlot>,
    /// Keyboard focus ring target (`Tab` navigation).
    keyboard_focus: Option<Target>,
    /// Scroll offset of the sidebar navigation when the window is short.
    sidebar_scroll: f32,
    dragging_sidebar_scrollbar: bool,
    /// Album/artist detail uses a cover grid instead of list rows.
    online_wall: bool,
    /// Queue filter text and the display-row -> queue-index mapping it produces.
    pub queue_filter: String,
    queue_view: Vec<usize>,
    queue_view_rev: u64,
    /// Zero-based page of the library list. The page holds exactly the rows
    /// that fit on screen; the library no longer streams additional rows.
    library_page: usize,
    /// Whether the core was told the current page window size.
    library_page_synced: bool,
    /// Set by a click on "添加目录"; the shell opens the native dialog after
    /// the click handler returns (a modal dialog inside the handler would
    /// re-enter the message loop while `State` is mutably borrowed).
    pick_folder_requested: bool,
    /// Settings → 网络/存储 folder pickers.
    stream_cache_pick_requested: bool,
    download_dir_pick_requested: bool,
    lyrics_panel_image_pick_requested: bool,
    /// Folder page multi-selection (Ctrl/Shift click).
    folder_selected: std::collections::HashSet<usize>,
    folder_anchor: Option<usize>,
    /// Folder row press used for click-vs-drag; dragging onto the "播放队列"
    /// nav item appends the selected files to the queue.
    folder_press: Option<(usize, f32, f32)>,
    folder_drag: bool,
    folder_drop_over_queue: bool,
    /// Queue drag-reorder state: pressed row and current drop row.
    queue_drag_from: Option<usize>,
    queue_drag_target: Option<usize>,
    queue_drag_active: bool,
    /// External (OLE) drag preview: file count, insertion row, queue hover.
    external_drag_active: bool,
    external_drag_count: usize,
    external_drag_index: Option<usize>,
    external_drag_over_queue: bool,
    /// "打开文件" on the Now Playing card.
    pick_files_requested: bool,
    /// "选择歌词" in the lyrics panel header.
    pick_lyrics_requested: bool,
    /// Queue page header shortcuts (the shell opens dialogs / clears).
    queue_import_requested: bool,
    queue_export_requested: bool,
    queue_clear_requested: bool,
    /// Statistics page "重置统计" requested; the shell confirms it.
    stats_reset_requested: bool,
    /// Settings import/export/reset; the shell opens the file dialog.
    settings_action: Option<SettingsAction>,
    /// UI scale changed: rebuild effective DPI, painter and native controls.
    /// "独立窗口" in the lyrics panel header; the shell owns the window.
    lyrics_window_requested: bool,
    /// "编辑" in the lyrics panel header; the shell opens the editor window.
    lyrics_editor_requested: bool,
    /// Color row waiting for the native picker; the shell consumes it after
    /// the click handler returns (a modal dialog must not re-enter the borrow).
    color_request: Option<SettingField>,
    /// Step row whose value is being typed into the native number EDIT.
    pub number_edit_row: Option<usize>,
    /// Row currently edited by the inline text EDIT.
    pub text_edit_row: Option<usize>,
    /// Font-size row clicked; the shell opens the EDIT after the borrow ends.
    number_edit_request: Option<usize>,
    /// Text row clicked; the shell shows the inline EDIT after the borrow.
    text_edit_request: Option<usize>,
    /// Desktop-lyrics background image picker requested.
    desktop_bg_pick_requested: bool,
    /// Desktop-lyrics font family picker requested.
    desktop_font_pick_requested: bool,
    desktop_trans_font_pick_requested: bool,
    desktop_footer_font_pick_requested: bool,
    lyrics_font_pick_requested: bool,
    large_font_pick_requested: bool,
    theme_font_pick_requested: bool,
    /// Whether the standalone lyrics window is open (set by the shell).
    pub lyrics_window_open: bool,
    /// Hide the song card and let the lyric panel fill the page.
    pub big_lyrics: bool,
    /// Playback settings flyout (speed / equalizer / pitch).
    pub player_panel_open: bool,
    /// Set when the flyout closes so the shell repaints the lyric child under it.
    player_panel_closed: bool,
    pub ui: Interaction,
    /// Whether the native sort LISTBOX is shown (owned by the shell).
    pub sort_menu_open: bool,
    /// Current toast (one at a time; newer messages replace older ones).
    toast: Option<Toast>,
    /// Last `Snapshot::notice_rev` drained from the core.
    last_notice_rev: u64,
    /// Smoothed visualizer bar heights (0..=1).
    pub visual_bars: Vec<f32>,
    /// Raw mono samples used by the waveform mode.
    pub visual_samples: Vec<f32>,
    /// Last time the flow background was invalidated.
    /// Last time the visualizer sampled audio (its own fps cap).
    last_visual_tick: Option<Instant>,
    pub core: AppCore,
}

pub const LIBRARY_SORTS: [LibrarySort; 8] = [
    LibrarySort::Title,
    LibrarySort::Artist,
    LibrarySort::Album,
    LibrarySort::Added,
    LibrarySort::Duration,
    LibrarySort::PlayCount,
    LibrarySort::LastPlayed,
    LibrarySort::Path,
];

pub const DEDUPE_ORDER: [DedupeMode; 3] = [
    DedupeMode::Metadata,
    DedupeMode::Path,
    DedupeMode::Fingerprint,
];

pub fn sort_index(sort: LibrarySort) -> usize {
    LIBRARY_SORTS
        .iter()
        .position(|candidate| *candidate == sort)
        .unwrap_or(0)
}

pub fn dedupe_index(mode: DedupeMode) -> usize {
    DEDUPE_ORDER
        .iter()
        .position(|candidate| *candidate == mode)
        .unwrap_or(0)
}

pub fn dedupe_label(mode: DedupeMode) -> &'static str {
    match mode {
        DedupeMode::Metadata => "元数据",
        DedupeMode::Path => "文件路径",
        DedupeMode::Fingerprint => "音频指纹",
    }
}

impl App {
    pub fn new(
        paths: Paths,
        wake: Option<Arc<dyn Fn() + Send + Sync>>,
        load_library: bool,
    ) -> Self {
        let core = AppCore::new(paths, wake, load_library);
        let page = Page::from_core(core.session.page);
        let mut app = Self {
            dpi: 96.0,
            layout: layout::compute(1024.0, 680.0),
            page,
            scroll: 0.0,
            dirty: Vec::new(),
            dragging_progress: false,
            drag_preview: None,
            dragging_scrollbar: false,
            dragging_settings_scrollbar: false,
            dragging_volume: false,
            window_visible: true,
            visible: Vec::new(),
            visibility_changed: false,
            visibility_poll: 20,
            frames: 0,
            ticks: 0,
            paint_nanos: 0,
            last_tick: None,
            last_position: None,
            settings_folders_scroll: 0,
            settings_tab: SettingsTab::Library,
            settings_scroll: 0.0,
            background_image_pick_requested: false,
            shortcut_capture: None,
            keyboard_focus: None,
            sidebar_scroll: 0.0,
            dragging_sidebar_scrollbar: false,
            online_wall: false,
            queue_filter: String::new(),
            queue_view: Vec::new(),
            queue_view_rev: u64::MAX,
            library_page: 0,
            library_page_synced: false,
            folder_selected: std::collections::HashSet::new(),
            folder_anchor: None,
            folder_press: None,
            folder_drag: false,
            folder_drop_over_queue: false,
            queue_drag_from: None,
            queue_drag_target: None,
            queue_drag_active: false,
            external_drag_active: false,
            external_drag_count: 0,
            external_drag_index: None,
            external_drag_over_queue: false,
            pick_folder_requested: false,
            stream_cache_pick_requested: false,
            download_dir_pick_requested: false,
            lyrics_panel_image_pick_requested: false,
            pick_files_requested: false,
            pick_lyrics_requested: false,
            queue_import_requested: false,
            queue_export_requested: false,
            queue_clear_requested: false,
            stats_reset_requested: false,
            settings_action: None,
            lyrics_window_requested: false,
            lyrics_window_open: false,
            lyrics_editor_requested: false,
            color_request: None,
            number_edit_row: None,
            text_edit_row: None,
            number_edit_request: None,
            text_edit_request: None,
            desktop_bg_pick_requested: false,
            desktop_font_pick_requested: false,
            desktop_trans_font_pick_requested: false,
            desktop_footer_font_pick_requested: false,
            lyrics_font_pick_requested: false,
            large_font_pick_requested: false,
            theme_font_pick_requested: false,
            big_lyrics: false,
            player_panel_open: false,
            player_panel_closed: false,
            ui: Interaction::default(),
            sort_menu_open: false,
            toast: None,
            last_notice_rev: 0,
            visual_bars: Vec::new(),
            visual_samples: Vec::new(),
            last_visual_tick: None,
            core,
        };
        app.rebuild_queue_view();
        if page == Page::Statistics {
            app.core.request_statistics();
        }
        if page == Page::Smart {
            let kind = app.core.snapshot().library_smart_kind;
            app.core.request_smart(kind);
        }
        if page == Page::Duplicates {
            let mode = app.core.snapshot().library_duplicate_mode;
            app.core.request_duplicates(mode);
        }
        app
    }

    pub fn set_metrics(&mut self, width_px: u32, height_px: u32, dpi: f32) {
        if dpi > 0.0 {
            self.dpi = dpi;
        }
        // Old occlusion rectangles describe the previous client size; using
        // them after a resize leaves stale pixels (e.g. old progress knobs)
        // outside the invalidated pieces. Force a full first paint.
        self.reset_visibility();
        let old_page_size = self.library_page_size();
        let old_page = self.library_page;
        let dip_width = width_px as f32 * 96.0 / self.dpi;
        let dip_height = height_px as f32 * 96.0 / self.dpi;
        self.layout = layout::compute_with(
            dip_width,
            dip_height,
            self.big_lyrics,
            self.visualizer_visible(),
        );
        self.scroll = self.scroll.clamp(0.0, self.max_scroll());
        self.settings_scroll = self.settings_scroll.clamp(0.0, self.settings_max_scroll());
        self.sidebar_scroll = self.sidebar_scroll.clamp(0.0, self.sidebar_max_scroll());
        let new_page_size = self.library_page_size();
        if new_page_size != old_page_size {
            // Keep roughly the same first row across resizes.
            self.library_page = old_page.saturating_mul(old_page_size) / new_page_size;
        }
        let page_size_changed = new_page_size != old_page_size;
        if self.page == Page::Library && (page_size_changed || !self.library_page_synced) {
            self.request_library_page();
        }
        if page_size_changed && self.page == Page::Search {
            let query = self.core.snapshot().online.query.clone();
            if !query.trim().is_empty() {
                let limit = self.online_page_size() as u32;
                self.core.handle(Command::OnlineSearch {
                    query,
                    offset: 0,
                    limit,
                });
            }
        }
        self.request_visible_covers();
    }

    fn relayout(&mut self) {
        let (width, height) = self.layout.size;
        self.layout = layout::compute_with(
            width,
            height,
            self.big_lyrics,
            self.visualizer_visible(),
        );
        self.settings_scroll = self.settings_scroll.clamp(0.0, self.settings_max_scroll());
    }

    /// Toggle the big-lyrics layout (card hidden, panel fills the page).
    pub fn set_big_lyrics(&mut self, hwnd: HWND, on: bool) {
        if self.big_lyrics == on {
            return;
        }
        self.big_lyrics = on;
        self.relayout();
        self.ui.clear();
        self.invalidate_all(hwnd);
    }

    /// Install the synthetic 10k-track fixture used by `--bench`.
    pub fn enable_demo(&mut self) {
        self.core
            .load_demo(crate::demo::core_tracks(10_000), crate::demo::core_lyrics());
        self.page = Page::NowPlaying;
    }

    /// Drop the cached exposed rectangles after a size/DPI/topology change so
    /// the next paint covers the whole client instead of a stale subset.
    pub fn reset_visibility(&mut self) {
        self.visible.clear();
        self.window_visible = true;
        self.visibility_poll = 20;
    }

    /// The window was hidden (tray / minimize) and is visible again. The
    /// occlusion poll only runs while animation does, so a paused window must
    /// explicitly clear the hidden flag and repaint every exposed pixel.
    pub fn restore_visibility(&mut self, hwnd: HWND) {
        self.visible.clear();
        self.window_visible = true;
        self.visibility_changed = true;
        self.visibility_poll = 20;
        self.last_tick = Some(Instant::now());
        let full = Rect::new(0.0, 0.0, self.layout.size.0, self.layout.size.1);
        self.invalidate_piece(hwnd, full);
    }

    pub fn current_track(&self) -> Option<&Track> {
        self.core.snapshot().current
    }

    pub fn current_cover(&self) -> Option<(&crate::core::util::image_util::RgbaImage, u64)> {
        self.core.current_cover()
    }

    pub fn desktop_lyrics_open(&self) -> bool {
        let settings = &self.core.settings.desktop_lyrics;
        settings.enabled && settings.visible_at_start
    }

    pub fn mini_player_open(&self) -> bool {
        self.core.settings.mini.visible
    }

    pub fn visualizer_visible(&self) -> bool {
        let settings = &self.core.settings;
        self.page == Page::NowPlaying
            && settings.features.visualizer
            && settings.visualizer.enabled
    }

    pub fn background_mode(&self) -> crate::core::config::BackgroundMode {
        self.core.settings.theme.background
    }

    /// True when the background is built from the current cover.
    pub fn background_uses_cover(&self) -> bool {
        matches!(
            self.core.settings.theme.background,
            crate::core::config::BackgroundMode::StaticBlur
                | crate::core::config::BackgroundMode::GpuBlur
        )
    }

    pub fn background_uses_image(&self) -> bool {
        self.core.settings.theme.background == crate::core::config::BackgroundMode::Image
    }

    pub fn background_image(&self) -> Option<(&crate::core::util::image_util::RgbaImage, u64)> {
        let snapshot = self.core.snapshot();
        snapshot.background_image.map(|image| (image, snapshot.background_image_rev))
    }

    /// Thumbnail for one visible queue/library row.
    pub fn row_cover(&self, index: usize) -> Option<&crate::core::util::image_util::RgbaImage> {
        let snapshot = self.core.snapshot();
        if self.page == Page::Search {
            let song = snapshot.online.rows().get(index)?;
            let url = song.pic.as_deref()?;
            return self.core.row_cover_by_key(&format!("url:{url}"));
        }
        if self.page == Page::Discover {
            let song = snapshot.discover.songs.get(index)?;
            let url = song.pic.as_deref()?;
            return self.core.row_cover_by_key(&format!("url:{url}"));
        }
        if matches!(self.page, Page::Library | Page::Smart | Page::Duplicates) {
            let entry = match self.page {
                Page::Smart => snapshot.library_smart.get(index)?,
                Page::Duplicates => snapshot.library_duplicate_rows.get(index)?,
                _ => snapshot.library.get(index)?,
            };
            if entry.path.as_os_str().is_empty() {
                return None;
            }
            return self.core.row_cover(&entry.path);
        }
        if self.page == Page::Folders {
            let entry = snapshot.browser.entries.get(index)?;
            // Only audio files have covers; directories/playlists keep icons.
            if entry.is_dir || entry.is_playlist {
                return None;
            }
            return self.core.row_cover(&entry.path);
        }
        let track = snapshot.queue.get(self.queue_index(index)?)?;
        let path = track.source.path()?;
        self.core.row_cover(path)
    }

    /// Ask the core to decode thumbnails for the rows currently on screen.
    /// Only visible rows are requested; the bounded cache keeps memory flat.
    pub fn request_visible_covers(&mut self) {
        if self.page == Page::Search {
            let (first, last) = if self.online_wall_active() {
                self.wall_visible_range()
            } else {
                self.visible_rows()
            };
            let urls: Vec<String> = {
                let snapshot = self.core.snapshot();
                (first..last)
                    .filter_map(|index| snapshot.online.rows().get(index))
                    .filter_map(|song| song.pic.clone())
                    .collect()
            };
            for url in urls {
                self.core.ensure_row_cover_url(&url);
            }
            return;
        }
        if self.page == Page::Discover {
            let (first, last) = self.visible_rows();
            let urls: Vec<String> = {
                let snapshot = self.core.snapshot();
                (first..last)
                    .filter_map(|index| snapshot.discover.songs.get(index))
                    .filter_map(|song| song.pic.clone())
                    .collect()
            };
            for url in urls {
                self.core.ensure_row_cover_url(&url);
            }
            return;
        }
        if matches!(self.page, Page::Library | Page::Smart | Page::Duplicates) {
            // Only the rows actually on screen. The smart list can hold
            // hundreds of entries; queuing covers for all of them would
            // decode the whole list in the background for no visible gain.
            let (first, last) = self.visible_rows();
            let snapshot = self.core.snapshot();
            let entries: &[_] = match self.page {
                Page::Smart => snapshot.library_smart,
                Page::Duplicates => snapshot.library_duplicate_rows,
                _ => snapshot.library,
            };
            let paths: Vec<std::path::PathBuf> = entries
                .get(first.min(entries.len())..last.min(entries.len()))
                .unwrap_or_default()
                .iter()
                .filter(|entry| !entry.path.as_os_str().is_empty())
                .map(|entry| entry.path.clone())
                .collect();
            for path in paths {
                self.core.ensure_row_cover(&path);
            }
        } else if self.page == Page::Queue {
            let (first, last) = self.visible_rows();
            let paths: Vec<std::path::PathBuf> = {
                let snapshot = self.core.snapshot();
                (first..last)
                    // Display row -> real queue index: under shuffle the
                    // visible order differs from `items`, and requesting the
                    // display index decoded the wrong tracks (blank covers).
                    .filter_map(|display| self.queue_index(display))
                    .filter_map(|index| snapshot.queue.get(index))
                    .filter_map(|track| track.source.path().map(|path| path.to_path_buf()))
                    .collect()
            };
            for path in paths {
                self.core.ensure_row_cover(&path);
            }
        } else if self.page == Page::Folders {
            let (first, last) = self.visible_rows();
            let paths: Vec<std::path::PathBuf> = {
                let snapshot = self.core.snapshot();
                (first..last)
                    .filter_map(|index| snapshot.browser.entries.get(index))
                    .filter(|entry| !entry.is_dir && !entry.is_playlist)
                    .map(|entry| entry.path.clone())
                    .collect()
            };
            for path in paths {
                self.core.ensure_row_cover(&path);
            }
        }
    }

    pub fn queue_len(&self) -> usize {
        self.core.snapshot().queue.len()
    }

    /// Rows in the list for the current page: filtered queue rows for most
    /// pages, the SQLite index for the library page, directory entries for
    /// the folder page.
    pub fn row_count(&self) -> usize {
        let snapshot = self.core.snapshot();
        match self.page {
            Page::Library => snapshot.library.len(),
            Page::Smart => snapshot.library_smart.len(),
            Page::Duplicates => snapshot.library_duplicate_rows.len(),
            Page::Folders => snapshot.browser.entries.len(),
            Page::Search => snapshot.online.rows().len(),
            Page::Discover => snapshot.discover.songs.len(),
            Page::Downloads => snapshot.downloads.len(),
            _ => self.queue_view.len(),
        }
    }

    /// Display row -> real queue index (filter may hide rows).
    pub fn queue_index(&self, display: usize) -> Option<usize> {
        self.queue_view.get(display).copied()
    }

    /// Reveal the currently playing row in the queue viewport (when it is not
    /// filtered out). The scroll offset is snapped to whole rows so the first
    /// visible row is never cut in half under the filter box.
    fn reveal_current_queue_row(&mut self) {
        let Some(current) = self.core.snapshot().current_index else {
            return;
        };
        let Some(display) = self.queue_view.iter().position(|index| *index == current) else {
            return;
        };
        if display >= self.row_count() {
            return;
        }
        let row_height = self.row_height().max(1.0);
        let area = self.list_area();
        let rows = (area.height() / row_height).floor().max(1.0) as usize;
        // Keep some context above the current track.
        let above = (rows / 2).max(1);
        let first = display.saturating_sub(above);
        self.scroll = (first as f32 * row_height).clamp(0.0, self.max_scroll());
        // Never leave a fractional offset at the clamped bottom.
        self.scroll = (self.scroll / row_height).round() * row_height;
        self.scroll = self.scroll.min(self.max_scroll());
    }

    /// Rebuild the filtered queue mapping when the core reports a change.
    pub fn refresh_queue_view(&mut self) {
        if self.core.snapshot().queue_rev != self.queue_view_rev {
            self.rebuild_queue_view();
        }
    }

    fn rebuild_queue_view(&mut self) {
        let snapshot = self.core.snapshot();
        // Under shuffle the playback order *is* the visible list; filtering is
        // applied on top of it so display row -> real index stays consistent.
        let order: Vec<usize> = if snapshot.shuffle && !snapshot.queue_order.is_empty() {
            snapshot
                .queue_order
                .iter()
                .copied()
                .filter(|index| *index < snapshot.queue.len())
                .collect()
        } else {
            (0..snapshot.queue.len()).collect()
        };
        self.queue_view = if self.queue_filter.trim().is_empty() {
            order
        } else {
            order
                .into_iter()
                .filter(|index| {
                    snapshot
                        .queue
                        .get(*index)
                        .is_some_and(|track| track.matches_query(&self.queue_filter))
                })
                .collect()
        };
        self.queue_view_rev = snapshot.queue_rev;
    }

    /// True when the queue page is showing the shuffled playback order, where
    /// drag reordering and move up/down do not match the visible rows.
    pub fn queue_shows_shuffle(&self) -> bool {
        let snapshot = self.core.snapshot();
        snapshot.shuffle && !snapshot.queue_order.is_empty()
    }

    pub fn queue_filter_text(&self) -> &str {
        &self.queue_filter
    }

    pub fn set_queue_filter(&mut self, hwnd: HWND, text: String) {
        if self.queue_filter == text {
            return;
        }
        self.queue_filter = text;
        self.rebuild_queue_view();
        self.scroll = 0.0;
        self.ui.set_hover(None);
        self.invalidate(hwnd, self.layout.list);
        self.invalidate(hwnd, self.scrollbar_rect());
        self.invalidate(hwnd, self.layout.header);
    }

    /// Client rect of the rows. The library reserves the bottom strip for its
    /// pager; the queue uses the full list height with a scrollbar.
    pub fn list_area(&self) -> Rect {
        match self.page {
            Page::Library => self.layout.library_list,
            Page::Queue => self.layout.queue_list,
            Page::Folders => self.layout.folder_list,
            Page::Search => self.layout.online_list,
            Page::Discover => self.layout.discover_list,
            Page::Downloads => self.layout.download_list,
            _ => self.layout.list,
        }
    }

    /// Scrollbar rect for the scrollable pages.
    pub fn scrollbar_rect(&self) -> Rect {
        match self.page {
            Page::Queue | Page::Smart | Page::Duplicates => self.layout.scrollbar,
            Page::Search => self.layout.online_scrollbar,
            Page::Discover => self.layout.discover_scrollbar,
            Page::Folders => self.layout.folder_scrollbar,
            Page::Downloads => self.layout.download_scrollbar,
            _ => Rect::default(),
        }
    }

    /// Sort / smart-kind / duplicate-mode chip. Smart and duplicate pages keep
    /// it in the header so it cannot cover the first row (and its repaint can
    /// no longer paint the row's duration over the button).
    pub fn page_sort_rect(&self) -> Rect {
        match self.page {
            Page::Smart | Page::Duplicates => self.layout.header_sort,
            _ => self.layout.sort,
        }
    }

    /// Rows that fit above the pager without scrolling.
    pub fn library_page_size(&self) -> usize {
        (self.layout.library_list.height() / self.row_height())
            .floor()
            .max(1.0) as usize
    }

    pub fn library_page(&self) -> usize {
        self.library_page
    }

    pub fn library_page_count(&self) -> usize {
        let size = self.library_page_size();
        self.core
            .snapshot()
            .library_total
            .div_ceil(size)
            .max(1)
    }

    /// Whole-library "add all" is meaningful as soon as the index has rows.
    pub fn library_can_queue_all(&self) -> bool {
        let snapshot = self.core.snapshot();
        snapshot.library_total > 0 || !snapshot.library.is_empty()
    }

    pub fn library_index_active(&self) -> bool {
        self.core.snapshot().library_index_active
    }

    /// Missing-file checks need at least one music folder.
    pub fn library_can_check_missing(&self) -> bool {
        !self.core.snapshot().library_folders.is_empty()
    }

    /// Ask the core for the page currently shown. The query window is exactly
    /// one page, so a background refresh can never shrink the visible list.
    fn request_library_page(&mut self) {
        let size = self.library_page_size();
        let offset = self.library_page.saturating_mul(size);
        self.library_page_synced = true;
        self.core.handle(Command::LibraryPage {
            offset,
            limit: size,
        });
    }

    /// Jump to a page (footer buttons, PgUp/PgDn, Home/End).
    pub fn set_library_page(&mut self, hwnd: HWND, page: usize) {
        let page = page.min(self.library_page_count().saturating_sub(1));
        if page == self.library_page {
            return;
        }
        self.library_page = page;
        self.scroll = 0.0;
        self.ui.set_hover(None);
        self.request_library_page();
        // A page switch is a large state change: repaint the whole window
        // once so no stale rows, pager or player-bar pixels survive.
        self.invalidate_all(hwnd);
    }

    fn turn_library_page(&mut self, hwnd: HWND, delta: i32) {
        let last = self.library_page_count().saturating_sub(1);
        let page = (self.library_page as i32 + delta).clamp(0, last as i32) as usize;
        self.set_library_page(hwnd, page);
    }

    pub fn row_info(&self, index: usize) -> Option<RowInfo<'_>> {
        let snapshot = self.core.snapshot();
        if self.page == Page::Search {
            let song = snapshot.online.rows().get(index)?;
            return Some(RowInfo {
                title: &song.name,
                artist: &song.artist,
                duration: None,
                current: false,
                missing: false,
                play_count: None,
                header: false,
            });
        }
        if self.page == Page::Discover {
            let song = snapshot.discover.songs.get(index)?;
            return Some(RowInfo {
                title: &song.name,
                artist: &song.artist,
                duration: None,
                current: false,
                missing: false,
                play_count: None,
                header: false,
            });
        }
        if matches!(self.page, Page::Library | Page::Smart | Page::Duplicates) {
            let entry = match self.page {
                Page::Smart => snapshot.library_smart.get(index)?,
                Page::Duplicates => snapshot.library_duplicate_rows.get(index)?,
                _ => snapshot.library.get(index)?,
            };
            let current = !entry.path.as_os_str().is_empty()
                && snapshot
                    .current
                    .and_then(|track| track.source.path())
                    .is_some_and(|path| path == entry.path.as_path());
            return Some(RowInfo {
                title: &entry.title,
                artist: &entry.artist,
                duration: entry.duration,
                current,
                missing: entry.missing,
                play_count: Some(entry.play_count),
                header: self.page == Page::Duplicates
                    && snapshot
                        .library_duplicate_headers
                        .get(index)
                        .copied()
                        .unwrap_or(false),
            });
        }
        if self.page == Page::Folders {
            let entry = snapshot.browser.entries.get(index)?;
            let kind = if entry.is_dir {
                "文件夹"
            } else if entry.is_playlist {
                "播放列表"
            } else {
                "音频"
            };
            return Some(RowInfo {
                title: &entry.name,
                artist: kind,
                duration: None,
                current: false,
                missing: false,
                play_count: None,
                header: false,
            });
        }
        let track = snapshot.queue.get(self.queue_index(index)?)?;
        Some(RowInfo {
            title: &track.title,
            artist: &track.artist,
            duration: track.duration,
            current: snapshot.current_index == self.queue_index(index),
            missing: false,
            play_count: None,
            header: false,
        })
    }

    /// Row under the cursor on the queue/library page, if any.
    pub fn hovered_row(&self) -> Option<usize> {
        match self.ui.hover_target() {
            Some(Target::Row(index)) => Some(index),
            _ => None,
        }
    }

    /// Generic settings row under the cursor, if any.
    pub fn hovered_setting_row(&self) -> Option<usize> {
        if self.page != Page::Settings || self.settings_tab == SettingsTab::Library {
            return None;
        }
        match self.ui.hover_target()? {
            Target::SettingsToggleRow(index)
            | Target::SettingsChoice(index, _)
            | Target::SettingsChoiceCycle(index)
            | Target::SettingsStep(index, _)
            | Target::SettingsAction(index)
            | Target::SettingsActionReset(index)
            | Target::SettingsText(index)
            | Target::SettingsColor(index) => Some(index),
            _ => None,
        }
    }

    /// Full label + value for a truncated settings row. Short values do not
    /// need a tooltip.
    pub fn setting_tooltip(&self, index: usize) -> Option<String> {
        if self.page != Page::Settings {
            return None;
        }
        let view = self.settings_views().get(index)?.clone();
        if view.value.chars().count() < 18 {
            return None;
        }
        Some(format!("{}：{}", view.label, view.value))
    }

    /// Multi-line hover tooltip for a queue or library row.
    pub fn row_tooltip(&self, index: usize) -> Option<String> {
        let snapshot = self.core.snapshot();
        if self.page == Page::Search {
            let song = snapshot.online.rows().get(index)?;
            let mut lines = vec![song.name.clone()];
            let mut sub = Vec::new();
            if !song.artist.trim().is_empty() {
                sub.push(song.artist.trim().to_string());
            }
            if !song.album.trim().is_empty() {
                sub.push(song.album.trim().to_string());
            }
            if !sub.is_empty() {
                lines.push(sub.join(" · "));
            }
            let mut meta = Vec::new();
            if let Some(date) = song.release_date.as_deref() {
                meta.push(date.to_string());
            }
            if let Some(addition) = song.addition.as_deref() {
                meta.push(addition.to_string());
            }
            if !meta.is_empty() {
                lines.push(meta.join(" · "));
            }
            lines.push(format!("在线曲目 ID {}", song.id));
            return Some(lines.join("\r\n"));
        }
        if self.page == Page::Library {
            let entry = snapshot.library.get(index)?;
            let mut lines = vec![entry.title.clone()];
            let mut sub = Vec::new();
            if !entry.artist.trim().is_empty() {
                sub.push(entry.artist.trim().to_string());
            }
            if !entry.album.trim().is_empty() {
                sub.push(entry.album.trim().to_string());
            }
            if !sub.is_empty() {
                lines.push(sub.join(" · "));
            }
            let mut meta = Vec::new();
            if !entry.format.trim().is_empty() {
                meta.push(entry.format.to_ascii_uppercase());
            }
            if let Some(duration) = entry.duration {
                meta.push(crate::paint::format_time(duration as f32));
            }
            if entry.play_count > 0 {
                meta.push(format!("播放 {} 次", entry.play_count));
            }
            if entry.missing {
                meta.push("文件缺失".to_string());
            }
            if !meta.is_empty() {
                lines.push(meta.join(" · "));
            }
            lines.push(entry.path.display().to_string());
            return Some(lines.join("\r\n"));
        }

        if self.page == Page::Folders {
            let entry = snapshot.browser.entries.get(index)?;
            let kind = if entry.is_dir {
                "文件夹"
            } else if entry.is_playlist {
                "播放列表"
            } else {
                "音频"
            };
            return Some(format!(
                "{}\r\n{}\r\n{}",
                entry.name,
                kind,
                entry.path.display()
            ));
        }

        let track = snapshot.queue.get(self.queue_index(index)?)?;
        let mut lines = vec![track.display_title()];
        let mut sub = Vec::new();
        let artist = track.display_artist();
        if !artist.trim().is_empty() {
            sub.push(artist);
        }
        if !track.album.trim().is_empty() {
            sub.push(track.album.trim().to_string());
        }
        if !sub.is_empty() {
            lines.push(sub.join(" · "));
        }
        let mut meta = Vec::new();
        if !track.source_name.trim().is_empty() {
            meta.push(track.source_name.clone());
        }
        if track.lossless {
            meta.push("无损".to_string());
        }
        if let Some(duration) = track.duration {
            meta.push(crate::paint::format_time(duration as f32));
        }
        if !meta.is_empty() {
            lines.push(meta.join(" · "));
        }
        match &track.source {
            TrackSource::Local { path } => lines.push(path.display().to_string()),
            TrackSource::Url { url } | TrackSource::Radio { url } => lines.push(url.clone()),
            TrackSource::Bamboo { song_id, .. } => lines.push(format!("Bamboo · {song_id}")),
        }
        Some(lines.join("\r\n"))
    }

    pub fn library_sort_label(&self) -> &'static str {
        self.core.snapshot().library_sort.label()
    }

    pub fn library_sort_index(&self) -> usize {
        sort_index(self.core.snapshot().library_sort)
    }

    pub fn sort_label_at(index: usize) -> &'static str {
        LIBRARY_SORTS
            .get(index)
            .map(|sort| sort.label())
            .unwrap_or("")
    }

    pub fn dedupe_mode(&self) -> DedupeMode {
        self.core.snapshot().library_dedupe_mode
    }

    /// Show/hide state is driven by the shell (it owns the LISTBOX child);
    /// this only stores it for painting and closes it on page changes.
    pub fn set_sort_menu_open(&mut self, hwnd: HWND, open: bool) {
        if self.sort_menu_open != open {
            self.sort_menu_open = open;
            self.invalidate(hwnd, self.layout.sort.inflate(2.0));
        }
    }

    pub fn settings_extensions_text(&self) -> String {
        self.core.snapshot().library_extensions.join(", ")
    }

    /// Switch settings category. A category change is a large layout change,
    /// so repaint the whole page once instead of invalidating every row.
    pub fn set_settings_tab(&mut self, hwnd: HWND, tab: SettingsTab) {
        if self.settings_tab == tab {
            return;
        }
        self.settings_tab = tab;
        self.settings_scroll = 0.0;
        self.ui.clear();
        if self.player_panel_open {
            self.set_player_panel(hwnd, false);
        }
        self.invalidate_all(hwnd);
    }

    pub fn settings_views(&self) -> Vec<SettingView> {
        self.settings_tab
            .fields()
            .iter()
            .map(|field| self.setting_view(*field))
            .collect()
    }

    fn source_position(&self) -> usize {
        let id = self.core.primary_source().map(|source| source.id.as_str());
        self.core
            .settings
            .sources
            .iter()
            .position(|source| Some(source.id.as_str()) == id)
            .map(|index| index + 1)
            .unwrap_or(1)
    }

    fn setting_view(&self, field: SettingField) -> SettingView {
        let settings = &self.core.settings;
        let playback = &settings.playback;
        let features = &settings.features;
        let window = &settings.window;
        let lyrics = &settings.lyrics;
        let desktop = &settings.desktop_lyrics;
        let toggle = |label, active| SettingView {
            label,
            kind: SettingKind::Toggle,
            value: if active { "开" } else { "关" }.to_string(),
            active,
            options: &[],
            selected: None,
        };
        let color = |label, rgba: [u8; 4]| SettingView {
            label,
            kind: SettingKind::Color,
            value: format!("#{:02X}{:02X}{:02X}", rgba[0], rgba[1], rgba[2]),
            active: false,
            options: &[],
            selected: None,
        };
        match field {
            SettingField::Gapless => toggle("无缝播放", playback.gapless),
            SettingField::AutoAdvance => toggle("自动下一首", playback.auto_advance),
            SettingField::Crossfade => SettingView {
                label: "交叉淡化",
                kind: SettingKind::Step,
                value: format!("{:.0} 秒", playback.crossfade_secs),
                active: playback.crossfade_secs > 0.05,
                options: &[],
                selected: None,
            },
            SettingField::ReplayGain => SettingView {
                label: "ReplayGain",
                kind: SettingKind::Choice,
                value: String::new(),
                active: playback.replaygain != ReplayGainMode::Off,
                options: &REPLAYGAIN_OPTIONS,
                selected: Some(match playback.replaygain {
                    ReplayGainMode::Off => 0,
                    ReplayGainMode::Track => 1,
                    ReplayGainMode::Album => 2,
                }),
            },
            SettingField::ReplayGainPreamp => SettingView {
                label: "预增益",
                kind: SettingKind::Step,
                value: format!("{:+.0} dB", playback.replaygain_preamp_db),
                active: playback.replaygain_preamp_db.abs() > 0.05,
                options: &[],
                selected: None,
            },
            SettingField::EqEnabled => toggle("均衡器", playback.eq_enabled),
            SettingField::EqPreset => SettingView {
                label: "均衡器预设",
                kind: SettingKind::Step,
                value: EQ_OPTIONS
                    .get(
                        EQ_PRESETS
                            .iter()
                            .position(|preset| *preset == playback.eq_preset)
                            .unwrap_or(0),
                    )
                    .copied()
                    .unwrap_or("平坦")
                    .to_string(),
                active: playback.eq_enabled,
                options: &[],
                selected: None,
            },
            SettingField::EqBand(index) => SettingView {
                label: EQ_BAND_LABELS[index as usize],
                kind: SettingKind::Step,
                value: format!("{:+.0} dB", playback.eq_bands[index as usize]),
                active: playback.eq_enabled,
                options: &[],
                selected: None,
            },
            SettingField::ReplayGainScan => toggle("扫描无标签曲目", playback.replaygain_scan),
            SettingField::SleepTimer => SettingView {
                label: "睡眠定时",
                kind: SettingKind::Choice,
                value: String::new(),
                active: self.core.sleep_selected_minutes().is_some(),
                options: &SLEEP_OPTIONS,
                selected: Some(match self.core.sleep_selected_minutes() {
                    None => 0,
                    Some(15) => 1,
                    Some(30) => 2,
                    Some(60) => 3,
                    Some(_) => 0,
                }),
            },
            SettingField::SleepTimerCustom => SettingView {
                label: "自定义睡眠定时",
                kind: SettingKind::Step,
                value: match self.core.sleep_selected_minutes() {
                    Some(minutes) => format!("{minutes} 分钟"),
                    None => "关闭".to_string(),
                },
                active: self.core.sleep_selected_minutes().is_some(),
                options: &[],
                selected: None,
            },
            SettingField::ABStart => SettingView {
                label: "A-B 循环起点",
                kind: SettingKind::Action,
                value: match self.core.snapshot().ab_start {
                    Some(secs) => format!("A {}", crate::core::util::format_time(secs)),
                    None => "设置 A 点".to_string(),
                },
                active: self.core.snapshot().ab_start.is_some(),
                options: &[],
                selected: None,
            },
            SettingField::ABEnd => SettingView {
                label: "A-B 循环终点",
                kind: SettingKind::Action,
                value: match self.core.snapshot().ab_end {
                    Some(secs) => format!("B {}", crate::core::util::format_time(secs)),
                    None => "设置 B 点".to_string(),
                },
                active: self.core.snapshot().ab_end.is_some(),
                options: &[],
                selected: None,
            },
            SettingField::ABClear => SettingView {
                label: "A-B 循环区间",
                kind: SettingKind::Action,
                value: "清除".to_string(),
                active: self.core.snapshot().ab_start.is_some(),
                options: &[],
                selected: None,
            },
            SettingField::OutputDevice => SettingView {
                label: "输出设备",
                kind: SettingKind::Step,
                value: settings
                    .playback
                    .device
                    .clone()
                    .unwrap_or_else(|| "系统默认".to_string()),
                active: settings.playback.device.is_some(),
                options: &[],
                selected: None,
            },
            SettingField::CloseBehavior => SettingView {
                label: "关闭按钮",
                kind: SettingKind::Choice,
                value: String::new(),
                active: window.close_behavior != CloseBehavior::Tray,
                options: &CLOSE_OPTIONS,
                selected: Some(match window.close_behavior {
                    CloseBehavior::Tray => 0,
                    CloseBehavior::Exit => 1,
                    CloseBehavior::Ask => 2,
                }),
            },
            SettingField::StartMinimized => toggle("启动时最小化", window.start_minimized),
            SettingField::RememberPosition => toggle("记住窗口位置", window.remember_position),
            SettingField::CaptureProtection => toggle("录屏捕获保护", window.capture_protection),
            SettingField::LyricsCaptureProtection => {
                toggle("歌词窗口捕获保护", window.lyrics_capture_protection)
            }
            SettingField::DesktopLyricsCaptureProtection => {
                toggle("桌面歌词捕获保护", settings.desktop_lyrics.capture_protection)
            }
            SettingField::AllowMultipleInstances => {
                toggle("允许多实例（需重启）", window.allow_multiple_instances)
            }
            SettingField::StartWithWindows => {
                toggle("开机启动", features.start_with_windows)
            }
            SettingField::FileAssociations => {
                toggle("音频文件关联", features.file_associations)
            }
            SettingField::PlaylistAssociations => {
                toggle(".mp3list 关联", features.playlist_associations)
            }
            SettingField::MediaControls => toggle("系统媒体控制", features.media_controls),
            SettingField::Notifications => toggle("切歌通知", features.notifications),
            SettingField::JumpList => toggle("任务栏跳转列表", features.jump_list),
            SettingField::ShortcutsEnabled => {
                toggle(crate::i18n::tr("全局快捷键"), settings.shortcuts.enabled)
            }
            SettingField::GamepadEnabled => {
                toggle(crate::i18n::tr("手柄控制"), settings.shortcuts.gamepad)
            }            SettingField::Shortcut(slot) => SettingView {
                label: crate::i18n::tr(slot.label()),
                kind: SettingKind::Action,
                value: if self.shortcut_capture == Some(slot) {
                    crate::i18n::tr("按下快捷键…（Esc 清除）").to_string()
                } else if settings.shortcuts.get(slot).trim().is_empty() {
                    crate::i18n::tr("未设置").to_string()
                } else {
                    settings.shortcuts.get(slot).to_string()
                },
                active: settings.shortcuts.enabled,
                options: &[],
                selected: None,
            },
            SettingField::ResetShortcuts => SettingView {
                label: crate::i18n::tr("恢复默认快捷键"),
                kind: SettingKind::Action,
                value: crate::i18n::tr("默认").to_string(),
                active: false,
                options: &[],
                selected: None,
            },
            SettingField::ExportSettings => SettingView {
                label: "导出设置…",
                kind: SettingKind::Action,
                value: "JSON".to_string(),
                active: false,
                options: &[],
                selected: None,
            },
            SettingField::ImportSettings => SettingView {
                label: "导入设置…",
                kind: SettingKind::Action,
                value: "JSON".to_string(),
                active: false,
                options: &[],
                selected: None,
            },
            SettingField::ResetSettings => SettingView {
                label: "恢复默认设置…",
                kind: SettingKind::Action,
                value: "全部".to_string(),
                active: false,
                options: &[],
                selected: None,
            },
            SettingField::SourceSelect => {
                let source = self.core.primary_source();
                SettingView {
                    label: "当前音源",
                    kind: SettingKind::Action,
                    value: source
                        .map(|source| {
                            format!("{} · {}/{}", source.name, self.source_position(), settings.sources.len())
                        })
                        .unwrap_or_else(|| "未配置".to_string()),
                    active: settings.sources.len() > 1,
                    options: &[],
                    selected: None,
                }
            }
            SettingField::SourceEnabled => {
                let enabled = self
                    .core
                    .primary_source()
                    .map(|source| source.enabled)
                    .unwrap_or(false);
                toggle("启用当前音源", enabled)
            }
            SettingField::SourceName => SettingView {
                label: "音源名称",
                kind: SettingKind::Text,
                value: self
                    .core
                    .primary_source()
                    .map(|source| source.name.clone())
                    .unwrap_or_default(),
                active: false,
                options: &[],
                selected: None,
            },
            SettingField::SourceBaseUrl => SettingView {
                label: "在线 API 地址",
                kind: SettingKind::Text,
                value: self
                    .core
                    .primary_source()
                    .map(|source| source.base_url.clone())
                    .unwrap_or_default(),
                active: true,
                options: &[],
                selected: None,
            },
            SettingField::SourceQuality => {
                let quality = self
                    .core
                    .primary_source()
                    .map(|source| source.quality)
                    .unwrap_or(crate::core::config::Quality::K320);
                SettingView {
                    label: "音源音质",
                    kind: SettingKind::Choice,
                    value: String::new(),
                    active: quality != crate::core::config::Quality::K320,
                    options: &SOURCE_QUALITY_OPTIONS,
                    selected: Some(match quality {
                        crate::core::config::Quality::K128 => 0,
                        crate::core::config::Quality::K320 => 1,
                        crate::core::config::Quality::Flac => 2,
                    }),
                }
            }
            SettingField::SourceHeaders => SettingView {
                label: "请求头 JSON",
                kind: SettingKind::Text,
                value: self
                    .core
                    .primary_source()
                    .map(|source| {
                        serde_json::to_string(&source.headers)
                            .unwrap_or_else(|_| "[]".to_string())
                    })
                    .unwrap_or_else(|| "[]".to_string()),
                active: self
                    .core
                    .primary_source()
                    .map(|source| !source.headers.is_empty())
                    .unwrap_or(false),
                options: &[],
                selected: None,
            },
            SettingField::SourceEndpoints => SettingView {
                label: "端点覆盖 JSON",
                kind: SettingKind::Text,
                value: self
                    .core
                    .primary_source()
                    .map(|source| {
                        serde_json::to_string(&source.endpoints)
                            .unwrap_or_else(|_| "{}".to_string())
                    })
                    .unwrap_or_else(|| "{}".to_string()),
                active: self
                    .core
                    .primary_source()
                    .map(|source| source.endpoints != crate::core::config::EndpointOverrides::default())
                    .unwrap_or(false),
                options: &[],
                selected: None,
            },
            SettingField::SourceAdd => SettingView {
                label: "新增音源",
                kind: SettingKind::Action,
                value: format!("共 {} 个", settings.sources.len()),
                active: false,
                options: &[],
                selected: None,
            },
            SettingField::SourceRemove => SettingView {
                label: "删除当前音源",
                kind: SettingKind::Action,
                value: "移除".to_string(),
                active: false,
                options: &[],
                selected: None,
            },
            SettingField::NetworkOffline => toggle("离线模式", settings.network.offline),
            SettingField::ApiMinInterval => SettingView {
                label: "在线请求间隔",
                kind: SettingKind::Step,
                value: format!("{} ms", settings.network.api_min_interval_ms),
                active: settings.network.api_min_interval_ms > 0,
                options: &[],
                selected: None,
            },
            SettingField::AlbumPageSize => SettingView {
                label: "专辑 / 歌手每页",
                kind: SettingKind::Step,
                value: format!("{} 首", settings.network.album_page_size),
                active: false,
                options: &[],
                selected: None,
            },
            SettingField::NetworkTimeout => SettingView {
                label: "请求超时",
                kind: SettingKind::Step,
                value: format!("{} 秒", settings.network.timeout_secs),
                active: false,
                options: &[],
                selected: None,
            },
            SettingField::NetworkRetries => SettingView {
                label: "失败重试",
                kind: SettingKind::Step,
                value: format!("{} 次", settings.network.retries),
                active: settings.network.retries > 0,
                options: &[],
                selected: None,
            },
            SettingField::ProxyMode => SettingView {
                label: "代理",
                kind: SettingKind::Choice,
                value: settings.network.proxy_url.clone(),
                active: settings.network.proxy_mode != crate::core::config::ProxyMode::System,
                options: &PROXY_OPTIONS,
                selected: Some(match settings.network.proxy_mode {
                    crate::core::config::ProxyMode::System => 0,
                    crate::core::config::ProxyMode::None => 1,
                    crate::core::config::ProxyMode::Manual => 2,
                }),
            },
            SettingField::ProxyUrl => SettingView {
                label: "代理地址",
                kind: SettingKind::Text,
                value: settings.network.proxy_url.clone(),
                active: !settings.network.proxy_url.is_empty(),
                options: &[],
                selected: None,
            },
            SettingField::CacheLimitMb => {
                let usage = self.core.snapshot().cache_usage;
                SettingView {
                    label: "流缓存上限",
                    kind: SettingKind::Step,
                    value: if settings.network.cache_limit_mb == 0 {
                        "关闭".to_string()
                    } else {
                        format!(
                            "{} MB · 已用 {:.0} MB",
                            settings.network.cache_limit_mb,
                            usage.bytes as f64 / (1024.0 * 1024.0)
                        )
                    },
                    active: settings.network.cache_limit_mb > 0,
                    options: &[],
                    selected: None,
                }
            }
            SettingField::StreamCacheDir => SettingView {
                label: "流缓存目录",
                kind: SettingKind::Action,
                value: settings
                    .network
                    .stream_cache_dir
                    .as_ref()
                    .map(|dir| dir.display().to_string())
                    .unwrap_or_else(|| "默认".to_string()),
                active: settings.network.stream_cache_dir.is_some(),
                options: &[],
                selected: None,
            },
            SettingField::ClearCache => SettingView {
                label: "清理缓存",
                kind: SettingKind::Action,
                value: "流 / 封面 / 歌词".to_string(),
                active: false,
                options: &[],
                selected: None,
            },
            SettingField::OnlineTest => SettingView {
                label: "测试在线音源",
                kind: SettingKind::Action,
                value: "连接".to_string(),
                active: false,
                options: &[],
                selected: None,
            },
            SettingField::DownloadDir => SettingView {
                label: "下载目录",
                kind: SettingKind::Action,
                value: settings
                    .download
                    .dir
                    .as_ref()
                    .map(|dir| dir.display().to_string())
                    .unwrap_or_else(|| "音乐文件夹 / BambooPlayer".to_string()),
                active: settings.download.dir.is_some(),
                options: &[],
                selected: None,
            },
            SettingField::DownloadConcurrency => SettingView {
                label: "同时下载",
                kind: SettingKind::Step,
                value: format!("{}", settings.download.concurrency),
                active: false,
                options: &[],
                selected: None,
            },
            SettingField::DownloadConflict => SettingView {
                label: "同名文件",
                kind: SettingKind::Choice,
                value: String::new(),
                active: settings.download.conflict != crate::core::config::ConflictPolicy::Rename,
                options: &CONFLICT_OPTIONS,
                selected: Some(match settings.download.conflict {
                    crate::core::config::ConflictPolicy::Rename => 0,
                    crate::core::config::ConflictPolicy::Skip => 1,
                    crate::core::config::ConflictPolicy::Overwrite => 2,
                }),
            },
            SettingField::DownloadQuality => SettingView {
                label: "下载音质",
                kind: SettingKind::Choice,
                value: String::new(),
                active: settings.download.quality.is_some(),
                options: &DOWNLOAD_QUALITY_OPTIONS,
                selected: Some(match settings.download.quality {
                    None => 0,
                    Some(crate::core::config::Quality::K320) => 1,
                    Some(crate::core::config::Quality::Flac) => 2,
                    Some(crate::core::config::Quality::K128) => 3,
                }),
            },
            SettingField::DownloadTemplate => SettingView {
                label: "文件名模板",
                kind: SettingKind::Text,
                value: settings.download.template.clone(),
                active: false,
                options: &[],
                selected: None,
            },
            SettingField::DownloadEmbedTags => {
                toggle("写入标签", settings.download.embed_tags)
            }
            SettingField::DownloadEmbedCover => {
                toggle("写入封面", settings.download.embed_cover)
            }
            SettingField::DownloadSaveLyrics => {
                toggle("保存歌词", settings.download.save_lyrics)
            }
            SettingField::DownloadAddToLibrary => {
                toggle("下载后加入曲库", settings.download.add_to_library)
            }
            SettingField::ClearFinishedDownloads => SettingView {
                label: "清除已结束的下载",
                kind: SettingKind::Action,
                value: format!(
                    "{} 项",
                    self.core
                        .snapshot()
                        .downloads
                        .iter()
                        .filter(|row| !row.state.is_active())
                        .count()
                ),
                active: false,
                options: &[],
                selected: None,
            },
            SettingField::LyricsKaraoke => toggle("逐字高亮（KTV）", settings.lyrics.karaoke),
            SettingField::LyricsWordsOnly => toggle(
                "仅逐字歌词逐字填充",
                settings.lyrics.karaoke_words_only,
            ),
            SettingField::LyricsTranslation => {
                toggle("显示翻译", settings.lyrics.show_translation)
            }
            SettingField::LyricsFontSize => SettingView {
                label: "歌词字号",
                kind: SettingKind::Step,
                value: format!("{:.0}", settings.lyrics.font_size),
                active: false,
                options: &[],
                selected: None,
            },
            SettingField::LyricsLineSpacing => SettingView {
                label: "歌词行距",
                kind: SettingKind::Step,
                value: format!("{:.0}", settings.lyrics.line_spacing),
                active: false,
                options: &[],
                selected: None,
            },
            SettingField::LyricsAlign => SettingView {
                label: "歌词对齐",
                kind: SettingKind::Choice,
                value: String::new(),
                active: false,
                options: &ALIGN_OPTIONS,
                selected: Some(match settings.lyrics.align {
                    LyricsAlign::Left => 0,
                    LyricsAlign::Center => 1,
                    LyricsAlign::Right => 2,
                }),
            },
            SettingField::LyricsFontFamily => SettingView {
                label: "歌词字体",
                kind: SettingKind::Action,
                value: settings
                    .lyrics
                    .font_family
                    .clone()
                    .unwrap_or_else(|| "跟随界面".to_string()),
                active: settings.lyrics.font_family.is_some(),
                options: &[],
                selected: None,
            },
            SettingField::LyricsFontWeight => SettingView {
                label: "字重",
                kind: SettingKind::Choice,
                value: String::new(),
                active: false,
                options: &WEIGHT_OPTIONS,
                selected: Some(weight_index(settings.lyrics.font_weight)),
            },
            SettingField::LyricsItalic => toggle("斜体", settings.lyrics.italic),
            SettingField::LyricsEncoding => SettingView {
                label: "歌词编码",
                kind: SettingKind::Choice,
                value: String::new(),
                active: settings.lyrics.encoding != crate::config::LyricsEncoding::Auto,
                options: &ENCODING_OPTIONS,
                selected: Some(lyrics_encoding_index(settings.lyrics.encoding)),
            },
            SettingField::LyricsOffset => SettingView {
                label: "歌词偏移",
                kind: SettingKind::Step,
                value: format!("{:+.1}s", settings.lyrics.offset_ms as f64 / 1000.0),
                active: settings.lyrics.offset_ms != 0,
                options: &[],
                selected: None,
            },
            SettingField::LyricsLanguage => SettingView {
                label: "翻译语言",
                kind: SettingKind::Choice,
                value: String::new(),
                active: settings.lyrics.translation_language != "auto",
                options: &LANGUAGE_OPTIONS,
                selected: Some(
                    LANGUAGE_VALUES
                        .iter()
                        .position(|value| *value == settings.lyrics.translation_language)
                        .unwrap_or(0),
                ),
            },
            SettingField::LyricsPrefetch => toggle("预取下一首歌词", settings.lyrics.prefetch_next),
            SettingField::LyricsShowSource => toggle("显示歌词来源", settings.lyrics.show_source),
            SettingField::LyricsPanelColor => color(
                "歌词面板底色",
                lyrics
                    .panel_color
                    .map(|rgb| [rgb[0], rgb[1], rgb[2], 255])
                    .unwrap_or([37, 39, 43, 255]),
            ),
            SettingField::LyricsPanelBackground => SettingView {
                label: "面板背景",
                kind: SettingKind::Choice,
                value: String::new(),
                active: lyrics.panel_background != LyricsPanelBackground::Theme,
                options: &LYRICS_PANEL_BG_OPTIONS,
                selected: Some(match lyrics.panel_background {
                    LyricsPanelBackground::Theme => 0,
                    LyricsPanelBackground::Solid => 1,
                    LyricsPanelBackground::Gradient => 2,
                    LyricsPanelBackground::Image => 3,
                }),
            },
            SettingField::LyricsPanelColor2 => color(
                "渐变终点色",
                [
                    lyrics.panel_color2[0],
                    lyrics.panel_color2[1],
                    lyrics.panel_color2[2],
                    255,
                ],
            ),
            SettingField::LyricsPanelBorderColor => color(
                "面板边框色",
                lyrics
                    .panel_border_color
                    .map(|rgb| [rgb[0], rgb[1], rgb[2], 255])
                    .unwrap_or([51, 54, 59, 255]),
            ),
            SettingField::LyricsPanelImage => SettingView {
                label: "背景图片",
                kind: SettingKind::Action,
                value: lyrics
                    .panel_image
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "选择图片…".to_string()),
                active: lyrics.panel_image.is_some(),
                options: &[],
                selected: None,
            },
            SettingField::LyricsPanelReset => SettingView {
                label: "恢复主题背景",
                kind: SettingKind::Action,
                value: "主题".to_string(),
                active: lyrics.panel_background == LyricsPanelBackground::Theme,
                options: &[],
                selected: None,
            },
            SettingField::LyricsOutlineWidth => SettingView {
                label: "描边宽度",
                kind: SettingKind::Step,
                value: format!("{:.1}", settings.lyrics.outline_width),
                active: settings.lyrics.outline_width > 0.0,
                options: &[],
                selected: None,
            },
            SettingField::LyricsOutlineColor => color(
                "描边颜色",
                [
                    settings.lyrics.outline_color[0],
                    settings.lyrics.outline_color[1],
                    settings.lyrics.outline_color[2],
                    255,
                ],
            ),
            SettingField::LyricsShadowOffset => SettingView {
                label: "阴影偏移",
                kind: SettingKind::Step,
                value: format!("{:.1}", settings.lyrics.shadow_offset),
                active: settings.lyrics.shadow_offset > 0.0,
                options: &[],
                selected: None,
            },
            SettingField::LyricsShadowColor => color(
                "阴影颜色",
                [
                    settings.lyrics.shadow_color[0],
                    settings.lyrics.shadow_color[1],
                    settings.lyrics.shadow_color[2],
                    255,
                ],
            ),
            SettingField::LyricsTransOutlineWidth => SettingView {
                label: "翻译描边宽度",
                kind: SettingKind::Step,
                value: format!("{:.1}", settings.lyrics.translation_outline_width),
                active: settings.lyrics.translation_outline_width > 0.0,
                options: &[],
                selected: None,
            },
            SettingField::LyricsTransOutlineColor => color(
                "翻译描边颜色",
                [
                    settings.lyrics.translation_outline_color[0],
                    settings.lyrics.translation_outline_color[1],
                    settings.lyrics.translation_outline_color[2],
                    255,
                ],
            ),
            SettingField::LyricsTransShadowOffset => SettingView {
                label: "翻译阴影偏移",
                kind: SettingKind::Step,
                value: format!("{:.1}", settings.lyrics.translation_shadow_offset),
                active: settings.lyrics.translation_shadow_offset > 0.0,
                options: &[],
                selected: None,
            },
            SettingField::LyricsTransShadowColor => color(
                "翻译阴影颜色",
                [
                    settings.lyrics.translation_shadow_color[0],
                    settings.lyrics.translation_shadow_color[1],
                    settings.lyrics.translation_shadow_color[2],
                    255,
                ],
            ),
            SettingField::LargeFontFamily => SettingView {
                label: "字体",
                kind: SettingKind::Action,
                value: settings
                    .large_lyrics
                    .font_family
                    .clone()
                    .unwrap_or_else(|| "跟随界面".to_string()),
                active: settings.large_lyrics.font_family.is_some(),
                options: &[],
                selected: None,
            },
            SettingField::LargeFontWeight => SettingView {
                label: "字重",
                kind: SettingKind::Choice,
                value: String::new(),
                active: false,
                options: &WEIGHT_OPTIONS,
                selected: Some(weight_index(settings.large_lyrics.font_weight)),
            },
            SettingField::LargeItalic => toggle("斜体", settings.large_lyrics.italic),
            SettingField::LargeOutlineWidth => SettingView {
                label: "描边宽度",
                kind: SettingKind::Step,
                value: format!("{:.1}", settings.large_lyrics.outline_width),
                active: settings.large_lyrics.outline_width > 0.0,
                options: &[],
                selected: None,
            },
            SettingField::LargeOutlineColor => color(
                "描边颜色",
                [
                    settings.large_lyrics.outline_color[0],
                    settings.large_lyrics.outline_color[1],
                    settings.large_lyrics.outline_color[2],
                    255,
                ],
            ),
            SettingField::LargeShadowOffset => SettingView {
                label: "阴影偏移",
                kind: SettingKind::Step,
                value: format!("{:.1}", settings.large_lyrics.shadow_offset),
                active: settings.large_lyrics.shadow_offset > 0.0,
                options: &[],
                selected: None,
            },
            SettingField::LargeShadowColor => color(
                "阴影颜色",
                [
                    settings.large_lyrics.shadow_color[0],
                    settings.large_lyrics.shadow_color[1],
                    settings.large_lyrics.shadow_color[2],
                    255,
                ],
            ),
            SettingField::LargeTransOutlineWidth => SettingView {
                label: "翻译描边宽度",
                kind: SettingKind::Step,
                value: format!("{:.1}", settings.large_lyrics.translation_outline_width),
                active: settings.large_lyrics.translation_outline_width > 0.0,
                options: &[],
                selected: None,
            },
            SettingField::LargeTransOutlineColor => color(
                "翻译描边颜色",
                [
                    settings.large_lyrics.translation_outline_color[0],
                    settings.large_lyrics.translation_outline_color[1],
                    settings.large_lyrics.translation_outline_color[2],
                    255,
                ],
            ),
            SettingField::LargeTransShadowOffset => SettingView {
                label: "翻译阴影偏移",
                kind: SettingKind::Step,
                value: format!("{:.1}", settings.large_lyrics.translation_shadow_offset),
                active: settings.large_lyrics.translation_shadow_offset > 0.0,
                options: &[],
                selected: None,
            },
            SettingField::LargeTransShadowColor => color(
                "翻译阴影颜色",
                [
                    settings.large_lyrics.translation_shadow_color[0],
                    settings.large_lyrics.translation_shadow_color[1],
                    settings.large_lyrics.translation_shadow_color[2],
                    255,
                ],
            ),
            SettingField::LargeLyricsFontSize => SettingView {
                label: "独立窗口字号",
                kind: SettingKind::Step,
                value: format!("{:.0}", settings.large_lyrics.font_size),
                active: false,
                options: &[],
                selected: None,
            },
            SettingField::LyricsHighlightColor => {
                color("高亮颜色", [lyrics.highlight_color[0], lyrics.highlight_color[1], lyrics.highlight_color[2], 255])
            }
            SettingField::LyricsCurrentColor => color(
                "当前行颜色",
                [lyrics.current_color[0], lyrics.current_color[1], lyrics.current_color[2], 255],
            ),
            SettingField::LyricsNormalColor => color(
                "普通行颜色",
                [lyrics.normal_color[0], lyrics.normal_color[1], lyrics.normal_color[2], 255],
            ),
            SettingField::LyricsTranslationColor => color(
                "翻译行颜色",
                [
                    lyrics.translation_color[0],
                    lyrics.translation_color[1],
                    lyrics.translation_color[2],
                    255,
                ],
            ),
            SettingField::LargeLineSpacing => SettingView {
                label: "独立窗口行距",
                kind: SettingKind::Step,
                value: format!("{:.0}", settings.large_lyrics.line_spacing),
                active: false,
                options: &[],
                selected: None,
            },
            SettingField::LargeAlign => SettingView {
                label: "独立窗口对齐",
                kind: SettingKind::Choice,
                value: String::new(),
                active: false,
                options: &ALIGN_OPTIONS,
                selected: Some(match settings.large_lyrics.align {
                    LyricsAlign::Left => 0,
                    LyricsAlign::Center => 1,
                    LyricsAlign::Right => 2,
                }),
            },
            SettingField::LargeTranslation => {
                toggle("显示翻译", settings.large_lyrics.show_translation)
            }
            SettingField::LargeKaraoke => toggle("逐字高亮（KTV）", settings.large_lyrics.karaoke),
            SettingField::LargeWordsOnly => toggle(
                "仅逐字歌词逐字填充",
                settings.large_lyrics.karaoke_words_only,
            ),
            SettingField::LargeShowCover => toggle("显示封面", settings.large_lyrics.show_cover),
            SettingField::LargeShowTitle => toggle("显示标题/歌手", settings.large_lyrics.show_title),
            SettingField::LargeShowFooter => {
                toggle("显示播放控制", settings.large_lyrics.show_footer)
            }
            SettingField::LargeHighlightColor => color(
                "高亮颜色",
                [
                    settings.large_lyrics.highlight_color[0],
                    settings.large_lyrics.highlight_color[1],
                    settings.large_lyrics.highlight_color[2],
                    255,
                ],
            ),
            SettingField::LargeCurrentColor => color(
                "当前行颜色",
                [
                    settings.large_lyrics.current_color[0],
                    settings.large_lyrics.current_color[1],
                    settings.large_lyrics.current_color[2],
                    255,
                ],
            ),
            SettingField::LargeNormalColor => color(
                "普通行颜色",
                [
                    settings.large_lyrics.normal_color[0],
                    settings.large_lyrics.normal_color[1],
                    settings.large_lyrics.normal_color[2],
                    255,
                ],
            ),
            SettingField::LargeTranslationColor => color(
                "翻译行颜色",
                [
                    settings.large_lyrics.translation_color[0],
                    settings.large_lyrics.translation_color[1],
                    settings.large_lyrics.translation_color[2],
                    255,
                ],
            ),
            SettingField::DesktopCurrentColor => color("当前行颜色", desktop.current_color),
            SettingField::DesktopNextColor => color("下一行颜色", desktop.next_color),
            SettingField::DesktopPrevColor => color("上一行颜色", desktop.prev_color),
            SettingField::DesktopFooterColor => color("标题/歌手颜色", desktop.footer_color),
            SettingField::DesktopTranslationColor => color("翻译行颜色", desktop.translation_color),
            SettingField::DesktopOutlineColor => color("描边颜色", desktop.outline_color),
            SettingField::DesktopShadowColor => color("阴影颜色", desktop.shadow_color),
            SettingField::DesktopBgColor => color("背景颜色", desktop.bg_color),
            SettingField::DesktopBgColor2 => color("渐变第二色", desktop.bg_color2),
            SettingField::DesktopBackgroundImage => {
                let name = desktop
                    .bg_image
                    .as_ref()
                    .and_then(|path| path.file_name())
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "选择图片…".to_string());
                SettingView {
                    label: "背景图片",
                    kind: SettingKind::Action,
                    value: name,
                    active: desktop.background
                        == crate::config::DesktopBackgroundMode::Image,
                    options: &[],
                    selected: None,
                }
            }
            SettingField::DesktopClearBackgroundImage => SettingView {
                label: "清除背景图片",
                kind: SettingKind::Action,
                value: "清除".to_string(),
                active: desktop.bg_image.is_some(),
                options: &[],
                selected: None,
            },
            SettingField::DesktopFontFamily => SettingView {
                label: "字体",
                kind: SettingKind::Action,
                value: desktop
                    .font_family
                    .clone()
                    .unwrap_or_else(|| "跟随界面".to_string()),
                active: desktop.font_family.is_some(),
                options: &[],
                selected: None,
            },
            SettingField::DesktopTransFontFamily => SettingView {
                label: "翻译字体",
                kind: SettingKind::Action,
                value: desktop
                    .translation_font_family
                    .clone()
                    .unwrap_or_else(|| "跟随主行".to_string()),
                active: desktop.translation_font_family.is_some(),
                options: &[],
                selected: None,
            },
            SettingField::DesktopFooterFontFamily => SettingView {
                label: "页脚字体",
                kind: SettingKind::Action,
                value: desktop
                    .footer_font_family
                    .clone()
                    .unwrap_or_else(|| "跟随主行".to_string()),
                active: desktop.footer_font_family.is_some(),
                options: &[],
                selected: None,
            },
            SettingField::DesktopTransBold => toggle("翻译加粗", desktop.translation_bold),
            SettingField::DesktopFooterBold => toggle("页脚加粗", desktop.footer_bold),
            SettingField::DesktopOutlineWidth => SettingView {
                label: "描边宽度",
                kind: SettingKind::Step,
                value: format!("{:.1}", desktop.outline_width),
                active: false,
                options: &[],
                selected: None,
            },
            SettingField::DesktopShadowOffset => SettingView {
                label: "阴影偏移",
                kind: SettingKind::Step,
                value: format!("{:.1}", desktop.shadow_offset),
                active: false,
                options: &[],
                selected: None,
            },
            SettingField::DesktopTranslationOutline => {
                toggle("翻译行描边", desktop.translation_outline)
            }
            SettingField::DesktopTranslationShadow => {
                toggle("翻译行阴影", desktop.translation_shadow)
            }
            SettingField::DesktopFooterOutline => toggle("标题/歌手描边", desktop.footer_outline),
            SettingField::DesktopFooterShadow => toggle("标题/歌手阴影", desktop.footer_shadow),
            SettingField::DesktopHideWhenFullscreen => {
                toggle("全屏时隐藏", desktop.hide_when_fullscreen)
            }
            SettingField::DesktopShow => {
                toggle("显示桌面歌词", desktop.enabled && desktop.visible_at_start)
            }
            SettingField::DesktopLock => toggle("锁定位置", desktop.locked),
            SettingField::DesktopClickThrough => toggle("鼠标穿透", desktop.click_through),
            SettingField::DesktopAlwaysOnTop => toggle("置顶", desktop.always_on_top),
            SettingField::DesktopLines => SettingView {
                label: "歌词行数",
                kind: SettingKind::Choice,
                value: String::new(),
                active: false,
                options: &DESKTOP_LINE_OPTIONS,
                selected: Some((desktop.lines.clamp(1, 3) - 1) as usize),
            },
            SettingField::DesktopTranslation => toggle("显示翻译", desktop.show_translation),
            SettingField::DesktopFooter => toggle("显示标题/歌手", desktop.show_footer),
            SettingField::DesktopFontSize => SettingView {
                label: "歌词字号",
                kind: SettingKind::Step,
                value: format!("{:.0}", desktop.font_size),
                active: false,
                options: &[],
                selected: None,
            },
            SettingField::DesktopNextFontSize => SettingView {
                label: "下一行字号",
                kind: SettingKind::Step,
                value: format!("{:.0}", desktop.next_font_size),
                active: false,
                options: &[],
                selected: None,
            },
            SettingField::DesktopPrevFontSize => SettingView {
                label: "上一行字号",
                kind: SettingKind::Step,
                value: format!("{:.0}", desktop.prev_font_size),
                active: false,
                options: &[],
                selected: None,
            },
            SettingField::DesktopFooterFontSize => SettingView {
                label: "信息字号",
                kind: SettingKind::Step,
                value: format!("{:.0}", desktop.footer_font_size),
                active: false,
                options: &[],
                selected: None,
            },
            SettingField::DesktopFrameRate => SettingView {
                label: "帧率",
                kind: SettingKind::Step,
                value: format!("{} fps", desktop.frame_rate.clamp(15, 120)),
                active: desktop.frame_rate != 60,
                options: &[],
                selected: None,
            },
            SettingField::DesktopBold => toggle("加粗", desktop.bold),
            SettingField::DesktopItalic => toggle("斜体", desktop.italic),
            SettingField::DesktopTransOutlineWidth => SettingView {
                label: "翻译描边宽度",
                kind: SettingKind::Step,
                value: format!("{:.1}", desktop.translation_outline_width),
                active: desktop.translation_outline_width > 0.0,
                options: &[],
                selected: None,
            },
            SettingField::DesktopTransShadowOffset => SettingView {
                label: "翻译阴影偏移",
                kind: SettingKind::Step,
                value: format!("{:.1}", desktop.translation_shadow_offset),
                active: desktop.translation_shadow_offset > 0.0,
                options: &[],
                selected: None,
            },
            SettingField::DesktopFooterOutlineWidth => SettingView {
                label: "页脚描边宽度",
                kind: SettingKind::Step,
                value: format!("{:.1}", desktop.footer_outline_width),
                active: desktop.footer_outline_width > 0.0,
                options: &[],
                selected: None,
            },
            SettingField::DesktopFooterShadowOffset => SettingView {
                label: "页脚阴影偏移",
                kind: SettingKind::Step,
                value: format!("{:.1}", desktop.footer_shadow_offset),
                active: desktop.footer_shadow_offset > 0.0,
                options: &[],
                selected: None,
            },
            SettingField::DesktopTransOutlineColor => color(
                "翻译描边颜色",
                [
                    desktop.translation_outline_color[0],
                    desktop.translation_outline_color[1],
                    desktop.translation_outline_color[2],
                    255,
                ],
            ),
            SettingField::DesktopTransShadowColor => color(
                "翻译阴影颜色",
                [
                    desktop.translation_shadow_color[0],
                    desktop.translation_shadow_color[1],
                    desktop.translation_shadow_color[2],
                    255,
                ],
            ),
            SettingField::DesktopFooterOutlineColor => color(
                "页脚描边颜色",
                [
                    desktop.footer_outline_color[0],
                    desktop.footer_outline_color[1],
                    desktop.footer_outline_color[2],
                    255,
                ],
            ),
            SettingField::DesktopFooterShadowColor => color(
                "页脚阴影颜色",
                [
                    desktop.footer_shadow_color[0],
                    desktop.footer_shadow_color[1],
                    desktop.footer_shadow_color[2],
                    255,
                ],
            ),
            SettingField::DesktopBlurRadius => SettingView {
                label: "背景模糊强度",
                kind: SettingKind::Step,
                value: format!("{:.0}", desktop.blur_radius),
                active: desktop.blur_radius > 0.0,
                options: &[],
                selected: None,
            },
            SettingField::DesktopAlign => SettingView {
                label: "对齐",
                kind: SettingKind::Choice,
                value: String::new(),
                active: false,
                options: &ALIGN_OPTIONS,
                selected: Some(match desktop.align {
                    LyricsAlign::Left => 0,
                    LyricsAlign::Center => 1,
                    LyricsAlign::Right => 2,
                }),
            },
            SettingField::DesktopBackground => SettingView {
                label: "背景",
                kind: SettingKind::Step,
                value: DESKTOP_BG_OPTIONS
                    [desktop_background_index(desktop.background) as usize]
                    .to_string(),
                active: desktop.background != crate::config::DesktopBackgroundMode::Transparent,
                options: &[],
                selected: None,
            },
            SettingField::DesktopOpacity => SettingView {
                label: "不透明度",
                kind: SettingKind::Step,
                value: format!("{:.0}%", desktop.opacity * 100.0),
                active: false,
                options: &[],
                selected: None,
            },
            SettingField::DesktopRadius => SettingView {
                label: "圆角",
                kind: SettingKind::Step,
                value: format!("{}", desktop.radius),
                active: false,
                options: &[],
                selected: None,
            },
            SettingField::DesktopPadding => SettingView {
                label: "内边距",
                kind: SettingKind::Step,
                value: format!("{:.0}", desktop.padding),
                active: false,
                options: &[],
                selected: None,
            },
            SettingField::DesktopLineSpacing => SettingView {
                label: "行间距",
                kind: SettingKind::Step,
                value: format!("{:.0}", desktop.line_spacing),
                active: false,
                options: &[],
                selected: None,
            },
            SettingField::DesktopShadow => toggle("文字阴影", desktop.shadow),
            SettingField::DesktopOutline => toggle("文字描边", desktop.outline),
            SettingField::DesktopBorder => toggle("面板边框", desktop.border),
            SettingField::DesktopWindowShadow => toggle("窗口阴影", desktop.window_shadow),
            SettingField::DesktopAutoSize => toggle("自动尺寸", desktop.auto_size),
            SettingField::DesktopWidth => SettingView {
                label: "窗口宽度",
                kind: SettingKind::Step,
                value: format!("{:.0}", desktop.width),
                active: false,
                options: &[],
                selected: None,
            },
            SettingField::DesktopHeight => SettingView {
                label: "窗口高度",
                kind: SettingKind::Step,
                value: format!("{:.0}", desktop.height),
                active: false,
                options: &[],
                selected: None,
            },
            SettingField::DesktopHideWhenPaused => {
                toggle("暂停时隐藏", desktop.hide_when_paused)
            }
            SettingField::DesktopCaptureProtection => {
                toggle("录屏捕获保护", desktop.capture_protection)
            }
            SettingField::DesktopKaraoke => toggle("逐字高亮（KTV）", desktop.karaoke),
            SettingField::DesktopWordsOnly => {
                toggle("仅逐字歌词逐字填充", desktop.karaoke_words_only)
            }
            SettingField::DesktopResetPosition => SettingView {
                label: "窗口位置",
                kind: SettingKind::Toggle,
                value: "重置".to_string(),
                active: false,
                options: &[],
                selected: None,
            },
            SettingField::MiniShowLyrics => toggle("显示歌词", settings.mini.show_lyrics),
            SettingField::MiniKaraoke => toggle("逐字高亮（KTV）", settings.mini.karaoke),
            SettingField::MiniWordsOnly => {
                toggle("仅逐字歌词逐字填充", settings.mini.karaoke_words_only)
            }
            SettingField::MiniLines => SettingView {
                label: "歌词行数",
                kind: SettingKind::Choice,
                value: String::new(),
                active: false,
                options: &MINI_LINE_OPTIONS,
                selected: Some((settings.mini.lyrics_lines.clamp(1, 2) - 1) as usize),
            },
            SettingField::MiniBold => toggle("歌词加粗", settings.mini.lyrics_bold),
            SettingField::MiniTranslation => {
                toggle("显示翻译", settings.mini.show_translation)
            }
            SettingField::MiniFontSize => SettingView {
                label: "歌词字号",
                kind: SettingKind::Step,
                value: format!("{:.0}", settings.mini.lyrics_font_size),
                active: false,
                options: &[],
                selected: None,
            },
            SettingField::MiniFrameRate => SettingView {
                label: "帧率",
                kind: SettingKind::Step,
                value: format!("{} fps", settings.mini.frame_rate.clamp(15, 120)),
                active: settings.mini.frame_rate != 60,
                options: &[],
                selected: None,
            },
            SettingField::MiniAutoHeight => toggle("自动高度", settings.mini.auto_height),
            SettingField::MiniHeight => SettingView {
                label: "窗口高度",
                kind: SettingKind::Step,
                value: format!("{:.0}", settings.mini.height),
                active: !settings.mini.auto_height,
                options: &[],
                selected: None,
            },
            SettingField::MiniAlwaysOnTop => toggle("置顶", settings.mini.always_on_top),
            SettingField::MiniOpacity => SettingView {
                label: "不透明度",
                kind: SettingKind::Step,
                value: format!("{:.0}%", settings.mini.opacity * 100.0),
                active: false,
                options: &[],
                selected: None,
            },
            SettingField::MiniWidth => SettingView {
                label: "窗口宽度",
                kind: SettingKind::Step,
                value: format!("{:.0}", settings.mini.width),
                active: false,
                options: &[],
                selected: None,
            },
            SettingField::MiniTaskbar => {
                toggle("显示在任务栏", settings.mini.show_in_taskbar)
            }
            SettingField::MiniPanelColor => {
                color("面板颜色", [settings.mini.panel_color[0], settings.mini.panel_color[1], settings.mini.panel_color[2], 255])
            }
            SettingField::MiniTextColor => {
                color("文字颜色", [settings.mini.text_color[0], settings.mini.text_color[1], settings.mini.text_color[2], 255])
            }
            SettingField::MiniAccentColor => {
                color("强调色", [settings.mini.accent_color[0], settings.mini.accent_color[1], settings.mini.accent_color[2], 255])
            }
            SettingField::ThemeFontFamily => SettingView {
                label: "界面字体",
                kind: SettingKind::Action,
                value: settings
                    .theme
                    .font_family
                    .clone()
                    .unwrap_or_else(|| "Microsoft YaHei UI".to_string()),
                active: settings.theme.font_family.is_some(),
                options: &[],
                selected: None,
            },
            SettingField::ThemeFontSize => SettingView {
                label: "界面字号",
                kind: SettingKind::Step,
                value: format!("{:.0}", settings.theme.font_size),
                active: false,
                options: &[],
                selected: None,
            },
            SettingField::ThemeMode => SettingView {
                label: "主题",
                kind: SettingKind::Choice,
                value: String::new(),
                active: false,
                options: &THEME_OPTIONS,
                selected: Some(match settings.theme.mode {
                    ThemeMode::Dark => 0,
                    ThemeMode::Light => 1,
                    ThemeMode::System => 2,
                }),
            },
            SettingField::Accent => SettingView {
                label: "强调色",
                kind: SettingKind::Step,
                value: crate::theme::ACCENT_NAMES[accent_index(settings.theme.accent)].to_string(),
                active: false,
                options: &[],
                selected: None,
            },
            SettingField::Density => SettingView {
                label: "列表密度",
                kind: SettingKind::Choice,
                value: String::new(),
                active: false,
                options: &DENSITY_OPTIONS,
                selected: Some(match settings.theme.list_density {
                    ListDensity::Compact => 0,
                    ListDensity::Comfortable => 1,
                    ListDensity::Spacious => 2,
                }),
            },
            SettingField::ReduceMotion => {
                toggle("减少动态效果", settings.theme.reduce_motion)
            }
            SettingField::FrameRate => SettingView {
                label: "动画帧率上限",
                kind: SettingKind::Step,
                value: format!("{} fps", settings.performance.frame_rate),
                active: settings.performance.frame_rate < 120,
                options: &[],
                selected: None,
            },
            SettingField::BackgroundMode => SettingView {
                label: "背景",
                kind: SettingKind::Choice,
                value: String::new(),
                active: settings.theme.background != crate::config::BackgroundMode::Solid,
                options: &BACKGROUND_OPTIONS,
                selected: Some(match settings.theme.background {
                    crate::config::BackgroundMode::Solid => 0,
                    crate::config::BackgroundMode::Image => 2,
                    _ => 1,
                }),
            },
            SettingField::BackgroundImage => SettingView {
                label: "背景图片",
                kind: SettingKind::Action,
                value: settings
                    .theme
                    .background_image
                    .as_ref()
                    .and_then(|path| path.file_name())
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "选择图片…".to_string()),
                active: matches!(
                    settings.theme.background,
                    crate::config::BackgroundMode::Image
                ),
                options: &[],
                selected: None,
            },
            SettingField::ClearBackgroundImage => SettingView {
                label: "清除背景图片",
                kind: SettingKind::Action,
                value: "清除".to_string(),
                active: settings.theme.background_image.is_some(),
                options: &[],
                selected: None,
            },
            SettingField::VisualizerEnabled => {
                toggle("音频可视化", settings.visualizer.enabled)
            }
            SettingField::VisualizerMode => SettingView {
                label: "可视化样式",
                kind: SettingKind::Choice,
                value: String::new(),
                active: settings.visualizer.enabled,
                options: &VISUALIZER_MODE_OPTIONS,
                selected: Some(match settings.visualizer.mode {
                    crate::config::VisualizerMode::Waveform => 1,
                    crate::config::VisualizerMode::Ring => 2,
                    _ => 0,
                }),
            },
            SettingField::MetadataMode => SettingView {
                label: "标签读取（下次启动）",
                kind: SettingKind::Choice,
                value: String::new(),
                active: settings.performance.metadata_mode
                    != crate::config::MetadataMode::Off,
                options: &METADATA_OPTIONS,
                selected: Some(match settings.performance.metadata_mode {
                    crate::config::MetadataMode::Off => 0,
                    crate::config::MetadataMode::All => 2,
                    _ => 1,
                }),
            },
            SettingField::LoadCovers => toggle("解码封面", settings.performance.load_covers),
            SettingField::LazyAudio => toggle(
                "音频设备延迟初始化（下次启动）",
                settings.performance.lazy_audio,
            ),
            SettingField::ReleaseAudioOutput => {
                toggle("暂停时释放音频设备", settings.performance.release_audio_output)
            }
            SettingField::DeferPausedRestore => {
                toggle("恢复暂停会话时不预解码", settings.performance.defer_paused_restore)
            }
            SettingField::IdleFrameRate => SettingView {
                label: "空闲刷新率",
                kind: SettingKind::Step,
                value: format!("{} fps", settings.performance.idle_frame_rate),
                active: false,
                options: &[],
                selected: None,
            },
            SettingField::PauseAnimationsUnfocused => toggle(
                "失焦/最小化时暂停动画",
                settings.performance.pause_animations_unfocused,
            ),
            SettingField::ThumbnailCache => SettingView {
                label: "缩略图缓存上限",
                kind: SettingKind::Step,
                value: if settings.performance.thumbnail_cache == 0 {
                    "不限".to_string()
                } else {
                    format!("{} 张", settings.performance.thumbnail_cache)
                },
                active: false,
                options: &[],
                selected: None,
            },
            SettingField::D2DHardware => toggle(
                "D2D 硬件渲染（桌面歌词/迷你）",
                settings.performance.d2d_hardware,
            ),
            SettingField::TrimWorkingSet => toggle(
                "空闲时回收内存",
                settings.window.trim_working_set_on_idle,
            ),
            SettingField::TrimIdleSecs => SettingView {
                label: "空闲回收延迟",
                kind: SettingKind::Step,
                value: format!("{} 秒", settings.window.trim_idle_secs),
                active: false,
                options: &[],
                selected: None,
            },
            SettingField::OptimizeIndex => SettingView {
                label: "曲库索引维护",
                kind: SettingKind::Action,
                value: "优化索引".to_string(),
                active: true,
                options: &[],
                selected: None,
            },
        }
    }

    /// Font-family rows that can be reset to "follow the UI (or main line)".
    pub fn font_reset_command(field: SettingField) -> Option<Command> {
        match field {
            SettingField::LyricsFontFamily => Some(Command::SetLyricsFontFamily(None)),
            SettingField::LargeFontFamily => Some(Command::SetLargeLyricsFontFamily(None)),
            SettingField::DesktopFontFamily => Some(Command::SetDesktopLyricsFontFamily(None)),
            SettingField::DesktopTransFontFamily => {
                Some(Command::SetDesktopLyricsTranslationFontFamily(None))
            }
            SettingField::DesktopFooterFontFamily => {
                Some(Command::SetDesktopLyricsFooterFontFamily(None))
            }
            SettingField::ThemeFontFamily => Some(Command::SetThemeFontFamily(None)),
            _ => None,
        }
    }

    /// Apply a click on one generic settings row. `option` selects a segmented
    /// choice; `delta` is used by stepper rows.
    pub fn apply_setting(
        &mut self,
        hwnd: HWND,
        index: usize,
        option: Option<usize>,
        delta: i32,
    ) {
        let Some(field) = self.settings_tab.fields().get(index).copied() else {
            return;
        };
        // Compact choice controls cycle with one click; expand that into the
        // next option index so the per-field arms stay unchanged.
        let option = if option.is_none() && delta > 0 {
            self.settings_views().get(index).and_then(|view| {
                let count = view.options.len().max(1);
                view.selected.map(|selected| (selected + 1) % count)
            })
        } else {
            option
        };
        let playback = &self.core.settings.playback;
        let command = match field {
            SettingField::Gapless => Command::SetGapless(!playback.gapless),
            SettingField::AutoAdvance => Command::SetAutoAdvance(!playback.auto_advance),
            SettingField::Crossfade => {
                Command::SetCrossfade(playback.crossfade_secs + delta.max(-1).min(1) as f32)
            }
            SettingField::ReplayGain => match option.unwrap_or(0) {
                1 => Command::SetReplayGainMode(ReplayGainMode::Track),
                2 => Command::SetReplayGainMode(ReplayGainMode::Album),
                _ => Command::SetReplayGainMode(ReplayGainMode::Off),
            },
            SettingField::ReplayGainPreamp => {
                Command::SetReplayGainPreamp(playback.replaygain_preamp_db + delta as f32)
            }
            SettingField::EqEnabled => Command::SetEqEnabled(!playback.eq_enabled),
            SettingField::EqPreset => {
                let current = EQ_PRESETS
                    .iter()
                    .position(|preset| *preset == playback.eq_preset)
                    .unwrap_or(0) as i32;
                let next = (current + delta).rem_euclid(EQ_PRESETS.len() as i32) as usize;
                Command::SetEqPreset(EQ_PRESETS[next])
            }
            SettingField::EqBand(index) => {
                let band = index as usize;
                Command::SetEqBand(band, playback.eq_bands[band] + delta as f32)
            }
            SettingField::ReplayGainScan => {
                Command::SetReplayGainScan(!playback.replaygain_scan)
            }
            SettingField::SleepTimer => {
                Command::SetSleepTimer(match option.unwrap_or(0) {
                    1 => Some(15),
                    2 => Some(30),
                    3 => Some(60),
                    _ => None,
                })
            }
            SettingField::SleepTimerCustom => {
                let current = self.core.sleep_selected_minutes().unwrap_or(0) as i32;
                let next = (current + delta * 5).clamp(0, 720);
                Command::SetSleepTimer((next > 0).then_some(next as u32))
            }
            SettingField::ABStart => Command::SetABStart,
            SettingField::ABEnd => Command::SetABEnd,
            SettingField::ABClear => Command::ClearABRepeat,
            SettingField::OutputDevice => {
                let mut options = vec![None];
                options.extend(
                    self.core
                        .output_devices()
                        .iter()
                        .cloned()
                        .map(Some),
                );
                let current = self
                    .core
                    .settings
                    .playback
                    .device
                    .as_ref()
                    .and_then(|device| options.iter().position(|item| item.as_ref() == Some(device)))
                    .unwrap_or(0) as i32;
                let next = (current + delta).rem_euclid(options.len() as i32) as usize;
                Command::SetOutputDevice(options[next].clone())
            }
            SettingField::CloseBehavior => {
                Command::SetCloseBehavior(match option.unwrap_or(0) {
                    1 => CloseBehavior::Exit,
                    2 => CloseBehavior::Ask,
                    _ => CloseBehavior::Tray,
                })
            }
            SettingField::StartMinimized => {
                Command::SetStartMinimized(!self.core.settings.window.start_minimized)
            }
            SettingField::RememberPosition => {
                Command::SetRememberPosition(!self.core.settings.window.remember_position)
            }
            SettingField::CaptureProtection => {
                Command::SetCaptureProtection(!self.core.settings.window.capture_protection)
            }
            SettingField::LyricsCaptureProtection => Command::SetLyricsCaptureProtection(
                !self.core.settings.window.lyrics_capture_protection,
            ),
            SettingField::DesktopLyricsCaptureProtection => {
                Command::DesktopLyricsSetCaptureProtection(
                    !self.core.settings.desktop_lyrics.capture_protection,
                )
            }
            SettingField::AllowMultipleInstances => Command::SetAllowMultipleInstances(
                !self.core.settings.window.allow_multiple_instances,
            ),
            SettingField::StartWithWindows => Command::SetStartWithWindows(
                !self.core.settings.features.start_with_windows,
            ),
            SettingField::FileAssociations => Command::SetFileAssociations(
                !self.core.settings.features.file_associations,
            ),
            SettingField::PlaylistAssociations => Command::SetPlaylistAssociations(
                !self.core.settings.features.playlist_associations,
            ),
            SettingField::MediaControls => {
                Command::SetMediaControls(!self.core.settings.features.media_controls)
            }
            SettingField::Notifications => {
                Command::SetNotifications(!self.core.settings.features.notifications)
            }
            SettingField::JumpList => {
                Command::SetJumpList(!self.core.settings.features.jump_list)
            }
            SettingField::ShortcutsEnabled => {
                Command::SetShortcutsEnabled(!self.core.settings.shortcuts.enabled)
            }
            SettingField::GamepadEnabled => {
                Command::SetGamepadEnabled(!self.core.settings.shortcuts.gamepad)
            }
            SettingField::Shortcut(slot) => {
                // Click again (or click another row) to leave capture mode.
                self.shortcut_capture = if self.shortcut_capture == Some(slot) {
                    None
                } else {
                    Some(slot)
                };
                self.invalidate(hwnd, self.layout.content);
                return;
            }
            SettingField::ResetShortcuts => Command::ResetShortcuts,
            SettingField::LyricsKaraoke => {
                Command::SetLyricsKaraoke(!self.core.settings.lyrics.karaoke)
            }
            SettingField::LyricsWordsOnly => {
                Command::SetLyricsWordsOnly(!self.core.settings.lyrics.karaoke_words_only)
            }
            SettingField::LyricsTranslation => {
                Command::SetLyricsTranslation(!self.core.settings.lyrics.show_translation)
            }
            SettingField::LyricsFontSize => Command::SetLyricsFontSize(
                self.core.settings.lyrics.font_size + delta as f32,
            ),
            SettingField::LyricsLineSpacing => Command::SetLyricsLineSpacing(
                self.core.settings.lyrics.line_spacing + delta as f32 * 2.0,
            ),
            SettingField::LyricsAlign => {
                Command::SetLyricsAlign(match option.unwrap_or(1) {
                    0 => LyricsAlign::Left,
                    2 => LyricsAlign::Right,
                    _ => LyricsAlign::Center,
                })
            }
            SettingField::LyricsFontFamily => {
                self.lyrics_font_pick_requested = true;
                return;
            }
            SettingField::LyricsFontWeight => {
                let index = weight_index(self.core.settings.lyrics.font_weight);
                let next = (index as i32 + delta).rem_euclid(WEIGHT_VALUES.len() as i32);
                Command::SetLyricsFontWeight(WEIGHT_VALUES[next as usize])
            }
            SettingField::LyricsItalic => {
                Command::SetLyricsItalic(!self.core.settings.lyrics.italic)
            }
            SettingField::LyricsEncoding => {
                use crate::config::LyricsEncoding as E;
                Command::SetLyricsEncoding(match option.unwrap_or(0) {
                    1 => E::Utf8,
                    2 => E::Gbk,
                    3 => E::Gb18030,
                    4 => E::Utf16Le,
                    5 => E::Utf16Be,
                    6 => E::Big5,
                    _ => E::Auto,
                })
            }
            SettingField::LyricsOffset => {
                let step = self.core.settings.lyrics.offset_step_ms.max(50);
                Command::SetLyricsOffset(
                    self.core.settings.lyrics.offset_ms + delta as i64 * step,
                )
            }
            SettingField::LyricsLanguage => {
                let index = option.unwrap_or(0).min(LANGUAGE_VALUES.len() - 1);
                Command::SetLyricsTranslationLanguage(
                    LANGUAGE_VALUES[index].to_string(),
                )
            }
            SettingField::LyricsPrefetch => {
                Command::SetLyricsPrefetch(!self.core.settings.lyrics.prefetch_next)
            }
            SettingField::LyricsShowSource => {
                Command::SetLyricsShowSource(!self.core.settings.lyrics.show_source)
            }
            SettingField::LyricsPanelColor => {
                self.color_request = Some(SettingField::LyricsPanelColor);
                return;
            }
            SettingField::LyricsPanelBackground => Command::SetLyricsPanelBackground(
                match option.unwrap_or(0) {
                    1 => LyricsPanelBackground::Solid,
                    2 => LyricsPanelBackground::Gradient,
                    3 => LyricsPanelBackground::Image,
                    _ => LyricsPanelBackground::Theme,
                },
            ),
            SettingField::LyricsPanelColor2 => {
                self.color_request = Some(SettingField::LyricsPanelColor2);
                return;
            }
            SettingField::LyricsPanelBorderColor => {
                self.color_request = Some(SettingField::LyricsPanelBorderColor);
                return;
            }
            SettingField::LyricsPanelImage => {
                self.lyrics_panel_image_pick_requested = true;
                return;
            }
            SettingField::LyricsPanelReset => Command::ResetLyricsPanelBackground,
            SettingField::LyricsOutlineWidth => Command::SetLyricsOutlineWidth(
                self.core.settings.lyrics.outline_width + delta as f32 * 0.5,
            ),
            SettingField::LyricsOutlineColor
            | SettingField::LyricsShadowColor
            | SettingField::LyricsTransOutlineColor
            | SettingField::LyricsTransShadowColor
            | SettingField::LargeOutlineColor
            | SettingField::LargeShadowColor
            | SettingField::LargeTransOutlineColor
            | SettingField::LargeTransShadowColor
            | SettingField::DesktopTransOutlineColor
            | SettingField::DesktopTransShadowColor
            | SettingField::DesktopFooterOutlineColor
            | SettingField::DesktopFooterShadowColor => {
                self.color_request = Some(field);
                return;
            }
            SettingField::LyricsShadowOffset => Command::SetLyricsShadowOffset(
                self.core.settings.lyrics.shadow_offset + delta as f32 * 0.5,
            ),
            SettingField::LyricsTransOutlineWidth => Command::SetLyricsTranslationOutlineWidth(
                self.core.settings.lyrics.translation_outline_width + delta as f32 * 0.5,
            ),
            SettingField::LyricsTransShadowOffset => Command::SetLyricsTranslationShadowOffset(
                self.core.settings.lyrics.translation_shadow_offset + delta as f32 * 0.5,
            ),
            SettingField::LargeFontFamily => {
                self.large_font_pick_requested = true;
                return;
            }
            SettingField::LargeFontWeight => {
                let index = weight_index(self.core.settings.large_lyrics.font_weight);
                let next = (index as i32 + delta).rem_euclid(WEIGHT_VALUES.len() as i32);
                Command::SetLargeLyricsFontWeight(WEIGHT_VALUES[next as usize])
            }
            SettingField::LargeItalic => {
                Command::SetLargeLyricsItalic(!self.core.settings.large_lyrics.italic)
            }
            SettingField::LargeOutlineWidth => Command::SetLargeLyricsOutlineWidth(
                self.core.settings.large_lyrics.outline_width + delta as f32 * 0.5,
            ),
            SettingField::LargeShadowOffset => Command::SetLargeLyricsShadowOffset(
                self.core.settings.large_lyrics.shadow_offset + delta as f32 * 0.5,
            ),
            SettingField::LargeTransOutlineWidth => {
                Command::SetLargeLyricsTranslationOutlineWidth(
                    self.core.settings.large_lyrics.translation_outline_width
                        + delta as f32 * 0.5,
                )
            }
            SettingField::LargeTransShadowOffset => {
                Command::SetLargeLyricsTranslationShadowOffset(
                    self.core.settings.large_lyrics.translation_shadow_offset
                        + delta as f32 * 0.5,
                )
            }
            SettingField::LargeLyricsFontSize => Command::SetLargeLyricsFontSize(
                self.core.settings.large_lyrics.font_size + delta as f32,
            ),
            SettingField::LyricsHighlightColor
            | SettingField::LyricsCurrentColor
            | SettingField::LyricsNormalColor
            | SettingField::LyricsTranslationColor
            | SettingField::LargeHighlightColor
            | SettingField::LargeCurrentColor
            | SettingField::LargeNormalColor
            | SettingField::LargeTranslationColor
            | SettingField::DesktopCurrentColor
            | SettingField::DesktopNextColor
            | SettingField::DesktopPrevColor
            | SettingField::DesktopFooterColor
            | SettingField::DesktopTranslationColor
            | SettingField::DesktopOutlineColor
            | SettingField::DesktopShadowColor
            | SettingField::DesktopBgColor
            | SettingField::DesktopBgColor2
            | SettingField::MiniPanelColor
            | SettingField::MiniTextColor
            | SettingField::MiniAccentColor => {
                // The shell opens the native picker after this borrow ends.
                self.color_request = Some(field);
                return;
            }
            SettingField::DesktopBackgroundImage => {
                self.desktop_bg_pick_requested = true;
                return;
            }
            SettingField::DesktopClearBackgroundImage => {
                Command::SetDesktopLyricsBackgroundImage(None)
            }
            SettingField::DesktopFontFamily => {
                self.desktop_font_pick_requested = true;
                return;
            }
            SettingField::DesktopTransFontFamily => {
                self.desktop_trans_font_pick_requested = true;
                return;
            }
            SettingField::DesktopFooterFontFamily => {
                self.desktop_footer_font_pick_requested = true;
                return;
            }
            SettingField::DesktopTransBold => Command::SetDesktopLyricsTranslationBold(
                !self.core.settings.desktop_lyrics.translation_bold,
            ),
            SettingField::DesktopFooterBold => Command::SetDesktopLyricsFooterBold(
                !self.core.settings.desktop_lyrics.footer_bold,
            ),
            SettingField::DesktopOutlineWidth => Command::SetDesktopLyricsOutlineWidth(
                self.core.settings.desktop_lyrics.outline_width + delta as f32,
            ),
            SettingField::DesktopShadowOffset => Command::SetDesktopLyricsShadowOffset(
                self.core.settings.desktop_lyrics.shadow_offset + delta as f32,
            ),
            SettingField::DesktopItalic => Command::SetDesktopLyricsItalic(
                !self.core.settings.desktop_lyrics.italic,
            ),
            SettingField::DesktopTransOutlineWidth => {
                Command::SetDesktopLyricsTranslationOutlineWidth(
                    self.core.settings.desktop_lyrics.translation_outline_width
                        + delta as f32 * 0.5,
                )
            }
            SettingField::DesktopTransShadowOffset => {
                Command::SetDesktopLyricsTranslationShadowOffset(
                    self.core.settings.desktop_lyrics.translation_shadow_offset
                        + delta as f32 * 0.5,
                )
            }
            SettingField::DesktopFooterOutlineWidth => {
                Command::SetDesktopLyricsFooterOutlineWidth(
                    self.core.settings.desktop_lyrics.footer_outline_width
                        + delta as f32 * 0.5,
                )
            }
            SettingField::DesktopFooterShadowOffset => {
                Command::SetDesktopLyricsFooterShadowOffset(
                    self.core.settings.desktop_lyrics.footer_shadow_offset
                        + delta as f32 * 0.5,
                )
            }
            SettingField::DesktopBlurRadius => Command::SetDesktopLyricsBlurRadius(
                self.core.settings.desktop_lyrics.blur_radius + delta as f32 * 2.0,
            ),
            SettingField::DesktopTranslationOutline => Command::SetDesktopLyricsTranslationOutline(
                !self.core.settings.desktop_lyrics.translation_outline,
            ),
            SettingField::DesktopTranslationShadow => Command::SetDesktopLyricsTranslationShadow(
                !self.core.settings.desktop_lyrics.translation_shadow,
            ),
            SettingField::DesktopFooterOutline => Command::SetDesktopLyricsFooterOutline(
                !self.core.settings.desktop_lyrics.footer_outline,
            ),
            SettingField::DesktopFooterShadow => Command::SetDesktopLyricsFooterShadow(
                !self.core.settings.desktop_lyrics.footer_shadow,
            ),
            SettingField::DesktopHideWhenFullscreen => Command::SetDesktopLyricsHideWhenFullscreen(
                !self.core.settings.desktop_lyrics.hide_when_fullscreen,
            ),
            SettingField::LargeLineSpacing => Command::SetLargeLyricsLineSpacing(
                self.core.settings.large_lyrics.line_spacing + delta as f32 * 2.0,
            ),
            SettingField::LargeAlign => Command::SetLargeLyricsAlign(match option.unwrap_or(1) {
                0 => LyricsAlign::Left,
                2 => LyricsAlign::Right,
                _ => LyricsAlign::Center,
            }),
            SettingField::LargeTranslation => Command::SetLargeLyricsTranslation(
                !self.core.settings.large_lyrics.show_translation,
            ),
            SettingField::LargeKaraoke => Command::SetLargeLyricsKaraoke(
                !self.core.settings.large_lyrics.karaoke,
            ),
            SettingField::LargeWordsOnly => Command::SetLargeLyricsWordsOnly(
                !self.core.settings.large_lyrics.karaoke_words_only,
            ),
            SettingField::LargeShowCover => Command::SetLargeLyricsShowCover(
                !self.core.settings.large_lyrics.show_cover,
            ),
            SettingField::LargeShowTitle => Command::SetLargeLyricsShowTitle(
                !self.core.settings.large_lyrics.show_title,
            ),
            SettingField::LargeShowFooter => Command::SetLargeLyricsShowFooter(
                !self.core.settings.large_lyrics.show_footer,
            ),
            SettingField::DesktopShow => Command::ToggleDesktopLyrics,
            SettingField::DesktopLock => {
                Command::DesktopLyricsSetLock(!self.core.settings.desktop_lyrics.locked)
            }
            SettingField::DesktopClickThrough => Command::DesktopLyricsSetClickThrough(
                !self.core.settings.desktop_lyrics.click_through,
            ),
            SettingField::DesktopAlwaysOnTop => Command::DesktopLyricsSetAlwaysOnTop(
                !self.core.settings.desktop_lyrics.always_on_top,
            ),
            SettingField::DesktopLines => {
                Command::DesktopLyricsSetLines(option.unwrap_or(0) as u8 + 1)
            }
            SettingField::DesktopTranslation => Command::DesktopLyricsSetTranslation(
                !self.core.settings.desktop_lyrics.show_translation,
            ),
            SettingField::DesktopFooter => Command::DesktopLyricsSetFooter(
                !self.core.settings.desktop_lyrics.show_footer,
            ),
            SettingField::DesktopFontSize => Command::DesktopLyricsSetFontSize(
                self.core.settings.desktop_lyrics.font_size + delta as f32,
            ),
            SettingField::DesktopNextFontSize => Command::DesktopLyricsSetNextFontSize(
                self.core.settings.desktop_lyrics.next_font_size + delta as f32,
            ),
            SettingField::DesktopPrevFontSize => Command::DesktopLyricsSetPrevFontSize(
                self.core.settings.desktop_lyrics.prev_font_size + delta as f32,
            ),
            SettingField::DesktopFooterFontSize => Command::DesktopLyricsSetFooterFontSize(
                self.core.settings.desktop_lyrics.footer_font_size + delta as f32,
            ),
            SettingField::DesktopFrameRate => Command::DesktopLyricsSetFrameRate(
                (self.core.settings.desktop_lyrics.frame_rate as i32 + delta * 5)
                    .clamp(15, 120) as u32,
            ),
            SettingField::DesktopBold => {
                Command::DesktopLyricsSetBold(!self.core.settings.desktop_lyrics.bold)
            }
            SettingField::DesktopAlign => Command::DesktopLyricsSetAlign(option.unwrap_or(1) as u8),
            SettingField::DesktopBackground => {
                let current = desktop_background_index(
                    self.core.settings.desktop_lyrics.background,
                );
                let next = (current + delta).rem_euclid(DESKTOP_BG_OPTIONS.len() as i32);
                Command::DesktopLyricsSetBackground(next as u8)
            }
            SettingField::DesktopOpacity => Command::DesktopLyricsSetOpacity(
                self.core.settings.desktop_lyrics.opacity + delta as f32 * 0.05,
            ),
            SettingField::DesktopRadius => Command::DesktopLyricsSetRadius(
                self.core.settings.desktop_lyrics.radius as i32 + delta * 2,
            ),
            SettingField::DesktopPadding => Command::DesktopLyricsSetPadding(
                self.core.settings.desktop_lyrics.padding + delta as f32 * 4.0,
            ),
            SettingField::DesktopLineSpacing => Command::DesktopLyricsSetLineSpacing(
                self.core.settings.desktop_lyrics.line_spacing + delta as f32 * 2.0,
            ),
            SettingField::DesktopShadow => Command::DesktopLyricsSetShadow(
                !self.core.settings.desktop_lyrics.shadow,
            ),
            SettingField::DesktopOutline => Command::DesktopLyricsSetOutline(
                !self.core.settings.desktop_lyrics.outline,
            ),
            SettingField::DesktopBorder => Command::DesktopLyricsSetBorder(
                !self.core.settings.desktop_lyrics.border,
            ),
            SettingField::DesktopWindowShadow => Command::DesktopLyricsSetWindowShadow(
                !self.core.settings.desktop_lyrics.window_shadow,
            ),
            SettingField::DesktopAutoSize => Command::DesktopLyricsSetAutoSize(
                !self.core.settings.desktop_lyrics.auto_size,
            ),
            SettingField::DesktopWidth => {
                let settings = &self.core.settings.desktop_lyrics;
                Command::DesktopLyricsSetSize {
                    width: settings.width + delta as f32 * 40.0,
                    height: settings.height,
                }
            }
            SettingField::DesktopHeight => {
                let settings = &self.core.settings.desktop_lyrics;
                Command::DesktopLyricsSetSize {
                    width: settings.width,
                    height: settings.height + delta as f32 * 20.0,
                }
            }
            SettingField::DesktopHideWhenPaused => Command::DesktopLyricsSetHideWhenPaused(
                !self.core.settings.desktop_lyrics.hide_when_paused,
            ),
            SettingField::DesktopCaptureProtection => {
                Command::DesktopLyricsSetCaptureProtection(
                    !self.core.settings.desktop_lyrics.capture_protection,
                )
            }
            SettingField::DesktopKaraoke => Command::DesktopLyricsSetKaraoke(
                !self.core.settings.desktop_lyrics.karaoke,
            ),
            SettingField::DesktopWordsOnly => Command::DesktopLyricsSetWordsOnly(
                !self.core.settings.desktop_lyrics.karaoke_words_only,
            ),
            SettingField::DesktopResetPosition => Command::DesktopLyricsResetPosition,
            SettingField::MiniShowLyrics => {
                Command::SetMiniShowLyrics(!self.core.settings.mini.show_lyrics)
            }
            SettingField::MiniKaraoke => {
                Command::SetMiniKaraoke(!self.core.settings.mini.karaoke)
            }
            SettingField::MiniWordsOnly => {
                Command::SetMiniWordsOnly(!self.core.settings.mini.karaoke_words_only)
            }
            SettingField::MiniLines => {
                Command::SetMiniLyricsLines(option.unwrap_or(0) as u8 + 1)
            }
            SettingField::MiniBold => {
                Command::SetMiniLyricsBold(!self.core.settings.mini.lyrics_bold)
            }
            SettingField::MiniTranslation => {
                Command::SetMiniTranslation(!self.core.settings.mini.show_translation)
            }
            SettingField::MiniFontSize => Command::SetMiniFontSize(
                self.core.settings.mini.lyrics_font_size + delta as f32,
            ),
            SettingField::MiniFrameRate => Command::SetMiniFrameRate(
                (self.core.settings.mini.frame_rate as i32 + delta * 5).clamp(15, 120) as u32,
            ),
            SettingField::MiniAutoHeight => {
                Command::SetMiniAutoHeight(!self.core.settings.mini.auto_height)
            }
            SettingField::MiniHeight => {
                Command::SetMiniHeight(self.core.settings.mini.height + delta as f32 * 8.0)
            }
            SettingField::MiniAlwaysOnTop => {
                Command::SetMiniAlwaysOnTop(!self.core.settings.mini.always_on_top)
            }
            SettingField::MiniOpacity => Command::SetMiniOpacity(
                self.core.settings.mini.opacity + delta as f32 * 0.05,
            ),
            SettingField::MiniWidth => Command::SetMiniWidth(
                self.core.settings.mini.width + delta as f32 * 20.0,
            ),
            SettingField::MiniTaskbar => {
                Command::SetMiniShowInTaskbar(!self.core.settings.mini.show_in_taskbar)
            }
            SettingField::ThemeFontFamily => {
                self.theme_font_pick_requested = true;
                return;
            }
            SettingField::ThemeFontSize => Command::SetThemeFontSize(
                self.core.settings.theme.font_size + delta as f32 * 0.5,
            ),
            SettingField::ThemeMode => Command::SetThemeMode(match option.unwrap_or(0) {
                1 => ThemeMode::Light,
                2 => ThemeMode::System,
                _ => ThemeMode::Dark,
            }),
            SettingField::Accent => {
                let current = accent_index(self.core.settings.theme.accent) as i32;
                let len = crate::theme::ACCENT_PRESETS.len() as i32;
                let next = (current + delta).rem_euclid(len) as usize;
                Command::SetAccent(crate::theme::ACCENT_PRESETS[next].1)
            }
            SettingField::Density => Command::SetListDensity(match option.unwrap_or(1) {
                0 => ListDensity::Compact,
                2 => ListDensity::Spacious,
                _ => ListDensity::Comfortable,
            }),
            SettingField::ReduceMotion => {
                Command::SetReduceMotion(!self.core.settings.theme.reduce_motion)
            }
            SettingField::FrameRate => Command::SetFrameRate(
                (self.core.settings.performance.frame_rate as i32 + delta * 5).max(15) as u32,
            ),
            SettingField::BackgroundMode => {
                use crate::config::BackgroundMode as Mode;
                let current = match self.core.settings.theme.background {
                    Mode::Solid => 0,
                    Mode::Image => 2,
                    _ => 1,
                };
                let next = option
                    .map(|value| value as i32)
                    .unwrap_or(current + delta)
                    .rem_euclid(BACKGROUND_OPTIONS.len() as i32);
                if next == 2 && self.core.settings.theme.background_image.is_none() {
                    self.background_image_pick_requested = true;
                    return;
                }
                Command::SetBackgroundMode(match next {
                    0 => Mode::Solid,
                    1 => Mode::StaticBlur,
                    _ => Mode::Image,
                })
            }
            SettingField::BackgroundImage => {
                self.background_image_pick_requested = true;
                return;
            }
            SettingField::ClearBackgroundImage => Command::SetBackgroundImage(None),
            SettingField::VisualizerEnabled => {
                Command::SetVisualizerEnabled(!self.core.settings.visualizer.enabled)
            }
            SettingField::VisualizerMode => {
                use crate::config::VisualizerMode as Mode;
                Command::SetVisualizerMode(match option.unwrap_or(0) {
                    1 => Mode::Waveform,
                    2 => Mode::Ring,
                    _ => Mode::Bars,
                })
            }
            SettingField::MetadataMode => {
                use crate::config::MetadataMode as Mode;
                Command::SetMetadataMode(match option.unwrap_or(1) {
                    0 => Mode::Off,
                    2 => Mode::All,
                    _ => Mode::CurrentOnly,
                })
            }
            SettingField::LoadCovers => Command::SetLoadCovers(
                !self.core.settings.performance.load_covers,
            ),
            SettingField::LazyAudio => Command::SetLazyAudio(
                !self.core.settings.performance.lazy_audio,
            ),
            SettingField::ReleaseAudioOutput => Command::SetReleaseAudioOutput(
                !self.core.settings.performance.release_audio_output,
            ),
            SettingField::DeferPausedRestore => Command::SetDeferPausedRestore(
                !self.core.settings.performance.defer_paused_restore,
            ),
            SettingField::IdleFrameRate => Command::SetIdleFrameRate(
                (self.core.settings.performance.idle_frame_rate as i32 + delta * 5).max(5) as u32,
            ),
            SettingField::PauseAnimationsUnfocused => Command::SetPauseAnimationsUnfocused(
                !self.core.settings.performance.pause_animations_unfocused,
            ),
            SettingField::ThumbnailCache => {
                let current = self.core.settings.performance.thumbnail_cache as i32;
                // 0 = unlimited; step down from 32 reaches it.
                let next = (current + delta * 32).max(0);
                Command::SetThumbnailCache(next as u32)
            }
            SettingField::D2DHardware => {
                Command::SetD2DHardware(!self.core.settings.performance.d2d_hardware)
            }
            SettingField::TrimWorkingSet => Command::SetTrimWorkingSetOnIdle(
                !self.core.settings.window.trim_working_set_on_idle,
            ),
            SettingField::TrimIdleSecs => Command::SetTrimIdleSecs(
                (self.core.settings.window.trim_idle_secs as i32 + delta * 5).max(5) as u64,
            ),
            SettingField::OptimizeIndex => Command::LibraryOptimize,
            SettingField::ExportSettings => {
                self.settings_action = Some(SettingsAction::Export);
                return;
            }
            SettingField::ImportSettings => {
                self.settings_action = Some(SettingsAction::Import);
                return;
            }
            SettingField::ResetSettings => {
                self.settings_action = Some(SettingsAction::Reset);
                return;
            }
            SettingField::SourceSelect => Command::CyclePrimarySource,
            SettingField::SourceEnabled => Command::SetSourceEnabled(
                !self
                    .core
                    .primary_source()
                    .map(|source| source.enabled)
                    .unwrap_or(false),
            ),
            SettingField::SourceName => {
                self.text_edit_request = Some(index);
                return;
            }
            SettingField::SourceQuality => Command::SetSourceQuality(match option.unwrap_or(1) {
                0 => crate::core::config::Quality::K128,
                2 => crate::core::config::Quality::Flac,
                _ => crate::core::config::Quality::K320,
            }),
            SettingField::SourceHeaders | SettingField::SourceEndpoints => {
                self.text_edit_request = Some(index);
                return;
            }
            SettingField::SourceAdd => Command::AddSource,
            SettingField::SourceRemove => Command::RemovePrimarySource,
            SettingField::SourceBaseUrl | SettingField::ProxyUrl => {
                self.text_edit_request = Some(index);
                return;
            }
            SettingField::DownloadTemplate => {
                self.text_edit_request = Some(index);
                return;
            }
            SettingField::NetworkOffline => {
                Command::SetNetworkOffline(!self.core.settings.network.offline)
            }
            SettingField::ApiMinInterval => Command::SetApiMinInterval(
                (self.core.settings.network.api_min_interval_ms as i64 + delta as i64 * 100)
                    .clamp(0, 3000) as u64,
            ),
            SettingField::AlbumPageSize => Command::SetAlbumPageSize(
                (self.core.settings.network.album_page_size as i64 + delta as i64 * 10)
                    .clamp(10, 200) as u32,
            ),
            SettingField::NetworkTimeout => Command::SetNetworkTimeout(
                (self.core.settings.network.timeout_secs as i64 + delta as i64)
                    .clamp(2, 120) as u64,
            ),
            SettingField::NetworkRetries => Command::SetNetworkRetries(
                (self.core.settings.network.retries as i64 + delta as i64).clamp(0, 10)
                    as u32,
            ),
            SettingField::ProxyMode => Command::SetProxyMode(match option.unwrap_or(0) {
                1 => crate::core::config::ProxyMode::None,
                2 => crate::core::config::ProxyMode::Manual,
                _ => crate::core::config::ProxyMode::System,
            }),
            SettingField::CacheLimitMb => {
                let step = if delta == 0 { 1 } else { delta };
                Command::SetCacheLimitMb(
                    (self.core.settings.network.cache_limit_mb as i64 + step as i64 * 128)
                        .clamp(0, 16 * 1024) as u64,
                )
            }
            SettingField::StreamCacheDir => {
                self.stream_cache_pick_requested = true;
                return;
            }
            SettingField::ClearCache => Command::ClearStreamCache,
            SettingField::OnlineTest => Command::OnlineTest,
            SettingField::DownloadDir => {
                self.download_dir_pick_requested = true;
                return;
            }
            SettingField::DownloadConcurrency => Command::SetDownloadConcurrency(
                (self.core.settings.download.concurrency as i64 + delta as i64).clamp(1, 4)
                    as u8,
            ),
            SettingField::DownloadConflict => Command::SetDownloadConflict(
                match option.unwrap_or(0) {
                    1 => crate::core::config::ConflictPolicy::Skip,
                    2 => crate::core::config::ConflictPolicy::Overwrite,
                    _ => crate::core::config::ConflictPolicy::Rename,
                },
            ),
            SettingField::DownloadQuality => Command::SetDownloadQuality(match option.unwrap_or(0)
            {
                1 => Some(crate::core::config::Quality::K320),
                2 => Some(crate::core::config::Quality::Flac),
                3 => Some(crate::core::config::Quality::K128),
                _ => None,
            }),
            SettingField::DownloadEmbedTags => {
                Command::SetDownloadEmbedTags(!self.core.settings.download.embed_tags)
            }
            SettingField::DownloadEmbedCover => {
                Command::SetDownloadEmbedCover(!self.core.settings.download.embed_cover)
            }
            SettingField::DownloadSaveLyrics => {
                Command::SetDownloadSaveLyrics(!self.core.settings.download.save_lyrics)
            }
            SettingField::DownloadAddToLibrary => Command::SetDownloadAddToLibrary(
                !self.core.settings.download.add_to_library,
            ),
            SettingField::ClearFinishedDownloads => Command::ClearFinishedDownloads,
        };
        self.core.handle(command);
        if field == SettingField::Density {
            // Row height changed: keep the first row at the top and re-query
            // the library page with the new number of visible rows.
            self.scroll = 0.0;
            self.request_library_page();
        }
        if matches!(
            field,
            SettingField::VisualizerEnabled
                | SettingField::VisualizerMode
                | SettingField::BackgroundMode
                | SettingField::ReduceMotion
                | SettingField::Density
        ) {
            self.relayout();
        }
        // Paused windows have no timer, so persist discrete setting changes
        // immediately instead of waiting for an exit save.
        self.core.flush_settings();
        // A settings toggle can change layout/active colors and shell state;
        // one page repaint is cheap and avoids per-row bookkeeping.
        if field == SettingField::BackgroundMode {
            // The blurred backdrop depends on the cover and must replace every
            // previously painted pixel, including parts currently covered by
            // another window.
            let full = Rect::new(0.0, 0.0, self.layout.size.0, self.layout.size.1);
            self.invalidate_force(hwnd, full);
        } else {
            self.invalidate_all(hwnd);
        }
    }

    /// A settings row is waiting for the next key combination.
    pub fn shortcut_capture_active(&self) -> bool {
        self.shortcut_capture.is_some()
    }

    /// Finish key capture. `combo` is the human-readable combination built
    /// from the pressed keys; `None` cancels (Esc). Duplicate combinations are
    /// rejected so two actions never fight over one global hotkey.
    pub fn commit_shortcut_capture(&mut self, hwnd: HWND, combo: Option<String>) {
        let Some(slot) = self.shortcut_capture.take() else {
            return;
        };
        self.invalidate(hwnd, self.layout.content);
        let Some(combo) = combo else {
            // Esc clears the binding instead of leaving the old combination.
            self.core.handle(Command::SetShortcut(slot, String::new()));
            self.core.flush_settings();
            return;
        };
        let Some(candidate) = crate::shortcuts::parse(&combo) else {
            self.push_toast(hwnd, crate::i18n::tr("快捷键需要至少一个修饰键").to_string(), true);
            return;
        };
        let conflict = crate::config::ShortcutSlot::ALL
            .into_iter()
            .filter(|other| *other != slot)
            .find(|other| {
                crate::shortcuts::parse(self.core.settings.shortcuts.get(*other))
                    == Some(candidate)
            });
        if let Some(other) = conflict {
            self.push_toast(
                hwnd,
                crate::trf!("与“{}”快捷键冲突", other.label()),
                true,
            );
            return;
        }
        self.core.handle(Command::SetShortcut(slot, combo));
        self.core.flush_settings();
    }

    pub fn settings_folder_rows(&self) -> usize {
        let settings = layout::settings(&self.layout);
        (settings.folders.height() / layout::FOLDER_ROW_HEIGHT)
            .floor()
            .max(0.0) as usize
    }

    /// Largest scroll offset for the generic setting rows. Short tabs return
    /// zero, so the wheel does nothing and rows stay put.
    pub fn settings_max_scroll(&self) -> f32 {
        let settings = layout::settings(&self.layout);
        let rows = self.settings_tab.fields().len() as f32 * layout::SETTINGS_ROW_HEIGHT;
        (rows + 8.0 - settings.generic.height()).max(0.0)
    }

    /// On narrow windows segmented choices are replaced by one cycling button.
    pub fn settings_compact_choice(&self) -> bool {
        layout::settings(&self.layout).label_width < 210.0
    }

    fn clamp_settings_scroll(&mut self) {
        let folders = self.core.snapshot().library_folders.len();
        let max = folders.saturating_sub(self.settings_folder_rows());
        self.settings_folders_scroll = self.settings_folders_scroll.min(max);
    }

    pub fn queue_drag_indicator(&self) -> Option<Rect> {
        if self.page != Page::Queue {
            return None;
        }
        if self.queue_drag_active {
            let target = self.queue_drag_target?;
            let row = self.row_rect(target);
            return Some(Rect::new(
                row.left + 4.0,
                row.top - 1.0,
                row.right - 4.0,
                row.top + 1.5,
            ));
        }
        if let Some(index) = self.external_drag_index {
            let count = self.row_count();
            let y = if count == 0 {
                self.list_area().top
            } else if index < count {
                self.row_rect(index).top
            } else {
                self.row_rect(count - 1).bottom
            };
            let area = self.list_area();
            return Some(Rect::new(area.left + 4.0, y - 1.0, area.right - 4.0, y + 1.5));
        }
        None
    }

    /// Begin an OLE/external drag over the window. The queue preview is only
    /// shown while the cursor is over the queue list or its nav item.
    pub fn external_drag_begin(&mut self, hwnd: HWND, count: usize) {
        self.external_drag_active = true;
        self.external_drag_count = count;
        self.external_drag_index = None;
        self.external_drag_over_queue = false;
        self.invalidate(hwnd, self.layout.header);
        self.invalidate(hwnd, self.nav_rect(1).inflate(2.0));
        self.invalidate(hwnd, self.list_area());
    }

    pub fn external_drag_move(&mut self, hwnd: HWND, x: f32, y: f32) {
        if !self.external_drag_active {
            return;
        }
        let mut over_queue = false;
        let mut index = None;
        if self.page == Page::Queue && self.list_area().contains(x, y) {
            over_queue = true;
            index = Some(self.row_at(y).min(self.row_count()));
        } else if self.nav_rect(1).inflate(2.0).contains(x, y) {
            over_queue = true;
        }
        if over_queue != self.external_drag_over_queue
            || index != self.external_drag_index
        {
            self.external_drag_over_queue = over_queue;
            self.external_drag_index = index;
            self.invalidate(hwnd, self.layout.header);
            self.invalidate(hwnd, self.nav_rect(1).inflate(2.0));
            self.invalidate(hwnd, self.list_area());
            self.invalidate(hwnd, self.scrollbar_rect());
        }
    }

    pub fn external_drag_leave(&mut self, hwnd: HWND) {
        if !self.external_drag_active {
            return;
        }
        self.external_drag_active = false;
        self.external_drag_count = 0;
        self.external_drag_index = None;
        self.external_drag_over_queue = false;
        self.invalidate(hwnd, self.layout.header);
        self.invalidate(hwnd, self.nav_rect(1).inflate(2.0));
        self.invalidate(hwnd, self.list_area());
        self.invalidate(hwnd, self.scrollbar_rect());
    }

    /// Finish an external drag. Returns the real queue insertion index when
    /// the drop landed on the queue list; `None` means append/AddPaths.
    pub fn external_drag_take_index(&mut self, hwnd: HWND) -> Option<usize> {
        let over_queue = self.external_drag_over_queue;
        let display = self.external_drag_index.take();
        self.external_drag_active = false;
        self.external_drag_count = 0;
        self.external_drag_over_queue = false;
        self.invalidate(hwnd, self.layout.header);
        self.invalidate(hwnd, self.nav_rect(1).inflate(2.0));
        self.invalidate(hwnd, self.list_area());
        self.invalidate(hwnd, self.scrollbar_rect());
        if !over_queue || self.page != Page::Queue {
            return None;
        }
        let Some(display) = display else {
            return Some(self.core.snapshot().queue.len());
        };
        if display >= self.queue_view.len() {
            Some(self.core.snapshot().queue.len())
        } else {
            self.queue_view.get(display).copied()
        }
    }

    pub fn external_drag_highlight(&self) -> bool {
        self.external_drag_active && self.external_drag_over_queue
    }

    pub fn external_drag_hint(&self) -> Option<String> {
        if !self.external_drag_active {
            return None;
        }
        let count = self.external_drag_count;
        if self.external_drag_over_queue {
            if let Some(index) = self.external_drag_index {
                return Some(format!(
                    "松开插入到第 {} 位 · {} 个文件",
                    index + 1,
                    count
                ));
            }
            return Some(format!("松开追加到播放队列末尾 · {} 个文件", count));
        }
        Some(format!("松开加入播放队列 · {} 个文件", count))
    }

    pub fn is_folder_selected(&self, index: usize) -> bool {
        self.folder_selected.contains(&index)
    }

    pub fn folder_selection_len(&self) -> usize {
        self.folder_selected.len()
    }

    fn folder_selected_paths(&self) -> Vec<std::path::PathBuf> {
        let snapshot = self.core.snapshot();
        let mut indexes: Vec<usize> = self.folder_selected.iter().copied().collect();
        indexes.sort_unstable();
        indexes
            .into_iter()
            .filter_map(|index| snapshot.browser.entries.get(index))
            .filter(|entry| !entry.is_dir && !entry.is_playlist)
            .map(|entry| entry.path.clone())
            .collect()
    }

    pub fn folder_can_go_back(&self) -> bool {
        !self.core.snapshot().browser.back.is_empty()
    }

    pub fn folder_can_go_forward(&self) -> bool {
        !self.core.snapshot().browser.forward.is_empty()
    }

    /// Text shown in the folder address bar; empty for the drive list.
    pub fn folder_address_text(&self) -> String {
        self.core
            .snapshot()
            .browser
            .current
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_default()
    }

    pub fn reset_folder_view(&mut self, hwnd: HWND) {
        self.scroll = 0.0;
        self.folder_selected.clear();
        self.folder_anchor = None;
        self.ui.set_hover(None);
        self.invalidate(hwnd, self.layout.header);
        self.invalidate(hwnd, self.layout.folder_list);
        self.invalidate(hwnd, self.layout.folder_scrollbar);
    }

    /// Header subtitle for the folder page: current path, counts and errors.
    pub fn folder_status(&self) -> String {
        if self.folder_drag {
            return if self.folder_drop_over_queue {
                "松开即可把选中文件加入播放队列".to_string()
            } else {
                "拖到左侧“播放队列”加入队列".to_string()
            };
        }
        let browser = &self.core.snapshot().browser;
        if browser.loading {
            return "正在读取目录…".to_string();
        }
        if let Some(error) = &browser.error {
            return format!("无法读取目录：{error}");
        }
        let path = browser
            .current
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "此电脑".to_string());
        let dirs = browser.entries.iter().filter(|entry| entry.is_dir).count();
        let files = browser.entries.len().saturating_sub(dirs);
        let truncated = if browser.truncated {
            " · 条目过多，仅显示前 20000 项"
        } else {
            ""
        };
        let selected = self.folder_selection_len();
        let selection = if selected > 0 {
            format!(" · 已选 {selected} 项")
        } else {
            String::new()
        };
        format!("{path} · {dirs} 个文件夹 · {files} 个文件{truncated}{selection}")
    }

    pub fn library_status(&self) -> String {        let snapshot = self.core.snapshot();
        if snapshot.library_index_active {
            let current = snapshot.library_index_current;
            let current = if current.is_empty() {
                String::new()
            } else {
                format!(
                    " · {}",
                    crate::core::util::truncate_chars(
                        std::path::Path::new(current)
                            .file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .unwrap_or_else(|| current.to_string())
                            .as_str(),
                        28
                    )
                )
            };
            return format!("索引中 {} 首{}", snapshot.library_index_progress, current);
        }
        if snapshot.library_loading {
            return "查询中…".to_string();
        }
        if snapshot.library_folders.is_empty() {
            return "未添加音乐目录 · 点击“添加目录”开始建立曲库".to_string();
        }
        let mut status = format!(
            "{} 首 · 每页 {} 首",
            snapshot.library_total,
            self.library_page_size()
        );
        if let Some(missing) = snapshot.library_missing_checked {
            status.push_str(&format!(" · 缺失文件：{missing} 首"));
        }
        status
    }

    pub fn smart_status(&self) -> String {
        let snapshot = self.core.snapshot();
        format!(
            "{} · {} 首 · 点击列表项播放",
            snapshot.library_smart_kind.label(),
            snapshot.library_smart.len()
        )
    }

    pub fn duplicate_status(&self) -> String {
        let snapshot = self.core.snapshot();
        let mode = match snapshot.library_duplicate_mode {
            crate::config::DedupeMode::Metadata => "元数据",
            crate::config::DedupeMode::Path => "文件路径",
            crate::config::DedupeMode::Fingerprint => "音频指纹",
        };
        format!(
            "查重方式：{} · {} 组 · {} 个文件",
            mode,
            snapshot.library_duplicates.len(),
            snapshot
                .library_duplicates
                .iter()
                .map(|group| group.entries.len())
                .sum::<usize>()
        )
    }

    /// Rows per page on the online search page (same rule as the library).
    pub fn online_page_size(&self) -> usize {
        (self.layout.online_list.height() / self.row_height())
            .floor()
            .max(1.0) as usize
    }

    pub fn online_page(&self) -> usize {
        let snapshot = self.core.snapshot();
        match snapshot.online.detail.as_ref() {
            Some(detail) => detail.page as usize,
            None => (snapshot.online.offset / snapshot.online.limit.max(1)) as usize,
        }
    }

    /// Whether the last page was full; `total` from the API is a rendering
    /// hint and is not used for pagination.
    pub fn online_has_more(&self) -> bool {
        let snapshot = self.core.snapshot();
        match snapshot.online.detail.as_ref() {
            Some(detail) => detail.has_more,
            None => snapshot.online.has_more,
        }
    }

    pub fn turn_online_page(&mut self, hwnd: HWND, delta: i32) {
        self.core.handle(Command::OnlinePage(delta));
        self.scroll = 0.0;
        self.ui.set_hover(None);
        self.invalidate(hwnd, self.layout.header);
        self.invalidate(hwnd, self.layout.online_list);
        self.invalidate(hwnd, self.layout.online_scrollbar);
        self.invalidate(hwnd, self.layout.library_pager);
    }

    pub fn online_can_queue_all(&self) -> bool {
        let snapshot = self.core.snapshot();
        !(snapshot.online.kind == crate::core::api::SearchKind::Album
            && !snapshot.online.is_detail())
            && self.row_count() > 0
    }

    pub fn online_query_text(&self) -> String {
        self.core.snapshot().online.query.clone()
    }

    pub fn online_header_title(&self) -> String {
        let snapshot = self.core.snapshot();
        match snapshot.online.detail.as_ref() {
            Some(detail) if !detail.title.trim().is_empty() => detail.title.clone(),
            _ if snapshot.online.kind == crate::core::api::SearchKind::Album => {
                "专辑搜索".to_string()
            }
            _ => "在线搜索".to_string(),
        }
    }

    pub fn online_kind_label(&self) -> &'static str {
        match self.core.snapshot().online.kind {
            crate::core::api::SearchKind::Song => "歌曲",
            crate::core::api::SearchKind::Album => "专辑",
        }
    }

    pub fn online_is_detail(&self) -> bool {
        self.core.snapshot().online.is_detail()
    }

    pub fn online_status(&self) -> String {
        let snapshot = self.core.snapshot();
        let online = snapshot.online;
        if let Some(detail) = online.detail.as_ref() {
            let mut status = detail.title.clone();
            if !detail.subtitle.trim().is_empty() && detail.subtitle != detail.title {
                if !status.is_empty() {
                    status.push_str(" · ");
                }
                status.push_str(&detail.subtitle);
            }
            if detail.loading {
                status.push_str(" · 加载中…");
            } else if let Some(error) = detail.error.as_deref() {
                status.push_str(" · ");
                status.push_str(error);
            } else {
                status.push_str(&format!(
                    " · 第 {} 页 · 本页 {} 首",
                    detail.page + 1,
                    detail.items.len()
                ));
            }
            return status;
        }
        if online.loading() {
            return if online.from_cache {
                "搜索中… · 先显示缓存结果".to_string()
            } else {
                "搜索中…".to_string()
            };
        }
        if let Some(error) = online.error() {
            return error.to_string();
        }
        if online.query.trim().is_empty() {
            return "输入关键词搜索在线曲库，或直接粘贴 http(s) 链接后回车".to_string();
        }
        let unit = if online.kind == crate::core::api::SearchKind::Album {
            "张专辑"
        } else {
            "首"
        };
        let mut status = format!(
            "第 {} 页 · 本页 {} {}",
            self.online_page() + 1,
            online.songs.len(),
            unit
        );
        if !online.suggestions.is_empty() {
            let hints: Vec<&str> = online
                .suggestions
                .iter()
                .take(3)
                .map(String::as_str)
                .collect();
            status.push_str(" · 相关：");
            status.push_str(&hints.join(" / "));
        }
        if online.from_cache {
            status = format!("缓存结果 · {status}");
        }
        status
    }

    pub fn discover_status(&self) -> String {
        let discover = &self.core.snapshot().discover;
        if let Some(error) = discover.error.as_deref() {
            return error.to_string();
        }
        if discover.loading {
            return "正在随机推荐…".to_string();
        }
        format!(
            "{} 首随机推荐 · 点击播放，右键可入队/下载 · 点“换一批”刷新",
            discover.songs.len()
        )
    }

    pub fn downloads_status(&self) -> String {        let snapshot = self.core.snapshot();
        let active = snapshot
            .downloads
            .iter()
            .filter(|row| row.state.is_active())
            .count();
        let failed = snapshot
            .downloads
            .iter()
            .filter(|row| matches!(row.state, crate::core::download::DownloadState::Failed))
            .count();
        let usage = snapshot.cache_usage;
        let mut text = format!("{} 个任务 · {} 个进行中", snapshot.downloads.len(), active);
        if failed > 0 {
            text.push_str(&format!(" · {} 个失败", failed));
        }
        text.push_str(&format!(
            " · 流缓存 {:.0} MB",
            usage.bytes as f64 / (1024.0 * 1024.0)
        ));
        text
    }

    pub fn download_quality_label(&self, index: usize) -> String {
        let snapshot = self.core.snapshot();
        let Some(row) = snapshot.downloads.get(index) else {
            return String::new();
        };
        match row.quality() {
            Some(quality) => quality.label().to_string(),
            None => "跟随音源".to_string(),
        }
    }

    /// Rect of one download-row action. The layout mirrors the painter so a
    /// click and the drawn button always agree.
    pub fn download_action_rect(&self, index: usize, action: DownloadAction) -> Rect {
        let row = self.row_rect(index);
        let height = 24.0;
        let top = row.top + (row.height() - height) * 0.5;
        let right = row.right - 12.0;
        let right_button = Rect::new(right - 64.0, top, right, top + height);
        let left_button = Rect::new(
            right_button.left - 6.0 - 64.0,
            top,
            right_button.left - 6.0,
            top + height,
        );
        let quality = Rect::new(
            left_button.left - 6.0 - 78.0,
            top,
            left_button.left - 6.0,
            top + height,
        );
        let (active, has_path) = self
            .core
            .snapshot()
            .downloads
            .get(index)
            .map(|row| {
                (
                    row.state.is_active(),
                    row.path.as_ref().is_some_and(|path| path.exists()),
                )
            })
            .unwrap_or((false, false));
        match action {
            DownloadAction::Cancel => right_button,
            DownloadAction::Open => right_button,
            DownloadAction::Retry => {
                if active || !has_path {
                    right_button
                } else {
                    left_button
                }
            }
            DownloadAction::Quality => quality,
        }
    }

    pub fn statistics_status(&self) -> String {
        if self.core.statistics_loading() {
            return "统计中…".to_string();
        }
        let Some(stats) = self.core.snapshot().statistics else {
            return "暂无统计数据 · 点击“刷新”重新统计".to_string();
        };
        format!(
            "{} 首 · {} 位歌手 · {} 张专辑 · 累计播放 {} 次",
            stats.tracks, stats.artists, stats.albums, stats.total_plays
        )
    }

    pub fn is_playing(&self) -> bool {
        self.core.is_playing()
    }

    /// Colors for every GDI surface in this window.
    pub fn palette(&self) -> crate::theme::Palette {
        let theme = &self.core.settings.theme;
        crate::theme::Palette::new(theme, crate::theme::resolve_dark(theme.mode))
    }

    /// Height of one queue/library row under the current list density.
    pub fn row_height(&self) -> f32 {
        crate::theme::density_row_height(&self.core.settings.theme)
    }

    pub fn position(&self) -> f32 {
        self.core.position_secs() as f32
    }

    /// Position the UI paints: while dragging the progress bar this is the
    /// local preview, otherwise the engine clock.
    pub fn display_position(&self) -> f32 {
        self.drag_preview.unwrap_or_else(|| self.position())
    }

    pub fn duration(&self) -> f32 {
        self.core.duration_secs() as f32
    }

    pub fn max_scroll(&self) -> f32 {
        (self.scroll_content_height() - self.list_area().height()).max(0.0)
    }

    /// First visible row and one past the last visible row.
    pub fn visible_rows(&self) -> (usize, usize) {
        let len = self.row_count();
        if len == 0 {
            return (0, 0);
        }
        let area = self.list_area();
        let first = (self.scroll / self.row_height()).floor().max(0.0) as usize;
        let rows = (area.height() / self.row_height()).ceil() as usize + 1;
        (first.min(len), (first + rows).min(len))
    }

    pub fn row_rect(&self, index: usize) -> Rect {
        if self.online_wall_active() {
            return self.wall_card_rect(index);
        }
        let area = self.list_area();
        let top = area.top + index as f32 * self.row_height() - self.scroll;
        Rect::new(
            area.left,
            top,
            area.right,
            top + self.row_height(),
        )
    }

    pub fn online_wall_active(&self) -> bool {
        self.page == Page::Search && self.online_wall && self.online_is_detail()
    }

    pub fn toggle_online_wall(&mut self) {
        if !self.online_is_detail() {
            return;
        }
        self.online_wall = !self.online_wall;
        self.scroll = 0.0;
        self.ui.clear();
    }

    fn wall_metrics(&self) -> (usize, f32, f32, f32) {
        let area = self.list_area();
        let gap = 10.0;
        let columns = (((area.width() + gap) / 138.0).floor() as usize).clamp(2, 8);
        let card_width = ((area.width() - gap * (columns.saturating_sub(1)) as f32)
            / columns as f32)
            .max(40.0);
        let row_height = card_width + 42.0 + gap;
        (columns, card_width, row_height, gap)
    }

    fn wall_rows(&self) -> usize {
        let (columns, _, _, _) = self.wall_metrics();
        self.row_count().div_ceil(columns.max(1))
    }

    fn wall_card_rect(&self, index: usize) -> Rect {
        let (columns, card_width, row_height, gap) = self.wall_metrics();
        let area = self.list_area();
        let row = index / columns.max(1);
        let column = index % columns.max(1);
        let left = area.left + column as f32 * (card_width + gap);
        let top = area.top + row as f32 * row_height - self.scroll;
        Rect::new(left, top, left + card_width, top + card_width)
    }

    pub fn wall_index_at(&self, x: f32, y: f32) -> Option<usize> {
        if !self.online_wall_active() {
            return None;
        }
        let area = self.list_area();
        if !area.contains(x, y) {
            return None;
        }
        let (columns, card_width, row_height, gap) = self.wall_metrics();
        let local_y = y - area.top + self.scroll;
        let row = (local_y / row_height).floor().max(0.0) as usize;
        if row >= self.wall_rows() {
            return None;
        }
        let relative_x = x - area.left;
        let column = (relative_x / (card_width + gap)).floor().max(0.0) as usize;
        if column >= columns || relative_x - column as f32 * (card_width + gap) > card_width {
            return None;
        }
        let index = row * columns + column;
        (index < self.row_count()).then_some(index)
    }

    pub fn wall_visible_range(&self) -> (usize, usize) {
        if !self.online_wall_active() {
            return (0, 0);
        }
        let (columns, _, row_height, _) = self.wall_metrics();
        let area = self.list_area();
        let first_row = (self.scroll / row_height).floor().max(0.0) as usize;
        let rows = (area.height() / row_height).ceil() as usize + 1;
        let first = (first_row * columns).min(self.row_count());
        let last = ((first_row + rows) * columns).min(self.row_count());
        (first, last)
    }

    /// Height used by scrollbars/wheel math; the cover grid has its own row
    /// height and must not reuse the list row grid.
    pub fn scroll_content_height(&self) -> f32 {
        if self.online_wall_active() {
            let (_, _, row_height, _) = self.wall_metrics();
            self.wall_rows() as f32 * row_height
        } else {
            self.row_count() as f32 * self.row_height()
        }
    }

    /// Index of the active lyric line, for cheap change detection.
    pub fn active_lyric_line(&self) -> Option<usize> {
        let snapshot = self.core.snapshot();
        let lyrics = snapshot.lyrics?;
        let position_ms = (snapshot.position_secs * 1000.0).max(0.0) as i64
            + self.core.settings.lyrics.offset_ms;
        lyrics.index_at(position_ms.max(0) as u64)
    }

    pub fn invalidate(&mut self, hwnd: HWND, area: Rect) {
        if !self.window_visible || area.width() <= 0.0 || area.height() <= 0.0 {
            return;
        }
        let mut pieces: Vec<Rect> = Vec::new();
        if self.visible.is_empty() {
            pieces.push(area);
        } else {
            for visible in &self.visible {
                let piece = intersect_rect(area, *visible);
                if piece.width() > 0.5 && piece.height() > 0.5 {
                    pieces.push(piece);
                }
            }
        }
        for piece in pieces {
            self.invalidate_piece(hwnd, piece);
        }
    }

    fn invalidate_piece(&mut self, hwnd: HWND, piece: Rect) {
        self.dirty.push(piece);
        let scale = self.dpi / 96.0;
        let rect = RECT {
            left: (piece.left * scale).floor() as i32 - 1,
            top: (piece.top * scale).floor() as i32 - 1,
            right: (piece.right * scale).ceil() as i32 + 1,
            bottom: (piece.bottom * scale).ceil() as i32 + 1,
        };
        let _ = unsafe { InvalidateRect(Some(hwnd), Some(&rect), false) };
    }

    /// Large state changes (page switch, track change, resize, DPI) redraw every
    /// exposed part in one pass; covered parts wait until the occlusion poll
    /// notices they are visible again.
    /// Invalidate a region even while it is marked covered. Used when a child
    /// window hides and the parent must repaint those pixels.
    pub fn invalidate_force(&mut self, hwnd: HWND, area: Rect) {
        if area.width() <= 0.0 || area.height() <= 0.0 {
            return;
        }
        self.invalidate_piece(hwnd, area);
    }

    /// A native modal dialog (MessageBox, file picker) was above the window.
    /// Its old footprint is not part of the cached `visible` pieces, so a
    /// normal `invalidate_all` can leave stale rows underneath. Clear the
    /// occlusion cache and force one full repaint.
    pub fn invalidate_after_modal(&mut self, hwnd: HWND) {
        self.reset_visibility();
        self.visibility_changed = true;
        let full = Rect::new(0.0, 0.0, self.layout.size.0, self.layout.size.1);
        self.invalidate_force(hwnd, full);
    }

    pub fn invalidate_all(&mut self, hwnd: HWND) {
        if !self.window_visible {
            return;
        }
        let full = Rect::new(0.0, 0.0, self.layout.size.0, self.layout.size.1);
        let pieces = if self.visible.is_empty() {
            vec![full]
        } else {
            self.visible.clone()
        };
        for piece in pieces {
            self.invalidate_piece(hwnd, piece);
        }
    }

    pub fn take_dirty(&mut self) -> Vec<Rect> {
        std::mem::take(&mut self.dirty)
    }

    pub fn record_frame(&mut self, elapsed_nanos: u128) {
        self.frames += 1;
        self.paint_nanos += elapsed_nanos;
    }

    /// Screen rectangle of the toast. The Now Playing page is covered by the
    /// lyrics child window, so its toast lives in the header; other pages have
    /// no full-size child and use the bottom-right corner above the player.
    pub fn toast_rect(&self) -> Rect {
        let layout = self.layout;
        if self.page == Page::NowPlaying {
            let right = layout.content.right - 16.0;
            let width = (layout.content.width() - 32.0).clamp(200.0, 420.0);
            return Rect::new(right - width, layout.header.top + 8.0, right, layout.header.top + 46.0);
        }
        let content = layout.content;
        let width = (content.width() - 48.0).clamp(220.0, 460.0);
        let mut bottom = (layout.player.top - 14.0).max(layout.header.bottom + 48.0);
        if self.page == Page::Library {
            // Keep the pager buttons readable.
            bottom = (layout.library_pager.top - 6.0).max(layout.header.bottom + 48.0);
        }
        Rect::new(content.right - 24.0 - width, bottom - 38.0, content.right - 24.0, bottom)
    }

    pub fn toast_active(&self) -> bool {
        self.toast.is_some()
    }

    pub fn toast_text(&self) -> Option<(&str, bool)> {
        self.toast.as_ref().map(|toast| (toast.text.as_str(), toast.error))
    }

    /// Show a transient message. Replaces the previous toast and invalidates
    /// only the old/new toast rectangles.
    pub fn push_toast(&mut self, hwnd: HWND, text: impl Into<String>, error: bool) {
        let text = text.into();
        if text.trim().is_empty() {
            return;
        }
        let old = self.toast.as_ref().map(|_| self.toast_rect());
        self.toast = Some(Toast {
            text,
            error,
            expires: Instant::now() + TOAST_DURATION,
        });
        if let Some(old) = old {
            self.invalidate(hwnd, old.inflate(2.0));
        }
        self.invalidate(hwnd, self.toast_rect().inflate(2.0));
    }

    fn expire_toast(&mut self, hwnd: HWND) {
        let expired = self
            .toast
            .as_ref()
            .is_some_and(|toast| Instant::now() >= toast.expires);
        if expired {
            let rect = self.toast_rect();
            self.toast = None;
            self.invalidate(hwnd, rect.inflate(2.0));
        }
    }

    fn drain_notices(&mut self, hwnd: HWND) {
        let rev = self.core.snapshot().notice_rev;
        if rev == self.last_notice_rev {
            return;
        }
        self.last_notice_rev = rev;
        for notice in self.core.drain_notices() {
            self.push_toast(hwnd, notice.text, notice.error);
        }
    }

    /// Pull mono samples and update the visualizer bars. Only runs while the
    /// strip is visible and audio is playing; a paused player keeps its last
    /// frame and produces no periodic repaint.
    fn update_visualizer(&mut self, hwnd: HWND) {
        if !self.visualizer_visible() || !self.is_playing() || !self.window_visible {
            return;
        }
        let rect = self.layout.visualizer;
        if rect.width() <= 1.0 {
            return;
        }
        let interval = 1000 / self.core.settings.visualizer.fps.clamp(10, 60) as u64;
        let now = Instant::now();
        if self
            .last_visual_tick
            .is_some_and(|last| now.duration_since(last).as_millis() < interval as u128)
        {
            return;
        }
        self.last_visual_tick = Some(now);
        let mode = self.core.settings.visualizer.mode;
        self.core.fill_visual_samples(&mut self.visual_samples, 2048);
        if self.visual_samples.is_empty() {
            // No audio tap yet (paused deck / bench fixture): keep the idle
            // placeholder without producing periodic repaints.
            return;
        }
        let bars = (rect.width() / 10.0).clamp(16.0, 96.0) as usize;
        if self.visual_bars.len() != bars {
            self.visual_bars.resize(bars, 0.0);
        }
        let samples = &self.visual_samples;
        for (index, bar) in self.visual_bars.iter_mut().enumerate() {
            // Contiguous time-domain windows are enough for a lightweight
            // level meter; a full FFT would cost more than the feature is
            // worth here.
            let start = index * samples.len() / bars.max(1);
            let end = ((index + 1) * samples.len() / bars.max(1)).min(samples.len());
            let mut sum = 0.0f32;
            for sample in &samples[start..end] {
                sum += sample * sample;
            }
            let rms = (sum / (end.saturating_sub(start).max(1) as f32)).sqrt();
            let target = (rms * 3.4).clamp(0.0, 1.0);
            *bar += (target - *bar) * 0.35;
        }
        if mode == crate::core::config::VisualizerMode::Waveform {
            // Waveform mode draws the raw samples; the bars are unused but
            // keep a stable length so switching modes does not flicker.
        }
        self.invalidate(hwnd, rect.inflate(2.0));
    }

    pub fn tick(&mut self, hwnd: HWND) {
        self.ticks += 1;
        let now = Instant::now();
        self.last_tick = Some(now);
        self.expire_toast(hwnd);
        self.drain_notices(hwnd);

        // Baseline for progress/time diffing is the *previous* tick: the
        // engine clock advances continuously between ticks.
        let previous_position = self.last_position.unwrap_or_else(|| self.position());
        let before = self.tick_state();
        let _ = self.core.tick();
        let after = self.tick_state();
        let position_now = self.position();
        self.last_position = Some(position_now);

        if before.index != after.index {
            // A rapid next/prev must not invalidate the whole window: repaint
            // only the player bar, Now Playing card and the old/new list rows.
            self.invalidate_track_change(hwnd, before.index, after.index);
            self.last_tick = Some(now);
        }
        if before.playing != after.playing || before.state != after.state {
            self.invalidate(hwnd, self.layout.play.inflate(6.0));
            // The card's primary action changes between 播放 and 暂停.
            self.invalidate(hwnd, self.layout.card_buttons[0].inflate(2.0));
            self.last_tick = Some(now);
        }
        if (before.duration - after.duration).abs() > 0.01 {
            self.invalidate(hwnd, self.layout.progress_hit);
            self.invalidate(hwnd, self.layout.time_right);
        }
        if (before.volume - after.volume).abs() > 0.001 || before.muted != after.muted {
            self.invalidate(hwnd, self.layout.volume_hit);
            self.invalidate(hwnd, self.layout.mute.inflate(2.0));
        }
        if before.shuffle != after.shuffle {
            self.invalidate(hwnd, self.layout.shuffle.inflate(2.0));
        }
        if before.repeat != after.repeat {
            self.invalidate(hwnd, self.layout.repeat.inflate(2.0));
        }
        if before.lyrics_rev != after.lyrics_rev || before.lyrics_line != after.lyrics_line {
            // The list itself is a child window; the parent only owns the
            // panel header (source label / loading state).
            self.invalidate(hwnd, self.layout.lyric_header);
        }
        if before.lyrics_background_rev != after.lyrics_background_rev {
            self.invalidate(hwnd, self.layout.lyric_panel);
        }
        if before.library_rev != after.library_rev {
            // Rows may have been replaced or re-sorted under the cursor.
            self.ui.set_hover(None);
            if self.page == Page::Library {
                // A background refresh can shrink the total; keep the page
                // indicator inside the new range.
                let last = self.library_page_count().saturating_sub(1);
                self.library_page = self.library_page.min(last);
            }
            self.scroll = self.scroll.clamp(0.0, self.max_scroll());
            match self.page {
                Page::Library => {
                    self.invalidate(hwnd, self.layout.list);
                    self.invalidate(hwnd, self.scrollbar_rect());
                    self.invalidate(hwnd, self.layout.header);
                }
                Page::Settings => {
                    self.invalidate(hwnd, layout::settings(&self.layout).folders);
                }
                Page::Statistics => {
                    self.invalidate(hwnd, self.layout.content);
                    self.invalidate(hwnd, self.layout.header);
                }
                Page::Smart | Page::Duplicates => {
                    self.ui.set_hover(None);
                    self.scroll = self.scroll.clamp(0.0, self.max_scroll());
                    self.invalidate(hwnd, self.layout.list);
                    self.invalidate(hwnd, self.layout.scrollbar);
                    self.invalidate(hwnd, self.layout.header);
                    self.request_visible_covers();
                }
                _ => {}
            }
            self.request_visible_covers();
        }
        if before.browse_rev != after.browse_rev && self.page == Page::Folders {
            // A directory read finished (or navigation started): rows are a
            // completely different set, so reset hover and scroll.
            self.ui.set_hover(None);
            self.scroll = self.scroll.clamp(0.0, self.max_scroll());
            self.invalidate(hwnd, self.layout.header);
            self.invalidate(hwnd, self.layout.folder_list);
            self.invalidate(hwnd, self.layout.folder_scrollbar);
            self.request_visible_covers();
        }
        if before.cover_rev != after.cover_rev {
            self.invalidate(hwnd, self.layout.cover.inflate(1.0));
            if self.page == Page::NowPlaying && self.layout.card_cover.width() > 0.0 {
                self.invalidate(hwnd, self.layout.card_cover.inflate(1.0));
            }
            // The blurred-cover backdrop is built from the current cover; when
            // the decode finishes after the page was painted, the whole
            // background must be repainted to show it.
            if self.background_uses_cover() {
                let full = Rect::new(0.0, 0.0, self.layout.size.0, self.layout.size.1);
                self.invalidate_force(hwnd, full);
            }
        }
        if before.background_image_rev != after.background_image_rev && self.background_uses_image() {
            let full = Rect::new(0.0, 0.0, self.layout.size.0, self.layout.size.1);
            self.invalidate_force(hwnd, full);
        }
        if before.row_cover_rev != after.row_cover_rev {
            // A thumbnail finished decoding: repaint the rows that show it.
            // `list_area()` covers every list page (queue, library, folders,
            // search, discover, smart, duplicates); leaving a page out kept
            // the note placeholder until the next hover repainted that row.
            if matches!(
                self.page,
                Page::Queue
                    | Page::Library
                    | Page::Folders
                    | Page::Search
                    | Page::Discover
                    | Page::Smart
                    | Page::Duplicates
            ) {
                self.invalidate(hwnd, self.list_area());
            }
            self.request_visible_covers();
        }
        if before.sleep_remaining.is_some() != after.sleep_remaining.is_some()
            && self.page == Page::Settings
            && self.settings_tab == SettingsTab::Playback
            && let Some(index) = self
                .settings_tab
                .fields()
                .iter()
                .position(|field| *field == SettingField::SleepTimer)
        {
            let settings = layout::settings(&self.layout);
            let row = layout::setting_row(&settings, index);
            self.invalidate(hwnd, row.inflate(2.0));
        }
        if before.queue_rev != after.queue_rev {
            self.rebuild_queue_view();
            self.scroll = self.scroll.clamp(0.0, self.max_scroll());
            if self.page == Page::Queue {
                // The order (and the "乱序播放" hint) can change under the
                // cursor, e.g. when shuffle is toggled off.
                self.ui.set_hover(None);
                self.invalidate(hwnd, self.layout.header);
                self.invalidate(hwnd, self.layout.list);
                self.invalidate(hwnd, self.scrollbar_rect());
            }
        }
        if before.online_rev != after.online_rev {
            if self.page == Page::Search {
                self.ui.set_hover(None);
                self.scroll = self.scroll.clamp(0.0, self.max_scroll());
                self.invalidate(hwnd, self.layout.header);
                self.invalidate(hwnd, self.layout.online_search.inflate(2.0));
                self.invalidate(hwnd, self.layout.online_kind.inflate(2.0));
                self.invalidate(hwnd, self.layout.online_queue_all.inflate(2.0));
                self.invalidate(hwnd, self.layout.online_random.inflate(2.0));
                self.invalidate(hwnd, self.list_area());
                self.invalidate(hwnd, self.scrollbar_rect());
                self.invalidate(hwnd, self.layout.library_pager);
            }
        }
        if before.discover_rev != after.discover_rev && self.page == Page::Discover {
            self.ui.set_hover(None);
            self.scroll = self.scroll.clamp(0.0, self.max_scroll());
            self.invalidate(hwnd, self.layout.header);
            self.invalidate(hwnd, self.list_area());
            self.invalidate(hwnd, self.scrollbar_rect());
            self.request_visible_covers();
        }
        if before.download_rev != after.download_rev {
            if self.page == Page::Downloads {
                self.scroll = self.scroll.clamp(0.0, self.max_scroll());
                self.invalidate(hwnd, self.layout.header);
                self.invalidate(hwnd, self.list_area());
                self.invalidate(hwnd, self.scrollbar_rect());
            } else if self.page == Page::Settings
                && matches!(
                    self.settings_tab,
                    SettingsTab::Storage | SettingsTab::System
                )
            {
                self.invalidate(hwnd, layout::settings(&self.layout).generic);
            }
        }
        if before.cache_rev != after.cache_rev {
            if self.page == Page::Downloads {
                self.invalidate(hwnd, self.layout.header);
            } else if self.page == Page::Settings
                && self.settings_tab == SettingsTab::Network
            {
                self.invalidate(hwnd, layout::settings(&self.layout).generic);
            }
        }

        self.request_visible_covers();
        self.update_visualizer(hwnd);

        if !after.playing {
            return;
        }

        // Keep the clock running while occluded, but only repaint the exposed
        // part of the window. Poll the z-order every ~0.5s: subtract every
        // window above us from our client rect. Focus is irrelevant; a
        // visible-but-unfocused window keeps animating.
        self.visibility_poll = self.visibility_poll.saturating_sub(1);
        if self.visibility_poll == 0 {
            self.visibility_poll = 20;
            let rects = visible_rects(hwnd, self.dpi);
            let visible = !rects.is_empty();
            let changed = rects != self.visible;
            if visible != self.window_visible || (visible && changed) {
                self.visible = rects;
                self.window_visible = visible;
                if visible {
                    self.visibility_changed = true;
                    self.last_tick = Some(now);
                    self.invalidate_all(hwnd);
                }
            }
        }
        if !self.window_visible {
            return;
        }

        let duration = self.duration().max(0.1);
        if !self.dragging_progress {
            let old_ratio = (previous_position / duration).clamp(0.0, 1.0);
            let new_ratio = (position_now / duration).clamp(0.0, 1.0);
            self.invalidate_progress_edge(hwnd, old_ratio, new_ratio);
            if position_now.floor() as i64 != previous_position.floor() as i64 {
                self.invalidate(hwnd, self.layout.time_left);
                self.invalidate(hwnd, self.layout.time_right);
            }
        }
    }

    fn tick_state(&self) -> TickState {
        let snapshot = self.core.snapshot();
        TickState {
            index: snapshot.current_index,
            playing: self.is_playing(),
            state: snapshot.state,
            duration: self.duration(),
            volume: snapshot.volume,
            muted: snapshot.muted,
            shuffle: snapshot.shuffle,
            repeat: snapshot.repeat,
            lyrics_rev: snapshot.lyrics_rev,
            lyrics_background_rev: snapshot.lyrics_background_rev,
            background_image_rev: snapshot.background_image_rev,
            lyrics_line: self.active_lyric_line(),
            library_rev: snapshot.library_rev,
            cover_rev: snapshot.cover_rev,
            row_cover_rev: snapshot.row_cover_rev,
            sleep_remaining: snapshot.sleep_remaining_secs,
            queue_rev: snapshot.queue_rev,
            browse_rev: snapshot.browse_rev,
            online_rev: snapshot.online_rev,
            discover_rev: snapshot.discover.rev,
            download_rev: snapshot.download_rev,
            cache_rev: snapshot.cache_rev,
        }
    }

    /// Track changes touch the player bar, the Now Playing card, the old/new
    /// queue rows and the progress/time area. Never the whole window.
    fn invalidate_track_change(
        &mut self,
        hwnd: HWND,
        old_index: Option<usize>,
        new_index: Option<usize>,
    ) {
        // The position jumped (new track / seek); re-baseline the progress diff
        // so the next tick does not invalidate a large slice of the bar.
        self.last_position = None;
        self.drag_preview = None;
        self.invalidate(hwnd, self.layout.cover);
        self.invalidate(hwnd, self.layout.track_text);
        self.invalidate(hwnd, self.layout.progress_hit);
        self.invalidate(hwnd, self.layout.time_left);
        self.invalidate(hwnd, self.layout.time_right);
        match self.page {
            Page::NowPlaying => {
                self.invalidate(hwnd, self.layout.card_cover);
                self.invalidate(hwnd, self.layout.card_title);
                self.invalidate(hwnd, self.layout.card_meta);
                self.invalidate(hwnd, self.layout.card_badges);
                self.invalidate(hwnd, self.layout.lyric_header);
            }
            Page::Queue => {
                let (first, last) = self.visible_rows();
                for index in [old_index, new_index].into_iter().flatten() {
                    // Queue rows are rendered through the filter mapping.
                    let Some(display) = self
                        .queue_view
                        .iter()
                        .position(|real| *real == index)
                    else {
                        continue;
                    };
                    if display >= first && display < last {
                        self.invalidate(hwnd, self.row_rect(display));
                    }
                }
            }
            Page::Library | Page::Smart | Page::Duplicates => {
                // These lists highlight the current row by file path, not by
                // queue index, so map the old/new queue entries back to the
                // visible rows. Without this the previous highlight stayed on
                // screen until the next hover repainted it.
                let rows: Vec<usize> = {
                    let snapshot = self.core.snapshot();
                    let path_of = |index: Option<usize>| {
                        index
                            .and_then(|index| snapshot.queue.get(index))
                            .and_then(|track| track.source.path())
                    };
                    let old_path = path_of(old_index);
                    let new_path = path_of(new_index);
                    let entries: &[_] = match self.page {
                        Page::Smart => snapshot.library_smart,
                        Page::Duplicates => snapshot.library_duplicate_rows,
                        _ => snapshot.library,
                    };
                    let (first, last) = self.visible_rows();
                    (first..last.min(entries.len()))
                        .filter(|row| {
                            let path = entries[*row].path.as_path();
                            old_path.is_some_and(|old| old == path)
                                || new_path.is_some_and(|new| new == path)
                        })
                        .collect()
                };
                for row in rows {
                    self.invalidate(hwnd, self.row_rect(row));
                }
            }
            _ => {}
        }
    }

    /// The progress bar only changes at its fill edge: repaint that sliver.
    fn invalidate_progress_edge(&mut self, hwnd: HWND, old_ratio: f32, new_ratio: f32) {
        let bar = self.layout.progress;
        let old_x = bar.left + bar.width() * old_ratio;
        let new_x = bar.left + bar.width() * new_ratio;
        if (new_x - old_x).abs() < 0.05 {
            return;
        }
        let left = old_x.min(new_x);
        let right = old_x.max(new_x);
        // Include the knob at the fill edge. The pressed knob is 18 DIP tall,
        // so the invalidation must reach at least 9 DIP above/below the bar;
        // a tighter band left white slivers at every drag position.
        self.invalidate(
            hwnd,
            Rect::new(left - 6.0, bar.top - 12.0, right + 6.0, bar.bottom + 12.0),
        );
    }

    pub fn button_state(&self, target: Target, active: bool) -> ButtonState {
        self.ui.button_state(target, active)
    }

    fn hit_target(&self, x: f32, y: f32) -> Option<Target> {
        if self.layout.nav_scrollbar.width() > 0.0
            && self.sidebar_max_scroll() > 0.5
            && self.layout.nav_scrollbar.inflate(3.0).contains(x, y)
        {
            return Some(Target::SidebarScrollbar);
        }
        for index in 0..self.layout.nav.len() {
            let item = self.nav_rect(index);
            if item.contains(x, y) && self.layout.nav_viewport.contains(x, y) {
                return Some(Target::Nav(index));
            }
        }
        if self.page == Page::Statistics {
            let stats = layout::statistics(&self.layout);
            if stats.refresh.inflate(2.0).contains(x, y) {
                return Some(Target::StatsRefresh);
            }
            if stats.reset.inflate(2.0).contains(x, y) {
                return Some(Target::StatsReset);
            }
            if stats.recent.contains(x, y) {
                let row = ((y - stats.recent.top - 4.0) / layout::STATS_ROW_HEIGHT).floor();
                if row >= 0.0 {
                    return Some(Target::StatsRecent(row as usize));
                }
            }
        }
        if self.layout.play.inflate(6.0).contains(x, y) {
            return Some(Target::Play);
        }
        if self.layout.prev.contains(x, y) {
            return Some(Target::Prev);
        }
        if self.layout.next.contains(x, y) {
            return Some(Target::Next);
        }
        if self.layout.shuffle.inflate(2.0).contains(x, y) {
            return Some(Target::Shuffle);
        }
        if self.layout.repeat.inflate(2.0).contains(x, y) {
            return Some(Target::Repeat);
        }
        if self.layout.volume_hit.width() > 0.0 && self.layout.volume_hit.contains(x, y) {
            return Some(Target::Volume);
        }
        if self.layout.mute.inflate(2.0).contains(x, y) {
            return Some(Target::Mute);
        }
        if self.layout.player_settings.inflate(2.0).contains(x, y) {
            return Some(Target::PlaySettings);
        }
        if self.layout.mini_player.width() > 0.0
            && self.layout.mini_player.inflate(2.0).contains(x, y)
        {
            return Some(Target::MiniPlayer);
        }
        if self.layout.desktop_lyrics.width() > 0.0
            && self.layout.desktop_lyrics.inflate(2.0).contains(x, y)
        {
            return Some(Target::DesktopLyrics);
        }
        if self.page == Page::Smart && self.page_sort_rect().inflate(2.0).contains(x, y) {
            return Some(Target::SmartKind);
        }
        if self.page == Page::Duplicates && self.page_sort_rect().inflate(2.0).contains(x, y) {
            return Some(Target::DuplicateMode);
        }
        if self.page == Page::Search {
            if self.layout.online_kind.inflate(2.0).contains(x, y) {
                return Some(if self.online_is_detail() {
                    Target::OnlineBack
                } else {
                    Target::OnlineKind
                });
            }
            if self.layout.online_queue_all.inflate(2.0).contains(x, y) {
                return Some(Target::OnlineQueueAll);
            }
            if self.layout.online_random.inflate(2.0).contains(x, y) {
                return Some(Target::OnlineRandom);
            }
            if self.online_page() > 0
                && self.layout.library_prev.inflate(2.0).contains(x, y)
            {
                return Some(Target::OnlinePrev);
            }
            if self.online_has_more()
                && self.layout.library_next.inflate(2.0).contains(x, y)
            {
                return Some(Target::OnlineNext);
            }
            if self.online_is_detail()
                && self.layout.online_wall.inflate(2.0).contains(x, y)
            {
                return Some(Target::OnlineWall);
            }
        }
        if self.page == Page::Discover
            && self.layout.discover_refresh.inflate(2.0).contains(x, y)
        {
            return Some(Target::DiscoverRefresh);
        }
        if self.layout.playlist.inflate(2.0).contains(x, y) {
            return Some(Target::Playlist);
        }
        if self.layout.progress_hit.contains(x, y) {
            return Some(Target::Progress);
        }
        if matches!(
            self.page,
            Page::Queue | Page::Folders | Page::Smart | Page::Duplicates | Page::Search
                | Page::Discover | Page::Downloads
        ) && self.scrollbar_rect().contains(x, y)
        {
            return Some(Target::Scrollbar);
        }

        if self.page == Page::Library {
            let pages = self.library_page_count();
            if self.library_page > 0
                && self.layout.library_prev.inflate(2.0).contains(x, y)
            {
                return Some(Target::LibraryPrev);
            }
            if self.library_page + 1 < pages
                && self.layout.library_next.inflate(2.0).contains(x, y)
            {
                return Some(Target::LibraryNext);
            }
        }

        if self.page == Page::NowPlaying {
            // 歌词面板头部按钮（选择/独立窗口/大歌词/编辑/刷新）。
            for (index, target) in [
                (0, Target::LyricsPick),
                (1, Target::LyricsWindow),
                (2, Target::LyricsBig),
                (3, Target::LyricsEdit),
                (4, Target::LyricsReload),
            ] {
                if self.layout.lyric_buttons[index]
                    .inflate(2.0)
                    .contains(x, y)
                {
                    return Some(target);
                }
            }
            if !self.big_lyrics {
                if self.layout.card_buttons[0].inflate(2.0).contains(x, y) {
                    return Some(Target::CardPlay);
                }
                if self.layout.card_buttons[1].inflate(2.0).contains(x, y) {
                    return Some(Target::CardOpen);
                }
            }
        }

        if self.page == Page::Library {
            if self.layout.sort.inflate(2.0).contains(x, y) {
                return Some(Target::Sort);
            }
            if self.layout.add_folder.inflate(2.0).contains(x, y) {
                return Some(Target::AddFolder);
            }
            if self.layout.rescan.inflate(2.0).contains(x, y) {
                return Some(Target::Rescan);
            }
            if self.library_can_queue_all()
                && self.layout.library_queue_all.inflate(2.0).contains(x, y)
            {
                return Some(Target::LibraryQueueAll);
            }
            if self.library_can_check_missing()
                && self.layout.library_check_missing.inflate(2.0).contains(x, y)
            {
                return Some(Target::LibraryCheckMissing);
            }
        }

        if self.page == Page::Queue {
            if self.layout.queue_import.inflate(2.0).contains(x, y) {
                return Some(Target::QueueImport);
            }
            if self.queue_len() > 0 && self.layout.queue_export.inflate(2.0).contains(x, y) {
                return Some(Target::QueueExport);
            }
            if self.queue_len() > 0 && self.layout.queue_clear.inflate(2.0).contains(x, y) {
                return Some(Target::QueueClear);
            }
        }

        if self.page == Page::Folders {
            for (rect, target) in [
                (self.layout.folder_back, Target::FolderBack),
                (self.layout.folder_forward, Target::FolderForward),
                (self.layout.folder_up, Target::FolderUp),
                (self.layout.folder_refresh, Target::FolderRefresh),
                (self.layout.folder_play_all, Target::FolderPlayAll),
                (self.layout.folder_queue_all, Target::FolderQueueAll),
            ] {
                if rect.inflate(2.0).contains(x, y) {
                    return Some(target);
                }
            }
        }

        if self.page == Page::Settings {
            let settings = layout::settings(&self.layout);
            for (index, rect) in settings.tabs.into_iter().enumerate() {
                if rect.inflate(2.0).contains(x, y) {
                    return Some(Target::SettingsTab(index));
                }
            }
            if self.settings_tab != SettingsTab::Library {
                if self.settings_max_scroll() > 0.5
                    && settings.scrollbar.inflate(2.0).contains(x, y)
                {
                    return Some(Target::SettingsScrollbar);
                }
                if !settings.generic.contains(x, y) {
                    return None;
                }
                let views = self.settings_views();
                let scroll = self.settings_scroll;
                for (index, view) in views.iter().enumerate() {
                    let mut control = layout::setting_control(&settings, index);
                    control.top -= scroll;
                    control.bottom -= scroll;
                    if control.bottom <= settings.generic.top
                        || control.top >= settings.generic.bottom
                    {
                        continue;
                    }
                    match view.kind {
                        SettingKind::Toggle => {
                            if control.inflate(2.0).contains(x, y) {
                                return Some(Target::SettingsToggleRow(index));
                            }
                        }
                        SettingKind::Choice => {
                            if self.settings_compact_choice() {
                                if control.inflate(2.0).contains(x, y) {
                                    return Some(Target::SettingsChoiceCycle(index));
                                }
                                continue;
                            }
                            for (option, rect) in
                                layout::segments(control, view.options.len())
                                    .into_iter()
                                    .enumerate()
                            {
                                if rect.inflate(2.0).contains(x, y) {
                                    return Some(Target::SettingsChoice(index, option));
                                }
                            }
                        }
                        SettingKind::Step => {
                            let (minus, _, plus) = layout::step_rects(control);
                            if minus.inflate(2.0).contains(x, y) {
                                return Some(Target::SettingsStep(index, -1));
                            }
                            if plus.inflate(2.0).contains(x, y) {
                                return Some(Target::SettingsStep(index, 1));
                            }
                        }
                        SettingKind::Color => {
                            if control.inflate(2.0).contains(x, y) {
                                return Some(Target::SettingsColor(index));
                            }
                        }
                        SettingKind::Action => {
                            let reset_available = view.active
                                && self
                                    .settings_tab
                                    .fields()
                                    .get(index)
                                    .copied()
                                    .is_some_and(|field| {
                                        Self::font_reset_command(field).is_some()
                                    });
                            if reset_available
                                && layout::action_reset_rect(control)
                                    .inflate(2.0)
                                    .contains(x, y)
                            {
                                return Some(Target::SettingsActionReset(index));
                            }
                            if control.inflate(2.0).contains(x, y) {
                                return Some(Target::SettingsAction(index));
                            }
                        }
                        SettingKind::Text => {
                            if control.inflate(2.0).contains(x, y) {
                                return Some(Target::SettingsText(index));
                            }
                        }
                    }
                }
                return None;
            }
            for (index, rect) in [
                settings.scan_toggle,
                settings.watch_toggle,
                settings.follow_toggle,
            ]
            .into_iter()
            .enumerate()
            {
                if rect.inflate(2.0).contains(x, y) {
                    return Some(Target::SettingToggle(index));
                }
            }
            for (index, rect) in layout::dedupe_options(&settings).into_iter().enumerate() {
                if rect.inflate(2.0).contains(x, y) {
                    return Some(Target::SettingDedupeOption(index));
                }
            }
            if settings.add_folder.inflate(2.0).contains(x, y) {
                return Some(Target::SettingAddFolder);
            }
            if settings.folders.contains(x, y) {
                let row = ((y - settings.folders.top) / layout::FOLDER_ROW_HEIGHT) as usize;
                if self.settings_folders_scroll + row < self.settings_folders().len() {
                    let row_rect = layout::folder_row(&settings, row);
                    let remove = Rect::new(
                        row_rect.right - 72.0,
                        row_rect.top,
                        row_rect.right,
                        row_rect.bottom,
                    );
                    if remove.contains(x, y) {
                        return Some(Target::FolderRemove(row));
                    }
                }
                return None;
            }
        }

        if self.page == Page::Downloads {
            if self.layout.download_clean_part.inflate(2.0).contains(x, y) {
                return Some(Target::DownloadsCleanPart);
            }
            if self.layout.download_clear_finished.inflate(2.0).contains(x, y) {
                return Some(Target::DownloadsClearFinished);
            }
            if self.list_area().contains(x, y) {
                let row = self.row_at(y);
                let snapshot = self.core.snapshot();
                if let Some(entry) = snapshot.downloads.get(row) {
                    let active = entry.state.is_active();
                    if self
                        .download_action_rect(row, DownloadAction::Quality)
                        .inflate(2.0)
                        .contains(x, y)
                    {
                        return Some(Target::DownloadAction(row, DownloadAction::Quality));
                    }
                    if active {
                        if self
                            .download_action_rect(row, DownloadAction::Cancel)
                            .inflate(2.0)
                            .contains(x, y)
                        {
                            return Some(Target::DownloadAction(row, DownloadAction::Cancel));
                        }
                    } else {
                        if entry.path.as_ref().is_some_and(|path| path.exists())
                            && self
                                .download_action_rect(row, DownloadAction::Open)
                                .inflate(2.0)
                                .contains(x, y)
                        {
                            return Some(Target::DownloadAction(row, DownloadAction::Open));
                        }
                        if self
                            .download_action_rect(row, DownloadAction::Retry)
                            .inflate(2.0)
                            .contains(x, y)
                        {
                            return Some(Target::DownloadAction(row, DownloadAction::Retry));
                        }
                    }
                    return Some(Target::Row(row));
                }
            }
        }

        if matches!(
            self.page,
            Page::Queue | Page::Library | Page::Folders | Page::Smart | Page::Duplicates
                | Page::Search | Page::Discover
        ) && self.list_area().contains(x, y)
        {
            if self.online_wall_active() {
                return self.wall_index_at(x, y).map(Target::Row);
            }
            let row = self.row_at(y);
            if row < self.row_count() {
                return Some(Target::Row(row));
            }
        }
        None
    }

    pub(crate) fn target_rect(&self, target: Target) -> Rect {
        match target {
            Target::Nav(index) => self.nav_rect(index).inflate(2.0),
            Target::SidebarScrollbar => self.layout.nav_scrollbar.inflate(3.0),
            Target::Play => self.layout.play.inflate(6.0),
            Target::Prev => self.layout.prev.inflate(4.0),
            Target::Next => self.layout.next.inflate(4.0),
            Target::Shuffle => self.layout.shuffle.inflate(2.0),
            Target::Repeat => self.layout.repeat.inflate(2.0),
            Target::Volume => self.layout.volume_hit,
            Target::Mute => self.layout.mute.inflate(2.0),
            Target::PlaySettings => self.layout.player_settings.inflate(2.0),
            Target::Playlist => self.layout.playlist.inflate(2.0),
            Target::MiniPlayer => self.layout.mini_player.inflate(2.0),
            Target::DesktopLyrics => self.layout.desktop_lyrics.inflate(2.0),
            Target::Progress => self.layout.progress_hit,
            Target::Row(index) => self.row_rect(index),
            Target::Scrollbar => self.scrollbar_rect(),
            Target::Sort => self.layout.sort.inflate(2.0),
            Target::AddFolder => self.layout.add_folder.inflate(2.0),
            Target::Rescan => self.layout.rescan.inflate(2.0),
            Target::LibraryQueueAll => self.layout.library_queue_all.inflate(2.0),
            Target::LibraryCheckMissing => self.layout.library_check_missing.inflate(2.0),
            Target::QueueImport => self.layout.queue_import.inflate(2.0),
            Target::QueueExport => self.layout.queue_export.inflate(2.0),
            Target::QueueClear => self.layout.queue_clear.inflate(2.0),
            Target::FolderBack => self.layout.folder_back.inflate(2.0),
            Target::FolderForward => self.layout.folder_forward.inflate(2.0),
            Target::FolderUp => self.layout.folder_up.inflate(2.0),
            Target::FolderRefresh => self.layout.folder_refresh.inflate(2.0),
            Target::FolderPlayAll => self.layout.folder_play_all.inflate(2.0),
            Target::FolderQueueAll => self.layout.folder_queue_all.inflate(2.0),
            Target::LibraryPrev => self.layout.library_prev.inflate(2.0),
            Target::LibraryNext => self.layout.library_next.inflate(2.0),
            Target::SettingToggle(index) => {
                let settings = layout::settings(&self.layout);
                [
                    settings.scan_toggle,
                    settings.watch_toggle,
                    settings.follow_toggle,
                ][index]
                    .inflate(2.0)
            }
            Target::SettingDedupeOption(index) => {
                let settings = layout::settings(&self.layout);
                layout::dedupe_options(&settings)[index].inflate(2.0)
            }
            Target::SettingAddFolder => {
                layout::settings(&self.layout).add_folder.inflate(2.0)
            }
            Target::SettingsTab(index) => layout::settings(&self.layout).tabs[index].inflate(2.0),
            Target::SettingsRow(index) => {
                let settings = layout::settings(&self.layout);
                let mut control = layout::setting_control(&settings, index);
                control.top -= self.settings_scroll;
                control.bottom -= self.settings_scroll;
                control.inflate(2.0)
            }
            Target::SettingsToggleRow(index) => {
                let settings = layout::settings(&self.layout);
                let mut control = layout::setting_control(&settings, index);
                control.top -= self.settings_scroll;
                control.bottom -= self.settings_scroll;
                control.inflate(2.0)
            }
            Target::SettingsChoice(index, option) => {
                let settings = layout::settings(&self.layout);
                let views = self.settings_views();
                let count = views
                    .get(index)
                    .map(|view| view.options.len())
                    .unwrap_or(1);
                let mut control = layout::setting_control(&settings, index);
                control.top -= self.settings_scroll;
                control.bottom -= self.settings_scroll;
                layout::segments(control, count)
                    .get(option)
                    .copied()
                    .unwrap_or(control)
                    .inflate(2.0)
            }
            Target::SettingsChoiceCycle(index) => {
                let settings = layout::settings(&self.layout);
                let mut control = layout::setting_control(&settings, index);
                control.top -= self.settings_scroll;
                control.bottom -= self.settings_scroll;
                control.inflate(2.0)
            }
            Target::SettingsStep(index, delta) => {
                let settings = layout::settings(&self.layout);
                let mut control = layout::setting_control(&settings, index);
                control.top -= self.settings_scroll;
                control.bottom -= self.settings_scroll;
                let (minus, _, plus) = layout::step_rects(control);
                if delta < 0 {
                    minus.inflate(2.0)
                } else {
                    plus.inflate(2.0)
                }
            }
            Target::SettingsColor(index)
            | Target::SettingsAction(index)
            | Target::SettingsText(index) => {
                let settings = layout::settings(&self.layout);
                let mut control = layout::setting_control(&settings, index);
                control.top -= self.settings_scroll;
                control.bottom -= self.settings_scroll;
                control.inflate(2.0)
            }
            Target::SettingsActionReset(index) => {
                let settings = layout::settings(&self.layout);
                let mut control = layout::setting_control(&settings, index);
                control.top -= self.settings_scroll;
                control.bottom -= self.settings_scroll;
                layout::action_reset_rect(control).inflate(2.0)
            }
            Target::SettingsScrollbar => layout::settings(&self.layout).scrollbar.inflate(2.0),
            Target::FolderRemove(row) => {
                let settings = layout::settings(&self.layout);
                let row = layout::folder_row(&settings, row);
                Rect::new(row.right - 72.0, row.top, row.right, row.bottom)
            }
            Target::CardPlay => self.layout.card_buttons[0].inflate(2.0),
            Target::CardOpen => self.layout.card_buttons[1].inflate(2.0),
            Target::LyricsPick => self.layout.lyric_buttons[0].inflate(2.0),
            Target::LyricsWindow => self.layout.lyric_buttons[1].inflate(2.0),
            Target::LyricsBig => self.layout.lyric_buttons[2].inflate(2.0),
            Target::LyricsEdit => self.layout.lyric_buttons[3].inflate(2.0),
            Target::LyricsReload => self.layout.lyric_buttons[4].inflate(2.0),
            Target::SmartKind | Target::DuplicateMode => self.layout.header_sort.inflate(2.0),
            Target::OnlineKind | Target::OnlineBack => self.layout.online_kind.inflate(2.0),
            Target::OnlineQueueAll => self.layout.online_queue_all.inflate(2.0),
            Target::OnlineRandom => self.layout.online_random.inflate(2.0),
            Target::OnlinePrev => self.layout.library_prev.inflate(2.0),
            Target::OnlineNext => self.layout.library_next.inflate(2.0),
            Target::OnlineWall => self.layout.online_wall.inflate(2.0),
            Target::DiscoverRefresh => self.layout.discover_refresh.inflate(2.0),
            Target::DownloadAction(index, action) => {
                self.download_action_rect(index, action).inflate(2.0)
            }
            Target::DownloadsCleanPart => self.layout.download_clean_part.inflate(2.0),
            Target::DownloadsClearFinished => {
                self.layout.download_clear_finished.inflate(2.0)
            }
            Target::StatsRefresh => layout::statistics(&self.layout).refresh.inflate(2.0),
            Target::StatsReset => layout::statistics(&self.layout).reset.inflate(2.0),
            Target::StatsRecent(index) => {
                let stats = layout::statistics(&self.layout);
                let top = stats.recent.top + 4.0 + index as f32 * layout::STATS_ROW_HEIGHT;
                Rect::new(
                    stats.recent.left,
                    top,
                    stats.recent.right,
                    top + layout::STATS_ROW_HEIGHT - 2.0,
                )
            }
        }
    }

    pub fn on_move(&mut self, hwnd: HWND, x: f32, y: f32) {
        if self.page == Page::Folders
            && let Some((_, start_x, start_y)) = self.folder_press
        {
            if !self.folder_drag && ((x - start_x).abs() + (y - start_y).abs()) > 6.0 {
                self.folder_drag = true;
            }
            if self.folder_drag {
                let over = self.nav_rect(1).inflate(2.0).contains(x, y);
                if over != self.folder_drop_over_queue {
                    self.folder_drop_over_queue = over;
                    self.invalidate(hwnd, self.nav_rect(1).inflate(2.0));
                    self.invalidate(hwnd, self.layout.header);
                }
            }
            return;
        }
        if self.page == Page::Queue
            && let Some(from) = self.queue_drag_from
        {
            let old = self.queue_drag_target;
            let area = self.list_area();
            let target = if area.contains(x, y) || (y >= area.top && y <= area.bottom) {
                let row = self.row_at(y).min(self.row_count().saturating_sub(1));
                Some(row)
            } else {
                old
            };
            if target != old {
                if let Some(index) = old {
                    self.invalidate(hwnd, self.row_rect(index).inflate(2.0));
                }
                if let Some(index) = target {
                    self.invalidate(hwnd, self.row_rect(index).inflate(2.0));
                }
            }
            self.queue_drag_target = target;
            self.queue_drag_active = target.is_some_and(|target| target != from)
                || self.queue_drag_active;
            return;
        }
        if self.dragging_volume {
            self.set_volume_to_x(hwnd, x);
            return;
        }
        if self.dragging_progress {
            self.drag_progress_to(hwnd, x);
            return;
        }
        if self.dragging_scrollbar {
            self.scroll_to_y(hwnd, y);
            return;
        }
        if self.dragging_settings_scrollbar {
            self.settings_scroll_to_y(hwnd, y);
            return;
        }
        if self.dragging_sidebar_scrollbar {
            self.sidebar_scroll_to_y(hwnd, y);
            return;
        }
        let target = self.hit_target(x, y);
        let (old, new) = self.ui.set_hover(target);
        if old != new {
            if let Some(old) = old {
                self.invalidate(hwnd, self.target_rect(old));
            }
            if let Some(new) = new {
                self.invalidate(hwnd, self.target_rect(new));
            }
        }
    }

    pub fn clear_hover(&mut self, hwnd: HWND) {
        let (hovered, pressed) = self.ui.clear();
        for target in [hovered, pressed].into_iter().flatten() {
            self.invalidate(hwnd, self.target_rect(target));
        }
    }

    pub fn on_press(&mut self, hwnd: HWND, x: f32, y: f32) {
        if self.shortcut_capture.take().is_some() {
            // Any click leaves capture mode; the release handler may start it
            // again when the clicked row is another shortcut.
            self.invalidate(hwnd, self.layout.content);
        }
        if self.hit_target(x, y) == Some(Target::SidebarScrollbar) {
            self.ui.set_pressed(Some(Target::SidebarScrollbar));
            self.dragging_sidebar_scrollbar = true;
            unsafe {
                SetCapture(hwnd);
            }
            self.sidebar_scroll_to_y(hwnd, y);
            return;
        }
        let target = self.hit_target(x, y);
        let (old, new) = self.ui.set_pressed(target);
        if old != new {
            if let Some(old) = old {
                self.invalidate(hwnd, self.target_rect(old));
            }
            if let Some(new) = new {
                self.invalidate(hwnd, self.target_rect(new));
            }
        }
    }

    pub fn set_input_focus(&mut self, hwnd: HWND, input: Option<Input>) {
        if self.ui.set_focus(input) != input {
            self.invalidate(hwnd, self.layout.search.inflate(2.0));
            self.invalidate(hwnd, self.layout.online_search.inflate(2.0));
            self.invalidate(hwnd, self.layout.queue_filter.inflate(2.0));
            self.invalidate(hwnd, layout::settings(&self.layout).ext_edit.inflate(2.0));
            self.invalidate(hwnd, self.layout.folder_address.inflate(2.0));
        }
    }

    /// Navigate the folder browser from the address bar (Enter) or a row.
    /// Resets the scroll and repaints the folder toolbar/list in one go.
    pub fn folder_go(&mut self, hwnd: HWND, path: Option<std::path::PathBuf>) {
        self.core.handle(Command::BrowseTo(path));
        self.reset_folder_view(hwnd);
    }

    pub fn on_click(&mut self, hwnd: HWND, x: f32, y: f32) {
        for (index, item) in self.layout.nav.iter().enumerate() {
            if item.contains(x, y) {
                let page = Page::ALL[index];
                if page != self.page {
                    self.set_page(page);
                    self.invalidate_all(hwnd);
                    if page == Page::Queue {
                        // The filter frame sits between the header and the
                        // list; a stale exposed-rect cache used to skip this
                        // thin strip when coming from the library page.
                        self.invalidate_force(hwnd, self.layout.queue_filter.inflate(6.0));
                    }
                }
                return;
            }
        }

        // Player-bar cover/title: jump to Now Playing from any page.
        if self.layout.cover.contains(x, y) || self.layout.track_text.contains(x, y) {
            if self.page != Page::NowPlaying {
                self.set_page(Page::NowPlaying);
                self.invalidate_all(hwnd);
            }
            return;
        }

        if self.page == Page::Discover
            && self.layout.discover_refresh.inflate(2.0).contains(x, y)
        {
            self.core.handle(Command::DiscoverRefresh);
            self.scroll = 0.0;
            self.invalidate_all(hwnd);
            return;
        }
        if self.page == Page::Search
            && self.online_is_detail()
            && self.layout.online_wall.inflate(2.0).contains(x, y)
        {
            self.toggle_online_wall();
            self.invalidate_all(hwnd);
            return;
        }

        if self.page == Page::Downloads {
            if self.layout.download_clean_part.inflate(2.0).contains(x, y) {
                self.core.handle(Command::CleanPartialDownloads);
                self.invalidate(hwnd, self.layout.header);
                return;
            }
            if self.layout.download_clear_finished.inflate(2.0).contains(x, y) {
                self.core.handle(Command::ClearFinishedDownloads);
                self.scroll = self.scroll.clamp(0.0, self.max_scroll());
                self.invalidate(hwnd, self.layout.header);
                self.invalidate(hwnd, self.list_area());
                self.invalidate(hwnd, self.scrollbar_rect());
                return;
            }
            if self.list_area().contains(x, y) {
                let row = self.row_at(y);
                let data = self.core.snapshot().downloads.get(row).map(|entry| {
                    (
                        entry.id,
                        entry.state.is_active(),
                        entry.path.clone(),
                    )
                });
                if let Some((id, active, path)) = data {
                    if self
                        .download_action_rect(row, DownloadAction::Quality)
                        .inflate(2.0)
                        .contains(x, y)
                    {
                        self.core.handle(Command::CycleDownloadQuality(id));
                        self.invalidate(hwnd, self.row_rect(row));
                        return;
                    }
                    if active {
                        if self
                            .download_action_rect(row, DownloadAction::Cancel)
                            .inflate(2.0)
                            .contains(x, y)
                        {
                            self.core.handle(Command::CancelDownload(id));
                            self.invalidate(hwnd, self.row_rect(row));
                            return;
                        }
                    } else {
                        if path.as_ref().is_some_and(|path| path.exists())
                            && self
                                .download_action_rect(row, DownloadAction::Open)
                                .inflate(2.0)
                                .contains(x, y)
                        {
                            if let Some(path) = path.as_ref() {
                                reveal_path(path);
                            }
                            return;
                        }
                        if self
                            .download_action_rect(row, DownloadAction::Retry)
                            .inflate(2.0)
                            .contains(x, y)
                        {
                            self.core.handle(Command::RetryDownload(id));
                            self.invalidate(hwnd, self.row_rect(row));
                            return;
                        }
                    }
                }
            }
        }

        if self.page == Page::NowPlaying {
            if self.layout.lyric_buttons[0].inflate(2.0).contains(x, y) {
                self.pick_lyrics_requested = true;
                return;
            }
            if self.layout.lyric_buttons[1].inflate(2.0).contains(x, y) {
                self.lyrics_window_requested = true;
                return;
            }
            if self.layout.lyric_buttons[2].inflate(2.0).contains(x, y) {
                let on = !self.big_lyrics;
                self.set_big_lyrics(hwnd, on);
                return;
            }
            if self.layout.lyric_buttons[3].inflate(2.0).contains(x, y) {
                self.lyrics_editor_requested = true;
                return;
            }
            if self.layout.lyric_buttons[4].inflate(2.0).contains(x, y) {
                self.core.handle(Command::ReloadLyrics);
                self.invalidate(hwnd, self.layout.lyric_header);
                return;
            }
            if !self.big_lyrics {
                if self.layout.card_buttons[0].inflate(2.0).contains(x, y) {
                    self.toggle_play(hwnd);
                    return;
                }
                if self.layout.card_buttons[1].inflate(2.0).contains(x, y) {
                    self.pick_files_requested = true;
                    return;
                }
            }
        }

        if self.page == Page::Smart && self.page_sort_rect().inflate(2.0).contains(x, y) {
            let current = self.core.snapshot().library_smart_kind;
            let index = crate::core::library::SmartKind::ALL
                .iter()
                .position(|kind| *kind == current)
                .unwrap_or(0);
            let next = crate::core::library::SmartKind::ALL
                [(index + 1) % crate::core::library::SmartKind::ALL.len()];
            self.core.handle(Command::LibrarySmart(next));
            self.scroll = 0.0;
            self.invalidate_all(hwnd);
            return;
        }
        if self.page == Page::Duplicates && self.page_sort_rect().inflate(2.0).contains(x, y) {
            let current = self.core.snapshot().library_duplicate_mode;
            let index = DEDUPE_ORDER
                .iter()
                .position(|mode| *mode == current)
                .unwrap_or(0);
            let next = DEDUPE_ORDER[(index + 1) % DEDUPE_ORDER.len()];
            self.core.handle(Command::LibraryDuplicates(next));
            self.scroll = 0.0;
            self.invalidate_all(hwnd);
            return;
        }

        if self.page == Page::Statistics {
            let stats = layout::statistics(&self.layout);
            if stats.refresh.inflate(2.0).contains(x, y) {
                self.core.handle(Command::LibraryStats);
                self.invalidate_all(hwnd);
                return;
            }
            if stats.reset.inflate(2.0).contains(x, y) {
                // Confirmation opens a modal box; the shell does it after the
                // borrow ends.
                self.stats_reset_requested = true;
                return;
            }
            if stats.recent.contains(x, y) {
                let slot = ((y - stats.recent.top - 4.0) / layout::STATS_ROW_HEIGHT).floor();
                if slot >= 0.0 {
                    let index = slot as usize;
                    let has_row = self
                        .core
                        .snapshot()
                        .statistics
                        .is_some_and(|stats| index < stats.recently_played.len());
                    if has_row {
                        self.core.handle(Command::StatisticsPlay(index));
                        self.invalidate_all(hwnd);
                    }
                }
                return;
            }
        }

        if self.page == Page::Settings {
            let settings = layout::settings(&self.layout);
            for (index, rect) in settings.tabs.into_iter().enumerate() {
                if rect.inflate(2.0).contains(x, y) {
                    self.set_settings_tab(hwnd, SettingsTab::ALL[index]);
                    return;
                }
            }
            if self.settings_tab != SettingsTab::Library
                && self.settings_max_scroll() > 0.5
                && settings.scrollbar.inflate(2.0).contains(x, y)
            {
                // Drag start must run before the generic-row guard below: the
                // scrollbar lives outside `generic`, so the guard would return.
                self.ui.set_pressed(Some(Target::SettingsScrollbar));
                self.dragging_settings_scrollbar = true;
                unsafe {
                    SetCapture(hwnd);
                }
                self.settings_scroll_to_y(hwnd, y);
                self.invalidate(hwnd, settings.scrollbar);
                return;
            }
            // Clicks outside the settings body (player bar, header) must fall
            // through to the shared controls below.
            if self.settings_tab != SettingsTab::Library && settings.generic.contains(x, y) {
                let views = self.settings_views();
                let scroll = self.settings_scroll;
                for (index, view) in views.iter().enumerate() {
                    let Some(field) = self.settings_tab.fields().get(index).copied() else {
                        let offset = index - self.settings_tab.fields().len();
                        let id = self
                            .core
                            .snapshot()
                            .downloads
                            .get(offset)
                            .map(|row| row.id);
                        if let Some(id) = id {
                            let mut control = layout::setting_control(&settings, index);
                            control.top -= scroll;
                            control.bottom -= scroll;
                            if control.inflate(2.0).contains(x, y) {
                                self.core.handle(Command::CancelDownload(id));
                                self.invalidate(hwnd, settings.generic);
                                return;
                            }
                        }
                        continue;
                    };
                    let mut control = layout::setting_control(&settings, index);
                    control.top -= scroll;
                    control.bottom -= scroll;
                    if control.bottom <= settings.generic.top
                        || control.top >= settings.generic.bottom
                    {
                        continue;
                    }
                    match view.kind {
                        SettingKind::Toggle => {
                            if control.inflate(2.0).contains(x, y) {
                                self.apply_setting(hwnd, index, None, 0);
                                return;
                            }
                        }
                        SettingKind::Choice => {
                            if self.settings_compact_choice() {
                                if control.inflate(2.0).contains(x, y) {
                                    self.apply_setting(hwnd, index, None, 1);
                                    return;
                                }
                                continue;
                            }
                            for (option, rect) in
                                layout::segments(control, view.options.len())
                                    .into_iter()
                                    .enumerate()
                            {
                                if rect.inflate(2.0).contains(x, y) {
                                    self.apply_setting(hwnd, index, Some(option), 0);
                                    return;
                                }
                            }
                        }
                        SettingKind::Step => {
                            let (minus, value, plus) = layout::step_rects(control);
                            if minus.inflate(2.0).contains(x, y) {
                                self.apply_setting(hwnd, index, None, -1);
                                return;
                            }
                            if plus.inflate(2.0).contains(x, y) {
                                self.apply_setting(hwnd, index, None, 1);
                                return;
                            }
                            if Self::number_input_field(field)
                                && value.inflate(2.0).contains(x, y)
                            {
                                self.number_edit_request = Some(index);
                                return;
                            }
                        }
                        SettingKind::Color => {
                            if control.inflate(2.0).contains(x, y) {
                                self.apply_setting(hwnd, index, None, 0);
                                return;
                            }
                        }
                        SettingKind::Action => {
                            if view.active
                                && let Some(command) = Self::font_reset_command(field)
                                && layout::action_reset_rect(control)
                                    .inflate(2.0)
                                    .contains(x, y)
                            {
                                self.core.handle(command);
                                self.core.flush_settings();
                                self.invalidate_all(hwnd);
                                return;
                            }
                            if control.inflate(2.0).contains(x, y) {
                                self.apply_setting(hwnd, index, None, 0);
                                return;
                            }
                        }
                        SettingKind::Text => {
                            if control.inflate(2.0).contains(x, y) {
                                self.apply_setting(hwnd, index, None, 0);
                                return;
                            }
                        }
                    }
                }
                return;
            }
            let snapshot = self.core.snapshot();
            if settings.scan_toggle.inflate(2.0).contains(x, y) {
                self.core
                    .handle(Command::LibrarySetScanOnStart(!snapshot.library_scan_on_start));
                self.core.flush_settings();
                self.invalidate_all(hwnd);
                return;
            }
            if settings.watch_toggle.inflate(2.0).contains(x, y) {
                self.core
                    .handle(Command::LibrarySetAutoWatch(!snapshot.library_auto_watch));
                self.core.flush_settings();
                self.invalidate_all(hwnd);
                return;
            }
            if settings.follow_toggle.inflate(2.0).contains(x, y) {
                self.core
                    .handle(Command::LibrarySetFollowSymlinks(!snapshot.library_follow_symlinks));
                self.core.flush_settings();
                self.invalidate_all(hwnd);
                return;
            }
            for (index, rect) in layout::dedupe_options(&settings).into_iter().enumerate() {
                if rect.inflate(2.0).contains(x, y) {
                    self.core
                        .handle(Command::LibrarySetDedupeMode(DEDUPE_ORDER[index]));
                    self.core.flush_settings();
                    self.invalidate(hwnd, settings.dedupe_toggle.inflate(2.0));
                    return;
                }
            }
            if settings.add_folder.inflate(2.0).contains(x, y) {
                self.pick_folder_requested = true;
                return;
            }
            if settings.folders.contains(x, y) {
                let row = ((y - settings.folders.top) / layout::FOLDER_ROW_HEIGHT) as usize;
                let index = self.settings_folders_scroll + row;
                let row_rect = layout::folder_row(&settings, row);
                let remove = Rect::new(row_rect.right - 72.0, row_rect.top, row_rect.right, row_rect.bottom);
                if remove.contains(x, y)
                    && let Some(folder) = snapshot.library_folders.get(index).cloned()
                {
                    self.core.handle(Command::LibraryRemoveFolder(folder));
                    self.clamp_settings_scroll();
                    self.invalidate_all(hwnd);
                }
                return;
            }
        }

        if self.page == Page::Search {
            if self.layout.online_kind.inflate(2.0).contains(x, y) {
                if self.online_is_detail() {
                    self.core.handle(Command::OnlineBack);
                } else {
                    let next = match self.core.snapshot().online.kind {
                        crate::core::api::SearchKind::Song => crate::core::api::SearchKind::Album,
                        crate::core::api::SearchKind::Album => crate::core::api::SearchKind::Song,
                    };
                    self.core.handle(Command::OnlineSetKind(next));
                }
                self.scroll = 0.0;
                self.ui.set_hover(None);
                self.invalidate_all(hwnd);
                return;
            }
            if self.layout.online_queue_all.inflate(2.0).contains(x, y) {
                let query = self.core.snapshot().online.query.trim().to_string();
                if query.starts_with("http://") || query.starts_with("https://") {
                    self.core.handle(Command::OpenUrl(query));
                } else {
                    self.core.handle(Command::OnlineQueueAll);
                }
                return;
            }
            if self.layout.online_random.inflate(2.0).contains(x, y) {
                self.core.handle(Command::OnlineRandom);
                self.scroll = 0.0;
                self.invalidate(hwnd, self.layout.header);
                self.invalidate(hwnd, self.layout.online_list);
                return;
            }
            if self.online_page() > 0
                && self.layout.library_prev.inflate(2.0).contains(x, y)
            {
                self.turn_online_page(hwnd, -1);
                return;
            }
            if self.online_has_more()
                && self.layout.library_next.inflate(2.0).contains(x, y)
            {
                self.turn_online_page(hwnd, 1);
                return;
            }
        }

        if self.page == Page::Library {
            if self.layout.add_folder.inflate(2.0).contains(x, y) {
                self.pick_folder_requested = true;
                return;
            }
            if self.layout.rescan.inflate(2.0).contains(x, y) {
                // While a scan runs the same button cancels it between files.
                if self.core.snapshot().library_index_active {
                    self.core.handle(Command::LibraryCancelIndex);
                } else {
                    self.core.handle(Command::LibraryReindex);
                }
                self.invalidate(hwnd, self.layout.header);
                return;
            }
            if self.library_can_queue_all()
                && self.layout.library_queue_all.inflate(2.0).contains(x, y)
            {
                self.core.handle(Command::LibraryQueueAll);
                return;
            }
            if self.library_can_check_missing()
                && self.layout.library_check_missing.inflate(2.0).contains(x, y)
            {
                self.core.handle(Command::LibraryCheckMissing);
                self.invalidate(hwnd, self.layout.header);
                return;
            }
        }

        if self.page == Page::Queue {
            if self.layout.queue_import.inflate(2.0).contains(x, y) {
                self.queue_import_requested = true;
                return;
            }
            if self.queue_len() > 0 && self.layout.queue_export.inflate(2.0).contains(x, y) {
                self.queue_export_requested = true;
                return;
            }
            if self.queue_len() > 0 && self.layout.queue_clear.inflate(2.0).contains(x, y) {
                self.queue_clear_requested = true;
                return;
            }
        }

        // Playback settings flyout: a click outside closes it first.
        let over_settings = self.layout.player_settings.inflate(2.0).contains(x, y);
        if self.player_panel_open && !over_settings {
            self.set_player_panel(hwnd, false);
        }
        if over_settings {
            self.set_player_panel(hwnd, !self.player_panel_open);
            return;
        }
        if self.layout.shuffle.inflate(2.0).contains(x, y) {
            self.core.handle(Command::ToggleShuffle);
            self.invalidate(hwnd, self.layout.shuffle.inflate(2.0));
            // A paused player has no animation timer, so the queue page would
            // keep the old order until something else repainted it.
            self.refresh_queue_view();
            if self.page == Page::Queue {
                self.scroll = self.scroll.clamp(0.0, self.max_scroll());
                self.invalidate(hwnd, self.layout.header);
                self.invalidate(hwnd, self.layout.list);
                self.invalidate(hwnd, self.scrollbar_rect());
            }
            return;
        }
        if self.layout.repeat.inflate(2.0).contains(x, y) {
            self.core.handle(Command::CycleRepeat);
            self.invalidate(hwnd, self.layout.repeat.inflate(2.0));
            return;
        }
        if self.layout.volume_hit.width() > 0.0 && self.layout.volume_hit.contains(x, y) {
            self.dragging_volume = true;
            unsafe {
                SetCapture(hwnd);
            }
            self.set_volume_to_x(hwnd, x);
            return;
        }
        if self.layout.mute.inflate(2.0).contains(x, y) {
            self.core.handle(Command::ToggleMute);
            self.invalidate(hwnd, self.layout.volume_hit);
            self.invalidate(hwnd, self.layout.mute.inflate(2.0));
            return;
        }
        if self.layout.playlist.inflate(2.0).contains(x, y) {
            if self.page != Page::Queue {
                self.set_page(Page::Queue);
                self.invalidate_all(hwnd);
            }
            return;
        }
        if self.layout.mini_player.width() > 0.0
            && self.layout.mini_player.inflate(2.0).contains(x, y)
        {
            self.core.handle(Command::ToggleMiniPlayer);
            self.invalidate(hwnd, self.layout.mini_player.inflate(2.0));
            return;
        }
        if self.layout.desktop_lyrics.width() > 0.0
            && self.layout.desktop_lyrics.inflate(2.0).contains(x, y)
        {
            self.core.handle(Command::ToggleDesktopLyrics);
            self.invalidate(hwnd, self.layout.desktop_lyrics.inflate(2.0));
            return;
        }
        if self.layout.play.inflate(6.0).contains(x, y) {
            self.toggle_play(hwnd);
            return;
        }
        if self.layout.prev.contains(x, y) {
            self.prev(hwnd);
            return;
        }
        if self.layout.next.contains(x, y) {
            self.next(hwnd);
            return;
        }
        // Only the bar itself starts a drag; the padded hit area above/below
        // is hover-only. Otherwise a press in the padding used to seek and
        // then keep seeking on every mouse move, flooding the audio thread.
        if self.layout.progress.inflate(4.0).contains(x, y) {
            self.dragging_progress = true;
            self.drag_preview = Some(self.progress_time_at(x));
            unsafe {
                SetCapture(hwnd);
            }
            self.invalidate(hwnd, self.layout.progress_hit);
            self.invalidate(hwnd, self.layout.time_left);
            self.invalidate(hwnd, self.layout.time_right);
            return;
        }
        if matches!(
            self.page,
            Page::Queue | Page::Folders | Page::Smart | Page::Duplicates | Page::Search
                | Page::Discover | Page::Downloads
        ) && self.scrollbar_rect().contains(x, y)
        {
            self.dragging_scrollbar = true;
            unsafe {
                SetCapture(hwnd);
            }
            self.scroll_to_y(hwnd, y);
            return;
        }
        if self.page == Page::Library {
            // Footer pager: 上一页 / 下一页.
            let pages = self.library_page_count();
            if self.library_page > 0 && self.layout.library_prev.inflate(2.0).contains(x, y) {
                self.turn_library_page(hwnd, -1);
                return;
            }
            if self.library_page + 1 < pages
                && self.layout.library_next.inflate(2.0).contains(x, y)
            {
                self.turn_library_page(hwnd, 1);
                return;
            }
        }

        // Queue/library rows are the only pages with a clickable list. Without
        // this guard, clicks on the Now Playing card/panel fall through to the
        // stale list rect and play a queue row (restart, next/prev, or a
        // seemingly random track).
        if self.page == Page::Folders {
            if self.layout.folder_back.inflate(2.0).contains(x, y) {
                if self.folder_can_go_back() {
                    self.core.handle(Command::BrowseBack);
                    self.reset_folder_view(hwnd);
                }
                return;
            }
            if self.layout.folder_forward.inflate(2.0).contains(x, y) {
                if self.folder_can_go_forward() {
                    self.core.handle(Command::BrowseForward);
                    self.reset_folder_view(hwnd);
                }
                return;
            }
            if self.layout.folder_up.inflate(2.0).contains(x, y) {
                self.core.handle(Command::BrowseUp);
                self.scroll = 0.0;
                self.invalidate(hwnd, self.layout.header);
                self.invalidate(hwnd, self.layout.folder_list);
                self.invalidate(hwnd, self.layout.folder_scrollbar);
                return;
            }
            if self.layout.folder_refresh.inflate(2.0).contains(x, y) {
                let current = self.core.snapshot().browser.current.clone();
                self.core.handle(Command::BrowseTo(current));
                return;
            }
            if self.layout.folder_play_all.inflate(2.0).contains(x, y) {
                self.core.handle(Command::BrowsePlayAll);
                self.invalidate_track_change(hwnd, None, None);
                return;
            }
            if self.layout.folder_queue_all.inflate(2.0).contains(x, y) {
                self.core.handle(Command::BrowseAddAll);
                self.invalidate(hwnd, self.layout.header);
                return;
            }
        }

        if matches!(
            self.page,
            Page::Queue | Page::Library | Page::Folders | Page::Smart | Page::Duplicates
                | Page::Search | Page::Discover
        ) && self.list_area().contains(x, y)
        {
            let row = if self.online_wall_active() {
                let Some(index) = self.wall_index_at(x, y) else {
                    return;
                };
                index
            } else {
                self.row_at(y)
            };
            if row < self.row_count() {
                if self.page == Page::Folders {
                    let ctrl = unsafe { GetKeyState(0x11) } < 0;
                    let shift = unsafe { GetKeyState(0x10) } < 0;
                    if ctrl {
                        if !self.folder_selected.remove(&row) {
                            self.folder_selected.insert(row);
                        }
                        self.folder_anchor = Some(row);
                        self.invalidate(hwnd, self.layout.header);
                        self.invalidate(hwnd, self.layout.folder_list);
                        return;
                    }
                    if shift && let Some(anchor) = self.folder_anchor {
                        let (start, end) = (anchor.min(row), anchor.max(row));
                        self.folder_selected.clear();
                        for index in start..=end {
                            self.folder_selected.insert(index);
                        }
                        self.invalidate(hwnd, self.layout.header);
                        self.invalidate(hwnd, self.layout.folder_list);
                        return;
                    }
                    if !self.folder_selected.contains(&row) {
                        self.folder_selected.clear();
                        self.folder_selected.insert(row);
                    }
                    self.folder_anchor = Some(row);
                    // Playback waits for the release so the row can also be
                    // dragged onto the queue nav item.
                    self.folder_press = Some((row, x, y));
                    self.folder_drag = false;
                    self.folder_drop_over_queue = false;
                    self.invalidate(hwnd, self.layout.header);
                    self.invalidate(hwnd, self.layout.folder_list);
                    self.invalidate(hwnd, self.layout.folder_scrollbar);
                    return;
                }
                if self.page == Page::Queue && !self.queue_shows_shuffle() {
                    // The thumbnail is the drag handle: dragging reorders the
                    // queue without starting playback on every press.
                    let row_rect = self.row_rect(row);
                    let handle = Rect::new(
                        row_rect.left + 2.0,
                        row_rect.top,
                        row_rect.left + 46.0,
                        row_rect.bottom,
                    );
                    if handle.contains(x, y) {
                        self.queue_drag_from = Some(row);
                        self.queue_drag_target = Some(row);
                        self.queue_drag_active = false;
                        unsafe { SetCapture(hwnd) };
                        return;
                    }
                }
                let command = match self.page {
                    Page::Library => Command::LibraryPlay(row),
                    Page::Smart => Command::LibraryPlaySmart(row),
                    Page::Duplicates => {
                        let header = self
                            .core
                            .snapshot()
                            .library_duplicate_headers
                            .get(row)
                            .copied()
                            .unwrap_or(false);
                        if header {
                            return;
                        }
                        Command::LibraryPlayDuplicate(row)
                    }
                    Page::Search => Command::OnlinePlay(row),
                    Page::Discover => Command::DiscoverPlay(row),
                    _ => match self.queue_index(row) {
                        Some(index) => Command::PlayIndex(index),
                        None => return,
                    },
                };
                let old = self.core.snapshot().current_index;
                self.core.handle(command);
                let new = self.core.snapshot().current_index;
                self.invalidate_track_change(hwnd, old, new);
            }
        }
    }

    pub fn on_release(&mut self, hwnd: HWND) {
        if let Some((row, _, _)) = self.folder_press.take() {
            let dragged = self.folder_drag;
            let drop_queue = self.folder_drop_over_queue;
            self.folder_drag = false;
            self.folder_drop_over_queue = false;
            self.invalidate(hwnd, self.nav_rect(1).inflate(2.0));
            self.invalidate(hwnd, self.layout.header);
            if dragged {
                if drop_queue {
                    let mut paths = self.folder_selected_paths();
                    if paths.is_empty() {
                        if let Some(entry) = self
                            .core
                            .snapshot()
                            .browser
                            .entries
                            .get(row)
                            .filter(|entry| !entry.is_dir && !entry.is_playlist)
                        {
                            paths.push(entry.path.clone());
                        }
                    }
                    if !paths.is_empty() {
                        self.core.handle(Command::EnqueuePaths(paths));
                    }
                }
            } else {
                self.core.handle(Command::BrowsePlay(row));
                self.scroll = 0.0;
                self.invalidate(hwnd, self.layout.folder_list);
                self.invalidate(hwnd, self.layout.folder_scrollbar);
            }
        }
        if let Some(from_display) = self.queue_drag_from.take() {
            let target = self.queue_drag_target.take();
            let active = self.queue_drag_active;
            self.queue_drag_active = false;
            unsafe {
                let _ = ReleaseCapture();
            }
            if active
                && let Some(target_display) = target
                && target_display != from_display
                && let (Some(from), Some(to)) =
                    (self.queue_index(from_display), self.queue_index(target_display))
            {
                self.core.handle(Command::QueueMove { from, to });
                self.refresh_queue_view();
            } else if !active && target == Some(from_display) {
                // A click without movement keeps the old thumbnail behaviour.
                if let Some(index) = self.queue_index(from_display) {
                    let old = self.core.snapshot().current_index;
                    self.core.handle(Command::PlayIndex(index));
                    let new = self.core.snapshot().current_index;
                    self.invalidate_track_change(hwnd, old, new);
                }
            }
            self.invalidate(hwnd, self.layout.list);
            self.invalidate(hwnd, self.layout.scrollbar);
        }
        if self.dragging_progress
            && let Some(target) = self.drag_preview
        {
            // One engine seek per drag (captured press, seek on release).
            let target = target.max(0.0);
            self.core.handle(Command::SeekSecs(target as f64));
            self.drag_preview = None;
            self.last_position = Some(target);
            self.last_tick = None;
            self.invalidate(hwnd, self.layout.progress_hit);
            self.invalidate(hwnd, self.layout.time_left);
            self.invalidate(hwnd, self.layout.time_right);
        }
        let was_dragging =
            self.dragging_progress || self.dragging_scrollbar || self.dragging_volume;
        self.dragging_progress = false;
        self.dragging_scrollbar = false;
        self.dragging_settings_scrollbar = false;
        self.dragging_sidebar_scrollbar = false;
        self.dragging_volume = false;
        if was_dragging {
            unsafe {
                let _ = ReleaseCapture();
            }
        }
        if let Some(old) = self.ui.clear_pressed() {
            self.invalidate(hwnd, self.target_rect(old));
        }
    }

    /// Consume a pending "添加目录" click. The shell opens the dialog after the
    /// mutable borrow of `State` has ended.
    pub fn take_pick_folder(&mut self) -> bool {
        std::mem::take(&mut self.pick_folder_requested)
    }

    pub fn take_stream_cache_pick(&mut self) -> bool {
        std::mem::take(&mut self.stream_cache_pick_requested)
    }

    pub fn take_download_dir_pick(&mut self) -> bool {
        std::mem::take(&mut self.download_dir_pick_requested)
    }

    pub fn take_lyrics_panel_image_pick(&mut self) -> bool {
        std::mem::take(&mut self.lyrics_panel_image_pick_requested)
    }

    pub fn take_background_image_pick(&mut self) -> bool {
        std::mem::take(&mut self.background_image_pick_requested)
    }

    /// Consume a pending "打开文件" click (audio files).
    pub fn take_pick_files(&mut self) -> bool {
        std::mem::take(&mut self.pick_files_requested)
    }

    /// Consume a pending "选择歌词" click (.lrc/.srt).
    pub fn take_pick_lyrics(&mut self) -> bool {
        std::mem::take(&mut self.pick_lyrics_requested)
    }

    /// RGBA value displayed by a color swatch row.
    pub fn setting_color(&self, field: SettingField) -> Option<[u8; 4]> {
        let settings = &self.core.settings;
        let with_alpha = |rgb: [u8; 3]| [rgb[0], rgb[1], rgb[2], 255];
        let color = match field {
            SettingField::LyricsHighlightColor => with_alpha(settings.lyrics.highlight_color),
            SettingField::LyricsCurrentColor => with_alpha(settings.lyrics.current_color),
            SettingField::LyricsNormalColor => with_alpha(settings.lyrics.normal_color),
            SettingField::LyricsTranslationColor => with_alpha(settings.lyrics.translation_color),
            SettingField::LargeHighlightColor => with_alpha(settings.large_lyrics.highlight_color),
            SettingField::LargeCurrentColor => with_alpha(settings.large_lyrics.current_color),
            SettingField::LargeNormalColor => with_alpha(settings.large_lyrics.normal_color),
            SettingField::LargeTranslationColor => {
                with_alpha(settings.large_lyrics.translation_color)
            }
            SettingField::DesktopCurrentColor => settings.desktop_lyrics.current_color,
            SettingField::DesktopNextColor => settings.desktop_lyrics.next_color,
            SettingField::DesktopPrevColor => settings.desktop_lyrics.prev_color,
            SettingField::DesktopFooterColor => settings.desktop_lyrics.footer_color,
            SettingField::DesktopTranslationColor => settings.desktop_lyrics.translation_color,
            SettingField::DesktopOutlineColor => settings.desktop_lyrics.outline_color,
            SettingField::DesktopShadowColor => settings.desktop_lyrics.shadow_color,
            SettingField::DesktopBgColor => settings.desktop_lyrics.bg_color,
            SettingField::DesktopBgColor2 => settings.desktop_lyrics.bg_color2,
            SettingField::LyricsOutlineColor => with_alpha(settings.lyrics.outline_color),
            SettingField::LyricsShadowColor => with_alpha(settings.lyrics.shadow_color),
            SettingField::LyricsTransOutlineColor => {
                with_alpha(settings.lyrics.translation_outline_color)
            }
            SettingField::LyricsTransShadowColor => {
                with_alpha(settings.lyrics.translation_shadow_color)
            }
            SettingField::LargeOutlineColor => with_alpha(settings.large_lyrics.outline_color),
            SettingField::LargeShadowColor => with_alpha(settings.large_lyrics.shadow_color),
            SettingField::LargeTransOutlineColor => {
                with_alpha(settings.large_lyrics.translation_outline_color)
            }
            SettingField::LargeTransShadowColor => {
                with_alpha(settings.large_lyrics.translation_shadow_color)
            }
            SettingField::DesktopTransOutlineColor => {
                settings.desktop_lyrics.translation_outline_color
            }
            SettingField::DesktopTransShadowColor => {
                settings.desktop_lyrics.translation_shadow_color
            }
            SettingField::DesktopFooterOutlineColor => {
                settings.desktop_lyrics.footer_outline_color
            }
            SettingField::DesktopFooterShadowColor => {
                settings.desktop_lyrics.footer_shadow_color
            }
            SettingField::MiniPanelColor => with_alpha(settings.mini.panel_color),
            SettingField::MiniTextColor => with_alpha(settings.mini.text_color),
            SettingField::MiniAccentColor => with_alpha(settings.mini.accent_color),
            _ => return None,
        };
        Some(color)
    }

    /// Consume a pending desktop background-image pick.
    pub fn take_desktop_bg_pick(&mut self) -> bool {
        std::mem::take(&mut self.desktop_bg_pick_requested)
    }

    /// Consume a pending desktop font-family pick.
    pub fn take_desktop_font_pick(&mut self) -> bool {
        std::mem::take(&mut self.desktop_font_pick_requested)
    }

    pub fn take_desktop_trans_font_pick(&mut self) -> bool {
        std::mem::take(&mut self.desktop_trans_font_pick_requested)
    }

    pub fn take_desktop_footer_font_pick(&mut self) -> bool {
        std::mem::take(&mut self.desktop_footer_font_pick_requested)
    }

    pub fn take_lyrics_font_pick(&mut self) -> bool {
        std::mem::take(&mut self.lyrics_font_pick_requested)
    }

    pub fn take_theme_font_pick(&mut self) -> bool {
        std::mem::take(&mut self.theme_font_pick_requested)
    }

    pub fn take_large_font_pick(&mut self) -> bool {
        std::mem::take(&mut self.large_font_pick_requested)
    }

    /// Consume a pending font-size row click.
    pub fn take_number_edit_request(&mut self) -> Option<(usize, String)> {
        let index = self.number_edit_request.take()?;
        let field = self.settings_tab.fields().get(index).copied()?;
        let text = self.setting_number_text(field)?;
        Some((index, text))
    }

    /// Current numeric value of a step row (editor initial text), in the same
    /// unit the row displays (percent for opacity).
    pub fn setting_number_text(&self, field: SettingField) -> Option<String> {
        let settings = &self.core.settings;
        let playback = &settings.playback;
        let value = match field {
            SettingField::Crossfade => playback.crossfade_secs,
            SettingField::SleepTimerCustom => {
                self.core.sleep_selected_minutes().unwrap_or(0) as f32
            }
            SettingField::ReplayGainPreamp => playback.replaygain_preamp_db,
            SettingField::EqBand(index) => *playback.eq_bands.get(index as usize)?,
            SettingField::LyricsFontSize => settings.lyrics.font_size,
            SettingField::LyricsLineSpacing => settings.lyrics.line_spacing,
            SettingField::LargeLyricsFontSize => settings.large_lyrics.font_size,
            SettingField::LargeLineSpacing => settings.large_lyrics.line_spacing,
            SettingField::DesktopFontSize => settings.desktop_lyrics.font_size,
            SettingField::DesktopNextFontSize => settings.desktop_lyrics.next_font_size,
            SettingField::DesktopPrevFontSize => settings.desktop_lyrics.prev_font_size,
            SettingField::DesktopFooterFontSize => settings.desktop_lyrics.footer_font_size,
            SettingField::DesktopFrameRate => settings.desktop_lyrics.frame_rate as f32,
            SettingField::DesktopOpacity => settings.desktop_lyrics.opacity * 100.0,
            SettingField::DesktopRadius => settings.desktop_lyrics.radius as f32,
            SettingField::DesktopPadding => settings.desktop_lyrics.padding,
            SettingField::DesktopLineSpacing => settings.desktop_lyrics.line_spacing,
            SettingField::DesktopWidth => settings.desktop_lyrics.width,
            SettingField::DesktopHeight => settings.desktop_lyrics.height,
            SettingField::DesktopOutlineWidth => settings.desktop_lyrics.outline_width,
            SettingField::DesktopShadowOffset => settings.desktop_lyrics.shadow_offset,
            SettingField::MiniFontSize => settings.mini.lyrics_font_size,
            SettingField::MiniFrameRate => settings.mini.frame_rate as f32,
            SettingField::MiniOpacity => settings.mini.opacity * 100.0,
            SettingField::MiniWidth => settings.mini.width,
            SettingField::MiniHeight => settings.mini.height,
            SettingField::ThemeFontSize => settings.theme.font_size,
            SettingField::FrameRate => settings.performance.frame_rate as f32,
            SettingField::LyricsOffset => settings.lyrics.offset_ms as f32 / 1000.0,
            SettingField::LyricsOutlineWidth => settings.lyrics.outline_width,
            SettingField::LyricsShadowOffset => settings.lyrics.shadow_offset,
            SettingField::LyricsTransOutlineWidth => settings.lyrics.translation_outline_width,
            SettingField::LyricsTransShadowOffset => settings.lyrics.translation_shadow_offset,
            SettingField::LargeOutlineWidth => settings.large_lyrics.outline_width,
            SettingField::LargeShadowOffset => settings.large_lyrics.shadow_offset,
            SettingField::LargeTransOutlineWidth => {
                settings.large_lyrics.translation_outline_width
            }
            SettingField::LargeTransShadowOffset => {
                settings.large_lyrics.translation_shadow_offset
            }
            SettingField::DesktopTransOutlineWidth => {
                settings.desktop_lyrics.translation_outline_width
            }
            SettingField::DesktopTransShadowOffset => {
                settings.desktop_lyrics.translation_shadow_offset
            }
            SettingField::DesktopFooterOutlineWidth => {
                settings.desktop_lyrics.footer_outline_width
            }
            SettingField::DesktopFooterShadowOffset => {
                settings.desktop_lyrics.footer_shadow_offset
            }
            SettingField::DesktopBlurRadius => settings.desktop_lyrics.blur_radius,
            SettingField::IdleFrameRate => settings.performance.idle_frame_rate as f32,
            SettingField::ThumbnailCache => settings.performance.thumbnail_cache as f32,
            SettingField::TrimIdleSecs => settings.window.trim_idle_secs as f32,
            SettingField::ApiMinInterval => settings.network.api_min_interval_ms as f32,
            SettingField::AlbumPageSize => settings.network.album_page_size as f32,
            SettingField::NetworkTimeout => settings.network.timeout_secs as f32,
            SettingField::NetworkRetries => settings.network.retries as f32,
            SettingField::CacheLimitMb => settings.network.cache_limit_mb as f32,
            SettingField::DownloadConcurrency => settings.download.concurrency as f32,
            _ => return None,
        };
        let percent = matches!(
            field,
            SettingField::DesktopOpacity | SettingField::MiniOpacity
        );
        Some(if percent {
            format!("{value:.0}")
        } else {
            format!("{value}")
        })
    }

    pub fn take_text_edit_request(&mut self) -> Option<usize> {
        self.text_edit_request.take()
    }

    /// Raw display value of one settings row (used to seed the inline EDIT).
    pub fn setting_value_text(&self, index: usize) -> String {
        self.settings_views()
            .get(index)
            .map(|view| view.value.clone())
            .unwrap_or_default()
    }

    pub fn begin_text_edit(&mut self, hwnd: HWND, index: usize) {
        self.text_edit_row = Some(index);
        if let Some(area) = self.text_edit_value_rect(index) {
            self.invalidate(hwnd, area.inflate(2.0));
        }
    }

    pub fn is_text_edit_row(&self, index: usize) -> bool {
        self.text_edit_row == Some(index)
    }

    /// Control rect used by the inline text EDIT.
    pub fn text_edit_value_rect(&self, index: usize) -> Option<Rect> {
        let settings = layout::settings(&self.layout);
        let mut control = layout::setting_control(&settings, index);
        control.top -= self.settings_scroll;
        control.bottom -= self.settings_scroll;
        settings
            .generic
            .contains(control.left, control.top)
            .then_some(control)
            .filter(|control| {
                settings
                    .generic
                    .contains(control.right - 0.5, control.bottom - 0.5)
            })
    }

    pub fn commit_text_edit(&mut self, hwnd: HWND, text: &str) {
        let Some(index) = self.text_edit_row.take() else {
            return;
        };
        let Some(field) = self.settings_tab.fields().get(index).copied() else {
            return;
        };
        let text = text.trim().to_string();
        let command = match field {
            SettingField::SourceName => Command::SetSourceName(text),
            SettingField::SourceHeaders => {
                let headers = if text.is_empty() {
                    Vec::new()
                } else {
                    match serde_json::from_str::<Vec<crate::core::config::HeaderPair>>(&text) {
                        Ok(headers) => headers,
                        Err(err) => {
                            self.push_toast(
                                hwnd,
                                crate::trf!("请求头 JSON 无效: {}", err),
                                true,
                            );
                            self.invalidate_all(hwnd);
                            return;
                        }
                    }
                };
                Command::SetSourceHeaders(headers)
            }
            SettingField::SourceEndpoints => {
                let endpoints = if text.is_empty() {
                    crate::core::config::EndpointOverrides::default()
                } else {
                    match serde_json::from_str::<crate::core::config::EndpointOverrides>(&text) {
                        Ok(endpoints) => endpoints,
                        Err(err) => {
                            self.push_toast(
                                hwnd,
                                crate::trf!("端点 JSON 无效: {}", err),
                                true,
                            );
                            self.invalidate_all(hwnd);
                            return;
                        }
                    }
                };
                Command::SetSourceEndpoints(endpoints)
            }
            SettingField::SourceBaseUrl => Command::SetSourceBaseUrl(text),
            SettingField::ProxyUrl => Command::SetNetworkProxyUrl(text),
            SettingField::DownloadTemplate => Command::SetDownloadTemplate(text),
            _ => {
                self.invalidate_all(hwnd);
                return;
            }
        };
        self.core.handle(command);
        self.core.flush_settings();
        // The value cell and its frame return to the painted form.
        if let Some(area) = self.text_edit_value_rect(index) {
            self.invalidate(hwnd, area.inflate(2.0));
        }
        self.invalidate_all(hwnd);
    }

    pub fn cancel_text_edit(&mut self, hwnd: HWND) {
        if self.text_edit_row.take().is_some() {
            self.invalidate_all(hwnd);
        }
    }

    pub fn begin_number_edit(&mut self, hwnd: HWND, index: usize) {
        self.number_edit_row = Some(index);
        // The native EDIT covers the value text, but the accent frame lives
        // outside it: repaint that row now so the frame is actually there.
        if let Some(area) = self.number_edit_value_rect(index) {
            self.invalidate(hwnd, area.inflate(2.0));
        }
    }

    pub fn dragging_settings_scroll(&self) -> bool {
        self.dragging_settings_scrollbar
    }

    pub fn is_number_edit_row(&self, index: usize) -> bool {
        self.number_edit_row == Some(index)
    }

    /// Numeric step rows accept a typed value instead of only ± steps.
    pub fn number_input_field(field: SettingField) -> bool {
        matches!(
            field,
            SettingField::Crossfade
                | SettingField::ReplayGainPreamp
                | SettingField::SleepTimerCustom
                | SettingField::EqBand(_)
                | SettingField::LyricsFontSize
                | SettingField::LyricsLineSpacing
                | SettingField::LargeLyricsFontSize
                | SettingField::LargeLineSpacing
                | SettingField::DesktopFontSize
                | SettingField::DesktopNextFontSize
                | SettingField::DesktopPrevFontSize
                | SettingField::DesktopFooterFontSize
                | SettingField::DesktopFrameRate
                | SettingField::DesktopOpacity
                | SettingField::DesktopRadius
                | SettingField::DesktopPadding
                | SettingField::DesktopLineSpacing
                | SettingField::DesktopWidth
                | SettingField::DesktopHeight
                | SettingField::DesktopOutlineWidth
                | SettingField::DesktopShadowOffset
                | SettingField::MiniFontSize
                | SettingField::MiniFrameRate
                | SettingField::MiniOpacity
                | SettingField::MiniWidth
                | SettingField::FrameRate
                | SettingField::IdleFrameRate
                | SettingField::ThumbnailCache
                | SettingField::TrimIdleSecs
                | SettingField::LyricsOffset
                | SettingField::LyricsOutlineWidth
                | SettingField::LyricsShadowOffset
                | SettingField::LyricsTransOutlineWidth
                | SettingField::LyricsTransShadowOffset
                | SettingField::LargeOutlineWidth
                | SettingField::LargeShadowOffset
                | SettingField::LargeTransOutlineWidth
                | SettingField::LargeTransShadowOffset
                | SettingField::DesktopTransOutlineWidth
                | SettingField::DesktopTransShadowOffset
                | SettingField::DesktopFooterOutlineWidth
                | SettingField::DesktopFooterShadowOffset
                | SettingField::DesktopBlurRadius
                | SettingField::ThemeFontSize
                | SettingField::MiniHeight
                | SettingField::ApiMinInterval
                | SettingField::AlbumPageSize
                | SettingField::NetworkTimeout
                | SettingField::NetworkRetries
                | SettingField::CacheLimitMb
                | SettingField::DownloadConcurrency
        )
    }

    /// Rectangle of the value part inside a step row (device-independent).
    pub fn number_edit_value_rect(&self, index: usize) -> Option<Rect> {
        let settings = layout::settings(&self.layout);
        let mut control = layout::setting_control(&settings, index);
        control.top -= self.settings_scroll;
        control.bottom -= self.settings_scroll;
        let (_, value, _) = layout::step_rects(control);
        settings
            .generic
            .contains(value.left, value.top)
            .then_some(value)
            .filter(|value| {
                // A partially scrolled row would put the native EDIT over the
                // tab strip or the footer; close it instead.
                settings.generic.contains(value.right - 0.5, value.bottom - 0.5)
            })
    }

    /// Commit a typed font size. Invalid text is ignored (the editor closes).
    pub fn commit_number_edit(&mut self, hwnd: HWND, text: &str) {
        let Some(index) = self.number_edit_row.take() else {
            return;
        };
        let Some(field) = self.settings_tab.fields().get(index).copied() else {
            return;
        };
        let Ok(value) = text.trim().parse::<f32>() else {
            self.invalidate_all(hwnd);
            return;
        };
        if !value.is_finite() {
            self.invalidate_all(hwnd);
            return;
        }
        let command = match field {
            SettingField::Crossfade => Command::SetCrossfade(value),
            SettingField::SleepTimerCustom => {
                let minutes = value.round().clamp(0.0, 720.0) as u32;
                Command::SetSleepTimer((minutes > 0).then_some(minutes))
            }
            SettingField::ReplayGainPreamp => Command::SetReplayGainPreamp(value),
            SettingField::EqBand(index) => Command::SetEqBand(index as usize, value),
            SettingField::LyricsFontSize => Command::SetLyricsFontSize(value),
            SettingField::LyricsLineSpacing => Command::SetLyricsLineSpacing(value),
            SettingField::LargeLyricsFontSize => Command::SetLargeLyricsFontSize(value),
            SettingField::LargeLineSpacing => Command::SetLargeLyricsLineSpacing(value),
            SettingField::DesktopFontSize => Command::DesktopLyricsSetFontSize(value),
            SettingField::DesktopNextFontSize => Command::DesktopLyricsSetNextFontSize(value),
            SettingField::DesktopPrevFontSize => Command::DesktopLyricsSetPrevFontSize(value),
            SettingField::DesktopFooterFontSize => {
                Command::DesktopLyricsSetFooterFontSize(value)
            }
            SettingField::DesktopFrameRate => {
                Command::DesktopLyricsSetFrameRate(value.round().clamp(15.0, 120.0) as u32)
            }
            SettingField::DesktopOpacity => Command::DesktopLyricsSetOpacity(value / 100.0),
            SettingField::DesktopRadius => {
                Command::DesktopLyricsSetRadius(value.round() as i32)
            }
            SettingField::DesktopPadding => Command::DesktopLyricsSetPadding(value),
            SettingField::DesktopLineSpacing => Command::DesktopLyricsSetLineSpacing(value),
            SettingField::DesktopWidth => Command::DesktopLyricsSetSize {
                width: value,
                height: self.core.settings.desktop_lyrics.height,
            },
            SettingField::DesktopHeight => Command::DesktopLyricsSetSize {
                width: self.core.settings.desktop_lyrics.width,
                height: value,
            },
            SettingField::DesktopOutlineWidth => Command::SetDesktopLyricsOutlineWidth(value),
            SettingField::DesktopShadowOffset => Command::SetDesktopLyricsShadowOffset(value),
            SettingField::MiniFontSize => Command::SetMiniFontSize(value),
            SettingField::MiniFrameRate => {
                Command::SetMiniFrameRate(value.round().clamp(15.0, 120.0) as u32)
            }
            SettingField::MiniOpacity => Command::SetMiniOpacity(value / 100.0),
            SettingField::MiniWidth => Command::SetMiniWidth(value),
            SettingField::FrameRate => Command::SetFrameRate(value.round() as u32),
            SettingField::IdleFrameRate => Command::SetIdleFrameRate(value.round() as u32),
            SettingField::ThumbnailCache => Command::SetThumbnailCache(value.round() as u32),
            SettingField::TrimIdleSecs => Command::SetTrimIdleSecs(value.round() as u64),
            SettingField::LyricsOffset => {
                Command::SetLyricsOffset((value * 1000.0).round() as i64)
            }
            SettingField::LyricsOutlineWidth => Command::SetLyricsOutlineWidth(value),
            SettingField::LyricsShadowOffset => Command::SetLyricsShadowOffset(value),
            SettingField::LyricsTransOutlineWidth => {
                Command::SetLyricsTranslationOutlineWidth(value)
            }
            SettingField::LyricsTransShadowOffset => {
                Command::SetLyricsTranslationShadowOffset(value)
            }
            SettingField::LargeOutlineWidth => Command::SetLargeLyricsOutlineWidth(value),
            SettingField::LargeShadowOffset => Command::SetLargeLyricsShadowOffset(value),
            SettingField::LargeTransOutlineWidth => {
                Command::SetLargeLyricsTranslationOutlineWidth(value)
            }
            SettingField::LargeTransShadowOffset => {
                Command::SetLargeLyricsTranslationShadowOffset(value)
            }
            SettingField::DesktopTransOutlineWidth => {
                Command::SetDesktopLyricsTranslationOutlineWidth(value)
            }
            SettingField::DesktopTransShadowOffset => {
                Command::SetDesktopLyricsTranslationShadowOffset(value)
            }
            SettingField::DesktopFooterOutlineWidth => {
                Command::SetDesktopLyricsFooterOutlineWidth(value)
            }
            SettingField::DesktopFooterShadowOffset => {
                Command::SetDesktopLyricsFooterShadowOffset(value)
            }
            SettingField::DesktopBlurRadius => Command::SetDesktopLyricsBlurRadius(value),
            SettingField::ThemeFontSize => Command::SetThemeFontSize(value),
            SettingField::MiniHeight => Command::SetMiniHeight(value),
            SettingField::ApiMinInterval => {
                Command::SetApiMinInterval(value.round().max(0.0) as u64)
            }
            SettingField::AlbumPageSize => {
                Command::SetAlbumPageSize(value.round().max(0.0) as u32)
            }
            SettingField::NetworkTimeout => {
                Command::SetNetworkTimeout(value.round().max(0.0) as u64)
            }
            SettingField::NetworkRetries => {
                Command::SetNetworkRetries(value.round().max(0.0) as u32)
            }
            SettingField::CacheLimitMb => {
                Command::SetCacheLimitMb(value.round().max(0.0) as u64)
            }
            SettingField::DownloadConcurrency => {
                Command::SetDownloadConcurrency(value.round().clamp(1.0, 4.0) as u8)
            }
            _ => return,
        };
        self.core.handle(command);
        self.core.flush_settings();
        self.invalidate_all(hwnd);
    }

    pub fn cancel_number_edit(&mut self, hwnd: HWND) {
        if self.number_edit_row.take().is_some() {
            self.invalidate_all(hwnd);
        }
    }

    /// Consume the color row waiting for the native picker.
    pub fn take_color_request(&mut self) -> Option<(SettingField, [u8; 3])> {
        let field = self.color_request.take()?;
        let color = self.setting_color(field)?;
        Some((field, [color[0], color[1], color[2]]))
    }

    /// Apply a color chosen by the native picker.
    pub fn apply_color(&mut self, hwnd: HWND, field: SettingField, color: [u8; 3]) {
        let command = match field {
            SettingField::LyricsHighlightColor => Command::SetLyricsHighlight(color),
            SettingField::LyricsPanelColor => Command::SetLyricsPanelColor(Some(color)),
            SettingField::LyricsPanelColor2 => Command::SetLyricsPanelColor2(color),
            SettingField::LyricsPanelBorderColor => {
                Command::SetLyricsPanelBorderColor(Some(color))
            }
            SettingField::LyricsCurrentColor => Command::SetLyricsCurrentColor(color),
            SettingField::LyricsNormalColor => Command::SetLyricsNormalColor(color),
            SettingField::LyricsTranslationColor => Command::SetLyricsTranslationColor(color),
            SettingField::LargeHighlightColor => Command::SetLargeLyricsHighlight(color),
            SettingField::LargeCurrentColor => Command::SetLargeLyricsCurrentColor(color),
            SettingField::LargeNormalColor => Command::SetLargeLyricsNormalColor(color),
            SettingField::LargeTranslationColor => Command::SetLargeLyricsTranslationColor(color),
            SettingField::DesktopCurrentColor => Command::SetDesktopLyricsColor(0, color),
            SettingField::DesktopNextColor => Command::SetDesktopLyricsColor(1, color),
            SettingField::DesktopPrevColor => Command::SetDesktopLyricsColor(2, color),
            SettingField::DesktopFooterColor => Command::SetDesktopLyricsColor(3, color),
            SettingField::DesktopTranslationColor => Command::SetDesktopLyricsColor(4, color),
            SettingField::DesktopOutlineColor => Command::SetDesktopLyricsColor(5, color),
            SettingField::DesktopShadowColor => Command::SetDesktopLyricsColor(6, color),
            SettingField::DesktopBgColor => Command::SetDesktopLyricsColor(7, color),
            SettingField::DesktopBgColor2 => Command::SetDesktopLyricsColor(8, color),
            SettingField::LyricsOutlineColor => Command::SetLyricsOutlineColor(color),
            SettingField::LyricsShadowColor => Command::SetLyricsShadowColor(color),
            SettingField::LyricsTransOutlineColor => {
                Command::SetLyricsTranslationOutlineColor(color)
            }
            SettingField::LyricsTransShadowColor => {
                Command::SetLyricsTranslationShadowColor(color)
            }
            SettingField::LargeOutlineColor => Command::SetLargeLyricsOutlineColor(color),
            SettingField::LargeShadowColor => Command::SetLargeLyricsShadowColor(color),
            SettingField::LargeTransOutlineColor => {
                Command::SetLargeLyricsTranslationOutlineColor(color)
            }
            SettingField::LargeTransShadowColor => {
                Command::SetLargeLyricsTranslationShadowColor(color)
            }
            SettingField::DesktopTransOutlineColor => {
                Command::SetDesktopLyricsColor(9, color)
            }
            SettingField::DesktopTransShadowColor => {
                Command::SetDesktopLyricsColor(10, color)
            }
            SettingField::DesktopFooterOutlineColor => {
                Command::SetDesktopLyricsColor(11, color)
            }
            SettingField::DesktopFooterShadowColor => {
                Command::SetDesktopLyricsColor(12, color)
            }
            SettingField::MiniPanelColor => Command::SetMiniPanelColor(color),
            SettingField::MiniTextColor => Command::SetMiniTextColor(color),
            SettingField::MiniAccentColor => Command::SetMiniAccentColor(color),
            _ => return,
        };
        self.core.handle(command);
        self.core.flush_settings();
        self.invalidate_all(hwnd);
    }

    /// Consume a pending "独立窗口" click.
    pub fn take_lyrics_window_toggle(&mut self) -> bool {
        std::mem::take(&mut self.lyrics_window_requested)
    }

    /// Consume a pending "编辑" click / Ctrl+E.
    pub fn take_lyrics_editor(&mut self) -> bool {
        std::mem::take(&mut self.lyrics_editor_requested)
    }

    /// Queue page header shortcuts.
    pub fn take_queue_import(&mut self) -> bool {
        std::mem::take(&mut self.queue_import_requested)
    }

    pub fn take_queue_export(&mut self) -> bool {
        std::mem::take(&mut self.queue_export_requested)
    }

    pub fn take_queue_clear(&mut self) -> bool {
        std::mem::take(&mut self.queue_clear_requested)
    }

    pub fn take_stats_reset(&mut self) -> bool {
        std::mem::take(&mut self.stats_reset_requested)
    }

    pub fn take_settings_action(&mut self) -> Option<SettingsAction> {
        self.settings_action.take()
    }

    pub fn on_wheel(&mut self, hwnd: HWND, wheel_delta: f32, over_list: bool) {
        // A wheel while dragging is fine; dragging still owns the thumb.
        if self.page == Page::Settings {
            if self.settings_tab != SettingsTab::Library {
                if !over_list {
                    return;
                }
                let max = self.settings_max_scroll();
                let step = -(wheel_delta / 120.0) * layout::SETTINGS_ROW_HEIGHT * 2.0;
                let next = (self.settings_scroll + step).clamp(0.0, max);
                if (next - self.settings_scroll).abs() < 0.5 {
                    return;
                }
                self.settings_scroll = next;
                let settings = layout::settings(&self.layout);
                self.invalidate(hwnd, settings.generic);
                // The thumb lives outside `generic`; without this it only
                // refreshes when a mouse move invalidates it for hover.
                self.invalidate(hwnd, settings.scrollbar);
                return;
            }
            if !over_list {
                return;
            }
            let folders = self.core.snapshot().library_folders.len();
            let visible = self.settings_folder_rows();
            let max = folders.saturating_sub(visible);
            let step = -((wheel_delta / 120.0).round() as i32) * 3;
            let next = (self.settings_folders_scroll as i32 + step).clamp(0, max as i32) as usize;
            if next != self.settings_folders_scroll {
                self.settings_folders_scroll = next;
                self.invalidate(hwnd, layout::settings(&self.layout).folders);
            }
            return;
        }
        if self.page == Page::NowPlaying || !over_list {
            return;
        }
        if self.page == Page::Library {
            // The library is paged with the footer buttons; a wheel does not
            // scroll or turn pages.
            return;
        }
        let rows = -(wheel_delta / 120.0) * 3.0;
        let step = if self.online_wall_active() {
            let (_, _, row_height, _) = self.wall_metrics();
            row_height
        } else {
            self.row_height()
        };
        let next = (self.scroll + rows * step).clamp(0.0, self.max_scroll());
        if (next - self.scroll).abs() < 0.5 {
            return;
        }
        self.scroll = next;
        self.ui.set_hover(None);
        self.invalidate(hwnd, self.list_area());
        self.invalidate(hwnd, self.scrollbar_rect());
        self.request_visible_covers();
    }

    /// Reset the virtual list to the top (new search or sort).
    pub fn reset_list_scroll(&mut self, hwnd: HWND) {
        self.ui.set_hover(None);
        self.library_page = 0;
        if self.scroll == 0.0 {
            return;
        }
        self.scroll = 0.0;
        self.invalidate(hwnd, self.list_area());
        self.invalidate(hwnd, self.scrollbar_rect());
    }

    /// Controls reachable with Tab on the current page. Zero-sized controls
    /// (hidden sliders/buttons) are skipped so focus never lands on nothing.
    pub(crate) fn focus_targets(&self) -> Vec<Target> {
        let mut out: Vec<Target> =
            (0..self.layout.nav.len()).map(Target::Nav).collect();
        out.extend([
            Target::Shuffle,
            Target::Repeat,
            Target::Prev,
            Target::Play,
            Target::Next,
            Target::Volume,
            Target::Progress,
            Target::Mute,
            Target::PlaySettings,
            Target::Playlist,
            Target::MiniPlayer,
            Target::DesktopLyrics,
        ]);
        match self.page {
            Page::NowPlaying => out.extend([
                Target::CardPlay,
                Target::CardOpen,
                Target::LyricsPick,
                Target::LyricsWindow,
                Target::LyricsBig,
                Target::LyricsEdit,
                Target::LyricsReload,
            ]),
            Page::Queue => out.extend([Target::QueueImport, Target::QueueExport, Target::QueueClear]),
            Page::Library => out.extend([
                Target::Sort,
                Target::AddFolder,
                Target::Rescan,
                Target::LibraryQueueAll,
                Target::LibraryCheckMissing,
                Target::LibraryPrev,
                Target::LibraryNext,
            ]),
            Page::Folders => out.extend([
                Target::FolderBack,
                Target::FolderForward,
                Target::FolderUp,
                Target::FolderRefresh,
                Target::FolderPlayAll,
                Target::FolderQueueAll,
            ]),
            Page::Search => out.extend([
                Target::OnlineKind,
                Target::OnlineBack,
                Target::OnlineQueueAll,
                Target::OnlineRandom,
                Target::OnlineWall,
                Target::OnlinePrev,
                Target::OnlineNext,
            ]),
            Page::Discover => out.push(Target::DiscoverRefresh),
            Page::Statistics => {
                out.extend([Target::StatsRefresh, Target::StatsReset]);
                let recent = self
                    .core
                    .snapshot()
                    .statistics
                    .map(|stats| stats.recently_played.len())
                    .unwrap_or(0);
                let stats = layout::statistics(&self.layout);
                let visible =
                    (stats.recent.height() / layout::STATS_ROW_HEIGHT).floor().max(0.0) as usize;
                out.extend((0..recent.min(visible)).map(Target::StatsRecent));
            }
            Page::Downloads => out.extend([Target::DownloadsCleanPart, Target::DownloadsClearFinished]),
            Page::Smart => out.extend([Target::SmartKind]),
            Page::Duplicates => out.extend([Target::DuplicateMode]),
            // Focused settings rows would require inline editors; tabs are the
            // useful keyboard navigation target on this page.
            Page::Settings => {
                out.extend((0..SettingsTab::ALL.len()).map(Target::SettingsTab));
                if self.settings_tab != SettingsTab::Library {
                    out.extend(
                        (0..self.settings_tab.fields().len()).map(Target::SettingsRow),
                    );
                }
            }
        }
        if matches!(
            self.page,
            Page::Queue
                | Page::Library
                | Page::Folders
                | Page::Search
                | Page::Discover
                | Page::Smart
                | Page::Duplicates
                | Page::Downloads
        ) {
            let (first, last) = self.visible_rows();
            out.extend((first..last).map(Target::Row));
        }
        out.retain(|target| {
            let rect = self.target_rect(*target);
            rect.width() > 0.0
                && rect.height() > 0.0
                && (!matches!(*target, Target::Nav(_))
                    || self.layout.nav_viewport.intersects(rect))
        });
        out
    }

    pub fn sidebar_max_scroll(&self) -> f32 {
        (self.layout.nav_content_height - self.layout.nav_viewport.height()).max(0.0)
    }

    pub fn sidebar_scroll(&self) -> f32 {
        self.sidebar_scroll
    }

    /// Navigation row in window coordinates (scrolled inside the viewport).
    pub fn nav_rect(&self, index: usize) -> Rect {
        let mut rect = self.layout.nav[index];
        rect.top -= self.sidebar_scroll;
        rect.bottom -= self.sidebar_scroll;
        rect
    }

    fn scroll_sidebar_by(&mut self, hwnd: HWND, delta: f32) {
        let max = self.sidebar_max_scroll();
        if max <= 0.5 {
            return;
        }
        let next = (self.sidebar_scroll + delta).clamp(0.0, max);
        if (next - self.sidebar_scroll).abs() < 0.5 {
            return;
        }
        self.sidebar_scroll = next;
        self.invalidate(hwnd, self.layout.nav_viewport);
        self.invalidate(hwnd, self.layout.nav_scrollbar.inflate(2.0));
    }

    /// Mouse wheel over the sidebar. Returns true when it consumed the wheel.
    pub fn sidebar_wheel(&mut self, hwnd: HWND, wheel_delta: f32) -> bool {
        if self.sidebar_max_scroll() <= 0.5 {
            return false;
        }
        self.scroll_sidebar_by(hwnd, -(wheel_delta / 120.0) * 38.0);
        true
    }

    fn sidebar_scroll_to_y(&mut self, hwnd: HWND, y: f32) {
        let bar = self.layout.nav_scrollbar;
        let max = self.sidebar_max_scroll();
        if max <= 0.5 || bar.height() <= 0.0 {
            return;
        }
        let view = self.layout.nav_viewport.height();
        let content = self.layout.nav_content_height;
        if content <= view {
            return;
        }
        let thumb = (view * view / content).max(24.0).min(bar.height());
        let travel = (bar.height() - thumb).max(1.0);
        let ratio = ((y - bar.top - thumb * 0.5) / travel).clamp(0.0, 1.0);
        let target = (ratio * max).clamp(0.0, max);
        self.scroll_sidebar_by(hwnd, target - self.sidebar_scroll);
    }

    /// Keep the focused/selected navigation item inside the viewport.
    fn ensure_nav_visible(&mut self, hwnd: HWND, index: usize) {
        if self.sidebar_max_scroll() <= 0.5 || index >= self.layout.nav.len() {
            return;
        }
        let viewport = self.layout.nav_viewport;
        let item = self.layout.nav[index];
        let top = item.top - self.sidebar_scroll;
        let target = if top < viewport.top {
            self.sidebar_scroll - (viewport.top - top)
        } else if top + item.height() > viewport.bottom {
            self.sidebar_scroll + (top + item.height() - viewport.bottom)
        } else {
            return;
        };
        self.scroll_sidebar_by(hwnd, target - self.sidebar_scroll);
    }

    /// Scroll a list row into the visible window (no-op on paged pages).
    fn ensure_row_visible(&mut self, hwnd: HWND, index: usize) {
        if self.scrollbar_rect().height() <= 0.0 || index >= self.row_count() {
            return;
        }
        let area = self.list_area();
        let rect = self.row_rect(index);
        let target = if rect.top < area.top {
            self.scroll - (area.top - rect.top)
        } else if rect.bottom > area.bottom {
            self.scroll + (rect.bottom - area.bottom)
        } else {
            return;
        };
        let next = target.clamp(0.0, self.max_scroll());
        if (next - self.scroll).abs() < 0.5 {
            return;
        }
        self.scroll = next;
        self.invalidate(hwnd, area);
        self.invalidate(hwnd, self.scrollbar_rect());
    }

    /// Scroll a generic settings row into view.
    fn ensure_setting_row_visible(&mut self, hwnd: HWND, index: usize) {
        if self.settings_tab == SettingsTab::Library {
            return;
        }
        let settings = layout::settings(&self.layout);
        let mut control = layout::setting_control(&settings, index);
        control.top -= self.settings_scroll;
        control.bottom -= self.settings_scroll;
        let target = if control.top < settings.generic.top {
            self.settings_scroll - (settings.generic.top - control.top)
        } else if control.bottom > settings.generic.bottom {
            self.settings_scroll + (control.bottom - settings.generic.bottom)
        } else {
            return;
        };
        let max = self.settings_max_scroll();
        let next = target.clamp(0.0, max);
        if (next - self.settings_scroll).abs() < 0.5 {
            return;
        }
        self.settings_scroll = next;
        self.invalidate(hwnd, settings.generic);
        self.invalidate(hwnd, settings.scrollbar);
    }

    fn focus_adjacent_row(&mut self, hwnd: HWND, delta: i32) {
        let Some(Target::Row(index)) = self.keyboard_focus else {
            return;
        };
        let count = self.row_count();
        if count == 0 {
            return;
        }
        // Moving past the visible window scrolls the list; navigation is not
        // limited to the rows that happened to be visible when Tab entered.
        let next = (index as i32 + delta).clamp(0, count as i32 - 1) as usize;
        self.focus_target(hwnd, Target::Row(next));
    }

    fn adjust_setting_row(&mut self, hwnd: HWND, index: usize, delta: i32) {
        let Some(view) = self.settings_views().get(index).cloned() else {
            return;
        };
        match view.kind {
            SettingKind::Step => self.apply_setting(hwnd, index, None, delta),
            SettingKind::Choice => {
                let count = view.options.len().max(1);
                let selected = view.selected.unwrap_or(0) as i32;
                let next = (selected + delta).rem_euclid(count as i32) as usize;
                self.apply_setting(hwnd, index, Some(next), 0);
            }
            SettingKind::Toggle => {
                if delta > 0 {
                    self.apply_setting(hwnd, index, None, 1);
                }
            }
            _ => {}
        }
    }

    /// Enter/Space on a focused settings row. Controls that open native
    /// dialogs are activated through a synthetic click so the shell's normal
    /// request handling runs.
    fn activate_setting_row(&mut self, hwnd: HWND, index: usize) {
        let Some(view) = self.settings_views().get(index).cloned() else {
            return;
        };
        match view.kind {
            SettingKind::Toggle => self.apply_setting(hwnd, index, None, 1),
            SettingKind::Choice => self.apply_setting(hwnd, index, None, 1),
            SettingKind::Step => self.apply_setting(hwnd, index, None, 1),
            SettingKind::Color | SettingKind::Action | SettingKind::Text => {
                self.activate_target(hwnd, Target::SettingsRow(index));
            }
        }
    }

    pub fn focus_rect(&self) -> Option<Rect> {
        // The drawn ring and the invalidated rectangle must be identical:
        // an extra inflation here left a 1 DIP sliver of the old XOR ring
        // behind every time Tab moved the focus.
        self.keyboard_focus.map(|target| self.target_rect(target))
    }

    pub(crate) fn keyboard_focus(&self) -> Option<Target> {
        self.keyboard_focus
    }

    /// Move the focus ring without activating. Used by UIA SetFocus.
    pub(crate) fn focus_target(&mut self, hwnd: HWND, target: Target) {
        if self.keyboard_focus == Some(target) {
            return;
        }
        if let Some(old) = self.keyboard_focus {
            self.invalidate(hwnd, self.target_rect(old));
        }
        self.keyboard_focus = Some(target);
        match target {
            Target::Nav(index) => self.ensure_nav_visible(hwnd, index),
            Target::Row(index) => self.ensure_row_visible(hwnd, index),
            Target::SettingsRow(index) => self.ensure_setting_row_visible(hwnd, index),
            _ => {}
        }
        self.invalidate(hwnd, self.target_rect(target));
    }

    fn move_focus(&mut self, hwnd: HWND, forward: bool) {
        let targets = self.focus_targets();
        if targets.is_empty() {
            return;
        }
        let current = self
            .keyboard_focus
            .and_then(|focus| targets.iter().position(|target| *target == focus));
        let next = match current {
            Some(index) if forward => (index + 1) % targets.len(),
            Some(index) => (index + targets.len() - 1) % targets.len(),
            None if forward => 0,
            None => targets.len() - 1,
        };
        let next = Some(targets[next]);
        let old = self.keyboard_focus;
        if next == old {
            return;
        }
        if let Some(target) = next {
            self.focus_target(hwnd, target);
        }
    }

    /// Activate the focused control by posting a real click at its center.
    /// The normal click path then runs, including dialogs and native editors.
    fn activate_focus(&mut self, hwnd: HWND) {
        let Some(target) = self.keyboard_focus else {
            return;
        };
        self.activate_target(hwnd, target);
    }

    /// Activate any visible target exactly like a mouse click.
    pub(crate) fn activate_target(&mut self, hwnd: HWND, target: Target) {
        let rect = self.target_rect(target);
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        let scale = self.dpi / 96.0;
        let x = ((rect.left + rect.right) * 0.5 * scale).round() as i32;
        let y = ((rect.top + rect.bottom) * 0.5 * scale).round() as i32;
        let packed = (((y as u16 as u32) << 16) | (x as u16 as u32)) as isize;
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_LBUTTONDOWN, WPARAM(1), LPARAM(packed));
            let _ = PostMessageW(Some(hwnd), WM_LBUTTONUP, WPARAM(0), LPARAM(packed));
        }
    }

    pub fn on_key(&mut self, hwnd: HWND, virtual_key: u16, ctrl: bool, shift: bool) {
        // Tab moves the accessibility focus ring through the controls of the
        // current page; Shift+Tab walks backwards.
        if virtual_key == 0x09 {
            self.move_focus(hwnd, !shift);
            return;
        }
        // A focused slider takes the arrow keys before the page-level
        // scroll/seek shortcuts do.
        if let Some(focus) = self.keyboard_focus {
            match focus {
                Target::Volume => {
                    if matches!(virtual_key, 0x25 | 0x28) {
                        self.change_volume(hwnd, -0.05);
                        return;
                    }
                    if matches!(virtual_key, 0x27 | 0x26) {
                        self.change_volume(hwnd, 0.05);
                        return;
                    }
                }
                Target::Progress => {
                    if virtual_key == 0x25 {
                        self.seek_by(hwnd, -(self.core.settings.playback.seek_step_secs as f32));
                        return;
                    }
                    if virtual_key == 0x27 {
                        self.seek_by(hwnd, self.core.settings.playback.seek_step_secs as f32);
                        return;
                    }
                }
                _ => {}
            }
        }
        // Settings rows use direct activation (so Choice cycles instead of
        // selecting the middle segment), while rows behave like a click.
        if let Some(focus) = self.keyboard_focus {
            match focus {
                Target::SettingsRow(index) => {
                    if matches!(virtual_key, 0x26 | 0x28) {
                        let delta = if virtual_key == 0x26 { -1 } else { 1 };
                        let next = index as i32 + delta;
                        let count = self.settings_tab.fields().len() as i32;
                        if (0..count).contains(&next) {
                            self.focus_target(hwnd, Target::SettingsRow(next as usize));
                        }
                        return;
                    }
                    if matches!(virtual_key, 0x25 | 0x27) {
                        self.adjust_setting_row(
                            hwnd,
                            index,
                            if virtual_key == 0x25 { -1 } else { 1 },
                        );
                        return;
                    }
                    if matches!(virtual_key, 0x0D | 0x20) {
                        self.activate_setting_row(hwnd, index);
                        return;
                    }
                    if virtual_key == 0x24 {
                        self.focus_target(hwnd, Target::SettingsRow(0));
                        return;
                    }
                    if virtual_key == 0x23 {
                        let last = self.settings_tab.fields().len().saturating_sub(1);
                        self.focus_target(hwnd, Target::SettingsRow(last));
                        return;
                    }
                }
                Target::StatsRecent(index) => {
                    let count = self
                        .core
                        .snapshot()
                        .statistics
                        .map(|stats| stats.recently_played.len())
                        .unwrap_or(0);
                    if count == 0 {
                        return;
                    }
                    if matches!(virtual_key, 0x26 | 0x28) {
                        let next =
                            (index as i32 + if virtual_key == 0x26 { -1 } else { 1 })
                                .clamp(0, count as i32 - 1);
                        self.focus_target(hwnd, Target::StatsRecent(next as usize));
                        return;
                    }
                    if virtual_key == 0x24 || virtual_key == 0x23 {
                        let row = if virtual_key == 0x24 { 0 } else { count - 1 };
                        self.focus_target(hwnd, Target::StatsRecent(row));
                        return;
                    }
                    if matches!(virtual_key, 0x0D | 0x20) {
                        self.activate_target(hwnd, focus);
                        return;
                    }
                }
                Target::Row(_) => {
                    if matches!(virtual_key, 0x26 | 0x28) {
                        self.focus_adjacent_row(
                            hwnd,
                            if virtual_key == 0x26 { -1 } else { 1 },
                        );
                        return;
                    }
                    if virtual_key == 0x24 || virtual_key == 0x23 {
                        let count = self.row_count();
                        if count > 0 {
                            let row = if virtual_key == 0x24 { 0 } else { count - 1 };
                            self.focus_target(hwnd, Target::Row(row));
                        }
                        return;
                    }
                    if matches!(virtual_key, 0x0D | 0x20) {
                        self.activate_target(hwnd, focus);
                        return;
                    }
                }
                _ => {}
            }
        }
        // Home/End on the focused sliders.
        if let Some(focus) = self.keyboard_focus {
            match (focus, virtual_key) {
                (Target::Volume, 0x24) => {
                    self.set_volume(0.0);
                    self.invalidate(hwnd, self.layout.volume_hit);
                    return;
                }
                (Target::Volume, 0x23) => {
                    self.set_volume(1.0);
                    self.invalidate(hwnd, self.layout.volume_hit);
                    return;
                }
                (Target::Progress, 0x24) => {
                    self.seek_to_secs(hwnd, 0.0);
                    return;
                }
                (Target::Progress, 0x23) => {
                    self.seek_to_secs(hwnd, self.duration() as f64);
                    return;
                }
                _ => {}
            }
        }
        // Enter/Space activates the focused control, exactly like a real click
        // (including dialogs and native pickers). Sliders only respond to the
        // arrow keys above; clicking their center would jump the value.
        if self.keyboard_focus.is_some() && matches!(virtual_key, 0x0D | 0x20) {
            if matches!(
                self.keyboard_focus,
                Some(Target::Volume) | Some(Target::Progress)
            ) {
                return;
            }
            self.activate_focus(hwnd);
            return;
        }
        if ctrl {
            match virtual_key {
                0x25 => {
                    // Ctrl+Left: previous track.
                    self.prev(hwnd);
                    return;
                }
                0x27 => {
                    // Ctrl+Right: next track.
                    self.next(hwnd);
                    return;
                }
                0x26 => {
                    // Ctrl+Up: volume +5%.
                    self.change_volume(hwnd, 0.05);
                    return;
                }
                0x28 => {
                    // Ctrl+Down: volume −5%.
                    self.change_volume(hwnd, -0.05);
                    return;
                }
                0x4F => {
                    // Ctrl+O: the shell opens the modal file picker after the
                    // `State` borrow ends.
                    self.pick_files_requested = true;
                    return;
                }
                0x45 => {
                    // Ctrl+E: lyrics editor window.
                    self.lyrics_editor_requested = true;
                    return;
                }
                0x0D if self.page == Page::Folders => {
                    // Ctrl+Enter: append every selected file to the queue.
                    let paths = self.folder_selected_paths();
                    if !paths.is_empty() {
                        self.core.handle(Command::AddPaths(paths));
                        self.invalidate(hwnd, self.layout.header);
                    }
                    return;
                }
                _ => {}
            }
        }
        let step = self.core.settings.playback.seek_step_secs as f32;
        if self.page == Page::Folders && virtual_key == 0x08 {
            // Backspace: parent directory.
            self.core.handle(Command::BrowseUp);
            self.scroll = 0.0;
            self.invalidate(hwnd, self.layout.header);
            self.invalidate(hwnd, self.layout.folder_list);
            self.invalidate(hwnd, self.layout.folder_scrollbar);
            return;
        }
        match virtual_key {
            0x1B => {
                // ESC closes the flyout, then leaves big-lyrics mode.
                if self.player_panel_open {
                    self.set_player_panel(hwnd, false);
                } else if self.big_lyrics {
                    self.set_big_lyrics(hwnd, false);
                }
            }
            0x20 => self.toggle_play(hwnd),                     // VK_SPACE
            0x25 => self.seek_by(hwnd, -step),                   // VK_LEFT
            0x27 => self.seek_by(hwnd, step),                     // VK_RIGHT
            0x26 => self.scroll_rows(hwnd, -3.0),                 // VK_UP
            0x28 => self.scroll_rows(hwnd, 3.0),                  // VK_DOWN
            0x21 => self.scroll_page(hwnd, -1),                   // VK_PRIOR
            0x22 => self.scroll_page(hwnd, 1),                    // VK_NEXT
            0x24 => self.scroll_home(hwnd),                       // VK_HOME
            0x23 => self.scroll_end(hwnd),                        // VK_END
            _ => {}
        }
    }

    pub fn set_page(&mut self, page: Page) {
        if page != self.page {
            // A page switch is a large state change: the whole client is
            // repainted. Drop the cached exposed rectangles first, otherwise
            // stale occlusion pieces can exclude a thin strip (e.g. the queue
            // filter frame) from `invalidate_all`, leaving it unpainted until
            // the next hover or occlusion poll.
            self.reset_visibility();
            self.visibility_changed = true;
        }
        self.page = page;
        // The focus ring belongs to the old page's controls.
        self.keyboard_focus = None;
        if let Some(index) = Page::ALL.iter().position(|candidate| *candidate == page) {
            let viewport = self.layout.nav_viewport;
            let item = self.layout.nav[index];
            let top = item.top - self.sidebar_scroll;
            if top < viewport.top {
                self.sidebar_scroll = (self.sidebar_scroll - (viewport.top - top)).max(0.0);
            } else if top + item.height() > viewport.bottom {
                self.sidebar_scroll += top + item.height() - viewport.bottom;
            }
            self.sidebar_scroll = self.sidebar_scroll.clamp(0.0, self.sidebar_max_scroll());
        }
        if page != Page::Search {
            self.online_wall = false;
        }
        self.core.handle(Command::SetPage(page.to_core()));
        // The Now Playing visualizer strip is page-specific; callers repaint
        // the whole window because a page switch is a large layout change.
        self.relayout();
        if page == Page::Library {
            self.library_page = 0;
            self.request_library_page();
        }
        if page == Page::Queue {
            self.refresh_queue_view();
        }
        if self.big_lyrics {
            self.big_lyrics = false;
            self.relayout();
        }
        if self.player_panel_open {
            self.player_panel_open = false;
            self.player_panel_closed = true;
        }
        self.scroll = 0.0;
        self.settings_folders_scroll = 0;
        self.sort_menu_open = false;
        self.ui.clear();
        // Entering the queue should show the track that is playing instead of
        // the top of a 9k list (the display row differs under shuffle/filter).
        if page == Page::Queue {
            self.reveal_current_queue_row();
        }
        self.request_visible_covers();
    }

    pub fn toggle_play(&mut self, hwnd: HWND) {
        self.core.handle(Command::PlayPause);
        self.last_tick = None;
        self.invalidate(hwnd, self.layout.play.inflate(6.0));
        if self.page == Page::NowPlaying {
            // The card action label flips between 播放 and 暂停.
            self.invalidate(hwnd, self.layout.card_buttons[0].inflate(2.0));
        }
    }

    pub fn next(&mut self, hwnd: HWND) {
        let old = self.core.snapshot().current_index;
        self.core.handle(Command::Next);
        let new = self.core.snapshot().current_index;
        self.last_tick = None;
        self.invalidate_track_change(hwnd, old, new);
    }

    pub fn prev(&mut self, hwnd: HWND) {
        let old = self.core.snapshot().current_index;
        self.core.handle(Command::Prev);
        let new = self.core.snapshot().current_index;
        self.last_tick = None;
        self.invalidate_track_change(hwnd, old, new);
    }

    pub fn set_volume(&mut self, volume: f32) {
        self.core.handle(Command::SetVolume(volume));
    }

    /// Keyboard volume step. Repaints immediately so the shortcut works while
    /// paused, when the animation timer is off.
    pub fn change_volume(&mut self, hwnd: HWND, delta: f32) {
        let before = self.core.snapshot().volume;
        let after = (before + delta).clamp(0.0, 1.0);
        if (after - before).abs() < 0.001 {
            return;
        }
        self.core.handle(Command::SetVolume(after));
        self.invalidate(hwnd, self.layout.volume_hit);
        self.invalidate(hwnd, self.layout.mute.inflate(2.0));
    }

    fn set_volume_to_x(&mut self, hwnd: HWND, x: f32) {
        let bar = self.layout.volume_slider;
        if bar.width() <= 0.0 {
            return;
        }
        let ratio = ((x - bar.left) / bar.width()).clamp(0.0, 1.0);
        self.core.handle(Command::SetVolume(ratio));
        self.invalidate(hwnd, bar.inflate(10.0));
        self.invalidate(hwnd, self.layout.mute.inflate(2.0));
    }

    /// Show/hide the playback settings flyout.
    pub fn set_player_panel(&mut self, hwnd: HWND, open: bool) {
        if self.player_panel_open == open {
            return;
        }
        self.player_panel_open = open;
        self.invalidate(hwnd, self.layout.player_settings.inflate(2.0));
        if !open {
            self.player_panel_closed = true;
            // The flyout floated above the lyric child; repaint that area.
            self.invalidate_force(hwnd, self.layout.player_panel);
        }
    }

    pub fn take_player_panel_closed(&mut self) -> bool {
        std::mem::take(&mut self.player_panel_closed)
    }

    fn progress_time_at(&self, x: f32) -> f32 {
        let ratio = ((x - self.layout.progress.left) / self.layout.progress.width().max(1.0))
            .clamp(0.0, 1.0);
        ratio * self.duration()
    }

    /// Update the drag preview only; no engine seek happens until release.
    fn drag_progress_to(&mut self, hwnd: HWND, x: f32) {
        let target = self.progress_time_at(x);
        let old = self.drag_preview.unwrap_or_else(|| self.position());
        if (old - target).abs() < 0.01 {
            return;
        }
        self.drag_preview = Some(target);
        let duration = self.duration().max(0.1);
        self.invalidate_progress_edge(hwnd, old / duration, target / duration);
        if old.floor() as i64 != target.floor() as i64 {
            self.invalidate(hwnd, self.layout.time_left);
            self.invalidate(hwnd, self.layout.time_right);
        }
    }

    pub fn seek_by(&mut self, hwnd: HWND, seconds: f32) {
        let target = (self.position() + seconds).clamp(0.0, self.duration());
        self.seek_to_secs(hwnd, target as f64);
    }

    /// Seek from outside the player bar (lyric click, keyboard, overlay).
    pub fn seek_to_secs(&mut self, hwnd: HWND, seconds: f64) {
        self.drag_preview = None;
        self.core.handle(Command::SeekSecs(seconds));
        self.last_tick = None;
        // Baseline the next tick at the requested position: the engine applies
        // the seek asynchronously, so reading it back here would still return
        // the old position.
        self.last_position = Some(seconds.max(0.0) as f32);
        self.invalidate(hwnd, self.layout.progress_hit);
        self.invalidate(hwnd, self.layout.time_left);
        self.invalidate(hwnd, self.layout.time_right);
    }

    /// Drag the queue scrollbar: map the cursor to the thumb travel.
    fn scroll_to_y(&mut self, hwnd: HWND, y: f32) {
        let bar = self.scrollbar_rect();
        let count = self.row_count();
        if count == 0 {
            return;
        }
        let content = self.scroll_content_height();
        let view = self.list_area().height();
        if content <= view || bar.height() <= 0.0 {
            return;
        }
        let thumb = (view * view / content).max(24.0).min(bar.height());
        let travel = (bar.height() - thumb).max(1.0);
        let ratio = ((y - bar.top - thumb * 0.5) / travel).clamp(0.0, 1.0);
        let target = ratio * self.max_scroll();
        if (target - self.scroll).abs() < 0.5 {
            return;
        }
        self.scroll = target.clamp(0.0, self.max_scroll());
        self.ui.set_hover(None);
        self.invalidate(hwnd, self.list_area());
        self.invalidate(hwnd, self.scrollbar_rect());
    }

    fn scroll_by(&mut self, hwnd: HWND, rows: f32) {
        let target = (self.scroll + rows * self.row_height()).clamp(0.0, self.max_scroll());
        self.scroll_to(hwnd, target);
    }

    /// Drag the settings-page scrollbar: map the cursor to the row offset.
    fn settings_scroll_to_y(&mut self, hwnd: HWND, y: f32) {
        let settings = layout::settings(&self.layout);
        let bar = settings.scrollbar;
        let max = self.settings_max_scroll();
        if max <= 0.5 {
            return;
        }
        let view = settings.generic.height();
        let content = self.settings_tab.fields().len() as f32 * layout::SETTINGS_ROW_HEIGHT;
        if content <= view || bar.height() <= 0.0 {
            return;
        }
        let thumb = (view * view / content).max(24.0).min(bar.height());
        let travel = (bar.height() - thumb).max(1.0);
        let ratio = ((y - bar.top - thumb * 0.5) / travel).clamp(0.0, 1.0);
        let target = (ratio * max).clamp(0.0, max);
        if (target - self.settings_scroll).abs() < 0.5 {
            return;
        }
        self.settings_scroll = target;
        self.ui.set_hover(None);
        self.invalidate(hwnd, settings.generic);
        self.invalidate(hwnd, settings.scrollbar);
    }

    /// Keyboard scrolling: the queue scrolls, the paged library ignores
    /// row-wise arrows (page keys still move between pages).
    fn scroll_rows(&mut self, hwnd: HWND, rows: f32) {
        if self.page == Page::Library {
            return;
        }
        self.scroll_by(hwnd, rows);
    }

    fn scroll_page(&mut self, hwnd: HWND, delta: i32) {
        if self.page == Page::Library {
            self.turn_library_page(hwnd, delta);
        } else {
            self.scroll_by(hwnd, delta as f32 * self.visible_page());
        }
    }

    fn scroll_home(&mut self, hwnd: HWND) {
        if self.page == Page::Library {
            self.set_library_page(hwnd, 0);
        } else {
            self.scroll_to(hwnd, 0.0);
        }
    }

    fn scroll_end(&mut self, hwnd: HWND) {
        if self.page == Page::Library {
            self.set_library_page(hwnd, usize::MAX);
        } else {
            self.scroll_to(hwnd, self.max_scroll());
        }
    }

    fn scroll_to(&mut self, hwnd: HWND, target: f32) {
        if (target - self.scroll).abs() < 0.5 {
            return;
        }
        self.scroll = target.clamp(0.0, self.max_scroll());
        self.ui.set_hover(None);
        self.invalidate(hwnd, self.list_area());
        self.invalidate(hwnd, self.scrollbar_rect());
    }

    fn row_at(&self, y: f32) -> usize {
        let relative = (y - self.list_area().top + self.scroll).max(0.0);
        (relative / self.row_height()) as usize
    }

    /// Row under a client point on a virtual list page (context menus).
    pub fn list_row_at(&self, x: f32, y: f32) -> Option<usize> {
        if !matches!(
            self.page,
            Page::Queue | Page::Library | Page::Folders | Page::Smart | Page::Duplicates
                | Page::Search | Page::Discover
        ) {
            return None;
        }
        if !self.list_area().contains(x, y) {
            return None;
        }
        if self.online_wall_active() {
            return self.wall_index_at(x, y);
        }
        let row = self.row_at(y);
        (row < self.row_count()).then_some(row)
    }

    /// Play a queue row from its context menu.
    pub fn play_queue_row(&mut self, hwnd: HWND, index: usize) {
        let old = self.core.snapshot().current_index;
        self.core.handle(Command::PlayIndex(index));
        let new = self.core.snapshot().current_index;
        self.invalidate_track_change(hwnd, old, new);
    }

    /// Play a library row from its context menu.
    pub fn play_library_row(&mut self, hwnd: HWND, index: usize) {
        let old = self.core.snapshot().current_index;
        self.core.handle(Command::LibraryPlay(index));
        let new = self.core.snapshot().current_index;
        self.invalidate_track_change(hwnd, old, new);
    }

    fn visible_page(&self) -> f32 {
        (self.layout.list.height() / self.row_height()).max(1.0)
    }
}

impl App {
    /// Labels used by the settings painter; kept here so the hit-test and the
    /// painted state use the same snapshot.
    pub fn settings_rows(&self) -> [(&'static str, String); 3] {
        let snapshot = self.core.snapshot();
        [
            (
                "扫描启动时索引曲库",
                if snapshot.library_scan_on_start { "开" } else { "关" }.to_string(),
            ),
            (
                "自动监听音乐目录",
                if snapshot.library_auto_watch { "开" } else { "关" }.to_string(),
            ),
            (
                "索引时跟随符号链接",
                if snapshot.library_follow_symlinks { "开" } else { "关" }.to_string(),
            ),
        ]
    }

    pub fn settings_folders(&self) -> &[std::path::PathBuf] {
        self.core.snapshot().library_folders
    }

    pub fn settings_folders_scroll(&self) -> usize {
        self.settings_folders_scroll
    }
}

/// Compute the exposed part of the client area as DIP rectangles by walking
/// top-level windows above ours and subtracting their rects. Fully covered
/// returns an empty vector. Child windows cannot occlude the top-level window.
/// Focus is intentionally ignored: visible-but-unfocused keeps animating.
/// Layered windows may composite arbitrary per-pixel alpha, and click-through
/// windows are explicitly non-opaque. Neither can be subtracted as a solid
/// occluder.
fn style_is_transparent(style: u32) -> bool {
    style & (WS_EX_LAYERED.0 | WS_EX_TRANSPARENT.0) != 0
}

fn is_transparent_window(hwnd: HWND) -> bool {
    let style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
    style_is_transparent(style)
}

fn visible_rects(hwnd: HWND, dpi: f32) -> Vec<Rect> {
    unsafe {
        if IsIconic(hwnd).as_bool() || !IsWindowVisible(hwnd).as_bool() {
            return Vec::new();
        }
        let mut client = RECT::default();
        if GetClientRect(hwnd, &mut client).is_err() {
            return Vec::new();
        }
        let scale = dpi / 96.0;
        let full = Rect::new(
            client.left as f32 / scale,
            client.top as f32 / scale,
            client.right as f32 / scale,
            client.bottom as f32 / scale,
        );
        if full.width() <= 0.0 || full.height() <= 0.0 {
            return vec![full];
        }

        let mut rects = vec![full];
        let mut above = GetWindow(hwnd, GW_HWNDPREV).ok();
        let mut guard = 0;
        while let Some(above_hwnd) = above {
            guard += 1;
            if guard > 64 {
                break;
            }
            if IsWindowVisible(above_hwnd).as_bool() && !IsIconic(above_hwnd).as_bool() {
                // A layered window may be fully or partly transparent (desktop
                // lyrics, mini player, tooltips). Treating it as an opaque
                // occluder leaves stale pixels on the exposed main window
                // underneath, because those rects are never invalidated.
                // Repainting a little under such a window is cheap: only the
                // animation dirty regions intersect it.
                if is_transparent_window(above_hwnd) {
                    above = GetWindow(above_hwnd, GW_HWNDPREV).ok();
                    continue;
                }
                let mut window = RECT::default();
                if GetWindowRect(above_hwnd, &mut window).is_ok() {
                    let mut top_left = POINT {
                        x: window.left,
                        y: window.top,
                    };
                    let mut bottom_right = POINT {
                        x: window.right,
                        y: window.bottom,
                    };
                    let _ = ScreenToClient(hwnd, &mut top_left);
                    let _ = ScreenToClient(hwnd, &mut bottom_right);
                    let cut = Rect::new(
                        top_left.x as f32 / scale,
                        top_left.y as f32 / scale,
                        bottom_right.x as f32 / scale,
                        bottom_right.y as f32 / scale,
                    );
                    if cut.width() > 0.0 && cut.height() > 0.0 {
                        let mut next = Vec::with_capacity(rects.len() * 2);
                        for rect in rects {
                            subtract_rect(rect, cut, &mut next);
                        }
                        rects = next;
                        if rects.is_empty() {
                            break;
                        }
                        if rects.len() > 48 {
                            return vec![full];
                        }
                    }
                }
            }
            above = GetWindow(above_hwnd, GW_HWNDPREV).ok();
        }
        rects
    }
}

fn subtract_rect(rect: Rect, cut: Rect, out: &mut Vec<Rect>) {
    let inter = intersect_rect(rect, cut);
    if inter.width() <= 0.0 || inter.height() <= 0.0 {
        out.push(rect);
        return;
    }
    if rect.left < inter.left {
        out.push(Rect::new(rect.left, rect.top, inter.left, rect.bottom));
    }
    if inter.right < rect.right {
        out.push(Rect::new(inter.right, rect.top, rect.right, rect.bottom));
    }
    if rect.top < inter.top {
        out.push(Rect::new(inter.left, rect.top, inter.right, inter.top));
    }
    if inter.bottom < rect.bottom {
        out.push(Rect::new(
            inter.left,
            inter.bottom,
            inter.right,
            rect.bottom,
        ));
    }
}

fn intersect_rect(a: Rect, b: Rect) -> Rect {
    Rect::new(
        a.left.max(b.left),
        a.top.max(b.top),
        a.right.min(b.right),
        a.bottom.min(b.bottom),
    )
}

/// Open Explorer at the downloaded file. Failures are non-fatal (the file may
/// live on a disconnected drive).
fn reveal_path(path: &std::path::Path) {
    use std::os::windows::process::CommandExt;
    let mut command = std::process::Command::new("explorer");
    // `explorer` reparses its own command line: without quotes a path with
    // spaces is truncated and Explorer falls back to the Documents folder.
    command.raw_arg(format!("/select,\"{}\"", path.display()));
    if let Err(err) = command.spawn() {
        tracing::warn!("open download location failed: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transparent_window_styles_are_not_occluders() {
        assert!(style_is_transparent(WS_EX_LAYERED.0));
        assert!(style_is_transparent(WS_EX_TRANSPARENT.0));
        assert!(style_is_transparent(WS_EX_LAYERED.0 | WS_EX_TRANSPARENT.0));
        assert!(!style_is_transparent(0));
    }
}
