//! Persistent cache for online search pages.
//!
//! Search results are small and change slowly, so keeping the last N pages on
//! disk gives offline users something to browse and avoids repeating identical
//! queries after a restart. Entries expire after a week and are capped so the
//! file cannot grow without bound.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::api::ApiSong;

const CACHE_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const MAX_ENTRIES: usize = 80;

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
}
