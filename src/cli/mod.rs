//! Clap definitions for the `kira-mux` CLI.
//!
//! Top-level commands, project target `.`, and send/wait flag parsing.

use std::convert::Infallible;
use std::str::FromStr;

use clap::{Parser, Subcommand};

pub(crate) mod workspace;

pub(crate) use workspace::{AgentsArgs, AgentsCommand};

impl Cli {
    /// Whether this invocation prints machine-readable JSON on stdout, so
    /// logging can keep stderr quiet for `2>&1` pipelines. Exhaustive on
    /// purpose: a new command variant must state its answer here or it
    /// does not compile. (A `--json` flag added to a variant already in
    /// the `false` arm still needs its arm moved — the guard covers new
    /// commands, not new flags.)
    pub(crate) fn wants_json(&self) -> bool {
        match &self.command {
            CommandKind::List { json, .. }
            | CommandKind::Status { json, .. }
            | CommandKind::Capture { json, .. } => *json,
            CommandKind::Agents(args) => match &args.command {
                AgentsCommand::List { json, .. }
                | AgentsCommand::Capabilities { json, .. }
                | AgentsCommand::Group { json, .. } => *json,
            },
            CommandKind::Open { .. }
            | CommandKind::Start { .. }
            | CommandKind::Attach { .. }
            | CommandKind::Restart { .. }
            | CommandKind::Kill { .. }
            | CommandKind::Init { .. }
            | CommandKind::Examples
            | CommandKind::Send { .. } => false,
        }
    }
}

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

