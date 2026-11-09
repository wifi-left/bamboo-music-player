//! Audio engine: symphonia decoding on a worker thread, cpal output, shared
//! ring buffer, seeking and track-end events.
//!
//! The engine intentionally keeps the UI thread free of audio work. The
//! callback only pops samples and applies a smoothed gain; all decoding,
//! resampling and network work happens off the callback thread.
//!
//! The engine uses two decks: `active` is audible, `next` is pre-opened ahead
//! of the track end for gapless playback and equal-power crossfades. Decoding,
//! resampling, EQ and ReplayGain all run on the worker thread; the realtime
//! callback only pops samples and applies a smoothed master gain.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use biquad::{Biquad, Coefficients, DirectForm2Transposed, ToHertz, Type};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig};
use lofty::config::ParseOptions;
use lofty::file::TaggedFileExt;
use lofty::probe::Probe;
use lofty::tag::ItemKey;
use parking_lot::Mutex;
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::Time;

pub mod fingerprint;
pub(crate) mod hls;
mod http_source;
pub mod loudness;
mod time_stretch;
mod ts_demux;

use time_stretch::TimeStretcher;

use crate::api::HttpOptions;
use crate::config::ReplayGainMode;
use http_source::HttpMediaSource;

const TARGET_BUFFER_SECS: f64 = 1.0;
const MAX_BUFFER_SECS: f64 = 2.0;

/// ISO/third-octave-ish center frequencies of the 10-band equalizer.
pub const EQ_BANDS_HZ: [f32; 10] = [
    32.0, 64.0, 125.0, 250.0, 500.0, 1_000.0, 2_000.0, 4_000.0, 8_000.0, 16_000.0,
];

/// Audio processing options mirrored from `Settings::playback`. The app calls
/// [`AudioEngine::set_dsp`] whenever a related setting changes; all fields are
/// safe to change while playing.
#[derive(Clone, Debug)]
pub struct DspConfig {
    pub eq_enabled: bool,
    /// Gain in dB for each band in [`EQ_BANDS_HZ`].
    pub eq_bands: [f32; 10],
    pub preamp_db: f32,
    pub replaygain: ReplayGainMode,
    pub replaygain_preamp_db: f32,
    pub speed: f32,
    /// Time-stretch instead of folding speed into the resampling ratio, so
    /// changing speed does not change pitch.
    pub pitch_preserve: bool,
    pub crossfade_secs: f32,
    pub gapless: bool,
}

impl Default for DspConfig {
    fn default() -> Self {
        Self {
            eq_enabled: false,
            eq_bands: [0.0; 10],
            preamp_db: 0.0,
            replaygain: ReplayGainMode::Off,
            replaygain_preamp_db: 0.0,
            speed: 1.0,
            pitch_preserve: true,
            crossfade_secs: 0.0,
            gapless: true,
        }
    }
}

impl DspConfig {
    fn normalized(mut self) -> Self {
        self.speed = self.speed.clamp(0.25, 4.0);
        self.crossfade_secs = self.crossfade_secs.clamp(0.0, 30.0);
        self.eq_bands = self.eq_bands.map(|g| g.clamp(-24.0, 24.0));
        self.preamp_db = self.preamp_db.clamp(-24.0, 24.0);
        self.replaygain_preamp_db = self.replaygain_preamp_db.clamp(-24.0, 24.0);
        self
    }
}

#[derive(Clone, Debug)]
pub enum EngineSource {
    Local(PathBuf),
    /// Direct HTTP(S) URL with an optional cache destination.
    Url {
        url: String,
        cache: Option<PathBuf>,
        http: HttpOptions,
        /// Download the whole response into `cache` before decoding. Used for
        /// local BambooMusic sources whose PHP server may only serve one
        /// request at a time: after the file is cached, playback stops
        /// occupying the server and the next info/url requests can proceed.
        download_first: bool,
    },
}

impl EngineSource {
    pub fn display(&self) -> String {
        match self {
            Self::Local(path) => path.display().to_string(),
            Self::Url { url, .. } => url.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackState {
    Stopped,
    Loading,
    Playing,
    Paused,
}

impl PlaybackState {
    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Loading,
            2 => Self::Playing,
            3 => Self::Paused,
            _ => Self::Stopped,
        }
    }

    fn as_u8(self) -> u8 {
        match self {
            Self::Stopped => 0,
            Self::Loading => 1,
            Self::Playing => 2,
            Self::Paused => 3,
        }
    }
}

#[derive(Clone, Debug)]
pub enum EngineEvent {
    Loaded {
        token: u64,
        duration: Option<f64>,
        sample_rate: u32,
        channels: u16,
    },
    /// The pre-armed next deck became audible (gapless switch or completed
    /// crossfade). `token` is the value supplied with [`Command::SetNext`].
    AutoAdvanced {
        token: u64,
        duration: Option<f64>,
        sample_rate: u32,
        channels: u16,
    },
    Ended,
    Error(String),
}

#[derive(Debug)]
enum Command {
    Load {
        source: EngineSource,
        start_secs: f64,
        autoplay: bool,
        /// Monotonic id echoed in `EngineEvent::Loaded`; the app discards a
        /// `Loaded` that belongs to a track it has already left.
        token: u64,
    },
    Play,
    Pause,
    Stop,
    Seek(f64),
    SetVolume(f32),
    SetMuted(bool),
    SetReleaseWhenIdle(bool),
    SetDsp(Box<DspConfig>),
    /// Re-read the scanned-gain cache (a background analysis completed).
    RefreshReplayGain,
    /// Pre-open `source` ahead of the current track's end. `None` cancels.
    SetNext {
        source: Option<EngineSource>,
        token: u64,
    },
    /// Create the cpal device/output without loading a file.
    InitOutput,
    Shutdown,
}

#[derive(Clone, Debug)]
pub struct OutputInfo {
    pub device_name: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub sample_format: String,
    /// False until the audio device has been created (lazy init).
    pub ready: bool,
}

impl Default for OutputInfo {
    fn default() -> Self {
        Self {
            device_name: String::new(),
            sample_rate: 44_100,
            channels: 2,
            sample_format: String::new(),
            ready: false,
        }
    }
}

/// Cheap cloneable handle to the live playback clock. Deferred UI viewports
/// (desktop lyrics, standalone lyrics window, mini player) use this so their
/// position keeps advancing even when the main window is not running its
/// per-frame logic.
#[derive(Clone)]
pub struct PlaybackClock {
    shared: Arc<Shared>,
}

impl PlaybackClock {
    pub fn state(&self) -> PlaybackState {
        self.shared.state()
    }

    pub fn position_secs(&self) -> f64 {
        self.shared.position_secs()
    }
}

struct Shared {
    state: AtomicU8,
    volume: AtomicU32,
    muted: AtomicBool,
    release_when_idle: AtomicBool,
    /// Position of the audible stream in media seconds. `position_frames` is
    /// counted in output frames, so speed scaling is kept here as well.
    speed_bits: AtomicU32,
    position_frames: AtomicU64,
    output_rate: AtomicU32,
    output_channels: AtomicU32,
    duration_ms: AtomicU64,
    seek_base_ms: AtomicU64,
    buffer: Mutex<SampleBuffer>,
    underruns: AtomicU64,
    output_info: Mutex<OutputInfo>,
    buffer_target: AtomicU64,
    /// Downsampled mono samples captured by the output callback, used by the
    /// UI visualizer. Written once per callback, read by the UI thread.
    visual: Mutex<VisualTap>,
    /// Whether the UI is currently consuming visualizer samples. The audio
    /// callback skips the visual ring (and its mutex) when this is false.
    visual_enabled: AtomicBool,
}

/// Fixed-size mono sample ring for spectrum/waveform drawing.
struct VisualTap {
    data: Vec<f32>,
    head: usize,
    len: usize,
}

impl VisualTap {
    const CAPACITY: usize = 8192;

    fn new() -> Self {
        Self {
            data: vec![0.0; Self::CAPACITY],
            head: 0,
            len: 0,
        }
    }

    fn push(&mut self, sample: f32) {
        let capacity = self.data.len();
        self.data[self.head] = sample.clamp(-1.0, 1.0);
        self.head = (self.head + 1) % capacity;
        self.len = (self.len + 1).min(capacity);
    }

    /// Oldest-to-newest copy of at most `max` samples into `out`. Reusing the
    /// caller's buffer keeps the visualizer at 30 fps allocation-free.
    fn snapshot_into(&self, out: &mut Vec<f32>, max: usize) {
        out.clear();
        let count = self.len.min(max).min(self.data.len());
        if count == 0 {
            return;
        }
        let capacity = self.data.len();
        let start = (self.head + capacity - count) % capacity;
        out.reserve(count);
        for i in 0..count {
            out.push(self.data[(start + i) % capacity]);
        }
    }
}

impl Shared {
    fn new() -> Self {
        Self {
            state: AtomicU8::new(PlaybackState::Stopped.as_u8()),
            volume: AtomicU32::new(0.8f32.to_bits()),
            muted: AtomicBool::new(false),
            release_when_idle: AtomicBool::new(true),
            speed_bits: AtomicU32::new(1.0f32.to_bits()),
            position_frames: AtomicU64::new(0),
            output_rate: AtomicU32::new(44_100),
            output_channels: AtomicU32::new(2),
            duration_ms: AtomicU64::new(0),
            seek_base_ms: AtomicU64::new(0),
            buffer: Mutex::new(SampleBuffer::new()),
            underruns: AtomicU64::new(0),
            output_info: Mutex::new(OutputInfo::default()),
            buffer_target: AtomicU64::new(44_100),
            visual: Mutex::new(VisualTap::new()),
            visual_enabled: AtomicBool::new(false),
        }
    }

