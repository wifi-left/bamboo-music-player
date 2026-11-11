//! HLS (`m3u8`) media source.
//!
//! The engine asks symphonia for one continuous byte stream, while HLS is a
//! playlist of many short files. A background thread resolves the playlist,
//! downloads segments sequentially and appends them to a shared buffer; the
//! decoder-side [`HlsMediaSource`] blocks until bytes are available.
//!
//! Supported segment payloads are AAC in ADTS, MP3 and similar elementary
//! streams, fragmented MP4 with an `#EXT-X-MAP` initialization segment
//! (symphonia reads `moof`/`mdat` pairs from an unseekable stream), and
//! MPEG-TS audio. AES-128 encrypted media (`#EXT-X-KEY`) is decrypted in the
//! fetch loop; the key never reaches the decoder. Segments may use
//! `#EXT-X-BYTERANGE` with or without an explicit offset. Live playlists are
//! refreshed at `#EXT-X-TARGETDURATION`, and consumed bytes are trimmed so a
//! long radio session does not grow without bound.

use aes::cipher::{BlockDecrypt, BlockSizeUser, KeyInit};
use std::collections::HashMap;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use symphonia::core::io::MediaSource;
use url::Url;

use crate::api::HttpOptions;

/// Stop appending when this much audio is buffered but not consumed yet.
/// Prevents an unbounded buffer while playback is paused.
const MAX_UNREAD_BYTES: u64 = 32 * 1024 * 1024;
/// Keep this much already-consumed data available for buffered seeks.
const TRIM_MARGIN: u64 = 4 * 1024 * 1024;
/// Initial data must arrive within this window or opening fails.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(25);
/// A single read while playing waits this long for new data.
const READ_TIMEOUT: Duration = Duration::from_secs(30);

pub fn is_hls_url(url: &str) -> bool {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    path.to_ascii_lowercase().ends_with(".m3u8")
}

pub struct HlsMediaSource {
    shared: Arc<Shared>,
    pos: u64,
}

struct Shared {
    state: Mutex<State>,
    cond: Condvar,
    stop: AtomicBool,
}

struct State {
    data: Vec<u8>,
    /// Absolute byte offset of `data[0]`.
    base: u64,
    /// Absolute byte offset the decoder has consumed.
    consumed: u64,
    finished: bool,
    error: Option<String>,
}

impl Shared {
    fn new() -> Self {
        Self {
            state: Mutex::new(State {
                data: Vec::new(),
                base: 0,
                consumed: 0,
                finished: false,
                error: None,
            }),
            cond: Condvar::new(),
            stop: AtomicBool::new(false),
        }
    }

    fn is_stopped(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    fn append(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let mut state = self.state.lock().unwrap();
        while !self.is_stopped() {
            let end = state.base + state.data.len() as u64;
            if end.saturating_sub(state.consumed) <= MAX_UNREAD_BYTES {
                break;
            }
            let (guard, _) = self
                .cond
                .wait_timeout(state, Duration::from_millis(500))
                .unwrap();
            state = guard;
        }
        if self.is_stopped() {
            return;
        }
        state.data.extend_from_slice(bytes);
        drop(state);
        self.cond.notify_all();
    }

    fn finish(&self) {
        let mut state = self.state.lock().unwrap();
        state.finished = true;
        drop(state);
        self.cond.notify_all();
    }

    fn fail(&self, message: String) {
        tracing::warn!("HLS failed: {message}");
        let mut state = self.state.lock().unwrap();
        state.error = Some(message);
        state.finished = true;
        drop(state);
        self.cond.notify_all();
    }
}

impl HlsMediaSource {
    pub fn open(url: &str, http: HttpOptions, cache: Option<PathBuf>) -> io::Result<Self> {
        let shared = Arc::new(Shared::new());
        let worker = shared.clone();
        let url = url.to_string();
        std::thread::Builder::new()
            .name("bamboo-hls".to_string())
            .spawn(move || {
                if let Err(err) = run(&worker, &url, http, cache) {
                    if !worker.is_stopped() {
                        worker.fail(err.to_string());
                    }
                } else {
                    worker.finish();
                }
            })
            .map_err(|err| {
                io::Error::new(
                    io::ErrorKind::Other,
                    crate::trf!("无法启动 HLS 线程: {}", err),
                )
            })?;

        let deadline = Instant::now() + STARTUP_TIMEOUT;
        let mut state = shared.state.lock().unwrap();
        loop {
            if !state.data.is_empty() {
                break;
            }
            if let Some(error) = &state.error {
                return Err(io::Error::new(io::ErrorKind::Other, error.clone()));
            }
            if state.finished {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    crate::i18n::tr("HLS 播放列表没有音频数据"),
                ));
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    crate::i18n::tr("HLS 流加载超时（检查网络或播放列表）"),
                ));
            }
            let (guard, _) = shared.cond.wait_timeout(state, deadline - now).unwrap();
            state = guard;
        }
        drop(state);
        Ok(Self { shared, pos: 0 })
    }
}

impl Read for HlsMediaSource {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let deadline = Instant::now() + READ_TIMEOUT;
        loop {
            let mut state = self.shared.state.lock().unwrap();
            let offset = self.pos.saturating_sub(state.base);
            if offset < state.data.len() as u64 {
                let start = offset as usize;
                let count = (state.data.len() - start).min(buf.len());
                buf[..count].copy_from_slice(&state.data[start..start + count]);
                self.pos += count as u64;
                state.consumed = state.consumed.max(self.pos);
                // Trim in ~4 MB chunks so a long stream stays bounded while
                // still tolerating small buffered rewinds.
                let keep_from = self.pos.saturating_sub(TRIM_MARGIN);
                if keep_from > state.base {
                    let drop = (keep_from - state.base) as usize;
                    if drop >= 1024 * 1024 && drop <= state.data.len() {
                        state.data.drain(..drop);
                        state.base += drop as u64;
                    }
                }
                drop(state);
                self.shared.cond.notify_all();
                return Ok(count);
            }
            if let Some(error) = &state.error {
                return Err(io::Error::new(io::ErrorKind::Other, error.clone()));
            }
            if state.finished {
                return Ok(0);
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    crate::i18n::tr("HLS 流读取超时（直播可能已断开）"),
                ));
            }
            let (guard, _) = self
                .shared
                .cond
                .wait_timeout(state, deadline - now)
                .unwrap();
            state = guard;
        }
    }
}

impl Seek for HlsMediaSource {
    fn seek(&mut self, target: SeekFrom) -> io::Result<u64> {
        match target {
            SeekFrom::Current(0) => Ok(self.pos),
            SeekFrom::Start(0) => {
                let state = self.shared.state.lock().unwrap();
                if state.base == 0 {
                    self.pos = 0;
                    Ok(0)
                } else {
                    Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        crate::i18n::tr("HLS 直播流不支持跳转"),
                    ))
                }
            }
            _ => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                crate::i18n::tr("HLS 直播流不支持跳转"),
            )),
        }
    }
}

impl MediaSource for HlsMediaSource {
    fn is_seekable(&self) -> bool {
        false
    }

    fn byte_len(&self) -> Option<u64> {
        None
    }
}

impl Drop for HlsMediaSource {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        self.shared.cond.notify_all();
    }
}

// ---------------------------------------------------------------------------
// Playlist model
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ByteRange {
    length: u64,
    /// Absolute offset in the resource. Missing offsets in the playlist are
    /// resolved while parsing, so this is always known here.
    offset: u64,
}

/// Whole-segment encryption applied by `#EXT-X-KEY`. Both AES-128 and the
/// non-standard-but-common AES-256-CBC variant use PKCS#7 padding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KeyMethod {
    Aes128,
    Aes256,
}

impl KeyMethod {
    fn key_len(self) -> usize {
        match self {
            Self::Aes128 => 16,
            Self::Aes256 => 32,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Aes128 => "AES-128",
            Self::Aes256 => "AES-256",
        }
    }
}

