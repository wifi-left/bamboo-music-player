//! Local metadata and cover extraction via lofty.

use std::path::{Path, PathBuf};

use lofty::file::{AudioFile, TaggedFileExt};
use lofty::tag::Accessor;

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

pub fn read_local(path: &Path) -> Option<LocalMeta> {
    let tagged = lofty::probe::read_from_path(path).ok()?;
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
        if let Some(pic) = tag.pictures().first() {
            meta.cover = Some(pic.data().to_vec());
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
    if let Some(meta) = read_local(path) {
        if let Some(cover) = meta.cover {
            return Some(cover);
        }
    }
    if let Some(dir) = path.parent() {
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
                if let Ok(bytes) = std::fs::read(&candidate) {
                    return Some(bytes);
                }
            }
        }
    }
    None
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

/// Background metadata reader so large folders never block the UI.
pub struct MetadataService {
    requests: crossbeam_channel::Sender<PathBuf>,
    results: crossbeam_channel::Receiver<(String, LocalMeta)>,
    _thread: std::thread::JoinHandle<()>,
}

impl MetadataService {
    pub fn new() -> Self {
        let (requests, request_rx) = crossbeam_channel::unbounded::<PathBuf>();
        let (result_tx, results) = crossbeam_channel::unbounded();
        let thread = std::thread::Builder::new()
            .name("bamboo-metadata".to_string())
            .spawn(move || {
                while let Ok(path) = request_rx.recv() {
                    if let Some(mut meta) = read_local(&path) {
                        // Covers are loaded lazily for the current track only.
                        meta.cover = None;
                        let _ = result_tx.send((crate::model::normalize_path(&path), meta));
                    }
                }
            })
            .expect("metadata thread");
        Self {
            requests,
            results,
            _thread: thread,
        }
    }

    pub fn request(&self, path: PathBuf) {
        let _ = self.requests.send(path);
    }

    pub fn poll(&self) -> Vec<(String, LocalMeta)> {
        let mut out = Vec::new();
        while let Ok(item) = self.results.try_recv() {
            out.push(item);
        }
        out
    }
}

