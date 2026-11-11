//! `AppCore` — UI-independent playback, queue, lyrics and persistence.
//!
//! M0 rule: this module owns business state and background services. The
//! native UI reads a [`Snapshot`] and sends [`Command`]s; it never touches
//! the audio engine, queue internals or the settings file directly.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::api::{
    ApiEvent, ApiService, ApiSong, CacheEvent, CacheUsage, PreparedPlay, SearchCache, SearchKind,
    StreamCacheService,
};
use crate::audio::{AudioEngine, DspConfig, EngineEvent, EngineSource, PlaybackState};
use crate::config::{
    CloseBehavior, EqPreset, ListDensity, LyricsAlign, LyricsPanelBackground, Paths,
    PlaylistShuffle, RecentFile, ReplayGainMode, Session, Settings, SourceConfig, SourceKind,
    ThemeMode, load_json, load_settings, load_settings_strict, save_session, save_settings,
};
use crate::download::{DownloadEvent, DownloadService, DownloadState};
use crate::library::{
    BrowseService, BrowserEntry, DbEvent, DbTrack, DuplicateGroup, FolderBrowser, LibraryDb,
    LibraryEntry, LibraryQuery, LibrarySort, SmartKind, WatchEvent, WatchService,
};
use crate::lyrics::{
    Lyrics, LyricsBackgroundService, LyricsService,
};
use crate::model::{Page, RepeatMode, Track, TrackSource};
use crate::playlist::Queue;
use crate::util::background_image::BackgroundImageService;
use crate::util::cover::{CoverService, RowCoverService};
use crate::util::image_util::RgbaImage;

/// Default library window size used until the UI reports how many rows fit
/// on screen. The UI pages the list, so this is only a startup fallback.
const LIBRARY_PAGE_SIZE: usize = 2000;

/// Intent sent by the UI. The core executes it; the UI never mutates state.
///
/// Some variants land with their page (library import, playlists, settings),
/// so the full command surface is defined here from the start.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub enum Command {
    PlayPause,
    Next,
    Prev,
    Stop,
    SeekSecs(f64),
    SetVolume(f32),
    ToggleMute,
    PlayIndex(usize),
    AddPaths(Vec<PathBuf>),
    /// Insert dropped paths before a queue row (OLE drop preview).
    AddPathsAt { paths: Vec<PathBuf>, index: usize },
    /// Folder browser: `None` lists drive roots, `Some` lists a directory.
    BrowseTo(Option<PathBuf>),
    /// Navigation history: visit the previous / next location.
    BrowseBack,
    BrowseForward,
    BrowseUp,
    /// Activate one browser row: enter a folder, play a file, import a list.
    BrowsePlay(usize),
    /// Append one browser file/playlist to the queue.
    BrowseEnqueue(usize),
    /// Append files without starting playback (folder → queue drag).
    EnqueuePaths(Vec<PathBuf>),
    /// Append every audio file in the current directory.
    BrowseAddAll,
    /// Replace the queue with every audio file here and start playing.
    BrowsePlayAll,
    /// Remove one row from the play queue (context menu).
    RemoveFromQueue(usize),
    /// Move a queue row to another position (drag/context menu).
    QueueMove { from: usize, to: usize },
    /// Add one library row to the queue; `next` inserts it after the current
    /// track, otherwise it is appended.
    LibraryQueue { index: usize, next: bool },
    ToggleShuffle,
    CycleRepeat,
    ClearQueue,
    SetPage(Page),
    /// Online search page: update the query without issuing a request.
    OnlineSetQuery(String),
    /// Run a song/album search against the first enabled online source.
    OnlineSearch { query: String, offset: u32, limit: u32 },
    /// Move one page in the current search / album / artist results.
    OnlinePage(i32),
    OnlineSetKind(SearchKind),
    /// Play / append one row from the current search or drill-down results.
    OnlinePlay(usize),
    OnlineQueue(usize),
    OnlineQueueAll,
    /// Queue a download for one result row (`core::download`).
    OnlineDownload(usize),
    /// Download one queue row (online/URL tracks only).
    DownloadQueue(usize),
    OnlineOpenAlbum(String),
    OnlineOpenArtist(String),
    OnlineBack,
    OnlineDetailMore,
    /// Discover: pull a random batch of songs.
    OnlineRandom,
    /// Discover page: refresh the random batch and act on one row.
    DiscoverRefresh,
    DiscoverPlay(usize),
    DiscoverQueue(usize),
    DiscoverDownload(usize),
    /// Add a direct HTTP(S)/HLS URL to the queue and play it.
    OpenUrl(String),
    /// Primary online source (API address / credentials).
    SetSourceEnabled(bool),
    SetSourceName(String),
    SetSourceBaseUrl(String),
    SetSourceScript(String),
    SetSourceQuality(crate::config::Quality),
    SetSourceSessionKey(String),
    SetSourceCookie(String),
    SetSourceInsecureTls(bool),
    /// Multi-source management.
    CyclePrimarySource,
    AddSource,
    RemovePrimarySource,
    SetSourceHeaders(Vec<crate::config::HeaderPair>),
    SetSourceEndpoints(crate::config::EndpointOverrides),
    /// Online network settings.
    SetNetworkOffline(bool),
    SetApiMinInterval(u64),
    SetAlbumPageSize(u32),
    SetNetworkTimeout(u64),
    SetNetworkRetries(u32),
    SetProxyMode(crate::config::ProxyMode),
    SetNetworkProxyUrl(String),
    SetCacheLimitMb(u64),
    SetStreamCacheDir(Option<PathBuf>),
    ClearStreamCache,
    /// Download settings and manager.
    SetDownloadDir(Option<PathBuf>),
    SetDownloadConcurrency(u8),
    SetDownloadConflict(crate::config::ConflictPolicy),
    SetDownloadQuality(Option<crate::config::Quality>),
    SetDownloadTemplate(String),
    SetDownloadEmbedTags(bool),
    SetDownloadEmbedCover(bool),
    SetDownloadSaveLyrics(bool),
    SetDownloadAddToLibrary(bool),
    CancelDownload(u64),
    RetryDownload(u64),
    CycleDownloadQuality(u64),
    ClearFinishedDownloads,
    /// Delete abandoned `.part` files under the download directory.
    CleanPartialDownloads,
    /// Connectivity test against the first enabled source (result is a toast).
    OnlineTest,
    LibrarySearch(String),
    LibraryRefresh,
    LibraryReindex,
    LibraryAddFolder(PathBuf),
    /// Load one complete library page. The UI sizes it to the rows that fit
    /// on screen, so the list never needs incremental streaming.
    LibraryPage { offset: usize, limit: usize },
    LibraryPlay(usize),
    /// Append every row matching the current search/sort (not just the page).
    LibraryQueueAll,
    /// Remove one row from the library index (never deletes the file).
    LibraryRemove(usize),
    /// Stop the running index scan between files.
    LibraryCancelIndex,
    /// Re-check every indexed path and refresh the page.
    LibraryCheckMissing,
    /// Load aggregate playback statistics (counts, top artists/tracks,
    /// recently played) for the statistics page.
    LibraryStats,
    /// Compact the SQLite index (PRAGMA optimize + VACUUM).
    LibraryOptimize,
    /// Clear play/skip counters and last-played timestamps.
    LibraryResetStats,
    /// Play one row from the statistics page's recently-played list.
    StatisticsPlay(usize),
    /// Smart-playlist page: load one preset list.
    LibrarySmart(SmartKind),
    LibraryPlaySmart(usize),
    LibraryQueueSmart(usize),
    LibraryRemoveSmart(usize),
    /// Duplicate results page: query one match mode.
    LibraryDuplicates(crate::config::DedupeMode),
    LibraryPlayDuplicate(usize),
    LibraryRemoveDuplicate(usize),
    /// Keep the first file of a duplicate group and drop the rest from the
    /// index (never deletes files).
    LibraryKeepDuplicateGroup(usize),
    /// Remove every file of one duplicate group from the index.
    LibraryRemoveDuplicateGroup(usize),
    LibrarySetSort(LibrarySort),
    LibrarySetScanOnStart(bool),
    LibrarySetAutoWatch(bool),
    LibrarySetFollowSymlinks(bool),
    LibrarySetDedupeMode(crate::config::DedupeMode),
    LibrarySetExtensions(String),
    LibraryRemoveFolder(PathBuf),
    ReloadLyrics,
    LoadLyricsFile(PathBuf),
    /// Playback settings from the player-bar flyout.
    SetSpeed(f32),
    SetEqEnabled(bool),
    SetPitchPreserve(bool),
    /// Open/close the native desktop-lyrics overlay.
    ToggleDesktopLyrics,
    /// Open/close the native mini player.
    ToggleMiniPlayer,
    DesktopLyricsSetLock(bool),
    DesktopLyricsSetClickThrough(bool),
    DesktopLyricsSetAlwaysOnTop(bool),
    DesktopLyricsFontDelta(i32),
    DesktopLyricsSetLines(u8),
    DesktopLyricsSetAlign(u8),
    DesktopLyricsSetAutoSize(bool),
    DesktopLyricsSetSize { width: f32, height: f32 },
    DesktopLyricsSetTranslation(bool),
    DesktopLyricsSetFooter(bool),
    DesktopLyricsSetBold(bool),
    /// `0=transparent, 1=solid, 2=gradient, 3=image, 4=blur cover`.
    DesktopLyricsSetBackground(u8),
    DesktopLyricsSetOpacity(f32),
    DesktopLyricsSetRadius(i32),
    DesktopLyricsSetPadding(f32),
    DesktopLyricsSetLineSpacing(f32),
    DesktopLyricsSetShadow(bool),
    DesktopLyricsSetOutline(bool),
    DesktopLyricsSetBorder(bool),
    DesktopLyricsSetWindowShadow(bool),
    DesktopLyricsSetHideWhenPaused(bool),
    DesktopLyricsSetCaptureProtection(bool),
    DesktopLyricsSetKaraoke(bool),
    DesktopLyricsSetWordsOnly(bool),
    /// Absolute current-line size; next/prev follow the same ratios.
    DesktopLyricsSetFontSize(f32),
    /// Independent overrides for the secondary rows (previous / next / footer).
    DesktopLyricsSetNextFontSize(f32),
    DesktopLyricsSetPrevFontSize(f32),
    DesktopLyricsSetFooterFontSize(f32),
    /// Karaoke animation refresh rate for the desktop lyrics window.
    DesktopLyricsSetFrameRate(u32),
    /// Native move/resize reported by the overlay (logical points).
    DesktopLyricsGeometry {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        resized: bool,
    },
    MiniPlayerGeometry {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        resized: bool,
    },
    /// Clear the remembered overlay position and let it re-centre.
    DesktopLyricsResetPosition,
    MiniPlayerResetPosition,
    SetGapless(bool),
    SetAutoAdvance(bool),
    SetCrossfade(f32),
    SetReplayGainMode(ReplayGainMode),
    SetReplayGainPreamp(f32),
    SetEqPreset(EqPreset),
    /// Set one equalizer band in dB; this also switches the preset to Custom.
    SetEqBand(usize, f32),
    SetReplayGainScan(bool),
    /// `None` cancels the sleep timer; otherwise minutes from now.
    SetSleepTimer(Option<u32>),
    /// A-B repeat range in seconds; B must be after A.
    SetABStart,
    SetABEnd,
    ClearABRepeat,
    /// `None` follows the system default output.
    SetOutputDevice(Option<String>),
    SetCloseBehavior(CloseBehavior),
    SetStartMinimized(bool),
    SetRememberPosition(bool),
    SetCaptureProtection(bool),
    SetLyricsCaptureProtection(bool),
    SetAllowMultipleInstances(bool),
    SetStartWithWindows(bool),
    SetFileAssociations(bool),
    SetPlaylistAssociations(bool),
    SetMediaControls(bool),
    SetNotifications(bool),
    SetJumpList(bool),
    /// Appearance: dark/light/system, accent color, list density.
    SetThemeMode(ThemeMode),
    /// UI font family / base size for all GDI text.
    SetThemeFontFamily(Option<String>),
    SetThemeFontSize(f32),
    SetAccent([u8; 3]),
    SetListDensity(ListDensity),
    SetReduceMotion(bool),
    /// Animation/progress refresh rate cap (15–120 fps).
    SetFrameRate(u32),
    /// Performance and memory trade-offs (settings → 性能).
    SetMetadataMode(crate::config::MetadataMode),
    SetLoadCovers(bool),
    SetLazyAudio(bool),
    SetReleaseAudioOutput(bool),
    SetDeferPausedRestore(bool),
    SetIdleFrameRate(u32),
    SetPauseAnimationsUnfocused(bool),
    SetThumbnailCache(u32),
    /// Use a hardware Direct2D DC target for the overlay surfaces instead of
    /// the software rasterizer.
    SetD2DHardware(bool),
    /// Global hotkeys: enable, rebind one slot, or restore defaults.
    SetShortcutsEnabled(bool),
    SetGamepadEnabled(bool),
    SetShortcut(crate::config::ShortcutSlot, String),
    ResetShortcuts,
    SetTrimWorkingSetOnIdle(bool),
    SetTrimIdleSecs(u64),
    /// Background style: solid / blurred cover / flow gradient / Mica.
    SetBackgroundMode(crate::config::BackgroundMode),
    /// User-selected main-window background picture (`None` clears it).
    SetBackgroundImage(Option<PathBuf>),
    /// Audio visualizer strip on the Now Playing page.
    SetVisualizerEnabled(bool),
    SetVisualizerMode(crate::config::VisualizerMode),
    /// Lyrics appearance (main panel and standalone window).
    SetLyricsKaraoke(bool),
    SetLyricsWordsOnly(bool),
    SetLyricsTranslation(bool),
    SetLyricsFontSize(f32),
    SetLyricsLineSpacing(f32),
    SetLyricsAlign(LyricsAlign),
    SetLyricsFontFamily(Option<String>),
    SetLyricsFontWeight(u16),
    SetLyricsItalic(bool),
    SetLyricsOutlineWidth(f32),
    SetLyricsOutlineColor([u8; 3]),
    SetLyricsShadowOffset(f32),
    SetLyricsShadowColor([u8; 3]),
    SetLyricsTranslationOutlineWidth(f32),
    SetLyricsTranslationOutlineColor([u8; 3]),
    SetLyricsTranslationShadowOffset(f32),
    SetLyricsTranslationShadowColor([u8; 3]),
    SetLyricsEncoding(crate::config::LyricsEncoding),
    /// Additional lead/lag in milliseconds on top of the file's `[offset]`.
    SetLyricsOffset(i64),
    SetLyricsTranslationLanguage(String),
    SetLyricsPrefetch(bool),
    SetLyricsShowSource(bool),
    /// Embedded lyrics panel fill; `None` follows the theme.
    SetLyricsPanelColor(Option<[u8; 3]>),
    SetLyricsPanelBackground(LyricsPanelBackground),
    SetLyricsPanelColor2([u8; 3]),
    SetLyricsPanelBorderColor(Option<[u8; 3]>),
    SetLyricsPanelImage(Option<PathBuf>),
    /// Restore the panel to the theme fill, clearing gradient/image/border.
    ResetLyricsPanelBackground,
    SetLargeLyricsFontSize(f32),
    SetLargeLyricsFontFamily(Option<String>),
    SetLargeLyricsFontWeight(u16),
    SetLargeLyricsItalic(bool),
    SetLargeLyricsOutlineWidth(f32),
    SetLargeLyricsOutlineColor([u8; 3]),
    SetLargeLyricsShadowOffset(f32),
    SetLargeLyricsShadowColor([u8; 3]),
    SetLargeLyricsTranslationOutlineWidth(f32),
    SetLargeLyricsTranslationOutlineColor([u8; 3]),
    SetLargeLyricsTranslationShadowOffset(f32),
    SetLargeLyricsTranslationShadowColor([u8; 3]),
    SetLyricsHighlight([u8; 3]),
    SetLyricsCurrentColor([u8; 3]),
    SetLyricsNormalColor([u8; 3]),
    SetLyricsTranslationColor([u8; 3]),
    SetLargeLyricsLineSpacing(f32),
    SetLargeLyricsAlign(LyricsAlign),
    SetLargeLyricsTranslation(bool),
    SetLargeLyricsKaraoke(bool),
    SetLargeLyricsWordsOnly(bool),
    SetLargeLyricsShowCover(bool),
    SetLargeLyricsShowTitle(bool),
    SetLargeLyricsShowFooter(bool),
    SetLargeLyricsHighlight([u8; 3]),
    SetLargeLyricsCurrentColor([u8; 3]),
    SetLargeLyricsNormalColor([u8; 3]),
    SetLargeLyricsTranslationColor([u8; 3]),
    /// Desktop overlay color slot: 0 current, 1 next, 2 prev, 3 footer,
    /// 4 translation, 5 outline, 6 window shadow, 7 background, 8 second
    /// background (gradient).
    /// Color slot: 0 current, 1 next, 2 prev, 3 footer, 4 translation,
    /// 5 outline, 6 window shadow, 7 background, 8 second background,
    /// 9 translation outline, 10 translation shadow, 11 footer outline,
    /// 12 footer shadow.
    SetDesktopLyricsColor(u8, [u8; 3]),
    SetDesktopLyricsBackgroundImage(Option<PathBuf>),
    SetDesktopLyricsFontFamily(Option<String>),
    SetDesktopLyricsTranslationFontFamily(Option<String>),
    SetDesktopLyricsFooterFontFamily(Option<String>),
    SetDesktopLyricsTranslationBold(bool),
    SetDesktopLyricsFooterBold(bool),
    SetDesktopLyricsOutlineWidth(f32),
    SetDesktopLyricsShadowOffset(f32),
    SetDesktopLyricsItalic(bool),
    SetDesktopLyricsTranslationOutlineWidth(f32),
    SetDesktopLyricsTranslationShadowOffset(f32),
    SetDesktopLyricsFooterOutlineWidth(f32),
    SetDesktopLyricsFooterShadowOffset(f32),
    SetDesktopLyricsBlurRadius(f32),
    SetDesktopLyricsTranslationOutline(bool),
    SetDesktopLyricsTranslationShadow(bool),
    SetDesktopLyricsFooterOutline(bool),
    SetDesktopLyricsFooterShadow(bool),
    SetDesktopLyricsHideWhenFullscreen(bool),
    /// Mini-player content and window options.
    SetMiniShowLyrics(bool),
    SetMiniKaraoke(bool),
    SetMiniWordsOnly(bool),
    SetMiniLyricsLines(u8),
    SetMiniLyricsBold(bool),
    SetMiniTranslation(bool),
    SetMiniFontSize(f32),
    /// Progress/lyric animation refresh rate for the mini player.
    SetMiniFrameRate(u32),
    SetMiniAutoHeight(bool),
    SetMiniHeight(f32),
    SetMiniAlwaysOnTop(bool),
    SetMiniOpacity(f32),
    SetMiniShowInTaskbar(bool),
    SetMiniWidth(f32),
    SetMiniPanelColor([u8; 3]),
    SetMiniTextColor([u8; 3]),
    SetMiniAccentColor([u8; 3]),
}

/// Read-only view handed to the UI for painting.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub struct Snapshot<'a> {
    pub page: Page,
    pub queue: &'a [Track],
    /// Playback order as indices into `queue`. Equals `0..len` when shuffle is
    /// off; the queue page renders this order so the shuffled list is visible.
    pub queue_order: &'a [usize],
    pub current_index: Option<usize>,
    pub current: Option<&'a Track>,
    pub state: PlaybackState,
    pub position_secs: f64,
    pub duration_secs: f64,
    pub volume: f32,
    pub muted: bool,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    pub lyrics: Option<&'a Lyrics>,
    pub lyrics_loading: bool,
    pub lyrics_error: Option<&'a str>,
    /// Decoded picture for the lyrics panel background (`Image` mode).
    pub lyrics_background: Option<&'a RgbaImage>,
    pub lyrics_background_rev: u64,
    /// Decoded user-selected main-window background picture.
    pub background_image: Option<&'a RgbaImage>,
    pub background_image_rev: u64,
    pub library: &'a [LibraryEntry],
    pub library_total: usize,
    pub library_loading: bool,
    /// `Some(n)` after an explicit missing-file check found n missing rows.
    pub library_missing_checked: Option<usize>,
    pub library_index_active: bool,
    pub library_index_progress: usize,
    pub library_index_current: &'a str,
    pub library_folders: &'a [PathBuf],
    pub library_extensions: &'a [String],
    pub library_sort: LibrarySort,
    pub library_scan_on_start: bool,
    pub library_auto_watch: bool,
    pub library_follow_symlinks: bool,
    pub library_dedupe_mode: crate::config::DedupeMode,
    /// Bumped whenever the current lyrics object/loading state changes.
    pub lyrics_rev: u64,
    /// Bumped whenever the library rows or index status change.
    pub library_rev: u64,
    /// Current track cover decoded off the UI thread.
    pub current_cover: Option<&'a RgbaImage>,
    /// PNG form of `current_cover` for overlay windows / SMTC.
    pub current_cover_png: Option<&'a [u8]>,
    /// Bumped when the current cover (or its absence) changes.
    pub cover_rev: u64,
    /// Bumped when a list-row thumbnail finishes loading.
    pub row_cover_rev: u64,
    /// Seconds left on the sleep timer, if one is armed.
    pub sleep_remaining_secs: Option<u64>,
    /// Bumped whenever the queue rows/order change.
    pub queue_rev: u64,
    /// Lazy folder-browser state (current directory and its entries).
    pub browser: &'a FolderBrowser,
    /// Bumped on navigation and when a directory read completes.
    pub browse_rev: u64,
    /// Aggregate playback statistics for the statistics page.
    pub statistics: Option<&'a crate::library::LibraryStats>,
    /// Bumped when a new status message is queued (`AppCore::drain_notices`).
    pub notice_rev: u64,
    /// Smart-playlist rows and the selected preset.
    pub library_smart: &'a [LibraryEntry],
    pub library_smart_kind: SmartKind,
    /// Duplicate groups and the flattened display rows (headers included).
    pub library_duplicates: &'a [DuplicateGroup],
    pub library_duplicate_rows: &'a [LibraryEntry],
    pub library_duplicate_headers: &'a [bool],
    pub library_duplicate_mode: crate::config::DedupeMode,
    /// A-B repeat points in seconds.
    pub ab_start: Option<f64>,
    pub ab_end: Option<f64>,
    /// Configured sleep-timer length (None = off).
    pub sleep_minutes: Option<u32>,
    /// Online search / album / artist state (search page).
    pub online: &'a OnlineState,
    /// Discover page random batch.
    pub discover: &'a DiscoverState,
    /// Bumped when online results, suggestions or drill-down state change.
    pub online_rev: u64,
    /// Download manager rows.
    pub downloads: &'a [DownloadRow],
    pub download_rev: u64,
    /// Stream-cache size reported by the background cache worker.
    pub cache_usage: CacheUsage,
    pub cache_rev: u64,
}

pub struct AppCore {
    pub settings: Settings,
    pub session: Session,
    pub paths: Paths,
    engine: AudioEngine,
    queue: Queue,
    current_track: Option<Track>,
    current_duration: Option<f64>,
    playback_state: PlaybackState,
    engine_load_token: u64,
    /// Track armed on the gapless next deck, with the token it was armed for.
    armed_next: Option<(usize, u64)>,
    /// Start position of a restored-but-not-yet-opened track.
    deferred_start: Option<f64>,
    lyrics_service: LyricsService,
    lyrics_background_service: LyricsBackgroundService,
    lyrics_background: Option<Arc<RgbaImage>>,
    lyrics_background_key: Option<PathBuf>,
    lyrics_background_pending: Option<PathBuf>,
    pub lyrics_background_rev: u64,
    background_image_service: BackgroundImageService,
    background_image: Option<Arc<RgbaImage>>,
    background_image_key: Option<PathBuf>,
    background_image_pending: Option<PathBuf>,
    pub background_image_rev: u64,
    lyrics: Option<Lyrics>,
    lyrics_key: String,
    lyrics_loading: bool,
    lyrics_error: Option<String>,
    lyrics_rev: u64,
    library_rev: u64,
    queue_rev: u64,
    cover: CoverState,
    row_covers: RowCovers,
    session_dirty: bool,
    settings_dirty: bool,
    /// Background writer for periodic session snapshots (see `maybe_autosave`).
    session_writer: SessionWriter,
    last_session_save: Instant,
    last_settings_save: Instant,
    /// Synthetic clock used only by `--bench`: there is no audio engine work
    /// behind it, so painting cost can be measured without a music library.
    demo: Option<DemoClock>,
    /// Runtime-only sleep timer; not persisted (a countdown across restarts
    /// would be surprising).
    sleep_deadline: Option<Instant>,
    sleep_minutes: Option<u32>,
    library: LibraryState,
    /// Current directory listing and its background reader.
    browser: FolderBrowser,
    browse_service: BrowseService,
    browse_rev: u64,
    /// Short status messages (errors, completed background work) queued for
    /// the UI toast. Capped so a failing watcher cannot grow without bound.
    notices: VecDeque<Notice>,
    notice_rev: u64,
    /// A-B repeat points (runtime only).
    ab_start: Option<f64>,
    ab_end: Option<f64>,
    /// Lyrics prefetched for upcoming tracks, keyed by `Track::dedupe_key`.
    prefetched_lyrics: HashMap<String, Lyrics>,
    prefetched_order: VecDeque<String>,
    /// Online search / album / artist state (search page).
    online: OnlineState,
    /// Discover page random batch (independent from search results).
    discover: DiscoverState,
    /// Off-thread Bamboo search/info/resolve worker.
    api: ApiService,
    /// Current-track resolve in flight (Bamboo).
    online_resolve: Option<OnlineResolve>,
    /// Resolved URL for the next track, used to pre-arm the gapless deck.
    online_next: Option<(String, PreparedPlay)>,
    /// Next-track resolve in flight (dedupe for repeated arm attempts).
    online_next_request: Option<String>,
    /// Background loudness scans requested by the ReplayGain setting.
    loudness: Option<crate::audio::loudness::LoudnessService>,
    loudness_pending: Option<String>,
    /// Kept so lazily created workers (loudness) can wake an idle UI.
    wake: Option<Arc<dyn Fn() + Send + Sync>>,
    /// Download manager (settings → 下载 / online rows).
    downloads: DownloadService,
    pub download_rows: Vec<DownloadRow>,
    download_next_id: u64,
    pub download_rev: u64,
    /// Persistent search-page cache (source + kind + query + page).
    search_cache: SearchCache,
    search_cache_path: PathBuf,
    search_cache_dirty: bool,
    search_cache_saved: Instant,
    /// Off-thread stream-cache usage/pruning worker.
    cache_service: StreamCacheService,
    cache_usage: CacheUsage,
    cache_rev: u64,
    /// Result of the background `.part` cleanup scan.
    partial_cleanup: Option<crossbeam_channel::Receiver<usize>>,
    /// Pending `api.info` for online lyrics of the current track.
    online_info: Option<(u64, String)>,
}

pub struct Notice {
    pub text: String,
    pub error: bool,
}

const NOTICE_LIMIT: usize = 8;

struct CoverState {
    service: CoverService,
    /// Stable key of the requested track; results for other keys are stale.
    key: String,
    image: Option<Arc<RgbaImage>>,
    png: Option<Arc<Vec<u8>>>,
    rev: u64,
}

struct RowCovers {
    service: RowCoverService,
    /// `None` caches a miss so a file without artwork is not re-read forever.
    images: HashMap<String, Option<Arc<RgbaImage>>>,
    inflight: HashSet<String>,
    order: VecDeque<String>,
    rev: u64,
}

struct LibraryState {
    db: LibraryDb,
    watch: WatchService,
    /// False in `--bench`/`--self-test`: no database worker is running.
    enabled: bool,
    rows: Vec<LibraryEntry>,
    total: usize,
    loading: bool,
    query_id: u64,
    /// Request id for the "queue the whole library" query.
    all_id: u64,
    search: String,
    sort: LibrarySort,
    /// SQL window of the page currently on screen. Background refreshes and
    /// play-statistics updates re-query the same window.
    offset: usize,
    limit: usize,
    /// Set while the user is typing; the query runs after a short idle gap.
    pending_search: Option<Instant>,
    loaded_once: bool,
    index_active: bool,
    index_progress: usize,
    index_current: String,
    /// Result of the last explicit missing-file check (`None` = not run).
    missing_checked: Option<usize>,
    /// Latest aggregate statistics; `None` until the first query returns.
    stats: Option<crate::library::LibraryStats>,
    stats_loading: bool,
    smart_kind: SmartKind,
    smart_rows: Vec<LibraryEntry>,
    duplicate_mode: crate::config::DedupeMode,
    duplicate_groups: Vec<DuplicateGroup>,
    duplicate_rows: Vec<LibraryEntry>,
    duplicate_headers: Vec<bool>,
    watch_roots: Vec<PathBuf>,
    watch_extensions: Vec<String>,
}

struct DemoClock {
    position: f64,
    last: Instant,
    paused: bool,
}

/// Discover page: one random batch pulled when the page is opened or refreshed.
#[derive(Clone, Debug, Default)]
pub struct DiscoverState {
    pub songs: Vec<ApiSong>,
    pub loading: bool,
    pub error: Option<String>,
    pub rev: u64,
    request: u64,
}

/// One online drill-down (album or artist) shown on the search page.
#[derive(Clone, Debug, Default)]
pub struct OnlineDetail {
    pub title: String,
    pub subtitle: String,
    pub album_id: Option<String>,
    pub artist_id: Option<String>,
    pub items: Vec<ApiSong>,
    pub page: u32,
    pub limit: u32,
    pub total: u64,
    pub last_page: u32,
    pub exhausted: bool,
    pub has_more: bool,
    pub loading: bool,
    pub error: Option<String>,
    request: u64,
}

/// Online search state. The UI only reads this; requests are issued through
/// `Command::OnlineSearch`/`OnlineOpenAlbum`/`OnlineOpenArtist`.
#[derive(Clone, Debug, Default)]
pub struct OnlineState {
    pub query: String,
    pub kind: SearchKind,
    pub songs: Vec<ApiSong>,
    pub total: u64,
    /// Request window of the current search page.
    pub offset: u32,
    pub limit: u32,
    /// Highest page index that actually returned rows.
    pub last_page: u32,
    /// A forward request returned an empty page: hide "next" and stay on the
    /// last non-empty page.
    pub exhausted: bool,
    /// The last page was full, so another page may exist. The server's
    /// `total` is a rendering hint and cannot be used for pagination.
    pub has_more: bool,
    pub loading: bool,
    pub error: Option<String>,
    /// Rows come from the persistent search cache (offline or before the
    /// network request finishes).
    pub from_cache: bool,
    pub suggestions: Vec<String>,
    pub detail: Option<OnlineDetail>,
    pub rev: u64,
    request: u64,
    suggest_request: u64,
}

