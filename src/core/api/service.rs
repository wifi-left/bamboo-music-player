//! Background worker for online search/info/resolve requests.
//!
//! Network calls must never run on the UI thread: a slow or unreachable source
//! would freeze every frame. This service owns a single worker thread and
//! returns typed results through a crossbeam channel. Requests carry a
//! monotonically increasing id so stale search results can be dropped when the
//! user types another query.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, unbounded};

use crate::api::{ApiClient, ApiResult, SearchKind, SearchPage, SongInfo};
use crate::config::{NetworkSettings, Quality, SourceConfig};
use crate::model::Track;

#[derive(Clone, Debug)]
pub struct PreparedPlay {
    pub url: String,
    pub info: Option<crate::api::ApiSong>,
    pub lrc: Option<String>,
}

#[derive(Clone, Debug)]
pub enum ApiEvent {
    Search {
        id: u64,
        kind: SearchKind,
        page: ApiResult<SearchPage>,
    },
    Album {
        id: u64,
        album_id: String,
        page: ApiResult<SearchPage>,
    },
    Artist {
        id: u64,
        artist_id: String,
        page: ApiResult<SearchPage>,
    },
    Suggest {
        id: u64,
        query: String,
        suggestions: ApiResult<Vec<String>>,
    },
    Resolve {
        id: u64,
        track_key: String,
        cache: Option<PathBuf>,
        start_secs: f64,
        autoplay: bool,
        result: ApiResult<PreparedPlay>,
    },
    /// Background resolve for the track the engine may pre-arm next. It never
    /// touches the current track's metadata or lyrics.
    NextResolved {
        id: u64,
        track_key: String,
        cache: Option<PathBuf>,
        result: ApiResult<PreparedPlay>,
    },
    Info {
        id: u64,
        result: ApiResult<SongInfo>,
    },
    Random {
        id: u64,
        page: ApiResult<SearchPage>,
    },
    Test {
        source_id: String,
        result: ApiResult<String>,
    },
}

enum Job {
    Search {
        id: u64,
        kind: SearchKind,
        client: ApiClient,
        keyword: String,
        offset: u32,
        limit: u32,
    },
    Album {
        id: u64,
        client: ApiClient,
        album_id: String,
        offset: u32,
        limit: u32,
    },
    Artist {
        id: u64,
        client: ApiClient,
        artist_id: String,
        offset: u32,
        limit: u32,
    },
    Suggest {
        id: u64,
        client: ApiClient,
        query: String,
    },
    Resolve {
        id: u64,
        client: ApiClient,
        track_key: String,
        song_id: String,
        quality: Quality,
        cache: Option<PathBuf>,
        start_secs: f64,
        autoplay: bool,
    },
    ResolveNext {
        id: u64,
        client: ApiClient,
        track_key: String,
        song_id: String,
        cache: Option<PathBuf>,
    },
    Info {
        id: u64,
        client: ApiClient,
        song_id: String,
    },
    Random {
        id: u64,
        client: ApiClient,
        count: u32,
        seed: u64,
    },
    Test {
        source_id: String,
        client: ApiClient,
    },
}

pub struct ApiService {
    jobs: Sender<Job>,
    events: Receiver<ApiEvent>,
    next_id: u64,
    limiter: Arc<RateLimiter>,
    // Newest request id per kind. The worker skips queued jobs that were
    // superseded while the user typed, paged or selected another track, so a
    // single slow server is not hit by a backlog of obsolete requests.
    latest_search: Arc<AtomicU64>,
    latest_suggest: Arc<AtomicU64>,
    latest_album: Arc<AtomicU64>,
    latest_artist: Arc<AtomicU64>,
    latest_resolve: Arc<AtomicU64>,
    latest_next: Arc<AtomicU64>,
    latest_info: Arc<AtomicU64>,
    _thread: std::thread::JoinHandle<()>,
}

/// Minimum spacing between API requests. Only `ApiService` work is throttled;
/// audio streaming and download workers keep their own connections.
struct RateLimiter {
    interval_ms: AtomicU64,
    next: Mutex<Option<Instant>>,
}

