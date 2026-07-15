//! Command handlers for the `kira-mux` CLI.

mod agent_cmds;
mod workspace_cmds;

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

use crate::cli::{Cli, CommandKind};
use crate::config::{ResolutionMode, load_project};
use crate::model::ResolvedProject;
use crate::paths::AppPaths;
use crate::tmux::TmuxClient;

const DEFAULT_CONFIG: &str = r#"session_prefix = "kira"
default_layout = "auto"
window_name = "agents"
remain_on_exit = "failed"
"#;

const DEFAULT_PROJECT: &str = r#"id = "example"
name = "Example"
root = "/absolute/path/to/project"

[[agents]]
id = "assistant"
command = "codex"
"#;

/// Dispatch a parsed CLI invocation.
pub(crate) fn run(cli: Cli) -> Result<()> {
    match cli.command {
        CommandKind::Init { force } => init(force),
        CommandKind::Open {
            project_id,
            profile,
        } => workspace_cmds::cmd_open(&project_id, profile.as_deref()),
        CommandKind::Start {
            project_id,
            profile,
        } => workspace_cmds::cmd_start(&project_id, profile.as_deref(), false),
        CommandKind::Attach {
            project_id,
            profile,
        } => workspace_cmds::cmd_attach(&project_id, profile.as_deref()),
        CommandKind::List { json } => workspace_cmds::cmd_list(json),
        CommandKind::Status {
            project_id,
            profile,
            json,
        } => workspace_cmds::cmd_status(&project_id, profile.as_deref(), json),
        CommandKind::Agents(args) => agent_cmds::cmd_agents_dispatch(args.command),
        CommandKind::Restart {
            project_id,
            agent_id,
            profile,
        } => workspace_cmds::cmd_restart(&project_id, profile.as_deref(), agent_id.as_deref()),
        CommandKind::Kill {
            project_id,
            profile,
            yes,
        } => workspace_cmds::cmd_kill(&project_id, profile.as_deref(), yes),
        CommandKind::Send {
            project_id,
            agent_id,
            prompt,
            profile,
            no_template,
            wait,
        } => agent_cmds::cmd_send(
            &project_id,
            profile.as_deref(),
            &agent_id,
            &prompt,
            no_template,
            wait,
        ),
        CommandKind::Capture {
            project_id,
            agent_id,
            lines,
            json,
            profile,
        } => agent_cmds::cmd_capture(&project_id, profile.as_deref(), &agent_id, lines, json),
    }
}

/// Load a project and build a tmux client for command handlers.
pub(super) fn load_project_context(
    project_id: &str,
    profile: Option<&str>,
    resolution_mode: ResolutionMode,
) -> Result<(ResolvedProject, TmuxClient)> {
    let paths = AppPaths::from_env()?;
    let project = load_project(&paths, project_id, profile, resolution_mode)?;
    let tmux = TmuxClient::from_env(project.tmux_bin.clone());
    Ok((project, tmux))
}

fn init(force: bool) -> Result<()> {
    let paths = AppPaths::from_env()?;

    fs::create_dir_all(paths.config_dir()).context("failed to create config directory")?;
    fs::create_dir_all(paths.projects_dir()).context("failed to create projects directory")?;

    // Per-file skip-and-report keeps `init` idempotent: an existing config
    // never blocks the example file (or vice versa) and is never clobbered
    // without --force.
    write_file(&paths.global_config_path(), DEFAULT_CONFIG, force)?;
    write_file(&paths.example_project_path(), DEFAULT_PROJECT, force)?;

    eprintln!("initialized config at {}", paths.config_dir().display());
    Ok(())
}

fn write_file(path: &Path, contents: &str, force: bool) -> Result<()> {
    if path.exists() && !force {
        eprintln!(
            "keeping existing file (use --force to overwrite): {}",
            path.display()
        );
        return Ok(());
    }

    fs::write(path, contents).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}