impl OnlineState {
    /// Rows currently visible: the drill-down list wins over search results.
    pub fn rows(&self) -> &[ApiSong] {
        match &self.detail {
            Some(detail) => &detail.items,
            None => &self.songs,
        }
    }

    pub fn is_detail(&self) -> bool {
        self.detail.is_some()
    }

    pub fn loading(&self) -> bool {
        self.detail
            .as_ref()
            .map_or(self.loading, |detail| detail.loading)
    }

    pub fn error(&self) -> Option<&str> {
        self.detail
            .as_ref()
            .and_then(|detail| detail.error.as_deref())
            .or(self.error.as_deref())
    }
}

/// Current Bamboo resolve in flight.
struct OnlineResolve {
    id: u64,
    key: String,
}

/// Everything needed to queue the same download again. Retry and per-track
/// quality changes reuse this instead of asking the search page again.
#[derive(Clone, Debug)]
pub struct DownloadRequest {
    pub track: Track,
    pub source: Option<SourceConfig>,
    pub quality: Option<crate::config::Quality>,
    pub network: crate::config::NetworkSettings,
    pub settings: crate::config::DownloadSettings,
    pub base_dir: PathBuf,
}

/// One row of the download manager.
#[derive(Clone, Debug)]
pub struct DownloadRow {
    pub id: u64,
    pub title: String,
    pub artist: String,
    pub state: DownloadState,
    pub received: u64,
    pub total: Option<u64>,
    pub path: Option<PathBuf>,
    pub error: Option<String>,
    pub request: DownloadRequest,
}

impl DownloadRow {
    pub fn quality(&self) -> Option<crate::config::Quality> {
        self.request.quality
    }
}

enum SessionSaveJob {
    Save(Box<Session>),
    /// Answer once every earlier job has been written; used by the exit path
    /// so an outdated snapshot can never overwrite the final one.
    Flush(std::sync::mpsc::Sender<()>),
}

/// Owns the thread that serializes and writes `session.json`. Building the
/// snapshot still happens on the caller, but the JSON encode and the file IO
/// (both hundreds of milliseconds for a 9k-track queue) run here.
struct SessionWriter {
    tx: std::sync::mpsc::Sender<SessionSaveJob>,
}

impl SessionWriter {
    fn new(path: PathBuf) -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<SessionSaveJob>();
        let _ = std::thread::Builder::new()
            .name("bamboo-session-writer".to_string())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    match job {
                        SessionSaveJob::Save(session) => {
                            if let Err(err) = save_session(&path, &session) {
                                tracing::warn!("background session save failed: {err}");
                            }
                        }
                        SessionSaveJob::Flush(ack) => {
                            let _ = ack.send(());
                        }
                    }
                }
            });
        Self { tx }
    }

    fn save(&self, session: Session) -> bool {
        self.tx
            .send(SessionSaveJob::Save(Box::new(session)))
            .is_ok()
    }

    fn flush(&self) {
        let (tx, rx) = std::sync::mpsc::channel();
        if self.tx.send(SessionSaveJob::Flush(tx)).is_ok() {
            let _ = rx.recv_timeout(Duration::from_secs(10));
        }
    }
}

impl AppCore {
    pub fn new(
        paths: Paths,
        wake: Option<Arc<dyn Fn() + Send + Sync>>,
        load_library: bool,
    ) -> Self {
        let mut settings = load_settings(&paths.settings_file);
        let mut session: Session = load_json(&paths.session_file);
        if session.saved_at.is_some() {
            settings.playback.volume = session.volume.clamp(0.0, 1.0);
        } else if session.volume <= 0.0 {
            session.volume = settings.playback.volume;
        }
        crate::i18n::set_language(settings.language);

        let search_cache_path = paths.cache.join("search.json");
        let search_cache = SearchCache::load(&search_cache_path);
        // Serializing a large queue (thousands of tracks) blocks for over
        // 100 ms, so periodic session saves must not run on the UI thread.
        let session_writer = SessionWriter::new(paths.session_file.clone());

        let library = LibraryState {
            db: if load_library {
                LibraryDb::open_with_wake(paths.library_db.clone(), wake.clone())
            } else {
                LibraryDb::disabled()
            },
            watch: if load_library {
                WatchService::with_wake(wake.clone())
            } else {
                WatchService::disabled()
            },
            enabled: load_library,
            rows: Vec::new(),
            total: 0,
            loading: false,
            query_id: 0,
            all_id: 0,
            search: String::new(),
            sort: LibrarySort::Title,
            offset: 0,
            limit: LIBRARY_PAGE_SIZE,
            pending_search: None,
            loaded_once: false,
            index_active: false,
            index_progress: 0,
            index_current: String::new(),
            missing_checked: None,
            stats: None,
            stats_loading: false,
            smart_kind: SmartKind::RecentlyPlayed,
            smart_rows: Vec::new(),
            duplicate_mode: crate::config::DedupeMode::Metadata,
            duplicate_groups: Vec::new(),
            duplicate_rows: Vec::new(),
            duplicate_headers: Vec::new(),
            watch_roots: Vec::new(),
            watch_extensions: Vec::new(),
        };

        let engine = AudioEngine::new(
            settings.playback.volume,
            settings.playback.muted,
            settings.performance.release_audio_output,
            dsp_config(&settings),
            wake.clone(),
        );
        let mut queue = Queue::default();
        queue.add_many(session.queue.iter().cloned());
        queue.shuffle = session.shuffle;
        queue.repeat = session.repeat;
        if queue.shuffle && !session.shuffle_keys.is_empty() {
            queue.restore_shuffle_order(&session.shuffle_keys);
        }
        // Restore the playlist position by track key: files may have been
        // added/removed, so the old index can point at a different song
        // (same approach as the reference implementation).
        queue.current = session
            .current_key
            .as_ref()
            .and_then(|key| queue.position_of_key(key))
            // A removed/renamed current file: fall back to the saved shuffle
            // cursor (which entry of the shuffled cycle was playing).
            .or_else(|| {
                if queue.shuffle {
                    session
                        .shuffle_cursor
                        .and_then(|position| queue.shuffle_order.get(position).copied())
                } else {
                    None
                }
            })
            .or_else(|| session.current_index.filter(|index| *index < queue.len()));

        let mut core = Self {
            notices: VecDeque::new(),
            notice_rev: 0,
            ab_start: None,
            ab_end: None,
            prefetched_lyrics: HashMap::new(),
            prefetched_order: VecDeque::new(),
            online: OnlineState::default(),
            discover: DiscoverState::default(),
            api: ApiService::with_wake(wake.clone()),
            online_resolve: None,
            online_next: None,
            online_next_request: None,
            loudness: None,
            loudness_pending: None,
            wake: wake.clone(),
            downloads: DownloadService::with_wake(
                settings.download.concurrency.clamp(1, 4) as usize,
                wake.clone(),
            ),
            download_rows: Vec::new(),
            download_next_id: 1,
            download_rev: 0,
            search_cache,
            search_cache_path,
            search_cache_dirty: false,
            search_cache_saved: Instant::now(),
            cache_service: StreamCacheService::with_wake(wake.clone()),
            cache_usage: CacheUsage::default(),
            cache_rev: 0,
            partial_cleanup: None,
            online_info: None,
            settings,
            session,
            paths,
            engine,
            queue,
            current_track: None,
            current_duration: None,
            playback_state: PlaybackState::Stopped,
            engine_load_token: 0,
            armed_next: None,
            deferred_start: None,
            lyrics_service: LyricsService::with_wake(wake.clone()),
            lyrics_background_service: LyricsBackgroundService::with_wake(wake.clone()),
            lyrics_background: None,
            lyrics_background_key: None,
            lyrics_background_pending: None,
            lyrics_background_rev: 0,
            background_image_service: BackgroundImageService::with_wake(wake.clone()),
            background_image: None,
            background_image_key: None,
            background_image_pending: None,
            background_image_rev: 0,
            lyrics: None,
            lyrics_key: String::new(),
            lyrics_loading: false,
            lyrics_error: None,
            lyrics_rev: 0,
            library_rev: 0,
            queue_rev: 0,
            cover: CoverState {
                service: CoverService::with_wake(wake.clone()),
                key: String::new(),
                image: None,
                png: None,
                rev: 0,
            },
            row_covers: RowCovers {
                service: RowCoverService::with_wake(wake.clone()),
                images: HashMap::new(),
                inflight: HashSet::new(),
                order: VecDeque::new(),
                rev: 0,
            },
            session_dirty: false,
            settings_dirty: false,
            session_writer,
            last_session_save: Instant::now(),
            last_settings_save: Instant::now(),
            demo: None,
            sleep_deadline: None,
            sleep_minutes: None,
            library,
            browser: FolderBrowser::default(),
            browse_service: BrowseService::with_wake(wake.clone()),
            browse_rev: 0,
        };

        core.engine.set_visual_enabled(
            core.settings.visualizer.enabled && core.settings.features.visualizer,
        );
        core.engine
            .set_device(core.settings.playback.device.clone());
        core.api
            .set_min_interval_ms(core.settings.network.api_min_interval_ms);
        // Measure the stream cache and evict expired entries once at startup.
        core.refresh_stream_cache(true);
        core.request_lyrics_background();
        core.request_background_image();

        // Startup restores which track was playing (and the shuffled cycle)
        // but always starts it at 0:00: per-song resume offsets are not
        // remembered, only the playlist position.
        let autoplay = core.settings.playback.resume_on_start;
        if core.settings.playlist.auto_refresh_on_startup
            && !core.queue.is_empty()
            && let Some(path) = core.session.playlist_path.clone()
            && path.is_file()
        {
            core.refresh_startup_playlist(&path);
        }
        if let Some(index) = core.queue.current {
            if autoplay {
                core.load_index(index, 0.0, true);
            } else {
                core.restore_paused(index, 0.0);
            }
        }

        if load_library {
            core.sync_library_watch();
            core.reset_library_query();
            if core.settings.library.scan_on_start && !core.settings.library.folders.is_empty() {
                core.start_library_index();
            }
        }
        core
    }

