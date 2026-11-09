//! Application state, layout shell, playback orchestration and persistence.

mod fonts;
mod gpu_bg;
mod theme;
pub mod views;
pub mod widgets;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::{Align2, Color32, Context, Id, Key, Modifiers, Order, RichText, ViewportCommand, vec2};
use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;

use crate::api::{ApiEvent, ApiService, ApiSong, HttpOptions, PreparedPlay, SearchCache, SearchKind};
use crate::audio::{AudioEngine, DspConfig, EngineEvent, EngineSource, OutputInfo, PlaybackState};
use crate::config::{
    CloseBehavior, Language, Paths, Quality, RecentFile, Session, Settings, load_settings,
    save_session, save_settings,
};
use crate::download::{DownloadEvent, DownloadService, DownloadState};
use crate::i18n::t;
use crate::library::{
    BrowseService, DbEvent, DbTrack, DuplicateGroup, FolderBrowser, LibraryDb, LibraryEntry,
    LibraryQuery, LibrarySort, LibraryStats, ScanEvent, ScanRoot, ScanService, SmartKind,
    WatchEvent, WatchService,
};
use crate::lyrics::{Lyrics, LyricsService};
use crate::model::{Page, Track, TrackSource, normalize_path};
use crate::platform::tray::{Tray, TrayAction};
use crate::platform::window;
use crate::playlist::Queue;
use crate::util::metadata::{LocalMeta, MetadataService};
use crate::util::{format_time, is_audio_file, open_in_explorer};
use theme::ThemeColors;

/// Work posted by a background thread and applied on the UI thread from
/// `logic` (used for native file dialogs, which must not run inside a paint
/// pass).
type AppTask = Box<dyn FnOnce(&mut App) + Send>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Success,
    Error,
}

#[derive(Clone, Debug)]
pub struct Toast {
    pub text: String,
    pub kind: ToastKind,
    pub created: Instant,
}

/// One entry in the downloads tab. The background service only reports
/// progress/state; the track and source stay here so retry can rebuild a job.
#[derive(Clone, Debug)]
pub struct DownloadItem {
    pub id: u64,
    pub track: Track,
    pub source_id: Option<String>,
    /// Per-download quality override (used by retry).
    pub quality: Option<Quality>,
    pub state: DownloadState,
    pub received: u64,
    pub total: Option<u64>,
    pub path: Option<PathBuf>,
    pub error: Option<String>,
    pub created: Instant,
}

/// Commands available from the Ctrl+K palette. Kept copy-only so the UI can
/// render a filtered list and execute after the frame borrow ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PaletteAction {
    Page(Page),
    Settings(views::SettingsTab),
    PlayPause,
    Next,
    Prev,
    ToggleShuffle,
    CycleRepeat,
    ToggleMute,
    VolumeUp,
    VolumeDown,
    ToggleLyrics,
    ToggleDesktopLyrics,
    ToggleFullscreenLyrics,
    ToggleLyricsWindow,
    EditLyrics,
    ToggleVisualizer,
    ToggleStation,
    ToggleMiniPlayer,
    OpenFiles,
    OpenFolder,
    ImportPlaylist,
    OpenUrl,
    LibraryIndex,
    OpenDownloads,
}

/// Human-readable page names shared by the command palette and tooltips.
fn page_label(page: Page) -> &'static str {
    match page {
        Page::NowPlaying => crate::i18n::tr("正在播放"),
        Page::Library => crate::i18n::tr("曲库"),
        Page::Playlists => crate::i18n::tr("队列与歌单"),
        Page::Search => crate::i18n::tr("在线搜索"),
        Page::Discover => crate::i18n::tr("网络电台"),
        Page::Settings => crate::i18n::tr("设置"),
    }
}

/// Fresh seed for BambooMusic's random station. The local API combines it with
/// the current date and returns deterministic batches for that seed.
fn station_seed() -> u64 {
    (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        % 10_000) as u64
}

/// Shared 2048-point FFT plan for the visualizer. Planning is cached because
/// the UI asks for a new spectrum every frame.
fn visualizer_fft() -> &'static std::sync::Arc<dyn rustfft::Fft<f32>> {
    static PLAN: std::sync::OnceLock<std::sync::Arc<dyn rustfft::Fft<f32>>> =
        std::sync::OnceLock::new();
    PLAN.get_or_init(|| {
        let mut planner = rustfft::FftPlanner::<f32>::new();
        planner.plan_fft_forward(2048)
    })
}

/// Translate persisted playback settings into the engine's live DSP config.
/// Feature toggles act as master switches: disabling a module immediately
/// neutralizes its effect without losing the user's values.
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

#[derive(Clone, Debug)]
pub struct ResumeIntent {
    pub key: String,
    pub position: f64,
    pub was_playing: bool,
    /// Saved shuffle cycle order (track keys).
    pub shuffle_keys: Vec<String>,
}

/// In-progress rename dialog for a saved playlist file.
#[derive(Clone, Debug)]
pub struct PlaylistRename {
    pub path: PathBuf,
    pub name: String,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct AlbumView {
    pub id: String,
    pub name: String,
    pub songs: Vec<ApiSong>,
    pub total: u64,
    pub page: u32,
    pub loading: bool,
    pub error: Option<String>,
    pub request_id: u64,
    /// The page is served from the persistent detail cache.
    pub cached: bool,
}

#[derive(Clone, Debug, Default)]
pub struct ArtistView {
    pub id: String,
    pub name: String,
    pub songs: Vec<ApiSong>,
    pub total: u64,
    pub page: u32,
    pub loading: bool,
    pub error: Option<String>,
    pub request_id: u64,
    /// The page is served from the persistent detail cache.
    pub cached: bool,
}

/// Backoff bookkeeping for a failed online thumbnail. Cover hosts can be slow
/// or return a transient error; retrying a few times hides that without letting
/// a permanently broken URL hammer the network on every frame.
pub struct ThumbRetry {
    pub attempts: u32,
    pub next_at: Instant,
}

const THUMB_MAX_ATTEMPTS: u32 = 3;
/// Which overlay windows are currently open. The heartbeat thread uses this to
/// also request repaints of the overlay viewports: immediate viewports are
/// rendered inside the root pass, and when the main window is hidden eframe
/// throttles root repaints to ~10 fps, while a repaint request on the (visible)
/// overlay window still wakes the root immediately.
#[derive(Default)]
pub struct OverlayActivity {
    pub lyrics_window: AtomicBool,
    pub mini_player: AtomicBool,
}

/// How close to the end of the current track we start resolving the next
/// Bamboo URL. Prefetching at track start doubled API traffic for every song
/// the user might skip; 35 s still leaves room for a slow source.
const NEXT_PREFETCH_WINDOW_SECS: f64 = 35.0;
/// Cached/auto-advanced online tracks refresh `info` at most this often.
const ONLINE_INFO_TTL: Duration = Duration::from_secs(600);

pub struct App {
    pub settings: Settings,
    pub session: Session,
    pub paths: Paths,
    pub lang: Language,
    pub page: Page,
    pub engine: AudioEngine,
    pub queue: Queue,
    pub current_track: Option<Track>,
    pub current_duration: Option<f64>,
    pub cover: Option<egui::TextureHandle>,
    /// Artwork key/encoded PNG mirrored to the Windows media flyout.
    pub smtc_cover_key: String,
    pub smtc_cover_png: Option<Vec<u8>>,
    /// Bumped when the same track's artwork arrives/clears so SMTC refreshes
    /// its thumbnail even though the track key did not change.
    smtc_cover_rev: u64,
    pub background: Option<egui::TextureHandle>,
    /// GPU background pipeline state (creates the wgpu paint callback).
    pub gpu_bg: gpu_bg::GpuBackground,
    /// True when the DWM Mica system backdrop was successfully applied.
    pub mica_active: bool,
    /// Custom background image; the `None` texture caches a failed decode so
    /// a broken file is not re-read every frame.
    pub custom_background: Option<(PathBuf, Option<egui::TextureHandle>)>,
    pub placeholder_cover: Option<([u8; 3], egui::TextureHandle)>,
    pub cover_key: String,
    pub toasts: VecDeque<Toast>,
    pub hwnd: Option<isize>,
    pub tray: Option<Tray>,
    pub tray_failed: bool,
    pub smtc: Option<crate::platform::smtc::SmtcController>,
    pub tray_state_key: String,
    tray_quit: Arc<AtomicBool>,
    pub window_hidden: bool,
    pub allow_close: bool,
    pub session_dirty: bool,
    pub settings_dirty: bool,
    pub last_session_save: Instant,
    pub last_settings_save: Instant,
    pub seek_preview: Option<f64>,
    pub playback_state: PlaybackState,
    pub device_info: OutputInfo,
    pub ctx: Context,
    pub metadata: MetadataService,
    pub cover_service: crate::util::cover::CoverService,
    pub cover_loading: bool,
    /// Offline ReplayGain analysis worker and its persisted cache.
    pub loudness: crate::audio::loudness::LoudnessService,
    replaygain_cache: HashMap<String, crate::audio::loudness::CachedGain>,
    replaygain_scanning: HashSet<PathBuf>,
    replaygain_tagged: HashSet<PathBuf>,
    replaygain_cache_dirty: bool,
    last_replaygain_save: Instant,
    pub lyrics_service: LyricsService,
    pub lyrics: Option<Lyrics>,
    pub lyrics_loading: bool,
    pub lyrics_key: String,
    pub lyrics_error: Option<String>,
    /// Keys already requested by the next-track lyrics prefetch.
    lyrics_prefetched: HashSet<String>,
    /// Loaded lyrics for upcoming tracks, consumed by `refresh_lyrics`.
    lyrics_prefetch_cache: HashMap<String, (Option<Lyrics>, Option<String>)>,
    lyrics_prefetch_order: VecDeque<String>,
    /// Bumped whenever the parsed lyrics object is replaced, so the desktop
    /// window can pick up new lyrics without cloning them every frame.
    pub lyrics_version: u64,
    pub lyrics_manual_scroll_until: Option<Instant>,
    /// Immersive (sidebar-less) lyrics page; can be windowed or fullscreen.
    pub fullscreen_lyrics: bool,
    pub lyrics_os_fullscreen: bool,
    /// Window rect saved when fullscreen lyrics moved to another display.
    lyrics_return_rect: Option<(i32, i32, i32, i32)>,
    /// Deferred restore, applied after winit has processed the fullscreen exit
    /// (winit restores its own saved rect during that transition).
    pending_window_restore: Option<((i32, i32, i32, i32), Instant)>,
    /// P10 lyrics editor. The document is kept while closed so an accidental
    /// close does not discard edits.
    pub lyrics_editor: Option<crate::lyrics::LyricsEditor>,
    pub lyrics_editor_open: bool,
    /// Start position of a restored-but-not-yet-opened track. While this is
    /// set the engine has no decoder and no audio device was created.
    pub deferred_start: Option<f64>,
    pub last_window_rect: Option<(f32, f32, f32, f32)>,
    pub window_maximized: bool,
    pub last_title: String,
    pub last_font_config: (Option<String>, u8, Option<String>, Option<String>),
    pub startup_args: Vec<PathBuf>,
    /// Flag commands from our own command line, applied on the first frame.
    startup_commands: Vec<String>,
    /// Commands/files forwarded by a second instance (jump list, open with).
    pub ipc_rx: crossbeam_channel::Receiver<Vec<String>>,
    jump_list_dirty: bool,
    jump_list_built: bool,
    jump_list_enabled: bool,
    jump_list_retry_at: Option<Instant>,
    pub restore_notice_pending: bool,
    pub show_lyrics_sidebar: bool,
    pub lyrics_sidebar_width: f32,
    pub settings_tab: views::SettingsTab,
    pub memory_info: Option<crate::platform::memory::MemoryInfo>,
    pub last_memory_sample: Instant,
    pub last_metadata_poll: Instant,
    pub last_trim: Instant,
    pub gpu_info: String,
    pub style_reapplied: bool,
    pub style_reapply_at: Instant,
    pub was_focused: bool,
    pub scan: ScanService,
    pub scan_id: u64,
    pub scan_active: bool,
    pub scan_found: usize,
    pub scan_current: String,
    pub scan_play_first: bool,
    pub scan_played_first: bool,
    pub scan_added: usize,
    pub scan_started: Instant,
    pub browser: FolderBrowser,
    pub browse: BrowseService,
    // ---- SQLite library index (P6) ----
    pub db: LibraryDb,
    /// Optional recursive directory watcher feeding the index.
    pub library_watch: WatchService,
    library_watch_roots: Vec<PathBuf>,
    library_watch_extensions: Vec<String>,
    library_watch_enabled: bool,
    library_watch_error_shown: bool,
    /// Editable comma-separated extension list shown in Settings → Library.
    pub library_extensions_text: String,
    pub library_tab: views::LibraryTab,
    pub library_search: String,
    pub library_sort: LibrarySort,
    pub library_rows: Vec<LibraryEntry>,
    pub library_total: usize,
    pub library_loading: bool,
    pub library_query_id: u64,
    /// Zero-based page of the library index list. The list is paged instead of
    /// rendered in one long scroll to keep each frame cheap.
    pub library_page: usize,
    pub db_index_active: bool,
    pub db_index_progress: usize,
    pub db_index_current: String,
    pub db_stats: Option<LibraryStats>,
    pub db_stats_loading: bool,
    pub db_duplicates: Vec<DuplicateGroup>,
    pub db_missing: Vec<LibraryEntry>,
    pub db_tools_busy: bool,
    /// Smart playlist requested from the database, and whether to start it.
    pub smart_pending: Option<(SmartKind, bool)>,
    pub current_playlist: Option<PathBuf>,
    /// Saved-playlist rename dialog state.
    pub playlist_rename: Option<PlaylistRename>,
    /// Text filter applied to the queue page. Kept while switching pages but
    /// cleared whenever the queue is replaced or emptied.
    pub queue_filter: String,
    /// Normalized keys of playlist paths currently missing on disk, refreshed
    /// periodically.
    playlist_missing: HashSet<String>,
    playlist_check_at: Option<Instant>,
    pub resume_after_import: Option<ResumeIntent>,
    /// While a playlist re-import is streaming, re-apply the old shuffle order
    /// as new batches arrive so the cycle keeps its original positions.
    pub pending_shuffle_keys: Option<Vec<String>>,
    /// Reset the central scroll position on the next frame (page changes).
    pub scroll_to_top: bool,
    pub api: ApiService,
    pub search_query: String,
    pub search_source: String,
    pub search_kind: SearchKind,
    pub search_results: Vec<ApiSong>,
    pub search_total: u64,
    pub search_page: u32,
    pub search_loading: bool,
    pub search_error: Option<String>,
    pub search_request_id: u64,
    /// Persistent search page cache (also used while offline).
    pub search_cache: SearchCache,
    pub search_cached: bool,
    search_request_key: String,
    pub search_suggestions: Vec<String>,
    pub search_suggest_index: Option<usize>,
    pub suggest_query: String,
    pub suggest_due: Option<Instant>,
    pub suggest_request_id: u64,
    pub album_view: Option<AlbumView>,
    pub artist_view: Option<ArtistView>,
    pub thumbnails: HashMap<String, egui::TextureHandle>,
    /// Insertion order for `thumbnails`, used by the bounded cache eviction.
    thumb_order: VecDeque<String>,
    pub thumb_pending: HashSet<String>,
    pub thumb_failed: HashMap<String, ThumbRetry>,
    pub thumb_service: crate::util::cover::ThumbnailService,
    pub online_request_id: u64,
    pub online_track_key: String,
    pub online_loading: bool,
    /// Background `info` refresh for a cache-hit online track.
    info_request_id: u64,
    info_track_key: String,
    /// Manual refresh: replace existing lyrics with the source's LRC and
    /// ignore the per-track TTL.
    info_force: bool,
    /// Last successful/attempted online `info` refresh per track. Prevents a
    /// replay or auto-advance from hammering the source every few seconds.
    info_refreshed: HashMap<String, Instant>,
    /// Token of the newest engine load; stale `Loaded` events are ignored.
    engine_load_token: u64,
    /// Deadline for the current online resolve. A worker can be stuck behind a
    /// busy stream server; without this the UI would spin forever.
    online_deadline: Option<Instant>,
    // ---- P8 downloads / cache ----
    pub downloads: DownloadService,
    pub download_items: Vec<DownloadItem>,
    download_seq: u64,
    last_cache_trim: Instant,
    pub cache_files: usize,
    pub cache_bytes: u64,
    /// Whole cache root, including covers/lyrics/ReplayGain; shown on the
    /// settings cache tab.
    pub cache_total_files: usize,
    pub cache_total_bytes: u64,
    cache_stats_at: Instant,
    // ---- P9 visualizer ----
    pub visual_levels: Vec<f32>,
    pub visual_peaks: Vec<f32>,
    pub visual_wave: Vec<f32>,
    /// Reused FFT scratch space, so a visible visualizer does not allocate a
    /// new 2048-point buffer every repaint.
    visual_fft: Vec<rustfft::num_complex::Complex<f32>>,
    visual_band_max: Vec<f32>,
    last_visualizer_update: Instant,
    /// Result of the last explicit source connection test.
    pub source_test_message: Option<(String, bool)>,
    pub url_dialog_open: bool,
    pub url_input: String,
    pub url_source: String,
    pub url_error: Option<String>,
    pub reset_confirm_open: bool,
    /// Frame interval for the UI heartbeat thread (kept in sync with the
    /// performance settings each pass).
    pub heartbeat_frame_ms: Arc<std::sync::atomic::AtomicU64>,
    /// True only while a hidden/minimized/occluded window still needs the
    /// heartbeat to drive an immediate overlay viewport.
    heartbeat_active: Arc<AtomicBool>,
    /// Which overlay windows are open (read by the heartbeat thread).
    pub overlay_activity: Arc<OverlayActivity>,
    /// Native Direct2D desktop-lyrics window thread. The player owns playback
    /// and parsing; this thread only paints and forwards menu/geometry actions.
    native_desktop_lyrics: Option<crate::platform::native_overlay::NativeOverlay>,
    native_desktop_lyrics_retry_at: Instant,
    desktop_lyrics_signature: Option<Vec<u8>>,
    desktop_lyrics_sent_lyrics_version: u64,
    desktop_lyrics_sent_cover_version: u64,
    desktop_lyrics_cover_png: Option<Vec<u8>>,
    desktop_lyrics_cover_version: u64,
    desktop_lyrics_last_state_check: Instant,
    desktop_lyrics_last_position_ms: u64,
    desktop_lyrics_last_playing: bool,
    desktop_lyrics_last_tick: Instant,
    /// Native Direct2D mini-player window thread. When enabled it replaces the
    /// immediate egui viewport, so the mini player no longer drives the root
    /// pass.
    native_mini: Option<crate::platform::native_mini::NativeMini>,
    native_mini_retry_at: Instant,
    native_mini_signature: Option<Vec<u8>>,
    native_mini_sent_lyrics_version: u64,
    native_mini_sent_cover_version: u64,
    native_mini_last_state_check: Instant,
    native_mini_last_position_ms: u64,
    native_mini_last_playing: bool,
    native_mini_last_tick: Instant,
    /// True while the native mini window (not the egui viewport) is showing.
    mini_native_active: bool,
    /// Independent fullscreen lyrics window (separate OS window).
    pub lyrics_window_open: bool,
    /// Shared state/actions for the deferred standalone lyrics viewport. A
    /// deferred viewport keeps repainting when the main window is hidden.
    pub lyrics_window_snapshot: Arc<Mutex<views::lyrics_window::LyricsWindowSnapshot>>,
    pub lyrics_window_actions: Arc<Mutex<Vec<views::lyrics_window::LyricsWindowAction>>>,
    pub lyrics_window_close: Arc<AtomicBool>,
    /// Remember whether the standalone window was last windowed instead of
    /// fullscreen.
    pub lyrics_window_windowed: bool,
    /// Apply the windowed size only once; re-applying it every frame would
    /// prevent the user from resizing the window.
    lyrics_window_size_pending: bool,
    /// Last content-protection value sent to the standalone lyrics window.
    lyrics_window_protected: Option<bool>,
    /// Mini player viewport state (independent window).
    pub mini_snapshot: Arc<Mutex<views::mini_player::MiniPlayerSnapshot>>,
    pub mini_actions: Arc<Mutex<Vec<views::mini_player::MiniPlayerAction>>>,
    pub mini_close: Arc<AtomicBool>,
    mini_spawned: bool,
    mini_hwnd: Option<isize>,
    mini_taskbar: Option<bool>,
    /// Background work (file dialogs) delivered to the UI thread.
    task_tx: std::sync::mpsc::Sender<AppTask>,
    task_rx: std::sync::mpsc::Receiver<AppTask>,
    /// True while a native file dialog is open; prevents opening two at once
    /// now that the UI keeps running during the dialog.
    file_dialog_open: Arc<AtomicBool>,
    /// Force the next state push to restore the saved/default position.
    pub desktop_lyrics_reset_pos: bool,
    // ---- P7 playback enhancements ----
    /// A-B repeat range in media seconds.
    pub ab_start: Option<f64>,
    pub ab_end: Option<f64>,
    ab_last_seek: Instant,
    /// Sleep timer deadline; `None` when inactive.
    pub sleep_deadline: Option<Instant>,
    /// Stop after the current track instead of at a wall-clock deadline.
    pub sleep_end_of_track: bool,
    /// Next-track pre-arm token and index handed to the engine.
    pub armed_next: Option<(u64, usize, String)>,
    /// Pending background URL resolve for the next online track: request id,
    /// queue index and the same signature used by `armed_next`.
    next_prefetch: Option<(u64, usize, String)>,
    /// Backoff after a failed prefetch: signature + earliest next attempt.
    next_prefetch_backoff: Option<(String, Instant)>,
    next_prefetch_failures: u32,
    /// Token/index of the deck the engine may already be playing after a
    /// gapless/crossfade transition.
    pub pending_auto: Option<(u64, usize)>,
    next_arm_token: u64,
    /// Periodic probe used to start a delayed online prefetch near track end.
    last_next_arm_poll: Instant,
    // ---- P9 command palette ----
    pub command_palette_open: bool,
    pub command_palette_query: String,
    pub command_palette_index: usize,
    command_palette_focus: bool,
    // ---- P9 local BambooMusic station (random radio) ----
    pub station_active: bool,
    pub station_fetching: bool,
    station_request: u64,
    station_play_on_arrival: bool,
    station_source_id: Option<String>,
    /// Keys of tracks appended by the station; used to prune only station
    /// entries, never the user's own queue, when a session runs for hours.
    station_keys: HashSet<String>,
    pub station_error: Option<String>,
    /// Newest first, capped at 8. Shown on the Discover page.
    pub station_history: Vec<Track>,
    // ---- Discover recommendation feed ----
    pub discover_tracks: Vec<Track>,
    pub discover_loading: bool,
    pub discover_error: Option<String>,
    pub discover_source_id: Option<String>,
    discover_request: u64,
    /// Avoid duplicate system notifications for the same track.
    last_notified_key: String,
    startup_minimize_pending: bool,
    last_fullscreen_probe: Instant,
    foreground_fullscreen: bool,
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        settings: Settings,
        mut session: Session,
        paths: Paths,
        startup_args: Vec<PathBuf>,
        startup_commands: Vec<String>,
    ) -> Self {
        let ctx = cc.egui_ctx.clone();
        theme::apply(&ctx, &settings);
        fonts::install(&ctx, &settings);

        let hwnd = window::hwnd_from_creation(cc);
        if let Some(raw) = hwnd {
            window::apply_style(
                raw,
                theme::is_dark(&ctx, &settings),
                settings.theme.roundness,
                settings.window.capture_protection,
            );
        }

        let mut settings = settings;
        if session.saved_at.is_some() {
            settings.playback.volume = session.volume.clamp(0.0, 1.0);
        }
        let engine = AudioEngine::new(
            settings.playback.volume,
            settings.playback.muted,
            settings.performance.release_audio_output,
            dsp_config(&settings),
            Some(overlay_waker(&ctx)),
        );
        let metadata = MetadataService::new();
        metadata.set_enabled(settings.performance.metadata_mode.enabled());
        let device_info = engine.output_info();
        let lang = settings.language;
        crate::i18n::set_language(lang);
        let gpu_info = cc
            .wgpu_render_state
            .as_ref()
            .map(|state| {
                let info = state.adapter.get_info();
                // Log the full adapter identity: on hybrid laptops the biggest
                // GPU cost is often simply that wgpu selected the integrated
                // GPU, and the settings page must be able to show which one is
                // actually in use.
                tracing::info!(
                    "wgpu adapter: name={} backend={:?} device_type={:?} driver={} {}",
                    info.name,
                    info.backend,
                    info.device_type,
                    info.driver,
                    info.driver_info
                );
                format!("{} · {:?} · {:?}", info.name, info.backend, info.device_type)
            })
            .unwrap_or_else(|| crate::i18n::tr("未知渲染后端").to_string());

        let mut queue = Queue {
            items: session.queue.clone(),
            current: None,
            shuffle: session.shuffle,
            repeat: session.repeat,
            ..Default::default()
        };
        queue.rebuild_keys();
        queue.current = session
            .current_key
            .as_ref()
            .and_then(|key| queue.position_of_key(key))
            .or_else(|| session.current_index.filter(|i| *i < session.queue.len()));
        if queue.shuffle {
            if session.shuffle_keys.is_empty() {
                queue.build_shuffle_order(true);
                queue.shuffle_append = true;
            } else {
                let matched = queue.restore_shuffle_order(&session.shuffle_keys);
                if matched == 0 {
                    queue.build_shuffle_order(true);
                }
                queue.shuffle_append = true;
            }
        }

        let last_window_rect = session
            .window_pos
            .zip(session.window_size)
            .map(|((x, y), (w, h))| (x, y, w, h));
        let window_maximized = session.window_maximized;
        let page = session.page;
        let show_lyrics_sidebar = settings.lyrics.sidebar_visible;
        let last_font_config = (
            settings.theme.font_family.clone(),
            settings.theme.font_size.round() as u8,
            settings.desktop_lyrics.font_family.clone(),
            settings.large_lyrics.font_family.clone(),
        );
        let current_playlist = session.playlist_path.clone();
        if let Some(path) = current_playlist.clone() {
            // Sessions written before saved playlists existed only carried the
            // current path; add it to the sidebar list on first launch.
            session.remember_playlist(path, crate::config::unix_now());
        }
        // A hand-edited or older session can contain the same path twice.
        let mut seen_playlists = HashSet::new();
        session.playlists.retain(|entry| {
            !entry.path.as_os_str().is_empty() && seen_playlists.insert(normalize_path(&entry.path))
        });
        session
            .playlists
            .truncate(crate::config::MAX_SAVED_PLAYLISTS);
        let fullscreen_lyrics = session.fullscreen_lyrics;
        let session_lyrics_window = session.lyrics_window;
        let session_lyrics_window_windowed = session.lyrics_window_windowed;
        let default_search_source = settings
            .enabled_sources()
            .find(|source| !source.base_url.trim().is_empty())
            .map(|source| source.id.clone())
            .unwrap_or_else(|| "local".to_string());
        let initial_lyrics_settings = settings.lyrics.clone();
        let initial_large_lyrics = settings.large_lyrics.clone();
        let library_extensions_text = settings.library.extensions.join(", ");

        // Open the index before `paths` moves into the struct.
        let library_db = LibraryDb::open(paths.library_db.clone());
        let downloads = DownloadService::new(settings.download.concurrency as usize);
        let startup_minimize_pending = settings.window.start_minimized;
        let heartbeat_frame_ms = settings.performance.frame_millis();
        let heartbeat_active = Arc::new(AtomicBool::new(false));
        let mini_colors = theme::colors(&ctx, &settings);
        let (task_tx, task_rx) = std::sync::mpsc::channel::<AppTask>();
        // In multi-instance mode each window owns its own queue, so there is
        // no primary IPC server (and no pipe-name collision).
        let ipc_rx = if settings.window.allow_multiple_instances {
            crate::platform::ipc::disabled()
        } else {
            crate::platform::ipc::start_server()
        };
        let mut replaygain_cache =
            crate::audio::loudness::load_cache(&paths.cache.join("replaygain.json"));
        replaygain_cache.retain(|key, entry| entry.matches(Path::new(key)));
        for (key, entry) in &replaygain_cache {
            let path = Path::new(key);
            crate::audio::loudness::set_cached_gain(path.to_path_buf(), entry.gain_db);
        }
        let search_cache = SearchCache::load(&paths.cache.join("search.json"));
        let mut app = Self {
            settings,
            session,
            paths,
            lang,
            page: Page::NowPlaying,
            engine,
            heartbeat_frame_ms: Arc::new(std::sync::atomic::AtomicU64::new(
                heartbeat_frame_ms,
            )),
            heartbeat_active: heartbeat_active.clone(),
            queue,
            current_track: None,
            current_duration: None,
            cover: None,
            smtc_cover_key: String::new(),
            smtc_cover_png: None,
            smtc_cover_rev: 0,
            background: None,
            gpu_bg: gpu_bg::GpuBackground::new(
                cc.wgpu_render_state
                    .as_ref()
                    .map(|state| state.target_format),
            ),
            mica_active: false,
            custom_background: None,
            placeholder_cover: None,
            cover_key: String::new(),
            toasts: VecDeque::new(),
            hwnd,
            tray: None,
            tray_failed: false,
            smtc: None,
            tray_state_key: String::new(),
            tray_quit: Arc::new(AtomicBool::new(false)),
            window_hidden: false,
            allow_close: false,
            session_dirty: false,
            settings_dirty: false,
            last_session_save: Instant::now(),
            last_settings_save: Instant::now(),
            seek_preview: None,
            playback_state: PlaybackState::Stopped,
            device_info,
            ctx: ctx.clone(),
            metadata,
            cover_service: crate::util::cover::CoverService::new(),
            cover_loading: false,
            loudness: crate::audio::loudness::LoudnessService::new(),
            replaygain_cache,
            replaygain_scanning: HashSet::new(),
            replaygain_tagged: HashSet::new(),
            replaygain_cache_dirty: false,
            last_replaygain_save: Instant::now(),
            lyrics_service: LyricsService::new(),
            lyrics: None,
            lyrics_loading: false,
            lyrics_key: String::new(),
            lyrics_error: None,
            lyrics_prefetched: HashSet::new(),
            lyrics_prefetch_cache: HashMap::new(),
            lyrics_prefetch_order: VecDeque::new(),
            lyrics_version: 0,
            lyrics_manual_scroll_until: None,
            fullscreen_lyrics,
            lyrics_os_fullscreen: false,
            lyrics_return_rect: None,
            pending_window_restore: None,
            lyrics_editor: None,
            lyrics_editor_open: false,
            deferred_start: None,
            last_window_rect,
            window_maximized,
            last_title: String::new(),
            last_font_config,
            startup_args,
            startup_commands,
            ipc_rx,
            jump_list_dirty: true,
            jump_list_built: false,
            jump_list_enabled: false,
            jump_list_retry_at: None,
            restore_notice_pending: false,
            show_lyrics_sidebar,
            lyrics_sidebar_width: 330.0,
            settings_tab: views::SettingsTab::default(),
            memory_info: crate::platform::memory::process_memory(),
            last_memory_sample: Instant::now(),
            last_metadata_poll: Instant::now(),
            last_trim: Instant::now(),
            gpu_info,
            style_reapplied: false,
            style_reapply_at: Instant::now() + Duration::from_millis(450),
            was_focused: true,
            scan: ScanService::new(),
            scan_id: 0,
            scan_active: false,
            scan_found: 0,
            scan_current: String::new(),
            scan_play_first: false,
            scan_played_first: false,
            scan_added: 0,
            scan_started: Instant::now(),
            browser: FolderBrowser::default(),
            browse: BrowseService::new(),
            db: library_db,
            library_watch: WatchService::new(),
            library_watch_roots: Vec::new(),
            library_watch_extensions: Vec::new(),
            library_watch_enabled: false,
            library_watch_error_shown: false,
            library_extensions_text,
            library_tab: views::LibraryTab::Browse,
            library_search: String::new(),
            library_sort: LibrarySort::default(),
            library_rows: Vec::new(),
            library_total: 0,
            library_loading: false,
            library_query_id: 0,
            library_page: 0,
            db_index_active: false,
            db_index_progress: 0,
            db_index_current: String::new(),
            db_stats: None,
            db_stats_loading: false,
            db_duplicates: Vec::new(),
            db_missing: Vec::new(),
            db_tools_busy: false,
            smart_pending: None,
            current_playlist,
            playlist_rename: None,
            queue_filter: String::new(),
            playlist_missing: HashSet::new(),
            playlist_check_at: None,
            resume_after_import: None,
            pending_shuffle_keys: None,
            scroll_to_top: true,
            api: ApiService::new(),
            search_query: String::new(),
            search_source: default_search_source.clone(),
            search_kind: SearchKind::Song,
            search_results: Vec::new(),
            search_total: 0,
            search_page: 1,
            search_loading: false,
            search_error: None,
            search_request_id: 0,
            search_cache,
            search_cached: false,
            search_request_key: String::new(),
            search_suggestions: Vec::new(),
            search_suggest_index: None,
            suggest_query: String::new(),
            suggest_due: None,
            suggest_request_id: 0,
            album_view: None,
            artist_view: None,
            thumbnails: HashMap::new(),
            thumb_order: VecDeque::new(),
            thumb_pending: HashSet::new(),
            thumb_failed: HashMap::new(),
            thumb_service: crate::util::cover::ThumbnailService::new(),
            online_request_id: 0,
            online_track_key: String::new(),
            online_loading: false,
            info_request_id: 0,
            info_track_key: String::new(),
            info_force: false,
            info_refreshed: HashMap::new(),
            engine_load_token: 0,
            online_deadline: None,
            downloads,
            download_items: Vec::new(),
            download_seq: 1,
            last_cache_trim: Instant::now() - Duration::from_secs(600),
            cache_files: 0,
            cache_bytes: 0,
            cache_total_files: 0,
            cache_total_bytes: 0,
            cache_stats_at: Instant::now() - Duration::from_secs(60),
            visual_levels: Vec::new(),
            visual_peaks: Vec::new(),
            visual_wave: Vec::new(),
            visual_fft: Vec::new(),
            visual_band_max: Vec::new(),
            last_visualizer_update: Instant::now(),
            source_test_message: None,
            url_dialog_open: false,
            url_input: String::new(),
            url_source: default_search_source.clone(),
            url_error: None,
            reset_confirm_open: false,
            overlay_activity: Arc::new(OverlayActivity::default()),
            native_desktop_lyrics: None,
            native_desktop_lyrics_retry_at: Instant::now(),
            desktop_lyrics_signature: None,
            desktop_lyrics_sent_lyrics_version: 0,
            desktop_lyrics_sent_cover_version: 0,
            desktop_lyrics_cover_png: None,
            desktop_lyrics_cover_version: 0,
            desktop_lyrics_last_state_check: Instant::now() - Duration::from_secs(5),
            desktop_lyrics_last_position_ms: 0,
            desktop_lyrics_last_playing: false,
            desktop_lyrics_last_tick: Instant::now() - Duration::from_secs(5),
            native_mini: None,
            native_mini_retry_at: Instant::now(),
            native_mini_signature: None,
            native_mini_sent_lyrics_version: u64::MAX,
            native_mini_sent_cover_version: u64::MAX,
            native_mini_last_state_check: Instant::now() - Duration::from_secs(5),
            native_mini_last_position_ms: 0,
            native_mini_last_playing: false,
            native_mini_last_tick: Instant::now() - Duration::from_secs(5),
            mini_native_active: false,
            lyrics_window_open: session_lyrics_window,
            lyrics_window_snapshot: Arc::new(Mutex::new(
                views::lyrics_window::LyricsWindowSnapshot {
                    title: String::new(),
                    artist: String::new(),
                    album: String::new(),
                    lyrics_loading: false,
                    playing: false,
                    position_ms: 0,
                    duration_ms: None,
                    offset_ms: 0,
                    windowed: session_lyrics_window_windowed,
                    settings: initial_lyrics_settings,
                    large: initial_large_lyrics,
                    colors: mini_colors.clone(),
                    cover: None,
                    background: None,
                    position_updated: None,
                    frame_rate: 60,
                    lyrics: None,
                    lyrics_version: 0,
                    clock: None,
                    window_size: None,
                    window_pos: None,
                },
            )),
            lyrics_window_actions: Arc::new(Mutex::new(Vec::new())),
            lyrics_window_close: Arc::new(AtomicBool::new(false)),
            lyrics_window_windowed: session_lyrics_window_windowed,
            lyrics_window_size_pending: session_lyrics_window && session_lyrics_window_windowed,
            lyrics_window_protected: None,
            desktop_lyrics_reset_pos: false,
            mini_snapshot: Arc::new(Mutex::new(views::mini_player::MiniPlayerSnapshot {
                title: String::new(),
                artist: String::new(),
                playing: false,
                position: 0.0,
                duration: 0.0,
                volume: 0.8,
                muted: false,
                cover: None,
                colors: mini_colors,
                opacity: 1.0,
                window_pos: None,
                frame_rate: 60,
                clock: None,
                show_lyrics: true,
                karaoke: true,
                karaoke_words_only: false,
                lyrics_lines: 1,
                lyrics_bold: false,
                show_translation: true,
                lyrics_font_size: 13.0,
                lyrics: None,
                lyrics_version: 0,
                lyrics_loading: false,
                position_ms: 0,
                position_updated: None,
                offset_ms: 0,
                requested_size: None,
                window_size: None,
            })),
            mini_actions: Arc::new(Mutex::new(Vec::new())),
            mini_close: Arc::new(AtomicBool::new(false)),
            mini_spawned: false,
            mini_hwnd: None,
            mini_taskbar: None,
            task_tx,
            task_rx,
            file_dialog_open: Arc::new(AtomicBool::new(false)),
            ab_start: None,
            ab_end: None,
            ab_last_seek: Instant::now() - Duration::from_secs(5),
            sleep_deadline: None,
            sleep_end_of_track: false,
            armed_next: None,
            next_prefetch: None,
            next_prefetch_backoff: None,
            next_prefetch_failures: 0,
            pending_auto: None,
            next_arm_token: 1,
            last_next_arm_poll: Instant::now(),
            command_palette_open: false,
            command_palette_query: String::new(),
            command_palette_index: 0,
            command_palette_focus: false,
            station_active: false,
            station_fetching: false,
            station_request: 0,
            station_play_on_arrival: false,
            station_source_id: None,
            station_keys: HashSet::new(),
            station_error: None,
            station_history: Vec::new(),
            discover_tracks: Vec::new(),
            discover_loading: false,
            discover_error: None,
            discover_source_id: None,
            discover_request: 0,
            last_notified_key: String::new(),
            startup_minimize_pending,
            last_fullscreen_probe: Instant::now() - Duration::from_secs(5),
            foreground_fullscreen: false,
        };

        // Overlay viewports cannot borrow `App`; give them a direct clock so
        // playback position keeps advancing even when the main window is
        // minimized/hidden and its logic ticks slowly.
        let clock = app.engine.clock();
        if let Ok(mut state) = app.lyrics_window_snapshot.lock() {
            state.clock = Some(clock.clone());
        }
        if let Ok(mut state) = app.mini_snapshot.lock() {
            state.clock = Some(clock);
        }

        // eframe only runs `App::logic` when the root viewport repaints, and a
        // hidden, occluded or unfocused main window can stay idle indefinitely.
        // Tray commands and overlay snapshots are driven from `logic`, and the
        // overlay windows are immediate viewports rendered inside the root
        // pass. eframe only runs that pass when the root repaints, so a small
        // thread wakes it at the animation rate while playing (and a few times
        // per second when idle).
        let heartbeat_ctx = ctx.clone();
        let heartbeat_clock = app.engine.clock();
        let heartbeat_frame = app.heartbeat_frame_ms.clone();
        let heartbeat_overlays = app.overlay_activity.clone();
        let heartbeat_active = app.heartbeat_active.clone();
        std::thread::Builder::new()
            .name("bamboo-ui-heartbeat".to_string())
            .spawn(move || {
                loop {
                    let playing =
                        heartbeat_clock.state() == crate::audio::PlaybackState::Playing;
                    let frame_ms = heartbeat_frame.load(Ordering::Relaxed).clamp(8, 100);
                    if !heartbeat_active.load(Ordering::Relaxed) {
                        // The visible/focused root is already scheduled by
                        // `App::logic`; a second wake-up here doubled the frame
                        // rate (and GPU) without rendering anything new.
                        std::thread::sleep(Duration::from_millis(200));
                        continue;
                    }
                    // Only an immediate egui viewport needs animation-rate
                    // frames. Native desktop lyrics and the native mini player
                    // own their render timers and interpolate the clock; they
                    // only need state changes at a low cadence.
                    let immediate_overlay = heartbeat_overlays.lyrics_window.load(Ordering::Relaxed)
                        || heartbeat_overlays.mini_player.load(Ordering::Relaxed);
                    let interval = if immediate_overlay && playing {
                        frame_ms
                    } else {
                        200
                    };
                    std::thread::sleep(Duration::from_millis(interval));
                    heartbeat_ctx.request_repaint_of(egui::ViewportId::ROOT);
                    // A repaint request on a visible overlay window wakes the
                    // root immediately even when eframe throttles the hidden
                    // root window to ~10 fps.
                    if heartbeat_overlays.lyrics_window.load(Ordering::Relaxed) {
                        heartbeat_ctx.request_repaint_of(views::lyrics_window::viewport_id());
                    }
                    if heartbeat_overlays.mini_player.load(Ordering::Relaxed) {
                        heartbeat_ctx.request_repaint_of(views::mini_player::viewport_id());
                    }
                }
            })
            .ok();

        app.page = page;
        app.station_active = app.session.station && app.settings.features.internet_radio;
        app.station_history = app.session.station_history.clone();
        app.restore_notice_pending = app.session.saved_at.is_some() && !app.queue.is_empty();

        let will_reimport = app.settings.playlist.auto_refresh_on_startup
            && app
                .current_playlist
                .as_ref()
                .map(|playlist| playlist.is_file())
                .unwrap_or(false);
        if let Some(index) = app.queue.current {
            // Restore the position even when paused; the setting only controls
            // whether playback starts automatically. When the playlist will be
            // re-imported immediately, skip opening the decoder twice: the
            // import resumes the track once the new queue exists.
            let start = app.session.position_secs.max(0.0);
            let autoplay = app.settings.playback.resume_on_start;
            if will_reimport || (!autoplay && app.settings.performance.defer_paused_restore) {
                app.prepare_restore(index, start, autoplay);
            } else {
                app.load_index(index, start, autoplay);
            }
        }
        if !app.settings.performance.lazy_audio {
            app.engine.warm_up();
        }

        if app.settings.playlist.auto_refresh_on_startup {
            if let Some(playlist) = app.current_playlist.clone() {
                if playlist.is_file() {
                    app.import_playlist(playlist, true);
                } else {
                    app.current_playlist = None;
                }
            }
        }
        // If the playlist was unreadable/missing, no import took over the
        // prepared restore; open the session track normally.
        if app.playback_state == PlaybackState::Loading {
            if let (Some(index), Some(start)) = (app.queue.current, app.deferred_start) {
                app.load_index(index, start, true);
            }
        }

        let args = app.startup_args.clone();
        if !args.is_empty() {
            app.add_paths(args, app.queue.current.is_none());
        }
        app.sync_library_watch();
        app.api
            .set_min_interval_ms(app.settings.network.api_min_interval_ms);
        if app.settings.library.scan_on_start && !app.settings.library.folders.is_empty() {
            app.start_library_index();
        }
        // Start the native mini window during construction so it appears
        // together with the main window. Waiting for the first root pass meant
        // a launch that starts minimized/hidden (or is occluded immediately)
        // did not show the mini player until the main window was opened.
        if app.settings.features.mini_player
            && app.settings.mini.visible
            && app.settings.mini.native_window
            && app.ensure_native_mini()
        {
            app.mini_native_active = true;
            app.sync_native_mini(&ctx);
        }
        app
    }

    // ------------------------------------------------------------------
    // Basic helpers
    // ------------------------------------------------------------------

    pub fn t(&self, key: &'static str) -> &'static str {
        t(self.lang, key)
    }

    pub fn colors(&self) -> ThemeColors {
        let mut colors = theme::colors(&self.ctx, &self.settings);
        // Mica shows the desktop wallpaper through the window, so panels and
        // cards become translucent. The backdrop is only visible when the DWM
        // system backdrop was actually applied.
        if self.mica_active && self.settings.theme.background == crate::config::BackgroundMode::Mica
        {
            colors.panel = Color32::from_rgba_unmultiplied(
                colors.panel.r(),
                colors.panel.g(),
                colors.panel.b(),
                205,
            );
            colors.card = Color32::from_rgba_unmultiplied(
                colors.card.r(),
                colors.card.g(),
                colors.card.b(),
                236,
            );
            colors.card_hover = Color32::from_rgba_unmultiplied(
                colors.card_hover.r(),
                colors.card_hover.g(),
                colors.card_hover.b(),
                244,
            );
        }
        colors
    }

    pub fn toast(&mut self, text: impl Into<String>, kind: ToastKind) {
        self.toasts.push_back(Toast {
            text: text.into(),
            kind,
            created: Instant::now(),
        });
        while self.toasts.len() > 4 {
            self.toasts.pop_front();
        }
        self.ctx.request_repaint();
    }

    pub fn set_page(&mut self, page: Page) {
        // The lyrics editor overlays every page. Navigating away first asks
        // about unsaved changes instead of silently discarding them.
        if self.lyrics_editor_open {
            if self
                .lyrics_editor
                .as_ref()
                .is_some_and(|editor| editor.dirty)
            {
                self.request_close_lyrics_editor();
                return;
            }
            self.force_close_lyrics_editor();
        }
        if self.page != page {
            self.scroll_to_top = true;
        }
        self.page = page;
        self.session_dirty = true;
    }

    pub fn is_playing(&self) -> bool {
        self.playback_state == PlaybackState::Playing
    }

    pub fn visualizer_visible(&self) -> bool {
        let settings = &self.settings.visualizer;
        self.settings.features.visualizer
            && settings.enabled
            && settings.mode != crate::config::VisualizerMode::Off
            && (!settings.auto_hide || self.is_playing())
    }

    /// Update cadence requested by the audio visualizer. The spectrum is
    /// smoothed, so 30 fps already looks continuous and costs half the CPU of
    /// running the FFT on every 60 fps UI frame.
    fn visualizer_millis(&self) -> u64 {
        (1000 / self.settings.visualizer.fps.clamp(5, 60) as u64).max(8)
    }

    /// True when the animated flow background should keep rendering. Static
    /// backgrounds never need a continuous frame stream.
    fn background_animation_active(&self, focused: bool, occluded: bool, minimized: bool) -> bool {
        let performance = &self.settings.performance;
        !self.window_hidden
            && !occluded
            && !minimized
            && (focused || !performance.pause_animations_unfocused)
            && performance.background_animation
            && performance.cover_background
            && self.settings.theme.animations
            && !self.settings.theme.reduce_motion
            && self.settings.theme.background == crate::config::BackgroundMode::Flow
            && self.gpu_bg.available()
    }

    /// True when a visible lyrics surface is doing per-frame karaoke fill.
    /// Plain (untimed) lyrics and the paused state are static, so they do not
    /// justify a frame-rate repaint.
    fn lyrics_animation_active(&self) -> bool {
        self.is_playing()
            && !self.lyrics_editor_open
            && (self.show_lyrics_sidebar
                || self.fullscreen_lyrics
                || self.page == Page::NowPlaying)
            && self.settings.lyrics.karaoke
            && self
                .lyrics
                .as_ref()
                .is_some_and(|lyrics| lyrics.timed)
    }

    /// Frame interval required by continuously animating content. `None`
    /// means the visible frame is static, so the much slower idle cadence is
    /// enough. Overlay windows drive their own repaints and are intentionally
    /// not included: the root window does not need to run at animation speed
    /// just because a mini player or desktop lyrics window is open.
    fn animation_millis(&self, focused: bool, occluded: bool, minimized: bool) -> Option<u64> {
        if self.window_hidden || occluded || minimized {
            return None;
        }
        // Eye-candy animation (visualizer/flow background) pauses in the
        // background by default. Karaoke lyrics are content, so they keep
        // running while the window is merely unfocused but still visible.
        let animations_allowed =
            focused || !self.settings.performance.pause_animations_unfocused;
        // Several animations can be active at once; the window must repaint
        // at the fastest of their intervals.
        let mut interval: Option<u64> = None;
        let mut add = |ms: u64| {
            interval = Some(interval.map_or(ms, |current: u64| current.min(ms)));
        };
        if animations_allowed {
            if self.visualizer_visible() && self.is_playing() {
                add(self.visualizer_millis());
            }
            if self.background_animation_active(focused, occluded, minimized) {
                add(self.settings.performance.background_millis());
            }
        }
        if self.lyrics_animation_active() {
            // Word/character karaoke fill and the auto-scroll animation still
            // want the configured animation rate.
            add(self.settings.performance.frame_millis());
        }
        interval
    }

    /// Repaint interval required to keep deferred overlay snapshots fresh.
    /// eframe only calls `App::logic` for the root viewport, so the egui
    /// lyrics window cannot update its own snapshot. Native overlay windows
    /// wake the root from their thread on every action and the audio engine
    /// wakes it on track/state events, so they only need an occasional clock
    /// correction tick while playing.
    fn overlay_millis(&self) -> Option<u64> {
        let desktop_visible = self.settings.features.desktop_lyrics
            && self.settings.desktop_lyrics.enabled
            && self.settings.desktop_lyrics.visible_at_start
            && (!self.settings.desktop_lyrics.hide_when_paused || self.is_playing());
        if self.lyrics_window_open {
            // An egui viewport: it needs the root pass at the animation rate.
            return Some(self.settings.performance.frame_millis());
        }
        let native_visible = desktop_visible
            || (self.settings.features.mini_player
                && self.settings.mini.visible
                && self.mini_native_active);
        if native_visible {
            // Position is interpolated in the native window; actions and
            // engine events wake the root immediately. A one-second correction
            // tick keeps the two clocks from drifting over long playback and
            // also drives the sleep timer; while paused it returns None so a
            // hidden player stays at 0% GPU.
            return self.is_playing().then_some(1_000);
        }
        if self.settings.features.mini_player && self.settings.mini.visible {
            // The egui mini player shows a progress line and the time.
            return Some(self.settings.performance.idle_millis());
        }
        None
    }

    fn update_visualizer(&mut self) {
        use rustfft::num_complex::Complex;

        let mode = self.settings.visualizer.mode;
        // Reuse the waveform buffer between frames (and keep the allocation
        // for the FFT pass below), so a visible visualizer does not allocate.
        let mut samples = std::mem::take(&mut self.visual_wave);
        self.engine.fill_visual_samples(&mut samples, 2048);
        if samples.is_empty() {
            self.visual_wave = samples;
            return;
        }
        self.visual_wave = samples;
        if mode == crate::config::VisualizerMode::Waveform {
            return;
        }

        const FFT_SIZE: usize = 2048;
        const BANDS: usize = 48;
        // Reuse the scratch buffer; `fill` is much cheaper than allocating
        // 16 KiB on every visualizer frame.
        if self.visual_fft.len() != FFT_SIZE {
            self.visual_fft
                .resize(FFT_SIZE, Complex::<f32>::new(0.0, 0.0));
        } else {
            self.visual_fft.fill(Complex::<f32>::new(0.0, 0.0));
        }
        let count = self.visual_wave.len().min(FFT_SIZE);
        let start = FFT_SIZE - count;
        for (index, sample) in self.visual_wave.iter().take(count).enumerate() {
            let window = 0.5
                - 0.5 * (std::f32::consts::TAU * index as f32 / (count.max(2) - 1) as f32).cos();
            self.visual_fft[start + index] = Complex::new(sample * window, 0.0);
        }
        visualizer_fft().process(&mut self.visual_fft);

        let sample_rate = self.device_info.sample_rate.max(8_000) as f32;
        let nyquist = sample_rate * 0.45;
        if self.visual_band_max.len() != BANDS {
            self.visual_band_max = vec![0.0; BANDS];
        } else {
            self.visual_band_max.fill(0.0);
        }
        // Log-spaced bands are wider than the FFT resolution at the low end
        // (a 2048-point FFT at 44.1 kHz gives ~21.5 Hz bins), so the first few
        // bands receive no bins at all and would stay flat forever. Remember
        // which bands were actually sampled and interpolate the empty ones.
        let mut mapped = [false; BANDS];
        for bin in 1..FFT_SIZE / 2 {
            let frequency = bin as f32 * sample_rate / FFT_SIZE as f32;
            // Start at 40 Hz: below that a 2048-point FFT has no usable
            // resolution and most music carries no energy anyway.
            if frequency < 40.0 {
                continue;
            }
            let position = (frequency / 40.0).ln() / (nyquist / 40.0).ln();
            let band = ((position * BANDS as f32) as usize).min(BANDS - 1);
            mapped[band] = true;
            let magnitude = self.visual_fft[bin].norm() * 2.0 / FFT_SIZE as f32;
            let slot = &mut self.visual_band_max[band];
            if magnitude > *slot {
                *slot = magnitude;
            }
        }
        let mut previous: Option<usize> = None;
        for index in 0..BANDS {
            if !mapped[index] {
                continue;
            }
            if let Some(prev) = previous {
                let gap = index - prev;
                if gap > 1 {
                    let start = self.visual_band_max[prev];
                    let end = self.visual_band_max[index];
                    for step in 1..gap {
                        let t = step as f32 / gap as f32;
                        self.visual_band_max[prev + step] = start + (end - start) * t;
                    }
                }
            } else if index > 0 {
                // Leading gap: hold the first sampled band's level.
                let value = self.visual_band_max[index];
                self.visual_band_max[..index].fill(value);
            }
            previous = Some(index);
        }
        if let Some(prev) = previous
            && prev + 1 < BANDS
        {
            let value = self.visual_band_max[prev];
            self.visual_band_max[prev + 1..].fill(value);
        }
        if self.visual_levels.len() != BANDS {
            self.visual_levels = vec![0.0; BANDS];
            self.visual_peaks = vec![0.0; BANDS];
        }
        let sensitivity = self.settings.visualizer.sensitivity.max(0.1);
        for index in 0..BANDS {
            let target = ((self.visual_band_max[index] * 10.0 * sensitivity).sqrt()).clamp(0.0, 1.0);
            let old = self.visual_levels[index];
            let next = if target > old {
                old + (target - old) * 0.6
            } else {
                old * 0.82
            };
            self.visual_levels[index] = next;
            self.visual_peaks[index] = next.max(self.visual_peaks[index] * 0.94);
        }
    }

    pub fn position_secs(&self) -> f64 {
        if let Some(preview) = self.seek_preview {
            return preview;
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

    pub fn total_duration(&self) -> f64 {
        self.queue
            .items
            .iter()
            .filter_map(|t| t.duration)
            .sum::<f64>()
    }

    pub fn volume(&self) -> f32 {
        self.settings.playback.volume
    }

    // ------------------------------------------------------------------
    // Playback
    // ------------------------------------------------------------------

    pub fn play_index(&mut self, index: usize) {
        // Selecting a concrete song is an explicit exit from station mode.
        if self.station_active {
            self.station_active = false;
            self.station_fetching = false;
            self.station_request = 0;
        }
        self.load_index(index, 0.0, true);
    }

    pub fn play_track_now(&mut self, track: &Track) {
        let key = track.dedupe_key();
        if let Some(index) = self.queue.position_of_key(&key) {
            self.play_index(index);
        } else if self.queue.add(track.clone()) {
            let index = self.queue.len() - 1;
            self.play_index(index);
        }
    }

    pub fn load_index(&mut self, index: usize, start_secs: f64, autoplay: bool) {
        if index >= self.queue.items.len() {
            return;
        }
        self.disarm_next();
        self.clear_ab_loop_silent();
        self.deferred_start = None;
        self.online_request_id = 0;
        self.online_loading = false;
        self.online_deadline = None;
        self.info_request_id = 0;
        self.info_track_key.clear();
        // Stop the old deck before the new source is resolved. Previously an
        // online track was resolved asynchronously while the previous song
        // kept playing, so the title/lyrics showed the new track while the
        // audio was still the old one.
        self.engine.stop();
        let mut track = self.queue.items[index].clone();
        track.apply_filename_fallback();
        self.queue.set_current(index);
        self.current_track = Some(track.clone());
        self.current_duration = track.duration;
        self.record_station_history(&track);
        self.record_recent_file(&track);
        self.notify_track_change(&track);
        self.cover_key.clear();
        self.request_current_metadata(&track);
        self.refresh_lyrics(&track);
        self.refresh_visuals();
        self.update_title();
        self.session_dirty = true;
        if autoplay
            && self.settings.features.statistics
            && let Some(db_track) = DbTrack::from_track(&track)
        {
            self.db.record_play(db_track);
        }
        self.playback_state = if autoplay {
            PlaybackState::Loading
        } else {
            PlaybackState::Paused
        };
        self.maybe_scan_replaygain(&track);

        match &track.source {
            TrackSource::Local { path } => {
                if !path.is_file() {
                    self.toast(
                        format!("{}: {}", self.t("audio.error"), path.display()),
                        ToastKind::Error,
                    );
                    self.playback_state = PlaybackState::Stopped;
                    return;
                }
                self.engine_load_token =
                    self.engine
                        .load(EngineSource::Local(path.clone()), start_secs, autoplay);
            }
            TrackSource::Url { url } | TrackSource::Radio { url } => {
                if self.settings.network.offline {
                    self.toast(
                        crate::i18n::tr("离线模式下不能播放在线链接"),
                        ToastKind::Error,
                    );
                    self.playback_state = PlaybackState::Stopped;
                    return;
                }
                let http = HttpOptions::for_network(&self.settings.network).for_stream();
                let cache = self.stream_cache_path_for_url(url);
                self.engine_load_token = self.engine.load(
                    EngineSource::Url {
                        url: url.clone(),
                        cache,
                        http,
                        download_first: false,
                    },
                    start_secs,
                    autoplay,
                );
            }
            TrackSource::Bamboo { .. } => {
                self.start_online_load(track.clone(), start_secs, autoplay);
            }
        }
        self.arm_next();
        self.maybe_refill_station();
    }

    /// Queue an offline ReplayGain analysis when the file has no gain tags and
    /// no valid cache entry. The worker does the decode; playback continues.
    pub(crate) fn maybe_scan_replaygain(&mut self, track: &Track) {
        if !self.settings.features.replaygain
            || self.settings.playback.replaygain == crate::config::ReplayGainMode::Off
            || !self.settings.playback.replaygain_scan
        {
            return;
        }
        let path = match &track.source {
            TrackSource::Local { path } => path.clone(),
            // A cached online track is decoded as a local file, so it can use
            // the same gain cache keyed by the cache path.
            TrackSource::Bamboo { .. } => match self.cached_engine_source_for(track) {
                Some(EngineSource::Local(path)) => path,
                _ => return,
            },
            _ => return,
        };
        if !path.is_file()
            || self.replaygain_scanning.contains(&path)
            || self.replaygain_tagged.contains(&path)
            || crate::audio::loudness::cached_gain(&path).is_some()
        {
            return;
        }
        let key = path.to_string_lossy().into_owned();
        if self
            .replaygain_cache
            .get(&key)
            .is_some_and(|entry| entry.matches(&path))
        {
            return;
        }
        self.replaygain_scanning.insert(path.clone());
        self.loudness.scan(path.clone());
        tracing::debug!("queued ReplayGain scan for {}", path.display());
    }

    fn poll_loudness(&mut self) {
        while let Some(event) = self.loudness.poll() {
            self.replaygain_scanning.remove(&event.path);
            match event.result {
                Ok(Some(gain)) => {
                    let Ok(metadata) = std::fs::metadata(&event.path) else {
                        continue;
                    };
                    let modified = metadata
                        .modified()
                        .ok()
                        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|duration| duration.as_secs() as i64)
                        .unwrap_or(0);
                    let key = event.path.to_string_lossy().into_owned();
                    self.replaygain_cache.insert(
                        key,
                        crate::audio::loudness::CachedGain {
                            gain_db: gain,
                            size: metadata.len(),
                            modified,
                        },
                    );
                    crate::audio::loudness::set_cached_gain(event.path.clone(), gain);
                    self.replaygain_cache_dirty = true;
                    let current = self
                        .current_track
                        .as_ref()
                        .and_then(|track| track.source.path())
                        .is_some_and(|path| path == event.path);
                    if current {
                        self.engine.refresh_replaygain();
                    }
                    tracing::info!(
                        "ReplayGain scan: {} -> {:+.2} dB",
                        event.path.display(),
                        gain
                    );
                }
                Ok(None) => {
                    self.replaygain_tagged.insert(event.path);
                }
                Err(err) => {
                    tracing::debug!("ReplayGain scan failed for {}: {err}", event.path.display());
                }
            }
        }
        if self.replaygain_cache_dirty
            && self.last_replaygain_save.elapsed() > Duration::from_secs(3)
        {
            self.save_replaygain_cache();
        }
    }

    fn save_replaygain_cache(&mut self) {
        if !self.replaygain_cache_dirty {
            return;
        }
        crate::audio::loudness::save_cache(
            &self.paths.cache.join("replaygain.json"),
            &self.replaygain_cache,
        );
        self.replaygain_cache_dirty = false;
        self.last_replaygain_save = Instant::now();
    }

    /// Resolve an online track's playback URL on a worker thread, then hand the
    /// URL to the audio engine. Cached streams are opened as local files but
    /// metadata/lyrics are still refreshed in the background.
    fn start_online_load(&mut self, track: Track, start_secs: f64, autoplay: bool) {
        let TrackSource::Bamboo {
            source_id,
            base_url,
            ..
        } = &track.source
        else {
            return;
        };
        let source = self.settings.source(source_id).cloned().or_else(|| {
            (!base_url.trim().is_empty()).then(|| crate::config::SourceConfig {
                id: source_id.clone(),
                name: track.source_name.clone(),
                kind: crate::config::SourceKind::Online,
                base_url: base_url.clone(),
                ..crate::config::SourceConfig::default()
            })
        });
        let Some(source) = source else {
            self.toast(
                crate::trf!("找不到在线音源 {source_id}，请检查设置", source_id),
                ToastKind::Error,
            );
            self.playback_state = PlaybackState::Stopped;
            return;
        };
        let cache = self.online_cache_path(&track, source.quality);
        // A completed cache entry needs no network at all. Playing it directly
        // also avoids the local API's single-request bottleneck while another
        // track is streaming.
        if let Some(cached) = cache.as_ref().filter(|path| path.is_file()).cloned() {
            let region = self.translation_region();
            match crate::lyrics::load_for_audio(&cached, self.settings.lyrics.encoding, &region) {
                Ok(lyrics) => {
                    self.lyrics = Some(lyrics);
                    self.lyrics_version = self.lyrics_version.wrapping_add(1);
                    self.lyrics_error = None;
                }
                Err(err) => self.lyrics_error = Some(err),
            }
            self.lyrics_loading = false;
            self.engine_load_token =
                self.engine
                    .load(EngineSource::Local(cached), start_secs, autoplay);
            // The cached audio is usable offline, but its metadata and lyrics
            // may be outdated or missing. Refresh them in the background when
            // the network is available; the response is applied only while
            // this track is still current. Force the request when a rapid
            // skip left us without lyrics, otherwise the TTL can suppress the
            // only request that would have supplied them.
            if !self.settings.network.offline {
                let force = self.lyrics.is_none();
                self.maybe_request_online_info(&track, force);
            }
            return;
        }
        if self.settings.network.offline {
            self.lyrics_loading = false;
            self.lyrics_error = Some(crate::i18n::tr("离线模式下该歌曲尚未缓存").to_string());
            self.toast(
                crate::i18n::tr("离线模式下该歌曲尚未缓存"),
                ToastKind::Error,
            );
            self.playback_state = PlaybackState::Stopped;
            return;
        }
        let request = self.api.resolve(
            &source,
            &self.settings.network,
            &track,
            cache,
            start_secs,
            autoplay,
        );
        let Some(request) = request else {
            self.playback_state = PlaybackState::Stopped;
            return;
        };
        self.online_request_id = request;
        self.online_track_key = track.dedupe_key();
        self.online_loading = true;
        // `prepare` performs an info and a URL request; allow both to time out
        // before giving up so a stuck request cannot leave the UI spinning.
        let timeout = self.settings.network.timeout_secs.clamp(5, 60);
        self.online_deadline = Some(
            Instant::now() + Duration::from_secs(timeout.saturating_mul(2).saturating_add(10)),
        );
        self.playback_state = if autoplay {
            PlaybackState::Loading
        } else {
            PlaybackState::Paused
        };
        if self.settings.lyrics.auto_load {
            self.lyrics_loading = true;
        }
        self.ctx.request_repaint();
    }

    fn online_cache_path(&self, track: &Track, quality: crate::config::Quality) -> Option<PathBuf> {
        let TrackSource::Bamboo {
            source_id, song_id, ..
        } = &track.source
        else {
            return None;
        };
        self.online_cache_path_for(source_id, song_id, quality)
    }

    pub fn online_cache_path_for(
        &self,
        source_id: &str,
        song_id: &str,
        quality: crate::config::Quality,
    ) -> Option<PathBuf> {
        if !self.settings.features.stream_cache {
            return None;
        }
        let ext = match quality {
            crate::config::Quality::Flac => "flac",
            _ => "mp3",
        };
        let key = format!("{source_id}|{song_id}|{}", quality.api_value());
        let hash = blake3::hash(key.as_bytes()).to_hex();
        let dir = self.stream_cache_dir();
        let _ = std::fs::create_dir_all(&dir);
        Some(dir.join(format!("{hash}.{ext}")))
    }

    /// Cache path for a direct URL or HLS stream. The bytes are published only
    /// after the source reaches EOF; `.cache` keeps the hint neutral because
    /// the URL does not always carry a reliable audio extension.
    fn stream_cache_path_for_url(&self, url: &str) -> Option<PathBuf> {
        if !self.settings.features.stream_cache {
            return None;
        }
        let hash = blake3::hash(url.as_bytes()).to_hex();
        let dir = self.stream_cache_dir();
        let _ = std::fs::create_dir_all(&dir);
        Some(dir.join(format!("{hash}.cache")))
    }

    /// Set up the restored track without touching the decoder. Used at startup
    /// when the playlist will be re-imported first (or when audio is deferred).
    fn prepare_restore(&mut self, index: usize, start_secs: f64, autoplay: bool) {
        if index >= self.queue.items.len() {
            return;
        }
        self.disarm_next();
        self.clear_ab_loop_silent();
        let mut track = self.queue.items[index].clone();
        track.apply_filename_fallback();
        self.queue.set_current(index);
        self.current_track = Some(track.clone());
        self.current_duration = track.duration;
        self.cover_key.clear();
        self.deferred_start = Some(start_secs.max(0.0));
        self.request_current_metadata(&track);
        self.refresh_lyrics(&track);
        self.refresh_visuals();
        self.update_title();
        self.playback_state = if autoplay {
            PlaybackState::Loading
        } else {
            PlaybackState::Paused
        };
        self.session_dirty = true;
    }

    /// Restore a saved track as paused without touching the audio device or
    /// opening the file. The decoder starts on the first Play.
    pub fn restore_paused(&mut self, index: usize, start_secs: f64) {
        if index >= self.queue.items.len() {
            return;
        }
        self.disarm_next();
        self.clear_ab_loop_silent();
        self.engine.stop();
        let mut track = self.queue.items[index].clone();
        track.apply_filename_fallback();
        self.queue.set_current(index);
        self.current_track = Some(track.clone());
        self.current_duration = track.duration;
        self.cover_key.clear();
        self.deferred_start = Some(start_secs.max(0.0));
        self.request_current_metadata(&track);
        self.refresh_lyrics(&track);
        self.refresh_visuals();
        self.update_title();
        self.playback_state = PlaybackState::Paused;
        self.session_dirty = true;
    }

    fn request_current_metadata(&self, track: &Track) {
        if !self.settings.performance.metadata_mode.enabled() {
            return;
        }
        if !track.artist.is_empty() && !track.album.is_empty() && track.duration.is_some() {
            return;
        }
        if let Some(path) = track.source.path() {
            self.metadata.request(path.to_path_buf());
        }
    }

    /// Bulk additions only enqueue tag reads in "all tracks" mode. In the
    /// default current-track mode, thousands of files are queued instantly.
    fn request_background_metadata(&self, path: &Path) {
        if self.settings.performance.metadata_mode.background_all() {
            self.metadata.request(path.to_path_buf());
        }
    }

    pub fn toggle_play(&mut self) {
        match self.playback_state {
            PlaybackState::Playing => {
                self.engine.pause();
                self.playback_state = PlaybackState::Paused;
                self.session_dirty = true;
            }
            PlaybackState::Paused => {
                if let Some(start) = self.deferred_start.take() {
                    if let Some(index) = self.queue.current {
                        self.load_index(index, start, true);
                    }
                } else {
                    self.engine.play();
                    self.playback_state = PlaybackState::Playing;
                    self.arm_next();
                }
            }
            PlaybackState::Stopped => {
                if let Some(index) = self
                    .queue
                    .current
                    .or_else(|| (!self.queue.is_empty()).then_some(0))
                {
                    self.load_index(index, 0.0, true);
                } else {
                    self.open_files_dialog();
                }
            }
            PlaybackState::Loading => {
                // The deck may already be audible while the app still shows
                // the resolving state; honour the requested pause/resume
                // instead of ignoring the click.
                match self.engine.state() {
                    PlaybackState::Playing => {
                        self.engine.pause();
                        self.playback_state = PlaybackState::Paused;
                        self.session_dirty = true;
                    }
                    PlaybackState::Paused => {
                        self.engine.play();
                        self.playback_state = PlaybackState::Playing;
                        self.arm_next();
                    }
                    _ => {}
                }
            }
        }
    }

    /// Advance one step, skipping local files that have been deleted. Each
    /// skip is reported and playback continues with the next track.
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
                    track.source.path().map(|path| path.to_path_buf())
                }
            });
            if let Some(path) = missing {
                skipped += 1;
                tracing::info!("skipping missing track: {}", path.display());
                if skipped == 1 {
                    self.toast(
                        crate::trf!("文件不存在，已跳过: {}", path.display()),
                        ToastKind::Error,
                    );
                }
                if skipped >= total {
                    if total > 1 {
                        self.toast(crate::i18n::tr("没有可播放的曲目"), ToastKind::Error);
                    }
                    return false;
                }
                continue;
            }
            self.load_index(index, 0.0, true);
            return true;
        }
    }

    pub fn next_track(&mut self) {
        // A manual skip within the first 15 seconds counts against the track
        // (only when statistics are enabled and the song is long enough).
        if self.settings.features.statistics
            && self.position_secs() < 15.0
            && self.duration_secs() > 30.0
            && let Some(track) = self.current_track.as_ref()
            && track.source.is_local()
        {
            self.db.record_skip(track.dedupe_key());
        }
        let _ = self.advance_track(false);
    }

    pub fn prev_track(&mut self) {
        // The Previous button always goes to the previous track. Seeking back
        // to the start of the current track is available by clicking the
        // progress bar (or the 3-second rule no longer applies here).
        if let Some(index) = self.queue.back() {
            self.load_index(index, 0.0, true);
        }
    }

    pub fn advance_auto(&mut self) {
        if !self.settings.playback.auto_advance {
            self.playback_state = PlaybackState::Stopped;
            return;
        }
        if self.advance_track(true) {
            return;
        }
        self.playback_state = PlaybackState::Stopped;
        self.toast(self.t("audio.ended").to_string(), ToastKind::Info);
    }

    pub fn seek_to(&mut self, secs: f64) {
        if self.current_track.is_none() {
            return;
        }
        let dur = self.duration_secs();
        let target = if dur > 0.0 {
            secs.clamp(0.0, dur)
        } else {
            secs.max(0.0)
        };
        if let Some(start) = self.deferred_start.as_mut() {
            *start = target;
            self.seek_preview = None;
            self.session_dirty = true;
            return;
        }
        self.engine.seek(target);
        self.seek_preview = None;
        self.session_dirty = true;
    }

    pub fn set_volume(&mut self, volume: f32) {
        let v = volume.clamp(0.0, 1.0);
        self.settings.playback.volume = v;
        self.engine.set_volume(v);
        if v > 0.0 && self.settings.playback.muted {
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
        let target = !self.queue.shuffle;
        self.queue.set_shuffle(target);
        // A manual shuffle change supersedes any in-progress playlist order
        // restore.
        self.pending_shuffle_keys = None;
        self.session_dirty = true;
        self.arm_next();
        self.sync_tray();
    }

    /// Re-randomize the queue with a separate action (not the shuffle toggle).
    pub fn reroll_shuffle(&mut self) {
        if self.queue.is_empty() {
            self.toast(crate::i18n::tr("队列为空，无法重新随机"), ToastKind::Info);
            return;
        }
        self.pending_shuffle_keys = None;
        self.queue.reroll_shuffle();
        self.session_dirty = true;
        self.arm_next();
        self.sync_tray();
        self.toast(crate::i18n::tr("已重新随机播放列表"), ToastKind::Success);
    }

    pub fn cycle_repeat(&mut self) {
        self.queue.repeat = self.queue.repeat.cycle();
        self.session_dirty = true;
        self.arm_next();
        self.sync_tray();
    }

    // ------------------------------------------------------------------
    // P7: DSP, gapless/crossfade, A-B repeat, sleep timer
    // ------------------------------------------------------------------

    /// Push the current playback settings into the audio engine.
    pub fn apply_dsp(&self) {
        self.engine.set_dsp(dsp_config(&self.settings));
    }

    /// Cancel a pre-armed next deck (manual load, queue change, feature off).
    fn disarm_next(&mut self) {
        self.next_prefetch = None;
        if self.armed_next.take().is_some() || self.pending_auto.take().is_some() {
            self.engine.set_next(None, 0);
        }
    }

    /// Resolve a Bamboo track's source config, including tracks imported from
    /// a share link that only carry the base URL.
    fn online_source_for(&self, track: &Track) -> Option<crate::config::SourceConfig> {
        let TrackSource::Bamboo {
            source_id,
            base_url,
            ..
        } = &track.source
        else {
            return None;
        };
        self.settings.source(source_id).cloned().or_else(|| {
            (!base_url.trim().is_empty()).then(|| crate::config::SourceConfig {
                id: source_id.clone(),
                name: track.source_name.clone(),
                kind: crate::config::SourceKind::Online,
                base_url: base_url.clone(),
                ..crate::config::SourceConfig::default()
            })
        })
    }

    fn cached_engine_source_for(&self, track: &Track) -> Option<EngineSource> {
        let source = self.online_source_for(track)?;
        let cache = self.online_cache_path(track, source.quality)?;
        cache.is_file().then(|| EngineSource::Local(cache))
    }

    fn engine_source_for(&self, track: &Track) -> Option<EngineSource> {
        match &track.source {
            TrackSource::Local { path } => {
                path.is_file().then(|| EngineSource::Local(path.clone()))
            }
            TrackSource::Url { url } | TrackSource::Radio { url } => {
                if self.settings.network.offline {
                    return None;
                }
                Some(EngineSource::Url {
                    url: url.clone(),
                    cache: self.stream_cache_path_for_url(url),
                    http: HttpOptions::for_network(&self.settings.network).for_stream(),
                    download_first: false,
                })
            }
            // A completed stream cache can be pre-opened as a local file;
            // otherwise `arm_next` starts the async URL resolve below.
            TrackSource::Bamboo { .. } => {
                if self.settings.network.offline {
                    return None;
                }
                self.cached_engine_source_for(track)
            }
        }
    }

    /// Pre-open the next track in the engine when seamless playback or a
    /// crossfade is enabled. Called after every load, queue change and
    /// settings change; duplicate arms are suppressed by a signature.
    pub fn arm_next(&mut self) {
        if self.sleep_end_of_track
            || !matches!(
                self.playback_state,
                PlaybackState::Playing | PlaybackState::Loading
            )
        {
            self.disarm_next();
            return;
        }
        let dsp = dsp_config(&self.settings);
        // Repeat One must loop the same deck; no next source is involved.
        if self.queue.repeat == crate::model::RepeatMode::One {
            self.disarm_next();
            return;
        }
        let Some(index) = self.queue.peek_next(true) else {
            self.disarm_next();
            return;
        };
        let Some(track) = self.queue.items.get(index).cloned() else {
            self.disarm_next();
            return;
        };
        // Lyrics prefetch does not depend on gapless/crossfade being enabled.
        if self.settings.lyrics.prefetch_next {
            self.prefetch_lyrics_for(&track);
        }
        if !dsp.gapless && dsp.crossfade_secs <= 0.05 {
            self.disarm_next();
            return;
        }
        let signature = format!("{index}|{}", track.dedupe_key());
        // Measure the upcoming local track while the current one plays, so its
        // gain is ready by the time the deck starts.
        self.maybe_scan_replaygain(&track);
        let Some(source) = self.engine_source_for(&track) else {
            // Bamboo URLs need an async resolve before the engine can open
            // them. Start it now so a nearly-finished current track does not
            // fall back to the end-to-next reload path.
            if matches!(track.source, TrackSource::Bamboo { .. }) && !self.settings.network.offline
            {
                // Do not resolve the next URL at the start of a long track.
                // The periodic `last_next_arm_poll` in `logic` retries once
                // the remaining time enters this window.
                let duration = self.duration_secs();
                let remaining = duration - self.position_secs();
                if duration > 0.0
                    && remaining > NEXT_PREFETCH_WINDOW_SECS + dsp.crossfade_secs.max(0.0) as f64
                {
                    return;
                }
                if let Some((backoff_sig, until)) = self.next_prefetch_backoff.clone() {
                    if backoff_sig == signature {
                        if Instant::now() < until {
                            return;
                        }
                        self.next_prefetch_backoff = None;
                    }
                }
                if self
                    .next_prefetch
                    .as_ref()
                    .is_some_and(|(_, pending_index, pending_sig)| {
                        *pending_index == index && pending_sig == &signature
                    })
                {
                    return;
                }
                self.disarm_next();
                let Some(source_config) = self.online_source_for(&track) else {
                    return;
                };
                let cache = self.online_cache_path(&track, source_config.quality);
                if let Some(request) =
                    self.api
                        .resolve_next(&source_config, &self.settings.network, &track, cache)
                {
                    self.next_prefetch = Some((request, index, signature));
                    tracing::debug!(
                        "pre-resolving next online track {} (queue index {index})",
                        track.display_title()
                    );
                }
            } else {
                self.disarm_next();
            }
            return;
        };
        if self
            .armed_next
            .as_ref()
            .is_some_and(|(_, armed_index, armed_sig)| {
                *armed_index == index && armed_sig == &signature
            })
        {
            return;
        }
        let token = self.next_arm_token;
        self.next_arm_token = self.next_arm_token.wrapping_add(1);
        self.engine.set_next(Some(source), token);
        self.armed_next = Some((token, index, signature));
        self.pending_auto = Some((token, index));
    }

    /// Promote the queue cursor when the engine already switched to the
    /// pre-armed deck. No reload is issued: audio is playing continuously.
    fn adopt_auto_advanced(&mut self, token: u64, duration: Option<f64>) {
        let Some((pending_token, index)) = self.pending_auto.take() else {
            return;
        };
        if pending_token != token {
            return;
        }
        self.armed_next = None;
        if index >= self.queue.items.len() {
            return;
        }
        self.queue.set_current(index);
        let mut track = self.queue.items[index].clone();
        track.apply_filename_fallback();
        self.current_track = Some(track.clone());
        self.current_duration = duration.or(track.duration);
        self.record_station_history(&track);
        self.record_recent_file(&track);
        self.notify_track_change(&track);
        self.cover_key.clear();
        self.deferred_start = None;
        self.clear_ab_loop_silent();
        self.maybe_scan_replaygain(&track);
        self.request_current_metadata(&track);
        self.refresh_lyrics(&track);
        self.maybe_request_online_info(&track, false);
        self.refresh_visuals();
        self.update_title();
        self.session_dirty = true;
        if self.settings.features.statistics
            && let Some(db_track) = DbTrack::from_track(&track)
        {
            self.db.record_play(db_track);
        }
        self.playback_state = self.engine.state();
        if self.sleep_end_of_track {
            self.finish_sleep_after_track();
            return;
        }
        self.arm_next();
    }

    pub fn ab_loop_active(&self) -> bool {
        self.settings.features.ab_repeat
            && self
                .ab_start
                .zip(self.ab_end)
                .is_some_and(|(start, end)| end > start + 0.25)
    }

    pub fn set_ab_start(&mut self) {
        let pos = self.position_secs();
        self.ab_start = Some(pos);
        if let Some(end) = self.ab_end
            && end <= pos + 0.25
        {
            self.ab_end = None;
        }
        self.toast(
            crate::trf!("A 点已设为 {}", format_time(pos)),
            ToastKind::Success,
        );
    }

    pub fn set_ab_end(&mut self) {
        let pos = self.position_secs();
        let start = self.ab_start.unwrap_or(0.0);
        if pos <= start + 0.25 {
            self.toast(crate::i18n::tr("B 点必须在 A 点之后"), ToastKind::Error);
            return;
        }
        self.ab_start = Some(start);
        self.ab_end = Some(pos);
        self.toast(
            crate::trf!("A-B 循环：{} – {}", format_time(start), format_time(pos)),
            ToastKind::Success,
        );
    }

    pub fn clear_ab_loop(&mut self) {
        if self.ab_start.take().is_some() || self.ab_end.take().is_some() {
            self.toast(crate::i18n::tr("已清除 A-B 循环"), ToastKind::Info);
        }
    }

    fn clear_ab_loop_silent(&mut self) {
        self.ab_start = None;
        self.ab_end = None;
    }

    fn tick_ab_loop(&mut self) {
        if !self.ab_loop_active() || !self.is_playing() {
            return;
        }
        let (Some(start), Some(end)) = (self.ab_start, self.ab_end) else {
            return;
        };
        if self.position_secs() < end || self.ab_last_seek.elapsed() < Duration::from_millis(250) {
            return;
        }
        self.ab_last_seek = Instant::now();
        self.engine.seek(start);
        self.seek_preview = None;
    }

    pub fn set_sleep_minutes(&mut self, minutes: u64) {
        if minutes == 0 {
            self.clear_sleep_timer();
            return;
        }
        self.sleep_end_of_track = false;
        self.sleep_deadline = Some(Instant::now() + Duration::from_secs(minutes * 60));
        self.toast(
            crate::trf!("睡眠定时：{minutes} 分钟后暂停", minutes),
            ToastKind::Success,
        );
    }

    pub fn set_sleep_after_track(&mut self) {
        self.sleep_deadline = None;
        self.sleep_end_of_track = true;
        self.disarm_next();
        self.toast(
            crate::i18n::tr("睡眠定时：当前曲目结束后暂停"),
            ToastKind::Success,
        );
    }

    pub fn clear_sleep_timer(&mut self) {
        let active = self.sleep_deadline.take().is_some() || self.sleep_end_of_track;
        self.sleep_end_of_track = false;
        if active {
            self.toast(crate::i18n::tr("已取消睡眠定时"), ToastKind::Info);
        }
    }

    fn finish_sleep_after_track(&mut self) {
        self.sleep_end_of_track = false;
        self.engine.pause();
        self.playback_state = PlaybackState::Paused;
        self.session_dirty = true;
        self.toast(
            crate::i18n::tr("睡眠定时：当前曲目结束，已暂停"),
            ToastKind::Info,
        );
    }

    fn tick_sleep_timer(&mut self) {
        let Some(deadline) = self.sleep_deadline else {
            return;
        };
        if Instant::now() < deadline {
            self.ctx.request_repaint_after(Duration::from_millis(500));
            return;
        }
        self.sleep_deadline = None;
        self.engine.pause();
        self.playback_state = PlaybackState::Paused;
        self.session_dirty = true;
        self.toast(crate::i18n::tr("睡眠定时结束，已暂停播放"), ToastKind::Info);
    }

    pub fn remove_index(&mut self, index: usize) {
        let was_current = self.queue.current == Some(index);
        self.queue.remove(index);
        self.session_dirty = true;
        if was_current {
            self.engine.stop();
            self.deferred_start = None;
            self.current_track = None;
            self.cover = None;
            self.background = None;
            self.cover_key.clear();
            self.lyrics_service.cancel();
            self.lyrics = None;
            self.lyrics_loading = false;
            self.lyrics_error = None;
            self.lyrics_key.clear();
            self.current_duration = None;
            if let Some(next) = self
                .queue
                .current
                .or_else(|| (!self.queue.is_empty()).then_some(0))
            {
                self.load_index(next, 0.0, true);
            } else {
                self.update_title();
            }
        } else {
            self.arm_next();
        }
    }

    pub fn clear_queue(&mut self) {
        self.disarm_next();
        self.station_active = false;
        self.station_fetching = false;
        self.station_request = 0;
        self.station_keys.clear();
        self.engine.stop();
        self.deferred_start = None;
        self.queue.clear();
        self.current_track = None;
        self.current_duration = None;
        self.cover = None;
        self.background = None;
        self.cover_key.clear();
        self.lyrics_service.cancel();
        self.lyrics = None;
        self.lyrics_loading = false;
        self.lyrics_error = None;
        self.lyrics_key.clear();
        self.playback_state = PlaybackState::Stopped;
        self.current_playlist = None;
        self.queue_filter.clear();
        self.resume_after_import = None;
        self.session_dirty = true;
        self.update_title();
    }

    // ------------------------------------------------------------------
    // Adding files
    // ------------------------------------------------------------------

    /// Run a blocking native dialog on a worker thread and apply its result on
    /// the UI thread. Calling `IFileOpenDialog::Show` from inside the egui
    /// paint pass blocks the app's message loop and intermittently leaves parts
    /// of the dialog unpainted until it is moved or resized.
    fn spawn_file_dialog<T, P, C>(&mut self, pick: P, apply: C)
    where
        T: Send + 'static,
        P: FnOnce() -> T + Send + 'static,
        C: FnOnce(&mut App, T) + Send + 'static,
    {
        if self.file_dialog_open.swap(true, Ordering::Relaxed) {
            self.toast(
                crate::i18n::tr("请先完成当前的文件选择"),
                ToastKind::Info,
            );
            return;
        }
        let ctx = self.ctx.clone();
        let tx = self.task_tx.clone();
        let flag = self.file_dialog_open.clone();
        let spawned = std::thread::Builder::new()
            .name("bamboo-file-dialog".to_string())
            .spawn(move || {
                let result = pick();
                let task: AppTask = Box::new(move |app: &mut App| {
                    flag.store(false, Ordering::Relaxed);
                    apply(app, result);
                });
                if tx.send(task).is_ok() {
                    ctx.request_repaint();
                }
            })
            .is_ok();
        if !spawned {
            self.file_dialog_open.store(false, Ordering::Relaxed);
        }
    }

    /// Apply work posted by background threads (file dialog results). Runs on
    /// the UI thread at the start of the root logic pass.
    fn poll_background_tasks(&mut self) {
        let tasks: Vec<AppTask> = self.task_rx.try_iter().collect();
        for task in tasks {
            task(self);
        }
    }

    pub fn open_files_dialog(&mut self) {
        let play_first = self.queue.current.is_none();
        self.spawn_file_dialog(
            || {
                rfd::FileDialog::new()
                    .add_filter(
                        "Audio",
                        &["mp3", "ogg", "wav", "flac", "m4a", "aac", "opus"],
                    )
                    .pick_files()
            },
            move |app, files| {
                if let Some(files) = files {
                    app.add_paths(files, play_first);
                }
            },
        );
    }

    pub fn open_folder_dialog(&mut self) {
        let play_first = self.queue.current.is_none();
        self.spawn_file_dialog(
            || rfd::FileDialog::new().pick_folder(),
            move |app, folder| {
                if let Some(folder) = folder {
                    app.add_paths(vec![folder], play_first);
                }
            },
        );
    }

    pub fn add_paths(&mut self, paths: Vec<PathBuf>, play_first: bool) {
        let extensions = self.settings.library.extensions.clone();
        let mut files: Vec<PathBuf> = Vec::new();
        let mut dirs: Vec<PathBuf> = Vec::new();
        let mut playlists: Vec<PathBuf> = Vec::new();
        let mut skipped = 0usize;

        for path in paths {
            if path.is_dir() {
                dirs.push(path);
            } else if is_audio_file(&path, &extensions) {
                files.push(path);
            } else if path.is_file() {
                let ext = path
                    .extension()
                    .map(|e| e.to_string_lossy().to_ascii_lowercase())
                    .unwrap_or_default();
                if matches!(ext.as_str(), "mp3list" | "m3u" | "m3u8") {
                    playlists.push(path);
                } else {
                    skipped += 1;
                }
            }
        }

        if let Some(playlist) = playlists.into_iter().next() {
            self.import_playlist(playlist, true);
            return;
        }

        let mut added = 0usize;
        if !files.is_empty() {
            added += self.add_audio_files(files, play_first);
        }
        let had_dirs = !dirs.is_empty();
        if had_dirs {
            self.start_scan(dirs, play_first);
        }
        if added > 0 {
            self.toast(
                format!("{}: {}", self.t("toast.files_added"), added),
                ToastKind::Success,
            );
        } else if !had_dirs && skipped > 0 {
            self.toast(crate::i18n::tr("没有找到可播放的音频文件"), ToastKind::Info);
        }
    }

    fn add_audio_files(&mut self, mut files: Vec<PathBuf>, play_first: bool) -> usize {
        files.sort_by_key(|p| p.to_string_lossy().to_ascii_lowercase());
        files.dedup_by(|a, b| normalize_path(a) == normalize_path(b));
        let mut added = 0usize;
        let mut first_added: Option<usize> = None;
        for path in files {
            let before = self.queue.len();
            if self.queue.add(Track::local(path.clone())) {
                added += 1;
                self.request_background_metadata(&path);
                if first_added.is_none() {
                    first_added = Some(before);
                }
            }
        }
        self.session_dirty |= added > 0;
        if play_first || (self.queue.current.is_none() && added > 0) {
            if let Some(index) = first_added.or(self.queue.current) {
                self.load_index(index, 0.0, true);
            }
        }
        added
    }

    /// Recursively scans directories on a worker thread and streams the queue
    /// in batches. This is the path used for very large trees.
    pub fn start_scan(&mut self, roots: Vec<PathBuf>, play_first: bool) {
        let roots = roots.into_iter().map(ScanRoot::new).collect();
        self.start_scan_roots(roots, play_first);
    }

    /// Scan roots may carry a `#type#*.mp3` filter from a playlist line.
    pub fn start_scan_roots(&mut self, roots: Vec<ScanRoot>, play_first: bool) {
        if roots.is_empty() {
            return;
        }
        self.scan.cancel();
        self.scan_id = self.scan.start(
            roots,
            self.settings.library.extensions.clone(),
            self.settings.library.follow_symlinks,
        );
        self.scan_active = true;
        self.scan_found = 0;
        self.scan_added = 0;
        self.scan_current.clear();
        self.scan_play_first = play_first;
        self.scan_played_first = false;
        self.scan_started = Instant::now();
        self.toast(crate::i18n::tr("正在后台扫描目录…"), ToastKind::Info);
    }

    pub fn cancel_scan(&mut self) {
        self.scan.cancel();
        self.scan_active = false;
        self.scan_id = 0;
        self.scan_current.clear();
        self.pending_shuffle_keys = None;
        if self.queue.shuffle {
            self.queue.shuffle_append = true;
        }
    }

    fn poll_scan(&mut self) {
        // At most two 500-file batches per frame keep queue insertion smooth.
        let mut batches = 0;
        while batches < 2 {
            let Some(event) = self.scan.try_recv() else {
                break;
            };
            match event {
                ScanEvent::Progress { id, found, current } if id == self.scan_id => {
                    self.scan_found = found;
                    self.scan_current = current;
                    self.ctx.request_repaint();
                }
                ScanEvent::Batch { id, paths } if id == self.scan_id => {
                    batches += 1;
                    let mut first_new = None;
                    for path in paths {
                        let before = self.queue.len();
                        if self.queue.add(Track::local(path.clone())) {
                            self.scan_added += 1;
                            self.request_background_metadata(&path);
                            if first_new.is_none() {
                                first_new = Some(before);
                            }
                        }
                    }
                    if self.scan_play_first && !self.scan_played_first {
                        if let Some(index) = first_new
                            .or(self.queue.current)
                            .or_else(|| (!self.queue.is_empty()).then_some(0))
                        {
                            self.load_index(index, 0.0, true);
                            self.scan_played_first = true;
                        }
                    }
                    self.session_dirty = true;
                    self.apply_pending_shuffle_order();
                    self.try_resume_after_import();
                }
                ScanEvent::Finished {
                    id,
                    total,
                    cancelled,
                    elapsed_ms,
                } if id == self.scan_id => {
                    tracing::info!(
                        "scan finished: total={total} cancelled={cancelled} elapsed={elapsed_ms}ms"
                    );
                    self.scan_active = false;
                    self.scan_current.clear();
                    self.apply_pending_shuffle_order();
                    self.pending_shuffle_keys = None;
                    if self.queue.shuffle {
                        // Everything already queued is the current cycle; any
                        // later additions wait until it finishes.
                        self.queue.shuffle_append = true;
                    }
                    if cancelled {
                        self.toast(
                            crate::trf!("扫描已取消，已加入 {} 首", self.scan_added),
                            ToastKind::Info,
                        );
                    } else if total == 0 {
                        self.toast(crate::i18n::tr("没有找到可播放的音频文件"), ToastKind::Info);
                    } else {
                        self.toast(
                            crate::trf!(
                                "扫描完成：{} 首（{} 秒）",
                                total,
                                (elapsed_ms / 1000).max(1)
                            ),
                            ToastKind::Success,
                        );
                    }
                    self.try_resume_after_import();
                }
                ScanEvent::Error { id, message } if id == self.scan_id => {
                    self.toast(message, ToastKind::Error);
                }
                _ => {}
            }
        }
        if self.scan_active {
            self.ctx
                .request_repaint_after(Duration::from_millis(100));
        }
    }

    pub fn browse_folder_dialog(&mut self) {
        self.spawn_file_dialog(
            || rfd::FileDialog::new().pick_folder(),
            |app, folder| {
                let Some(folder) = folder else {
                    return;
                };
                let mut added = false;
                if !app.settings.library.folders.iter().any(|f| f == &folder) {
                    app.settings.library.folders.push(folder.clone());
                    app.settings_dirty = true;
                    added = true;
                }
                if added {
                    app.sync_library_watch();
                }
                app.open_browser(folder);
            },
        );
    }

    pub fn open_browser(&mut self, path: PathBuf) {
        self.browser.current = Some(path.clone());
        self.browser.loading = true;
        self.browser.error = None;
        self.browser.entries.clear();
        self.browser.truncated = false;
        self.browse
            .request(path, self.settings.library.extensions.clone());
        self.ctx.request_repaint();
    }

    pub fn browser_up(&mut self) {
        let Some(current) = self.browser.current.clone() else {
            return;
        };
        if let Some(parent) = current.parent() {
            self.open_browser(parent.to_path_buf());
        }
    }

    fn poll_browser(&mut self) {
        for result in self.browse.poll() {
            if self.browser.current.as_ref() == Some(&result.path) {
                self.browser.entries = result.entries;
                self.browser.error = result.error;
                self.browser.truncated = result.truncated;
                self.browser.loading = false;
                self.ctx.request_repaint();
            }
        }
    }

    pub fn play_browser_file(&mut self, path: PathBuf) {
        let key = Track::local(path.clone()).dedupe_key();
        if let Some(index) = self.queue.position_of_key(&key) {
            self.load_index(index, 0.0, true);
            return;
        }
        let before = self.queue.len();
        if self.queue.add(Track::local(path.clone())) {
            self.request_background_metadata(&path);
            self.session_dirty = true;
            self.load_index(before, 0.0, true);
        }
    }

    // ------------------------------------------------------------------
    // Library database (P6)
    // ------------------------------------------------------------------

    fn poll_db(&mut self) {
        while let Some(event) = self.db.poll() {
            match event {
                DbEvent::Error(message) => {
                    // Never leave the UI stuck on a spinner when the database
                    // could not be opened or a request failed.
                    self.db_index_active = false;
                    self.db_tools_busy = false;
                    self.library_loading = false;
                    self.db_stats_loading = false;
                    self.toast(message, ToastKind::Error);
                }
                DbEvent::IndexProgress {
                    indexed, current, ..
                } => {
                    self.db_index_progress = indexed;
                    self.db_index_current = current;
                    self.ctx.request_repaint();
                }
                DbEvent::IndexDone {
                    indexed,
                    missing,
                    elapsed_ms,
                    cancelled,
                    ..
                } => {
                    self.db_index_active = false;
                    self.db_index_current.clear();
                    if cancelled {
                        self.toast(
                            crate::trf!("曲库索引已取消：已写入 {indexed} 首", indexed),
                            ToastKind::Info,
                        );
                    } else {
                        self.toast(
                            crate::trf!(
                                "曲库索引完成：{indexed} 首（{} 秒）{}",
                                indexed,
                                (elapsed_ms / 1000).max(1),
                                if missing > 0 {
                                    crate::trf!("，{missing} 个文件缺失", missing)
                                } else {
                                    String::new()
                                }
                            ),
                            ToastKind::Success,
                        );
                    }
                    self.db.stats();
                    if self.library_tab == views::LibraryTab::Index {
                        self.search_library(false);
                    }
                }
                DbEvent::QueryDone {
                    request_id,
                    entries,
                    total,
                } => {
                    if request_id != self.library_query_id {
                        continue;
                    }
                    self.library_total = total;
                    self.library_rows = entries;
                    self.library_loading = false;
                    self.ctx.request_repaint();
                }
                DbEvent::StatsDone(stats) => {
                    self.db_stats = Some(*stats);
                    self.db_stats_loading = false;
                    self.ctx.request_repaint();
                }
                DbEvent::DuplicatesDone(groups) => {
                    self.db_duplicates = groups;
                    self.db_tools_busy = false;
                    self.ctx.request_repaint();
                }
                DbEvent::MissingDone(entries) => {
                    self.db_missing = entries;
                    self.db_tools_busy = false;
                    self.ctx.request_repaint();
                }
                DbEvent::SmartDone { kind, entries } => {
                    self.apply_smart_playlist(kind, entries);
                }
                DbEvent::Changed => {
                    if self.db_stats.is_some() {
                        self.db.stats();
                    }
                    if self.library_tab == views::LibraryTab::Index {
                        self.search_library(false);
                    }
                }
            }
        }
    }

    /// Index every configured music folder into the SQLite library.
    pub fn start_library_index(&mut self) {
        if self.db_index_active {
            return;
        }
        let roots = self.settings.library.folders.clone();
        if roots.is_empty() {
            self.toast(crate::i18n::tr("请先在设置中添加音乐目录"), ToastKind::Info);
            return;
        }
        self.db_index_active = true;
        self.db_index_progress = 0;
        self.db_index_current.clear();
        self.db.index_roots(
            roots,
            self.settings.library.extensions.clone(),
            self.settings.library.follow_symlinks,
        );
        self.toast(crate::i18n::tr("正在建立曲库索引…"), ToastKind::Info);
    }

    /// Remove a configured music folder from the settings and the watcher.
    /// Index rows are kept (they may still be reachable through another root).
    pub fn remove_library_folder(&mut self, path: &std::path::Path) {
        self.settings.library.folders.retain(|root| root != path);
        self.settings_dirty = true;
        self.sync_library_watch();
        self.toast(
            crate::trf!("已移除音乐目录: {}", path.display()),
            ToastKind::Info,
        );
        self.ctx.request_repaint();
    }

    /// Keep the recursive directory watcher in sync with the settings.
    pub fn sync_library_watch(&mut self) {
        let enabled = self.settings.library.auto_watch;
        let roots = if enabled {
            self.settings.library.folders.clone()
        } else {
            Vec::new()
        };
        let extensions = if self.settings.library.extensions.is_empty() {
            crate::config::LibrarySettings::default().extensions
        } else {
            self.settings.library.extensions.clone()
        };
        if self.library_watch_enabled == enabled
            && self.library_watch_roots == roots
            && self.library_watch_extensions == extensions
        {
            return;
        }
        self.library_watch_enabled = enabled;
        self.library_watch_roots = roots.clone();
        self.library_watch_extensions = extensions.clone();
        if enabled && !roots.is_empty() {
            self.library_watch_error_shown = false;
            self.library_watch.set_roots(roots, extensions);
        } else {
            self.library_watch.stop();
        }
    }

    /// Apply incremental create/modify/delete notifications to the index.
    fn poll_library_watch(&mut self) {
        let events = self.library_watch.poll();
        if events.is_empty() {
            return;
        }
        for event in events {
            match event {
                WatchEvent::Changed(paths) => {
                    let mut changed = Vec::new();
                    let mut removed = Vec::new();
                    for path in paths {
                        if path.is_file() {
                            changed.push(path);
                        } else {
                            removed.push(Track::local(path).dedupe_key());
                        }
                    }
                    if !changed.is_empty() {
                        tracing::debug!("library watch indexing {} files", changed.len());
                        self.db.index_paths(changed);
                    }
                    if !removed.is_empty() {
                        tracing::debug!("library watch removing {} keys", removed.len());
                        self.db.remove_keys(removed);
                    }
                }
                WatchEvent::Error(message) => {
                    tracing::warn!("library watch: {message}");
                    if !self.library_watch_error_shown {
                        self.library_watch_error_shown = true;
                        self.toast(message, ToastKind::Error);
                    }
                }
            }
        }
    }

    /// Stop the current index run; the worker reports the partial result.
    pub fn cancel_library_index(&mut self) {
        if !self.db_index_active {
            return;
        }
        self.db.cancel_index();
        self.db_index_current = crate::i18n::tr("正在取消…").to_string();
    }

    /// Query the index. `append` keeps the current rows and loads the next
    /// page; otherwise the first page replaces them.
    pub fn search_library(&mut self, append: bool) {
        if !append {
            self.library_page = 0;
        }
        let offset = if append { self.library_rows.len() } else { 0 };
        self.library_query_id = self.library_query_id.wrapping_add(1);
        self.library_loading = true;
        self.db.query(
            self.library_query_id,
            LibraryQuery {
                search: self.library_search.clone(),
                sort: self.library_sort,
                offset,
                limit: 300,
            },
        );
        self.ctx.request_repaint();
    }

    pub fn request_library_stats(&mut self) {
        if !self.settings.features.statistics {
            return;
        }
        self.db_stats_loading = true;
        self.db.stats();
    }

    pub fn find_duplicates(&mut self) {
        if !self.settings.features.duplicate_finder {
            return;
        }
        self.db_tools_busy = true;
        self.db.duplicates(self.settings.library.dedupe_mode);
    }

    pub fn check_missing(&mut self) {
        if !self.settings.features.duplicate_finder {
            return;
        }
        self.db_tools_busy = true;
        self.db.check_missing();
    }

    pub fn remove_library_keys(&mut self, keys: Vec<String>) {
        if keys.is_empty() {
            return;
        }
        self.db.remove_keys(keys);
        self.toast(crate::i18n::tr("已从曲库移除"), ToastKind::Success);
    }

    pub fn clear_missing_entries(&mut self) {
        self.db.clear_missing();
        self.toast(crate::i18n::tr("已移除全部缺失条目"), ToastKind::Success);
    }

    pub fn reset_play_statistics(&mut self) {
        self.db.reset_stats();
        self.toast(crate::i18n::tr("播放统计已清空"), ToastKind::Success);
    }

    /// Ask the database for a smart playlist; the first result is played when
    /// `play` is true, otherwise the tracks are only appended to the queue.
    pub fn load_smart_playlist(&mut self, kind: SmartKind, play: bool) {
        if !self.settings.features.smart_playlists {
            self.toast(
                crate::i18n::tr("智能歌单已在功能开关中关闭"),
                ToastKind::Info,
            );
            return;
        }
        if self.smart_pending.is_some() {
            self.toast(crate::i18n::tr("正在加载上一个智能歌单…"), ToastKind::Info);
            return;
        }
        self.smart_pending = Some((kind, play));
        let limit = if kind == SmartKind::Random { 50 } else { 500 };
        self.db.smart(kind, limit);
        self.toast(
            crate::trf!("正在加载「{}」…", kind.label()),
            ToastKind::Info,
        );
    }

    fn apply_smart_playlist(&mut self, kind: SmartKind, entries: Vec<LibraryEntry>) {
        let Some((pending, play)) = self.smart_pending.take() else {
            return;
        };
        if pending != kind {
            return;
        }
        if entries.is_empty() {
            self.toast(
                crate::trf!("「{}」没有符合条件的曲目", kind.label()),
                ToastKind::Info,
            );
            return;
        }
        let mut first = None;
        let mut added = 0usize;
        for entry in entries {
            if entry.missing {
                continue;
            }
            let track = entry.to_track();
            let key = track.dedupe_key();
            if let Some(index) = self.queue.position_of_key(&key) {
                if first.is_none() {
                    first = Some(index);
                }
                continue;
            }
            if first.is_none() {
                first = Some(self.queue.len());
            }
            if self.queue.add(track) {
                added += 1;
            }
        }
        self.session_dirty = true;
        if play && let Some(index) = first {
            self.load_index(index, 0.0, true);
        }
        self.toast(
            crate::trf!("「{}」已加入 {added} 首", kind.label(), added),
            ToastKind::Success,
        );
    }

    pub fn play_library_entry(&mut self, entry: &LibraryEntry) {
        if entry.missing {
            self.toast(crate::i18n::tr("文件已缺失"), ToastKind::Error);
            return;
        }
        let track = entry.to_track();
        let key = track.dedupe_key();
        if let Some(index) = self.queue.position_of_key(&key) {
            self.load_index(index, 0.0, true);
            return;
        }
        let before = self.queue.len();
        if self.queue.add(track) {
            self.session_dirty = true;
            self.load_index(before, 0.0, true);
        }
    }

    pub fn queue_library_entries(&mut self, entries: &[LibraryEntry]) {
        let mut added = 0usize;
        for entry in entries {
            if !entry.missing && self.queue.add(entry.to_track()) {
                added += 1;
            }
        }
        if added > 0 {
            self.session_dirty = true;
            self.toast(
                crate::trf!("已加入队列：{added} 首", added),
                ToastKind::Success,
            );
        } else {
            self.toast(crate::i18n::tr("没有可加入的曲目"), ToastKind::Info);
        }
    }

    pub fn import_playlist_dialog(&mut self) {
        self.spawn_file_dialog(
            || {
                rfd::FileDialog::new()
                    .add_filter("Bamboo Playlist", &["mp3list"])
                    .add_filter("M3U", &["m3u", "m3u8"])
                    .pick_file()
            },
            |app, path| {
                if let Some(path) = path {
                    app.import_playlist(path, true);
                }
            },
        );
    }

    pub fn export_playlist_dialog(&mut self) {
        if self.queue.is_empty() {
            self.toast(crate::i18n::tr("当前队列为空"), ToastKind::Info);
            return;
        }
        let default_name = self
            .current_playlist
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "playlist.mp3list".to_string());
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Bamboo Playlist", &["mp3list"])
            .set_file_name(default_name)
            .save_file()
        {
            match crate::playlist::mp3list::export(
                &path,
                &self.queue.items,
                self.settings.playlist.export_utf8_bom,
            ) {
                Ok(count) => self.toast(
                    crate::trf!("{}: {} 首", self.t("toast.exported"), count),
                    ToastKind::Success,
                ),
                Err(err) => self.toast(err, ToastKind::Error),
            }
        }
    }

    pub fn rescan_current_playlist(&mut self) {
        if let Some(path) = self.current_playlist.clone() {
            self.import_playlist(path, true);
        }
    }

    /// Disk existence is checked at most every few seconds so the sidebar can
    /// mark deleted playlists without stat-ing every file each frame.
    fn refresh_playlist_status(&mut self) {
        if self
            .playlist_check_at
            .is_some_and(|checked| checked.elapsed() < Duration::from_secs(10))
        {
            return;
        }
        self.playlist_missing.clear();
        for entry in &self.session.playlists {
            if !entry.path.is_file() {
                self.playlist_missing.insert(normalize_path(&entry.path));
            }
        }
        self.playlist_check_at = Some(Instant::now());
    }

    fn forget_saved_playlist(&mut self, path: &Path) {
        if self.session.forget_playlist(path) {
            self.playlist_check_at = None;
            self.session_dirty = true;
            self.toast(crate::i18n::tr("已从“我的歌单”移除"), ToastKind::Info);
        }
    }

    fn begin_rename_saved_playlist(&mut self, path: PathBuf) {
        let name = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.playlist_rename = Some(PlaylistRename {
            path,
            name,
            error: None,
        });
        self.ctx.request_repaint();
    }

    /// Rename the playlist file on disk and update the saved-playlist entry,
    /// the current playlist path and the missing-file cache.
    fn commit_playlist_rename(&mut self) {
        let Some(mut dialog) = self.playlist_rename.take() else {
            return;
        };
        let typed = dialog.name.trim().trim_end_matches(['.', ' ']).to_string();
        if typed.is_empty() {
            dialog.error = Some(crate::i18n::tr("请输入新的歌单名称").to_string());
            self.playlist_rename = Some(dialog);
            return;
        }
        if typed.chars().any(|ch| {
            matches!(ch, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || ch.is_control()
        }) {
            dialog.error = Some(crate::i18n::tr("名称不能包含 < > : \" / \\ | ? *").to_string());
            self.playlist_rename = Some(dialog);
            return;
        }
        let old_extension = dialog
            .path
            .extension()
            .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
            .filter(|ext| matches!(ext.as_str(), "mp3list" | "m3u" | "m3u8"))
            .unwrap_or_else(|| "mp3list".to_string());
        let typed_path = Path::new(&typed);
        let typed_extension = typed_path
            .extension()
            .map(|ext| ext.to_string_lossy().to_ascii_lowercase());
        if let Some(extension) = typed_extension.as_ref()
            && !matches!(extension.as_str(), "mp3list" | "m3u" | "m3u8")
        {
            dialog.error = Some(crate::trf!(
                "歌单扩展名必须是 .mp3list / .m3u / .m3u8（当前为 .{extension}）",
                extension
            ));
            self.playlist_rename = Some(dialog);
            return;
        }
        let file_name = if typed_extension.is_some() {
            typed.clone()
        } else {
            format!("{typed}.{old_extension}")
        };
        let target = dialog.path.with_file_name(file_name);
        let old_key = normalize_path(&dialog.path);
        let new_key = normalize_path(&target);
        if old_key == new_key {
            if dialog.path == target {
                self.playlist_rename = None;
                return;
            }
            // Case-only rename: the normalized keys match but the target
            // spelling differs, so continue without the existence check.
        } else if target.exists() {
            dialog.error = Some(crate::i18n::tr("同名歌单文件已存在").to_string());
            self.playlist_rename = Some(dialog);
            return;
        }
        if let Err(err) = std::fs::rename(&dialog.path, &target) {
            dialog.error = Some(crate::trf!("重命名失败: {err}", err));
            self.playlist_rename = Some(dialog);
            return;
        }
        for entry in &mut self.session.playlists {
            if normalize_path(&entry.path) == old_key {
                entry.path = target.clone();
                entry.name = target
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
                    .unwrap_or_default();
            }
        }
        if self
            .current_playlist
            .as_ref()
            .is_some_and(|path| normalize_path(path) == old_key)
        {
            self.current_playlist = Some(target.clone());
        }
        self.playlist_check_at = None;
        self.session_dirty = true;
        self.toast(
            crate::trf!("歌单已重命名为「{}」", target.display()),
            ToastKind::Success,
        );
        self.ctx.request_repaint();
    }

    fn playlist_rename_dialog(&mut self, ctx: &Context) {
        let Some(mut dialog) = self.playlist_rename.take() else {
            return;
        };
        let colors = self.colors();
        let mut open = true;
        let mut submit = false;
        let mut cancel = false;
        egui::Window::new(crate::i18n::tr("重命名歌单"))
            .id(egui::Id::new("bamboo-playlist-rename"))
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .anchor(egui::Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(crate::trf!("原文件：{}", dialog.path.display()))
                        .size(10.5)
                        .color(colors.text_faint),
                );
                ui.add_space(6.0);
                let response = ui.add(
                    egui::TextEdit::singleline(&mut dialog.name)
                        .desired_width(340.0)
                        .hint_text(crate::i18n::tr("歌单名称")),
                );
                if response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
                    submit = true;
                }
                ui.add_space(3.0);
                ui.label(
                    RichText::new(crate::i18n::tr(
                        "只修改文件名，歌单内容不变；不填写扩展名时沿用原扩展名",
                    ))
                    .size(10.0)
                    .color(colors.text_faint),
                );
                if let Some(error) = dialog.error.as_ref() {
                    ui.add_space(4.0);
                    ui.label(RichText::new(error).size(11.0).color(colors.danger));
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if widgets::primary_button(ui, crate::i18n::tr("重命名"), &colors).clicked()
                    {
                        submit = true;
                    }
                    if widgets::secondary_button(ui, crate::i18n::tr("取消"), &colors).clicked() {
                        cancel = true;
                    }
                });
            });
        if submit {
            self.playlist_rename = Some(dialog);
            self.commit_playlist_rename();
        } else if cancel || !open {
            self.playlist_rename = None;
        } else {
            self.playlist_rename = Some(dialog);
        }
    }

    /// Re-reads a `.mp3list` and re-expands every `#SEARCH#` directory. The
    /// current track and position are captured first and matched by path key
    /// after the new queue has been built, so added/removed files do not shift
    /// the resume point.
    pub fn import_playlist(&mut self, path: PathBuf, replace: bool) {
        let parsed = match crate::playlist::mp3list::parse(&path) {
            Ok(parsed) => parsed,
            Err(err) => {
                self.toast(err, ToastKind::Error);
                return;
            }
        };

        tracing::info!(
            "playlist {}: {} dirs, {} files, {} urls, {} online, {} missing ({})",
            path.display(),
            parsed.dirs.len(),
            parsed.files.len(),
            parsed.urls.len(),
            parsed.online.len(),
            parsed.missing,
            parsed.encoding
        );
        self.session
            .remember_playlist(path.clone(), crate::config::unix_now());
        self.playlist_check_at = None;
        let old_shuffle_keys: Vec<String> = if replace && self.queue.shuffle {
            self.queue
                .display_order()
                .iter()
                .filter_map(|&index| self.queue.items.get(index))
                .map(|track| track.dedupe_key())
                .collect()
        } else {
            Vec::new()
        };
        let intent = if replace {
            self.current_track.as_ref().map(|track| ResumeIntent {
                key: track.dedupe_key(),
                position: self.position_secs(),
                was_playing: self.is_playing() || self.playback_state == PlaybackState::Loading,
                shuffle_keys: old_shuffle_keys.clone(),
            })
        } else {
            None
        };

        if replace {
            self.engine.stop();
            self.deferred_start = None;
            self.scan.cancel();
            self.scan_active = false;
            self.scan_id = 0;
            self.queue.clear();
            self.current_track = None;
            self.current_duration = None;
            self.cover = None;
            self.background = None;
            self.cover_key.clear();
            self.lyrics_service.cancel();
            self.lyrics = None;
            self.lyrics_loading = false;
            self.lyrics_error = None;
            self.lyrics_key.clear();
            self.playback_state = PlaybackState::Stopped;
            self.resume_after_import = intent;
            self.current_playlist = Some(path.clone());
            self.queue_filter.clear();
            self.pending_shuffle_keys = if self.queue.shuffle && !old_shuffle_keys.is_empty() {
                Some(old_shuffle_keys)
            } else {
                None
            };
            self.queue.shuffle_append = self.pending_shuffle_keys.is_some();
        } else if self.queue.shuffle {
            // Adding to an existing cycle: new tracks go after what is left.
            self.queue.shuffle_append = true;
        }

        let added = self.add_imported_entries(&parsed);
        self.apply_pending_shuffle_order();
        if !parsed.dirs.is_empty() {
            let roots = parsed
                .dirs
                .iter()
                .map(|dir| {
                    if dir.extensions.is_empty() {
                        ScanRoot::new(dir.path.clone())
                    } else {
                        ScanRoot::with_extensions(dir.path.clone(), dir.extensions.clone())
                    }
                })
                .collect();
            self.start_scan_roots(roots, false);
        } else {
            // No streaming scan will deliver a Finished event.
            self.pending_shuffle_keys = None;
            if self.queue.shuffle {
                self.queue.shuffle_append = true;
            }
        }
        self.try_resume_after_import();

        if self.queue.is_empty() && !self.scan_active {
            self.toast(
                crate::i18n::tr("歌单为空或文件已全部失效"),
                ToastKind::Error,
            );
        } else {
            let missing = if parsed.missing > 0 {
                crate::trf!("，{} 个文件不存在", parsed.missing)
            } else {
                String::new()
            };
            self.toast(
                crate::trf!(
                    "已导入 {} 首{missing} · {}",
                    added,
                    missing,
                    parsed.encoding
                ),
                ToastKind::Success,
            );
        }
        self.session_dirty = true;
        self.update_title();
    }

    fn add_imported_entries(&mut self, parsed: &crate::playlist::mp3list::ParsedPlaylist) -> usize {
        let mut added = 0usize;
        for path in &parsed.files {
            if self.queue.add(Track::local(path.clone())) {
                added += 1;
                self.request_background_metadata(path);
            }
        }
        for url in &parsed.urls {
            let track = Track {
                source: TrackSource::Url { url: url.clone() },
                title: url.clone(),
                source_name: "在线".to_string(),
                ..Default::default()
            };
            if self.queue.add(track) {
                added += 1;
            }
        }
        for track in &parsed.online {
            if self.queue.add(track.clone()) {
                added += 1;
            }
        }
        added
    }

    fn apply_pending_shuffle_order(&mut self) {
        if let Some(keys) = self.pending_shuffle_keys.as_ref() {
            self.queue.restore_shuffle_order(keys);
        }
    }

    /// First existing, playable track after `intent.key` in the saved shuffle
    /// order. Used when the saved track disappeared from the playlist.
    fn next_playable_after(&self, intent: &ResumeIntent) -> Option<usize> {
        let start = intent
            .shuffle_keys
            .iter()
            .position(|key| key == &intent.key)?;
        for key in intent.shuffle_keys.iter().skip(start + 1) {
            if let Some(index) = self.queue.position_of_key(key) {
                if self
                    .queue
                    .items
                    .get(index)
                    .map(Track::is_playable_local)
                    .unwrap_or(false)
                {
                    return Some(index);
                }
            }
        }
        None
    }

    fn try_resume_after_import(&mut self) {
        let Some(intent) = self.resume_after_import.clone() else {
            return;
        };
        let found = self.queue.position_of_key(&intent.key).filter(|&index| {
            self.queue
                .items
                .get(index)
                .map(Track::is_playable_local)
                .unwrap_or(false)
        });
        if let Some(index) = found {
            self.resume_after_import = None;
            tracing::info!(
                "resume {} at {:.2}s (autoplay={})",
                intent.key,
                intent.position,
                intent.was_playing
            );
            if !intent.was_playing && self.settings.performance.defer_paused_restore {
                self.restore_paused(index, intent.position);
            } else {
                self.load_index(index, intent.position, intent.was_playing);
            }
            self.toast(
                crate::i18n::tr("已恢复上次的曲目与播放位置"),
                ToastKind::Info,
            );
        } else if !self.scan_active {
            self.resume_after_import = None;
            if let Some(index) = self.next_playable_after(&intent) {
                self.toast(
                    crate::i18n::tr("上次曲目已不存在，已跳到下一首"),
                    ToastKind::Error,
                );
                if !intent.was_playing && self.settings.performance.defer_paused_restore {
                    self.restore_paused(index, 0.0);
                } else {
                    self.load_index(index, 0.0, intent.was_playing);
                }
            } else if !self.queue.is_empty() {
                self.toast(
                    crate::i18n::tr("上次播放的曲目已不在新歌单中"),
                    ToastKind::Info,
                );
            }
        }
    }

    // ------------------------------------------------------------------
    // Visuals / metadata
    // ------------------------------------------------------------------

    /// Update the cover state for the current track. Tag reading and image
    /// decoding happen on worker threads; this only shows a placeholder and
    /// queues the background job, so switching tracks never blocks the frame.
    fn refresh_visuals(&mut self) {
        let Some(track) = self.current_track.clone() else {
            self.cover = None;
            self.background = None;
            self.smtc_cover_key.clear();
            self.smtc_cover_png = None;
            self.cover_key.clear();
            self.gpu_bg.clear();
            return;
        };
        let key = track.dedupe_key();
        if self.cover_key == key {
            return;
        }
        self.cover_key = key.clone();
        // Show the bundled default in the flyout until the new cover decodes.
        self.smtc_cover_key = key.clone();
        self.smtc_cover_png = None;
        self.smtc_cover_rev = self.smtc_cover_rev.wrapping_add(1);
        // GPU modes fall back to the accent gradient until the thumbnail
        // arrives, so the old cover must not linger in the pipeline.
        self.gpu_bg.set_source(key.clone(), None);
        if self.current_duration.is_none() {
            self.current_duration = track.duration;
        }

        self.background = None;
        self.set_placeholder_cover();

        if !self.settings.performance.load_covers {
            self.cover_loading = false;
            self.cover_service.cancel();
            return;
        }
        // Only decode the CPU-blurred background when some code path can
        // actually paint it. GPU modes use their own wgpu pipeline, and an
        // active Mica backdrop is composited by the system; rendering the
        // texture there wasted both decode time and ~1 MB of texture memory.
        let mode = self.settings.theme.background;
        let want_background = self.settings.performance.cover_background
            && match mode {
                crate::config::BackgroundMode::StaticBlur => true,
                crate::config::BackgroundMode::Mica => !self.mica_active,
                crate::config::BackgroundMode::GpuBlur
                | crate::config::BackgroundMode::Flow => !self.gpu_bg.available(),
                _ => false,
            };
        if let Some(path) = track.source.path() {
            self.cover_loading = true;
            self.cover_service.request(
                key,
                path.to_path_buf(),
                self.settings.theme.accent,
                512,
                want_background,
            );
            return;
        }
        if let Some(url) = track.cover_url.clone().filter(|s| !s.trim().is_empty()) {
            let http = self.cover_http_for(&track);
            self.cover_loading = true;
            self.cover_service.request_url(
                key,
                url,
                http,
                self.settings.theme.accent,
                512,
                want_background,
            );
        }
    }

    /// HTTP options for cover URLs. Online tracks use their source's headers;
    /// plain URL tracks fall back to the global network settings.
    fn cover_http_for(&self, track: &Track) -> HttpOptions {
        match &track.source {
            TrackSource::Bamboo { source_id, .. } => self
                .settings
                .source(source_id)
                .map(|source| HttpOptions::from_settings(source, &self.settings.network))
                .unwrap_or_else(|| HttpOptions::for_network(&self.settings.network)),
            _ => HttpOptions::for_network(&self.settings.network),
        }
    }

    fn set_placeholder_cover(&mut self) {
        let accent = self.settings.theme.accent;
        let reuse = self
            .placeholder_cover
            .as_ref()
            .filter(|(a, _)| *a == accent)
            .map(|(_, texture)| texture.clone());
        self.cover = Some(reuse.unwrap_or_else(|| {
            let texture = crate::util::image_util::placeholder_texture(
                &self.ctx,
                "bamboo-placeholder",
                192,
                accent,
            );
            self.placeholder_cover = Some((accent, texture.clone()));
            texture
        }));
    }

    fn poll_cover(&mut self) {
        while let Some(result) = self.cover_service.poll() {
            if result.key != self.cover_key {
                continue;
            }
            self.cover_loading = false;
            self.smtc_cover_key = result.key.clone();
            self.smtc_cover_png = result.smtc_png;
            self.smtc_cover_rev = self.smtc_cover_rev.wrapping_add(1);
            // The native overlay renders in another process; send the already
            // CPU-blurred cover as PNG instead of trying to share a GPU texture.
            if let Some(png) = result
                .background
                .as_ref()
                .or(result.cover.as_ref())
                .and_then(crate::util::cover::encode_png)
            {
                self.desktop_lyrics_cover_png = Some(png);
                self.desktop_lyrics_cover_version =
                    self.desktop_lyrics_cover_version.wrapping_add(1);
            }
            if let Some(cover) = result.cover {
                self.cover = Some(self.ctx.load_texture(
                    format!("bamboo-cover-{}", result.key),
                    cover,
                    egui::TextureOptions::LINEAR,
                ));
            }
            if let Some(background) = result.background {
                self.background = Some(self.ctx.load_texture(
                    format!("bamboo-bg-{}", result.key),
                    background,
                    egui::TextureOptions::LINEAR,
                ));
            }
            if result.source.is_some() {
                self.gpu_bg.set_source(result.key.clone(), result.source);
            }
            self.ctx.request_repaint();
        }
    }

    /// Re-request the cover after a performance/appearance setting changed.
    pub fn reload_cover(&mut self) {
        self.cover_key.clear();
        self.cover = None;
        self.background = None;
        if !self.settings.performance.load_covers {
            self.cover_service.cancel();
            self.set_placeholder_cover();
            return;
        }
        self.refresh_visuals();
        self.ctx.request_repaint();
    }

    /// Lazily decode the custom background image. The decoded texture is kept
    /// next to its path so switching modes back and forth never re-reads it.
    fn ensure_custom_background(&mut self) {
        if self.settings.theme.background != crate::config::BackgroundMode::Image {
            self.custom_background = None;
            return;
        }
        let Some(path) = self.settings.theme.background_image.clone() else {
            self.custom_background = None;
            return;
        };
        if self
            .custom_background
            .as_ref()
            .map(|(cached, _)| cached == &path)
            .unwrap_or(false)
        {
            return;
        }
        let texture = match image::open(&path) {
            Ok(image) => {
                let image = image.thumbnail(1920, 1080);
                let rgba = image.to_rgba8();
                let size = [rgba.width() as usize, rgba.height() as usize];
                let pixels = egui::ColorImage::from_rgba_unmultiplied(size, &rgba.into_raw());
                Some(self.ctx.load_texture(
                    "bamboo-custom-background",
                    pixels,
                    egui::TextureOptions::LINEAR,
                ))
            }
            Err(err) => {
                tracing::warn!("custom background decode failed: {err}");
                None
            }
        };
        self.custom_background = Some((path, texture));
    }

    pub fn set_metadata_mode(&mut self, mode: crate::config::MetadataMode) {
        self.settings.performance.metadata_mode = mode;
        self.metadata.set_enabled(mode.enabled());
        if mode.enabled() {
            if let Some(track) = self.current_track.clone() {
                self.request_current_metadata(&track);
            }
        }
    }

    /// Queue tag reads for every local track that is missing information.
    pub fn fill_queue_metadata(&mut self) {
        if !self.settings.performance.metadata_mode.enabled() {
            self.toast(
                crate::i18n::tr("元信息读取已关闭，请先启用"),
                ToastKind::Info,
            );
            return;
        }
        let mut requested = 0usize;
        for track in &self.queue.items {
            if track.duration.is_none() || track.artist.is_empty() || track.album.is_empty() {
                if let Some(path) = track.source.path() {
                    self.metadata.request(path.to_path_buf());
                    requested += 1;
                }
            }
        }
        self.toast(
            crate::trf!("已加入 {} 首曲目的元信息读取队列", requested),
            if requested > 0 {
                ToastKind::Success
            } else {
                ToastKind::Info
            },
        );
    }

    fn poll_metadata(&mut self) {
        // Metadata arrives continuously during a large scan. Apply updates in
        // bounded batches at ~10 Hz so the queue's key index is rebuilt at most
        // ten times per second instead of every frame.
        if self.last_metadata_poll.elapsed() < Duration::from_millis(100) {
            if self.metadata.has_pending() {
                self.ctx.request_repaint_after(Duration::from_millis(100));
            }
            return;
        }
        let updates = self.metadata.poll_batch(400);
        if updates.is_empty() {
            return;
        }
        self.last_metadata_poll = Instant::now();
        let current_key = self.current_track.as_ref().map(|t| t.dedupe_key());
        let mut index = std::collections::HashMap::with_capacity(self.queue.items.len());
        for (i, track) in self.queue.items.iter().enumerate() {
            index.insert(track.dedupe_key(), i);
        }
        let mut refresh_current = false;
        let mut db_updates = Vec::new();
        for (key, meta) in updates {
            let Some(&position) = index.get(&key) else {
                continue;
            };
            let updated = {
                let track = &mut self.queue.items[position];
                apply_meta(track, &meta);
                if track.duration.is_none() {
                    track.duration = meta.duration;
                }
                track.clone()
            };
            if let Some(db_track) = DbTrack::from_track(&updated) {
                db_updates.push(db_track);
            }
            if current_key.as_deref() == Some(key.as_str()) {
                if self.current_duration.is_none() {
                    self.current_duration = updated.duration;
                }
                self.current_track = Some(updated);
                refresh_current = true;
            }
        }
        self.db.upsert(db_updates);
        if refresh_current && self.cover_key.is_empty() {
            self.refresh_visuals();
        }
        // `poll_metadata` itself runs at 10 Hz; scheduling the next frame in
        // 100 ms avoids an extra 60 fps wake-up after every batch.
        self.ctx
            .request_repaint_after(Duration::from_millis(100));
    }

    // ------------------------------------------------------------------
    // Engine events, tray, shortcuts
    // ------------------------------------------------------------------

    /// Fail the pending online resolve after its deadline so a blocked worker
    /// cannot leave the title bar and lyrics spinner in a permanent loading
    /// state. The late event is ignored because `online_request_id` is reset.
    fn tick_online_timeout(&mut self) {
        if !self.online_loading {
            self.online_deadline = None;
            return;
        }
        let Some(deadline) = self.online_deadline else {
            return;
        };
        if Instant::now() < deadline {
            return;
        }
        self.online_deadline = None;
        self.online_loading = false;
        self.online_request_id = 0;
        self.playback_state = self.engine.state();
        if self.playback_state == PlaybackState::Loading {
            self.playback_state = PlaybackState::Stopped;
        }
        self.lyrics_loading = false;
        self.lyrics_error = Some(crate::i18n::tr("在线加载超时").to_string());
        self.toast(
            crate::i18n::tr("在线加载超时，请检查音源或网络"),
            ToastKind::Error,
        );
        self.ctx.request_repaint();
    }

    fn handle_engine_events(&mut self) {
        let events = self.engine.poll_events();
        for event in events {
            match event {
                EngineEvent::Loaded {
                    token,
                    duration,
                    sample_rate,
                    channels,
                } => {
                    if token != self.engine_load_token {
                        tracing::debug!("ignoring stale loaded event (token {token})");
                        continue;
                    }
                    if let Some(d) = duration {
                        self.current_duration = Some(d);
                        if let Some(track) = self.current_track.as_mut() {
                            if track.duration.is_none() {
                                track.duration = Some(d);
                            }
                        }
                    }
                    tracing::info!("audio loaded: {} Hz, {} ch", sample_rate, channels);
                    self.playback_state = self.engine.state();
                    self.arm_next();
                }
                EngineEvent::AutoAdvanced {
                    token,
                    duration,
                    sample_rate,
                    channels,
                } => {
                    tracing::info!(
                        "gapless/crossfade advanced: {} Hz, {} ch, duration={:?}",
                        sample_rate,
                        channels,
                        duration
                    );
                    self.adopt_auto_advanced(token, duration);
                }
                EngineEvent::Ended => {
                    self.armed_next = None;
                    self.pending_auto = None;
                    self.playback_state = PlaybackState::Stopped;
                    if self.sleep_end_of_track {
                        self.finish_sleep_after_track();
                    } else {
                        self.advance_auto();
                    }
                }
                EngineEvent::Error(err) => {
                    self.armed_next = None;
                    self.pending_auto = None;
                    self.playback_state = PlaybackState::Stopped;
                    self.toast(
                        format!("{}: {err}", self.t("audio.error")),
                        ToastKind::Error,
                    );
                }
            }
        }
        // Safety net for a `Loaded` event that was observed before the deck
        // finished switching to `Playing`: never keep the app in `Loading`
        // while the engine is actually playing or paused.
        if self.playback_state == PlaybackState::Loading && !self.online_loading {
            match self.engine.state() {
                state @ (PlaybackState::Playing | PlaybackState::Paused) => {
                    self.playback_state = state;
                }
                _ => {}
            }
        }
    }

    fn ensure_tray(&mut self) {
        if !self.settings.features.tray {
            self.tray = None;
            return;
        }
        if self.tray.is_none() && !self.tray_failed {
            match Tray::new(
                self.ctx.clone(),
                self.lang,
                self.settings.theme.accent,
                self.hwnd.unwrap_or(0),
                self.tray_quit.clone(),
            ) {
                Ok(tray) => {
                    self.tray = Some(tray);
                    self.sync_tray();
                }
                Err(err) => {
                    tracing::warn!("tray creation failed: {err:#}");
                    self.tray_failed = true;
                    self.toast(crate::i18n::tr("托盘图标创建失败"), ToastKind::Error);
                }
            }
        }
    }

    /// Keep the taskbar jump list in sync with tasks and recent local files.
    fn sync_jump_list(&mut self) {
        if !self.settings.features.jump_list || self.settings.window.allow_multiple_instances {
            if self.jump_list_built && crate::platform::jump_list::clear() {
                self.jump_list_built = false;
            }
            self.jump_list_enabled = false;
            self.jump_list_dirty = false;
            self.jump_list_retry_at = None;
            return;
        }
        let now = Instant::now();
        let retry_due = self.jump_list_retry_at.is_some_and(|at| now >= at);
        let needs_update = self.jump_list_dirty || !self.jump_list_enabled || retry_due;
        self.jump_list_enabled = true;
        if !needs_update {
            return;
        }
        if self.jump_list_retry_at.is_some_and(|at| now < at) {
            return;
        }
        self.jump_list_dirty = false;
        if let Ok(exe) = std::env::current_exe() {
            self.jump_list_built =
                crate::platform::jump_list::update(&exe, &self.session.recent_files);
            self.jump_list_retry_at =
                (!self.jump_list_built).then(|| Instant::now() + Duration::from_secs(60));
        }
    }

    /// Mirror playback into the Windows media flyout and handle its buttons.
    fn sync_smtc(&mut self) {
        if !self.settings.features.media_controls {
            self.smtc = None;
            return;
        }
        if self.smtc.is_none() {
            if let Some(raw) = self.hwnd {
                self.smtc = crate::platform::smtc::SmtcController::new(raw);
                if self.smtc.is_none() {
                    tracing::warn!("SMTC initialization failed");
                }
            }
        }
        let events: Vec<_> = self
            .smtc
            .as_ref()
            .map(|smtc| std::iter::from_fn(|| smtc.poll()).collect())
            .unwrap_or_default();
        for event in events {
            match event {
                crate::platform::smtc::SmtcEvent::Play => {
                    if !self.is_playing() {
                        self.toggle_play();
                    }
                }
                crate::platform::smtc::SmtcEvent::Pause => {
                    if self.is_playing() {
                        self.toggle_play();
                    }
                }
                crate::platform::smtc::SmtcEvent::Next => self.next_track(),
                crate::platform::smtc::SmtcEvent::Previous => self.prev_track(),
                crate::platform::smtc::SmtcEvent::Seek(secs) => self.seek_to(secs),
                crate::platform::smtc::SmtcEvent::Stop => {
                    self.engine.stop();
                    self.playback_state = PlaybackState::Stopped;
                    self.session_dirty = true;
                }
            }
        }
        let playing = self.is_playing();
        let has_track = self.current_track.is_some();
        let position = self.position_secs();
        let duration = self
            .current_duration
            .or_else(|| self.engine.duration_secs());
        let Some(smtc) = self.smtc.as_mut() else {
            return;
        };
        let (title, artist, album) = match self.current_track.as_ref() {
            Some(track) => (
                track.display_title().to_string(),
                track.display_artist().to_string(),
                track.album.clone(),
            ),
            None => (String::new(), String::new(), String::new()),
        };
        smtc.update(&title, &artist, &album, playing, has_track);
        smtc.set_thumbnail(
            &self.smtc_cover_key,
            self.smtc_cover_rev,
            self.smtc_cover_png.as_deref(),
        );
        smtc.update_timeline(position, duration);
    }

    fn sync_tray(&mut self) {
        let tooltip = match self.current_track.as_ref() {
            Some(track) => format!(
                "Bamboo Music Player\n{} - {}",
                track.display_artist(),
                track.display_title()
            ),
            None => "Bamboo Music Player".to_string(),
        };
        let key = format!(
            "{}-{}-{:?}-{}-{}-{tooltip}",
            self.is_playing(),
            self.queue.shuffle,
            self.queue.repeat,
            self.settings.desktop_lyrics.visible_at_start,
            self.settings.desktop_lyrics.locked
        );
        if key == self.tray_state_key {
            return;
        }
        self.tray_state_key = key;
        let Some(tray) = self.tray.as_ref() else {
            return;
        };
        tray.set_playing(self.is_playing());
        tray.set_shuffle(self.queue.shuffle);
        tray.set_repeat(self.queue.repeat);
        tray.set_desktop_lyrics(self.settings.desktop_lyrics.visible_at_start);
        tray.set_locked(self.settings.desktop_lyrics.locked);
        tray.set_tooltip(&tooltip);
    }

    fn handle_tray(&mut self, ctx: &Context) {
        self.ensure_tray();
        let actions = self.tray.as_ref().map(|t| t.poll()).unwrap_or_default();
        for action in actions {
            match action {
                TrayAction::ShowWindow => self.show_window(ctx),
                TrayAction::HideWindow => self.hide_window(ctx),
                TrayAction::ToggleWindow => {
                    if self.window_hidden {
                        self.show_window(ctx);
                    } else {
                        self.hide_window(ctx);
                    }
                }
                TrayAction::PlayPause => self.toggle_play(),
                TrayAction::Prev => self.prev_track(),
                TrayAction::Next => self.next_track(),
                TrayAction::ToggleShuffle => self.toggle_shuffle(),
                TrayAction::CycleRepeat => self.cycle_repeat(),
                TrayAction::ToggleDesktopLyrics => self.toggle_desktop_lyrics(),
                TrayAction::ToggleLockLyrics => {
                    self.settings.desktop_lyrics.locked = !self.settings.desktop_lyrics.locked;
                    self.settings_dirty = true;
                }
                TrayAction::Volume(v) => self.set_volume(v),
                TrayAction::Quit => {
                    self.quit_immediately();
                }
            }
        }
        self.sync_tray();
    }

    pub fn show_window(&mut self, ctx: &Context) {
        ctx.send_viewport_cmd(ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(ViewportCommand::Focus);
        self.window_hidden = false;
        self.reapply_window_style();
        self.ctx.request_repaint();
    }

    fn reapply_window_style(&mut self) {
        if let Some(raw) = self.hwnd {
            let dark = theme::is_dark(&self.ctx, &self.settings);
            window::apply_style(
                raw,
                dark,
                self.settings.theme.roundness,
                self.settings.window.capture_protection,
            );
            let want_mica = self.settings.theme.background == crate::config::BackgroundMode::Mica
                && window::transparency_supported();
            let mica = window::set_mica(raw, want_mica, dark);
            if mica != self.mica_active {
                self.mica_active = mica;
                if self.settings.theme.background == crate::config::BackgroundMode::Mica {
                    if mica {
                        // The system backdrop is visible; the CPU-blurred
                        // cover texture is not painted.
                        self.background = None;
                    } else {
                        // Mica was lost (composition change / remote session):
                        // decode the blurred fallback that `paint_background`
                        // now needs.
                        self.cover_key.clear();
                        self.refresh_visuals();
                    }
                }
                self.ctx.request_repaint();
            }
        }
    }

    pub fn hide_window(&mut self, ctx: &Context) {
        ctx.send_viewport_cmd(ViewportCommand::Visible(false));
        self.window_hidden = true;
        self.save_now();
        if self.settings.window.trim_working_set_on_idle {
            crate::platform::memory::trim_working_set();
            self.memory_info = crate::platform::memory::process_memory();
            self.last_trim = Instant::now();
        }
    }

    fn handle_close_request(&mut self, ctx: &Context) {
        let close_requested = ctx.input(|i| i.viewport().close_requested());
        if !close_requested {
            return;
        }
        let tray_quit = self
            .tray
            .as_ref()
            .is_some_and(|tray| tray.take_quit_request())
            || self.tray_quit.swap(false, Ordering::Relaxed);
        if tray_quit || self.allow_close {
            self.quit_immediately();
            return;
        }
        match self.settings.window.close_behavior {
            CloseBehavior::Exit => {
                self.quit_immediately();
            }
            CloseBehavior::Tray | CloseBehavior::Ask => {
                ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                ctx.send_viewport_cmd(ViewportCommand::Visible(false));
                self.window_hidden = true;
                self.save_now();
            }
        }
    }

    /// Apply command-line arguments forwarded by a second instance. This is
    /// how jump-list tasks and "open with" reach an already running player.
    fn poll_ipc(&mut self) {
        while let Ok(args) = self.ipc_rx.try_recv() {
            tracing::info!("ipc command: {:?}", args);
            self.apply_commands(args);
        }
    }

    /// Execute one batch of flag/file arguments. Used for both IPC commands and
    /// commands from our own command line when no instance was running.
    fn apply_commands(&mut self, args: Vec<String>) {
        let ctx = self.ctx.clone();
        let mut paths = Vec::new();
        let mut show = false;
        for arg in args {
            match arg.as_str() {
                "--show" => show = true,
                "--play-pause" => self.toggle_play(),
                "--next" => self.next_track(),
                "--prev" => self.prev_track(),
                "--settings" => {
                    self.settings_tab = views::SettingsTab::default();
                    self.set_page(Page::Settings);
                    show = true;
                }
                "--open-files" => {
                    self.open_files_dialog();
                    show = true;
                }
                "--open-url" => {
                    self.open_url_dialog();
                    show = true;
                }
                "--import-playlist" => {
                    self.import_playlist_dialog();
                    show = true;
                }
                _ if !arg.starts_with("--") => {
                    let path = PathBuf::from(&arg);
                    if path.exists() {
                        paths.push(path);
                        show = true;
                    }
                }
                _ => {}
            }
        }
        if !paths.is_empty() {
            self.add_paths(paths, true);
        }
        if show {
            self.show_window(&ctx);
        }
        self.ctx.request_repaint();
    }

    fn handle_shortcuts(&mut self, ctx: &Context) {
        if self.settings.features.command_palette
            && ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::K))
        {
            self.toggle_command_palette();
        }
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let mut toggle = false;
        let mut next = false;
        let mut prev = false;
        let mut vol_up = false;
        let mut vol_down = false;
        let mut open = false;
        let mut lyrics = false;
        let mut edit_lyrics = false;
        let mut escape_lyrics = false;
        let mut toggle_fullscreen = false;
        ctx.input_mut(|i| {
            if i.consume_key(Modifiers::NONE, Key::Space) {
                toggle = true;
            }
            if i.consume_key(Modifiers::CTRL, Key::ArrowRight) {
                next = true;
            }
            if i.consume_key(Modifiers::CTRL, Key::ArrowLeft) {
                prev = true;
            }
            if i.consume_key(Modifiers::CTRL, Key::ArrowUp) {
                vol_up = true;
            }
            if i.consume_key(Modifiers::CTRL, Key::ArrowDown) {
                vol_down = true;
            }
            if i.consume_key(Modifiers::CTRL, Key::O) {
                open = true;
            }
            if i.consume_key(Modifiers::CTRL, Key::L) {
                lyrics = true;
            }
            if i.consume_key(Modifiers::CTRL, Key::E) {
                edit_lyrics = true;
            }
            if (self.fullscreen_lyrics || self.lyrics_os_fullscreen)
                && i.consume_key(Modifiers::NONE, Key::Escape)
            {
                escape_lyrics = true;
            }
            if i.consume_key(Modifiers::NONE, Key::F11) {
                toggle_fullscreen = true;
            }
        });
        if toggle {
            self.toggle_play();
        }
        if next {
            self.next_track();
        }
        if prev {
            self.prev_track();
        }
        if vol_up {
            self.set_volume(self.volume() + 0.05);
        }
        if vol_down {
            self.set_volume(self.volume() - 0.05);
        }
        if open {
            self.open_files_dialog();
        }
        if lyrics {
            self.toggle_lyrics_sidebar();
        }
        if edit_lyrics {
            self.open_lyrics_editor();
        }
        if escape_lyrics {
            if self.lyrics_os_fullscreen {
                self.set_lyrics_os_fullscreen(false);
            } else if self.fullscreen_lyrics {
                self.toggle_fullscreen_lyrics();
            }
        }
        if toggle_fullscreen {
            self.toggle_lyrics_os_fullscreen();
        }
    }

    fn handle_drop(&mut self, ctx: &Context) {
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        if dropped.is_empty() {
            return;
        }
        let paths: Vec<PathBuf> = dropped.into_iter().filter_map(|f| f.path).collect();
        if !paths.is_empty() {
            self.add_paths(paths, self.queue.current.is_none());
        }
    }

    // ------------------------------------------------------------------
    // URL opening
    // ------------------------------------------------------------------

    pub fn open_url_dialog(&mut self) {
        self.url_dialog_open = true;
        self.url_error = None;
        if self.url_source.is_empty() || self.settings.source(&self.url_source).is_none() {
            self.url_source = self
                .settings
                .enabled_sources()
                .find(|source| !source.base_url.trim().is_empty())
                .map(|source| source.id.clone())
                .unwrap_or_else(|| "local".to_string());
        }
        self.ctx.request_repaint();
    }

    pub fn submit_url(&mut self) {
        let raw = self.url_input.trim().to_string();
        if raw.is_empty() {
            self.url_error = Some(crate::i18n::tr("请输入 URL").to_string());
            return;
        }
        let candidate = if raw.contains("://") {
            raw.clone()
        } else {
            format!("https://{raw}")
        };
        let Ok(url) = url::Url::parse(&candidate) else {
            self.url_error = Some(crate::i18n::tr("URL 格式不正确").to_string());
            return;
        };

        if let Some(id) = bamboo_id_from_url(&url) {
            let source = self
                .settings
                .source(&self.url_source)
                .cloned()
                .or_else(|| self.settings.enabled_sources().next().cloned());
            let Some(source) = source else {
                self.url_error = Some(crate::i18n::tr("请先在设置中配置在线音源").to_string());
                return;
            };
            let track = Track {
                source: TrackSource::Bamboo {
                    source_id: source.id.clone(),
                    song_id: id.clone(),
                    base_url: source.base_url.clone(),
                },
                title: crate::trf!("在线歌曲 {id}", id),
                source_name: source.name.clone(),
                ..Default::default()
            };
            self.add_url_track(track);
            return;
        }

        if is_audio_url(&url) {
            let title = url
                .path_segments()
                .and_then(|mut parts| parts.next_back())
                .filter(|name| !name.is_empty())
                .map(|name| {
                    name.rsplit_once('.')
                        .map(|(stem, _)| stem.to_string())
                        .unwrap_or_else(|| name.to_string())
                })
                .unwrap_or_else(|| crate::i18n::tr("在线音频").to_string());
            let track = Track {
                source: TrackSource::Url {
                    url: url.to_string(),
                },
                title,
                source_name: "URL".to_string(),
                ..Default::default()
            };
            self.add_url_track(track);
            return;
        }

        if is_playlist_url(&url) {
            self.import_remote_playlist(url);
            return;
        }

        self.url_error = Some(
            crate::i18n::tr(
                "无法识别：支持 Bamboo 分享链接（含 musicid）、音频直链、.mp3list/.m3u/.m3u8 歌单",
            )
            .to_string(),
        );
    }

    fn import_remote_playlist(&mut self, url: url::Url) {
        let http = HttpOptions::for_network(&self.settings.network).for_stream();
        let agent = http.cached_api_agent();
        let mut response = match http.apply(agent.get(url.as_str())).call() {
            Ok(response) => response,
            Err(err) => {
                self.url_error = Some(crate::trf!("歌单下载失败: {err}", err));
                return;
            }
        };
        if response.status().as_u16() >= 400 {
            self.url_error = Some(crate::trf!("歌单下载失败: HTTP {}", response.status()));
            return;
        }
        let body = match response
            .body_mut()
            .with_config()
            .limit(4 * 1024 * 1024)
            .read_to_string()
        {
            Ok(body) => body,
            Err(err) => {
                self.url_error = Some(crate::trf!("歌单读取失败: {err}", err));
                return;
            }
        };

        let mut tracks = Vec::new();
        let mut pending_title = String::new();
        for line in body.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Some(rest) = line.strip_prefix("#BAMBOO#") {
                let mut parts = rest.split('|');
                let source_id = parts.next().unwrap_or("local").trim().to_string();
                let song_id = parts.next().unwrap_or("").trim().to_string();
                if song_id.is_empty() {
                    continue;
                }
                let title = parts
                    .next()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .unwrap_or(&song_id)
                    .to_string();
                let artist = parts.next().unwrap_or("").trim().to_string();
                tracks.push(Track {
                    source: TrackSource::Bamboo {
                        source_id,
                        song_id,
                        base_url: String::new(),
                    },
                    title,
                    artist,
                    source_name: "在线".to_string(),
                    ..Default::default()
                });
                continue;
            }
            if let Some(rest) = line.strip_prefix("#EXTINF:") {
                if let Some((_, title)) = rest.split_once(',') {
                    pending_title = title.trim().to_string();
                }
                continue;
            }
            if line.starts_with('#') {
                continue;
            }
            let resolved = if line.contains("://") {
                url::Url::parse(line).ok()
            } else {
                url.join(line).ok()
            };
            let Some(resolved) = resolved else {
                continue;
            };
            if !is_audio_url(&resolved) {
                continue;
            }
            let title = if pending_title.is_empty() {
                resolved
                    .path_segments()
                    .and_then(|mut parts| parts.next_back())
                    .unwrap_or(crate::i18n::tr("在线音频"))
                    .to_string()
            } else {
                std::mem::take(&mut pending_title)
            };
            tracks.push(Track {
                source: TrackSource::Url {
                    url: resolved.to_string(),
                },
                title,
                source_name: "URL".to_string(),
                ..Default::default()
            });
        }

        if tracks.is_empty() {
            self.url_error = Some(crate::i18n::tr("歌单中没有可识别的曲目").to_string());
            return;
        }
        let before = self.queue.items.len();
        let added = self.queue.add_many(tracks);
        if added == 0 {
            self.url_error = Some(crate::i18n::tr("歌单中的曲目都已在队列中").to_string());
            return;
        }
        self.session_dirty = true;
        if self.queue.current.is_none() {
            self.play_index(before);
        }
        self.url_dialog_open = false;
        self.url_error = None;
        self.toast(
            crate::trf!("已从 URL 导入 {added} 首", added),
            ToastKind::Success,
        );
    }

    fn add_url_track(&mut self, track: Track) {
        if self.queue.add(track) {
            let index = self.queue.items.len() - 1;
            self.play_index(index);
            self.url_dialog_open = false;
            self.url_error = None;
            self.toast(crate::i18n::tr("已打开 URL"), ToastKind::Success);
        } else {
            self.url_error = Some(crate::i18n::tr("该 URL 已在队列中").to_string());
        }
    }

    // ------------------------------------------------------------------
    // Internet radio
    // ------------------------------------------------------------------

    // ------------------------------------------------------------------
    // P9: local BambooMusic "random station" (随机电台)
    // ------------------------------------------------------------------

    /// Prefer the local BambooMusic source, then any enabled source with a URL.
    fn station_source(&self) -> Option<crate::config::SourceConfig> {
        let usable = |source: &&crate::config::SourceConfig| {
            source.enabled && !source.base_url.trim().is_empty()
        };
        self.settings
            .sources
            .iter()
            .find(|source| source.kind == crate::config::SourceKind::Local && usable(&source))
            .or_else(|| self.settings.sources.iter().find(usable))
            .cloned()
    }

    pub fn toggle_station(&mut self) {
        if self.station_active {
            self.stop_station();
        } else {
            self.start_station();
        }
    }

    pub fn start_station(&mut self) {
        if !self.settings.features.internet_radio {
            self.toast(
                crate::i18n::tr("随机电台已在功能开关中关闭"),
                ToastKind::Info,
            );
            return;
        }
        if self.settings.network.offline {
            self.toast(
                crate::i18n::tr("离线模式下不能使用随机电台"),
                ToastKind::Error,
            );
            return;
        }
        if self.station_fetching {
            return;
        }
        let Some(source) = self.station_source() else {
            self.toast(
                crate::i18n::tr("请先配置本地 BambooMusic 音源地址"),
                ToastKind::Error,
            );
            return;
        };
        self.station_active = true;
        self.station_fetching = true;
        self.station_play_on_arrival = true;
        self.station_error = None;
        self.station_keys.clear();
        self.station_source_id = Some(source.id.clone());
        let seed = station_seed();
        self.station_request = self.api.random(&source, &self.settings.network, 24, seed);
        self.toast(
            crate::trf!("正在从「{}」开启随机电台…", source.name),
            ToastKind::Info,
        );
        self.ctx.request_repaint();
    }

    pub fn stop_station(&mut self) {
        if !self.station_active && !self.station_fetching {
            return;
        }
        self.station_active = false;
        self.station_fetching = false;
        self.station_play_on_arrival = false;
        self.station_request = 0;
        self.toast(
            crate::i18n::tr("已停止随机电台（队列中的歌曲保留）"),
            ToastKind::Info,
        );
        self.ctx.request_repaint();
    }

    pub fn station_source_name(&self) -> Option<String> {
        self.station_source_id
            .as_deref()
            .and_then(|id| self.settings.source(id))
            .map(|source| source.name.clone())
            .or_else(|| self.station_source().map(|source| source.name))
    }

    /// Fetch another random batch before the queue runs out.
    fn refill_station(&mut self) {
        if !self.station_active || self.station_fetching {
            return;
        }
        let Some(source) = self
            .station_source_id
            .as_deref()
            .and_then(|id| self.settings.source(id).cloned())
            .or_else(|| self.station_source())
        else {
            self.station_active = false;
            self.toast(
                crate::i18n::tr("随机电台音源不可用，已停止"),
                ToastKind::Error,
            );
            return;
        };
        self.station_fetching = true;
        self.station_play_on_arrival = false;
        self.station_source_id = Some(source.id.clone());
        let seed = station_seed();
        self.station_request = self.api.random(&source, &self.settings.network, 24, seed);
    }

    fn maybe_refill_station(&mut self) {
        if !self.station_active || self.station_fetching {
            return;
        }
        let Some(current) = self.queue.current else {
            return;
        };
        // Keep at least five upcoming random songs so gapless/crossfade always
        // has a next source to pre-arm.
        if current + 5 >= self.queue.len() {
            self.refill_station();
        }
    }

    fn notify_track_change(&mut self, track: &Track) {
        let notifications = &self.settings.notifications;
        if !self.settings.features.notifications
            || !notifications.enabled
            || !notifications.on_track_change
        {
            return;
        }
        let focused = self
            .ctx
            .input(|input| input.viewport().focused)
            .unwrap_or(true);
        if notifications.only_when_unfocused && focused {
            return;
        }
        let key = track.dedupe_key();
        if self.last_notified_key == key {
            return;
        }
        self.last_notified_key = key;
        let mut body = track.display_artist();
        if !track.album.trim().is_empty() {
            body.push_str(" · ");
            body.push_str(&track.album);
        }
        crate::platform::notify::show(&track.display_title(), &body);
    }

    fn record_station_history(&mut self, track: &Track) {
        if !self.station_active {
            return;
        }
        let key = track.dedupe_key();
        if self
            .station_history
            .first()
            .is_some_and(|first| first.dedupe_key() == key)
        {
            return;
        }
        self.station_history.retain(|item| item.dedupe_key() != key);
        self.station_history.insert(0, track.clone());
        self.station_history.truncate(8);
        self.session_dirty = true;
    }

    /// Remember a local file for the jump list's "recent" category.
    fn record_recent_file(&mut self, track: &Track) {
        let Some(path) = track.source.path() else {
            return;
        };
        if !path.is_file() {
            return;
        }
        let title = track.display_title();
        self.session.recent_files.retain(|item| item.path != path);
        self.session.recent_files.insert(
            0,
            RecentFile {
                path: path.to_path_buf(),
                title,
            },
        );
        self.session.recent_files.truncate(8);
        self.session_dirty = true;
        self.jump_list_dirty = true;
    }

    pub fn clear_station_history(&mut self) {
        if !self.station_history.is_empty() {
            self.station_history.clear();
            self.session_dirty = true;
            self.toast(crate::i18n::tr("已清空随机电台历史"), ToastKind::Info);
        }
    }

    pub fn station_available(&self) -> bool {
        self.settings.features.internet_radio && self.station_source().is_some()
    }

    // ------------------------------------------------------------------
    // Discover recommendation feed
    // ------------------------------------------------------------------

    pub fn discover_available(&self) -> bool {
        self.settings.features.internet_radio
            && !self.settings.network.offline
            && self.station_source().is_some()
    }

    pub fn discover_source_name(&self) -> Option<String> {
        self.discover_source_id
            .as_deref()
            .and_then(|id| self.settings.source(id))
            .map(|source| source.name.clone())
            .or_else(|| self.station_source().map(|source| source.name))
    }

    /// Fetch a fresh random batch for the Discover page. This is independent
    /// from the station queue: browsing recommendations never starts playback
    /// or changes the current queue.
    pub fn refresh_discover_feed(&mut self) {
        if !self.settings.features.internet_radio {
            self.discover_error = Some(crate::i18n::tr("推荐流已在功能开关中关闭").to_string());
            return;
        }
        if self.settings.network.offline {
            self.discover_error = Some(crate::i18n::tr("离线模式下不能获取在线推荐").to_string());
            return;
        }
        let Some(source) = self.station_source() else {
            self.discover_error =
                Some(crate::i18n::tr("请先配置本地 BambooMusic 音源地址").to_string());
            return;
        };
        self.discover_loading = true;
        self.discover_error = None;
        self.discover_source_id = Some(source.id.clone());
        let seed = station_seed();
        self.discover_request = self.api.random(&source, &self.settings.network, 18, seed);
        self.ctx.request_repaint();
    }

    fn handle_discover_random(
        &mut self,
        id: u64,
        page: crate::api::ApiResult<crate::api::SearchPage>,
    ) {
        if id != self.discover_request {
            return;
        }
        self.discover_request = 0;
        self.discover_loading = false;
        let source = self
            .discover_source_id
            .as_deref()
            .and_then(|id| self.settings.source(id).cloned())
            .or_else(|| self.station_source());
        let Some(source) = source else {
            self.discover_error = Some(crate::i18n::tr("音源配置已删除").to_string());
            return;
        };
        match page {
            Ok(page) => {
                let mut seen = HashSet::new();
                self.discover_tracks = page
                    .items
                    .iter()
                    .filter(|song| song.has_audio)
                    .map(|song| song.to_track(&source))
                    .filter(|track| seen.insert(track.dedupe_key()))
                    .take(18)
                    .collect();
                if self.discover_tracks.is_empty() {
                    self.discover_error =
                        Some(crate::i18n::tr("音源没有返回可播放的推荐歌曲").to_string());
                }
            }
            Err(err) => {
                self.discover_tracks.clear();
                self.discover_error = Some(err);
            }
        }
        self.ctx.request_repaint();
    }

    /// Append every recommendation to the queue and start with the first new
    /// addition. Existing queue entries are kept (add_many dedupes by key).
    pub fn play_discover_feed(&mut self) {
        if self.discover_tracks.is_empty() {
            return;
        }
        let start = self.queue.len();
        let added = self.queue.add_many(self.discover_tracks.clone());
        if added == 0 {
            self.toast(crate::i18n::tr("推荐歌曲都已在队列中"), ToastKind::Info);
            return;
        }
        self.session_dirty = true;
        self.load_index(start.min(self.queue.len().saturating_sub(1)), 0.0, true);
    }

    pub fn enqueue_discover_feed(&mut self) {
        if self.discover_tracks.is_empty() {
            return;
        }
        let added = self.queue.add_many(self.discover_tracks.clone());
        if added == 0 {
            self.toast(crate::i18n::tr("推荐歌曲都已在队列中"), ToastKind::Info);
        } else {
            self.session_dirty = true;
            self.toast(
                crate::trf!("已加入 {added} 首推荐歌曲", added),
                ToastKind::Success,
            );
        }
    }

    /// Drop old station entries once a long session has grown the queue. Only
    /// tracks recorded as station entries are touched; the user's own queue is
    /// left intact.
    fn prune_station_queue(&mut self) {
        let Some(current) = self.queue.current else {
            return;
        };
        if self.queue.len() <= 220 || current <= 60 {
            return;
        }
        let remove_before = current - 60;
        let mut indices: Vec<usize> = (0..remove_before)
            .filter(|&index| {
                self.queue
                    .items
                    .get(index)
                    .is_some_and(|track| self.station_keys.contains(&track.dedupe_key()))
            })
            .collect();
        let removed = indices.len();
        for index in indices.drain(..).rev() {
            self.queue.remove(index);
        }
        if removed > 0 {
            self.session_dirty = true;
        }
    }

    fn handle_station_random(
        &mut self,
        id: u64,
        page: crate::api::ApiResult<crate::api::SearchPage>,
    ) {
        if id != self.station_request {
            return;
        }
        self.station_request = 0;
        self.station_fetching = false;
        let play_now = self.station_play_on_arrival;
        self.station_play_on_arrival = false;
        if !self.station_active && !play_now {
            return;
        }
        let Some(source) = self
            .station_source_id
            .as_deref()
            .and_then(|id| self.settings.source(id).cloned())
            .or_else(|| self.station_source())
        else {
            self.station_active = false;
            self.station_error = Some(crate::i18n::tr("音源配置已删除").to_string());
            return;
        };
        match page {
            Ok(page) => {
                let tracks: Vec<Track> = page
                    .items
                    .iter()
                    .filter(|song| song.has_audio)
                    .map(|song| song.to_track(&source))
                    .collect();
                if tracks.is_empty() {
                    self.station_error =
                        Some(crate::i18n::tr("音源没有返回可播放的歌曲").to_string());
                    if play_now {
                        self.station_active = false;
                        self.toast(crate::i18n::tr("随机电台没有取得歌曲"), ToastKind::Error);
                    }
                    return;
                }
                let first = self.queue.len();
                let keys: Vec<String> = tracks.iter().map(|track| track.dedupe_key()).collect();
                let added = self.queue.add_many(tracks);
                if added > 0 {
                    self.station_keys.extend(keys);
                }
                self.prune_station_queue();
                if play_now && added > 0 {
                    self.load_index(first, 0.0, true);
                    self.toast(crate::i18n::tr("随机电台已开启"), ToastKind::Success);
                } else if play_now {
                    self.station_active = false;
                    self.toast(
                        crate::i18n::tr("没有取得新的随机歌曲（曲库可能太小）"),
                        ToastKind::Info,
                    );
                } else {
                    self.session_dirty = true;
                    self.arm_next();
                }
            }
            Err(err) => {
                self.station_error = Some(err.clone());
                if play_now {
                    self.station_active = false;
                }
                self.toast(
                    crate::trf!("随机电台获取失败: {err}", err),
                    ToastKind::Error,
                );
            }
        }
        self.ctx.request_repaint();
    }

    // ------------------------------------------------------------------
    // P9: command palette
    // ------------------------------------------------------------------

    pub fn toggle_command_palette(&mut self) {
        if !self.settings.features.command_palette {
            self.toast(
                crate::i18n::tr("命令面板已在功能开关中关闭"),
                ToastKind::Info,
            );
            return;
        }
        self.command_palette_open = !self.command_palette_open;
        if self.command_palette_open {
            self.command_palette_query.clear();
            self.command_palette_index = 0;
            self.command_palette_focus = true;
        }
        self.ctx.request_repaint();
    }

    fn palette_entries(&self) -> Vec<(String, &'static str, PaletteAction)> {
        let playing = self.is_playing();
        let repeat = self.queue.repeat.label();
        let mut entries: Vec<(String, &'static str, PaletteAction)> = vec![
            (
                if playing {
                    crate::i18n::tr("暂停播放").to_string()
                } else {
                    crate::i18n::tr("开始播放").to_string()
                },
                "Space",
                PaletteAction::PlayPause,
            ),
            (
                crate::i18n::tr("下一首").to_string(),
                "Ctrl+→",
                PaletteAction::Next,
            ),
            (
                crate::i18n::tr("上一首").to_string(),
                "Ctrl+←",
                PaletteAction::Prev,
            ),
            (
                crate::trf!(
                    "随机播放：{}",
                    crate::i18n::tr(if self.queue.shuffle {
                        crate::i18n::tr("开")
                    } else {
                        crate::i18n::tr("关")
                    })
                ),
                "",
                PaletteAction::ToggleShuffle,
            ),
            (
                crate::trf!("循环模式：{repeat}（切换）", crate::i18n::tr(repeat)),
                "",
                PaletteAction::CycleRepeat,
            ),
            (
                if self.settings.playback.muted {
                    crate::i18n::tr("取消静音").to_string()
                } else {
                    crate::i18n::tr("静音").to_string()
                },
                "",
                PaletteAction::ToggleMute,
            ),
            (
                crate::i18n::tr("音量 +5%").to_string(),
                "",
                PaletteAction::VolumeUp,
            ),
            (
                crate::i18n::tr("音量 -5%").to_string(),
                "",
                PaletteAction::VolumeDown,
            ),
            (
                crate::i18n::tr("显示/隐藏歌词栏").to_string(),
                "Ctrl+L",
                PaletteAction::ToggleLyrics,
            ),
            (
                crate::trf!(
                    "桌面歌词：{}",
                    if self.settings.desktop_lyrics.visible_at_start {
                        crate::i18n::tr("开")
                    } else {
                        crate::i18n::tr("关")
                    }
                ),
                "",
                PaletteAction::ToggleDesktopLyrics,
            ),
            (
                crate::i18n::tr("大歌词页").to_string(),
                "F11",
                PaletteAction::ToggleFullscreenLyrics,
            ),
            (
                crate::i18n::tr("独立歌词窗口").to_string(),
                "",
                PaletteAction::ToggleLyricsWindow,
            ),
            (
                crate::i18n::tr("编辑当前歌词").to_string(),
                "",
                PaletteAction::EditLyrics,
            ),
            (
                crate::trf!(
                    "可视化：{}",
                    if self.settings.visualizer.enabled {
                        crate::i18n::tr("开")
                    } else {
                        crate::i18n::tr("关")
                    }
                ),
                "",
                PaletteAction::ToggleVisualizer,
            ),
            (
                crate::trf!(
                    "随机电台：{}",
                    if self.station_active {
                        crate::i18n::tr("停止")
                    } else {
                        crate::i18n::tr("开始")
                    }
                ),
                "",
                PaletteAction::ToggleStation,
            ),
            (
                crate::trf!(
                    "迷你播放器：{}",
                    if self.settings.mini.visible {
                        crate::i18n::tr("隐藏")
                    } else {
                        crate::i18n::tr("显示")
                    }
                ),
                "",
                PaletteAction::ToggleMiniPlayer,
            ),
            (
                crate::i18n::tr("打开文件").to_string(),
                "Ctrl+O",
                PaletteAction::OpenFiles,
            ),
            (
                crate::i18n::tr("打开文件夹").to_string(),
                "",
                PaletteAction::OpenFolder,
            ),
            (
                crate::i18n::tr("导入歌单").to_string(),
                "",
                PaletteAction::ImportPlaylist,
            ),
            (
                crate::i18n::tr("打开 URL").to_string(),
                "",
                PaletteAction::OpenUrl,
            ),
            (
                crate::i18n::tr("更新曲库索引").to_string(),
                "",
                PaletteAction::LibraryIndex,
            ),
            (
                crate::i18n::tr("下载管理").to_string(),
                "",
                PaletteAction::OpenDownloads,
            ),
        ];
        for page in Page::ALL {
            entries.push((
                crate::trf!("前往：{}", page_label(page)),
                "",
                PaletteAction::Page(page),
            ));
        }
        for tab in views::SettingsTab::ALL {
            entries.push((
                crate::trf!("设置：{}", tab.label()),
                "",
                PaletteAction::Settings(tab),
            ));
        }
        entries
    }

    fn run_palette_action(&mut self, action: PaletteAction) {
        match action {
            PaletteAction::Page(page) => self.set_page(page),
            PaletteAction::Settings(tab) => {
                self.settings_tab = tab;
                self.set_page(Page::Settings);
            }
            PaletteAction::PlayPause => self.toggle_play(),
            PaletteAction::Next => self.next_track(),
            PaletteAction::Prev => self.prev_track(),
            PaletteAction::ToggleShuffle => self.toggle_shuffle(),
            PaletteAction::CycleRepeat => self.cycle_repeat(),
            PaletteAction::ToggleMute => self.toggle_mute(),
            PaletteAction::VolumeUp => self.set_volume(self.volume() + 0.05),
            PaletteAction::VolumeDown => self.set_volume(self.volume() - 0.05),
            PaletteAction::ToggleLyrics => self.toggle_lyrics_sidebar(),
            PaletteAction::ToggleDesktopLyrics => self.toggle_desktop_lyrics(),
            PaletteAction::ToggleFullscreenLyrics => self.toggle_fullscreen_lyrics(),
            PaletteAction::ToggleLyricsWindow => self.toggle_lyrics_window(),
            PaletteAction::EditLyrics => self.open_lyrics_editor(),
            PaletteAction::ToggleVisualizer => {
                self.settings.visualizer.enabled = !self.settings.visualizer.enabled;
                self.settings_dirty = true;
                self.toast(
                    if self.settings.visualizer.enabled {
                        crate::i18n::tr("可视化已开启")
                    } else {
                        crate::i18n::tr("可视化已关闭")
                    },
                    ToastKind::Info,
                );
            }
            PaletteAction::OpenFiles => self.open_files_dialog(),
            PaletteAction::ToggleStation => self.toggle_station(),
            PaletteAction::ToggleMiniPlayer => self.toggle_mini_player(),
            PaletteAction::OpenFolder => self.open_folder_dialog(),
            PaletteAction::ImportPlaylist => self.import_playlist_dialog(),
            PaletteAction::OpenUrl => self.open_url_dialog(),
            PaletteAction::LibraryIndex => {
                self.start_library_index();
                self.library_tab = views::LibraryTab::Index;
                self.set_page(Page::Library);
            }
            PaletteAction::OpenDownloads => {
                self.library_tab = views::LibraryTab::Downloads;
                self.set_page(Page::Library);
            }
        }
    }

    fn command_palette(&mut self, ctx: &Context) {
        if !self.command_palette_open {
            return;
        }
        let colors = self.colors();
        let entries = self.palette_entries();
        let matcher = SkimMatcherV2::default();
        let query = std::mem::take(&mut self.command_palette_query);
        let mut query = query;
        let mut index = self.command_palette_index;
        let mut request_focus = self.command_palette_focus;
        let mut chosen: Option<PaletteAction> = None;
        let mut close = false;
        let screen_width = ctx.input(|i| i.viewport_rect().width());
        let width = 560.0_f32.min(screen_width - 32.0);

        egui::Area::new(Id::new("bamboo-command-palette"))
            .order(Order::Foreground)
            .anchor(Align2::CENTER_TOP, vec2(0.0, 92.0))
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(colors.panel)
                    .stroke(egui::Stroke::new(1.0, colors.line))
                    .corner_radius(egui::CornerRadius::same(12))
                    .inner_margin(egui::Margin::symmetric(12, 10))
                    .shadow(egui::epaint::Shadow {
                        offset: [0, 8],
                        blur: 24,
                        spread: 0,
                        color: Color32::from_black_alpha(90),
                    })
                    .show(ui, |ui| {
                        ui.set_width(width);
                        let response = ui.add(
                            egui::TextEdit::singleline(&mut query)
                                .hint_text(crate::i18n::tr("输入命令、页面或设置…"))
                                .desired_width(f32::INFINITY)
                                .margin(egui::Margin::symmetric(10, 8)),
                        );
                        if request_focus {
                            response.request_focus();
                            request_focus = false;
                        }

                        let trimmed = query.trim();
                        let mut matches: Vec<(i64, &(String, &'static str, PaletteAction))> =
                            entries
                                .iter()
                                .filter_map(|entry| {
                                    if trimmed.is_empty() {
                                        Some((0, entry))
                                    } else {
                                        matcher
                                            .fuzzy_match(&entry.0, trimmed)
                                            .map(|score| (score, entry))
                                    }
                                })
                                .collect();
                        matches.sort_by(|a, b| b.0.cmp(&a.0));
                        let len = matches.len();
                        if len == 0 {
                            index = 0;
                        } else {
                            index = index.min(len - 1);
                            if ctx.input(|i| i.key_pressed(Key::ArrowDown)) {
                                index = (index + 1) % len;
                            }
                            if ctx.input(|i| i.key_pressed(Key::ArrowUp)) {
                                index = (index + len - 1) % len;
                            }
                            if ctx.input(|i| i.key_pressed(Key::Enter)) {
                                chosen = Some(matches[index].1.2);
                                close = true;
                            }
                        }
                        if ctx.input(|i| i.key_pressed(Key::Escape)) {
                            close = true;
                        }

                        ui.add_space(6.0);
                        if len == 0 {
                            ui.label(
                                RichText::new(crate::i18n::tr("没有匹配的命令"))
                                    .size(12.0)
                                    .color(colors.text_faint),
                            );
                        } else {
                            egui::ScrollArea::vertical()
                                .max_height(330.0)
                                .auto_shrink([false, true])
                                .show(ui, |ui| {
                                    for (position, (_, entry)) in
                                        matches.iter().take(80).enumerate()
                                    {
                                        let selected = position == index;
                                        let (rect, response) = ui.allocate_exact_size(
                                            vec2(ui.available_width(), 34.0),
                                            egui::Sense::click(),
                                        );
                                        if selected || response.hovered() {
                                            ui.painter().rect_filled(
                                                rect,
                                                egui::CornerRadius::same(7),
                                                if selected {
                                                    colors.accent_soft
                                                } else {
                                                    colors.card_hover
                                                },
                                            );
                                        }
                                        ui.painter().text(
                                            rect.left_center() + vec2(10.0, 0.0),
                                            Align2::LEFT_CENTER,
                                            &entry.0,
                                            egui::FontId::proportional(13.0),
                                            if selected { colors.accent } else { colors.text },
                                        );
                                        if !entry.1.is_empty() {
                                            ui.painter().text(
                                                rect.right_center() - vec2(10.0, 0.0),
                                                Align2::RIGHT_CENTER,
                                                entry.1,
                                                egui::FontId::proportional(10.5),
                                                colors.text_faint,
                                            );
                                        }
                                        if response.hovered() {
                                            index = position;
                                        }
                                        if response.clicked() {
                                            chosen = Some(entry.2);
                                            close = true;
                                        }
                                    }
                                });
                        }
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(crate::i18n::tr("↑↓ 选择 · Enter 执行 · Esc 关闭"))
                                .size(10.0)
                                .color(colors.text_faint),
                        );
                    });
            });

        self.command_palette_query = query;
        self.command_palette_index = index;
        self.command_palette_focus = request_focus;
        if close {
            self.command_palette_open = false;
            self.command_palette_query.clear();
            self.command_palette_index = 0;
        }
        if let Some(action) = chosen {
            self.run_palette_action(action);
        }
    }

    pub fn toggle_lyrics_sidebar(&mut self) {
        self.show_lyrics_sidebar = !self.show_lyrics_sidebar;
        self.settings.lyrics.sidebar_visible = self.show_lyrics_sidebar;
        self.settings_dirty = true;
        self.ctx.request_repaint();
    }

    pub fn toggle_desktop_lyrics(&mut self) {
        if !self.settings.features.desktop_lyrics || !self.settings.desktop_lyrics.enabled {
            self.toast(
                crate::i18n::tr("桌面歌词已在功能开关中关闭"),
                ToastKind::Info,
            );
            return;
        }
        let visible = !self.settings.desktop_lyrics.visible_at_start;
        self.set_desktop_lyrics_visible(visible);
        self.toast(
            if visible {
                crate::i18n::tr("桌面歌词已显示")
            } else {
                crate::i18n::tr("桌面歌词已隐藏")
            },
            ToastKind::Info,
        );
    }

    pub fn set_desktop_lyrics_visible(&mut self, visible: bool) {
        self.settings.desktop_lyrics.visible_at_start = visible;
        self.settings_dirty = true;
        if !visible {
            self.stop_native_desktop_lyrics();
        }
        self.sync_tray();
        self.ctx.request_repaint();
    }

    /// Open/close and configure the independent desktop lyrics viewport.
    /// Called from the root UI pass; the viewport itself runs on its own
    /// repaint schedule.
    // ------------------------------------------------------------------
    // P9: mini player viewport
    // ------------------------------------------------------------------

    pub fn toggle_mini_player(&mut self) {
        if !self.settings.features.mini_player {
            self.toast(
                crate::i18n::tr("迷你播放器已在功能开关中关闭"),
                ToastKind::Info,
            );
            return;
        }
        self.settings.mini.visible = !self.settings.mini.visible;
        self.settings_dirty = true;
        if !self.settings.mini.visible {
            self.mini_spawned = false;
        }
        self.ctx.request_repaint();
    }

    pub fn hide_mini_player(&mut self) {
        if self.settings.mini.visible {
            self.settings.mini.visible = false;
            self.settings_dirty = true;
            // Drop the native window now instead of waiting for a root pass
            // (which may be delayed while the main window is occluded).
            if let Some(raw) = window::find_window_by_title("Bamboo Mini Player") {
                window::hide_window(raw);
            }
            self.overlay_activity
                .mini_player
                .store(false, Ordering::Relaxed);
            self.mini_spawned = false;
            self.mini_hwnd = None;
            self.ctx.request_repaint();
        }
    }

    /// Publish the mini player state. This also runs from deferred viewports
    /// (see `logic`) so the mini window keeps a fresh position while the root
    /// window is hidden or repainting at the idle cadence.
    fn update_mini_snapshot(&mut self) {
        let colors = self.colors();
        let title = self
            .current_track
            .as_ref()
            .map(|track| track.display_title())
            .unwrap_or_default();
        let artist = self
            .current_track
            .as_ref()
            .map(|track| track.display_artist())
            .unwrap_or_default();
        let playing = self.is_playing();
        let position = self.position_secs() as f32;
        let position_ms = (self.position_secs().max(0.0) * 1000.0).round() as u64;
        let duration = self.duration_secs() as f32;
        let volume = self.settings.playback.volume;
        let muted = self.settings.playback.muted;
        let cover = self.cover.clone();
        let opacity = self.settings.mini.opacity;
        let show_lyrics = self.settings.mini.show_lyrics;
        let karaoke = self.settings.mini.karaoke;
        let karaoke_words_only = self.settings.mini.karaoke_words_only;
        let lyrics_lines = self.settings.mini.lyrics_lines;
        let lyrics_bold = self.settings.mini.lyrics_bold;
        let show_translation = self.settings.mini.show_translation;
        let lyrics_font_size = self.settings.mini.lyrics_font_size;
        let offset_ms = self.settings.lyrics.offset_ms;
        let lyrics_loading = self.lyrics_loading;
        let lyrics_version = self.lyrics_version;
        let should_have_lyrics = show_lyrics.then_some(self.lyrics.as_ref()).flatten();
        // Karaoke needs the animation cadence; without lyrics the mini window
        // only moves the progress line, so the idle rate is enough.
        let frame_rate = if show_lyrics {
            self.settings.performance.frame_rate
        } else {
            self.settings.performance.idle_frame_rate
        };
        let mut snapshot = self.mini_snapshot.lock().unwrap();
        snapshot.title = title;
        snapshot.artist = artist;
        snapshot.playing = playing;
        snapshot.position = position;
        snapshot.duration = duration;
        snapshot.volume = volume;
        snapshot.muted = muted;
        snapshot.cover = cover;
        snapshot.colors = colors;
        snapshot.opacity = opacity;
        snapshot.show_lyrics = show_lyrics;
        snapshot.karaoke = karaoke;
        snapshot.karaoke_words_only = karaoke_words_only;
        snapshot.lyrics_lines = lyrics_lines;
        snapshot.lyrics_bold = lyrics_bold;
        snapshot.show_translation = show_translation;
        snapshot.lyrics_font_size = lyrics_font_size;
        snapshot.position_ms = position_ms;
        snapshot.position_updated = Some(Instant::now());
        snapshot.offset_ms = offset_ms;
        snapshot.lyrics_loading = lyrics_loading;
        snapshot.frame_rate = frame_rate;
        // Clone the parsed lyrics only when they changed: the snapshot is
        // rebuilt on every tick while the mini window is visible. Clearing the
        // lyrics when the option is off also frees the clone.
        let update_lyrics = match (&should_have_lyrics, &snapshot.lyrics) {
            (Some(_), None) | (None, Some(_)) => true,
            (Some(_), Some(_)) => snapshot.lyrics_version != lyrics_version,
            (None, None) => false,
        };
        if update_lyrics {
            snapshot.lyrics = should_have_lyrics.map(|lyrics| Arc::new(lyrics.clone()));
            snapshot.lyrics_version = lyrics_version;
        }
    }

    fn sync_mini_player(&mut self, ctx: &Context) {
        if !self.settings.features.mini_player || !self.settings.mini.visible {
            self.stop_native_mini();
            self.mini_native_active = false;
            self.overlay_activity
                .mini_player
                .store(false, Ordering::Relaxed);
            self.mini_spawned = false;
            self.mini_hwnd = None;
            self.mini_taskbar = None;
            return;
        }
        if self.settings.mini.native_window && self.ensure_native_mini() {
            // The native window has its own timer and interacts directly with
            // the host. Drop the egui viewport if the option was just enabled.
            if self.mini_spawned {
                ctx.send_viewport_cmd_to(
                    views::mini_player::viewport_id(),
                    egui::ViewportCommand::Close,
                );
            }
            self.overlay_activity
                .mini_player
                .store(false, Ordering::Relaxed);
            self.mini_native_active = true;
            self.mini_spawned = false;
            self.mini_hwnd = None;
            self.mini_taskbar = None;
            self.sync_native_mini(ctx);
            return;
        }
        // Either the user chose the egui renderer or the native window could
        // not start; the immediate viewport remains the fallback.
        self.stop_native_mini();
        self.mini_native_active = false;
        self.overlay_activity
            .mini_player
            .store(true, Ordering::Relaxed);
        self.update_mini_snapshot();

        let width = views::mini_player::window_width(&self.settings.mini);
        let height = views::mini_player::window_height(&self.settings.mini);
        let mut builder = egui::ViewportBuilder::default()
            .with_title("Bamboo Mini Player")
            .with_min_inner_size([280.0, 120.0])
            .with_decorations(false)
            .with_transparent(true)
            .with_has_shadow(true)
            .with_resizable(true)
            .with_taskbar(self.settings.mini.show_in_taskbar)
            .with_active(false)
            .with_window_level(views::mini_player::window_level(
                self.settings.mini.always_on_top,
            ));
        // Only pass a size/position while the native window is created. The
        // builder is patched every frame, and sending the (one-frame stale)
        // settings size after a manual resize made the window fight the drag
        // (visible as width/height jitter). Programmatic changes go through
        // `ViewportCommand::InnerSize` below instead.
        if !self.mini_spawned {
            builder = builder.with_inner_size([width, height]);
            if let Some((x, y)) = self.settings.mini.position {
                builder = builder.with_position([x, y]);
            }
        }
        let snapshot = self.mini_snapshot.clone();
        let actions = self.mini_actions.clone();
        let close = self.mini_close.clone();
        let id = views::mini_player::viewport_id();
        let was_hidden = !self.mini_spawned;
        // Immediate (not deferred): eframe paints it inline during the root
        // pass. Deferred viewports on Windows fight over WM_PAINT with the
        // main window and starve each other (egui issues #8466/#7686).
        ctx.show_viewport_immediate(id, builder, move |ui, class| {
            views::mini_player::mini_player_ui(ui, class, &snapshot, &actions, &close);
        });
        if was_hidden {
            ctx.send_viewport_cmd_to(id, egui::ViewportCommand::Visible(true));
        }
        self.mini_spawned = true;

        // ViewportBuilder sizes only apply while the window is created. When
        // the lyric options change on an already open window, ask the OS for
        // the new size once instead of fighting it every frame.
        let desired = (width, height);
        let needs_resize = self
            .mini_snapshot
            .lock()
            .map(|state| {
                state.requested_size.is_none_or(|(w, h)| {
                    (w - desired.0).abs() > 0.5 || (h - desired.1).abs() > 0.5
                })
            })
            .unwrap_or(false);
        if needs_resize {
            ctx.send_viewport_cmd_to(
                id,
                egui::ViewportCommand::InnerSize(vec2(desired.0, desired.1)),
            );
            if let Ok(mut state) = self.mini_snapshot.lock() {
                state.requested_size = Some(desired);
            }
            // The remembered position was saved for the previous size. Pull
            // the window back on-screen when the larger layout would fall off
            // the right/bottom edge.
            let position = self
                .mini_snapshot
                .lock()
                .ok()
                .and_then(|state| state.window_pos)
                .or(self.settings.mini.position);
            if let Some((x, y)) = position {
                let clamped = views::desktop_lyrics::clamp_position(
                    egui::pos2(x, y),
                    desired.0,
                    desired.1,
                );
                if (clamped.x - x).abs() > 1.0 || (clamped.y - y).abs() > 1.0 {
                    ctx.send_viewport_cmd_to(
                        id,
                        egui::ViewportCommand::OuterPosition(egui::pos2(
                            clamped.x, clamped.y,
                        )),
                    );
                    self.settings.mini.position = Some((clamped.x, clamped.y));
                    self.settings_dirty = true;
                }
            }
        } else {
            // No programmatic resize is pending, so a reported size that
            // differs from what we asked for comes from the user dragging the
            // window edge. Persist it and turn auto-height off (same behaviour
            // as the desktop lyrics window).
            let observed = self
                .mini_snapshot
                .lock()
                .ok()
                .map(|state| (state.window_size, state.requested_size));
            if let Some((Some((w, h)), requested)) = observed
                && w.is_finite()
                && h.is_finite()
                && requested.is_none_or(|(rw, rh)| {
                    (w - rw).abs() > 2.0 || (h - rh).abs() > 2.0
                })
            {
                let width = w.clamp(280.0, 900.0);
                let height = h.clamp(120.0, 600.0);
                // If the drag ended below the minimum size, bring the window
                // back once (the OS may allow a programme-driven size below
                // its own minimum track size).
                let outside = (w - width).abs() > 1.0 || (h - height).abs() > 1.0;
                if outside && requested != Some((width, height)) {
                    ctx.send_viewport_cmd_to(
                        id,
                        egui::ViewportCommand::InnerSize(vec2(width, height)),
                    );
                }
                self.settings.mini.width = width;
                self.settings.mini.height = height;
                self.settings.mini.auto_height = false;
                self.settings_dirty = true;
                if let Ok(mut state) = self.mini_snapshot.lock() {
                    state.requested_size = Some((width, height));
                }
            }
        }

        if self.mini_hwnd.is_none()
            && let Some(raw) = window::find_window_by_title("Bamboo Mini Player")
        {
            window::apply_overlay_style(raw, false);
            window::set_taskbar_visible(raw, self.settings.mini.show_in_taskbar);
            self.mini_taskbar = Some(self.settings.mini.show_in_taskbar);
            self.mini_hwnd = Some(raw);
        }
        if let Some(raw) = self.mini_hwnd
            && self.mini_taskbar != Some(self.settings.mini.show_in_taskbar)
        {
            window::set_taskbar_visible(raw, self.settings.mini.show_in_taskbar);
            self.mini_taskbar = Some(self.settings.mini.show_in_taskbar);
        }

        self.handle_mini_actions(ctx);

        let position = self.mini_snapshot.lock().unwrap().window_pos;
        if let Some(position) = position {
            let changed =
                self.settings.mini.position.is_none_or(|(x, y)| {
                    (x - position.0).abs() > 1.0 || (y - position.1).abs() > 1.0
                });
            if changed {
                self.settings.mini.position = Some(position);
                self.settings_dirty = true;
            }
        }
    }

    /// Drain the mini player's command queue. Called from `raw_input_hook`
    /// (every viewport) and from the root logic pass, so the tiny window keeps
    /// responding even when the main window is occluded and not repainting.
    fn handle_mini_actions(&mut self, ctx: &Context) {
        if self.mini_close.swap(false, Ordering::Relaxed) {
            self.hide_mini_player();
            return;
        }
        let queued: Vec<views::mini_player::MiniPlayerAction> =
            self.mini_actions.lock().unwrap().drain(..).collect();
        if queued.is_empty() {
            return;
        }
        for action in queued {
            match action {
                views::mini_player::MiniPlayerAction::PlayPause => self.toggle_play(),
                views::mini_player::MiniPlayerAction::Prev => self.prev_track(),
                views::mini_player::MiniPlayerAction::Next => self.next_track(),
                views::mini_player::MiniPlayerAction::ShowMain => self.show_window(ctx),
                views::mini_player::MiniPlayerAction::ToggleMute => self.toggle_mute(),
                views::mini_player::MiniPlayerAction::SetVolume(volume) => {
                    self.set_volume(volume);
                }
                views::mini_player::MiniPlayerAction::Seek(secs) => self.seek_to(secs),
                views::mini_player::MiniPlayerAction::Hide => {
                    self.hide_mini_player();
                    return;
                }
            }
        }
        self.ctx.request_repaint();
        // The mini window painted the click; refresh its own state (play
        // icon, position) right away even if it is paused.
        self.ctx
            .request_repaint_of(views::mini_player::viewport_id());
    }

    // ------------------------------------------------------------------
    // Native mini player
    // ------------------------------------------------------------------

    /// Start the native window on demand. Returns false when it could not be
    /// started, so the caller keeps the egui viewport as a fallback.
    fn ensure_native_mini(&mut self) -> bool {
        if self.native_mini.is_some() {
            return true;
        }
        if Instant::now() < self.native_mini_retry_at {
            return false;
        }
        match crate::platform::native_mini::NativeMini::spawn(overlay_waker(&self.ctx)) {
            Ok(process) => {
                tracing::info!("native mini player window started");
                self.native_mini = Some(process);
                // An empty vector cannot equal a real serialized signature,
                // so the first check always sends the full state.
                self.native_mini_signature = Some(Vec::new());
                self.native_mini_sent_lyrics_version = u64::MAX;
                self.native_mini_sent_cover_version = u64::MAX;
                self.native_mini_last_state_check = Instant::now() - Duration::from_secs(5);
                true
            }
            Err(err) => {
                tracing::warn!("could not start native mini player: {err}");
                self.native_mini_retry_at = Instant::now() + Duration::from_secs(5);
                false
            }
        }
    }

    fn stop_native_mini(&mut self) {
        if let Some(mut process) = self.native_mini.take() {
            tracing::info!("stopping native mini player");
            process.shutdown();
        }
        self.native_mini_signature = None;
        self.native_mini_sent_lyrics_version = u64::MAX;
        self.native_mini_sent_cover_version = u64::MAX;
        self.native_mini_last_playing = false;
    }

    fn sync_native_mini(&mut self, ctx: &Context) {
        let actions = self
            .native_mini
            .as_ref()
            .map(crate::platform::native_mini::NativeMini::take_actions)
            .unwrap_or_default();
        self.handle_native_mini_actions(ctx, actions);

        if self
            .native_mini
            .as_mut()
            .is_some_and(|process| !process.is_alive())
        {
            tracing::warn!("native mini player window exited unexpectedly");
            self.native_mini = None;
            self.native_mini_signature = None;
            self.native_mini_retry_at = Instant::now() + Duration::from_secs(1);
            return;
        }

        let position_ms = (self.position_secs().max(0.0) * 1000.0).round() as u64;
        let playing = self.is_playing();
        let position_changed = position_ms.abs_diff(self.native_mini_last_position_ms) > 50;
        let playing_changed = playing != self.native_mini_last_playing;
        let send_tick = playing_changed
            || position_changed
            || (playing && self.native_mini_last_tick.elapsed() >= Duration::from_millis(100));

        let mut state_message = None;
        if self.native_mini_last_state_check.elapsed() >= Duration::from_millis(100)
            || self.native_mini_signature.is_none()
        {
            self.native_mini_last_state_check = Instant::now();
            let include_lyrics = self.lyrics_version != self.native_mini_sent_lyrics_version;
            let include_cover = self.smtc_cover_rev != self.native_mini_sent_cover_version;
            let state = self.build_mini_state(include_lyrics, include_cover);
            let mut signature_state = state.clone();
            signature_state.position = None;
            signature_state.position_ms = 0;
            signature_state.playing = false;
            signature_state.lyrics = None;
            signature_state.cover_png = None;
            let signature = serde_json::to_vec(&signature_state).ok();
            if signature != self.native_mini_signature {
                self.native_mini_signature = signature;
                self.native_mini_sent_lyrics_version = self.lyrics_version;
                self.native_mini_sent_cover_version = self.smtc_cover_rev;
                state_message = Some(crate::overlay_protocol::MiniHostMessage::State(Box::new(
                    state,
                )));
            }
        }

        let Some(process) = self.native_mini.as_ref() else {
            return;
        };
        if let Some(message) = state_message {
            process.send(message);
        }
        if send_tick {
            process.send(crate::overlay_protocol::MiniHostMessage::Position {
                position_ms,
                playing,
            });
            self.native_mini_last_position_ms = position_ms;
            self.native_mini_last_playing = playing;
            self.native_mini_last_tick = Instant::now();
        }
    }

    fn handle_native_mini_actions(&mut self, ctx: &Context, actions: Vec<crate::overlay_protocol::MiniAction>) {
        if actions.is_empty() {
            return;
        }
        for action in actions {
            match action {
                crate::overlay_protocol::MiniAction::PlayPause => self.toggle_play(),
                crate::overlay_protocol::MiniAction::Prev => self.prev_track(),
                crate::overlay_protocol::MiniAction::Next => self.next_track(),
                crate::overlay_protocol::MiniAction::ShowMain => self.show_window(ctx),
                crate::overlay_protocol::MiniAction::ToggleMute => self.toggle_mute(),
                crate::overlay_protocol::MiniAction::SetVolume { volume } => {
                    self.set_volume(volume);
                }
                crate::overlay_protocol::MiniAction::Seek { position_ms } => {
                    self.seek_to(position_ms as f64 / 1000.0);
                }
                crate::overlay_protocol::MiniAction::Geometry {
                    x,
                    y,
                    width,
                    height,
                    resized,
                } => {
                    let changed = self.settings.mini.position.is_none_or(|(px, py)| {
                        (px - x).abs() > 1.0 || (py - y).abs() > 1.0
                    });
                    if changed {
                        self.settings.mini.position = Some((x, y));
                        self.settings_dirty = true;
                    }
                    if resized
                        && ((self.settings.mini.width - width).abs() > 0.5
                            || (self.settings.mini.height - height).abs() > 0.5)
                    {
                        self.settings.mini.width = width.clamp(280.0, 900.0);
                        self.settings.mini.height = height.clamp(120.0, 600.0);
                        self.settings.mini.auto_height = false;
                        self.settings_dirty = true;
                    }
                }
                crate::overlay_protocol::MiniAction::Hide => {
                    self.hide_mini_player();
                    return;
                }
            }
        }
        // Apply the command and push a fresh state next frame.
        self.ctx.request_repaint();
    }

    fn build_mini_state(
        &self,
        include_lyrics: bool,
        include_cover: bool,
    ) -> crate::overlay_protocol::MiniState {
        let (title, artist) = match self.current_track.as_ref() {
            Some(track) => (
                track.display_title().to_string(),
                track.display_artist().to_string(),
            ),
            None => (String::new(), String::new()),
        };
        let frame_rate = if self.settings.mini.show_lyrics {
            self.settings.performance.frame_rate
        } else {
            self.settings.performance.idle_frame_rate
        };
        crate::overlay_protocol::MiniState {
            title,
            artist,
            playing: self.is_playing(),
            position_ms: (self.position_secs().max(0.0) * 1000.0).round() as u64,
            duration_ms: (self.duration_secs().max(0.0) * 1000.0).round() as u64,
            volume: self.settings.playback.volume,
            muted: self.settings.playback.muted,
            cover_version: self.smtc_cover_rev,
            cover_png: if include_cover {
                self.smtc_cover_png.clone()
            } else {
                None
            },
            lyrics_version: self.lyrics_version,
            lyrics: if include_lyrics {
                self.overlay_lyrics()
            } else {
                None
            },
            lyrics_loading: self.lyrics_loading,
            offset_ms: self.settings.lyrics.offset_ms,
            show_lyrics: self.settings.mini.show_lyrics,
            karaoke: self.settings.mini.karaoke,
            karaoke_words_only: self.settings.mini.karaoke_words_only,
            lyrics_lines: self.settings.mini.lyrics_lines,
            lyrics_bold: self.settings.mini.lyrics_bold,
            show_translation: self.settings.mini.show_translation,
            lyrics_font_size: self.settings.mini.lyrics_font_size,
            width: views::mini_player::window_width(&self.settings.mini),
            height: views::mini_player::window_height(&self.settings.mini),
            opacity: self.settings.mini.opacity,
            always_on_top: self.settings.mini.always_on_top,
            show_in_taskbar: self.settings.mini.show_in_taskbar,
            frame_rate,
            font_family: self.settings.theme.font_family.clone(),
            colors: self.mini_colors(),
            position: self.settings.mini.position,
            labels: self.mini_labels(),
        }
    }

    fn mini_colors(&self) -> crate::overlay_protocol::MiniColors {
        let colors = self.colors();
        let rgba = |color: egui::Color32| [color.r(), color.g(), color.b(), color.a()];
        crate::overlay_protocol::MiniColors {
            panel: rgba(colors.panel),
            card: rgba(colors.card),
            card_hover: rgba(colors.card_hover),
            line: rgba(colors.line),
            text: rgba(colors.text),
            text_weak: rgba(colors.text_weak),
            text_faint: rgba(colors.text_faint),
            accent: rgba(colors.accent),
        }
    }

    fn mini_labels(&self) -> crate::overlay_protocol::MiniLabels {
        use crate::i18n::tr;
        crate::overlay_protocol::MiniLabels {
            loading: tr("正在查找歌词…").to_string(),
            no_lyrics: tr("暂无歌词").to_string(),
            not_playing: tr("未在播放").to_string(),
            mini_player: tr("迷你播放器").to_string(),
            play: tr("播放").to_string(),
            pause: tr("暂停").to_string(),
            prev: tr("上一首").to_string(),
            next: tr("下一首").to_string(),
            mute: tr("静音").to_string(),
            unmute: tr("取消静音").to_string(),
            close: tr("关闭迷你播放器").to_string(),
        }
    }

    /// Open the standalone fullscreen lyrics window.
    pub fn open_lyrics_window(&mut self) {
        if !self.settings.features.fullscreen_lyrics {
            self.toast(crate::i18n::tr("大歌词页已在设置中关闭"), ToastKind::Info);
            return;
        }
        if !self.lyrics_window_open {
            self.lyrics_window_open = true;
            self.lyrics_window_close.store(false, Ordering::Relaxed);
            // Respect the user's chosen mode (settings checkbox or the mode
            // remembered from the previous session). In windowed mode the
            // window is resizable and restores its saved size/position.
            self.lyrics_window_size_pending = self.lyrics_window_windowed;
            self.lyrics_window_protected = None;
            if let Ok(mut state) = self.lyrics_window_snapshot.lock() {
                state.windowed = self.lyrics_window_windowed;
            }
            let id = views::lyrics_window::viewport_id();
            self.ctx.send_viewport_cmd_to(
                id,
                egui::ViewportCommand::Fullscreen(!self.lyrics_window_windowed),
            );
            // The viewport may survive a previous close as a hidden native
            // window; make sure a re-open actually shows it again.
            self.ctx
                .send_viewport_cmd_to(id, egui::ViewportCommand::Visible(true));
            self.session_dirty = true;
            self.ctx.request_repaint();
        }
    }

    /// Switch the standalone window between borderless fullscreen and a normal
    /// resizable window. The choice and the window geometry are remembered.
    pub fn set_lyrics_window_windowed(&mut self, windowed: bool) {
        if self.lyrics_window_windowed == windowed {
            return;
        }
        self.lyrics_window_windowed = windowed;
        self.session.lyrics_window_windowed = windowed;
        self.lyrics_window_size_pending = windowed;
        self.session_dirty = true;
        if let Ok(mut state) = self.lyrics_window_snapshot.lock() {
            state.windowed = windowed;
        }
        let id = views::lyrics_window::viewport_id();
        if self.lyrics_window_open {
            self.ctx
                .send_viewport_cmd_to(id, egui::ViewportCommand::Fullscreen(!windowed));
        }
        self.ctx.request_repaint_of(id);
        self.ctx.request_repaint();
    }

    pub fn close_lyrics_window(&mut self) {
        if self.lyrics_window_open {
            self.lyrics_window_open = false;
            self.overlay_activity
                .lyrics_window
                .store(false, Ordering::Relaxed);
            self.lyrics_window_protected = None;
            // Removing the egui viewport only happens on the next root pass,
            // which can be long delayed while the main window is occluded or
            // hidden. Hide the native window right now so the close button
            // and Esc feel immediate; the viewport is cleaned up afterwards.
            if let Some(raw) = window::find_window_by_title("Bamboo Lyrics") {
                window::hide_window(raw);
            }
            self.session_dirty = true;
            self.ctx.request_repaint();
        }
    }

    pub fn toggle_lyrics_window(&mut self) {
        if self.lyrics_window_open {
            self.close_lyrics_window();
        } else {
            self.open_lyrics_window();
        }
    }

    /// Publish the current lyric/playback state to the deferred standalone
    /// window. Called from logic, so it also runs while the main window is
    /// hidden in the tray.
    fn update_lyrics_window_snapshot(&mut self) {
        if !self.lyrics_window_open {
            return;
        }
        let position_ms = (self.position_secs().max(0.0) * 1000.0).round() as u64;
        let duration = self.duration_secs();
        let duration_ms = (duration > 0.0).then(|| (duration * 1000.0) as u64);
        let (title, artist, album) = match self.current_track.as_ref() {
            Some(track) => (
                track.display_title().to_string(),
                track.display_artist().to_string(),
                track.album.clone(),
            ),
            None => (String::new(), String::new(), String::new()),
        };
        let Ok(mut state) = self.lyrics_window_snapshot.lock() else {
            return;
        };
        state.title = title;
        state.artist = artist;
        state.album = album;
        state.lyrics_loading = self.lyrics_loading;
        state.playing = self.is_playing();
        state.position_ms = position_ms;
        state.position_updated = Some(Instant::now());
        state.duration_ms = duration_ms;
        state.offset_ms = self.settings.lyrics.offset_ms;
        state.windowed = self.lyrics_window_windowed;
        state.settings = self.settings.lyrics.clone();
        state.large = self.settings.large_lyrics.clone();
        state.frame_rate = self.settings.performance.frame_rate;
        state.colors = self.colors();
        state.cover = self.cover.clone();
        state.background = self.background.clone();
        if state.lyrics_version != self.lyrics_version
            || (self.lyrics.is_none() && state.lyrics.is_some())
        {
            state.lyrics = self
                .lyrics
                .as_ref()
                .map(|lyrics| std::sync::Arc::new(lyrics.clone()));
            state.lyrics_version = self.lyrics_version;
        }
    }

    fn handle_lyrics_window_actions(&mut self) {
        if self.lyrics_window_close.swap(false, Ordering::Relaxed) {
            self.close_lyrics_window();
        }
        let actions = self
            .lyrics_window_actions
            .lock()
            .map(|mut actions| std::mem::take(&mut *actions))
            .unwrap_or_default();
        if actions.is_empty() {
            return;
        }
        let id = views::lyrics_window::viewport_id();
        for action in actions {
            match action {
                views::lyrics_window::LyricsWindowAction::PlayPause => self.toggle_play(),
                views::lyrics_window::LyricsWindowAction::Next => self.next_track(),
                views::lyrics_window::LyricsWindowAction::Prev => self.prev_track(),
                views::lyrics_window::LyricsWindowAction::Seek(ms) => {
                    self.seek_to(ms as f64 / 1000.0);
                }
                views::lyrics_window::LyricsWindowAction::ToggleFullscreen => {
                    self.set_lyrics_window_windowed(!self.lyrics_window_windowed);
                }
            }
        }
        if let Ok(mut state) = self.lyrics_window_snapshot.lock() {
            state.windowed = self.lyrics_window_windowed;
        }
        // Repaint the overlay itself: its controls (play/pause, windowed
        // toggle) changed, and a paused window repaints only slowly.
        self.ctx.request_repaint_of(id);
        self.ctx.request_repaint();
    }

    /// Show the standalone window as a deferred viewport. Unlike an immediate
    /// viewport it is painted by its own event loop, so lyrics keep updating
    /// when the main window is minimized or hidden in the tray.
    fn sync_lyrics_window(&mut self, ctx: &Context) {
        if !self.lyrics_window_open {
            self.overlay_activity
                .lyrics_window
                .store(false, Ordering::Relaxed);
            return;
        }
        self.overlay_activity
            .lyrics_window
            .store(true, Ordering::Relaxed);
        let windowed = self.lyrics_window_windowed;
        let mut builder = egui::ViewportBuilder::default()
            .with_title("Bamboo Lyrics")
            .with_decorations(false)
            .with_taskbar(true)
            .with_active(true);
        if windowed {
            builder = builder
                .with_min_inner_size([640.0, 400.0])
                .with_resizable(true)
                .with_window_level(egui::WindowLevel::Normal);
            // Same as the mini player: the builder is patched every frame, so
            // only pass a size/position while the window is being (re)created.
            // Passing the one-frame-stale saved size during a manual resize
            // made the window fight the drag.
            if self.lyrics_window_size_pending {
                let (width, height) = self
                    .session
                    .lyrics_window_size
                    .map(|(w, h)| (w.clamp(640.0, 4000.0), h.clamp(400.0, 3000.0)))
                    .unwrap_or((1100.0, 700.0));
                builder = builder.with_inner_size([width, height]);
                if let Some((x, y)) = self.session.lyrics_window_pos {
                    builder = builder.with_position([x, y]);
                }
            }
        } else {
            builder = builder
                .with_fullscreen(true)
                .with_resizable(false)
                // Stay above always-on-top overlays (desktop lyrics and other
                // apps); otherwise those windows can swallow clicks aimed at
                // the fullscreen window's controls.
                .with_window_level(egui::WindowLevel::AlwaysOnTop);
        }
        if let Some(index) = self.settings.window.lyrics_monitor
            && let Some(native) = window::native_monitor_index(index)
        {
            builder = builder.with_monitor(native);
        }
        let id = views::lyrics_window::viewport_id();
        let snapshot = self.lyrics_window_snapshot.clone();
        let actions = self.lyrics_window_actions.clone();
        let close = self.lyrics_window_close.clone();
        // Immediate viewport: rendered inline with the root pass. This avoids
        // the Windows WM_PAINT starvation between deferred viewports.
        ctx.show_viewport_immediate(id, builder, move |ui, class| {
            let Ok(state) = snapshot.lock().map(|state| state.clone()) else {
                return;
            };
            let mut actions_out = Vec::new();
            let mut should_close = false;
            ui.push_id("bamboo-lyrics-window-ui", |ui| {
                views::lyrics_window::lyrics_window_ui(
                    ui,
                    class,
                    &snapshot,
                    &state,
                    state.lyrics.as_deref(),
                    &mut actions_out,
                    &mut should_close,
                );
            });
            if should_close {
                close.store(true, Ordering::Relaxed);
            } else if !actions_out.is_empty() {
                if let Ok(mut queue) = actions.lock() {
                    queue.extend(actions_out);
                }
            }
        });
        // Remember a user resize/move of the windowed standalone window. Skip
        // the transition frame: the backend may still report the previous
        // fullscreen geometry right after switching modes.
        if self.lyrics_window_windowed
            && !self.lyrics_window_size_pending
            && let Some((size, position)) = self
                .lyrics_window_snapshot
                .lock()
                .ok()
                .map(|state| (state.window_size, state.window_pos))
        {
            if let Some((width, height)) = size
                && width.is_finite()
                && height.is_finite()
            {
                let width = width.clamp(640.0, 4000.0);
                let height = height.clamp(400.0, 3000.0);
                let changed = self
                    .session
                    .lyrics_window_size
                    .is_none_or(|(sw, sh)| {
                        (sw - width).abs() > 1.0 || (sh - height).abs() > 1.0
                    });
                if changed {
                    self.session.lyrics_window_size = Some((width, height));
                    self.session_dirty = true;
                }
            }
            if let Some((x, y)) = position
                && x.is_finite()
                && y.is_finite()
            {
                let changed = self
                    .session
                    .lyrics_window_pos
                    .is_none_or(|(sx, sy)| (sx - x).abs() > 1.0 || (sy - y).abs() > 1.0);
                if changed {
                    self.session.lyrics_window_pos = Some((x, y));
                    self.session_dirty = true;
                }
            }
        }
        self.lyrics_window_size_pending = false;
        let protected = self.settings.window.lyrics_capture_protection;
        if self.lyrics_window_protected != Some(protected) {
            self.ctx
                .send_viewport_cmd_to(id, egui::ViewportCommand::ContentProtected(protected));
            self.lyrics_window_protected = Some(protected);
        }
    }

    fn sync_desktop_lyrics(&mut self, ctx: &Context) {
        let show = self.desktop_lyrics_should_show();
        if !show {
            self.stop_native_desktop_lyrics();
            return;
        }

        if self.native_desktop_lyrics.is_none() {
            if Instant::now() < self.native_desktop_lyrics_retry_at {
                return;
            }
            match crate::platform::native_overlay::NativeOverlay::spawn(overlay_waker(&self.ctx)) {
                Ok(process) => {
                    tracing::info!("native desktop lyrics window started");
                    self.native_desktop_lyrics = Some(process);
                    self.desktop_lyrics_signature = None;
                    self.desktop_lyrics_sent_lyrics_version = u64::MAX;
                    self.desktop_lyrics_sent_cover_version = u64::MAX;
                    self.desktop_lyrics_last_state_check =
                        Instant::now() - Duration::from_secs(5);
                }
                Err(err) => {
                    tracing::warn!("could not start native desktop lyrics: {err}");
                    self.native_desktop_lyrics_retry_at =
                        Instant::now() + Duration::from_secs(5);
                    return;
                }
            }
        }

        // A new track's cover request clears the old background immediately.
        // Clear the overlay copy too instead of leaving the previous artwork
        // visible until the new cover finishes decoding.
        if self.background.is_none() && self.desktop_lyrics_cover_png.is_some() {
            self.desktop_lyrics_cover_png = None;
            self.desktop_lyrics_cover_version =
                self.desktop_lyrics_cover_version.wrapping_add(1);
        }

        let actions = self
            .native_desktop_lyrics
            .as_ref()
            .map(crate::platform::native_overlay::NativeOverlay::take_actions)
            .unwrap_or_default();
        self.handle_native_desktop_lyrics_actions(ctx, actions);

        if self
            .native_desktop_lyrics
            .as_mut()
            .is_some_and(|process| !process.is_alive())
        {
            self.native_desktop_lyrics = None;
            self.desktop_lyrics_signature = None;
            self.native_desktop_lyrics_retry_at = Instant::now() + Duration::from_secs(1);
            return;
        }

        let position_ms = (self.position_secs().max(0.0) * 1000.0).round() as u64;
        let playing = self.is_playing();
        let position_changed = position_ms.abs_diff(self.desktop_lyrics_last_position_ms) > 50;
        let playing_changed = playing != self.desktop_lyrics_last_playing;
        let send_tick = if playing {
            self.desktop_lyrics_last_tick.elapsed() >= Duration::from_millis(50)
                || position_changed
                || playing_changed
        } else {
            position_changed || playing_changed
        };

        let mut state_message = None;
        if self.desktop_lyrics_last_state_check.elapsed() >= Duration::from_millis(100)
            || self.desktop_lyrics_signature.is_none()
        {
            self.desktop_lyrics_last_state_check = Instant::now();
            let include_lyrics = self.lyrics_version != self.desktop_lyrics_sent_lyrics_version;
            let include_cover =
                self.desktop_lyrics_cover_version != self.desktop_lyrics_sent_cover_version;
            let state = self.build_overlay_state(include_lyrics, include_cover);
            let mut signature_state = state.clone();
            signature_state.settings.position = None;
            signature_state.position_ms = 0;
            signature_state.playing = false;
            signature_state.lyrics = None;
            signature_state.cover_png = None;
            let signature = serde_json::to_vec(&signature_state).ok();
            if signature != self.desktop_lyrics_signature {
                tracing::info!(
                    "desktop lyrics state: pos={:?} size={}x{} auto={} lyrics_v={} cover_v={}",
                    state.settings.position,
                    state.settings.width,
                    state.settings.height,
                    state.settings.auto_size,
                    state.lyrics_version,
                    state.cover_version
                );
                self.desktop_lyrics_signature = signature;
                self.desktop_lyrics_sent_lyrics_version = self.lyrics_version;
                self.desktop_lyrics_sent_cover_version = self.desktop_lyrics_cover_version;
                state_message = Some(crate::overlay_protocol::HostMessage::State(Box::new(state)));
            }
        }

        let reset = std::mem::take(&mut self.desktop_lyrics_reset_pos);
        let Some(process) = self.native_desktop_lyrics.as_ref() else {
            return;
        };
        if reset {
            process.send(crate::overlay_protocol::HostMessage::ResetPosition);
        }
        if let Some(message) = state_message {
            process.send(message);
        }
        if send_tick {
            process.send(crate::overlay_protocol::HostMessage::Position {
                position_ms,
                playing,
            });
            self.desktop_lyrics_last_position_ms = position_ms;
            self.desktop_lyrics_last_playing = playing;
            self.desktop_lyrics_last_tick = Instant::now();
        }
    }

    fn stop_native_desktop_lyrics(&mut self) {
        if let Some(mut process) = self.native_desktop_lyrics.take() {
            process.shutdown();
        }
        self.desktop_lyrics_signature = None;
        self.desktop_lyrics_sent_lyrics_version = u64::MAX;
        self.desktop_lyrics_sent_cover_version = u64::MAX;
        self.desktop_lyrics_last_playing = false;
    }

    fn desktop_lyrics_should_show(&mut self) -> bool {
        // The standalone lyrics window is an explicit, larger duplicate;
        // showing both stacks two lyrics windows and the always-on-top
        // desktop overlay would intercept clicks aimed at the other one.
        if self.lyrics_window_open {
            return false;
        }
        let settings = &self.settings;
        if !settings.features.desktop_lyrics
            || !settings.desktop_lyrics.enabled
            || !settings.desktop_lyrics.visible_at_start
        {
            return false;
        }
        if settings.desktop_lyrics.hide_when_paused && !self.is_playing() {
            return false;
        }
        if settings.desktop_lyrics.hide_when_fullscreen {
            if self.last_fullscreen_probe.elapsed() > Duration::from_millis(1500) {
                self.foreground_fullscreen = window::foreground_is_fullscreen_excluding_self();
                self.last_fullscreen_probe = Instant::now();
            }
            if self.foreground_fullscreen {
                return false;
            }
        }
        true
    }

    fn overlay_settings(&self) -> crate::overlay_protocol::OverlaySettings {
        use crate::config::DesktopBackgroundMode;
        let s = &self.settings.desktop_lyrics;
        crate::overlay_protocol::OverlaySettings {
            locked: s.locked,
            click_through: s.click_through,
            always_on_top: s.always_on_top,
            lines: s.lines,
            show_translation: s.show_translation,
            show_footer: s.show_footer,
            font_family: s.font_family.clone(),
            font_size: s.font_size,
            next_font_size: s.next_font_size,
            prev_font_size: s.prev_font_size,
            footer_font_size: s.footer_font_size,
            bold: s.bold,
            align: match s.align {
                crate::config::LyricsAlign::Left => 0,
                crate::config::LyricsAlign::Center => 1,
                crate::config::LyricsAlign::Right => 2,
            },
            current_color: s.current_color,
            next_color: s.next_color,
            prev_color: s.prev_color,
            footer_color: s.footer_color,
            translation_color: s.translation_color,
            background: match s.background {
                DesktopBackgroundMode::Transparent => 0,
                DesktopBackgroundMode::Solid => 1,
                DesktopBackgroundMode::Gradient => 2,
                DesktopBackgroundMode::Image => 3,
                DesktopBackgroundMode::BlurCover => 4,
            },
            bg_color: s.bg_color,
            bg_color2: s.bg_color2,
            bg_image: s
                .bg_image
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
            opacity: s.opacity,
            radius: s.radius,
            padding: s.padding,
            line_spacing: s.line_spacing,
            shadow: s.shadow,
            outline: s.outline,
            translation_outline: s.translation_outline,
            translation_shadow: s.translation_shadow,
            footer_outline: s.footer_outline,
            footer_shadow: s.footer_shadow,
            border: s.border,
            window_shadow: s.window_shadow,
            outline_color: s.outline_color,
            outline_width: s.outline_width,
            shadow_color: s.shadow_color,
            shadow_offset: s.shadow_offset,
            capture_protection: s.capture_protection,
            karaoke: s.karaoke,
            karaoke_words_only: s.karaoke_words_only,
            width: s.width,
            height: s.height,
            auto_size: s.auto_size,
            position: s.position.or(self.session.desktop_lyrics_pos),
            frame_rate: self.settings.performance.frame_rate,
        }
    }

    fn overlay_labels(&self) -> crate::overlay_protocol::OverlayLabels {
        use crate::i18n::tr;
        crate::overlay_protocol::OverlayLabels {
            open_main: tr("打开主窗口").to_string(),
            open_settings: tr("桌面歌词设置…").to_string(),
            lock: tr("锁定位置").to_string(),
            click_through: tr("鼠标穿透").to_string(),
            always_on_top: tr("窗口置顶").to_string(),
            lines: tr("显示行数").to_string(),
            line_1: crate::trf!("{} 行", 1),
            line_2: crate::trf!("{} 行", 2),
            line_3: crate::trf!("{} 行", 3),
            align: tr("对齐方向").to_string(),
            align_left: tr("左对齐").to_string(),
            align_center: tr("居中").to_string(),
            align_right: tr("右对齐").to_string(),
            auto_size: tr("自动高度").to_string(),
            window_size: tr("窗口大小").to_string(),
            size_small: tr("小（600×120）").to_string(),
            size_medium: tr("中（900×190）").to_string(),
            size_large: tr("大（1200×280）").to_string(),
            font_increase: tr("字号增大").to_string(),
            font_decrease: tr("字号减小").to_string(),
            reset_position: tr("重置位置").to_string(),
            hide: tr("隐藏桌面歌词").to_string(),
            loading: tr("正在查找歌词…").to_string(),
            no_lyrics: tr("暂无歌词").to_string(),
            app_name: "Bamboo Music Player".to_string(),
        }
    }

    /// Flatten parsed lyrics into the wire type shared by the native desktop
    /// lyrics window and the native mini player.
    fn overlay_lyrics(&self) -> Option<crate::overlay_protocol::OverlayLyrics> {
        self.lyrics
            .as_ref()
            .map(|lyrics| crate::overlay_protocol::OverlayLyrics {
                lines: lyrics
                    .lines
                    .iter()
                    .map(|line| crate::overlay_protocol::OverlayLine {
                        start_ms: line.start_ms,
                        end_ms: line.end_ms,
                        text: line.text.clone(),
                        translation: line.translation.clone(),
                        words: line
                            .words
                            .iter()
                            .map(|word| crate::overlay_protocol::OverlayWord {
                                start_ms: word.start_ms,
                                end_ms: word.end_ms,
                                text: word.text.clone(),
                            })
                            .collect(),
                    })
                    .collect(),
                offset_ms: lyrics.offset_ms,
                timed: lyrics.timed,
            })
    }

    fn build_overlay_state(
        &self,
        include_lyrics: bool,
        include_cover: bool,
    ) -> crate::overlay_protocol::OverlayState {
        let (title, artist) = match self.current_track.as_ref() {
            Some(track) => (
                track.display_title().to_string(),
                track.display_artist().to_string(),
            ),
            None => (String::new(), String::new()),
        };
        let lyrics = if include_lyrics {
            self.overlay_lyrics()
        } else {
            None
        };
        crate::overlay_protocol::OverlayState {
            settings: self.overlay_settings(),
            title,
            artist,
            lyrics_version: self.lyrics_version,
            lyrics,
            lyrics_loading: self.lyrics_loading,
            playing: self.is_playing(),
            position_ms: (self.position_secs().max(0.0) * 1000.0).round() as u64,
            offset_ms: self.settings.lyrics.offset_ms,
            cover_version: self.desktop_lyrics_cover_version,
            cover_png: if include_cover {
                self.desktop_lyrics_cover_png.clone()
            } else {
                None
            },
            labels: self.overlay_labels(),
        }
    }

    fn handle_native_desktop_lyrics_actions(
        &mut self,
        ctx: &Context,
        actions: Vec<crate::overlay_protocol::OverlayAction>,
    ) {
        use crate::overlay_protocol::OverlayAction;
        let mut settings_changed = false;
        let mut hide = false;
        for action in actions {
            match action {
                OverlayAction::ToggleLock => {
                    self.settings.desktop_lyrics.locked =
                        !self.settings.desktop_lyrics.locked;
                    settings_changed = true;
                }
                OverlayAction::ToggleClickThrough => {
                    self.settings.desktop_lyrics.click_through =
                        !self.settings.desktop_lyrics.click_through;
                    settings_changed = true;
                }
                OverlayAction::ToggleAlwaysOnTop => {
                    self.settings.desktop_lyrics.always_on_top =
                        !self.settings.desktop_lyrics.always_on_top;
                    settings_changed = true;
                }
                OverlayAction::Hide => hide = true,
                OverlayAction::ShowMainWindow => self.show_window(ctx),
                OverlayAction::OpenSettings => {
                    self.show_window(ctx);
                    self.set_page(Page::Settings);
                }
                OverlayAction::ResetPosition => {
                    self.session.desktop_lyrics_pos = None;
                    self.settings.desktop_lyrics.position = None;
                    self.desktop_lyrics_reset_pos = true;
                    settings_changed = true;
                }
                OverlayAction::FontDelta { delta } => {
                    let size = (self.settings.desktop_lyrics.font_size + delta as f32)
                        .clamp(16.0, 96.0);
                    self.settings.desktop_lyrics.font_size = size;
                    settings_changed = true;
                }
                OverlayAction::SetLines { lines } => {
                    self.settings.desktop_lyrics.lines = lines.clamp(1, 3);
                    settings_changed = true;
                }
                OverlayAction::SetAlign { align } => {
                    self.settings.desktop_lyrics.align = match align {
                        0 => crate::config::LyricsAlign::Left,
                        2 => crate::config::LyricsAlign::Right,
                        _ => crate::config::LyricsAlign::Center,
                    };
                    settings_changed = true;
                }
                OverlayAction::ToggleAutoSize => {
                    self.settings.desktop_lyrics.auto_size =
                        !self.settings.desktop_lyrics.auto_size;
                    settings_changed = true;
                }
                OverlayAction::SetAutoSize { enabled } => {
                    self.settings.desktop_lyrics.auto_size = enabled;
                    settings_changed = true;
                }
                OverlayAction::SetSize { width, height } => {
                    self.settings.desktop_lyrics.width = width;
                    self.settings.desktop_lyrics.height = height;
                    self.settings.desktop_lyrics.auto_size = false;
                    settings_changed = true;
                }
                OverlayAction::Geometry {
                    x,
                    y,
                    width,
                    height,
                    resized,
                } => {
                    tracing::info!(
                        "desktop lyrics geometry: x={x:.1} y={y:.1} size={width:.1}x{height:.1} resized={resized}"
                    );
                    let x = x.clamp(-100_000.0, 100_000.0);
                    let y = y.clamp(-100_000.0, 100_000.0);
                    if self.settings.desktop_lyrics.position != Some((x, y)) {
                        self.settings.desktop_lyrics.position = Some((x, y));
                        self.settings_dirty = true;
                    }
                    if self.session.desktop_lyrics_pos != Some((x, y)) {
                        self.session.desktop_lyrics_pos = Some((x, y));
                        self.session_dirty = true;
                    }
                    if resized {
                        self.settings.desktop_lyrics.auto_size = false;
                        self.settings.desktop_lyrics.width = width.clamp(240.0, 2000.0);
                        self.settings.desktop_lyrics.height = height.clamp(48.0, 700.0);
                        self.settings_dirty = true;
                    }
                }
            }
        }
        if hide {
            self.set_desktop_lyrics_visible(false);
        }
        if settings_changed {
            self.settings_dirty = true;
            self.ctx.request_repaint();
            self.sync_tray();
        }
    }

    // ------------------------------------------------------------------
    // Lyrics
    // ------------------------------------------------------------------

    fn refresh_lyrics(&mut self, track: &Track) {
        self.lyrics_service.cancel();
        self.lyrics_version = self.lyrics_version.wrapping_add(1);
        self.lyrics = None;
        self.lyrics_loading = false;
        self.lyrics_error = None;
        self.lyrics_manual_scroll_until = None;
        self.lyrics_key = track.dedupe_key();
        if !self.settings.lyrics.enabled {
            self.lyrics_error = Some(crate::i18n::tr("歌词功能已关闭").to_string());
            return;
        }
        if !self.settings.lyrics.auto_load {
            return;
        }
        // The next-track prefetch may already have parsed this file while the
        // previous song was still playing.
        if let Some((cached, error)) = self.lyrics_prefetch_cache.remove(&self.lyrics_key) {
            self.lyrics_prefetched.remove(&self.lyrics_key);
            self.lyrics = cached;
            self.lyrics_error = error;
            self.ctx.request_repaint();
            return;
        }
        if let Some(path) = track.source.path() {
            let region = self.translation_region();
            self.lyrics_service.request_auto(
                self.lyrics_key.clone(),
                path.to_path_buf(),
                self.settings.lyrics.encoding,
                &region,
            );
            self.lyrics_loading = true;
        } else if matches!(track.source, TrackSource::Bamboo { .. }) {
            // A completed stream cache may have a sidecar (including a
            // translation file); reload it without contacting the source.
            if let Some(EngineSource::Local(cached)) = self.cached_engine_source_for(track) {
                let region = self.translation_region();
                self.lyrics_service.request_auto(
                    self.lyrics_key.clone(),
                    cached,
                    self.settings.lyrics.encoding,
                    &region,
                );
                self.lyrics_loading = true;
            }
            // Otherwise the resolve request started by `start_online_load`
            // returns the LRC and owns the loading flag. A restored-but-not-
            // yet-played online track must not show a spinner forever.
        } else {
            self.lyrics_error = Some(crate::i18n::tr("该曲目没有可用的歌词来源").to_string());
        }
    }

    /// Drop lyrics parsed for upcoming tracks (settings changed or prefetch
    /// was turned off).
    pub fn clear_lyrics_prefetch(&mut self) {
        self.lyrics_prefetch_cache.clear();
        self.lyrics_prefetched.clear();
        self.lyrics_prefetch_order.clear();
    }

    /// Manual refresh of the current track: re-read local tags and/or ask the
    /// online source for fresh metadata and LRC, ignoring the normal TTL.
    pub fn refresh_current_track(&mut self) {
        let Some(track) = self.current_track.clone() else {
            self.toast(crate::i18n::tr("请先播放一首歌曲"), ToastKind::Info);
            return;
        };
        if matches!(track.source, TrackSource::Bamboo { .. }) {
            // Cached online tracks have a local sidecar worth re-reading. A
            // streaming track has no local file; keep its current lyrics until
            // the forced `info` response arrives.
            if self.cached_engine_source_for(&track).is_some() {
                self.reload_lyrics();
            } else {
                self.clear_lyrics_prefetch();
            }
            if self.settings.network.offline {
                self.toast(
                    crate::i18n::tr("离线模式：已重新读取本地歌词，未请求在线元信息"),
                    ToastKind::Info,
                );
                return;
            }
            if self.online_source_for(&track).is_none() {
                self.toast(
                    crate::i18n::tr("找不到该在线曲目的音源配置"),
                    ToastKind::Error,
                );
                return;
            }
            if self.info_request_id != 0 {
                self.toast(
                    crate::i18n::tr("正在刷新当前曲目元信息…"),
                    ToastKind::Info,
                );
                return;
            }
            self.info_refreshed.remove(&track.dedupe_key());
            self.maybe_request_online_info(&track, true);
            self.toast(
                crate::i18n::tr("正在刷新当前曲目元信息与歌词…"),
                ToastKind::Info,
            );
        } else {
            self.reload_lyrics();
            if let Some(path) = track.source.path() {
                self.metadata.request_force(path.to_path_buf());
            }
            self.toast(
                crate::i18n::tr("正在刷新当前曲目标签与歌词…"),
                ToastKind::Info,
            );
        }
        self.ctx.request_repaint();
    }

    /// Translation sidecar region used for reads and editor saves.
    pub(crate) fn translation_region(&self) -> String {
        crate::lyrics::resolve_region(&self.settings.lyrics.translation_language)
    }

    pub fn reload_lyrics(&mut self) {
        // A manual reload may follow an encoding/translation change; cached
        // prefetched lyrics were parsed with the old settings.
        self.clear_lyrics_prefetch();
        if let Some(track) = self.current_track.clone() {
            self.refresh_lyrics(&track);
            self.ctx.request_repaint();
        }
    }

    pub fn pick_lyrics_file(&mut self) {
        let Some(key) = self.current_track.as_ref().map(Track::dedupe_key) else {
            self.toast(crate::i18n::tr("请先播放一首歌曲"), ToastKind::Info);
            return;
        };
        self.spawn_file_dialog(
            || {
                rfd::FileDialog::new()
                    .add_filter("Lyrics", &["lrc", "srt"])
                    .pick_file()
            },
            move |app, path| {
                let Some(path) = path else {
                    return;
                };
                // The track may have changed while the dialog was open; a
                // sidecar chosen for the old track must not replace the new
                // track's lyrics.
                if app
                    .current_track
                    .as_ref()
                    .map(Track::dedupe_key)
                    .as_deref()
                    != Some(key.as_str())
                {
                    return;
                }
                app.lyrics_prefetch_cache.remove(&key);
                app.lyrics_prefetched.remove(&key);
                app.lyrics_key = key.clone();
                app.lyrics = None;
                app.lyrics_loading = true;
                app.lyrics_error = None;
                let region = app.translation_region();
                app.lyrics_service.request_file(
                    key,
                    path,
                    app.settings.lyrics.encoding,
                    &region,
                );
                app.ctx.request_repaint();
            },
        );
    }

    // ---- P10 lyrics editor ----

    pub fn open_lyrics_editor(&mut self) {
        if !self.settings.features.lyric_editor {
            self.toast(
                crate::i18n::tr("歌词编辑器已在功能开关中关闭"),
                ToastKind::Info,
            );
            return;
        }
        let Some(track) = self.current_track.clone() else {
            self.toast(crate::i18n::tr("请先播放一首歌曲"), ToastKind::Info);
            return;
        };
        let key = track.dedupe_key();
        if let Some(editor) = self.lyrics_editor.as_ref() {
            if editor.track_key == key {
                self.lyrics_editor_open = true;
                self.ctx.request_repaint();
                return;
            }
        }
        let lyrics = (self.lyrics_key == key)
            .then_some(self.lyrics.as_ref())
            .flatten();
        let audio_path = track.source.path().map(|path| path.to_path_buf());
        let mut subtitle = track.display_artist();
        if !track.album.trim().is_empty() {
            if !subtitle.is_empty() {
                subtitle.push_str(" · ");
            }
            subtitle.push_str(&track.album);
        }
        let cover_png = (self.smtc_cover_key == key)
            .then(|| self.smtc_cover_png.clone())
            .flatten();
        let mut editor = crate::lyrics::LyricsEditor::from_lyrics(
            lyrics,
            key,
            track.display_title(),
            audio_path,
        );
        editor.track_subtitle = subtitle;
        editor.cover_png = cover_png;
        editor.translation_region = self.translation_region();
        self.lyrics_editor = Some(editor);
        self.lyrics_editor_open = true;
        self.ctx.request_repaint();
    }

    /// Close the editor, asking first when there are unsaved changes.
    pub fn request_close_lyrics_editor(&mut self) {
        if self
            .lyrics_editor
            .as_ref()
            .is_some_and(|editor| editor.dirty)
        {
            if let Some(editor) = self.lyrics_editor.as_mut() {
                editor.confirm_close = true;
            }
            return;
        }
        self.lyrics_editor_open = false;
    }

    pub fn force_close_lyrics_editor(&mut self) {
        self.lyrics_editor_open = false;
        if let Some(editor) = self.lyrics_editor.as_mut() {
            editor.confirm_close = false;
        }
        self.ctx.request_repaint();
    }

    /// Save the editor document. Returns false when the dialog was cancelled
    /// or writing failed.
    pub fn save_lyrics_editor(&mut self, save_as: bool) -> bool {
        let Some(mut editor) = self.lyrics_editor.take() else {
            return false;
        };
        if editor.lines.iter().all(|line| line.text.trim().is_empty()) {
            editor.status = Some(crate::i18n::tr("歌词内容为空，未保存").to_string());
            self.toast(crate::i18n::tr("歌词内容为空，未保存"), ToastKind::Info);
            self.lyrics_editor = Some(editor);
            return false;
        }
        let mut path = if save_as {
            None
        } else {
            editor.default_save_path()
        };
        if path.is_none() {
            let suggested = editor
                .audio_path
                .as_ref()
                .and_then(|audio| audio.file_stem())
                .map(|stem| format!("{}.lrc", stem.to_string_lossy()))
                .unwrap_or_else(|| "lyrics.lrc".to_string());
            path = rfd::FileDialog::new()
                .add_filter("LRC", &["lrc"])
                .set_file_name(&suggested)
                .save_file();
        }
        let Some(path) = path else {
            self.lyrics_editor = Some(editor);
            return false;
        };
        if let Err(err) = editor.write_to(&path) {
            editor.status = Some(crate::trf!("保存失败: {err}", err));
            self.toast(crate::trf!("歌词保存失败: {err}", err), ToastKind::Error);
            self.lyrics_editor = Some(editor);
            return false;
        }
        // Translations live in `stem.<region>.lrc` beside the base file. An
        // empty translation set removes the sidecar this editor loaded, but
        // never touches a file it did not own.
        let mut translation_warning = None;
        if editor.has_translations() {
            let translation_path = editor.translation_sidecar_path(&path);
            match editor.write_translation_to(&translation_path) {
                Ok(()) => editor.translation_path = Some(translation_path),
                Err(err) => {
                    translation_warning = Some(crate::trf!("译文写入失败: {err}", err));
                }
            }
        } else if let Some(old) = editor.translation_path.take() {
            if old != path {
                let _ = std::fs::remove_file(&old);
            }
        }
        let lyrics = editor.to_lyrics(Some(path.clone()));
        editor.path = Some(path.clone());
        editor.mark_saved();
        editor.confirm_close = false;
        editor.status = Some(match translation_warning.as_ref() {
            Some(warning) => crate::trf!("已保存到 {}（{}）", path.display(), warning),
            None => crate::trf!("已保存到 {}", path.display()),
        });
        // Only replace the live lyrics when they belong to the current track;
        // the editor may still be open for a song the user has moved past.
        let current_key = self.current_track.as_ref().map(Track::dedupe_key);
        self.lyrics_prefetch_cache.remove(&editor.track_key);
        self.lyrics_prefetched.remove(&editor.track_key);
        if current_key.as_deref() == Some(editor.track_key.as_str()) {
            self.lyrics = Some(lyrics);
            self.lyrics_version = self.lyrics_version.wrapping_add(1);
            self.lyrics_key = editor.track_key.clone();
            self.lyrics_error = None;
            self.lyrics_loading = false;
            self.lyrics_manual_scroll_until = None;
        }
        self.toast(
            crate::trf!("歌词已保存: {}", path.display()),
            ToastKind::Success,
        );
        self.lyrics_editor = Some(editor);
        self.ctx.request_repaint();
        true
    }

    /// Re-read the document from disk (or the currently loaded lyrics).
    pub fn reload_lyrics_editor(&mut self) {
        let Some(editor) = self.lyrics_editor.take() else {
            return;
        };
        let region = editor.translation_region.clone();
        let from_file = editor.path.as_ref().and_then(|path| {
            crate::lyrics::load_file_with_translation(
                path,
                self.settings.lyrics.encoding,
                crate::lyrics::LyricsSource::Edited,
                &region,
            )
            .ok()
        });
        let lyrics = from_file.or_else(|| {
            let current = self.current_track.as_ref().map(Track::dedupe_key);
            (current.as_deref() == Some(editor.track_key.as_str()))
                .then(|| self.lyrics.clone())
                .flatten()
        });
        let mut fresh = crate::lyrics::LyricsEditor::from_lyrics(
            lyrics.as_ref(),
            editor.track_key.clone(),
            editor.track_label.clone(),
            editor.audio_path.clone(),
        );
        fresh.path = editor
            .path
            .clone()
            .or_else(|| lyrics.as_ref().and_then(|lyrics| lyrics.path.clone()));
        fresh.track_subtitle = editor.track_subtitle.clone();
        fresh.cover_png = editor.cover_png.clone();
        fresh.translation_region = region;
        fresh.status = Some(if lyrics.is_some() {
            crate::i18n::tr("已重新载入").to_string()
        } else {
            crate::i18n::tr("没有可重新载入的歌词").to_string()
        });
        self.lyrics_editor = Some(fresh);
        self.ctx.request_repaint();
    }

    /// Render the selected lyrics context as a 1200×1600 PNG share card.
    pub fn save_lyrics_share_image(&mut self) {
        let Some(editor) = self.lyrics_editor.as_ref() else {
            return;
        };
        let total = editor.lines.len();
        let start = editor.selected.saturating_sub(2);
        let mut lines = Vec::new();
        let mut highlight = None;
        for index in start..(start + 5).min(total) {
            let text = editor.lines[index].text.trim();
            if text.is_empty() {
                continue;
            }
            if index == editor.selected {
                highlight = Some(lines.len());
            }
            let translation = editor.lines[index]
                .translation
                .as_deref()
                .map(str::trim)
                .filter(|translation| !translation.is_empty())
                .map(str::to_string);
            lines.push(crate::util::share_image::ShareLine {
                text: text.to_string(),
                translation,
            });
        }
        if lines.is_empty() {
            self.toast(crate::i18n::tr("没有可分享的歌词"), ToastKind::Info);
            return;
        }
        let title = editor
            .title
            .clone()
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| editor.track_label.clone());
        let mut subtitle = editor.track_subtitle.clone();
        if subtitle.trim().is_empty() {
            subtitle = editor.artist.clone().unwrap_or_default();
            if let Some(album) = editor
                .album
                .clone()
                .filter(|album| !album.trim().is_empty())
            {
                if !subtitle.is_empty() {
                    subtitle.push_str(" · ");
                }
                subtitle.push_str(&album);
            }
        }
        let cover = editor.cover_png.clone().or_else(|| {
            editor
                .audio_path
                .as_ref()
                .and_then(|path| crate::util::metadata::cover_bytes(path))
        });
        let accent = self.settings.theme.accent;
        let suggested = crate::trf!("{} - 歌词.png", crate::download::sanitize_component(&title));
        let Some(path) = rfd::FileDialog::new()
            .add_filter(crate::i18n::tr("PNG 图片"), &["png"])
            .set_file_name(&suggested)
            .save_file()
        else {
            return;
        };
        let path = if path.extension().is_none() {
            path.with_extension("png")
        } else {
            path
        };
        let card = crate::util::share_image::ShareCard {
            title: &title,
            subtitle: &subtitle,
            lines: &lines,
            highlight,
            cover: cover.as_deref(),
            accent,
        };
        match crate::util::share_image::render_lyrics_card(&path, &card) {
            Ok(()) => self.toast(
                crate::trf!("歌词分享图已保存: {}", path.display()),
                ToastKind::Success,
            ),
            Err(err) => self.toast(err, ToastKind::Error),
        }
        self.ctx.request_repaint();
    }

    fn poll_lyrics(&mut self) {
        while let Some(result) = self.lyrics_service.poll() {
            if result.key != self.lyrics_key {
                self.store_prefetched_lyrics(result);
                continue;
            }
            self.lyrics = result.lyrics;
            self.lyrics_version = self.lyrics_version.wrapping_add(1);
            self.lyrics_loading = false;
            self.lyrics_error = result.error;
            if let Some(lyrics) = self.lyrics.as_ref() {
                tracing::info!(
                    "lyrics loaded: {} lines, timed={}, source={:?}",
                    lyrics.lines.len(),
                    lyrics.timed,
                    lyrics.source
                );
            }
            self.ctx.request_repaint();
        }
    }

    /// Start a background load for the next queue track. Only local files (and
    /// already-cached online streams) are eligible; online LRC arrives with the
    /// URL resolve instead.
    fn prefetch_lyrics_for(&mut self, track: &Track) {
        let path = track.source.path().map(Path::to_path_buf).or_else(|| {
            match self.cached_engine_source_for(track) {
                Some(EngineSource::Local(path)) => Some(path),
                _ => None,
            }
        });
        let Some(path) = path else {
            return;
        };
        let key = track.dedupe_key();
        if key == self.lyrics_key
            || self.lyrics_prefetched.contains(&key)
            || self.lyrics_prefetch_cache.contains_key(&key)
        {
            return;
        }
        // Requests can be dropped when the generation changes before the
        // worker starts them; keep the pending set bounded.
        if self.lyrics_prefetched.len() >= 64 {
            self.lyrics_prefetched.clear();
        }
        self.lyrics_prefetched.insert(key.clone());
        let region = self.translation_region();
        self.lyrics_service.request_prefetch(
            key,
            path,
            self.settings.lyrics.encoding,
            &region,
        );
    }

    /// Keep a bounded cache of prefetched future lyrics.
    fn store_prefetched_lyrics(&mut self, result: crate::lyrics::LyricsResult) {
        const MAX_PREFETCHED: usize = 6;
        if !self.lyrics_prefetched.contains(&result.key) {
            return;
        }
        self.lyrics_prefetch_order.retain(|key| key != &result.key);
        self.lyrics_prefetch_order.push_back(result.key.clone());
        self.lyrics_prefetch_cache
            .insert(result.key, (result.lyrics, result.error));
        while self.lyrics_prefetch_order.len() > MAX_PREFETCHED {
            if let Some(oldest) = self.lyrics_prefetch_order.pop_front() {
                self.lyrics_prefetch_cache.remove(&oldest);
                self.lyrics_prefetched.remove(&oldest);
            }
        }
    }

    /// Open/close the large lyrics page. This is windowed by default; use
    /// F11 or the in-page button to also go full-screen.
    pub fn toggle_fullscreen_lyrics(&mut self) {
        if !self.fullscreen_lyrics && !self.settings.features.fullscreen_lyrics {
            self.toast(crate::i18n::tr("大歌词页已在设置中关闭"), ToastKind::Info);
            return;
        }
        self.fullscreen_lyrics = !self.fullscreen_lyrics;
        if !self.fullscreen_lyrics && self.lyrics_os_fullscreen {
            self.set_lyrics_os_fullscreen(false);
        }
        self.ctx.request_repaint();
    }

    pub fn toggle_lyrics_os_fullscreen(&mut self) {
        if !self.fullscreen_lyrics {
            self.fullscreen_lyrics = true;
        }
        let target = !self.lyrics_os_fullscreen;
        self.set_lyrics_os_fullscreen(target);
    }

    fn set_lyrics_os_fullscreen(&mut self, on: bool) {
        if on {
            // Supersede any restore that has not fired yet.
            self.pending_window_restore = None;
            // Move to the configured display *before* asking winit for
            // fullscreen; borderless fullscreen uses the window's monitor.
            self.lyrics_return_rect = None;
            if let (Some(raw), Some(index)) = (self.hwnd, self.settings.window.lyrics_monitor)
                && crate::platform::window::monitors().get(index).is_some()
            {
                self.lyrics_return_rect = window::window_rect(raw);
                window::move_window_to_monitor(raw, index);
            }
        }
        self.lyrics_os_fullscreen = on;
        match self.settings.window.lyrics_fullscreen {
            crate::config::FullscreenMode::Borderless => {
                // winit maps this to Fullscreen::Borderless, so the display
                // mode is never switched.
                self.ctx.send_viewport_cmd(ViewportCommand::Fullscreen(on));
            }
            crate::config::FullscreenMode::Maximized => {
                if on {
                    self.ctx
                        .send_viewport_cmd(ViewportCommand::Fullscreen(false));
                    self.ctx
                        .send_viewport_cmd(ViewportCommand::Decorations(false));
                    self.ctx.send_viewport_cmd(ViewportCommand::Maximized(true));
                } else {
                    self.ctx
                        .send_viewport_cmd(ViewportCommand::Maximized(false));
                    self.ctx
                        .send_viewport_cmd(ViewportCommand::Decorations(true));
                }
            }
        }
        if !on {
            if let Some(rect) = self.lyrics_return_rect.take() {
                self.pending_window_restore =
                    Some((rect, Instant::now() + Duration::from_millis(600)));
                self.ctx.request_repaint_after(Duration::from_millis(600));
            }
            // Leaving fullscreen re-creates the native frame; re-apply DWM
            // styling after winit has resized the window.
            self.style_reapplied = false;
            self.style_reapply_at = Instant::now() + Duration::from_millis(350);
        }
        self.ctx.request_repaint();
    }

    // ------------------------------------------------------------------
    // Online search / playback
    // ------------------------------------------------------------------

    pub fn search_source_config(&self) -> Option<crate::config::SourceConfig> {
        self.settings
            .source(&self.search_source)
            .or_else(|| {
                self.settings
                    .enabled_sources()
                    .find(|source| !source.base_url.trim().is_empty())
            })
            .cloned()
    }

    pub fn submit_search(&mut self) {
        let keyword = self.search_query.trim().to_string();
        if keyword.is_empty() {
            self.toast(crate::i18n::tr("请输入搜索关键词"), ToastKind::Info);
            return;
        }
        self.search_page = 1;
        self.search_error = None;
        self.search_loading = false;
        self.search_request_id = 0;
        self.album_view = None;
        self.artist_view = None;
        self.clear_search_suggestions();
        if self.settings.network.offline {
            let key = self.search_cache_key(&keyword, 1);
            if self.show_cached_search(&key) {
                self.toast(
                    crate::i18n::tr("离线模式：显示缓存的搜索结果"),
                    ToastKind::Info,
                );
            } else {
                self.search_error = Some(
                    crate::i18n::tr("离线模式已开启，且这个关键词没有缓存结果").to_string(),
                );
                self.search_results.clear();
                self.search_total = 0;
                self.search_cached = false;
                self.toast(
                    crate::i18n::tr("离线模式已开启，已缓存歌曲仍可播放"),
                    ToastKind::Info,
                );
            }
            return;
        }
        let Some(source) = self.search_source_config() else {
            self.toast(
                crate::i18n::tr("没有可用的音源，请先在设置中填写 Bamboo API 地址"),
                ToastKind::Error,
            );
            return;
        };
        self.search_results.clear();
        self.search_total = 0;
        self.search_cached = false;
        self.start_search_request(&source, &keyword, 1);
    }

    fn search_cache_key(&self, keyword: &str, page: u32) -> String {
        let kind = match self.search_kind {
            SearchKind::Song => "song",
            SearchKind::Album => "album",
        };
        let base_url = self
            .settings
            .source(&self.search_source)
            .map(|source| source.base_url.as_str())
            .unwrap_or("");
        crate::api::cache::search_key(
            &self.search_source,
            base_url,
            kind,
            keyword,
            page,
        )
    }

    /// Show a cached search page. Returns false when the key is not cached.
    fn show_cached_search(&mut self, key: &str) -> bool {
        let Some(entry) = self.search_cache.get(key) else {
            return false;
        };
        self.search_results = entry.items.clone();
        self.search_total = entry.total;
        self.search_error = None;
        self.search_loading = false;
        self.search_cached = true;
        self.ctx.request_repaint();
        true
    }

    /// Called by the search box whenever the user edits the query. Suggestions
    /// are debounced so a fast typist does not queue one request per keystroke.
    pub fn note_search_input_changed(&mut self) {
        self.search_suggestions.clear();
        self.search_suggest_index = None;
        self.suggest_request_id = 0;
        let query = self.search_query.trim().to_string();
        if query.is_empty() {
            self.suggest_query.clear();
            self.suggest_due = None;
            return;
        }
        self.suggest_query = query;
        let due = Instant::now() + Duration::from_millis(320);
        self.suggest_due = Some(due);
        self.ctx.request_repaint_after(due - Instant::now());
    }

    pub fn clear_search_suggestions(&mut self) {
        self.search_suggestions.clear();
        self.search_suggest_index = None;
        self.suggest_query.clear();
        self.suggest_due = None;
        self.suggest_request_id = 0;
    }

    fn tick_search_suggestions(&mut self) {
        let Some(due) = self.suggest_due else {
            return;
        };
        let now = Instant::now();
        if now < due {
            self.ctx.request_repaint_after(due - now);
            return;
        }
        self.suggest_due = None;
        let query = self.suggest_query.trim().to_string();
        if query.is_empty() || query != self.search_query.trim() {
            return;
        }
        if self.settings.network.offline {
            return;
        }
        let Some(source) = self.search_source_config() else {
            return;
        };
        self.suggest_request_id = self.api.suggest(&source, &self.settings.network, &query);
        self.ctx.request_repaint();
    }

    pub fn search_to_page(&mut self, page: u32) {
        let keyword = self.search_query.trim().to_string();
        if keyword.is_empty() {
            return;
        }
        let page = page.max(1);
        if self.settings.network.offline {
            self.search_page = page;
            self.search_loading = false;
            self.search_request_id = 0;
            let key = self.search_cache_key(&keyword, page);
            if self.show_cached_search(&key) {
                self.toast(
                    crate::i18n::tr("离线模式：显示缓存的搜索结果"),
                    ToastKind::Info,
                );
            } else {
                self.search_error =
                    Some(crate::i18n::tr("这个页码没有缓存结果").to_string());
                self.search_cached = false;
            }
            return;
        }
        let Some(source) = self.search_source_config() else {
            return;
        };
        self.search_page = page;
        self.start_search_request(&source, &keyword, page);
    }

    fn start_search_request(
        &mut self,
        source: &crate::config::SourceConfig,
        keyword: &str,
        page: u32,
    ) {
        if self.settings.network.offline {
            self.search_loading = false;
            self.search_request_id = 0;
            let key = self.search_cache_key(keyword, page);
            if !self.show_cached_search(&key) {
                self.search_error =
                    Some(crate::i18n::tr("这个页码没有缓存结果").to_string());
            }
            return;
        }
        self.search_loading = true;
        self.search_error = None;
        self.search_request_key = self.search_cache_key(keyword, page);
        self.search_request_id = self.api.search(
            source,
            &self.settings.network,
            self.search_kind,
            keyword,
            page,
            30,
        );
        self.ctx.request_repaint();
    }

    fn detail_cache_key(&self, id: &str, page: u32, album: bool) -> String {
        let base_url = self
            .settings
            .source(&self.search_source)
            .map(|source| source.base_url.as_str())
            .unwrap_or("");
        if album {
            crate::api::cache::album_key(&self.search_source, base_url, id, page)
        } else {
            crate::api::cache::artist_key(&self.search_source, base_url, id, page)
        }
    }

    fn load_cached_album_view(&mut self, id: &str, fallback_name: &str, page: u32) -> bool {
        let key = self.detail_cache_key(id, page, true);
        let Some(entry) = self.search_cache.get(&key) else {
            return false;
        };
        let songs = entry.items.clone();
        let total = entry.total;
        let name = entry
            .name
            .clone()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| fallback_name.to_string());
        self.album_view = Some(AlbumView {
            id: id.to_string(),
            name,
            songs,
            total,
            page,
            loading: false,
            error: None,
            request_id: 0,
            cached: true,
        });
        self.ctx.request_repaint();
        true
    }

    fn load_cached_artist_view(&mut self, id: &str, fallback_name: &str, page: u32) -> bool {
        let key = self.detail_cache_key(id, page, false);
        let Some(entry) = self.search_cache.get(&key) else {
            return false;
        };
        let songs = entry.items.clone();
        let total = entry.total;
        let name = entry
            .name
            .clone()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| fallback_name.to_string());
        self.artist_view = Some(ArtistView {
            id: id.to_string(),
            name,
            songs,
            total,
            page,
            loading: false,
            error: None,
            request_id: 0,
            cached: true,
        });
        self.ctx.request_repaint();
        true
    }

    pub fn open_album(&mut self, id: String, name: String) {
        // The album page is rendered in place of the artist page; without this
        // the artist view would keep winning the render check.
        self.artist_view = None;
        self.album_view = None;
        self.clear_search_suggestions();
        if self.settings.network.offline {
            if !self.load_cached_album_view(&id, &name, 1) {
                self.toast(
                    crate::i18n::tr("离线模式：该专辑页没有缓存"),
                    ToastKind::Info,
                );
            }
            return;
        }
        let Some(source) = self.search_source_config() else {
            return;
        };
        let mut view = AlbumView {
            id,
            name,
            loading: true,
            page: 1,
            ..Default::default()
        };
        let page_size = self.settings.network.album_page_size.clamp(10, 200);
        view.request_id = self
            .api
            .album(&source, &self.settings.network, &view.id, 1, page_size);
        self.album_view = Some(view);
        self.ctx.request_repaint();
    }

    pub fn album_to_page(&mut self, page: u32) {
        let Some(mut view) = self.album_view.take() else {
            return;
        };
        let page = page.max(1);
        if self.settings.network.offline {
            view.page = page;
            view.loading = false;
            view.error = None;
            view.cached = false;
            let id = view.id.clone();
            let name = view.name.clone();
            self.album_view = Some(view);
            if !self.load_cached_album_view(&id, &name, page)
                && let Some(view) = self.album_view.as_mut()
            {
                view.error = Some(crate::i18n::tr("离线模式：该页码没有缓存").to_string());
            }
            return;
        }
        let Some(source) = self.search_source_config() else {
            self.album_view = Some(view);
            return;
        };
        view.page = page;
        view.loading = true;
        view.error = None;
        view.cached = false;
        let page_size = self.settings.network.album_page_size.clamp(10, 200);
        view.request_id = self
            .api
            .album(&source, &self.settings.network, &view.id, view.page, page_size);
        self.album_view = Some(view);
        self.ctx.request_repaint();
    }

    pub fn close_album(&mut self) {
        self.album_view = None;
        self.ctx.request_repaint();
    }

    pub fn open_artist(&mut self, id: String, name: String) {
        self.album_view = None;
        self.artist_view = None;
        self.clear_search_suggestions();
        if self.settings.network.offline {
            if !self.load_cached_artist_view(&id, &name, 1) {
                self.toast(
                    crate::i18n::tr("离线模式：该歌手页没有缓存"),
                    ToastKind::Info,
                );
            }
            return;
        }
        let Some(source) = self.search_source_config() else {
            return;
        };
        let mut view = ArtistView {
            id,
            name,
            loading: true,
            page: 1,
            ..Default::default()
        };
        let page_size = self.settings.network.album_page_size.clamp(10, 200);
        view.request_id = self
            .api
            .artist(&source, &self.settings.network, &view.id, 1, page_size);
        self.artist_view = Some(view);
        self.ctx.request_repaint();
    }

    pub fn artist_to_page(&mut self, page: u32) {
        let Some(mut view) = self.artist_view.take() else {
            return;
        };
        let page = page.max(1);
        if self.settings.network.offline {
            view.page = page;
            view.loading = false;
            view.error = None;
            view.cached = false;
            let id = view.id.clone();
            let name = view.name.clone();
            self.artist_view = Some(view);
            if !self.load_cached_artist_view(&id, &name, page)
                && let Some(view) = self.artist_view.as_mut()
            {
                view.error = Some(crate::i18n::tr("离线模式：该页码没有缓存").to_string());
            }
            return;
        }
        let Some(source) = self.search_source_config() else {
            self.artist_view = Some(view);
            return;
        };
        view.page = page;
        view.loading = true;
        view.error = None;
        view.cached = false;
        let page_size = self.settings.network.album_page_size.clamp(10, 200);
        view.request_id =
            self.api
                .artist(&source, &self.settings.network, &view.id, view.page, page_size);
        self.artist_view = Some(view);
        self.ctx.request_repaint();
    }

    pub fn close_artist(&mut self) {
        self.artist_view = None;
        self.ctx.request_repaint();
    }

    pub fn play_api_song(&mut self, song: &ApiSong) {
        let Some(source) = self.search_source_config() else {
            self.toast(crate::i18n::tr("音源不可用"), ToastKind::Error);
            return;
        };
        let track = song.to_track(&source);
        let key = track.dedupe_key();
        if let Some(index) = self.queue.position_of_key(&key) {
            self.load_index(index, 0.0, true);
            return;
        }
        let before = self.queue.len();
        if self.queue.add(track) {
            self.session_dirty = true;
            self.load_index(before, 0.0, true);
        }
    }

    pub fn queue_api_song(&mut self, song: &ApiSong) {
        let Some(source) = self.search_source_config() else {
            return;
        };
        if self.queue.add(song.to_track(&source)) {
            self.session_dirty = true;
            self.toast(crate::trf!("已加入队列：{}", song.name), ToastKind::Success);
        } else {
            self.toast(crate::i18n::tr("该歌曲已在队列中"), ToastKind::Info);
        }
    }

    pub fn queue_search_songs(&mut self, songs: &[ApiSong]) {
        let Some(source) = self.search_source_config() else {
            return;
        };
        let mut added = 0usize;
        for song in songs {
            if song.name.trim().is_empty() || !song.has_audio {
                continue;
            }
            if self.queue.add(song.to_track(&source)) {
                added += 1;
            }
        }
        if added > 0 {
            self.session_dirty = true;
            self.toast(
                crate::trf!("已加入队列：{added} 首", added),
                ToastKind::Success,
            );
        } else {
            self.toast(crate::i18n::tr("没有可加入的歌曲"), ToastKind::Info);
        }
    }

    pub fn play_search_songs(&mut self, songs: &[ApiSong]) {
        let Some(source) = self.search_source_config() else {
            return;
        };
        let mut first = None;
        for song in songs {
            if song.name.trim().is_empty() || !song.has_audio {
                continue;
            }
            let track = song.to_track(&source);
            let key = track.dedupe_key();
            if let Some(index) = self.queue.position_of_key(&key) {
                if first.is_none() {
                    first = Some(index);
                }
                continue;
            }
            if first.is_none() {
                first = Some(self.queue.len());
            }
            if self.queue.add(track) {
                self.session_dirty = true;
            }
        }
        if let Some(index) = first {
            self.load_index(index, 0.0, true);
        }
    }

    pub fn test_source(&mut self, id: &str) {
        let Some(source) = self.settings.source(id).cloned() else {
            return;
        };
        self.source_test_message = Some((crate::trf!("正在测试 {}…", source.name), false));
        let _ = self.api.test(&source, &self.settings.network);
        self.ctx.request_repaint();
    }

    fn stream_cache_dir(&self) -> PathBuf {
        self.settings
            .network
            .stream_cache_dir
            .clone()
            .unwrap_or_else(|| self.paths.stream_cache.clone())
    }

    /// A custom cache directory may live outside the app data directory, but
    /// never inside it (that could hold downloads/settings) unless it is the
    /// default cache subtree. External folders must be empty or contain only
    /// our hashed cache files, so a mistaken folder choice cannot delete
    /// unrelated files.
    fn stream_cache_dir_safe(&self, dir: &Path) -> bool {
        if dir.starts_with(&self.paths.cache) {
            return true;
        }
        if dir.starts_with(&self.paths.root) {
            return false;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return false;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                return false;
            }
            let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                return false;
            };
            if stem.len() != 64 || !stem.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return false;
            }
        }
        true
    }

    pub fn choose_stream_cache_dir(&mut self) {
        let start = self.stream_cache_dir();
        self.spawn_file_dialog(
            move || rfd::FileDialog::new().set_directory(start).pick_folder(),
            |app, dir| {
                let Some(dir) = dir else {
                    return;
                };
                if !app.stream_cache_dir_safe(&dir) {
                    app.toast(
                        crate::i18n::tr(
                            "缓存目录必须为空或只包含播放器缓存文件，且不能位于应用数据目录内",
                        ),
                        ToastKind::Error,
                    );
                    return;
                }
                app.settings.network.stream_cache_dir = Some(dir);
                app.settings_dirty = true;
            },
        );
    }

    pub fn open_stream_cache_dir(&self) {
        open_in_explorer(&self.stream_cache_dir());
    }

    pub fn clear_stream_cache(&mut self) {
        let dir = self.stream_cache_dir();
        if !self.stream_cache_dir_safe(&dir) {
            self.toast(
                crate::i18n::tr("缓存目录不安全，已取消清理"),
                ToastKind::Error,
            );
            return;
        }
        let (removed, freed) = clear_cache_files(&dir, false);
        self.cache_files = 0;
        self.cache_bytes = 0;
        self.cache_total_files = 0;
        self.cache_total_bytes = 0;
        self.cache_stats_at = Instant::now();
        self.toast(
            crate::trf!(
                "已清除 {removed} 个缓存文件（{:.1} MB）",
                removed,
                format!("{:.1}", freed as f64 / 1024.0 / 1024.0)
            ),
            ToastKind::Success,
        );
    }

    /// Total cache usage: the whole cache root plus a custom stream-cache
    /// directory when it lives outside the root.
    pub fn cache_usage(&self) -> (usize, u64) {
        let (mut files, mut bytes) = cache_dir_usage(&self.paths.cache);
        let stream = self.stream_cache_dir();
        if !stream.starts_with(&self.paths.cache) {
            let (extra_files, extra_bytes) = cache_dir_usage(&stream);
            files += extra_files;
            bytes += extra_bytes;
        }
        (files, bytes)
    }

    pub fn clear_cover_cache(&mut self) {
        let (removed, freed) = clear_cache_files(&self.paths.covers, true);
        self.thumbnails.clear();
        self.thumb_order.clear();
        self.thumb_pending.clear();
        self.thumb_failed.clear();
        self.toast(
            crate::trf!(
                "已清除 {removed} 个封面缓存文件（{:.1} MB）",
                removed,
                format!("{:.1}", freed as f64 / 1024.0 / 1024.0)
            ),
            ToastKind::Success,
        );
    }

    pub fn clear_lyrics_cache(&mut self) {
        let (removed, freed) = clear_cache_files(&self.paths.lyrics_cache, true);
        self.toast(
            crate::trf!(
                "已清除 {removed} 个歌词缓存文件（{:.1} MB）",
                removed,
                format!("{:.1}", freed as f64 / 1024.0 / 1024.0)
            ),
            ToastKind::Success,
        );
    }

    pub fn clear_replaygain_cache(&mut self) {
        let path = self.paths.cache.join("replaygain.json");
        let mut removed = 0usize;
        let mut freed = 0u64;
        if let Ok(metadata) = std::fs::metadata(&path)
            && std::fs::remove_file(&path).is_ok()
        {
            removed = 1;
            freed = metadata.len();
        }
        self.replaygain_cache.clear();
        self.replaygain_scanning.clear();
        self.replaygain_tagged.clear();
        crate::audio::loudness::clear_cached_gains();
        self.toast(
            crate::trf!(
                "已清除响度扫描缓存（{removed} 个文件，{:.1} MB）",
                removed,
                format!("{:.1}", freed as f64 / 1024.0 / 1024.0)
            ),
            ToastKind::Success,
        );
    }

    pub fn clear_fingerprint_cache(&mut self) {
        self.db.clear_fingerprints();
        self.toast(crate::i18n::tr("已清除音频指纹缓存"), ToastKind::Success);
    }

    /// Clears every derived file the player owns: stream/cover/lyrics caches,
    /// ReplayGain scan results, audio fingerprints and in-memory thumbnails.
    /// Downloaded audio files are user data and are never touched.
    pub fn clear_all_caches(&mut self) {
        let (mut removed, mut freed) = clear_cache_files(&self.paths.cache, true);
        let stream = self.stream_cache_dir();
        if !stream.starts_with(&self.paths.cache) && self.stream_cache_dir_safe(&stream) {
            let (extra_removed, extra_freed) = clear_cache_files(&stream, false);
            removed += extra_removed;
            freed += extra_freed;
        }
        self.replaygain_cache.clear();
        self.replaygain_scanning.clear();
        self.replaygain_tagged.clear();
        self.search_cache.clear();
        self.search_cached = false;
        crate::audio::loudness::clear_cached_gains();
        self.db.clear_fingerprints();
        self.thumbnails.clear();
        self.thumb_order.clear();
        self.thumb_pending.clear();
        self.thumb_failed.clear();
        self.cache_files = 0;
        self.cache_bytes = 0;
        self.cache_total_files = 0;
        self.cache_total_bytes = 0;
        self.cache_stats_at = Instant::now();
        self.toast(
            crate::trf!(
                "已清理所有缓存：{removed} 个文件（{:.1} MB）",
                removed,
                format!("{:.1}", freed as f64 / 1024.0 / 1024.0)
            ),
            ToastKind::Success,
        );
        self.ctx.request_repaint();
    }

    // ------------------------------------------------------------------
    // P8: downloads and stream-cache maintenance
    // ------------------------------------------------------------------

    pub fn download_dir(&self) -> PathBuf {
        self.settings
            .download
            .dir
            .clone()
            .unwrap_or_else(|| self.paths.default_download_dir())
    }

    pub fn choose_download_dir(&mut self) {
        let start = self.download_dir();
        self.spawn_file_dialog(
            move || rfd::FileDialog::new().set_directory(start).pick_folder(),
            |app, dir| {
                if let Some(dir) = dir {
                    app.settings.download.dir = Some(dir);
                    app.settings_dirty = true;
                }
            },
        );
    }

    pub fn open_download_dir(&self) {
        let dir = self.download_dir();
        let _ = std::fs::create_dir_all(&dir);
        open_in_explorer(&dir);
    }

    /// Queue one API song, optionally overriding the source's quality.
    pub fn start_download_song_with_quality(&mut self, song: &ApiSong, quality: Option<Quality>) {
        let Some(source) = self.search_source_config() else {
            self.toast(crate::i18n::tr("没有可用的在线音源"), ToastKind::Error);
            return;
        };
        let track = song.to_track(&source);
        self.start_download_track_with_quality(&track, Some(&source.id), quality);
    }

    /// Queue every song currently listed (search result page or album page).
    pub fn download_search_songs(&mut self, songs: &[ApiSong]) {
        let Some(source) = self.search_source_config() else {
            self.toast(crate::i18n::tr("没有可用的在线音源"), ToastKind::Error);
            return;
        };
        let mut added = 0usize;
        let mut last_error = None;
        for song in songs {
            let track = song.to_track(&source);
            match self.enqueue_download(&track, Some(&source.id), None) {
                Ok(()) => added += 1,
                Err((message, _)) => last_error = Some(message),
            }
        }
        if added > 0 {
            self.toast(
                crate::trf!("已加入 {added} 首下载任务", added),
                ToastKind::Success,
            );
        } else if let Some(message) = last_error {
            self.toast(message, ToastKind::Info);
        }
    }

    pub fn start_download_track(&mut self, track: &Track, source_id: Option<&str>) {
        self.start_download_track_with_quality(track, source_id, None);
    }

    pub fn start_download_track_with_quality(
        &mut self,
        track: &Track,
        source_id: Option<&str>,
        quality: Option<Quality>,
    ) {
        match self.enqueue_download(track, source_id, quality) {
            Ok(()) => {
                self.toast(
                    crate::trf!("已加入下载队列：{}", track.display_title()),
                    ToastKind::Success,
                );
            }
            Err((message, kind)) => self.toast(message, kind),
        }
    }

    fn enqueue_download(
        &mut self,
        track: &Track,
        source_id: Option<&str>,
        quality: Option<Quality>,
    ) -> Result<(), (String, ToastKind)> {
        if !self.settings.features.downloads {
            return Err((
                crate::i18n::tr("下载功能已在功能开关中关闭").to_string(),
                ToastKind::Info,
            ));
        }
        if self.settings.network.offline {
            return Err((
                crate::i18n::tr("离线模式下不能下载").to_string(),
                ToastKind::Error,
            ));
        }
        let key = track.dedupe_key();
        if self
            .download_items
            .iter()
            .any(|item| item.state.is_active() && item.track.dedupe_key() == key)
        {
            return Err((
                crate::i18n::tr("这首歌曲已在下载队列中").to_string(),
                ToastKind::Info,
            ));
        }
        let source = source_id
            .map(str::to_string)
            .or_else(|| match &track.source {
                TrackSource::Bamboo { source_id, .. } => Some(source_id.clone()),
                _ => None,
            })
            .and_then(|id| self.settings.source(&id).cloned());
        if matches!(track.source, TrackSource::Bamboo { .. }) && source.is_none() {
            return Err((
                crate::i18n::tr("找不到该在线曲目的音源配置").to_string(),
                ToastKind::Error,
            ));
        }
        let id = self.download_seq;
        self.download_seq = self.download_seq.wrapping_add(1).max(1);
        self.download_items.push(DownloadItem {
            id,
            track: track.clone(),
            source_id: source.as_ref().map(|source| source.id.clone()),
            quality,
            state: DownloadState::Queued,
            received: 0,
            total: None,
            path: None,
            error: None,
            created: Instant::now(),
        });
        self.downloads.start(
            id,
            track.clone(),
            source,
            quality,
            self.settings.network.clone(),
            self.settings.download.clone(),
            self.download_dir(),
        );
        self.ctx.request_repaint();
        Ok(())
    }

    pub fn cancel_download(&mut self, id: u64) {
        self.downloads.cancel(id);
        if let Some(item) = self.download_items.iter_mut().find(|item| item.id == id)
            && item.state.is_active()
        {
            item.state = DownloadState::Canceled;
        }
    }

    pub fn retry_download(&mut self, id: u64) {
        let Some(item) = self
            .download_items
            .iter()
            .find(|item| item.id == id)
            .cloned()
        else {
            return;
        };
        if item.state.is_active() {
            return;
        }
        self.download_items.retain(|item| item.id != id);
        self.start_download_track_with_quality(
            &item.track,
            item.source_id.as_deref(),
            item.quality,
        );
    }

    pub fn remove_download(&mut self, id: u64) {
        if let Some(item) = self.download_items.iter().find(|item| item.id == id)
            && item.state.is_active()
        {
            self.downloads.cancel(id);
        }
        self.download_items.retain(|item| item.id != id);
    }

    pub fn cancel_all_downloads(&mut self) {
        let ids: Vec<u64> = self
            .download_items
            .iter()
            .filter(|item| item.state.is_active())
            .map(|item| item.id)
            .collect();
        for id in ids {
            self.cancel_download(id);
        }
    }

    pub fn clear_finished_downloads(&mut self) {
        self.download_items.retain(|item| item.state.is_active());
    }

    /// Remove leftover `*.part`/`*.part.meta` files from failed or canceled
    /// downloads. Active jobs are left alone.
    pub fn clear_download_partials(&mut self) {
        if self.unfinished_downloads() > 0 {
            self.toast(
                crate::i18n::tr("请先停止正在进行的下载，再清理未完成分片"),
                ToastKind::Info,
            );
            return;
        }
        let dir = self.download_dir();
        let (removed, freed) = clear_download_partials_in(&dir);
        self.toast(
            crate::trf!(
                "已清理 {removed} 个未完成下载分片（{:.1} MB）",
                removed,
                format!("{:.1}", freed as f64 / 1024.0 / 1024.0)
            ),
            ToastKind::Success,
        );
        self.ctx.request_repaint();
    }

    pub fn unfinished_downloads(&self) -> usize {
        self.download_items
            .iter()
            .filter(|item| item.state.is_active())
            .count()
    }

    fn poll_downloads(&mut self) {
        let events = self.downloads.poll();
        if events.is_empty() {
            return;
        }
        let mut completed: Vec<PathBuf> = Vec::new();
        let mut messages: Vec<(String, ToastKind)> = Vec::new();
        for event in events {
            match event {
                DownloadEvent::Progress {
                    id,
                    received,
                    total,
                } => {
                    if let Some(item) = self.download_items.iter_mut().find(|item| item.id == id) {
                        item.received = received;
                        if total.is_some() {
                            item.total = total;
                        }
                        if item.state == DownloadState::Queued {
                            item.state = DownloadState::Downloading;
                        }
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
                    let title = self
                        .download_items
                        .iter()
                        .find(|item| item.id == id)
                        .map(|item| item.track.display_title())
                        .unwrap_or_default();
                    if let Some(item) = self.download_items.iter_mut().find(|item| item.id == id) {
                        item.state = state;
                        item.received = received;
                        item.total = total.or(item.total);
                        item.path = path.clone();
                        item.error = error.clone();
                    }
                    match state {
                        DownloadState::Completed => {
                            messages.push((
                                crate::trf!("下载完成：{title}", title),
                                ToastKind::Success,
                            ));
                            if let Some(path) = path {
                                completed.push(path);
                            }
                        }
                        DownloadState::Failed => {
                            messages.push((
                                crate::trf!(
                                    "下载失败：{title}{}",
                                    title,
                                    error.map(|e| format!("（{e}）")).unwrap_or_default()
                                ),
                                ToastKind::Error,
                            ));
                        }
                        DownloadState::Skipped => {
                            messages.push((
                                crate::trf!("文件已存在，已跳过：{title}", title),
                                ToastKind::Info,
                            ));
                        }
                        _ => {}
                    }
                }
            }
        }
        for path in completed {
            if self.settings.download.add_to_library
                && let Some(track) = DbTrack::from_track(&Track::local(path))
            {
                self.db.upsert(vec![track]);
            }
        }
        for (message, kind) in messages {
            self.toast(message, kind);
        }
        if self.unfinished_downloads() > 0 {
            self.ctx.request_repaint_after(Duration::from_millis(250));
        }
    }

    /// Scan the stream cache (flat hashed files) for file count and size.
    fn scan_stream_cache(&self) -> Vec<(PathBuf, u64, std::time::SystemTime)> {
        let dir = self.stream_cache_dir();
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            let modified = metadata
                .modified()
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            out.push((path, metadata.len(), modified));
        }
        out
    }

    pub fn refresh_cache_stats(&mut self, force: bool) {
        if !force && self.cache_stats_at.elapsed() < Duration::from_secs(2) {
            return;
        }
        let files = self.scan_stream_cache();
        self.cache_files = files.len();
        self.cache_bytes = files.iter().map(|(_, size, _)| *size).sum();
        let (total_files, total_bytes) = self.cache_usage();
        self.cache_total_files = total_files;
        self.cache_total_bytes = total_bytes;
        self.cache_stats_at = Instant::now();
    }

    /// Enforce the configured stream-cache limit by deleting the oldest files.
    /// Stale `.part` files (interrupted streams) are removed as well.
    pub fn trim_stream_cache(&mut self, announce: bool) {
        if !self.stream_cache_dir_safe(&self.stream_cache_dir()) {
            if announce {
                self.toast(
                    crate::i18n::tr("缓存目录不安全，已取消清理"),
                    ToastKind::Error,
                );
            }
            return;
        }
        let limit = self
            .settings
            .network
            .cache_limit_mb
            .saturating_mul(1024 * 1024);
        if limit == 0 {
            return;
        }
        let mut files = self.scan_stream_cache();
        let mut removed = 0usize;
        let mut freed = 0u64;
        let now = std::time::SystemTime::now();
        files.retain(|(path, size, modified)| {
            let stale_part = path.extension().is_some_and(|ext| ext == "part")
                && now.duration_since(*modified).unwrap_or_default() > Duration::from_secs(3600);
            if stale_part {
                if std::fs::remove_file(path).is_ok() {
                    removed += 1;
                    freed += size;
                }
                false
            } else {
                true
            }
        });
        let mut total: u64 = files.iter().map(|(_, size, _)| *size).sum();
        if total > limit {
            files.sort_by_key(|(_, _, modified)| *modified);
            for (path, size, _) in &files {
                if total <= limit {
                    break;
                }
                if std::fs::remove_file(path).is_ok() {
                    removed += 1;
                    freed += size;
                    total = total.saturating_sub(*size);
                }
            }
        }
        if removed > 0 {
            tracing::info!(
                "stream cache trimmed: {removed} files, {:.1} MB freed",
                freed as f64 / 1024.0 / 1024.0
            );
        }
        self.refresh_cache_stats(true);
        if announce {
            self.toast(
                if removed == 0 {
                    crate::trf!(
                        "缓存已在上限内（{:.1} MB）",
                        format!("{:.1}", total as f64 / 1024.0 / 1024.0)
                    )
                } else {
                    crate::trf!(
                        "已清理 {removed} 个缓存文件（{:.1} MB）",
                        removed,
                        format!("{:.1}", freed as f64 / 1024.0 / 1024.0)
                    )
                },
                ToastKind::Success,
            );
        }
    }

    /// True when this online song already has a completed stream-cache file.
    pub fn song_cached(&self, song: &ApiSong) -> bool {
        let Some(source) = self.settings.source(&self.search_source).or_else(|| {
            self.settings
                .enabled_sources()
                .find(|source| !source.base_url.trim().is_empty())
        }) else {
            return false;
        };
        self.online_cache_path_for(&source.id, &song.id, source.quality)
            .is_some_and(|path| path.is_file())
    }

    pub fn request_thumbnail(&mut self, url: &str) {
        if url.trim().is_empty()
            || self.thumbnails.contains_key(url)
            || self.thumb_pending.contains(url)
        {
            return;
        }
        if let Some(retry) = self.thumb_failed.get(url) {
            if retry.attempts >= THUMB_MAX_ATTEMPTS {
                return;
            }
            let now = Instant::now();
            if now < retry.next_at {
                // Wake up again in case the scheduled repaint fired a hair
                // before the deadline and no other event follows.
                self.ctx.request_repaint_after(retry.next_at - now);
                return;
            }
        }
        let Some(source) = self.search_source_config() else {
            return;
        };
        let http = HttpOptions::from_settings(&source, &self.settings.network);
        if self
            .thumb_service
            .request(url.to_string(), url.to_string(), http)
        {
            self.thumb_pending.insert(url.to_string());
        } else {
            // Queue full after fast scrolling: retry when the worker catches
            // up instead of dropping the visible thumbnail forever.
            self.ctx.request_repaint_after(Duration::from_millis(250));
        }
    }

    fn poll_thumbnails(&mut self) {
        let mut changed = false;
        while let Some(result) = self.thumb_service.poll() {
            self.thumb_pending.remove(&result.key);
            match result.image {
                Some(image) => {
                    self.thumb_failed.remove(&result.key);
                    let texture = self.ctx.load_texture(
                        format!("bamboo-thumb-{}", result.key),
                        image,
                        egui::TextureOptions::LINEAR,
                    );
                    self.thumb_order.retain(|key| key != &result.key);
                    self.thumb_order.push_back(result.key.clone());
                    self.thumbnails.insert(result.key, texture);
                    self.trim_thumbnail_cache();
                    changed = true;
                }
                None => {
                    let attempts = self
                        .thumb_failed
                        .get(&result.key)
                        .map(|retry| retry.attempts)
                        .unwrap_or(0)
                        + 1;
                    // 2 s, then 10 s; a third failure is treated as final so a
                    // dead cover host cannot keep waking the UI thread.
                    let delay = match attempts {
                        1 => Duration::from_secs(2),
                        2 => Duration::from_secs(10),
                        _ => Duration::from_secs(60 * 60),
                    };
                    self.thumb_failed.insert(
                        result.key,
                        ThumbRetry {
                            attempts,
                            next_at: Instant::now() + delay,
                        },
                    );
                    if attempts < THUMB_MAX_ATTEMPTS {
                        self.ctx.request_repaint_after(delay);
                    }
                }
            }
        }
        if changed {
            self.ctx.request_repaint();
        }
        // Failed-cover bookkeeping is small, but a long session of scrolling
        // through online search results can still accumulate thousands of
        // entries. Expired final failures are safe to forget: a later request
        // will simply retry.
        if self.thumb_failed.len() > 512 {
            let now = Instant::now();
            self.thumb_failed.retain(|_, retry| retry.next_at > now);
        }
    }

    /// Keep at most `thumbnail_cache` decoded online covers alive. Dropping a
    /// `TextureHandle` releases the GPU texture at the end of the frame, which
    /// is the main memory win after browsing a long search list.
    pub(crate) fn trim_thumbnail_cache(&mut self) {
        let configured = self.settings.performance.thumbnail_cache as usize;
        if configured == 0 {
            return;
        }
        // Below a screenful the cache would thrash on every scroll.
        let cap = configured.max(32);
        while self.thumb_order.len() > cap {
            let Some(oldest) = self.thumb_order.pop_front() else {
                break;
            };
            self.thumbnails.remove(&oldest);
        }
    }

    fn handle_api_event(&mut self, event: ApiEvent) {
        match event {
            ApiEvent::Search { id, kind, page } => {
                if id != self.search_request_id || kind != self.search_kind {
                    return;
                }
                self.search_loading = false;
                let key = std::mem::take(&mut self.search_request_key);
                match page {
                    Ok(page) => {
                        self.search_total = page.total;
                        self.search_results = page.items;
                        self.search_error = None;
                        self.search_cached = false;
                        if !key.is_empty() {
                            self.search_cache.insert(
                                key,
                                self.search_total,
                                None,
                                self.search_results.clone(),
                            );
                            if let Err(err) = self
                                .search_cache
                                .save(&self.paths.cache.join("search.json"))
                            {
                                tracing::debug!("search cache save failed: {err}");
                            }
                        }
                    }
                    Err(err) => {
                        // A network-less start or a flaky source should still
                        // show the last successful page for this query.
                        let cached = self.show_cached_search(&key);
                        if cached {
                            self.toast(
                                crate::trf!("在线搜索失败，已显示缓存结果: {err}", err),
                                ToastKind::Info,
                            );
                        } else {
                            self.search_error = Some(err.clone());
                            self.search_results.clear();
                            self.search_cached = false;
                            self.toast(
                                crate::trf!("在线搜索失败: {err}", err),
                                ToastKind::Error,
                            );
                        }
                    }
                }
                self.ctx.request_repaint();
            }
            ApiEvent::Album { id, album_id, page } => {
                let Some(mut view) = self.album_view.take() else {
                    return;
                };
                if view.request_id != id || view.id != album_id {
                    self.album_view = Some(view);
                    return;
                }
                view.loading = false;
                match page {
                    Ok(page) => {
                        view.total = page.total;
                        view.songs = page.items;
                        view.error = None;
                        view.cached = false;
                        if view.name.is_empty() {
                            if let Some(name) = page.name {
                                view.name = name;
                            }
                        }
                        let key = self.detail_cache_key(&view.id, view.page, true);
                        let cache_name =
                            (!view.name.trim().is_empty()).then(|| view.name.clone());
                        self.search_cache.insert(
                            key,
                            view.total,
                            cache_name,
                            view.songs.clone(),
                        );
                        if let Err(err) = self
                            .search_cache
                            .save(&self.paths.cache.join("search.json"))
                        {
                            tracing::debug!("detail cache save failed: {err}");
                        }
                        self.album_view = Some(view);
                    }
                    Err(err) => {
                        let id = view.id.clone();
                        let name = view.name.clone();
                        let page_number = view.page;
                        if self.load_cached_album_view(&id, &name, page_number) {
                            self.toast(
                                crate::trf!("在线加载失败，已显示缓存结果: {err}", err),
                                ToastKind::Info,
                            );
                        } else {
                            view.error = Some(err);
                            view.cached = false;
                            self.album_view = Some(view);
                        }
                    }
                }
                self.ctx.request_repaint();
            }
            ApiEvent::Artist {
                id,
                artist_id,
                page,
            } => {
                let Some(mut view) = self.artist_view.take() else {
                    return;
                };
                if view.request_id != id || view.id != artist_id {
                    self.artist_view = Some(view);
                    return;
                }
                view.loading = false;
                match page {
                    Ok(page) => {
                        view.total = page.total;
                        view.songs = page.items;
                        view.error = None;
                        view.cached = false;
                        if view.name.is_empty() {
                            if let Some(name) = page.name {
                                view.name = name;
                            }
                        }
                        let key = self.detail_cache_key(&view.id, view.page, false);
                        let cache_name =
                            (!view.name.trim().is_empty()).then(|| view.name.clone());
                        self.search_cache.insert(
                            key,
                            view.total,
                            cache_name,
                            view.songs.clone(),
                        );
                        if let Err(err) = self
                            .search_cache
                            .save(&self.paths.cache.join("search.json"))
                        {
                            tracing::debug!("detail cache save failed: {err}");
                        }
                        self.artist_view = Some(view);
                    }
                    Err(err) => {
                        let id = view.id.clone();
                        let name = view.name.clone();
                        let page_number = view.page;
                        if self.load_cached_artist_view(&id, &name, page_number) {
                            self.toast(
                                crate::trf!("在线加载失败，已显示缓存结果: {err}", err),
                                ToastKind::Info,
                            );
                        } else {
                            view.error = Some(err);
                            view.cached = false;
                            self.artist_view = Some(view);
                        }
                    }
                }
                self.ctx.request_repaint();
            }
            ApiEvent::Suggest {
                id,
                query,
                suggestions,
            } => {
                if id != self.suggest_request_id || query != self.search_query.trim() {
                    return;
                }
                let mut seen = std::collections::HashSet::new();
                self.search_suggestions = suggestions
                    .unwrap_or_default()
                    .into_iter()
                    .map(|text| text.trim().to_string())
                    .filter(|text| !text.is_empty() && seen.insert(text.clone()))
                    .take(8)
                    .collect();
                self.ctx.request_repaint();
            }
            ApiEvent::Resolve {
                id,
                track_key,
                cache,
                start_secs,
                autoplay,
                result,
            } => {
                if id != self.online_request_id {
                    return;
                }
                let is_current = self
                    .current_track
                    .as_ref()
                    .map(|track| track.dedupe_key() == track_key)
                    .unwrap_or(false);
                if !is_current {
                    self.online_loading = false;
                    self.online_deadline = None;
                    return;
                }
                self.online_loading = false;
                self.online_deadline = None;
                match result {
                    Ok(prepared) => {
                        self.finish_online_resolve(prepared, cache, start_secs, autoplay)
                    }
                    Err(err) => {
                        self.playback_state = PlaybackState::Stopped;
                        self.lyrics_loading = false;
                        self.lyrics_error = Some(err.clone());
                        self.toast(crate::trf!("在线播放失败: {err}", err), ToastKind::Error);
                        self.ctx.request_repaint();
                    }
                }
            }
            ApiEvent::NextResolved {
                id,
                track_key,
                cache,
                result,
            } => {
                self.handle_next_resolved(id, track_key, cache, result);
            }
            ApiEvent::Random { id, page } => {
                if id == self.discover_request {
                    self.handle_discover_random(id, page);
                } else {
                    self.handle_station_random(id, page);
                }
            }
            ApiEvent::Info { id, result } => {
                self.handle_cached_online_info(id, result);
            }
            ApiEvent::Test { source_id, result } => {
                let name = self
                    .settings
                    .source(&source_id)
                    .map(|source| source.name.clone())
                    .unwrap_or(source_id);
                match result {
                    Ok(message) => {
                        self.source_test_message = Some((format!("{name}: {message}"), true));
                        self.toast(message, ToastKind::Success);
                    }
                    Err(err) => {
                        self.source_test_message = Some((format!("{name}: {err}"), false));
                        self.toast(crate::trf!("连接失败: {err}", err), ToastKind::Error);
                    }
                }
                self.ctx.request_repaint();
            }
        }
    }

    fn handle_next_resolved(
        &mut self,
        id: u64,
        track_key: String,
        cache: Option<PathBuf>,
        result: crate::api::ApiResult<PreparedPlay>,
    ) {
        let Some((pending, index, signature)) = self.next_prefetch.clone() else {
            return;
        };
        if pending != id {
            return;
        }
        self.next_prefetch = None;
        if !matches!(
            self.playback_state,
            PlaybackState::Playing | PlaybackState::Loading
        ) || self.sleep_end_of_track
            || self.queue.peek_next(true) != Some(index)
        {
            return;
        }
        let Some(track) = self.queue.items.get(index).cloned() else {
            return;
        };
        if track.dedupe_key() != track_key {
            return;
        }
        let Ok(prepared) = result else {
            self.next_prefetch_failures = self.next_prefetch_failures.saturating_add(1);
            let delay = match self.next_prefetch_failures {
                1 => Duration::from_secs(10),
                2 => Duration::from_secs(30),
                _ => Duration::from_secs(60),
            };
            self.next_prefetch_backoff = Some((signature, Instant::now() + delay));
            tracing::debug!("next-track pre-resolve failed; retrying in {:?}", delay);
            return;
        };
        self.next_prefetch_failures = 0;
        self.next_prefetch_backoff = None;
        let Some(source_config) = self.online_source_for(&track) else {
            return;
        };
        // Fill in metadata the search response may not have carried; the
        // queue entry is what `adopt_auto_advanced` displays.
        if let Some(info) = prepared.info.as_ref()
            && let Some(slot) = self.queue.items.get_mut(index)
        {
            if !info.name.trim().is_empty() {
                slot.title = info.name.clone();
            }
            if !info.artist.trim().is_empty() {
                slot.artist = info.artist.clone();
            }
            if !info.album.trim().is_empty() {
                slot.album = info.album.clone();
            }
            if info.pic.is_some() {
                slot.cover_url = info.pic.clone();
            }
        }
        let http = HttpOptions::from_settings(&source_config, &self.settings.network).for_stream();
        let download_first =
            cache.is_some() && source_config.kind == crate::config::SourceKind::Local;
        let source = if let Some(cached) = cache.as_ref().filter(|path| path.is_file()) {
            EngineSource::Local(cached.clone())
        } else {
            EngineSource::Url {
                url: prepared.url,
                cache,
                http,
                download_first,
            }
        };
        let token = self.next_arm_token;
        self.next_arm_token = self.next_arm_token.wrapping_add(1);
        self.engine.set_next(Some(source), token);
        self.armed_next = Some((token, index, signature));
        self.pending_auto = Some((token, index));
        tracing::info!("pre-armed online next track (queue index {index})");
        self.ctx.request_repaint();
    }

    /// Refresh a Bamboo track's metadata/lyrics at most once per TTL. Used for
    /// completed stream caches and tracks adopted from a pre-armed deck, where
    /// the playback URL is already known and no `prepare` call is needed.
    fn maybe_request_online_info(&mut self, track: &Track, force: bool) {
        if self.settings.network.offline {
            return;
        }
        let TrackSource::Bamboo { song_id, .. } = &track.source else {
            return;
        };
        let key = track.dedupe_key();
        if !force
            && self
                .info_refreshed
                .get(&key)
                .is_some_and(|at| at.elapsed() < ONLINE_INFO_TTL)
        {
            return;
        }
        let Some(source) = self.online_source_for(track) else {
            return;
        };
        self.info_refreshed.insert(key.clone(), Instant::now());
        if self.info_refreshed.len() > 512 {
            self.info_refreshed
                .retain(|_, at| at.elapsed() < ONLINE_INFO_TTL);
        }
        self.info_force = force;
        self.info_request_id = self.api.info(&source, &self.settings.network, song_id);
        self.info_track_key = key;
    }

    /// Apply a background `info` refresh for a cache-hit online track. The
    /// audio already plays from the local cache; only metadata/lyrics change.
    fn handle_cached_online_info(
        &mut self,
        id: u64,
        result: crate::api::ApiResult<crate::api::SongInfo>,
    ) {
        if id != self.info_request_id {
            return;
        }
        self.info_request_id = 0;
        let force = std::mem::take(&mut self.info_force);
        let expected_key = std::mem::take(&mut self.info_track_key);
        let current_key = self.current_track.as_ref().map(Track::dedupe_key);
        if current_key.as_deref() != Some(expected_key.as_str()) {
            return;
        }
        let Ok(info) = result else {
            tracing::debug!("cached online info refresh failed");
            return;
        };
        let Some(mut track) = self.current_track.clone() else {
            return;
        };
        if !info.song.name.trim().is_empty() {
            track.title = info.song.name.clone();
        }
        if !info.song.artist.trim().is_empty() {
            track.artist = info.song.artist.clone();
        }
        if !info.song.album.trim().is_empty() {
            track.album = info.song.album.clone();
        }
        if info.song.pic.is_some() {
            track.cover_url = info.song.pic.clone();
        }
        if info.song.album_id.is_some() {
            track.album_id = info.song.album_id.clone();
        }
        if info.song.artist_id.is_some() {
            track.artist_id = info.song.artist_id.clone();
        }
        if let Some(addition) = info.song.addition.clone()
            && track.warning.is_none()
        {
            track.warning = Some(addition);
        }
        self.current_track = Some(track.clone());
        if let Some(index) = self.queue.current
            && let Some(slot) = self.queue.items.get_mut(index)
        {
            *slot = track.clone();
        }
        self.cover_key.clear();
        self.refresh_visuals();
        self.update_title();
        self.session_dirty = true;

        // Keep existing (sidecar/edited) lyrics authoritative during automatic
        // refreshes; a manual refresh replaces them with the source LRC.
        if let Some(lrc) = info.lrc.clone().filter(|text| !text.trim().is_empty())
            && (force || self.lyrics.is_none())
        {
            let lyrics = crate::lyrics::parse_text(
                &lrc,
                crate::lyrics::LyricsSource::Bamboo,
                None,
                Some(crate::i18n::tr("在线").to_string()),
            );
            if !lyrics.is_empty() {
                self.lyrics = Some(lyrics);
                self.lyrics_version = self.lyrics_version.wrapping_add(1);
                self.lyrics_key = track.dedupe_key();
                self.lyrics_error = None;
                self.lyrics_loading = false;
                self.lyrics_manual_scroll_until = None;
                // Persist the online LRC beside the cache so an offline start
                // has lyrics without contacting the source again.
                if let Some(source) = self.online_source_for(&track)
                    && let Some(cache) = self.online_cache_path(&track, source.quality)
                {
                    let sidecar = cache.with_extension("lrc");
                    if force || !sidecar.exists() {
                        let _ = std::fs::write(&sidecar, &lrc);
                    }
                }
            }
        }
        self.ctx.request_repaint();
    }

    fn finish_online_resolve(
        &mut self,
        prepared: PreparedPlay,
        cache: Option<PathBuf>,
        start_secs: f64,
        autoplay: bool,
    ) {
        let Some(mut track) = self.current_track.clone() else {
            return;
        };
        if let Some(info) = prepared.info.as_ref() {
            if !info.name.trim().is_empty() {
                track.title = info.name.clone();
            }
            if !info.artist.trim().is_empty() {
                track.artist = info.artist.clone();
            }
            if !info.album.trim().is_empty() {
                track.album = info.album.clone();
            }
            if info.pic.is_some() {
                track.cover_url = info.pic.clone();
            }
            if info.album_id.is_some() {
                track.album_id = info.album_id.clone();
            }
            if info.artist_id.is_some() {
                track.artist_id = info.artist_id.clone();
            }
            if let Some(addition) = info.addition.clone() {
                if track.warning.is_none() {
                    track.warning = Some(addition);
                }
            }
        }
        if prepared.info.is_some() {
            self.info_refreshed
                .insert(track.dedupe_key(), Instant::now());
        }
        self.current_track = Some(track.clone());
        if let Some(index) = self.queue.current {
            if let Some(slot) = self.queue.items.get_mut(index) {
                *slot = track.clone();
            }
        }
        // The info endpoint may have supplied a cover that the search response
        // did not include; clear the key so the remote cover is fetched.
        self.cover_key.clear();
        self.refresh_visuals();
        self.update_title();

        if let Some(lrc) = prepared.lrc.filter(|text| !text.trim().is_empty()) {
            let lyrics = crate::lyrics::parse_text(
                &lrc,
                crate::lyrics::LyricsSource::Bamboo,
                None,
                Some(crate::i18n::tr("在线").to_string()),
            );
            if !lyrics.lines.is_empty() {
                self.lyrics = Some(lyrics);
                self.lyrics_version = self.lyrics_version.wrapping_add(1);
                self.lyrics_error = None;
            } else {
                self.lyrics_error = Some(crate::i18n::tr("在线歌词为空").to_string());
            }
        } else {
            self.lyrics = None;
            self.lyrics_error = Some(crate::i18n::tr("未找到在线歌词").to_string());
        }
        self.lyrics_loading = false;
        self.lyrics_key = track.dedupe_key();
        self.lyrics_manual_scroll_until = None;
        // The resolve payload can be missing lyrics when several skips race
        // through the source. Ask the info endpoint once more; the cached-info
        // handler replaces them when the response arrives for this track.
        if self.lyrics.is_none() && !self.settings.network.offline {
            self.maybe_request_online_info(&track, true);
        }

        if let Some(cached) = cache.as_ref().filter(|path| path.is_file()) {
            self.engine
                .load(EngineSource::Local(cached.clone()), start_secs, autoplay);
        } else {
            let source = match &track.source {
                TrackSource::Bamboo { source_id, .. } => self.settings.source(source_id).cloned(),
                _ => None,
            };
            let http = source
                .as_ref()
                .map(|source| HttpOptions::from_settings(source, &self.settings.network))
                .unwrap_or_else(|| HttpOptions::for_network(&self.settings.network))
                .for_stream();
            let download_first = cache.is_some()
                && source
                    .as_ref()
                    .is_some_and(|source| source.kind == crate::config::SourceKind::Local);
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
        }
        self.session_dirty = true;
        self.ctx.request_repaint();
    }

    // ------------------------------------------------------------------
    // Persistence
    // ------------------------------------------------------------------

    /// Fast quit: hide the window first, persist state, then terminate without
    /// waiting for wgpu/DWM/audio teardown (which could take seconds on some
    /// drivers). Windows releases the audio device and GPU handles on exit.
    fn quit_immediately(&mut self) {
        if let Some(hwnd) = self.hwnd {
            window::hide_window(hwnd);
        }
        self.save_now();
        tracing::info!("fast shutdown");
        // Give the non-blocking log writer a moment to flush.
        std::thread::sleep(Duration::from_millis(25));
        std::process::exit(0);
    }

    pub fn save_now(&mut self) {
        self.save_replaygain_cache();
        let settings = self.settings.clone();
        if let Err(err) = save_settings(&self.paths.settings_file, &settings) {
            tracing::warn!("save settings failed: {err}");
        } else {
            self.settings_dirty = false;
            self.last_settings_save = Instant::now();
        }

        let mut session = self.build_session();
        if let Err(err) = save_session(&self.paths.session_file, &session) {
            tracing::warn!("save session failed: {err}");
        } else {
            self.session_dirty = false;
            self.last_session_save = Instant::now();
            self.session = std::mem::take(&mut session);
        }
    }

    fn build_session(&self) -> Session {
        let (window_pos, window_size) = match self.last_window_rect {
            Some((x, y, w, h)) => (Some((x, y)), Some((w, h))),
            None => (self.session.window_pos, self.session.window_size),
        };
        let position = if let Some(start) = self.deferred_start {
            start
        } else if self.current_track.is_some() {
            self.engine.position_secs()
        } else {
            0.0
        };
        Session {
            queue: self.queue.items.clone(),
            current_index: self.queue.current,
            position_secs: position,
            shuffle: self.queue.shuffle,
            repeat: self.queue.repeat,
            volume: self.settings.playback.volume,
            page: self.page,
            playlist_scroll: self.session.playlist_scroll,
            window_pos,
            window_size,
            window_maximized: self.window_maximized,
            desktop_lyrics_pos: self.session.desktop_lyrics_pos,
            desktop_lyrics_monitor: self.session.desktop_lyrics_monitor,
            current_key: self.current_track.as_ref().map(|t| t.dedupe_key()),
            fullscreen_lyrics: self.fullscreen_lyrics,
            lyrics_window: self.lyrics_window_open,
            lyrics_window_windowed: self.lyrics_window_windowed,
            lyrics_window_size: self.session.lyrics_window_size,
            lyrics_window_pos: self.session.lyrics_window_pos,
            shuffle_keys: if self.queue.shuffle {
                self.queue
                    .shuffle_order
                    .iter()
                    .filter_map(|&index| self.queue.items.get(index))
                    .map(|track| track.dedupe_key())
                    .collect()
            } else {
                Vec::new()
            },
            playlist_path: self.current_playlist.clone(),
            playlists: self.session.playlists.clone(),
            station: self.station_active,
            station_history: self.station_history.clone(),
            recent_files: self.session.recent_files.clone(),
            saved_at: Some(chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()),
        }
    }

    fn maybe_autosave(&mut self, ctx: &Context) {
        self.update_window_rect(ctx);
        if self.settings_dirty && self.last_settings_save.elapsed() > Duration::from_millis(900) {
            let settings = self.settings.clone();
            if let Err(err) = save_settings(&self.paths.settings_file, &settings) {
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
            self.save_now();
        }
    }

    fn update_window_rect(&mut self, ctx: &Context) {
        if self.lyrics_os_fullscreen {
            return;
        }
        let info = ctx.input(|i| i.viewport().clone());
        if let Some(max) = info.maximized {
            self.window_maximized = max;
        }
        if self.window_maximized {
            // Keep the last restored size; a maximized inner rect must not be
            // persisted as the windowed size.
            return;
        }
        let Some(outer) = info.outer_rect else {
            return;
        };
        let size = info
            .inner_rect
            .map(|r| r.size())
            .unwrap_or_else(|| outer.size());
        if size.x > 400.0 && size.y > 300.0 {
            self.last_window_rect = Some((outer.min.x, outer.min.y, size.x, size.y));
        }
    }

    pub fn apply_appearance(&mut self) {
        theme::apply(&self.ctx, &self.settings);
        let font_config = (
            self.settings.theme.font_family.clone(),
            self.settings.theme.font_size.round() as u8,
            self.settings.desktop_lyrics.font_family.clone(),
            self.settings.large_lyrics.font_family.clone(),
        );
        if font_config != self.last_font_config {
            fonts::install(&self.ctx, &self.settings);
            self.last_font_config = font_config;
        }
        self.reapply_window_style();
        self.cover_key.clear();
        self.refresh_visuals();
        self.settings_dirty = true;
        self.ctx.request_repaint();
    }

    /// Reload font definitions after the desktop lyrics font changed. Font
    /// definitions are global to the egui context, so the change also reaches
    /// the deferred desktop window.
    pub fn reload_fonts(&mut self) {
        fonts::install(&self.ctx, &self.settings);
        self.last_font_config = (
            self.settings.theme.font_family.clone(),
            self.settings.theme.font_size.round() as u8,
            self.settings.desktop_lyrics.font_family.clone(),
            self.settings.large_lyrics.font_family.clone(),
        );
        self.ctx.request_repaint();
    }

    pub fn reset_settings(&mut self) {
        let sources = self.settings.sources.clone();
        self.settings = Settings::default();
        // Keep user-configured endpoints: resetting should not erase them.
        if !sources.is_empty() {
            self.settings.sources = sources;
        }
        self.library_extensions_text = self.settings.library.extensions.join(", ");
        self.sync_library_watch();
        self.api
            .set_min_interval_ms(self.settings.network.api_min_interval_ms);
        self.apply_appearance();
        self.apply_dsp();
        self.clear_ab_loop_silent();
        self.clear_sleep_timer();
        self.arm_next();
        self.toast(crate::i18n::tr("设置已恢复默认"), ToastKind::Success);
    }

    pub fn autostart_enabled(&self) -> bool {
        crate::platform::registry::autostart_enabled()
    }

    pub fn associations_enabled(&self) -> bool {
        crate::platform::registry::associations_enabled()
    }

    pub fn set_autostart(&mut self, enabled: bool) {
        match crate::platform::registry::set_autostart(enabled) {
            Ok(()) => {
                self.settings.features.start_with_windows = enabled;
                self.settings_dirty = true;
                self.toast(
                    if enabled {
                        crate::i18n::tr("已启用开机启动")
                    } else {
                        crate::i18n::tr("已关闭开机启动")
                    },
                    ToastKind::Success,
                );
            }
            Err(err) => self.toast(
                crate::trf!("开机启动设置失败: {err}", err),
                ToastKind::Error,
            ),
        }
    }

    pub fn set_file_associations(&mut self, enabled: bool) {
        match crate::platform::registry::set_associations(enabled) {
            Ok(()) => {
                self.settings.features.file_associations = enabled;
                self.settings_dirty = true;
                self.toast(
                    if enabled {
                        crate::i18n::tr("已注册音频文件关联（mp3/flac/wav/ogg/m4a/aac/opus 等）")
                    } else {
                        crate::i18n::tr("已移除音频文件关联")
                    },
                    ToastKind::Success,
                );
            }
            Err(err) => self.toast(
                crate::trf!("文件关联设置失败: {err}", err),
                ToastKind::Error,
            ),
        }
    }

    pub fn playlist_association_enabled(&self) -> bool {
        crate::platform::registry::playlist_association_enabled()
    }

    pub fn set_playlist_association(&mut self, enabled: bool) {
        match crate::platform::registry::set_playlist_association(enabled) {
            Ok(()) => {
                self.settings.features.playlist_associations = enabled;
                self.settings_dirty = true;
                self.toast(
                    if enabled {
                        crate::i18n::tr("已注册 .mp3list 歌单关联")
                    } else {
                        crate::i18n::tr("已移除 .mp3list 歌单关联")
                    },
                    ToastKind::Success,
                );
            }
            Err(err) => self.toast(
                crate::trf!("歌单关联设置失败: {err}", err),
                ToastKind::Error,
            ),
        }
    }

    pub fn open_data_dir(&self) {
        open_in_explorer(&self.paths.root);
    }

    pub fn open_log_dir(&self) {
        open_in_explorer(&self.paths.logs);
    }

    pub fn update_title(&mut self) {
        // `logic` also runs for the desktop lyrics viewport; never send the
        // main window title command to that window.
        if self.ctx.viewport_id() != egui::ViewportId::ROOT {
            return;
        }
        let title = match self.current_track.as_ref() {
            Some(track) => format!(
                "{} - {} — Bamboo Music Player",
                track.display_artist(),
                track.display_title()
            ),
            None => "Bamboo Music Player".to_string(),
        };
        if title != self.last_title {
            self.ctx
                .send_viewport_cmd(ViewportCommand::Title(title.clone()));
            self.last_title = title;
        }
    }

    // ------------------------------------------------------------------
    // UI shell
    // ------------------------------------------------------------------

    fn player_bar(&mut self, ui: &mut egui::Ui) {
        let colors = self.colors();
        let bar = ui.available_rect_before_wrap();
        let top_h = 68.0;
        let gap_v = 4.0;
        let bottom_h = 24.0;
        let content_h = top_h + gap_v + bottom_h;
        let y0 = bar.top() + ((bar.height() - content_h) * 0.5).max(0.0);
        let top_rect =
            egui::Rect::from_min_size(egui::pos2(bar.left(), y0), egui::vec2(bar.width(), top_h));
        let bottom_rect = egui::Rect::from_min_size(
            egui::pos2(bar.left(), y0 + top_h + gap_v),
            egui::vec2(bar.width(), bottom_h),
        );

        // Strict three-column geometry: equal side columns keep the transport
        // group centered in the window regardless of the text lengths.
        let total_w = bar.width();
        let hgap = 10.0;
        let side_w = ((total_w - 240.0 - hgap * 2.0) * 0.5).clamp(150.0, 300.0);
        let side_w = side_w.min(((total_w - hgap * 2.0 - 120.0) * 0.5).max(120.0));
        let left_rect = egui::Rect::from_min_size(top_rect.min, egui::vec2(side_w, top_h));
        let right_rect = egui::Rect::from_min_size(
            egui::pos2(top_rect.right() - side_w, top_rect.top()),
            egui::vec2(side_w, top_h),
        );
        let center_rect = egui::Rect::from_min_max(
            egui::pos2(left_rect.right() + hgap, top_rect.top()),
            egui::pos2(right_rect.left() - hgap, top_rect.bottom()),
        );

        // Left: cover + single-line truncated title/artist.
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(left_rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
            |ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                let cover_side = 54.0;
                let (cover_rect, _) = ui
                    .allocate_exact_size(egui::vec2(cover_side, cover_side), egui::Sense::hover());
                if let Some(cover) = &self.cover {
                    ui.painter().image(
                        cover.id(),
                        cover_rect,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        Color32::WHITE,
                    );
                } else {
                    ui.painter()
                        .rect_filled(cover_rect, egui::CornerRadius::same(8), colors.card);
                    ui.painter().text(
                        cover_rect.center(),
                        Align2::CENTER_CENTER,
                        egui_phosphor::regular::MUSIC_NOTES,
                        egui::FontId::proportional(22.0),
                        colors.text_faint,
                    );
                }
                let text_w = (left_rect.width() - cover_side - 10.0).max(40.0);
                ui.allocate_ui_with_layout(
                    egui::vec2(text_w, 48.0),
                    egui::Layout::top_down(egui::Align::LEFT),
                    |ui| {
                        ui.add_space(5.0);
                        let title = self
                            .current_track
                            .as_ref()
                            .map(|t| t.display_title())
                            .unwrap_or_else(|| self.t("player.no_track").to_string());
                        let artist = self
                            .current_track
                            .as_ref()
                            .map(|t| t.display_artist())
                            .unwrap_or_default();
                        ui.allocate_ui_with_layout(
                            egui::vec2(text_w, 19.0),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(title).size(13.5).strong().color(colors.text),
                                    )
                                    .truncate()
                                    .halign(egui::Align::LEFT),
                                );
                            },
                        );
                        ui.allocate_ui_with_layout(
                            egui::vec2(text_w, 16.0),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(artist).size(11.0).color(colors.text_weak),
                                    )
                                    .truncate()
                                    .halign(egui::Align::LEFT),
                                );
                            },
                        );
                    },
                );
            },
        );
        let left_response = ui.interact(
            left_rect,
            ui.id().with("bamboo-player-left"),
            egui::Sense::click(),
        );
        let bar_label = match self.current_track.as_ref() {
            Some(track) => {
                let artist = track.display_artist();
                if artist.is_empty() {
                    track.display_title().to_string()
                } else {
                    format!("{} - {artist}", track.display_title())
                }
            }
            None => self.t("player.no_track").to_string(),
        };
        left_response.widget_info(|| {
            egui::WidgetInfo::selected(
                egui::WidgetType::SelectableLabel,
                true,
                self.page == Page::NowPlaying,
                &bar_label,
            )
        });
        if left_response.has_focus() {
            ui.painter().rect_stroke(
                left_rect.shrink(2.0),
                egui::CornerRadius::same(6),
                egui::Stroke::new(1.5, colors.accent),
                egui::StrokeKind::Inside,
            );
        }
        if left_response.clicked() {
            self.set_page(Page::NowPlaying);
        }
        if let Some(track) = self.current_track.clone() {
            left_response.context_menu(|ui| {
                widgets::track_clipboard_menu(ui, &track);
            });
        }

        // Middle: transport controls. The group width is fixed by the button
        // sizes, and the group rect is centered on the bar's center point.
        const CONTROLS_W: f32 = 34.0 + 36.0 + 46.0 + 36.0 + 34.0 + 4.0 * 6.0;
        let controls_rect = egui::Rect::from_center_size(
            egui::pos2(bar.center().x, center_rect.center().y),
            egui::vec2(CONTROLS_W, 46.0),
        );
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(controls_rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
            |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                if widgets::icon_button(
                    ui,
                    egui_phosphor::regular::SHUFFLE,
                    18.0,
                    self.t("player.shuffle"),
                    self.queue.shuffle,
                    true,
                    &colors,
                )
                .clicked()
                {
                    self.toggle_shuffle();
                }
                if widgets::icon_button(
                    ui,
                    egui_phosphor::regular::SKIP_BACK,
                    20.0,
                    self.t("player.prev"),
                    false,
                    !self.queue.is_empty(),
                    &colors,
                )
                .clicked()
                {
                    self.prev_track();
                }
                if play_button(
                    ui,
                    self.is_playing(),
                    self.current_track.is_some() || !self.queue.is_empty(),
                    &colors,
                )
                .clicked()
                {
                    self.toggle_play();
                }
                if widgets::icon_button(
                    ui,
                    egui_phosphor::regular::SKIP_FORWARD,
                    20.0,
                    self.t("player.next"),
                    false,
                    !self.queue.is_empty(),
                    &colors,
                )
                .clicked()
                {
                    self.next_track();
                }
                let repeat_icon = match self.queue.repeat {
                    crate::model::RepeatMode::One => egui_phosphor::regular::REPEAT_ONCE,
                    _ => egui_phosphor::regular::REPEAT,
                };
                if widgets::icon_button(
                    ui,
                    repeat_icon,
                    18.0,
                    self.queue.repeat.label(),
                    self.queue.repeat != crate::model::RepeatMode::Off,
                    true,
                    &colors,
                )
                .clicked()
                {
                    self.cycle_repeat();
                }
            },
        );

        // Right: volume and utilities, right-aligned with a computed slider width.
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(right_rect)
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
            |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                if widgets::icon_button(
                    ui,
                    egui_phosphor::regular::QUEUE,
                    18.0,
                    self.t("player.queue"),
                    self.page == Page::Playlists,
                    true,
                    &colors,
                )
                .clicked()
                {
                    self.set_page(Page::Playlists);
                }
                if widgets::icon_button(
                    ui,
                    crate::i18n::tr("词"),
                    18.0,
                    self.t("player.lyrics"),
                    self.show_lyrics_sidebar,
                    true,
                    &colors,
                )
                .clicked()
                {
                    self.toggle_lyrics_sidebar();
                }
                if right_rect.width() >= 280.0
                    && widgets::icon_button(
                        ui,
                        egui_phosphor::regular::MONITOR_PLAY,
                        18.0,
                        self.t("player.desktop_lyrics"),
                        self.settings.desktop_lyrics.visible_at_start,
                        true,
                        &colors,
                    )
                    .clicked()
                {
                    self.toggle_desktop_lyrics();
                }
                if self.settings.features.mini_player
                    && widgets::icon_button(
                        ui,
                        egui_phosphor::regular::PICTURE_IN_PICTURE,
                        18.0,
                        crate::i18n::tr("迷你播放器"),
                        self.settings.mini.visible,
                        true,
                        &colors,
                    )
                    .clicked()
                {
                    self.toggle_mini_player();
                }
                let enhance_active = self.settings.playback.speed != 1.0
                    || self.ab_loop_active()
                    || self.sleep_deadline.is_some()
                    || self.sleep_end_of_track;
                let enhance = widgets::icon_button(
                    ui,
                    egui_phosphor::regular::SLIDERS,
                    18.0,
                    crate::i18n::tr("播放增强：变速 / A-B 循环 / 睡眠定时"),
                    enhance_active,
                    true,
                    &colors,
                );
                egui::Popup::menu(&enhance).show(|ui| self.enhance_menu(ui));
                let speaker = if self.settings.playback.muted || self.volume() <= 0.001 {
                    egui_phosphor::regular::SPEAKER_SLASH
                } else {
                    egui_phosphor::regular::SPEAKER_HIGH
                };
                if widgets::icon_button(
                    ui,
                    speaker,
                    18.0,
                    self.t("player.mute"),
                    false,
                    true,
                    &colors,
                )
                .clicked()
                {
                    self.toggle_mute();
                }
                let available = ui.available_width().max(20.0);
                let volume_w = (available - 4.0).clamp(30.0, 100.0).min(available);
                if let Some(v) = widgets::volume_bar(ui, self.volume(), &colors, volume_w) {
                    self.set_volume(v);
                }
            },
        );

        // Bottom: full-width timeline with fixed-width time labels.
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(bottom_rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
            |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let time_w = 42.0;
                let seek_w = (bottom_rect.width() - time_w * 2.0 - 16.0).max(40.0);
                let duration = self.duration_secs() as f32;
                let position = self.position_secs() as f32;

                let (left_time, _) =
                    ui.allocate_exact_size(egui::vec2(time_w, bottom_h), egui::Sense::hover());
                ui.painter().text(
                    left_time.left_center(),
                    Align2::LEFT_CENTER,
                    format_time(self.position_secs()),
                    egui::FontId::proportional(10.5),
                    colors.text_weak,
                );
                if let Some(result) = widgets::seek_bar(
                    ui,
                    seek_w,
                    position,
                    duration,
                    &colors,
                    self.current_track.is_some(),
                ) {
                    if let Some(v) = result.value {
                        self.seek_preview = Some(v as f64);
                    }
                    if result.commit {
                        let target = result
                            .value
                            .map(|v| v as f64)
                            .or(self.seek_preview)
                            .unwrap_or(position as f64);
                        self.seek_to(target);
                    }
                }
                let (right_time, _) =
                    ui.allocate_exact_size(egui::vec2(time_w, bottom_h), egui::Sense::hover());
                ui.painter().text(
                    right_time.right_center(),
                    Align2::RIGHT_CENTER,
                    format_time(duration as f64),
                    egui::FontId::proportional(10.5),
                    colors.text_weak,
                );
            },
        );
    }

    fn enhance_menu(&mut self, ui: &mut egui::Ui) {
        let colors = self.colors();
        ui.set_min_width(250.0);
        ui.label(
            RichText::new(crate::i18n::tr("播放增强"))
                .size(13.0)
                .strong()
                .color(colors.text),
        );
        ui.add_space(4.0);

        if self.settings.features.speed_control {
            ui.label(
                RichText::new(crate::i18n::tr("播放速度"))
                    .size(11.0)
                    .color(colors.text_weak),
            );
            let mut speed = self.settings.playback.speed;
            if ui
                .add(
                    egui::Slider::new(&mut speed, 0.5..=2.0)
                        .suffix("x")
                        .fixed_decimals(2),
                )
                .changed()
            {
                self.settings.playback.speed = speed;
                self.settings_dirty = true;
                self.apply_dsp();
            }
            ui.horizontal(|ui| {
                for preset in [0.75, 1.0, 1.25, 1.5, 2.0] {
                    let selected = (self.settings.playback.speed - preset).abs() < 0.005;
                    if ui
                        .selectable_label(selected, format!("{preset:.2}x"))
                        .clicked()
                    {
                        self.settings.playback.speed = preset;
                        self.settings_dirty = true;
                        self.apply_dsp();
                    }
                }
            });
            ui.separator();
        }

        if self.settings.features.ab_repeat {
            ui.label(
                RichText::new(crate::i18n::tr("A-B 循环"))
                    .size(11.0)
                    .color(colors.text_weak),
            );
            ui.horizontal(|ui| {
                if ui.button(crate::i18n::tr("设 A 点")).clicked() {
                    self.set_ab_start();
                }
                if ui.button(crate::i18n::tr("设 B 点")).clicked() {
                    self.set_ab_end();
                }
                let can_clear = self.ab_start.is_some() || self.ab_end.is_some();
                if ui
                    .add_enabled(can_clear, egui::Button::new(crate::i18n::tr("清除")))
                    .clicked()
                {
                    self.clear_ab_loop();
                }
            });
            let status = match (self.ab_start, self.ab_end) {
                (Some(a), Some(b)) => format!(
                    "{} – {}{}",
                    format_time(a),
                    format_time(b),
                    if self.ab_loop_active() {
                        crate::i18n::tr(" · 循环中")
                    } else {
                        ""
                    }
                ),
                (Some(a), None) => crate::trf!("A {} · 等待设置 B 点", format_time(a)),
                _ => crate::i18n::tr("尚未设置").to_string(),
            };
            ui.label(RichText::new(status).size(10.0).color(colors.text_faint));
            ui.separator();
        }

        if self.settings.features.sleep_timer {
            ui.label(
                RichText::new(crate::i18n::tr("睡眠定时"))
                    .size(11.0)
                    .color(colors.text_weak),
            );
            ui.horizontal_wrapped(|ui| {
                for minutes in [15u64, 30, 45, 60, 90] {
                    if ui.button(crate::trf!("{minutes} 分钟", minutes)).clicked() {
                        self.set_sleep_minutes(minutes);
                    }
                }
            });
            ui.horizontal(|ui| {
                if ui.button(crate::i18n::tr("当前曲目结束后")).clicked() {
                    self.set_sleep_after_track();
                }
                let active = self.sleep_deadline.is_some() || self.sleep_end_of_track;
                if ui
                    .add_enabled(active, egui::Button::new(crate::i18n::tr("取消定时")))
                    .clicked()
                {
                    self.clear_sleep_timer();
                }
            });
            let status = if let Some(deadline) = self.sleep_deadline {
                let secs = deadline.saturating_duration_since(Instant::now()).as_secs();
                crate::trf!(
                    "{:.0} 分 {:.0} 秒后暂停",
                    format!("{:.0}", secs / 60),
                    format!("{:.0}", secs % 60)
                )
            } else if self.sleep_end_of_track {
                crate::i18n::tr("当前曲目结束后暂停").to_string()
            } else {
                crate::i18n::tr("未启用").to_string()
            };
            ui.label(RichText::new(status).size(10.0).color(colors.text_faint));
            ui.separator();
        }

        let dsp = dsp_config(&self.settings);
        if ui
            .add_enabled(
                self.settings.features.equalizer,
                egui::Checkbox::new(
                    &mut self.settings.playback.eq_enabled,
                    crate::i18n::tr("均衡器（10 段）"),
                ),
            )
            .changed()
        {
            self.settings_dirty = true;
            self.apply_dsp();
        }
        if ui.button(crate::i18n::tr("打开播放设置…")).clicked() {
            self.settings_tab = views::SettingsTab::Playback;
            self.set_page(Page::Settings);
            ui.close();
        }
        ui.label(
            RichText::new(crate::trf!(
                "无缝：{} · 交叉淡化：{:.1}s · 增益：{}",
                if dsp.gapless {
                    crate::i18n::tr("开")
                } else {
                    crate::i18n::tr("关")
                },
                format!("{:.1}", dsp.crossfade_secs),
                match dsp.replaygain {
                    crate::config::ReplayGainMode::Off => crate::i18n::tr("关"),
                    crate::config::ReplayGainMode::Track => crate::i18n::tr("按曲目"),
                    crate::config::ReplayGainMode::Album => crate::i18n::tr("按专辑"),
                }
            ))
            .size(9.5)
            .color(colors.text_faint),
        );
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        #[derive(Clone)]
        enum PlaylistAction {
            Open(PathBuf),
            Append(PathBuf),
            Rescan(PathBuf),
            Reveal(PathBuf),
            Rename(PathBuf),
            Remove(PathBuf),
        }

        let colors = self.colors();
        self.refresh_playlist_status();
        // The brand header stays fixed; only the navigation/playlist buttons
        // scroll when the window is short.
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            let (rect, _) = ui.allocate_exact_size(vec2(36.0, 36.0), egui::Sense::hover());
            ui.painter()
                .rect_filled(rect, egui::CornerRadius::same(9), colors.accent);
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                egui_phosphor::regular::MUSIC_NOTES,
                egui::FontId::proportional(21.0),
                Color32::from_rgb(10, 14, 22),
            );
            ui.add_space(8.0);
            ui.vertical(|ui| {
                ui.label(
                    RichText::new("Bamboo")
                        .size(18.0)
                        .strong()
                        .color(colors.text),
                );
                ui.label(
                    RichText::new("MUSIC PLAYER")
                        .size(8.5)
                        .color(colors.text_faint),
                );
            });
        });
        ui.add_space(16.0);

        // Navigation scrolls when the window is too short. The Settings /
        // version block below is pinned so it never overlaps the playlist
        // section at the minimum window height. The extra bottom space keeps
        // the version label from sitting on the panel edge.
        let bottom_height = 84.0;
        let top_height = (ui.available_height() - bottom_height).max(120.0);
        // The pinned Settings button lives outside the scroll area, so its
        // available width includes the scrollbar gutter that the nav items
        // inside the scroll area do not have. Reuse the measured content width
        // to keep every sidebar row exactly the same width.
        let mut nav_width = 0.0;
        let mut playlist_action = None;
        let mut import_clicked = false;
        egui::ScrollArea::vertical()
            .id_salt("bamboo-sidebar-scroll")
            .max_height(top_height)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let nav = [
                    (
                        Page::NowPlaying,
                        egui_phosphor::regular::PLAY_CIRCLE,
                        "nav.now_playing",
                    ),
                    (
                        Page::Library,
                        egui_phosphor::regular::MUSIC_NOTES,
                        "nav.library",
                    ),
                    (
                        Page::Playlists,
                        egui_phosphor::regular::QUEUE,
                        "nav.playlists",
                    ),
                    (
                        Page::Search,
                        egui_phosphor::regular::MAGNIFYING_GLASS,
                        "nav.search",
                    ),
                    (
                        Page::Discover,
                        egui_phosphor::regular::COMPASS,
                        "nav.discover",
                    ),
                ];
                nav_width = ui.available_width();
                for (page, icon, key) in nav {
                    if widgets::nav_button(
                        ui,
                        icon,
                        self.t(key),
                        self.page == page,
                        &colors,
                        nav_width,
                    )
                    .clicked()
                    {
                        self.set_page(page);
                    }
                }

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(self.t("nav.playlists_section"))
                            .size(10.5)
                            .color(colors.text_faint),
                    );
                });
                ui.add_space(4.0);
                let saved = self.session.playlists.clone();
                let missing = self.playlist_missing.clone();
                if saved.is_empty() {
                    ui.horizontal(|ui| {
                        ui.add_space(12.0);
                        ui.label(
                            RichText::new(crate::i18n::tr("导入 .mp3list / .m3u 后显示在这里"))
                                .size(11.0)
                                .color(colors.text_faint),
                        );
                    });
                } else {
                    for entry in saved {
                        let key = normalize_path(&entry.path);
                        let selected = self
                            .current_playlist
                            .as_ref()
                            .is_some_and(|path| normalize_path(path) == key);
                        let absent = missing.contains(&key);
                        let response = widgets::playlist_button(
                            ui,
                            &entry.display_name(),
                            selected,
                            absent,
                            &colors,
                            nav_width,
                        );
                        let response = if absent {
                            response.on_hover_text(crate::trf!(
                                "歌单文件不存在：{}",
                                entry.path.display()
                            ))
                        } else {
                            response.on_hover_text(entry.path.display().to_string())
                        };
                        if response.clicked() {
                            playlist_action = Some(PlaylistAction::Open(entry.path.clone()));
                        }
                        response.context_menu(|ui| {
                            if ui.button(crate::i18n::tr("打开（替换队列）")).clicked() {
                                playlist_action = Some(PlaylistAction::Open(entry.path.clone()));
                                ui.close();
                            }
                            if ui.button(crate::i18n::tr("加入当前队列")).clicked() {
                                playlist_action = Some(PlaylistAction::Append(entry.path.clone()));
                                ui.close();
                            }
                            if ui.button(crate::i18n::tr("重新扫描")).clicked() {
                                playlist_action = Some(PlaylistAction::Rescan(entry.path.clone()));
                                ui.close();
                            }
                            if ui.button(crate::i18n::tr("在资源管理器中显示")).clicked() {
                                playlist_action = Some(PlaylistAction::Reveal(entry.path.clone()));
                                ui.close();
                            }
                            if ui.button(crate::i18n::tr("重命名…")).clicked() {
                                playlist_action = Some(PlaylistAction::Rename(entry.path.clone()));
                                ui.close();
                            }
                            ui.separator();
                            if ui.button(crate::i18n::tr("从“我的歌单”移除")).clicked() {
                                playlist_action = Some(PlaylistAction::Remove(entry.path.clone()));
                                ui.close();
                            }
                        });
                    }
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new(crate::i18n::tr("导入歌单…"))
                                    .size(11.5)
                                    .color(colors.text_weak),
                            )
                            .frame(false),
                        )
                        .clicked()
                    {
                        import_clicked = true;
                    }
                });
            });

        if import_clicked {
            self.import_playlist_dialog();
        }
        match playlist_action {
            Some(PlaylistAction::Open(path)) | Some(PlaylistAction::Rescan(path)) => {
                if path.is_file() {
                    self.import_playlist(path, true);
                } else {
                    self.playlist_check_at = None;
                    self.toast(crate::i18n::tr("歌单文件不存在"), ToastKind::Error);
                }
            }
            Some(PlaylistAction::Append(path)) => {
                if path.is_file() {
                    self.import_playlist(path, false);
                } else {
                    self.playlist_check_at = None;
                    self.toast(crate::i18n::tr("歌单文件不存在"), ToastKind::Error);
                }
            }
            Some(PlaylistAction::Reveal(path)) => {
                crate::util::reveal_in_explorer(&path);
            }
            Some(PlaylistAction::Rename(path)) => self.begin_rename_saved_playlist(path),
            Some(PlaylistAction::Remove(path)) => self.forget_saved_playlist(&path),
            None => {}
        }

        ui.add_space(2.0);
        let settings_width = if nav_width > 0.0 {
            nav_width
        } else {
            ui.available_width()
        };
        if widgets::nav_button(
            ui,
            egui_phosphor::regular::GEAR,
            self.t("nav.settings"),
            self.page == Page::Settings,
            &colors,
            settings_width,
        )
        .clicked()
        {
            self.set_page(Page::Settings);
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(
                RichText::new(format!("v{}", env!("CARGO_PKG_VERSION")))
                    .size(10.5)
                    .color(colors.text_faint),
            );
        });
    }

    fn central(&mut self, ui: &mut egui::Ui) {
        let colors = self.colors();
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                paint_background(self, ui, &colors);
                let immersive = self.fullscreen_lyrics && !self.lyrics_editor_open;
                let margin = if immersive {
                    egui::Margin::same(0)
                } else {
                    egui::Margin::symmetric(26, 20)
                };
                egui::Frame::NONE.inner_margin(margin).show(ui, |ui| {
                    if self.lyrics_editor_open {
                        views::lyrics_editor(self, ui);
                    } else if self.fullscreen_lyrics {
                        views::fullscreen_lyrics(self, ui);
                    } else if self.page == Page::NowPlaying {
                        // The now-playing page fits its hero + lyrics panels to
                        // the window; the lyrics area owns the scrolling.
                        views::now_playing(self, ui);
                    } else {
                        ui.scope(|ui| {
                            // Let the page scrollbar float over the content
                            // edge instead of reserving 10 px on the right;
                            // reserving it made the right margin wider than the
                            // left one on every scrollable page.
                            ui.style_mut().spacing.scroll.floating_allocated_width = 0.0;
                            let mut scroll = egui::ScrollArea::vertical()
                                .id_salt(("bamboo-page-scroll", self.page))
                                .auto_shrink([false, false]);
                            if self.scroll_to_top {
                                scroll = scroll.vertical_scroll_offset(0.0);
                                self.scroll_to_top = false;
                            }
                            scroll.show(ui, |ui| match self.page {
                                Page::NowPlaying => views::now_playing(self, ui),
                                Page::Library => views::library(self, ui),
                                Page::Playlists => views::playlists(self, ui),
                                Page::Search => views::search(self, ui),
                                Page::Discover => views::discover(self, ui),
                                Page::Settings => views::settings(self, ui),
                            });
                        });
                    }
                });
            });
    }

    fn toasts(&mut self, ctx: &Context) {
        self.toasts
            .retain(|t| t.created.elapsed() < Duration::from_secs(4));
        if self.toasts.is_empty() {
            return;
        }
        let colors = self.colors();
        egui::Area::new(Id::new("bamboo-toasts"))
            .order(Order::Foreground)
            .anchor(Align2::RIGHT_BOTTOM, vec2(-20.0, -108.0))
            .show(ctx, |ui| {
                ui.set_max_width(390.0);
                ui.vertical(|ui| {
                    for toast in self.toasts.iter().rev() {
                        let (fg, bg) = match toast.kind {
                            ToastKind::Info => (colors.text, colors.panel),
                            ToastKind::Success => (colors.success, colors.panel),
                            ToastKind::Error => (colors.danger, colors.panel),
                        };
                        // Ask the painter for the unwrapped text width so short
                        // toasts stay on one line; long messages wrap at 360 px
                        // instead of being squeezed into a narrow column.
                        let natural = ui
                            .painter()
                            .layout_no_wrap(
                                toast.text.clone(),
                                egui::FontId::proportional(12.5),
                                fg,
                            )
                            .size()
                            .x;
                        let text_width = (natural + 2.0).clamp(180.0, 360.0);
                        egui::Frame::new()
                            .fill(bg)
                            .stroke(egui::Stroke::new(1.0, colors.line))
                            .corner_radius(egui::CornerRadius::same(8))
                            .inner_margin(egui::Margin::symmetric(14, 9))
                            .show(ui, |ui| {
                                ui.set_width(text_width);
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(&toast.text).size(12.5).color(fg),
                                    )
                                    .wrap(),
                                );
                            });
                    }
                });
            });
    }
}

impl eframe::App for App {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        // The desktop lyrics viewport is a transparent window: keep the clear
        // color fully transparent so only the painted panel is visible.
        [0.0, 0.0, 0.0, 0.0]
    }

    /// eframe calls this hook for every viewport before its UI runs, while
    /// `App::logic` only runs for the root viewport. Deferred overlays
    /// (standalone lyrics, mini player) publish snapshots and queue their
    /// controls for `logic`; if the main window is occluded, minimized or
    /// hidden, the root may not repaint for a long time, which made the
    /// overlay buttons look dead until the main window was focused. Handling
    /// their traffic here ties it to the overlay's own frame instead.
    fn raw_input_hook(&mut self, ctx: &Context, _raw_input: &mut eframe::egui::RawInput) {
        self.update_lyrics_window_snapshot();
        // The native mini window is fed by `sync_native_mini`; the egui
        // snapshot (which clones the cover texture handle and lyrics) is only
        // needed for the fallback viewport.
        if !self.mini_native_active {
            self.update_mini_snapshot();
        }
        self.handle_lyrics_window_actions();
        self.handle_mini_actions(ctx);
    }

    fn logic(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        // eframe calls `logic` only for the root viewport. The overlay windows
        // are immediate viewports rendered from here; the heartbeat thread in
        // `App::new` keeps the root pass running so they stay animated even
        // when the main window is hidden or unfocused.
        if ctx.viewport_id() != egui::ViewportId::ROOT {
            return;
        }

        // File dialog results posted by the worker thread.
        self.poll_background_tasks();

        // The overlay windows are immediate viewports: their frame rate is the
        // root pass rate, so keep the heartbeat in sync with the configured
        // animation frame rate.
        self.heartbeat_frame_ms
            .store(self.settings.performance.frame_millis(), Ordering::Relaxed);

        self.handle_engine_events();
        self.tick_online_timeout();
        self.poll_downloads();
        // Retry a delayed online prefetch near the end of the current track.
        // `arm_next` itself decides whether the remaining time is inside the
        // prefetch window, so this poll is cheap for long local tracks too.
        if self.playback_state == PlaybackState::Playing
            && self.last_next_arm_poll.elapsed() >= Duration::from_secs(2)
        {
            self.last_next_arm_poll = Instant::now();
            self.arm_next();
        }
        if self.startup_minimize_pending {
            self.startup_minimize_pending = false;
            if self.settings.window.start_minimized {
                ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
            }
        }
        if !self.startup_commands.is_empty() {
            let commands = std::mem::take(&mut self.startup_commands);
            tracing::info!("startup commands: {:?}", commands);
            self.apply_commands(commands);
        }
        self.tick_ab_loop();
        self.tick_sleep_timer();
        self.sync_smtc();
        self.poll_cover();
        self.poll_thumbnails();
        self.poll_lyrics();
        self.poll_metadata();
        self.poll_scan();
        self.poll_browser();
        self.poll_db();
        self.poll_library_watch();
        self.poll_loudness();
        if self.last_cache_trim.elapsed() > Duration::from_secs(180) {
            self.last_cache_trim = Instant::now();
            self.trim_stream_cache(false);
        }
        let visualizer_on = self.visualizer_visible() && self.is_playing();
        self.engine.set_visual_enabled(visualizer_on);
        if visualizer_on {
            let interval = Duration::from_millis(self.visualizer_millis());
            if self.last_visualizer_update.elapsed() >= interval {
                self.last_visualizer_update = Instant::now();
                self.update_visualizer();
            }
        }
        while let Some(event) = self.api.poll() {
            self.handle_api_event(event);
        }
        self.tick_search_suggestions();
        // Keep the overlay windows alive even when the main window is hidden
        // in the tray: `ui` is not called for invisible root windows. Their
        // snapshots and command queues are handled in `raw_input_hook`, which
        // eframe also calls for the deferred viewports themselves.
        self.sync_desktop_lyrics(ctx);
        self.sync_mini_player(ctx);
        self.sync_lyrics_window(ctx);
        self.handle_tray(ctx);
        self.handle_close_request(ctx);
        self.handle_shortcuts(ctx);
        self.poll_ipc();
        self.sync_jump_list();
        self.maybe_autosave(ctx);

        let focused = ctx.input(|i| i.viewport().focused).unwrap_or(true);
        if let Some((rect, at)) = self.pending_window_restore
            && Instant::now() >= at
        {
            if let Some(raw) = self.hwnd {
                window::restore_window_rect(raw, rect);
            }
            self.pending_window_restore = None;
        }
        if !self.style_reapplied && Instant::now() >= self.style_reapply_at {
            self.reapply_window_style();
            self.style_reapplied = true;
        }
        if focused != self.was_focused {
            self.was_focused = focused;
            if focused {
                self.reapply_window_style();
            }
        }

        if self.last_memory_sample.elapsed() > Duration::from_secs(2) {
            self.memory_info = crate::platform::memory::process_memory();
            self.last_memory_sample = Instant::now();
        }
        if self.settings.window.trim_working_set_on_idle && !self.is_playing() {
            let viewport = ctx.input(|i| i.viewport().clone());
            let focused = viewport.focused.unwrap_or(true);
            let minimized = viewport.minimized.unwrap_or(false);
            let occluded = viewport.occluded.unwrap_or(false);
            // Hidden/minimized windows release memory quickly; a focused,
            // idle window waits longer so page faults do not make the open
            // page feel sluggish.
            let wait = if self.window_hidden || minimized {
                5
            } else if occluded || !focused {
                20
            } else {
                120
            };
            if self.last_trim.elapsed() > Duration::from_secs(wait) {
                crate::platform::memory::trim_working_set();
                self.memory_info = crate::platform::memory::process_memory();
                self.last_trim = Instant::now();
            }
        } else if self.is_playing() {
            self.last_trim = Instant::now();
        }

        if self.restore_notice_pending {
            self.restore_notice_pending = false;
            if !self.queue.is_empty() {
                self.toast(self.t("session.restored").to_string(), ToastKind::Info);
            }
        }

        let viewport = ctx.input(|i| i.viewport().clone());
        let minimized = viewport.minimized.unwrap_or(false);
        let occluded = viewport.occluded.unwrap_or(false);
        let focused = viewport.focused.unwrap_or(true);
        // Only immediate egui viewports need the heartbeat: they are rendered
        // inline with the root pass. Native overlay windows own their timer
        // and wake the root on demand (actions/engine events).
        let egui_mini_visible = self.settings.features.mini_player
            && self.settings.mini.visible
            && !self.mini_native_active;
        let overlay_needed = self.lyrics_window_open || egui_mini_visible;
        self.heartbeat_active.store(
            overlay_needed && (self.window_hidden || minimized || occluded),
            Ordering::Relaxed,
        );
        if self.cover_loading || self.lyrics_loading {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        let animation_ms = self.animation_millis(focused, occluded, minimized);
        let overlay_ms = self.overlay_millis();
        // The root also drives deferred overlay snapshots, so its cadence
        // must be fast enough for those windows even when the main window is
        // hidden or its own content is static.
        let fastest_ms = match (animation_ms, overlay_ms) {
            (Some(animation), Some(overlay)) => Some(animation.min(overlay)),
            (animation, overlay) => animation.or(overlay),
        };
        match self.playback_state {
            PlaybackState::Playing => {
                // Only pay for animation frames when something actually
                // moves: the flow background, the visualizer, karaoke
                // lyrics, or a visible lyrics/mini overlay. Otherwise the
                // progress bar is fine at the idle cadence.
                let idle_ms = self.settings.performance.idle_millis();
                // Overlay cadences are a lower bound, not a replacement: a
                // native mini player only needs a slow state tick, but a
                // visible main window must still redraw its own progress bar
                // at the idle rate.
                let base_ms = if self.window_hidden || minimized || occluded {
                    1000
                } else if focused {
                    idle_ms
                } else {
                    idle_ms.max(250)
                };
                let ms = fastest_ms.map_or(base_ms, |overlay| overlay.min(base_ms));
                // egui subtracts one predicted frame from request_repaint_after
                // to avoid overshooting the target. Without adding it back, a
                // 30 fps request becomes roughly 60 fps on a 60 Hz display.
                let lead = Duration::from_secs_f32(
                    ctx.input(|input| input.predicted_dt).clamp(0.0, 0.1),
                );
                ctx.request_repaint_after(Duration::from_millis(ms) + lead);
            }
            PlaybackState::Loading => {
                let lead = Duration::from_secs_f32(
                    ctx.input(|input| input.predicted_dt).clamp(0.0, 0.1),
                );
                ctx.request_repaint_after(
                    Duration::from_millis(fastest_ms.unwrap_or(100).min(100)) + lead,
                );
            }
            _ => {
                if overlay_ms.is_some() {
                    // Paused/stopped overlays still need prompt state
                    // updates (pause, seek, track change); no karaoke frames
                    // are needed, so a quarter second is plenty.
                    ctx.request_repaint_after(Duration::from_millis(250));
                } else if !self.toasts.is_empty() {
                    ctx.request_repaint_after(Duration::from_millis(250));
                }
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // The overlays are immediate viewports rendered from `logic`; while the
        // main window sits in the tray there is nothing to draw, and skipping
        // the whole UI is what lets the overlays keep their frame rate in a
        // debug build.
        if self.window_hidden {
            return;
        }
        let ctx = ui.ctx().clone();
        self.handle_drop(&ctx);
        self.update_window_rect(&ctx);
        self.device_info = self.engine.output_info();

        let colors = theme::colors(&ctx, &self.settings);
        let bar_frame = egui::Frame::new()
            .fill(colors.panel)
            .stroke(egui::Stroke::new(1.0, colors.line))
            .inner_margin(egui::Margin::symmetric(16, 0));
        egui::Panel::bottom("bamboo-player-bar")
            .exact_size(104.0)
            .frame(bar_frame)
            .show(ui, |ui| self.player_bar(ui));

        let side_frame = egui::Frame::new()
            .fill(colors.panel)
            .inner_margin(egui::Margin::symmetric(8, 10));
        if !self.fullscreen_lyrics {
            egui::Panel::left("bamboo-sidebar")
                .exact_size(self.settings.theme.sidebar_width)
                .resizable(false)
                .frame(side_frame)
                .show(ui, |ui| self.sidebar(ui));
        }

        if self.show_lyrics_sidebar && !self.fullscreen_lyrics {
            let available = ui.available_width();
            let max_width = 520.0_f32.min((available - 80.0).max(200.0));
            let width = self.lyrics_sidebar_width.clamp(200.0, max_width);
            let mut resize_delta = 0.0;
            let lyrics_frame = egui::Frame::new()
                .fill(colors.panel)
                .stroke(egui::Stroke::new(1.0, colors.line))
                .inner_margin(egui::Margin {
                    left: 18,
                    right: 18,
                    top: 16,
                    bottom: 16,
                });
            // Custom splitter instead of egui's persisted panel resize: the
            // width lives in the app state, so there is no stored drag state
            // that can keep moving the panel on its own.
            egui::Panel::right("bamboo-lyrics-sidebar")
                .exact_size(width)
                .resizable(false)
                .show_separator_line(false)
                .frame(lyrics_frame)
                .show(ui, |ui| {
                    views::lyrics_sidebar(self, ui);
                });

            // Splitter drawn in the remaining content area so it sits exactly
            // on the panel boundary and can show a hover highlight line.
            let area = ui.available_rect_before_wrap();
            let boundary = area.right();
            let handle = egui::Rect::from_min_max(
                egui::pos2(boundary - 6.0, area.top()),
                egui::pos2(boundary, area.bottom()),
            );
            let response = ui.interact(
                handle,
                ui.id().with("bamboo-lyrics-sidebar-resize"),
                egui::Sense::drag(),
            );
            if response.hovered() || response.dragged() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                ui.ctx()
                    .layer_painter(egui::LayerId::new(
                        egui::Order::Foreground,
                        egui::Id::new("bamboo-lyrics-splitter-line"),
                    ))
                    .line_segment(
                        [
                            egui::pos2(boundary - 1.0, area.top()),
                            egui::pos2(boundary - 1.0, area.bottom()),
                        ],
                        egui::Stroke::new(2.0, colors.accent),
                    );
            }
            if response.dragged() {
                resize_delta -= response.drag_delta().x;
            }
            if resize_delta != 0.0 {
                self.lyrics_sidebar_width =
                    (self.lyrics_sidebar_width + resize_delta).clamp(200.0, max_width);
            }
        }
        self.central(ui);
        self.playlist_rename_dialog(&ctx);
        self.toasts(&ctx);
        self.command_palette(&ctx);
        self.update_title();
    }

    fn save(&mut self, _storage: &mut dyn eframe::Storage) {
        self.save_now();
    }

    fn on_exit(&mut self) {
        self.save_now();
        self.engine.shutdown();
    }
}

pub fn app_icon(accent: [u8; 3]) -> egui::IconData {
    if let Some((rgba, width, height)) =
        crate::util::icon::decode_ico(crate::util::icon::APP_ICON_ICO)
    {
        return egui::IconData {
            rgba,
            width,
            height,
        };
    }
    const SIZE: usize = 64;
    let mut rgba = vec![0u8; SIZE * SIZE * 4];
    let cx = (SIZE as f32 - 1.0) / 2.0;
    let cy = (SIZE as f32 - 1.0) / 2.0;
    let radius = 29.0f32;
    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            if (dx * dx + dy * dy).sqrt() <= radius {
                let idx = (y * SIZE + x) * 4;
                rgba[idx] = accent[0];
                rgba[idx + 1] = accent[1];
                rgba[idx + 2] = accent[2];
                rgba[idx + 3] = 255;
            }
        }
    }
    let tri = [(23.0f32, 17.0f32), (23.0, 47.0), (49.0, 32.0)];
    for y in 0..SIZE {
        for x in 0..SIZE {
            if point_in_triangle((x as f32 + 0.5, y as f32 + 0.5), tri[0], tri[1], tri[2]) {
                let idx = (y * SIZE + x) * 4;
                rgba[idx] = 255;
                rgba[idx + 1] = 255;
                rgba[idx + 2] = 255;
                rgba[idx + 3] = 255;
            }
        }
    }
    egui::IconData {
        rgba,
        width: SIZE as u32,
        height: SIZE as u32,
    }
}

fn point_in_triangle(p: (f32, f32), a: (f32, f32), b: (f32, f32), c: (f32, f32)) -> bool {
    fn sign(p1: (f32, f32), p2: (f32, f32), p3: (f32, f32)) -> f32 {
        (p1.0 - p3.0) * (p2.1 - p3.1) - (p2.0 - p3.0) * (p1.1 - p3.1)
    }
    let d1 = sign(p, a, b);
    let d2 = sign(p, b, c);
    let d3 = sign(p, c, a);
    let has_neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let has_pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(has_neg && has_pos)
}

fn play_button(
    ui: &mut egui::Ui,
    playing: bool,
    enabled: bool,
    colors: &ThemeColors,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(46.0, 46.0), egui::Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            enabled,
            if playing {
                crate::i18n::tr("暂停")
            } else {
                crate::i18n::tr("播放")
            },
        )
    });
    if response.has_focus() {
        ui.painter()
            .circle_stroke(rect.center(), 23.5, egui::Stroke::new(1.5, colors.accent));
    }
    let color = if enabled {
        colors.accent
    } else {
        colors.text_faint
    };
    if response.hovered() && enabled {
        ui.painter()
            .circle_filled(rect.center(), 24.0, colors.accent_soft);
    }
    ui.painter().circle_filled(rect.center(), 21.0, color);
    let icon_color = Color32::from_rgb(10, 14, 22);
    let center = rect.center();
    if playing {
        // Two bars, exactly symmetric around the circle center.
        for dx in [-4.0, 4.0] {
            ui.painter().rect_filled(
                egui::Rect::from_center_size(center + vec2(dx, 0.0), vec2(4.0, 14.0)),
                egui::CornerRadius::same(1),
                icon_color,
            );
        }
    } else {
        // A font glyph has side bearings that make the triangle look
        // off-center; draw it explicitly with the centroid at the center.
        let points = vec![
            egui::pos2(center.x - 4.0, center.y - 7.0),
            egui::pos2(center.x - 4.0, center.y + 7.0),
            egui::pos2(center.x + 8.0, center.y),
        ];
        ui.painter().add(egui::Shape::convex_polygon(
            points,
            icon_color,
            egui::Stroke::NONE,
        ));
    }
    response
}

/// Paint the main-window background for the selected mode.
///
/// `StaticBlur` keeps the CPU-blurred texture path (cheapest), `GpuBlur` and
/// `Flow` hand the cover to a real wgpu pipeline through an egui render
/// callback, and `Mica` leaves the DWM system backdrop visible behind a
/// translucent scrim. If Mica is unavailable (older Windows or an opaque
/// swapchain) it falls back to the static cover so the window is never blank.
pub fn paint_background(app: &mut App, ui: &mut egui::Ui, colors: &ThemeColors) {
    use crate::config::BackgroundMode;

    let rect = ui.max_rect();
    let painter = ui.painter().clone();
    let mode = app.settings.theme.background;
    let dark = theme::is_dark(&app.ctx, &app.settings);

    if mode == BackgroundMode::Mica && app.mica_active {
        // The backdrop is drawn by the compositor; only tint it so text stays
        // readable and the wallpaper does not overpower the UI.
        let alpha = if dark { 170 } else { 195 };
        painter.rect_filled(
            rect,
            egui::CornerRadius::ZERO,
            Color32::from_rgba_unmultiplied(colors.bg.r(), colors.bg.g(), colors.bg.b(), alpha),
        );
        accent_glow(&painter, rect, colors.accent, 9);
        return;
    }

    // Opaque base so an unsupported callback can never leave a transparent
    // hole in the window.
    painter.rect_filled(rect, egui::CornerRadius::ZERO, colors.bg);

    let gpu_mode = matches!(mode, BackgroundMode::GpuBlur | BackgroundMode::Flow)
        && app.gpu_bg.available()
        && app.settings.performance.cover_background;
    let cpu_cover = matches!(
        mode,
        BackgroundMode::StaticBlur
            | BackgroundMode::GpuBlur
            | BackgroundMode::Flow
            | BackgroundMode::Mica
    );

    if gpu_mode {
        let viewport = ui.ctx().input(|i| i.viewport().clone());
        let focused = viewport.focused.unwrap_or(true);
        let minimized = viewport.minimized.unwrap_or(false);
        let occluded = viewport.occluded.unwrap_or(false);
        // Pause the motion (rather than the whole background) while the
        // window is in the background. The shader keeps its last time so the
        // frozen frame does not visibly jump to the start.
        let animated = app.background_animation_active(focused, occluded, minimized)
            && mode == BackgroundMode::Flow;
        // A paused flow background does not need a full-screen shader every
        // root frame. While the window is hidden/unfocused (the common case
        // with the mini player open), draw the already CPU-blurred cover
        // instead, which removes the shader's fill-rate cost entirely.
        let use_gpu = if mode == BackgroundMode::Flow {
            animated || app.background.is_none()
        } else {
            focused && !minimized && !occluded && !app.window_hidden
                || app.background.is_none()
        };
        if use_gpu {
        let time = ui.input(|i| i.time) as f32;
        let gpu_mode_id = u32::from(mode == BackgroundMode::Flow);
        app.gpu_bg.paint(
            &painter,
            rect,
            app.settings.theme.accent,
            gpu_mode_id,
            app.settings.theme.blur_radius,
            app.settings.theme.background_opacity,
            dark,
            time,
            animated,
        );
        } else if cpu_cover && let Some(texture) = app.background.as_ref() {
            painter.image(
                texture.id(),
                rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                Color32::from_rgba_unmultiplied(255, 255, 255, 70),
            );
            painter.rect_filled(
                rect,
                egui::CornerRadius::ZERO,
                Color32::from_black_alpha(70),
            );
        }
        // Repaint pacing is owned by `App::logic` (animation_millis), which
        // avoids scheduling a frame-rate repaint when the window is hidden or
        // when the user disabled background animation.
    } else {
        // Soft vertical gradient.
        let top = colors.bg;
        let bottom = colors.bg2;
        let mut mesh = egui::Mesh::default();
        mesh.colored_vertex(rect.left_top(), top);
        mesh.colored_vertex(rect.right_top(), top);
        mesh.colored_vertex(rect.right_bottom(), bottom);
        mesh.colored_vertex(rect.left_bottom(), bottom);
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(0, 2, 3);
        painter.add(egui::Shape::mesh(mesh));

        if mode == BackgroundMode::Image {
            app.ensure_custom_background();
            if let Some((_, Some(texture))) = &app.custom_background {
                let image_size = texture.size_vec2();
                let rect_aspect = rect.width() / rect.height().max(1.0);
                let image_aspect = image_size.x / image_size.y.max(1.0);
                let mut uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                // Cover-fit instead of stretching: crop the longer source axis.
                if image_aspect > rect_aspect {
                    let half = rect_aspect / image_aspect * 0.5;
                    uv.min.x = 0.5 - half;
                    uv.max.x = 0.5 + half;
                } else if image_aspect < rect_aspect {
                    let half = image_aspect / rect_aspect * 0.5;
                    uv.min.y = 0.5 - half;
                    uv.max.y = 0.5 + half;
                }
                painter.image(texture.id(), rect, uv, Color32::WHITE);
                painter.rect_filled(
                    rect,
                    egui::CornerRadius::ZERO,
                    Color32::from_rgba_unmultiplied(
                        8,
                        10,
                        16,
                        (60.0 + 80.0 * app.settings.theme.background_opacity).clamp(0.0, 180.0)
                            as u8,
                    ),
                );
            }
        } else if cpu_cover && let Some(texture) = app.background.as_ref() {
            painter.image(
                texture.id(),
                rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                Color32::from_rgba_unmultiplied(255, 255, 255, 70),
            );
            painter.rect_filled(
                rect,
                egui::CornerRadius::ZERO,
                Color32::from_black_alpha(70),
            );
        }
    }

    accent_glow(&painter, rect, colors.accent, 9);
}

fn accent_glow(painter: &egui::Painter, rect: egui::Rect, accent: Color32, alpha: u8) {
    painter.circle_filled(
        egui::pos2(rect.right() - 40.0, rect.top() + 30.0),
        300.0,
        Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), alpha),
    );
}

fn apply_meta(track: &mut Track, meta: &LocalMeta) {
    if !meta.title.trim().is_empty() {
        track.title = meta.title.clone();
    }
    if !meta.artist.trim().is_empty() {
        track.artist = meta.artist.clone();
    }
    if !meta.album.trim().is_empty() {
        track.album = meta.album.clone();
    }
    if meta.track_no.is_some() {
        track.track_no = meta.track_no;
    }
    if meta.genre.is_some() {
        track.genre = meta.genre.clone();
    }
    if meta.duration.is_some() {
        track.duration = meta.duration;
    }
}

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
                if matches!(key.as_str(), "musicid" | "songid" | "mid" | "id") {
                    let value = value.trim();
                    if !value.is_empty() {
                        return Some(value.to_string());
                    }
                }
            }
        }
    }
    None
}

fn is_audio_url(url: &url::Url) -> bool {
    let path = url.path().to_ascii_lowercase();
    ["mp3", "ogg", "wav", "flac", "m4a", "aac", "opus"]
        .iter()
        .any(|ext| path.ends_with(&format!(".{ext}")))
}

fn is_playlist_url(url: &url::Url) -> bool {
    let path = url.path().to_ascii_lowercase();
    path.ends_with(".mp3list") || path.ends_with(".m3u") || path.ends_with(".m3u8")
}

fn cache_dir_usage(dir: &Path) -> (usize, u64) {
    let mut files = 0usize;
    let mut bytes = 0u64;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (0, 0);
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.is_dir() {
            let (sub_files, sub_bytes) = cache_dir_usage(&path);
            files += sub_files;
            bytes += sub_bytes;
        } else if metadata.is_file() {
            files += 1;
            bytes += metadata.len();
        }
    }
    (files, bytes)
}

/// Best-effort recursive removal of cache files. Directories are kept and
/// files still in use (for example the audio cache currently being streamed)
/// are skipped, so the returned totals describe what was actually removed.
fn clear_cache_files(dir: &Path, recursive: bool) -> (usize, u64) {
    let mut removed = 0usize;
    let mut freed = 0u64;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (0, 0);
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.is_dir() {
            if recursive {
                let (sub_removed, sub_freed) = clear_cache_files(&path, true);
                removed += sub_removed;
                freed += sub_freed;
            }
        } else if metadata.is_file() && std::fs::remove_file(&path).is_ok() {
            removed += 1;
            freed += metadata.len();
        }
    }
    (removed, freed)
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut os = path.as_os_str().to_os_string();
    os.push(suffix);
    PathBuf::from(os)
}

/// Remove download `.part` files and their `.part.meta` companions. Orphan
/// metadata files (whose part is already gone) are removed too.
fn clear_download_partials_in(dir: &Path) -> (usize, u64) {
    let mut removed = 0usize;
    let mut freed = 0u64;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (0, 0);
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.is_dir() {
            let (sub_removed, sub_freed) = clear_download_partials_in(&path);
            removed += sub_removed;
            freed += sub_freed;
            continue;
        }
        let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        if name.ends_with(".part") {
            let meta = with_suffix(&path, ".meta");
            if std::fs::remove_file(&path).is_ok() {
                removed += 1;
                freed += metadata.len();
                if let Ok(meta_size) = std::fs::metadata(&meta) {
                    let _ = std::fs::remove_file(&meta);
                    freed += meta_size.len();
                }
            }
        } else if name.ends_with(".part.meta") {
            let part = path.with_extension("part");
            if !part.exists() && std::fs::remove_file(&path).is_ok() {
                freed += metadata.len();
            }
        }
    }
    (removed, freed)
}

pub fn default_settings_and_session(paths: &Paths) -> (Settings, Session) {
    let settings = load_settings(&paths.settings_file);
    let mut session: Session = crate::config::load_json(&paths.session_file);
    if session.volume <= 0.0 {
        session.volume = settings.playback.volume;
    }
    (settings, session)
}

/// Waker handed to the native overlay threads. A button/menu action wakes the
/// hidden root window immediately so the action queue is drained without a
/// periodic repaint heartbeat.
fn overlay_waker(ctx: &Context) -> std::sync::Arc<dyn Fn() + Send + Sync> {
    let ctx = ctx.clone();
    std::sync::Arc::new(move || {
        ctx.request_repaint_of(egui::ViewportId::ROOT);
    })
}

