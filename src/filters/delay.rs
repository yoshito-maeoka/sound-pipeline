//! Delay / echo. (TODO: implement)

use crate::plugin::FilterDescriptor;

pub fn delay_descriptor() -> FilterDescriptor {
    super::stub("delay", "Delay", "Time")
}

pub fn echo_descriptor() -> FilterDescriptor {
    super::stub("echo", "Echo", "Time")
}