    pub fn handle(&mut self, command: Command) {
        match command {
            Command::PlayPause => self.toggle_play(),
            Command::Next => self.next_track(),
            Command::Prev => self.prev_track(),
            Command::Stop => {
                self.engine.stop();
                self.playback_state = PlaybackState::Stopped;
                self.session_dirty = true;
            }
            Command::SeekSecs(secs) => self.seek_to(secs),
            Command::SetVolume(volume) => self.set_volume(volume),
            Command::ToggleMute => self.toggle_mute(),
            Command::PlayIndex(index) => self.play_index(index),
            Command::AddPaths(paths) => {
                // Dropping/opening files starts playback when the queue was
                // empty; adding to a running queue must not interrupt it.
                self.add_paths(paths, true);
            }
            Command::AddPathsAt { paths, index } => self.add_paths_at(paths, index),
            Command::BrowseTo(path) => self.browse_to(path),
            Command::BrowseBack => self.browse_history(false),
            Command::BrowseForward => self.browse_history(true),
            Command::BrowseUp => {
                let parent = self.browser.current.as_ref().and_then(|path| path.parent());
                match parent {
                    Some(parent) if !parent.as_os_str().is_empty() => {
                        self.browse_to(Some(parent.to_path_buf()));
                    }
                    _ => self.browse_to(None),
                }
            }
            Command::BrowsePlay(index) => self.browse_play(index),
            Command::BrowseEnqueue(index) => self.browse_enqueue(index),
            Command::EnqueuePaths(paths) => {
                let before = self.queue.len();
                self.add_paths(paths, false);
                let added = self.queue.len().saturating_sub(before);
                if added > 0 {
                    self.notify(
                        crate::trf!("已把 {} 首歌加入播放队列", added),
                        false,
                    );
                }
            }
            Command::BrowseAddAll => self.browse_add_all(),
            Command::BrowsePlayAll => self.browse_play_all(),
            Command::RemoveFromQueue(index) => self.remove_from_queue(index),
            Command::QueueMove { from, to } => {
                if self.queue.move_item(from, to) {
                    self.queue_changed();
                }
            }
            Command::LibraryQueue { index, next } => self.queue_library_entry(index, next),
            Command::ToggleShuffle => self.toggle_shuffle(),
            Command::CycleRepeat => self.cycle_repeat(),
            Command::ClearQueue => {
                self.engine.stop();
                self.disarm_next();
                self.queue.clear();
                self.deferred_start = None;
                self.current_track = None;
                self.current_duration = None;
                self.playback_state = PlaybackState::Stopped;
                self.lyrics = None;
                self.lyrics_key.clear();
                self.lyrics_loading = false;
                self.lyrics_error = None;
                self.lyrics_rev = self.lyrics_rev.wrapping_add(1);
                self.clear_cover();
                self.queue_changed();
                // Clearing the queue also detaches the remembered playlist;
                // otherwise startup auto-refresh would import it again.
                self.session.playlist_path = None;
                self.session_dirty = true;
            }
            Command::SetPage(page) => {
                self.session.page = page;
                self.session_dirty = true;
                if page == Page::Statistics {
                    self.request_statistics();
                }
                if page == Page::Smart {
                    self.request_smart(self.library.smart_kind);
                }
                if page == Page::Duplicates {
                    self.request_duplicates(self.library.duplicate_mode);
                }
                if page == Page::Discover
                    && self.discover.songs.is_empty()
                    && !self.discover.loading
                {
                    self.discover_refresh();
                }
                // First visit to the folder page lists drives; later visits
                // keep the directory the user was in.
                if page == Page::Folders
                    && self.browser.current.is_none()
                    && self.browser.entries.is_empty()
                {
                    self.browse_to(None);
                }
            }
            Command::OnlineSetQuery(text) => {
                self.online.query = text;
                self.online.rev = self.online.rev.wrapping_add(1);
            }
            Command::OnlineSearch {
                query,
                offset,
                limit,
            } => self.start_online_search(query, offset, limit),
            Command::OnlinePage(delta) => self.online_turn_page(delta),
            Command::OnlineSetKind(kind) => {
                if self.online.kind != kind {
                    self.online.kind = kind;
                    self.online.detail = None;
                    if !self.online.query.trim().is_empty() {
                        let limit = self.online.limit.max(1);
                        self.start_online_search(self.online.query.clone(), 0, limit);
                    } else {
                        self.online.rev = self.online.rev.wrapping_add(1);
                    }
                }
            }
            Command::OnlinePlay(index) => self.play_online(index, false),
            Command::OnlineQueue(index) => self.play_online(index, true),
            Command::OnlineQueueAll => self.queue_online_all(),
            Command::OnlineDownload(index) => self.download_online(index),
            Command::DownloadQueue(index) => self.download_queue(index),
            Command::OnlineOpenAlbum(album_id) => self.open_online_album(album_id),
            Command::OnlineOpenArtist(artist_id) => self.open_online_artist(artist_id),
            Command::OnlineBack => {
                if self.online.detail.take().is_some() {
                    self.online.rev = self.online.rev.wrapping_add(1);
                }
            }
            Command::OnlineDetailMore => self.online_turn_page(1),
            Command::OnlineRandom => self.start_online_random(),
            Command::DiscoverRefresh => self.discover_refresh(),
            Command::DiscoverPlay(index) => self.discover_play(index, false),
            Command::DiscoverQueue(index) => self.discover_play(index, true),
            Command::DiscoverDownload(index) => self.discover_download(index),
            Command::OpenUrl(url) => self.open_url(url),
            Command::SetSourceEnabled(on) => self.edit_primary_source(|source| source.enabled = on),
            Command::SetSourceName(name) => {
                self.edit_primary_source(|source| source.name = name.trim().to_string())
            }
            Command::SetSourceBaseUrl(url) => {
                let url = url.trim().trim_end_matches('/').to_string();
                self.edit_primary_source(|source| source.base_url = url);
                // The old results/agents belong to the previous endpoint.
                self.online.detail = None;
                self.online.songs.clear();
                self.online.error = None;
                self.online.rev = self.online.rev.wrapping_add(1);
            }
            Command::SetSourceScript(script) => {
                self.edit_primary_source(|source| source.script = script.trim().to_string())
            }
            Command::SetSourceQuality(quality) => {
                self.edit_primary_source(|source| source.quality = quality)
            }
            Command::SetSourceSessionKey(key) => {
                self.edit_primary_source(|source| source.session_key = key.trim().to_string())
            }
            Command::SetSourceCookie(cookie) => {
                self.edit_primary_source(|source| source.cookie = cookie.trim().to_string())
            }
            Command::SetSourceInsecureTls(on) => {
                self.edit_primary_source(|source| source.insecure_tls = on)
            }
            Command::CyclePrimarySource => self.cycle_primary_source(),
            Command::AddSource => self.add_source(),
            Command::RemovePrimarySource => self.remove_primary_source(),
            Command::SetSourceHeaders(headers) => {
                self.edit_primary_source(|source| source.headers = headers)
            }
            Command::SetSourceEndpoints(endpoints) => {
                self.edit_primary_source(|source| source.endpoints = endpoints)
            }
            Command::SetNetworkOffline(offline) => {
                self.settings.network.offline = offline;
                self.settings_dirty = true;
            }
            Command::SetApiMinInterval(ms) => {
                let ms = ms.min(3000);
                self.settings.network.api_min_interval_ms = ms;
                self.api.set_min_interval_ms(ms);
                self.settings_dirty = true;
            }
            Command::SetAlbumPageSize(size) => {
                self.settings.network.album_page_size = size.clamp(10, 200);
                self.settings_dirty = true;
            }
            Command::SetNetworkTimeout(secs) => {
                self.settings.network.timeout_secs = secs.clamp(2, 120);
                self.settings.network.connect_timeout_secs =
                    self.settings.network.connect_timeout_secs.min(secs.clamp(1, 120));
                self.settings_dirty = true;
            }
            Command::SetNetworkRetries(retries) => {
                self.settings.network.retries = retries.min(10);
                self.settings_dirty = true;
            }
            Command::SetProxyMode(mode) => {
                self.settings.network.proxy_mode = mode;
                self.settings_dirty = true;
            }
            Command::SetNetworkProxyUrl(url) => {
                self.settings.network.proxy_url = url.trim().to_string();
                self.settings_dirty = true;
            }
            Command::SetCacheLimitMb(mb) => {
                self.settings.network.cache_limit_mb = mb.clamp(0, 16 * 1024);
                self.settings_dirty = true;
                self.refresh_stream_cache(true);
            }
            Command::SetStreamCacheDir(dir) => {
                self.settings.network.stream_cache_dir =
                    dir.filter(|dir| !dir.as_os_str().is_empty());
                self.settings_dirty = true;
                self.refresh_stream_cache(true);
            }
            Command::ClearStreamCache => self.clear_stream_cache(),
            Command::SetDownloadDir(dir) => {
                self.settings.download.dir = dir.filter(|dir| !dir.as_os_str().is_empty());
                self.settings_dirty = true;
            }
            Command::SetDownloadConcurrency(value) => {
                let value = value.clamp(1, 4);
                self.settings.download.concurrency = value;
                self.downloads.set_concurrency(value as usize);
                self.settings_dirty = true;
            }
            Command::SetDownloadConflict(policy) => {
                self.settings.download.conflict = policy;
                self.settings_dirty = true;
            }
            Command::SetDownloadQuality(quality) => {
                self.settings.download.quality = quality;
                self.settings_dirty = true;
            }
            Command::SetDownloadTemplate(template) => {
                let template = template.trim().to_string();
                if !template.is_empty() {
                    self.settings.download.template = template;
                    self.settings_dirty = true;
                }
            }
            Command::SetDownloadEmbedTags(on) => {
                self.settings.download.embed_tags = on;
                self.settings_dirty = true;
            }
            Command::SetDownloadEmbedCover(on) => {
                self.settings.download.embed_cover = on;
                self.settings_dirty = true;
            }
            Command::SetDownloadSaveLyrics(on) => {
                self.settings.download.save_lyrics = on;
                self.settings_dirty = true;
            }
            Command::SetDownloadAddToLibrary(on) => {
                self.settings.download.add_to_library = on;
                self.settings_dirty = true;
            }
            Command::CancelDownload(id) => {
                self.downloads.cancel(id);
                if let Some(row) = self.download_rows.iter_mut().find(|row| row.id == id) {
                    row.state = DownloadState::Canceled;
                }
                self.download_rev = self.download_rev.wrapping_add(1);
            }
            Command::RetryDownload(id) => self.retry_download(id),
            Command::CycleDownloadQuality(id) => {
                if let Some(row) = self.download_rows.iter_mut().find(|row| row.id == id)
                    && !row.state.is_active()
                {
                    row.request.quality = next_quality(row.request.quality);
                    self.download_rev = self.download_rev.wrapping_add(1);
                }
            }
            Command::ClearFinishedDownloads => {
                self.download_rows
                    .retain(|row| row.state.is_active());
                self.download_rev = self.download_rev.wrapping_add(1);
            }
            Command::CleanPartialDownloads => self.clean_partial_downloads(),
            Command::OnlineTest => {
                let Some(source) = self.online_primary_source() else {
                    self.notify(crate::i18n::tr("请先在设置中添加在线音源"), true);
                    return;
                };
                self.api.test(&source, &self.settings.network);
                self.notify(crate::i18n::tr("正在检测在线音源…"), false);
            }
            Command::LibrarySearch(text) => {
                if self.library.search != text {
                    self.library.search = text;
                    self.library.pending_search = Some(Instant::now());
                    self.library.loading = true;
                }
            }
            Command::LibraryRefresh => self.reset_library_query(),
            Command::LibraryReindex => self.start_library_index(),
            Command::LibraryAddFolder(path) => self.add_library_folder(path),
            Command::LibraryPage { offset, limit } => self.set_library_page_window(offset, limit),
            Command::LibraryPlay(index) => self.play_library_entry(index),
            Command::LibraryQueueAll => self.queue_all_library(),
            Command::LibraryRemove(index) => self.remove_library_entry(index),
            Command::LibraryCancelIndex => {
                if self.library.index_active {
                    self.library.db.cancel_index();
                }
            }
            Command::LibraryCheckMissing => {
                self.library.missing_checked = None;
                self.library.db.check_missing();
            }
            Command::LibraryStats => self.request_statistics(),
            Command::StatisticsPlay(index) => self.play_statistics_entry(index),
            Command::LibrarySmart(kind) => self.request_smart(kind),
            Command::LibraryPlaySmart(index) => self.play_smart_entry(index),
            Command::LibraryQueueSmart(index) => self.queue_smart_entry(index),
            Command::LibraryRemoveSmart(index) => self.remove_smart_entry(index),
            Command::LibraryDuplicates(mode) => self.request_duplicates(mode),
            Command::LibraryPlayDuplicate(index) => self.play_duplicate_entry(index),
            Command::LibraryRemoveDuplicate(index) => self.remove_duplicate_entry(index),
            Command::LibraryKeepDuplicateGroup(index) => self.keep_duplicate_group(index),
            Command::LibraryRemoveDuplicateGroup(index) => {
                self.remove_duplicate_group(index)
            }
            Command::LibraryOptimize => {
                if !self.library.enabled {
                    self.notify(crate::i18n::tr("曲库未启用，无法优化索引"), true);
                    return;
                }
                self.library.stats_loading = true;
                self.library.db.optimize();
                self.notify(crate::i18n::tr("正在优化曲库索引…"), false);
            }
            Command::LibraryResetStats => {
                if !self.library.enabled {
                    return;
                }
                self.library.db.reset_stats();
                self.notify(crate::i18n::tr("播放统计已重置"), false);
            }
            Command::LibrarySetSort(sort) => {
                if self.library.sort != sort {
                    self.library.sort = sort;
                    self.reset_library_query();
                }
            }
            Command::LibrarySetScanOnStart(on) => {
                self.settings.library.scan_on_start = on;
                self.settings_dirty = true;
            }
            Command::LibrarySetAutoWatch(on) => {
                self.settings.library.auto_watch = on;
                self.settings_dirty = true;
                self.sync_library_watch();
            }
            Command::LibrarySetFollowSymlinks(on) => {
                self.settings.library.follow_symlinks = on;
                self.settings_dirty = true;
                self.sync_library_watch();
            }
            Command::LibrarySetDedupeMode(mode) => {
                self.settings.library.dedupe_mode = mode;
                self.settings_dirty = true;
            }
            Command::LibrarySetExtensions(text) => self.set_library_extensions(&text),
            Command::LibraryRemoveFolder(path) => {
                self.settings.library.folders.retain(|root| root != &path);
                self.settings_dirty = true;
                self.sync_library_watch();
            }
            Command::ReloadLyrics => {
                if let Some(track) = self.current_track.clone() {
                    self.refresh_lyrics(&track);
                }
            }
            Command::LoadLyricsFile(path) => {
                if self.current_track.is_none() {
                    return;
                }
                let region =
                    crate::lyrics::resolve_region(&self.settings.lyrics.translation_language);
                self.lyrics_service.request_file(
                    self.lyrics_key.clone(),
                    path,
                    self.settings.lyrics.encoding,
                    &region,
                );
                self.lyrics_loading = true;
                self.lyrics_error = None;
                self.lyrics_rev = self.lyrics_rev.wrapping_add(1);
            }
            Command::SetSpeed(speed) => {
                let speed = speed.clamp(0.5, 2.0);
                if (self.settings.playback.speed - speed).abs() > 0.0005 {
                    self.settings.playback.speed = speed;
                    self.settings_dirty = true;
                    self.engine.set_dsp(dsp_config(&self.settings));
                }
            }
            Command::SetEqEnabled(on) => {
                if self.settings.playback.eq_enabled != on {
                    self.settings.playback.eq_enabled = on;
                    self.settings_dirty = true;
                    self.engine.set_dsp(dsp_config(&self.settings));
                }
            }
            Command::SetPitchPreserve(on) => {
                if self.settings.playback.pitch_preserve != on {
                    self.settings.playback.pitch_preserve = on;
                    self.settings_dirty = true;
                    self.engine.set_dsp(dsp_config(&self.settings));
                }
            }
            Command::SetGapless(on) => {
                self.settings.playback.gapless = on;
                self.settings_dirty = true;
                if on {
                    self.arm_next();
                } else {
                    self.disarm_next();
                }
            }
            Command::SetAutoAdvance(on) => {
                self.settings.playback.auto_advance = on;
                self.settings_dirty = true;
            }
            Command::SetCrossfade(secs) => {
                self.settings.playback.crossfade_secs = secs.clamp(0.0, 30.0);
                self.settings_dirty = true;
                self.engine.set_dsp(dsp_config(&self.settings));
            }
            Command::SetReplayGainMode(mode) => {
                self.settings.playback.replaygain = mode;
                self.settings_dirty = true;
                self.engine.set_dsp(dsp_config(&self.settings));
            }
            Command::SetReplayGainPreamp(db) => {
                self.settings.playback.replaygain_preamp_db = db.clamp(-24.0, 24.0);
                self.settings_dirty = true;
                self.engine.set_dsp(dsp_config(&self.settings));
            }
            Command::SetEqPreset(preset) => {
                self.settings.playback.eq_preset = preset;
                // "Custom" keeps the hand-edited band gains.
                if preset != EqPreset::Custom {
                    self.settings.playback.eq_bands = preset.bands();
                }
                if preset != EqPreset::Flat {
                    self.settings.playback.eq_enabled = true;
                }
                self.settings_dirty = true;
                self.engine.set_dsp(dsp_config(&self.settings));
            }
            Command::SetEqBand(index, db) => {
                if let Some(band) = self.settings.playback.eq_bands.get_mut(index) {
                    *band = db.clamp(-24.0, 24.0);
                    self.settings.playback.eq_preset = EqPreset::Custom;
                    self.settings.playback.eq_enabled = true;
                    self.settings_dirty = true;
                    self.engine.set_dsp(dsp_config(&self.settings));
                }
            }
            Command::SetReplayGainScan(on) => {
                self.settings.playback.replaygain_scan = on;
                self.settings_dirty = true;
            }
            Command::SetSleepTimer(minutes) => {
                self.sleep_minutes = minutes;
                self.sleep_deadline = minutes
                    .map(|minutes| Instant::now() + Duration::from_secs(minutes as u64 * 60));
            }
            Command::SetABStart => {
                self.ab_start = Some(self.position_secs());
                if self.ab_end.is_some_and(|end| end <= self.position_secs()) {
                    self.ab_end = None;
                }
            }
            Command::SetABEnd => {
                let end = self.position_secs();
                if self.ab_start.is_some_and(|start| end > start + 0.25) {
                    self.ab_end = Some(end);
                } else {
                    self.notify(
                        crate::i18n::tr("B 点必须晚于 A 点"),
                        true,
                    );
                }
            }
            Command::ClearABRepeat => {
                self.ab_start = None;
                self.ab_end = None;
            }
            Command::SetOutputDevice(device) => {
                self.settings.playback.device = device.clone();
                self.settings_dirty = true;
                self.engine.set_device(device);
            }
            Command::SetCloseBehavior(behavior) => {
                self.settings.window.close_behavior = behavior;
                self.settings_dirty = true;
            }
            Command::SetStartMinimized(on) => {
                self.settings.window.start_minimized = on;
                self.settings_dirty = true;
            }
            Command::SetRememberPosition(on) => {
                self.settings.window.remember_position = on;
                self.settings_dirty = true;
            }
            Command::SetCaptureProtection(on) => {
                self.settings.window.capture_protection = on;
                self.settings_dirty = true;
            }
            Command::SetLyricsCaptureProtection(on) => {
                self.settings.window.lyrics_capture_protection = on;
                self.settings_dirty = true;
            }
            Command::SetAllowMultipleInstances(on) => {
                self.settings.window.allow_multiple_instances = on;
                self.settings_dirty = true;
            }
            Command::SetStartWithWindows(on) => {
                self.settings.features.start_with_windows = on;
                self.settings_dirty = true;
            }
            Command::SetFileAssociations(on) => {
                self.settings.features.file_associations = on;
                self.settings_dirty = true;
            }
            Command::SetPlaylistAssociations(on) => {
                self.settings.features.playlist_associations = on;
                self.settings_dirty = true;
            }
            Command::SetMediaControls(on) => {
                self.settings.features.media_controls = on;
                self.settings_dirty = true;
            }
            Command::SetNotifications(on) => {
                self.settings.features.notifications = on;
                self.settings_dirty = true;
            }
            Command::SetJumpList(on) => {
                self.settings.features.jump_list = on;
                self.settings_dirty = true;
            }
            Command::SetThemeMode(mode) => {
                self.settings.theme.mode = mode;
                self.settings_dirty = true;
            }
            Command::SetThemeFontFamily(family) => {
                self.settings.theme.font_family = family
                    .map(|name| name.trim().to_string())
                    .filter(|name| !name.is_empty());
                self.settings_dirty = true;
            }
            Command::SetThemeFontSize(size) => {
                self.settings.theme.font_size = size.clamp(10.0, 24.0);
                self.settings_dirty = true;
            }
            Command::SetAccent(color) => {
                self.settings.theme.accent = color;
                self.settings_dirty = true;
            }
            Command::SetListDensity(density) => {
                self.settings.theme.list_density = density;
                self.settings_dirty = true;
            }
            Command::SetReduceMotion(on) => {
                self.settings.theme.reduce_motion = on;
                self.settings_dirty = true;
            }
            Command::SetFrameRate(fps) => {
                self.settings.performance.frame_rate = fps.clamp(15, 120);
                self.settings_dirty = true;
            }
            Command::SetMetadataMode(mode) => {
                self.settings.performance.metadata_mode = mode;
                self.settings_dirty = true;
            }
            Command::SetLoadCovers(on) => {
                self.settings.performance.load_covers = on;
                self.settings_dirty = true;
                if !on {
                    self.clear_cover();
                } else if let Some(track) = self.current_track.clone() {
                    self.refresh_cover(&track);
                }
            }
            Command::SetLazyAudio(on) => {
                self.settings.performance.lazy_audio = on;
                self.settings_dirty = true;
            }
            Command::SetReleaseAudioOutput(on) => {
                self.settings.performance.release_audio_output = on;
                self.settings_dirty = true;
                self.engine.set_release_when_idle(on);
            }
            Command::SetDeferPausedRestore(on) => {
                self.settings.performance.defer_paused_restore = on;
                self.settings_dirty = true;
            }
            Command::SetIdleFrameRate(fps) => {
                self.settings.performance.idle_frame_rate = fps.clamp(5, 60);
                self.settings_dirty = true;
            }
            Command::SetPauseAnimationsUnfocused(on) => {
                self.settings.performance.pause_animations_unfocused = on;
                self.settings_dirty = true;
            }
            Command::SetThumbnailCache(limit) => {
                self.settings.performance.thumbnail_cache = limit.min(4096);
                self.settings_dirty = true;
            }
            Command::SetD2DHardware(on) => {
                self.settings.performance.d2d_hardware = on;
                self.settings_dirty = true;
            }
            Command::SetShortcutsEnabled(on) => {
                self.settings.shortcuts.enabled = on;
                self.settings_dirty = true;
            }
            Command::SetGamepadEnabled(on) => {
                self.settings.shortcuts.gamepad = on;
                self.settings_dirty = true;
            }
            Command::SetShortcut(slot, combo) => {
                // Empty is a valid binding: the action simply has no global
                // hotkey. Esc during capture writes an empty string.
                self.settings.shortcuts.set(slot, combo.trim().to_string());
                self.settings_dirty = true;
            }
            Command::ResetShortcuts => {
                self.settings.shortcuts = crate::config::ShortcutSettings::default();
                self.settings_dirty = true;
            }
            Command::SetTrimWorkingSetOnIdle(on) => {
                self.settings.window.trim_working_set_on_idle = on;
                self.settings_dirty = true;
            }
            Command::SetTrimIdleSecs(secs) => {
                self.settings.window.trim_idle_secs = secs.clamp(5, 3600);
                self.settings_dirty = true;
            }
            Command::SetBackgroundMode(mode) => {
                self.settings.theme.background = mode;
                self.settings_dirty = true;
                self.request_background_image();
            }
            Command::SetBackgroundImage(path) => {
                self.settings.theme.background_image = path.clone();
                self.settings.theme.background = if path.is_some() {
                    crate::config::BackgroundMode::Image
                } else {
                    crate::config::BackgroundMode::Solid
                };
                self.settings.theme.background_image = path;
                self.settings_dirty = true;
                self.request_background_image();
            }
            Command::SetVisualizerEnabled(on) => {
                self.settings.visualizer.enabled = on;
                self.settings_dirty = true;
                self.engine.set_visual_enabled(on && self.settings.features.visualizer);
            }
            Command::SetVisualizerMode(mode) => {
                self.settings.visualizer.mode = mode;
                self.settings_dirty = true;
            }
            Command::SetLyricsKaraoke(on) => {
                self.settings.lyrics.karaoke = on;
                self.settings.large_lyrics.karaoke = on;
                self.settings_dirty = true;
            }
            Command::SetLyricsWordsOnly(on) => {
                self.settings.lyrics.karaoke_words_only = on;
                self.settings.large_lyrics.karaoke_words_only = on;
                self.settings_dirty = true;
            }
            Command::SetLyricsTranslation(on) => {
                self.settings.lyrics.show_translation = on;
                self.settings.large_lyrics.show_translation = on;
                self.settings_dirty = true;
            }
            Command::SetLyricsFontSize(size) => {
                // Independent from the standalone window font now that the
                // "独立歌词" tab exposes its own size.
                self.settings.lyrics.font_size = size.clamp(8.0, 120.0);
                self.settings_dirty = true;
            }
            Command::SetLyricsLineSpacing(spacing) => {
                self.settings.lyrics.line_spacing = spacing.clamp(0.0, 120.0);
                self.settings_dirty = true;
            }
            Command::SetLyricsAlign(align) => {
                self.settings.lyrics.align = align;
                self.settings.large_lyrics.align = align;
                self.settings_dirty = true;
            }
            Command::SetLyricsFontFamily(family) => {
                self.settings.lyrics.font_family = family;
                self.settings_dirty = true;
            }
            Command::SetLyricsFontWeight(weight) => {
                self.settings.lyrics.font_weight = weight.clamp(100, 900);
                self.settings_dirty = true;
            }
            Command::SetLyricsItalic(on) => {
                self.settings.lyrics.italic = on;
                self.settings_dirty = true;
            }
            Command::SetLyricsOutlineWidth(width) => {
                self.settings.lyrics.outline_width = width.clamp(0.0, 8.0);
                self.settings_dirty = true;
            }
            Command::SetLyricsOutlineColor(color) => {
                self.settings.lyrics.outline_color = color;
                self.settings_dirty = true;
            }
            Command::SetLyricsShadowOffset(offset) => {
                self.settings.lyrics.shadow_offset = offset.clamp(0.0, 24.0);
                self.settings_dirty = true;
            }
            Command::SetLyricsShadowColor(color) => {
                self.settings.lyrics.shadow_color = color;
                self.settings_dirty = true;
            }
            Command::SetLyricsTranslationOutlineWidth(width) => {
                self.settings.lyrics.translation_outline_width = width.clamp(0.0, 8.0);
                self.settings_dirty = true;
            }
            Command::SetLyricsTranslationOutlineColor(color) => {
                self.settings.lyrics.translation_outline_color = color;
                self.settings_dirty = true;
            }
            Command::SetLyricsTranslationShadowOffset(offset) => {
                self.settings.lyrics.translation_shadow_offset = offset.clamp(0.0, 24.0);
                self.settings_dirty = true;
            }
            Command::SetLyricsTranslationShadowColor(color) => {
                self.settings.lyrics.translation_shadow_color = color;
                self.settings_dirty = true;
            }
            Command::SetLyricsEncoding(encoding) => {
                self.settings.lyrics.encoding = encoding;
                self.settings_dirty = true;
                self.prefetched_lyrics.clear();
                self.prefetched_order.clear();
                if let Some(track) = self.current_track.clone() {
                    self.refresh_lyrics(&track);
                }
            }
            Command::SetLyricsOffset(offset) => {
                self.settings.lyrics.offset_ms = offset.clamp(-60_000, 60_000);
                self.settings_dirty = true;
            }
            Command::SetLyricsTranslationLanguage(language) => {
                self.settings.lyrics.translation_language = language;
                self.settings_dirty = true;
                self.prefetched_lyrics.clear();
                self.prefetched_order.clear();
                if let Some(track) = self.current_track.clone() {
                    self.refresh_lyrics(&track);
                }
            }
            Command::SetLyricsPrefetch(on) => {
                self.settings.lyrics.prefetch_next = on;
                self.settings_dirty = true;
            }
            Command::SetLyricsShowSource(on) => {
                self.settings.lyrics.show_source = on;
                self.settings_dirty = true;
            }
            Command::SetLyricsPanelColor(color) => {
                self.settings.lyrics.panel_color = color;
                self.settings_dirty = true;
            }
            Command::SetLyricsPanelBackground(background) => {
                self.settings.lyrics.panel_background = background;
                self.settings_dirty = true;
                self.request_lyrics_background();
            }
            Command::SetLyricsPanelColor2(color) => {
                self.settings.lyrics.panel_color2 = color;
                self.settings_dirty = true;
            }
            Command::SetLyricsPanelBorderColor(color) => {
                self.settings.lyrics.panel_border_color = color;
                self.settings_dirty = true;
            }
            Command::SetLyricsPanelImage(path) => {
                self.settings.lyrics.panel_image = path;
                self.settings_dirty = true;
                self.request_lyrics_background();
            }
            Command::ResetLyricsPanelBackground => {
                let lyrics = &mut self.settings.lyrics;
                lyrics.panel_background = LyricsPanelBackground::Theme;
                lyrics.panel_color = None;
                lyrics.panel_color2 = [46, 52, 64];
                lyrics.panel_image = None;
                lyrics.panel_border_color = None;
                self.settings_dirty = true;
                self.request_lyrics_background();
            }
            Command::SetLargeLyricsFontFamily(family) => {
                self.settings.large_lyrics.font_family = family;
                self.settings_dirty = true;
            }
            Command::SetLargeLyricsFontWeight(weight) => {
                self.settings.large_lyrics.font_weight = weight.clamp(100, 900);
                self.settings_dirty = true;
            }
            Command::SetLargeLyricsItalic(on) => {
                self.settings.large_lyrics.italic = on;
                self.settings_dirty = true;
            }
            Command::SetLargeLyricsOutlineWidth(width) => {
                self.settings.large_lyrics.outline_width = width.clamp(0.0, 8.0);
                self.settings_dirty = true;
            }
            Command::SetLargeLyricsOutlineColor(color) => {
                self.settings.large_lyrics.outline_color = color;
                self.settings_dirty = true;
            }
            Command::SetLargeLyricsShadowOffset(offset) => {
                self.settings.large_lyrics.shadow_offset = offset.clamp(0.0, 24.0);
                self.settings_dirty = true;
            }
            Command::SetLargeLyricsShadowColor(color) => {
                self.settings.large_lyrics.shadow_color = color;
                self.settings_dirty = true;
            }
            Command::SetLargeLyricsTranslationOutlineWidth(width) => {
                self.settings.large_lyrics.translation_outline_width =
                    width.clamp(0.0, 8.0);
                self.settings_dirty = true;
            }
            Command::SetLargeLyricsTranslationOutlineColor(color) => {
                self.settings.large_lyrics.translation_outline_color = color;
                self.settings_dirty = true;
            }
            Command::SetLargeLyricsTranslationShadowOffset(offset) => {
                self.settings.large_lyrics.translation_shadow_offset =
                    offset.clamp(0.0, 24.0);
                self.settings_dirty = true;
            }
            Command::SetLargeLyricsTranslationShadowColor(color) => {
                self.settings.large_lyrics.translation_shadow_color = color;
                self.settings_dirty = true;
            }
            Command::SetLargeLyricsFontSize(size) => {
                self.settings.large_lyrics.font_size = size.clamp(10.0, 200.0);
                self.settings_dirty = true;
            }
            Command::SetLyricsHighlight(color) => {
                self.settings.lyrics.highlight_color = color;
                self.settings_dirty = true;
            }
            Command::SetLyricsCurrentColor(color) => {
                self.settings.lyrics.current_color = color;
                self.settings_dirty = true;
            }
            Command::SetLyricsNormalColor(color) => {
                self.settings.lyrics.normal_color = color;
                self.settings_dirty = true;
            }
            Command::SetLyricsTranslationColor(color) => {
                self.settings.lyrics.translation_color = color;
                self.settings_dirty = true;
            }
            Command::SetLargeLyricsLineSpacing(spacing) => {
                self.settings.large_lyrics.line_spacing = spacing.clamp(0.0, 160.0);
                self.settings_dirty = true;
            }
            Command::SetLargeLyricsAlign(align) => {
                self.settings.large_lyrics.align = align;
                self.settings_dirty = true;
            }
            Command::SetLargeLyricsTranslation(on) => {
                self.settings.large_lyrics.show_translation = on;
                self.settings_dirty = true;
            }
            Command::SetLargeLyricsKaraoke(on) => {
                self.settings.large_lyrics.karaoke = on;
                self.settings_dirty = true;
            }
            Command::SetLargeLyricsWordsOnly(on) => {
                self.settings.large_lyrics.karaoke_words_only = on;
                self.settings_dirty = true;
            }
            Command::SetLargeLyricsShowCover(on) => {
                self.settings.large_lyrics.show_cover = on;
                self.settings_dirty = true;
            }
            Command::SetLargeLyricsShowTitle(on) => {
                self.settings.large_lyrics.show_title = on;
                self.settings_dirty = true;
            }
            Command::SetLargeLyricsShowFooter(on) => {
                self.settings.large_lyrics.show_footer = on;
                self.settings_dirty = true;
            }
            Command::SetLargeLyricsHighlight(color) => {
                self.settings.large_lyrics.highlight_color = color;
                self.settings_dirty = true;
            }
            Command::SetLargeLyricsCurrentColor(color) => {
                self.settings.large_lyrics.current_color = color;
                self.settings_dirty = true;
            }
            Command::SetLargeLyricsNormalColor(color) => {
                self.settings.large_lyrics.normal_color = color;
                self.settings_dirty = true;
            }
            Command::SetLargeLyricsTranslationColor(color) => {
                self.settings.large_lyrics.translation_color = color;
                self.settings_dirty = true;
            }
            Command::SetDesktopLyricsColor(slot, color) => {
                let settings = &mut self.settings.desktop_lyrics;
                match slot {
                    0 => settings.current_color = [color[0], color[1], color[2], settings.current_color[3]],
                    1 => settings.next_color = [color[0], color[1], color[2], settings.next_color[3]],
                    2 => settings.prev_color = [color[0], color[1], color[2], settings.prev_color[3]],
                    3 => settings.footer_color = [color[0], color[1], color[2], settings.footer_color[3]],
                    4 => {
                        settings.translation_color =
                            [color[0], color[1], color[2], settings.translation_color[3]]
                    }
                    5 => {
                        settings.outline_color =
                            [color[0], color[1], color[2], settings.outline_color[3]]
                    }
                    6 => {
                        settings.shadow_color =
                            [color[0], color[1], color[2], settings.shadow_color[3]]
                    }
                    7 => {
                        settings.bg_color =
                            [color[0], color[1], color[2], settings.bg_color[3]]
                    }
                    8 => {
                        settings.bg_color2 =
                            [color[0], color[1], color[2], settings.bg_color2[3]]
                    }
                    9 => {
                        settings.translation_outline_color = [
                            color[0],
                            color[1],
                            color[2],
                            settings.translation_outline_color[3],
                        ]
                    }
                    10 => {
                        settings.translation_shadow_color = [
                            color[0],
                            color[1],
                            color[2],
                            settings.translation_shadow_color[3],
                        ]
                    }
                    11 => {
                        settings.footer_outline_color = [
                            color[0],
                            color[1],
                            color[2],
                            settings.footer_outline_color[3],
                        ]
                    }
                    _ => {
                        settings.footer_shadow_color = [
                            color[0],
                            color[1],
                            color[2],
                            settings.footer_shadow_color[3],
                        ]
                    }
                }
                self.settings_dirty = true;
            }
            Command::SetDesktopLyricsBackgroundImage(path) => {
                let settings = &mut self.settings.desktop_lyrics;
                settings.bg_image = path;
                if settings.bg_image.is_some() {
                    settings.background = crate::config::DesktopBackgroundMode::Image;
                }
                self.settings_dirty = true;
            }
            Command::SetDesktopLyricsFontFamily(family) => {
                self.settings.desktop_lyrics.font_family = family;
                self.settings_dirty = true;
            }
            Command::SetDesktopLyricsTranslationFontFamily(family) => {
                self.settings.desktop_lyrics.translation_font_family = family;
                self.settings_dirty = true;
            }
            Command::SetDesktopLyricsFooterFontFamily(family) => {
                self.settings.desktop_lyrics.footer_font_family = family;
                self.settings_dirty = true;
            }
            Command::SetDesktopLyricsTranslationBold(on) => {
                self.settings.desktop_lyrics.translation_bold = on;
                self.settings_dirty = true;
            }
            Command::SetDesktopLyricsFooterBold(on) => {
                self.settings.desktop_lyrics.footer_bold = on;
                self.settings_dirty = true;
            }
            Command::SetDesktopLyricsOutlineWidth(width) => {
                self.settings.desktop_lyrics.outline_width = width.clamp(0.0, 20.0);
                self.settings_dirty = true;
            }
            Command::SetDesktopLyricsShadowOffset(offset) => {
                self.settings.desktop_lyrics.shadow_offset = offset.clamp(0.0, 40.0);
                self.settings_dirty = true;
            }
            Command::SetDesktopLyricsItalic(on) => {
                self.settings.desktop_lyrics.italic = on;
                self.settings_dirty = true;
            }
            Command::SetDesktopLyricsTranslationOutlineWidth(width) => {
                self.settings.desktop_lyrics.translation_outline_width =
                    width.clamp(0.0, 20.0);
                self.settings_dirty = true;
            }
            Command::SetDesktopLyricsTranslationShadowOffset(offset) => {
                self.settings.desktop_lyrics.translation_shadow_offset =
                    offset.clamp(0.0, 40.0);
                self.settings_dirty = true;
            }
            Command::SetDesktopLyricsFooterOutlineWidth(width) => {
                self.settings.desktop_lyrics.footer_outline_width = width.clamp(0.0, 20.0);
                self.settings_dirty = true;
            }
            Command::SetDesktopLyricsFooterShadowOffset(offset) => {
                self.settings.desktop_lyrics.footer_shadow_offset = offset.clamp(0.0, 40.0);
                self.settings_dirty = true;
            }
            Command::SetDesktopLyricsBlurRadius(radius) => {
                self.settings.desktop_lyrics.blur_radius = radius.clamp(0.0, 64.0);
                self.settings_dirty = true;
            }
            Command::SetDesktopLyricsTranslationOutline(on) => {
                self.settings.desktop_lyrics.translation_outline = on;
                self.settings_dirty = true;
            }
            Command::SetDesktopLyricsTranslationShadow(on) => {
                self.settings.desktop_lyrics.translation_shadow = on;
                self.settings_dirty = true;
            }
            Command::SetDesktopLyricsFooterOutline(on) => {
                self.settings.desktop_lyrics.footer_outline = on;
                self.settings_dirty = true;
            }
            Command::SetDesktopLyricsFooterShadow(on) => {
                self.settings.desktop_lyrics.footer_shadow = on;
                self.settings_dirty = true;
            }
            Command::SetDesktopLyricsHideWhenFullscreen(on) => {
                self.settings.desktop_lyrics.hide_when_fullscreen = on;
                self.settings_dirty = true;
            }
            Command::SetMiniShowLyrics(on) => {
                self.settings.mini.show_lyrics = on;
                self.settings_dirty = true;
            }
            Command::SetMiniKaraoke(on) => {
                self.settings.mini.karaoke = on;
                self.settings_dirty = true;
            }
            Command::SetMiniWordsOnly(on) => {
                self.settings.mini.karaoke_words_only = on;
                self.settings_dirty = true;
            }
            Command::SetMiniLyricsLines(lines) => {
                self.settings.mini.lyrics_lines = lines.clamp(1, 2);
                self.settings_dirty = true;
            }
            Command::SetMiniLyricsBold(on) => {
                self.settings.mini.lyrics_bold = on;
                self.settings_dirty = true;
            }
            Command::SetMiniTranslation(on) => {
                self.settings.mini.show_translation = on;
                self.settings_dirty = true;
            }
            Command::SetMiniFontSize(size) => {
                self.settings.mini.lyrics_font_size = size.clamp(8.0, 64.0);
                self.settings_dirty = true;
            }
            Command::SetMiniFrameRate(fps) => {
                self.settings.mini.frame_rate = fps.clamp(15, 120);
                self.settings_dirty = true;
            }
            Command::SetMiniAutoHeight(on) => {
                self.settings.mini.auto_height = on;
                self.settings_dirty = true;
            }
            Command::SetMiniHeight(height) => {
                self.settings.mini.height = height.clamp(120.0, 600.0);
                self.settings.mini.auto_height = false;
                self.settings_dirty = true;
            }
            Command::SetMiniAlwaysOnTop(on) => {
                self.settings.mini.always_on_top = on;
                self.settings_dirty = true;
            }
            Command::SetMiniOpacity(opacity) => {
                self.settings.mini.opacity = opacity.clamp(0.0, 1.0);
                self.settings_dirty = true;
            }
            Command::SetMiniShowInTaskbar(on) => {
                self.settings.mini.show_in_taskbar = on;
                self.settings_dirty = true;
            }
            Command::SetMiniWidth(width) => {
                self.settings.mini.width = width.clamp(200.0, 1600.0);
                self.settings_dirty = true;
            }
            Command::SetMiniPanelColor(color) => {
                self.settings.mini.panel_color = color;
                self.settings_dirty = true;
            }
            Command::SetMiniTextColor(color) => {
                self.settings.mini.text_color = color;
                self.settings_dirty = true;
            }
            Command::SetMiniAccentColor(color) => {
                self.settings.mini.accent_color = color;
                self.settings_dirty = true;
            }
            Command::ToggleDesktopLyrics => {
                let settings = &mut self.settings.desktop_lyrics;
                let open = settings.enabled && settings.visible_at_start;
                settings.enabled = !open;
                settings.visible_at_start = !open;
                self.settings_dirty = true;
            }
            Command::ToggleMiniPlayer => {
                self.settings.mini.visible = !self.settings.mini.visible;
                self.settings_dirty = true;
            }
            Command::DesktopLyricsSetLock(on) => {
                self.settings.desktop_lyrics.locked = on;
                self.settings_dirty = true;
            }
            Command::DesktopLyricsSetClickThrough(on) => {
                self.settings.desktop_lyrics.click_through = on;
                self.settings_dirty = true;
            }
            Command::DesktopLyricsSetAlwaysOnTop(on) => {
                self.settings.desktop_lyrics.always_on_top = on;
                self.settings_dirty = true;
            }
            Command::DesktopLyricsFontDelta(delta) => {
                let settings = &mut self.settings.desktop_lyrics;
                let apply = |size: f32| (size + delta as f32).clamp(8.0, 200.0);
                settings.font_size = apply(settings.font_size);
                settings.next_font_size = apply(settings.next_font_size);
                settings.prev_font_size = apply(settings.prev_font_size);
                self.settings_dirty = true;
            }
            Command::DesktopLyricsSetLines(lines) => {
                self.settings.desktop_lyrics.lines = lines.clamp(1, 3);
                self.settings_dirty = true;
            }
            Command::DesktopLyricsSetAlign(align) => {
                self.settings.desktop_lyrics.align = match align {
                    0 => crate::config::LyricsAlign::Left,
                    2 => crate::config::LyricsAlign::Right,
                    _ => crate::config::LyricsAlign::Center,
                };
                self.settings_dirty = true;
            }
            Command::DesktopLyricsSetAutoSize(on) => {
                self.settings.desktop_lyrics.auto_size = on;
                self.settings_dirty = true;
            }
            Command::DesktopLyricsSetSize { width, height } => {
                let settings = &mut self.settings.desktop_lyrics;
                settings.width = width.clamp(160.0, 6000.0);
                settings.height = height.clamp(24.0, 2000.0);
                settings.auto_size = false;
                self.settings_dirty = true;
            }
            Command::DesktopLyricsSetTranslation(on) => {
                self.settings.desktop_lyrics.show_translation = on;
                self.settings_dirty = true;
            }
            Command::DesktopLyricsSetFooter(on) => {
                self.settings.desktop_lyrics.show_footer = on;
                self.settings_dirty = true;
            }
            Command::DesktopLyricsSetBold(on) => {
                self.settings.desktop_lyrics.bold = on;
                self.settings_dirty = true;
            }
            Command::DesktopLyricsSetBackground(mode) => {
                self.settings.desktop_lyrics.background = match mode {
                    0 => crate::config::DesktopBackgroundMode::Transparent,
                    2 => crate::config::DesktopBackgroundMode::Gradient,
                    3 => crate::config::DesktopBackgroundMode::Image,
                    4 => crate::config::DesktopBackgroundMode::BlurCover,
                    _ => crate::config::DesktopBackgroundMode::Solid,
                };
                self.settings_dirty = true;
            }
            Command::DesktopLyricsSetOpacity(opacity) => {
                self.settings.desktop_lyrics.opacity = opacity.clamp(0.0, 1.0);
                self.settings_dirty = true;
            }
            Command::DesktopLyricsSetRadius(radius) => {
                self.settings.desktop_lyrics.radius = radius.clamp(0, 200) as u8;
                self.settings_dirty = true;
            }
            Command::DesktopLyricsSetPadding(padding) => {
                self.settings.desktop_lyrics.padding = padding.clamp(0.0, 400.0);
                self.settings_dirty = true;
            }
            Command::DesktopLyricsSetLineSpacing(spacing) => {
                self.settings.desktop_lyrics.line_spacing = spacing.clamp(-96.0, 160.0);
                self.settings_dirty = true;
            }
            Command::DesktopLyricsSetShadow(on) => {
                self.settings.desktop_lyrics.shadow = on;
                self.settings_dirty = true;
            }
            Command::DesktopLyricsSetOutline(on) => {
                self.settings.desktop_lyrics.outline = on;
                self.settings_dirty = true;
            }
            Command::DesktopLyricsSetBorder(on) => {
                self.settings.desktop_lyrics.border = on;
                self.settings_dirty = true;
            }
            Command::DesktopLyricsSetWindowShadow(on) => {
                self.settings.desktop_lyrics.window_shadow = on;
                self.settings_dirty = true;
            }
            Command::DesktopLyricsSetHideWhenPaused(on) => {
                self.settings.desktop_lyrics.hide_when_paused = on;
                self.settings_dirty = true;
            }
            Command::DesktopLyricsSetCaptureProtection(on) => {
                self.settings.desktop_lyrics.capture_protection = on;
                self.settings_dirty = true;
            }
            Command::DesktopLyricsSetKaraoke(on) => {
                self.settings.desktop_lyrics.karaoke = on;
                self.settings_dirty = true;
            }
            Command::DesktopLyricsSetWordsOnly(on) => {
                self.settings.desktop_lyrics.karaoke_words_only = on;
                self.settings_dirty = true;
            }
            Command::DesktopLyricsSetFontSize(size) => {
                let settings = &mut self.settings.desktop_lyrics;
                let size = size.clamp(8.0, 200.0);
                settings.font_size = size;
                settings.next_font_size = (size * 0.76).clamp(6.0, 160.0);
                settings.prev_font_size = (size * 0.70).clamp(6.0, 160.0);
                self.settings_dirty = true;
            }
            Command::DesktopLyricsSetNextFontSize(size) => {
                self.settings.desktop_lyrics.next_font_size = size.clamp(6.0, 120.0);
                self.settings_dirty = true;
            }
            Command::DesktopLyricsSetPrevFontSize(size) => {
                self.settings.desktop_lyrics.prev_font_size = size.clamp(6.0, 120.0);
                self.settings_dirty = true;
            }
            Command::DesktopLyricsSetFooterFontSize(size) => {
                self.settings.desktop_lyrics.footer_font_size = size.clamp(6.0, 48.0);
                self.settings_dirty = true;
            }
            Command::DesktopLyricsSetFrameRate(fps) => {
                self.settings.desktop_lyrics.frame_rate = fps.clamp(15, 120);
                self.settings_dirty = true;
            }
            Command::DesktopLyricsGeometry {
                x,
                y,
                width,
                height,
                resized,
            } => {
                let settings = &mut self.settings.desktop_lyrics;
                settings.position = Some((x, y));
                if resized {
                    settings.width = width.clamp(160.0, 6000.0);
                    settings.height = height.clamp(24.0, 2000.0);
                    settings.auto_size = false;
                }
                self.session.desktop_lyrics_pos = Some((x, y));
                self.settings_dirty = true;
                self.session_dirty = true;
            }
            Command::MiniPlayerGeometry {
                x,
                y,
                width,
                height,
                resized,
            } => {
                let settings = &mut self.settings.mini;
                settings.position = Some((x, y));
                if resized {
                    settings.width = width.clamp(200.0, 1600.0);
                    settings.height = height.clamp(48.0, 2000.0);
                    settings.auto_height = false;
                }
                self.settings_dirty = true;
            }
            Command::DesktopLyricsResetPosition => {
                self.settings.desktop_lyrics.position = None;
                self.session.desktop_lyrics_pos = None;
                self.settings_dirty = true;
                self.session_dirty = true;
            }
            Command::MiniPlayerResetPosition => {
                self.settings.mini.position = None;
                self.settings_dirty = true;
            }
        }
    }

