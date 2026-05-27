//! A 10-band graphic equalizer applied in the audio path.
//!
//! librespot has no built-in EQ, so we wrap its output [`Sink`] with one that
//! runs a chain of peaking biquad filters over the decoded f64 samples before
//! they reach the real backend. Band gains live in a shared [`EqState`] of
//! atomics, so the UI can adjust them live from another thread; the filter
//! recomputes its coefficients only when a gain actually changes.

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Arc;

use librespot::playback::audio_backend::{Sink, SinkResult};
use librespot::playback::convert::Converter;
use librespot::playback::decoder::AudioPacket;

/// Spotify streams are 44.1 kHz stereo.
const SAMPLE_RATE: f64 = 44_100.0;
const CHANNELS: usize = 2;

/// Number of EQ bands.
pub const BANDS: usize = 10;

/// ISO-ish octave band centre frequencies (Hz).
pub const FREQS: [f64; BANDS] = [
    31.0, 62.0, 125.0, 250.0, 500.0, 1_000.0, 2_000.0, 4_000.0, 8_000.0, 16_000.0,
];

/// Short labels for the UI.
pub const LABELS: [&str; BANDS] = [
    "31", "62", "125", "250", "500", "1k", "2k", "4k", "8k", "16k",
];

/// Gain limit per band, in dB.
pub const MAX_DB: i32 = 12;

