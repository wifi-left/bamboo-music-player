//! Background recursive audio-file scanner. The UI receives paths in batches so
//! very large libraries (tens of thousands of files) never block painting.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use crossbeam_channel::{unbounded, Receiver, Sender};
use walkdir::WalkDir;

use crate::model::normalize_path;
use crate::util::is_audio_file;

pub const BATCH_SIZE: usize = 500;

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
    roots: Vec<PathBuf>,
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

    pub fn start(&self, roots: Vec<PathBuf>, extensions: Vec<String>, follow_links: bool) -> u64 {
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

    pub fn poll(&self) -> Vec<ScanEvent> {
        let mut out = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            out.push(event);
        }
        out
    }

    pub fn try_recv(&self) -> Option<ScanEvent> {
        self.events.try_recv().ok()
    }
}

fn run_scan(req: Request, events: &Sender<ScanEvent>, current: &AtomicU64) {
    let started = Instant::now();
    let mut path_list: Vec<PathBuf> = Vec::new();
    let mut cancelled = false;

    'roots: for root in &req.roots {
        if !root.is_dir() {
            let _ = events.send(ScanEvent::Error {
                id: req.id,
                message: format!("目录不存在: {}", root.display()),
            });
            continue;
        }
        for entry in WalkDir::new(root)
            .follow_links(req.follow_links)
            .into_iter()
            .filter_map(Result::ok)
        {
            if current.load(Ordering::Acquire) != req.id {
                cancelled = true;
                break 'roots;
            }
            if entry.file_type().is_file() && is_audio_file(entry.path(), &req.extensions) {
                path_list.push(entry.path().to_path_buf());
                if path_list.len() % 500 == 0 {
                    let _ = events.send(ScanEvent::Progress {
                        id: req.id,
                        found: path_list.len(),
                        current: entry
                            .path()
                            .parent()
                            .map(|p| p.display().to_string())
                            .unwrap_or_default(),
                    });
                }
            }
        }
    }

    if cancelled {
        let _ = events.send(ScanEvent::Finished {
            id: req.id,
            total: path_list.len(),
            cancelled: true,
            elapsed_ms: started.elapsed().as_millis(),
        });
        return;
    }

    path_list.sort_by_key(|p| p.to_string_lossy().to_ascii_lowercase());
    path_list.dedup_by(|a, b| normalize_path(a) == normalize_path(b));
    let total = path_list.len();

    for chunk in path_list.chunks(BATCH_SIZE) {
        if current.load(Ordering::Acquire) != req.id {
            cancelled = true;
            break;
        }
        let _ = events.send(ScanEvent::Batch {
            id: req.id,
            paths: chunk.to_vec(),
        });
    }

    let _ = events.send(ScanEvent::Finished {
        id: req.id,
        total,
        cancelled,
        elapsed_ms: started.elapsed().as_millis(),
    });
}