impl RateLimiter {
    fn new(interval_ms: u64) -> Self {
        Self {
            interval_ms: AtomicU64::new(interval_ms.min(10_000)),
            next: Mutex::new(None),
        }
    }

    fn set_min_interval_ms(&self, interval_ms: u64) {
        self.interval_ms
            .store(interval_ms.min(10_000), Ordering::Relaxed);
    }

    fn wait(&self) {
        let interval = self.interval_ms.load(Ordering::Relaxed);
        if interval == 0 {
            return;
        }
        let mut next = self.next.lock().unwrap();
        let now = Instant::now();
        if let Some(at) = *next
            && at > now
        {
            std::thread::sleep(at - now);
        }
        *next = Some(Instant::now() + Duration::from_millis(interval));
    }
}

impl ApiService {
    pub fn new() -> Self {
        Self::with_wake(None)
    }

    /// Wake the UI when a worker event is ready. Without it results only
    /// arrive while some other timer happens to run (the search page used to
    /// spin forever when playback was idle).
    pub fn with_wake(wake: Option<Arc<dyn Fn() + Send + Sync>>) -> Self {
        let worker_wake = wake.clone();
        let (jobs, job_rx) = unbounded::<Job>();
        let (event_tx, events) = unbounded();
        let limiter = Arc::new(RateLimiter::new(0));
        let worker_limiter = limiter.clone();
        let latest_search = Arc::new(AtomicU64::new(0));
        let latest_suggest = Arc::new(AtomicU64::new(0));
        let latest_album = Arc::new(AtomicU64::new(0));
        let latest_artist = Arc::new(AtomicU64::new(0));
        let latest_resolve = Arc::new(AtomicU64::new(0));
        let latest_next = Arc::new(AtomicU64::new(0));
        let latest_info = Arc::new(AtomicU64::new(0));
        let worker_search = latest_search.clone();
        let worker_suggest = latest_suggest.clone();
        let worker_album = latest_album.clone();
        let worker_artist = latest_artist.clone();
        let worker_resolve = latest_resolve.clone();
        let worker_next = latest_next.clone();
        let worker_info = latest_info.clone();
        let thread = std::thread::Builder::new()
            .name("bamboo-api".to_string())
            .spawn(move || {
                while let Ok(job) = job_rx.recv() {
                    match job {
                        Job::Search {
                            id,
                            kind,
                            client,
                            keyword,
                            offset,
                            limit,
                        } => {
                            if id != worker_search.load(Ordering::Relaxed) {
                                continue;
                            }
                            worker_limiter.wait();
                            let page = match kind {
                                SearchKind::Song => client.search(&keyword, offset, limit),
                                SearchKind::Album => client.search_albums(&keyword, offset, limit),
                            };
                            let _ = event_tx.send(ApiEvent::Search { id, kind, page });
                        }
                        Job::Album {
                            id,
                            client,
                            album_id,
                            offset,
                            limit,
                        } => {
                            if id != worker_album.load(Ordering::Relaxed) {
                                continue;
                            }
                            worker_limiter.wait();
                            let page = client.album(&album_id, offset, limit);
                            let _ = event_tx.send(ApiEvent::Album { id, album_id, page });
                        }
                        Job::Artist {
                            id,
                            client,
                            artist_id,
                            offset,
                            limit,
                        } => {
                            if id != worker_artist.load(Ordering::Relaxed) {
                                continue;
                            }
                            worker_limiter.wait();
                            let page = client.artist(&artist_id, offset, limit);
                            let _ = event_tx.send(ApiEvent::Artist {
                                id,
                                artist_id,
                                page,
                            });
                        }
                        Job::Suggest { id, client, query } => {
                            if id != worker_suggest.load(Ordering::Relaxed) {
                                continue;
                            }
                            worker_limiter.wait();
                            let suggestions = client.suggest(&query);
                            let _ = event_tx.send(ApiEvent::Suggest {
                                id,
                                query,
                                suggestions,
                            });
                        }
                        Job::Resolve {
                            id,
                            client,
                            track_key,
                            song_id,
                            quality,
                            cache,
                            start_secs,
                            autoplay,
                        } => {
                            if id != worker_resolve.load(Ordering::Relaxed) {
                                continue;
                            }
                            worker_limiter.wait();
                            let result = client.prepare(&song_id, quality);
                            tracing::info!(
                                "online resolve {song_id}: {}",
                                if result.is_ok() { "ok" } else { "failed" }
                            );
                            let _ = event_tx.send(ApiEvent::Resolve {
                                id,
                                track_key,
                                cache,
                                start_secs,
                                autoplay,
                                result,
                            });
                        }
                        Job::ResolveNext {
                            id,
                            client,
                            track_key,
                            song_id,
                            cache,
                        } => {
                            if id != worker_next.load(Ordering::Relaxed) {
                                continue;
                            }
                            worker_limiter.wait();
                            // The prefetch only needs a URL. Fetching `info`
                            // here would double the API calls for every track
                            // and waste a request when the user skips it.
                            let result = client.song_url(&song_id).map(|url| PreparedPlay {
                                url,
                                info: None,
                                lrc: None,
                            });
                            tracing::debug!(
                                "next-track pre-resolve {song_id}: {}",
                                if result.is_ok() { "ok" } else { "failed" }
                            );
                            let _ = event_tx.send(ApiEvent::NextResolved {
                                id,
                                track_key,
                                cache,
                                result,
                            });
                        }
                        Job::Info {
                            id,
                            client,
                            song_id,
                        } => {
                            if id != worker_info.load(Ordering::Relaxed) {
                                continue;
                            }
                            worker_limiter.wait();
                            let result = client.info(&song_id);
                            let _ = event_tx.send(ApiEvent::Info { id, result });
                        }
                        Job::Random {
                            id,
                            client,
                            count,
                            seed,
                        } => {
                            worker_limiter.wait();
                            let page = client.random_seeded(seed, count);
                            let _ = event_tx.send(ApiEvent::Random { id, page });
                        }
                        Job::Test { source_id, client } => {
                            worker_limiter.wait();
                            let result = client.status().or_else(|_| {
                                client.search("a", 1, 1).map(|page| {
                                    crate::trf!("连接成功 · 返回 {} 条", page.items.len())
                                })
                            });
                            let _ = event_tx.send(ApiEvent::Test { source_id, result });
                        }
                    }
                    if let Some(wake) = worker_wake.as_ref() {
                        wake();
                    }
                }
            })
            .expect("api thread");
        Self {
            jobs,
            events,
            next_id: 1,
            limiter,
            latest_search,
            latest_suggest,
            latest_album,
            latest_artist,
            latest_resolve,
            latest_next,
            latest_info,
            _thread: thread,
        }
    }