/// Encryption key that applies to one or more media segments. `#EXT-X-KEY` is
/// stateful: the tag keeps applying until another one or `METHOD=NONE` appears.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SegmentKey {
    url: String,
    /// Explicit IV from the playlist; absent means "use the media sequence
    /// number as a 128-bit big-endian IV".
    iv: Option<[u8; 16]>,
    method: KeyMethod,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Segment {
    url: String,
    byterange: Option<ByteRange>,
    key: Option<SegmentKey>,
    /// `#EXTINF` duration, used for download progress and rejecting live
    /// playlists. Zero when the tag is absent.
    duration_ms: u64,
    /// `#EXT-X-GAP` segments are intentionally unavailable and must be
    /// skipped without a request.
    gap: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct MediaPlaylist {
    segments: Vec<Segment>,
    /// `#EXT-X-MAP` initialization segment for fragmented MP4 streams.
    init: Option<Segment>,
    target_duration: f64,
    media_sequence: u64,
    endlist: bool,
}

fn run(
    shared: &Shared,
    url: &str,
    http: HttpOptions,
    cache_path: Option<PathBuf>,
) -> Result<(), String> {
    let agent = http.stream_agent();
    let text = fetch_text(&agent, &http, url)
        .map_err(|err| crate::trf!("HLS 播放列表请求失败: {}", err))?;

    let master = is_master_playlist(&text);
    let playlist_url = select_playlist_url(&text, url)?;
    let media_text = if master {
        tracing::info!("HLS master playlist selected {playlist_url}");
        fetch_text(&agent, &http, &playlist_url)
            .map_err(|err| crate::trf!("HLS 媒体播放列表请求失败: {}", err))?
    } else {
        text
    };

    let mut playlist = parse_media_playlist(&media_text, &playlist_url)?;
    let live = !playlist.endlist;
    // Only finished VOD playlists are cacheable: a live stream has no final
    // audio file and may run for hours. The cache is written through the same
    // demuxing path the decoder consumes, so a later open can play it locally.
    let mut cache: Option<CacheWriter> = if live {
        None
    } else {
        cache_path.and_then(|path| match CacheWriter::create(&path) {
            Ok(writer) => Some(writer),
            Err(err) => {
                tracing::warn!("HLS stream cache disabled: {err}");
                None
            }
        })
    };
    let mut next_sequence = if live {
        // Join the live edge instead of replaying the whole window.
        playlist
            .media_sequence
            .saturating_add(playlist.segments.len().saturating_sub(3) as u64)
    } else {
        playlist.media_sequence
    };
    let refresh = Duration::from_secs_f64(playlist.target_duration.clamp(1.0, 10.0) / 2.0);
    let mut first_segment = true;
    let mut appended_init: Option<Segment> = None;
    let mut ts_demux: Option<super::ts_demux::TsDemuxer> = None;
    let mut key_cache: HashMap<String, Vec<u8>> = HashMap::new();

    loop {
        if shared.is_stopped() {
            return Ok(());
        }
        let segments = playlist.segments.clone();
        for (index, segment) in segments.iter().enumerate() {
            if shared.is_stopped() {
                return Ok(());
            }
            let sequence = playlist.media_sequence + index as u64;
            if sequence < next_sequence {
                continue;
            }
            if segment.gap {
                next_sequence = sequence + 1;
                continue;
            }
            // A fragmented MP4 stream needs its `#EXT-X-MAP` initialization
            // segment once, before the first media fragment.
            if let Some(init) = &playlist.init {
                if appended_init.as_ref() != Some(init) {
                    let bytes = fetch_segment(&agent, &http, init)
                        .map_err(|err| crate::trf!("HLS 初始化段下载失败: {}", err))?;
                    if !bytes.is_empty() {
                        append_audio(shared, cache.as_mut(), &bytes)?;
                    }
                    appended_init = Some(init.clone());
                }
            }
            match fetch_segment(&agent, &http, segment) {
                Ok(bytes) if !bytes.is_empty() => {
                    let bytes = match &segment.key {
                        Some(key) => {
                            decrypt_segment(&bytes, key, sequence, &mut key_cache, &agent, &http)
                                .map_err(|err| crate::trf!("HLS 分片解密失败: {}", err))?
                        }
                        None => bytes,
                    };
                    if bytes.is_empty() {
                        next_sequence = sequence + 1;
                        continue;
                    }
                    if first_segment {
                        first_segment = false;
                        if bytes[0] == 0x47 {
                            // MPEG-TS: symphonia has no TS demuxer, so extract
                            // the audio elementary stream ourselves.
                            ts_demux = Some(super::ts_demux::TsDemuxer::new());
                        } else if playlist.init.is_none() {
                            detect_unsupported(&bytes)?;
                        }
                    }
                    if let Some(demuxer) = ts_demux.as_mut() {
                        let mut audio = Vec::new();
                        demuxer.feed(&bytes, &mut audio);
                        if let Some(codec) = demuxer.unsupported_codec() {
                            return Err(crate::trf!(
                                "HLS MPEG-TS 音频编码 {} 暂不支持（请选择 AAC/MP3 音轨的电台）",
                                codec
                            ));
                        }
                        if demuxer.audio_stream_type().is_none() {
                            if demuxer.has_program_map() {
                                return Err(crate::i18n::tr(
                                    "HLS MPEG-TS 分片中没有可解码的音频轨道",
                                )
                                .to_string());
                            }
                            // PAT/PMT may be split across segments; keep
                            // feeding until the table is complete.
                        }
                        if !audio.is_empty() {
                            append_audio(shared, cache.as_mut(), &audio)?;
                        }
                    } else {
                        append_audio(shared, cache.as_mut(), &bytes)?;
                    }
                    next_sequence = sequence + 1;
                }
                Ok(_) => {}
                Err(err) if live => {
                    tracing::warn!("HLS segment skipped ({}): {err}", segment.url);
                    next_sequence = sequence + 1;
                }
                Err(err) => return Err(crate::trf!("HLS 分片下载失败: {}", err)),
            }
        }

        if playlist.endlist {
            if let Some(writer) = cache.take() {
                writer
                    .finish()
                    .map_err(|err| crate::trf!("HLS 流缓存写入失败: {}", err))?;
            }
            return Ok(());
        }

        // Wait for the next media playlist, waking early when stopped.
        let state = shared.state.lock().unwrap();
        let _ = shared.cond.wait_timeout(state, refresh);

        let text = match fetch_text(&agent, &http, &playlist_url) {
            Ok(text) => text,
            Err(err) => {
                tracing::warn!("HLS playlist refresh failed: {err}");
                continue;
            }
        };
        let refreshed = parse_media_playlist(&text, &playlist_url)?;
        if refreshed.segments.is_empty() {
            continue;
        }
        // Sequence numbers normally keep growing; when a server resets the
        // timeline, catch up from the new window instead of skipping forever.
        let end_sequence =
            refreshed.media_sequence + refreshed.segments.len().saturating_sub(1) as u64;
        if end_sequence < next_sequence {
            next_sequence = refreshed.media_sequence;
        }
        playlist = refreshed;
    }
}

/// Append decoded audio to the decoder buffer and, for a VOD stream, mirror
/// the exact bytes into the on-disk cache.
fn append_audio(
    shared: &Shared,
    cache: Option<&mut CacheWriter>,
    bytes: &[u8],
) -> Result<(), String> {
    if let Some(writer) = cache {
        writer
            .write(bytes)
            .map_err(|err| crate::trf!("HLS 流缓存写入失败: {}", err))?;
    }
    shared.append(bytes);
    Ok(())
}

/// Writes a completed VOD stream next to the final cache path. The temporary
/// `.part` file is removed on early stop or error, so a truncated stream can
/// never be mistaken for a complete cache entry.
struct CacheWriter {
    part: PathBuf,
    final_path: PathBuf,
    file: std::fs::File,
    finished: bool,
}

impl CacheWriter {
    fn create(path: &Path) -> io::Result<Self> {
        let part = path.with_extension("part");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::File::create(&part)?;
        Ok(Self {
            part,
            final_path: path.to_path_buf(),
            file,
            finished: false,
        })
    }

    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.file.write_all(bytes)
    }

    fn finish(mut self) -> io::Result<()> {
        self.file.flush()?;
        self.file.sync_all()?;
        // Windows rename fails when the destination exists; the caller only
        // reaches here when no completed cache was present.
        let _ = std::fs::remove_file(&self.final_path);
        std::fs::rename(&self.part, &self.final_path)?;
        self.finished = true;
        Ok(())
    }
}

impl Drop for CacheWriter {
    fn drop(&mut self) {
        if !self.finished {
            let _ = std::fs::remove_file(&self.part);
        }
    }
}

fn is_master_playlist(text: &str) -> bool {
    text.lines()
        .any(|line| line.trim_start().starts_with("#EXT-X-STREAM-INF"))
}

/// Pick the media playlist to use. A master playlist may expose a dedicated
/// audio rendition (`#EXT-X-MEDIA:TYPE=AUDIO`); that is much better for a
/// music player than downloading a video variant and demuxing its audio.
fn select_playlist_url(text: &str, base_url: &str) -> Result<String, String> {
    if !is_master_playlist(text) {
        return Ok(base_url.to_string());
    }
    if let Some(audio) = select_audio_rendition(text, base_url)? {
        return Ok(audio);
    }
    select_variant(text, base_url)
}

fn select_audio_rendition(text: &str, base_url: &str) -> Result<Option<String>, String> {
    let base = Url::parse(base_url).map_err(|err| crate::trf!("HLS 地址无效: {}", err))?;
    let mut best: Option<(u8, String)> = None;
    for line in text.lines() {
        let line = line.trim();
        let Some(attributes) = line.strip_prefix("#EXT-X-MEDIA:") else {
            continue;
        };
        if !attribute_value(attributes, "TYPE")
            .is_some_and(|value| value.eq_ignore_ascii_case("AUDIO"))
        {
            continue;
        }
        let Some(uri) = attribute_value(attributes, "URI") else {
            // The audio track is muxed into the variant itself.
            continue;
        };
        let default = attribute_value(attributes, "DEFAULT")
            .is_some_and(|value| value.eq_ignore_ascii_case("YES"));
        let autoselect = attribute_value(attributes, "AUTOSELECT")
            .is_some_and(|value| value.eq_ignore_ascii_case("YES"));
        let rank = if default {
            2
        } else if autoselect {
            1
        } else {
            0
        };
        if best.as_ref().is_none_or(|(best_rank, _)| rank > *best_rank) {
            let url = base
                .join(uri)
                .map_err(|err| crate::trf!("HLS 音频轨地址无效: {}", err))?
                .to_string();
            best = Some((rank, url));
        }
    }
    Ok(best.map(|(_, url)| url))
}

