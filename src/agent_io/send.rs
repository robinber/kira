//! Deliver a prompt into a live agent pane.
//!
//! Prepares (optional template), pastes or types text, applies submit
//! policy, and can seed `wait` with a pre-submit capture for convergence.

use std::time::Duration;

use anyhow::Result;

use super::policy::{SubmitBehavior, infer_submit_behavior, needs_send_keys_for_text};
use super::resolve::{or_dead_pane, resolve_managed_pane};
use crate::error::KiraMuxError;
use crate::inspector::WorkspaceTopology;
use crate::model::{ResolvedAgent, ResolvedProject};
use crate::prompt::PromptContext;
use crate::tmux::metadata::PANE_AGENT_COMMAND;
use crate::tmux::{PaneInfo, TmuxAdapter};

/// Delay before the second Enter for double-enter agents. A frame delta
/// cannot attest that the TUI consumed the first Enter (spinners repaint
/// regardless), so this spacing stays a fixed delay rather than a
/// lookalike readiness check.
const DOUBLE_ENTER_DELAY: Duration = Duration::from_millis(200);
/// Default lines of pane history observed by `send --wait`.
pub(crate) const DEFAULT_WAIT_CAPTURE_LINES: usize = 200;

/// Result of a successful prompt delivery.
pub(crate) struct DeliveredPrompt {
    /// Rendered text that was pasted/submitted into the pane.
    pub(crate) rendered: String,
    /// Pane that received the prompt.
    pub(crate) pane_id: String,
}

/// Observation captured around prompt delivery for `send --wait`.
pub(crate) struct WaitSeed {
    /// Delivery result: target pane plus the exact rendered prompt (the
    /// latter is used only as an opportunistic submission hint).
    pub(crate) delivered: DeliveredPrompt,
    /// Pane capture taken immediately before submission.
    pub(crate) pre_submit: String,
    /// History lines used for the pre-submit capture and subsequent wait polls.
    pub(crate) capture_lines: usize,
    /// Lowercase busy-marker fragments for this agent; while one is visible
    /// near the pane bottom the wait loop refuses to converge.
    pub(crate) busy_markers: Vec<String>,
}

struct PreparedPrompt<'a> {
    pane: PaneInfo,
    agent: &'a ResolvedAgent,
    rendered: String,
}

/// Render the final prompt for `agent_id` and deliver it to the agent's
/// managed pane. Returns the rendered text and the target pane id.
///
/// Delivery requires a **live** pane only. Kira does not wait for the agent
/// TUI to be input-ready (trust dialogs, logins, …). Operators bootstrap
/// interactive tools via `open` / attach before unattended `send` — see the
/// README “Running vs input-ready” section.
///
/// # Errors
///
/// Fails when the session is absent, the workspace is drifted, the agent is
/// unknown, the pane is dead, or tmux rejects the delivery.
pub(crate) fn send_prompt(
    tmux: &dyn TmuxAdapter,
    project: &ResolvedProject,
    agent_id: &str,
    prompt: &str,
    no_template: bool,
) -> Result<DeliveredPrompt> {
    let prepared = prepare_prompt(tmux, project, agent_id, prompt, no_template)?;
    deliver_prepared(tmux, agent_id, prepared)
}

/// Capture the pane immediately before delivery, then submit the prompt.
///
/// This observed path is reserved for `send --wait`: plain `send` keeps its
/// single delivery path and does not pay for an extra pane capture.
///
/// `capture_lines` is the history window used for the pre-submit snapshot and
/// every poll during wait (CLI default: [`DEFAULT_WAIT_CAPTURE_LINES`]).
pub(crate) fn send_prompt_for_wait(
    tmux: &dyn TmuxAdapter,
    project: &ResolvedProject,
    agent_id: &str,
    prompt: &str,
    no_template: bool,
    capture_lines: usize,
) -> Result<WaitSeed> {
    let prepared = prepare_prompt(tmux, project, agent_id, prompt, no_template)?;
    let pre_submit = capture_before_submit(tmux, &prepared.pane.pane_id, agent_id, capture_lines)?;
    // Best-effort pane-command read: marker inference degrades to the agent
    // config alone, and a genuinely broken pane fails typed during delivery.
    let pane_command = tmux
        .get_pane_option(&prepared.pane.pane_id, PANE_AGENT_COMMAND)
        .unwrap_or(None);
    let mut busy_markers =
        super::policy::infer_busy_markers(prepared.agent, pane_command.as_deref());
    // A marker phrase inside the prompt itself would match the prompt echo
    // near the pane bottom and pin the wait to its hard timeout: drop those
    // markers for this send and fall back to frame-diff convergence.
    let rendered_search = crate::tmux::normalize_search_text(&prepared.rendered).to_lowercase();
    busy_markers.retain(|marker| {
        let keep = !rendered_search.contains(marker.as_str());
        if !keep {
            tracing::debug!(
                agent = agent_id,
                marker,
                "busy marker appears in the rendered prompt; disabled for this wait"
            );
        }
        keep
    });
    let delivered = deliver_prepared(tmux, agent_id, prepared)?;
    Ok(WaitSeed {
        delivered,
        pre_submit,
        capture_lines,
        busy_markers,
    })
}