    fn state(&self) -> PlaybackState {
        PlaybackState::from_u8(self.state.load(Ordering::Acquire))
    }

    fn set_state(&self, state: PlaybackState) {
        self.state.store(state.as_u8(), Ordering::Release);
    }

    fn speed(&self) -> f32 {
        f32::from_bits(self.speed_bits.load(Ordering::Relaxed)).clamp(0.25, 4.0)
    }

    fn position_secs(&self) -> f64 {
        let base = self.seek_base_ms.load(Ordering::Relaxed) as f64 / 1000.0;
        let rate = self.output_rate.load(Ordering::Relaxed).max(1) as f64;
        let frames = self.position_frames.load(Ordering::Relaxed) as f64;
        let speed = self.speed() as f64;
        base + frames / rate * speed
    }

    fn duration_secs(&self) -> Option<f64> {
        let ms = self.duration_ms.load(Ordering::Relaxed);
        if ms == 0 {
            None
        } else {
            Some(ms as f64 / 1000.0)
        }
    }

}

/// Interleaved stereo f32 samples in a fixed ring buffer. A ring avoids the
/// per-sample allocation/indices of `VecDeque` in the audio callback, and the
/// storage is only allocated once the output device is actually opened.
struct SampleBuffer {
    data: Vec<f32>,
    head: usize,
    len: usize,
}

impl SampleBuffer {
    fn new() -> Self {
        Self {
            data: Vec::new(),
            head: 0,
            len: 0,
        }
    }

    fn len(&self) -> usize {
        self.len
    }

    fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
    }

    /// Allocate storage rounded up to a power of two so indexing can use a
    /// bit mask instead of a division.
    fn ensure_capacity(&mut self, max_samples: usize) {
        let capacity = max_samples.max(8_192).next_power_of_two();
        if self.data.len() >= capacity {
            return;
        }
        let mut data = vec![0.0f32; capacity];
        if !self.data.is_empty() {
            let old_capacity = self.data.len();
            for i in 0..self.len {
                data[i] = self.data[(self.head + i) % old_capacity];
            }
        }
        self.data = data;
        self.head = 0;
    }

    fn push_frame(&mut self, l: f32, r: f32) {
        let capacity = self.data.len();
        if capacity == 0 || self.len + 2 > capacity {
            return;
        }
        let mask = capacity - 1;
        let tail = (self.head + self.len) & mask;
        self.data[tail] = l;
        self.data[(tail + 1) & mask] = r;
        self.len += 2;
    }

    fn pop_frame(&mut self) -> Option<(f32, f32)> {
        if self.len < 2 {
            return None;
        }
        let capacity = self.data.len();
        let mask = capacity - 1;
        let l = self.data[self.head];
        let r = self.data[(self.head + 1) & mask];
        self.head = (self.head + 2) & mask;
        self.len -= 2;
        Some((l, r))
    }
}

/// Stateful linear resampler. Speed control folds into the ratio, so a faster
/// speed consumes more input frames per output frame (tape-style; pitch is not
/// preserved).
struct LinearResampler {
    step: f64,
    pos: f64,
    last: [f32; 2],
    initialized: bool,
}

/// Input frames consumed per output frame. A faster speed consumes more input
/// per output frame, so a track finishes in less wall-clock time.
fn ratio(src_rate: u32, dst_rate: u32, speed: f32) -> f64 {
    src_rate.max(1) as f64 * (speed.clamp(0.25, 4.0) as f64) / dst_rate.max(1) as f64
}

impl LinearResampler {
    fn new(src_rate: u32, dst_rate: u32, speed: f32) -> Self {
        let step = ratio(src_rate, dst_rate, speed);
        Self {
            step,
            pos: 0.0,
            last: [0.0, 0.0],
            initialized: false,
        }
    }

    fn reset(&mut self) {
        self.pos = 0.0;
        self.initialized = false;
    }

    /// True when resampling and speed control are both bypassed.
    fn is_identity(&self) -> bool {
        (self.step - 1.0).abs() < 1e-9
    }

    fn set_rates(&mut self, src_rate: u32, dst_rate: u32, speed: f32) {
        let step = ratio(src_rate, dst_rate, speed);
        if (step - self.step).abs() < 1e-9 {
            return;
        }
        self.step = step;
        self.reset();
    }

    /// Input is interleaved stereo (mono is duplicated by the caller).
    fn process(&mut self, input: &[[f32; 2]], out: &mut Vec<f32>) {
        for frame in input {
            if !self.initialized {
                self.last = *frame;
                self.initialized = true;
                continue;
            }
            // step < 1 means we upsample: emit several points between frames.
            // step > 1 means downsample: skip some input frames.
            while self.pos < 1.0 {
                let t = self.pos as f32;
                let l = self.last[0] + (frame[0] - self.last[0]) * t;
                let r = self.last[1] + (frame[1] - self.last[1]) * t;
                out.push(l);
                out.push(r);
                self.pos += self.step;
            }
            self.pos -= 1.0;
            self.last = *frame;
        }
    }
}

pub struct AudioEngine {
    tx: crossbeam_channel::Sender<Command>,
    events: crossbeam_channel::Receiver<EngineEvent>,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    next_load_token: AtomicU64,
    /// Forwards engine events to the UI thread and wakes it, so a hidden main
    /// window does not need a periodic polling heartbeat.
    _event_watcher: Option<JoinHandle<()>>,
}

impl AudioEngine {
    pub fn new(
        volume: f32,
        muted: bool,
        release_when_idle: bool,
        dsp: DspConfig,
        wake: Option<Arc<dyn Fn() + Send + Sync>>,
    ) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        let (event_tx, raw_events) = crossbeam_channel::unbounded();
        let (events_tx, events) = crossbeam_channel::unbounded();
        let event_watcher = std::thread::Builder::new()
            .name("bamboo-audio-events".to_string())
            .spawn(move || {
                while let Ok(event) = raw_events.recv() {
                    if events_tx.send(event).is_err() {
                        break;
                    }
                    if let Some(wake) = &wake {
                        wake();
                    }
                }
            })
            .ok();
        let shared = Arc::new(Shared::new());
        shared
            .volume
            .store(volume.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
        shared.muted.store(muted, Ordering::Relaxed);
        shared
            .release_when_idle
            .store(release_when_idle, Ordering::Relaxed);
        shared
            .speed_bits
            .store(dsp.speed.clamp(0.25, 4.0).to_bits(), Ordering::Relaxed);

        let thread_shared = shared.clone();
        let dsp = Box::new(dsp.normalized());
        let thread = std::thread::Builder::new()
            .name("bamboo-audio".to_string())
            .spawn(move || {
                if let Err(err) = run_audio(thread_shared.clone(), rx, event_tx.clone(), *dsp) {
                    tracing::error!("audio thread failed: {err:#}");
                    thread_shared.set_state(PlaybackState::Stopped);
                    let _ = event_tx.send(EngineEvent::Error(err.to_string()));
                }
            })
            .ok();

        Self {
            tx,
            events,
            shared,
            thread,
            next_load_token: AtomicU64::new(0),
            _event_watcher: event_watcher,
        }
    }

    /// Queue a load and return its token. `EngineEvent::Loaded` carries the
    /// same token so the caller can ignore events from superseded loads.
    pub fn load(&self, source: EngineSource, start_secs: f64, autoplay: bool) -> u64 {
        let token = self
            .next_load_token
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        let _ = self.tx.send(Command::Load {
            source,
            start_secs,
            autoplay,
            token,
        });
        token
    }

    pub fn play(&self) {
        let _ = self.tx.send(Command::Play);
    }

    pub fn pause(&self) {
        let _ = self.tx.send(Command::Pause);
    }

    pub fn stop(&self) {
        let _ = self.tx.send(Command::Stop);
    }

    pub fn seek(&self, secs: f64) {
        let _ = self.tx.send(Command::Seek(secs.max(0.0)));
    }

    pub fn set_volume(&self, volume: f32) {
        let v = volume.clamp(0.0, 1.0);
        self.shared.volume.store(v.to_bits(), Ordering::Relaxed);
        let _ = self.tx.send(Command::SetVolume(v));
    }

    pub fn set_muted(&self, muted: bool) {
        self.shared.muted.store(muted, Ordering::Relaxed);
        let _ = self.tx.send(Command::SetMuted(muted));
    }

    pub fn set_release_when_idle(&self, release: bool) {
        self.shared
            .release_when_idle
            .store(release, Ordering::Relaxed);
        let _ = self.tx.send(Command::SetReleaseWhenIdle(release));
    }

    /// Update EQ / ReplayGain / speed / crossfade settings at runtime.
    pub fn set_dsp(&self, dsp: DspConfig) {
        self.shared
            .speed_bits
            .store(dsp.speed.clamp(0.25, 4.0).to_bits(), Ordering::Relaxed);
        let _ = self.tx.send(Command::SetDsp(Box::new(dsp.normalized())));
    }

    /// Re-apply ReplayGain after an offline loudness scan populated the cache.
    pub fn refresh_replaygain(&self) {
        let _ = self.tx.send(Command::RefreshReplayGain);
    }

    /// Pre-open the next source for gapless/crossfade. The app re-arms this
    /// whenever the queue or playback settings change; `None` cancels.
    pub fn set_next(&self, source: Option<EngineSource>, token: u64) {
        let _ = self.tx.send(Command::SetNext { source, token });
    }

    /// Create the output device now instead of waiting for the first load.
    pub fn warm_up(&self) {
        let _ = self.tx.send(Command::InitOutput);
    }

