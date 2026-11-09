//! Background download queue for online tracks.
//!
//! Downloads resolve an online track to a direct URL (Bamboo `prepare`) and
//! stream it to disk on worker threads, so the UI never blocks on the network.
//! Progress and terminal states are reported through a channel. Post-processing
//! (tag/cover embedding, `.lrc` sidecar) is best-effort: a failure there never
//! fails the download itself.

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, unbounded};
use lofty::config::WriteOptions;
use lofty::file::TaggedFileExt;
use lofty::picture::{Picture, PictureType};
use lofty::tag::{Accessor, Tag, TagExt};
use serde::{Deserialize, Serialize};

use crate::api::{ApiClient, HttpOptions};
use crate::config::{ConflictPolicy, DownloadSettings, NetworkSettings, Quality, SourceConfig};
use crate::model::{Track, TrackSource};

const PROGRESS_INTERVAL: Duration = Duration::from_millis(180);
const MAX_COVER_BYTES: u64 = 12 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DownloadState {
    Queued,
    Resolving,
    Downloading,
    Completed,
    Failed,
    Canceled,
    Skipped,
}

impl DownloadState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Queued => crate::i18n::tr("排队中"),
            Self::Resolving => crate::i18n::tr("解析地址"),
            Self::Downloading => crate::i18n::tr("下载中"),
            Self::Completed => crate::i18n::tr("已完成"),
            Self::Failed => crate::i18n::tr("失败"),
            Self::Canceled => crate::i18n::tr("已取消"),
            Self::Skipped => crate::i18n::tr("已跳过"),
        }
    }

    pub fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Resolving | Self::Downloading)
    }
}

#[derive(Clone, Debug)]
pub enum DownloadEvent {
    Progress {
        id: u64,
        received: u64,
        total: Option<u64>,
    },
    State {
        id: u64,
        state: DownloadState,
        received: u64,
        total: Option<u64>,
        path: Option<PathBuf>,
        error: Option<String>,
    },
}

struct Job {
    id: u64,
    track: Track,
    source: Option<SourceConfig>,
    /// Per-download quality override; `None` uses the source's quality.
    quality: Option<Quality>,
    network: NetworkSettings,
    settings: DownloadSettings,
    base_dir: PathBuf,
}

enum Command {
    Start(Box<Job>),
    Shutdown,
}

/// Worker concurrency gate. Workers are always spawned (up to the maximum) so
/// the setting can be raised at runtime; the gate decides how many may run.
struct Limiter {
    state: Mutex<LimiterState>,
    changed: Condvar,
}

struct LimiterState {
    limit: usize,
    active: usize,
    closed: bool,
}

impl Limiter {
    fn new(limit: usize) -> Self {
        Self {
            state: Mutex::new(LimiterState {
                limit: limit.clamp(1, MAX_DOWNLOAD_WORKERS),
                active: 0,
                closed: false,
            }),
            changed: Condvar::new(),
        }
    }

    /// Wait for a slot. Returns false once the service is shutting down.
    fn acquire(&self) -> bool {
        let mut state = self.state.lock().unwrap();
        while !state.closed && state.active >= state.limit {
            state = self.changed.wait(state).unwrap();
        }
        if state.closed {
            return false;
        }
        state.active += 1;
        true
    }

    fn release(&self) {
        let mut state = self.state.lock().unwrap();
        state.active = state.active.saturating_sub(1);
        self.changed.notify_all();
    }

    fn set_limit(&self, limit: usize) {
        let mut state = self.state.lock().unwrap();
        state.limit = limit.clamp(1, MAX_DOWNLOAD_WORKERS);
        self.changed.notify_all();
    }

    fn close(&self) {
        let mut state = self.state.lock().unwrap();
        state.closed = true;
        self.changed.notify_all();
    }
}

pub struct DownloadService {
    jobs: Sender<Command>,
    events: Receiver<DownloadEvent>,
    cancelled: Arc<Mutex<HashSet<u64>>>,
    limiter: Arc<Limiter>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

/// Maximum workers that can be active at once. The UI range matches this.
const MAX_DOWNLOAD_WORKERS: usize = 4;

impl DownloadService {
    pub fn new(concurrency: usize) -> Self {
        let (jobs, job_rx) = unbounded::<Command>();
        let (event_tx, events) = unbounded();
        let cancelled = Arc::new(Mutex::new(HashSet::new()));
        let limiter = Arc::new(Limiter::new(concurrency));
        let mut threads = Vec::with_capacity(MAX_DOWNLOAD_WORKERS);
        for index in 0..MAX_DOWNLOAD_WORKERS {
            let job_rx: Receiver<Command> = job_rx.clone();
            let event_tx = event_tx.clone();
            let cancelled = cancelled.clone();
            let limiter = limiter.clone();
            let thread = std::thread::Builder::new()
                .name(format!("bamboo-download-{index}"))
                .spawn(move || {
                    while let Ok(command) = job_rx.recv() {
                        match command {
                            Command::Start(job) => {
                                // The gate is acquired after taking the job, so
                                // lowering the setting keeps already queued
                                // downloads waiting instead of oversubscribing.
                                if !limiter.acquire() {
                                    break;
                                }
                                run_job(*job, &cancelled, &event_tx);
                                limiter.release();
                            }
                            Command::Shutdown => break,
                        }
                    }
                })
                .expect("download thread");
            threads.push(thread);
        }
        Self {
            jobs,
            events,
            cancelled,
            limiter,
            threads,
        }
    }

