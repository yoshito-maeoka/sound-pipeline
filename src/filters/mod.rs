//! Built-in filters. Each module exposes `*descriptor()` constructors.

pub mod biquad;
pub mod compressor;
pub mod delay;
mod delayline;
pub mod distortion;
pub mod eq;
pub mod gain;
pub mod modulation;
pub mod pass;
pub mod reverb;

use crate::plugin::{FilterDescriptor, Passthrough, Registry};

/// Picker order: Tone, Dynamics, Drive, Modulation, Time.
pub fn register_builtins(reg: &mut Registry) {
    reg.register(eq::descriptor());
    reg.register(pass::lowpass_descriptor());
    reg.register(pass::highpass_descriptor());
    reg.register(gain::descriptor());
    reg.register(compressor::descriptor());
    reg.register(distortion::descriptor());
    reg.register(modulation::chorus_descriptor());
    reg.register(modulation::flanger_descriptor());
    reg.register(delay::delay_descriptor());
    reg.register(delay::echo_descriptor());
    reg.register(reverb::descriptor());
}

/// Placeholder used until a filter is implemented.
#[allow(dead_code)]
pub(crate) fn stub(id: &str, name: &str, category: &str) -> FilterDescriptor {
    FilterDescriptor::native(id, name, category, Vec::new(), |_| Box::new(Passthrough))
        .description("(not implemented yet)")
}
