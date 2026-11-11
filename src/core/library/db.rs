//! SQLite-backed local library index.
//!
//! The database stores one row per audio file (keyed by [`Track::dedupe_key`]),
//! its tag metadata and play statistics. Indexing, queries and aggregation all
//! run on a worker thread so a large library never blocks the UI thread. The
//! UI receives snapshots through a channel.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use crossbeam_channel::{Receiver, Sender, unbounded};
use rayon::prelude::*;
use rusqlite::{Connection, params};
use walkdir::WalkDir;

use crate::config::DedupeMode;
use crate::model::{Track, normalize_path};
use crate::util::is_audio_file;
use crate::util::metadata::read_local_with_cover;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS tracks (
    key            TEXT PRIMARY KEY,
    path           TEXT NOT NULL,
    title          TEXT NOT NULL DEFAULT '',
    artist         TEXT NOT NULL DEFAULT '',
    album          TEXT NOT NULL DEFAULT '',
    genre          TEXT,
    year           INTEGER,
    track_no       INTEGER,
    duration       REAL,
    bitrate        INTEGER,
    sample_rate    INTEGER,
    lossless       INTEGER NOT NULL DEFAULT 0,
    format         TEXT NOT NULL DEFAULT '',
    size           INTEGER NOT NULL DEFAULT 0,
    added_at       INTEGER NOT NULL DEFAULT 0,
    modified_at    INTEGER,
    play_count     INTEGER NOT NULL DEFAULT 0,
    skip_count     INTEGER NOT NULL DEFAULT 0,
    last_played_at INTEGER,
    missing        INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_tracks_artist ON tracks(artist);
CREATE INDEX IF NOT EXISTS idx_tracks_album ON tracks(album);
CREATE INDEX IF NOT EXISTS idx_tracks_added ON tracks(added_at);
CREATE INDEX IF NOT EXISTS idx_tracks_played ON tracks(last_played_at);
CREATE INDEX IF NOT EXISTS idx_tracks_play_count ON tracks(play_count);
CREATE TABLE IF NOT EXISTS fingerprints (
    key      TEXT PRIMARY KEY,
    size     INTEGER NOT NULL,
    modified INTEGER NOT NULL,
    simhash  INTEGER NOT NULL,
    hashes   BLOB NOT NULL
);
"#;

// ---------------------------------------------------------------------------
// Public data types
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct DbTrack {
    pub key: String,
    pub path: PathBuf,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub genre: Option<String>,
    pub year: Option<i32>,
    pub track_no: Option<u32>,
    pub duration: Option<f64>,
    pub bitrate: Option<u32>,
    pub sample_rate: Option<u32>,
    pub lossless: bool,
    pub format: String,
    pub size: u64,
    pub modified_at: Option<i64>,
}

impl DbTrack {
    pub fn from_track(track: &Track) -> Option<Self> {
        let path = track.source.path()?.to_path_buf();
        let metadata = std::fs::metadata(&path).ok();
        Some(Self {
            key: track.dedupe_key(),
            path: path.clone(),
            title: track.title.clone(),
            artist: track.artist.clone(),
            album: track.album.clone(),
            genre: track.genre.clone(),
            year: track.year,
            track_no: track.track_no,
            duration: track.duration,
            bitrate: None,
            sample_rate: None,
            lossless: track.lossless,
            format: format_of(&path),
            size: metadata.as_ref().map(|m| m.len()).unwrap_or(0),
            modified_at: metadata.as_ref().and_then(modified_secs),
        })
    }
}

#[derive(Clone, Debug)]
pub struct LibraryEntry {
    pub key: String,
    pub path: PathBuf,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration: Option<f64>,
    pub play_count: i64,
    pub last_played_at: Option<i64>,
    pub lossless: bool,
    pub format: String,
    pub missing: bool,
    pub size: u64,
}

impl LibraryEntry {
    pub fn to_track(&self) -> Track {
        let mut track = Track::local(self.path.clone());
        if !self.title.trim().is_empty() {
            track.title = self.title.clone();
        }
        if !self.artist.trim().is_empty() {
            track.artist = self.artist.clone();
        }
        if !self.album.trim().is_empty() {
            track.album = self.album.clone();
        }
        track.duration = self.duration;
        track.lossless = self.lossless;
        track
    }

    pub fn subtitle(&self) -> String {
        if self.album.trim().is_empty() {
            self.artist.clone()
        } else {
            format!("{} · {}", self.artist, self.album)
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LibrarySort {
    #[default]
    Title,
    Artist,
    Album,
    Added,
    Duration,
    PlayCount,
    LastPlayed,
    Path,
}

impl LibrarySort {
    pub fn label(self) -> &'static str {
        match self {
            Self::Title => crate::i18n::tr("标题"),
            Self::Artist => crate::i18n::tr("歌手"),
            Self::Album => crate::i18n::tr("专辑"),
            Self::Added => crate::i18n::tr("加入时间"),
            Self::Duration => crate::i18n::tr("时长"),
            Self::PlayCount => crate::i18n::tr("播放次数"),
            Self::LastPlayed => crate::i18n::tr("最近播放"),
            Self::Path => crate::i18n::tr("路径"),
        }
    }

    fn order_by(self) -> &'static str {
        match self {
            Self::Title => "lower(title), lower(artist)",
            Self::Artist => "lower(artist), lower(album), track_no",
            Self::Album => "lower(album), track_no, lower(title)",
            Self::Added => "added_at DESC",
            Self::Duration => "duration DESC",
            Self::PlayCount => "play_count DESC, lower(artist)",
            Self::LastPlayed => "last_played_at DESC",
            Self::Path => "lower(path)",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct LibraryQuery {
    pub search: String,
    pub sort: LibrarySort,
    pub offset: usize,
    pub limit: usize,
}

#[derive(Clone, Debug, Default)]
pub struct LibraryStats {
    pub tracks: u64,
    pub artists: u64,
    pub albums: u64,
    pub total_secs: f64,
    pub total_plays: u64,
    pub lossless: u64,
    pub missing: u64,
    pub total_bytes: u64,
    pub top_artists: Vec<(String, u64)>,
    pub top_tracks: Vec<(String, String, u64)>,
    pub formats: Vec<(String, u64)>,
    pub recently_played: Vec<LibraryEntry>,
}

#[derive(Clone, Debug, Default)]
pub struct DuplicateGroup {
    pub title: String,
    pub artist: String,
    pub entries: Vec<LibraryEntry>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SmartKind {
    RecentlyAdded,
    RecentlyPlayed,
    MostPlayed,
    NeverPlayed,
    Longest,
    Lossless,
    Random,
}

impl SmartKind {
    pub const ALL: [SmartKind; 7] = [
        SmartKind::RecentlyAdded,
        SmartKind::RecentlyPlayed,
        SmartKind::MostPlayed,
        SmartKind::NeverPlayed,
        SmartKind::Longest,
        SmartKind::Lossless,
        SmartKind::Random,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::RecentlyAdded => crate::i18n::tr("最近加入"),
            Self::RecentlyPlayed => crate::i18n::tr("最近播放"),
            Self::MostPlayed => crate::i18n::tr("最常播放"),
            Self::NeverPlayed => crate::i18n::tr("从未播放"),
            Self::Longest => crate::i18n::tr("最长曲目"),
            Self::Lossless => crate::i18n::tr("无损收藏"),
            Self::Random => crate::i18n::tr("随机 50 首"),
        }
    }

    fn sql(self, limit: usize) -> String {
        match self {
            Self::RecentlyAdded => format!(
                "SELECT {COLUMNS} FROM tracks WHERE missing = 0 ORDER BY added_at DESC LIMIT {limit}"
            ),
            Self::RecentlyPlayed => format!(
                "SELECT {COLUMNS} FROM tracks WHERE missing = 0 AND last_played_at IS NOT NULL \
                 ORDER BY last_played_at DESC LIMIT {limit}"
            ),
            Self::MostPlayed => format!(
                "SELECT {COLUMNS} FROM tracks WHERE missing = 0 AND play_count > 0 \
                 ORDER BY play_count DESC, last_played_at DESC LIMIT {limit}"
            ),
            Self::NeverPlayed => format!(
                "SELECT {COLUMNS} FROM tracks WHERE missing = 0 AND play_count = 0 \
                 ORDER BY added_at DESC LIMIT {limit}"
            ),
            Self::Longest => format!(
                "SELECT {COLUMNS} FROM tracks WHERE missing = 0 ORDER BY duration DESC LIMIT {limit}"
            ),
            Self::Lossless => format!(
                "SELECT {COLUMNS} FROM tracks WHERE missing = 0 AND lossless = 1 \
                 ORDER BY lower(artist), lower(album), track_no LIMIT {limit}"
            ),
            Self::Random => format!(
                "SELECT {COLUMNS} FROM tracks WHERE missing = 0 ORDER BY RANDOM() LIMIT {limit}"
            ),
        }
    }
}

const COLUMNS: &str = "key, path, title, artist, album, duration, play_count, last_played_at, lossless, format, missing, size";

// ---------------------------------------------------------------------------
// Events / requests
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub enum DbEvent {
    Error(String),
    IndexProgress {
        indexed: usize,
        current: String,
    },
    IndexDone {
        indexed: usize,
        missing: u64,
        elapsed_ms: u128,
        /// True when the user stopped indexing before the scan finished.
        cancelled: bool,
    },
    QueryDone {
        request_id: u64,
        entries: Vec<LibraryEntry>,
        total: usize,
    },
    /// Every row matching a search/sort, for "add the whole library" actions.
    AllDone {
        request_id: u64,
        entries: Vec<LibraryEntry>,
    },
    StatsDone(Box<LibraryStats>),
    /// `PRAGMA optimize` + `VACUUM` finished; carries fresh statistics.
    MaintenanceDone(Box<LibraryStats>),
    DuplicatesDone(Vec<DuplicateGroup>),
    MissingDone(Vec<LibraryEntry>),
    SmartDone {
        kind: SmartKind,
        entries: Vec<LibraryEntry>,
    },
    Changed,
}

enum DbRequest {
    IndexRoots {
        roots: Vec<PathBuf>,
        extensions: Vec<String>,
        follow_links: bool,
    },
    /// Index a handful of paths reported by the directory watcher without
    /// walking the whole library again.
    IndexPaths(Vec<PathBuf>),
    Query {
        request_id: u64,
        query: LibraryQuery,
    },
    /// Same filter/sort as `Query` but without the page window.
    QueryAll {
        request_id: u64,
        search: String,
        sort: LibrarySort,
    },
    Stats,
    Optimize,
    Duplicates(DedupeMode),
    ClearFingerprints,
    Missing,
    CheckMissing,
    RemoveKeys(Vec<String>),
    ClearMissing,
    RecordPlay(Box<DbTrack>),
    RecordSkip(String),
    Upsert(Vec<DbTrack>),
    Smart {
        kind: SmartKind,
        limit: usize,
    },
    ResetStats,
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

pub struct LibraryDb {
    requests: Sender<DbRequest>,
    events: Receiver<DbEvent>,
    /// Shared with the worker; checked between files while indexing.
    index_cancel: Arc<AtomicBool>,
    _thread: Option<std::thread::JoinHandle<()>>,
    _forward: Option<std::thread::JoinHandle<()>>,
}

impl LibraryDb {
    pub fn open(path: PathBuf) -> Self {
        Self::open_with_wake(path, None)
    }

    /// No worker thread and no database handle. Used by `--bench`/`--self-test`
    /// so those modes do not open or index the user's library.
    pub fn disabled() -> Self {
        let (requests, _request_rx) = unbounded::<DbRequest>();
        let (_event_tx, events) = unbounded::<DbEvent>();
        Self {
            requests,
            events,
            index_cancel: Arc::new(AtomicBool::new(true)),
            _thread: None,
            _forward: None,
        }
    }

    /// `wake` runs after each event is queued so a paused/idle UI thread can
    /// repaint index progress and query results without a polling heartbeat.
    pub fn open_with_wake(
        path: PathBuf,
        wake: Option<Arc<dyn Fn() + Send + Sync>>,
    ) -> Self {
        let (requests, request_rx) = unbounded::<DbRequest>();
        let (raw_tx, raw_events) = unbounded::<DbEvent>();
        let (event_tx, events) = unbounded::<DbEvent>();
        let forward = std::thread::Builder::new()
            .name("bamboo-library-events".to_string())
            .spawn(move || {
                while let Ok(event) = raw_events.recv() {
                    if event_tx.send(event).is_err() {
                        break;
                    }
                    if let Some(wake) = &wake {
                        wake();
                    }
                }
            })
            .expect("library event thread");
        let index_cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = index_cancel.clone();
        let thread = std::thread::Builder::new()
            .name("bamboo-library-db".to_string())
            .spawn(move || {
                let connection = match Connection::open(&path) {
                    Ok(connection) => {
                        // Multiple instances may write concurrently; wait for
                        // the WAL writer instead of failing with "database is
                        // locked".
                        let _ = connection.busy_timeout(std::time::Duration::from_secs(5));
                        if let Err(err) = connection
                            .execute_batch(SCHEMA)
                            .and_then(|_| connection.pragma_update(None, "journal_mode", "WAL"))
                            .and_then(|_| connection.pragma_update(None, "synchronous", "NORMAL"))
                        {
                            tracing::warn!("library db init failed: {err}");
                        }
                        Some(connection)
                    }
                    Err(err) => {
                        tracing::error!("cannot open library db {}: {err}", path.display());
                        let _ = raw_tx
                            .send(DbEvent::Error(crate::trf!("曲库数据库无法打开：{}", err)));
                        None
                    }
                };
                run_worker(connection, request_rx, raw_tx, worker_cancel);
            })
            .expect("library db thread");
        Self {
            requests,
            events,
            index_cancel,
            _thread: Some(thread),
            _forward: Some(forward),
        }
    }

    fn send(&self, request: DbRequest) {
        let _ = self.requests.send(request);
    }

    pub fn index_roots(&self, roots: Vec<PathBuf>, extensions: Vec<String>, follow_links: bool) {
        self.index_cancel.store(false, Ordering::SeqCst);
        self.send(DbRequest::IndexRoots {
            roots,
            extensions,
            follow_links,
        });
    }

    /// Ask the worker to stop the current index run between files. The run
    /// still reports `IndexDone` with `cancelled = true`.
    pub fn cancel_index(&self) {
        self.index_cancel.store(true, Ordering::SeqCst);
    }

    pub fn query(&self, request_id: u64, query: LibraryQuery) {
        self.send(DbRequest::Query { request_id, query });
    }

    /// Query every matching row (no LIMIT/OFFSET) for "add all" actions.
    pub fn query_all(&self, request_id: u64, search: String, sort: LibrarySort) {
        self.send(DbRequest::QueryAll {
            request_id,
            search,
            sort,
        });
    }

    pub fn stats(&self) {
        self.send(DbRequest::Stats);
    }

    /// Compact the index after a large scan or removal. Runs on the worker
    /// thread; `MaintenanceDone` carries the refreshed statistics.
    pub fn optimize(&self) {
        self.send(DbRequest::Optimize);
    }

    pub fn duplicates(&self, mode: DedupeMode) {
        self.send(DbRequest::Duplicates(mode));
    }

    pub fn clear_fingerprints(&self) {
        self.send(DbRequest::ClearFingerprints);
    }

    pub fn missing(&self) {
        self.send(DbRequest::Missing);
    }

    pub fn check_missing(&self) {
        self.send(DbRequest::CheckMissing);
    }

    pub fn remove_keys(&self, keys: Vec<String>) {
        self.send(DbRequest::RemoveKeys(keys));
    }

    pub fn clear_missing(&self) {
        self.send(DbRequest::ClearMissing);
    }

    pub fn record_play(&self, track: DbTrack) {
        self.send(DbRequest::RecordPlay(Box::new(track)));
    }

    pub fn record_skip(&self, key: String) {
        self.send(DbRequest::RecordSkip(key));
    }

    pub fn upsert(&self, tracks: Vec<DbTrack>) {
        if !tracks.is_empty() {
            self.send(DbRequest::Upsert(tracks));
        }
    }

    /// Parse and index only the given files. Used by the directory watcher so
    /// adding one song does not re-walk (and re-parse) the entire library.
    pub fn index_paths(&self, paths: Vec<PathBuf>) {
        if !paths.is_empty() {
            self.send(DbRequest::IndexPaths(paths));
        }
    }

    pub fn smart(&self, kind: SmartKind, limit: usize) {
        self.send(DbRequest::Smart { kind, limit });
    }

    pub fn reset_stats(&self) {
        self.send(DbRequest::ResetStats);
    }

    pub fn poll(&self) -> Option<DbEvent> {
        self.events.try_recv().ok()
    }
}

fn run_worker(
    connection: Option<Connection>,
    requests: Receiver<DbRequest>,
    events: Sender<DbEvent>,
    index_cancel: Arc<AtomicBool>,
) {
    let Some(connection) = connection else {
        // Drain requests so senders never block; every operation is a no-op.
        while requests.recv().is_ok() {}
        return;
    };
    while let Ok(request) = requests.recv() {
        let result = handle(&connection, request, &events, &index_cancel);
        if let Err(err) = result {
            tracing::warn!("library db request failed: {err}");
            let _ = events.send(DbEvent::Error(crate::trf!("曲库操作失败：{}", err)));
        }
    }
}

fn handle(
    conn: &Connection,
    request: DbRequest,
    events: &Sender<DbEvent>,
    index_cancel: &AtomicBool,
) -> rusqlite::Result<()> {
    match request {
        DbRequest::IndexRoots {
            roots,
            extensions,
            follow_links,
        } => {
            let started = Instant::now();
            let (indexed, cancelled) =
                index_roots(conn, &roots, &extensions, follow_links, events, index_cancel)?;
            // The index mirrors the configured folders: a rescan drops rows
            // that no longer live under any of them (including every row when
            // the last folder was removed). Rows under a configured root that
            // is offline stay, so an unplugged drive does not lose history.
            if !cancelled {
                prune_outside_roots(conn, &roots)?;
            }
            let missing = if cancelled { 0 } else { recheck_missing(conn)? };
            let _ = events.send(DbEvent::IndexDone {
                indexed,
                missing,
                elapsed_ms: started.elapsed().as_millis(),
                cancelled,
            });
            let _ = events.send(DbEvent::Changed);
        }
        DbRequest::IndexPaths(paths) => {
            let rows: Vec<DbTrack> = paths
                .par_iter()
                .filter_map(|path| indexed_track(path))
                .collect();
            if !rows.is_empty() {
                let transaction = conn.unchecked_transaction()?;
                for row in &rows {
                    upsert_track(&transaction, row)?;
                }
                transaction.commit()?;
            }
            let _ = events.send(DbEvent::Changed);
        }
        DbRequest::Query { request_id, query } => {
            let (entries, total) = run_query(conn, &query)?;
            let _ = events.send(DbEvent::QueryDone {
                request_id,
                entries,
                total,
            });
        }
        DbRequest::QueryAll {
            request_id,
            search,
            sort,
        } => {
            let entries = run_query_all(conn, &search, sort)?;
            let _ = events.send(DbEvent::AllDone {
                request_id,
                entries,
            });
        }
        DbRequest::Stats => {
            let stats = run_stats(conn)?;
            let _ = events.send(DbEvent::StatsDone(Box::new(stats)));
        }
        DbRequest::Optimize => {
            conn.execute_batch("PRAGMA optimize; VACUUM;")?;
            let stats = run_stats(conn)?;
            let _ = events.send(DbEvent::MaintenanceDone(Box::new(stats)));
        }
        DbRequest::Duplicates(mode) => {
            let groups = run_duplicates(conn, mode)?;
            let _ = events.send(DbEvent::DuplicatesDone(groups));
        }
        DbRequest::ClearFingerprints => {
            conn.execute("DELETE FROM fingerprints", [])?;
            let _ = events.send(DbEvent::Changed);
        }
        DbRequest::Missing => {
            let entries = query_rows(
                conn,
                &format!(
                    "SELECT {COLUMNS} FROM tracks WHERE missing = 1 ORDER BY lower(artist), lower(title)"
                ),
                params![],
            )?;
            let _ = events.send(DbEvent::MissingDone(entries));
        }
        DbRequest::CheckMissing => {
            recheck_missing(conn)?;
            let entries = query_rows(
                conn,
                &format!(
                    "SELECT {COLUMNS} FROM tracks WHERE missing = 1 ORDER BY lower(artist), lower(title)"
                ),
                params![],
            )?;
            let _ = events.send(DbEvent::MissingDone(entries));
            let _ = events.send(DbEvent::Changed);
        }
        DbRequest::RemoveKeys(keys) => {
            let tx = conn.unchecked_transaction()?;
            for key in &keys {
                tx.execute("DELETE FROM tracks WHERE key = ?1", params![key])?;
                tx.execute("DELETE FROM fingerprints WHERE key = ?1", params![key])?;
            }
            tx.commit()?;
            let _ = events.send(DbEvent::Changed);
        }
        DbRequest::ClearMissing => {
            conn.execute(
                "DELETE FROM fingerprints WHERE key IN \
                 (SELECT key FROM tracks WHERE missing = 1)",
                [],
            )?;
            conn.execute("DELETE FROM tracks WHERE missing = 1", [])?;
            let _ = events.send(DbEvent::MissingDone(Vec::new()));
            let _ = events.send(DbEvent::Changed);
        }
        DbRequest::RecordPlay(track) => {
            record_play(conn, &track)?;
            let _ = events.send(DbEvent::Changed);
        }
        DbRequest::RecordSkip(key) => {
            conn.execute(
                "UPDATE tracks SET skip_count = skip_count + 1 WHERE key = ?1",
                params![key],
            )?;
            let _ = events.send(DbEvent::Changed);
        }
        DbRequest::Upsert(tracks) => {
            let tx = conn.unchecked_transaction()?;
            for track in &tracks {
                upsert_track(&tx, track)?;
            }
            tx.commit()?;
        }
        DbRequest::Smart { kind, limit } => {
            let entries = query_rows(conn, &kind.sql(limit.clamp(1, 1000)), params![])?;
            let _ = events.send(DbEvent::SmartDone { kind, entries });
        }
        DbRequest::ResetStats => {
            conn.execute(
                "UPDATE tracks SET play_count = 0, skip_count = 0, last_played_at = NULL",
                [],
            )?;
            let _ = events.send(DbEvent::Changed);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Indexing
// ---------------------------------------------------------------------------

fn index_roots(
    conn: &Connection,
    roots: &[PathBuf],
    extensions: &[String],
    follow_links: bool,
    events: &Sender<DbEvent>,
    cancel: &AtomicBool,
) -> rusqlite::Result<(usize, bool)> {
    // Re-indexing a large library is dominated by tag parsing. Load the
    // existing size/mtime map once, skip unchanged files, parse changed files
    // in parallel with rayon and write each batch in one SQLite transaction.
    let mut existing: HashMap<String, (u64, Option<i64>)> = HashMap::new();
    if let Ok(mut statement) = conn.prepare("SELECT key, size, modified_at FROM tracks") {
        if let Ok(rows) = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?.max(0) as u64,
                row.get::<_, Option<i64>>(2)?,
            ))
        }) {
            for row in rows.flatten() {
                existing.insert(row.0, (row.1, row.2));
            }
        }
    }

    let mut indexed = 0usize;
    const BATCH: usize = 256;
    'roots: for root in roots {
        if cancel.load(Ordering::Relaxed) {
            return Ok((indexed, true));
        }
        if !root.is_dir() {
            continue;
        }
        let mut batch: Vec<PathBuf> = Vec::with_capacity(BATCH);
        for entry in WalkDir::new(root)
            .follow_links(follow_links)
            .sort_by_file_name()
            .into_iter()
            .filter_map(Result::ok)
        {
            if cancel.load(Ordering::Relaxed) {
                break 'roots;
            }
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            if !is_audio_file(path, extensions) {
                continue;
            }
            batch.push(path.to_path_buf());
            if batch.len() >= BATCH {
                process_index_batch(conn, &batch, &mut existing, indexed, events, cancel)?;
                indexed += batch.len();
                batch.clear();
                if cancel.load(Ordering::Relaxed) {
                    return Ok((indexed, true));
                }
            }
        }
        if !batch.is_empty() {
            process_index_batch(conn, &batch, &mut existing, indexed, events, cancel)?;
            indexed += batch.len();
        }
    }
    Ok((indexed, cancel.load(Ordering::Relaxed)))
}

/// Parse one batch in parallel and upsert only new/changed files. Returns the
/// number of rows actually written.
fn process_index_batch(
    conn: &Connection,
    batch: &[PathBuf],
    existing: &mut HashMap<String, (u64, Option<i64>)>,
    so_far: usize,
    events: &Sender<DbEvent>,
    cancel: &AtomicBool,
) -> rusqlite::Result<()> {
    let changed: Vec<PathBuf> = batch
        .par_iter()
        .filter(|path| {
            let Ok(metadata) = std::fs::metadata(path) else {
                return true;
            };
            let key = normalize_path(path);
            match existing.get(&key) {
                Some((size, modified)) => {
                    *size != metadata.len() || *modified != modified_secs(&metadata)
                }
                None => true,
            }
        })
        .cloned()
        .collect();
    let current = changed
        .last()
        .or_else(|| batch.last())
        .and_then(|path| path.parent())
        .map(|path| path.display().to_string())
        .unwrap_or_default();
    if changed.is_empty() || cancel.load(Ordering::Relaxed) {
        if !cancel.load(Ordering::Relaxed) {
            let _ = events.send(DbEvent::IndexProgress {
                indexed: so_far + batch.len(),
                current,
            });
        }
        return Ok(());
    }
    let rows: Vec<DbTrack> = changed
        .par_iter()
        .filter_map(|path| indexed_track(path))
        .collect();
    if rows.is_empty() {
        let _ = events.send(DbEvent::IndexProgress {
            indexed: so_far + batch.len(),
            current,
        });
        return Ok(());
    }
    let transaction = conn.unchecked_transaction()?;
    for row in &rows {
        upsert_track(&transaction, row)?;
    }
    transaction.commit()?;
    for row in &rows {
        existing.insert(row.key.clone(), (row.size, row.modified_at));
    }
    let _ = events.send(DbEvent::IndexProgress {
        indexed: so_far + batch.len(),
        current,
    });
    Ok(())
}

/// Read one file's tags without decoding cover art. Files that cannot be
/// parsed still get a row from their file name so they remain searchable.
fn indexed_track(path: &Path) -> Option<DbTrack> {
    let meta = std::fs::metadata(path).ok();
    let mut track = Track::local(path.to_path_buf());
    if let Some(local) = read_local_with_cover(path, false) {
        if !local.title.trim().is_empty() {
            track.title = local.title;
        }
        if !local.artist.trim().is_empty() {
            track.artist = local.artist;
        }
        track.album = local.album;
        track.genre = local.genre;
        track.track_no = local.track_no;
        track.duration = local.duration;
        let mut row = DbTrack::from_track(&track)?;
        row.bitrate = local.bitrate;
        row.sample_rate = local.sample_rate;
        row.modified_at = meta.as_ref().and_then(modified_secs);
        return Some(row);
    }
    let mut row = DbTrack::from_track(&track)?;
    row.modified_at = meta.as_ref().and_then(modified_secs);
    Some(row)
}

fn upsert_track(conn: &Connection, track: &DbTrack) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO tracks (
            key, path, title, artist, album, genre, year, track_no, duration,
            bitrate, sample_rate, lossless, format, size, added_at, modified_at, missing
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, 0)
         ON CONFLICT(key) DO UPDATE SET
            title = excluded.title,
            artist = excluded.artist,
            album = excluded.album,
            genre = excluded.genre,
            year = excluded.year,
            track_no = excluded.track_no,
            duration = excluded.duration,
            bitrate = excluded.bitrate,
            sample_rate = excluded.sample_rate,
            lossless = excluded.lossless,
            format = excluded.format,
            size = excluded.size,
            modified_at = excluded.modified_at,
            missing = 0",
        params![
            track.key,
            track.path.to_string_lossy(),
            track.title,
            track.artist,
            track.album,
            track.genre,
            track.year,
            track.track_no,
            track.duration,
            track.bitrate,
            track.sample_rate,
            i64::from(track.lossless),
            track.format,
            track.size as i64,
            now(),
            track.modified_at,
        ],
    )?;
    Ok(())
}

/// Playback statistics must not rewrite indexed metadata. The playback Track
/// is lightweight (no duration/bitrate), so a full upsert here used to erase
/// duration and other scan-only fields. Insert the row only when the key is
/// not indexed yet, then bump the counters.
fn record_play(conn: &Connection, track: &DbTrack) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO tracks (
            key, path, title, artist, album, genre, year, track_no, duration,
            bitrate, sample_rate, lossless, format, size, added_at, modified_at, missing
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, 0)",
        params![
            track.key,
            track.path.to_string_lossy(),
            track.title,
            track.artist,
            track.album,
            track.genre,
            track.year,
            track.track_no,
            track.duration,
            track.bitrate,
            track.sample_rate,
            i64::from(track.lossless),
            track.format,
            track.size as i64,
            now(),
            track.modified_at,
        ],
    )?;
    conn.execute(
        "UPDATE tracks SET play_count = play_count + 1, last_played_at = ?2, missing = 0 \
         WHERE key = ?1",
        params![track.key, now()],
    )?;
    Ok(())
}

