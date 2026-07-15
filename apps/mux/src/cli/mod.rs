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

/// Product-A command surface.
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
    /// Deliver a prompt to an agent pane.
    Send {
        project_id: String,
        agent_id: String,
        prompt: String,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        no_template: bool,
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
