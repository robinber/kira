use anyhow::Result;

use super::load_project_context;
use crate::config::ResolutionMode;
use crate::error::KiraMuxError;
use crate::{interaction, output, workspace};

pub(crate) fn cmd_start(project_id: &str, profile: Option<&str>, attach_after: bool) -> Result<()> {
    let (project, tmux) = load_project_context(project_id, profile, ResolutionMode::Runtime)?;
    let outcome = workspace::start(&tmux, &project, attach_after)?;
    if outcome == workspace::StartOutcome::Degraded {
        eprintln!(
            "warning: workspace started in degraded state — one or more agents failed to launch"
        );
        return Err(KiraMuxError::Degraded(project_id.to_string()).into());
    }
    Ok(())
}

pub(crate) fn cmd_open(project_id: &str, profile: Option<&str>) -> Result<()> {
    cmd_start(project_id, profile, true)
}

pub(crate) fn cmd_attach(project_id: &str, profile: Option<&str>) -> Result<()> {
    let (project, tmux) = load_project_context(project_id, profile, ResolutionMode::Deferred)?;
    workspace::attach(&tmux, &project)
}

pub(crate) fn cmd_restart(
    project_id: &str,
    profile: Option<&str>,
    agent_id: Option<&str>,
) -> Result<()> {
    let (project, tmux) = load_project_context(project_id, profile, ResolutionMode::Runtime)?;
    workspace::restart(&tmux, &project, agent_id)
}

pub(crate) fn cmd_kill(project_id: &str, profile: Option<&str>, yes: bool) -> Result<()> {
    let (project, tmux) = load_project_context(project_id, profile, ResolutionMode::Deferred)?;
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
    let (project, tmux) = load_project_context(project_id, profile, ResolutionMode::Deferred)?;
    let status = workspace::project_status(&tmux, &project)?;
    output::print_status(&status, json)
}