/// Delete rows whose file is not under any configured library folder.
///
/// `roots` is the configured list, not the walkable one: rows under an
/// offline root are kept (the drive may just be unplugged), while rows from a
/// folder the user removed go away. An empty root list therefore clears the
/// whole index, which is what a rescan should do after removing the last
/// folder.
fn prune_outside_roots(conn: &Connection, roots: &[PathBuf]) -> rusqlite::Result<usize> {
    let normalized: Vec<String> = roots
        .iter()
        .map(|root| {
            let text = root.to_string_lossy();
            let trimmed = text.trim_end_matches(['\\', '/']);
            normalize_path(Path::new(trimmed))
        })
        .collect();
    let mut stale: Vec<String> = Vec::new();
    {
        let mut statement = conn.prepare("SELECT key FROM tracks")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        for key in rows.flatten() {
            let inside = normalized.iter().any(|root| {
                key == *root
                    || key
                        .strip_prefix(root)
                        .is_some_and(|rest| rest.starts_with('\\'))
            });
            if !inside {
                stale.push(key);
            }
        }
    }
    if stale.is_empty() {
        return Ok(0);
    }
    let transaction = conn.unchecked_transaction()?;
    for key in &stale {
        transaction.execute("DELETE FROM tracks WHERE key = ?1", params![key])?;
        transaction.execute("DELETE FROM fingerprints WHERE key = ?1", params![key])?;
    }
    transaction.commit()?;
    Ok(stale.len())
}

