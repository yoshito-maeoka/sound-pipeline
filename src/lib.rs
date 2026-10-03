//! soundpipe — chain audio filters like Unix pipes, in a TUI.
//!
//! To add your own Rust filter, build a binary that registers it:
//!
//! ```no_run
//! let mut reg = soundpipe::Registry::with_builtins();
//! // reg.register(my_filter_descriptor());
//! soundpipe::run(reg).unwrap();
//! ```

pub mod audio;
pub mod cli;
pub mod engine;
pub mod exec_plugin;
pub mod filters;
pub mod plugin;
pub mod tui;

pub use plugin::{
    EditorKind, Filter, FilterContext, FilterDescriptor, FilterKind, ParamSpec, Registry, Scale,
};

/// Run the `sp` command line with the given registry.
pub fn run(registry: Registry) -> anyhow::Result<()> {
    cli::run(registry)
}
