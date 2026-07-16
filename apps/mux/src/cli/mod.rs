//! Clap definitions for the `kira-mux` CLI.

use std::convert::Infallible;
use std::str::FromStr;

use clap::{Parser, Subcommand};

pub(crate) mod workspace;

pub(crate) use workspace::{AgentsArgs, AgentsCommand};

/// Top-level CLI parser.
#[derive(Debug, Parser)]
#[command(
    name = "kira-mux",
    version,
    about = "Local tmux multi-agent workspaces",
    long_about = "Define coding agents in TOML, open a managed tmux session, send \
prompts, capture pane output, and take over any pane with normal tmux muscle memory.\n\n\
No daemon, cloud, or database — just your machine, tmux, and the agents you already run.",
    arg_required_else_help = true
)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: CommandKind,
}

/// Project selector accepted by commands that operate on one workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProjectTarget {
    /// Stable project ID from the XDG registry.
    Id(String),
    /// Registered project whose root contains the process current directory.
    CurrentDirectory,
}

impl FromStr for ProjectTarget {
    type Err = Infallible;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value == "." {
            Ok(Self::CurrentDirectory)
        } else {
            Ok(Self::Id(value.to_string()))
        }
    }
}

/// CLI command surface.
#[derive(Debug, Subcommand)]
pub(crate) enum CommandKind {
    /// Create or repair the workspace and attach.
    ///
    /// Prefer `open` for interactive agents on a cold start so you can finish
    /// first-run UI (trust directory, login, …) before unattended `send`.
    Open {
        /// Project id, or `.` for the registered project containing the CWD.
        project: ProjectTarget,
        /// Alternate agent layout from `[profiles.<name>]` in the project file.
        #[arg(long)]
        profile: Option<String>,
    },
    /// Create or repair the workspace without attaching.
    ///
    /// Fine once agents are already bootstrapped. On a cold interactive first
    /// launch, use `open` (or `start` then `attach`) before the first `send`.
    Start {
        /// Project id, or `.` for the registered project containing the CWD.
        project: ProjectTarget,
        /// Alternate agent layout from `[profiles.<name>]` in the project file.
        #[arg(long)]
        profile: Option<String>,
    },
    /// Attach to an existing workspace session.
    Attach {
        /// Project id, or `.` for the registered project containing the CWD.
        project: ProjectTarget,
        /// Alternate agent layout from `[profiles.<name>]` in the project file.
        #[arg(long)]
        profile: Option<String>,
    },
    /// List configured projects and live session state.
    ///
    /// Invalid project files appear as `state = "config_error"` (exit code 2
    /// when any such row is present).
    List {
        /// Emit machine-readable JSON on stdout.
        #[arg(long)]
        json: bool,
    },
    /// Show live workspace and agent state.
    ///
    /// `running` means the pane process is alive, not that the agent TUI is
    /// past setup and ready for tasks.
    Status {
        /// Project id, or `.` for the registered project containing the CWD.
        project: ProjectTarget,
        /// Alternate agent layout from `[profiles.<name>]` in the project file.
        #[arg(long)]
        profile: Option<String>,
        /// Emit machine-readable JSON on stdout.
        #[arg(long)]
        json: bool,
    },
    /// Inspect configured agents (list, capabilities, groups).
    Agents(AgentsArgs),
    /// Restart one agent pane, or all panes when no agent id is given.
    ///
    /// Use after changing host env referenced by `$VAR` agent env entries so
    /// panes re-resolve and re-apply injections.
    Restart {
        /// Project id, or `.` for the registered project containing the CWD.
        project: ProjectTarget,
        /// Agent id to restart; omit to restart every pane in the workspace.
        agent_id: Option<String>,
        /// Alternate agent layout from `[profiles.<name>]` in the project file.
        #[arg(long)]
        profile: Option<String>,
    },
    /// Kill the managed tmux session.
    Kill {
        /// Project id, or `.` for the registered project containing the CWD.
        project: ProjectTarget,
        /// Alternate agent layout from `[profiles.<name>]` in the project file.
        #[arg(long)]
        profile: Option<String>,
        /// Skip the interactive confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
    /// Write default XDG config under `~/.config/kira-mux/`.
    Init {
        /// Overwrite existing default files if they are already present.
        #[arg(long)]
        force: bool,
    },
    /// Deliver a prompt to a live agent pane.
    ///
    /// Does not wait for TUI readiness: `send` only refuses dead panes. On a
    /// cold interactive first launch, finish setup with `open` (or attach)
    /// before the first unattended send.
    Send {
        /// Project id, or `.` for the registered project containing the CWD.
        project: ProjectTarget,
        /// Target agent id within the project.
        agent_id: String,
        /// Prompt text delivered to the pane (after optional template render).
        prompt: String,
        /// Alternate agent layout from `[profiles.<name>]` in the project file.
        #[arg(long)]
        profile: Option<String>,
        /// Send the prompt literally; skip the agent's `prompt_template`.
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
    /// Capture recent pane output from a live agent.
    Capture {
        /// Project id, or `.` for the registered project containing the CWD.
        project: ProjectTarget,
        /// Target agent id within the project.
        agent_id: String,
        /// Number of history lines to capture.
        #[arg(long, default_value_t = 30)]
        lines: usize,
        /// Emit machine-readable JSON on stdout.
        #[arg(long)]
        json: bool,
        /// Alternate agent layout from `[profiles.<name>]` in the project file.
        #[arg(long)]
        profile: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::ProjectTarget;

    #[test]
    fn project_target_dot_selects_current_directory() {
        assert_eq!(
            ".".parse::<ProjectTarget>(),
            Ok(ProjectTarget::CurrentDirectory)
        );
    }

    #[test]
    fn project_target_preserves_explicit_id() {
        assert_eq!(
            "demo".parse::<ProjectTarget>(),
            Ok(ProjectTarget::Id("demo".to_string()))
        );
    }
}