fn select_variant(text: &str, base_url: &str) -> Result<String, String> {
    let base = Url::parse(base_url).map_err(|err| crate::trf!("HLS 地址无效: {}", err))?;
    // (codec rank, bandwidth, url). Rank prefers audio-only variants so a
    // high-bitrate video rendition does not waste bandwidth or pick a stream
    // symphonia cannot decode.
    let mut best: Option<(u8, u64, String)> = None;
    let mut pending: Option<(u64, Option<String>)> = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(attributes) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            let bandwidth = attribute_value(attributes, "BANDWIDTH")
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(0);
            let codecs = attribute_value(attributes, "CODECS").map(str::to_string);
            pending = Some((bandwidth, codecs));
        } else if !line.is_empty() && !line.starts_with('#') {
            if let Some((bandwidth, codecs)) = pending.take() {
                let Some(rank) = variant_codec_rank(codecs.as_deref()) else {
                    continue;
                };
                let url = base
                    .join(line)
                    .map_err(|err| crate::trf!("HLS 变体地址无效: {}", err))?
                    .to_string();
                if best.as_ref().is_none_or(|(best_rank, best_bw, _)| {
                    (rank, bandwidth) >= (*best_rank, *best_bw)
                }) {
                    best = Some((rank, bandwidth, url));
                }
            }
        }
    }
    best.map(|(_, _, url)| url)
        .ok_or_else(|| crate::i18n::tr("HLS 主播放列表中没有可解码的音频变体").to_string())
}

/// Score a `CODECS` attribute: `None` means the variant has no audio track we
/// can decode. Variants without a `CODECS` attribute get the middle rank and
/// are still attempted.
fn variant_codec_rank(codecs: Option<&str>) -> Option<u8> {
    let Some(codecs) = codecs else {
        return Some(1);
    };
    let codecs: Vec<String> = codecs
        .split(',')
        .map(|codec| codec.trim().trim_matches('"').to_ascii_lowercase())
        .filter(|codec| !codec.is_empty())
        .collect();
    if codecs.is_empty() {
        return Some(1);
    }
    let has_audio = codecs.iter().any(|codec| {
        codec.starts_with("mp4a")
            || codec.starts_with("mp3")
            || codec.starts_with("flac")
            || codec.starts_with("alac")
            || codec.starts_with("vorbis")
            || codec.starts_with("opus")
    });
    if !has_audio {
        return None;
    }
    let has_video = codecs.iter().any(|codec| {
        codec.starts_with("avc1")
            || codec.starts_with("avc3")
            || codec.starts_with("hvc1")
            || codec.starts_with("hev1")
            || codec.starts_with("vp09")
            || codec.starts_with("av01")
    });
    Some(if has_video { 1 } else { 2 })
}

fn parse_media_playlist(text: &str, base_url: &str) -> Result<MediaPlaylist, String> {
    let base = Url::parse(base_url).map_err(|err| crate::trf!("HLS 地址无效: {}", err))?;
    let mut playlist = MediaPlaylist {
        target_duration: 6.0,
        ..Default::default()
    };
    let mut pending_byterange: Option<(u64, Option<u64>)> = None;
    // EXT-X-BYTERANGE may omit the offset, meaning "continue after the
    // previous sub-range of the same resource".
    let mut range_ends: HashMap<String, u64> = HashMap::new();
    let mut current_key: Option<SegmentKey> = None;
    let mut pending_duration_ms = 0u64;
    let mut pending_gap = false;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(value) = line.strip_prefix("#EXT-X-TARGETDURATION:") {
            if let Ok(seconds) = value.trim().parse::<f64>() {
                playlist.target_duration = seconds.max(1.0);
            }
            continue;
        }
        if let Some(value) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            if let Ok(sequence) = value.trim().parse::<u64>() {
                playlist.media_sequence = sequence;
            }
            continue;
        }
        if line == "#EXT-X-ENDLIST" {
            playlist.endlist = true;
            continue;
        }
        if let Some(value) = line.strip_prefix("#EXT-X-KEY:") {
            current_key = parse_key(value, &base)?;
            continue;
        }
        if let Some(value) = line.strip_prefix("#EXT-X-MAP:") {
            let uri = attribute_value(value, "URI")
                .ok_or_else(|| crate::i18n::tr("HLS 初始化段缺少 URI").to_string())?;
            let url = base
                .join(uri)
                .map_err(|err| crate::trf!("HLS 初始化段地址无效: {}", err))?
                .to_string();
            let byterange = attribute_value(value, "BYTERANGE")
                .and_then(|value| parse_byte_range(value).ok())
                .map(|(length, offset)| ByteRange {
                    length,
                    offset: offset.unwrap_or(0),
                });
            playlist.init = Some(Segment {
                url,
                byterange,
                key: None,
                duration_ms: 0,
                gap: false,
            });
            continue;
        }
        if let Some(value) = line.strip_prefix("#EXT-X-BYTERANGE:") {
            pending_byterange = Some(
                parse_byte_range(value.trim())
                    .map_err(|err| crate::trf!("HLS 分片字节范围无效: {}", err))?,
            );
            continue;
        }
        if let Some(value) = line.strip_prefix("#EXTINF:") {
            let seconds = value
                .split(',')
                .next()
                .unwrap_or_default()
                .trim()
                .parse::<f64>()
                .unwrap_or(0.0);
            pending_duration_ms = (seconds.max(0.0) * 1000.0).round() as u64;
            continue;
        }
        if line == "#EXT-X-GAP" {
            pending_gap = true;
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        // Segment URI (EXTINF is conventional but some streams omit it).
        let url = base
            .join(line)
            .map_err(|err| crate::trf!("HLS 分片地址无效: {}", err))?
            .to_string();
        let byterange = match pending_byterange.take() {
            Some((length, Some(offset))) => Some(ByteRange { length, offset }),
            Some((length, None)) => {
                let offset = range_ends.get(&url).copied().unwrap_or(0);
                Some(ByteRange { length, offset })
            }
            None => None,
        };
        if let Some(range) = byterange {
            range_ends.insert(url.clone(), range.offset + range.length);
        }
        playlist.segments.push(Segment {
            url,
            byterange,
            key: current_key.clone(),
            duration_ms: std::mem::take(&mut pending_duration_ms),
            gap: std::mem::take(&mut pending_gap),
        });
    }
    Ok(playlist)
}

/// Parse an `#EXT-X-KEY` tag. `METHOD=NONE` disables encryption again.
fn parse_key(value: &str, base: &Url) -> Result<Option<SegmentKey>, String> {
    let method = attribute_value(value, "METHOD").unwrap_or_default();
    if method.eq_ignore_ascii_case("NONE") {
        return Ok(None);
    }
    let method = if method.eq_ignore_ascii_case("AES-128") {
        KeyMethod::Aes128
    } else if method.eq_ignore_ascii_case("AES-256") {
        KeyMethod::Aes256
    } else {
        return Err(crate::trf!(
            "HLS 加密方式 {} 暂不支持（仅支持 AES-128/AES-256 整段加密）",
            method
        ));
    };
    if let Some(format) = attribute_value(value, "KEYFORMAT")
        && !format.eq_ignore_ascii_case("identity")
    {
        return Err(crate::trf!(
            "HLS 密钥格式 {} 暂不支持（DRM 加密无法解密）",
            format
        ));
    }
    let uri = attribute_value(value, "URI")
        .ok_or_else(|| crate::trf!("HLS {} 密钥缺少 URI", method.name()))?;
    let url = base
        .join(uri)
        .map_err(|err| crate::trf!("HLS 密钥地址无效: {}", err))?
        .to_string();
    let iv = match attribute_value(value, "IV") {
        Some(iv) => Some(parse_iv(iv)?),
        None => None,
    };
    Ok(Some(SegmentKey { url, iv, method }))
}

/// Parse a 128-bit IV written as up to 32 hexadecimal digits.
fn parse_iv(value: &str) -> Result<[u8; 16], String> {
    let text = value.trim();
    let text = text
        .strip_prefix("0x")
        .or_else(|| text.strip_prefix("0X"))
        .unwrap_or(text);
    if text.is_empty() || text.len() > 32 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(crate::i18n::tr("HLS AES-128 IV 无效").to_string());
    }
    let value = u128::from_str_radix(text, 16)
        .map_err(|_| crate::i18n::tr("HLS AES-128 IV 无效").to_string())?;
    Ok(value.to_be_bytes())
}

/// RFC 8216: when a key has no IV, the media sequence number is used as a
/// 128-bit big-endian IV.
fn sequence_iv(sequence: u64) -> [u8; 16] {
    let mut iv = [0u8; 16];
    iv[8..].copy_from_slice(&sequence.to_be_bytes());
    iv
}

fn decrypt_segment(
    data: &[u8],
    key: &SegmentKey,
    sequence: u64,
    cache: &mut HashMap<String, Vec<u8>>,
    agent: &ureq::Agent,
    http: &HttpOptions,
) -> Result<Vec<u8>, String> {
    let key_bytes = match cache.get(&key.url) {
        Some(bytes) => bytes.clone(),
        None => {
            let bytes = fetch_key(agent, http, &key.url)?;
            let expected = key.method.key_len();
            if bytes.len() != expected {
                return Err(crate::trf!(
                    "{} 密钥长度应为 {} 字节，实际 {} 字节",
                    key.method.name(),
                    expected,
                    bytes.len()
                ));
            }
            cache.insert(key.url.clone(), bytes.clone());
            bytes
        }
    };
    let iv = key.iv.unwrap_or_else(|| sequence_iv(sequence));
    let padded = aes_cbc_decrypt(data, &key_bytes, &iv, key.method)?;
    unpad_pkcs7(padded)
}

