//! Compressor filter. (TODO: implement)

use crate::plugin::FilterDescriptor;

pub fn descriptor() -> FilterDescriptor {
    super::stub("compressor", "Compressor", "Dynamics")
}
