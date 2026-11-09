//! Background recursive audio-file scanner. Batches are streamed while walking
//! so the queue starts filling immediately, even for very large trees.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use crossbeam_channel::{Receiver, Sender, unbounded};
use walkdir::WalkDir;

use crate::util::is_audio_file;

pub const BATCH_SIZE: usize = 300;

/// One directory root. An empty extension list means "use the default audio
/// extensions"; a non-empty list comes from a `#type#*.mp3` playlist directive.
#[derive(Clone, Debug)]
pub struct ScanRoot {
    pub path: PathBuf,
    pub extensions: Vec<String>,
}

impl ScanRoot {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            extensions: Vec::new(),
        }
    }

    pub fn with_extensions(path: impl Into<PathBuf>, extensions: Vec<String>) -> Self {
        Self {
            path: path.into(),
            extensions,
        }
    }
}

#[derive(Clone, Debug)]
pub enum ScanEvent {
    Progress {
        id: u64,
        found: usize,
        current: String,
    },
    Batch {
        id: u64,
        paths: Vec<PathBuf>,
    },
    Finished {
        id: u64,
        total: usize,
        cancelled: bool,
        elapsed_ms: u128,
    },
    Error {
        id: u64,
        message: String,
    },
}

struct Request {
    id: u64,
    roots: Vec<ScanRoot>,
    extensions: Vec<String>,
    follow_links: bool,
}

pub struct ScanService {
    requests: Sender<Request>,
    events: Receiver<ScanEvent>,
    current: Arc<AtomicU64>,
    next_id: AtomicU64,
    _thread: std::thread::JoinHandle<()>,
}

impl ScanService {
    pub fn new() -> Self {
        let (requests, request_rx) = unbounded::<Request>();
        let (event_tx, events) = unbounded();
        let current = Arc::new(AtomicU64::new(0));
        let worker_current = current.clone();
        let _thread = std::thread::Builder::new()
            .name("bamboo-scan".to_string())
            .spawn(move || {
                while let Ok(req) = request_rx.recv() {
                    run_scan(req, &event_tx, &worker_current);
                }
            })
            .expect("scan thread");
        Self {
            requests,
            events,
            current,
            next_id: AtomicU64::new(1),
            _thread,
        }
    }

    pub fn start(&self, roots: Vec<ScanRoot>, extensions: Vec<String>, follow_links: bool) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.current.store(id, Ordering::Release);
        let _ = self.requests.send(Request {
            id,
            roots,
            extensions,
            follow_links,
        });
        id
    }

    pub fn cancel(&self) {
        self.current.store(0, Ordering::Release);
    }

    pub fn try_recv(&self) -> Option<ScanEvent> {
        self.events.try_recv().ok()
    }
}

fn send_batch(events: &Sender<ScanEvent>, id: u64, batch: &mut Vec<PathBuf>) {
    if batch.is_empty() {
        return;
    }
    let _ = events.send(ScanEvent::Batch {
        id,
        paths: std::mem::take(batch),
    });
    batch.reserve(BATCH_SIZE);
}

fn run_scan(req: Request, events: &Sender<ScanEvent>, current: &AtomicU64) {
    let started = Instant::now();
    let mut batch: Vec<PathBuf> = Vec::with_capacity(BATCH_SIZE);
    let mut total = 0usize;
    let mut cancelled = false;

    for root in &req.roots {
        if current.load(Ordering::Acquire) != req.id {
            cancelled = true;
            break;
        }
        if !root.path.is_dir() {
            let _ = events.send(ScanEvent::Error {
                id: req.id,
                message: crate::trf!("目录不存在: {}", root.path.display()),
            });
            continue;
        }
        let extensions = effective_extensions(root, &req.extensions);
        for entry in WalkDir::new(&root.path)
            .follow_links(req.follow_links)
            .sort_by_file_name()
            .into_iter()
            .filter_map(Result::ok)
        {
            if current.load(Ordering::Acquire) != req.id {
                cancelled = true;
                break;
            }
            if !entry.file_type().is_file() {
                continue;
            }
            if !is_audio_file(entry.path(), extensions) {
                continue;
            }
            batch.push(entry.path().to_path_buf());
            total += 1;
            if batch.len() >= BATCH_SIZE {
                send_batch(events, req.id, &mut batch);
                let _ = events.send(ScanEvent::Progress {
                    id: req.id,
                    found: total,
                    current: entry
                        .path()
                        .parent()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default(),
                });
            }
        }
        if cancelled {
            break;
        }
    }

    // Flush the partial batch even when cancelled so already-found songs are
    // not lost when the user stops a long scan.
    send_batch(events, req.id, &mut batch);

    let _ = events.send(ScanEvent::Finished {
        id: req.id,
        total,
        cancelled,
        elapsed_ms: started.elapsed().as_millis(),
    });
}

fn effective_extensions<'a>(root: &'a ScanRoot, defaults: &'a [String]) -> &'a [String] {
    if root.extensions.is_empty() {
        defaults
    } else {
        &root.extensions
    }
}
