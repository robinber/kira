use std::time::Duration;

use anyhow::Result;

use super::policy::{SubmitBehavior, infer_submit_behavior, needs_send_keys_for_text};
use super::resolve::resolve_managed_pane;
use crate::error::KiraMuxError;
use crate::inspector::WorkspaceTopology;
use crate::model::{ResolvedAgent, ResolvedProject};
use crate::prompt::PromptContext;
use crate::tmux::metadata::PANE_AGENT_COMMAND;
use crate::tmux::{PaneInfo, TmuxAdapter};

/// Delay between typing literal text and submitting it, so the TUI has
/// rendered the input before the Enter arrives.
const SEND_TEXT_SETTLE: Duration = Duration::from_millis(100);
/// Delay before the second Enter for double-enter agents.
const DOUBLE_ENTER_DELAY: Duration = Duration::from_millis(200);

/// Result of a successful prompt delivery.
pub(crate) struct DeliveredPrompt {
    /// Rendered text that was pasted/submitted into the pane.
    pub(crate) rendered: String,
    /// Pane that received the prompt; reuse for `send --wait` without a
    /// second full workspace inspect (keeps the post-submit baseline tight).
    pub(crate) pane_id: String,
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
    let (pane, agent, topology) = resolve_managed_pane(tmux, project, agent_id)?;
    // Gate: process liveness only — not application readiness.
    if pane.pane_dead {
        return Err(KiraMuxError::DeadPane(agent_id.to_string()).into());
    }

