use std::time::Duration;

use anyhow::{Context, Result};

use crate::agent_io::{SubmitBehavior, infer_submit_behavior};
use crate::config::{AgentMode, Layout};
use crate::domain::{ResolvedAgent, ResolvedProject};
use crate::tmux::TmuxAdapter;
use crate::tmux::metadata::PANE_AGENT_COMMAND;

pub(super) struct TopologyGuard<'a> {
    tmux: &'a dyn TmuxAdapter,
    session: String,
    committed: bool,
    failure_reason: Option<String>,
}

impl<'a> TopologyGuard<'a> {
    pub(super) fn new(tmux: &'a dyn TmuxAdapter, session: &str) -> Self {
        Self {
            tmux,
            session: session.to_string(),
            committed: false,
            failure_reason: None,
        }
    }

    pub(super) fn mark_failed(&mut self, reason: impl Into<String>) {
        self.failure_reason = Some(reason.into());
    }

    pub(super) fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for TopologyGuard<'_> {
    fn drop(&mut self) {
        if !self.committed {
            if let Some(reason) = &self.failure_reason {
                tracing::error!(
                    session = %self.session,
                    reason = %reason,
                    "rolling back partial session after topology failure"
                );
            } else {
                tracing::warn!(
                    session = %self.session,
                    "rolling back partial session after topology failure"
                );
            }
            let _ = self.tmux.kill_session(&self.session);
        }
    }
}

pub(super) fn apply_layout(
    tmux: &dyn TmuxAdapter,
    project: &ResolvedProject,
    window_target: &str,
) -> Result<()> {
    let layout = match project.layout {
        Layout::Auto => match project.agents.len() {
            0 | 1 => None,
            2 => Some("even-horizontal"),
            3 => Some("main-vertical"),
            _ => Some("tiled"),
        },
        Layout::SideBySide => Some("even-horizontal"),
        Layout::Stacked => Some("even-vertical"),
        Layout::MainLeft => Some("main-vertical"),
        Layout::MainTop => Some("main-horizontal"),
        Layout::Grid => Some("tiled"),
    };

    if matches!(project.layout, Layout::MainLeft)
        || matches!(project.layout, Layout::Auto) && project.agents.len() == 3
    {
        tmux.set_window_option(
            window_target,
            "main-pane-width",
            &format!("{}%", project.main_pane_ratio),
        )?;
    }

    if matches!(project.layout, Layout::MainTop) {
        tmux.set_window_option(
            window_target,
            "main-pane-height",
            &format!("{}%", project.main_pane_ratio),
        )?;
    }

    if let Some(layout) = layout {
        tmux.select_layout(window_target, layout)?;
    }

    Ok(())
}

fn agent_command_basename(agent: &ResolvedAgent) -> Option<String> {
    match agent.mode {
        AgentMode::Direct => agent
            .command
            .as_ref()
            .map(|cmd| cmd.rsplit('/').next().unwrap_or(cmd).to_string()),
        AgentMode::Shell => agent
            .shell_command
            .as_ref()
            .map(|_| "__shell__".to_string()),
    }
}

pub(super) fn launch_agent(
    tmux: &dyn TmuxAdapter,
    pane_id: &str,
    project: &ResolvedProject,
    agent: &ResolvedAgent,
) -> Result<()> {
    launch_agent_inner(tmux, pane_id, project, agent, None)
}

/// Launch an agent pane with an extra argument appended to the command.
///
/// Only direct-mode agents are supported; shell-mode agents are rejected
/// because the orchestrator prompt cannot be safely injected into an
/// arbitrary shell command string.
#[cfg(test)]
pub(super) fn launch_agent_with_extra_arg(
    tmux: &dyn TmuxAdapter,
    pane_id: &str,
    project: &ResolvedProject,
    agent: &ResolvedAgent,
    extra_arg: &str,
) -> Result<()> {
    if agent.mode == AgentMode::Shell {
        return Err(crate::error::AiMuxError::OrchestratorShellModeUnsupported {
            agent_id: agent.id.clone(),
        }
        .into());
    }
    launch_agent_inner(tmux, pane_id, project, agent, Some(extra_arg))
}

/// Prompts whose byte length **exceeds** this threshold are delivered via
/// `paste_then_submit_text` after respawn; shorter prompts travel as a
/// trailing argv argument.
const ORCH_PROMPT_PASTE_THRESHOLD_BYTES: usize = 4 * 1024;

