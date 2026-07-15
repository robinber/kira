use anyhow::{Result, bail};

use super::launch::{TopologyGuard, apply_layout, launch_agent};
use super::{session_name, window_target};
use crate::config::ConfigError;
use crate::error::KiraMuxError;
use crate::inspector::{self, ManagedPane, WorkspaceTopology};
use crate::model::ResolvedProject;
use crate::tmux::TmuxAdapter;
use crate::tmux::metadata::{
    PANE_AGENT_ID, SESSION_CONFIG_FINGERPRINT, SESSION_PROFILE_ID, SESSION_PROJECT_ID, WINDOW_ROLE,
    WINDOW_ROLE_AGENTS,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StartOutcome {
    Healthy,
    Degraded,
}

pub(crate) fn start(
    tmux: &dyn TmuxAdapter,
    project: &ResolvedProject,
    attach_after: bool,
) -> Result<StartOutcome> {
    let session = session_name(project);
    tracing::debug!(
        project_id = project.id.as_str(),
        session,
        "starting workspace"
    );

    let outcome = match inspector::inspect(tmux, project)? {
        WorkspaceTopology::Absent => create(tmux, project, &session)?,
        WorkspaceTopology::Healthy(_) => StartOutcome::Healthy,
        WorkspaceTopology::Degraded(workspace) => repair(tmux, project, &workspace.panes),
        WorkspaceTopology::Drifted { reason } => {
            return Err(KiraMuxError::Drifted {
                project_id: project.id.clone(),
                reason,
            }
            .into());
        }
    };

    if attach_after {
        attach_to_session(tmux, &session)?;
    }

    Ok(outcome)
}

pub(crate) fn attach(tmux: &dyn TmuxAdapter, project: &ResolvedProject) -> Result<()> {
    let session = session_name(project);
    tracing::debug!(
        project_id = project.id.as_str(),
        session,
        "attaching workspace"
    );

    if !inspector::session_exists(tmux, &session)? {
        return Err(KiraMuxError::SessionAbsent.into());
    }

    attach_to_session(tmux, &session)
}

pub(crate) fn restart(
    tmux: &dyn TmuxAdapter,
    project: &ResolvedProject,
    agent_id: Option<&str>,
) -> Result<()> {
    let session = session_name(project);
    tracing::debug!(
        project_id = project.id.as_str(),
        session,
        agent_id,
        "restarting workspace panes"
    );

    let panes = match inspector::inspect(tmux, project)? {
        WorkspaceTopology::Absent => return Err(KiraMuxError::SessionAbsent.into()),
        WorkspaceTopology::Healthy(w) | WorkspaceTopology::Degraded(w) => w.panes,
        WorkspaceTopology::Drifted { reason } => {
            return Err(KiraMuxError::Drifted {
                project_id: project.id.clone(),
                reason,
            }
            .into());
        }
    };

    restart_managed_panes(tmux, project, &panes, agent_id)
}

pub(crate) fn kill(tmux: &dyn TmuxAdapter, project: &ResolvedProject) -> Result<()> {
    let session = session_name(project);
    tracing::debug!(
        project_id = project.id.as_str(),
        session,
        "killing workspace"
    );

    if !inspector::session_exists(tmux, &session)? {
        return Ok(());
    }

    if let Err(error) = tmux.kill_session(&session) {
        // The session may have died between the existence check and the
        // kill; the goal is reached either way.
        if inspector::session_exists(tmux, &session)? {
            return Err(error);
        }
    }
    Ok(())
}

fn attach_to_session(tmux: &dyn TmuxAdapter, session: &str) -> Result<()> {
    if std::env::var_os("TMUX").is_some() {
        tmux.switch_client(session)?;
    } else {
        tmux.attach_session(session)?;
    }
    Ok(())
}

fn create(
    tmux: &dyn TmuxAdapter,
    project: &ResolvedProject,
    session: &str,
) -> Result<StartOutcome> {
    // Existence is launch's concern, not resolution's: kill/status must keep
    // working on a project whose directory was deleted, but launching into a
    // missing root would only produce broken panes.
    if !project.root.is_dir() {
        return Err(
            KiraMuxError::ConfigValidation(ConfigError::ProjectRootNotFound(project.root.clone()))
                .into(),
        );
    }

    for agent in &project.agents {
        if !agent.cwd.is_dir() {
            let validation = if agent.cwd.exists() {
                ConfigError::AgentCwdNotDirectory {
                    agent_id: agent.id.clone(),
                    path: agent.cwd.clone(),
                }
            } else {
                ConfigError::AgentCwdNotFound {
                    agent_id: agent.id.clone(),
                    path: agent.cwd.clone(),
                }
            };
            return Err(KiraMuxError::ConfigValidation(validation).into());
        }
    }

    let root = project.root.display().to_string();
    let window_target = window_target(session, &project.window_name);

    tmux.create_detached_session(session, &root, &project.window_name, project.agents.len())?;
    let mut guard = TopologyGuard::new(tmux, session);
    let setup = (|| -> Result<Vec<crate::tmux::PaneInfo>> {
        tmux.set_session_option(session, SESSION_PROJECT_ID, &project.id)?;
        tmux.set_session_option(session, SESSION_PROFILE_ID, &project.profile_id)?;
        tmux.set_session_option(session, SESSION_CONFIG_FINGERPRINT, &project.fingerprint)?;
        tmux.set_window_option(&window_target, WINDOW_ROLE, WINDOW_ROLE_AGENTS)?;
        tmux.set_window_option(
            &window_target,
            "remain-on-exit",
            project.remain_on_exit.as_str(),
        )?;

        let existing = tmux.list_panes(&window_target)?.len();
        for _ in existing..project.agents.len() {
            tmux.split_window(&window_target, &root)?;
            tmux.select_layout(&window_target, "even-vertical")?;
        }

        let panes = tmux.list_panes(&window_target)?;
        if panes.len() != project.agents.len() {
            bail!(
                "expected {} panes after window setup, found {}",
                project.agents.len(),
                panes.len()
            );
        }
        for (pane, agent) in panes.iter().zip(project.agents.iter()) {
            tmux.set_pane_option(&pane.pane_id, PANE_AGENT_ID, &agent.id)?;
        }

        apply_layout(tmux, project, &window_target)?;

        Ok(panes)
    })();
    let panes = match setup {
        Ok(panes) => panes,
        Err(error) => {
            guard.mark_failed(error.to_string());
            return Err(error);
        }
    };

    guard.commit();

    let mut any_launch_failed = false;
    for (pane, agent) in panes.iter().zip(project.agents.iter()) {
        let launch_result = launch_agent(tmux, pane.pane_id.as_str(), project, agent);
        if let Err(error) = launch_result {
            tracing::warn!(
                project_id = project.id.as_str(),
                agent_id = agent.id.as_str(),
                %error,
                "agent launch failed, workspace will be degraded"
            );
            any_launch_failed = true;
        }
    }

    if any_launch_failed {
        Ok(StartOutcome::Degraded)
    } else {
        Ok(StartOutcome::Healthy)
    }
}

fn repair(
    tmux: &dyn TmuxAdapter,
    project: &ResolvedProject,
    panes: &[ManagedPane],
) -> StartOutcome {
    let mut any_launch_failed = false;
    for managed in panes {
        if managed.pane.pane_dead {
            let launch_result =
                launch_agent(tmux, managed.pane.pane_id.as_str(), project, &managed.agent);
            if let Err(error) = launch_result {
                tracing::warn!(
                    project_id = project.id.as_str(),
                    agent_id = managed.agent.id.as_str(),
                    %error,
                    "agent re-launch failed during repair, workspace remains degraded"
                );
                any_launch_failed = true;
            }
        }
    }

    if any_launch_failed {
        StartOutcome::Degraded
    } else {
        StartOutcome::Healthy
    }
}

fn restart_managed_panes(
    tmux: &dyn TmuxAdapter,
    project: &ResolvedProject,
    panes: &[ManagedPane],
    agent_id: Option<&str>,
) -> Result<()> {
    if let Some(agent_id) = agent_id {
        let managed = panes
            .iter()
            .find(|pane| pane.agent.id == agent_id)
            .ok_or_else(|| KiraMuxError::UnknownAgentId(agent_id.to_string()))?;
        launch_agent(tmux, managed.pane.pane_id.as_str(), project, &managed.agent)?;
        return Ok(());
    }

    // Match create()/repair(): keep going past individual failures and
    // report the workspace as degraded, instead of stopping half-restarted
    // with no signal.
    let mut any_launch_failed = false;
    for managed in panes {
        if let Err(error) =
            launch_agent(tmux, managed.pane.pane_id.as_str(), project, &managed.agent)
        {
            tracing::warn!(
                project_id = project.id.as_str(),
                agent_id = managed.agent.id.as_str(),
                %error,
                "agent restart failed, workspace will be degraded"
            );
            any_launch_failed = true;
        }
    }
    if any_launch_failed {
        return Err(KiraMuxError::Degraded(project.id.clone()).into());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentMode;
    use crate::error::WorkspaceDriftReason;
    use crate::test_support::{FakeTmux, TestResultExt, setup_healthy_session, test_project};
    use crate::workspace::session_name;

    #[test]
    fn start_creates_new_workspace_from_absent() {
        let fake = FakeTmux::new();
        let mut project = test_project();
        project.root = std::env::temp_dir();
        for agent in &mut project.agents {
            agent.cwd = std::env::temp_dir();
        }

        let outcome = start(&fake, &project, false).or_panic();
        assert_eq!(outcome, StartOutcome::Healthy);
        assert!(fake.session_exists(&session_name(&project)).or_panic());
    }

    #[test]
    fn start_reuses_healthy_session() {
        let fake = FakeTmux::new();
        let project = test_project();
        setup_healthy_session(&fake, &project);

        let outcome = start(&fake, &project, false).or_panic();
        assert_eq!(outcome, StartOutcome::Healthy);
    }

    #[test]
    fn start_repairs_degraded_session() {
        let fake = FakeTmux::new();
        let project = test_project();
        crate::test_support::setup_session_with_dead_panes(&fake, &project, &[1]);

        let outcome = start(&fake, &project, false).or_panic();
        assert_eq!(outcome, StartOutcome::Healthy);
    }

    #[test]
    fn start_refuses_drifted_session() {
        let fake = FakeTmux::new();
        let project = test_project();
        let session = session_name(&project);

        fake.add_session(&session);
        fake.set_session_opt(&session, "@kira_mux_config_fingerprint", "wrong");
        fake.set_session_opt(&session, "@kira_mux_project_id", &project.id);

        let err = start(&fake, &project, false).err_or_panic();
        assert!(err.downcast_ref::<KiraMuxError>().is_some());
    }

    #[test]
    fn restart_all_agents_on_healthy_session() {
        let fake = FakeTmux::new();
        let project = test_project();
        setup_healthy_session(&fake, &project);

        restart(&fake, &project, None).or_panic();
    }

    #[test]
    fn restart_all_reports_degraded_after_attempting_every_pane() {
        let fake = FakeTmux::new();
        let project = test_project();
        setup_healthy_session(&fake, &project);
        fake.set_fail_respawn(true);

        let err = restart(&fake, &project, None).err_or_panic();
        assert!(
            matches!(
                err.downcast_ref::<KiraMuxError>(),
                Some(KiraMuxError::Degraded(_))
            ),
            "restart must keep create()/repair() degraded semantics, got: {err}"
        );
    }

    #[test]
    fn restart_single_agent() {
        let fake = FakeTmux::new();
        let project = test_project();
        setup_healthy_session(&fake, &project);

        restart(&fake, &project, Some("alpha")).or_panic();
    }

    #[test]
    fn restart_unknown_agent_fails() {
        let fake = FakeTmux::new();
        let project = test_project();
        setup_healthy_session(&fake, &project);

        let err = restart(&fake, &project, Some("nonexistent")).err_or_panic();
        assert!(matches!(
            err.downcast_ref::<KiraMuxError>(),
            Some(KiraMuxError::UnknownAgentId(_))
        ));
    }

    #[test]
    fn restart_absent_session_fails() {
        let fake = FakeTmux::new();
        let project = test_project();

        let err = restart(&fake, &project, None).err_or_panic();
        assert!(matches!(
            err.downcast_ref::<KiraMuxError>(),
            Some(KiraMuxError::SessionAbsent)
        ));
    }

    #[test]
    fn restart_drifted_session_fails() {
        let fake = FakeTmux::new();
        let project = test_project();
        let session = session_name(&project);

        fake.add_session(&session);
        fake.set_session_opt(&session, "@kira_mux_config_fingerprint", "wrong");
        fake.set_session_opt(&session, "@kira_mux_project_id", &project.id);

        let err = restart(&fake, &project, None).err_or_panic();
        assert!(matches!(
            err.downcast_ref::<KiraMuxError>(),
            Some(KiraMuxError::Drifted {
                reason: WorkspaceDriftReason::FingerprintMismatch,
                ..
            })
        ));
    }

    #[test]
    fn kill_removes_session() {
        let fake = FakeTmux::new();
        let project = test_project();
        setup_healthy_session(&fake, &project);

        kill(&fake, &project).or_panic();
        assert!(!fake.session_exists(&session_name(&project)).or_panic());
    }

    #[test]
    fn kill_absent_session_succeeds() {
        let fake = FakeTmux::new();
        let project = test_project();

        kill(&fake, &project).or_panic();
    }

    #[test]
    fn launch_sets_command_metadata() {
        let fake = FakeTmux::new();
        let mut project = test_project();
        project.root = std::env::temp_dir();
        for agent in &mut project.agents {
            agent.cwd = std::env::temp_dir();
        }

        let outcome = start(&fake, &project, false).or_panic();
        assert_eq!(outcome, StartOutcome::Healthy);

        let val = fake
            .get_pane_option("%0", "@kira_mux_agent_command")
            .or_panic();
        assert_eq!(val.as_deref(), Some("echo"));
    }

    #[test]
    fn launch_sets_path_basename() {
        let fake = FakeTmux::new();
        let mut project = test_project();
        project.root = std::env::temp_dir();
        project.agents[0].command = Some("/usr/bin/codex".to_string());
        for agent in &mut project.agents {
            agent.cwd = std::env::temp_dir();
        }

        start(&fake, &project, false).or_panic();

        let val = fake
            .get_pane_option("%0", "@kira_mux_agent_command")
            .or_panic();
        assert_eq!(val.as_deref(), Some("codex"));
    }

    #[test]
    fn launch_sets_shell_sentinel() {
        let fake = FakeTmux::new();
        let mut project = test_project();
        project.root = std::env::temp_dir();
        project.agents[0].mode = AgentMode::Shell;
        project.agents[0].command = None;
        project.agents[0].shell_command = Some("codex --full-auto".to_string());
        for agent in &mut project.agents {
            agent.cwd = std::env::temp_dir();
        }

        start(&fake, &project, false).or_panic();

        let val = fake
            .get_pane_option("%0", "@kira_mux_agent_command")
            .or_panic();
        assert_eq!(val.as_deref(), Some("__shell__"));
    }
}
