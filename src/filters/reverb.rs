//! Freeverb-style stereo reverb.

use super::delayline::DelayLine;
use crate::plugin::{Filter, FilterContext, FilterDescriptor, ParamSpec};

const COMBS: [usize; 8] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
const ALLPASSES: [usize; 4] = [556, 441, 341, 225];
const SPREAD: usize = 23;
const INPUT_GAIN: f32 = 0.015;
const WET_SCALE: f32 = 3.0;

struct Comb {
    buf: Vec<f32>,
    pos: usize,
    store: f32,
}

impl Comb {
    fn new(n: usize) -> Self {
        Self { buf: vec![0.0; n.max(1)], pos: 0, store: 0.0 }
    }
    #[inline]
    fn tick(&mut self, x: f32, fb: f32, damp: f32) -> f32 {
        let out = self.buf[self.pos];
        self.store = out * (1.0 - damp) + self.store * damp;
        self.buf[self.pos] = x + self.store * fb;
        self.pos += 1;
        if self.pos == self.buf.len() {
            self.pos = 0;
        }
        out
    }
}

struct Allpass {
    buf: Vec<f32>,
    pos: usize,
}

impl Allpass {
    fn new(n: usize) -> Self {
        Self { buf: vec![0.0; n.max(1)], pos: 0 }
    }
    #[inline]
    fn tick(&mut self, x: f32) -> f32 {
        let b = self.buf[self.pos];
        self.buf[self.pos] = x + b * 0.5;
        self.pos += 1;
        if self.pos == self.buf.len() {
            self.pos = 0;
        }
        b - x
    }
}

struct Chan {
    combs: Vec<Comb>,
    allpasses: Vec<Allpass>,
    pre: DelayLine,
}

struct Reverb {
    chans: Vec<Chan>,
    sr: f32,
    feedback: f32,
    damp: f32,
    width: f32,
    predelay: f32,
    mix: f32,
}

impl Reverb {
    fn new(ctx: &FilterContext) -> Self {
        let scale = ctx.sample_rate / 44100.0;
        let sc = |n: usize, extra: usize| (((n + extra) as f32) * scale).round() as usize;
        let chans = (0..ctx.channels.max(1))
            .map(|c| {
                let extra = SPREAD * (c % 2);
                Chan {
                    combs: COMBS.iter().map(|&n| Comb::new(sc(n, extra))).collect(),
                    allpasses: ALLPASSES.iter().map(|&n| Allpass::new(sc(n, extra))).collect(),
                    pre: DelayLine::new((0.2 * ctx.sample_rate) as usize + 2),
                }
            })
            .collect();
        Self {
            chans,
            sr: ctx.sample_rate,
            feedback: 0.6 * 0.28 + 0.7,
            damp: 0.5 * 0.4,
            width: 1.0,
            predelay: 0.01 * ctx.sample_rate,
            mix: 0.3,
        }
    }

    #[inline]
    fn run(ch: &mut Chan, x: f32, fb: f32, damp: f32) -> f32 {
        let mut sum = 0.0;
        for c in &mut ch.combs {
            sum += c.tick(x, fb, damp);
        }
        for a in &mut ch.allpasses {
            sum = a.tick(sum);
        }
        sum
    }
}

impl Filter for Reverb {
    fn set_param(&mut self, index: usize, value: f32) {
        match index {
            0 => self.feedback = value * 0.28 + 0.7,
            1 => self.damp = value * 0.4,
            2 => self.width = value,
            3 => self.predelay = value * 0.001 * self.sr,
            4 => self.mix = value,
            _ => {}
        }
    }

