//! User-selected main-window background picture.
//!
//! Decoding a photo is IO plus CPU work, so it runs on a worker thread and
//! wakes the UI when an RGBA frame is ready. Painting only ever consumes the
//! cached frame; it never opens the file on the UI thread.

use std::path::PathBuf;
use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender, unbounded};

use crate::util::image_util::RgbaImage;

/// Longest side kept for the background. Larger sources only waste memory;
/// the painter applies a dark veil on top anyway.
const MAX_SIDE: u32 = 2560;

#[derive(Clone, Debug)]
pub struct BackgroundImageResult {
    pub path: PathBuf,
    pub image: Option<RgbaImage>,
    pub error: Option<String>,
}

pub struct BackgroundImageService {
    jobs: Sender<PathBuf>,
    results: Receiver<BackgroundImageResult>,
    _thread: std::thread::JoinHandle<()>,
}

impl BackgroundImageService {
    pub fn new() -> Self {
        Self::with_wake(None)
    }

    pub fn with_wake(wake: Option<Arc<dyn Fn() + Send + Sync>>) -> Self {
        let (jobs, job_rx) = unbounded::<PathBuf>();
        let (result_tx, results) = unbounded();
        let thread = std::thread::Builder::new()
            .name("bamboo-background-image".to_string())
            .spawn(move || {
                while let Ok(path) = job_rx.recv() {
                    let result = decode(&path);
                    let _ = result_tx.send(result);
                    if let Some(wake) = wake.as_ref() {
                        wake();
                    }
                }
            })
            .expect("background image thread");
        Self {
            jobs,
            results,
            _thread: thread,
        }
    }

    pub fn load(&self, path: PathBuf) {
        let _ = self.jobs.send(path);
    }

    pub fn poll(&self) -> Option<BackgroundImageResult> {
        self.results.try_recv().ok()
    }
}

impl Default for BackgroundImageService {
    fn default() -> Self {
        Self::new()
    }
}

fn decode(path: &std::path::Path) -> BackgroundImageResult {
    let mut result = BackgroundImageResult {
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
    fn decodes_and_bounds_a_background_picture() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bg.png");
        let width = (MAX_SIDE + 300) as u32;
        let height = 90u32;
        let pixels = vec![60u8; (width * height * 4) as usize];
        image::save_buffer(&path, &pixels, width, height, image::ColorType::Rgba8).unwrap();

        let result = decode(&path);
        assert!(result.error.is_none(), "{:?}", result.error);
        let image = result.image.expect("decoded pixels");
        assert!(image.size[0].max(image.size[1]) <= MAX_SIDE as usize);
    }

    #[test]
    fn missing_file_reports_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let result = decode(&dir.path().join("missing.png"));
        assert!(result.image.is_none());
        assert!(result.error.is_some());
    }
}
