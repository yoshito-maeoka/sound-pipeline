//! Delay and tape-style echo.

use super::delayline::{smooth_coef, DelayLine};
use crate::plugin::{Filter, FilterContext, FilterDescriptor, ParamSpec};

const MAX_MS: f32 = 2000.0;

struct Delay {
    lines: Vec<DelayLine>,
    channels: usize,
    sr: f32,
    coef: f32,
    cur: f32,
    target: f32,
    fresh: bool,
    feedback: f32,
    mix: f32,
    // echo only
    echo: bool,
    lp_a: f32,
    lp: Vec<f32>,
    pingpong: bool,
}

impl Delay {
    fn new(ctx: &FilterContext, echo: bool) -> Self {
        let channels = ctx.channels.max(1);
        let cap = (MAX_MS * 0.001 * ctx.sample_rate).ceil() as usize + 2;
        let t = if echo { 300.0 } else { 350.0 } * 0.001 * ctx.sample_rate;
        Self {
            lines: (0..channels).map(|_| DelayLine::new(cap)).collect(),
            channels,
            sr: ctx.sample_rate,
            coef: smooth_coef(ctx.sample_rate, 0.05),
            cur: t,
            target: t,
            fresh: true,
            feedback: 0.35,
            mix: 0.35,
            echo,
            lp_a: 1.0,
            lp: vec![0.0; channels],
            pingpong: false,
        }
    }
}

impl Filter for Delay {
    fn set_param(&mut self, index: usize, value: f32) {
        match (self.echo, index) {
            (_, 0) => {
                self.target = (value * self.sr / 1000.0).max(1.0);
                if self.fresh {
                    self.cur = self.target;
                }
            }
            (_, 1) => self.feedback = value,
            (false, 2) | (true, 4) => self.mix = value,
            (true, 2) => {
                let a = (-std::f32::consts::TAU * value / self.sr).exp();
                self.lp_a = 1.0 - a;
            }
            (true, 3) => self.pingpong = value >= 0.5,
            _ => {}
        }
    }

    fn process(&mut self, buf: &mut [f32]) {
        self.fresh = false;
        let ch = self.channels;
        let pp = self.echo && self.pingpong && ch == 2;
        for frame in buf.chunks_exact_mut(ch) {
            self.cur += (self.target - self.cur) * self.coef;
            if (self.target - self.cur).abs() < 1e-3 {
                self.cur = self.target;
            }
            let d = self.cur;
            if pp {
                let wl = self.lines[0].read(d);
                let wr = self.lines[1].read(d);
                let (il, ir) = (frame[0], frame[1]);
                // tone filter on the feedback path, cross-fed between channels
                self.lp[0] += self.lp_a * (wr - self.lp[0]);
                self.lp[1] += self.lp_a * (wl - self.lp[1]);
                self.lines[0].push(0.5 * (il + ir) + self.feedback * self.lp[0]);
                self.lines[1].push(self.feedback * self.lp[1]);
                frame[0] = il * (1.0 - self.mix) + wl * self.mix;
                frame[1] = ir * (1.0 - self.mix) + wr * self.mix;
            } else {
                for (c, s) in frame.iter_mut().enumerate() {
                    let wet = self.lines[c].read(d);
                    let fb = if self.echo {
                        self.lp[c] += self.lp_a * (wet - self.lp[c]);
                        self.lp[c]
                    } else {
                        wet
                    };
                    self.lines[c].push(*s + self.feedback * fb);
                    *s = *s * (1.0 - self.mix) + wet * self.mix;
                }
            }
        }
    }

    fn reset(&mut self) {
        self.lines.iter_mut().for_each(DelayLine::clear);
        self.lp.fill(0.0);
    }
}

pub fn delay_descriptor() -> FilterDescriptor {
    FilterDescriptor::native(
        "delay",
        "Delay",
        "Time",
        vec![
            ParamSpec::new("time", "Time", 1.0, MAX_MS, 350.0).unit("ms").log(),
            ParamSpec::new("feedback", "Feedback", 0.0, 0.95, 0.35),
            ParamSpec::new("mix", "Mix", 0.0, 1.0, 0.35),
        ],
        |ctx| Box::new(Delay::new(ctx, false)),
    )
    .description("Clean digital delay with feedback.")
}

