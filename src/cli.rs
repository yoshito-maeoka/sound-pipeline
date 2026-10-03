//! Command line entry: `sp <file>` opens the TUI; subcommands expose the
//! same filters as plain Unix pipe tools. (TODO: implement)
//!
//!   sp <file> [--device NAME]              open the TUI
//!   sp list                                list filters and their params
//!   sp decode <file> [--rate R] [--channels C]   file -> raw f32le on stdout
//!   sp filter <id> [key=value ...] [--rate R] [--channels C]   stdin -> stdout
//!   sp play [--rate R] [--channels C] [--device NAME]          stdin -> speaker
//!
//! Raw stream format between commands: interleaved f32 little-endian,
//! default 44100 Hz, 2 channels.

use std::path::Path;

use anyhow::Result;

use crate::engine::ChainNode;
use crate::plugin::Registry;

pub const DEFAULT_RATE: u32 = 44100;
pub const DEFAULT_CHANNELS: usize = 2;

/// Parse `std::env::args()` and run. Loads exec plugins into `registry` first.
pub fn run(mut registry: Registry) -> Result<()> {
    registry.load_exec_plugins();
    let path = std::env::args().nth(1).ok_or_else(|| anyhow::anyhow!("usage: sp <file>"))?;
    crate::tui::run(registry, path.into(), None)
}

/// Shell pipeline equivalent to the chain (bypassed nodes omitted), e.g.
/// `sp decode 'song.mp3' --rate 48000 --channels 2 | sp filter lowpass cutoff=800 q=0.71 --rate 48000 --channels 2 | sp play --rate 48000 --channels 2`
pub fn export_pipeline(file: &Path, chain: &[ChainNode], rate: u32, channels: usize) -> String {
    let _ = (chain, rate, channels);
    format!("sp decode '{}' | sp play", file.display())
}