fn prepare_prompt<'a>(
    tmux: &dyn TmuxAdapter,
    project: &'a ResolvedProject,
    agent_id: &str,
    prompt: &str,
    no_template: bool,
) -> Result<PreparedPrompt<'a>> {
    let (pane, agent, topology) = resolve_managed_pane(tmux, project, agent_id)?;
    // Gate: process liveness only — not application readiness.
    if pane.pane_dead {
        return Err(KiraMuxError::DeadPane(agent_id.to_string()).into());
    }

    let rendered = render_final_prompt(project, agent, prompt, no_template, &topology);
    Ok(PreparedPrompt {
        pane,
        agent,
        rendered,
    })
}

fn deliver_prepared(
    tmux: &dyn TmuxAdapter,
    agent_id: &str,
    prepared: PreparedPrompt<'_>,
) -> Result<DeliveredPrompt> {
    paste_and_submit(
        tmux,
        &prepared.pane,
        prepared.agent,
        agent_id,
        &prepared.rendered,
    )?;
    Ok(DeliveredPrompt {
        rendered: prepared.rendered,
        pane_id: prepared.pane.pane_id,
    })
}

fn capture_before_submit(
    tmux: &dyn TmuxAdapter,
    pane_id: &str,
    agent_id: &str,
    capture_lines: usize,
) -> Result<String> {
    or_dead_pane(agent_id, tmux.capture_pane(pane_id, capture_lines))
}

/// Compute the final prompt text for `agent` without mutating tmux.
///
/// Applies the agent's `prompt_template` (when present and `no_template`
/// is `false`) using the topology already inspected for pane resolution, so
/// the pane context rendered into the prompt describes the same workspace
/// state the prompt is delivered into. Returns the raw prompt unchanged when
/// no template applies.
fn render_final_prompt(
    project: &ResolvedProject,
    agent: &ResolvedAgent,
    prompt: &str,
    no_template: bool,
    topology: &WorkspaceTopology,
) -> String {
    match agent.prompt_template.as_deref() {
        Some(template) if !no_template => {
            let (active_agents, agent_states) =
                crate::prompt::extract_agent_state(topology, project);
            let ctx = PromptContext {
                user_prompt: prompt.to_owned(),
                // Prefer the configured label so templates see the human name.
                agent_name: agent.label.clone(),
                project_name: project.name.clone(),
                active_agents,
                agent_states,
            };
            crate::prompt::render(template, &ctx)
        }
        _ => prompt.to_owned(),
    }
}

/// Paste `final_prompt` (when non-empty) into `pane` and submit one or two
/// `Enter` keys according to the agent's submit behavior.
///
/// A pane that vanishes between the pre-submit liveness check and delivery
/// surfaces as [`KiraMuxError::DeadPane`] (exit 6), matching the dead-pane
/// gate above and the wait-path pattern in [`super::wait`].
fn paste_and_submit(
    tmux: &dyn TmuxAdapter,
    pane: &PaneInfo,
    agent: &ResolvedAgent,
    agent_id: &str,
    final_prompt: &str,
) -> Result<()> {
    or_dead_pane(
        agent_id,
        paste_and_submit_inner(tmux, pane, agent, final_prompt),
    )
}

fn paste_and_submit_inner(
    tmux: &dyn TmuxAdapter,
    pane: &PaneInfo,
    agent: &ResolvedAgent,
    final_prompt: &str,
) -> Result<()> {
    let pane_command = tmux.get_pane_option(&pane.pane_id, PANE_AGENT_COMMAND)?;
    let behavior = infer_submit_behavior(agent, pane_command.as_deref());
    if !final_prompt.is_empty() && needs_send_keys_for_text(agent, pane_command.as_deref()) {
        tracing::debug!(
            pane = %pane.pane_id,
            delivery = "send-keys",
            submit = ?behavior,
            "delivering prompt"
        );
        crate::tmux::send_then_submit_text(tmux, &pane.pane_id, final_prompt)?;
    } else {
        tracing::debug!(
            pane = %pane.pane_id,
            delivery = "paste",
            submit = ?behavior,
            "delivering prompt"
        );
        crate::tmux::paste_then_submit_text(tmux, &pane.pane_id, final_prompt)?;
    }

    if behavior == SubmitBehavior::DoubleEnter {
        std::thread::sleep(DOUBLE_ENTER_DELAY);
        tmux.send_keys(&pane.pane_id, &["Enter"])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
