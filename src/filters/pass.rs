//! Low-pass and high-pass filters.

use super::biquad::{Biquad, BiquadKind};
use crate::plugin::{Filter, FilterContext, FilterDescriptor, ParamSpec};

struct PassFilter {
    bq: Biquad,
    freq: f32,
    q: f32,
}

impl Filter for PassFilter {
    fn set_param(&mut self, index: usize, value: f32) {
        match index {
            0 => self.freq = value,
            1 => self.q = value,
            _ => return,
        }
        self.bq.set(self.freq, self.q, 0.0);
    }
    fn process(&mut self, buf: &mut [f32]) {
        self.bq.process(buf);
    }
    fn reset(&mut self) {
        self.bq.reset();
    }
}

fn params(default_cutoff: f32) -> Vec<ParamSpec> {
    vec![
        ParamSpec::new("cutoff", "Cutoff", 20.0, 20000.0, default_cutoff).unit("Hz").log(),
        ParamSpec::new("q", "Resonance (Q)", 0.1, 10.0, 0.707),
    ]
}

fn make(kind: BiquadKind, ctx: &FilterContext, cutoff: f32) -> Box<dyn Filter> {
    Box::new(PassFilter {
        bq: Biquad::new(kind, ctx.sample_rate, ctx.channels, cutoff, 0.707, 0.0),
        freq: cutoff,
        q: 0.707,
    })
}

pub fn lowpass_descriptor() -> FilterDescriptor {
    FilterDescriptor::native("lowpass", "Low-pass", "Tone", params(1000.0), |ctx| {
        make(BiquadKind::LowPass, ctx, 1000.0)
    })
    .description("Removes frequencies above the cutoff.")
}

pub fn highpass_descriptor() -> FilterDescriptor {
    FilterDescriptor::native("highpass", "High-pass", "Tone", params(200.0), |ctx| {
        make(BiquadKind::HighPass, ctx, 200.0)
    })
    .description("Removes frequencies below the cutoff.")
}

#[cfg(test)]
mod tests {
    use super::super::biquad::testutil::*;
    use super::*;

    fn ctx() -> FilterContext {
        FilterContext { sample_rate: 48000.0, channels: 2, realtime: false }
    }

    fn gain_of(d: &FilterDescriptor, vals: &[f32], tone: f32) -> f32 {
        let mut f = d.instantiate(&ctx(), vals).unwrap();
        let input = sine(tone, 48000.0, 2, 24000, 0.5);
        let mut buf = input.clone();
        f.process(&mut buf);
        assert!(buf.iter().all(|x| x.is_finite()));
        rms_tail(&buf) / rms_tail(&input)
    }

    #[test]
    fn lowpass_cuts_10k_at_500() {
        let d = lowpass_descriptor();
        assert!(gain_of(&d, &[500.0, 0.707], 10000.0) < 0.01);
        assert!(gain_of(&d, &[500.0, 0.707], 50.0) > 0.95);
    }

    #[test]
    fn highpass_cuts_lows() {
        let d = highpass_descriptor();
        assert_eq!(d.params[0].default, 200.0);
        assert!(gain_of(&d, &[2000.0, 0.707], 100.0) < 0.01);
        assert!(gain_of(&d, &[200.0, 0.707], 5000.0) > 0.95);
    }
}
