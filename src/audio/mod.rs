//! Audio engine: symphonia decoding on a worker thread, cpal output, shared
//! ring buffer, seeking and track-end events.
//!
//! The engine intentionally keeps the UI thread free of audio work. The
//! callback only pops samples and applies a smoothed gain; all decoding,
//! resampling and network work happens off the callback thread.
//!
//! The two-deck (A/B) mixer for gapless/crossfade is introduced in a later
//! stage; this module already exposes the commands/events that the queue uses.

use std::collections::VecDeque;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig};
use parking_lot::Mutex;
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::Time;

const TARGET_BUFFER_SECS: f64 = 1.0;
const MAX_BUFFER_SECS: f64 = 2.5;

#[derive(Clone, Debug)]
pub enum EngineSource {
    Local(PathBuf),
    /// Direct HTTP(S) URL. Full streaming support arrives with the online stage.
    Url(String),
}

impl EngineSource {
    pub fn display(&self) -> String {
        match self {
            Self::Local(path) => path.display().to_string(),
            Self::Url(url) => url.clone(),
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
        duration: Option<f64>,
        sample_rate: u32,
        channels: u16,
    },
    Ended,
    Error(String),
    Underrun,
}

#[derive(Debug)]
enum Command {
    Load {
        source: EngineSource,
        start_secs: f64,
        autoplay: bool,
    },
    Play,
    Pause,
    Stop,
    Seek(f64),
    SetVolume(f32),
    SetMuted(bool),
    Shutdown,
}

#[derive(Clone, Debug)]
pub struct OutputInfo {
    pub device_name: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub sample_format: String,
}

struct Shared {
    state: AtomicU8,
    volume: AtomicU32,
    muted: AtomicBool,
    position_frames: AtomicU64,
    output_rate: AtomicU32,
    output_channels: AtomicU32,
    duration_ms: AtomicU64,
    seek_base_ms: AtomicU64,
    buffer: Mutex<SampleBuffer>,
    underruns: AtomicU64,
    output_info: Mutex<OutputInfo>,
    buffer_target: AtomicU64,
}

impl Shared {
    fn new() -> Self {
        Self {
            state: AtomicU8::new(PlaybackState::Stopped.as_u8()),
            volume: AtomicU32::new(0.8f32.to_bits()),
            muted: AtomicBool::new(false),
            position_frames: AtomicU64::new(0),
            output_rate: AtomicU32::new(44_100),
            output_channels: AtomicU32::new(2),
            duration_ms: AtomicU64::new(0),
            seek_base_ms: AtomicU64::new(0),
            buffer: Mutex::new(SampleBuffer::new()),
            underruns: AtomicU64::new(0),
            output_info: Mutex::new(OutputInfo {
                device_name: String::new(),
                sample_rate: 44_100,
                channels: 2,
                sample_format: String::new(),
            }),
            buffer_target: AtomicU64::new(44_100),
        }
    }

    fn state(&self) -> PlaybackState {
        PlaybackState::from_u8(self.state.load(Ordering::Acquire))
    }

    fn set_state(&self, state: PlaybackState) {
        self.state.store(state.as_u8(), Ordering::Release);
    }

    fn buffer_target_samples(&self) -> usize {
        self.buffer_target.load(Ordering::Relaxed) as usize
    }

    fn position_secs(&self) -> f64 {
        let base = self.seek_base_ms.load(Ordering::Relaxed) as f64 / 1000.0;
        let rate = self.output_rate.load(Ordering::Relaxed).max(1) as f64;
        let frames = self.position_frames.load(Ordering::Relaxed) as f64;
        base + frames / rate
    }

    fn duration_secs(&self) -> Option<f64> {
        let ms = self.duration_ms.load(Ordering::Relaxed);
        if ms == 0 {
            None
        } else {
            Some(ms as f64 / 1000.0)
        }
    }

    fn output_channels(&self) -> usize {
        self.output_channels.load(Ordering::Relaxed).max(1) as usize
    }
}

/// Interleaved stereo f32 samples.
struct SampleBuffer {
    samples: VecDeque<f32>,
    max_samples: usize,
}

impl SampleBuffer {
    fn new() -> Self {
        Self {
            samples: VecDeque::with_capacity(96_000),
            max_samples: 288_000,
        }
    }

    fn len(&self) -> usize {
        self.samples.len()
    }

    fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    fn clear(&mut self) {
        self.samples.clear();
    }

    fn push_frame(&mut self, l: f32, r: f32) {
        if self.samples.len() + 2 <= self.max_samples {
            self.samples.push_back(l);
            self.samples.push_back(r);
        }
    }

    fn pop_frame(&mut self) -> Option<(f32, f32)> {
        if self.samples.len() >= 2 {
            let l = self.samples.pop_front().unwrap_or(0.0);
            let r = self.samples.pop_front().unwrap_or(0.0);
            Some((l, r))
        } else {
            None
        }
    }
}

/// Stateful linear resampler. Quality is intentionally modest here; the
/// ReplayGain/EQ stage can swap in a windowed-sinc resampler later.
struct LinearResampler {
    step: f64,
    pos: f64,
    last: [f32; 2],
    initialized: bool,
}

impl LinearResampler {
    fn new(src_rate: u32, dst_rate: u32) -> Self {
        let step = if dst_rate == 0 {
            1.0
        } else {
            src_rate as f64 / dst_rate as f64
        };
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

    fn set_rates(&mut self, src_rate: u32, dst_rate: u32) {
        self.step = if dst_rate == 0 {
            1.0
        } else {
            src_rate as f64 / dst_rate as f64
        };
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
}

impl AudioEngine {
    pub fn new(volume: f32, muted: bool) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        let (event_tx, events) = crossbeam_channel::unbounded();
        let shared = Arc::new(Shared::new());
        shared
            .volume
            .store(volume.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
        shared.muted.store(muted, Ordering::Relaxed);

        let thread_shared = shared.clone();
        let thread = std::thread::Builder::new()
            .name("bamboo-audio".to_string())
            .spawn(move || {
                if let Err(err) = run_audio(thread_shared.clone(), rx, event_tx.clone()) {
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
        }
    }

    pub fn load(&self, source: EngineSource, start_secs: f64, autoplay: bool) {
        let _ = self.tx.send(Command::Load {
            source,
            start_secs,
            autoplay,
        });
    }

    pub fn play(&self) {
        let _ = self.tx.send(Command::Play);
    }

    pub fn pause(&self) {
        let _ = self.tx.send(Command::Pause);
    }

    pub fn toggle(&self) {
        match self.state() {
            PlaybackState::Playing => self.pause(),
            _ => self.play(),
        }
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

    pub fn state(&self) -> PlaybackState {
        self.shared.state()
    }

    pub fn position_secs(&self) -> f64 {
        self.shared.position_secs()
    }

    pub fn duration_secs(&self) -> Option<f64> {
        self.shared.duration_secs()
    }

    pub fn output_info(&self) -> OutputInfo {
        self.shared.output_info.lock().clone()
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

fn run_audio(
    shared: Arc<Shared>,
    rx: crossbeam_channel::Receiver<Command>,
    event_tx: crossbeam_channel::Sender<EngineEvent>,
) -> Result<()> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| anyhow!("没有可用的音频输出设备"))?;
    let device_name = device
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_else(|_| "默认输出设备".to_string());
    let supported = device
        .default_output_config()
        .context("无法获取默认音频输出配置")?;
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
    *shared.output_info.lock() = OutputInfo {
        device_name,
        sample_rate: out_rate,
        channels: out_channels,
        sample_format: format!("{sample_format:?}"),
    };

    let stream = match sample_format {
        SampleFormat::F32 => build_stream::<f32>(&device, config.clone(), shared.clone()),
        SampleFormat::I16 => build_stream::<i16>(&device, config.clone(), shared.clone()),
        SampleFormat::U16 => build_stream::<u16>(&device, config.clone(), shared.clone()),
        SampleFormat::I32 => build_stream::<i32>(&device, config.clone(), shared.clone()),
        SampleFormat::F64 => build_stream::<f64>(&device, config.clone(), shared.clone()),
        other => Err(anyhow!("不支持的输出采样格式: {other:?}")),
    }?;
    stream.play().context("无法启动音频输出流")?;

    let mut player = Player::new(shared.clone(), event_tx.clone());
    loop {
        match rx.recv_timeout(Duration::from_millis(15)) {
            Ok(cmd) => match cmd {
                Command::Load {
                    source,
                    start_secs,
                    autoplay,
                } => {
                    if let Err(err) = player.load(source, start_secs, autoplay) {
                        tracing::warn!("load failed: {err:#}");
                        shared.set_state(PlaybackState::Stopped);
                        let _ = event_tx.send(EngineEvent::Error(err.to_string()));
                    }
                }
                Command::Play => player.play(),
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
                Command::Shutdown => break,
            },
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }

        if shared.state() == PlaybackState::Playing {
            player.pump();
        }
        player.maybe_finish();
    }

    player.stop();
    drop(stream);
    Ok(())
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
                    let mut underrun = false;
                    for frame in data.chunks_mut(out_channels) {
                        let (l, r) = match buffer.pop_frame() {
                            Some(pair) => pair,
                            None => {
                                underrun = true;
                                (0.0, 0.0)
                            }
                        };
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
        .context("无法创建音频输出流")?;
    Ok(stream)
}

struct Player {
    shared: Arc<Shared>,
    events: crossbeam_channel::Sender<EngineEvent>,
    format: Option<Box<dyn FormatReader>>,
    decoder: Option<Box<dyn AudioDecoder>>,
    track_id: u32,
    src_rate: u32,
    src_channels: usize,
    out_rate: u32,
    resampler: LinearResampler,
    eof: bool,
    ended_sent: bool,
    loaded: bool,
    time_base: Option<symphonia::core::units::TimeBase>,
    /// Samples that should be dropped first (used to align accurate seeks).
    drop_samples: usize,
}

impl Player {
    fn new(shared: Arc<Shared>, events: crossbeam_channel::Sender<EngineEvent>) -> Self {
        let out_rate = shared.output_rate.load(Ordering::Relaxed);
        Self {
            shared,
            events,
            format: None,
            decoder: None,
            track_id: 0,
            src_rate: out_rate,
            src_channels: 2,
            out_rate,
            resampler: LinearResampler::new(out_rate, out_rate),
            eof: false,
            ended_sent: false,
            loaded: false,
            time_base: None,
            drop_samples: 0,
        }
    }

    fn load(&mut self, source: EngineSource, start_secs: f64, autoplay: bool) -> Result<()> {
        self.stop_inner();
        self.shared.set_state(PlaybackState::Loading);
        self.shared.buffer.lock().clear();
        self.shared.seek_base_ms.store(
            (start_secs.max(0.0) * 1000.0) as u64,
            Ordering::Relaxed,
        );
        self.shared.position_frames.store(0, Ordering::Relaxed);
        self.shared.duration_ms.store(0, Ordering::Relaxed);

        let (format, decoder, track_id, src_rate, src_channels, duration, time_base) =
            open_source(&source)?;

        self.format = Some(format);
        self.decoder = Some(decoder);
        self.track_id = track_id;
        self.src_rate = src_rate.max(1);
        self.src_channels = src_channels.max(1);
        self.out_rate = self.shared.output_rate.load(Ordering::Relaxed).max(1);
        self.time_base = time_base;
        self.resampler = LinearResampler::new(self.src_rate, self.out_rate);
        self.eof = false;
        self.ended_sent = false;
        self.loaded = true;
        self.drop_samples = 0;

        if let Some(duration) = duration {
            self.shared
                .duration_ms
                .store((duration * 1000.0).max(0.0) as u64, Ordering::Relaxed);
        }
        tracing::info!(
            "loaded {} ({} Hz, {} ch, duration={:?})",
            source.display(),
            self.src_rate,
            self.src_channels,
            duration
        );

        if start_secs > 0.05 {
            self.seek(start_secs)?;
        }

        let _ = self.events.send(EngineEvent::Loaded {
            duration,
            sample_rate: self.src_rate,
            channels: self.src_channels as u16,
        });

        if autoplay {
            self.shared.set_state(PlaybackState::Playing);
            self.pump();
        } else {
            self.shared.set_state(PlaybackState::Paused);
            // Prefill a little so resuming is instant.
            self.pump_once(0.25);
        }
        Ok(())
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
        self.format = None;
        self.decoder = None;
        self.loaded = false;
        self.eof = false;
        self.ended_sent = false;
        self.drop_samples = 0;
    }

    fn seek(&mut self, secs: f64) -> Result<()> {
        let Some(format) = self.format.as_mut() else {
            return Ok(());
        };
        let target = secs.max(0.0);
        let time = Time::try_from_secs_f64(target).ok_or_else(|| anyhow!("无效的跳转位置"))?;
        let target_ms = (target * 1000.0) as u64;
        self.shared.buffer.lock().clear();
        self.shared.position_frames.store(0, Ordering::Relaxed);
        self.shared.seek_base_ms.store(target_ms, Ordering::Relaxed);
        self.resampler.reset();
        self.eof = false;
        self.ended_sent = false;
        self.drop_samples = 0;

        let seeked = format
            .seek(
                SeekMode::Accurate,
                SeekTo::Time {
                    time,
                    track_id: Some(self.track_id),
                },
            )
            .context("跳转失败")?;
        if let Some(decoder) = self.decoder.as_mut() {
            decoder.reset();
        }
        // Accurate seek lands at or before the requested time. Drop the
        // difference so the reported position and audio stay aligned.
        let actual = self
            .time_base
            .and_then(|tb| tb.calc_time(seeked.actual_ts))
            .map(|t| t.as_secs_f64())
            .unwrap_or(0.0);
        let delta = (target - actual).max(0.0);
        if delta > 0.001 {
            self.drop_samples = (delta * self.out_rate as f64 * 2.0) as usize;
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

    fn pump_until(&mut self, target_secs: f64) {
        if self.eof || self.format.is_none() || self.decoder.is_none() {
            return;
        }
        let target_samples = (self.out_rate as f64 * target_secs * 2.0) as usize;
        let max_samples = (self.out_rate as f64 * MAX_BUFFER_SECS * 2.0) as usize;

        loop {
            {
                let buffer = self.shared.buffer.lock();
                if buffer.len() >= target_samples || buffer.len() >= max_samples {
                    break;
                }
            }

            let Some(format) = self.format.as_mut() else {
                break;
            };
            let packet = match format.next_packet() {
                Ok(Some(packet)) => packet,
                Ok(None) => {
                    self.eof = true;
                    break;
                }
                Err(SymphoniaError::ResetRequired) => {
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
                    let channels = spec.channels().count();
                    if rate != self.src_rate || channels != self.src_channels {
                        self.src_rate = rate.max(1);
                        self.src_channels = channels.max(1);
                        self.resampler
                            .set_rates(self.src_rate, self.out_rate);
                    }
                    let mut interleaved: Vec<f32> = Vec::new();
                    audio.copy_to_vec_interleaved(&mut interleaved);
                    if interleaved.is_empty() {
                        continue;
                    }
                    let frames = interleaved.len() / self.src_channels.max(1);
                    let mut stereo: Vec<[f32; 2]> = Vec::with_capacity(frames);
                    for i in 0..frames {
                        let base = i * self.src_channels;
                        let l = interleaved.get(base).copied().unwrap_or(0.0);
                        let r = if self.src_channels >= 2 {
                            interleaved.get(base + 1).copied().unwrap_or(l)
                        } else {
                            l
                        };
                        stereo.push([l, r]);
                    }
                    let mut out: Vec<f32> = Vec::with_capacity(frames * 2);
                    self.resampler.process(&stereo, &mut out);
                    let mut buf = self.shared.buffer.lock();
                    let mut idx = 0;
                    while idx + 1 < out.len() {
                        if self.drop_samples > 0 {
                            self.drop_samples = self.drop_samples.saturating_sub(2);
                            idx += 2;
                            continue;
                        }
                        buf.push_frame(out[idx], out[idx + 1]);
                        idx += 2;
                    }
                    drop(buf);
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

        let _ = self.drop_samples;
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
        EngineSource::Url(url) => Err(anyhow!(
            "在线流播放将在在线音源阶段启用：{url}"
        )),
    }
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
    let file = File::open(path).with_context(|| format!("无法打开 {}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension() {
        hint.with_extension(&ext.to_string_lossy().to_lowercase());
    }
    let format = symphonia::default::get_probe()
        .probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|e| anyhow!("无法识别音频格式: {e}"))?;

    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| anyhow!("文件中没有可播放的音轨"))?;
    let track_id = track.id;
    let time_base = track.time_base;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or_else(|| anyhow!("缺少音频参数"))?
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
        .map_err(|e| anyhow!("不支持的音频编码: {e}"))?;

    let media = format.media_info();
    let duration = media
        .time_base
        .zip(media.duration)
        .and_then(|(tb, d)| tb.calc_duration(d))
        .map(|t| t.as_secs_f64());

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

