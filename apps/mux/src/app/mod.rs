//! Command handlers for the product-A CLI.

mod agent_cmds;
mod workspace_cmds;

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::cli::{Cli, CommandKind};
use crate::paths::AppPaths;

const DEFAULT_CONFIG: &str = r#"session_prefix = "ai"
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
        Some(CommandKind::Init { force }) => init(force),
        Some(CommandKind::Open {
            project_id,
            profile,
        }) => workspace_cmds::cmd_open(&project_id, profile.as_deref()),
        Some(CommandKind::Start {
            project_id,
            profile,
        }) => workspace_cmds::cmd_start(&project_id, profile.as_deref(), false),
        Some(CommandKind::Attach {
            project_id,
            profile,
        }) => workspace_cmds::cmd_attach(&project_id, profile.as_deref()),
        Some(CommandKind::List { json }) => workspace_cmds::cmd_list(json),
        Some(CommandKind::Status {
            project_id,
            profile,
            json,
        }) => workspace_cmds::cmd_status(&project_id, profile.as_deref(), json),
        Some(CommandKind::Agents(args)) => {
            agent_cmds::cmd_agents_dispatch(agent_cmds::resolve_agents_args(args)?)
        }
        Some(CommandKind::Restart {
            project_id,
            agent_id,
            profile,
        }) => workspace_cmds::cmd_restart(&project_id, profile.as_deref(), agent_id.as_deref()),
        Some(CommandKind::Kill {
            project_id,
            profile,
            yes,
        }) => workspace_cmds::cmd_kill(&project_id, profile.as_deref(), yes),
        Some(CommandKind::Send {
            project_id,
            agent_id,
            prompt,
            profile,
            no_template,
        }) => agent_cmds::cmd_send(
            &project_id,
            profile.as_deref(),
            &agent_id,
            &prompt,
            no_template,
        ),
        Some(CommandKind::Capture {
            project_id,
            agent_id,
            lines,
            json,
            profile,
        }) => agent_cmds::cmd_capture(&project_id, profile.as_deref(), &agent_id, lines, json),
        None => {
            bail!("no command provided; try `kira-mux --help`");
        }
    }
}

fn init(force: bool) -> Result<()> {
    let paths = AppPaths::from_env()?;

    fs::create_dir_all(paths.config_dir()).context("failed to create config directory")?;
    fs::create_dir_all(paths.projects_dir()).context("failed to create projects directory")?;

    write_file(&paths.global_config_path(), DEFAULT_CONFIG, force)?;
    write_file(&paths.example_project_path(), DEFAULT_PROJECT, force)?;

    eprintln!("initialized config at {}", paths.config_dir().display());
    Ok(())
}

fn write_file(path: &Path, contents: &str, force: bool) -> Result<()> {
    if path.exists() && !force {
        bail!("refusing to overwrite existing file: {}", path.display());
    }

    fs::write(path, contents).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}
