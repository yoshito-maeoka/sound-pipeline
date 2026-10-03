//! Low-pass / high-pass filters. (TODO: implement)

use crate::plugin::FilterDescriptor;

pub fn lowpass_descriptor() -> FilterDescriptor {
    super::stub("lowpass", "Low-pass", "Tone")
}

pub fn highpass_descriptor() -> FilterDescriptor {
    super::stub("highpass", "High-pass", "Tone")
}