fn fetch_key(agent: &ureq::Agent, http: &HttpOptions, url: &str) -> Result<Vec<u8>, String> {
    let mut last_error = None;
    for attempt in 0..3 {
        match fetch_bytes(agent, http, url, None) {
            Ok(bytes) => return Ok(bytes),
            Err(err) => {
                last_error = Some(err);
                if attempt < 2 {
                    std::thread::sleep(Duration::from_millis(300 * (attempt + 1) as u64));
                }
            }
        }
    }
    Err(crate::trf!(
        "密钥下载失败: {}",
        last_error
            .map(|err| err.to_string())
            .unwrap_or_else(|| crate::i18n::tr("请求失败").to_string())
    ))
}

/// CBC decryption without padding removal, so it can be checked against
/// standard test vectors.
fn aes_cbc_decrypt(
    data: &[u8],
    key: &[u8],
    iv: &[u8; 16],
    method: KeyMethod,
) -> Result<Vec<u8>, String> {
    match method {
        KeyMethod::Aes128 => aes_cbc_decrypt_with::<aes::Aes128>(data, key, iv, method),
        KeyMethod::Aes256 => aes_cbc_decrypt_with::<aes::Aes256>(data, key, iv, method),
    }
}

fn aes_cbc_decrypt_with<C>(
    data: &[u8],
    key: &[u8],
    iv: &[u8; 16],
    method: KeyMethod,
) -> Result<Vec<u8>, String>
where
    C: BlockDecrypt + KeyInit + BlockSizeUser,
{
    if data.is_empty() {
        return Ok(Vec::new());
    }
    let block_size = C::block_size();
    if data.len() % block_size != 0 {
        return Err(crate::trf!("密文长度不是 {} 字节的整数倍", block_size));
    }
    let cipher = C::new_from_slice(key).map_err(|_| crate::trf!("{} 密钥无效", method.name()))?;
    let mut out = Vec::with_capacity(data.len());
    let mut previous = *iv;
    for chunk in data.chunks_exact(block_size) {
        let mut block = aes::cipher::Block::<C>::clone_from_slice(chunk);
        cipher.decrypt_block(&mut block);
        for index in 0..block_size {
            out.push(block[index] ^ previous[index]);
        }
        previous.copy_from_slice(chunk);
    }
    Ok(out)
}

fn unpad_pkcs7(mut data: Vec<u8>) -> Result<Vec<u8>, String> {
    let Some(&last) = data.last() else {
        return Ok(data);
    };
    let padding = last as usize;
    if padding == 0 || padding > 16 || padding > data.len() {
        return Err(crate::i18n::tr("HLS 分片填充无效").to_string());
    }
    if !data[data.len() - padding..]
        .iter()
        .all(|byte| *byte as usize == padding)
    {
        return Err(crate::i18n::tr("HLS 分片填充无效").to_string());
    }
    data.truncate(data.len() - padding);
    Ok(data)
}

// ---------------------------------------------------------------------------
// On-demand (VOD) download
// ---------------------------------------------------------------------------

/// A resolved on-demand HLS stream ready to be written to one audio file.
///
/// The first media segment is fetched during [`HlsDownload::prepare`] to
/// detect the payload type (AAC/ADTS, MP3, fragmented MP4 or MPEG-TS) and to
/// choose the file extension before the downloader renders the target path.
/// Live playlists are intentionally rejected: they have no end and would never
/// complete a download job.
pub struct HlsDownload {
    agent: ureq::Agent,
    http: HttpOptions,
    playlist: MediaPlaylist,
    key_cache: HashMap<String, Vec<u8>>,
    first_index: usize,
    first_data: Option<Vec<u8>>,
    extension: &'static str,
    ts_demux: Option<super::ts_demux::TsDemuxer>,
    /// Sum of non-gap segment durations, used to estimate total bytes for the
    /// progress bar while the download is running.
    total_duration_ms: u64,
}

impl HlsDownload {
    pub fn prepare(url: &str, http: HttpOptions) -> Result<Self, String> {
        let agent = http.stream_agent();
        let text = fetch_text(&agent, &http, url)
            .map_err(|err| crate::trf!("HLS 播放列表请求失败: {}", err))?;
        let master = is_master_playlist(&text);
        let playlist_url = select_playlist_url(&text, url)?;
        let media_text = if master {
            fetch_text(&agent, &http, &playlist_url)
                .map_err(|err| crate::trf!("HLS 媒体播放列表请求失败: {}", err))?
        } else {
            text
        };
        let playlist = parse_media_playlist(&media_text, &playlist_url)?;
        if !playlist.endlist {
            return Err(crate::i18n::tr(
                "HLS 直播流暂不支持下载（仅支持带 #EXT-X-ENDLIST 的点播流）",
            )
            .to_string());
        }
        if playlist.segments.is_empty() {
            return Err(crate::i18n::tr("HLS 播放列表没有音频分片").to_string());
        }

        let first_index = playlist
            .segments
            .iter()
            .position(|segment| !segment.gap)
            .ok_or_else(|| crate::i18n::tr("HLS 播放列表全部是空分片").to_string())?;
        let first = playlist.segments[first_index].clone();
        let sequence = playlist.media_sequence + first_index as u64;
        let raw = fetch_segment(&agent, &http, &first)
            .map_err(|err| crate::trf!("HLS 首个分片下载失败: {}", err))?;
        let mut key_cache = HashMap::new();
        let decrypted = match &first.key {
            Some(key) => decrypt_segment(&raw, key, sequence, &mut key_cache, &agent, &http)
                .map_err(|err| crate::trf!("HLS 分片解密失败: {}", err))?,
            None => raw,
        };
        if decrypted.is_empty() {
            return Err(crate::i18n::tr("HLS 首个分片为空").to_string());
        }
        let (extension, ts_demux, first_data) = detect_download_audio(&decrypted)?;
        let total_duration_ms: u64 = playlist
            .segments
            .iter()
            .filter(|segment| !segment.gap)
            .map(|segment| segment.duration_ms)
            .sum();
        tracing::info!(
            "HLS download prepared: {} segments, {:.0} s, .{extension}",
            playlist.segments.len(),
            total_duration_ms as f64 / 1000.0
        );

        Ok(Self {
            agent,
            http,
            playlist,
            key_cache,
            first_index,
            first_data: Some(first_data),
            extension,
            ts_demux,
            total_duration_ms,
        })
    }

    pub fn extension(&self) -> &'static str {
        self.extension
    }

    /// Write the stream to `part`. `cancelled` is polled between segments and
    /// `progress` receives (bytes written, total bytes when known).
    pub fn write_to<F, G>(
        &mut self,
        part: &Path,
        mut cancelled: F,
        mut progress: G,
    ) -> Result<Option<u64>, String>
    where
        F: FnMut() -> bool,
        G: FnMut(u64, Option<u64>),
    {
        let mut file =
            std::fs::File::create(part).map_err(|err| crate::trf!("无法创建文件: {}", err))?;
        let mut written = 0u64;
        let mut done_ms = 0u64;
        let mut last_emit = Instant::now();

        if let Some(init) = self.playlist.init.clone() {
            let bytes = fetch_segment(&self.agent, &self.http, &init)
                .map_err(|err| crate::trf!("HLS 初始化段下载失败: {}", err))?;
            if !bytes.is_empty() {
                file.write_all(&bytes)
                    .map_err(|err| crate::trf!("写入失败: {}", err))?;
                written += bytes.len() as u64;
            }
        }

        let segments = self.playlist.segments.clone();
        for (index, segment) in segments.iter().enumerate() {
            if cancelled() {
                return Err("canceled".to_string());
            }
            if segment.gap {
                continue;
            }
            let data = if index == self.first_index {
                self.first_data.take()
            } else {
                let sequence = self.playlist.media_sequence + index as u64;
                let raw = fetch_segment(&self.agent, &self.http, segment)
                    .map_err(|err| crate::trf!("HLS 分片下载失败: {}", err))?;
                Some(match &segment.key {
                    Some(key) => decrypt_segment(
                        &raw,
                        key,
                        sequence,
                        &mut self.key_cache,
                        &self.agent,
                        &self.http,
                    )
                    .map_err(|err| crate::trf!("HLS 分片解密失败: {}", err))?,
                    None => raw,
                })
            };
            if let Some(data) = data {
                let data = match self.ts_demux.as_mut() {
                    Some(demuxer) if index != self.first_index => {
                        let mut audio = Vec::new();
                        demuxer.feed(&data, &mut audio);
                        if let Some(codec) = demuxer.unsupported_codec() {
                            return Err(crate::trf!("HLS MPEG-TS 音频编码 {} 暂不支持下载", codec));
                        }
                        if demuxer.audio_stream_type().is_none() && demuxer.has_program_map() {
                            return Err(crate::i18n::tr("HLS MPEG-TS 分片中没有可解码的音频轨道")
                                .to_string());
                        }
                        audio
                    }
                    // The first TS segment was already demuxed during prepare.
                    _ => data,
                };
                if !data.is_empty() {
                    file.write_all(&data)
                        .map_err(|err| crate::trf!("写入失败: {}", err))?;
                    written += data.len() as u64;
                }
            }
            done_ms += segment.duration_ms;
            if last_emit.elapsed() >= Duration::from_millis(180) {
                last_emit = Instant::now();
                progress(
                    written,
                    estimate_total_bytes(written, done_ms, self.total_duration_ms),
                );
            }
        }
        file.flush()
            .map_err(|err| crate::trf!("写入失败: {}", err))?;
        if written == 0 {
            return Err(crate::i18n::tr("HLS 下载没有音频数据").to_string());
        }
        progress(written, Some(written));
        Ok(Some(written))
    }
}

