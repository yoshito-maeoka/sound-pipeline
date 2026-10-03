//! Distortion filter. (TODO: implement)

use crate::plugin::FilterDescriptor;

pub fn descriptor() -> FilterDescriptor {
    super::stub("distortion", "Distortion", "Drive")
}