    /// Poll background work. Returns true when the visible state changed.
    pub fn tick(&mut self) -> bool {
        let mut changed = false;

        if self.demo.is_some() {
            changed |= self.tick_demo();
        }

        while let Some(result) = self.lyrics_service.poll() {
            if result.key == self.lyrics_key {
                self.lyrics = result.lyrics;
                self.lyrics_error = result.error;
                self.lyrics_loading = false;
                self.lyrics_rev = self.lyrics_rev.wrapping_add(1);
                changed = true;
            } else if let Some(lyrics) = result.lyrics {
                // Prefetched result for an upcoming track. Keep the cache
                // small: a shuffled 10k queue must not retain every LRC.
                let key = result.key;
                if self.prefetched_lyrics.insert(key.clone(), lyrics).is_none() {
                    self.prefetched_order.push_back(key);
                    while self.prefetched_order.len() > 8 {
                        if let Some(old) = self.prefetched_order.pop_front() {
                            self.prefetched_lyrics.remove(&old);
                        }
                    }
                }
            }
        }

        if let Some(result) = self.cover.service.poll() {
            if result.key == self.cover.key {
                self.cover.image = result.cover.map(Arc::new);
                self.cover.png = result.smtc_png.map(Arc::new);
                self.cover.rev = self.cover.rev.wrapping_add(1);
                changed = true;
            }
        }

        changed |= self.poll_online_events();
        changed |= self.poll_downloads();
        changed |= self.poll_loudness();
        changed |= self.poll_cache();
        changed |= self.poll_partial_cleanup();
        changed |= self.poll_lyrics_background();
        changed |= self.poll_background_image();

        while let Some(result) = self.row_covers.service.poll() {
            self.row_covers.inflight.remove(&result.key);
            let image = result.image.map(Arc::new);
            if self
                .row_covers
                .images
                .insert(result.key.clone(), image)
                .is_none()
            {
                self.row_covers.order.push_back(result.key);
                let limit = match self.settings.performance.thumbnail_cache {
                    0 => usize::MAX,
                    value => value as usize,
                };
                while self.row_covers.order.len() > limit {
                    if let Some(old) = self.row_covers.order.pop_front() {
                        self.row_covers.images.remove(&old);
                    }
                }
            }
            self.row_covers.rev = self.row_covers.rev.wrapping_add(1);
            changed = true;
        }

        for result in self.browse_service.poll() {
            // A slow read for a directory the user already left must not
            // overwrite the current listing.
            if self.browser.current.as_deref() == Some(result.path.as_path()) {
                self.browser.entries = result.entries;
                self.browser.error = result.error;
                self.browser.truncated = result.truncated;
                self.browser.loading = false;
                self.browse_rev = self.browse_rev.wrapping_add(1);
                changed = true;
            }
        }

        for event in self.engine.poll_events() {
            match event {
                EngineEvent::Loaded { token, duration, .. } => {
                    if token != self.engine_load_token {
                        continue;
                    }
                    if duration.is_some() {
                        self.current_duration = duration;
                    }
                    self.playback_state = self.engine.state();
                    changed = true;
                }
                EngineEvent::AutoAdvanced { token, duration, .. } => {
                    if let Some((index, armed_token)) = self.armed_next.take()
                        && token == armed_token
                    {
                        self.adopt_auto_advanced(index, duration);
                        changed = true;
                    }
                }
                EngineEvent::Ended => {
                    self.advance_auto();
                    changed = true;
                }
                EngineEvent::Error(message) => {
                    tracing::warn!("audio engine error: {message}");
                    self.playback_state = PlaybackState::Stopped;
                    self.notify(
                        crate::trf!("播放失败：{}", message),
                        true,
                    );
                    changed = true;
                }
            }
        }

        if self.current_track.is_some() && self.deferred_start.is_none() {
            let state = self.engine.state();
            if state != self.playback_state
                && !matches!(self.playback_state, PlaybackState::Loading)
            {
                self.playback_state = state;
                changed = true;
            }
        }

        if let (Some(a), Some(b)) = (self.ab_start, self.ab_end)
            && b > a + 0.25
            && self.is_playing()
            && self.position_secs() + 0.02 >= b
        {
            self.seek_to(a);
            changed = true;
        }

        if let Some(deadline) = self.sleep_deadline
            && Instant::now() >= deadline
        {
            self.sleep_deadline = None;
            self.sleep_minutes = None;
            if self.is_playing() {
                if self.demo.is_some() {
                    if let Some(demo) = self.demo.as_mut() {
                        demo.paused = true;
                    }
                } else {
                    self.engine.pause();
                }
                self.playback_state = PlaybackState::Paused;
                self.session_dirty = true;
            }
            changed = true;
        }

        changed |= self.poll_library();
        self.maybe_autosave();
        changed
    }