fn estimate_total_bytes(written: u64, done_ms: u64, total_ms: u64) -> Option<u64> {
    if written == 0 || done_ms == 0 || total_ms == 0 {
        return None;
    }
    let total = (written as u128 * total_ms as u128 / done_ms as u128) as u64;
    Some(total.max(written))
}

fn detect_download_audio(
    bytes: &[u8],
) -> Result<(&'static str, Option<super::ts_demux::TsDemuxer>, Vec<u8>), String> {
    if bytes.first() == Some(&0x47) {
        let mut demuxer = super::ts_demux::TsDemuxer::new();
        let mut audio = Vec::new();
        demuxer.feed(bytes, &mut audio);
        if let Some(codec) = demuxer.unsupported_codec() {
            return Err(crate::trf!("HLS MPEG-TS 音频编码 {} 暂不支持下载", codec));
        }
        let extension = match demuxer.audio_stream_type() {
            Some(0x0F | 0x11) => "aac",
            Some(0x03 | 0x04) => "mp3",
            Some(_) => "aac",
            None => {
                if demuxer.has_program_map() {
                    return Err(
                        crate::i18n::tr("HLS MPEG-TS 分片中没有可解码的音频轨道").to_string()
                    );
                }
                // The first segment only carried a PAT; the PMT may arrive in
                // the next one. Default to AAC for the file extension.
                "aac"
            }
        };
        return Ok((extension, Some(demuxer), audio));
    }
    if bytes.len() >= 8
        && matches!(
            &bytes[4..8],
            b"ftyp" | b"styp" | b"moof" | b"sidx" | b"emsg" | b"prft" | b"mdat"
        )
    {
        return Ok(("m4a", None, bytes.to_vec()));
    }
    if bytes.len() >= 2 && bytes[0] == 0xFF && (bytes[1] & 0xF0) == 0xF0 {
        return Ok(("aac", None, bytes.to_vec()));
    }
    if bytes.len() >= 2 && bytes[0] == 0xFF && (bytes[1] & 0xE0) == 0xE0 {
        return Ok(("mp3", None, bytes.to_vec()));
    }
    Err("无法识别 HLS 分片的音频格式（不支持的分片类型）".to_string())
}

/// Parse `length[@offset]` from `EXT-X-BYTERANGE`.
fn parse_byte_range(value: &str) -> Result<(u64, Option<u64>), String> {
    let value = value.trim();
    let (length, offset) = match value.split_once('@') {
        Some((length, offset)) => (length, Some(offset)),
        None => (value, None),
    };
    let length = length
        .trim()
        .parse::<u64>()
        .map_err(|_| "长度不是有效数字".to_string())?;
    if length == 0 {
        return Err("长度不能为 0".to_string());
    }
    let offset = match offset {
        Some(offset) => Some(
            offset
                .trim()
                .parse::<u64>()
                .map_err(|_| "偏移不是有效数字".to_string())?,
        ),
        None => None,
    };
    Ok((length, offset))
}

/// Read an attribute value from an `#EXT-X-*` tag, honoring quoted commas.
fn attribute_value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let mut rest = text;
    while !rest.is_empty() {
        let (part, remainder) = split_attribute(rest);
        if let Some((name, value)) = part.split_once('=') {
            if name.trim() == key {
                return Some(value.trim().trim_matches('"'));
            }
        }
        rest = remainder;
    }
    None
}

fn split_attribute(text: &str) -> (&str, &str) {
    let mut quoted = false;
    for (index, ch) in text.char_indices() {
        match ch {
            '"' => quoted = !quoted,
            ',' if !quoted => return (&text[..index], &text[index + 1..]),
            _ => {}
        }
    }
    (text, "")
}

fn detect_unsupported(bytes: &[u8]) -> Result<(), String> {
    if bytes.len() >= 8 && &bytes[4..8] == b"ftyp" {
        return Err("HLS 分片 MP4 缺少 EXT-X-MAP 初始化段".to_string());
    }
    Ok(())
}