fn recheck_missing(conn: &Connection) -> rusqlite::Result<u64> {
    let mut updates: Vec<(String, i64)> = Vec::new();
    {
        let mut stmt = conn.prepare("SELECT key, path, missing FROM tracks")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
        for row in rows {
            let (key, path, missing) = row?;
            let exists = Path::new(&path).is_file();
            let want = if exists { 0 } else { 1 };
            if want != missing {
                updates.push((key, want));
            }
        }
    }
    if !updates.is_empty() {
        let tx = conn.unchecked_transaction()?;
        for (key, missing) in &updates {
            tx.execute(
                "UPDATE tracks SET missing = ?2 WHERE key = ?1",
                params![key, missing],
            )?;
        }
        tx.commit()?;
    }
    let mut stmt = conn.prepare("SELECT COUNT(*) FROM tracks WHERE missing = 1")?;
    let missing = stmt.query_row([], |row| row.get::<_, i64>(0))?.max(0) as u64;
    Ok(missing)
}

// ---------------------------------------------------------------------------
// Queries
// ---------------------------------------------------------------------------

/// WHERE clause shared by the paged and whole-library queries. `?1` is the
/// lower-cased search term; `None` means no search filter.
fn search_clause(search: &str) -> (&'static str, Option<String>) {
    let search = search.trim().to_lowercase();
    if search.is_empty() {
        ("missing = 0", None)
    } else {
        (
            "missing = 0 AND (instr(lower(title), ?1) > 0 \
             OR instr(lower(artist), ?1) > 0 \
             OR instr(lower(album), ?1) > 0)",
            Some(search),
        )
    }
}