    pub fn snapshot(&self) -> Snapshot<'_> {
        Snapshot {
            page: self.session.page,
            queue: &self.queue.items,
            queue_order: &self.queue.shuffle_order,
            current_index: self.queue.current,
            current: self.current_track.as_ref(),
            state: self.playback_state,
            position_secs: self.position_secs(),
            duration_secs: self.duration_secs(),
            volume: self.settings.playback.volume,
            muted: self.settings.playback.muted,
            shuffle: self.queue.shuffle,
            repeat: self.queue.repeat,
            lyrics: self.lyrics.as_ref(),
            lyrics_loading: self.lyrics_loading,
            lyrics_error: self.lyrics_error.as_deref(),
            lyrics_background: self.lyrics_background.as_deref(),
            lyrics_background_rev: self.lyrics_background_rev,
            background_image: self.background_image.as_deref(),
            background_image_rev: self.background_image_rev,
            library: &self.library.rows,
            library_total: self.library.total,
            library_loading: self.library.loading,
            library_missing_checked: self.library.missing_checked,
            library_index_active: self.library.index_active,
            library_index_progress: self.library.index_progress,
            library_index_current: &self.library.index_current,
            library_folders: &self.settings.library.folders,
            library_extensions: &self.settings.library.extensions,
            library_sort: self.library.sort,
            library_scan_on_start: self.settings.library.scan_on_start,
            library_auto_watch: self.settings.library.auto_watch,
            library_follow_symlinks: self.settings.library.follow_symlinks,
            library_dedupe_mode: self.settings.library.dedupe_mode,
            lyrics_rev: self.lyrics_rev,
            library_rev: self.library_rev,
            current_cover: self.cover.image.as_deref(),
            current_cover_png: self.cover.png.as_deref().map(Vec::as_slice),
            cover_rev: self.cover.rev,
            row_cover_rev: self.row_covers.rev,
            sleep_remaining_secs: self.sleep_remaining_secs(),
            queue_rev: self.queue_rev,
            browser: &self.browser,
            browse_rev: self.browse_rev,
            statistics: self.library.stats.as_ref(),
            notice_rev: self.notice_rev,
            library_smart: &self.library.smart_rows,
            library_smart_kind: self.library.smart_kind,
            library_duplicates: &self.library.duplicate_groups,
            library_duplicate_rows: &self.library.duplicate_rows,
            library_duplicate_headers: &self.library.duplicate_headers,
            library_duplicate_mode: self.library.duplicate_mode,
            ab_start: self.ab_start,
            ab_end: self.ab_end,
            sleep_minutes: self.sleep_minutes,
            online: &self.online,
            discover: &self.discover,
            online_rev: self.online.rev,
            downloads: &self.download_rows,
            download_rev: self.download_rev,
            cache_usage: self.cache_usage,
            cache_rev: self.cache_rev,
        }
    }

    /// Queue a status message for the UI toast. Duplicate consecutive
    /// messages collapse so a repeated watcher error does not spam the stack.
    pub fn notify(&mut self, text: impl Into<String>, error: bool) {
        let text = text.into();
        if text.trim().is_empty() {
            return;
        }
        if self.notices.back().is_some_and(|last| last.text == text) {
            return;
        }
        self.notices.push_back(Notice { text, error });
        while self.notices.len() > NOTICE_LIMIT {
            self.notices.pop_front();
        }
        self.notice_rev = self.notice_rev.wrapping_add(1);
    }

    /// Take every queued notice. Called by the UI once per tick/repaint.
    pub fn drain_notices(&mut self) -> Vec<Notice> {
        self.notices.drain(..).collect()
    }

    pub fn statistics_loading(&self) -> bool {
        self.library.stats_loading
    }

    /// Query aggregate statistics. Results arrive on the DB worker and are
    /// reported through `DbEvent::StatsDone`.
    /// Smart playlist page: ask the worker for one preset list.
    pub fn request_smart(&mut self, kind: SmartKind) {
        if !self.library.enabled {
            self.library.smart_kind = kind;
            self.library.smart_rows = self.queue.items.iter().map(|track| LibraryEntry {
                key: track.dedupe_key(),
                path: track
                    .source
                    .path()
                    .map(Path::to_path_buf)
                    .unwrap_or_default(),
                title: track.display_title(),
                artist: track.display_artist(),
                album: track.album.clone(),
                duration: track.duration,
                play_count: 0,
                last_played_at: None,
                lossless: track.lossless,
                format: track
                    .source
                    .path()
                    .and_then(|path| path.extension())
                    .map(|ext| ext.to_string_lossy().to_ascii_uppercase())
                    .unwrap_or_default(),
                missing: false,
                size: 0,
            }).collect();
            self.library_rev = self.library_rev.wrapping_add(1);
            return;
        }
        self.library.smart_kind = kind;
        self.library.smart_rows.clear();
        self.library.loading = true;
        self.library.db.smart(kind, 500);
    }

    fn play_smart_entry(&mut self, index: usize) {
        if let Some(entry) = self.library.smart_rows.get(index).cloned() {
            self.play_library_entry_value(entry);
        }
    }

    fn queue_smart_entry(&mut self, index: usize) {
        if let Some(entry) = self.library.smart_rows.get(index).cloned() {
            self.queue_library_entry_value(entry, false);
        }
    }

    fn remove_smart_entry(&mut self, index: usize) {
        if let Some(entry) = self.library.smart_rows.get(index) {
            self.library.db.remove_keys(vec![entry.key.clone()]);
            self.library.smart_rows.remove(index);
            self.library_rev = self.library_rev.wrapping_add(1);
        }
    }

    /// Duplicate page: query one dedupe mode.
    pub fn request_duplicates(&mut self, mode: crate::config::DedupeMode) {
        if !self.library.enabled {
            self.notify(crate::i18n::tr("曲库未启用，无法查重"), true);
            return;
        }
        self.library.duplicate_mode = mode;
        self.library.duplicate_groups.clear();
        self.library.duplicate_rows.clear();
        self.library.duplicate_headers.clear();
        self.library.loading = true;
        self.library.db.duplicates(mode);
    }

    fn play_duplicate_entry(&mut self, index: usize) {
        if self
            .library
            .duplicate_headers
            .get(index)
            .copied()
            .unwrap_or(true)
        {
            return;
        }
        if let Some(entry) = self.library.duplicate_rows.get(index).cloned() {
            self.play_library_entry_value(entry);
        }
    }

    fn remove_duplicate_entry(&mut self, index: usize) {
        if self
            .library
            .duplicate_headers
            .get(index)
            .copied()
            .unwrap_or(true)
        {
            return;
        }
        if let Some(entry) = self.library.duplicate_rows.get(index).cloned() {
            self.library.db.remove_keys(vec![entry.key]);
            self.library.duplicate_rows.remove(index);
            self.library.duplicate_headers.remove(index);
            self.library_rev = self.library_rev.wrapping_add(1);
        }
    }

    /// Group ordinal (into `duplicate_groups`) for a flattened display row.
    fn duplicate_group_at_row(&self, index: usize) -> Option<usize> {
        let headers = self.library.duplicate_headers.get(..=index)?;
        headers
            .iter()
            .filter(|header| **header)
            .count()
            .checked_sub(1)
    }

    fn keep_duplicate_group(&mut self, index: usize) {
        let Some(group_index) = self.duplicate_group_at_row(index) else {
            return;
        };
        let Some(group) = self.library.duplicate_groups.get(group_index) else {
            return;
        };
        let remove: Vec<String> = group.entries.iter().skip(1).map(|entry| entry.key.clone()).collect();
        if remove.is_empty() {
            return;
        }
        let removed = remove.len();
        self.library.db.remove_keys(remove);
        self.library.duplicate_groups.remove(group_index);
        self.rebuild_duplicate_rows();
        self.library_rev = self.library_rev.wrapping_add(1);
        self.notify(
            crate::trf!("已从索引移除 {} 个重复文件", removed),
            false,
        );
    }

    fn remove_duplicate_group(&mut self, index: usize) {
        let Some(group_index) = self.duplicate_group_at_row(index) else {
            return;
        };
        let Some(group) = self.library.duplicate_groups.get(group_index) else {
            return;
        };
        let keys: Vec<String> = group.entries.iter().map(|entry| entry.key.clone()).collect();
        if keys.is_empty() {
            return;
        }
        let removed = keys.len();
        self.library.db.remove_keys(keys);
        self.library.duplicate_groups.remove(group_index);
        self.rebuild_duplicate_rows();
        self.library_rev = self.library_rev.wrapping_add(1);
        self.notify(
            crate::trf!("已从索引移除整组 {} 个文件", removed),
            false,
        );
    }

    fn rebuild_duplicate_rows(&mut self) {
        self.library.duplicate_rows.clear();
        self.library.duplicate_headers.clear();
        for (group_index, group) in self.library.duplicate_groups.iter().enumerate() {
            let title = if group.title.trim().is_empty() {
                crate::i18n::tr("未命名曲目").to_string()
            } else {
                group.title.clone()
            };
            self.library.duplicate_rows.push(LibraryEntry {
                key: format!("__group_{group_index}"),
                path: PathBuf::new(),
                title: crate::trf!("副本组 {} · {}", group_index + 1, title),
                artist: if group.artist.trim().is_empty() {
                    crate::trf!("{} 个文件", group.entries.len())
                } else {
                    crate::trf!("{} · {} 个文件", group.artist, group.entries.len())
                },
                album: String::new(),
                duration: None,
                play_count: 0,
                last_played_at: None,
                lossless: false,
                format: String::new(),
                missing: false,
                size: 0,
            });
            self.library.duplicate_headers.push(true);
            for entry in &group.entries {
                self.library.duplicate_rows.push(entry.clone());
                self.library.duplicate_headers.push(false);
            }
        }
    }

    fn play_library_entry_value(&mut self, entry: LibraryEntry) {
        let track = entry.to_track();
        let key = track.dedupe_key();
        if let Some(position) = self.queue.position_of_key(&key) {
            self.play_index(position);
        } else if self.queue.add(track) {
            let position = self.queue.len() - 1;
            self.play_index(position);
        }
    }

    fn queue_library_entry_value(&mut self, entry: LibraryEntry, next: bool) {
        let track = entry.to_track();
        let added = if next {
            self.queue.insert_next(track).is_some()
        } else {
            self.queue.add(track)
        };
        if added {
            self.queue_changed();
            self.session_dirty = true;
        }
    }

    pub fn request_statistics(&mut self) {
        if !self.library.enabled {
            // Bench/self-test: show a zeroed page instead of "统计中…" forever.
            self.library.stats = Some(crate::library::LibraryStats::default());
            self.library.stats_loading = false;
            self.library_rev = self.library_rev.wrapping_add(1);
            return;
        }
        self.library.stats_loading = true;
        self.library.db.stats();
    }

    /// Copy the latest mono samples for the UI visualizer.
    pub fn fill_visual_samples(&self, out: &mut Vec<f32>, max: usize) {
        self.engine.fill_visual_samples(out, max);
    }

    pub fn output_devices(&self) -> Vec<String> {
        self.engine.output_devices()
    }

    /// Current cover image and its revision (0 until the first decode).
    pub fn current_cover(&self) -> Option<(&RgbaImage, u64)> {
        let image = self.cover.image.as_deref()?;
        Some((image, self.cover.rev))
    }

    /// Cheap shared handle used by the standalone lyrics window, which keeps
    /// its own copy between paints.
    pub fn current_cover_owned(&self) -> Option<(Arc<RgbaImage>, u64)> {
        self.cover.image.clone().map(|image| (image, self.cover.rev))
    }

    /// Recently played local files (newest first, max 8), mirrored to the
    /// Windows jump list by the shell.
    pub fn recent_files(&self) -> &[RecentFile] {
        &self.session.recent_files
    }

    /// Current library search text, used to restore the native EDIT when the
    /// user switches between the queue filter and the library search.
    pub fn library_search_text(&self) -> &str {
        &self.library.search
    }

    fn remember_recent(&mut self, path: &Path, title: &str) {
        let key = crate::model::normalize_path(path);
        self.session
            .recent_files
            .retain(|entry| crate::model::normalize_path(&entry.path) != key);
        self.session.recent_files.insert(
            0,
            RecentFile {
                path: path.to_path_buf(),
                title: title.to_string(),
            },
        );
        self.session.recent_files.truncate(8);
        self.session_dirty = true;
    }

    /// Decoded thumbnail for one queue/library row.
    pub fn row_cover(&self, path: &Path) -> Option<&RgbaImage> {        let key = crate::model::normalize_path(path);
        self.row_covers.images.get(&key)?.as_deref()
    }

    /// Queue a thumbnail for a row if it has not been requested yet. The UI
    /// calls this for its visible window; IO/decoding stays off this thread.
    pub fn ensure_row_cover(&mut self, path: &Path) {
        if !self.settings.performance.load_covers {
            return;
        }
        let key = crate::model::normalize_path(path);
        if self.row_covers.images.contains_key(&key) || self.row_covers.inflight.contains(&key) {
            return;
        }
        if self
            .row_covers
            .service
            .request(key.clone(), path.to_path_buf())
        {
            self.row_covers.inflight.insert(key);
        }
    }

    pub fn row_cover_by_key(&self, key: &str) -> Option<&RgbaImage> {
        self.row_covers.images.get(key)?.as_deref()
    }

    /// Queue an online thumbnail, keyed by `url:<address>`.
    pub fn ensure_row_cover_url(&mut self, url: &str) {
        if !self.settings.performance.load_covers
            || url.is_empty()
            || !(url.starts_with("http://") || url.starts_with("https://"))
        {
            return;
        }
        let key = format!("url:{url}");
        if self.row_covers.images.contains_key(&key) || self.row_covers.inflight.contains(&key) {
            return;
        }
        let http = crate::api::HttpOptions::for_network(&self.settings.network);
        if self
            .row_covers
            .service
            .request_url(key.clone(), url.to_string(), http)
        {
            self.row_covers.inflight.insert(key);
        }
    }

    /// Seconds left on the sleep timer (rounded up), or `None` when off.
    pub fn sleep_remaining_secs(&self) -> Option<u64> {
        let deadline = self.sleep_deadline?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        Some(remaining.as_secs().max(1))
    }

    /// Selected preset duration, for highlighting the settings control.
    pub fn sleep_selected_minutes(&self) -> Option<u32> {
        self.sleep_minutes
    }

    pub fn is_playing(&self) -> bool {
        if let Some(demo) = self.demo.as_ref() {
            return !demo.paused;
        }
        matches!(
            self.playback_state,
            PlaybackState::Playing | PlaybackState::Loading
        )
    }

    pub fn position_secs(&self) -> f64 {
        if let Some(demo) = self.demo.as_ref() {
            return demo.position;
        }
        if let Some(start) = self.deferred_start {
            return start;
        }
        if self.current_track.is_some() {
            self.engine.position_secs()
        } else {
            0.0
        }
    }

    pub fn duration_secs(&self) -> f64 {
        self.current_duration
            .or_else(|| self.engine.duration_secs())
            .unwrap_or(0.0)
    }

    // ------------------------------------------------------------------
    // Playback
    // ------------------------------------------------------------------

    pub fn play_index(&mut self, index: usize) {
        self.load_index(index, 0.0, true);
    }

    pub fn toggle_play(&mut self) {
        if let Some(demo) = self.demo.as_mut() {
            demo.paused = !demo.paused;
            demo.last = Instant::now();
            if !demo.paused {
                self.playback_state = PlaybackState::Playing;
            } else {
                self.playback_state = PlaybackState::Paused;
            }
            return;
        }
        match self.playback_state {
            PlaybackState::Playing => {
                self.engine.pause();
                self.playback_state = PlaybackState::Paused;
                self.session_dirty = true;
            }
            PlaybackState::Paused | PlaybackState::Stopped => {
                if let Some(start) = self.deferred_start.take() {
                    if let Some(index) = self.queue.current {
                        self.load_index(index, start, true);
                    }
                } else if self.current_track.is_some() {
                    self.engine.play();
                    self.playback_state = PlaybackState::Playing;
                    self.arm_next();
                } else if let Some(index) = self
                    .queue
                    .current
                    .or_else(|| (!self.queue.is_empty()).then_some(0))
                {
                    self.load_index(index, 0.0, true);
                }
            }
            PlaybackState::Loading => match self.engine.state() {
                PlaybackState::Playing => {
                    self.engine.pause();
                    self.playback_state = PlaybackState::Paused;
                }
                PlaybackState::Paused => {
                    self.engine.play();
                    self.playback_state = PlaybackState::Playing;
                }
                _ => {}
            },
        }
    }

    pub fn next_track(&mut self) {
        if self.demo.is_some() {
            let _ = self.advance_track(false);
            return;
        }
        // A manual skip within the first 15 seconds counts against the track
        // (only when statistics are enabled and the song is long enough).
        if self.settings.features.statistics
            && self.position_secs() < 15.0
            && self.duration_secs() > 30.0
            && let Some(track) = self.current_track.as_ref()
            && track.source.is_local()
        {
            self.library.db.record_skip(track.dedupe_key());
        }
        self.advance_track(false);
    }

    pub fn prev_track(&mut self) {
        if self.demo.is_some() {
            if let Some(index) = self.queue.back() {
                self.select_demo_index(index);
            }
            return;
        }
        if let Some(index) = self.queue.back() {
            self.load_index(index, 0.0, true);
        }
    }

    pub fn seek_to(&mut self, secs: f64) {
        let dur = self.duration_secs();
        let target = if dur > 0.0 {
            secs.clamp(0.0, dur)
        } else {
            secs.max(0.0)
        };
        if let Some(demo) = self.demo.as_mut() {
            demo.position = target;
            return;
        }
        if self.deferred_start.is_some() {
            self.deferred_start = Some(target);
            self.session_dirty = true;
            return;
        }
        self.engine.seek(target);
        self.session_dirty = true;
    }

    pub fn set_volume(&mut self, volume: f32) {
        let volume = volume.clamp(0.0, 1.0);
        self.settings.playback.volume = volume;
        self.engine.set_volume(volume);
        if volume > 0.0 && self.settings.playback.muted {
            self.settings.playback.muted = false;
            self.engine.set_muted(false);
        }
        self.settings_dirty = true;
        self.session_dirty = true;
    }

    pub fn toggle_mute(&mut self) {
        self.settings.playback.muted = !self.settings.playback.muted;
        self.engine.set_muted(self.settings.playback.muted);
        self.settings_dirty = true;
    }

    pub fn toggle_shuffle(&mut self) {
        self.queue.set_shuffle(!self.queue.shuffle);
        self.session_dirty = true;
        // The queue page displays the playback order, so switching shuffle
        // on/off changes every visible row.
        self.queue_changed();
        self.arm_next();
    }

    pub fn cycle_repeat(&mut self) {
        self.queue.repeat = self.queue.repeat.cycle();
        self.session_dirty = true;
        self.arm_next();
    }

    pub fn add_paths(&mut self, paths: Vec<PathBuf>, play_first: bool) {
        let (mut files, playlist) = self.collect_paths(paths);
        // A dropped/opened playlist replaces the queue (reference behaviour):
        // its #SEARCH# directories are re-expanded and the previous track is
        // matched by key. Only one playlist per drop is imported.
        if let Some(playlist) = playlist {
            let was_playing = self.is_playing();
            self.import_playlist(&playlist, was_playing);
            return;
        }
        files.sort();
        let was_empty = self.queue.is_empty();
        let added = self.queue.add_many(files.into_iter().map(Track::local));
        if added > 0 {
            self.queue_changed();
            self.session_dirty = true;
        }
        if play_first && was_empty && !self.queue.is_empty() {
            self.play_index(0);
        }
    }

    /// Insert dropped files before `index` (real queue index). Playlists keep
    /// the replace-the-queue behaviour; a filtered queue is remapped by the UI
    /// before this command is sent.
    pub fn add_paths_at(&mut self, paths: Vec<PathBuf>, index: usize) {
        let (mut files, playlist) = self.collect_paths(paths);
        if let Some(playlist) = playlist {
            let was_playing = self.is_playing();
            self.import_playlist(&playlist, was_playing);
            return;
        }
        files.sort();
        let added = self
            .queue
            .insert_many(index, files.into_iter().map(Track::local));
        if added > 0 {
            self.queue_changed();
            self.session_dirty = true;
            self.notify(
                crate::trf!("已插入 {} 首歌曲", added),
                false,
            );
        }
    }

    /// Expand dropped paths into audio files plus at most one playlist.
    fn collect_paths(&self, paths: Vec<PathBuf>) -> (Vec<PathBuf>, Option<PathBuf>) {
        let extensions = self.settings.library.extensions.clone();
        let mut files = Vec::new();
        let mut playlist = None;
        for path in paths {
            if path.is_dir() {
                for entry in walkdir::WalkDir::new(&path)
                    .max_depth(12)
                    .follow_links(self.settings.library.follow_symlinks)
                    .into_iter()
                    .flatten()
                {
                    let candidate = entry.path();
                    if candidate.is_file() && crate::util::is_audio_file(candidate, &extensions) {
                        files.push(candidate.to_path_buf());
                    }
                }
            } else if path.is_file() {
                let extension = path
                    .extension()
                    .map(|value| value.to_string_lossy().to_ascii_lowercase())
                    .unwrap_or_default();
                if matches!(extension.as_str(), "mp3list" | "m3u" | "m3u8") {
                    if playlist.is_none() {
                        playlist = Some(path);
                    }
                } else {
                    files.push(path);
                }
            }
        }
        (files, playlist)
    }

    /// Point the folder browser at a directory, or at the drive list for
    /// `None`. Reading happens on the browse thread; `browse_rev` tells the UI
    /// to repaint when the result arrives.
    ///
    /// Navigating records the previous location so `BrowseBack` can return to
    /// it; re-opening the same path (the refresh button) is not a new history
    /// entry and keeps the forward stack intact.
    fn browse_to(&mut self, path: Option<PathBuf>) {
        let current = self.browser.current.clone();
        if current != path {
            self.browser.back.push(current);
            self.browser.forward.clear();
        }
        self.load_browse_location(path);
    }

    /// Pop one location off the back/forward stack and load it.
    fn browse_history(&mut self, forward: bool) {
        let target = if forward {
            self.browser.forward.pop()
        } else {
            self.browser.back.pop()
        };
        let Some(target) = target else {
            return;
        };
        let current = self.browser.current.clone();
        if forward {
            self.browser.back.push(current);
        } else {
            self.browser.forward.push(current);
        }
        self.load_browse_location(target);
    }

    fn load_browse_location(&mut self, path: Option<PathBuf>) {
        match path {
            Some(path) => {
                self.browser.current = Some(path.clone());
                self.browser.loading = true;
                self.browser.error = None;
                self.browser.entries.clear();
                self.browser.truncated = false;
                let extensions = self.settings.library.extensions.clone();
                self.browse_service.request(path, extensions);
            }
            None => {
                self.browser.current = None;
                self.browser.loading = false;
                self.browser.error = None;
                self.browser.truncated = false;
                self.browser.entries = drive_entries();
            }
        }
        self.browse_rev = self.browse_rev.wrapping_add(1);
    }

    fn browse_play(&mut self, index: usize) {
        let Some(entry) = self.browser.entries.get(index).cloned() else {
            return;
        };
        if entry.is_dir {
            self.browse_to(Some(entry.path));
            return;
        }
        if entry.is_playlist {
            let was_playing = self.is_playing();
            self.import_playlist(&entry.path, was_playing);
            return;
        }
        let key = Track::local(entry.path.clone()).dedupe_key();
        self.add_paths(vec![entry.path], false);
        if let Some(index) = self
            .queue
            .items
            .iter()
            .position(|track| track.dedupe_key() == key)
        {
            self.play_index(index);
        }
    }

    fn browse_enqueue(&mut self, index: usize) {
        let Some(entry) = self.browser.entries.get(index).cloned() else {
            return;
        };
        if entry.is_dir {
            return;
        }
        if entry.is_playlist {
            // Importing a playlist replaces the queue by design; a context
            // action that says "add" must not do that.
            return;
        }
        self.add_paths(vec![entry.path], false);
    }

    fn browse_add_all(&mut self) {
        let paths: Vec<PathBuf> = self
            .browser
            .entries
            .iter()
            .filter(|entry| !entry.is_dir && !entry.is_playlist)
            .map(|entry| entry.path.clone())
            .collect();
        if !paths.is_empty() {
            self.add_paths(paths, false);
        }
    }

    fn browse_play_all(&mut self) {
        let paths: Vec<PathBuf> = self
            .browser
            .entries
            .iter()
            .filter(|entry| !entry.is_dir && !entry.is_playlist)
            .map(|entry| entry.path.clone())
            .collect();
        if paths.is_empty() {
            return;
        }
        self.handle(Command::ClearQueue);
        self.add_paths(paths, false);
        self.play_index(0);
    }

    /// Snapshot of the current shuffle cycle: keys in playback order, the
    /// cursor position and the track key at that cursor.
    fn shuffle_snapshot(&self) -> Option<(Vec<String>, usize, Option<String>)> {
        if !self.queue.shuffle {
            return None;
        }
        let keys: Vec<String> = self
            .queue
            .shuffle_order
            .iter()
            .filter_map(|&index| self.queue.items.get(index))
            .map(Track::dedupe_key)
            .collect();
        if keys.is_empty() {
            return None;
        }
        let cursor = self
            .queue
            .current
            .and_then(|current| self.queue.shuffle_order.iter().position(|index| *index == current))
            .unwrap_or(0)
            .min(keys.len() - 1);
        let cursor_key = self.current_track.as_ref().map(Track::dedupe_key);
        Some((keys, cursor, cursor_key))
    }

    /// Remember the shuffled order/position for the playlist that is about to
    /// be replaced, so switching away and back keeps its progress.
    fn cache_current_playlist_shuffle(&mut self) {
        let Some(path) = self.session.playlist_path.clone() else {
            return;
        };
        let Some((keys, cursor, cursor_key)) = self.shuffle_snapshot() else {
            return;
        };
        upsert_playlist_shuffle(&mut self.session.playlist_shuffle, &path, keys, cursor, cursor_key);
        self.session_dirty = true;
    }

    fn cached_playlist_shuffle(&self, path: &Path) -> Option<PlaylistShuffle> {
        let key = crate::model::normalize_path(path);
        self.session
            .playlist_shuffle
            .iter()
            .find(|entry| crate::model::normalize_path(&entry.path) == key)
            .cloned()
    }

    /// Import a `.mp3list`/`.m3u` playlist, replacing the current queue.
    ///
    /// Mirrors the reference implementation: `#SEARCH#` directories are
    /// expanded against the files currently on disk, the playlist path is
    /// remembered, and the previous track is looked up by key so added or
    /// removed files do not shift the resume point. The track always starts
    /// at 0:00; `autoplay` only controls whether it starts playing at once.
    ///
    /// Returns the number of tracks added. 0 means nothing changed: an empty
    /// or unreadable playlist never wipes the live queue.
    /// Parse an `.mp3list` into playable tracks, expanding `#SEARCH#`
    /// directory blocks. Shared by explicit imports and the startup refresh.
    fn playlist_tracks(&self, path: &Path) -> Option<Vec<Track>> {
        let parsed = match crate::playlist::mp3list::parse(path) {
            Ok(parsed) => parsed,
            Err(err) => {
                tracing::warn!("playlist {}: {err}", path.display());
                return None;
            }
        };
        let mut tracks: Vec<Track> = parsed.files.into_iter().map(Track::local).collect();
        for url in parsed.urls {
            tracks.push(Track {
                source: TrackSource::Url { url: url.clone() },
                title: url,
                source_name: "在线".to_string(),
                ..Default::default()
            });
        }
        tracks.extend(parsed.online);
        for dir in &parsed.dirs {
            let extensions = if dir.extensions.is_empty() {
                self.settings.library.extensions.clone()
            } else {
                dir.extensions.clone()
            };
            let mut dir_files: Vec<PathBuf> = walkdir::WalkDir::new(&dir.path)
                .max_depth(12)
                .follow_links(self.settings.library.follow_symlinks)
                .into_iter()
                .flatten()
                .filter(|entry| entry.path().is_file())
                .filter(|entry| crate::util::is_audio_file(entry.path(), &extensions))
                .map(|entry| entry.path().to_path_buf())
                .collect();
            dir_files.sort();
            tracks.extend(dir_files.into_iter().map(Track::local));
        }
        if tracks.is_empty() {
            tracing::warn!(
                "playlist {}: no playable tracks ({} missing)",
                path.display(),
                parsed.missing
            );
            return None;
        }
        Some(tracks)
    }

    /// Startup auto-refresh for the remembered playlist: keep the restored
    /// queue (and its shuffle cycle / current position) and only append tracks
    /// that were newly scanned into the file. Returns how many were added.
    fn refresh_startup_playlist(&mut self, path: &Path) -> usize {
        let Some(tracks) = self.playlist_tracks(path) else {
            return 0;
        };
        let mut added = 0;
        if self.queue.shuffle {
            // Newly scanned files join after the restored cycle instead of
            // being shuffled into already-played entries.
            self.queue.shuffle_append = true;
        }
        for track in tracks {
            if self.queue.add(track) {
                added += 1;
            }
        }
        if added > 0 {
            self.queue_changed();
            self.session_dirty = true;
            self.cache_current_playlist_shuffle();
        }
        added
    }

    /// Time offset to restore at startup, but only for the track the saved
    /// position belongs to.
    pub fn import_playlist(&mut self, path: &Path, autoplay: bool) -> usize {
        let Some(tracks) = self.playlist_tracks(path) else {
            return 0;
        };
        let resume_key = self
            .current_track
            .as_ref()
            .map(Track::dedupe_key)
            .or_else(|| self.session.current_key.clone());
        // Persist the outgoing playlist's cycle first, then look up the
        // incoming playlist's cached cycle and cursor.
        self.cache_current_playlist_shuffle();
        let same_playlist = self
            .session
            .playlist_path
            .as_ref()
            .is_some_and(|current| {
                crate::model::normalize_path(current) == crate::model::normalize_path(path)
            });
        let cached = self.cached_playlist_shuffle(path);
        let cached_keys = cached
            .as_ref()
            .map(|entry| entry.keys.clone())
            .unwrap_or_default();
        let cached_cursor = cached.as_ref().map(|entry| entry.cursor).unwrap_or(0);
        let cached_cursor_key = cached
            .as_ref()
            .and_then(|entry| entry.cursor_key.clone());
        let old_shuffle_keys: Vec<String> = if !self.queue.shuffle {
            Vec::new()
        } else if !cached_keys.is_empty() {
            cached_keys
        } else if same_playlist {
            self.queue
                .display_order()
                .iter()
                .filter_map(|&index| self.queue.items.get(index))
                .map(Track::dedupe_key)
                .collect()
        } else {
            Vec::new()
        };

        // Build the new list before touching the live queue.
        self.disarm_next();
        self.engine.stop();
        self.deferred_start = None;
        self.queue.clear();
        self.current_track = None;
        self.current_duration = None;
        self.playback_state = PlaybackState::Stopped;
        self.lyrics = None;
        self.lyrics_key.clear();
        self.lyrics_loading = false;
        self.lyrics_error = None;
        self.lyrics_rev = self.lyrics_rev.wrapping_add(1);

        let added = self.queue.add_many(tracks);
        if self.queue.shuffle {
            if old_shuffle_keys.is_empty() {
                // First open of this playlist (or every track changed): start
                // a fresh random cycle instead of insertion order.
                self.queue.build_shuffle_order(false);
            } else {
                let matched = self.queue.restore_shuffle_order(&old_shuffle_keys);
                if matched == 0 {
                    self.queue.build_shuffle_order(false);
                }
            }
        }
        self.session
            .remember_playlist(path.to_path_buf(), crate::config::unix_now());
        self.session.playlist_path = Some(path.to_path_buf());

        // Resume by the saved track key; if that file disappeared, use this
        // playlist's shuffle cursor so progress is not lost.
        let index = resume_key
            .as_deref()
            .and_then(|key| self.queue.position_of_key(key))
            .or_else(|| {
                cached_cursor_key
                    .as_deref()
                    .and_then(|key| self.queue.position_of_key(key))
            })
            .or_else(|| {
                if self.queue.shuffle {
                    self.queue
                        .shuffle_order
                        .get(cached_cursor)
                        .copied()
                        .or_else(|| self.queue.shuffle_order.first().copied())
                } else {
                    None
                }
            })
            .or(if added > 0 { Some(0) } else { None });
        // The playlist remembers which item was playing, not the offset
        // inside it, so the restored track always starts at 0:00.
        if let Some(index) = index {
            if autoplay {
                self.load_index(index, 0.0, true);
            } else {
                self.restore_paused(index, 0.0);
            }
        }
        // Store the freshly expanded order so the next open appends new files
        // after it and keeps the same cursor.
        self.cache_current_playlist_shuffle();
        self.queue_changed();
        self.session_dirty = true;
        added
    }

    // ------------------------------------------------------------------
    // Internals
    // ------------------------------------------------------------------

    fn load_index(&mut self, index: usize, start_secs: f64, autoplay: bool) {
        if index >= self.queue.items.len() {
            return;
        }
        let start_secs = clamp_start_secs(
            self.queue.items.get(index).and_then(|track| track.duration),
            start_secs,
        );
        if self.demo.is_some() {
            self.select_demo_index(index);
            if let Some(demo) = self.demo.as_mut() {
                demo.position = start_secs.max(0.0);
            }
            self.playback_state = if autoplay {
                PlaybackState::Playing
            } else {
                PlaybackState::Paused
            };
            return;
        }
        self.disarm_next();
        self.deferred_start = None;
        self.ab_start = None;
        self.ab_end = None;
        self.engine.stop();
        let mut track = self.queue.items[index].clone();
        track.apply_filename_fallback();
        self.queue.set_current(index);
        self.current_track = Some(track.clone());
        self.current_duration = track.duration;
        self.playback_state = if autoplay {
            PlaybackState::Loading
        } else {
            PlaybackState::Paused
        };
        self.session_dirty = true;
        self.refresh_lyrics(&track);
        self.refresh_cover(&track);
        self.maybe_scan_loudness(&track);
        if autoplay
            && self.settings.features.statistics
            && let Some(db_track) = DbTrack::from_track(&track)
        {
            self.library.db.record_play(db_track);
        }

        match &track.source {
            TrackSource::Local { path } => {
                if !path.is_file() {
                    tracing::warn!("missing local track: {}", path.display());
                    self.playback_state = PlaybackState::Stopped;
                    return;
                }
                self.remember_recent(path, &track.display_title());
                self.engine_load_token = self.engine.load(
                    EngineSource::Local(path.clone()),
                    start_secs,
                    autoplay,
                );
            }
            TrackSource::Url { url } | TrackSource::Radio { url } => {
                if self.settings.network.offline {
                    tracing::warn!("offline: cannot stream {url}");
                    self.playback_state = PlaybackState::Stopped;
                    return;
                }
                let http = crate::api::HttpOptions::for_network(&self.settings.network).for_stream();
                self.engine_load_token = self.engine.load(
                    EngineSource::Url {
                        url: url.clone(),
                        cache: None,
                        http,
                        download_first: false,
                    },
                    start_secs,
                    autoplay,
                );
            }
            TrackSource::Bamboo { .. } => {
                if self.start_online_resolve(&track, start_secs, autoplay) {
                    // The resolve event loads the stream and arms the next
                    // deck; arming now would point at an unresolved URL.
                    return;
                }
                self.playback_state = PlaybackState::Stopped;
                return;
            }
        }
        self.arm_next();
    }

    fn restore_paused(&mut self, index: usize, start_secs: f64) {
        if index >= self.queue.items.len() {
            return;
        }
        if !self.settings.performance.defer_paused_restore {
            // Open the decoder and stay paused; the setting trades a slower
            // startup for an instant first Play.
            self.load_index(index, start_secs, false);
            return;
        }
        let mut track = self.queue.items[index].clone();
        track.apply_filename_fallback();
        self.queue.set_current(index);
        self.current_track = Some(track.clone());
        self.current_duration = track.duration;
        self.playback_state = PlaybackState::Paused;
        self.refresh_lyrics(&track);
        self.refresh_cover(&track);
        self.deferred_start = Some(clamp_start_secs(track.duration, start_secs));
    }

    fn advance_track(&mut self, auto: bool) -> bool {
        let total = self.queue.len().max(1);
        let mut skipped = 0usize;
        loop {
            let Some(index) = self.queue.advance(auto) else {
                return false;
            };
            let missing = self.queue.items.get(index).and_then(|track| {
                if track.is_playable_local() {
                    None
                } else {
                    track.source.path().map(Path::to_path_buf)
                }
            });
            if self.demo.is_some() {
                self.select_demo_index(index);
                return true;
            }
            if let Some(path) = missing {
                skipped += 1;
                tracing::info!("skipping missing track: {}", path.display());
                if skipped >= total {
                    return false;
                }
                continue;
            }
            self.load_index(index, 0.0, true);
            return true;
        }
    }

    fn advance_auto(&mut self) {
        if !self.settings.playback.auto_advance {
            self.playback_state = PlaybackState::Stopped;
            return;
        }
        if !self.advance_track(true) {
            self.playback_state = PlaybackState::Stopped;
        }
    }

    /// Adopt the gapless deck: the pre-armed track is now audible.
    fn adopt_auto_advanced(&mut self, index: usize, duration: Option<f64>) {
        if index >= self.queue.items.len() {
            return;
        }
        let mut track = self.queue.items[index].clone();
        track.apply_filename_fallback();
        self.queue.set_current(index);
        self.current_track = Some(track.clone());
        self.current_duration = duration.or(track.duration);
        self.playback_state = PlaybackState::Playing;
        self.session_dirty = true;
        if let Some(path) = track.source.path().filter(|path| path.is_file()) {
            self.remember_recent(path, &track.display_title());
        }
        self.refresh_lyrics(&track);
        self.refresh_cover(&track);
        self.maybe_scan_loudness(&track);
        self.arm_next();
    }

    /// Pre-open the next track on the engine's second deck (gapless).
    fn arm_next(&mut self) {
        self.disarm_next();
        if !(self.settings.features.gapless && self.settings.playback.gapless) {
            return;
        }
        let Some(index) = self.queue.peek_next(true) else {
            return;
        };
        let Some(track) = self.queue.items.get(index) else {
            return;
        };
        if let TrackSource::Bamboo { source_id, .. } = &track.source {
            let key = track.dedupe_key();
            let Some(source) = self.online_source(source_id) else {
                return;
            };
            let ready = match self.online_next.as_ref() {
                Some((cached, _)) if cached == &key => {
                    self.online_next.take().map(|(_, prepared)| prepared)
                }
                _ => None,
            };
            if let Some(prepared) = ready {
                let token = self.engine_load_token.wrapping_add(1);
                let http =
                    crate::api::HttpOptions::for_network(&self.settings.network).for_stream();
                self.engine.set_next(
                    Some(EngineSource::Url {
                        url: prepared.url,
                        cache: self.online_cache(&key, &source),
                        http,
                        download_first: source.kind == SourceKind::Local,
                    }),
                    token,
                );
                self.armed_next = Some((index, token));
                return;
            }
            // Resolve the next URL ahead of time; `NextResolved` re-enters
            // this function once the answer is available.
            if self.settings.network.offline
                || self.online_next_request.as_deref() == Some(key.as_str())
            {
                return;
            }
            let cache = self.online_cache(&key, &source);
            if self
                .api
                .resolve_next(
                    &source,
                    &self.settings.network,
                    track,
                    cache,
                )
                .is_some()
            {
                self.online_next_request = Some(key);
            }
            return;
        }
        let Some(source) = engine_source_for(track, &self.settings) else {
            return;
        };
        let token = self.engine_load_token.wrapping_add(1);
        self.engine.set_next(Some(source), token);
        self.armed_next = Some((index, token));
    }

    fn disarm_next(&mut self) {
        if self.armed_next.take().is_some() {
            self.engine.set_next(None, 0);
        }
    }

    /// Request artwork for the newly selected track. The previous image stays
    /// visible until the worker answers, so switching songs does not flash a
    /// placeholder; the revision bump is only for the eventual result.
    fn refresh_cover(&mut self, track: &Track) {
        if self.demo.is_some() {
            return;
        }
        let key = track.dedupe_key();
        if self.cover.key == key {
            return;
        }
        self.cover.service.cancel();
        self.cover.key = key.clone();
        self.cover.image = None;
        self.cover.png = None;
        self.cover.rev = self.cover.rev.wrapping_add(1);
        if !self.settings.performance.load_covers {
            return;
        }
        let accent = self.settings.theme.accent;
        if let Some(path) = track.source.path() {
            self.cover
                .service
                .request(key, path.to_path_buf(), accent, 640, false);
            return;
        }
        if let Some(url) = track
            .cover_url
            .as_ref()
            .filter(|url| url.starts_with("http://") || url.starts_with("https://"))
        {
            let http = crate::api::HttpOptions::for_network(&self.settings.network);
            self.cover
                .service
                .request_url(key, url.clone(), http, accent, 640, false);
        }
    }

    fn clear_cover(&mut self) {
        self.cover.service.cancel();
        self.cover.key.clear();
        self.cover.image = None;
        self.cover.png = None;
        self.cover.rev = self.cover.rev.wrapping_add(1);
    }

    // ---------------------------------------------------------------------
    // Online (Bamboo) search, playback and downloads
    // ---------------------------------------------------------------------

    fn primary_source_index(&self) -> Option<usize> {
        if let Some(id) = self.settings.active_source.as_deref()
            && let Some(index) = self
                .settings
                .sources
                .iter()
                .position(|source| source.id == id)
        {
            return Some(index);
        }
        self.settings
            .sources
            .iter()
            .position(|source| source.enabled && source.kind == SourceKind::Online)
            .or_else(|| {
                self.settings
                    .sources
                    .iter()
                    .position(|source| source.enabled)
            })
            .or_else(|| (!self.settings.sources.is_empty()).then_some(0))
    }

    /// Source whose address/credentials the settings page edits.
    pub fn primary_source(&self) -> Option<&SourceConfig> {
        self.primary_source_index()
            .and_then(|index| self.settings.sources.get(index))
    }

    fn edit_primary_source(&mut self, edit: impl FnOnce(&mut SourceConfig)) {
        if let Some(index) = self.primary_source_index()
            && let Some(source) = self.settings.sources.get_mut(index)
        {
            edit(source);
            self.settings_dirty = true;
        }
    }

    fn cycle_primary_source(&mut self) {
        let count = self.settings.sources.len();
        if count == 0 {
            return;
        }
        let current = self.primary_source_index().unwrap_or(0);
        let next = (current + 1) % count;
        // Selecting a source is an explicit "use this" action; enable it so
        // the cycle can never stop on a source that search refuses to use.
        self.settings.sources[next].enabled = true;
        let id = self.settings.sources[next].id.clone();
        let name = self.settings.sources[next].name.clone();
        self.settings.active_source = Some(id);
        self.settings_dirty = true;
        self.notify(crate::trf!("当前音源：{}", name), false);
    }

    fn add_source(&mut self) {
        let mut index = 1usize;
        let id = loop {
            let candidate = format!("online{index}");
            if !self
                .settings
                .sources
                .iter()
                .any(|source| source.id == candidate)
            {
                break candidate;
            }
            index += 1;
        };
        let mut source = SourceConfig::preset(SourceKind::Online);
        source.id = id.clone();
        source.name = crate::trf!("在线音源 {}", index);
        source.enabled = true;
        let name = source.name.clone();
        self.settings.sources.push(source);
        self.settings.active_source = Some(id);
        self.settings_dirty = true;
        self.notify(crate::trf!("已新增音源：{}", name), false);
    }

    fn remove_primary_source(&mut self) {
        if self.settings.sources.len() <= 1 {
            self.notify(crate::i18n::tr("至少保留一个音源"), true);
            return;
        }
        let Some(index) = self.primary_source_index() else {
            return;
        };
        let removed = self.settings.sources.remove(index);
        let next = self
            .settings
            .sources
            .iter()
            .find(|source| source.enabled)
            .or_else(|| self.settings.sources.first())
            .map(|source| source.id.clone());
        if self.settings.active_source.as_deref() == Some(removed.id.as_str())
            || self.settings.active_source.is_none()
        {
            self.settings.active_source = next;
        }
        self.settings_dirty = true;
        self.notify(crate::trf!("已删除音源：{}", removed.name), false);
    }

    fn online_source(&self, source_id: &str) -> Option<SourceConfig> {
        self.settings
            .sources
            .iter()
            .find(|source| source.enabled && source.id == source_id)
            .cloned()
            .or_else(|| self.online_primary_source())
    }

    fn online_primary_source(&self) -> Option<SourceConfig> {
        if let Some(id) = self.settings.active_source.as_deref()
            && let Some(source) = self
                .settings
                .sources
                .iter()
                .find(|source| source.enabled && source.id == id)
        {
            return Some(source.clone());
        }
        self.settings
            .sources
            .iter()
            .find(|source| source.enabled && source.kind == SourceKind::Online)
            .or_else(|| self.settings.sources.iter().find(|source| source.enabled))
            .cloned()
    }

    fn stream_cache_path(&self, key: &str, source: &SourceConfig) -> PathBuf {
        let dir = self
            .settings
            .network
            .stream_cache_dir
            .clone()
            .filter(|dir| !dir.as_os_str().is_empty())
            .unwrap_or_else(|| self.paths.stream_cache.clone());
        let digest = blake3::hash(format!("{}|{:?}", key, source.quality).as_bytes())
            .to_hex()
            .to_string();
        dir.join(format!("{}.stream", &digest[..32]))
    }

    fn online_cache(&self, key: &str, source: &SourceConfig) -> Option<PathBuf> {
        (self.settings.network.cache_limit_mb > 0)
            .then(|| self.stream_cache_path(key, source))
    }

    fn start_online_search(&mut self, query: String, offset: u32, limit: u32) {
        self.online.query = query.clone();
        self.online.detail = None;
        self.online.limit = limit.clamp(1, 500);
        self.online.offset = offset;
        if offset == 0 {
            self.online.last_page = 0;
            self.online.exhausted = false;
            self.online.has_more = false;
            self.online.songs.clear();
            self.online.total = 0;
        }
        self.online.rev = self.online.rev.wrapping_add(1);
        self.online.suggestions.clear();
        self.online.from_cache = false;
        let query = query.trim().to_string();
        if query.is_empty() {
            self.online.songs.clear();
            self.online.total = 0;
            self.online.loading = false;
            self.online.error = None;
            return;
        }
        let Some(source) = self.online_primary_source() else {
            self.online.songs.clear();
            self.online.loading = false;
            self.online.error =
                Some(crate::i18n::tr("请先在设置中添加在线音源").to_string());
            return;
        };
        // Serve the cached page immediately, then refresh it from the network
        // (stale-while-revalidate). Offline uses the cache alone.
        let cache_key = crate::api::cache::search_key(
            &source.id,
            &source.base_url,
            match self.online.kind {
                SearchKind::Song => "song",
                SearchKind::Album => "album",
            },
            &query,
            offset,
        );
        let cached = self.search_cache.get(&cache_key).cloned();
        if let Some(cached) = cached.as_ref() {
            self.online.songs = cached.items.clone();
            self.online.total = cached.total;
            self.online.last_page = offset / self.online.limit.max(1);
            self.online.has_more = self.online.songs.len() >= self.online.limit as usize;
            self.online.exhausted = false;
            self.online.from_cache = true;
            self.online.rev = self.online.rev.wrapping_add(1);
        }
        if self.settings.network.offline {
            self.online.loading = false;
            if cached.is_none() {
                self.online.songs.clear();
                self.online.error =
                    Some(crate::i18n::tr("离线模式下无法搜索").to_string());
            } else {
                self.online.error = None;
            }
            return;
        }
        self.online.request = self.api.search(
            &source,
            &self.settings.network,
            self.online.kind,
            &query,
            offset,
            self.online.limit,
        );
        self.online.loading = true;
        self.online.error = None;
        self.start_online_suggest(query);
    }

    fn cache_search_page(
        &mut self,
        offset: u32,
        _limit: u32,
        total: u64,
        name: Option<String>,
        items: Vec<ApiSong>,
    ) {
        let Some(source) = self.online_primary_source() else {
            return;
        };
        let key = crate::api::cache::search_key(
            &source.id,
            &source.base_url,
            match self.online.kind {
                SearchKind::Song => "song",
                SearchKind::Album => "album",
            },
            self.online.query.trim(),
            offset,
        );
        self.search_cache.insert(key, total, name, items);
        self.search_cache_dirty = true;
    }

    fn save_search_cache(&mut self) {
        if !self.search_cache_dirty {
            return;
        }
        match self.search_cache.save(&self.search_cache_path) {
            Ok(()) => {
                self.search_cache_dirty = false;
                self.search_cache_saved = Instant::now();
            }
            Err(err) => tracing::warn!("save search cache failed: {err}"),
        }
    }

    fn start_online_suggest(&mut self, query: String) {
        if self.settings.network.offline || query.trim().is_empty() {
            return;
        }
        let Some(source) = self.online_primary_source() else {
            return;
        };
        self.online.suggest_request = self
            .api
            .suggest(&source, &self.settings.network, query.trim());
    }

    fn open_online_album(&mut self, album_id: String) {
        let Some(source) = self.online_primary_source() else {
            self.notify(crate::i18n::tr("请先在设置中添加在线音源"), true);
            return;
        };
        let (title, subtitle) = self
            .online
            .rows()
            .iter()
            .find(|song| {
                song.album_id.as_deref() == Some(album_id.as_str()) || song.id == album_id
            })
            .map(|song| {
                let title = if self.online.kind == SearchKind::Album {
                    song.name.clone()
                } else if song.album.trim().is_empty() {
                    song.name.clone()
                } else {
                    song.album.clone()
                };
                (title, song.artist.clone())
            })
            .unwrap_or_default();
        let limit = self.online.limit.max(1);
        let request = self
            .api
            .album(&source, &self.settings.network, &album_id, 0, limit);
        self.online.detail = Some(OnlineDetail {
            title,
            subtitle,
            album_id: Some(album_id),
            artist_id: None,
            items: Vec::new(),
            page: 0,
            limit,
            total: 0,
            last_page: 0,
            exhausted: false,
            has_more: false,
            loading: true,
            error: None,
            request,
        });
        self.online.rev = self.online.rev.wrapping_add(1);
    }

    fn open_online_artist(&mut self, artist_id: String) {
        let Some(source) = self.online_primary_source() else {
            self.notify(crate::i18n::tr("请先在设置中添加在线音源"), true);
            return;
        };
        let subtitle = self
            .online
            .rows()
            .iter()
            .find(|song| song.artist_id.as_deref() == Some(artist_id.as_str()))
            .map(|song| song.artist.clone())
            .unwrap_or_default();
        let limit = self.online.limit.max(1);
        let request = self
            .api
            .artist(&source, &self.settings.network, &artist_id, 0, limit);
        self.online.detail = Some(OnlineDetail {
            title: subtitle.clone(),
            subtitle,
            album_id: None,
            artist_id: Some(artist_id),
            items: Vec::new(),
            page: 0,
            limit,
            total: 0,
            last_page: 0,
            exhausted: false,
            has_more: false,
            loading: true,
            error: None,
            request,
        });
        self.online.rev = self.online.rev.wrapping_add(1);
    }

    /// Move one page in the current search or drill-down results. The
    /// server's `total` is a rendering hint, so "next" is gated by the last
    /// page being full instead of a computed page count.
    fn online_turn_page(&mut self, delta: i32) {
        if delta > 0
            && self
                .online
                .detail
                .as_ref()
                .map_or(self.online.has_more, |detail| detail.has_more)
                == false
        {
            return;
        }
        let Some(source) = self.online_primary_source() else {
            return;
        };
        if let Some(detail) = self.online.detail.as_mut() {
            let limit = detail.limit.max(1);
            let next = detail.page as i64 + delta as i64;
            if next < 0 {
                return;
            }
            let next = next as u32;
            let offset = next * limit;
            let request = match (&detail.album_id, &detail.artist_id) {
                (Some(album_id), _) => self.api.album(
                    &source,
                    &self.settings.network,
                    album_id,
                    offset,
                    limit,
                ),
                (_, Some(artist_id)) => self.api.artist(
                    &source,
                    &self.settings.network,
                    artist_id,
                    offset,
                    limit,
                ),
                _ => return,
            };
            detail.page = next;
            detail.loading = true;
            detail.error = None;
            detail.request = request;
            self.online.rev = self.online.rev.wrapping_add(1);
            return;
        }
        let limit = self.online.limit.max(1);
        let next = (self.online.offset / limit) as i64 + delta as i64;
        if next < 0 {
            return;
        }
        let query = self.online.query.clone();
        self.start_online_search(query, next as u32 * limit, limit);
    }

    fn start_online_random(&mut self) {
        if self.settings.network.offline {
            self.online.error = Some(crate::i18n::tr("离线模式下无法加载推荐").to_string());
            self.online.rev = self.online.rev.wrapping_add(1);
            return;
        }
        let Some(source) = self.online_primary_source() else {
            self.online.error = Some(crate::i18n::tr("请先在设置中添加在线音源").to_string());
            self.online.rev = self.online.rev.wrapping_add(1);
            return;
        };
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|time| time.as_secs())
            .unwrap_or(1);
        self.online.request = self
            .api
            .random(&source, &self.settings.network, 30, seed);
        self.online.detail = None;
        self.online.loading = true;
        self.online.error = None;
        self.online.rev = self.online.rev.wrapping_add(1);
    }

    fn discover_refresh(&mut self) {
        if self.settings.network.offline {
            self.discover.error =
                Some(crate::i18n::tr("离线模式下无法加载推荐").to_string());
            self.discover.loading = false;
            self.discover.rev = self.discover.rev.wrapping_add(1);
            return;
        }
        let Some(source) = self.online_primary_source() else {
            self.discover.error =
                Some(crate::i18n::tr("请先在设置中添加在线音源").to_string());
            self.discover.loading = false;
            self.discover.rev = self.discover.rev.wrapping_add(1);
            return;
        };
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|time| time.as_secs())
            .unwrap_or(1);
        self.discover.request = self
            .api
            .random(&source, &self.settings.network, 18, seed);
        self.discover.loading = true;
        self.discover.error = None;
        self.discover.rev = self.discover.rev.wrapping_add(1);
    }

    fn discover_play(&mut self, index: usize, queue_only: bool) {
        let Some(source) = self.online_primary_source() else {
            self.notify(crate::i18n::tr("请先在设置中添加在线音源"), true);
            return;
        };
        let Some(song) = self.discover.songs.get(index).cloned() else {
            return;
        };
        let track = song.to_track(&source);
        if queue_only {
            if self.queue.add_many([track]) > 0 {
                self.queue_changed();
                self.notify(crate::i18n::tr("已加入播放队列"), false);
            }
            return;
        }
        let key = track.dedupe_key();
        let added = self.queue.add_many([track]);
        if added > 0 {
            self.queue_changed();
        }
        if let Some(index) = self.queue.position_of_key(&key) {
            self.play_index(index);
        }
    }

    fn discover_download(&mut self, index: usize) {
        if !self.settings.features.downloads {
            self.notify(crate::i18n::tr("下载功能已关闭"), true);
            return;
        }
        let Some(source) = self.online_primary_source() else {
            self.notify(crate::i18n::tr("请先在设置中添加在线音源"), true);
            return;
        };
        let Some(song) = self.discover.songs.get(index).cloned() else {
            return;
        };
        let track = song.to_track(&source);
        self.queue_download(track, Some(source), self.settings.download.quality);
        self.notify(crate::i18n::tr("已加入下载队列"), false);
    }

    fn play_online(&mut self, index: usize, queue_only: bool) {
        let Some(song) = self.online.rows().get(index).cloned() else {
            return;
        };
        // Album search rows carry an album id, not a song id: clicking one
        // must open the album instead of trying to stream it.
        if self.online.kind == SearchKind::Album && !self.online.is_detail() {
            let album_id = song.album_id.clone().unwrap_or_else(|| song.id.clone());
            self.open_online_album(album_id);
            return;
        }
        let Some(source) = self.online_primary_source() else {
            self.notify(crate::i18n::tr("请先在设置中添加在线音源"), true);
            return;
        };
        let track = song.to_track(&source);
        let key = track.dedupe_key();
        if queue_only {
            if self.queue.add_many([track]) > 0 {
                self.queue_changed();
                self.notify(crate::i18n::tr("已加入播放队列"), false);
            }
            return;
        }
        let index = match self
            .queue
            .items
            .iter()
            .position(|item| item.dedupe_key() == key)
        {
            Some(index) => index,
            None => {
                self.queue.add_many([track]);
                self.queue_changed();
                self.queue.len().saturating_sub(1)
            }
        };
        self.play_index(index);
    }

    fn queue_online_all(&mut self) {
        if self.online.kind == SearchKind::Album && !self.online.is_detail() {
            self.notify(crate::i18n::tr("请先打开专辑，再加入其中的歌曲"), true);
            return;
        }
        let Some(source) = self.online_primary_source() else {
            self.notify(crate::i18n::tr("请先在设置中添加在线音源"), true);
            return;
        };
        let rows: Vec<Track> = self
            .online
            .rows()
            .iter()
            .map(|song| song.to_track(&source))
            .collect();
        let added = self.queue.add_many(rows);
        if added > 0 {
            self.queue_changed();
            self.notify(
                crate::trf!("已把 {} 首歌曲加入播放队列", added),
                false,
            );
        }
    }

    fn download_online(&mut self, index: usize) {
        if self.online.kind == SearchKind::Album && !self.online.is_detail() {
            self.notify(crate::i18n::tr("请先打开专辑，再下载其中的歌曲"), true);
            return;
        }
        if !self.settings.features.downloads {
            self.notify(crate::i18n::tr("下载功能已关闭"), true);
            return;
        }
        let Some(source) = self.online_primary_source() else {
            self.notify(crate::i18n::tr("请先在设置中添加在线音源"), true);
            return;
        };
        let Some(song) = self.online.rows().get(index).cloned() else {
            return;
        };
        let track = song.to_track(&source);
        self.queue_download(track, Some(source), self.settings.download.quality);
        self.notify(crate::i18n::tr("已加入下载队列"), false);
    }

    fn download_queue(&mut self, index: usize) {
        if !self.settings.features.downloads {
            self.notify(crate::i18n::tr("下载功能已关闭"), true);
            return;
        }
        let Some(track) = self.queue.items.get(index).cloned() else {
            return;
        };
        if track.source.is_local() {
            self.notify(crate::i18n::tr("本地文件无需下载"), true);
            return;
        }
        let source = match &track.source {
            TrackSource::Bamboo { source_id, .. } => self.online_source(source_id),
            _ => None,
        };
        if matches!(track.source, TrackSource::Bamboo { .. }) && source.is_none() {
            self.notify(crate::i18n::tr("请先在设置中添加在线音源"), true);
            return;
        }
        self.queue_download(track, source, self.settings.download.quality);
        self.notify(crate::i18n::tr("已加入下载队列"), false);
    }

    fn queue_download(
        &mut self,
        track: Track,
        source: Option<SourceConfig>,
        quality: Option<crate::config::Quality>,
    ) {
        let id = self.download_next_id;
        self.download_next_id = self.download_next_id.wrapping_add(1);
        let base = self.download_base_dir();
        let request = DownloadRequest {
            track: track.clone(),
            source,
            quality,
            network: self.settings.network.clone(),
            settings: self.settings.download.clone(),
            base_dir: base,
        };
        self.download_rows.push(DownloadRow {
            id,
            title: track.display_title(),
            artist: track.display_artist(),
            state: DownloadState::Queued,
            received: 0,
            total: None,
            path: None,
            error: None,
            request: request.clone(),
        });
        self.downloads.start(
            id,
            request.track,
            request.source,
            request.quality,
            request.network,
            request.settings,
            request.base_dir,
        );
        self.download_rev = self.download_rev.wrapping_add(1);
    }

    fn retry_download(&mut self, id: u64) {
        let base = self.download_base_dir();
        let download_settings = self.settings.download.clone();
        let network = self.settings.network.clone();
        let Some(row) = self.download_rows.iter_mut().find(|row| row.id == id) else {
            return;
        };
        if row.state.is_active() {
            return;
        }
        row.state = DownloadState::Queued;
        row.received = 0;
        row.total = None;
        row.path = None;
        row.error = None;
        // Keep the per-track quality override, but pick up later changes to
        // template, conflict policy, tags and directory.
        row.request.network = network;
        row.request.settings = download_settings;
        row.request.base_dir = base;
        let request = row.request.clone();
        self.downloads.start(
            id,
            request.track,
            request.source,
            request.quality,
            request.network,
            request.settings,
            request.base_dir,
        );
        self.download_rev = self.download_rev.wrapping_add(1);
        self.notify(crate::i18n::tr("已重新加入下载队列"), false);
    }

    fn download_base_dir(&self) -> PathBuf {
        self.settings
            .download
            .dir
            .clone()
            .filter(|dir| !dir.as_os_str().is_empty())
            .unwrap_or_else(|| self.paths.default_download_dir())
    }

    /// Remove abandoned `.part` files below the download directory. The scan
    /// runs on a worker thread; the count arrives through `tick`.
    fn clean_partial_downloads(&mut self) {
        if self.partial_cleanup.is_some() {
            return;
        }
        let base = self.download_base_dir();
        let (tx, rx) = crossbeam_channel::bounded(1);
        let wake = self.wake.clone();
        match std::thread::Builder::new()
            .name("bamboo-part-clean".to_string())
            .spawn(move || {
                let removed = remove_part_files(&base);
                let _ = tx.send(removed);
                if let Some(wake) = wake.as_ref() {
                    wake();
                }
            }) {
            Ok(_) => self.partial_cleanup = Some(rx),
            Err(err) => {
                tracing::warn!("partial cleanup thread failed: {err}");
                self.notify(crate::i18n::tr("无法启动清理任务").to_string(), true);
            }
        }
    }

    fn poll_partial_cleanup(&mut self) -> bool {
        let Some(rx) = self.partial_cleanup.as_ref() else {
            return false;
        };
        let Ok(removed) = rx.try_recv() else {
            return false;
        };
        self.partial_cleanup = None;
        self.download_rev = self.download_rev.wrapping_add(1);
        self.notify(crate::trf!("已清理 {} 个未完成文件", removed), false);
        true
    }

    fn stream_cache_dir(&self) -> PathBuf {
        self.settings
            .network
            .stream_cache_dir
            .clone()
            .filter(|dir| !dir.as_os_str().is_empty())
            .unwrap_or_else(|| self.paths.stream_cache.clone())
    }

    fn refresh_stream_cache(&self, prune: bool) {
        self.cache_service.refresh(
            self.stream_cache_dir(),
            self.settings.network.cache_limit_mb,
            crate::api::cache::STREAM_CACHE_TTL_SECS,
            prune,
        );
    }

    fn open_url(&mut self, url: String) {
        let url = url.trim().to_string();
        if url.is_empty() {
            return;
        }
        // A bare numeric song id or a Bamboo share link resolves through the
        // online source instead of being treated as a direct stream.
        if !url.is_empty() && url.chars().all(|ch| ch.is_ascii_digit()) {
            self.open_bamboo_id(url);
            return;
        }
        let lower = url.to_ascii_lowercase();
        if !(lower.starts_with("http://") || lower.starts_with("https://")) {
            self.notify(
                crate::i18n::tr("只支持 http/https 链接、Bamboo 分享链接或歌曲 ID"),
                true,
            );
            return;
        }
        if let Ok(parsed) = url::Url::parse(&url)
            && let Some(song_id) = bamboo_id_from_url(&parsed)
        {
            self.open_bamboo_id(song_id);
            return;
        }
        if self.settings.network.offline {
            self.notify(crate::i18n::tr("离线模式下无法播放网络链接"), true);
            return;
        }
        let track = Track {
            source: if lower.contains(".m3u8") {
                TrackSource::Radio { url: url.clone() }
            } else {
                TrackSource::Url { url: url.clone() }
            },
            title: url,
            source_name: crate::i18n::tr("网络").to_string(),
            ..Default::default()
        };
        self.queue.add_many([track]);
        self.queue_changed();
        let index = self.queue.len().saturating_sub(1);
        self.play_index(index);
    }

    fn open_bamboo_id(&mut self, song_id: String) {
        if self.settings.network.offline {
            self.notify(crate::i18n::tr("离线模式下无法播放在线歌曲"), true);
            return;
        }
        let Some(source) = self.online_primary_source() else {
            self.notify(crate::i18n::tr("请先在设置中添加在线音源"), true);
            return;
        };
        let track = Track {
            source: TrackSource::Bamboo {
                source_id: source.id.clone(),
                song_id: song_id.clone(),
                base_url: source.base_url.clone(),
            },
            title: crate::trf!("在线歌曲 {}", song_id),
            source_name: source.name.clone(),
            ..Default::default()
        };
        self.queue.add_many([track]);
        self.queue_changed();
        let index = self.queue.len().saturating_sub(1);
        self.play_index(index);
    }

    /// Start a Bamboo resolve. Returns true when playback is now pending an
    /// API answer (the caller must not arm the next deck yet).
    fn start_online_resolve(&mut self, track: &Track, start_secs: f64, autoplay: bool) -> bool {
        let TrackSource::Bamboo { source_id, .. } = &track.source else {
            return false;
        };
        if self.settings.network.offline {
            self.playback_state = PlaybackState::Stopped;
            self.notify(crate::i18n::tr("离线模式下无法播放在线歌曲"), true);
            return true;
        }
        let Some(source) = self.online_source(source_id) else {
            self.playback_state = PlaybackState::Stopped;
            self.notify(crate::i18n::tr("请先在设置中添加在线音源"), true);
            return true;
        };
        let key = track.dedupe_key();
        let cache = self.online_cache(&key, &source);
        // A pre-resolved URL from `arm_next` avoids a second API round-trip.
        if let Some((cached_key, prepared)) = self.online_next.take()
            && cached_key == key
        {
            self.start_online_playback(key, prepared, cache, start_secs, autoplay);
            return true;
        }
        let Some(id) = self.api.resolve(
            &source,
            &self.settings.network,
            track,
            cache,
            start_secs,
            autoplay,
        ) else {
            self.playback_state = PlaybackState::Stopped;
            return true;
        };
        self.online_resolve = Some(OnlineResolve { id, key });
        self.playback_state = PlaybackState::Loading;
        true
    }

    fn start_online_playback(
        &mut self,
        key: String,
        prepared: PreparedPlay,
        cache: Option<PathBuf>,
        start_secs: f64,
        autoplay: bool,
    ) {
        let Some(current) = self.current_track.as_ref() else {
            return;
        };
        if current.dedupe_key() != key {
            return;
        }
        if let Some(info) = prepared.info.as_ref() {
            self.merge_online_info(info);
        }
        self.set_online_lyrics(prepared.lrc.clone());
        // `resolve_next` only returns a URL; fetch the lyrics info in the
        // background so the lyric panel is not permanently blank.
        if prepared.lrc.is_none()
            && prepared.info.is_none()
            && self.settings.lyrics.enabled
            && self.lyrics.is_none()
        {
            if let TrackSource::Bamboo { source_id, song_id, .. } =
                &self.current_track.as_ref().unwrap().source
                && let Some(source) = self.online_source(source_id)
            {
                let id = self.api.info(&source, &self.settings.network, song_id);
                self.online_info = Some((id, key.clone()));
            }
        }
        let download_first = self
            .current_track
            .as_ref()
            .and_then(|track| match &track.source {
                TrackSource::Bamboo { source_id, .. } => self.online_source(source_id),
                _ => None,
            })
            .is_some_and(|source| source.kind == SourceKind::Local);
        let http = crate::api::HttpOptions::for_network(&self.settings.network).for_stream();
        if cache.is_some() {
            // Refresh usage and apply the TTL/size policy before this track
            // starts writing a new cache entry.
            self.refresh_stream_cache(true);
        }
        self.engine_load_token = self.engine.load(
            EngineSource::Url {
                url: prepared.url,
                cache,
                http,
                download_first,
            },
            start_secs,
            autoplay,
        );
        self.arm_next();
    }

    fn merge_online_info(&mut self, info: &ApiSong) {
        let merge = |track: &mut Track| {
            if track.title.trim().is_empty() || track.title == info.id {
                track.title = info.name.clone();
            }
            if track.artist.trim().is_empty() {
                track.artist = info.artist.clone();
            }
            if track.album.trim().is_empty() {
                track.album = info.album.clone();
            }
            if track.album_id.is_none() {
                track.album_id = info.album_id.clone();
            }
            if track.artist_id.is_none() {
                track.artist_id = info.artist_id.clone();
            }
            if track.cover_url.is_none() {
                track.cover_url = info.pic.clone();
            }
        };
        if let Some(current) = self.current_track.as_mut() {
            merge(current);
        }
        if let Some(index) = self.queue.current
            && let Some(item) = self.queue.items.get_mut(index)
        {
            merge(item);
        }
        if let Some(track) = self.current_track.clone() {
            self.refresh_cover(&track);
        }
        self.session_dirty = true;
    }

    fn set_online_lyrics(&mut self, lrc: Option<String>) {
        if let Some(text) = lrc.filter(|text| !text.trim().is_empty()) {
            let lyrics = crate::lyrics::parse_text(
                &text,
                crate::lyrics::LyricsSource::Bamboo,
                None,
                None,
            );
            if !lyrics.lines.is_empty() {
                self.lyrics = Some(lyrics);
                self.lyrics_error = None;
            } else if self.lyrics.is_none() {
                self.lyrics_error = Some(crate::i18n::tr("暂无歌词").to_string());
            }
        } else if self.lyrics.is_none() {
            self.lyrics_error = Some(crate::i18n::tr("暂无歌词").to_string());
        }
        self.lyrics_loading = false;
        self.lyrics_rev = self.lyrics_rev.wrapping_add(1);
    }

    fn poll_online_events(&mut self) -> bool {
        let mut changed = false;
        while let Some(event) = self.api.poll() {
            changed = true;
            match event {
                ApiEvent::Search { id, page, .. } => {
                    if id != self.online.request {
                        continue;
                    }
                    self.online.loading = false;
                    match page {
                        Ok(page) => {
                            let limit = self.online.limit.max(1);
                            let requested = self.online.offset / limit;
                            if page.items.is_empty() && requested > 0 {
                                // The server's `total` is only a rendering
                                // hint. Keep the previous page's rows (they
                                // are still in `songs`), rewind the cursor and
                                // disable "next".
                                self.online.exhausted = true;
                                self.online.has_more = false;
                                self.online.offset = self.online.last_page * limit;
                            } else {
                                let items = page.items.clone();
                                self.online.has_more =
                                    page.items.len() >= limit as usize;
                                self.online.songs = page.items;
                                self.online.total = page.total;
                                self.online.last_page = requested;
                                self.online.error = None;
                                self.online.from_cache = false;
                                self.cache_search_page(
                                    requested * limit,
                                    limit,
                                    page.total,
                                    page.name,
                                    items,
                                );
                            }
                        }
                        Err(err) => {
                            if self.online.from_cache && !self.online.songs.is_empty() {
                                // Offline/transient failure: keep the cached
                                // rows on screen instead of replacing them.
                                self.online.error = None;
                                self.notify(
                                    crate::i18n::tr("网络请求失败，显示缓存结果"),
                                    true,
                                );
                            } else {
                                self.online.error = Some(err);
                            }
                        }
                    }
                    self.online.rev = self.online.rev.wrapping_add(1);
                }
                ApiEvent::Album { id, page, .. } | ApiEvent::Artist { id, page, .. } => {
                    let Some(detail) = self.online.detail.as_mut() else {
                        continue;
                    };
                    if id != detail.request {
                        continue;
                    }
                    detail.loading = false;
                    match page {
                        Ok(page) => {
                            // Each request is a complete page window.
                            if page.items.is_empty() && detail.page > 0 {
                                // Same as search: keep the previous page's
                                // rows and stop paging forward.
                                detail.exhausted = true;
                                detail.has_more = false;
                                detail.page = detail.last_page;
                                self.online.rev = self.online.rev.wrapping_add(1);
                                continue;
                            }
                            detail.has_more = page.items.len() >= detail.limit as usize;
                            detail.items = page.items;
                            detail.total = page.total;
                            detail.last_page = detail.page;
                            detail.error = None;
                            if let Some(name) = page.name
                                && detail.title.trim().is_empty()
                            {
                                detail.title = name;
                            }
                        }
                        Err(err) => detail.error = Some(err),
                    }
                    self.online.rev = self.online.rev.wrapping_add(1);
                }
                ApiEvent::Suggest {
                    id,
                    suggestions: result,
                    ..
                } => {
                    if id != self.online.suggest_request {
                        continue;
                    }
                    if let Ok(items) = result {
                        self.online.suggestions = items;
                        self.online.rev = self.online.rev.wrapping_add(1);
                    }
                }
                ApiEvent::Random { id, page } => {
                    if id == self.discover.request && id != 0 {
                        self.discover.loading = false;
                        match page {
                            Ok(page) => {
                                self.discover.songs = page.items;
                                self.discover.error = None;
                            }
                            Err(err) => self.discover.error = Some(err),
                        }
                        self.discover.rev = self.discover.rev.wrapping_add(1);
                        continue;
                    }
                    self.online.loading = false;
                    match page {
                        Ok(page) => {
                            self.online.detail = None;
                            self.online.songs = page.items;
                            self.online.total = page.total;
                            self.online.offset = 0;
                            self.online.last_page = 0;
                            self.online.exhausted = true;
                            self.online.has_more = false;
                            self.online.error = None;
                        }
                        Err(err) => self.online.error = Some(err),
                    }
                    self.online.rev = self.online.rev.wrapping_add(1);
                }
                ApiEvent::Resolve {
                    id,
                    track_key,
                    cache,
                    start_secs,
                    autoplay,
                    result,
                } => {
                    let Some(pending) = self.online_resolve.take() else {
                        continue;
                    };
                    if pending.id != id || pending.key != track_key {
                        continue;
                    }
                    match result {
                        Ok(prepared) => {
                            self.start_online_playback(
                                track_key, prepared, cache, start_secs, autoplay,
                            );
                        }
                        Err(err) => {
                            self.playback_state = PlaybackState::Stopped;
                            self.lyrics_loading = false;
                            self.notify(crate::trf!("在线播放失败：{}", err), true);
                        }
                    }
                }
                ApiEvent::NextResolved {
                    id,
                    track_key,
                    result,
                    ..
                } => {
                    let _ = id;
                    self.online_next_request = None;
                    if let Ok(prepared) = result {
                        self.online_next = Some((track_key, prepared));
                        self.arm_next();
                    }
                }
                ApiEvent::Info { id, result } => {
                    let Some((pending_id, key)) = self.online_info.clone() else {
                        continue;
                    };
                    if id != pending_id {
                        continue;
                    }
                    self.online_info = None;
                    if self.current_track.as_ref().map(Track::dedupe_key) != Some(key) {
                        continue;
                    }
                    if let Ok(info) = result {
                        self.merge_online_info(&info.song);
                        self.set_online_lyrics(info.lrc);
                    }
                }
                ApiEvent::Test { result, .. } => match result {
                    Ok(message) => self.notify(message, false),
                    Err(err) => self.notify(err, true),
                },
            }
        }
        changed
    }

    /// Remove cached streams, cover art and downloaded lyric files. Never
    /// touches the user's music or completed downloads. The stream directory
    /// is cleared on the cache worker; covers/lyrics are small.
    fn clear_stream_cache(&mut self) {
        let stream = self.stream_cache_dir();
        self.cache_service.clear(stream);
        let mut removed = 0usize;
        for dir in [self.paths.covers.clone(), self.paths.lyrics_cache.clone()] {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() && std::fs::remove_file(&path).is_ok() {
                    removed += 1;
                }
            }
        }
        self.notify(
            crate::trf!("已清理 {} 个缓存文件", removed),
            false,
        );
    }

    fn poll_downloads(&mut self) -> bool {        let mut changed = false;
        for event in self.downloads.poll() {
            changed = true;
            match event {
                DownloadEvent::Progress {
                    id,
                    received,
                    total,
                } => {
                    if let Some(row) = self.download_rows.iter_mut().find(|row| row.id == id) {
                        row.received = received;
                        row.total = total;
                    }
                }
                DownloadEvent::State {
                    id,
                    state,
                    received,
                    total,
                    path,
                    error,
                } => {
                    let mut finished = None;
                    if let Some(row) = self.download_rows.iter_mut().find(|row| row.id == id) {
                        row.state = state;
                        row.received = received;
                        row.total = total;
                        row.path = path.clone();
                        row.error = error.clone();
                        finished = Some((row.title.clone(), path.clone()));
                    }
                    match state {
                        DownloadState::Completed => {
                            if self.settings.download.add_to_library
                                && let Some(path) = path
                            {
                                self.library.db.index_paths(vec![path]);
                            }
                            if let Some((title, _)) = finished {
                                self.notify(crate::trf!("下载完成：{}", title), false);
                            }
                        }
                        DownloadState::Failed => {
                            let message = error.unwrap_or_else(|| {
                                crate::i18n::tr("下载失败").to_string()
                            });
                            self.notify(message, true);
                        }
                        _ => {}
                    }
                }
            }
        }
        if changed {
            self.download_rev = self.download_rev.wrapping_add(1);
        }
        changed
    }

    fn poll_loudness(&mut self) -> bool {
        let Some(service) = self.loudness.as_ref() else {
            return false;
        };
        let mut changed = false;
        while let Some(event) = service.poll() {
            changed = true;
            if matches!(event.result, Ok(Some(_))) {
                self.engine.refresh_replaygain();
            }
            if self.loudness_pending.as_deref() == Some(event.path.to_string_lossy().as_ref()) {
                self.loudness_pending = None;
            }
        }
        changed
    }

    fn poll_cache(&mut self) -> bool {
        let mut changed = false;
        while let Some(event) = self.cache_service.poll() {
            let CacheEvent::Usage { usage, .. } = event;
            self.cache_usage = usage;
            self.cache_rev = self.cache_rev.wrapping_add(1);
            changed = true;
        }
        changed
    }

    fn maybe_scan_loudness(&mut self, track: &Track) {
        if !self.settings.playback.replaygain_scan
            || self.settings.playback.replaygain == ReplayGainMode::Off
        {
            return;
        }
        let Some(path) = track.source.path().filter(|path| path.is_file()) else {
            return;
        };
        let key = track.dedupe_key();
        if self.loudness_pending.as_deref() == Some(key.as_str()) {
            return;
        }
        if crate::audio::loudness::cached_gain(path).is_some() {
            return;
        }
        let wake = self.wake.clone();
        self.loudness
            .get_or_insert_with(|| {
                crate::audio::loudness::LoudnessService::with_wake(wake)
            })
            .scan(path.to_path_buf());
        self.loudness_pending = Some(key);
    }

    fn queue_changed(&mut self) {
        self.queue_rev = self.queue_rev.wrapping_add(1);
    }

    /// Request the panel picture when Image mode is selected. Decoding happens
    /// on `LyricsBackgroundService`; the result is matched by path so a quick
    /// sequence of picks cannot apply a stale image.
    fn request_lyrics_background(&mut self) {
        let wanted = if self.settings.lyrics.panel_background == LyricsPanelBackground::Image {
            self.settings.lyrics.panel_image.clone()
        } else {
            None
        };
        match wanted {
            Some(path) => {
                if self.lyrics_background_key.as_ref() != Some(&path)
                    && self.lyrics_background_pending.as_ref() != Some(&path)
                {
                    // Drop the old picture immediately so the panel never
                    // shows the previous choice while the new one decodes.
                    self.lyrics_background = None;
                    self.lyrics_background_rev = self.lyrics_background_rev.wrapping_add(1);
                    self.lyrics_background_pending = Some(path.clone());
                    self.lyrics_background_service.load(path);
                }
            }
            None => {
                if self.lyrics_background.is_some() || self.lyrics_background_key.is_some() {
                    self.lyrics_background = None;
                    self.lyrics_background_key = None;
                    self.lyrics_background_rev = self.lyrics_background_rev.wrapping_add(1);
                }
                self.lyrics_background_pending = None;
            }
        }
    }

    fn poll_lyrics_background(&mut self) -> bool {
        let mut changed = false;
        while let Some(result) = self.lyrics_background_service.poll() {
            if self.lyrics_background_pending.as_deref() != Some(result.path.as_path()) {
                continue;
            }
            self.lyrics_background_pending = None;
            self.lyrics_background_key = Some(result.path);
            self.lyrics_background = result.image.map(Arc::new);
            self.lyrics_background_rev = self.lyrics_background_rev.wrapping_add(1);
            changed = true;
            if let Some(error) = result.error {
                self.notify(error, true);
            }
        }
        changed
    }

    /// Decode the custom background picture off the UI thread. The result is
    /// matched by path so quickly picking several files cannot apply a stale
    /// frame; the old picture is dropped immediately to avoid showing it.
    fn request_background_image(&mut self) {
        let wanted = if self.settings.theme.background
            == crate::config::BackgroundMode::Image
        {
            self.settings.theme.background_image.clone()
        } else {
            None
        };
        match wanted {
            Some(path) => {
                if self.background_image_key.as_ref() != Some(&path)
                    && self.background_image_pending.as_ref() != Some(&path)
                {
                    self.background_image = None;
                    self.background_image_rev = self.background_image_rev.wrapping_add(1);
                    self.background_image_pending = Some(path.clone());
                    self.background_image_service.load(path);
                }
            }
            None => {
                if self.background_image.is_some() || self.background_image_key.is_some() {
                    self.background_image = None;
                    self.background_image_key = None;
                    self.background_image_rev = self.background_image_rev.wrapping_add(1);
                }
                self.background_image_pending = None;
            }
        }
    }

    fn poll_background_image(&mut self) -> bool {
        let Some(result) = self.background_image_service.poll() else {
            return false;
        };
        if self.background_image_pending.as_deref() != Some(result.path.as_path()) {
            return false;
        }
        self.background_image_pending = None;
        self.background_image_key = Some(result.path);
        self.background_image = result.image.map(Arc::new);
        self.background_image_rev = self.background_image_rev.wrapping_add(1);
        if let Some(error) = result.error {
            self.notify(error, true);
        }
        true
    }

    fn refresh_lyrics(&mut self, track: &Track) {
        self.lyrics_service.cancel();
        self.lyrics = None;
        self.lyrics_loading = false;
        self.lyrics_error = None;
        self.lyrics_key = track.dedupe_key();
        self.lyrics_rev = self.lyrics_rev.wrapping_add(1);
        if !self.settings.lyrics.enabled {
            self.lyrics_error = Some(crate::i18n::tr("歌词功能已关闭").to_string());
            return;
        }
        if !self.settings.lyrics.auto_load {
            return;
        }
        // A prefetched result avoids a blank lyric panel on gapless/next.
        if let Some(cached) = self.prefetched_lyrics.remove(&self.lyrics_key) {
            self.prefetched_order.retain(|key| key != &self.lyrics_key);
            self.lyrics = Some(cached);
            self.prefetch_next_lyrics();
            return;
        }
        if let Some(path) = track.source.path() {
            let region = crate::lyrics::resolve_region(&self.settings.lyrics.translation_language);
            self.lyrics_service.request_auto(
                self.lyrics_key.clone(),
                path.to_path_buf(),
                self.settings.lyrics.encoding,
                &region,
            );
            self.lyrics_loading = true;
            self.prefetch_next_lyrics();
            return;
        }
        if matches!(track.source, TrackSource::Bamboo { .. }) {
            // `api.resolve` returns the online LRC together with the stream
            // URL; keep the panel in its loading state until it arrives.
            self.lyrics_loading = true;
            self.prefetch_next_lyrics();
            return;
        }
        self.lyrics_error = Some(crate::i18n::tr("该曲目没有可用的歌词来源").to_string());
        self.prefetch_next_lyrics();
    }

    /// Ask the lyrics worker for the next local track's lyrics in the
    /// background. Results are cached until that track becomes current.
    fn prefetch_next_lyrics(&mut self) {
        if !self.settings.lyrics.prefetch_next {
            return;
        }
        let Some(path) = self
            .queue
            .peek_next(true)
            .and_then(|index| self.queue.items.get(index))
            .and_then(|track| track.source.path())
            .map(Path::to_path_buf)
        else {
            return;
        };
        let key = Track::local(path.clone()).dedupe_key();
        if key == self.lyrics_key || self.prefetched_lyrics.contains_key(&key) {
            return;
        }
        let region = crate::lyrics::resolve_region(&self.settings.lyrics.translation_language);
        self.lyrics_service.request_prefetch(
            key,
            path,
            self.settings.lyrics.encoding,
            &region,
        );
    }

    /// Install synthetic data for `--bench` (no audio decoding involved).
    pub fn load_demo(&mut self, tracks: Vec<Track>, lyrics: Lyrics) {
        self.queue.clear();
        self.queue.add_many(tracks);
        self.queue.repeat = RepeatMode::All;
        self.queue.shuffle = false;
        self.queue.set_current(0);
        self.lyrics = Some(lyrics);
        self.lyrics_key = String::new();
        self.lyrics_loading = false;
        self.lyrics_error = None;
        self.clear_cover();
        self.demo = Some(DemoClock {
            position: 0.0,
            last: Instant::now(),
            paused: false,
        });
        self.select_demo_index(0);
        self.playback_state = PlaybackState::Playing;
    }

    fn select_demo_index(&mut self, index: usize) {
        if index >= self.queue.items.len() {
            return;
        }
        self.queue.set_current(index);
        self.current_track = self.queue.items.get(index).cloned();
        self.current_duration = self.current_track.as_ref().and_then(|track| track.duration);
    }

    fn tick_demo(&mut self) -> bool {
        let now = Instant::now();
        let duration = self.duration_secs().max(0.1);
        let finished = {
            let Some(demo) = self.demo.as_mut() else {
                return false;
            };
            let delta = now.duration_since(demo.last).as_secs_f64().clamp(0.0, 0.1);
            demo.last = now;
            if demo.paused {
                return false;
            }
            demo.position += delta;
            demo.position >= duration
        };
        if finished {
            let _ = self.advance_track(true);
            if let Some(demo) = self.demo.as_mut() {
                demo.position = 0.0;
            }
            return true;
        }
        false
    }

    // ------------------------------------------------------------------
    // Library (SQLite index)
    // ------------------------------------------------------------------

    /// Refresh the page currently on screen (play statistics, index done).
    fn refresh_library(&mut self) {
        self.start_library_query();
    }

    /// New search: drop the loaded page and query the first page again.
    fn reset_library_query(&mut self) {
        self.library.offset = 0;
        self.library.rows.clear();
        self.library.total = 0;
        self.library.loaded_once = false;
        self.start_library_query();
    }

    /// Show one complete page. The UI derives `limit` from the list height,
    /// so every visible row comes from this single query — there is no
    /// incremental load that a background refresh could truncate.
    fn set_library_page_window(&mut self, offset: usize, limit: usize) {
        let limit = limit.max(1);
        let same_window = self.library.offset == offset && self.library.limit == limit;
        self.library.offset = offset;
        self.library.limit = limit;
        if !same_window {
            self.library.rows.clear();
            self.library.loaded_once = false;
        }
        self.start_library_query();
    }

    fn start_library_query(&mut self) {
        self.library.pending_search = None;
        self.library.query_id = self.library.query_id.wrapping_add(1);
        self.library.loading = true;
        self.library.db.query(
            self.library.query_id,
            LibraryQuery {
                search: self.library.search.clone(),
                sort: self.library.sort,
                offset: self.library.offset,
                limit: self.library.limit.max(1),
            },
        );
    }

    fn play_statistics_entry(&mut self, index: usize) {
        let Some(entry) = self
            .library
            .stats
            .as_ref()
            .and_then(|stats| stats.recently_played.get(index))
            .cloned()
        else {
            return;
        };
        let track = entry.to_track();
        let key = track.dedupe_key();
        if let Some(position) = self.queue.position_of_key(&key) {
            self.play_index(position);
        } else if self.queue.add(track) {
            let position = self.queue.len() - 1;
            self.play_index(position);
        }
    }

    fn play_library_entry(&mut self, index: usize) {
        let Some(entry) = self.library.rows.get(index).cloned() else {
            return;
        };
        let track = entry.to_track();
        let key = track.dedupe_key();
        if let Some(position) = self.queue.position_of_key(&key) {
            self.play_index(position);
        } else if self.queue.add(track) {
            let position = self.queue.len() - 1;
            self.play_index(position);
        }
    }

    /// "Add all": query every row matching the current search/sort, not the
    /// visible page. Results arrive asynchronously on the DB worker thread.
    fn queue_all_library(&mut self) {
        self.library.all_id = self.library.all_id.wrapping_add(1);
        self.library.db.query_all(
            self.library.all_id,
            self.library.search.clone(),
            self.library.sort,
        );
    }

    /// Drop one row from the index (the file itself is untouched).
    fn remove_library_entry(&mut self, index: usize) {
        let Some(entry) = self.library.rows.get(index) else {
            return;
        };
        self.library.db.remove_keys(vec![entry.key.clone()]);
    }

    fn queue_library_entry(&mut self, index: usize, next: bool) {
        let Some(entry) = self.library.rows.get(index).cloned() else {
            return;
        };
        let track = entry.to_track();
        let added = if next {
            self.queue.insert_next(track).is_some()
        } else {
            self.queue.add(track)
        };
        if added {
            self.queue_changed();
            self.session_dirty = true;
        }
    }

    fn remove_from_queue(&mut self, index: usize) {
        let was_current = self.queue.current == Some(index);
        if self.queue.remove(index).is_none() {
            return;
        }
        self.disarm_next();
        self.queue_changed();
        self.session_dirty = true;
        if was_current {
            self.engine.stop();
            self.deferred_start = None;
            self.current_track = None;
            self.current_duration = None;
            self.playback_state = PlaybackState::Stopped;
            self.lyrics = None;
            self.lyrics_key.clear();
            self.lyrics_loading = false;
            self.lyrics_error = None;
            self.lyrics_rev = self.lyrics_rev.wrapping_add(1);
        }
    }

    fn add_library_folder(&mut self, path: PathBuf) {
        if !path.is_dir() {
            return;
        }
        if !self.settings.library.folders.contains(&path) {
            self.settings.library.folders.push(path);
            self.settings_dirty = true;
            self.session_dirty = true;
        }
        self.sync_library_watch();
        self.start_library_index();
    }

    fn set_library_extensions(&mut self, text: &str) {
        let mut extensions: Vec<String> = text
            .split([',', '，', ';', '；', ' ', '\t'])
            .map(|value| value.trim().trim_start_matches('.').to_ascii_lowercase())
            .filter(|value| !value.is_empty())
            .collect();
        extensions.sort();
        extensions.dedup();
        if extensions == self.settings.library.extensions {
            return;
        }
        self.settings.library.extensions = extensions;
        self.settings_dirty = true;
        self.sync_library_watch();
        self.start_library_index();
    }

    fn start_library_index(&mut self) {
        if self.library.index_active {
            return;
        }
        // No database worker in --bench/--self-test.
        if !self.library.enabled {
            return;
        }
        let roots = self.settings.library.folders.clone();
        let extensions = library_extensions(&self.settings);
        self.library.index_active = true;
        self.library.index_progress = 0;
        self.library.index_current.clear();
        // An empty root list is still a real index run: the worker prunes
        // every row that is no longer under a configured folder, so "重新扫描"
        // after removing the last folder clears the library instead of
        // leaving the old rows behind.
        self.library
            .db
            .index_roots(roots, extensions, self.settings.library.follow_symlinks);
    }

    fn sync_library_watch(&mut self) {
        let roots = if self.settings.library.auto_watch {
            self.settings.library.folders.clone()
        } else {
            Vec::new()
        };
        let extensions = library_extensions(&self.settings);
        if self.library.watch_roots == roots && self.library.watch_extensions == extensions {
            return;
        }
        self.library.watch_roots = roots.clone();
        self.library.watch_extensions = extensions.clone();
        if roots.is_empty() {
            self.library.watch.stop();
        } else {
            self.library.watch.set_roots(roots, extensions);
        }
    }

    fn poll_library(&mut self) -> bool {
        let mut changed = false;

        for event in self.library.watch.poll() {
            match event {
                WatchEvent::Changed(paths) => {
                    let mut added = Vec::new();
                    let mut removed = Vec::new();
                    for path in paths {
                        if path.is_file() {
                            added.push(path);
                        } else {
                            removed.push(Track::local(path).dedupe_key());
                        }
                    }
                    if !added.is_empty() {
                        self.library.db.index_paths(added);
                    }
                    if !removed.is_empty() {
                        self.library.db.remove_keys(removed);
                    }
                }
                WatchEvent::Error(message) => tracing::warn!("library watch: {message}"),
            }
        }

        if let Some(pending) = self.library.pending_search
            && pending.elapsed() > Duration::from_millis(180)
        {
            self.reset_library_query();
        }

        while let Some(event) = self.library.db.poll() {
            match event {
                DbEvent::QueryDone {
                    request_id,
                    entries,
                    total,
                } => {
                    if request_id != self.library.query_id {
                        continue;
                    }
                    // A play-statistics re-sort can shrink the result set while
                    // a deep page is on screen. Fall back to the last page
                    // instead of showing an empty list.
                    if entries.is_empty() && total > 0 && self.library.offset > 0 {
                        let limit = self.library.limit.max(1);
                        let last_offset = ((total - 1) / limit) * limit;
                        if last_offset != self.library.offset {
                            self.library.offset = last_offset;
                            self.library.rows.clear();
                            self.start_library_query();
                            continue;
                        }
                    }
                    self.library.rows = entries;
                    self.library.total = total;
                    self.library.loading = false;
                    self.library.loaded_once = true;
                    self.library_rev = self.library_rev.wrapping_add(1);
                    changed = true;
                }
                DbEvent::IndexProgress { indexed, current } => {
                    self.library.index_progress = indexed;
                    self.library.index_current = current;
                    self.library_rev = self.library_rev.wrapping_add(1);
                    changed = true;
                }
                DbEvent::IndexDone {
                    indexed,
                    missing,
                    cancelled,
                    ..
                } => {
                    self.library.index_active = false;
                    self.library.index_progress = indexed;
                    self.library.index_current.clear();
                    self.refresh_library();
                    self.library_rev = self.library_rev.wrapping_add(1);
                    if cancelled {
                        self.notify(
                            crate::trf!("索引已取消 · 已收录 {} 首", indexed),
                            false,
                        );
                    } else {
                        // Routine startup indexing must stay quiet. Only tell
                        // the user when the indexed/missing counts changed
                        // since the previous completed run.
                        let previous = (
                            self.session.last_indexed,
                            self.session.last_index_missing,
                        );
                        let current = (Some(indexed), Some(missing));
                        self.session.last_indexed = Some(indexed);
                        self.session.last_index_missing = Some(missing);
                        self.session_dirty = true;
                        // A missing baseline (first run after upgrading) is
                        // adopted silently, so the routine launch shows no
                        // toast at all; later changes still notify.
                        let has_baseline = previous.0.is_some() || previous.1.is_some();
                        if has_baseline && previous != current {
                            let mut text =
                                crate::trf!("索引完成 · 已收录 {} 首", indexed);
                            if missing > 0 {
                                text.push_str(&format!(" · 缺失 {missing} 首"));
                            }
                            self.notify(text, missing > 0);
                        }
                    }
                    changed = true;
                }
                DbEvent::StatsDone(stats) => {
                    self.library.stats = Some(*stats);
                    self.library.stats_loading = false;
                    self.library_rev = self.library_rev.wrapping_add(1);
                    changed = true;
                }
                DbEvent::MaintenanceDone(stats) => {
                    self.library.stats = Some(*stats);
                    self.library.stats_loading = false;
                    self.library_rev = self.library_rev.wrapping_add(1);
                    self.notify(crate::i18n::tr("曲库索引优化完成"), false);
                    changed = true;
                }
                DbEvent::Changed => {
                    // Play statistics / watcher writes: only refresh while the
                    // matching page is visible. Entering a page issues its own
                    // refresh, so a hidden page does not query on every play.
                    if self.library.loaded_once && self.session.page == Page::Library {
                        self.refresh_library();
                    }
                    if self.session.page == Page::Statistics {
                        self.request_statistics();
                    }
                }
                DbEvent::AllDone {
                    request_id,
                    entries,
                } => {
                    if request_id != self.library.all_id {
                        continue;
                    }
                    let tracks: Vec<Track> =
                        entries.iter().map(LibraryEntry::to_track).collect();
                    let added = self.queue.add_many(tracks);
                    if added > 0 {
                        self.queue_changed();
                        self.session_dirty = true;
                        changed = true;
                    }
                    self.notify(
                        crate::trf!("已加入播放队列 · {} 首", added),
                        added == 0,
                    );
                }
                DbEvent::SmartDone { kind, entries } => {
                    if kind == self.library.smart_kind {
                        self.library.smart_rows = entries;
                        self.library.loading = false;
                        self.library_rev = self.library_rev.wrapping_add(1);
                        changed = true;
                    }
                }
                DbEvent::DuplicatesDone(groups) => {
                    self.library.duplicate_groups = groups;
                    self.rebuild_duplicate_rows();
                    self.library.loading = false;
                    self.library_rev = self.library_rev.wrapping_add(1);
                    changed = true;
                }
                DbEvent::MissingDone(entries) => {
                    self.library.missing_checked = Some(entries.len());
                    self.library_rev = self.library_rev.wrapping_add(1);
                    self.notify(
                        if entries.is_empty() {
                            crate::i18n::tr("检查完成 · 没有缺失文件").to_string()
                        } else {
                            crate::trf!("检查完成 · 缺失文件 {} 首", entries.len())
                        },
                        false,
                    );
                    changed = true;
                }
                DbEvent::Error(message) => {
                    tracing::warn!("library db: {message}");
                    self.library.loading = false;
                    self.library.stats_loading = false;
                    self.notify(message, true);
                    changed = true;
                }
            }
        }
        changed
    }

    fn maybe_autosave(&mut self) {
        // Bench/self-test fixtures must never touch the user's settings or
        // session, even when the user toggles player-bar controls meanwhile.
        if self.demo.is_some() {
            return;
        }
        if self.settings_dirty
            && self.last_settings_save.elapsed() > Duration::from_millis(900)
        {
            if let Err(err) = save_settings(&self.paths.settings_file, &self.settings) {
                tracing::warn!("autosave settings failed: {err}");
            } else {
                self.settings_dirty = false;
                self.last_settings_save = Instant::now();
            }
        }
        let interval = if self.is_playing() {
            Duration::from_secs(5)
        } else {
            Duration::from_secs(30)
        };
        if self.session_dirty && self.last_session_save.elapsed() > interval {
            // Only snapshot construction runs here; encoding/writing a large
            // queue happens on the session-writer thread.
            let session = self.build_session();
            if self.session_writer.save(session) {
                self.session_dirty = false;
                self.last_session_save = Instant::now();
            }
        }
        if self.search_cache_dirty
            && self.search_cache_saved.elapsed() > Duration::from_secs(10)
        {
            self.save_search_cache();
        }
    }

    /// Persist pending settings immediately. Called after a discrete settings
    /// toggle so a paused player (whose animation timer is off) does not wait
    /// for the next tick or exit to save.
    pub fn flush_settings(&mut self) {
        if self.demo.is_some() || !self.settings_dirty {
            return;
        }
        match save_settings(&self.paths.settings_file, &self.settings) {
            Ok(()) => {
                self.settings_dirty = false;
                self.last_settings_save = Instant::now();
            }
            Err(err) => tracing::warn!("save settings failed: {err}"),
        }
    }

    pub fn save_now(&mut self) {
        if self.demo.is_some() {
            // Bench fixtures must never overwrite the user's session.
            return;
        }
        // Drain queued background saves before writing the final snapshot;
        // otherwise an older one could land afterwards and win.
        self.session_writer.flush();
        let settings = self.settings.clone();
        if let Err(err) = save_settings(&self.paths.settings_file, &settings) {
            tracing::warn!("save settings failed: {err}");
        } else {
            self.settings_dirty = false;
            self.last_settings_save = Instant::now();
        }

        let session = self.build_session();
        if let Err(err) = save_session(&self.paths.session_file, &session) {
            tracing::warn!("save session failed: {err}");
        } else {
            self.session_dirty = false;
            self.last_session_save = Instant::now();
            self.session = session;
        }
        self.save_search_cache();
    }

    /// Export the whole `Settings` tree as pretty JSON (no session data).
    pub fn export_settings(&self, path: &Path) -> std::io::Result<()> {
        save_settings(path, &self.settings)
    }

    /// Replace all settings from a JSON file. Rejects malformed files instead
    /// of falling back to defaults, so a bad import cannot erase settings.
    pub fn import_settings(&mut self, path: &Path) -> Result<(), String> {
        let settings = load_settings_strict(path)?;
        self.apply_settings(settings);
        Ok(())
    }

    /// Factory defaults for every category.
    pub fn reset_settings(&mut self) {
        self.apply_settings(Settings::default());
    }

    /// Re-apply runtime services after every setting changed at once.
    fn apply_settings(&mut self, settings: Settings) {
        self.settings = settings;
        self.settings_dirty = true;
        self.session_dirty = true;
        self.engine.set_volume(self.settings.playback.volume);
        self.engine.set_muted(self.settings.playback.muted);
        self.engine.set_dsp(dsp_config(&self.settings));
        self.api
            .set_min_interval_ms(self.settings.network.api_min_interval_ms);
        self.downloads
            .set_concurrency(self.settings.download.concurrency.clamp(1, 4) as usize);
        // Folder list/extensions may have changed: restart watching and index
        // whatever is now configured.
        self.sync_library_watch();
        self.start_library_index();
        self.request_lyrics_background();
        self.request_background_image();
        self.flush_settings();
    }

    /// Remember the main window placement (outer rect in DIPs). Bench runs
    /// have a demo clock and must never overwrite the user's session.
    pub fn remember_main_window(
        &mut self,
        pos: Option<(f32, f32)>,
        size: Option<(f32, f32)>,
        maximized: bool,
    ) {
        if self.demo.is_some() {
            return;
        }
        let mut changed = false;
        if let Some(pos) = pos
            && self.session.window_pos != Some(pos)
        {
            self.session.window_pos = Some(pos);
            changed = true;
        }
        if let Some(size) = size
            && self.session.window_size != Some(size)
        {
            self.session.window_size = Some(size);
            changed = true;
        }
        if self.session.window_maximized != maximized {
            self.session.window_maximized = maximized;
            changed = true;
        }
        if changed {
            self.session_dirty = true;
        }
    }

    /// Remember the standalone lyrics window: open flag, display mode and
    /// windowed placement. Persisted with the next session save.
    pub fn remember_lyrics_window(
        &mut self,
        open: bool,
        windowed: bool,
        pos: Option<(f32, f32)>,
        size: Option<(f32, f32)>,
    ) {
        self.session.lyrics_window = open;
        self.session.lyrics_window_windowed = windowed;
        if pos.is_some() {
            self.session.lyrics_window_pos = pos;
        }
        if size.is_some() {
            self.session.lyrics_window_size = size;
        }
        self.session_dirty = true;
    }

    fn build_session(&self) -> Session {
        let shuffle = self.shuffle_snapshot();
        let mut playlist_shuffle = self.session.playlist_shuffle.clone();
        if let (Some(path), Some((keys, cursor, cursor_key))) =
            (self.session.playlist_path.as_ref(), shuffle.as_ref())
        {
            upsert_playlist_shuffle(
                &mut playlist_shuffle,
                path,
                keys.clone(),
                *cursor,
                cursor_key.clone(),
            );
        }
        Session {
            queue: self.queue.items.clone(),
            current_index: self.queue.current,
            // Only the playlist position is remembered (which track), never
            // the offset inside it; the field stays for session-file
            // compatibility and is always written as 0.
            position_secs: 0.0,
            shuffle: self.queue.shuffle,
            repeat: self.queue.repeat,
            volume: self.settings.playback.volume,
            current_key: self.current_track.as_ref().map(Track::dedupe_key),
            shuffle_keys: shuffle
                .as_ref()
                .map(|(keys, _, _)| keys.clone())
                .unwrap_or_default(),
            shuffle_cursor: shuffle.as_ref().map(|(_, cursor, _)| *cursor),
            playlist_shuffle,
            // An empty queue must not remember an auto-refresh playlist, or
            // the next launch would silently restore what the user cleared.
            playlist_path: if self.queue.is_empty() {
                None
            } else {
                self.session.playlist_path.clone()
            },
            saved_at: Some(chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()),
            ..self.session.clone()
        }
    }
}

