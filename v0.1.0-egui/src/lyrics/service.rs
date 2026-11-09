//! Background lyrics discovery/loading so large files or tag parsing never
//! block a frame.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crossbeam_channel::{Receiver, Sender, unbounded};

use crate::config::LyricsEncoding;
use crate::lyrics::{Lyrics, LyricsSource};

pub struct LyricsResult {
    pub key: String,
    pub lyrics: Option<Lyrics>,
    pub error: Option<String>,
}

enum Job {
    Auto {
        generation: u64,
        key: String,
        path: PathBuf,
        encoding: LyricsEncoding,
        region: String,
    },
    File {
        generation: u64,
        key: String,
        path: PathBuf,
        encoding: LyricsEncoding,
        region: String,
    },
}

pub struct LyricsService {
    jobs: Sender<Job>,
    results: Receiver<LyricsResult>,
    generation: Arc<AtomicU64>,
    _thread: std::thread::JoinHandle<()>,
}

impl LyricsService {
    pub fn new() -> Self {
        let (jobs, job_rx) = unbounded::<Job>();
        let (result_tx, results) = unbounded();
        let generation = Arc::new(AtomicU64::new(0));
        let worker_generation = generation.clone();
        let thread = std::thread::Builder::new()
            .name("bamboo-lyrics".to_string())
            .spawn(move || {
                while let Ok(job) = job_rx.recv() {
                    let (generation, key, path, encoding, region, manual) = match job {
                        Job::Auto {
                            generation,
                            key,
                            path,
                            encoding,
                            region,
                        } => (generation, key, path, encoding, region, false),
                        Job::File {
                            generation,
                            key,
                            path,
                            encoding,
                            region,
                        } => (generation, key, path, encoding, region, true),
                    };
                    if generation != worker_generation.load(Ordering::Relaxed) {
                        continue;
                    }
                    let loaded = if manual {
                        crate::lyrics::load_file_with_translation(
                            &path,
                            encoding,
                            LyricsSource::Manual,
                            &region,
                        )
                    } else {
                        crate::lyrics::load_for_audio(&path, encoding, &region)
                    };
                    if generation != worker_generation.load(Ordering::Relaxed) {
                        continue;
                    }
                    let result = match loaded {
                        Ok(lyrics) => LyricsResult {
                            key,
                            lyrics: Some(lyrics),
                            error: None,
                        },
                        Err(error) => LyricsResult {
                            key,
                            lyrics: None,
                            error: Some(error),
                        },
                    };
                    let _ = result_tx.send(result);
                }
            })
            .expect("lyrics thread");
        Self {
            jobs,
            results,
            generation,
            _thread: thread,
        }
    }

    fn next_generation(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::AcqRel) + 1
    }

    pub fn request_auto(&self, key: String, path: PathBuf, encoding: LyricsEncoding, region: &str) {
        let generation = self.next_generation();
        let _ = self.jobs.send(Job::Auto {
            generation,
            key,
            path,
            encoding,
            region: region.to_string(),
        });
    }

    /// Load lyrics for an upcoming track without cancelling the current
    /// request. The result uses a different key and is cached by the app.
    pub fn request_prefetch(
        &self,
        key: String,
        path: PathBuf,
        encoding: LyricsEncoding,
        region: &str,
    ) {
        let generation = self.generation.load(Ordering::Acquire);
        let _ = self.jobs.send(Job::Auto {
            generation,
            key,
            path,
            encoding,
            region: region.to_string(),
        });
    }

    pub fn request_file(&self, key: String, path: PathBuf, encoding: LyricsEncoding, region: &str) {
        let generation = self.next_generation();
        let _ = self.jobs.send(Job::File {
            generation,
            key,
            path,
            encoding,
            region: region.to_string(),
        });
    }

    pub fn cancel(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
    }

    pub fn poll(&self) -> Option<LyricsResult> {
        self.results.try_recv().ok()
    }
}
