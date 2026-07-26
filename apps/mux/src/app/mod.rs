//! Command handlers for the `kira-mux` CLI.

mod agent_cmds;
mod workspace_cmds;

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

use crate::cli::{Cli, CommandKind, ProjectTarget};
use crate::config::{ResolutionMode, load_project, load_project_from_current_directory};
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

/// Static copy-paste recipes for `kira-mux examples` (no config / tmux).
const EXAMPLES: &str = "\
Set up:     kira-mux init
            # edit ~/.config/kira-mux/projects/example.toml (set root, agents)

Open:       kira-mux open <project>
            # finish any first-run agent UI in the pane, then detach (Ctrl-b d)

Work here:  kira-mux status .
            kira-mux agents list .

Send:       kira-mux send . <agent> \"...\"
            kira-mux send . <agent> \"...\" --wait
            kira-mux send --clear . <agent>

Inspect:    kira-mux capture . <agent> --lines 80
List:       kira-mux list
Tear down:  kira-mux kill <project> --yes

Details:    kira-mux <command> --help
Also see:   examples/solo-coder/ in the repository
";

/// Dispatch a parsed CLI invocation.
pub(crate) fn run(cli: Cli) -> Result<()> {
    match cli.command {
        CommandKind::Init { force } => init(force),
        CommandKind::Examples => examples(),
        CommandKind::Open { project, profile } => {
            workspace_cmds::cmd_open(&project, profile.as_deref())
        }
        CommandKind::Start { project, profile } => {
            workspace_cmds::cmd_start(&project, profile.as_deref(), false)
        }
        CommandKind::Attach { project, profile } => {
            workspace_cmds::cmd_attach(&project, profile.as_deref())
        }
        CommandKind::List { json } => workspace_cmds::cmd_list(json),
        CommandKind::Status {
            project,
            profile,
            json,
        } => workspace_cmds::cmd_status(&project, profile.as_deref(), json),
        CommandKind::Agents(args) => agent_cmds::cmd_agents_dispatch(args.command),
        CommandKind::Restart {
            project,
            agent_id,
            profile,
        } => workspace_cmds::cmd_restart(&project, profile.as_deref(), agent_id.as_deref()),
        CommandKind::Kill {
            project,
            profile,
            yes,
        } => workspace_cmds::cmd_kill(&project, profile.as_deref(), yes),
        CommandKind::Send {
            project,
            agent_id,
            prompt,
            profile,
            no_template,
            clear,
            wait,
            lines,
        } => {
            // `--clear` is sugar for a literal `/clear` with no template wrap.
            let (prompt, no_template) = if clear {
                (agent_cmds::CLEAR_PROMPT.to_string(), true)
            } else {
                (
                    prompt.ok_or_else(|| {
                        anyhow::anyhow!("internal error: send requires PROMPT unless --clear")
                    })?,
                    no_template,
                )
            };
            agent_cmds::cmd_send(
                &project,
                profile.as_deref(),
                &agent_id,
                &prompt,
                no_template,
                wait,
                lines,
            )
        }
        CommandKind::Capture {
            project,
            agent_id,
            lines,
            json,
            profile,
        } => agent_cmds::cmd_capture(&project, profile.as_deref(), &agent_id, lines, json),
    }
}

fn examples() -> Result<()> {
    crate::output::print_pane_text(EXAMPLES)
}

/// Load a project and build a tmux client for command handlers.
pub(super) fn load_project_context(
    project_target: &ProjectTarget,
    profile: Option<&str>,
    resolution_mode: ResolutionMode,
) -> Result<(ResolvedProject, TmuxClient)> {
    let paths = AppPaths::from_env()?;
    let project = match project_target {
        ProjectTarget::Id(project_id) => {
            load_project(&paths, project_id, profile, resolution_mode)?
        }
        ProjectTarget::CurrentDirectory => {
            load_project_from_current_directory(&paths, profile, resolution_mode)?
        }
    };
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

#[cfg(test)]
mod tests {
    #[test]
    fn examples_text_covers_core_recipes() {
        let text = super::EXAMPLES;
        for needle in [
            "kira-mux init",
            "kira-mux open <project>",
            "kira-mux status .",
            "kira-mux send . <agent>",
            "kira-mux capture . <agent>",
            "kira-mux <command> --help",
        ] {
            assert!(
                text.contains(needle),
                "examples text missing recipe fragment: {needle}"
            );
        }
    }
}