    /// Change the number of concurrent downloads without restarting the app.
    /// Running downloads continue; the new limit applies to queued jobs as
    /// slots become free.
    pub fn set_concurrency(&self, concurrency: usize) {
        self.limiter.set_limit(concurrency);
    }

    pub fn start(
        &self,
        id: u64,
        track: Track,
        source: Option<SourceConfig>,
        quality: Option<Quality>,
        network: NetworkSettings,
        settings: DownloadSettings,
        base_dir: PathBuf,
    ) {
        let _ = self.jobs.send(Command::Start(Box::new(Job {
            id,
            track,
            source,
            quality,
            network,
            settings,
            base_dir,
        })));
    }

    pub fn cancel(&self, id: u64) {
        self.cancelled.lock().unwrap().insert(id);
    }

    pub fn poll(&self) -> Vec<DownloadEvent> {
        let mut out = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            out.push(event);
        }
        out
    }

}

impl Drop for DownloadService {
    fn drop(&mut self) {
        self.limiter.close();
        for _ in 0..self.threads.len() {
            let _ = self.jobs.send(Command::Shutdown);
        }
    }
}

fn run_job(job: Job, cancelled: &Arc<Mutex<HashSet<u64>>>, events: &Sender<DownloadEvent>) {
    let id = job.id;
    let is_cancelled = || cancelled.lock().unwrap().contains(&id);
    let finish = |state: DownloadState,
                  path: Option<PathBuf>,
                  error: Option<String>,
                  received: u64,
                  total: Option<u64>| {
        cancelled.lock().unwrap().remove(&id);
        let _ = events.send(DownloadEvent::State {
            id,
            state,
            received,
            total,
            path,
            error,
        });
    };

    if is_cancelled() {
        finish(DownloadState::Canceled, None, None, 0, None);
        return;
    }

    if job.network.offline {
        finish(
            DownloadState::Failed,
            None,
            Some(crate::i18n::tr("离线模式下不能下载").to_string()),
            0,
            None,
        );
        return;
    }

    let mut track = job.track.clone();
    let mut lrc: Option<String> = None;
    let source_url: Option<String>;
    let _ = events.send(DownloadEvent::State {
        id,
        state: DownloadState::Resolving,
        received: 0,
        total: None,
        path: None,
        error: None,
    });
    let http = match job.source.as_ref() {
        Some(source) => HttpOptions::from_settings(source, &job.network).for_stream(),
        None => HttpOptions::for_network(&job.network).for_stream(),
    };

    if let TrackSource::Bamboo { song_id, .. } = &track.source {
        let Some(source) = job.source.as_ref() else {
            finish(
                DownloadState::Failed,
                None,
                Some(crate::i18n::tr("找不到在线音源配置").to_string()),
                0,
                None,
            );
            return;
        };
        let client = ApiClient::new(source, &job.network);
        match client.prepare(song_id, job.quality.unwrap_or(source.quality)) {
            Ok(prepared) => {
                source_url = Some(prepared.url);
                lrc = prepared.lrc;
                if let Some(info) = prepared.info {
                    if !info.name.trim().is_empty() {
                        track.title = info.name;
                    }
                    if !info.artist.trim().is_empty() {
                        track.artist = info.artist;
                    }
                    if !info.album.trim().is_empty() {
                        track.album = info.album;
                    }
                    if track.cover_url.is_none() {
                        track.cover_url = info.pic;
                    }
                }
            }
            Err(err) => {
                finish(DownloadState::Failed, None, Some(err), 0, None);
                return;
            }
        }
    } else {
        source_url = match &track.source {
            TrackSource::Url { url } | TrackSource::Radio { url } => Some(url.clone()),
            _ => None,
        };
    }

    let Some(url) = source_url else {
        finish(
            DownloadState::Failed,
            None,
            Some(crate::i18n::tr("该曲目没有可下载的地址").to_string()),
            0,
            None,
        );
        return;
    };

    // HLS needs a media playlist fetched before the file extension is known.
    // Preparing also downloads the first segment, so the media type is probed
    // exactly once instead of fetching it again in the write loop.
    let mut hls = if crate::audio::hls::is_hls_url(&url) {
        match crate::audio::hls::HlsDownload::prepare(&url, http.clone()) {
            Ok(download) => Some(download),
            Err(err) => {
                finish(DownloadState::Failed, None, Some(err), 0, None);
                return;
            }
        }
    } else {
        None
    };
    let extension = match &hls {
        Some(download) => download.extension().to_string(),
        None => audio_extension(
            &url,
            job.quality
                .or_else(|| job.source.as_ref().map(|source| source.quality)),
        ),
    };
    let relative = render_relative_path(&job.settings.template, &track, &extension);
    let target = job.base_dir.join(relative);
    let skip_path = target.clone();
    let Some(target) = resolve_conflict(target, job.settings.conflict) else {
        finish(DownloadState::Skipped, Some(skip_path), None, 0, None);
        return;
    };
    let part = with_part_suffix(&target);
    if let Some(parent) = target.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let resumable = hls.is_none();
    let result = match hls.as_mut() {
        Some(download) => {
            let cancelled = cancelled.clone();
            let events = events.clone();
            download.write_to(
                &part,
                move || cancelled.lock().unwrap().contains(&id),
                move |received, total| {
                    let _ = events.send(DownloadEvent::Progress {
                        id,
                        received,
                        total,
                    });
                },
            )
        }
        None => download(&url, &http, &part, id, cancelled, events),
    };
    let total = match result {
        Ok(total) => total,
        Err(err) if err == "canceled" => {
            if !resumable {
                let _ = fs::remove_file(&part);
                remove_part_meta(&part);
            }
            finish(DownloadState::Canceled, None, None, 0, None);
            return;
        }
        Err(err) => {
            if !resumable {
                let _ = fs::remove_file(&part);
                remove_part_meta(&part);
            }
            finish(DownloadState::Failed, None, Some(err), 0, None);
            return;
        }
    };

    if let Err(err) = fs::rename(&part, &target) {
        let _ = fs::remove_file(&part);
        remove_part_meta(&part);
        finish(
            DownloadState::Failed,
            None,
            Some(crate::trf!("无法保存文件: {}", err)),
            0,
            total,
        );
        return;
    }
    remove_part_meta(&part);

    let received = fs::metadata(&target).map(|m| m.len()).unwrap_or(0);
    let _ = events.send(DownloadEvent::Progress {
        id,
        received,
        total: total.or(Some(received)),
    });

    let mut warnings = Vec::new();
    if job.settings.save_lyrics
        && let Some(lyrics) = lrc.as_deref()
        && !lyrics.trim().is_empty()
    {
        let lrc_path = target.with_extension("lrc");
        if let Err(err) = fs::write(&lrc_path, lyrics) {
            warnings.push(crate::trf!("歌词写入失败: {}", err));
        }
    }

    if job.settings.embed_tags || job.settings.embed_cover {
        let cover = if job.settings.embed_cover {
            track
                .cover_url
                .as_deref()
                .and_then(|url| fetch_cover(url, &http).ok().flatten())
        } else {
            None
        };
        if let Err(err) = embed_metadata(&target, &track, cover.as_deref()) {
            warnings.push(crate::trf!("标签写入失败: {}", err));
        }
    }

    finish(
        DownloadState::Completed,
        Some(target),
        (!warnings.is_empty()).then(|| warnings.join("；")),
        received,
        total,
    );
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct PartMeta {
    /// URL path (without the query) of the resource the partial bytes belong
    /// to. Bamboo URLs carry a changing token in the query, so only the path
    /// is compared.
    url_path: String,
    etag: Option<String>,
    last_modified: Option<String>,
    total: Option<u64>,
}

fn url_path(url: &str) -> String {
    url::Url::parse(url)
        .map(|parsed| parsed.path().to_string())
        .unwrap_or_else(|_| url.to_string())
}

fn part_meta_path(part: &Path) -> PathBuf {
    let mut os = part.as_os_str().to_os_string();
    os.push(".meta");
    PathBuf::from(os)
}

fn read_part_meta(part: &Path) -> Option<PartMeta> {
    fs::read(part_meta_path(part))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
}

fn write_part_meta(part: &Path, meta: &PartMeta) {
    if let Ok(bytes) = serde_json::to_vec(meta) {
        let _ = fs::write(part_meta_path(part), bytes);
    }
}

fn remove_part_meta(part: &Path) {
    let _ = fs::remove_file(part_meta_path(part));
}

/// Returns the full file size (if known) on success or an error message.
/// The `.part` file is kept on interruption so a retry can resume with an
/// HTTP Range request; it is removed only when the transfer restarts or the
/// file is moved into place.
fn download(
    url: &str,
    http: &HttpOptions,
    part: &Path,
    id: u64,
    cancelled: &Arc<Mutex<HashSet<u64>>>,
    events: &Sender<DownloadEvent>,
) -> Result<Option<u64>, String> {
    if crate::audio::hls::is_hls_url(url) {
        return Err(crate::i18n::tr("HLS 流需要通过 HlsDownload 下载").to_string());
    }
    download_inner(url, http, part, id, cancelled, events, true)
}

#[allow(clippy::too_many_arguments)]
fn download_inner(
    url: &str,
    http: &HttpOptions,
    part: &Path,
    id: u64,
    cancelled: &Arc<Mutex<HashSet<u64>>>,
    events: &Sender<DownloadEvent>,
    allow_resume: bool,
) -> Result<Option<u64>, String> {
    if !allow_resume {
        let _ = fs::remove_file(part);
        remove_part_meta(part);
    }
    let mut existing = if allow_resume {
        fs::metadata(part).map(|meta| meta.len()).unwrap_or(0)
    } else {
        0
    };
    let meta = if existing > 0 {
        read_part_meta(part).unwrap_or_default()
    } else {
        PartMeta::default()
    };
    if existing > 0
        && !meta.url_path.is_empty()
        && meta.url_path != url_path(url)
    {
        // The partial file was produced from a different resource.
        existing = 0;
    }

    let agent = http.stream_agent();
    let mut request = http
        .apply(agent.get(url))
        .header("Accept-Encoding", "identity");
    if existing > 0 {
        request = request.header("Range", format!("bytes={existing}-"));
        if let Some(etag) = meta.etag.as_deref() {
            request = request.header("If-Range", etag);
        } else if let Some(last_modified) = meta.last_modified.as_deref() {
            request = request.header("If-Range", last_modified);
        }
    }
    let response = request
        .call()
        .map_err(|err| crate::trf!("请求失败: {}", err))?;
    let status = response.status().as_u16();

    if status == 416 && existing > 0 {
        // The partial file may already be complete. RFC 9110 returns
        // `Content-Range: bytes */<total>` for an unsatisfiable range.
        let total = response
            .headers()
            .get("content-range")
            .and_then(|value| value.to_str().ok())
            .and_then(parse_unsatisfied_range);
        if total == Some(existing) {
            return Ok(total);
        }
        return download_inner(url, http, part, id, cancelled, events, false);
    }
    if status >= 400 {
        return Err(format!("HTTP {status}"));
    }

    let content_range = response
        .headers()
        .get("content-range")
        .and_then(|value| value.to_str().ok())
        .and_then(parse_content_range);
    let mut append = false;
    let total: Option<u64>;
    let mut received: u64;
    if status == 206 && existing > 0 {
        let Some((start, _end, full)) = content_range else {
            // A malformed 206 cannot be trusted; restart without a Range.
            return download_inner(url, http, part, id, cancelled, events, false);
        };
        if start != existing {
            return download_inner(url, http, part, id, cancelled, events, false);
        }
        append = true;
        total = Some(full);
        received = existing;
    } else {
        // 200 means the server ignored the range (or If-Range failed). Start
        // over so a changed remote file cannot be mixed with the old bytes.
        total = response
            .headers()
            .get("content-length")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok());
        received = 0;
    }

    let etag = response
        .headers()
        .get("etag")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let last_modified = response
        .headers()
        .get("last-modified")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    write_part_meta(
        part,
        &PartMeta {
            url_path: url_path(url),
            etag,
            last_modified,
            total,
        },
    );
    let _ = events.send(DownloadEvent::State {
        id,
        state: DownloadState::Downloading,
        received,
        total,
        path: None,
        error: None,
    });

    let mut reader = response.into_body().into_reader();
    let mut file = if append {
        OpenOptions::new()
            .append(true)
            .open(part)
            .map_err(|err| crate::trf!("无法打开文件: {}", err))?
    } else {
        File::create(part).map_err(|err| crate::trf!("无法创建文件: {}", err))?
    };
    let mut buffer = [0u8; 64 * 1024];
    let mut last_emit = Instant::now();
    loop {
        if cancelled.lock().unwrap().contains(&id) {
            return Err("canceled".to_string());
        }
        let read = reader
            .read(&mut buffer)
            .map_err(|err| crate::trf!("读取失败: {}", err))?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read])
            .map_err(|err| crate::trf!("写入失败: {}", err))?;
        received += read as u64;
        if last_emit.elapsed() >= PROGRESS_INTERVAL {
            last_emit = Instant::now();
            let _ = events.send(DownloadEvent::Progress {
                id,
                received,
                total,
            });
        }
    }
    file.flush()
        .map_err(|err| crate::trf!("写入失败: {}", err))?;
    if let Some(total) = total
        && received < total
    {
        // Keep the partial bytes for a later Range retry.
        return Err(crate::trf!(
            "下载不完整：{received}/{total} 字节",
            received,
            total
        ));
    }
    let _ = events.send(DownloadEvent::Progress {
        id,
        received,
        total: total.or(Some(received)),
    });
    Ok(total.or(Some(received)))
}

