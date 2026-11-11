//! Persistent cache for online search pages.
//!
//! Search results are small and change slowly, so keeping the last N pages on
//! disk gives offline users something to browse and avoids repeating identical
//! queries after a restart. Entries expire after a week and are capped so the
//! file cannot grow without bound.
//!
//! The same module also owns stream-cache maintenance: a completed online
//! track is written to `stream/*.stream`, older entries are evicted by
//! last-use time, and expired entries are removed. Pruning runs off the UI
//! thread through [`StreamCacheService`].

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crossbeam_channel::{Receiver, Sender, unbounded};
use serde::{Deserialize, Serialize};

use crate::api::ApiSong;

const CACHE_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const MAX_ENTRIES: usize = 80;
/// Stream-cache entries unused for this long are removed even below the size
/// limit. Online tokens expire, so very old copies are rarely usable.
pub const STREAM_CACHE_TTL_SECS: u64 = 30 * 24 * 60 * 60;
/// Abandoned stream-cache partials are never resumable; delete them after an
/// hour so a crashed player cannot leave them forever.
const STREAM_CACHE_PART_TTL_SECS: u64 = 60 * 60;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CachedSearch {
    pub key: String,
    pub saved_at: i64,
    pub total: u64,
    /// Album/artist title returned with the page, if any.
    #[serde(default)]
    pub name: Option<String>,
    pub items: Vec<ApiSong>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SearchCache {
    #[serde(default)]
    entries: Vec<CachedSearch>,
}

impl SearchCache {
    pub fn load(path: &Path) -> Self {
        let Ok(bytes) = std::fs::read(path) else {
            return Self::default();
        };
        let mut cache: Self = serde_json::from_slice(&bytes).unwrap_or_default();
        cache.prune();
        cache
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let bytes = serde_json::to_vec(self).map_err(|err| err.to_string())?;
        std::fs::write(path, bytes).map_err(|err| err.to_string())
    }

    pub fn get(&self, key: &str) -> Option<&CachedSearch> {
        let now = now();
        self.entries
            .iter()
            .find(|entry| entry.key == key && now - entry.saved_at < CACHE_TTL.as_secs() as i64)
    }

    pub fn insert(&mut self, key: String, total: u64, name: Option<String>, items: Vec<ApiSong>) {
        self.entries.retain(|entry| entry.key != key);
        self.entries.push(CachedSearch {
            key,
            saved_at: now(),
            total,
            name,
            items,
        });
        self.prune();
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    fn prune(&mut self) {
        let now = now();
        self.entries
            .retain(|entry| now - entry.saved_at < CACHE_TTL.as_secs() as i64);
        if self.entries.len() > MAX_ENTRIES {
            self.entries.sort_by_key(|entry| entry.saved_at);
            let remove = self.entries.len() - MAX_ENTRIES;
            self.entries.drain(..remove);
        }
    }
}

/// Cache key for one search page. The base URL is included so editing a
/// source does not serve results from the previous endpoint.
pub fn search_key(
    source_id: &str,
    base_url: &str,
    kind: &str,
    query: &str,
    page: u32,
) -> String {
    page_key(source_id, base_url, kind, &query.trim().to_lowercase(), page)
}

/// Cache key for one album detail page.
pub fn album_key(source_id: &str, base_url: &str, album_id: &str, page: u32) -> String {
    page_key(source_id, base_url, "album-detail", album_id, page)
}

/// Cache key for one artist detail page.
pub fn artist_key(source_id: &str, base_url: &str, artist_id: &str, page: u32) -> String {
    page_key(source_id, base_url, "artist-detail", artist_id, page)
}

fn page_key(source_id: &str, base_url: &str, kind: &str, value: &str, page: u32) -> String {
    format!(
        "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
        source_id, base_url, kind, value, page
    )
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

/// Size and file count of the stream cache, shown in settings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheUsage {
    pub files: usize,
    pub bytes: u64,
}

#[derive(Clone, Debug)]
pub enum CacheEvent {
    Usage {
        dir: PathBuf,
        usage: CacheUsage,
    },
}

enum CacheJob {
    /// Measure the directory; `prune` also applies the TTL/size policy.
    Refresh {
        dir: PathBuf,
        limit_mb: u64,
        ttl_secs: u64,
        prune: bool,
    },
    Clear {
        dir: PathBuf,
    },
}

/// Off-thread stream-cache maintenance. Directory scans never run on the UI
/// thread; the worker reports usage back through a channel.
pub struct StreamCacheService {
    jobs: Sender<CacheJob>,
    events: Receiver<CacheEvent>,
    _thread: std::thread::JoinHandle<()>,
}

impl StreamCacheService {
    pub fn new() -> Self {
        Self::with_wake(None)
    }

    pub fn with_wake(wake: Option<Arc<dyn Fn() + Send + Sync>>) -> Self {
        let (jobs, job_rx) = unbounded::<CacheJob>();
        let (event_tx, events) = unbounded();
        let thread = std::thread::Builder::new()
            .name("bamboo-cache".to_string())
            .spawn(move || {
                while let Ok(job) = job_rx.recv() {
                    let (dir, usage) = match job {
                        CacheJob::Refresh {
                            dir,
                            limit_mb,
                            ttl_secs,
                            prune,
                        } => {
                            let usage = if prune {
                                prune_stream_cache(&dir, limit_mb, ttl_secs)
                            } else {
                                stream_cache_usage(&dir)
                            };
                            (dir, usage)
                        }
                        CacheJob::Clear { dir } => {
                            clear_stream_cache(&dir);
                            let usage = stream_cache_usage(&dir);
                            (dir, usage)
                        }
                    };
                    let _ = event_tx.send(CacheEvent::Usage { dir, usage });
                    if let Some(wake) = wake.as_ref() {
                        wake();
                    }
                }
            })
            .expect("cache thread");
        Self {
            jobs,
            events,
            _thread: thread,
        }
    }

    pub fn refresh(
        &self,
        dir: PathBuf,
        limit_mb: u64,
        ttl_secs: u64,
        prune: bool,
    ) {
        let _ = self.jobs.send(CacheJob::Refresh {
            dir,
            limit_mb,
            ttl_secs,
            prune,
        });
    }

    pub fn clear(&self, dir: PathBuf) {
        let _ = self.jobs.send(CacheJob::Clear { dir });
    }

    pub fn poll(&self) -> Option<CacheEvent> {
        self.events.try_recv().ok()
    }
}

impl Default for StreamCacheService {
    fn default() -> Self {
        Self::new()
    }
}

/// Count `.stream` entries. Partials are excluded from the reported size
/// because they are temporary.
pub fn stream_cache_usage(dir: &Path) -> CacheUsage {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return CacheUsage::default();
    };
    let mut usage = CacheUsage::default();
    for entry in entries.flatten() {
        let path = entry.path();
        if !is_stream_entry(&path) {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.is_file() {
            usage.files += 1;
            usage.bytes = usage.bytes.saturating_add(meta.len());
        }
    }
    usage
}

/// Remove expired entries, then evict least-recently-used files until the
/// directory fits `limit_mb` (0 = no size limit). Returns the surviving usage.
pub fn prune_stream_cache(dir: &Path, limit_mb: u64, ttl_secs: u64) -> CacheUsage {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return CacheUsage::default();
    };
    let now = SystemTime::now();
    let mut files: Vec<(PathBuf, u64, SystemTime)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let modified = meta.modified().unwrap_or(UNIX_EPOCH);
        let age = now.duration_since(modified).unwrap_or_default().as_secs();
        if is_stream_partial(&path) {
            if age > STREAM_CACHE_PART_TTL_SECS {
                let _ = std::fs::remove_file(&path);
            }
            continue;
        }
        if !is_stream_entry(&path) {
            continue;
        }
        if ttl_secs > 0 && age > ttl_secs {
            let _ = std::fs::remove_file(&path);
            continue;
        }
        files.push((path, meta.len(), modified));
    }
    if limit_mb > 0 {
        let limit = limit_mb.saturating_mul(1024 * 1024);
        let mut total: u64 = files.iter().map(|(_, len, _)| *len).sum();
        if total > limit {
            files.sort_by_key(|(_, _, modified)| *modified);
            for (path, len, _) in files {
                if total <= limit {
                    break;
                }
                if std::fs::remove_file(&path).is_ok() {
                    total = total.saturating_sub(len);
                }
            }
        }
    }
    stream_cache_usage(dir)
}

