//! Lightweight acoustic fingerprint for the library duplicate finder.
//!
//! The decoder produces mono samples at 11.025 kHz. Short-time FFT peaks are
//! paired across neighbouring frames into 21-bit landmark hashes (the classic
//! Shazam-style construction). Two files are compared by the Jaccard overlap of
//! their hash sets; a 64-bit SimHash over the same hashes is used as a cheap
//! prefilter. This is not Chromaprint, but it ignores tags and tolerates
//! moderate re-encoding without decoding the files during comparison.

use std::collections::BTreeSet;
use std::path::Path;

use rustfft::FftPlanner;
use rustfft::num_complex::Complex;
use symphonia::core::errors::Error as SymphoniaError;

use super::EngineSource;

const TARGET_RATE: u32 = 11_025;
const FFT_SIZE: usize = 1024;
const HOP: usize = 512;
const MAX_SECONDS: f64 = 8.0 * 60.0;
const MAX_HASHES: usize = 4096;
const PEAKS_PER_FRAME: usize = 4;

#[derive(Clone, Debug)]
pub struct Fingerprint {
    pub simhash: u64,
    /// Sorted landmark hashes.
    pub hashes: Vec<u32>,
}

pub fn compute(path: &Path) -> Result<Fingerprint, String> {
    let (mut format, mut decoder, track_id, src_rate, _, _, _) =
        super::open_source(&EngineSource::Local(path.to_path_buf()), 0.0)
            .map_err(|err| err.to_string())?;
    let mut mono = Vec::new();
    let mut downsampled = Vec::with_capacity((src_rate as usize / 4).max(1));
    let ratio = src_rate.max(1) as f64 / TARGET_RATE as f64;
    let mut previous = 0.0f32;
    let mut next_output = 0.0f64;
    let mut sample_index = 0u64;
    let limit = (MAX_SECONDS * src_rate.max(1) as f64) as u64;

    loop {
        if sample_index >= limit {
            break;
        }
        let packet = match format.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) | Err(SymphoniaError::ResetRequired) => break,
            Err(SymphoniaError::IoError(err)) => {
                tracing::debug!("fingerprint read error: {err}");
                break;
            }
            Err(err) => return Err(crate::trf!("读取音频失败: {}", err)),
        };
        if packet.track_id != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(audio) => {
                mono.clear();
                audio.copy_to_vec_interleaved(&mut mono);
                let channels = audio.spec().channels().count().max(1);
                let frames = mono.len() / channels;
                for frame in 0..frames {
                    let base = frame * channels;
                    let mut sum = 0.0f32;
                    for channel in 0..channels {
                        sum += mono.get(base + channel).copied().unwrap_or(0.0);
                    }
                    let sample = sum / channels as f32;
                    if sample_index == 0 {
                        previous = sample;
                    } else {
                        while next_output <= sample_index as f64 {
                            let t = (next_output - (sample_index as f64 - 1.0)) as f32;
                            downsampled.push(previous + (sample - previous) * t);
                            next_output += ratio;
                        }
                        previous = sample;
                    }
                    sample_index += 1;
                    if sample_index >= limit {
                        break;
                    }
                }
            }
            Err(SymphoniaError::DecodeError(err)) => {
                tracing::debug!("fingerprint decode error (skipped): {err}");
            }
            Err(SymphoniaError::IoError(err)) => {
                tracing::debug!("fingerprint decoder io error: {err}");
            }
            Err(err) => return Err(crate::trf!("解码音频失败: {}", err)),
        }
    }

    if downsampled.len() < FFT_SIZE * 2 {
        return Err(crate::i18n::tr("音频太短，无法生成指纹").to_string());
    }
    let hashes = landmark_hashes(&downsampled);
    if hashes.is_empty() {
        return Err(crate::i18n::tr("没有检测到可用于指纹的音频特征").to_string());
    }
    let simhash = simhash(&hashes);
    Ok(Fingerprint { simhash, hashes })
}

impl Fingerprint {
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.hashes.is_empty()
    }
    pub fn from_parts(simhash: u64, hashes: Vec<u32>) -> Self {
        Self { simhash, hashes }
    }

    pub fn hamming(&self, other: &Self) -> u32 {
        (self.simhash ^ other.simhash).count_ones()
    }

    /// Number of landmark hashes present in both fingerprints.
    pub fn common(&self, other: &Self) -> usize {
        let mut i = 0;
        let mut j = 0;
        let mut common = 0usize;
        while i < self.hashes.len() && j < other.hashes.len() {
            match self.hashes[i].cmp(&other.hashes[j]) {
                std::cmp::Ordering::Less => i += 1,
                std::cmp::Ordering::Greater => j += 1,
                std::cmp::Ordering::Equal => {
                    common += 1;
                    i += 1;
                    j += 1;
                }
            }
        }
        common
    }

    /// Jaccard overlap of the two hash sets (0.0 ..= 1.0).
    pub fn similarity(&self, other: &Self) -> f32 {
        if self.hashes.is_empty() || other.hashes.is_empty() {
            return 0.0;
        }
        let common = self.common(other);
        let union = self.hashes.len() + other.hashes.len() - common;
        common as f32 / union.max(1) as f32
    }
}

