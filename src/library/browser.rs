//! Lazy folder browsing. Only one directory is read at a time, off the UI
//! thread, so opening a folder with tens of thousands of entries stays smooth.

use std::path::PathBuf;

use crossbeam_channel::{unbounded, Receiver, Sender};

use crate::util::is_audio_file;

#[derive(Clone, Debug)]
pub struct BrowserEntry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub is_playlist: bool,
    pub size: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct FolderBrowser {
    pub current: Option<PathBuf>,
    pub entries: Vec<BrowserEntry>,
    pub loading: bool,
    pub error: Option<String>,
    pub truncated: bool,
    pub history: Vec<PathBuf>,
}

struct Request {
    path: PathBuf,
    extensions: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct BrowseResult {
    pub path: PathBuf,
    pub entries: Vec<BrowserEntry>,
    pub error: Option<String>,
    pub truncated: bool,
}

pub struct BrowseService {
    requests: Sender<Request>,
    results: Receiver<BrowseResult>,
    _thread: std::thread::JoinHandle<()>,
}

impl BrowseService {
    pub fn new() -> Self {
        let (requests, request_rx) = unbounded::<Request>();
        let (result_tx, results) = unbounded();
        let _thread = std::thread::Builder::new()
            .name("bamboo-browse".to_string())
            .spawn(move || {
                while let Ok(req) = request_rx.recv() {
                    let result = read_dir(&req.path, &req.extensions);
                    let _ = result_tx.send(result);
                }
            })
            .expect("browse thread");
        Self {
            requests,
            results,
            _thread,
        }
    }

    pub fn request(&self, path: PathBuf, extensions: Vec<String>) {
        let _ = self.requests.send(Request { path, extensions });
    }

    pub fn poll(&self) -> Vec<BrowseResult> {
        let mut out = Vec::new();
        while let Ok(result) = self.results.try_recv() {
            out.push(result);
        }
        out
    }
}

fn read_dir(path: &PathBuf, extensions: &[String]) -> BrowseResult {
    let mut entries = Vec::new();
    let mut truncated = false;
    match std::fs::read_dir(path) {
        Ok(read) => {
            for entry in read.flatten() {
                if entries.len() >= 20_000 {
                    truncated = true;
                    break;
                }
                let p = entry.path();
                let Ok(file_type) = entry.file_type() else {
                    continue;
                };
                if file_type.is_dir() {
                    entries.push(BrowserEntry {
                        name: entry.file_name().to_string_lossy().into_owned(),
                        path: p,
                        is_dir: true,
                        is_playlist: false,
                        size: None,
                    });
                } else if file_type.is_file() {
                    let ext = p
                        .extension()
                        .map(|e| e.to_string_lossy().to_ascii_lowercase())
                        .unwrap_or_default();
                    let playlist = matches!(ext.as_str(), "mp3list" | "m3u" | "m3u8");
                    if playlist || is_audio_file(&p, extensions) {
                        entries.push(BrowserEntry {
                            name: entry.file_name().to_string_lossy().into_owned(),
                            size: entry.metadata().ok().map(|m| m.len()),
                            path: p,
                            is_dir: false,
                            is_playlist: playlist,
                        });
                    }
                }
            }
            entries.sort_by(|a, b| {
                b.is_dir
                    .cmp(&a.is_dir)
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            });
            BrowseResult {
                path: path.clone(),
                entries,
                error: None,
                truncated,
            }
        }
        Err(err) => BrowseResult {
            path: path.clone(),
            entries: Vec::new(),
            error: Some(err.to_string()),
            truncated: false,
        },
    }
}
