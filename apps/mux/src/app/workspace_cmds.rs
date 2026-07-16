use anyhow::Result;

use super::load_project_context;
use crate::cli::ProjectTarget;
use crate::config::ResolutionMode;
use crate::error::KiraMuxError;
use crate::{interaction, output, workspace};

pub(crate) fn cmd_start(
    project_target: &ProjectTarget,
    profile: Option<&str>,
    attach_after: bool,
) -> Result<()> {
    let (project, tmux) = load_project_context(project_target, profile, ResolutionMode::Runtime)?;
    let outcome = workspace::start(&tmux, &project, attach_after)?;
    if outcome == workspace::StartOutcome::Degraded {
        eprintln!(
            "warning: workspace started in degraded state — one or more agents failed to launch"
        );
        return Err(KiraMuxError::Degraded(project.id).into());
    }
    Ok(())
}

pub(crate) fn cmd_open(project_target: &ProjectTarget, profile: Option<&str>) -> Result<()> {
    cmd_start(project_target, profile, true)
}

pub(crate) fn cmd_attach(project_target: &ProjectTarget, profile: Option<&str>) -> Result<()> {
    let (project, tmux) = load_project_context(project_target, profile, ResolutionMode::Deferred)?;
    workspace::attach(&tmux, &project)
}

pub(crate) fn cmd_restart(
    project_target: &ProjectTarget,
    profile: Option<&str>,
    agent_id: Option<&str>,
) -> Result<()> {
    let (project, tmux) = load_project_context(project_target, profile, ResolutionMode::Runtime)?;
    workspace::restart(&tmux, &project, agent_id)
}

pub(crate) fn cmd_kill(
    project_target: &ProjectTarget,
    profile: Option<&str>,
    yes: bool,
) -> Result<()> {
    let (project, tmux) = load_project_context(project_target, profile, ResolutionMode::Deferred)?;
    if !crate::inspector::session_exists(&tmux, &workspace::session_name(&project))? {
        eprintln!("session for project {} is already stopped", project.id);
        return Ok(());
    }

    if !yes {
        interaction::confirm_kill(&project.id)?;
    }

    workspace::kill(&tmux, &project)?;
    Ok(())
}

pub(crate) fn cmd_list(json: bool) -> Result<()> {
    let summaries = workspace::load_project_summaries()?;
    output::print_list(&summaries, json)?;

    let failure_count = summaries
        .iter()
        .filter(|row| row.state == crate::model::ProjectState::ConfigError)
        .count();
    if failure_count > 0 {
        // Entries already carry per-file diagnostics on stdout (text + JSON).
        // Exit non-zero so automation does not treat a broken config as
        // "project simply absent".
        return Err(crate::config::ConfigError::ProjectConfigLoadFailures {
            count: failure_count,
        }
        .into());
    }
    Ok(())
}

pub(crate) fn cmd_status(
    project_target: &ProjectTarget,
    profile: Option<&str>,
    json: bool,
) -> Result<()> {
    let (project, tmux) = load_project_context(project_target, profile, ResolutionMode::Deferred)?;
    let status = workspace::project_status(&tmux, &project)?;
    output::print_status(&status, json)
}