impl Drop for AppCore {
    fn drop(&mut self) {
        self.save_now();
        self.engine.shutdown();
    }
}

fn clamp_start_secs(duration: Option<f64>, secs: f64) -> f64 {
    let secs = secs.max(0.0);
    if secs < 2.0 {
        return 0.0;
    }
    if let Some(duration) = duration
        && duration > 10.0
        && secs > duration - 3.0
    {
        return 0.0;
    }
    secs
}

fn upsert_playlist_shuffle(
    cache: &mut Vec<PlaylistShuffle>,
    path: &Path,
    keys: Vec<String>,
    cursor: usize,
    cursor_key: Option<String>,
) {
    let key = crate::model::normalize_path(path);
    cache.retain(|entry| crate::model::normalize_path(&entry.path) != key);
    cache.insert(
        0,
        PlaylistShuffle {
            path: path.to_path_buf(),
            keys,
            cursor,
            cursor_key,
        },
    );
    // Keep the most recent few playlists; each entry is just keys + one index.
    cache.truncate(8);
}

pub fn dsp_config(settings: &Settings) -> DspConfig {
    let playback = &settings.playback;
    let features = &settings.features;
    DspConfig {
        eq_enabled: features.equalizer && playback.eq_enabled,
        eq_bands: playback.eq_bands,
        preamp_db: playback.preamp_db,
        replaygain: if features.replaygain {
            playback.replaygain
        } else {
            crate::config::ReplayGainMode::Off
        },
        replaygain_preamp_db: playback.replaygain_preamp_db,
        speed: if features.speed_control {
            playback.speed
        } else {
            1.0
        },
        pitch_preserve: features.speed_control && playback.pitch_preserve,
        crossfade_secs: if features.crossfade {
            playback.crossfade_secs
        } else {
            0.0
        },
        gapless: features.gapless && playback.gapless,
    }
}

