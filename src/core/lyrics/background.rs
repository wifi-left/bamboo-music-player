//! Background image decoding for the embedded lyrics panel.
//!
//! Decoding user-selected pictures is IO plus CPU work, so it runs on a worker
//! thread and wakes the UI when the RGBA frame is ready.

use std::path::PathBuf;
use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender, unbounded};

use crate::util::image_util::RgbaImage;

/// Longest side kept for the panel background. The panel is never wider than
/// the main window, so a larger source only wastes memory.
const MAX_SIDE: u32 = 1280;

#[derive(Clone, Debug)]
pub struct LyricsBackgroundResult {
    pub path: PathBuf,
    pub image: Option<RgbaImage>,
    pub error: Option<String>,
}

pub struct LyricsBackgroundService {
    jobs: Sender<PathBuf>,
    results: Receiver<LyricsBackgroundResult>,
    _thread: std::thread::JoinHandle<()>,
}

impl LyricsBackgroundService {
    pub fn new() -> Self {
        Self::with_wake(None)
    }

    pub fn with_wake(wake: Option<Arc<dyn Fn() + Send + Sync>>) -> Self {
        let (jobs, job_rx) = unbounded::<PathBuf>();
        let (result_tx, results) = unbounded();
        let thread = std::thread::Builder::new()
            .name("bamboo-lyrics-bg".to_string())
            .spawn(move || {
                while let Ok(path) = job_rx.recv() {
                    let result = decode(&path);
                    let _ = result_tx.send(result);
                    if let Some(wake) = wake.as_ref() {
                        wake();
                    }
                }
            })
            .expect("lyrics background thread");
        Self {
            jobs,
            results,
            _thread: thread,
        }
    }

    pub fn load(&self, path: PathBuf) {
        let _ = self.jobs.send(path);
    }

    pub fn poll(&self) -> Option<LyricsBackgroundResult> {
        self.results.try_recv().ok()
    }
}

impl Default for LyricsBackgroundService {
    fn default() -> Self {
        Self::new()
    }
}

fn decode(path: &std::path::Path) -> LyricsBackgroundResult {
    let mut result = LyricsBackgroundResult {
        path: path.to_path_buf(),
        image: None,
        error: None,
    };
    let image = match image::open(path) {
        Ok(image) => image,
        Err(err) => {
            result.error = Some(crate::trf!("无法读取背景图片: {}", err));
            return result;
        }
    };
    let image = if image.width() > MAX_SIDE || image.height() > MAX_SIDE {
        image.thumbnail(MAX_SIDE, MAX_SIDE)
    } else {
        image
    };
    let rgba = image.to_rgba8();
    result.image = Some(RgbaImage::from_rgba_unmultiplied(
        [rgba.width() as usize, rgba.height() as usize],
        &rgba.into_raw(),
    ));
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_and_bounds_a_picture() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("panel.png");
        let width = (MAX_SIDE + 400) as u32;
        let height = 120u32;
        let pixels = vec![90u8; (width * height * 4) as usize];
        image::save_buffer(
            &path,
            &pixels,
            width,
            height,
            image::ColorType::Rgba8,
        )
        .unwrap();

        let result = decode(&path);
        assert!(result.error.is_none(), "{:?}", result.error);
        let image = result.image.expect("decoded pixels");
        assert_eq!(image.size[0], MAX_SIDE as usize);
        assert!(image.size[1] <= height as usize);
    }

    #[test]
    fn missing_file_reports_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let result = decode(&dir.path().join("nope.png"));
        assert!(result.image.is_none());
        assert!(result.error.is_some());
    }
}
