//! Library backing the `kira-mux` CLI.
//!
//! Loads XDG project configuration, manages tmux workspaces, and delivers
//! prompts to agent panes.

use anyhow::Result;
use clap::Parser;

pub(crate) mod agent_io;
pub(crate) mod app;
pub(crate) mod cli;
pub mod config;
pub(crate) mod error;
pub(crate) mod inspector;
pub(crate) mod interaction;
pub mod logging;
pub mod model;
pub use model as domain;
pub(crate) mod output;
pub mod paths;
pub(crate) mod prompt;
#[cfg(test)]
pub(crate) mod test_support;
pub mod tmux;
pub(crate) mod workspace;

pub use error::{AiMuxError, WorkspaceDriftReason};

/// Parse CLI args and run the selected command.
///
/// # Errors
///
/// Returns any configuration, tmux, or validation error raised while handling
/// the selected command.
pub fn run() -> Result<()> {
    let cli = cli::Cli::parse();
    app::run(cli)
}
