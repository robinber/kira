use anyhow::Result;

use crate::config::{EnvResolutionMode, load_project};
use crate::error::AiMuxError;
use crate::paths::AppPaths;
use crate::tmux::TmuxClient;
use crate::{interaction, output, workspace};

pub(crate) fn cmd_start(project_id: &str, profile: Option<&str>, attach_after: bool) -> Result<()> {
    let paths = AppPaths::from_env()?;
    let project = load_project(&paths, project_id, profile, EnvResolutionMode::Runtime)?;
    let tmux = TmuxClient::from_env(project.tmux_bin.clone());
    let outcome = workspace::start(&tmux, &project, attach_after)?;
    if outcome == workspace::StartOutcome::Degraded {
        eprintln!(
            "warning: workspace started in degraded state — one or more agents failed to launch"
        );
        return Err(AiMuxError::Degraded(project_id.to_string()).into());
    }
    Ok(())
}

pub(crate) fn cmd_open(project_id: &str, profile: Option<&str>) -> Result<()> {
    cmd_start(project_id, profile, true)
}

pub(crate) fn cmd_attach(project_id: &str, profile: Option<&str>) -> Result<()> {
    let paths = AppPaths::from_env()?;
    let project = load_project(&paths, project_id, profile, EnvResolutionMode::Deferred)?;
    let tmux = TmuxClient::from_env(project.tmux_bin.clone());
    workspace::attach(&tmux, &project)
}

pub(crate) fn cmd_restart(
    project_id: &str,
    profile: Option<&str>,
    agent_id: Option<&str>,
) -> Result<()> {
    let paths = AppPaths::from_env()?;
    let project = load_project(&paths, project_id, profile, EnvResolutionMode::Runtime)?;
    let tmux = TmuxClient::from_env(project.tmux_bin.clone());
    workspace::restart(&tmux, &project, agent_id)
}

pub(crate) fn cmd_kill(project_id: &str, profile: Option<&str>, yes: bool) -> Result<()> {
    let paths = AppPaths::from_env()?;
    let project = load_project(&paths, project_id, profile, EnvResolutionMode::Deferred)?;
    let tmux = TmuxClient::from_env(project.tmux_bin.clone());
    if !crate::inspector::session_exists(&tmux, &workspace::session_name(&project))? {
        eprintln!("session for project {project_id} is already stopped");
        return Ok(());
    }

    if !yes {
        interaction::confirm_kill(project_id)?;
    }

    workspace::kill(&tmux, &project)?;
    Ok(())
}

pub(crate) fn cmd_list(json: bool) -> Result<()> {
    let summaries = workspace::load_project_summaries()?;
    output::print_list(&summaries, json)
}

pub(crate) fn cmd_status(project_id: &str, profile: Option<&str>, json: bool) -> Result<()> {
    let paths = AppPaths::from_env()?;
    let project = load_project(&paths, project_id, profile, EnvResolutionMode::Deferred)?;
    let tmux = TmuxClient::from_env(project.tmux_bin.clone());
    let status = workspace::project_status(&tmux, &project)?;
    output::print_status(&status, json)
}
