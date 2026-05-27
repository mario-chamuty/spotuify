//! Real-time spectrum analysis.
//!
//! Rather than an FFT (which would need a shared sample buffer and allocation
//! on the audio thread), we run the audio through a bank of bandpass biquads —
//! one per EQ band — and publish each band's RMS energy through atomics. The
//! probe lives in the audio path ([`crate::eq::EqSink`]); the UI reads the
//! shared [`SpectrumState`].

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use crate::eq::{BANDS, FREQS};

const SAMPLE_RATE: f64 = 44_100.0;
/// Samples between RMS updates (~23 ms at 44.1 kHz).
const WINDOW: usize = 1024;

/// Per-band RMS levels: written by the audio probe, read by the UI.
pub struct SpectrumState {
    bands: [AtomicU32; BANDS],
}

pub type SharedSpectrum = Arc<SpectrumState>;

impl SpectrumState {
    pub fn new() -> SharedSpectrum {
        Arc::new(Self {
            bands: std::array::from_fn(|_| AtomicU32::new(0)),
        })
    }

    /// Current RMS energy for band `i` (linear, 0..~1).
    pub fn band(&self, i: usize) -> f32 {
        f32::from_bits(self.bands[i].load(Ordering::Relaxed))
    }

    fn set_band(&self, i: usize, v: f32) {
        self.bands[i].store(v.to_bits(), Ordering::Relaxed);
    }
}

/// A bank of bandpass filters that measures per-band energy on the audio
/// thread. Owned by the sink; updates a [`SharedSpectrum`].
pub struct SpectrumProbe {
    filters: [Biquad; BANDS],
    sumsq: [f64; BANDS],
    count: usize,
    state: SharedSpectrum,
}

impl SpectrumProbe {
    pub fn new(state: SharedSpectrum) -> Self {
        Self {
            filters: std::array::from_fn(|i| Biquad::bandpass(FREQS[i], 1.41, SAMPLE_RATE)),
            sumsq: [0.0; BANDS],
            count: 0,
            state,
        }
    }

    /// Feed interleaved-stereo f64 samples (mixed to mono per frame).
    pub fn feed(&mut self, samples: &[f64]) {
        for frame in samples.chunks(2) {
            let mono = if frame.len() == 2 {
                (frame[0] + frame[1]) * 0.5
            } else {
                frame[0]
            };
            for (b, filter) in self.filters.iter_mut().enumerate() {
                let y = filter.process(mono);
                self.sumsq[b] += y * y;
            }
            self.count += 1;
            if self.count >= WINDOW {
                for b in 0..BANDS {
                    let rms = (self.sumsq[b] / self.count as f64).sqrt() as f32;
                    self.state.set_band(b, rms);
                    self.sumsq[b] = 0.0;
                }
                self.count = 0;
            }
        }
    }
}

/// RBJ constant-peak-gain bandpass biquad (transposed direct form II).
struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    z1: f64,
    z2: f64,
}

impl Biquad {
    fn bandpass(freq: f64, q: f64, fs: f64) -> Self {
        let w0 = 2.0 * std::f64::consts::PI * freq / fs;
        let (sin, cos) = (w0.sin(), w0.cos());
        let alpha = sin / (2.0 * q);
        let a0 = 1.0 + alpha;
        Self {
            b0: alpha / a0,
            b1: 0.0,
            b2: -alpha / a0,
            a1: -2.0 * cos / a0,
            a2: (1.0 - alpha) / a0,
            z1: 0.0,
            z2: 0.0,
        }
    }

    #[inline]
    fn process(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
}
