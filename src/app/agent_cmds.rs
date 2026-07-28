//! Agent listing, send, and capture handlers.

use anyhow::{Context, Result};

use super::load_project_context;
use crate::cli::{AgentsCommand, ProjectTarget};
use crate::config::ResolutionMode;
use crate::error::KiraMuxError;
use crate::output;

pub(super) fn cmd_agents_dispatch(sub: AgentsCommand) -> Result<()> {
    let (project_target, profile) = match &sub {
        AgentsCommand::List {
            project, profile, ..
        }
        | AgentsCommand::Capabilities {
            project, profile, ..
        }
        | AgentsCommand::Group {
            project, profile, ..
        } => (project, profile.as_deref()),
    };
    let (project, tmux) = load_project_context(project_target, profile, ResolutionMode::Deferred)?;
    let topology = crate::inspector::inspect(&tmux, &project)?;
    let agents_output = crate::model::build_agents_output(&project, &topology);

    match sub {
        AgentsCommand::List { json, .. } => {
            output::print_agents(&agents_output, json)?;
        }
        AgentsCommand::Capabilities { agent_id, json, .. } => {
            let agent = agents_output
                .agents
                .iter()
                .find(|a| a.id == agent_id)
                .ok_or_else(|| KiraMuxError::UnknownAgentId(agent_id.clone()))?;
            output::print_agent_capabilities(agent, json)?;
        }
        AgentsCommand::Group {
            group_name, json, ..
        } => {
            let members = agents_output
                .groups
                .get(&group_name)
                .ok_or_else(|| KiraMuxError::UnknownGroupName(group_name.clone()))?;
            let group_members: Vec<_> = members
                .iter()
                .filter_map(|id| agents_output.agents.iter().find(|a| &a.id == id))
                .collect();
            output::print_group(&group_name, &group_members, json)?;
        }
    }
    Ok(())
}

/// Literal slash command delivered by `kira-mux send --clear`.
pub(super) const CLEAR_PROMPT: &str = "/clear";

pub(super) fn cmd_send(
    project_target: &ProjectTarget,
    profile: Option<&str>,
    agent_id: &str,
    prompt: &str,
    no_template: bool,
    wait: bool,
    lines: Option<usize>,
) -> Result<()> {
    let (project, tmux) = load_project_context(project_target, profile, ResolutionMode::Deferred)?;
    if !wait {
        let delivered =
            crate::agent_io::send_prompt(&tmux, &project, agent_id, prompt, no_template)?;
        log_prompt_delivered(agent_id, &delivered);
        return Ok(());
    }

    // Clap rejects zero when `--lines` is set; omitted uses the wait default.
    let capture_lines = lines.unwrap_or(crate::agent_io::DEFAULT_WAIT_CAPTURE_LINES);
    debug_assert!(
        capture_lines >= 1,
        "wait capture window must stay non-empty (got {capture_lines})"
    );
    let seed = crate::agent_io::send_prompt_for_wait(
        &tmux,
        &project,
        agent_id,
        prompt,
        no_template,
        capture_lines,
    )?;
    log_prompt_delivered(agent_id, &seed.delivered);
    let wait_result = crate::agent_io::wait_on_pane(
        &tmux,
        agent_id,
        &seed,
        &crate::agent_io::WaitOptions::from_env()?,
    )
    // Alternate-screen TUIs cap the converged capture at the visible frame;
    // deepen it best-effort so long replies come back whole.
    .map(|converged| {
        crate::agent_io::deepen_wait_capture(
            &tmux,
            &seed.delivered.pane_id,
            capture_lines,
            converged,
        )
    });
    finish_wait(wait_result)
}

/// Length only: prompt content may carry user secrets, keep it out of logs.
fn log_prompt_delivered(agent_id: &str, delivered: &crate::agent_io::DeliveredPrompt) {
    tracing::debug!(
        agent = agent_id,
        pane = %delivered.pane_id,
        rendered_len = delivered.rendered.len(),
        "prompt delivered"
    );
}

/// Map a wait outcome to stdout/stderr and the propagated error.
///
/// Success: pane text on stdout (trailing newline guaranteed). Timeout: last
/// capture on stderr so stdout stays reserved for confirmed-stable output;
/// the typed error is still returned for exit-code mapping.
fn finish_wait(result: Result<String>) -> Result<()> {
    match result {
        Ok(captured) => output::print_pane_text(&captured),
        Err(error) => {
            if let Some(partial) = wait_timeout_stderr_payload(&error) {
                // Best-effort: broken stderr pipes must not mask the timeout error.
                let _ = write_stderr_timeout_capture(partial);
            }
            Err(error)
        }
    }
}

fn write_stderr_timeout_capture(partial: &str) -> Result<()> {
    use std::io::Write;
    let mut err = std::io::stderr().lock();
    write!(err, "{partial}").context("failed to write wait timeout capture to stderr")?;
    if !partial.ends_with('\n') {
        writeln!(err).context("failed to write wait timeout capture to stderr")?;
    }
    Ok(())
}

/// Extract the last capture from a wait-timeout error for stderr emission.
fn wait_timeout_stderr_payload(error: &anyhow::Error) -> Option<&str> {
    match error.downcast_ref::<KiraMuxError>() {
        Some(KiraMuxError::WaitTimeout { last_capture, .. }) => Some(last_capture.as_str()),
        _ => None,
    }
}

pub(super) fn cmd_capture(
    project_target: &ProjectTarget,
    profile: Option<&str>,
    agent_id: &str,
    lines: usize,
    json: bool,
) -> Result<()> {
    let (project, tmux) = load_project_context(project_target, profile, ResolutionMode::Deferred)?;
    let capture = crate::agent_io::capture_output(&tmux, &project, agent_id, lines)?;
    output::print_capture(&capture, json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wait_timeout_exposes_last_capture_for_stderr() {
        let err = anyhow::Error::new(KiraMuxError::WaitTimeout {
            agent_id: "alpha".into(),
            last_capture: "partial output".into(),
        });
        assert_eq!(wait_timeout_stderr_payload(&err), Some("partial output"));
    }

    #[test]
    fn wait_timeout_payload_absent_for_other_errors() {
        let err = anyhow::Error::new(KiraMuxError::PaneDiedDuringWait("alpha".into()));
        assert_eq!(wait_timeout_stderr_payload(&err), None);

        let err = anyhow::Error::new(KiraMuxError::DeadPane("alpha".into()));
        assert_eq!(wait_timeout_stderr_payload(&err), None);
    }

    #[test]
    fn finish_wait_propagates_timeout_error_after_exposing_capture() {
        let err = anyhow::Error::new(KiraMuxError::WaitTimeout {
            agent_id: "alpha".into(),
            last_capture: "partial\n".into(),
        });
        // finish_wait writes to the process stderr; we only assert the error
        // chain and payload extraction stay aligned for exit-code mapping.
        assert!(wait_timeout_stderr_payload(&err).is_some());
        let result = finish_wait(Err(err));
        assert!(matches!(
            result.as_ref().err().and_then(|e| e.downcast_ref::<KiraMuxError>()),
            Some(KiraMuxError::WaitTimeout { agent_id, last_capture })
                if agent_id == "alpha" && last_capture == "partial\n"
        ));
    }
}
