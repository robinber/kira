//! Create panes, apply layout, and launch agent commands into a session.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::config::{AgentMode, Layout};
use crate::model::{ResolvedAgent, ResolvedProject};
use crate::tmux::metadata::{PANE_AGENT_COMMAND, PANE_COMMAND_SHELL};
use crate::tmux::{TmuxAdapter, TmuxError};

/// How long to watch a pane after `respawn-pane` for an immediate exit.
///
/// Short enough that interactive tools still initializing are not treated as
/// failed; long enough to catch missing binaries and commands that exit on
/// the first tick (the #13 false-success case).
const POST_LAUNCH_HEALTH_WINDOW: Duration = Duration::from_millis(400);
const POST_LAUNCH_HEALTH_POLL: Duration = Duration::from_millis(50);

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
            .map(|cmd| crate::agent_io::command_basename(cmd).to_string()),
        AgentMode::Shell => agent
            .shell_command
            .as_ref()
            .map(|_| PANE_COMMAND_SHELL.to_string()),
    }
}

/// Respawn the pane with the agent's command and tag its metadata. No
/// health verification here — callers batch that over one shared window
/// via [`verify_panes_survived_launch`].
pub(super) fn respawn_agent(
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

/// Poll every target's `pane_dead` over one shared bounded window.
///
/// Success for a pane means its process was still alive at the end of the
/// window — not that the agent is "ready" for prompts. Immediate exits
/// (missing binary, `exit 1`, crash on start) come back as per-agent
/// failures so callers can map them to the degraded exit code. The window
/// is shared because the checks are independent: `start` latency stays
/// flat in agent count instead of paying the window per agent.
pub(super) fn verify_panes_survived_launch<'a>(
    tmux: &dyn TmuxAdapter,
    targets: &[(&'a str, &'a ResolvedAgent)],
) -> Vec<(&'a ResolvedAgent, anyhow::Error)> {
    let mut pending: Vec<(&str, &ResolvedAgent)> = targets.to_vec();
    let mut failures = Vec::new();
    let deadline = Instant::now() + POST_LAUNCH_HEALTH_WINDOW;
    loop {
        let mut still_alive = Vec::with_capacity(pending.len());
        for (pane_id, agent) in pending {
            match pane_is_dead(tmux, pane_id) {
                Ok(false) => still_alive.push((pane_id, agent)),
                Ok(true) => failures.push((
                    agent,
                    anyhow::anyhow!("agent '{}' exited immediately after launch", agent.id),
                )),
                Err(error) => failures.push((agent, error)),
            }
        }
        pending = still_alive;
        let now = Instant::now();
        if pending.is_empty() || now >= deadline {
            return failures;
        }
        std::thread::sleep(POST_LAUNCH_HEALTH_POLL.min(deadline - now));
    }
}