/// Parse `--wait --lines`: at least one history line (zero empties captures).
fn parse_wait_capture_lines(raw: &str) -> Result<usize, String> {
    let lines: usize = raw
        .parse()
        .map_err(|error: std::num::ParseIntError| error.to_string())?;
    if lines == 0 {
        return Err("must be at least 1 (zero empties every pane capture)".to_string());
    }
    Ok(lines)
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
    /// Print usage recipes (no config or tmux side effects).
    Examples,
    /// Deliver a prompt to a live agent pane.
    ///
    /// Does not wait for TUI readiness: `send` only refuses dead panes. On a
    /// cold interactive first launch, finish setup with `open` (or attach)
    /// before the first unattended send.
    ///
    /// `send --clear` delivers the literal `/clear` slash command (no
    /// template).
    Send {
        /// Project id, or `.` for the registered project containing the CWD.
        project: ProjectTarget,
        /// Target agent id within the project.
        agent_id: String,
        /// Prompt text delivered to the pane (after optional template render).
        ///
        /// Required unless `--clear` is set.
        #[arg(required_unless_present = "clear")]
        prompt: Option<String>,
        /// Alternate agent layout from `[profiles.<name>]` in the project file.
        #[arg(long)]
        profile: Option<String>,
        /// Send the prompt literally; skip the agent's `prompt_template`.
        #[arg(long)]
        no_template: bool,
        /// Deliver `/clear` literally (implies no template). Do not pass
        /// PROMPT.
        #[arg(long, conflicts_with = "prompt")]
        clear: bool,
        /// Block until the pane output settles, then print it on stdout.
        ///
        /// Waits for pane *convergence*: submission redraws are excluded, then
        /// every distinct frame resets a quiet window sized to the evidence
        /// (5 s after durable production, 10 s for weak production, 30 s when
        /// nothing changed after the submission acknowledgement). This is a
        /// proxy for completion, not a formal agent done signal — perfectly
        /// aliased activity, a mid-reply pause longer than the active window,
        /// silent work, or monotonic idle counters can fool it. An internal
        /// hard timeout (~10 min) aborts with a dedicated exit code and the
        /// last capture on stderr.
        #[arg(long)]
        wait: bool,
        /// History lines captured while waiting (only with `--wait`).
        ///
        /// Mirrors `capture --lines`. Default is 200 when omitted.
        /// Must be at least 1: zero empties every capture and stalls wait until
        /// the hard timeout.
        ///
        /// For alternate-screen TUIs (Claude Code, Grok Build) the final
        /// capture deepens automatically via a temporary zoom/resize when
        /// the request exceeds the pane height — one geometry change after
        /// convergence, never during the wait polls. See `capture --help`.
        #[arg(long, requires = "wait", value_parser = parse_wait_capture_lines)]
        lines: Option<usize>,
    },
    /// Capture recent pane output from a live agent.
    ///
    /// Alternate-screen TUIs (Claude Code, Grok Build) accumulate no tmux
    /// history: a plain capture is capped at the visible frame. When
    /// `--lines` exceeds the pane height on such a pane, kira deep-captures:
    /// it zooms the pane, temporarily grows the window (to at most 1000
    /// rows) so the TUI repaints its internal transcript, captures, then
    /// restores the window exactly as found — size, zoom, active pane, and
    /// window-size policy (attached clients see a brief resize). Concurrent
    /// deep captures of the same window are serialized by a per-window
    /// lock; the contender falls back to the visible frame immediately. If
    /// deepening cannot run or the TUI never repaints, the visible-frame
    /// capture is returned and a warning is logged. `--json` reports the
    /// depth context: `alternate_on`, `pane_height`, `deep_capture`,
    /// `deep_capture_status` (`not_applicable` / `not_needed` / `completed`
    /// / `busy` / `unavailable`), and `depth_request_clamped`.
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
    use clap::Parser;

    use super::{Cli, CommandKind, ProjectTarget};

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

    #[test]
    fn send_wait_lines_requires_wait_flag() {
        let err =
            match Cli::try_parse_from(["kira-mux", "send", "demo", "alpha", "hi", "--lines", "10"])
            {
                Ok(cli) => panic!("expected --lines without --wait to fail, got {cli:?}"),
                Err(error) => error,
            };
        let message = err.to_string();
        assert!(
            message.contains("wait") || message.contains("--lines"),
            "error should mention the wait dependency, got: {message}"
        );
    }

    #[test]
    fn send_wait_accepts_optional_lines() {
        let cli = match Cli::try_parse_from([
            "kira-mux", "send", "demo", "alpha", "hi", "--wait", "--lines", "500",
        ]) {
            Ok(cli) => cli,
            Err(error) => panic!("parse failed: {error}"),
        };
        match cli.command {
            CommandKind::Send {
                wait: true,
                lines: Some(500),
                ..
            } => {}
            other => panic!("expected send --wait --lines 500, got {other:?}"),
        }
    }

    #[test]
    fn send_wait_rejects_zero_lines() {
        let err = match Cli::try_parse_from([
            "kira-mux", "send", "demo", "alpha", "hi", "--wait", "--lines", "0",
        ]) {
            Ok(cli) => panic!("expected --lines 0 to fail, got {cli:?}"),
            Err(error) => error,
        };
        let message = err.to_string();
        assert!(
            message.contains("at least 1") || message.contains("zero"),
            "error should reject zero lines, got: {message}"
        );
    }

    #[test]
    fn parse_wait_capture_lines_rejects_zero() {
        match super::parse_wait_capture_lines("0") {
            Ok(value) => panic!("expected Err, got {value}"),
            Err(message) => assert!(
                message.contains("at least 1"),
                "unexpected message: {message}"
            ),
        }
    }

    #[test]
    fn parse_wait_capture_lines_accepts_positive() {
        assert_eq!(super::parse_wait_capture_lines("1"), Ok(1));
        assert_eq!(super::parse_wait_capture_lines("200"), Ok(200));
    }

    #[test]
    fn send_wait_defaults_lines_to_none() {
        let cli = match Cli::try_parse_from(["kira-mux", "send", "demo", "alpha", "hi", "--wait"]) {
            Ok(cli) => cli,
            Err(error) => panic!("parse failed: {error}"),
        };
        match cli.command {
            CommandKind::Send {
                wait: true,
                lines: None,
                ..
            } => {}
            other => panic!("expected send --wait with lines=None, got {other:?}"),
        }
    }

    #[test]
    fn send_clear_omits_prompt() {
        let cli = match Cli::try_parse_from(["kira-mux", "send", "--clear", "demo", "alpha"]) {
            Ok(cli) => cli,
            Err(error) => panic!("parse failed: {error}"),
        };
        match cli.command {
            CommandKind::Send {
                clear: true,
                prompt: None,
                no_template: false,
                ..
            } => {}
            other => panic!("expected send --clear without prompt, got {other:?}"),
        }
    }

    #[test]
    fn send_clear_rejects_prompt_argument() {
        let err =
            match Cli::try_parse_from(["kira-mux", "send", "--clear", "demo", "alpha", "extra"]) {
                Ok(cli) => panic!("expected --clear with PROMPT to fail, got {cli:?}"),
                Err(error) => error,
            };
        let message = err.to_string();
        assert!(
            message.contains("cannot be used with")
                || message.contains("conflict")
                || message.contains("--clear")
                || message.contains("prompt"),
            "error should report clear/prompt conflict, got: {message}"
        );
    }

    #[test]
    fn send_without_clear_requires_prompt() {
        let err = match Cli::try_parse_from(["kira-mux", "send", "demo", "alpha"]) {
            Ok(cli) => panic!("expected missing PROMPT to fail, got {cli:?}"),
            Err(error) => error,
        };
        let message = err.to_string();
        assert!(
            message.contains("required")
                || message.contains("PROMPT")
                || message.contains("prompt"),
            "error should require PROMPT, got: {message}"
        );
    }

    #[test]
    fn examples_parses_as_unit_command() {
        let cli = match Cli::try_parse_from(["kira-mux", "examples"]) {
            Ok(cli) => cli,
            Err(error) => panic!("parse failed: {error}"),
        };
        match cli.command {
            CommandKind::Examples => {}
            other => panic!("expected Examples, got {other:?}"),
        }
    }
}