    pub fn state(&self) -> PlaybackState {
        self.shared.state()
    }

    pub fn position_secs(&self) -> f64 {
        self.shared.position_secs()
    }

    pub fn duration_secs(&self) -> Option<f64> {
        self.shared.duration_secs()
    }

    /// Cloneable live clock for deferred viewports that cannot borrow `App`.
    pub fn clock(&self) -> PlaybackClock {
        PlaybackClock {
            shared: self.shared.clone(),
        }
    }

    pub fn output_info(&self) -> OutputInfo {
        self.shared.output_info.lock().clone()
    }

    /// Fill `out` with the latest mono samples (oldest→newest) for the
    /// visualizer. The callback clamps and stores at most 8192 samples
    /// (~170 ms at 48 kHz); reusing `out` avoids an allocation per UI frame.
    pub fn fill_visual_samples(&self, out: &mut Vec<f32>, max: usize) {
        self.shared.visual.lock().snapshot_into(out, max);
    }

    /// Tell the realtime callback whether the visualizer is visible. Disabling
    /// it removes a mutex acquire and two ring writes from every output frame.
    pub fn set_visual_enabled(&self, enabled: bool) {
        self.shared.visual_enabled.store(enabled, Ordering::Relaxed);
    }

    pub fn poll_events(&self) -> Vec<EngineEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            events.push(event);
        }
        events
    }

    pub fn shutdown(&mut self) {
        let _ = self.tx.send(Command::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Shutdown);
    }
}

struct AudioOutput {
    stream: cpal::Stream,
    paused: bool,
}

fn run_audio(
    shared: Arc<Shared>,
    rx: crossbeam_channel::Receiver<Command>,
    event_tx: crossbeam_channel::Sender<EngineEvent>,
    dsp: DspConfig,
) -> Result<()> {
    let mut player = Player::new(shared.clone(), event_tx.clone(), dsp);
    // The device and output stream are created on demand: launching the app
    // with a paused session no longer pays WASAPI init or allocates buffers.
    let mut output: Option<AudioOutput> = None;

    loop {
        // While playing the engine pumps decoded audio into the ring buffer
        // frequently. When paused/stopped there is no periodic work at all:
        // every command wakes `recv_timeout` immediately, so a long timeout
        // only removes idle wake-ups (~2/s instead of ~8/s) without adding
        // latency to Play/Seek/Pause.
        let timeout = if shared.state() == PlaybackState::Playing {
            Duration::from_millis(20)
        } else {
            Duration::from_millis(500)
        };
        match rx.recv_timeout(timeout) {
            Ok(cmd) => match cmd {
                Command::Load {
                    source,
                    start_secs,
                    autoplay,
                    token,
                } => match ensure_output(&mut output, &shared) {
                    Ok(()) => {
                        if let Err(err) = player.load(source, start_secs, autoplay, token) {
                            tracing::warn!("load failed: {err:#}");
                            shared.set_state(PlaybackState::Stopped);
                            let _ = event_tx.send(EngineEvent::Error(err.to_string()));
                        }
                    }
                    Err(err) => {
                        tracing::warn!("audio output init failed: {err:#}");
                        shared.set_state(PlaybackState::Stopped);
                        let _ = event_tx.send(EngineEvent::Error(err.to_string()));
                    }
                },
                Command::Play => match ensure_output(&mut output, &shared) {
                    Ok(()) => player.play(),
                    Err(err) => {
                        tracing::warn!("audio output init failed: {err:#}");
                        shared.set_state(PlaybackState::Stopped);
                        let _ = event_tx.send(EngineEvent::Error(err.to_string()));
                    }
                },
                Command::Pause => player.pause(),
                Command::Stop => player.stop(),
                Command::Seek(secs) => {
                    if let Err(err) = player.seek(secs) {
                        tracing::warn!("seek failed: {err:#}");
                    }
                }
                Command::SetVolume(v) => {
                    shared.volume.store(v.to_bits(), Ordering::Relaxed);
                }
                Command::SetMuted(m) => {
                    shared.muted.store(m, Ordering::Relaxed);
                }
                Command::SetReleaseWhenIdle(release) => {
                    shared.release_when_idle.store(release, Ordering::Relaxed);
                }
                Command::SetDsp(config) => player.set_dsp(*config),
                Command::RefreshReplayGain => player.refresh_replaygain(),
                Command::SetNext { source, token } => player.set_next(source, token),
                Command::InitOutput => {
                    if let Err(err) = ensure_output(&mut output, &shared) {
                        tracing::warn!("audio output init failed: {err:#}");
                        let _ = event_tx.send(EngineEvent::Error(err.to_string()));
                    }
                }
                Command::Shutdown => break,
            },
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }

        if shared.state() == PlaybackState::Playing {
            player.pump();
        }
        player.maybe_finish();
        sync_output(&mut output, &shared);
    }

    player.stop();
    drop(output);
    Ok(())
}

fn ensure_output(output: &mut Option<AudioOutput>, shared: &Arc<Shared>) -> Result<()> {
    if output.is_some() {
        return Ok(());
    }
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| anyhow!(crate::i18n::tr("没有可用的音频输出设备")))?;
    let device_name = device
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_else(|_| crate::i18n::tr("默认输出设备").to_string());
    let supported = device
        .default_output_config()
        .context(crate::i18n::tr("无法获取默认音频输出配置"))?;
    let sample_format = supported.sample_format();
    let config: StreamConfig = supported.config();
    let out_rate = config.sample_rate;
    let out_channels = config.channels;
    shared.output_rate.store(out_rate, Ordering::Relaxed);
    shared
        .output_channels
        .store(out_channels as u32, Ordering::Relaxed);
    shared.buffer_target.store(
        (out_rate as f64 * TARGET_BUFFER_SECS * 2.0) as u64,
        Ordering::Relaxed,
    );
    shared
        .buffer
        .lock()
        .ensure_capacity((out_rate as f64 * MAX_BUFFER_SECS * 2.0) as usize);
    *shared.output_info.lock() = OutputInfo {
        device_name,
        sample_rate: out_rate,
        channels: out_channels,
        sample_format: format!("{sample_format:?}"),
        ready: true,
    };

    let stream = match sample_format {
        SampleFormat::F32 => build_stream::<f32>(&device, config.clone(), shared.clone()),
        SampleFormat::I16 => build_stream::<i16>(&device, config.clone(), shared.clone()),
        SampleFormat::U16 => build_stream::<u16>(&device, config.clone(), shared.clone()),
        SampleFormat::I32 => build_stream::<i32>(&device, config.clone(), shared.clone()),
        SampleFormat::F64 => build_stream::<f64>(&device, config.clone(), shared.clone()),
        other => Err(anyhow!(crate::trf!(
            "不支持的输出采样格式: {}",
            format!("{other:?}")
        ))),
    }?;
    // cpal streams start paused. sync_output() starts the stream as soon as
    // the engine is actually playing.
    *output = Some(AudioOutput {
        stream,
        paused: true,
    });
    Ok(())
}

/// Start the output stream when playing and pause it while idle. Keeping the
/// stream paused stops WASAPI callbacks entirely, which removes the idle CPU
/// wakeups and most of the standby power draw.
fn sync_output(output: &mut Option<AudioOutput>, shared: &Shared) {
    let Some(output) = output.as_mut() else {
        return;
    };
    let want_playing = shared.state() == PlaybackState::Playing;
    if want_playing && output.paused {
        match output.stream.play() {
            Ok(()) => output.paused = false,
            Err(err) => tracing::warn!("audio resume failed: {err}"),
        }
    } else if !want_playing && !output.paused && shared.release_when_idle.load(Ordering::Relaxed) {
        match output.stream.pause() {
            Ok(()) => output.paused = true,
            Err(err) => tracing::warn!("audio pause failed: {err}"),
        }
    }
}

fn build_stream<T>(
    device: &cpal::Device,
    config: StreamConfig,
    shared: Arc<Shared>,
) -> Result<cpal::Stream>
where
    T: SizedSample + FromSample<f32> + Send + 'static,
{
    let out_channels = config.channels as usize;
    let mut gain = f32::from_bits(shared.volume.load(Ordering::Relaxed));
    let err_fn = |err| {
        tracing::warn!("audio output error: {err}");
    };
    let stream = device
        .build_output_stream(
            config,
            move |data: &mut [T], _| {
                let state = shared.state();
                let playing = state == PlaybackState::Playing;
                let target = if shared.muted.load(Ordering::Relaxed) {
                    0.0
                } else {
                    f32::from_bits(shared.volume.load(Ordering::Relaxed))
                };
                let frames = data.len() / out_channels;
                if playing {
                    let mut buffer = shared.buffer.lock();
                    let mut visual = shared
                        .visual_enabled
                        .load(Ordering::Relaxed)
                        .then(|| shared.visual.lock());
                    let mut underrun = false;
                    for frame in data.chunks_mut(out_channels) {
                        let (l, r) = match buffer.pop_frame() {
                            Some(pair) => pair,
                            None => {
                                underrun = true;
                                (0.0, 0.0)
                            }
                        };
                        if let Some(visual) = visual.as_mut() {
                            visual.push((l + r) * 0.5);
                        }
                        gain += (target - gain) * 0.0015;
                        let l = l * gain;
                        let r = r * gain;
                        if out_channels == 1 {
                            frame[0] = T::from_sample((l + r) * 0.5);
                        } else {
                            frame[0] = T::from_sample(l);
                            frame[1] = T::from_sample(r);
                            for sample in frame.iter_mut().skip(2) {
                                *sample = T::from_sample(0.0);
                            }
                        }
                    }
                    drop(visual);
                    drop(buffer);
                    if underrun {
                        shared.underruns.fetch_add(1, Ordering::Relaxed);
                    }
                    shared
                        .position_frames
                        .fetch_add(frames as u64, Ordering::Relaxed);
                } else {
                    // Silence while paused/loading/stopped. Do not advance the position.
                    for sample in data.iter_mut() {
                        *sample = T::from_sample(0.0);
                    }
                    gain = target;
                }
            },
            err_fn,
            None,
        )
        .context(crate::i18n::tr("无法创建音频输出流"))?;
    Ok(stream)
}

