//! Offline ReplayGain analysis (EBU R128) and the gain cache shared by decks.
//!
//! Files with `REPLAYGAIN_TRACK_GAIN`/`REPLAYGAIN_ALBUM_GAIN` tags never reach
//! the scanner. Files without tags are decoded in a background thread and
//! measured with the EBU R128 integrated loudness filter; the resulting gain
//! uses the ReplayGain 2.0 reference of -18 LUFS. The result is cached on disk
//! by the app and mirrored into the process-wide map read by `Deck`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use crossbeam_channel::{Receiver, Sender, unbounded};
use ebur128::{EbuR128, Mode};
use lofty::config::ParseOptions;
use lofty::file::TaggedFileExt;
use lofty::probe::Probe;
use lofty::tag::ItemKey;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use symphonia::core::errors::Error as SymphoniaError;

use super::EngineSource;

/// ReplayGain 2.0 reference loudness.
const REFERENCE_LUFS: f64 = -18.0;
const MIN_GAIN_DB: f32 = -24.0;
const MAX_GAIN_DB: f32 = 24.0;

static CACHED_GAINS: LazyLock<RwLock<HashMap<PathBuf, f32>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// Gain computed by a previous scan, persisted between runs.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct CachedGain {
    pub gain_db: f32,
    pub size: u64,
    pub modified: i64,
}

impl CachedGain {
    /// A cache entry is valid while the file has the same size and mtime;
    /// re-encoded or replaced files are measured again.
    pub fn matches(&self, path: &Path) -> bool {
        let Ok(metadata) = std::fs::metadata(path) else {
            return false;
        };
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs() as i64)
            .unwrap_or(0);
        metadata.len() == self.size && modified == self.modified
    }
}

pub fn cached_gain(path: &Path) -> Option<f32> {
    CACHED_GAINS.read().get(path).copied()
}

pub fn set_cached_gain(path: PathBuf, gain_db: f32) {
    CACHED_GAINS.write().insert(path, gain_db);
}

/// Drop all scanned gains (used by the settings cache cleaner). Decks re-read
/// tags on the next gain refresh; scanned values are gone until the files are
/// analyzed again.
pub fn clear_cached_gains() {
    CACHED_GAINS.write().clear();
}