/// Delete every stream-cache entry. Used by "清理缓存".
pub fn clear_stream_cache(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if is_stream_entry(&path) || is_stream_partial(&path) {
            let _ = std::fs::remove_file(&path);
        }
    }
}

fn is_stream_entry(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("stream"))
}

fn is_stream_partial(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("part"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song(id: &str) -> ApiSong {
        ApiSong {
            id: id.to_string(),
            name: format!("Song {id}"),
            ..Default::default()
        }
    }

    #[test]
    fn inserts_replaces_and_evicts_entries() {
        let mut cache = SearchCache::default();
        cache.insert("a".to_string(), 10, None, vec![song("1")]);
        cache.insert(
            "a".to_string(),
            11,
            Some("Album".to_string()),
            vec![song("2")],
        );
        assert_eq!(cache.get("a").unwrap().total, 11);
        assert_eq!(cache.get("a").unwrap().name.as_deref(), Some("Album"));
        assert_eq!(cache.get("a").unwrap().items[0].id, "2");

        for index in 0..(MAX_ENTRIES + 10) {
            cache.insert(format!("k{index}"), 1, None, Vec::new());
        }
        assert!(cache.entries.len() <= MAX_ENTRIES);
    }

    #[test]
    fn cache_round_trips_through_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("search.json");
        let mut cache = SearchCache::default();
        cache.insert("q".to_string(), 3, None, vec![song("7")]);
        cache.save(&path).unwrap();
        let loaded = SearchCache::load(&path);
        assert_eq!(loaded.get("q").unwrap().items[0].id, "7");
    }

    #[test]
    fn stream_cache_prunes_old_and_oversized_entries() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("old.stream");
        let keep = dir.path().join("keep.stream");
        let other = dir.path().join("notes.txt");
        std::fs::write(&old, vec![0u8; 1024]).unwrap();
        std::fs::write(&keep, vec![0u8; 1024]).unwrap();
        std::fs::write(&other, b"x").unwrap();
        // Age the first entry beyond the TTL.
        let stale = SystemTime::now() - Duration::from_secs(STREAM_CACHE_TTL_SECS + 60);
        set_modified(&old, stale);

        let usage = prune_stream_cache(dir.path(), 0, STREAM_CACHE_TTL_SECS);
        assert!(!old.exists(), "expired entry must be removed");
        assert!(keep.exists());
        assert!(other.exists(), "non-cache files are never touched");
        assert_eq!(usage.files, 1);
    }

    #[test]
    fn stream_cache_size_limit_evicts_the_oldest_file() {
        let dir = tempfile::tempdir().unwrap();
        let older = dir.path().join("older.stream");
        let newer = dir.path().join("newer.stream");
        std::fs::write(&older, vec![0u8; 600 * 1024]).unwrap();
        std::fs::write(&newer, vec![0u8; 600 * 1024]).unwrap();
        set_modified(&older, SystemTime::now() - Duration::from_secs(3_600));
        set_modified(&newer, SystemTime::now() - Duration::from_secs(60));

        let usage = prune_stream_cache(dir.path(), 1, STREAM_CACHE_TTL_SECS);
        assert!(!older.exists(), "LRU entry should be evicted");
        assert!(newer.exists());
        assert_eq!(usage.files, 1);
    }

    #[test]
    fn clear_removes_only_stream_entries() {
        let dir = tempfile::tempdir().unwrap();
        let stream = dir.path().join("a.stream");
        let part = dir.path().join("a.part");
        let other = dir.path().join("keep.txt");
        for path in [&stream, &part, &other] {
            std::fs::write(path, b"x").unwrap();
        }
        clear_stream_cache(dir.path());
        assert!(!stream.exists());
        assert!(!part.exists());
        assert!(other.exists());
    }

    fn set_modified(path: &Path, time: SystemTime) {
        let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        file.set_modified(time).unwrap();
    }
}