/// Parse `bytes <start>-<end>/<total>`.
fn parse_content_range(value: &str) -> Option<(u64, u64, u64)> {
    let value = value.trim().strip_prefix("bytes ")?;
    let (range, total) = value.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    Some((
        start.trim().parse().ok()?,
        end.trim().parse().ok()?,
        total.trim().parse().ok()?,
    ))
}

/// Parse `bytes */<total>` from a 416 response.
fn parse_unsatisfied_range(value: &str) -> Option<u64> {
    value.trim().strip_prefix("bytes */")?.trim().parse().ok()
}

fn fetch_cover(url: &str, http: &HttpOptions) -> Result<Option<Vec<u8>>, String> {
    if url.trim().is_empty() {
        return Ok(None);
    }
    let agent = http.cached_api_agent();
    let response = http
        .apply(agent.get(url))
        .call()
        .map_err(|err| err.to_string())?;
    if response.status().as_u16() >= 400 {
        return Ok(None);
    }
    if response
        .headers()
        .get("content-length")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|len| len > MAX_COVER_BYTES)
    {
        return Ok(None);
    }
    let reader = response.into_body().into_reader();
    let mut data = Vec::new();
    reader
        .take(MAX_COVER_BYTES)
        .read_to_end(&mut data)
        .map_err(|err| err.to_string())?;
    Ok((!data.is_empty()).then_some(data))
}