/// A 10-band peaking equalizer applied to the mixed stereo stream.
struct EqChain {
    sample_rate: u32,
    enabled: bool,
    preamp: f32,
    bands: Vec<[DirectForm2Transposed<f32>; 2]>,
}

impl EqChain {
    fn new() -> Self {
        Self {
            sample_rate: 0,
            enabled: false,
            preamp: 1.0,
            bands: Vec::new(),
        }
    }

    fn configure(&mut self, sample_rate: u32, dsp: &DspConfig) {
        if sample_rate == 0 {
            return;
        }
        self.sample_rate = sample_rate;
        self.bands.clear();
        if !dsp.eq_enabled {
            self.enabled = false;
            self.preamp = 1.0;
            return;
        }
        self.preamp = db_to_gain(dsp.preamp_db);
        let fs = sample_rate as f32;
        for (index, band) in dsp.eq_bands.iter().enumerate() {
            if band.abs() < 0.05 {
                continue;
            }
            // Stay below Nyquist for low sample-rate files (e.g. 32 kHz).
            let f0 = EQ_BANDS_HZ[index].min(fs * 0.45);
            let coeffs =
                Coefficients::<f32>::from_params(Type::PeakingEQ(*band), fs.hz(), f0.hz(), 0.9);
            if let Ok(coeffs) = coeffs {
                self.bands.push([
                    DirectForm2Transposed::<f32>::new(coeffs),
                    DirectForm2Transposed::<f32>::new(coeffs),
                ]);
            }
        }
        self.enabled = !self.bands.is_empty() || dsp.preamp_db.abs() > 0.05;
    }

    fn process(&mut self, l: f32, r: f32) -> (f32, f32) {
        if !self.enabled {
            return (l, r);
        }
        let mut left = l * self.preamp;
        let mut right = r * self.preamp;
        for [filter_l, filter_r] in &mut self.bands {
            left = filter_l.run(left);
            right = filter_r.run(right);
        }
        (left.clamp(-1.0, 1.0), right.clamp(-1.0, 1.0))
    }

    fn reset(&mut self) {
        // Rebuilding the filters clears their delay lines; this is called on
        // seek so a stale state cannot click.
        let sample_rate = self.sample_rate;
        if sample_rate == 0 {
            return;
        }
        let bands = self.bands.len();
        self.bands.clear();
        if bands > 0 {
            // Coefficients are unknown here; the next `configure` rebuilds.
        }
    }
}

/// One independent decoder with its own seek point, speed-aware resampler and
/// per-track ReplayGain. Decks are moved between `active`/`next`, never cloned.
struct Deck {
    format: Box<dyn FormatReader>,
    decoder: Option<Box<dyn AudioDecoder>>,
    track_id: u32,
    src_rate: u32,
    src_channels: usize,
    out_rate: u32,
    speed: f32,
    pitch_preserve: bool,
    resampler: LinearResampler,
    /// Present when speed != 1 and pitch is preserved. It runs before the
    /// resampler, which then only converts the sample rate.
    stretcher: Option<TimeStretcher>,
    eof: bool,
    time_base: Option<symphonia::core::units::TimeBase>,
    /// Output frames that should be dropped first (accurate seek alignment).
    drop_samples: usize,
    duration: Option<f64>,
    produced_frames: u64,
    /// Media position at which `produced_frames` started counting. Rebasing
    /// this on speed changes/seeks keeps fade timing exact.
    position_base: f64,
    gain: f32,
    path: Option<PathBuf>,
    rg_track: Option<f32>,
    rg_album: Option<f32>,
    /// Gain from an offline EBU R128 scan. Only used when the file has no
    /// ReplayGain tags, keeping tagged files authoritative.
    rg_scan: Option<f32>,
    interleaved: Vec<f32>,
    stereo: Vec<[f32; 2]>,
    stretched: Vec<[f32; 2]>,
    resampled: Vec<f32>,
}

impl Deck {
    fn new(source: &EngineSource, out_rate: u32, dsp: &DspConfig) -> Result<Self> {
        let (format, decoder, track_id, src_rate, src_channels, duration, time_base) =
            open_source(source, 0.0)?;
        let path = match source {
            EngineSource::Local(path) => Some(path.clone()),
            EngineSource::Url { .. } => None,
        };
        let mut deck = Self {
            format,
            decoder: Some(decoder),
            track_id,
            src_rate: src_rate.max(1),
            src_channels: src_channels.max(1),
            out_rate: out_rate.max(1),
            speed: dsp.speed,
            pitch_preserve: dsp.pitch_preserve,
            resampler: LinearResampler::new(src_rate, out_rate, 1.0),
            stretcher: None,
            eof: false,
            time_base,
            drop_samples: 0,
            duration,
            produced_frames: 0,
            position_base: 0.0,
            gain: 1.0,
            path,
            rg_track: None,
            rg_album: None,
            rg_scan: None,
            interleaved: Vec::with_capacity(8192),
            stereo: Vec::with_capacity(4096),
            stretched: Vec::with_capacity(8192),
            resampled: Vec::with_capacity(8192),
        };
        deck.configure_speed(dsp.speed, dsp.pitch_preserve);
        deck.refresh_gain(dsp);
        Ok(deck)
    }

    /// Route speed through the WSOLA stretcher while preserving pitch, or
    /// fold it into the resampling ratio (tape-style).
    fn configure_speed(&mut self, speed: f32, pitch_preserve: bool) {
        let stretch = pitch_preserve && (speed - 1.0).abs() > 1e-3;
        self.speed = speed;
        self.pitch_preserve = pitch_preserve;
        if stretch {
            match self.stretcher.as_mut() {
                Some(stretcher) => stretcher.set_speed(speed),
                None => self.stretcher = Some(TimeStretcher::new(self.src_rate, speed)),
            }
            self.resampler.set_rates(self.src_rate, self.out_rate, 1.0);
        } else {
            self.stretcher = None;
            self.resampler
                .set_rates(self.src_rate, self.out_rate, speed);
        }
    }

    fn position_secs(&self, speed: f32) -> f64 {
        self.position_base
            + self.produced_frames as f64 / self.out_rate.max(1) as f64 * speed as f64
    }

