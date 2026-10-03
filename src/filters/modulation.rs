//! Chorus / flanger (modulated delay lines).

use super::delayline::DelayLine;
use crate::plugin::{Filter, FilterContext, FilterDescriptor, ParamSpec};

struct ModFx {
    lines: Vec<DelayLine>,
    channels: usize,
    sr: f32,
    phase: f32,
    rate: f32,
    depth_ms: f32,
    delay_ms: f32,
    feedback: f32,
    mix: f32,
    /// Last wet sample per channel (for flanger feedback).
    fb_state: Vec<f32>,
    flanger: bool,
}

impl ModFx {
    fn new(ctx: &FilterContext, max_ms: f32, flanger: bool) -> Self {
        let channels = ctx.channels.max(1);
        let cap = (max_ms * 0.001 * ctx.sample_rate).ceil() as usize + 4;
        Self {
            lines: (0..channels).map(|_| DelayLine::new(cap)).collect(),
            channels,
            sr: ctx.sample_rate,
            phase: 0.0,
            rate: 0.8,
            depth_ms: 3.0,
            delay_ms: 15.0,
            feedback: 0.0,
            mix: 0.5,
            fb_state: vec![0.0; channels],
            flanger,
        }
    }
}

impl Filter for ModFx {
    fn set_param(&mut self, index: usize, value: f32) {
        match (self.flanger, index) {
            (_, 0) => self.rate = value,
            (_, 1) => self.depth_ms = value,
            (_, 2) => self.delay_ms = value,
            (true, 3) => self.feedback = value,
            (true, 4) | (false, 3) => self.mix = value,
            _ => {}
        }
    }

    fn process(&mut self, buf: &mut [f32]) {
        let inc = self.rate / self.sr;
        let ms = 0.001 * self.sr;
        let ch = self.channels;
        for frame in buf.chunks_exact_mut(ch) {
            for (c, s) in frame.iter_mut().enumerate() {
                let p = self.phase + c as f32 / ch as f32 * 0.5;
                let lfo = (p * std::f32::consts::TAU).sin();
                let d = (self.delay_ms + self.depth_ms * lfo) * ms;
                let line = &mut self.lines[c];
                let wet = line.read(d);
                line.push(*s + self.feedback * self.fb_state[c]);
                self.fb_state[c] = wet;
                *s = *s * (1.0 - self.mix) + wet * self.mix;
            }
            self.phase += inc;
            if self.phase >= 1.0 {
                self.phase -= 1.0;
            }
        }
    }

    fn reset(&mut self) {
        self.lines.iter_mut().for_each(DelayLine::clear);
        self.fb_state.fill(0.0);
        self.phase = 0.0;
    }
}

pub fn chorus_descriptor() -> FilterDescriptor {
    FilterDescriptor::native(
        "chorus",
        "Chorus",
        "Modulation",
        vec![
            ParamSpec::new("rate", "Rate", 0.05, 5.0, 0.8).unit("Hz").log(),
            ParamSpec::new("depth", "Depth", 0.0, 10.0, 3.0).unit("ms"),
            ParamSpec::new("delay", "Delay", 5.0, 30.0, 15.0).unit("ms"),
            ParamSpec::new("mix", "Mix", 0.0, 1.0, 0.5),
        ],
        |ctx| Box::new(ModFx::new(ctx, 41.0, false)),
    )
    .description("Thickens the sound with a slowly modulated short delay.")
}

pub fn flanger_descriptor() -> FilterDescriptor {
    FilterDescriptor::native(
        "flanger",
        "Flanger",
        "Modulation",
        vec![
            ParamSpec::new("rate", "Rate", 0.05, 5.0, 0.25).unit("Hz").log(),
            ParamSpec::new("depth", "Depth", 0.0, 5.0, 2.0).unit("ms"),
            ParamSpec::new("delay", "Delay", 0.1, 5.0, 1.0).unit("ms"),
            ParamSpec::new("feedback", "Feedback", -0.95, 0.95, 0.5),
            ParamSpec::new("mix", "Mix", 0.0, 1.0, 0.5),
        ],
        |ctx| Box::new(ModFx::new(ctx, 11.0, true)),
    )
    .description("Sweeping comb-filter effect from a short modulated delay with feedback.")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> FilterContext {
        FilterContext { sample_rate: 48000.0, channels: 2, realtime: false }
    }

    fn noise(n: usize) -> Vec<f32> {
        let mut s = 12345u32;
        (0..n)
            .map(|_| {
                s = s.wrapping_mul(1664525).wrapping_add(1013904223);
                (s >> 8) as f32 / (1u32 << 23) as f32 - 1.0
            })
            .collect()
    }

    #[test]
    fn bounded_and_finite() {
        for d in [chorus_descriptor(), flanger_descriptor()] {
            let mut vals = d.defaults();
            if d.id == "flanger" {
                vals[3] = 0.95;
            }
            let mut f = d.instantiate(&ctx(), &vals).unwrap();
            let mut b = noise(48000 * 2 * 4);
            for c in b.chunks_mut(1024) {
                f.process(c);
            }
            assert!(b.iter().all(|x| x.is_finite() && x.abs() < 40.0), "{}", d.id);
        }
    }

    #[test]
    fn mix_zero_is_dry() {
        let d = chorus_descriptor();
        let mut vals = d.defaults();
        vals[3] = 0.0;
        let mut f = d.instantiate(&ctx(), &vals).unwrap();
        let src = noise(2000);
        let mut b = src.clone();
        f.process(&mut b);
        assert_eq!(b, src);
    }

    #[test]
    fn wet_appears_after_delay() {
        let d = chorus_descriptor();
        let mut vals = d.defaults();
        vals[1] = 0.0; // no modulation
        vals[3] = 1.0;
        let mut f = d.instantiate(&ctx(), &vals).unwrap();
        let mut b = vec![0.0; 2 * 2000];
        b[0] = 1.0;
        f.process(&mut b);
        let idx = (15.0f32 * 48.0) as usize; // 720 frames
        assert!((b[idx * 2] - 1.0).abs() < 1e-4);
        assert!(b[..idx * 2].iter().all(|x| x.abs() < 1e-6));
    }
}
