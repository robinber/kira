//! Clap definitions for the `kira-mux` CLI.

use clap::{Parser, Subcommand};

pub(crate) mod workspace;

pub(crate) use workspace::{AgentsArgs, AgentsCommand};

/// Top-level CLI parser.
#[derive(Debug, Parser)]
#[command(
    name = "kira-mux",
    version,
    about = "tmux multi-agent workspaces",
    arg_required_else_help = true
)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: CommandKind,
}

/// CLI command surface.
#[derive(Debug, Subcommand)]
pub(crate) enum CommandKind {
    /// Create or repair the workspace and attach.
    Open {
        project_id: String,
        #[arg(long)]
        profile: Option<String>,
    },
    /// Create or repair the workspace without attaching.
    Start {
        project_id: String,
        #[arg(long)]
        profile: Option<String>,
    },
    /// Attach to an existing workspace session.
    Attach {
        project_id: String,
        #[arg(long)]
        profile: Option<String>,
    },
    /// List configured projects.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Show live workspace status.
    Status {
        project_id: String,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Inspect configured agents.
    Agents(AgentsArgs),
    /// Restart one agent pane, or all panes when no agent id is given.
    Restart {
        project_id: String,
        agent_id: Option<String>,
        #[arg(long)]
        profile: Option<String>,
    },
    /// Kill the managed tmux session.
    Kill {
        project_id: String,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        yes: bool,
    },
    /// Write default XDG config files.
    Init {
        #[arg(long)]
        force: bool,
    },
    /// Deliver a prompt to a live agent pane (does not wait for TUI readiness).
    Send {
        project_id: String,
        agent_id: String,
        prompt: String,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        no_template: bool,
        /// Block until the pane output settles, then print it on stdout.
        ///
        /// Waits for pane *stability*: the pane must first change after the
        /// prompt is submitted (response activity), then stay unchanged for a
        /// few seconds. This is a proxy for completion, not a formal agent
        /// done signal — panes that keep redrawing (clocks, watchers) or go
        /// quiet mid-stream can fool it. An internal hard timeout (~10 min)
        /// aborts with a dedicated exit code and the last capture on stderr.
        #[arg(long)]
        wait: bool,
    },
    /// Capture recent pane output.
    Capture {
        project_id: String,
        agent_id: String,
        #[arg(long, default_value_t = 30)]
        lines: usize,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        profile: Option<String>,
    },
}
