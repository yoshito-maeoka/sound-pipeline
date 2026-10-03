//! Gain and pan.

use crate::plugin::{Filter, FilterContext, FilterDescriptor, ParamSpec};

struct Gain {
    channels: usize,
    lin: f32,
    pan: f32,
    gl: f32,
    gr: f32,
}

impl Gain {
    fn update(&mut self, gain_db: Option<f32>, pan: Option<f32>) {
        if let Some(g) = gain_db {
            self.lin = 10f32.powf(g / 20.0);
        }
        if let Some(p) = pan {
            self.pan = p;
        }
        // Balance-style pan: center is unity, the far side is attenuated.
        self.gl = self.lin * (1.0 - self.pan.max(0.0));
        self.gr = self.lin * (1.0 + self.pan.min(0.0));
    }
}

impl Filter for Gain {
    fn set_param(&mut self, index: usize, value: f32) {
        match index {
            0 => self.update(Some(value), None),
            1 => self.update(None, Some(value)),
            _ => {}
        }
    }

    fn process(&mut self, buf: &mut [f32]) {
        if self.channels == 2 {
            for f in buf.chunks_exact_mut(2) {
                f[0] *= self.gl;
                f[1] *= self.gr;
            }
        } else {
            for s in buf.iter_mut() {
                *s *= self.lin;
            }
        }
    }
}

pub fn descriptor() -> FilterDescriptor {
    FilterDescriptor::native(
        "gain",
        "Gain",
        "Tone",
        vec![
            ParamSpec::new("gain", "Gain", -60.0, 24.0, 0.0).unit("dB"),
            ParamSpec::new("pan", "Pan", -1.0, 1.0, 0.0),
        ],
        |ctx: &FilterContext| {
            Box::new(Gain { channels: ctx.channels, lin: 1.0, pan: 0.0, gl: 1.0, gr: 1.0 })
        },
    )
    .description("Volume control with stereo pan.")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(ch: usize, vals: &[f32], input: &[f32]) -> Vec<f32> {
        let ctx = FilterContext { sample_rate: 48000.0, channels: ch, realtime: false };
        let mut f = descriptor().instantiate(&ctx, vals).unwrap();
        let mut b = input.to_vec();
        f.process(&mut b);
        b
    }

    #[test]
    fn gain_db_applied() {
        let o = run(2, &[-6.0206, 0.0], &[1.0, 1.0]);
        assert!((o[0] - 0.5).abs() < 1e-3 && (o[1] - 0.5).abs() < 1e-3);
    }

    #[test]
    fn pan_hard_right_mutes_left() {
        let o = run(2, &[0.0, 1.0], &[1.0, 1.0]);
        assert_eq!(o, vec![0.0, 1.0]);
        let o = run(2, &[0.0, -1.0], &[1.0, 1.0]);
        assert_eq!(o, vec![1.0, 0.0]);
    }

    #[test]
    fn mono_ignores_pan() {
        let o = run(1, &[0.0, 1.0], &[0.5, 0.5]);
        assert_eq!(o, vec![0.5, 0.5]);
    }
}