/// A pane that vanished after a successful respawn means the process exited
/// and tmux reaped it, so it counts as dead — the same
/// `is_target_unavailable` classification `agent_io::wait` uses.
fn pane_is_dead(tmux: &dyn TmuxAdapter, pane_id: &str) -> Result<bool> {
    match tmux.list_panes(pane_id) {
        Ok(panes) => panes
            .iter()
            .find(|pane| pane.pane_id == pane_id)
            .map(|pane| pane.pane_dead)
            .with_context(|| format!("pane {pane_id} missing after launch")),
        Err(error) if TmuxError::is_target_unavailable(&error) => Ok(true),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use tracing_subscriber::fmt::MakeWriter;

    use super::TopologyGuard;
    use crate::model::ResolvedProject;
    use crate::test_support::{FakeOp, FakeTmux, TestResultExt, test_project};
    use crate::tmux::TmuxAdapter;

    #[derive(Clone, Default)]
    struct SharedLogBuffer(Arc<Mutex<Vec<u8>>>);

    impl SharedLogBuffer {
        fn contents(&self) -> String {
            String::from_utf8(self.0.lock().or_panic("contents").clone()).or_panic("contents")
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
            self.0.lock().or_panic("write").extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn minimal_project() -> ResolvedProject {
        let mut project = test_project();
        let mut coder = crate::test_support::test_agent("coder");
        coder.command = Some("codex".to_string());
        coder.args = vec!["--profile".to_string(), "fast".to_string()];
        coder.cwd = PathBuf::from("/tmp");
        project.agents = vec![coder];
        project
    }

    #[test]
    fn launch_agent_respawns_with_command_and_args() {
        let fake = FakeTmux::new();
        fake.add_session("s");
        fake.add_window("s", "agents");
        fake.add_pane("s", "agents", "%0", false);
        let project = minimal_project();
        let agent = &project.agents[0];

        super::respawn_agent(&fake, "%0", &project, agent)
            .or_panic("launch_agent_respawns_with_command_and_args");

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
    fn launch_agent_fails_when_process_exits_immediately() {
        let fake = FakeTmux::new();
        fake.add_session("s");
        fake.add_window("s", "agents");
        fake.add_pane("s", "agents", "%0", false);
        fake.set_respawn_exits_immediately(true);
        let project = minimal_project();
        let agent = &project.agents[0];

        super::respawn_agent(&fake, "%0", &project, agent)
            .or_panic("launch_agent_fails_when_process_exits_immediately: respawn");
        let failures = super::verify_panes_survived_launch(&fake, &[("%0", agent)]);
        assert_eq!(failures.len(), 1, "the dead pane must fail verification");
        assert!(
            failures[0]
                .1
                .to_string()
                .contains("exited immediately after launch"),
            "got: {}",
            failures[0].1
        );
    }

    #[test]
    fn health_window_is_shared_across_live_panes() {
        // Discriminates batching from the old serial verify: two live
        // panes must cost one window, not one per pane.
        let fake = FakeTmux::new();
        fake.add_session("s");
        fake.add_window("s", "agents");
        fake.add_pane("s", "agents", "%0", false);
        fake.add_pane("s", "agents", "%1", false);
        let project = test_project();

        let started = std::time::Instant::now();
        let failures = super::verify_panes_survived_launch(
            &fake,
            &[("%0", &project.agents[0]), ("%1", &project.agents[1])],
        );
        let elapsed = started.elapsed();

        assert!(failures.is_empty(), "live panes must pass verification");
        assert!(
            elapsed >= super::POST_LAUNCH_HEALTH_WINDOW,
            "the full window must elapse for live panes, got {elapsed:?}"
        );
        assert!(
            elapsed < super::POST_LAUNCH_HEALTH_WINDOW + std::time::Duration::from_millis(250),
            "two live panes must share one window, not pay one each, got {elapsed:?}"
        );
    }

    #[test]
    fn shared_health_window_reports_only_the_dead_panes() {
        let fake = FakeTmux::new();
        fake.add_session("s");
        fake.add_window("s", "agents");
        fake.add_pane("s", "agents", "%0", false);
        fake.add_pane("s", "agents", "%1", true);
        let mut project = test_project();
        project.agents[1].id = "beta".to_string();

        let failures = super::verify_panes_survived_launch(
            &fake,
            &[("%0", &project.agents[0]), ("%1", &project.agents[1])],
        );

        assert_eq!(failures.len(), 1, "only the dead pane fails");
        assert_eq!(failures[0].0.id, "beta");
    }

    #[test]
    fn pane_is_dead_errors_when_target_missing_from_listing() {
        let fake = FakeTmux::new();
        fake.add_session("s");
        fake.add_window("s", "agents");
        fake.add_pane("s", "agents", "%0", true);

        // A window target is the only way to reach the defensive branch (a
        // listing that succeeds without the target id): real tmux and
        // FakeTmux both error for a vanished %id target. The branch must
        // error rather than read a sibling's liveness.
        let error = super::pane_is_dead(&fake, "s:agents")
            .err_or_panic("pane_is_dead_errors_when_target_missing_from_listing: expected Err");
        assert!(
            error.to_string().contains("missing after launch"),
            "got: {error}"
        );
    }

    #[test]
    fn pane_is_dead_treats_vanished_pane_as_dead() {
        let fake = FakeTmux::new();
        fake.add_session("s");
        fake.add_window("s", "agents");
        fake.add_pane("s", "agents", "%0", false);

        let dead =
            super::pane_is_dead(&fake, "%9").or_panic("pane_is_dead_treats_vanished_pane_as_dead");
        assert!(
            dead,
            "vanished pane must count as dead, not a transport error"
        );
    }

    #[test]
    fn topology_guard_logs_failure_reason_on_rollback() {
        let fake = FakeTmux::new();
        fake.add_session("kira-test");
        let logs = SharedLogBuffer::default();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .with_writer(logs.clone())
            .without_time()
            .finish();

        tracing::subscriber::with_default(subscriber, || {
            let mut guard = TopologyGuard::new(&fake, "kira-test");
            guard.mark_failed("simulated topology failure");
        });

        assert!(
            !fake
                .session_exists("kira-test")
                .or_panic("topology_guard_logs_failure_reason_on_rollback")
        );

        let output = logs.contents();
        assert!(output.contains("ERROR"));
        assert!(output.contains("rolling back partial session after topology failure"));
        assert!(output.contains("reason="));
        assert!(output.contains("simulated topology failure"));
    }
}