/// Drive roots shown when the folder browser has no current directory.
/// `A:`/`B:` are skipped: probing them can block on machines with a floppy.
fn drive_entries() -> Vec<BrowserEntry> {
    let mut entries = Vec::new();
    for letter in b'C'..=b'Z' {
        let root = format!("{}:\\", letter as char);
        let path = PathBuf::from(&root);
        if path.exists() {
            entries.push(BrowserEntry {
                path,
                name: format!("本地磁盘 ({})", &root[..2]),
                is_dir: true,
                is_playlist: false,
                size: None,
            });
        }
    }
    entries
}

fn library_extensions(settings: &Settings) -> Vec<String> {    if settings.library.extensions.is_empty() {
        crate::config::LibrarySettings::default().extensions
    } else {
        settings.library.extensions.clone()
    }
}

fn engine_source_for(track: &Track, settings: &Settings) -> Option<EngineSource> {
    match &track.source {
        TrackSource::Local { path } if path.is_file() => {
            Some(EngineSource::Local(path.clone()))
        }
        TrackSource::Url { url } | TrackSource::Radio { url } => {
            if settings.network.offline {
                return None;
            }
            let http = crate::api::HttpOptions::for_network(&settings.network).for_stream();
            Some(EngineSource::Url {
                url: url.clone(),
                cache: None,
                http,
                download_first: false,
            })
        }
        _ => None,
    }
}

/// Extract a Bamboo song id from a share link. Real pages use `musicid`,
/// `songid` or `mid`; some use `type=song&id=…` and a few put the id in the
/// fragment.
fn bamboo_id_from_url(url: &url::Url) -> Option<String> {
    let mut fallback = None;
    let mut kind = String::new();
    for (key, value) in url.query_pairs() {
        let key = key.to_ascii_lowercase();
        let value = value.trim().to_string();
        if value.is_empty() {
            continue;
        }
        if matches!(key.as_str(), "musicid" | "songid" | "mid") {
            return Some(value);
        }
        if key == "type" {
            kind = value.to_ascii_lowercase();
        } else if key == "id" {
            fallback = Some(value);
        }
    }
    if matches!(kind.as_str(), "music" | "song") {
        return fallback;
    }
    if let Some(fragment) = url.fragment() {
        for part in fragment.split('&') {
            if let Some((key, value)) = part.split_once('=') {
                let key = key.trim().to_ascii_lowercase();
                let value = value.trim();
                if !value.is_empty()
                    && matches!(key.as_str(), "musicid" | "songid" | "mid" | "id")
                {
                    return Some(value.to_string());
                }
            }
        }
    }
    None
}

fn next_quality(current: Option<crate::config::Quality>) -> Option<crate::config::Quality> {    use crate::config::Quality;
    match current {
        None => Some(Quality::K320),
        Some(Quality::K320) => Some(Quality::Flac),
        Some(Quality::Flac) => Some(Quality::K128),
        Some(Quality::K128) => None,
    }
}

