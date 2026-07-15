//! Agent listing, send, and capture handlers.

use anyhow::Result;

use super::load_project_context;
use crate::cli::AgentsCommand;
use crate::config::ResolutionMode;
use crate::error::KiraMuxError;
use crate::output;

pub(super) fn cmd_agents_dispatch(sub: AgentsCommand) -> Result<()> {
    let (project_id, profile) = match &sub {
        AgentsCommand::List {
            project_id,
            profile,
            ..
        }
        | AgentsCommand::Capabilities {
            project_id,
            profile,
            ..
        }
        | AgentsCommand::Group {
            project_id,
            profile,
            ..
        } => (project_id.as_str(), profile.as_deref()),
    };
    let (project, tmux) = load_project_context(project_id, profile, ResolutionMode::Deferred)?;
    let topology = crate::inspector::inspect(&tmux, &project)?;
    let agents_output = crate::model::build_agents_output(&project, &topology);

    match sub {
        AgentsCommand::List { json, .. } => {
            if json {
                output::print_json(&agents_output)?;
            } else {
                output::print_agents_table(&agents_output);
            }
        }
        AgentsCommand::Capabilities { agent_id, json, .. } => {
            let agent = agents_output
                .agents
                .iter()
                .find(|a| a.id == agent_id)
                .ok_or_else(|| KiraMuxError::UnknownAgentId(agent_id.clone()))?;
            if json {
                output::print_json(&output::AgentCapabilitiesOutput::from(agent))?;
            } else {
                output::print_agent_capabilities(agent);
            }
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
            if json {
                output::print_json(&output::GroupOutput::new(&group_name, &group_members))?;
            } else {
                output::print_group(&group_name, &group_members);
            }
        }
    }
    Ok(())
}

pub(super) fn cmd_send(
    project_id: &str,
    profile: Option<&str>,
    agent_id: &str,
    prompt: &str,
    no_template: bool,
    wait: bool,
) -> Result<()> {
    let (project, tmux) = load_project_context(project_id, profile, ResolutionMode::Deferred)?;
    let delivered = crate::agent_io::send_prompt(&tmux, &project, agent_id, prompt, no_template)?;
    // Length only: prompt content may carry user secrets, keep it out of logs.
    tracing::debug!(
        agent = agent_id,
        pane = %delivered.pane_id,
        rendered_len = delivered.rendered.len(),
        "prompt delivered"
    );
    if !wait {
        return Ok(());
    }
    // Reuse the pane id from send so the post-submit baseline is captured
    // without a second full inspect (narrows the fast-reply race).
    let wait_result = crate::agent_io::wait_on_pane(
        &tmux,
        agent_id,
        &delivered.pane_id,
        &crate::agent_io::WaitOptions::default(),
    );
    finish_wait(wait_result)
}

/// Map a wait outcome to stdout/stderr and the propagated error.
///
/// Success: pane text on stdout (trailing newline guaranteed). Timeout: last
/// capture on stderr so stdout stays reserved for confirmed-stable output;
/// the typed error is still returned for exit-code mapping.
fn finish_wait(result: Result<String>) -> Result<()> {
    match result {
        Ok(captured) => {
            print_pane_text(&captured);
            Ok(())
        }
        Err(error) => {
            if let Some(partial) = wait_timeout_stderr_payload(&error) {
                eprint!("{partial}");
                if !partial.ends_with('\n') {
                    eprintln!();
                }
            }
            Err(error)
        }
    }
}

/// Extract the last capture from a wait-timeout error for stderr emission.
fn wait_timeout_stderr_payload(error: &anyhow::Error) -> Option<&str> {
    match error.downcast_ref::<KiraMuxError>() {
        Some(KiraMuxError::WaitTimeout { last_capture, .. }) => Some(last_capture.as_str()),
        _ => None,
    }
}

pub(super) fn cmd_capture(
    project_id: &str,
    profile: Option<&str>,
    agent_id: &str,
    lines: usize,
    json: bool,
) -> Result<()> {
    let (project, tmux) = load_project_context(project_id, profile, ResolutionMode::Deferred)?;
    let capture = crate::agent_io::capture_output(&tmux, &project, agent_id, lines)?;
    if json {
        output::print_json(&capture)?;
    } else {
        print_pane_text(&capture.output);
    }
    Ok(())
}

/// Print captured pane text, guaranteeing a trailing newline.
fn print_pane_text(output: &str) {
    print!("{output}");
    if !output.ends_with('\n') {
        println!();
    }
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
