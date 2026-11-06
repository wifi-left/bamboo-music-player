//! Application state, layout shell, playback orchestration and persistence.

mod fonts;
mod theme;
pub mod views;
pub mod widgets;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui::{Align2, Color32, Context, Id, Key, Modifiers, Order, RichText, ViewportCommand, vec2};

use crate::audio::{AudioEngine, EngineEvent, EngineSource, OutputInfo, PlaybackState};
use crate::config::{
    CloseBehavior, Language, Paths, Session, Settings, load_settings, save_session, save_settings,
};
use crate::i18n::t;
use crate::model::{Page, Track, TrackSource, normalize_path};
use crate::platform::tray::{Tray, TrayAction};
use crate::platform::window;
use crate::library::{BrowseService, FolderBrowser, ScanEvent, ScanService};
use crate::playlist::Queue;
use crate::util::metadata::{self, LocalMeta, MetadataService};
use crate::util::{format_time, is_audio_file, open_in_explorer};
use theme::ThemeColors;



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

#[derive(Clone, Debug)]
pub struct ResumeIntent {
    pub key: String,
    pub position: f64,
    pub was_playing: bool,
}

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
    pub background: Option<egui::TextureHandle>,
    pub placeholder_cover: Option<([u8; 3], egui::TextureHandle)>,
    pub cover_key: String,
    pub toasts: VecDeque<Toast>,
    pub hwnd: Option<isize>,
    pub tray: Option<Tray>,
    pub tray_failed: bool,
    pub tray_state_key: String,
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
    pub last_window_rect: Option<(f32, f32, f32, f32)>,
    pub window_maximized: bool,
    pub last_title: String,
    pub last_font_config: (Option<String>, u8),
    pub startup_args: Vec<PathBuf>,
    pub restore_notice_pending: bool,
    pub show_lyrics_sidebar: bool,
    pub settings_scroll_to: Option<&'static str>,
    pub memory_info: Option<crate::platform::memory::MemoryInfo>,
    pub last_memory_sample: Instant,
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
    pub current_playlist: Option<PathBuf>,
    pub resume_after_import: Option<ResumeIntent>,
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        settings: Settings,
        session: Session,
        paths: Paths,
        startup_args: Vec<PathBuf>,
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
        let engine = AudioEngine::new(settings.playback.volume, settings.playback.muted);
        let device_info = engine.output_info();
        let lang = settings.language;
        let gpu_info = cc
            .wgpu_render_state
            .as_ref()
            .map(|state| {
                let info = state.adapter.get_info();
                format!("{} · {:?}", info.name, info.backend)
            })
            .unwrap_or_else(|| "未知渲染后端".to_string());

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
            .or_else(|| {
                session
                    .current_index
                    .filter(|i| *i < session.queue.len())
            });
        if queue.shuffle {
            queue.build_shuffle_order(true);
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
        );
        let current_playlist = session.playlist_path.clone();

        let mut app = Self {
            settings,
            session,
            paths,
            lang,
            page: Page::NowPlaying,
            engine,
            queue,
            current_track: None,
            current_duration: None,
            cover: None,
            background: None,
            placeholder_cover: None,
            cover_key: String::new(),
            toasts: VecDeque::new(),
            hwnd,
            tray: None,
            tray_failed: false,
            tray_state_key: String::new(),
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
            metadata: MetadataService::new(),
            last_window_rect,
            window_maximized,
            last_title: String::new(),
            last_font_config,
            startup_args,
            restore_notice_pending: false,
            show_lyrics_sidebar,
            settings_scroll_to: None,
            memory_info: crate::platform::memory::process_memory(),
            last_memory_sample: Instant::now(),
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
            current_playlist,
            resume_after_import: None,
        };

        app.page = page;
        app.restore_notice_pending = app.session.saved_at.is_some() && !app.queue.is_empty();

        if let Some(index) = app.queue.current {
            // Restore the position even when paused; the setting only controls
            // whether playback starts automatically.
            let start = app.session.position_secs.max(0.0);
            let autoplay = app.settings.playback.resume_on_start;
            app.load_index(index, start, autoplay);
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

        let args = app.startup_args.clone();
        if !args.is_empty() {
            app.add_paths(args, app.queue.current.is_none());
        }
        app.playback_state = app.engine.state();
        app
    }

    // ------------------------------------------------------------------
    // Basic helpers
    // ------------------------------------------------------------------

    pub fn t(&self, key: &'static str) -> &'static str {
        t(self.lang, key)
    }

    pub fn colors(&self) -> ThemeColors {
        theme::colors(&self.ctx, &self.settings)
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
        self.page = page;
        self.session_dirty = true;
    }

    pub fn is_playing(&self) -> bool {
        self.playback_state == PlaybackState::Playing
    }

    pub fn position_secs(&self) -> f64 {
        self.seek_preview.unwrap_or_else(|| {
            if self.current_track.is_some() {
                self.engine.position_secs()
            } else {
                0.0
            }
        })
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

    pub fn theme_key(&self) -> String {
        format!(
            "{:?}-{:?}-{}-{}",
            self.settings.theme.mode,
            self.settings.theme.background,
            self.settings.theme.roundness,
            self.settings.theme.accent[0] as u32
                + self.settings.theme.accent[1] as u32 * 256
                + self.settings.theme.accent[2] as u32 * 65536
        )
    }

    // ------------------------------------------------------------------
    // Playback
    // ------------------------------------------------------------------

    pub fn play_index(&mut self, index: usize) {
        self.load_index(index, 0.0, true);
    }

    pub fn load_index(&mut self, index: usize, start_secs: f64, autoplay: bool) {
        if index >= self.queue.items.len() {
            return;
        }
        let track = self.queue.items[index].clone();
        self.queue.set_current(index);
        self.current_track = Some(track.clone());
        self.current_duration = track.duration;
        self.cover_key.clear();
        self.refresh_visuals();
        self.update_title();
        self.session_dirty = true;
        self.playback_state = if autoplay {
            PlaybackState::Loading
        } else {
            PlaybackState::Paused
        };

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
                self.engine
                    .load(EngineSource::Local(path.clone()), start_secs, autoplay);
            }
            TrackSource::Url { url } | TrackSource::Radio { url } => {
                self.engine
                    .load(EngineSource::Url(url.clone()), start_secs, autoplay);
            }
            TrackSource::Bamboo { .. } => {
                self.toast(
                    "在线曲目播放将在在线音源阶段启用".to_string(),
                    ToastKind::Info,
                );
                self.playback_state = PlaybackState::Stopped;
            }
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
                self.engine.play();
                self.playback_state = PlaybackState::Playing;
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
            PlaybackState::Loading => {}
        }
    }

    pub fn next_track(&mut self) {
        if let Some(index) = self.queue.advance(false) {
            self.load_index(index, 0.0, true);
        }
    }

    pub fn prev_track(&mut self) {
        if self.current_track.is_some() && self.engine.position_secs() > 3.0 {
            self.seek_to(0.0);
            return;
        }
        if let Some(index) = self.queue.back() {
            self.load_index(index, 0.0, true);
        }
    }

    pub fn advance_auto(&mut self) {
        if !self.settings.playback.auto_advance {
            self.playback_state = PlaybackState::Stopped;
            return;
        }
        match self.queue.advance(true) {
            Some(index) => self.load_index(index, 0.0, true),
            None => {
                self.playback_state = PlaybackState::Stopped;
                self.toast(self.t("audio.ended").to_string(), ToastKind::Info);
            }
        }
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
        self.session_dirty = true;
        self.sync_tray();
    }

    pub fn cycle_repeat(&mut self) {
        self.queue.repeat = self.queue.repeat.cycle();
        self.session_dirty = true;
        self.sync_tray();
    }

    pub fn remove_index(&mut self, index: usize) {
        let was_current = self.queue.current == Some(index);
        self.queue.remove(index);
        self.session_dirty = true;
        if was_current {
            self.engine.stop();
            self.current_track = None;
            self.cover = None;
            self.background = None;
            self.cover_key.clear();
            self.current_duration = None;
            if let Some(next) = self.queue.current.or_else(|| {
                (!self.queue.is_empty()).then_some(0)
            }) {
                self.load_index(next, 0.0, true);
            } else {
                self.update_title();
            }
        }
    }

    pub fn clear_queue(&mut self) {
        self.engine.stop();
        self.queue.clear();
        self.current_track = None;
        self.current_duration = None;
        self.cover = None;
        self.background = None;
        self.cover_key.clear();
        self.playback_state = PlaybackState::Stopped;
        self.current_playlist = None;
        self.resume_after_import = None;
        self.session_dirty = true;
        self.update_title();
    }

    // ------------------------------------------------------------------
    // Adding files
    // ------------------------------------------------------------------

    pub fn open_files_dialog(&mut self) {
        if let Some(files) = rfd::FileDialog::new()
            .add_filter(
                "Audio",
                &["mp3", "ogg", "wav", "flac", "m4a", "aac", "opus"],
            )
            .pick_files()
        {
            self.add_paths(files, self.queue.current.is_none());
        }
    }

    pub fn open_folder_dialog(&mut self) {
        if let Some(folder) = rfd::FileDialog::new().pick_folder() {
            self.add_paths(vec![folder], self.queue.current.is_none());
        }
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
            self.toast("没有找到可播放的音频文件（歌单导入将在下一阶段启用）", ToastKind::Info);
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
                self.metadata.request(path);
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
        self.toast("正在后台扫描目录…", ToastKind::Info);
    }

    pub fn cancel_scan(&mut self) {
        self.scan.cancel();
        self.scan_active = false;
        self.scan_id = 0;
        self.scan_current.clear();
    }

    fn poll_scan(&mut self) {
        // At most two 500-file batches per frame keep queue insertion smooth.
        let mut batches = 0;
        while batches < 2 {
            let Some(event) = self.scan.try_recv() else {
                break;
            };
            match event {
                ScanEvent::Progress {
                    id,
                    found,
                    current,
                } if id == self.scan_id => {
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
                            self.metadata.request(path);
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
                    if cancelled {
                        self.toast(
                            format!("扫描已取消，已加入 {} 首", self.scan_added),
                            ToastKind::Info,
                        );
                    } else if total == 0 {
                        self.toast("没有找到可播放的音频文件", ToastKind::Info);
                    } else {
                        self.toast(
                            format!(
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
            self.ctx.request_repaint_after(Duration::from_millis(33));
        }
    }

    pub fn browse_folder_dialog(&mut self) {
        if let Some(folder) = rfd::FileDialog::new().pick_folder() {
            if !self.settings.library.folders.iter().any(|f| f == &folder) {
                self.settings.library.folders.push(folder.clone());
                self.settings_dirty = true;
            }
            self.open_browser(folder);
        }
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
            self.metadata.request(path);
            self.session_dirty = true;
            self.load_index(before, 0.0, true);
        }
    }

    pub fn import_playlist_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Bamboo Playlist", &["mp3list"])
            .add_filter("M3U", &["m3u", "m3u8"])
            .pick_file()
        {
            self.import_playlist(path, true);
        }
    }

    pub fn export_playlist_dialog(&mut self) {
        if self.queue.is_empty() {
            self.toast("当前队列为空", ToastKind::Info);
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
                    format!("{}: {} 首", self.t("toast.exported"), count),
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
        let intent = if replace {
            self.current_track.as_ref().map(|track| ResumeIntent {
                key: track.dedupe_key(),
                position: self.engine.position_secs(),
                was_playing: self.is_playing(),
            })
        } else {
            None
        };

        if replace {
            self.engine.stop();
            self.scan.cancel();
            self.scan_active = false;
            self.scan_id = 0;
            self.queue.clear();
            self.current_track = None;
            self.current_duration = None;
            self.cover = None;
            self.background = None;
            self.cover_key.clear();
            self.playback_state = PlaybackState::Stopped;
            self.resume_after_import = intent;
            self.current_playlist = Some(path.clone());
        }

        let added = self.add_imported_entries(&parsed);
        if !parsed.dirs.is_empty() {
            self.start_scan(parsed.dirs.clone(), false);
        }
        self.try_resume_after_import();

        if self.queue.is_empty() && !self.scan_active {
            self.toast("歌单为空或文件已全部失效", ToastKind::Error);
        } else {
            let missing = if parsed.missing > 0 {
                format!("，{} 个文件不存在", parsed.missing)
            } else {
                String::new()
            };
            self.toast(
                format!("已导入 {} 首{missing} · {}", added, parsed.encoding),
                ToastKind::Success,
            );
        }
        self.session_dirty = true;
        self.update_title();
    }

    fn add_imported_entries(
        &mut self,
        parsed: &crate::playlist::mp3list::ParsedPlaylist,
    ) -> usize {
        let mut added = 0usize;
        for path in &parsed.files {
            if self.queue.add(Track::local(path.clone())) {
                added += 1;
                self.metadata.request(path.clone());
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

    fn try_resume_after_import(&mut self) {
        let Some(intent) = self.resume_after_import.clone() else {
            return;
        };
        if let Some(index) = self.queue.position_of_key(&intent.key) {
            self.resume_after_import = None;
            tracing::info!(
                "resume {} at {:.2}s (autoplay={})",
                intent.key,
                intent.position,
                intent.was_playing
            );
            self.load_index(index, intent.position, intent.was_playing);
            self.toast("已恢复上次的曲目与播放位置", ToastKind::Info);
        } else if !self.scan_active {
            self.resume_after_import = None;
            if !self.queue.is_empty() {
                self.toast("上次播放的曲目已不在新歌单中", ToastKind::Info);
            }
        }
    }

    pub fn remove_missing_entries(&mut self) {
        let missing: Vec<usize> = self
            .queue
            .items
            .iter()
            .enumerate()
            .filter_map(|(i, t)| match &t.source {
                TrackSource::Local { path } if !path.is_file() => Some(i),
                _ => None,
            })
            .collect();
        for index in missing.into_iter().rev() {
            self.queue.remove(index);
        }
        self.session_dirty = true;
    }

    // ------------------------------------------------------------------
    // Visuals / metadata
    // ------------------------------------------------------------------

    fn refresh_visuals(&mut self) {
        let Some(track) = self.current_track.clone() else {
            self.cover = None;
            self.background = None;
            self.cover_key.clear();
            return;
        };
        let key = track.dedupe_key();
        if self.cover_key == key && self.cover.is_some() {
            return;
        }

        let accent = self.settings.theme.accent;
        let mut updated = track.clone();
        let mut cover_bytes: Option<Vec<u8>> = None;

        if let Some(path) = track.source.path() {
            if let Some(meta) = metadata::read_local(path) {
                apply_meta(&mut updated, &meta);
                if updated.duration.is_none() {
                    updated.duration = meta.duration;
                }
                cover_bytes = meta.cover;
            }
            updated.apply_filename_fallback();
            if cover_bytes.is_none() {
                if let Some(sidecar) = metadata::sidecar_cover_path(path) {
                    cover_bytes = std::fs::read(sidecar).ok();
                }
            }
        }

        if let Some(index) = self.queue.current {
            if let Some(item) = self.queue.items.get_mut(index) {
                if item.dedupe_key() == key {
                    *item = updated.clone();
                }
            }
        }
        self.current_track = Some(updated.clone());
        if self.current_duration.is_none() {
            self.current_duration = updated.duration;
        }

        self.cover = None;
        self.background = None;
        if let Some(bytes) = cover_bytes.as_deref() {
            if let Some(textures) = crate::util::image_util::cover_textures(
                &self.ctx,
                bytes,
                accent,
                512,
                6,
            ) {
                self.cover = Some(textures.cover);
                self.background = textures.background;
            }
        }
        if self.cover.is_none() {
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
        self.cover_key = key;
        self.update_title();
    }

    fn poll_metadata(&mut self) {
        // Bounded batches keep the frame time predictable even when a scan
        // enqueues thousands of tag reads.
        let updates = self.metadata.poll_batch(100);
        if updates.is_empty() {
            return;
        }
        let current_key = self.current_track.as_ref().map(|t| t.dedupe_key());
        let mut index = std::collections::HashMap::with_capacity(self.queue.items.len());
        for (i, track) in self.queue.items.iter().enumerate() {
            index.insert(track.dedupe_key(), i);
        }
        let mut refresh_current = false;
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
            if current_key.as_deref() == Some(key.as_str()) {
                if self.current_duration.is_none() {
                    self.current_duration = updated.duration;
                }
                self.current_track = Some(updated);
                refresh_current = true;
            }
        }
        if refresh_current && self.cover_key.is_empty() {
            self.refresh_visuals();
        }
        self.ctx.request_repaint_after(Duration::from_millis(33));
    }

    // ------------------------------------------------------------------
    // Engine events, tray, shortcuts
    // ------------------------------------------------------------------

    fn handle_engine_events(&mut self) {
        let events = self.engine.poll_events();
        for event in events {
            match event {
                EngineEvent::Loaded {
                    duration,
                    sample_rate,
                    channels,
                } => {
                    if let Some(d) = duration {
                        self.current_duration = Some(d);
                        if let Some(track) = self.current_track.as_mut() {
                            if track.duration.is_none() {
                                track.duration = Some(d);
                            }
                        }
                    }
                    tracing::info!(
                        "audio loaded: {} Hz, {} ch",
                        sample_rate,
                        channels
                    );
                    self.playback_state = self.engine.state();
                }
                EngineEvent::Ended => {
                    self.playback_state = PlaybackState::Stopped;
                    self.advance_auto();
                }
                EngineEvent::Error(err) => {
                    self.playback_state = PlaybackState::Stopped;
                    self.toast(format!("{}: {err}", self.t("audio.error")), ToastKind::Error);
                }
                EngineEvent::Underrun => {}
            }
        }
    }

    fn ensure_tray(&mut self) {
        if !self.settings.features.tray {
            self.tray = None;
            return;
        }
        if self.tray.is_none() && !self.tray_failed {
            match Tray::new(self.ctx.clone(), self.lang, self.settings.theme.accent) {
                Ok(tray) => {
                    self.tray = Some(tray);
                    self.sync_tray();
                }
                Err(err) => {
                    tracing::warn!("tray creation failed: {err:#}");
                    self.tray_failed = true;
                    self.toast("托盘图标创建失败", ToastKind::Error);
                }
            }
        }
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
        let actions = self
            .tray
            .as_ref()
            .map(|t| t.poll())
            .unwrap_or_default();
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
                TrayAction::ToggleDesktopLyrics => {
                    self.settings.desktop_lyrics.visible_at_start =
                        !self.settings.desktop_lyrics.visible_at_start;
                    self.settings_dirty = true;
                    self.toast("桌面歌词窗口将在桌面歌词阶段启用", ToastKind::Info);
                }
                TrayAction::ToggleLockLyrics => {
                    self.settings.desktop_lyrics.locked =
                        !self.settings.desktop_lyrics.locked;
                    self.settings_dirty = true;
                }
                TrayAction::Volume(v) => self.set_volume(v),
                TrayAction::Quit => {
                    self.allow_close = true;
                    self.save_now();
                    ctx.send_viewport_cmd(ViewportCommand::Close);
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
            window::apply_style(
                raw,
                theme::is_dark(&self.ctx, &self.settings),
                self.settings.theme.roundness,
                self.settings.window.capture_protection,
            );
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
        if !close_requested || self.allow_close {
            return;
        }
        match self.settings.window.close_behavior {
            CloseBehavior::Exit => {
                self.save_now();
                self.allow_close = true;
            }
            CloseBehavior::Tray | CloseBehavior::Ask => {
                ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                ctx.send_viewport_cmd(ViewportCommand::Visible(false));
                self.window_hidden = true;
                self.save_now();
            }
        }
    }

    fn handle_shortcuts(&mut self, ctx: &Context) {
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

    pub fn toggle_lyrics_sidebar(&mut self) {
        self.show_lyrics_sidebar = !self.show_lyrics_sidebar;
        self.settings.lyrics.sidebar_visible = self.show_lyrics_sidebar;
        self.settings_dirty = true;
        self.ctx.request_repaint();
    }

    pub fn toggle_desktop_lyrics(&mut self) {
        self.settings.desktop_lyrics.visible_at_start =
            !self.settings.desktop_lyrics.visible_at_start;
        self.settings_dirty = true;
        self.toast("桌面歌词窗口将在桌面歌词阶段启用", ToastKind::Info);
        self.sync_tray();
    }

    // ------------------------------------------------------------------
    // Persistence
    // ------------------------------------------------------------------

    pub fn save_now(&mut self) {
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
        let position = if self.current_track.is_some() {
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
            playlist_path: self.current_playlist.clone(),
            saved_at: Some(chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()),
        }
    }

    fn maybe_autosave(&mut self, ctx: &Context) {
        self.update_window_rect(ctx);
        if self.settings_dirty
            && self.last_settings_save.elapsed() > Duration::from_millis(900)
        {
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

    pub fn reset_settings(&mut self) {
        let sources = self.settings.sources.clone();
        self.settings = Settings::default();
        // Keep user-configured endpoints: resetting should not erase them.
        if !sources.is_empty() {
            self.settings.sources = sources;
        }
        self.apply_appearance();
        self.toast("设置已恢复默认", ToastKind::Success);
    }

    pub fn open_data_dir(&self) {
        open_in_explorer(&self.paths.root);
    }

    pub fn open_log_dir(&self) {
        open_in_explorer(&self.paths.logs);
    }

    pub fn update_title(&mut self) {
        let title = match self.current_track.as_ref() {
            Some(track) => format!(
                "{} - {} — Bamboo Music Player",
                track.display_artist(),
                track.display_title()
            ),
            None => "Bamboo Music Player".to_string(),
        };
        if title != self.last_title {
            self.ctx.send_viewport_cmd(ViewportCommand::Title(title.clone()));
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
        let top_rect = egui::Rect::from_min_size(
            egui::pos2(bar.left(), y0),
            egui::vec2(bar.width(), top_h),
        );
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
                let (cover_rect, _) = ui.allocate_exact_size(
                    egui::vec2(cover_side, cover_side),
                    egui::Sense::hover(),
                );
                if let Some(cover) = &self.cover {
                    ui.painter().image(
                        cover.id(),
                        cover_rect,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        Color32::WHITE,
                    );
                } else {
                    ui.painter().rect_filled(
                        cover_rect,
                        egui::CornerRadius::same(9),
                        colors.card,
                    );
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
                                        RichText::new(title)
                                            .size(13.5)
                                            .strong()
                                            .color(colors.text),
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
                                        RichText::new(artist)
                                            .size(11.0)
                                            .color(colors.text_weak),
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
        if ui
            .interact(
                left_rect,
                ui.id().with("bamboo-player-left"),
                egui::Sense::click(),
            )
            .clicked()
        {
            self.set_page(Page::NowPlaying);
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
                    egui_phosphor::regular::TEXT_ALIGN_LEFT,
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
                if let Some(v) =
                    widgets::volume_bar(ui, self.volume(), &colors, volume_w)
                {
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

                let (left_time, _) = ui.allocate_exact_size(
                    egui::vec2(time_w, bottom_h),
                    egui::Sense::hover(),
                );
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
                let (right_time, _) = ui.allocate_exact_size(
                    egui::vec2(time_w, bottom_h),
                    egui::Sense::hover(),
                );
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

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        let colors = self.colors();
        let width = self.settings.theme.sidebar_width;
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            let (rect, _) = ui.allocate_exact_size(vec2(36.0, 36.0), egui::Sense::hover());
            ui.painter()
                .rect_filled(rect, egui::CornerRadius::same(11), colors.accent);
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                egui_phosphor::regular::MUSIC_NOTES,
                egui::FontId::proportional(21.0),
                Color32::from_rgb(10, 14, 22),
            );
            ui.add_space(8.0);
            ui.vertical(|ui| {
                ui.label(RichText::new("Bamboo").size(18.0).strong().color(colors.text));
                ui.label(
                    RichText::new("MUSIC PLAYER")
                        .size(8.5)
                        .color(colors.text_faint),
                );
            });
        });
        ui.add_space(16.0);

        let nav = [
            (Page::NowPlaying, egui_phosphor::regular::PLAY_CIRCLE, "nav.now_playing"),
            (Page::Library, egui_phosphor::regular::MUSIC_NOTES, "nav.library"),
            (Page::Playlists, egui_phosphor::regular::QUEUE, "nav.playlists"),
            (Page::Search, egui_phosphor::regular::MAGNIFYING_GLASS, "nav.search"),
            (Page::Discover, egui_phosphor::regular::COMPASS, "nav.discover"),
        ];
        for (page, icon, key) in nav {
            if widgets::nav_button(ui, icon, self.t(key), self.page == page, &colors).clicked() {
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
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            ui.label(
                RichText::new("导入 .mp3list / .m3u 后显示在这里")
                    .size(11.0)
                    .color(colors.text_faint),
            );
        });

        ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.label(
                    RichText::new(format!("v{}", env!("CARGO_PKG_VERSION")))
                        .size(10.5)
                        .color(colors.text_faint),
                );
            });
            ui.add_space(4.0);
            if widgets::nav_button(
                ui,
                egui_phosphor::regular::GEAR,
                self.t("nav.settings"),
                self.page == Page::Settings,
                &colors,
            )
            .clicked()
            {
                self.set_page(Page::Settings);
            }
            ui.add_space(4.0);
            let _ = width;
        });
    }

    fn central(&mut self, ui: &mut egui::Ui) {
        let colors = self.colors();
        let background = self.background.clone();
        let mode = self.settings.theme.background;
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show_inside(ui, |ui| {
                paint_background(ui, &colors, background.as_ref(), mode);
                egui::Frame::NONE
                    .inner_margin(egui::Margin::symmetric(26, 20))
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                match self.page {
                                    Page::NowPlaying => views::now_playing(self, ui),
                                    Page::Library => views::library(self, ui),
                                    Page::Playlists => views::playlists(self, ui),
                                    Page::Search => views::search(self, ui),
                                    Page::Discover => views::discover(self, ui),
                                    Page::Settings => views::settings(self, ui),
                                }
                            });
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
                ui.vertical(|ui| {
                    for toast in self.toasts.iter().rev() {
                        let (fg, bg) = match toast.kind {
                            ToastKind::Info => (colors.text, colors.panel),
                            ToastKind::Success => (colors.success, colors.panel),
                            ToastKind::Error => (colors.danger, colors.panel),
                        };
                        egui::Frame::new()
                            .fill(bg)
                            .stroke(egui::Stroke::new(1.0, colors.line))
                            .corner_radius(egui::CornerRadius::same(10))
                            .inner_margin(egui::Margin::symmetric(14, 9))
                            .show(ui, |ui| {
                                ui.label(RichText::new(&toast.text).size(12.5).color(fg));
                            });
                    }
                });
            });
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        self.handle_engine_events();
        self.poll_metadata();
        self.poll_scan();
        self.poll_browser();
        self.handle_tray(ctx);
        self.handle_close_request(ctx);
        self.handle_shortcuts(ctx);
        self.maybe_autosave(ctx);

        let focused = ctx.input(|i| i.viewport().focused).unwrap_or(true);
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
        match self.playback_state {
            PlaybackState::Playing => {
                let ms = if self.window_hidden || minimized || occluded {
                    1000
                } else if focused {
                    100
                } else {
                    250
                };
                ctx.request_repaint_after(Duration::from_millis(ms));
            }
            PlaybackState::Loading => {
                ctx.request_repaint_after(Duration::from_millis(60));
            }
            _ => {
                if !self.toasts.is_empty() {
                    ctx.request_repaint_after(Duration::from_millis(250));
                }
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
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
            .show_inside(ui, |ui| self.player_bar(ui));

        let side_frame = egui::Frame::new()
            .fill(colors.panel)
            .inner_margin(egui::Margin::symmetric(8, 10));
        egui::Panel::left("bamboo-sidebar")
            .exact_size(self.settings.theme.sidebar_width)
            .resizable(false)
            .frame(side_frame)
            .show_inside(ui, |ui| self.sidebar(ui));

        if self.show_lyrics_sidebar {
            let lyrics_frame = egui::Frame::new()
                .fill(colors.panel)
                .stroke(egui::Stroke::new(1.0, colors.line))
                .inner_margin(egui::Margin::symmetric(18, 16));
            egui::Panel::right("bamboo-lyrics-sidebar")
                .default_size(330.0)
                .size_range(260.0..=520.0)
                .resizable(true)
                .frame(lyrics_frame)
                .show_inside(ui, |ui| {
                    views::lyrics_sidebar(self, ui);
                });
        }

        self.central(ui);
        self.toasts(&ctx);
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
            if point_in_triangle(
                (x as f32 + 0.5, y as f32 + 0.5),
                tri[0],
                tri[1],
                tri[2],
            ) {
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

fn play_button(ui: &mut egui::Ui, playing: bool, enabled: bool, colors: &ThemeColors) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(46.0, 46.0), egui::Sense::click());
    let color = if enabled { colors.accent } else { colors.text_faint };
    if response.hovered() && enabled {
        ui.painter()
            .circle_filled(rect.center(), 24.0, colors.accent_soft);
    }
    ui.painter().circle_filled(rect.center(), 21.0, color);
    let icon = if playing {
        egui_phosphor::regular::PAUSE
    } else {
        egui_phosphor::regular::PLAY
    };
    ui.painter().text(
        rect.center() + vec2(if playing { 0.0 } else { 1.5 }, 0.0),
        Align2::CENTER_CENTER,
        icon,
        egui::FontId::proportional(20.0),
        Color32::from_rgb(10, 14, 22),
    );
    response
}

pub fn paint_background(
    ui: &mut egui::Ui,
    colors: &ThemeColors,
    background: Option<&egui::TextureHandle>,
    mode: crate::config::BackgroundMode,
) {
    let rect = ui.max_rect();
    let painter = ui.painter();
    painter.rect_filled(rect, egui::CornerRadius::ZERO, colors.bg);
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

    if matches!(
        mode,
        crate::config::BackgroundMode::StaticBlur
            | crate::config::BackgroundMode::GpuBlur
            | crate::config::BackgroundMode::Flow
    ) {
        if let Some(texture) = background {
            painter.image(
                texture.id(),
                rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                Color32::from_rgba_unmultiplied(255, 255, 255, 70),
            );
            painter.rect_filled(rect, egui::CornerRadius::ZERO, Color32::from_black_alpha(70));
        }
    }

    // Accent glow in the upper right.
    let accent = colors.accent;
    painter.circle_filled(
        egui::pos2(rect.right() - 40.0, rect.top() + 30.0),
        300.0,
        Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 9),
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

pub fn default_settings_and_session(paths: &Paths) -> (Settings, Session) {
    let settings = load_settings(&paths.settings_file);
    let mut session: Session = crate::config::load_json(&paths.session_file);
    if session.volume <= 0.0 {
        session.volume = settings.playback.volume;
    }
    (settings, session)
}

pub fn open_path(path: &Path) {
    open_in_explorer(path);
}