fn fetch_text(agent: &ureq::Agent, http: &HttpOptions, url: &str) -> io::Result<String> {
    let bytes = fetch_bytes(agent, http, url, None)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn fetch_segment(
    agent: &ureq::Agent,
    http: &HttpOptions,
    segment: &Segment,
) -> io::Result<Vec<u8>> {
    let mut last_error = None;
    for attempt in 0..3 {
        match fetch_bytes(agent, http, &segment.url, segment.byterange) {
            Ok(bytes) => return Ok(bytes),
            Err(err) => {
                last_error = Some(err);
                if attempt < 2 {
                    std::thread::sleep(Duration::from_millis(300 * (attempt + 1) as u64));
                }
            }
        }
    }
    Err(last_error.unwrap_or_else(|| io::Error::new(io::ErrorKind::Other, "分片请求失败")))
}

fn fetch_bytes(
    agent: &ureq::Agent,
    http: &HttpOptions,
    url: &str,
    range: Option<ByteRange>,
) -> io::Result<Vec<u8>> {
    let mut request = http
        .apply(agent.get(url))
        .header("Accept-Encoding", "identity");
    if let Some(range) = range {
        let end = range.offset + range.length - 1;
        request = request.header("Range", format!("bytes={}-{}", range.offset, end));
    }
    let response = request
        .call()
        .map_err(|err| io::Error::new(io::ErrorKind::Other, crate::trf!("请求失败: {}", err)))?;
    let status = response.status().as_u16();
    if status >= 400 {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            format!("HTTP {status}"),
        ));
    }
    let mut bytes = Vec::new();
    response.into_body().into_reader().read_to_end(&mut bytes)?;
    // Servers are allowed to ignore Range and answer with the full entity;
    // slice locally in that case so playback still starts.
    if let Some(range) = range {
        if status == 200 {
            let start = range.offset as usize;
            let end = start.saturating_add(range.length as usize);
            if start >= bytes.len() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "服务器忽略 Range 且响应过短",
                ));
            }
            bytes = bytes[start..end.min(bytes.len())].to_vec();
        }
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_hls_urls() {
        assert!(is_hls_url("https://example.com/live.m3u8"));
        assert!(is_hls_url("https://example.com/live.M3U8?token=1"));
        assert!(!is_hls_url("https://example.com/live.mp3"));
        assert!(!is_hls_url("https://example.com/live"));
    }

    #[test]
    fn parses_media_playlist_with_relative_urls() {
        let text = "\
#EXTM3U
#EXT-X-VERSION:3
#EXT-X-TARGETDURATION:10
#EXT-X-MEDIA-SEQUENCE:42
#EXTINF:9.9,
seg0.aac
#EXTINF:9.9,
sub/seg1.aac
#EXT-X-ENDLIST
";
        let playlist = parse_media_playlist(text, "https://example.com/radio/live.m3u8").unwrap();
        assert_eq!(playlist.target_duration, 10.0);
        assert_eq!(playlist.media_sequence, 42);
        assert!(playlist.endlist);
        assert_eq!(playlist.segments.len(), 2);
        assert_eq!(
            playlist.segments[0].url,
            "https://example.com/radio/seg0.aac"
        );
        assert_eq!(
            playlist.segments[1].url,
            "https://example.com/radio/sub/seg1.aac"
        );
    }

    #[test]
    fn picks_the_highest_bandwidth_variant() {
        let text = "\
#EXTM3U
#EXT-X-STREAM-INF:BANDWIDTH=64000,CODECS=\"mp4a.40.2\"
low/index.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=192000,CODECS=\"mp4a.40.2\"
high/index.m3u8
";
        assert!(is_master_playlist(text));
        assert_eq!(
            select_variant(text, "https://example.com/master.m3u8").unwrap(),
            "https://example.com/high/index.m3u8"
        );
    }

    #[test]
    fn prefers_audio_only_variants_and_skips_video_only() {
        let text = "\
#EXTM3U
#EXT-X-STREAM-INF:BANDWIDTH=4000000,CODECS=\"avc1.64001f,mp4a.40.2\"
video/index.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=128000,CODECS=\"mp4a.40.2\"
audio/index.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=8000000,CODECS=\"avc1.64001f\"
silent/index.m3u8
";
        assert_eq!(
            select_variant(text, "https://example.com/master.m3u8").unwrap(),
            "https://example.com/audio/index.m3u8"
        );

        let video_only =
            "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=8000000,CODECS=\"avc1.64001f\"\nsilent.m3u8\n";
        assert!(
            select_variant(video_only, "https://example.com/master.m3u8")
                .unwrap_err()
                .contains("音频")
        );
    }

    #[test]
    fn attaches_aes_128_key_to_following_segments() {
        let text = "\
#EXTM3U
#EXT-X-TARGETDURATION:5
#EXT-X-KEY:METHOD=AES-128,URI=\"keys/k1.bin\",IV=0x0102030405060708090A0B0C0D0E0F10
#EXTINF:5,
a.aac
#EXTINF:5,
b.aac
#EXT-X-KEY:METHOD=NONE
#EXTINF:5,
c.aac
#EXT-X-ENDLIST
";
        let playlist = parse_media_playlist(text, "https://example.com/radio/a.m3u8").unwrap();
        let key = playlist.segments[0]
            .key
            .as_ref()
            .expect("key on first segment");
        assert_eq!(key.url, "https://example.com/radio/keys/k1.bin");
        assert_eq!(
            key.iv,
            Some([
                0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E,
                0x0F, 0x10
            ])
        );
        assert_eq!(playlist.segments[1].key, playlist.segments[0].key);
        assert!(playlist.segments[2].key.is_none());
    }

    #[test]
    fn accepts_aes_256_and_rejects_sample_aes_and_drm_key_formats() {
        let text = "#EXTM3U\n#EXT-X-KEY:METHOD=AES-256,URI=\"key.bin\"\n#EXTINF:5,\na.aac\n";
        let playlist = parse_media_playlist(text, "https://example.com/a.m3u8").unwrap();
        assert_eq!(
            playlist.segments[0].key.as_ref().map(|key| key.method),
            Some(KeyMethod::Aes256)
        );
        for method in ["SAMPLE-AES", "SAMPLE-AES-CTR", "SAMPLE-AES-CENC"] {
            let text =
                format!("#EXTM3U\n#EXT-X-KEY:METHOD={method},URI=\"key.bin\"\n#EXTINF:5,\na.aac\n");
            let err = parse_media_playlist(&text, "https://example.com/a.m3u8").unwrap_err();
            assert!(err.contains(method), "{err}");
        }
        let key_format = "#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"k\",KEYFORMAT=\"com.apple.streamingkeydelivery\"\n#EXTINF:5,\na.aac\n";
        let err = parse_media_playlist(key_format, "https://example.com/a.m3u8").unwrap_err();
        assert!(err.contains("密钥格式"), "{err}");
        assert!(err.contains("DRM"), "{err}");
    }

    #[test]
    fn decrypts_aes_128_cbc_against_nist_vector() {
        let key = hex_bytes("2b7e151628aed2a6abf7158809cf4f3c");
        let iv = hex_bytes("000102030405060708090a0b0c0d0e0f");
        let ciphertext = hex_bytes(
            "7649abac8119b246cee98e9b12e9197d\
             5086cb9b507219ee95db113a917678b2\
             73bed6b8e3c1743b7116e69e22229516\
             3ff1caa1681fac09120eca307586e1a7",
        );
        let plaintext = hex_bytes(
            "6bc1bee22e409f96e93d7e117393172a\
             ae2d8a571e03ac9c9eb76fac45af8e51\
             30c81c46a35ce411e5fbc1191a0a52ef\
             f69f2445df4f9b17ad2b417be66c3710",
        );
        let key: [u8; 16] = key.try_into().unwrap();
        let iv: [u8; 16] = iv.try_into().unwrap();
        assert_eq!(
            aes_cbc_decrypt(&ciphertext, &key, &iv, KeyMethod::Aes128).unwrap(),
            plaintext
        );
    }

    #[test]
    fn decrypts_aes_256_cbc_against_nist_vector() {
        let key = hex_bytes("603deb1015ca71be2b73aef0857d77811f352c073b6108d72d9810a30914dff4");
        let iv = hex_bytes("000102030405060708090a0b0c0d0e0f");
        let ciphertext = hex_bytes(
            "f58c4c04d6e5f1ba779eabfb5f7bfbd6\
             9cfc4e967edb808d679f777bc6702c7d\
             39f23369a9d9bacfa530e26304231461\
             b2eb05e2c39be9fcda6c19078c6a9d1b",
        );
        let plaintext = hex_bytes(
            "6bc1bee22e409f96e93d7e117393172a\
             ae2d8a571e03ac9c9eb76fac45af8e51\
             30c81c46a35ce411e5fbc1191a0a52ef\
             f69f2445df4f9b17ad2b417be66c3710",
        );
        let iv: [u8; 16] = iv.try_into().unwrap();
        assert_eq!(
            aes_cbc_decrypt(&ciphertext, &key, &iv, KeyMethod::Aes256).unwrap(),
            plaintext
        );
    }

    #[test]
    fn sequence_number_is_the_default_iv() {
        let iv = sequence_iv(0x0102);
        assert_eq!(iv[..14], [0u8; 14]);
        assert_eq!(&iv[14..], &[0x01, 0x02]);
        assert_eq!(parse_iv("0x102").unwrap(), iv);
        assert!(parse_iv("xyz").is_err());
        assert!(parse_iv("0x123456789012345678901234567890123").is_err());
    }

    #[test]
    fn downloads_and_decrypts_aes_128_segments() {
        use std::io::Write;
        use std::net::TcpListener;
        use std::thread;

        let key = hex_bytes("2b7e151628aed2a6abf7158809cf4f3c");
        let iv = hex_bytes("000102030405060708090a0b0c0d0e0f");
        let plaintext = b"ENCRYPTED HLS SEGMENT".to_vec();
        let encrypted = encrypt_aes_128_cbc(&plaintext, &key, &iv);

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server_key = key.clone();
        let server_encrypted = encrypted.clone();
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
                    "/media.m3u8" => b"#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\",IV=0x000102030405060708090A0B0C0D0E0F\n#EXTINF:2,\nseg0.bin\n#EXT-X-ENDLIST\n".to_vec(),
                    "/key.bin" => server_key.clone(),
                    "/seg0.bin" => server_encrypted.clone(),
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

        let http = HttpOptions {
            user_agent: "BambooTest/1.0".to_string(),
            timeout_secs: 5,
            connect_timeout_secs: 2,
            proxy: crate::api::ProxySetting::None,
            headers: Vec::new(),
            cookie: None,
            insecure_tls: false,
        };
        let mut source =
            HlsMediaSource::open(&format!("http://{address}/media.m3u8"), http, None).unwrap();
        let mut downloaded = Vec::new();
        source.read_to_end(&mut downloaded).unwrap();
        assert_eq!(downloaded, plaintext);
        let _ = server.join();
    }

    #[test]
    fn downloads_and_decrypts_aes_256_segments() {
        use std::io::Write;
        use std::net::TcpListener;
        use std::thread;

        let key = hex_bytes("603deb1015ca71be2b73aef0857d77811f352c073b6108d72d9810a30914dff4");
        let iv = hex_bytes("000102030405060708090a0b0c0d0e0f");
        let plaintext = b"ENCRYPTED AES-256 SEGMENT".to_vec();
        let encrypted = encrypt_aes_256_cbc(&plaintext, &key, &iv);

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server_key = key.clone();
        let server_encrypted = encrypted.clone();
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
                    "/media.m3u8" => b"#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-KEY:METHOD=AES-256,URI=\"key.bin\",IV=0x000102030405060708090A0B0C0D0E0F\n#EXTINF:2,\nseg0.bin\n#EXT-X-ENDLIST\n".to_vec(),
                    "/key.bin" => server_key.clone(),
                    "/seg0.bin" => server_encrypted.clone(),
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

        let mut source =
            HlsMediaSource::open(&format!("http://{address}/media.m3u8"), test_http(), None)
                .unwrap();
        let mut downloaded = Vec::new();
        source.read_to_end(&mut downloaded).unwrap();
        assert_eq!(downloaded, plaintext);
        let _ = server.join();
    }

    fn hex_bytes(text: &str) -> Vec<u8> {
        let text: String = text.chars().filter(|ch| !ch.is_whitespace()).collect();
        assert!(text.len() % 2 == 0);
        (0..text.len() / 2)
            .map(|index| u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).unwrap())
            .collect()
    }

    fn encrypt_aes_128_cbc(data: &[u8], key: &[u8], iv: &[u8]) -> Vec<u8> {
        use aes::cipher::BlockEncrypt;
        let cipher = aes::Aes128::new_from_slice(key).unwrap();
        let padding = 16 - data.len() % 16;
        let mut padded = data.to_vec();
        padded.extend(std::iter::repeat_n(padding as u8, padding));
        let mut previous = <[u8; 16]>::try_from(iv).unwrap();
        let mut out = Vec::with_capacity(padded.len());
        for chunk in padded.chunks_exact(16) {
            let mut block =
                aes::cipher::Block::<aes::Aes128>::from(<[u8; 16]>::try_from(chunk).unwrap());
            for index in 0..16 {
                block[index] ^= previous[index];
            }
            cipher.encrypt_block(&mut block);
            previous = <[u8; 16]>::try_from(&block[..]).unwrap();
            out.extend_from_slice(&block);
        }
        out
    }

    fn encrypt_aes_256_cbc(data: &[u8], key: &[u8], iv: &[u8]) -> Vec<u8> {
        use aes::cipher::BlockEncrypt;
        let cipher = aes::Aes256::new_from_slice(key).unwrap();
        let padding = 16 - data.len() % 16;
        let mut padded = data.to_vec();
        padded.extend(std::iter::repeat_n(padding as u8, padding));
        let mut previous = <[u8; 16]>::try_from(iv).unwrap();
        let mut out = Vec::with_capacity(padded.len());
        for chunk in padded.chunks_exact(16) {
            let mut block =
                aes::cipher::Block::<aes::Aes256>::from(<[u8; 16]>::try_from(chunk).unwrap());
            for index in 0..16 {
                block[index] ^= previous[index];
            }
            cipher.encrypt_block(&mut block);
            previous = <[u8; 16]>::try_from(&block[..]).unwrap();
            out.extend_from_slice(&block);
        }
        out
    }

    #[test]
    fn parses_fragmented_mp4_init_segment() {
        let text = "\
#EXTM3U
#EXT-X-TARGETDURATION:4
#EXT-X-MAP:URI=\"init.mp4\",BYTERANGE=\"720@0\"
#EXTINF:4,
seg0.m4s
#EXTINF:4,
seg1.m4s
#EXT-X-ENDLIST
";
        let playlist = parse_media_playlist(text, "https://example.com/media.m3u8").unwrap();
        let init = playlist.init.expect("init segment");
        assert_eq!(init.url, "https://example.com/init.mp4");
        assert_eq!(
            init.byterange,
            Some(ByteRange {
                length: 720,
                offset: 0
            })
        );
        assert_eq!(playlist.segments.len(), 2);
        assert_eq!(playlist.segments[1].url, "https://example.com/seg1.m4s");
    }

    #[test]
    fn parses_byte_range_offsets() {
        let text = "\
#EXTM3U
#EXT-X-TARGETDURATION:2
#EXT-X-BYTERANGE:100@10
bundle.mp3
#EXT-X-BYTERANGE:200
bundle.mp3
#EXT-X-BYTERANGE:50@500
bundle.mp3
#EXT-X-ENDLIST
";
        let playlist = parse_media_playlist(text, "https://example.com/a.m3u8").unwrap();
        assert_eq!(playlist.segments.len(), 3);
        assert_eq!(
            playlist.segments[0].byterange,
            Some(ByteRange {
                length: 100,
                offset: 10
            })
        );
        // Missing offset continues right after the previous sub-range.
        assert_eq!(
            playlist.segments[1].byterange,
            Some(ByteRange {
                length: 200,
                offset: 110
            })
        );
        assert_eq!(
            playlist.segments[2].byterange,
            Some(ByteRange {
                length: 50,
                offset: 500
            })
        );
    }

    #[test]
    fn downloads_playlist_and_segments_in_order() {
        use std::io::Write;
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            // master playlist, media playlist, two segments
            for _ in 0..4 {
                let Ok((mut socket, _)) = listener.accept() else {
                    return;
                };
                let mut request = [0u8; 2048];
                let count = socket.read(&mut request).unwrap_or(0);
                let request = String::from_utf8_lossy(&request[..count]);
                let path = request.split_whitespace().nth(1).unwrap_or("/");
                let body: Vec<u8> = match path {
                    "/master.m3u8" => {
                        b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=128000\n/media.m3u8\n".to_vec()
                    }
                    "/media.m3u8" => {
                        b"#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\n/seg0.bin\n#EXTINF:2,\n/seg1.bin\n#EXT-X-ENDLIST\n".to_vec()
                    }
                    "/seg0.bin" => b"AAA".to_vec(),
                    "/seg1.bin" => b"BBBB".to_vec(),
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

        let http = HttpOptions {
            user_agent: "BambooTest/1.0".to_string(),
            timeout_secs: 5,
            connect_timeout_secs: 2,
            proxy: crate::api::ProxySetting::None,
            headers: Vec::new(),
            cookie: None,
            insecure_tls: false,
        };
        let mut source =
            HlsMediaSource::open(&format!("http://{address}/master.m3u8"), http, None).unwrap();
        let mut downloaded = Vec::new();
        source.read_to_end(&mut downloaded).unwrap();
        assert_eq!(downloaded, b"AAABBBB");
        let _ = server.join();
    }

    #[test]
    fn caches_a_completed_vod_stream_for_local_replay() {
        use std::io::Write;
        use std::net::TcpListener;
        use std::thread;

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
                    "/seg0.aac" => b"AAA".to_vec(),
                    "/seg1.aac" => b"BBBB".to_vec(),
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
        let cache = dir.path().join("radio.aac");
        let mut source = HlsMediaSource::open(
            &format!("http://{address}/media.m3u8"),
            test_http(),
            Some(cache.clone()),
        )
        .unwrap();
        let mut downloaded = Vec::new();
        source.read_to_end(&mut downloaded).unwrap();
        assert_eq!(downloaded, b"AAABBBB");
        assert_eq!(std::fs::read(&cache).unwrap(), b"AAABBBB");
        assert!(!cache.with_extension("part").exists());
        let _ = server.join();
    }

    #[test]
    fn downloads_fragmented_mp4_with_byte_ranges() {
        use std::io::Write;
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            for _ in 0..4 {
                let Ok((mut socket, _)) = listener.accept() else {
                    return;
                };
                let mut request = [0u8; 4096];
                let count = socket.read(&mut request).unwrap_or(0);
                let request = String::from_utf8_lossy(&request[..count]).into_owned();
                let path = request.split_whitespace().nth(1).unwrap_or("/");
                let (full, status): (Vec<u8>, &str) = match path {
                    "/media.m3u8" => (
                        b"#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MAP:URI=\"init.mp4\",BYTERANGE=\"4@2\"\n#EXTINF:2,\n#EXT-X-BYTERANGE:4@3\nseg0.m4s\n#EXTINF:2,\n#EXT-X-BYTERANGE:4@0\nseg1.m4s\n#EXT-X-ENDLIST\n".to_vec(),
                        "200 OK",
                    ),
                    "/init.mp4" => (b"XXINIT".to_vec(), "206 Partial Content"),
                    "/seg0.m4s" => (b"YYYAAAA".to_vec(), "206 Partial Content"),
                    "/seg1.m4s" => (b"BBBB".to_vec(), "206 Partial Content"),
                    _ => (Vec::new(), "404 Not Found"),
                };
                let (body, status, extra) = match request_range(&request) {
                    Some((start, end)) if path != "/media.m3u8" => {
                        let end = end.min(full.len().saturating_sub(1));
                        (
                            full[start.min(full.len())..=end].to_vec(),
                            status,
                            format!("Content-Range: bytes {start}-{end}/{}\r\n", full.len()),
                        )
                    }
                    _ => (full, status, String::new()),
                };
                let header = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n",
                    body.len()
                );
                let _ = socket.write_all(header.as_bytes());
                let _ = socket.write_all(&body);
            }
        });

        let http = HttpOptions {
            user_agent: "BambooTest/1.0".to_string(),
            timeout_secs: 5,
            connect_timeout_secs: 2,
            proxy: crate::api::ProxySetting::None,
            headers: Vec::new(),
            cookie: None,
            insecure_tls: false,
        };
        let mut source =
            HlsMediaSource::open(&format!("http://{address}/media.m3u8"), http, None).unwrap();
        let mut downloaded = Vec::new();
        source.read_to_end(&mut downloaded).unwrap();
        assert_eq!(downloaded, b"INITAAAABBBB");
        let _ = server.join();
    }

    fn request_range(request: &str) -> Option<(usize, usize)> {
        let value = request.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.eq_ignore_ascii_case("range") {
                value.trim().strip_prefix("bytes=")
            } else {
                None
            }
        })?;
        let (start, end) = value.split_once('-')?;
        Some((start.parse().ok()?, end.parse().ok()?))
    }

    fn test_http() -> HttpOptions {
        HttpOptions {
            user_agent: "BambooTest/1.0".to_string(),
            timeout_secs: 5,
            connect_timeout_secs: 2,
            proxy: crate::api::ProxySetting::None,
            headers: Vec::new(),
            cookie: None,
            insecure_tls: false,
        }
    }

    #[test]
    fn prefers_a_dedicated_audio_rendition() {
        let text = "\
#EXTM3U
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"English\",DEFAULT=YES,URI=\"audio/en.m3u8\"
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"Commentary\",URI=\"audio/commentary.m3u8\"
#EXT-X-STREAM-INF:BANDWIDTH=5000000,CODECS=\"avc1.64001f,mp4a.40.2\"
video/index.m3u8
";
        assert_eq!(
            select_playlist_url(text, "https://example.com/master.m3u8").unwrap(),
            "https://example.com/audio/en.m3u8"
        );
        assert_eq!(
            select_playlist_url("not a master", "https://example.com/media.m3u8").unwrap(),
            "https://example.com/media.m3u8"
        );
    }

    #[test]
    fn parses_segment_durations_and_gaps() {
        let text = "\
#EXTM3U
#EXT-X-TARGETDURATION:5
#EXTINF:4.5,
a.aac
#EXT-X-GAP
#EXTINF:2.0,
gap.aac
#EXTINF:3.25,
b.aac
#EXT-X-ENDLIST
";
        let playlist = parse_media_playlist(text, "https://example.com/media.m3u8").unwrap();
        assert_eq!(playlist.segments.len(), 3);
        assert_eq!(playlist.segments[0].duration_ms, 4500);
        assert!(!playlist.segments[0].gap);
        assert!(playlist.segments[1].gap);
        assert_eq!(playlist.segments[1].duration_ms, 2000);
        assert_eq!(playlist.segments[2].duration_ms, 3250);
    }

    #[test]
    fn downloads_vod_hls_into_an_aac_file() {
        use std::io::Write;
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

        let mut download =
            HlsDownload::prepare(&format!("http://{address}/media.m3u8"), test_http()).unwrap();
        assert_eq!(download.extension(), "aac");
        let dir = tempfile::tempdir().unwrap();
        let part = dir.path().join("out.part");
        download.write_to(&part, || false, |_, _| {}).unwrap();
        let mut expected = first.to_vec();
        expected.extend_from_slice(&second);
        assert_eq!(std::fs::read(&part).unwrap(), expected);
        let _ = server.join();
    }

    #[test]
    fn downloads_and_decrypts_aes_vod_hls() {
        use std::io::Write;
        use std::net::TcpListener;
        use std::thread;

        let key = hex_bytes("2b7e151628aed2a6abf7158809cf4f3c");
        let iv = hex_bytes("000102030405060708090a0b0c0d0e0f");
        let plain0 = [0xFFu8, 0xF1, 0x50, 0x80, b'A', b'A', b'A'];
        let plain1 = [0xFFu8, 0xF1, 0x50, 0x80, b'B', b'B', b'B'];
        let encrypted0 = encrypt_aes_128_cbc(&plain0, &key, &iv);
        let encrypted1 = encrypt_aes_128_cbc(&plain1, &key, &iv);

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            for _ in 0..4 {
                let Ok((mut socket, _)) = listener.accept() else {
                    return;
                };
                let mut request = [0u8; 2048];
                let count = socket.read(&mut request).unwrap_or(0);
                let request = String::from_utf8_lossy(&request[..count]);
                let path = request.split_whitespace().nth(1).unwrap_or("/");
                let body: Vec<u8> = match path {
                    "/media.m3u8" => b"#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\",IV=0x000102030405060708090A0B0C0D0E0F\n#EXTINF:2,\nseg0.aac\n#EXTINF:2,\nseg1.aac\n#EXT-X-ENDLIST\n".to_vec(),
                    "/key.bin" => key.clone(),
                    "/seg0.aac" => encrypted0.clone(),
                    "/seg1.aac" => encrypted1.clone(),
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

        let mut download =
            HlsDownload::prepare(&format!("http://{address}/media.m3u8"), test_http()).unwrap();
        assert_eq!(download.extension(), "aac");
        let dir = tempfile::tempdir().unwrap();
        let part = dir.path().join("out.part");
        download.write_to(&part, || false, |_, _| {}).unwrap();
        let mut expected = plain0.to_vec();
        expected.extend_from_slice(&plain1);
        assert_eq!(std::fs::read(&part).unwrap(), expected);
        let _ = server.join();
    }

    #[test]
    fn downloads_ts_vod_as_the_audio_elementary_stream() {
        use std::io::Write;
        use std::net::TcpListener;
        use std::thread;

        let ts = ts_segment(b"TSAUDIO");
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            for _ in 0..2 {
                let Ok((mut socket, _)) = listener.accept() else {
                    return;
                };
                let mut request = [0u8; 2048];
                let count = socket.read(&mut request).unwrap_or(0);
                let request = String::from_utf8_lossy(&request[..count]);
                let path = request.split_whitespace().nth(1).unwrap_or("/");
                let body: Vec<u8> = match path {
                    "/media.m3u8" => {
                        b"#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\nseg0.ts\n#EXT-X-ENDLIST\n"
                            .to_vec()
                    }
                    "/seg0.ts" => ts.clone(),
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

        let mut download =
            HlsDownload::prepare(&format!("http://{address}/media.m3u8"), test_http()).unwrap();
        assert_eq!(download.extension(), "aac");
        let dir = tempfile::tempdir().unwrap();
        let part = dir.path().join("out.part");
        download.write_to(&part, || false, |_, _| {}).unwrap();
        assert_eq!(std::fs::read(&part).unwrap(), b"TSAUDIO");
        let _ = server.join();
    }

    #[test]
    fn downloads_fragmented_mp4_hls_with_init_first() {
        use std::io::Write;
        use std::net::TcpListener;
        use std::thread;

        let init = b"INIT".to_vec();
        let first = b"\x00\x00\x00\x08stypFIRST".to_vec();
        let second = b"\x00\x00\x00\x08moofSECOND".to_vec();
        let server_init = init.clone();
        let server_first = first.clone();
        let server_second = second.clone();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            for _ in 0..4 {
                let Ok((mut socket, _)) = listener.accept() else {
                    return;
                };
                let mut request = [0u8; 2048];
                let count = socket.read(&mut request).unwrap_or(0);
                let request = String::from_utf8_lossy(&request[..count]);
                let path = request.split_whitespace().nth(1).unwrap_or("/");
                let body: Vec<u8> = match path {
                    "/media.m3u8" => b"#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:2,\nseg0.m4s\n#EXTINF:2,\nseg1.m4s\n#EXT-X-ENDLIST\n".to_vec(),
                    "/init.mp4" => server_init.clone(),
                    "/seg0.m4s" => server_first.clone(),
                    "/seg1.m4s" => server_second.clone(),
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

        let mut download =
            HlsDownload::prepare(&format!("http://{address}/media.m3u8"), test_http()).unwrap();
        assert_eq!(download.extension(), "m4a");
        let dir = tempfile::tempdir().unwrap();
        let part = dir.path().join("out.part");
        download.write_to(&part, || false, |_, _| {}).unwrap();
        let mut expected = init;
        expected.extend_from_slice(&first);
        expected.extend_from_slice(&second);
        assert_eq!(std::fs::read(&part).unwrap(), expected);
        let _ = server.join();
    }

    #[test]
    fn rejects_live_hls_downloads() {
        use std::io::Write;
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let Ok((mut socket, _)) = listener.accept() else {
                return;
            };
            let mut request = [0u8; 2048];
            let count = socket.read(&mut request).unwrap_or(0);
            let _ = count;
            let body = b"#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\nseg0.aac\n".to_vec();
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = socket.write_all(header.as_bytes());
            let _ = socket.write_all(&body);
        });

        let err = match HlsDownload::prepare(&format!("http://{address}/live.m3u8"), test_http()) {
            Ok(_) => panic!("live playlists must not be downloadable"),
            Err(err) => err,
        };
        assert!(err.contains("直播"), "{err}");
        let _ = server.join();
    }

    fn ts_segment(audio: &[u8]) -> Vec<u8> {
        const PMT_PID: u16 = 0x1000;
        const AUDIO_PID: u16 = 0x0101;
        let mut out = Vec::new();
        out.extend_from_slice(&psi_packet(0, 0, &pat(PMT_PID)));
        out.extend_from_slice(&psi_packet(PMT_PID, 0, &pmt(&[(0x0F, AUDIO_PID)])));
        out.extend_from_slice(&ts_packet(AUDIO_PID, true, 0, &pes(audio)));
        out
    }

    fn ts_packet(pid: u16, pusi: bool, cc: u8, payload: &[u8]) -> [u8; 188] {
        assert!(payload.len() <= 183);
        let mut packet = [0xFFu8; 188];
        packet[0] = 0x47;
        packet[1] = ((pid >> 8) as u8 & 0x1F) | if pusi { 0x40 } else { 0 };
        packet[2] = pid as u8;
        packet[3] = 0x30 | (cc & 0x0F);
        let af_len = 183 - payload.len();
        packet[4] = af_len as u8;
        if af_len > 0 {
            packet[5] = 0x00;
            for byte in &mut packet[6..5 + af_len] {
                *byte = 0xFF;
            }
        }
        let start = 5 + af_len;
        packet[start..start + payload.len()].copy_from_slice(payload);
        packet
    }

    fn psi_packet(pid: u16, cc: u8, section: &[u8]) -> [u8; 188] {
        let mut payload = Vec::with_capacity(section.len() + 1);
        payload.push(0);
        payload.extend_from_slice(section);
        ts_packet(pid, true, cc, &payload)
    }

    fn pat(pmt_pid: u16) -> Vec<u8> {
        let mut section = vec![0x00, 0xB0, 13, 0x00, 0x01, 0xC1, 0x00, 0x00];
        section.extend_from_slice(&[0x00, 0x01]);
        section.extend_from_slice(&[0xE0 | (pmt_pid >> 8) as u8, pmt_pid as u8]);
        section.extend_from_slice(&[0, 0, 0, 0]);
        section
    }

    fn pmt(streams: &[(u8, u16)]) -> Vec<u8> {
        let section_len = 9 + streams.len() * 5 + 4;
        let mut section = vec![0x02, 0xB0 | (section_len >> 8) as u8, section_len as u8];
        section.extend_from_slice(&[0x00, 0x01, 0xC1, 0x00, 0x00]);
        section.extend_from_slice(&[0xE0, 0x01, 0xF0, 0x00]);
        for (stream_type, pid) in streams {
            section.push(*stream_type);
            section.extend_from_slice(&[0xE0 | (pid >> 8) as u8, *pid as u8, 0xF0, 0x00]);
        }
        section.extend_from_slice(&[0, 0, 0, 0]);
        section
    }

    fn pes(data: &[u8]) -> Vec<u8> {
        let len = 3 + data.len();
        let mut out = vec![0x00, 0x00, 0x01, 0xC0];
        out.extend_from_slice(&(len as u16).to_be_bytes());
        out.extend_from_slice(&[0x00, 0x00, 0x00]);
        out.extend_from_slice(data);
        out
    }
}