fn embed_metadata(path: &Path, track: &Track, cover: Option<&[u8]>) -> Result<(), String> {
    let mut tagged = lofty::read_from_path(path).map_err(|err| err.to_string())?;
    let tag_type = tagged.primary_tag_type();
    let mut tag = tagged
        .remove(tag_type)
        .unwrap_or_else(|| Tag::new(tag_type));
    if !track.display_title().trim().is_empty() {
        tag.set_title(track.display_title());
    }
    if !track.artist.trim().is_empty() {
        tag.set_artist(track.artist.clone());
    }
    if !track.album.trim().is_empty() {
        tag.set_album(track.album.clone());
    }
    if let Some(genre) = track.genre.as_ref().filter(|g| !g.trim().is_empty()) {
        tag.set_genre(genre.clone());
    }
    if let Some(number) = track.track_no {
        tag.set_track(number);
    }
    if let Some(bytes) = cover {
        let mut cursor = Cursor::new(bytes);
        if let Ok(mut picture) = Picture::from_reader(&mut cursor) {
            picture.set_pic_type(PictureType::CoverFront);
            tag.push_picture(picture);
        }
    }
    tag.save_to_path(path, WriteOptions::default())
        .map_err(|err| err.to_string())
}

/// Sanitize one path component for Windows. Reserved device names are
/// prefixed so `CON`, `NUL`, etc. cannot be created.
pub fn sanitize_component(input: &str) -> String {
    let mut out: String = input
        .chars()
        .map(|ch| {
            if matches!(ch, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || ch.is_control()
            {
                '_'
            } else {
                ch
            }
        })
        .collect();
    out = out.trim().trim_matches('.').trim().to_string();
    if out.is_empty() {
        out = crate::i18n::tr("未知").to_string();
    }
    const RESERVED: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    let stem = out
        .split('.')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_uppercase();
    if RESERVED.contains(&stem.as_str()) {
        out = format!("_{out}");
    }
    let max_chars = 90;
    if out.chars().count() > max_chars {
        out = out.chars().take(max_chars).collect();
    }
    out
}

pub fn render_relative_path(template: &str, track: &Track, extension: &str) -> PathBuf {
    let title = sanitize_component(&track.display_title());
    let artist = if track.artist.trim().is_empty() {
        crate::i18n::tr("未知歌手").to_string()
    } else {
        sanitize_component(&track.artist)
    };
    let album = if track.album.trim().is_empty() {
        crate::i18n::tr("未知专辑").to_string()
    } else {
        sanitize_component(&track.album)
    };
    let mut rendered = if template.trim().is_empty() {
        "{artist}/{album}/{title}".to_string()
    } else {
        template.to_string()
    };
    let genre = track.genre.clone().unwrap_or_default();
    let year = track.year.map(|y| y.to_string()).unwrap_or_default();
    let track_no = track
        .track_no
        .map(|n| format!("{n:02}"))
        .unwrap_or_default();
    for (token, value) in [
        ("{artist}", artist.as_str()),
        ("{album}", album.as_str()),
        ("{title}", title.as_str()),
        ("{genre}", genre.as_str()),
        ("{year}", year.as_str()),
        ("{track}", track_no.as_str()),
        ("{source}", track.source_name.as_str()),
    ] {
        rendered = rendered.replace(token, value);
    }
    let mut path = PathBuf::new();
    for part in rendered
        .split(['/', '\\'])
        .filter(|part| !part.trim().is_empty())
    {
        path.push(sanitize_component(part));
    }
    if path.as_os_str().is_empty() {
        path.push(sanitize_component(&title));
    }
    let mut path = path;
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "track".to_string());
    path.set_file_name(format!("{file_name}.{extension}"));
    path
}

