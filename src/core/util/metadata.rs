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

/// `cover_bytes` may read the embedded picture only while the "read song
/// metadata" analysis is enabled; with analysis off, opening every file just
/// to look for artwork would defeat that setting.
static EMBEDDED_COVERS: AtomicBool = AtomicBool::new(true);

pub fn set_embedded_covers_enabled(enabled: bool) {
    EMBEDDED_COVERS.store(enabled, Ordering::Relaxed);
}

fn embedded_covers_enabled() -> bool {
    EMBEDDED_COVERS.load(Ordering::Relaxed)
}

/// Cover resolution order:
///
/// 1. Artwork embedded in the file itself — the strongest per-song signal.
///    Only read when song-data analysis is enabled (`MetadataMode`).
/// 2. A picture named after the song (`<stem>.jpg` / `.png`); it belongs to
///    that song only, so `a - b.jpg` never becomes `a - c`'s cover.
/// 3. Otherwise `cover.jpg` / `cover.png` in the folder, for the remaining
///    files there.
///
/// `folder.jpg` / `front.jpg` / ... are deliberately not treated as folder
/// covers: a catch-all directory with one such image would show the same
/// picture for hundreds of unrelated songs.
pub fn cover_bytes(path: &Path) -> Option<Vec<u8>> {
    if embedded_covers_enabled()
        && let Some(meta) = read_local_with_cover(path, true)
        && let Some(cover) = meta.cover
    {
        return Some(cover);
    }
    if let Some(candidate) = stem_cover_path(path)
        && let Ok(bytes) = std::fs::read(candidate)
    {
        return Some(bytes);
    }
    if let Some(candidate) = sidecar_cover_path(path)
        && let Ok(bytes) = std::fs::read(candidate)
    {
        return Some(bytes);
    }
    None
}

fn stem_cover_path(path: &Path) -> Option<PathBuf> {
    let dir = path.parent()?;
    let stem = path.file_stem()?.to_string_lossy();
    if stem.is_empty() {
        return None;
    }
    for extension in ["jpg", "jpeg", "png"] {
        let candidate = dir.join(format!("{stem}.{extension}"));
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Folder-wide cover: only the explicit `cover.*` names.
pub fn sidecar_cover_path(path: &Path) -> Option<PathBuf> {
    let dir = path.parent()?;
    for name in ["cover.jpg", "cover.jpeg", "cover.png"] {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, bytes: &[u8]) {
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn song_named_picture_wins_over_the_folder_cover() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("a - b.mp3");
        let second = dir.path().join("a - c.mp3");
        write(&first, b"not a real mp3");
        write(&second, b"not a real mp3");
        write(&dir.path().join("a - b.jpg"), b"song-art");
        write(&dir.path().join("cover.jpg"), b"folder-art");
        // The specific picture stays with its own file ...
        assert_eq!(cover_bytes(&first).as_deref(), Some(&b"song-art"[..]));
        // ... and the folder cover serves the other songs.
        assert_eq!(cover_bytes(&second).as_deref(), Some(&b"folder-art"[..]));
    }

    #[test]
    fn folder_cover_is_the_fallback_for_other_songs() {
        let dir = tempfile::tempdir().unwrap();
        let song = dir.path().join("a.mp3");
        write(&song, b"not a real mp3");
        write(&dir.path().join("cover.png"), b"folder-art");
        assert_eq!(cover_bytes(&song).as_deref(), Some(&b"folder-art"[..]));
    }

    #[test]
    fn folder_and_front_images_are_not_folder_covers() {
        let dir = tempfile::tempdir().unwrap();
        let song = dir.path().join("a.mp3");
        write(&song, b"not a real mp3");
        write(&dir.path().join("Folder.jpg"), b"folder-art");
        write(&dir.path().join("front.png"), b"front-art");
        assert!(cover_bytes(&song).is_none());
    }
}
