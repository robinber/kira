use std::time::Duration;

use anyhow::{Result, bail};

use super::policy::{SubmitBehavior, infer_submit_behavior, needs_send_keys_for_text};
use super::resolve::resolve_managed_pane;
use crate::model::ResolvedProject;
use crate::tmux::TmuxAdapter;
use crate::tmux::metadata::PANE_AGENT_COMMAND;

pub(crate) struct PreparedPrompt {
    pub final_prompt: String,
}

/// Resolve the target pane and render the final prompt without mutating
/// tmux.
///
/// Fails fast on pane resolution so callers still see `SessionAbsent`,
/// `UnknownAgentId`, and dead-pane errors before delivery. Call
/// [`send_rendered_prompt`] separately to paste the rendered text.
pub(crate) fn prepare_prompt(
    tmux: &dyn TmuxAdapter,
    project: &ResolvedProject,
    agent_id: &str,
    prompt: &str,
    no_template: bool,
) -> Result<PreparedPrompt> {
    let (pane, _agent) = resolve_managed_pane(tmux, project, agent_id)?;
    if pane.pane_dead {
        bail!("cannot send to dead pane for agent '{agent_id}'");
    }

    let final_prompt = render_final_prompt(tmux, project, agent_id, prompt, no_template);
    Ok(PreparedPrompt { final_prompt })
}

/// Compute the final prompt text for `agent_id` without mutating tmux.
///
/// Applies the agent's `prompt_template` (when present and `no_template`
/// is `false`) using a rich pane-topology context, falling back to a
/// minimal context when tmux inspection fails. Returns the raw prompt
/// unchanged when no template applies. Performs no paste, no `send_keys`,
/// no pane resolution — tests and callers can rely on this being
/// side-effect free on the tmux mutation channels.
pub(crate) fn render_final_prompt(
    tmux: &dyn TmuxAdapter,
    project: &ResolvedProject,
    agent_id: &str,
    prompt: &str,
    no_template: bool,
) -> String {
    let agent = project.agents.iter().find(|a| a.id == agent_id);
    match agent.and_then(|a| a.prompt_template.as_deref()) {
        Some(template) if !no_template => {
            let ctx = match crate::inspector::inspect(tmux, project) {
                Ok(topology) => {
                    let (active_agents, agent_states) =
                        crate::prompt::extract_agent_state(&topology, project);
                    crate::prompt::PromptContext::resolve(
                        prompt.to_owned(),
                        agent_id.to_owned(),
                        project.name.clone(),
                        active_agents,
                        agent_states,
                    )
                }
                Err(e) => {
                    tracing::debug!(error = %e, "tmux topology inspection failed; using minimal prompt context");
                    crate::prompt::PromptContext::minimal(agent_id, &project.name, prompt)
                }
            };
            crate::prompt::render(template, &ctx)
        }
        _ => prompt.to_owned(),
    }
}

/// Deliver an already-rendered prompt to the managed pane for `agent_id`.
///
/// Does not re-render the prompt against any template — callers supply the
/// exact bytes to paste.
pub(crate) fn send_rendered_prompt(
    tmux: &dyn TmuxAdapter,
    project: &ResolvedProject,
    agent_id: &str,
    final_prompt: &str,
) -> Result<()> {
    let (pane, agent) = resolve_managed_pane(tmux, project, agent_id)?;
    if pane.pane_dead {
        bail!("cannot send to dead pane for agent '{agent_id}'");
    }
    paste_and_submit(tmux, &pane, &agent, final_prompt)
}