    fn load_replaygain(&mut self) {
        let Some(path) = self.path.clone() else {
            return;
        };
        if self.rg_scan.is_none() {
            self.rg_scan = loudness::cached_gain(&path);
        }
        if self.rg_track.is_some() || self.rg_album.is_some() {
            return;
        }
        let options = ParseOptions::new().read_cover_art(false);
        let Ok(tagged) = Probe::open(&path).and_then(|probe| probe.options(options).read()) else {
            return;
        };
        let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) else {
            return;
        };
        self.rg_track = tag
            .get_string(ItemKey::ReplayGainTrackGain)
            .and_then(parse_gain_db);
        self.rg_album = tag
            .get_string(ItemKey::ReplayGainAlbumGain)
            .and_then(parse_gain_db);
    }

    fn refresh_gain(&mut self, dsp: &DspConfig) {
        if dsp.replaygain != ReplayGainMode::Off {
            self.load_replaygain();
        }
        let db = match dsp.replaygain {
            ReplayGainMode::Off => 0.0,
            ReplayGainMode::Track => {
                self.rg_track
                    .or(self.rg_album)
                    .or(self.rg_scan)
                    .unwrap_or(0.0)
                    + dsp.replaygain_preamp_db
            }
            ReplayGainMode::Album => {
                self.rg_album
                    .or(self.rg_track)
                    .or(self.rg_scan)
                    .unwrap_or(0.0)
                    + dsp.replaygain_preamp_db
            }
        };
        // Keep extreme tags from producing an unusable output.
        self.gain = db_to_gain(db).clamp(0.02, 8.0);
    }

    fn seek(&mut self, secs: f64) -> Result<()> {
        let target = secs.max(0.0);
        let time = Time::try_from_secs_f64(target)
            .ok_or_else(|| anyhow!(crate::i18n::tr("无效的跳转位置")))?;
        self.resampler.reset();
        if let Some(stretcher) = self.stretcher.as_mut() {
            stretcher.reset(self.src_rate);
        }
        self.eof = false;
        self.drop_samples = 0;
        self.produced_frames = 0;
        self.position_base = target;

        let seeked = self
            .format
            .seek(
                SeekMode::Accurate,
                SeekTo::Time {
                    time,
                    track_id: Some(self.track_id),
                },
            )
            .context(crate::i18n::tr("跳转失败"))?;
        if let Some(decoder) = self.decoder.as_mut() {
            decoder.reset();
        }
        let actual = self
            .time_base
            .and_then(|tb| tb.calc_time(seeked.actual_ts))
            .map(|t| t.as_secs_f64())
            .unwrap_or(0.0);
        let delta = (target - actual).max(0.0);
        if delta > 0.001 {
            self.drop_samples = (delta * self.out_rate as f64 * 2.0) as usize;
        }
        Ok(())
    }

    /// Decode up to roughly `max_frames` stereo frames and append them to
    /// `out` with this deck's ReplayGain applied.
    fn fill_frames(&mut self, out: &mut Vec<[f32; 2]>, max_frames: usize) {
        let start_len = out.len();
        while out.len() < start_len + max_frames && !self.eof {
            let packet = match self.format.next_packet() {
                Ok(Some(packet)) => packet,
                Ok(None) | Err(SymphoniaError::ResetRequired) => {
                    self.eof = true;
                    break;
                }
                Err(SymphoniaError::IoError(err)) => {
                    tracing::warn!("read error: {err}");
                    self.eof = true;
                    break;
                }
                Err(err) => {
                    tracing::warn!("format error: {err}");
                    self.eof = true;
                    break;
                }
            };
            if packet.track_id != self.track_id {
                continue;
            }
            let Some(decoder) = self.decoder.as_mut() else {
                break;
            };
            match decoder.decode(&packet) {
                Ok(audio) => {
                    let spec = audio.spec().clone();
                    let rate = spec.rate();
                    let channels = spec.channels().count().max(1);
                    if rate != self.src_rate || channels != self.src_channels {
                        self.src_rate = rate.max(1);
                        self.src_channels = channels;
                        if let Some(stretcher) = self.stretcher.as_mut() {
                            stretcher.reset(self.src_rate);
                        }
                        let resample_speed = if self.stretcher.is_some() {
                            1.0
                        } else {
                            self.speed
                        };
                        self.resampler
                            .set_rates(self.src_rate, self.out_rate, resample_speed);
                    }
                    self.interleaved.clear();
                    audio.copy_to_vec_interleaved(&mut self.interleaved);
                    if self.interleaved.is_empty() {
                        continue;
                    }
                    let frames = self.interleaved.len() / self.src_channels;
                    self.stereo.clear();
                    self.stereo.reserve(frames);
                    for i in 0..frames {
                        let base = i * self.src_channels;
                        let l = self.interleaved.get(base).copied().unwrap_or(0.0);
                        let r = if self.src_channels >= 2 {
                            self.interleaved.get(base + 1).copied().unwrap_or(l)
                        } else {
                            l
                        };
                        self.stereo.push([l, r]);
                    }

                    let Self {
                        stretcher,
                        stereo,
                        stretched,
                        resampler,
                        resampled,
                        drop_samples,
                        ..
                    } = self;
                    match stretcher.as_mut() {
                        Some(stretcher) => {
                            stretched.clear();
                            stretcher.process(stereo, stretched, false);
                            push_resampled(resampler, stretched, resampled, out, drop_samples);
                        }
                        None => {
                            push_resampled(resampler, stereo, resampled, out, drop_samples);
                        }
                    }
                }
                Err(SymphoniaError::DecodeError(err)) => {
                    tracing::debug!("decode error (skipped): {err}");
                }
                Err(SymphoniaError::IoError(err)) => {
                    tracing::warn!("decoder io error: {err}");
                }
                Err(err) => {
                    tracing::warn!("decoder error: {err}");
                    self.eof = true;
                    break;
                }
            }
        }

        if self.eof {
            self.flush_stretched(out);
        }

        let added = out.len() - start_len;
        if added == 0 {
            return;
        }
        self.produced_frames += added as u64;
        if (self.gain - 1.0).abs() > 1e-4 {
            let gain = self.gain;
            for frame in &mut out[start_len..] {
                frame[0] *= gain;
                frame[1] *= gain;
            }
        }
    }

    /// Emit the remainder of the stretcher's last window once decoding ended.
    fn flush_stretched(&mut self, out: &mut Vec<[f32; 2]>) {
        let Self {
            stretcher,
            stretched,
            resampler,
            resampled,
            drop_samples,
            ..
        } = self;
        let Some(stretcher) = stretcher.as_mut() else {
            return;
        };
        stretched.clear();
        stretcher.process(&[], stretched, true);
        if !stretched.is_empty() {
            push_resampled(resampler, stretched, resampled, out, drop_samples);
        }
    }
}

/// Resample (or pass through) decoded stereo frames into the output buffer,
/// honoring the seek-alignment drop counter.
fn push_resampled(
    resampler: &mut LinearResampler,
    input: &[[f32; 2]],
    scratch: &mut Vec<f32>,
    out: &mut Vec<[f32; 2]>,
    drop_samples: &mut usize,
) {
    if resampler.is_identity() {
        for frame in input {
            if *drop_samples > 0 {
                *drop_samples = drop_samples.saturating_sub(2);
                continue;
            }
            out.push(*frame);
        }
        return;
    }
    scratch.clear();
    scratch.reserve(input.len() * 2 + 8);
    resampler.process(input, scratch);
    let mut idx = 0;
    while idx + 1 < scratch.len() {
        if *drop_samples > 0 {
            *drop_samples = drop_samples.saturating_sub(2);
            idx += 2;
            continue;
        }
        out.push([scratch[idx], scratch[idx + 1]]);
        idx += 2;
    }
}

fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// Accepts tag values such as "-6.50 dB", "−6.5 dB" and "+1.2".
fn parse_gain_db(text: &str) -> Option<f32> {
    let normalized = text.replace('\u{2212}', "-");
    let cleaned: String = normalized
        .chars()
        .filter(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '+'))
        .collect();
    cleaned.parse::<f32>().ok()
}

struct Player {
    shared: Arc<Shared>,
    events: crossbeam_channel::Sender<EngineEvent>,
    dsp: DspConfig,
    eq: EqChain,
    active: Option<Deck>,
    next: Option<Deck>,
    pending_next: Option<(EngineSource, u64)>,
    next_token: u64,
    fade_active: bool,
    fade_frames: u64,
    eof: bool,
    ended_sent: bool,
    loaded: bool,
    frames_a: Vec<[f32; 2]>,
    frames_b: Vec<[f32; 2]>,
}

impl Player {
    fn new(
        shared: Arc<Shared>,
        events: crossbeam_channel::Sender<EngineEvent>,
        dsp: DspConfig,
    ) -> Self {
        Self {
            shared,
            events,
            dsp,
            eq: EqChain::new(),
            active: None,
            next: None,
            pending_next: None,
            next_token: 0,
            fade_active: false,
            fade_frames: 0,
            eof: false,
            ended_sent: false,
            loaded: false,
            frames_a: Vec::with_capacity(8192),
            frames_b: Vec::with_capacity(8192),
        }
    }

    fn load(
        &mut self,
        source: EngineSource,
        start_secs: f64,
        autoplay: bool,
        token: u64,
    ) -> Result<()> {
        self.stop_inner();
        self.shared.set_state(PlaybackState::Loading);
        self.shared.buffer.lock().clear();
        self.shared.seek_base_ms.store(0, Ordering::Relaxed);
        self.shared.position_frames.store(0, Ordering::Relaxed);
        self.shared.duration_ms.store(0, Ordering::Relaxed);

        let out_rate = self.shared.output_rate.load(Ordering::Relaxed).max(1);
        let deck = Deck::new(&source, out_rate, &self.dsp)
            .with_context(|| crate::trf!("无法打开 {}", source.display()))?;
        let duration = deck.duration;
        let src_rate = deck.src_rate;
        let src_channels = deck.src_channels;
        self.active = Some(deck);
        self.eq.configure(out_rate, &self.dsp);
        self.eof = false;
        self.ended_sent = false;
        self.loaded = true;

        if let Some(duration) = duration {
            self.shared
                .duration_ms
                .store((duration * 1000.0).max(0.0) as u64, Ordering::Relaxed);
        }
        tracing::info!(
            "loaded {} ({} Hz, {} ch, duration={:?})",
            source.display(),
            src_rate,
            src_channels,
            duration
        );

        if start_secs > 0.05 {
            self.seek(start_secs)?;
        }

        // Publish the final state before notifying the UI. The app reads
        // `engine.state()` from the `Loaded` handler; sending the event first
        // let it observe the transient `Loading` state and stay there forever.
        if autoplay {
            self.shared.set_state(PlaybackState::Playing);
        } else {
            self.shared.set_state(PlaybackState::Paused);
        }
        let _ = self.events.send(EngineEvent::Loaded {
            token,
            duration,
            sample_rate: src_rate,
            channels: src_channels as u16,
        });

        if autoplay {
            self.pump();
        } else {
            // Prefill a little so resuming is instant.
            self.pump_once(0.25);
        }
        Ok(())
    }

    fn set_dsp(&mut self, config: DspConfig) {
        let speed_changed = (config.speed - self.dsp.speed).abs() > 1e-3;
        let pitch_changed = config.pitch_preserve != self.dsp.pitch_preserve;
        if speed_changed || pitch_changed {
            // Rebase the position so the reported time does not jump when the
            // speed multiplier changes. Use the *old* speed here: the shared
            // atomic may already hold the new value from the UI thread.
            let position = {
                let base = self.shared.seek_base_ms.load(Ordering::Relaxed) as f64 / 1000.0;
                let rate = self.shared.output_rate.load(Ordering::Relaxed).max(1) as f64;
                let frames = self.shared.position_frames.load(Ordering::Relaxed) as f64;
                base + frames / rate * self.dsp.speed as f64
            };
            self.shared
                .seek_base_ms
                .store((position * 1000.0) as u64, Ordering::Relaxed);
            self.shared.position_frames.store(0, Ordering::Relaxed);
            self.shared
                .speed_bits
                .store(config.speed.to_bits(), Ordering::Relaxed);
            if let Some(deck) = self.active.as_mut() {
                let position = deck.position_secs(self.dsp.speed);
                deck.position_base = position;
                deck.produced_frames = 0;
                deck.configure_speed(config.speed, config.pitch_preserve);
            }
            if let Some(deck) = self.next.as_mut() {
                let position = deck.position_secs(self.dsp.speed);
                deck.position_base = position;
                deck.produced_frames = 0;
                deck.configure_speed(config.speed, config.pitch_preserve);
            }
        }
        let eq_changed = config.eq_enabled != self.dsp.eq_enabled
            || config.eq_bands != self.dsp.eq_bands
            || (config.preamp_db - self.dsp.preamp_db).abs() > 0.01;
        let rg_changed = config.replaygain != self.dsp.replaygain
            || (config.replaygain_preamp_db - self.dsp.replaygain_preamp_db).abs() > 0.01;
        self.dsp = config;
        if eq_changed && let Some(rate) = self.active.as_ref().map(|deck| deck.out_rate) {
            self.eq.configure(rate, &self.dsp);
        }
        if rg_changed {
            if let Some(deck) = self.active.as_mut() {
                deck.refresh_gain(&self.dsp);
            }
            if let Some(deck) = self.next.as_mut() {
                deck.refresh_gain(&self.dsp);
            }
        }
        if self.dsp.crossfade_secs <= 0.05 {
            self.fade_active = false;
            self.fade_frames = 0;
        }
        self.pump();
    }

