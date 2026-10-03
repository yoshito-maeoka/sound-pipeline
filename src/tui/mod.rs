//! Terminal UI. (TODO: implement)

use std::path::PathBuf;

use anyhow::Result;

use crate::plugin::Registry;

/// Decode `path`, start the engine on `device`, and run the UI until quit.
pub fn run(registry: Registry, path: PathBuf, device: Option<String>) -> Result<()> {
    let _ = (registry, device);
    anyhow::bail!("TUI not implemented ({})", path.display())
}
