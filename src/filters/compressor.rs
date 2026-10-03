//! Feed-forward compressor with soft knee and linked-channel detection.

use crate::plugin::{Filter, FilterContext, FilterDescriptor, ParamSpec};

struct Compressor {
    sr: f32,
    channels: usize,
    threshold: f32,
    ratio: f32,
    knee: f32,
    makeup_lin: f32,
    att: f32,
    rel: f32,
    /// Smoothed gain reduction in dB (>= 0).
    gr: f32,
}

fn coef(ms: f32, sr: f32) -> f32 {
    (-1.0 / (ms.max(0.01) * 0.001 * sr)).exp()
}

impl Compressor {
    /// Static curve: gain reduction (dB, >= 0) for input level in dB.
    fn target_gr(&self, level_db: f32) -> f32 {
        let over = level_db - self.threshold;
        let slope = 1.0 - 1.0 / self.ratio;
        if self.knee > 0.0 && over.abs() <= self.knee / 2.0 {
            let x = over + self.knee / 2.0;
            slope * x * x / (2.0 * self.knee)
        } else if over > 0.0 {
            slope * over
        } else {
            0.0
        }
    }
}

impl Filter for Compressor {
    fn set_param(&mut self, index: usize, value: f32) {
        match index {
            0 => self.threshold = value,
            1 => self.ratio = value.max(1.0),
            2 => self.att = coef(value, self.sr),
            3 => self.rel = coef(value, self.sr),
            4 => self.knee = value,
            5 => self.makeup_lin = 10f32.powf(value / 20.0),
            _ => {}
        }
    }

    fn process(&mut self, buf: &mut [f32]) {
        for frame in buf.chunks_mut(self.channels) {
            let peak = frame.iter().fold(0.0f32, |m, s| m.max(s.abs()));
            let level_db = 20.0 * peak.max(1e-6).log10();
            let target = self.target_gr(level_db);
            let c = if target > self.gr { self.att } else { self.rel };
            self.gr = target + c * (self.gr - target);
            let g = 10f32.powf(-self.gr / 20.0) * self.makeup_lin;
            for s in frame.iter_mut() {
                *s *= g;
            }
        }
    }

    fn reset(&mut self) {
        self.gr = 0.0;
    }

    fn meter(&self) -> Option<f32> {
        Some(self.gr)
    }
}

pub fn descriptor() -> FilterDescriptor {
    FilterDescriptor::native(
        "compressor",
        "Compressor",
        "Dynamics",
        vec![
            ParamSpec::new("threshold", "Threshold", -60.0, 0.0, -18.0).unit("dB"),
            ParamSpec::new("ratio", "Ratio", 1.0, 20.0, 4.0).unit(":1"),
            ParamSpec::new("attack", "Attack", 0.1, 100.0, 10.0).unit("ms").log(),
            ParamSpec::new("release", "Release", 10.0, 1000.0, 120.0).unit("ms").log(),
            ParamSpec::new("knee", "Knee", 0.0, 12.0, 6.0).unit("dB"),
            ParamSpec::new("makeup", "Makeup", 0.0, 24.0, 0.0).unit("dB"),
        ],
        |ctx: &FilterContext| {
            Box::new(Compressor {
                sr: ctx.sample_rate,
                channels: ctx.channels.max(1),
                threshold: -18.0,
                ratio: 4.0,
                knee: 6.0,
                makeup_lin: 1.0,
                att: coef(10.0, ctx.sample_rate),
                rel: coef(120.0, ctx.sample_rate),
                gr: 0.0,
            })
        },
    )
    .meter("Gain reduction")
    .description("Reduces dynamic range of loud passages.")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make(vals: &[f32]) -> Box<dyn Filter> {
        let ctx = FilterContext { sample_rate: 48000.0, channels: 2, realtime: false };
        descriptor().instantiate(&ctx, vals).unwrap()
    }

    fn sine(amp: f32, frames: usize) -> Vec<f32> {
        let mut v = Vec::new();
        for i in 0..frames {
            let s = amp * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 48000.0).sin();
            v.push(s);
            v.push(s);
        }
        v
    }

    fn peak_tail(b: &[f32]) -> f32 {
        b[b.len() / 2..].iter().fold(0.0, |m, s| s.abs().max(m))
    }

    #[test]
    fn reduces_loud_signal() {
        let mut f = make(&descriptor().defaults());
        let mut b = sine(1.0, 48000);
        f.process(&mut b);
        assert!(b.iter().all(|x| x.is_finite()));
        let p = peak_tail(&b);
        assert!(p < 0.6, "{p}");
        let gr = f.meter().unwrap();
        assert!(gr > 6.0, "{gr}");
    }

    #[test]
    fn leaves_quiet_signal() {
        let mut f = make(&descriptor().defaults());
        let mut b = sine(0.01, 24000);
        let orig = b.clone();
        f.process(&mut b);
        for (a, o) in b.iter().zip(orig.iter()) {
            assert!((a - o).abs() < 1e-5);
        }
        assert!(f.meter().unwrap() < 0.01);
    }

    #[test]
    fn makeup_and_ratio_one() {
        // ratio 1 => no compression; makeup 6 dB doubles level.
        let mut f = make(&[-18.0, 1.0, 10.0, 120.0, 6.0, 6.0206]);
        let mut b = sine(0.5, 4800);
        let orig = b.clone();
        f.process(&mut b);
        for (a, o) in b.iter().zip(orig.iter()) {
            assert!((a - 2.0 * o).abs() < 1e-3);
        }
    }

    #[test]
    fn linked_stereo() {
        let mut f = make(&descriptor().defaults());
        let mut b = Vec::new();
        for _ in 0..24000 {
            b.push(1.0);
            b.push(0.1);
        }
        f.process(&mut b);
        let l = b[b.len() - 2];
        let r = b[b.len() - 1];
        // same gain applied to both channels
        assert!((r / l - 0.1).abs() < 1e-4);
    }
}