    /// A background loudness scan completed; both decks may now have a gain
    /// for their file. Applying mid-track is safe because the gain is folded
    /// into newly decoded frames.
    fn refresh_replaygain(&mut self) {
        if self.dsp.replaygain == ReplayGainMode::Off {
            return;
        }
        if let Some(deck) = self.active.as_mut() {
            deck.refresh_gain(&self.dsp);
        }
        if let Some(deck) = self.next.as_mut() {
            deck.refresh_gain(&self.dsp);
        }
        self.pump();
    }

    fn set_next(&mut self, source: Option<EngineSource>, token: u64) {
        self.next = None;
        self.fade_active = false;
        self.fade_frames = 0;
        self.pending_next = source.map(|source| (source, token));
        self.next_token = token;
    }

    fn play(&mut self) {
        if !self.loaded {
            return;
        }
        if self.shared.state() == PlaybackState::Stopped {
            // Restart from the current position if the decoder is still open.
            let pos = self.shared.position_secs();
            let _ = self.seek(pos);
        }
        self.shared.set_state(PlaybackState::Playing);
        self.pump();
    }

    fn pause(&mut self) {
        if self.shared.state() == PlaybackState::Playing {
            self.shared.set_state(PlaybackState::Paused);
        }
    }

    fn stop(&mut self) {
        self.stop_inner();
    }

    fn stop_inner(&mut self) {
        self.shared.set_state(PlaybackState::Stopped);
        self.shared.buffer.lock().clear();
        self.shared.position_frames.store(0, Ordering::Relaxed);
        self.shared.seek_base_ms.store(0, Ordering::Relaxed);
        self.active = None;
        self.next = None;
        self.pending_next = None;
        self.fade_active = false;
        self.fade_frames = 0;
        self.loaded = false;
        self.eof = false;
        self.ended_sent = false;
        self.eq.reset();
    }

    fn seek(&mut self, secs: f64) -> Result<()> {
        let Some(deck) = self.active.as_mut() else {
            return Ok(());
        };
        let target = secs.max(0.0);
        let target_ms = (target * 1000.0) as u64;
        self.shared.buffer.lock().clear();
        self.shared.position_frames.store(0, Ordering::Relaxed);
        self.shared.seek_base_ms.store(target_ms, Ordering::Relaxed);
        self.eof = false;
        self.ended_sent = false;
        self.fade_active = false;
        self.fade_frames = 0;
        deck.seek(target)?;
        if let Some(rate) = Some(deck.out_rate) {
            self.eq.configure(rate, &self.dsp);
        }
        if self.shared.state() == PlaybackState::Playing {
            self.pump_once(0.2);
        }
        Ok(())
    }

    fn pump(&mut self) {
        self.pump_until(TARGET_BUFFER_SECS);
    }

    fn pump_once(&mut self, secs: f64) {
        self.pump_until(secs);
    }

    /// Pre-open the next deck once the active track is close enough to its end
    /// that a gapless transition can be prepared in time.
    fn ensure_next_deck(&mut self) {
        if self.next.is_some() {
            return;
        }
        let Some((source, token)) = self.pending_next.clone() else {
            return;
        };
        let Some(active) = self.active.as_ref() else {
            return;
        };
        // Without a known duration (live streams) there is no reliable point
        // to pre-open; fall back to the normal ended/advance path.
        let Some(duration) = active.duration else {
            return;
        };
        let remaining = duration - active.position_secs(self.dsp.speed);
        let preload = if self.dsp.crossfade_secs > 0.05 {
            self.dsp.crossfade_secs as f64 + 5.0
        } else {
            8.0
        };
        if remaining > preload {
            return;
        }
        match Deck::new(&source, active.out_rate, &self.dsp) {
            Ok(deck) => {
                tracing::debug!(
                    "pre-opened next {:?} (token {token}, {:.1}s left)",
                    source.display(),
                    remaining
                );
                self.next = Some(deck);
                self.next_token = token;
                self.pending_next = None;
            }
            Err(err) => {
                tracing::warn!("next deck open failed: {err:#}");
                self.pending_next = None;
            }
        }
    }

    fn maybe_start_fade(&mut self) {
        if self.fade_active || self.dsp.crossfade_secs <= 0.05 || self.next.is_none() {
            return;
        }
        let Some(active) = self.active.as_ref() else {
            return;
        };
        let Some(duration) = active.duration else {
            return;
        };
        let remaining = duration - active.position_secs(self.dsp.speed);
        // The mix point is a media-time threshold: already-buffered audio
        // makes the fade become audible exactly at `duration - crossfade`.
        if remaining <= self.dsp.crossfade_secs as f64 {
            self.fade_active = true;
            self.fade_frames = 0;
            tracing::debug!("crossfade started with {remaining:.2}s remaining");
        }
    }

    fn switch_to_next(&mut self, from_fade: bool) {
        let Some(mut next) = self.next.take() else {
            return;
        };
        let out_rate = next.out_rate.max(1) as f64;
        let speed = self.dsp.speed as f64;
        // Media seconds still queued in the ring buffer. After a crossfade the
        // audible cursor is this far behind the decoded one, so the new
        // track's displayed position starts at `fade - buffered`, not 0.
        let buffered_media = {
            let buffer = self.shared.buffer.lock();
            (buffer.len() / 2) as f64 / out_rate * speed
        };
        let decoded_position = next.position_secs(self.dsp.speed);
        let fade_frames = self.fade_frames as f64;
        next.position_base = decoded_position;
        next.produced_frames = 0;
        self.active = Some(next);
        self.fade_active = false;
        self.fade_frames = 0;
        self.eof = false;
        self.ended_sent = false;
        self.loaded = true;
        let base = if from_fade {
            (fade_frames / out_rate * speed - buffered_media).max(0.0)
        } else {
            0.0
        };
        self.shared
            .seek_base_ms
            .store((base * 1000.0) as u64, Ordering::Relaxed);
        self.shared.position_frames.store(0, Ordering::Relaxed);
        let duration = self.active.as_ref().and_then(|deck| deck.duration);
        self.shared.duration_ms.store(
            (duration.unwrap_or(0.0) * 1000.0).max(0.0) as u64,
            Ordering::Relaxed,
        );
        if let Some(deck) = self.active.as_ref() {
            let _ = self.events.send(EngineEvent::AutoAdvanced {
                token: self.next_token,
                duration: deck.duration,
                sample_rate: deck.src_rate,
                channels: deck.src_channels as u16,
            });
            tracing::info!(
                "auto-advanced to next track ({} Hz, {} ch, duration={:?}, position={:.2}s)",
                deck.src_rate,
                deck.src_channels,
                deck.duration,
                base
            );
        }
        self.shared.set_state(PlaybackState::Playing);
    }

    fn pump_until(&mut self, target_secs: f64) {
        if self.eof || self.active.is_none() {
            return;
        }
        let out_rate = self.shared.output_rate.load(Ordering::Relaxed).max(1) as usize;
        let target_frames = (out_rate as f64 * target_secs) as usize;
        let max_frames = (out_rate as f64 * MAX_BUFFER_SECS) as usize;
        let target_frames = target_frames.min(max_frames).max(1);

        // Bounded so a corrupt/degenerate stream cannot spin this loop.
        for _ in 0..256 {
            let buffered = {
                let buffer = self.shared.buffer.lock();
                buffer.len() / 2
            };
            if buffered >= target_frames {
                break;
            }
            self.ensure_next_deck();
            self.maybe_start_fade();

            let want = target_frames.saturating_sub(buffered).min(4096).max(1);
            self.frames_a.clear();
            if let Some(active) = self.active.as_mut() {
                active.fill_frames(&mut self.frames_a, want);
            }

            if self.fade_active {
                self.frames_b.clear();
                if let Some(next) = self.next.as_mut() {
                    next.fill_frames(&mut self.frames_b, want);
                }
                if self.frames_a.is_empty() && self.frames_b.is_empty() {
                    // Both decks ran dry mid-fade; complete it so the next
                    // track continues from its own position.
                    self.switch_to_next(true);
                    continue;
                }
                let n = self.frames_a.len().max(self.frames_b.len());
                // The fade is defined in media seconds; output frames per
                // media second scale with the speed multiplier.
                let speed = self.dsp.speed.max(0.25) as f64;
                let total = (self.dsp.crossfade_secs as f64 * out_rate as f64 / speed).max(1.0);
                let mut buffer = self.shared.buffer.lock();
                for i in 0..n {
                    let t = ((self.fade_frames + i as u64) as f64 / total).clamp(0.0, 1.0) as f32;
                    let fade_out = (t * std::f32::consts::FRAC_PI_2).cos();
                    let fade_in = (t * std::f32::consts::FRAC_PI_2).sin();
                    let a = self.frames_a.get(i).copied().unwrap_or([0.0, 0.0]);
                    let b = self.frames_b.get(i).copied().unwrap_or([0.0, 0.0]);
                    let (l, r) = self.eq.process(
                        a[0] * fade_out + b[0] * fade_in,
                        a[1] * fade_out + b[1] * fade_in,
                    );
                    buffer.push_frame(l, r);
                }
                drop(buffer);
                self.fade_frames += n as u64;
                if self.fade_frames >= total as u64 {
                    self.switch_to_next(true);
                }
                continue;
            }

            if self.frames_a.is_empty() {
                if self.dsp.gapless && self.next.is_some() {
                    self.switch_to_next(false);
                    continue;
                }
                self.eof = true;
                break;
            }
            let mut buffer = self.shared.buffer.lock();
            for i in 0..self.frames_a.len() {
                let frame = self.frames_a[i];
                let (l, r) = self.eq.process(frame[0], frame[1]);
                buffer.push_frame(l, r);
            }
        }
    }