fn run_query(
    conn: &Connection,
    query: &LibraryQuery,
) -> rusqlite::Result<(Vec<LibraryEntry>, usize)> {
    let (where_sql, arg) = search_clause(&query.search);
    let limit = query.limit.clamp(1, 2000);
    let offset = query.offset.min(i64::MAX as usize);
    let total: usize = if let Some(value) = &arg {
        conn.query_row(
            &format!("SELECT COUNT(*) FROM tracks WHERE {where_sql}"),
            params![value],
            |row| row.get::<_, i64>(0),
        )?
    } else {
        conn.query_row(
            &format!("SELECT COUNT(*) FROM tracks WHERE {where_sql}"),
            params![],
            |row| row.get::<_, i64>(0),
        )?
    }
    .max(0) as usize;
    let sql = format!(
        "SELECT {COLUMNS} FROM tracks WHERE {where_sql} ORDER BY {} LIMIT {limit} OFFSET {offset}",
        query.sort.order_by()
    );
    let entries = if let Some(value) = arg {
        query_rows(conn, &sql, params![value])?
    } else {
        query_rows(conn, &sql, params![])?
    };
    Ok((entries, total))
}

/// Same filter and order as [`run_query`], but every matching row.
fn run_query_all(
    conn: &Connection,
    search: &str,
    sort: LibrarySort,
) -> rusqlite::Result<Vec<LibraryEntry>> {
    let (where_sql, arg) = search_clause(search);
    let sql = format!(
        "SELECT {COLUMNS} FROM tracks WHERE {where_sql} ORDER BY {}",
        sort.order_by()
    );
    if let Some(value) = arg {
        query_rows(conn, &sql, params![value])
    } else {
        query_rows(conn, &sql, params![])
    }
}

