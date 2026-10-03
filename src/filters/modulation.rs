//! Chorus / flanger (modulated delay). (TODO: implement)

use crate::plugin::FilterDescriptor;

pub fn chorus_descriptor() -> FilterDescriptor {
    super::stub("chorus", "Chorus", "Modulation")
}

pub fn flanger_descriptor() -> FilterDescriptor {
    super::stub("flanger", "Flanger", "Modulation")
}
