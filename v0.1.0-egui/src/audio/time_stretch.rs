//! Pitch-preserving time stretching (WSOLA).
//!
//! Speed changes are normally folded into the resampling ratio, which also
//! shifts pitch (tape-style). To keep pitch, this module time-scales the
//! source first: 2x speed emits half as many frames at the original sample
//! rate, then the regular resampler converts to the output rate without any
//! extra ratio.
//!
//! WSOLA overlaps 50% Hann windows and searches a small neighbourhood for the
//! start that best continues the previous frame. The overlap of two 50% Hann
//! windows sums to exactly one, so no extra normalization is needed. The
//! search keeps periodic content phase-aligned without a full phase vocoder,
//! and the window lengths are short enough that transients stay reasonably
//! sharp.

const MIN_FRAME: usize = 256;
const MAX_FRAME: usize = 4096;
/// Analysis window target in seconds (about 23 ms at 44.1 kHz).
const FRAME_SECS: f32 = 0.023;

/// Streaming WSOLA stretcher. Input and output run at the same sample rate;
/// `speed > 1` consumes more input per output frame (shorter duration).
pub struct TimeStretcher {
    /// Analysis/synthesis window length in frames (even).
    frame_len: usize,
    /// Output hop, half of `frame_len`.
    hop: usize,
    /// Maximum start offset searched around the nominal position.
    tolerance: usize,
    speed: f32,
    /// Buffered input; `buffer[0]` is absolute frame `base`.
    buffer: Vec<[f32; 2]>,
    base: u64,
    /// Absolute frame index of the next nominal analysis start.
    next_start: u64,
    /// Start of the previous window; the next frame must continue this one.
    prev_start: Option<u64>,
    /// Windowed second half of the previous frame, waiting to overlap-add.
    tail: Vec<[f32; 2]>,
    window: Vec<f32>,
    /// Scratch for the windowed current frame.
    windowed: Vec<[f32; 2]>,
    finished: bool,
}

impl TimeStretcher {
    pub fn new(rate: u32, speed: f32) -> Self {
        let target = (rate.max(8_000) as f32 * FRAME_SECS).round() as usize;
        let frame_len = (target.clamp(MIN_FRAME, MAX_FRAME)) & !1;
        let hop = frame_len / 2;
        let tolerance = (hop / 8).max(1);
        Self {
            frame_len,
            hop,
            tolerance,
            speed: speed.clamp(0.25, 4.0),
            buffer: Vec::new(),
            base: 0,
            next_start: 0,
            prev_start: None,
            tail: Vec::with_capacity(hop),
            window: hann(frame_len),
            windowed: Vec::with_capacity(frame_len),
            finished: false,
        }
    }

    pub fn set_speed(&mut self, speed: f32) {
        self.speed = speed.clamp(0.25, 4.0);
    }

    /// Drop all buffered audio, for a seek or a source sample-rate change.
    pub fn reset(&mut self, rate: u32) {
        *self = Self::new(rate, self.speed);
    }

    /// Append time-scaled frames to `out`. Set `eof` once the source is fully
    /// drained; the remaining window tail is then flushed and later calls are
    /// no-ops.
    pub fn process(&mut self, input: &[[f32; 2]], out: &mut Vec<[f32; 2]>, eof: bool) {
        if self.finished {
            return;
        }
        self.buffer.extend_from_slice(input);
        let end = self.base + self.buffer.len() as u64;
        let frame_len = self.frame_len as u64;

        while self.next_start + frame_len <= end {
            let start = self.best_start(end);
            self.emit(start, out);

            // The next nominal position comes from the fixed output hop, not
            // from the chosen start: otherwise a systematic correlation bias
            // would accumulate and change the effective speed.
            let keep_from = self
                .next_start
                .saturating_sub(self.tolerance as u64)
                .min(start + self.hop as u64);
            let drop = keep_from.saturating_sub(self.base) as usize;
            if drop > 0 {
                self.buffer.drain(..drop);
                self.base = keep_from;
            }
            self.prev_start = Some(start);
            let advance = (self.hop as f32 * self.speed).round() as u64;
            self.next_start += advance.max(1);
        }

        if eof {
            self.finish(out);
        }
    }

    /// Choose the analysis start near `next_start` whose first half best
    /// matches the natural continuation of the previous frame.
    fn best_start(&self, end: u64) -> u64 {
        let Some(prev) = self.prev_start else {
            return self.next_start;
        };
        let frame_len = self.frame_len as u64;
        let target = (prev + self.hop as u64 - self.base) as usize;
        let target_energy: f32 = self.buffer[target..target + self.hop]
            .iter()
            .map(|frame| frame[0] * frame[0] + frame[1] * frame[1])
            .sum();

        // Never move backwards: a tiny progress floor also keeps very slow
        // stretches from stalling on a repeated correlation peak.
        let lo = self
            .next_start
            .saturating_sub(self.tolerance as u64)
            .max(prev + 1)
            .max(self.base);
        let hi = (self.next_start + self.tolerance as u64).min(end - frame_len);
        if lo > hi {
            return self.next_start.min(end - frame_len);
        }

        let mut best = lo;
        let mut best_score = f32::MIN;
        let step = 2u64;
        let mut cand = lo;
        while cand <= hi {
            let score = self.correlation(cand, target, target_energy);
            if score > best_score {
                best_score = score;
                best = cand;
            }
            cand += step;
        }
        // The coarse pass may step over the true peak.
        for delta in [-1i64, 1] {
            let cand = best as i64 + delta;
            if cand >= lo as i64 && cand <= hi as i64 {
                let score = self.correlation(cand as u64, target, target_energy);
                if score > best_score {
                    best_score = score;
                    best = cand as u64;
                }
            }
        }
        best
    }