fn query_rows<P: rusqlite::Params>(
    conn: &Connection,
    sql: &str,
    params: P,
) -> rusqlite::Result<Vec<LibraryEntry>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params, row_to_entry)?;
    rows.collect()
}

fn row_to_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<LibraryEntry> {
    Ok(LibraryEntry {
        key: row.get(0)?,
        path: PathBuf::from(row.get::<_, String>(1)?),
        title: row.get(2)?,
        artist: row.get(3)?,
        album: row.get(4)?,
        duration: row.get(5)?,
        play_count: row.get(6)?,
        last_played_at: row.get(7)?,
        lossless: row.get::<_, i64>(8)? != 0,
        format: row.get(9)?,
        missing: row.get::<_, i64>(10)? != 0,
        size: row.get::<_, i64>(11)?.max(0) as u64,
    })
}

fn run_stats(conn: &Connection) -> rusqlite::Result<LibraryStats> {
    let mut stats = LibraryStats::default();
    let mut stmt = conn.prepare(
        "SELECT COUNT(*), COUNT(DISTINCT lower(artist)), COUNT(DISTINCT lower(album)), \
                COALESCE(SUM(duration), 0), COALESCE(SUM(play_count), 0), \
                COALESCE(SUM(lossless), 0), COALESCE(SUM(missing), 0), COALESCE(SUM(size), 0) \
         FROM tracks",
    )?;
    stmt.query_row([], |row| {
        stats.tracks = row.get::<_, i64>(0)?.max(0) as u64;
        stats.artists = row.get::<_, i64>(1)?.max(0) as u64;
        stats.albums = row.get::<_, i64>(2)?.max(0) as u64;
        stats.total_secs = row.get(3)?;
        stats.total_plays = row.get::<_, i64>(4)?.max(0) as u64;
        stats.lossless = row.get::<_, i64>(5)?.max(0) as u64;
        stats.missing = row.get::<_, i64>(6)?.max(0) as u64;
        stats.total_bytes = row.get::<_, i64>(7)?.max(0) as u64;
        Ok(())
    })?;

    let mut stmt = conn.prepare(
        "SELECT MAX(artist), SUM(play_count) AS plays FROM tracks \
         WHERE play_count > 0 AND artist <> '' GROUP BY lower(artist) \
         ORDER BY plays DESC LIMIT 8",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?.max(0) as u64,
        ))
    })?;
    stats.top_artists = rows.collect::<rusqlite::Result<Vec<_>>>()?;

    let mut stmt = conn.prepare(
        "SELECT title, artist, play_count FROM tracks \
         WHERE play_count > 0 ORDER BY play_count DESC, last_played_at DESC LIMIT 10",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?.max(0) as u64,
        ))
    })?;
    stats.top_tracks = rows.collect::<rusqlite::Result<Vec<_>>>()?;

    let mut stmt = conn.prepare(
        "SELECT format, COUNT(*) FROM tracks GROUP BY format ORDER BY COUNT(*) DESC LIMIT 12",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?.max(0) as u64,
        ))
    })?;
    stats.formats = rows.collect::<rusqlite::Result<Vec<_>>>()?;

    stats.recently_played = query_rows(
        conn,
        &format!(
            "SELECT {COLUMNS} FROM tracks WHERE last_played_at IS NOT NULL \
             ORDER BY last_played_at DESC LIMIT 8"
        ),
        params![],
    )?;
    Ok(stats)
}