fn audio_extension(url: &str, quality: Option<Quality>) -> String {
    const EXTS: [&str; 10] = [
        "mp3", "flac", "m4a", "aac", "ogg", "opus", "wav", "aiff", "ape", "wv",
    ];
    if let Ok(parsed) = url::Url::parse(url)
        && let Some(ext) = Path::new(parsed.path())
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase())
        && EXTS.contains(&ext.as_str())
    {
        return ext;
    }
    match quality {
        Some(Quality::Flac) => "flac".to_string(),
        _ => "mp3".to_string(),
    }
}

/// Applies the conflict policy. `None` means the caller should skip the file.
fn resolve_conflict(path: PathBuf, policy: ConflictPolicy) -> Option<PathBuf> {
    if !path.exists() {
        return Some(path);
    }
    match policy {
        ConflictPolicy::Skip => None,
        ConflictPolicy::Overwrite => {
            let _ = fs::remove_file(&path);
            Some(path)
        }
        ConflictPolicy::Rename => {
            let parent = path.parent().map(Path::to_path_buf).unwrap_or_default();
            let stem = path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_else(|| "track".to_string());
            let extension = path
                .extension()
                .map(|ext| ext.to_string_lossy().into_owned())
                .unwrap_or_default();
            for index in 1..=9999 {
                let name = if extension.is_empty() {
                    format!("{stem} ({index})")
                } else {
                    format!("{stem} ({index}).{extension}")
                };
                let candidate = parent.join(name);
                if !candidate.exists() {
                    return Some(candidate);
                }
            }
            None
        }
    }
}

