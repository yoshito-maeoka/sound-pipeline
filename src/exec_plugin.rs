//! External process plugins — the Unix-pipe way to write a custom filter in
//! any language. (TODO: implement)
//!
//! A plugin is a `*.toml` manifest in a plugin directory, describing a
//! command that reads interleaved f32le samples on stdin and writes the same
//! number of processed samples on stdout.

use std::path::PathBuf;
use std::process::Command;

use crate::plugin::{FilterContext, ParamSpec, Registry};

/// Parsed manifest, shared by every instance of the plugin.
#[derive(Clone, Debug)]
pub struct ExecSpec {
    pub id: String,
    /// Directory of the manifest; the command runs with this as cwd.
    pub dir: PathBuf,
    pub command: Vec<String>,
}

impl ExecSpec {
    /// Build the process command with SP_* env vars set for `ctx` and the
    /// given param values. Used by the in-app filter and by `sp filter`,
    /// which execs it directly with inherited stdio.
    pub fn build_command(&self, ctx: &FilterContext, params: &[ParamSpec], values: &[f32]) -> Command {
        let _ = (ctx, params, values);
        let mut cmd = Command::new(&self.command[0]);
        cmd.args(&self.command[1..]).current_dir(&self.dir);
        cmd
    }
}

/// Plugin search path, in priority order.
pub fn plugin_dirs() -> Vec<PathBuf> {
    Vec::new()
}

/// Scan `plugin_dirs()` and register each manifest; problems go to `reg.warnings`.
pub fn load_into(reg: &mut Registry) {
    let _ = reg;
}