fn run_duplicates(conn: &Connection, mode: DedupeMode) -> rusqlite::Result<Vec<DuplicateGroup>> {
    match mode {
        DedupeMode::Metadata => run_duplicates_metadata(conn),
        DedupeMode::Path => run_duplicates_path(conn),
        DedupeMode::Fingerprint => run_duplicates_fingerprint(conn),
    }
}

fn run_duplicates_metadata(conn: &Connection) -> rusqlite::Result<Vec<DuplicateGroup>> {
    // Title + artist is the cheap heuristic; durations within one second are
    // compared when building the group so different songs with the same name
    // are not merged.
    let mut stmt = conn.prepare(
        "SELECT lower(trim(title)), lower(trim(artist)), COUNT(*) AS dupes \
         FROM tracks WHERE missing = 0 AND trim(title) <> '' \
         GROUP BY 1, 2 HAVING dupes > 1 ORDER BY dupes DESC LIMIT 200",
    )?;
    let keys = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut groups = Vec::new();
    for (title_lc, artist_lc) in keys {
        let entries = query_rows(
            conn,
            &format!(
                "SELECT {COLUMNS} FROM tracks WHERE missing = 0 \
                 AND lower(trim(title)) = ?1 AND lower(trim(artist)) = ?2 \
                 ORDER BY size DESC, lower(path)"
            ),
            params![title_lc, artist_lc],
        )?;
        if entries.len() > 1 {
            let first = entries[0].clone();
            let mut buckets: Vec<DuplicateGroup> = Vec::new();
            for entry in entries {
                if let Some(group) = buckets.iter_mut().find(|group| {
                    let base = group.entries[0].duration.unwrap_or(0.0);
                    let d = entry.duration.unwrap_or(0.0);
                    (base - d).abs() <= 1.5
                }) {
                    group.entries.push(entry);
                } else {
                    buckets.push(DuplicateGroup {
                        title: first.title.clone(),
                        artist: first.artist.clone(),
                        entries: vec![entry],
                    });
                }
            }
            groups.extend(buckets.into_iter().filter(|group| group.entries.len() > 1));
        }
    }
    Ok(groups)
}

