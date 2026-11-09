//! Background cover-art loading.
//!
//! Reading embedded artwork and decoding it can take tens to hundreds of
//! milliseconds for large JPEGs. Doing that on the UI thread made track
//! switching and startup feel slow, so the IO + image decode run here and the
//! UI thread only uploads the finished `ColorImage` as a texture.

use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crossbeam_channel::{Receiver, Sender, unbounded};
use egui::ColorImage;

use crate::api::HttpOptions;

enum CoverSource {
    Local(PathBuf),
    Remote { url: String, http: HttpOptions },
}

struct CoverJob {
    generation: u64,
    key: String,
    source: CoverSource,
    accent: [u8; 3],
    max_side: u32,
    want_background: bool,
}

pub struct CoverResult {
    pub key: String,
    pub cover: Option<ColorImage>,
    pub background: Option<ColorImage>,
    /// Unblurred thumbnail used by the GPU background pipeline.
    pub source: Option<ColorImage>,
    /// Encoded artwork for the Windows media flyout (SMTC thumbnail).
    pub smtc_png: Option<Vec<u8>>,
}

pub struct ThumbResult {
    pub key: String,
    pub image: Option<ColorImage>,
}

/// Decodes one image URL for the search grid. Unlike [`CoverService`] this is
/// keyed and cancellable per request, so a screen full of results can load
/// concurrently without invalidating the currently playing cover.
pub struct ThumbnailService {
    jobs: Sender<(String, String, HttpOptions)>,
    results: Receiver<ThumbResult>,
    _thread: std::thread::JoinHandle<()>,
}

impl ThumbnailService {
    pub fn new() -> Self {
        // Visible rows only, but scrolling can still outrun the worker; cap
        // the queue instead of buffering an unbounded page of image requests.
        let (jobs, job_rx) = crossbeam_channel::bounded::<(String, String, HttpOptions)>(64);
        let (result_tx, results) = unbounded();
        let thread = std::thread::Builder::new()
            .name("bamboo-thumbs".to_string())
            .spawn(move || {
                while let Ok((key, url, http)) = job_rx.recv() {
                    let bytes = fetch_image(&url, &http);
                    let image = bytes.as_deref().and_then(|bytes| {
                        crate::util::image_util::decode_cover_images(bytes, [0, 0, 0], 96, false, 4)
                            .map(|decoded| decoded.cover)
                    });
                    let _ = result_tx.send(ThumbResult { key, image });
                }
            })
            .expect("thumbnail thread");
        Self {
            jobs,
            results,
            _thread: thread,
        }
    }

    pub fn request(&self, key: String, url: String, http: HttpOptions) -> bool {
        self.jobs.try_send((key, url, http)).is_ok()
    }

    pub fn poll(&self) -> Option<ThumbResult> {
        self.results.try_recv().ok()
    }
}

impl Default for ThumbnailService {
    fn default() -> Self {
        Self::new()
    }
}

/// Encode a decoded cover as PNG for the Windows media flyout.
pub(crate) fn encode_png(image: &ColorImage) -> Option<Vec<u8>> {
    use image::ImageEncoder;
    let width = image.size[0] as u32;
    let height = image.size[1] as u32;
    let mut rgba = Vec::with_capacity(image.pixels.len() * 4);
    for pixel in &image.pixels {
        rgba.extend_from_slice(&pixel.to_array());
    }
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(&rgba, width, height, image::ExtendedColorType::Rgba8)
        .ok()?;
    Some(out)
}

fn fetch_image(url: &str, http: &HttpOptions) -> Option<Vec<u8>> {
    let agent = http.cached_api_agent();
    let request = http.apply(agent.get(url));
    let mut response = request.call().ok()?;
    if response.status().as_u16() >= 400 {
        return None;
    }
    let mut bytes = Vec::new();
    response
        .body_mut()
        .with_config()
        .limit(12 * 1024 * 1024)
        .reader()
        .read_to_end(&mut bytes)
        .ok()?;
    (!bytes.is_empty()).then_some(bytes)
}

pub struct CoverService {
    jobs: Sender<CoverJob>,
    results: Receiver<CoverResult>,
    generation: Arc<AtomicU64>,
    _thread: std::thread::JoinHandle<()>,
}

impl CoverService {
    pub fn new() -> Self {
        let (jobs, job_rx) = unbounded::<CoverJob>();
        let (result_tx, results) = unbounded();
        let generation = Arc::new(AtomicU64::new(0));
        let worker_generation = generation.clone();
        let thread = std::thread::Builder::new()
            .name("bamboo-cover".to_string())
            .spawn(move || {
                while let Ok(job) = job_rx.recv() {
                    if job.generation != worker_generation.load(Ordering::Relaxed) {
                        continue;
                    }
                    let bytes = match &job.source {
                        CoverSource::Local(path) => crate::util::metadata::cover_bytes(path),
                        CoverSource::Remote { url, http } => fetch_image(url, http),
                    };
                    let decoded = bytes.as_deref().and_then(|bytes| {
                        crate::util::image_util::decode_cover_images(
                            bytes,
                            job.accent,
                            job.max_side,
                            job.want_background,
                            6,
                        )
                    });
                    // A newer track was selected while this cover decoded.
                    if job.generation != worker_generation.load(Ordering::Relaxed) {
                        continue;
                    }
                    let result = match decoded {
                        Some(decoded) => {
                            let smtc_png = encode_png(&decoded.cover);
                            CoverResult {
                                key: job.key,
                                cover: Some(decoded.cover),
                                background: decoded.background,
                                source: decoded.source,
                                smtc_png,
                            }
                        }
                        None => CoverResult {
                            key: job.key,
                            cover: None,
                            background: None,
                            source: None,
                            smtc_png: None,
                        },
                    };
                    let _ = result_tx.send(result);
                }
            })
            .expect("cover thread");
        Self {
            jobs,
            results,
            generation,
            _thread: thread,
        }
    }

    pub fn request(
        &self,
        key: String,
        path: PathBuf,
        accent: [u8; 3],
        max_side: u32,
        want_background: bool,
    ) {
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        let _ = self.jobs.send(CoverJob {
            generation,
            key,
            source: CoverSource::Local(path),
            accent,
            max_side,
            want_background,
        });
    }

    pub fn request_url(
        &self,
        key: String,
        url: String,
        http: HttpOptions,
        accent: [u8; 3],
        max_side: u32,
        want_background: bool,
    ) {
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        let _ = self.jobs.send(CoverJob {
            generation,
            key,
            source: CoverSource::Remote { url, http },
            accent,
            max_side,
            want_background,
        });
    }

    /// Invalidate pending work (e.g. when covers are switched off).
    pub fn cancel(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
    }

    pub fn poll(&self) -> Option<CoverResult> {
        self.results.try_recv().ok()
    }
}