    fn correlation(&self, cand: u64, target: usize, target_energy: f32) -> f32 {
        let idx = (cand - self.base) as usize;
        let mut dot = 0.0f32;
        let mut energy = 0.0f32;
        for i in 0..self.hop {
            let a = self.buffer[idx + i];
            let b = self.buffer[target + i];
            dot += a[0] * b[0] + a[1] * b[1];
            energy += a[0] * a[0] + a[1] * a[1];
        }
        dot / (energy * target_energy).sqrt().max(1e-9)
    }

    fn emit(&mut self, start: u64, out: &mut Vec<[f32; 2]>) {
        let idx = (start - self.base) as usize;
        let hop = self.hop;
        self.windowed.clear();
        for i in 0..self.frame_len {
            let w = self.window[i];
            let frame = self.buffer[idx + i];
            self.windowed.push([frame[0] * w, frame[1] * w]);
        }

        if self.tail.len() == hop {
            for i in 0..hop {
                let a = self.windowed[i];
                let b = self.tail[i];
                out.push([a[0] + b[0], a[1] + b[1]]);
            }
        } else {
            // First frame: the ascending window gives a short fade-in.
            out.extend_from_slice(&self.windowed[..hop]);
        }
        self.tail.clear();
        self.tail.extend_from_slice(&self.windowed[hop..]);
    }

    fn finish(&mut self, out: &mut Vec<[f32; 2]>) {
        self.finished = true;
        if self.prev_start.is_none() {
            // Shorter than one analysis window: pass the source through.
            out.append(&mut self.buffer);
            return;
        }
        // The tail is the windowed second half of the last frame. With no
        // following frame to crossfade with, it simply fades out.
        out.append(&mut self.tail);
    }
}

/// Periodic Hann window. With a hop of `N / 2`, `w[i] + w[i + N / 2] == 1`,
/// which makes the overlap-add unity gain.
fn hann(len: usize) -> Vec<f32> {
    (0..len)
        .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / len as f32).cos())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, seconds: f32, hz: f32) -> Vec<[f32; 2]> {
        let total = (rate as f32 * seconds) as usize;
        (0..total)
            .map(|i| {
                let s = (std::f32::consts::TAU * hz * i as f32 / rate as f32).sin();
                [s * 0.5, s * 0.5]
            })
            .collect()
    }

    fn stretch(speed: f32, input: &[[f32; 2]], chunk: usize) -> Vec<[f32; 2]> {
        let mut stretcher = TimeStretcher::new(44_100, speed);
        let mut out = Vec::new();
        for block in input.chunks(chunk) {
            stretcher.process(block, &mut out, false);
        }
        stretcher.process(&[], &mut out, true);
        out
    }

    #[test]
    fn faster_speed_shortens_output() {
        let input = sine(44_100, 1.0, 440.0);
        let out = stretch(2.0, &input, 1024);
        let expected = input.len() as f64 / 2.0;
        let error = (out.len() as f64 - expected).abs();
        // One window tail plus search granularity.
        assert!(
            error < 1200.0,
            "got {} frames, expected {expected}",
            out.len()
        );
    }

    #[test]
    fn slower_speed_lengthens_output() {
        let input = sine(44_100, 1.0, 440.0);
        let out = stretch(0.5, &input, 1024);
        let expected = input.len() as f64 * 2.0;
        let error = (out.len() as f64 - expected).abs();
        assert!(
            error < 2400.0,
            "got {} frames, expected {expected}",
            out.len()
        );
    }

    #[test]
    fn speed_changes_duration_but_not_pitch() {
        let input = sine(44_100, 1.0, 440.0);
        let out = stretch(2.0, &input, 512);
        // Skip the first window, then count zero crossings on the left channel.
        let start = 2048.min(out.len() / 4);
        let mut crossings = 0usize;
        let mut prev = out[start][0];
        for frame in &out[start + 1..] {
            if (prev <= 0.0 && frame[0] > 0.0) || (prev >= 0.0 && frame[0] < 0.0) {
                crossings += 1;
            }
            prev = frame[0];
        }
        let seconds = (out.len() - start) as f64 / 44_100.0;
        let hz = crossings as f64 / (2.0 * seconds);
        assert!(
            (hz - 440.0).abs() < 15.0,
            "estimated {hz:.1} Hz, expected 440 Hz"
        );
    }

    #[test]
    fn speed_one_matches_input_length_and_stays_finite() {
        let input = sine(44_100, 0.5, 220.0);
        let out = stretch(1.0, &input, 4096);
        assert!((out.len() as f64 - input.len() as f64).abs() < 1200.0);
        assert!(
            out.iter()
                .all(|frame| frame[0].is_finite() && frame[1].is_finite())
        );
    }

    #[test]
    fn short_input_is_not_lost() {
        let mut stretcher = TimeStretcher::new(44_100, 2.0);
        let input = sine(44_100, 0.004, 440.0); // shorter than one window
        let mut out = Vec::new();
        stretcher.process(&input, &mut out, true);
        assert_eq!(out.len(), input.len());
    }
}