    fn maybe_finish(&mut self) {
        if !self.eof || self.ended_sent {
            return;
        }
        let drained = {
            let buffer = self.shared.buffer.lock();
            buffer.is_empty()
        };
        if drained {
            self.ended_sent = true;
            self.shared.set_state(PlaybackState::Stopped);
            let _ = self.events.send(EngineEvent::Ended);
        }
    }
}

#[allow(clippy::type_complexity)]
fn open_source(
    source: &EngineSource,
    start_secs: f64,
) -> Result<(
    Box<dyn FormatReader>,
    Box<dyn AudioDecoder>,
    u32,
    u32,
    usize,
    Option<f64>,
    Option<symphonia::core::units::TimeBase>,
)> {
    match source {
        EngineSource::Local(path) => open_local(path),
        EngineSource::Url {
            url,
            cache,
            http,
            download_first,
        } => {
            // A completed stream cache is preferred even when the online source
            // is available: it starts instantly and supports exact seeks.
            if let Some(cache) = cache.as_ref().filter(|path| path.is_file()) {
                tracing::info!("playing cached stream {}", cache.display());
                return open_local(cache);
            }
            if *download_first {
                if let Some(cache) = cache {
                    match download_to_cache(url, cache, http) {
                        Ok(()) => {
                            tracing::info!("downloaded stream cache {}", cache.display());
                            return open_local(cache);
                        }
                        Err(err) => {
                            tracing::warn!(
                                "eager stream download failed ({}): {err}; streaming instead",
                                cache.display()
                            );
                        }
                    }
                }
            }
            let cache = if start_secs <= 0.05 {
                cache.clone()
            } else {
                None
            };
            open_url(url, cache, http, start_secs)
        }
    }
}

/// Download the complete response to `cache` before decoding. Returns an error
/// on HTTP failure, premature EOF, or an empty body; callers fall back to
/// normal range streaming so a broken cache never blocks playback.
fn download_to_cache(url: &str, cache: &Path, http: &HttpOptions) -> Result<()> {
    let agent = http.stream_agent();
    let response = http
        .apply(agent.get(url))
        .header("Accept-Encoding", "identity")
        .call()
        .with_context(|| crate::trf!("无法下载在线音频: {}", url))?;
    let status = response.status().as_u16();
    if status >= 400 {
        anyhow::bail!("HTTP {status}");
    }
    let expected = response
        .headers()
        .get("content-length")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    let part = cache.with_extension("part");
    if let Some(parent) = cache.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut file =
        File::create(&part).with_context(|| crate::trf!("无法创建缓存 {}", part.display()))?;
    let mut reader = response.into_body().into_reader();
    let copied =
        std::io::copy(&mut reader, &mut file).context(crate::i18n::tr("下载在线音频中断"))?;
    std::io::Write::flush(&mut file).ok();
    drop(file);
    if copied == 0 {
        let _ = std::fs::remove_file(&part);
        anyhow::bail!(crate::i18n::tr("在线音频内容为空"));
    }
    if let Some(expected) = expected
        && copied != expected
    {
        let _ = std::fs::remove_file(&part);
        anyhow::bail!(crate::trf!("下载不完整 ({}/{} 字节)", copied, expected));
    }
    if std::fs::rename(&part, cache).is_err() {
        let _ = std::fs::remove_file(cache);
        std::fs::rename(&part, cache)
            .with_context(|| crate::trf!("无法写入缓存 {}", cache.display()))?;
    }
    Ok(())
}

#[allow(clippy::type_complexity)]
fn open_url(
    url: &str,
    cache: Option<PathBuf>,
    http: &HttpOptions,
    start_secs: f64,
) -> Result<(
    Box<dyn FormatReader>,
    Box<dyn AudioDecoder>,
    u32,
    u32,
    usize,
    Option<f64>,
    Option<symphonia::core::units::TimeBase>,
)> {
    let _ = start_secs;
    if hls::is_hls_url(url) {
        // A completed VOD HLS cache is finalized by the HLS worker; passing the
        // path here lets the next play open it as a local file (exact seeks,
        // known duration and gapless playback).
        let source = hls::HlsMediaSource::open(url, http.for_stream(), cache)
            .with_context(|| crate::trf!("无法打开 HLS 流: {}", url))?;
        let mss = MediaSourceStream::new(Box::new(source), Default::default());
        return finish_probe(mss, Hint::new());
    }
    let source = HttpMediaSource::open(url.to_string(), http.for_stream(), cache)
        .with_context(|| crate::trf!("无法打开在线音频: {}", url))?;
    let mss = MediaSourceStream::new(Box::new(source), Default::default());
    let mut hint = Hint::new();
    if let Ok(parsed) = url::Url::parse(url) {
        if let Some(ext) = std::path::Path::new(parsed.path())
            .extension()
            .and_then(|e| e.to_str())
        {
            hint.with_extension(&ext.to_ascii_lowercase());
        }
    }
    finish_probe(mss, hint)
}

#[allow(clippy::type_complexity)]
fn finish_probe(
    mss: MediaSourceStream<'static>,
    hint: Hint,
) -> Result<(
    Box<dyn FormatReader>,
    Box<dyn AudioDecoder>,
    u32,
    u32,
    usize,
    Option<f64>,
    Option<symphonia::core::units::TimeBase>,
)> {
    let format = symphonia::default::get_probe()
        .probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|e| anyhow!(crate::trf!("无法识别音频格式: {}", e)))?;

    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| anyhow!(crate::i18n::tr("文件中没有可播放的音轨")))?;
    let track_id = track.id;
    let time_base = track.time_base;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or_else(|| anyhow!(crate::i18n::tr("缺少音频参数")))?
        .clone();
    let src_rate = params.sample_rate.unwrap_or(44_100).max(1);
    let src_channels = params
        .channels
        .as_ref()
        .map(|c| c.count())
        .unwrap_or(2)
        .max(1);
    let decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&params, &AudioDecoderOptions::default())
        .map_err(|e| anyhow!(crate::trf!("不支持的音频编码: {}", e)))?;

    let media = format.media_info();
    let duration = media
        .time_base
        .zip(media.duration)
        .and_then(|(tb, d)| tb.calc_duration(d))
        .map(|t| t.as_secs_f64())
        // Fragmented streams often carry a zero placeholder duration; treat
        // that as unknown so the UI does not show a 0:00 progress bar.
        .filter(|secs| *secs > 0.0);

    Ok((
        format,
        decoder,
        track_id,
        src_rate,
        src_channels,
        duration,
        time_base,
    ))
}

#[allow(clippy::type_complexity)]
fn open_local(
    path: &Path,
) -> Result<(
    Box<dyn FormatReader>,
    Box<dyn AudioDecoder>,
    u32,
    u32,
    usize,
    Option<f64>,
    Option<symphonia::core::units::TimeBase>,
)> {
    let extension = path
        .extension()
        .map(|ext| ext.to_string_lossy().to_lowercase());
    match open_local_with_extension(path, extension.clone()) {
        Ok(opened) => Ok(opened),
        Err(first_error) => {
            // A completed HLS cache may contain ADTS AAC or fragmented MP4
            // while its cache path still ends in `.mp3`. Retry without the
            // extension hint so content sniffing can select the real reader.
            open_local_with_extension(path, None).map_err(|_| first_error)
        }
    }
}

