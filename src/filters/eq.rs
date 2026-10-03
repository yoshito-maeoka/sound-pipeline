//! 8-band graphic equalizer built from RBJ biquads.

use super::biquad::{Biquad, BiquadKind};
use crate::plugin::{EQ_BANDS, EditorKind, Filter, FilterDescriptor, ParamSpec};

const KEYS: [&str; 8] = ["b60", "b150", "b400", "b1k", "b2k5", "b6k", "b12k", "b16k"];
const BAND_Q: f32 = 1.0;
const N: usize = EQ_BANDS.len();

struct Eq {
    /// None when the band lies above 0.45 * sample rate.
    bands: Vec<Option<Biquad>>,
    gains: [f32; N],
    out_lin: f32,
}

impl Filter for Eq {
    fn set_param(&mut self, index: usize, value: f32) {
        if index < N {
            self.gains[index] = value;
            if let Some(b) = &mut self.bands[index] {
                b.set_gain_db(value);
            }
        } else if index == N {
            self.out_lin = 10f32.powf(value / 20.0);
        }
    }

    fn process(&mut self, buf: &mut [f32]) {
        for (b, g) in self.bands.iter_mut().zip(self.gains.iter()) {
            if let Some(b) = b
                && *g != 0.0
            {
                b.process(buf);
            }
        }
        if self.out_lin != 1.0 {
            for s in buf.iter_mut() {
                *s *= self.out_lin;
            }
        }
    }

    fn reset(&mut self) {
        for b in self.bands.iter_mut().flatten() {
            b.reset();
        }
    }
}

pub fn descriptor() -> FilterDescriptor {
    let mut params: Vec<ParamSpec> = KEYS
        .iter()
        .zip(EQ_BANDS.iter())
        .map(|(k, f)| {
            let label = if *f >= 1000.0 { format!("{} kHz", f / 1000.0) } else { format!("{f} Hz") };
            ParamSpec::new(k, &label, -12.0, 12.0, 0.0).unit("dB")
        })
        .collect();
    params.push(ParamSpec::new("output", "Output", -12.0, 12.0, 0.0).unit("dB"));

    FilterDescriptor::native("eq", "Equalizer", "Tone", params, |ctx| {
        let bands = EQ_BANDS
            .iter()
            .enumerate()
            .map(|(i, &f)| {
                if f >= 0.45 * ctx.sample_rate {
                    return None;
                }
                let kind = match i {
                    0 => BiquadKind::LowShelf,
                    i if i == N - 1 => BiquadKind::HighShelf,
                    _ => BiquadKind::Peaking,
                };
                Some(Biquad::new(kind, ctx.sample_rate, ctx.channels, f, BAND_Q, 0.0))
            })
            .collect();
        Box::new(Eq { bands, gains: [0.0; N], out_lin: 1.0 }) as Box<dyn Filter>
    })
    .editor(EditorKind::Equalizer)
    .description("8-band graphic equalizer with output gain.")
}

#[cfg(test)]
mod tests {
    use super::super::biquad::testutil::*;
    use super::*;
    use crate::plugin::FilterContext;

    fn ctx(sr: f32) -> FilterContext {
        FilterContext { sample_rate: sr, channels: 2, realtime: false }
    }

    fn gain_db(vals: &[f32], tone: f32, sr: f32) -> f32 {
        let mut f = descriptor().instantiate(&ctx(sr), vals).unwrap();
        let input = sine(tone, sr, 2, 24000, 0.25);
        let mut buf = input.clone();
        f.process(&mut buf);
        assert!(buf.iter().all(|x| x.is_finite()));
        20.0 * (rms_tail(&buf) / rms_tail(&input)).log10()
    }

    #[test]
    fn layout() {
        let d = descriptor();
        assert_eq!(d.params.len(), 9);
        assert_eq!(d.params[3].key, "b1k");
        assert_eq!(d.params[8].key, "output");
        assert_eq!(d.editor, EditorKind::Equalizer);
    }

    #[test]
    fn flat_is_transparent() {
        let d = descriptor();
        assert!(gain_db(&d.defaults(), 1000.0, 48000.0).abs() < 0.01);
    }

    #[test]
    fn boost_1k() {
        let mut v = descriptor().defaults();
        v[3] = 12.0;
        let g = gain_db(&v, 1000.0, 48000.0);
        assert!((g - 12.0).abs() < 0.5, "{g}");
        assert!(gain_db(&v, 100.0, 48000.0) < 2.0);
    }

    #[test]
    fn output_gain_and_shelves() {
        let mut v = descriptor().defaults();
        v[8] = -6.0;
        assert!((gain_db(&v, 1000.0, 48000.0) + 6.0).abs() < 0.1);
        let mut v = descriptor().defaults();
        v[0] = 12.0;
        assert!(gain_db(&v, 20.0, 48000.0) > 10.0);
        let mut v = descriptor().defaults();
        v[7] = -12.0;
        assert!(gain_db(&v, 20000.0, 48000.0) < -9.0);
    }

    #[test]
    fn low_sample_rate_skips_high_bands() {
        let v = vec![12.0; 9];
        let g = gain_db(&v, 500.0, 16000.0);
        assert!(g.is_finite());
    }
}