fn with_part_suffix(path: &Path) -> PathBuf {
    let mut os = path.as_os_str().to_os_string();
    os.push(".part");
    PathBuf::from(os)
}

/// Helper used by the app before queueing a download.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limiter_limit_changes_at_runtime_and_clamps() {
        let limiter = Limiter::new(1);
        assert_eq!(limiter.state.lock().unwrap().limit, 1);
        limiter.set_limit(4);
        assert_eq!(limiter.state.lock().unwrap().limit, 4);
        limiter.set_limit(0);
        assert_eq!(limiter.state.lock().unwrap().limit, 1);
        assert!(limiter.acquire());
        limiter.release();
        limiter.close();
        assert!(!limiter.acquire(), "closed limiter must not admit jobs");
    }

    fn track() -> Track {
        let mut track = Track::local("C:\\music\\x.mp3");
        track.title = "A/B: Test?".to_string();
        track.artist = "Artist".to_string();
        track.album = "Album".to_string();
        track.year = Some(2024);
        track.track_no = Some(3);
        track
    }

    #[test]
    fn sanitizes_windows_reserved_characters_and_names() {
        assert_eq!(sanitize_component("A/B:C*?"), "A_B_C__");
        assert_eq!(sanitize_component("  .. "), "未知");
        assert_eq!(sanitize_component("CON"), "_CON");
        assert_eq!(sanitize_component("正常名称"), "正常名称");
    }

    #[test]
    fn renders_template_with_extension() {
        let path = render_relative_path("{artist}/{album}/{title}", &track(), "flac");
        assert_eq!(
            path,
            PathBuf::from("Artist")
                .join("Album")
                .join("A_B_ Test_.flac")
        );
    }

    #[test]
    fn conflict_rename_uses_next_free_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("song.mp3");
        fs::write(&path, b"x").unwrap();
        let renamed = resolve_conflict(path.clone(), ConflictPolicy::Rename).unwrap();
        assert_eq!(renamed, dir.path().join("song (1).mp3"));
        assert!(resolve_conflict(path, ConflictPolicy::Skip).is_none());
    }

    #[test]
    fn picks_extension_from_url_and_quality() {
        assert_eq!(
            audio_extension("https://example.com/a.flac?token=1", None),
            "flac"
        );
        assert_eq!(
            audio_extension("https://example.com/stream", Some(Quality::Flac)),
            "flac"
        );
        assert_eq!(
            audio_extension("https://example.com/stream", Some(Quality::K320)),
            "mp3"
        );
    }

    #[test]
    fn direct_download_entrypoint_rejects_hls_playlists() {
        let http = HttpOptions::for_network(&NetworkSettings::default()).for_stream();
        let cancelled = Arc::new(Mutex::new(HashSet::new()));
        let (tx, _rx) = unbounded();
        let dir = tempfile::tempdir().unwrap();
        let err = download(
            "https://example.com/live.m3u8",
            &http,
            &dir.path().join("x.part"),
            1,
            &cancelled,
            &tx,
        )
        .unwrap_err();
        assert!(err.contains("HlsDownload"), "{err}");
    }

    #[test]
    fn embeds_basic_tags_into_a_wav() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tone.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 8_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&path, spec).unwrap();
        for _ in 0..800 {
            writer.write_sample(0i16).unwrap();
        }
        writer.finalize().unwrap();

        let track = Track {
            title: "Title".to_string(),
            artist: "Artist".to_string(),
            album: "Album".to_string(),
            ..Default::default()
        };
        embed_metadata(&path, &track, None).unwrap();
        let tagged = lofty::read_from_path(&path).unwrap();
        let tag = tagged
            .primary_tag()
            .or_else(|| tagged.first_tag())
            .expect("tag written");
        assert_eq!(tag.title().as_deref(), Some("Title"));
        assert_eq!(tag.artist().as_deref(), Some("Artist"));
        assert_eq!(tag.album().as_deref(), Some("Album"));
    }

    #[test]
    fn parses_range_headers() {
        assert_eq!(parse_content_range("bytes 80000-199999/200000"), Some((80000, 199999, 200000)));
        assert_eq!(parse_content_range("bytes 0-9/10"), Some((0, 9, 10)));
        assert_eq!(parse_content_range("nonsense"), None);
        assert_eq!(parse_unsatisfied_range("bytes */200000"), Some(200000));
        assert_eq!(parse_unsatisfied_range("bytes 0-1/2"), None);
    }

    #[test]
    fn resumes_a_truncated_download_with_a_range_request() {
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let payload: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let served = payload.clone();
        let server = thread::spawn(move || {
            // First request: announce the full length but close early.
            let (mut socket, _) = listener.accept().unwrap();
            let mut request = [0u8; 4096];
            let count = socket.read(&mut request).unwrap_or(0);
            let head = String::from_utf8_lossy(&request[..count]).to_ascii_lowercase();
            assert!(!head.contains("range:"), "first request must not resume");
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                served.len()
            );
            socket.write_all(header.as_bytes()).unwrap();
            socket.write_all(&served[..80_000]).unwrap();
            drop(socket);

            // Second request must continue from the bytes already on disk.
            let (mut socket, _) = listener.accept().unwrap();
            let count = socket.read(&mut request).unwrap_or(0);
            let head = String::from_utf8_lossy(&request[..count]).to_ascii_lowercase();
            assert!(
                head.contains("range: bytes=80000-"),
                "resume request missing range: {head}"
            );
            let header = format!(
                "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes 80000-199999/{}\r\nConnection: close\r\n\r\n",
                served.len() - 80_000,
                served.len()
            );
            socket.write_all(header.as_bytes()).unwrap();
            socket.write_all(&served[80_000..]).unwrap();
        });

        let dir = tempfile::tempdir().unwrap();
        let part = dir.path().join("track.mp3.part");
        let http = HttpOptions::for_network(&NetworkSettings {
            proxy_mode: crate::config::ProxyMode::None,
            ..Default::default()
        })
        .for_stream();
        let cancelled = Arc::new(Mutex::new(HashSet::new()));
        let (tx, _rx) = unbounded();
        let url = format!("http://{address}/track.mp3");

        let first = download(&url, &http, &part, 1, &cancelled, &tx);
        assert!(first.is_err(), "truncated transfer must fail");
        assert_eq!(fs::metadata(&part).unwrap().len(), 80_000);
        assert!(read_part_meta(&part).is_some());

        let second = download(&url, &http, &part, 2, &cancelled, &tx).unwrap();
        server.join().unwrap();
        assert_eq!(second, Some(200_000));
        assert_eq!(fs::read(&part).unwrap(), payload);
    }

    #[test]
    fn downloads_a_direct_url_into_the_template_path() {
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let payload: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
        let served = payload.clone();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut request = [0u8; 1024];
            let _ = socket.read(&mut request);
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n",
                served.len()
            );
            socket.write_all(header.as_bytes()).unwrap();
            socket.write_all(&served).unwrap();
        });

        let dir = tempfile::tempdir().unwrap();
        let track = Track {
            source: TrackSource::Url {
                url: format!("http://{address}/tone.mp3"),
            },
            title: "Tone".to_string(),
            artist: "Artist".to_string(),
            album: "Album".to_string(),
            ..Default::default()
        };
        let settings = DownloadSettings {
            template: "{artist}/{title}".to_string(),
            embed_tags: false,
            embed_cover: false,
            save_lyrics: false,
            add_to_library: false,
            conflict: ConflictPolicy::Rename,
            ..Default::default()
        };
        let service = DownloadService::new(1);
        let network = NetworkSettings {
            proxy_mode: crate::config::ProxyMode::None,
            ..Default::default()
        };
        service.start(
            7,
            track,
            None,
            None,
            network,
            settings,
            dir.path().to_path_buf(),
        );

        let deadline = Instant::now() + Duration::from_secs(10);
        let mut completed = None;
        let mut failed = None;
        while Instant::now() < deadline {
            for event in service.poll() {
                if let DownloadEvent::State {
                    state, path, error, ..
                } = event
                {
                    match state {
                        DownloadState::Completed => completed = path,
                        DownloadState::Failed => failed = error,
                        _ => {}
                    }
                }
            }
            if completed.is_some() || failed.is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        server.join().unwrap();
        assert!(failed.is_none(), "download failed: {failed:?}");
        let path = completed.expect("download did not complete");
        assert_eq!(path, dir.path().join("Artist").join("Tone.mp3"));
        assert_eq!(fs::read(&path).unwrap(), payload);
    }

    #[test]
    fn downloads_vod_hls_with_the_detected_extension() {
        use std::net::TcpListener;
        use std::thread;

        let first = [0xFFu8, 0xF1, 0x50, 0x80, b'A', b'A', b'A'];
        let second = [0xFFu8, 0xF1, 0x50, 0x80, b'B', b'B', b'B'];
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            for _ in 0..3 {
                let Ok((mut socket, _)) = listener.accept() else {
                    return;
                };
                let mut request = [0u8; 2048];
                let count = socket.read(&mut request).unwrap_or(0);
                let request = String::from_utf8_lossy(&request[..count]);
                let path = request.split_whitespace().nth(1).unwrap_or("/");
                let body: Vec<u8> = match path {
                    "/media.m3u8" => b"#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\nseg0.aac\n#EXTINF:2,\nseg1.aac\n#EXT-X-ENDLIST\n".to_vec(),
                    "/seg0.aac" => first.to_vec(),
                    "/seg1.aac" => second.to_vec(),
                    _ => Vec::new(),
                };
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = socket.write_all(header.as_bytes());
                let _ = socket.write_all(&body);
            }
        });

        let dir = tempfile::tempdir().unwrap();
        let track = Track {
            source: TrackSource::Url {
                url: format!("http://{address}/media.m3u8"),
            },
            title: "HLS".to_string(),
            artist: "Artist".to_string(),
            ..Default::default()
        };
        let settings = DownloadSettings {
            template: "{artist}/{title}".to_string(),
            embed_tags: false,
            embed_cover: false,
            save_lyrics: false,
            add_to_library: false,
            conflict: ConflictPolicy::Rename,
            ..Default::default()
        };
        let service = DownloadService::new(1);
        service.start(
            9,
            track,
            None,
            None,
            NetworkSettings {
                proxy_mode: crate::config::ProxyMode::None,
                ..Default::default()
            },
            settings,
            dir.path().to_path_buf(),
        );

        let deadline = Instant::now() + Duration::from_secs(10);
        let mut completed = None;
        let mut failed = None;
        while Instant::now() < deadline {
            for event in service.poll() {
                if let DownloadEvent::State {
                    state, path, error, ..
                } = event
                {
                    match state {
                        DownloadState::Completed => completed = path,
                        DownloadState::Failed => failed = error,
                        _ => {}
                    }
                }
            }
            if completed.is_some() || failed.is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        server.join().unwrap();
        assert!(failed.is_none(), "download failed: {failed:?}");
        let path = completed.expect("download did not complete");
        assert_eq!(path, dir.path().join("Artist").join("HLS.aac"));
        let mut expected = first.to_vec();
        expected.extend_from_slice(&second);
        assert_eq!(fs::read(&path).unwrap(), expected);
    }
}
