//! Waveshaping distortion with post tone filter.

use super::biquad::{Biquad, BiquadKind};
use crate::plugin::{Filter, FilterContext, FilterDescriptor, ParamSpec};

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Soft,
    Hard,
    Fuzz,
}

struct Distortion {
    drive: f32,
    mode: Mode,
    mix: f32,
    out: f32,
    tone: Biquad,
    channels: usize,
}

#[inline]
fn shape(mode: Mode, x: f32) -> f32 {
    match mode {
        Mode::Soft => x.tanh(),
        Mode::Hard => x.clamp(-1.0, 1.0),
        // Asymmetric: positive half softly clipped, negative half hard-ish.
        Mode::Fuzz => {
            if x >= 0.0 {
                (x * 2.0).tanh()
            } else {
                (x * 0.7).clamp(-0.6, 0.0) * 1.2
            }
        }
    }
}

impl Filter for Distortion {
    fn set_param(&mut self, index: usize, value: f32) {
        match index {
            0 => self.drive = 10f32.powf(value / 20.0),
            1 => {
                self.mode = match value.round() as i32 {
                    1 => Mode::Hard,
                    2 => Mode::Fuzz,
                    _ => Mode::Soft,
                }
            }
            2 => {
                self.tone.set(value, 0.707, 0.0);
            }
            3 => self.mix = value,
            4 => self.out = 10f32.powf(value / 20.0),
            _ => {}
        }
    }

    fn process(&mut self, buf: &mut [f32]) {
        for frame in buf.chunks_mut(self.channels) {
            for (ch, s) in frame.iter_mut().enumerate() {
                let dry = *s;
                let wet = self.tone.tick(ch, shape(self.mode, dry * self.drive));
                *s = (dry + (wet - dry) * self.mix) * self.out;
            }
        }
    }

    fn reset(&mut self) {
        self.tone.reset();
    }
}

pub fn descriptor() -> FilterDescriptor {
    FilterDescriptor::native(
        "distortion",
        "Distortion",
        "Drive",
        vec![
            ParamSpec::new("drive", "Drive", 0.0, 40.0, 12.0).unit("dB"),
            ParamSpec::new("mode", "Mode", 0.0, 2.0, 0.0).choices(&["soft", "hard", "fuzz"]),
            ParamSpec::new("tone", "Tone", 500.0, 20000.0, 8000.0).unit("Hz").log(),
            ParamSpec::new("mix", "Mix", 0.0, 1.0, 1.0),
            ParamSpec::new("output", "Output", -24.0, 6.0, -6.0).unit("dB"),
        ],
        |ctx: &FilterContext| {
            Box::new(Distortion {
                drive: 10f32.powf(12.0 / 20.0),
                mode: Mode::Soft,
                mix: 1.0,
                out: 10f32.powf(-6.0 / 20.0),
                tone: Biquad::new(BiquadKind::LowPass, ctx.sample_rate, ctx.channels, 8000.0, 0.707, 0.0),
                channels: ctx.channels.max(1),
            })
        },
    )
    .description("Soft/hard clipping and fuzz with tone control.")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(vals: &[f32], amp: f32) -> Vec<f32> {
        let ctx = FilterContext { sample_rate: 48000.0, channels: 2, realtime: false };
        let mut f = descriptor().instantiate(&ctx, vals).unwrap();
        let mut b = Vec::new();
        for i in 0..4800 {
            let s = amp * (2.0 * std::f32::consts::PI * 220.0 * i as f32 / 48000.0).sin();
            b.push(s);
            b.push(s);
        }
        f.process(&mut b);
        b
    }

    #[test]
    fn all_modes_finite_and_bounded() {
        for mode in 0..3 {
            let b = run(&[40.0, mode as f32, 8000.0, 1.0, 6.0], 1.0);
            assert!(b.iter().all(|x| x.is_finite() && x.abs() < 3.0), "mode {mode}");
        }
    }

    #[test]
    fn drive_adds_gain_to_quiet_signal_and_clips() {
        let b = run(&[30.0, 1.0, 20000.0, 1.0, 0.0], 0.5);
        let peak = b.iter().fold(0.0f32, |m, x| m.max(x.abs()));
        assert!(peak > 0.9 && peak < 1.1, "{peak}");
    }

    #[test]
    fn mix_zero_is_dry() {
        let b = run(&[30.0, 0.0, 8000.0, 0.0, 0.0], 0.5);
        let peak = b.iter().fold(0.0f32, |m, x| m.max(x.abs()));
        assert!((peak - 0.5).abs() < 0.01);
    }

    #[test]
    fn tone_darkens() {
        let ctx = FilterContext { sample_rate: 48000.0, channels: 1, realtime: false };
        let energy = |tone: f32| {
            let mut f = descriptor().instantiate(&ctx, &[20.0, 1.0, tone, 1.0, 0.0]).unwrap();
            let mut b: Vec<f32> = (0..9600)
                .map(|i| 0.8 * (2.0 * std::f32::consts::PI * 9000.0 * i as f32 / 48000.0).sin())
                .collect();
            f.process(&mut b);
            b[4800..].iter().map(|x| x * x).sum::<f32>()
        };
        assert!(energy(500.0) < energy(20000.0) * 0.1);
    }
}