fn landmark_hashes(samples: &[f32]) -> Vec<u32> {
    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(FFT_SIZE);
    let window: Vec<f32> = (0..FFT_SIZE)
        .map(|index| {
            0.5 - 0.5 * (std::f32::consts::TAU * index as f32 / (FFT_SIZE - 1) as f32).cos()
        })
        .collect();
    let mut buffer = vec![Complex::new(0.0f32, 0.0); FFT_SIZE];
    let mut frames: Vec<Vec<(u16, f32)>> = Vec::new();

    let mut start = 0;
    while start + FFT_SIZE <= samples.len() {
        for (index, slot) in buffer.iter_mut().enumerate() {
            *slot = Complex::new(samples[start + index] * window[index], 0.0);
        }
        fft.process(&mut buffer);
        let half = FFT_SIZE / 2;
        let magnitudes: Vec<f32> = (1..half).map(|bin| buffer[bin].norm().ln_1p()).collect();
        let mean = magnitudes.iter().sum::<f32>() / magnitudes.len().max(1) as f32;
        let mut peaks: Vec<(u16, f32)> = Vec::new();
        for bin in 1..magnitudes.len().saturating_sub(1) {
            let value = magnitudes[bin];
            if value > mean * 1.2 && value >= magnitudes[bin - 1] && value >= magnitudes[bin + 1] {
                peaks.push(((bin + 1) as u16, value));
            }
        }
        peaks.sort_by(|a, b| b.1.total_cmp(&a.1));
        peaks.truncate(PEAKS_PER_FRAME);
        peaks.sort_by_key(|peak| peak.0);
        frames.push(peaks);
        start += HOP;
    }

    let mut hashes = BTreeSet::new();
    'outer: for frame in 0..frames.len() {
        for &(first, _) in &frames[frame] {
            for delta in 1..=4u16 {
                let next_frame = frame + delta as usize;
                if next_frame >= frames.len() {
                    break;
                }
                for &(second, _) in &frames[next_frame] {
                    let hash = (u32::from(first) << 23)
                        | (u32::from(second) << 14)
                        | (u32::from(delta) << 11);
                    hashes.insert(hash);
                    if hashes.len() >= MAX_HASHES {
                        break 'outer;
                    }
                }
            }
        }
    }
    hashes.into_iter().collect()
}

fn simhash(hashes: &[u32]) -> u64 {
    let mut votes = [0i32; 64];
    for hash in hashes {
        let value = splitmix64(u64::from(*hash));
        for (bit, vote) in votes.iter_mut().enumerate() {
            if (value >> bit) & 1 == 1 {
                *vote += 1;
            } else {
                *vote -= 1;
            }
        }
    }
    let mut out = 0u64;
    for (bit, vote) in votes.iter().enumerate() {
        if *vote > 0 {
            out |= 1u64 << bit;
        }
    }
    out
}

fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9E37_79B9_7F4A_7C15);
    value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^ (value >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_tone(path: &Path, frequency: f32, seconds: f32) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 22_050,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        let count = (seconds * spec.sample_rate as f32) as u32;
        for index in 0..count {
            let (sin, _) = (std::f32::consts::TAU * frequency * index as f32
                / spec.sample_rate as f32)
                .sin_cos();
            writer.write_sample((sin * 0.4 * 32767.0) as i16).unwrap();
        }
        writer.finalize().unwrap();
    }

    #[test]
    fn identical_audio_has_identical_fingerprint() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tone.wav");
        write_tone(&path, 440.0, 2.0);
        let first = compute(&path).unwrap();
        let second = compute(&path).unwrap();
        assert!(!first.is_empty());
        assert_eq!(first.simhash, second.simhash);
        assert_eq!(first.hashes, second.hashes);
        assert!((first.similarity(&second) - 1.0).abs() < 1e-6);
        assert_eq!(first.hamming(&second), 0);
    }

    #[test]
    fn different_audio_is_not_identical() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.wav");
        let b = dir.path().join("b.wav");
        write_tone(&a, 440.0, 2.0);
        write_tone(&b, 1000.0, 2.0);
        let first = compute(&a).unwrap();
        let second = compute(&b).unwrap();
        assert!(first.similarity(&second) < 0.5);
    }

    #[test]
    fn parses_stored_hash_bytes() {
        let fingerprint = Fingerprint::from_parts(0x1234, vec![1, 2, 3]);
        assert_eq!(fingerprint.hashes, vec![1, 2, 3]);
        assert!(!fingerprint.is_empty());
    }
}