/// Common graphic-EQ presets (gain per band in dB), in the order
/// 31, 62, 125, 250, 500, 1k, 2k, 4k, 8k, 16k.
pub const PRESETS: &[(&str, [i32; BANDS])] = &[
    ("Flat", [0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
    ("Bass Boost", [8, 7, 6, 4, 2, 0, 0, 0, 0, 0]),
    ("Treble Boost", [0, 0, 0, 0, 0, 1, 3, 5, 7, 8]),
    ("Loudness", [7, 5, 0, -1, -2, -1, 0, 1, 5, 7]),
    ("Rock", [5, 4, 3, 1, -1, -1, 1, 3, 4, 5]),
    ("Pop", [-1, 1, 3, 4, 4, 2, 0, -1, -1, -2]),
    ("Hip-Hop", [6, 5, 4, 2, 0, -1, -1, 1, 2, 3]),
    ("Dance", [6, 5, 2, 0, 0, -3, -4, -4, 1, 2]),
    ("Jazz", [4, 3, 1, 2, -1, -1, 0, 1, 3, 4]),
    ("Classical", [5, 4, 3, 2, -1, -1, 0, 2, 3, 4]),
    ("Acoustic", [4, 4, 3, 1, 2, 2, 3, 3, 2, 1]),
    ("Electronic", [5, 4, 1, 0, -2, 1, 1, 2, 4, 5]),
    ("Vocal", [-2, -3, -2, 1, 4, 5, 4, 3, 1, -1]),
];

/// Shared, lock-free EQ state adjustable from the UI thread.
pub struct EqState {
    enabled: AtomicBool,
    gains_db: [AtomicI32; BANDS],
}

pub type SharedEq = Arc<EqState>;

impl EqState {
    pub fn new(enabled: bool, gains_db: &[i32]) -> SharedEq {
        let gains = std::array::from_fn(|i| {
            AtomicI32::new(gains_db.get(i).copied().unwrap_or(0).clamp(-MAX_DB, MAX_DB))
        });
        Arc::new(Self {
            enabled: AtomicBool::new(enabled),
            gains_db: gains,
        })
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    pub fn toggle(&self) {
        self.enabled.fetch_xor(true, Ordering::Relaxed);
    }

    pub fn gain(&self, band: usize) -> i32 {
        self.gains_db[band].load(Ordering::Relaxed)
    }

    /// Adjust a band by `delta` dB, clamped to ±[`MAX_DB`].
    pub fn adjust(&self, band: usize, delta: i32) {
        let v = (self.gain(band) + delta).clamp(-MAX_DB, MAX_DB);
        self.gains_db[band].store(v, Ordering::Relaxed);
    }

    /// Reset every band to flat (0 dB).
    pub fn reset(&self) {
        for g in &self.gains_db {
            g.store(0, Ordering::Relaxed);
        }
    }

    /// Snapshot of the gains, for persisting to config.
    pub fn gains(&self) -> Vec<i32> {
        self.gains_db.iter().map(|g| g.load(Ordering::Relaxed)).collect()
    }

    /// Set every band at once (e.g. when applying a preset).
    pub fn set_gains(&self, gains: &[i32]) {
        for (i, slot) in self.gains_db.iter().enumerate() {
            let v = gains.get(i).copied().unwrap_or(0).clamp(-MAX_DB, MAX_DB);
            slot.store(v, Ordering::Relaxed);
        }
    }

    /// Index of the [`PRESETS`] entry matching the current gains, if any.
    pub fn matched_preset(&self) -> Option<usize> {
        let cur = self.gains();
        PRESETS.iter().position(|(_, g)| g.as_slice() == cur.as_slice())
    }
}

/// A transposed-direct-form-II biquad with f64 state.
#[derive(Clone, Copy, Default)]
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
    /// RBJ cookbook peaking-EQ coefficients.
    fn peaking(freq: f64, q: f64, gain_db: f64, fs: f64) -> Self {
        let a = 10f64.powf(gain_db / 40.0);
        let w0 = 2.0 * std::f64::consts::PI * freq / fs;
        let (sin, cos) = (w0.sin(), w0.cos());
        let alpha = sin / (2.0 * q);

        let b0 = 1.0 + alpha * a;
        let b1 = -2.0 * cos;
        let b2 = 1.0 - alpha * a;
        let a0 = 1.0 + alpha / a;
        let a1 = -2.0 * cos;
        let a2 = 1.0 - alpha / a;

        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
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

/// The per-channel filter chains and a cache of the gains they were built for.
struct EqProcessor {
    filters: [[Biquad; BANDS]; CHANNELS],
    cached: [i32; BANDS],
}

impl EqProcessor {
    fn new() -> Self {
        Self {
            filters: [[Biquad::default(); BANDS]; CHANNELS],
            // Force a coefficient rebuild on the first packet.
            cached: [i32::MIN; BANDS],
        }
    }

    /// Recompute coefficients if any band gain changed. ~1.41 Q approximates
    /// one-octave-wide bands.
    fn sync(&mut self, state: &EqState) {
        let mut changed = false;
        for b in 0..BANDS {
            let g = state.gain(b);
            if g != self.cached[b] {
                self.cached[b] = g;
                changed = true;
            }
        }
        if !changed {
            return;
        }
        for ch in self.filters.iter_mut() {
            for (b, filter) in ch.iter_mut().enumerate() {
                let coeffs = Biquad::peaking(FREQS[b], 1.41, self.cached[b] as f64, SAMPLE_RATE);
                // Keep the running state; only replace coefficients.
                filter.b0 = coeffs.b0;
                filter.b1 = coeffs.b1;
                filter.b2 = coeffs.b2;
                filter.a1 = coeffs.a1;
                filter.a2 = coeffs.a2;
            }
        }
    }

    fn process(&mut self, samples: &mut [f64]) {
        for frame in samples.chunks_mut(CHANNELS) {
            for (ch, sample) in frame.iter_mut().enumerate() {
                let mut x = *sample;
                for filter in self.filters[ch].iter_mut() {
                    x = filter.process(x);
                }
                *sample = x;
            }
        }
    }
}

/// A [`Sink`] that EQs samples then forwards them to the real backend sink.
struct EqSink {
    inner: Box<dyn Sink>,
    proc: EqProcessor,
    state: SharedEq,
}

impl Sink for EqSink {
    fn start(&mut self) -> SinkResult<()> {
        self.inner.start()
    }

    fn stop(&mut self) -> SinkResult<()> {
        self.inner.stop()
    }

    fn write(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        let packet = match packet {
            AudioPacket::Samples(mut samples) if self.state.enabled() => {
                self.proc.sync(&self.state);
                self.proc.process(&mut samples);
                AudioPacket::Samples(samples)
            }
            other => other,
        };
        self.inner.write(packet, converter)
    }
}

/// Wrap a backend sink so the equalizer runs ahead of it.
pub fn wrap(inner: Box<dyn Sink>, state: SharedEq) -> Box<dyn Sink> {
    Box::new(EqSink {
        inner,
        proc: EqProcessor::new(),
        state,
    })
}
