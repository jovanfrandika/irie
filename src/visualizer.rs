//! Real-time spectrum analysis of the PCM captured by [`crate::engine::Tap`].

use crate::engine::SAMPLE_RATE;
use rustfft::{Fft, FftPlanner, num_complex::Complex};
use std::sync::Arc;

pub const FFT_SIZE: usize = 4096;
const MIN_HZ: f32 = 35.0;
const MAX_HZ: f32 = 16_000.0;
const FLOOR_DB: f32 = -62.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Bars,
    Mirror,
    Wave,
}

impl Mode {
    pub fn next(self) -> Self {
        match self {
            Self::Bars => Self::Mirror,
            Self::Mirror => Self::Wave,
            Self::Wave => Self::Bars,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Bars => "spectrum",
            Self::Mirror => "mirror",
            Self::Wave => "oscilloscope",
        }
    }
}

pub struct Analyzer {
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    buf: Vec<Complex<f32>>,
    pub samples: Vec<f32>,
    /// Smoothed band levels in 0..=1.
    pub bands: Vec<f32>,
    /// Falling peak markers in 0..=1.
    pub peaks: Vec<f32>,
    /// Smoothed RMS loudness in 0..=1.
    pub level: f32,
    pub active: bool,
    /// Sample rate of `samples`, in Hz.
    pub sample_rate: f32,
}

impl Default for Analyzer {
    fn default() -> Self {
        let fft = FftPlanner::new().plan_fft_forward(FFT_SIZE);
        let window = (0..FFT_SIZE)
            .map(|i| {
                0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (FFT_SIZE - 1) as f32).cos()
            })
            .collect();
        Self {
            fft,
            window,
            buf: vec![Complex::default(); FFT_SIZE],
            samples: vec![0.0; FFT_SIZE],
            bands: Vec::new(),
            peaks: Vec::new(),
            level: 0.0,
            active: false,
            sample_rate: SAMPLE_RATE,
        }
    }
}

impl Analyzer {
    /// Recompute `n` bands. Call once per frame after filling `self.samples`
    /// (or with `has_audio == false` to let everything fall to zero).
    pub fn update(&mut self, n: usize, has_audio: bool) {
        if self.bands.len() != n {
            self.bands = vec![0.0; n];
            self.peaks = vec![0.0; n];
        }
        self.active = has_audio;
        if n == 0 {
            return;
        }

        let mut target = vec![0.0f32; n];
        if has_audio {
            for (i, (s, w)) in self.samples.iter().zip(&self.window).enumerate() {
                self.buf[i] = Complex::new(s * w, 0.0);
            }
            self.fft.process(&mut self.buf);

            let bin_hz = self.sample_rate / FFT_SIZE as f32;
            let half = FFT_SIZE / 2;
            // Normalise so a full-scale sine reads ~0 dB (Hann window halves amplitude).
            let norm = 2.0 / (FFT_SIZE as f32 * 0.5);
            let ratio = (MAX_HZ / MIN_HZ).powf(1.0 / n as f32);
            for (b, t) in target.iter_mut().enumerate() {
                let lo = MIN_HZ * ratio.powi(b as i32);
                let hi = lo * ratio;
                let lo_bin = ((lo / bin_hz).floor() as usize).clamp(1, half - 1);
                let hi_bin = ((hi / bin_hz).ceil() as usize).clamp(lo_bin + 1, half);
                let mag = self.buf[lo_bin..hi_bin]
                    .iter()
                    .map(|c| c.norm())
                    .fold(0.0f32, f32::max)
                    * norm;
                // Gentle tilt so highs are not drowned out by bass energy.
                let tilt = 1.0 + 3.0 * (b as f32 / n as f32);
                let db = 20.0 * (mag * tilt).max(1e-9).log10();
                *t = ((db - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0);
            }
            let rms = (self.samples.iter().map(|s| s * s).sum::<f32>() / FFT_SIZE as f32).sqrt();
            self.level += ((rms * 3.0).min(1.0) - self.level) * 0.3;
        } else {
            self.samples.fill(0.0);
            self.level *= 0.85;
        }

        for (i, &t) in target.iter().enumerate() {
            let cur = self.bands[i];
            // Fast attack, gravity-like release.
            self.bands[i] = if t > cur {
                cur + (t - cur) * 0.65
            } else {
                (cur - 0.035).max(t)
            };
            self.peaks[i] = if self.bands[i] >= self.peaks[i] {
                self.bands[i]
            } else {
                (self.peaks[i] - 0.012).max(0.0)
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sine_peaks_in_the_right_band() {
        let mut a = Analyzer::default();
        let hz = 1000.0;
        for (i, s) in a.samples.iter_mut().enumerate() {
            *s = (2.0 * std::f32::consts::PI * hz * i as f32 / SAMPLE_RATE).sin() * 0.8;
        }
        for _ in 0..20 {
            a.update(32, true);
        }
        let loudest = a
            .bands
            .iter()
            .enumerate()
            .max_by(|x, y| x.1.total_cmp(y.1))
            .unwrap()
            .0;
        let ratio = (MAX_HZ / MIN_HZ).powf(1.0 / 32.0);
        let expected = ((hz / MIN_HZ).ln() / ratio.ln()) as usize;
        assert!(
            (loudest as i32 - expected as i32).abs() <= 1,
            "loudest={loudest} expected={expected}"
        );
    }

    #[test]
    fn silence_decays_to_zero() {
        let mut a = Analyzer {
            bands: vec![1.0; 8],
            peaks: vec![1.0; 8],
            ..Default::default()
        };
        for _ in 0..200 {
            a.update(8, false);
        }
        assert!(a.bands.iter().all(|&b| b == 0.0));
    }
}
