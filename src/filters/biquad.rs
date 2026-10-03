//! RBJ-cookbook biquad shared by pass filters and EQ.

use std::f64::consts::PI;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BiquadKind {
    LowPass,
    HighPass,
    Peaking,
    LowShelf,
    HighShelf,
}

/// Normalized coefficients (a0 == 1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Coeffs {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
}

impl Coeffs {
    pub const IDENTITY: Coeffs = Coeffs { b0: 1.0, b1: 0.0, b2: 0.0, a1: 0.0, a2: 0.0 };

    /// Compute RBJ coefficients. `gain_db` only matters for peaking/shelves.
    /// Frequencies are clamped below Nyquist so the result is always stable.
    pub fn design(kind: BiquadKind, sample_rate: f32, freq: f32, q: f32, gain_db: f32) -> Coeffs {
        let sr = sample_rate.max(1.0) as f64;
        let f = (freq as f64).clamp(1.0, sr * 0.49);
        let q = (q as f64).max(0.01);
        let a = 10f64.powf(gain_db as f64 / 40.0);
        let w0 = 2.0 * PI * f / sr;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * q);
        let (b0, b1, b2, a0, a1, a2) = match kind {
            BiquadKind::LowPass => {
                ((1.0 - cos) / 2.0, 1.0 - cos, (1.0 - cos) / 2.0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha)
            }
            BiquadKind::HighPass => {
                ((1.0 + cos) / 2.0, -(1.0 + cos), (1.0 + cos) / 2.0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha)
            }
            BiquadKind::Peaking => (
                1.0 + alpha * a,
                -2.0 * cos,
                1.0 - alpha * a,
                1.0 + alpha / a,
                -2.0 * cos,
                1.0 - alpha / a,
            ),
            BiquadKind::LowShelf => {
                let s = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) - (a - 1.0) * cos + s),
                    2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
                    a * ((a + 1.0) - (a - 1.0) * cos - s),
                    (a + 1.0) + (a - 1.0) * cos + s,
                    -2.0 * ((a - 1.0) + (a + 1.0) * cos),
                    (a + 1.0) + (a - 1.0) * cos - s,
                )
            }
            BiquadKind::HighShelf => {
                let s = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) + (a - 1.0) * cos + s),
                    -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
                    a * ((a + 1.0) + (a - 1.0) * cos - s),
                    (a + 1.0) - (a - 1.0) * cos + s,
                    2.0 * ((a - 1.0) - (a + 1.0) * cos),
                    (a + 1.0) - (a - 1.0) * cos - s,
                )
            }
        };
        Coeffs { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: a1 / a0, a2: a2 / a0 }
    }
}

/// Multi-channel biquad (transposed direct form II), per-channel state.
/// Coefficients are only recomputed when a parameter setter is called.
pub struct Biquad {
    kind: BiquadKind,
    sample_rate: f32,
    freq: f32,
    q: f32,
    gain_db: f32,
    coeffs: Coeffs,
    /// Two state variables per channel.
    z: Vec<[f64; 2]>,
}

impl Biquad {
    pub fn new(kind: BiquadKind, sample_rate: f32, channels: usize, freq: f32, q: f32, gain_db: f32) -> Self {
        let mut b = Self {
            kind,
            sample_rate,
            freq,
            q,
            gain_db,
            coeffs: Coeffs::IDENTITY,
            z: vec![[0.0; 2]; channels.max(1)],
        };
        b.update();
        b
    }

    fn update(&mut self) {
        self.coeffs = Coeffs::design(self.kind, self.sample_rate, self.freq, self.q, self.gain_db);
    }

    /// Set parameters; recomputes coefficients only if something changed.
    pub fn set(&mut self, freq: f32, q: f32, gain_db: f32) {
        if freq != self.freq || q != self.q || gain_db != self.gain_db {
            self.freq = freq;
            self.q = q;
            self.gain_db = gain_db;
            self.update();
        }
    }

    pub fn set_gain_db(&mut self, gain_db: f32) {
        self.set(self.freq, self.q, gain_db);
    }

    pub fn reset(&mut self) {
        for z in &mut self.z {
            *z = [0.0; 2];
        }
    }