    let final_prompt = render_final_prompt(project, agent, prompt, no_template, &topology);
    paste_and_submit(tmux, &pane, agent, &final_prompt)?;
    Ok(DeliveredPrompt {
        rendered: final_prompt,
        pane_id: pane.pane_id,
    })
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
fn paste_and_submit(
    tmux: &dyn TmuxAdapter,
    pane: &PaneInfo,
    agent: &ResolvedAgent,
    final_prompt: &str,
) -> Result<()> {
    let pane_command = tmux.get_pane_option(&pane.pane_id, PANE_AGENT_COMMAND)?;
    if !final_prompt.is_empty() && needs_send_keys_for_text(agent, pane_command.as_deref()) {
        tmux.send_text(&pane.pane_id, final_prompt)?;
        std::thread::sleep(SEND_TEXT_SETTLE);
        tmux.send_keys(&pane.pane_id, &["Enter"])?;
    } else {
        crate::tmux::paste_then_submit_text(tmux, &pane.pane_id, final_prompt)?;
    }

    let behavior = infer_submit_behavior(agent, pane_command.as_deref());
    if behavior == SubmitBehavior::DoubleEnter {
        std::thread::sleep(DOUBLE_ENTER_DELAY);
        tmux.send_keys(&pane.pane_id, &["Enter"])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    // Readiness is operator-managed: these tests only assert dead-pane /
    // topology gates. There is intentionally no “wait until TUI idle” path.
    use super::*;
    use crate::test_support::{FakeOp, TestResultExt, setup_session_with_dead_panes};
    use crate::workspace::session_name;

    #[test]
    fn send_prompt_generic_agent_sends_paste_then_single_enter() {
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        crate::test_support::setup_healthy_session(&fake, &project);

        send_prompt(&fake, &project, "alpha", "hello world", false).or_panic();

        let ops = fake.ops();
        assert_eq!(ops.len(), 2, "expected paste + 1 Enter, got: {ops:?}");
        assert_eq!(
            ops[0],
            FakeOp::PasteText {
                pane_id: "%0".to_string(),
                text: "hello world".to_string(),
            }
        );
        assert_eq!(
            ops[1],
            FakeOp::SendKeys {
                pane_id: "%0".to_string(),
                keys: vec!["Enter".to_string()],
            }
        );
    }

    #[test]
    fn send_prompt_codex_agent_sends_paste_then_double_enter() {
        let fake = crate::test_support::FakeTmux::new();
        let mut project = crate::test_support::test_project();
        project.agents[0].command = Some("codex".to_string());
        crate::test_support::setup_healthy_session(&fake, &project);

        send_prompt(&fake, &project, "alpha", "review this", false).or_panic();

        let ops = fake.ops();
        assert_eq!(ops.len(), 3, "expected paste + 2 Enters, got: {ops:?}");
        assert_eq!(
            ops[0],
            FakeOp::PasteText {
                pane_id: "%0".to_string(),
                text: "review this".to_string(),
            }
        );
        assert_eq!(
            ops[1],
            FakeOp::SendKeys {
                pane_id: "%0".to_string(),
                keys: vec!["Enter".to_string()],
            }
        );
        assert_eq!(
            ops[2],
            FakeOp::SendKeys {
                pane_id: "%0".to_string(),
                keys: vec!["Enter".to_string()],
            }
        );
    }

    #[test]
    fn send_prompt_opencode_agent_types_literal_text() {
        let fake = crate::test_support::FakeTmux::new();
        let mut project = crate::test_support::test_project();
        project.agents[0].command = Some("opencode".to_string());
        crate::test_support::setup_healthy_session(&fake, &project);

        // A prompt matching a tmux key name must arrive as literal text, not
        // as a keypress.
        send_prompt(&fake, &project, "alpha", "Enter", false).or_panic();

        let ops = fake.ops();
        assert_eq!(
            ops[0],
            FakeOp::SendText {
                pane_id: "%0".to_string(),
                text: "Enter".to_string(),
            },
            "prompt text must go through the literal-text channel, got: {ops:?}"
        );
        assert!(
            ops[1..]
                .iter()
                .all(|op| matches!(op, FakeOp::SendKeys { keys, .. } if keys == &["Enter"])),
            "remaining ops must be Enter submits, got: {ops:?}"
        );
    }

    #[test]
    fn send_prompt_reads_pane_metadata_for_submit_behavior() {
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        crate::test_support::setup_healthy_session(&fake, &project);
        let session = session_name(&project);
        fake.set_pane_opt(
            &session,
            &project.window_name,
            0,
            "@kira_mux_agent_command",
            "codex",
        );

        send_prompt(&fake, &project, "alpha", "hello", false).or_panic();

        let ops = fake.ops();
        assert_eq!(ops.len(), 3, "expected paste + 2 Enters, got: {ops:?}");
        assert_eq!(
            ops[0],
            FakeOp::PasteText {
                pane_id: "%0".to_string(),
                text: "hello".to_string(),
            }
        );
        assert_eq!(
            ops[1],
            FakeOp::SendKeys {
                pane_id: "%0".to_string(),
                keys: vec!["Enter".to_string()],
            }
        );
        assert_eq!(
            ops[2],
            FakeOp::SendKeys {
                pane_id: "%0".to_string(),
                keys: vec!["Enter".to_string()],
            }
        );
    }

    #[test]
    fn send_prompt_falls_back_without_pane_metadata() {
        let fake = crate::test_support::FakeTmux::new();
        let mut project = crate::test_support::test_project();
        project.agents[0].command = Some("codex".to_string());
        crate::test_support::setup_healthy_session(&fake, &project);

        send_prompt(&fake, &project, "alpha", "hello", false).or_panic();

        let ops = fake.ops();
        assert_eq!(ops.len(), 3, "expected paste + 2 Enters, got: {ops:?}");
    }

    #[test]
    fn send_prompt_dead_pane_fails() {
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        setup_session_with_dead_panes(&fake, &project, &[0]);

        let err = send_prompt(&fake, &project, "alpha", "hello", false).err_or_panic();
        assert!(matches!(
            err.downcast_ref::<KiraMuxError>(),
            Some(KiraMuxError::DeadPane(id)) if id == "alpha"
        ));
        assert!(fake.ops().is_empty());
    }

    #[test]
    fn send_prompt_absent_session_fails() {
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        let err = send_prompt(&fake, &project, "alpha", "hello", false).err_or_panic();
        assert!(matches!(
            err.downcast_ref::<KiraMuxError>(),
            Some(KiraMuxError::SessionAbsent)
        ));
    }

    #[test]
    fn send_prompt_propagates_paste_failure() {
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        crate::test_support::setup_healthy_session(&fake, &project);
        fake.set_fail_paste(true);

        let result = send_prompt(&fake, &project, "alpha", "hello", false);
        assert!(result.is_err());
    }

    #[test]
    fn send_prompt_propagates_send_keys_failure() {
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        crate::test_support::setup_healthy_session(&fake, &project);
        fake.set_fail_send_keys(true);

        let result = send_prompt(&fake, &project, "alpha", "hello", false);
        assert!(result.is_err());
    }

    #[test]
    fn send_prompt_empty_skips_paste() {
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        crate::test_support::setup_healthy_session(&fake, &project);

        send_prompt(&fake, &project, "alpha", "", false).or_panic();

        let ops = fake.ops();
        assert_eq!(ops.len(), 1, "expected only Enter, got: {ops:?}");
        assert_eq!(
            ops[0],
            FakeOp::SendKeys {
                pane_id: "%0".to_string(),
                keys: vec!["Enter".to_string()],
            }
        );
    }

    #[test]
    fn send_prompt_with_template_renders_context() {
        let fake = crate::test_support::FakeTmux::new();
        let mut project = crate::test_support::test_project();
        project.agents[0].prompt_template =
            Some("Agent {{agent_name}} in {{project_name}}: {{user_prompt}}".to_string());
        crate::test_support::setup_healthy_session(&fake, &project);

        send_prompt(&fake, &project, "alpha", "hello world", false).or_panic();

        let ops = fake.ops();
        assert!(
            ops.iter().any(|op| matches!(
                op,
                FakeOp::PasteText { text, .. }
                    if text == "Agent Alpha in Test: hello world"
            )),
            "expected rendered template (label as agent_name) in paste, got: {ops:?}"
        );
    }

    #[test]
    fn send_prompt_without_template_sends_raw() {
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        crate::test_support::setup_healthy_session(&fake, &project);

        send_prompt(&fake, &project, "alpha", "raw prompt", false).or_panic();

        let ops = fake.ops();
        assert!(
            ops.iter().any(|op| matches!(
                op,
                FakeOp::PasteText { text, .. }
                    if text == "raw prompt"
            )),
            "expected raw prompt in paste, got: {ops:?}"
        );
    }

    #[test]
    fn send_prompt_no_template_bypasses_rendering() {
        let fake = crate::test_support::FakeTmux::new();
        let mut project = crate::test_support::test_project();
        project.agents[0].prompt_template =
            Some("Agent {{agent_name}} in {{project_name}}: {{user_prompt}}".to_string());
        crate::test_support::setup_healthy_session(&fake, &project);

        send_prompt(&fake, &project, "alpha", "raw prompt", true).or_panic();

        let ops = fake.ops();
        assert!(
            ops.iter().any(|op| matches!(
                op,
                FakeOp::PasteText { text, .. }
                    if text == "raw prompt"
            )),
            "expected raw prompt (no template rendering) in paste, got: {ops:?}"
        );
    }

    #[test]
    fn send_prompt_delivers_exactly_once() {
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        crate::test_support::setup_healthy_session(&fake, &project);

        send_prompt(&fake, &project, "alpha", "one message", false).or_panic();

        let ops = fake.ops();
        let paste_count = ops
            .iter()
            .filter(|op| matches!(op, FakeOp::PasteText { .. }))
            .count();
        let enter_count = ops
            .iter()
            .filter(|op| matches!(op, FakeOp::SendKeys { keys, .. } if keys == &["Enter"]))
            .count();
        assert_eq!(paste_count, 1, "expected exactly one paste, got: {ops:?}");
        assert_eq!(enter_count, 1, "expected exactly one Enter, got: {ops:?}");
    }

    #[test]
    fn send_prompt_returns_rendered_prompt_not_raw() {
        let fake = crate::test_support::FakeTmux::new();
        let mut project = crate::test_support::test_project();
        project.agents[0].prompt_template =
            Some("Agent {{agent_name}} in {{project_name}}: {{user_prompt}}".to_string());
        crate::test_support::setup_healthy_session(&fake, &project);

        let sent = send_prompt(&fake, &project, "alpha", "hello world", false).or_panic();
        assert_eq!(
            sent.rendered, "Agent Alpha in Test: hello world",
            "send_prompt must return the rendered prompt, not the raw input"
        );
        assert_eq!(sent.pane_id, "%0");
    }

    #[test]
    fn send_prompt_returns_raw_slash_command_unchanged() {
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        crate::test_support::setup_healthy_session(&fake, &project);

        let sent = send_prompt(&fake, &project, "alpha", "/help", false).or_panic();
        assert_eq!(sent.rendered, "/help");
        assert_eq!(sent.pane_id, "%0");
    }

    #[test]
    fn send_prompt_returns_rendered_slash_command() {
        let fake = crate::test_support::FakeTmux::new();
        let mut project = crate::test_support::test_project();
        project.agents[0].prompt_template = Some("/cmd {{user_prompt}}".to_string());
        crate::test_support::setup_healthy_session(&fake, &project);

        let sent = send_prompt(&fake, &project, "alpha", "args here", false).or_panic();
        assert_eq!(sent.rendered, "/cmd args here");
        assert_eq!(sent.pane_id, "%0");
    }
}
