use anyhow::{Context, Result};

use crate::config::{AgentMode, Layout};
use crate::model::{ResolvedAgent, ResolvedProject};
use crate::tmux::TmuxAdapter;
use crate::tmux::metadata::{PANE_AGENT_COMMAND, PANE_COMMAND_SHELL};

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
    // One decision point per layout: the tmux layout preset plus the
    // main-pane option (if any) that must be set before applying it.
    let (layout, main_pane_option) = match project.layout {
        Layout::Auto => match project.agents.len() {
            0 | 1 => (None, None),
            2 => (Some("even-horizontal"), None),
            3 => (Some("main-vertical"), Some("main-pane-width")),
            _ => (Some("tiled"), None),
        },
        Layout::SideBySide => (Some("even-horizontal"), None),
        Layout::Stacked => (Some("even-vertical"), None),
        Layout::MainLeft => (Some("main-vertical"), Some("main-pane-width")),
        Layout::MainTop => (Some("main-horizontal"), Some("main-pane-height")),
        Layout::Grid => (Some("tiled"), None),
    };

    if let Some(option) = main_pane_option {
        tmux.set_window_option(
            window_target,
            option,
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
            .map(|_| PANE_COMMAND_SHELL.to_string()),
    }
}

pub(super) fn launch_agent(
    tmux: &dyn TmuxAdapter,
    pane_id: &str,
    project: &ResolvedProject,
    agent: &ResolvedAgent,
) -> Result<()> {
    let env_overrides = agent
        .env
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect::<Vec<_>>();
    let command = match agent.mode {
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

    tracing::debug!(
        project_id = project.id.as_str(),
        agent_id = agent.id.as_str(),
        pane_id,
        cwd = %agent.cwd.display(),
        // Field expressions are only evaluated when DEBUG is enabled, so the
        // redaction pass costs nothing on the default WARN level.
        env = ?env_overrides
            .iter()
            .map(|(key, value)| crate::logging::redact_env_value(key, value))
            .collect::<Vec<_>>(),
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

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::io;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use tracing_subscriber::fmt::MakeWriter;

    use super::TopologyGuard;
    use crate::config::{AgentMode, Layout, RemainOnExit};
    use crate::model::{ResolvedAgent, ResolvedProject};
    use crate::test_support::{FakeOp, FakeTmux, TestResultExt};
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

    fn direct_agent() -> ResolvedAgent {
        ResolvedAgent {
            id: "coder".to_string(),
            label: "Coder".to_string(),
            mode: AgentMode::Direct,
            command: Some("codex".to_string()),
            shell_command: None,
            args: vec!["--profile".to_string(), "fast".to_string()],
            cwd: PathBuf::from("/tmp"),
            env: BTreeMap::new(),
            capabilities: vec![],
            prompt_template: None,
        }
    }

    fn minimal_project() -> ResolvedProject {
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
        }
    }

    #[test]
    fn launch_agent_respawns_with_command_and_args() {
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
            ]
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
}