    pub fn next_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        id
    }

    /// Change the minimum spacing between API requests at runtime. `0` means
    /// no artificial delay.
    pub fn set_min_interval_ms(&self, interval_ms: u64) {
        self.limiter.set_min_interval_ms(interval_ms);
    }

    pub fn search(
        &mut self,
        source: &SourceConfig,
        network: &NetworkSettings,
        kind: SearchKind,
        keyword: &str,
        offset: u32,
        limit: u32,
    ) -> u64 {
        let id = self.next_id();
        self.latest_search.store(id, Ordering::Relaxed);
        let _ = self.jobs.send(Job::Search {
            id,
            kind,
            client: ApiClient::new(source, network),
            keyword: keyword.to_string(),
            offset,
            limit,
        });
        id
    }

    pub fn album(
        &mut self,
        source: &SourceConfig,
        network: &NetworkSettings,
        album_id: &str,
        offset: u32,
        limit: u32,
    ) -> u64 {
        let id = self.next_id();
        self.latest_album.store(id, Ordering::Relaxed);
        let _ = self.jobs.send(Job::Album {
            id,
            client: ApiClient::new(source, network),
            album_id: album_id.to_string(),
            offset,
            limit,
        });
        id
    }

    pub fn artist(
        &mut self,
        source: &SourceConfig,
        network: &NetworkSettings,
        artist_id: &str,
        offset: u32,
        limit: u32,
    ) -> u64 {
        let id = self.next_id();
        self.latest_artist.store(id, Ordering::Relaxed);
        let _ = self.jobs.send(Job::Artist {
            id,
            client: ApiClient::new(source, network),
            artist_id: artist_id.to_string(),
            offset,
            limit,
        });
        id
    }

    pub fn suggest(
        &mut self,
        source: &SourceConfig,
        network: &NetworkSettings,
        query: &str,
    ) -> u64 {
        let id = self.next_id();
        self.latest_suggest.store(id, Ordering::Relaxed);
        let _ = self.jobs.send(Job::Suggest {
            id,
            client: ApiClient::new(source, network),
            query: query.to_string(),
        });
        id
    }

    pub fn resolve(
        &mut self,
        source: &SourceConfig,
        network: &NetworkSettings,
        track: &Track,
        cache: Option<PathBuf>,
        start_secs: f64,
        autoplay: bool,
    ) -> Option<u64> {
        let crate::model::TrackSource::Bamboo { song_id, .. } = &track.source else {
            return None;
        };
        let id = self.next_id();
        // A new current track supersedes any queued prefetch for the old one.
        self.latest_next.store(0, Ordering::Relaxed);
        self.latest_resolve.store(id, Ordering::Relaxed);
        let _ = self.jobs.send(Job::Resolve {
            id,
            client: ApiClient::new(source, network),
            track_key: track.dedupe_key(),
            song_id: song_id.clone(),
            quality: source.quality,
            cache,
            start_secs,
            autoplay,
        });
        Some(id)
    }

    /// Resolve the next queue track ahead of time so the audio engine can
    /// pre-open it for gapless playback or a crossfade.
    pub fn resolve_next(
        &mut self,
        source: &SourceConfig,
        network: &NetworkSettings,
        track: &Track,
        cache: Option<PathBuf>,
    ) -> Option<u64> {
        let crate::model::TrackSource::Bamboo { song_id, .. } = &track.source else {
            return None;
        };
        let id = self.next_id();
        self.latest_next.store(id, Ordering::Relaxed);
        let _ = self.jobs.send(Job::ResolveNext {
            id,
            client: ApiClient::new(source, network),
            track_key: track.dedupe_key(),
            song_id: song_id.clone(),
            cache,
        });
        Some(id)
    }

    pub fn info(&mut self, source: &SourceConfig, network: &NetworkSettings, song_id: &str) -> u64 {
        let id = self.next_id();
        self.latest_info.store(id, Ordering::Relaxed);
        let _ = self.jobs.send(Job::Info {
            id,
            client: ApiClient::new(source, network),
            song_id: song_id.to_string(),
        });
        id
    }

    pub fn random(
        &mut self,
        source: &SourceConfig,
        network: &NetworkSettings,
        count: u32,
        seed: u64,
    ) -> u64 {
        let id = self.next_id();
        let _ = self.jobs.send(Job::Random {
            id,
            client: ApiClient::new(source, network),
            count,
            seed,
        });
        id
    }

    pub fn test(&mut self, source: &SourceConfig, network: &NetworkSettings) -> String {
        let source_id = source.id.clone();
        let _ = self.jobs.send(Job::Test {
            source_id: source_id.clone(),
            client: ApiClient::new(source, network),
        });
        source_id
    }

    pub fn poll(&self) -> Option<ApiEvent> {
        self.events.try_recv().ok()
    }
}

impl Default for ApiService {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limiter_spaces_consecutive_requests() {
        let limiter = RateLimiter::new(40);
        let started = Instant::now();
        limiter.wait();
        limiter.wait();
        assert!(
            started.elapsed() >= Duration::from_millis(30),
            "requests were not spaced: {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn zero_interval_does_not_sleep() {
        let limiter = RateLimiter::new(0);
        let started = Instant::now();
        limiter.wait();
        limiter.wait();
        assert!(started.elapsed() < Duration::from_millis(100));
    }
}
