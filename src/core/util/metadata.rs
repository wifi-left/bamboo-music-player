//! Local metadata and cover extraction via lofty.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use lofty::config::ParseOptions;
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::probe::Probe;
use lofty::tag::{Accessor, ItemKey};

#[derive(Clone, Debug, Default)]
pub struct LocalMeta {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub track_no: Option<u32>,
    pub genre: Option<String>,
    pub duration: Option<f64>,
    pub sample_rate: Option<u32>,
    pub bitrate: Option<u32>,
    pub cover: Option<Vec<u8>>,
}

/// `with_cover = false` is used by the bulk scanner so it does not decode
/// embedded artwork for thousands of files. Lofty still parses tags, but skips
/// the expensive picture payload.
pub fn read_local_with_cover(path: &Path, with_cover: bool) -> Option<LocalMeta> {
    let options = ParseOptions::new().read_cover_art(with_cover);
    let tagged = Probe::open(path).ok()?.options(options).read().ok()?;
    let mut meta = LocalMeta::default();

    if let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) {
        if let Some(title) = tag.title() {
            meta.title = title.to_string();
        }
        if let Some(artist) = tag.artist() {
            meta.artist = artist.to_string();
        }
        if let Some(album) = tag.album() {
            meta.album = album.to_string();
        }
        meta.track_no = tag.track();
        meta.genre = tag.genre().map(|g| g.to_string());
        if with_cover {
            if let Some(pic) = tag.pictures().first() {
                meta.cover = Some(pic.data().to_vec());
            }
        }
    }

    let props = tagged.properties();
    let duration = props.duration().as_secs_f64();
    if duration > 0.0 {
        meta.duration = Some(duration);
    }
    meta.sample_rate = props.sample_rate();
    meta.bitrate = props.audio_bitrate().or_else(|| props.overall_bitrate());

    Some(meta)
}

/// Embedded picture, then common sidecar cover names in the same folder.
pub fn cover_bytes(path: &Path) -> Option<Vec<u8>> {
    if let Some(meta) = read_local_with_cover(path, true) {
        if let Some(cover) = meta.cover {
            return Some(cover);
        }
    }
    let candidate = sidecar_cover_path(path)?;
    std::fs::read(candidate).ok()
}

pub fn sidecar_cover_path(path: &Path) -> Option<PathBuf> {
    let dir = path.parent()?;
    for name in [
        "cover.jpg",
        "cover.jpeg",
        "cover.png",
        "folder.jpg",
        "folder.jpeg",
        "folder.png",
        "front.jpg",
        "front.jpeg",
        "front.png",
        "album.jpg",
        "album.jpeg",
        "album.png",
    ] {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Unsynchronised lyrics stored in the file's primary tag.
pub fn read_embedded_lyrics(path: &Path) -> Option<String> {
    let options = ParseOptions::new().read_cover_art(false);
    let tagged = Probe::open(path).ok()?.options(options).read().ok()?;
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag())?;
    tag.get_string(ItemKey::UnsyncLyrics)
        .or_else(|| tag.get_string(ItemKey::Lyrics))
        .map(str::to_string)
}

/// Background metadata reader so large folders never block the UI.
pub struct MetadataService {
    requests: crossbeam_channel::Sender<PathBuf>,
    results: crossbeam_channel::Receiver<(String, LocalMeta)>,
    enabled: Arc<AtomicBool>,
    _thread: std::thread::JoinHandle<()>,
}

impl MetadataService {
    pub fn new() -> Self {
        let (requests, request_rx) = crossbeam_channel::unbounded::<PathBuf>();
        let (result_tx, results) = crossbeam_channel::unbounded();
        let enabled = Arc::new(AtomicBool::new(true));
        let worker_enabled = enabled.clone();
        let thread = std::thread::Builder::new()
            .name("bamboo-metadata".to_string())
            .spawn(move || {
                while let Ok(path) = request_rx.recv() {
                    if !worker_enabled.load(Ordering::Relaxed) {
                        continue;
                    }
                    if let Some(meta) = read_local_with_cover(&path, false) {
                        let _ = result_tx.send((crate::model::normalize_path(&path), meta));
                    }
                }
            })
            .expect("metadata thread");
        Self {
            requests,
            results,
            enabled,
            _thread: thread,
        }
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    pub fn request(&self, path: PathBuf) {
        if self.enabled.load(Ordering::Relaxed) {
            let _ = self.requests.send(path);
        }
    }

    /// Manual refresh: read the file even when background metadata parsing is
    /// disabled in the performance settings.
    pub fn request_force(&self, path: PathBuf) {
        let _ = self.requests.send(path);
    }

    pub fn has_pending(&self) -> bool {
        !self.results.is_empty()
    }

    /// Drain at most `max` updates so a huge library cannot stall a frame.
    pub fn poll_batch(&self, max: usize) -> Vec<(String, LocalMeta)> {
        let mut out = Vec::new();
        while out.len() < max {
            match self.results.try_recv() {
                Ok(item) => out.push(item),
                Err(_) => break,
            }
        }
        out
    }
}
