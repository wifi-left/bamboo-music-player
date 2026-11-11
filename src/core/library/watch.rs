//! Recursive directory watcher for the library roots.
//!
//! The watcher is optional and best-effort: if the platform watcher cannot be
//! created the app keeps working with the manual scan/index buttons. Events are
//! debounced so copy/move operations produce one batch instead of hundreds of
//! individual callbacks.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, unbounded};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use crate::util::is_audio_file;

const DEBOUNCE: Duration = Duration::from_millis(450);
const POLL_TICK: Duration = Duration::from_millis(150);

#[derive(Clone, Debug)]
pub enum WatchEvent {
    /// Audio paths that were created, modified, renamed or removed. Removed
    /// paths are included; the caller checks `is_file()` before indexing.
    Changed(Vec<PathBuf>),
    Error(String),
}

enum Request {
    SetRoots {
        roots: Vec<PathBuf>,
        extensions: Vec<String>,
    },
    Stop,
    Shutdown,
}

pub struct WatchService {
    requests: Sender<Request>,
    events: Receiver<WatchEvent>,
    _thread: Option<std::thread::JoinHandle<()>>,
    _forward: Option<std::thread::JoinHandle<()>>,
}

impl WatchService {
    pub fn new() -> Self {
        Self::with_wake(None)
    }

    /// No watcher thread. Used by `--bench`/`--self-test`.
    pub fn disabled() -> Self {
        let (requests, _request_rx) = unbounded::<Request>();
        let (_event_tx, events) = unbounded::<WatchEvent>();
        Self {
            requests,
            events,
            _thread: None,
            _forward: None,
        }
    }

    /// `wake` runs after each watch event is queued, so the UI drains the
    /// incremental index updates without polling.
    pub fn with_wake(wake: Option<Arc<dyn Fn() + Send + Sync>>) -> Self {
        let (requests, request_rx) = unbounded::<Request>();
        let (raw_tx, raw_events) = unbounded::<WatchEvent>();
        let (event_tx, events) = unbounded::<WatchEvent>();
        let forward = std::thread::Builder::new()
            .name("bamboo-library-watch-events".to_string())
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
            .expect("library watch event thread");
        let thread = std::thread::Builder::new()
            .name("bamboo-library-watch".to_string())
            .spawn(move || run_worker(request_rx, raw_tx))
            .expect("library watch thread");
        Self {
            requests,
            events,
            _thread: Some(thread),
            _forward: Some(forward),
        }
    }

    /// Replace the watched roots.
    pub fn set_roots(&self, roots: Vec<PathBuf>, extensions: Vec<String>) {
        let _ = self.requests.send(Request::SetRoots { roots, extensions });
    }

    pub fn stop(&self) {
        let _ = self.requests.send(Request::Stop);
    }

    pub fn poll(&self) -> Vec<WatchEvent> {
        let mut out = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            out.push(event);
        }
        out
    }
}

impl Drop for WatchService {
    fn drop(&mut self) {
        let _ = self.requests.send(Request::Shutdown);
    }
}

fn run_worker(requests: Receiver<Request>, events: Sender<WatchEvent>) {
    let (raw_tx, raw_rx) = unbounded::<notify::Result<notify::Event>>();
    let mut watcher: Option<RecommendedWatcher> = None;
    let mut extensions: Vec<String> = Vec::new();
    let mut pending: HashSet<PathBuf> = HashSet::new();
    let mut last_event: Option<Instant> = None;
    let mut active = false;

    loop {
        // With no watcher installed there is no event stream to drain, so
        // block on the request channel instead of waking every 150 ms.
        let timeout = if active {
            POLL_TICK
        } else {
            Duration::from_secs(3600)
        };
        match requests.recv_timeout(timeout) {
            Ok(Request::SetRoots {
                roots,
                extensions: next_extensions,
            }) => {
                extensions = next_extensions;
                let next = create_watcher(&raw_tx);
                match next {
                    Ok(mut next_watcher) => {
                        for root in roots {
                            if !root.is_dir() {
                                continue;
                            }
                            if let Err(err) = next_watcher.watch(&root, RecursiveMode::Recursive) {
                                let _ = events.send(WatchEvent::Error(crate::trf!(
                                    "无法监听目录 {}: {}",
                                    root.display(),
                                    err
                                )));
                            }
                        }
                        watcher = Some(next_watcher);
                    }
                    Err(err) => {
                        let _ = events.send(WatchEvent::Error(err));
                        watcher = None;
                    }
                }
                active = watcher.is_some();
                pending.clear();
                last_event = None;
            }
            Ok(Request::Stop) => {
                watcher = None;
                pending.clear();
                active = false;
            }
            Ok(Request::Shutdown) | Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                return;
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
        }

        if let Some(watcher) = watcher.as_mut() {
            // Keep the watcher alive for the whole loop; the OS notifier sends
            // into `raw_tx` from its own thread.
            let _ = watcher;
        }
        while let Ok(result) = raw_rx.try_recv() {
            match result {
                Ok(event) => {
                    for path in event.paths {
                        if !extensions.is_empty() && !is_audio_file(&path, &extensions) {
                            continue;
                        }
                        pending.insert(path);
                        last_event = Some(Instant::now());
                    }
                }
                Err(err) => {
                    let _ = events.send(WatchEvent::Error(crate::trf!("目录监听错误: {}", err)));
                }
            }
        }

        if !pending.is_empty()
            && last_event.is_some_and(|at| at.elapsed() >= DEBOUNCE)
        {
            let paths: Vec<PathBuf> = pending.drain().collect();
            last_event = None;
            if active && !paths.is_empty() {
                let _ = events.send(WatchEvent::Changed(paths));
            }
        }
    }
}

fn create_watcher(
    raw_tx: &Sender<notify::Result<notify::Event>>,
) -> Result<RecommendedWatcher, String> {
    let sender = raw_tx.clone();
    notify::recommended_watcher(move |result| {
        let _ = sender.send(result);
    })
    .map_err(|err| crate::trf!("无法启动目录监听: {}", err))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_new_audio_files_and_ignores_other_extensions() {
        let dir = tempfile::tempdir().unwrap();
        let service = WatchService::new();
        service.set_roots(
            vec![dir.path().to_path_buf()],
            vec!["mp3".to_string()],
        );
        // Give the worker a moment to install the OS watcher.
        std::thread::sleep(Duration::from_millis(300));
        let audio = dir.path().join("track.mp3");
        let other = dir.path().join("notes.txt");
        std::fs::write(&audio, b"x").unwrap();
        std::fs::write(&other, b"x").unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut paths: Vec<PathBuf> = Vec::new();
        while Instant::now() < deadline {
            for event in service.poll() {
                if let WatchEvent::Changed(found) = event {
                    paths.extend(found);
                }
            }
            if paths.iter().any(|path| path == &audio) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            paths.iter().any(|path| path == &audio),
            "audio change was not reported: {paths:?}"
        );
        assert!(
            !paths.iter().any(|path| path == &other),
            "non-audio file must be ignored: {paths:?}"
        );
    }
}