pub fn load_cache(path: &Path) -> HashMap<String, CachedGain> {
    let Ok(bytes) = std::fs::read(path) else {
        return HashMap::new();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

pub fn save_cache(path: &Path, cache: &HashMap<String, CachedGain>) {
    let Some(parent) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let Ok(bytes) = serde_json::to_vec_pretty(cache) else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, bytes).is_ok() {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::rename(&tmp, path);
    }
}

#[derive(Clone, Debug)]
pub struct ScanEvent {
    pub path: PathBuf,
    /// `Ok(Some(gain))` for a measured file, `Ok(None)` when the file already
    /// carries ReplayGain tags, and `Err` when analysis failed.
    pub result: Result<Option<f32>, String>,
}

struct Job {
    path: PathBuf,
}

pub struct LoudnessService {
    jobs: Sender<Job>,
    events: Receiver<ScanEvent>,
    _thread: std::thread::JoinHandle<()>,
}

impl LoudnessService {
    pub fn new() -> Self {
        Self::with_wake(None)
    }

    /// Wake the UI when a scan finishes. ReplayGain results otherwise waited
    /// for the next unrelated tick, which never came while paused.
    pub fn with_wake(wake: Option<Arc<dyn Fn() + Send + Sync>>) -> Self {
        let (jobs, job_rx) = unbounded::<Job>();
        let (event_tx, events) = unbounded();
        let thread = std::thread::Builder::new()
            .name("bamboo-loudness".to_string())
            .spawn(move || {
                while let Ok(job) = job_rx.recv() {
                    let result = scan_file(&job.path);
                    if let Err(err) = &result {
                        tracing::debug!("loudness scan failed for {}: {err}", job.path.display());
                    }
                    let _ = event_tx.send(ScanEvent {
                        path: job.path,
                        result,
                    });
                    if let Some(wake) = wake.as_ref() {
                        wake();
                    }
                }
            })
            .expect("loudness thread");
        Self {
            jobs,
            events,
            _thread: thread,
        }
    }

    pub fn scan(&mut self, path: PathBuf) {
        let _ = self.jobs.send(Job { path });
    }

    pub fn poll(&self) -> Option<ScanEvent> {
        self.events.try_recv().ok()
    }
}

impl Default for LoudnessService {
    fn default() -> Self {
        Self::new()
    }
}

/// Measure one file. Returns `Ok(None)` when the file has a ReplayGain tag
/// (the decoder already applies it), otherwise the scanned track gain in dB.
pub fn scan_file(path: &Path) -> Result<Option<f32>, String> {
    if has_replaygain_tag(path) {
        return Ok(None);
    }
    let (mut format, decoder, track_id, mut rate, mut channels, _duration, _time_base) =
        super::open_source(&EngineSource::Local(path.to_path_buf()), 0.0)
            .map_err(|err| err.to_string())?;
    let mut decoder = decoder;
    let mut meter = EbuR128::new(channels as u32, rate, Mode::I).map_err(|err| err.to_string())?;
    let mut interleaved = Vec::new();
    let mut measured = false;

    loop {
        let packet = match format.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) | Err(SymphoniaError::ResetRequired) => break,
            Err(SymphoniaError::IoError(err)) => {
                tracing::debug!("loudness read error: {err}");
                break;
            }
            Err(err) => {
                return Err(crate::trf!("读取音频失败: {}", err));
            }
        };
        if packet.track_id != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(audio) => {
                let spec = audio.spec();
                let new_rate = spec.rate().max(1);
                let new_channels = spec.channels().count().max(1) as u32;
                if new_rate != rate || new_channels != channels as u32 {
                    rate = new_rate;
                    channels = new_channels as usize;
                    meter
                        .change_parameters(new_channels, new_rate)
                        .map_err(|err| err.to_string())?;
                }
                interleaved.clear();
                audio.copy_to_vec_interleaved(&mut interleaved);
                if interleaved.is_empty() {
                    continue;
                }
                meter
                    .add_frames_f32(&interleaved)
                    .map_err(|err| err.to_string())?;
                measured = true;
            }
            Err(SymphoniaError::DecodeError(err)) => {
                tracing::debug!("loudness decode error (skipped): {err}");
            }
            Err(SymphoniaError::IoError(err)) => {
                tracing::debug!("loudness decoder io error: {err}");
            }
            Err(err) => return Err(crate::trf!("解码音频失败: {}", err)),
        }
    }

    if !measured {
        return Err(crate::i18n::tr("没有可分析的音频数据").to_string());
    }
    let loudness = meter
        .loudness_global()
        .map_err(|err| crate::trf!("响度分析失败: {}", err))?;
    Ok(Some(gain_from_loudness(loudness)))
}

fn gain_from_loudness(lufs: f64) -> f32 {
    ((REFERENCE_LUFS - lufs) as f32).clamp(MIN_GAIN_DB, MAX_GAIN_DB)
}

fn has_replaygain_tag(path: &Path) -> bool {
    let options = ParseOptions::new().read_cover_art(false);
    let Ok(tagged) = Probe::open(path).and_then(|probe| probe.options(options).read()) else {
        return false;
    };
    let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) else {
        return false;
    };
    tag.get_string(ItemKey::ReplayGainTrackGain).is_some()
        || tag.get_string(ItemKey::ReplayGainAlbumGain).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gain_uses_replaygain_2_reference_and_clamps() {
        assert!((gain_from_loudness(-18.0) - 0.0).abs() < 1e-6);
        assert!((gain_from_loudness(-23.0) - 5.0).abs() < 1e-6);
        assert!((gain_from_loudness(-10.0) - -8.0).abs() < 1e-6);
        assert_eq!(gain_from_loudness(-80.0), MAX_GAIN_DB);
        assert_eq!(gain_from_loudness(80.0), MIN_GAIN_DB);
    }

    #[test]
    fn scans_a_generated_wav() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tone.wav");
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 44_100,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&path, spec).unwrap();
        for index in 0..44_100 {
            let value =
                (std::f32::consts::TAU * 1000.0 * index as f32 / 44_100.0).sin() * 0.5 * 32767.0;
            writer.write_sample(value as i16).unwrap();
            writer.write_sample(value as i16).unwrap();
        }
        writer.finalize().unwrap();

        let gain = scan_file(&path).unwrap().expect("no tags, scan expected");
        assert!(gain.is_finite());
        assert!((-24.0..=24.0).contains(&gain));
    }
}