fn run_duplicates_path(conn: &Connection) -> rusqlite::Result<Vec<DuplicateGroup>> {
    let entries = query_rows(
        conn,
        &format!("SELECT {COLUMNS} FROM tracks WHERE missing = 0 ORDER BY size DESC, lower(path)"),
        params![],
    )?;
    let mut buckets: HashMap<String, Vec<LibraryEntry>> = HashMap::new();
    for entry in entries {
        // Symlinks/case-only differences canonicalize to the same path. The
        // normalized fallback still groups entries whose files disappeared.
        let identity = std::fs::canonicalize(&entry.path)
            .map(|path| format!("canon:{}", path.to_string_lossy().to_lowercase()))
            .unwrap_or_else(|_| crate::model::normalize_path(&entry.path));
        buckets.entry(identity).or_default().push(entry);
    }
    Ok(buckets
        .into_values()
        .filter(|entries| entries.len() > 1)
        .map(|entries| DuplicateGroup {
            title: entries[0].title.clone(),
            artist: entries[0].artist.clone(),
            entries,
        })
        .collect())
}

fn run_duplicates_fingerprint(conn: &Connection) -> rusqlite::Result<Vec<DuplicateGroup>> {
    let entries = query_rows(
        conn,
        &format!("SELECT {COLUMNS} FROM tracks WHERE missing = 0 ORDER BY size DESC, lower(path)"),
        params![],
    )?;
    let mut fingerprints: Vec<Option<crate::audio::fingerprint::Fingerprint>> =
        Vec::with_capacity(entries.len());
    for entry in &entries {
        fingerprints.push(fingerprint_for_entry(conn, entry));
    }

    let mut parent: Vec<usize> = (0..entries.len()).collect();
    for i in 0..entries.len() {
        let Some(first) = fingerprints[i].as_ref() else {
            continue;
        };
        for j in (i + 1)..entries.len() {
            let Some(second) = fingerprints[j].as_ref() else {
                continue;
            };
            if let (Some(a), Some(b)) = (entries[i].duration, entries[j].duration)
                && (a - b).abs() > 8.0
            {
                continue;
            }
            // SimHash is the cheap prefilter; only plausible pairs pay for the
            // exact Jaccard comparison.
            if first.hamming(second) > 20 {
                continue;
            }
            let common = first.common(second);
            if common >= 8 && first.similarity(second) >= 0.5 {
                union_find(&mut parent, i, j);
            }
        }
    }

    let mut buckets: HashMap<usize, Vec<LibraryEntry>> = HashMap::new();
    for (index, entry) in entries.into_iter().enumerate() {
        let root = find_root(&mut parent, index);
        buckets.entry(root).or_default().push(entry);
    }
    let mut groups: Vec<DuplicateGroup> = buckets
        .into_values()
        .filter(|entries| entries.len() > 1)
        .map(|entries| DuplicateGroup {
            title: entries[0].title.clone(),
            artist: entries[0].artist.clone(),
            entries,
        })
        .collect();
    groups.sort_by(|a, b| b.entries.len().cmp(&a.entries.len()));
    Ok(groups)
}

fn fingerprint_for_entry(
    conn: &Connection,
    entry: &LibraryEntry,
) -> Option<crate::audio::fingerprint::Fingerprint> {
    let metadata = std::fs::metadata(&entry.path).ok()?;
    let size = metadata.len();
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    if let Some(cached) = load_fingerprint(conn, &entry.key, size, modified) {
        return Some(cached);
    }
    match crate::audio::fingerprint::compute(&entry.path) {
        Ok(fingerprint) => {
            let bytes: Vec<u8> = fingerprint
                .hashes
                .iter()
                .flat_map(|hash| hash.to_le_bytes())
                .collect();
            let _ = conn.execute(
                "INSERT INTO fingerprints(key, size, modified, simhash, hashes) \
                 VALUES(?1, ?2, ?3, ?4, ?5) \
                 ON CONFLICT(key) DO UPDATE SET \
                 size = excluded.size, modified = excluded.modified, \
                 simhash = excluded.simhash, hashes = excluded.hashes",
                params![
                    &entry.key,
                    size as i64,
                    modified,
                    fingerprint.simhash as i64,
                    bytes
                ],
            );
            Some(fingerprint)
        }
        Err(err) => {
            tracing::debug!("fingerprint skipped for {}: {err}", entry.path.display());
            None
        }
    }
}