    fn process(&mut self, buf: &mut [f32]) {
        let n = self.chans.len();
        let (fb, damp, mix) = (self.feedback, self.damp, self.mix);
        let pre = self.predelay;
        let use_pre = pre >= 1.0;
        let w1 = self.width * 0.5 + 0.5;
        let w2 = (1.0 - self.width) * 0.5;
        for frame in buf.chunks_exact_mut(n) {
            if n == 2 {
                let inp = (frame[0] + frame[1]) * INPUT_GAIN;
                let mut x = [0.0; 2];
                for (c, xc) in x.iter_mut().enumerate() {
                    let ch = &mut self.chans[c];
                    *xc = if use_pre { ch.pre.read(pre) } else { inp };
                    ch.pre.push(inp);
                }
                let l = Self::run(&mut self.chans[0], x[0], fb, damp);
                let r = Self::run(&mut self.chans[1], x[1], fb, damp);
                let wl = (l * w1 + r * w2) * WET_SCALE;
                let wr = (r * w1 + l * w2) * WET_SCALE;
                frame[0] = frame[0] * (1.0 - mix) + wl * mix;
                frame[1] = frame[1] * (1.0 - mix) + wr * mix;
            } else {
                for (c, s) in frame.iter_mut().enumerate() {
                    let ch = &mut self.chans[c];
                    let inp = *s * INPUT_GAIN;
                    let x = if use_pre { ch.pre.read(pre) } else { inp };
                    ch.pre.push(inp);
                    let w = Self::run(ch, x, fb, damp) * WET_SCALE;
                    *s = *s * (1.0 - mix) + w * mix;
                }
            }
        }
    }

    fn reset(&mut self) {
        for ch in &mut self.chans {
            ch.pre.clear();
            for c in &mut ch.combs {
                c.buf.fill(0.0);
                c.store = 0.0;
            }
            for a in &mut ch.allpasses {
                a.buf.fill(0.0);
            }
        }
    }
}

pub fn descriptor() -> FilterDescriptor {
    FilterDescriptor::native(
        "reverb",
        "Reverb",
        "Time",
        vec![
            ParamSpec::new("size", "Size", 0.0, 1.0, 0.6),
            ParamSpec::new("damping", "Damping", 0.0, 1.0, 0.5),
            ParamSpec::new("width", "Width", 0.0, 1.0, 1.0),
            ParamSpec::new("predelay", "Pre-delay", 0.0, 200.0, 10.0).unit("ms"),
            ParamSpec::new("mix", "Mix", 0.0, 1.0, 0.3),
        ],
        |ctx| Box::new(Reverb::new(ctx)),
    )
    .description("Freeverb-style stereo room reverb.")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(channels: usize) -> FilterContext {
        FilterContext { sample_rate: 44100.0, channels, realtime: false }
    }

    #[test]
    fn tail_decays() {
        let d = descriptor();
        let mut vals = d.defaults();
        vals[4] = 1.0;
        let mut f = d.instantiate(&ctx(2), &vals).unwrap();
        let mut b = vec![0.0; 2 * 44100 * 6];
        b[0] = 1.0;
        b[1] = 1.0;
        f.process(&mut b);
        let en = |a: usize, z: usize| b[a * 2 * 4410..z * 2 * 4410].iter().map(|x| x * x).sum::<f32>();
        let early = en(1, 5);
        let late = en(50, 60);
        assert!(early > 1e-6, "no tail");
        assert!(late < early * 0.01, "early {early} late {late}");
        assert!(b.iter().all(|x| x.is_finite()));
    }

    #[test]
    fn predelay_delays_onset() {
        let d = descriptor();
        let mut f = d.instantiate(&ctx(2), &[0.6, 0.5, 1.0, 100.0, 1.0]).unwrap();
        let mut b = vec![0.0; 2 * 8000];
        b[0] = 1.0;
        b[1] = 1.0;
        f.process(&mut b);
        // 100 ms = 4410 frames
        assert!(b[..2 * 4410].iter().all(|x| x.abs() < 1e-9));
        assert!(b[2 * 4410..].iter().any(|x| x.abs() > 1e-6));
    }

    #[test]
    fn bounded_on_noise_all_channel_counts() {
        for ch in [1, 2, 3] {
            let d = descriptor();
            let mut vals = d.defaults();
            vals[0] = 1.0;
            vals[1] = 0.0;
            vals[4] = 1.0;
            let mut f = d.instantiate(&ctx(ch), &vals).unwrap();
            let mut s = 1u32;
            let mut b: Vec<f32> = (0..44100 * ch * 5)
                .map(|_| {
                    s = s.wrapping_mul(1664525).wrapping_add(1013904223);
                    (s >> 8) as f32 / (1u32 << 23) as f32 - 1.0
                })
                .collect();
            for c in b.chunks_mut(1000 * ch) {
                f.process(c);
            }
            let m = b.iter().fold(0.0f32, |a, x| a.max(x.abs()));
            assert!(m.is_finite() && m < 30.0, "ch {ch} max {m}");
        }
    }
}
