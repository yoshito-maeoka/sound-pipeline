//! A custom Rust filter plugin: a tremolo, registered next to the built-ins.
//!
//!     cargo run --example custom_filter -- song.mp3
//!     cargo run --example custom_filter -- list

use soundpipe::{Filter, FilterContext, FilterDescriptor, ParamSpec, Registry};

struct Tremolo {
    sample_rate: f32,
    channels: usize,
    phase: f32,
    rate: f32,
    depth: f32,
    square: bool,
}

impl Filter for Tremolo {
    fn set_param(&mut self, index: usize, value: f32) {
        match index {
            0 => self.rate = value,
            1 => self.depth = value,
            2 => self.square = value >= 0.5,
            _ => {}
        }
    }

    fn process(&mut self, buf: &mut [f32]) {
        let inc = self.rate / self.sample_rate;
        for frame in buf.chunks_mut(self.channels) {
            let lfo = if self.square {
                if self.phase < 0.5 { 1.0 } else { -1.0 }
            } else {
                (self.phase * std::f32::consts::TAU).sin()
            };
            // gain swings between 1 - depth and 1
            let gain = 1.0 - self.depth * 0.5 * (1.0 - lfo);
            for s in frame {
                *s *= gain;
            }
            self.phase = (self.phase + inc).fract();
        }
    }

    fn reset(&mut self) {
        self.phase = 0.0;
    }
}

fn tremolo() -> FilterDescriptor {
    FilterDescriptor::native(
        "tremolo",
        "Tremolo",
        "Modulation",
        vec![
            ParamSpec::new("rate", "Rate", 0.1, 20.0, 5.0).unit("Hz").log(),
            ParamSpec::new("depth", "Depth", 0.0, 1.0, 0.6),
            ParamSpec::new("shape", "Shape", 0.0, 1.0, 0.0).choices(&["sine", "square"]),
        ],
        |ctx: &FilterContext| {
            Box::new(Tremolo {
                sample_rate: ctx.sample_rate,
                channels: ctx.channels.max(1),
                phase: 0.0,
                rate: 5.0,
                depth: 0.6,
                square: false,
            })
        },
    )
    .description("Amplitude modulation with a sine or square LFO")
}

fn main() {
    let mut reg = Registry::with_builtins();
    reg.register(tremolo());
    if let Err(e) = soundpipe::run(reg) {
        eprintln!("sp: {e:#}");
        std::process::exit(1);
    }
}