    #[inline]
    pub fn tick(&mut self, ch: usize, x: f32) -> f32 {
        let c = self.coeffs;
        let z = &mut self.z[ch];
        let x = x as f64;
        let y = c.b0 * x + z[0];
        z[0] = c.b1 * x - c.a1 * y + z[1];
        z[1] = c.b2 * x - c.a2 * y;
        y as f32
    }

    /// Process interleaved samples in place.
    pub fn process(&mut self, buf: &mut [f32]) {
        let n = self.z.len();
        for frame in buf.chunks_mut(n) {
            for (ch, s) in frame.iter_mut().enumerate() {
                *s = self.tick(ch, *s);
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod testutil {
    pub fn sine(freq: f32, sr: f32, channels: usize, frames: usize, amp: f32) -> Vec<f32> {
        let mut v = Vec::with_capacity(frames * channels);
        for i in 0..frames {
            let s = amp * (2.0 * std::f32::consts::PI * freq * i as f32 / sr).sin();
            for _ in 0..channels {
                v.push(s);
            }
        }
        v
    }
    /// RMS of the second half of an interleaved buffer (skips transient).
    pub fn rms_tail(buf: &[f32]) -> f32 {
        let t = &buf[buf.len() / 2..];
        (t.iter().map(|x| x * x).sum::<f32>() / t.len() as f32).sqrt()
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::*;
    use super::*;

    const SR: f32 = 48000.0;

    fn run(kind: BiquadKind, f: f32, q: f32, g: f32, tone: f32) -> f32 {
        let mut b = Biquad::new(kind, SR, 1, f, q, g);
        let mut buf = sine(tone, SR, 1, 24000, 0.5);
        b.process(&mut buf);
        rms_tail(&buf) / rms_tail(&sine(tone, SR, 1, 24000, 0.5))
    }

    #[test]
    fn lowpass_attenuates_highs_passes_lows() {
        assert!(run(BiquadKind::LowPass, 500.0, 0.707, 0.0, 10000.0) < 0.01);
        assert!((run(BiquadKind::LowPass, 5000.0, 0.707, 0.0, 100.0) - 1.0).abs() < 0.02);
    }

    #[test]
    fn highpass_attenuates_lows() {
        assert!(run(BiquadKind::HighPass, 2000.0, 0.707, 0.0, 100.0) < 0.01);
        assert!((run(BiquadKind::HighPass, 200.0, 0.707, 0.0, 8000.0) - 1.0).abs() < 0.02);
    }

    #[test]
    fn peaking_and_shelves_gain() {
        let g = run(BiquadKind::Peaking, 1000.0, 1.0, 12.0, 1000.0);
        assert!((20.0 * g.log10() - 12.0).abs() < 0.3);
        let g = run(BiquadKind::LowShelf, 200.0, 0.707, 12.0, 30.0);
        assert!((20.0 * g.log10() - 12.0).abs() < 0.5);
        let g = run(BiquadKind::HighShelf, 4000.0, 0.707, -12.0, 15000.0);
        assert!((20.0 * g.log10() + 12.0).abs() < 0.5);
    }

    #[test]
    fn stereo_channels_independent_and_finite() {
        let mut b = Biquad::new(BiquadKind::LowPass, SR, 2, 1000.0, 0.707, 0.0);
        let mut buf = vec![0.0f32; 2000];
        for (i, f) in buf.chunks_mut(2).enumerate() {
            f[0] = if i % 50 == 0 { 1.0 } else { 0.0 };
        }
        b.process(&mut buf);
        assert!(buf.iter().all(|x| x.is_finite()));
        assert!(buf.chunks(2).all(|f| f[1] == 0.0));
        assert!(buf.chunks(2).any(|f| f[0] != 0.0));
    }

    #[test]
    fn extreme_freq_is_stable() {
        let mut b = Biquad::new(BiquadKind::LowPass, 8000.0, 1, 20000.0, 10.0, 0.0);
        let mut buf = sine(1000.0, 8000.0, 1, 4000, 1.0);
        b.process(&mut buf);
        assert!(buf.iter().all(|x| x.is_finite() && x.abs() < 20.0));
    }
}