#[allow(clippy::type_complexity)]
fn open_local_with_extension(
    path: &Path,
    extension: Option<String>,
) -> Result<(
    Box<dyn FormatReader>,
    Box<dyn AudioDecoder>,
    u32,
    u32,
    usize,
    Option<f64>,
    Option<symphonia::core::units::TimeBase>,
)> {
    let file = File::open(path).with_context(|| crate::trf!("无法打开 {}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = extension {
        hint.with_extension(&ext);
    }
    finish_probe(mss, hint)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::ApiClient;
    use crate::config::{NetworkSettings, SourceConfig, SourceKind};

    #[test]
    #[ignore]
    fn live_http_stream_probes_decodes_and_caches() {
        let network = NetworkSettings::default();
        let source = SourceConfig::preset(SourceKind::Local);
        let client = ApiClient::new(&source, &network);
        let page = client.search("莫文蔚", 1, 1).expect("search");
        let song = page.items.first().expect("result").clone();
        let url = client.song_url(&song.id).expect("url");

        let cache = std::env::temp_dir().join("bamboo-live-stream-test.mp3");
        let _ = std::fs::remove_file(&cache);
        let _ = std::fs::remove_file(cache.with_extension("part"));

        let http = crate::api::HttpOptions::from_settings(&source, &network).for_stream();
        let (mut format, mut decoder, track_id, rate, channels, duration, _) = open_source(
            &EngineSource::Url {
                url,
                cache: Some(cache.clone()),
                http,
                download_first: false,
            },
            0.0,
        )
        .expect("open online stream");
        eprintln!("stream: {rate} Hz, {channels} ch, duration={duration:?}");
        assert!(rate > 0);

        let mut packets = 0usize;
        loop {
            let packet = match format.next_packet() {
                Ok(Some(packet)) => packet,
                Ok(None) => break,
                Err(SymphoniaError::ResetRequired) => break,
                Err(err) => panic!("stream read failed: {err}"),
            };
            if packet.track_id != track_id {
                continue;
            }
            if decoder.decode(&packet).is_ok() {
                packets += 1;
            }
            if packets > 200 {
                break;
            }
        }
        assert!(packets > 0);
        drop(format);
        drop(decoder);
        if cache.is_file() {
            eprintln!("cache: {} bytes", std::fs::metadata(&cache).unwrap().len());
            let _ = std::fs::remove_file(&cache);
        } else {
            eprintln!("cache not finalized (stream stopped before EOF)");
        }
    }

    #[test]
    fn parses_replaygain_tag_values() {
        assert_eq!(parse_gain_db("-6.50 dB"), Some(-6.5));
        assert_eq!(parse_gain_db("\u{2212}6.5 dB"), Some(-6.5));
        assert_eq!(parse_gain_db("+1.25"), Some(1.25));
        assert_eq!(parse_gain_db("0"), Some(0.0));
        assert_eq!(parse_gain_db("loud"), None);
    }

    #[test]
    fn speed_scales_output_frames() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tone.wav");
        write_test_wav(&path, 1.0);

        let dsp = DspConfig {
            speed: 2.0,
            ..Default::default()
        };
        let mut deck = Deck::new(&EngineSource::Local(path.clone()), 44_100, &dsp).unwrap();
        let mut frames = Vec::new();
        while !deck.eof {
            deck.fill_frames(&mut frames, 4096);
        }
        // Twice the speed means roughly half the output frames. The linear
        // resampler drops the first input frame while priming.
        let expected = 22_050.0;
        assert!(
            (frames.len() as f64 - expected).abs() < 600.0,
            "got {} frames",
            frames.len()
        );

        let dsp = DspConfig::default();
        let mut deck = Deck::new(&EngineSource::Local(path), 44_100, &dsp).unwrap();
        let mut frames = Vec::new();
        while !deck.eof {
            deck.fill_frames(&mut frames, 4096);
        }
        assert!((frames.len() as f64 - 44_100.0).abs() < 600.0);
    }

    fn estimate_hz(frames: &[[f32; 2]], rate: f64) -> f64 {
        // Skip the stretcher's initial window, then count positive-going
        // zero crossings on one channel.
        let start = 2048.min(frames.len() / 3);
        let mut crossings = 0usize;
        let mut prev = frames[start][0];
        for frame in &frames[start + 1..] {
            if prev <= 0.0 && frame[0] > 0.0 {
                crossings += 1;
            }
            prev = frame[0];
        }
        let seconds = (frames.len() - start) as f64 / rate;
        crossings as f64 / seconds
    }

    #[test]
    fn pitch_preserved_speed_keeps_tone_frequency() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tone.wav");
        write_test_wav(&path, 1.0);

        let dsp = DspConfig {
            speed: 2.0,
            pitch_preserve: true,
            ..Default::default()
        };
        let mut deck = Deck::new(&EngineSource::Local(path.clone()), 44_100, &dsp).unwrap();
        let mut frames = Vec::new();
        while !deck.eof {
            deck.fill_frames(&mut frames, 4096);
        }
        assert!(
            (frames.len() as f64 - 22_050.0).abs() < 1_200.0,
            "got {} frames",
            frames.len()
        );
        let hz = estimate_hz(&frames, 44_100.0);
        assert!((hz - 440.0).abs() < 20.0, "estimated {hz:.1} Hz");

        let dsp = DspConfig {
            speed: 2.0,
            pitch_preserve: false,
            ..Default::default()
        };
        let mut deck = Deck::new(&EngineSource::Local(path), 44_100, &dsp).unwrap();
        let mut frames = Vec::new();
        while !deck.eof {
            deck.fill_frames(&mut frames, 4096);
        }
        let hz = estimate_hz(&frames, 44_100.0);
        assert!((hz - 880.0).abs() < 40.0, "tape-style got {hz:.1} Hz");
    }

    #[test]
    fn replaygain_preamp_scales_samples() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tone.wav");
        write_test_wav(&path, 0.2);
        let dsp = DspConfig {
            replaygain: ReplayGainMode::Track,
            replaygain_preamp_db: 6.0206,
            ..Default::default()
        };
        let mut deck = Deck::new(&EngineSource::Local(path), 44_100, &dsp).unwrap();
        let mut frames = Vec::new();
        while !deck.eof {
            deck.fill_frames(&mut frames, 4096);
        }
        let peak = frames
            .iter()
            .map(|frame| frame[0].abs().max(frame[1].abs()))
            .fold(0.0f32, f32::max);
        // Source amplitude is 0.5; +6 dB doubles it to ~1.0.
        assert!((peak - 1.0).abs() < 0.05, "peak {peak}");
    }

    #[test]
    fn eq_chain_handles_low_sample_rates_and_stays_finite() {
        let mut eq = EqChain::new();
        let mut dsp = DspConfig {
            eq_enabled: true,
            eq_bands: crate::config::EqPreset::Rock.bands(),
            preamp_db: 3.0,
            ..Default::default()
        };
        // 32 kHz would put the 16 kHz band exactly at Nyquist; configure must
        // clamp it instead of failing.
        eq.configure(32_000, &dsp);
        for _ in 0..2048 {
            let (l, r) = eq.process(0.25, -0.25);
            assert!(l.is_finite() && r.is_finite());
            assert!((-1.0..=1.0).contains(&l) && (-1.0..=1.0).contains(&r));
        }
        dsp.eq_enabled = false;
        eq.configure(44_100, &dsp);
        assert_eq!(eq.process(0.25, -0.25), (0.25, -0.25));
    }

    fn write_test_wav(path: &Path, seconds: f64) {
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 44_100,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        let total = (44_100.0 * seconds) as usize;
        for i in 0..total {
            let sample = ((i as f32 * 440.0 * std::f32::consts::TAU / 44_100.0).sin()
                * 0.5
                * 32767.0) as i16;
            writer.write_sample(sample).unwrap();
            writer.write_sample(sample).unwrap();
        }
        writer.finalize().unwrap();
    }

    #[test]
    fn eager_download_writes_a_complete_cache_entry() {
        use std::io::{Read as _, Write as _};
        use std::net::TcpListener;

        let dir = tempfile::tempdir().unwrap();
        let source_path = dir.path().join("source.wav");
        write_test_wav(&source_path, 0.2);
        let body = std::fs::read(&source_path).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let served = body.clone();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 2048];
            let _ = stream.read(&mut request);
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: audio/wav\r\nConnection: close\r\n\r\n",
                served.len()
            );
            stream.write_all(header.as_bytes()).unwrap();
            stream.write_all(&served).unwrap();
        });

        let cache = dir.path().join("eager.wav");
        let http = HttpOptions {
            proxy: crate::api::ProxySetting::None,
            timeout_secs: 5,
            connect_timeout_secs: 3,
            user_agent: "BambooTest/1.0".to_string(),
            ..Default::default()
        };
        download_to_cache(&format!("http://{address}/eager.wav"), &cache, &http)
            .expect("eager download");
        server.join().unwrap();

        assert_eq!(std::fs::read(&cache).unwrap(), body);
        assert!(!cache.with_extension("part").exists());
        let opened = open_source(&EngineSource::Local(cache), 0.0);
        assert!(opened.is_ok(), "cached file must be playable");
    }

    #[test]
    #[ignore]
    fn live_http_stream_cache_completes() {
        let network = NetworkSettings::default();
        let source = SourceConfig::preset(SourceKind::Local);
        let client = ApiClient::new(&source, &network);
        let page = client.search("莫文蔚", 1, 1).expect("search");
        let song = page.items.first().expect("result").clone();
        let url = client.song_url(&song.id).expect("url");

        let cache = std::env::temp_dir().join("bamboo-live-cache-test.mp3");
        let _ = std::fs::remove_file(&cache);
        let _ = std::fs::remove_file(cache.with_extension("part"));

        let http = crate::api::HttpOptions::from_settings(&source, &network).for_stream();
        let (mut format, mut decoder, track_id, _, _, duration, _) = open_source(
            &EngineSource::Url {
                url,
                cache: Some(cache.clone()),
                http,
                download_first: false,
            },
            0.0,
        )
        .expect("open online stream");
        assert!(duration.is_some());

        loop {
            match format.next_packet() {
                Ok(Some(packet)) => {
                    if packet.track_id == track_id {
                        let _ = decoder.decode(&packet);
                    }
                }
                Ok(None) => break,
                Err(SymphoniaError::ResetRequired) => break,
                Err(err) => panic!("stream read failed: {err}"),
            }
        }
        drop(format);
        drop(decoder);
        assert!(cache.is_file(), "cache file was not finalized");
        let len = std::fs::metadata(&cache).unwrap().len();
        eprintln!("complete cache: {len} bytes");
        assert!(len > 1024);
        let _ = std::fs::remove_file(&cache);
    }
}