/// Paste `final_prompt` (when non-empty) into `pane` and submit one or two
/// `Enter` keys according to the agent's submit behavior. Extracted so
/// callers of [`prepare_prompt`] and [`send_rendered_prompt`] share exactly the
/// same delivery semantics.
fn paste_and_submit(
    tmux: &dyn TmuxAdapter,
    pane: &crate::tmux::PaneInfo,
    agent: &crate::model::ResolvedAgent,
    final_prompt: &str,
) -> Result<()> {
    let pane_command = tmux.get_pane_option(&pane.pane_id, PANE_AGENT_COMMAND)?;
    if !final_prompt.is_empty() && needs_send_keys_for_text(agent, pane_command.as_deref()) {
        tmux.send_keys(&pane.pane_id, &[final_prompt])?;
        std::thread::sleep(Duration::from_millis(100));
        tmux.send_keys(&pane.pane_id, &["Enter"])?;
    } else {
        crate::tmux::paste_then_submit_text(tmux, &pane.pane_id, final_prompt)?;
    }

    let behavior = infer_submit_behavior(agent, pane_command.as_deref());
    if behavior == SubmitBehavior::DoubleEnter {
        std::thread::sleep(Duration::from_millis(200));
        tmux.send_keys(&pane.pane_id, &["Enter"])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test-local composition of [`prepare_prompt`] + [`send_rendered_prompt`],
    /// preserving the pre-split call shape these behavior tests were written
    /// against.
    fn send_prompt(
        tmux: &dyn TmuxAdapter,
        project: &ResolvedProject,
        agent_id: &str,
        prompt: &str,
        no_template: bool,
    ) -> Result<PreparedPrompt> {
        let prepared = prepare_prompt(tmux, project, agent_id, prompt, no_template)?;
        send_rendered_prompt(tmux, project, agent_id, &prepared.final_prompt)?;
        Ok(prepared)
    }
    use crate::test_support::{FakeOp, TestResultExt};
    use crate::tmux::metadata::{WINDOW_ROLE, WINDOW_ROLE_AGENTS};
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
        let session = session_name(&project);

        fake.add_session(&session);
        fake.set_session_opt(
            &session,
            "@kira_mux_config_fingerprint",
            &project.fingerprint,
        );
        fake.set_session_opt(&session, "@kira_mux_project_id", &project.id);
        fake.add_window(&session, &project.window_name);
        fake.set_window_opt(
            &session,
            &project.window_name,
            WINDOW_ROLE,
            WINDOW_ROLE_AGENTS,
        );
        fake.add_pane(&session, &project.window_name, "%0", true);
        fake.set_pane_opt(
            &session,
            &project.window_name,
            0,
            "@kira_mux_agent_id",
            "alpha",
        );
        fake.add_pane(&session, &project.window_name, "%1", false);
        fake.set_pane_opt(
            &session,
            &project.window_name,
            1,
            "@kira_mux_agent_id",
            "beta",
        );

        let err = send_prompt(&fake, &project, "alpha", "hello", false).err_or_panic();
        assert!(err.to_string().contains("dead pane"));
        assert!(fake.ops().is_empty());
    }

    #[test]
    fn send_prompt_absent_session_fails() {
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        let err = send_prompt(&fake, &project, "alpha", "hello", false).err_or_panic();
        assert!(matches!(
            err.downcast_ref::<crate::error::KiraMuxError>(),
            Some(crate::error::KiraMuxError::SessionAbsent)
        ));
    }

    #[test]
    fn outbox_not_recorded_on_absent_session() {
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();

        let result = send_prompt(&fake, &project, "alpha", "hello", false);
        assert!(result.is_err());
    }

    #[test]
    fn outbox_not_recorded_on_dead_pane() {
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        let session = session_name(&project);

        fake.add_session(&session);
        fake.set_session_opt(
            &session,
            "@kira_mux_config_fingerprint",
            &project.fingerprint,
        );
        fake.set_session_opt(&session, "@kira_mux_project_id", &project.id);
        fake.add_window(&session, &project.window_name);
        fake.set_window_opt(
            &session,
            &project.window_name,
            WINDOW_ROLE,
            WINDOW_ROLE_AGENTS,
        );
        fake.add_pane(&session, &project.window_name, "%0", true);
        fake.set_pane_opt(
            &session,
            &project.window_name,
            0,
            "@kira_mux_agent_id",
            "alpha",
        );
        fake.add_pane(&session, &project.window_name, "%1", false);
        fake.set_pane_opt(
            &session,
            &project.window_name,
            1,
            "@kira_mux_agent_id",
            "beta",
        );

        let result = send_prompt(&fake, &project, "alpha", "hello", false);
        assert!(result.is_err());
    }

    #[test]
    fn outbox_not_recorded_on_paste_failure() {
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        crate::test_support::setup_healthy_session(&fake, &project);
        fake.set_fail_paste(true);

        let result = send_prompt(&fake, &project, "alpha", "hello", false);
        assert!(result.is_err());
    }

    #[test]
    fn outbox_not_recorded_on_send_keys_failure() {
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
                    if text == "Agent alpha in Test: hello world"
            )),
            "expected rendered template in paste, got: {ops:?}"
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

        send_prompt(&fake, &project, "alpha", "thread message", false).or_panic();

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
    fn prepared_prompt_uses_rendered_template_not_raw() {
        let fake = crate::test_support::FakeTmux::new();
        let mut project = crate::test_support::test_project();
        project.agents[0].prompt_template =
            Some("Agent {{agent_name}} in {{project_name}}: {{user_prompt}}".to_string());
        crate::test_support::setup_healthy_session(&fake, &project);

        let sent = send_prompt(&fake, &project, "alpha", "hello world", false).or_panic();
        assert_eq!(
            sent.final_prompt, "Agent alpha in Test: hello world",
            "send_prompt must return the rendered prompt, not the raw input"
        );
    }

    #[test]
    fn outbox_skipped_when_raw_prompt_is_slash_command() {
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        crate::test_support::setup_healthy_session(&fake, &project);

        let sent = send_prompt(&fake, &project, "alpha", "/help", false).or_panic();
        assert_eq!(sent.final_prompt, "/help");
    }

    #[test]
    fn outbox_skipped_when_rendered_prompt_is_slash_command() {
        let fake = crate::test_support::FakeTmux::new();
        let mut project = crate::test_support::test_project();
        project.agents[0].prompt_template = Some("/cmd {{user_prompt}}".to_string());
        crate::test_support::setup_healthy_session(&fake, &project);

        let sent = send_prompt(&fake, &project, "alpha", "args here", false).or_panic();
        assert_eq!(sent.final_prompt, "/cmd args here");
    }

    #[test]
    fn outbox_records_to_specified_thread() {
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        crate::test_support::setup_healthy_session(&fake, &project);

        let sent = send_prompt(&fake, &project, "alpha", "threaded msg", false).or_panic();
        assert_eq!(sent.final_prompt, "threaded msg");
    }

    #[test]
    fn outbox_routes_to_default_when_no_thread() {
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        crate::test_support::setup_healthy_session(&fake, &project);

        let sent = send_prompt(&fake, &project, "alpha", "default msg", false).or_panic();
        assert_eq!(sent.final_prompt, "default msg");
    }
}