/// Recursively delete `*.part` / `*.part.meta` files. Bounded depth keeps a
/// symlink loop from hanging the worker.
fn remove_part_files(root: &Path) -> usize {
    fn walk(dir: &Path, depth: usize) -> usize {
        if depth > 24 {
            return 0;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return 0;
        };
        let mut removed = 0usize;
        let mut directories = Vec::new();
        let mut partials = Vec::new();
        let mut metas = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                directories.push(path);
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            if name.ends_with(".part") {
                partials.push(path);
            } else if name.ends_with(".part.meta") {
                metas.push(path);
            }
        }
        for path in partials {
            if std::fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
        // Only remove metadata whose partial is gone (an active download may
        // still be using it).
        for path in metas {
            if !path.with_extension("part").exists()
                && std::fs::remove_file(&path).is_ok()
            {
                removed += 1;
            }
        }
        for path in directories {
            removed += walk(&path, depth + 1);
        }
        removed
    }
    walk(root, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_paths(root: &Path) -> Paths {
        Paths {
            root: root.to_path_buf(),
            cache: root.join("cache"),
            covers: root.join("covers"),
            lyrics_cache: root.join("lyrics"),
            stream_cache: root.join("stream"),
            logs: root.join("logs"),
            settings_file: root.join("settings.json"),
            session_file: root.join("session.json"),
            library_db: root.join("library.db"),
            portable: true,
        }
    }

    #[test]
    fn demo_clock_advances_and_pauses() {
        let dir = tempfile::tempdir().unwrap();
        let mut core = AppCore::new(temp_paths(dir.path()), None, false);
        core.load_demo(
            crate::demo::core_tracks(4),
            crate::demo::core_lyrics(),
        );
        assert!(core.is_playing());
        std::thread::sleep(Duration::from_millis(30));
        core.tick();
        assert!(core.position_secs() > 0.0);

        core.handle(Command::PlayPause);
        assert!(!core.is_playing());
        let paused = core.position_secs();
        std::thread::sleep(Duration::from_millis(20));
        core.tick();
        assert_eq!(core.position_secs(), paused);

        core.handle(Command::Next);
        assert_eq!(core.snapshot().current_index, Some(1));
    }

    #[test]
    fn session_round_trip_persists_queue() {
        let dir = tempfile::tempdir().unwrap();
        let paths = temp_paths(dir.path());
        let song = dir.path().join("song.mp3");
        std::fs::write(&song, b"not really audio").unwrap();

        {
            let mut core = AppCore::new(paths.clone(), None, false);
            core.handle(Command::AddPaths(vec![song.clone()]));
            assert_eq!(core.snapshot().queue.len(), 1);
            core.save_now();
        }

        let core = AppCore::new(paths, None, false);
        assert_eq!(core.snapshot().queue.len(), 1);
        assert_eq!(core.snapshot().queue[0].title, "song");
    }

    #[test]
    fn session_restores_the_track_but_not_the_position() {
        let dir = tempfile::tempdir().unwrap();
        let paths = temp_paths(dir.path());
        let first = dir.path().join("first.wav");
        let second = dir.path().join("second.wav");
        write_test_wav(&first);
        write_test_wav(&second);

        {
            let mut core = AppCore::new(paths.clone(), None, false);
            core.handle(Command::AddPaths(vec![first, second]));
            core.handle(Command::PlayIndex(1));
            std::thread::sleep(Duration::from_millis(40));
            core.tick();
            core.save_now();
        }

        let mut saved: Session = load_json(&paths.session_file);
        assert_eq!(saved.current_index, Some(1));
        assert!(saved.position_secs.abs() < 0.5, "no per-song offset is saved");
        // Even a session file written by an older build with an offset must
        // restart the restored item at 0:00.
        saved.position_secs = 12.0;
        save_session(&paths.session_file, &saved).unwrap();

        let core = AppCore::new(paths, None, false);
        assert_eq!(core.snapshot().current_index, Some(1));
        assert!(core.snapshot().position_secs.abs() < 0.001);
        assert_eq!(core.deferred_start, Some(0.0));
    }

    #[test]
    fn overlay_frame_rates_are_clamped() {
        let dir = tempfile::tempdir().unwrap();
        let mut core = AppCore::new(temp_paths(dir.path()), None, false);
        core.handle(Command::DesktopLyricsSetFrameRate(500));
        assert_eq!(core.settings.desktop_lyrics.frame_rate, 120);
        core.handle(Command::DesktopLyricsSetFrameRate(1));
        assert_eq!(core.settings.desktop_lyrics.frame_rate, 15);
        core.handle(Command::SetMiniFrameRate(0));
        assert_eq!(core.settings.mini.frame_rate, 15);
        core.handle(Command::SetMiniFrameRate(75));
        assert_eq!(core.settings.mini.frame_rate, 75);
    }

    #[test]
    fn start_offsets_are_clamped_for_explicit_seeks() {
        assert_eq!(clamp_start_secs(Some(300.0), 1.0), 0.0);
        assert_eq!(clamp_start_secs(Some(300.0), 120.0), 120.0);
        assert_eq!(clamp_start_secs(Some(300.0), 299.0), 0.0);
        assert_eq!(clamp_start_secs(None, 8.0), 8.0);
    }

    #[test]
    fn startup_refresh_appends_new_files_after_the_shuffled_cycle() {
        let dir = tempfile::tempdir().unwrap();
        let paths = temp_paths(dir.path());
        let music = dir.path().join("music");
        std::fs::create_dir_all(&music).unwrap();
        write_test_wav(&music.join("a.wav"));
        write_test_wav(&music.join("b.wav"));
        let list = dir.path().join("list.mp3list");
        std::fs::write(&list, "#SEARCH#music\n").unwrap();

        let mut core = AppCore::new(paths, None, false);
        core.import_playlist(&list, false);
        assert_eq!(core.queue.len(), 2);
        core.queue.set_shuffle(true);
        let before: Vec<usize> = core.queue.display_order();
        assert_eq!(before.len(), 2);

        write_test_wav(&music.join("c.wav"));
        assert_eq!(core.refresh_startup_playlist(&list), 1);
        assert_eq!(core.queue.len(), 3);
        let after = core.queue.display_order();
        assert_eq!(
            &after[..2],
            &before[..],
            "the restored cycle keeps its order; new files go last"
        );
        assert_eq!(
            core.queue.items[after[2]].display_title(),
            "c",
            "the newly scanned track is appended"
        );
    }

    #[test]
    fn session_restores_playlist_position_by_key() {
        let dir = tempfile::tempdir().unwrap();
        let paths = temp_paths(dir.path());
        let first = dir.path().join("first.wav");
        let second = dir.path().join("second.wav");
        write_test_wav(&first);
        write_test_wav(&second);
        let second_key = Track::local(second.clone()).dedupe_key();

        {
            let mut core = AppCore::new(paths.clone(), None, false);
            core.handle(Command::AddPaths(vec![first, second]));
            core.handle(Command::PlayIndex(1));
            core.save_now();
        }

        // Simulate a re-imported playlist: the index now points at another
        // track, but the saved key must win.
        let mut session: Session = load_json(&paths.session_file);
        session.queue.swap(0, 1);
        session.current_index = Some(1);
        std::fs::write(&paths.session_file, serde_json::to_vec(&session).unwrap()).unwrap();

        let core = AppCore::new(paths, None, false);
        assert_eq!(core.snapshot().current_index, Some(0));
        assert_eq!(core.snapshot().current.unwrap().dedupe_key(), second_key);
    }

    #[test]
    fn main_window_placement_survives_restart() {
        let dir = tempfile::tempdir().unwrap();
        let paths = temp_paths(dir.path());
        {
            let mut core = AppCore::new(paths.clone(), None, false);
            core.remember_main_window(Some((120.0, 80.0)), Some((1280.0, 760.0)), true);
            core.save_now();
        }
        let core = AppCore::new(paths, None, false);
        assert_eq!(core.session.window_pos, Some((120.0, 80.0)));
        assert_eq!(core.session.window_size, Some((1280.0, 760.0)));
        assert!(core.session.window_maximized);
    }

    #[test]
    fn mp3list_import_replaces_queue_and_remembers_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let paths = temp_paths(dir.path());
        let first = dir.path().join("first.wav");
        let second = dir.path().join("second.wav");
        write_test_wav(&first);
        write_test_wav(&second);
        let list = dir.path().join("mix.mp3list");
        std::fs::write(
            &list,
            format!(
                "#BAMBOO test\n{}\n#SEARCH#{}#type#*.wav\n",
                first.display(),
                dir.path().display()
            ),
        )
        .unwrap();

        let mut core = AppCore::new(paths.clone(), None, false);
        core.handle(Command::AddPaths(vec![list.clone()]));
        // `first` appears both as a file and in the search dir; dedupe keeps
        // the queue unique.
        assert_eq!(core.snapshot().queue.len(), 2);
        assert_eq!(core.snapshot().current_index, Some(0));
        assert!(core.snapshot().position_secs.abs() < 0.001);
        assert_eq!(core.session.playlist_path.as_deref(), Some(list.as_path()));
        core.save_now();

        // A restart re-expands the playlist and keeps the same track at 0:00.
        let core = AppCore::new(paths, None, false);
        assert_eq!(core.snapshot().queue.len(), 2);
        assert_eq!(core.snapshot().current_index, Some(0));
        assert!(core.snapshot().position_secs.abs() < 0.001);
    }

    #[test]
    fn queue_context_commands_move_and_remove_rows() {
        let dir = tempfile::tempdir().unwrap();
        let paths = temp_paths(dir.path());
        let first = dir.path().join("first.wav");
        let second = dir.path().join("second.wav");
        let third = dir.path().join("third.wav");
        write_test_wav(&first);
        write_test_wav(&second);
        write_test_wav(&third);

        let mut core = AppCore::new(paths, None, false);
        core.handle(Command::AddPaths(vec![first, second, third]));
        core.handle(Command::PlayIndex(1));
        assert_eq!(core.snapshot().current_index, Some(1));

        // Removing an earlier row shifts the current index with it.
        core.handle(Command::RemoveFromQueue(0));
        assert_eq!(core.snapshot().queue.len(), 2);
        assert_eq!(core.snapshot().current_index, Some(0));

        // Removing the playing row stops playback and clears the current.
        core.handle(Command::RemoveFromQueue(0));
        assert_eq!(core.snapshot().queue.len(), 1);
        assert!(core.snapshot().current.is_none());
        assert_eq!(core.snapshot().state, PlaybackState::Stopped);
    }

    fn write_test_wav(path: &Path) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 8_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        for index in 0..8_000 {
            writer.write_sample((index % 100) as i16).unwrap();
        }
        writer.finalize().unwrap();
    }

    fn wait_library(core: &mut AppCore) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            core.tick();
            let snapshot = core.snapshot();
            if !snapshot.library_index_active && snapshot.library_total >= 1 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "library index timed out: total={} active={}",
                snapshot.library_total,
                snapshot.library_index_active
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn wait_rows(core: &mut AppCore, expected: usize) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            core.tick();
            let snapshot = core.snapshot();
            if !snapshot.library_loading && snapshot.library.len() == expected {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "library page timed out: rows={} loading={}",
                snapshot.library.len(),
                snapshot.library_loading
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn library_index_search_and_play() {
        let dir = tempfile::tempdir().unwrap();
        let music = dir.path().join("music");
        std::fs::create_dir_all(&music).unwrap();
        write_test_wav(&music.join("Track One.wav"));

        let mut core = AppCore::new(temp_paths(dir.path()), None, true);
        core.handle(Command::LibraryAddFolder(music.clone()));
        wait_library(&mut core);
        assert!(
            core.snapshot()
                .library
                .iter()
                .any(|entry| entry.title.contains("Track One")),
            "indexed rows: {:?}",
            core.snapshot()
                .library
                .iter()
                .map(|entry| entry.title.clone())
                .collect::<Vec<_>>()
        );

        // Search goes through the 180 ms debounce then a new query id.
        core.handle(Command::LibrarySearch("track one".to_string()));
        std::thread::sleep(Duration::from_millis(220));
        core.tick();
        std::thread::sleep(Duration::from_millis(80));
        core.tick();
        assert_eq!(core.snapshot().library.len(), 1);

        core.handle(Command::LibraryPlay(0));
        assert_eq!(core.snapshot().queue.len(), 1);
        assert!(core.snapshot().current.is_some());
    }

    #[test]
    fn library_pages_load_only_the_visible_window() {
        let dir = tempfile::tempdir().unwrap();
        let music = dir.path().join("music");
        std::fs::create_dir_all(&music).unwrap();
        for name in ["A", "B", "C", "D", "E"] {
            write_test_wav(&music.join(format!("{name}.wav")));
        }

        let mut core = AppCore::new(temp_paths(dir.path()), None, true);
        core.handle(Command::SetPage(Page::Library));
        core.handle(Command::LibraryAddFolder(music));
        wait_library(&mut core);
        assert_eq!(core.snapshot().library_total, 5);

        // Only rows 2..3 of the sorted result are returned; the UI never sees
        // or stores a streamed window that a refresh could truncate.
        core.handle(Command::LibraryPage {
            offset: 2,
            limit: 2,
        });
        wait_rows(&mut core, 2);
        let titles: Vec<String> = core
            .snapshot()
            .library
            .iter()
            .map(|entry| entry.title.clone())
            .collect();
        assert_eq!(titles, vec!["C".to_string(), "D".to_string()]);

        // Clicking the first row plays the track from this page.
        core.handle(Command::LibraryPlay(0));
        assert_eq!(core.snapshot().current.unwrap().title, "C");
        std::thread::sleep(Duration::from_millis(80));
        core.tick();
        core.tick();
        assert_eq!(core.snapshot().library.len(), 2);
        assert_eq!(core.snapshot().library[0].title, "C");
    }

    #[test]
    fn library_queue_all_uses_the_whole_result_not_the_page() {
        let dir = tempfile::tempdir().unwrap();
        let music = dir.path().join("music");
        std::fs::create_dir_all(&music).unwrap();
        for name in ["A", "B", "C", "D", "E"] {
            write_test_wav(&music.join(format!("{name}.wav")));
        }

        let mut core = AppCore::new(temp_paths(dir.path()), None, true);
        core.handle(Command::SetPage(Page::Library));
        core.handle(Command::LibraryAddFolder(music));
        wait_library(&mut core);

        // Only two rows are loaded on screen; "add all" must still queue five.
        core.handle(Command::LibraryPage {
            offset: 2,
            limit: 2,
        });
        wait_rows(&mut core, 2);

        core.handle(Command::LibraryQueueAll);
        let deadline = Instant::now() + Duration::from_secs(10);
        while core.snapshot().queue.len() < 5 {
            core.tick();
            assert!(
                Instant::now() < deadline,
                "queue-all timed out: queue={}",
                core.snapshot().queue.len()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut titles: Vec<String> = core
            .snapshot()
            .queue
            .iter()
            .map(|track| track.title.clone())
            .collect();
        titles.sort();
        assert_eq!(
            titles,
            vec![
                "A".to_string(),
                "B".to_string(),
                "C".to_string(),
                "D".to_string(),
                "E".to_string()
            ]
        );
    }

    #[test]
    fn library_missing_check_and_remove_update_the_index() {
        let dir = tempfile::tempdir().unwrap();
        let music = dir.path().join("music");
        std::fs::create_dir_all(&music).unwrap();
        for name in ["A", "B", "C", "D", "E"] {
            write_test_wav(&music.join(format!("{name}.wav")));
        }

        let mut core = AppCore::new(temp_paths(dir.path()), None, true);
        core.handle(Command::SetPage(Page::Library));
        core.handle(Command::LibraryAddFolder(music.clone()));
        wait_library(&mut core);
        assert_eq!(core.snapshot().library_total, 5);

        std::fs::remove_file(music.join("C.wav")).unwrap();
        core.handle(Command::LibraryCheckMissing);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            core.tick();
            let snapshot = core.snapshot();
            if snapshot.library_missing_checked == Some(1)
                && snapshot.library_total == 4
                && !snapshot.library_loading
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "missing check timed out: missing={:?} total={} loading={}",
                snapshot.library_missing_checked,
                snapshot.library_total,
                snapshot.library_loading
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let titles: Vec<String> = core
            .snapshot()
            .library
            .iter()
            .map(|entry| entry.title.clone())
            .collect();
        assert_eq!(titles, vec!["A", "B", "D", "E"]);

        // Remove one row from the index; the page re-queries itself.
        let index = core
            .snapshot()
            .library
            .iter()
            .position(|entry| entry.title == "B")
            .expect("B is listed");
        core.handle(Command::LibraryRemove(index));
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            core.tick();
            let snapshot = core.snapshot();
            if snapshot.library_total == 3 && !snapshot.library_loading {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "remove timed out: total={} loading={}",
                snapshot.library_total,
                snapshot.library_loading
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            core.snapshot()
                .library
                .iter()
                .all(|entry| entry.title != "B")
        );
    }

    #[test]
    fn volume_commands_unmute_and_clamp() {
        let dir = tempfile::tempdir().unwrap();
        let mut core = AppCore::new(temp_paths(dir.path()), None, false);
        core.handle(Command::ToggleMute);
        assert!(core.settings.playback.muted);
        core.handle(Command::SetVolume(0.6));
        assert!(!core.settings.playback.muted);
        assert!((core.settings.playback.volume - 0.6).abs() < 1e-6);
        core.handle(Command::SetVolume(9.0));
        assert!((core.settings.playback.volume - 1.0).abs() < 1e-6);
    }

    fn wait_online(core: &mut AppCore) {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            core.tick();
            if !core.online.loading() {
                break;
            }
            assert!(Instant::now() < deadline, "online request timed out");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Manual smoke test against the local Bamboo API: album search rows carry
    /// album ids, so clicking one must open the album instead of streaming it.
    #[test]
    fn live_album_search_opens_album_instead_of_playing() {
        let Ok(base) = std::env::var("BAMBOO_LIVE_API") else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let mut core = AppCore::new(temp_paths(dir.path()), None, false);
        core.settings.sources[0].base_url = base;
        core.settings.sources[0].enabled = true;
        core.settings.network.offline = false;

        core.handle(Command::OnlineSearch {
            query: "1".to_string(),
            offset: 0,
            limit: 5,
        });
        wait_online(&mut core);
        core.handle(Command::OnlineSetKind(SearchKind::Album));
        wait_online(&mut core);
        assert!(
            !core.online.songs.is_empty(),
            "album search returned no rows: {:?}",
            core.online.error
        );

        core.handle(Command::OnlinePlay(0));
        assert!(core.online.detail.is_some(), "album row must open detail");
        assert!(core.current_track.is_none(), "album id must not be played");
        wait_online(&mut core);
        let detail = core.online.detail.as_ref().unwrap();
        assert!(!detail.items.is_empty(), "album detail has no songs");
        assert!(!detail.title.trim().is_empty());
    }
}

#[cfg(test)]
mod settings_tests {
    use super::*;

    fn temp_paths(root: &Path) -> Paths {
        Paths {
            root: root.to_path_buf(),
            cache: root.join("cache"),
            covers: root.join("covers"),
            lyrics_cache: root.join("lyrics"),
            stream_cache: root.join("stream"),
            logs: root.join("logs"),
            settings_file: root.join("settings.json"),
            session_file: root.join("session.json"),
            library_db: root.join("library.db"),
            portable: true,
        }
    }

    #[test]
    fn sleep_timer_pauses_and_clears_itself() {
        let dir = tempfile::tempdir().unwrap();
        let mut core = AppCore::new(temp_paths(dir.path()), None, false);
        core.load_demo(
            crate::demo::core_tracks(2),
            crate::demo::core_lyrics(),
        );
        core.handle(Command::SetSleepTimer(Some(15)));
        assert_eq!(core.sleep_selected_minutes(), Some(15));
        // Force the deadline into the past; the next tick pauses playback.
        core.sleep_deadline = Some(Instant::now() - Duration::from_millis(1));
        core.tick();
        assert!(!core.is_playing());
        assert_eq!(core.sleep_selected_minutes(), None);
    }

    #[test]
    fn flush_settings_persists_discrete_toggles() {
        let dir = tempfile::tempdir().unwrap();
        let paths = temp_paths(dir.path());
        let settings_file = paths.settings_file.clone();
        let mut core = AppCore::new(paths, None, false);
        core.handle(Command::SetGapless(false));
        core.flush_settings();
        let restored: Settings = load_json(&settings_file);
        assert!(!restored.playback.gapless);
    }

    #[test]
    fn frame_rate_clamp() {
        let dir = tempfile::tempdir().unwrap();
        let mut core = AppCore::new(temp_paths(dir.path()), None, false);
        core.handle(Command::SetFrameRate(1000));
        assert_eq!(core.settings.performance.frame_rate, 120);
        core.handle(Command::SetFrameRate(1));
        assert_eq!(core.settings.performance.frame_rate, 15);
    }

    #[test]
    fn clearing_the_queue_detaches_the_playlist() {
        let dir = tempfile::tempdir().unwrap();
        let mut core = AppCore::new(temp_paths(dir.path()), None, false);
        core.session.playlist_path = Some(std::path::PathBuf::from("E:/playlist/ALL.mp3list"));
        core.handle(Command::AddPaths(vec![std::path::PathBuf::from(
            dir.path().join("song.mp3"),
        )]));
        core.handle(Command::ClearQueue);
        assert!(core.queue.is_empty());
        assert!(core.session.playlist_path.is_none());
    }

    #[test]
    fn empty_shortcut_binding_is_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let mut core = AppCore::new(temp_paths(dir.path()), None, false);
        core.handle(Command::SetShortcut(
            crate::config::ShortcutSlot::PlayPause,
            "  ".to_string(),
        ));
        assert!(core.settings.shortcuts.play_pause.is_empty());
        core.handle(Command::SetShortcut(
            crate::config::ShortcutSlot::PlayPause,
            "Ctrl+Alt+P".to_string(),
        ));
        assert_eq!(core.settings.shortcuts.play_pause, "Ctrl+Alt+P");
    }

    #[test]
    fn settings_export_import_and_reset_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let mut core = AppCore::new(temp_paths(dir.path()), None, false);

        core.handle(Command::SetThemeMode(ThemeMode::Light));
        core.handle(Command::SetAccent([10, 200, 30]));
        core.handle(Command::SetListDensity(ListDensity::Compact));
        let export = dir.path().join("export.json");
        core.export_settings(&export).unwrap();

        core.handle(Command::SetThemeMode(ThemeMode::Dark));
        core.handle(Command::SetAccent([1, 2, 3]));
        core.import_settings(&export).unwrap();
        assert_eq!(core.settings.theme.mode, ThemeMode::Light);
        assert_eq!(core.settings.theme.accent, [10, 200, 30]);
        assert_eq!(core.settings.theme.list_density, ListDensity::Compact);

        // A malformed import is rejected and leaves settings untouched.
        let bad = dir.path().join("bad.json");
        std::fs::write(&bad, "{not json").unwrap();
        assert!(core.import_settings(&bad).is_err());
        assert_eq!(core.settings.theme.accent, [10, 200, 30]);

        core.reset_settings();
        let defaults = Settings::default();
        assert_eq!(core.settings.theme.mode, defaults.theme.mode);
        assert_eq!(core.settings.theme.accent, defaults.theme.accent);
        assert_eq!(core.settings.theme.list_density, defaults.theme.list_density);
    }
}

#[cfg(test)]
mod playlist_shuffle_tests {
    use super::*;

    fn paths(root: &Path) -> Paths {
        Paths {
            root: root.to_path_buf(),
            cache: root.join("cache"),
            covers: root.join("covers"),
            lyrics_cache: root.join("lyrics"),
            stream_cache: root.join("stream"),
            logs: root.join("logs"),
            settings_file: root.join("settings.json"),
            session_file: root.join("session.json"),
            library_db: root.join("library.db"),
            portable: true,
        }
    }

    #[test]
    fn reopening_playlist_keeps_shuffle_progress_and_appends_new_files() {
        let dir = tempfile::tempdir().unwrap();
        let music = dir.path().join("music");
        std::fs::create_dir_all(&music).unwrap();
        for name in ["a.mp3", "b.mp3"] {
            std::fs::write(music.join(name), b"").unwrap();
        }
        let list = dir.path().join("list.mp3list");
        std::fs::write(&list, format!("#SEARCH#{}\n", music.display())).unwrap();

        let mut core = AppCore::new(paths(dir.path()), None, false);
        core.handle(Command::ToggleShuffle);
        assert!(core.queue.shuffle);
        core.import_playlist(&list, false);
        assert_eq!(core.queue.len(), 2);
        let original: Vec<String> = core
            .queue
            .shuffle_order
            .iter()
            .filter_map(|index| core.queue.items.get(*index))
            .map(Track::dedupe_key)
            .collect();
        // Play the second entry of the shuffled cycle.
        let second = core.queue.shuffle_order[1];
        core.restore_paused(second, 0.0);
        let progress = core.current_track.as_ref().unwrap().dedupe_key();

        std::fs::write(music.join("c.mp3"), b"").unwrap();
        // A re-import rebuilds the queue from disk (3 tracks) but keeps the
        // previous shuffled order and appends the new file at the end.
        assert_eq!(core.import_playlist(&list, false), 3);
        assert_eq!(core.queue.len(), 3);
        assert_eq!(
            core.current_track.as_ref().map(Track::dedupe_key),
            Some(progress.clone())
        );
        let updated: Vec<String> = core
            .queue
            .shuffle_order
            .iter()
            .filter_map(|index| core.queue.items.get(*index))
            .map(Track::dedupe_key)
            .collect();
        let kept: Vec<String> = updated
            .iter()
            .filter(|key| original.contains(key))
            .cloned()
            .collect();
        assert_eq!(kept, original, "existing shuffled order must be preserved");
        let new_key = crate::model::normalize_path(&music.join("c.mp3"));
        assert_eq!(updated.last(), Some(&new_key), "new files go to the end");
    }

    #[test]
    fn eq_band_edits_become_custom_and_presets_reset_bands() {
        let dir = tempfile::tempdir().unwrap();
        let mut core = AppCore::new(paths(dir.path()), None, false);

        core.handle(Command::SetEqBand(3, 30.0));
        assert!((core.settings.playback.eq_bands[3] - 24.0).abs() < 1e-6);
        assert_eq!(core.settings.playback.eq_preset, EqPreset::Custom);
        assert!(core.settings.playback.eq_enabled);

        core.handle(Command::SetEqBand(3, -30.0));
        assert!((core.settings.playback.eq_bands[3] + 24.0).abs() < 1e-6);

        core.handle(Command::SetEqPreset(EqPreset::Rock));
        assert_eq!(core.settings.playback.eq_bands, EqPreset::Rock.bands());

        // Selecting "custom" must keep the hand-edited gains.
        core.handle(Command::SetEqBand(0, 1.5));
        core.handle(Command::SetEqPreset(EqPreset::Custom));
        assert!((core.settings.playback.eq_bands[0] - 1.5).abs() < 1e-6);
    }

    #[test]
    fn lyrics_color_commands_store_rgb_and_keep_desktop_alpha() {
        let dir = tempfile::tempdir().unwrap();
        let mut core = AppCore::new(paths(dir.path()), None, false);

        core.handle(Command::SetLyricsHighlight([10, 20, 30]));
        core.handle(Command::SetLyricsCurrentColor([40, 50, 60]));
        core.handle(Command::SetLargeLyricsNormalColor([70, 80, 90]));
        assert_eq!(core.settings.lyrics.highlight_color, [10, 20, 30]);
        assert_eq!(core.settings.lyrics.current_color, [40, 50, 60]);
        assert_eq!(core.settings.large_lyrics.normal_color, [70, 80, 90]);

        let before = core.settings.desktop_lyrics.outline_color;
        core.handle(Command::SetDesktopLyricsColor(5, [1, 2, 3]));
        let after = core.settings.desktop_lyrics.outline_color;
        assert_eq!([after[0], after[1], after[2]], [1, 2, 3]);
        assert_eq!(after[3], before[3], "alpha must be preserved");
    }

    #[test]
    fn font_size_commands_accept_the_widened_range() {
        let dir = tempfile::tempdir().unwrap();
        let mut core = AppCore::new(paths(dir.path()), None, false);

        core.handle(Command::SetLyricsFontSize(500.0));
        assert!((core.settings.lyrics.font_size - 120.0).abs() < 1e-6);
        core.handle(Command::SetLyricsFontSize(1.0));
        assert!((core.settings.lyrics.font_size - 8.0).abs() < 1e-6);

        core.handle(Command::SetLargeLyricsFontSize(500.0));
        assert!((core.settings.large_lyrics.font_size - 200.0).abs() < 1e-6);

        core.handle(Command::DesktopLyricsSetFontSize(500.0));
        assert!((core.settings.desktop_lyrics.font_size - 200.0).abs() < 1e-6);

        core.handle(Command::SetMiniFontSize(500.0));
        assert!((core.settings.mini.lyrics_font_size - 64.0).abs() < 1e-6);
    }

    #[test]
    fn folder_browser_lists_entries_and_enqueues_audio() {
        let dir = tempfile::tempdir().unwrap();
        let music = dir.path().join("music");
        std::fs::create_dir_all(music.join("sub")).unwrap();
        for name in ["a.wav", "b.wav"] {
            // The browser only filters by extension; no valid audio needed.
            std::fs::write(music.join(name), b"").unwrap();
        }
        let mut core = AppCore::new(paths(dir.path()), None, false);
        core.handle(Command::BrowseTo(Some(music)));

        let mut tries = 0;
        while core.snapshot().browser.loading && tries < 400 {
            core.tick();
            std::thread::sleep(Duration::from_millis(5));
            tries += 1;
        }
        let index = {
            let snapshot = core.snapshot();
            assert!(!snapshot.browser.loading);
            assert!(
                snapshot
                    .browser
                    .entries
                    .iter()
                    .any(|entry| entry.is_dir && entry.name == "sub")
            );
            snapshot
                .browser
                .entries
                .iter()
                .position(|entry| entry.name == "a.wav")
                .expect("a.wav listed")
        };
        core.handle(Command::BrowseEnqueue(index));
        assert_eq!(core.snapshot().queue.len(), 1);
        assert_eq!(core.snapshot().queue[0].title, "a");
    }

    #[test]
    fn folder_browser_history_goes_back_and_forward() {
        fn wait(core: &mut AppCore) {
            let mut tries = 0;
            while core.snapshot().browser.loading && tries < 400 {
                core.tick();
                std::thread::sleep(Duration::from_millis(5));
                tries += 1;
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = a.join("b");
        std::fs::create_dir_all(&b).unwrap();
        let mut core = AppCore::new(paths(dir.path()), None, false);

        core.handle(Command::BrowseTo(Some(a.clone())));
        wait(&mut core);
        core.handle(Command::BrowseTo(Some(b.clone())));
        wait(&mut core);

        core.handle(Command::BrowseBack);
        wait(&mut core);
        assert_eq!(
            core.snapshot().browser.current.as_deref(),
            Some(a.as_path())
        );
        assert_eq!(core.snapshot().browser.forward.len(), 1);

        core.handle(Command::BrowseForward);
        wait(&mut core);
        assert_eq!(
            core.snapshot().browser.current.as_deref(),
            Some(b.as_path())
        );
        assert!(core.snapshot().browser.forward.is_empty());

        // A fresh navigation after going back drops the forward stack.
        core.handle(Command::BrowseBack);
        wait(&mut core);
        core.handle(Command::BrowseTo(Some(dir.path().to_path_buf())));
        wait(&mut core);
        assert!(core.snapshot().browser.forward.is_empty());
        assert_eq!(
            core.snapshot().browser.current.as_deref(),
            Some(dir.path())
        );

        // Refreshing the same location is not a history entry.
        let depth = core.snapshot().browser.back.len();
        core.handle(Command::BrowseTo(Some(dir.path().to_path_buf())));
        wait(&mut core);
        assert_eq!(core.snapshot().browser.back.len(), depth);
    }

    #[test]
    fn desktop_background_extras_fill_the_gaps() {        let dir = tempfile::tempdir().unwrap();
        let mut core = AppCore::new(paths(dir.path()), None, false);

        let image = dir.path().join("backdrop.png");
        core.handle(Command::SetDesktopLyricsBackgroundImage(Some(image.clone())));
        assert_eq!(
            core.settings.desktop_lyrics.background,
            crate::config::DesktopBackgroundMode::Image
        );
        assert_eq!(core.settings.desktop_lyrics.bg_image, Some(image));

        let alpha = core.settings.desktop_lyrics.bg_color[3];
        core.handle(Command::SetDesktopLyricsColor(7, [9, 8, 7]));
        assert_eq!(
            core.settings.desktop_lyrics.bg_color,
            [9, 8, 7, alpha]
        );

        core.handle(Command::SetDesktopLyricsOutlineWidth(99.0));
        assert!((core.settings.desktop_lyrics.outline_width - 20.0).abs() < 1e-6);
        core.handle(Command::SetDesktopLyricsShadowOffset(-5.0));
        assert!((core.settings.desktop_lyrics.shadow_offset - 0.0).abs() < 1e-6);

        core.handle(Command::SetDesktopLyricsFontFamily(Some("Segoe UI".into())));
        assert_eq!(
            core.settings.desktop_lyrics.font_family.as_deref(),
            Some("Segoe UI")
        );
    }

}

#[cfg(test)]
mod download_cleanup_tests {
    use super::*;

    #[test]
    fn removes_partials_then_orphaned_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let nested = root.join("Artist").join("Album");
        std::fs::create_dir_all(&nested).unwrap();
        let part = nested.join("song.mp3.part");
        let meta = nested.join("song.mp3.part.meta");
        let orphan = nested.join("old.mp3.part.meta");
        let keep = nested.join("song.mp3");
        std::fs::write(&part, b"x").unwrap();
        std::fs::write(&meta, b"{}").unwrap();
        std::fs::write(&orphan, b"{}").unwrap();
        std::fs::write(&keep, b"x").unwrap();

        let removed = remove_part_files(root);
        assert_eq!(removed, 3, "part + its meta + the orphaned meta");
        assert!(!part.exists());
        assert!(!meta.exists());
        assert!(!orphan.exists());
        assert!(keep.exists(), "completed downloads are never touched");
    }

    #[test]
    fn quality_cycle_wraps_back_to_follow_source() {
        use crate::config::Quality;
        assert_eq!(next_quality(None), Some(Quality::K320));
        assert_eq!(next_quality(Some(Quality::K320)), Some(Quality::Flac));
        assert_eq!(next_quality(Some(Quality::Flac)), Some(Quality::K128));
        assert_eq!(next_quality(Some(Quality::K128)), None);
    }

    #[test]
    fn extracts_song_ids_from_share_links() {
        let cases = [
            ("https://site/song?musicid=101", Some("101")),
            ("https://site/song?songid=202&type=music", Some("202")),
            ("https://site/song?type=song&id=303", Some("303")),
            ("https://site/song#musicid=404", Some("404")),
            ("https://site/list?id=505", None),
        ];
        for (url, expected) in cases {
            let parsed = url::Url::parse(url).unwrap();
            assert_eq!(
                bamboo_id_from_url(&parsed).as_deref(),
                expected,
                "{url}"
            );
        }
    }
}

#[cfg(test)]
mod lyrics_panel_tests {
    use super::*;

    fn temp_paths(root: &Path) -> Paths {
        Paths {
            root: root.to_path_buf(),
            cache: root.join("cache"),
            covers: root.join("covers"),
            lyrics_cache: root.join("lyrics"),
            stream_cache: root.join("stream"),
            logs: root.join("logs"),
            settings_file: root.join("settings.json"),
            session_file: root.join("session.json"),
            library_db: root.join("library.db"),
            portable: true,
        }
    }

    #[test]
    fn panel_background_commands_round_trip_and_reset() {
        let dir = tempfile::tempdir().unwrap();
        let mut core = AppCore::new(temp_paths(dir.path()), None, false);
        core.handle(Command::SetLyricsPanelBackground(
            LyricsPanelBackground::Gradient,
        ));
        core.handle(Command::SetLyricsPanelColor2([1, 2, 3]));
        core.handle(Command::SetLyricsPanelBorderColor(Some([4, 5, 6])));
        assert_eq!(
            core.settings.lyrics.panel_background,
            LyricsPanelBackground::Gradient
        );
        assert_eq!(core.settings.lyrics.panel_color2, [1, 2, 3]);
        assert_eq!(core.settings.lyrics.panel_border_color, Some([4, 5, 6]));

        core.handle(Command::ResetLyricsPanelBackground);
        assert_eq!(
            core.settings.lyrics.panel_background,
            LyricsPanelBackground::Theme
        );
        assert_eq!(core.settings.lyrics.panel_border_color, None);
        assert_eq!(core.settings.lyrics.panel_image, None);
        assert!(core.lyrics_background.is_none());
    }

    #[test]
    fn image_mode_decodes_the_panel_picture_off_thread() {
        let dir = tempfile::tempdir().unwrap();
        let picture = dir.path().join("panel.png");
        let pixels = vec![80u8; 64 * 32 * 4];
        image::save_buffer(
            &picture,
            &pixels,
            64,
            32,
            image::ColorType::Rgba8,
        )
        .unwrap();
        let mut core = AppCore::new(temp_paths(dir.path()), None, false);
        core.handle(Command::SetLyricsPanelImage(Some(picture.clone())));
        core.handle(Command::SetLyricsPanelBackground(
            LyricsPanelBackground::Image,
        ));
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            core.tick();
            if core.lyrics_background.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let image = core.lyrics_background.as_ref().expect("decoded picture");
        assert_eq!(image.size, [64, 32]);
    }

    #[test]
    fn source_management_cycles_adds_and_removes() {
        let dir = tempfile::tempdir().unwrap();
        let mut core = AppCore::new(temp_paths(dir.path()), None, false);
        let initial = core.settings.sources.len();
        core.handle(Command::CyclePrimarySource);
        let first = core
            .settings
            .active_source
            .clone()
            .expect("cycle selects a source");
        assert!(
            core.settings
                .sources
                .iter()
                .any(|source| source.id == first)
        );

        core.handle(Command::AddSource);
        assert_eq!(core.settings.sources.len(), initial + 1);
        let added = core.settings.active_source.clone().unwrap();
        assert_ne!(added, first);

        core.handle(Command::RemovePrimarySource);
        assert_eq!(core.settings.sources.len(), initial);
        assert_ne!(core.settings.active_source.as_deref(), Some(added.as_str()));

        while core.settings.sources.len() > 1 {
            core.handle(Command::RemovePrimarySource);
        }
        core.handle(Command::RemovePrimarySource);
        assert_eq!(core.settings.sources.len(), 1, "the last source is kept");
    }
}