pub fn echo_descriptor() -> FilterDescriptor {
    FilterDescriptor::native(
        "echo",
        "Echo",
        "Time",
        vec![
            ParamSpec::new("time", "Time", 20.0, MAX_MS, 300.0).unit("ms").log(),
            ParamSpec::new("feedback", "Feedback", 0.0, 0.95, 0.5),
            ParamSpec::new("tone", "Tone", 500.0, 12000.0, 3000.0).unit("Hz").log(),
            ParamSpec::new("pingpong", "Ping-pong", 0.0, 1.0, 1.0).choices(&["off", "on"]),
            ParamSpec::new("mix", "Mix", 0.0, 1.0, 0.4),
        ],
        |ctx| Box::new(Delay::new(ctx, true)),
    )
    .description("Tape-style echo with darkening repeats and optional ping-pong.")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(channels: usize) -> FilterContext {
        FilterContext { sample_rate: 48000.0, channels, realtime: false }
    }

    fn noise(n: usize) -> Vec<f32> {
        let mut s = 987u32;
        (0..n)
            .map(|_| {
                s = s.wrapping_mul(1664525).wrapping_add(1013904223);
                (s >> 8) as f32 / (1u32 << 23) as f32 - 1.0
            })
            .collect()
    }

    #[test]
    fn impulse_appears_at_right_sample() {
        let d = delay_descriptor();
        let mut f = d.instantiate(&ctx(1), &[10.0, 0.0, 1.0]).unwrap();
        let mut b = vec![0.0; 2000];
        b[0] = 1.0;
        f.process(&mut b);
        assert_eq!(b[480], 1.0);
        assert_eq!(b.iter().filter(|x| x.abs() > 1e-6).count(), 1);
    }

    #[test]
    fn feedback_repeats() {
        let d = delay_descriptor();
        let mut f = d.instantiate(&ctx(1), &[10.0, 0.5, 1.0]).unwrap();
        let mut b = vec![0.0; 2000];
        b[0] = 1.0;
        f.process(&mut b);
        assert!((b[960] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn bounded_with_max_feedback() {
        for (d, ch) in [(delay_descriptor(), 2), (echo_descriptor(), 2), (echo_descriptor(), 1)] {
            let mut vals = d.defaults();
            vals[1] = 0.95;
            let mut f = d.instantiate(&ctx(ch), &vals).unwrap();
            let mut b = noise(48000 * ch * 6);
            for c in b.chunks_mut(1024 * ch) {
                f.process(c);
            }
            assert!(b.iter().all(|x| x.is_finite() && x.abs() < 40.0), "{}", d.id);
        }
    }

    #[test]
    fn pingpong_alternates_channels() {
        let d = echo_descriptor();
        // time 20ms, feedback .5, tone max, pingpong on, mix 1
        let mut f = d.instantiate(&ctx(2), &[20.0, 0.5, 12000.0, 1.0, 1.0]).unwrap();
        let mut b = vec![0.0; 2 * 4000];
        b[0] = 1.0;
        b[1] = 1.0;
        f.process(&mut b);
        let e = |lo: usize, hi: usize, c: usize| (lo..hi).map(|i| b[i * 2 + c].abs()).sum::<f32>();
        // first repeat (frame 960) left only, second (1920) right
        assert!(e(950, 970, 0) > 0.5 && e(950, 970, 1) < 1e-6);
        assert!(e(1900, 1980, 1) > 0.05);
    }

    #[test]
    fn time_change_is_smooth() {
        let d = delay_descriptor();
        let mut f = d.instantiate(&ctx(1), &[100.0, 0.0, 1.0]).unwrap();
        let mut b: Vec<f32> = (0..48000).map(|i| (i as f32 * 0.05).sin()).collect();
        f.process(&mut b[..24000]);
        f.set_param(0, 200.0);
        f.process(&mut b[24000..]);
        let max_step = b.windows(2).skip(5000).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max);
        assert!(max_step < 0.5, "{max_step}");
    }
}