fn load_fingerprint(
    conn: &Connection,
    key: &str,
    size: u64,
    modified: i64,
) -> Option<crate::audio::fingerprint::Fingerprint> {
    let (simhash, bytes): (i64, Vec<u8>) = conn
        .query_row(
            "SELECT simhash, hashes FROM fingerprints \
             WHERE key = ?1 AND size = ?2 AND modified = ?3",
            params![key, size as i64, modified],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .ok()?;
    let hashes = bytes
        .chunks_exact(4)
        .map(|chunk| u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect();
    Some(crate::audio::fingerprint::Fingerprint::from_parts(
        simhash as u64,
        hashes,
    ))
}

fn find_root(parent: &mut [usize], mut index: usize) -> usize {
    while parent[index] != index {
        parent[index] = parent[parent[index]];
        index = parent[index];
    }
    index
}

fn union_find(parent: &mut [usize], a: usize, b: usize) {
    let root_a = find_root(parent, a);
    let root_b = find_root(parent, b);
    if root_a != root_b {
        parent[root_b] = root_a;
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn format_of(path: &Path) -> String {
    path.extension()
        .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

fn modified_secs(metadata: &std::fs::Metadata) -> Option<i64> {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        conn
    }

    fn track(key: &str, title: &str, artist: &str, plays: i64, added: i64) -> DbTrack {
        let _ = (plays, added);
        DbTrack {
            key: key.to_string(),
            path: PathBuf::from(format!("C:\\music\\{key}.mp3")),
            title: title.to_string(),
            artist: artist.to_string(),
            album: "Album".to_string(),
            genre: None,
            year: None,
            track_no: None,
            duration: Some(180.0),
            bitrate: Some(320),
            sample_rate: Some(44_100),
            lossless: false,
            format: "mp3".to_string(),
            size: 1024,
            modified_at: None,
        }
    }

    fn seed(conn: &Connection, track: &DbTrack, plays: i64, added: i64, last_played: Option<i64>) {
        upsert_track(conn, track).unwrap();
        conn.execute(
            "UPDATE tracks SET play_count = ?2, added_at = ?3, last_played_at = ?4 WHERE key = ?1",
            params![track.key, plays, added, last_played],
        )
        .unwrap();
    }

    #[test]
    fn record_play_keeps_indexed_metadata() {
        let conn = memory_db();
        let indexed = track("a", "Title", "Artist", 0, 0);
        seed(&conn, &indexed, 0, 0, None);

        // Playback tracks carry no duration/bitrate; recording a play must not
        // erase the values written by the scanner.
        let mut played = indexed.clone();
        played.duration = None;
        played.bitrate = None;
        played.sample_rate = None;
        record_play(&conn, &played).unwrap();

        let (duration, bitrate, plays): (Option<f64>, Option<u32>, i64) = conn
            .query_row(
                "SELECT duration, bitrate, play_count FROM tracks WHERE key = ?1",
                params!["a"],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(duration, Some(180.0));
        assert_eq!(bitrate, Some(320));
        assert_eq!(plays, 1);
    }

    #[test]
    fn index_reports_cancellation_and_keeps_partial_rows() {
        let dir = tempfile::tempdir().unwrap();
        for index in 0..3 {
            std::fs::write(dir.path().join(format!("track{index}.mp3")), b"").unwrap();
        }
        let conn = memory_db();
        let (events, _rx) = unbounded();
        let extensions = vec!["mp3".to_string()];

        let cancel = AtomicBool::new(false);
        let (indexed, cancelled) = index_roots(
            &conn,
            &[dir.path().to_path_buf()],
            &extensions,
            false,
            &events,
            &cancel,
        )
        .unwrap();
        assert_eq!(indexed, 3);
        assert!(!cancelled);

        let cancel = AtomicBool::new(true);
        let (indexed, cancelled) = index_roots(
            &conn,
            &[dir.path().to_path_buf()],
            &extensions,
            false,
            &events,
            &cancel,
        )
        .unwrap();
        assert_eq!(indexed, 0);
        assert!(cancelled);

        let rows: i64 = conn
            .query_row("SELECT count(*) FROM tracks", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 3, "a cancelled run must not remove existing rows");
    }

    #[test]
    fn rescan_prunes_rows_outside_the_configured_roots() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("keep.mp3"), b"").unwrap();
        let conn = memory_db();
        let (events, _rx) = unbounded();
        let extensions = vec!["mp3".to_string()];
        let cancel = AtomicBool::new(false);
        index_roots(
            &conn,
            &[dir.path().to_path_buf()],
            &extensions,
            false,
            &events,
            &cancel,
        )
        .unwrap();
        // A row left over from a folder that is no longer configured.
        seed(
            &conn,
            &track("local:e:\\old\\gone.mp3", "Gone", "X", 0, 0),
            0,
            0,
            None,
        );
        let pruned = prune_outside_roots(&conn, &[dir.path().to_path_buf()]).unwrap();
        assert_eq!(pruned, 1);
        let rows: i64 = conn
            .query_row("SELECT count(*) FROM tracks", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 1, "rows under the configured folder stay indexed");

        // Removing the last folder and rescanning clears the whole index.
        let pruned = prune_outside_roots(&conn, &[]).unwrap();
        assert_eq!(pruned, 1);
        let rows: i64 = conn
            .query_row("SELECT count(*) FROM tracks", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 0);
    }

    #[test]
    fn search_and_sort_round_trip() {
        let conn = memory_db();
        seed(&conn, &track("a", "Hello", "Adele", 3, 100), 3, 100, None);
        seed(
            &conn,
            &track("b", "Hello Again", "Other", 0, 200),
            0,
            200,
            None,
        );

        let (entries, total) = run_query(
            &conn,
            &LibraryQuery {
                search: "hello".to_string(),
                sort: LibrarySort::PlayCount,
                offset: 0,
                limit: 10,
            },
        )
        .unwrap();
        assert_eq!(total, 2);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].key, "a");
    }

    #[test]
    fn stats_and_duplicates() {
        let conn = memory_db();
        seed(&conn, &track("a", "Song", "Artist", 2, 1), 2, 1, None);
        seed(&conn, &track("b", "Song", "Artist", 0, 2), 0, 2, None);
        seed(&conn, &track("c", "Different", "Artist", 0, 3), 0, 3, None);

        let stats = run_stats(&conn).unwrap();
        assert_eq!(stats.tracks, 3);
        assert_eq!(stats.artists, 1);
        assert_eq!(stats.total_plays, 2);
        assert_eq!(stats.top_tracks[0].0, "Song");

        let groups = run_duplicates(&conn, DedupeMode::Metadata).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].entries.len(), 2);
    }

    #[test]
    fn path_mode_groups_case_insensitive_duplicates() {
        let conn = memory_db();
        let mut a = track("a", "One", "Artist", 0, 1);
        a.path = PathBuf::from("C:\\music\\Same.mp3");
        let mut b = track("b", "Two", "Artist", 0, 2);
        b.path = PathBuf::from("c:\\MUSIC\\same.MP3");
        seed(&conn, &a, 0, 1, None);
        seed(&conn, &b, 0, 2, None);
        let groups = run_duplicates(&conn, DedupeMode::Path).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].entries.len(), 2);
    }

    #[test]
    fn fingerprint_mode_groups_identical_audio() {
        let dir = tempfile::tempdir().unwrap();
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 22_050,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let make = |name: &str| {
            let path = dir.path().join(name);
            let mut writer = hound::WavWriter::create(&path, spec).unwrap();
            for index in 0..44_100 {
                let t = index as f32 / 22_050.0;
                let value = (std::f32::consts::TAU * 440.0 * t).sin() * 0.2
                    + (std::f32::consts::TAU * 997.0 * t).sin() * 0.2
                    + (std::f32::consts::TAU * 2530.0 * t).sin() * 0.2;
                writer.write_sample((value * 32767.0) as i16).unwrap();
            }
            writer.finalize().unwrap();
            path
        };
        let first = make("one.wav");
        let second = make("two.wav");
        let first_fp = crate::audio::fingerprint::compute(&first).unwrap();
        let second_fp = crate::audio::fingerprint::compute(&second).unwrap();
        assert_eq!(first_fp.simhash, second_fp.simhash);
        assert!(first_fp.common(&second_fp) >= 8);
        let conn = memory_db();
        let mut a = track("a", "Song", "Artist", 0, 1);
        a.path = first;
        let mut b = track("b", "Song copy", "Artist", 0, 2);
        b.path = second;
        seed(&conn, &a, 0, 1, None);
        seed(&conn, &b, 0, 2, None);
        let groups = run_duplicates(&conn, DedupeMode::Fingerprint).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].entries.len(), 2);
        // Second run uses the cached fingerprint row.
        let groups = run_duplicates(&conn, DedupeMode::Fingerprint).unwrap();
        assert_eq!(groups.len(), 1);
    }

    #[test]
    fn smart_queries_respect_play_counts() {
        let conn = memory_db();
        seed(
            &conn,
            &track("a", "Played", "Artist", 1, 10),
            1,
            10,
            Some(100),
        );
        seed(&conn, &track("b", "Unplayed", "Artist", 0, 20), 0, 20, None);

        let entries = query_rows(&conn, &SmartKind::NeverPlayed.sql(10), params![]).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].key, "b");
        let entries = query_rows(&conn, &SmartKind::RecentlyPlayed.sql(10), params![]).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].key, "a");
    }
}