fn launch_agent_inner(
    tmux: &dyn TmuxAdapter,
    pane_id: &str,
    project: &ResolvedProject,
    agent: &ResolvedAgent,
    extra_arg: Option<&str>,
) -> Result<()> {
    let env_overrides = agent
        .env
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect::<Vec<_>>();
    let redacted_env = env_overrides
        .iter()
        .map(|(key, value)| crate::logging::redact_env_value(key, value))
        .collect::<Vec<_>>();
    let mut command = match agent.mode {
        AgentMode::Direct => {
            let mut parts = vec![agent.command.clone().context("missing agent command")?];
            parts.extend(agent.args.clone());
            parts
        }
        AgentMode::Shell => vec![
            project.default_shell.clone(),
            "-c".to_string(),
            agent
                .shell_command
                .clone()
                .context("missing shell command")?,
        ],
    };

    let oversize_prompt = extra_arg.filter(|arg| arg.len() > ORCH_PROMPT_PASTE_THRESHOLD_BYTES);
    let inline_prompt = if oversize_prompt.is_some() {
        None
    } else {
        extra_arg.filter(|arg| !arg.is_empty())
    };

    if let Some(arg) = inline_prompt {
        command.push(arg.to_string());
    }

    let prompt_delivery = if oversize_prompt.is_some() {
        "post_launch_paste"
    } else {
        "argv_inline"
    };

    tracing::debug!(
        project_id = project.id.as_str(),
        agent_id = agent.id.as_str(),
        pane_id,
        cwd = %agent.cwd.display(),
        env = ?redacted_env,
        prompt_delivery,
        "launching agent pane"
    );

    tmux.respawn_pane(
        pane_id,
        &agent.cwd.display().to_string(),
        &env_overrides,
        &command,
    )?;

    if let Some(basename) = agent_command_basename(agent) {
        tmux.set_pane_option(pane_id, PANE_AGENT_COMMAND, &basename)?;
    }

    if let Some(prompt) = oversize_prompt {
        crate::tmux::paste_then_submit_text(tmux, pane_id, prompt)?;

        // `paste_then_submit_text` always sends exactly one Enter; codex /
        // claude / qwen / opencode TUIs need a second Enter to actually
        // submit. Mirror the policy that `agent_io::send::paste_and_submit`
        // applies on the regular send path so oversize launches are not
        // left with the prompt sitting unsubmitted in the input field.
        let pane_command = tmux.get_pane_option(pane_id, PANE_AGENT_COMMAND)?;
        if infer_submit_behavior(agent, pane_command.as_deref()) == SubmitBehavior::DoubleEnter {
            std::thread::sleep(Duration::from_millis(200));
            tmux.send_keys(pane_id, &["Enter"])?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::{Arc, Mutex};

    use tracing_subscriber::fmt::MakeWriter;

    use super::{ORCH_PROMPT_PASTE_THRESHOLD_BYTES, TopologyGuard};
    use crate::test_support::{FakeTmux, TestOptionExt, TestResultExt};
    use crate::tmux::TmuxAdapter;

    #[derive(Clone, Default)]
    struct SharedLogBuffer(Arc<Mutex<Vec<u8>>>);

    impl SharedLogBuffer {
        fn contents(&self) -> String {
            String::from_utf8(self.0.lock().or_panic().clone()).or_panic()
        }
    }

    impl<'a> MakeWriter<'a> for SharedLogBuffer {
        type Writer = SharedLogWriter;

        fn make_writer(&'a self) -> Self::Writer {
            SharedLogWriter(Arc::clone(&self.0))
        }
    }

    struct SharedLogWriter(Arc<Mutex<Vec<u8>>>);

    impl io::Write for SharedLogWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().or_panic().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use crate::config::AgentMode;
    use crate::domain::{ResolvedAgent, ResolvedProject};
    use crate::error::AiMuxError;
    use crate::test_support::FakeOp;

    fn direct_agent() -> ResolvedAgent {
        ResolvedAgent {
            id: "orch-1".to_string(),
            label: "Orch".to_string(),
            mode: AgentMode::Direct,
            command: Some("codex".to_string()),
            shell_command: None,
            args: vec!["--profile".to_string(), "fast".to_string()],
            cwd: PathBuf::from("/tmp"),
            env: BTreeMap::new(),
            capabilities: vec!["orchestrator".to_string()],
            prompt_template: None,
            orchestrator_prompt_template: None,
        }
    }

    fn minimal_project() -> ResolvedProject {
        use crate::config::{Layout, RemainOnExit};
        ResolvedProject {
            id: "test".to_string(),
            profile_id: "default".to_string(),
            name: "Test".to_string(),
            root: PathBuf::from("/tmp/test-project"),
            layout: Layout::Auto,
            main_pane_ratio: 50,
            window_name: "agents".to_string(),
            session_prefix: "ai".to_string(),
            default_shell: "/bin/sh".to_string(),
            remain_on_exit: RemainOnExit::Failed,
            tmux_bin: "tmux".to_string(),
            agents: vec![direct_agent()],
            fingerprint: "abc123".to_string(),
            groups: BTreeMap::new(),
            orchestration: None,
        }
    }

    #[test]
    fn launch_with_extra_arg_appends_to_command() {
        let fake = FakeTmux::new();
        let project = minimal_project();
        let agent = &project.agents[0];

        super::launch_agent_with_extra_arg(&fake, "%0", &project, agent, "hello orchestrator")
            .or_panic();

        let ops = fake.ops();
        let Some(FakeOp::RespawnPane { command, .. }) = ops
            .iter()
            .find(|op| matches!(op, FakeOp::RespawnPane { .. }))
        else {
            panic!("expected a RespawnPane op");
        };
        assert_eq!(
            command,
            &vec![
                "codex".to_string(),
                "--profile".to_string(),
                "fast".to_string(),
                "hello orchestrator".to_string(),
            ]
        );
    }

    #[test]
    fn launch_with_extra_arg_rejects_shell_mode() {
        let fake = FakeTmux::new();
        let project = minimal_project();
        let mut agent = direct_agent();
        agent.mode = AgentMode::Shell;
        agent.command = None;
        agent.shell_command = Some("codex --full-auto".to_string());

        let err = super::launch_agent_with_extra_arg(&fake, "%0", &project, &agent, "hello")
            .err_or_panic();
        assert!(
            matches!(
                err.downcast_ref::<AiMuxError>(),
                Some(AiMuxError::OrchestratorShellModeUnsupported { .. })
            ),
            "expected OrchestratorShellModeUnsupported, got: {err:?}"
        );
    }

    #[test]
    fn launch_agent_normal_does_not_append_extra_arg() {
        let fake = FakeTmux::new();
        let project = minimal_project();
        let agent = &project.agents[0];

        super::launch_agent(&fake, "%0", &project, agent).or_panic();

        let ops = fake.ops();
        let Some(FakeOp::RespawnPane { command, .. }) = ops
            .iter()
            .find(|op| matches!(op, FakeOp::RespawnPane { .. }))
        else {
            panic!("expected a RespawnPane op");
        };
        assert_eq!(
            command,
            &vec![
                "codex".to_string(),
                "--profile".to_string(),
                "fast".to_string(),
            ],
            "normal launch_agent must not append extra args"
        );
    }

    #[test]
    fn topology_guard_logs_failure_reason_on_rollback() {
        let fake = FakeTmux::new();
        fake.add_session("ai-test");
        let logs = SharedLogBuffer::default();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .with_writer(logs.clone())
            .without_time()
            .finish();

        tracing::subscriber::with_default(subscriber, || {
            let mut guard = TopologyGuard::new(&fake, "ai-test");
            guard.mark_failed("simulated topology failure");
        });

        assert!(!fake.session_exists("ai-test").or_panic());

        let output = logs.contents();
        assert!(output.contains("ERROR"));
        assert!(output.contains("rolling back partial session after topology failure"));
        assert!(output.contains("reason="));
        assert!(output.contains("simulated topology failure"));
    }

    #[test]
    fn launch_with_small_extra_arg_uses_argv_path() {
        let fake = FakeTmux::new();
        let project = minimal_project();
        let agent = &project.agents[0];
        let small_prompt = "x".repeat(100);

        super::launch_agent_with_extra_arg(&fake, "%0", &project, agent, &small_prompt).or_panic();

        let ops = fake.ops();
        let respawn = ops
            .iter()
            .find_map(|op| match op {
                FakeOp::RespawnPane { command, .. } => Some(command.clone()),
                _ => None,
            })
            .or_panic();
        assert!(
            respawn.last().is_some_and(|s| s == &small_prompt),
            "small prompt must travel via argv: {respawn:?}"
        );
        assert!(
            !ops.iter().any(|op| matches!(op, FakeOp::PasteText { .. })),
            "no paste expected for small prompt"
        );
    }

    #[test]
    fn launch_with_extra_arg_at_threshold_uses_argv_path() {
        let fake = FakeTmux::new();
        let project = minimal_project();
        let agent = &project.agents[0];
        let prompt = "x".repeat(ORCH_PROMPT_PASTE_THRESHOLD_BYTES);

        super::launch_agent_with_extra_arg(&fake, "%0", &project, agent, &prompt).or_panic();

        let ops = fake.ops();
        let respawn = ops
            .iter()
            .find_map(|op| match op {
                FakeOp::RespawnPane { command, .. } => Some(command.clone()),
                _ => None,
            })
            .or_panic();
        assert!(
            respawn.last().is_some_and(|s| s == &prompt),
            "exactly-at-threshold prompt must still go via argv (boundary is `>` not `>=`)"
        );
        assert!(
            !ops.iter().any(|op| matches!(op, FakeOp::PasteText { .. })),
            "no paste expected at the threshold boundary"
        );
    }

    #[test]
    fn launch_with_oversize_extra_arg_pastes_after_spawn() {
        let fake = FakeTmux::new();
        let project = minimal_project();
        let agent = &project.agents[0];
        let oversize = "x".repeat(ORCH_PROMPT_PASTE_THRESHOLD_BYTES + 1);

        super::launch_agent_with_extra_arg(&fake, "%0", &project, agent, &oversize).or_panic();

        let ops = fake.ops();

        let respawn = ops
            .iter()
            .find_map(|op| match op {
                FakeOp::RespawnPane { command, .. } => Some(command.clone()),
                _ => None,
            })
            .or_panic();
        assert_eq!(
            respawn,
            vec![
                "codex".to_string(),
                "--profile".to_string(),
                "fast".to_string()
            ],
            "oversize prompt must NOT travel via argv"
        );

        let paste_idx = ops
            .iter()
            .position(|op| matches!(op, FakeOp::PasteText { text, .. } if text == &oversize))
            .or_panic();
        let respawn_idx = ops
            .iter()
            .position(|op| matches!(op, FakeOp::RespawnPane { .. }))
            .or_panic();
        assert!(
            respawn_idx < paste_idx,
            "respawn must precede paste (respawn={respawn_idx}, paste={paste_idx})"
        );

        assert!(
            ops.iter()
                .any(|op| matches!(op, FakeOp::SendKeys { keys, .. } if keys == &vec!["Enter".to_string()])),
            "oversize path must submit Enter after paste"
        );
    }

    #[test]
    fn launch_with_oversize_extra_arg_sends_double_enter_for_codex_family() {
        let fake = FakeTmux::new();
        let project = minimal_project();
        let agent = &project.agents[0];
        let oversize = "x".repeat(ORCH_PROMPT_PASTE_THRESHOLD_BYTES + 1);

        super::launch_agent_with_extra_arg(&fake, "%0", &project, agent, &oversize).or_panic();

        let ops = fake.ops();
        let enter_indices: Vec<usize> = ops
            .iter()
            .enumerate()
            .filter_map(|(i, op)| match op {
                FakeOp::SendKeys { keys, .. } if keys == &vec!["Enter".to_string()] => Some(i),
                _ => None,
            })
            .collect();
        assert_eq!(
            enter_indices.len(),
            2,
            "codex-family oversize launch must send DoubleEnter; got ops: {ops:?}"
        );
    }

    #[test]
    fn launch_oversize_with_shell_mode_still_rejected_before_size_check() {
        let fake = FakeTmux::new();
        let project = minimal_project();
        let mut agent = direct_agent();
        agent.mode = AgentMode::Shell;
        agent.command = None;
        agent.shell_command = Some("codex --full-auto".to_string());
        let oversize = "x".repeat(ORCH_PROMPT_PASTE_THRESHOLD_BYTES + 1);

        let err = super::launch_agent_with_extra_arg(&fake, "%0", &project, &agent, &oversize)
            .err_or_panic();
        assert!(
            matches!(
                err.downcast_ref::<AiMuxError>(),
                Some(AiMuxError::OrchestratorShellModeUnsupported { .. })
            ),
            "shell-mode rejection must fire before any size check: {err:?}"
        );
    }
}
