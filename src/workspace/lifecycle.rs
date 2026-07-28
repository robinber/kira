//! Start, repair, attach, restart, and kill managed workspaces.
//!
//! Ownership checks and topology guards keep foreign sessions out of scope.

use anyhow::{Result, bail};

use super::launch::{TopologyGuard, apply_layout, respawn_agent, verify_panes_survived_launch};
use super::{session_name, window_target};
use crate::config::ConfigError;
use crate::error::KiraMuxError;
use crate::inspector::{self, ManagedPane, WorkspaceTopology};
use crate::model::{ResolvedAgent, ResolvedProject};
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
        WorkspaceTopology::Degraded(workspace) => repair(tmux, project, &workspace.panes)?,
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

    inspector::ensure_session_owned(tmux, project)?;
    attach_to_session(tmux, &session)
}

pub(crate) fn restart(
    tmux: &dyn TmuxAdapter,
    project: &ResolvedProject,
    agent_id: Option<&str>,
) -> Result<StartOutcome> {
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

    inspector::ensure_session_owned(tmux, project)?;
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
    let result = if std::env::var_os("TMUX").is_some() {
        tmux.switch_client(session)
    } else {
        tmux.attach_session(session)
    };
    match result {
        Ok(()) => Ok(()),
        Err(error) => {
            // Interactive attach/switch only report a status code. If the
            // session vanished between ownership check and attach, surface the
            // stable SessionAbsent outcome instead of a generic exit 1.
            if !inspector::session_exists(tmux, session)? {
                return Err(KiraMuxError::SessionAbsent.into());
            }
            Err(error)
        }
    }
}

fn create(
    tmux: &dyn TmuxAdapter,
    project: &ResolvedProject,
    session: &str,
) -> Result<StartOutcome> {
    validate_launch_paths(project, project.agents.iter())?;

    let root = project.root.display().to_string();
    let window_target = window_target(session, &project.window_name);

    tmux.create_detached_session(session, &root, &project.window_name, project.agents.len())?;
    let mut guard = TopologyGuard::new(tmux, session);
    let setup = (|| -> Result<Vec<String>> {
        tmux.set_session_option(session, SESSION_PROJECT_ID, &project.id)?;
        tmux.set_session_option(session, SESSION_PROFILE_ID, &project.profile_id)?;
        tmux.set_session_option(session, SESSION_CONFIG_FINGERPRINT, &project.fingerprint)?;
        tmux.set_window_option(&window_target, WINDOW_ROLE, WINDOW_ROLE_AGENTS)?;
        tmux.set_window_option(
            &window_target,
            "remain-on-exit",
            project.remain_on_exit.as_str(),
        )?;

        // Pane ids are collected explicitly — the fresh window's single
        // seed pane, then each id split-window reports — so agent binding
        // never depends on listing order (with even-vertical applied, real
        // tmux lists panes out of creation order). The interim layout only
        // helps subsequent splits fit; the session height reserves the
        // actual room.
        let mut pane_ids: Vec<String> = tmux
            .list_panes(&window_target)?
            .into_iter()
            .map(|pane| pane.pane_id)
            .collect();
        if pane_ids.len() != 1 {
            bail!(
                "fresh session window has {} panes, expected the single seed pane",
                pane_ids.len()
            );
        }
        for _ in pane_ids.len()..project.agents.len() {
            let pane_id = tmux.split_window(&window_target, &root)?;
            tmux.select_layout(&window_target, "even-vertical")?;
            pane_ids.push(pane_id);
        }

        // Postcondition on final tmux state, not on the constructed list: a
        // seed window with extra panes or a concurrent split must roll back
        // rather than commit a partially unmanaged topology.
        let listed = tmux.list_panes(&window_target)?.len();
        if pane_ids.len() != project.agents.len() || listed != project.agents.len() {
            bail!(
                "expected {} panes after window setup, found {listed}",
                project.agents.len()
            );
        }
        for (pane_id, agent) in pane_ids.iter().zip(project.agents.iter()) {
            tmux.set_pane_option(pane_id, PANE_AGENT_ID, &agent.id)?;
        }

        apply_layout(tmux, project, &window_target)?;

        Ok(pane_ids)
    })();
    let pane_ids = match setup {
        Ok(pane_ids) => pane_ids,
        Err(error) => {
            guard.mark_failed(error.to_string());
            return Err(error);
        }
    };

    guard.commit();

    Ok(launch_all(
        tmux,
        project,
        "create",
        pane_ids
            .iter()
            .map(String::as_str)
            .zip(project.agents.iter()),
    ))
}

/// Launch every `(pane_id, agent)` target, keep going past individual
/// failures, and report one outcome — the single degraded-launch policy
/// shared by create, repair, and restart. The app layer maps the outcome
/// to the user-facing warning and exit exactly once.
fn launch_all<'a>(
    tmux: &dyn TmuxAdapter,
    project: &ResolvedProject,
    op: &'static str,
    targets: impl IntoIterator<Item = (&'a str, &'a ResolvedAgent)>,
) -> StartOutcome {
    let mut any_launch_failed = false;
    let mut respawned = Vec::new();
    for (pane_id, agent) in targets {
        match respawn_agent(tmux, pane_id, project, agent) {
            Ok(()) => respawned.push((pane_id, agent)),
            Err(error) => {
                tracing::warn!(
                    project_id = project.id.as_str(),
                    agent_id = agent.id.as_str(),
                    op,
                    %error,
                    "agent launch failed, workspace degraded"
                );
                any_launch_failed = true;
            }
        }
    }
    for (agent, error) in verify_panes_survived_launch(tmux, &respawned) {
        tracing::warn!(
            project_id = project.id.as_str(),
            agent_id = agent.id.as_str(),
            op,
            %error,
            "agent launch failed, workspace degraded"
        );
        any_launch_failed = true;
    }
    if any_launch_failed {
        StartOutcome::Degraded
    } else {
        StartOutcome::Healthy
    }
}

fn repair(
    tmux: &dyn TmuxAdapter,
    project: &ResolvedProject,
    panes: &[ManagedPane],
) -> Result<StartOutcome> {
    validate_launch_paths(
        project,
        panes
            .iter()
            .filter(|managed| managed.pane.pane_dead)
            .map(|managed| &managed.agent),
    )?;

    Ok(launch_all(
        tmux,
        project,
        "repair",
        panes
            .iter()
            .filter(|managed| managed.pane.pane_dead)
            .map(|managed| (managed.pane.pane_id.as_str(), &managed.agent)),
    ))
}

fn restart_managed_panes(
    tmux: &dyn TmuxAdapter,
    project: &ResolvedProject,
    panes: &[ManagedPane],
    agent_id: Option<&str>,
) -> Result<StartOutcome> {
    if let Some(agent_id) = agent_id {
        let managed = panes
            .iter()
            .find(|pane| pane.agent.id == agent_id)
            .ok_or_else(|| KiraMuxError::UnknownAgentId(agent_id.to_string()))?;
        validate_launch_paths(project, std::iter::once(&managed.agent))?;
        return Ok(launch_all(
            tmux,
            project,
            "restart",
            std::iter::once((managed.pane.pane_id.as_str(), &managed.agent)),
        ));
    }

    validate_launch_paths(project, panes.iter().map(|managed| &managed.agent))?;

    Ok(launch_all(
        tmux,
        project,
        "restart",
        panes
            .iter()
            .map(|managed| (managed.pane.pane_id.as_str(), &managed.agent)),
    ))
}

fn validate_launch_paths<'a>(
    project: &ResolvedProject,
    agents: impl IntoIterator<Item = &'a ResolvedAgent>,
) -> Result<()> {
    if !project.root.exists() {
        return Err(
            KiraMuxError::ConfigValidation(ConfigError::ProjectRootNotFound(project.root.clone()))
                .into(),
        );
    }
    if !project.root.is_dir() {
        return Err(
            KiraMuxError::ConfigValidation(ConfigError::ProjectRootNotDirectory(
                project.root.clone(),
            ))
            .into(),
        );
    }

    for agent in agents {
        if !agent.cwd.exists() {
            return Err(
                KiraMuxError::ConfigValidation(ConfigError::AgentCwdNotFound {
                    agent_id: agent.id.clone(),
                    path: agent.cwd.clone(),
                })
                .into(),
            );
        }
        if !agent.cwd.is_dir() {
            return Err(
                KiraMuxError::ConfigValidation(ConfigError::AgentCwdNotDirectory {
                    agent_id: agent.id.clone(),
                    path: agent.cwd.clone(),
                })
                .into(),
            );
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentMode;
    use crate::error::WorkspaceDriftReason;
    use crate::test_support::{FakeTmux, TestResultExt, setup_healthy_session, test_project};
    use crate::tmux::metadata::PANE_AGENT_COMMAND;
    use crate::workspace::session_name;

    fn make_launchable(project: &mut ResolvedProject) {
        project.root = std::env::temp_dir();
        for agent in &mut project.agents {
            agent.cwd = std::env::temp_dir();
        }
    }

    #[test]
    fn start_creates_new_workspace_from_absent() {
        let fake = FakeTmux::new();
        let mut project = test_project();
        make_launchable(&mut project);

        let outcome =
            start(&fake, &project, false).or_panic("start_creates_new_workspace_from_absent");
        assert_eq!(outcome, StartOutcome::Healthy);
        assert!(
            fake.session_exists(&session_name(&project))
                .or_panic("start_creates_new_workspace_from_absent")
        );
    }

    #[test]
    fn create_binds_each_agent_to_the_pane_id_reported_at_creation() {
        let fake = FakeTmux::new();
        // Reversed listings defeat any binding that zips list_panes output
        // with the agent roster — only split-reported ids survive this.
        fake.set_reverse_pane_listing(true);
        let mut project = test_project();
        make_launchable(&mut project);
        assert!(
            project.agents.len() >= 2,
            "binding order is only meaningful with multiple agents"
        );

        start(&fake, &project, false)
            .or_panic("create_binds_each_agent_to_the_pane_id_reported_at_creation");

        // FakeTmux allocates %0 for the seed pane and %N per split, in
        // creation order — each agent must be tagged on exactly the pane
        // created for it, not wherever a listing happens to place it.
        for (idx, agent) in project.agents.iter().enumerate() {
            let pane_id = format!("%{idx}");
            assert_eq!(
                fake.get_pane_option(&pane_id, PANE_AGENT_ID)
                    .or_panic("create_binds_each_agent_to_the_pane_id_reported_at_creation"),
                Some(agent.id.clone()),
                "agent '{}' must land on {pane_id}",
                agent.id
            );
        }
    }

    #[test]
    fn start_reuses_healthy_session() {
        let fake = FakeTmux::new();
        let project = test_project();
        setup_healthy_session(&fake, &project);

        let outcome = start(&fake, &project, false).or_panic("start_reuses_healthy_session");
        assert_eq!(outcome, StartOutcome::Healthy);
    }

    #[test]
    fn start_on_healthy_does_not_respawn_panes() {
        // Env reference host-value rotation is invisible to the fingerprint,
        // so start must not re-inject env on a healthy session — that is
        // restart's job (#16).
        let fake = FakeTmux::new();
        let mut project = test_project();
        make_launchable(&mut project);
        setup_healthy_session(&fake, &project);
        let before = fake
            .ops()
            .iter()
            .filter(|op| matches!(op, crate::test_support::FakeOp::RespawnPane { .. }))
            .count();

        start(&fake, &project, false).or_panic("start_on_healthy_does_not_respawn_panes");

        let after = fake
            .ops()
            .iter()
            .filter(|op| matches!(op, crate::test_support::FakeOp::RespawnPane { .. }))
            .count();
        assert_eq!(
            before, after,
            "healthy start must not respawn panes (would be required to refresh $VAR env)"
        );
    }

    #[test]
    fn restart_respawns_panes_to_refresh_runtime_env() {
        let fake = FakeTmux::new();
        let mut project = test_project();
        make_launchable(&mut project);
        setup_healthy_session(&fake, &project);
        let before = fake
            .ops()
            .iter()
            .filter(|op| matches!(op, crate::test_support::FakeOp::RespawnPane { .. }))
            .count();

        restart(&fake, &project, None).or_panic("restart_respawns_panes_to_refresh_runtime_env");

        let after = fake
            .ops()
            .iter()
            .filter(|op| matches!(op, crate::test_support::FakeOp::RespawnPane { .. }))
            .count();
        assert!(
            after > before,
            "restart must respawn so newly resolved $VAR values reach panes"
        );
    }

    #[test]
    fn start_repairs_degraded_session() {
        let fake = FakeTmux::new();
        let mut project = test_project();
        make_launchable(&mut project);
        crate::test_support::setup_session_with_dead_panes(&fake, &project, &[1]);

        let outcome = start(&fake, &project, false).or_panic("start_repairs_degraded_session");
        assert_eq!(outcome, StartOutcome::Healthy);
    }

    #[test]
    fn start_reports_degraded_when_agent_exits_immediately() {
        let fake = FakeTmux::new();
        let mut project = test_project();
        make_launchable(&mut project);
        fake.set_respawn_exits_immediately(true);

        let outcome = start(&fake, &project, false)
            .or_panic("start_reports_degraded_when_agent_exits_immediately");
        assert_eq!(outcome, StartOutcome::Degraded);
    }

    #[test]
    fn restart_single_agent_reports_degraded_on_immediate_exit() {
        let fake = FakeTmux::new();
        let mut project = test_project();
        make_launchable(&mut project);
        setup_healthy_session(&fake, &project);
        fake.set_respawn_exits_immediately(true);

        let outcome = restart(&fake, &project, Some("alpha"))
            .or_panic("restart_single_agent_reports_degraded_on_immediate_exit");
        assert_eq!(
            outcome,
            StartOutcome::Degraded,
            "single-agent restart must use degraded semantics"
        );
    }

    #[test]
    fn start_repair_rejects_missing_project_root_before_respawn() {
        let fake = FakeTmux::new();
        let base = tempfile::tempdir()
            .or_panic("start_repair_rejects_missing_project_root_before_respawn");
        let mut project = test_project();
        project.root = base.path().join("missing-root");
        project.agents[0].cwd = project.root.clone();
        crate::test_support::setup_session_with_dead_panes(&fake, &project, &[0]);

        let error = start(&fake, &project, false)
            .err_or_panic("start_repair_rejects_missing_project_root_before_respawn: expected Err");

        assert!(matches!(
            error.downcast_ref::<KiraMuxError>(),
            Some(KiraMuxError::ConfigValidation(
                ConfigError::ProjectRootNotFound(_)
            ))
        ));
    }

    #[test]
    fn start_refuses_drifted_session() {
        let fake = FakeTmux::new();
        let project = test_project();
        let session = session_name(&project);

        fake.add_session(&session);
        fake.set_session_opt(&session, SESSION_CONFIG_FINGERPRINT, "wrong");
        fake.set_session_opt(&session, SESSION_PROJECT_ID, &project.id);

        let err = start(&fake, &project, false)
            .err_or_panic("start_refuses_drifted_session: expected Err");
        assert!(err.downcast_ref::<KiraMuxError>().is_some());
    }

    #[test]
    fn restart_all_agents_on_healthy_session() {
        let fake = FakeTmux::new();
        let mut project = test_project();
        make_launchable(&mut project);
        setup_healthy_session(&fake, &project);

        restart(&fake, &project, None).or_panic("restart_all_agents_on_healthy_session");
    }

    #[test]
    fn restart_all_reports_degraded_after_attempting_every_pane() {
        let fake = FakeTmux::new();
        let mut project = test_project();
        make_launchable(&mut project);
        setup_healthy_session(&fake, &project);
        fake.set_fail_respawn(true);

        let outcome = restart(&fake, &project, None)
            .or_panic("restart_all_reports_degraded_after_attempting_every_pane");
        assert_eq!(
            outcome,
            StartOutcome::Degraded,
            "restart must keep create()/repair() degraded semantics"
        );
    }

    #[test]
    fn restart_single_agent() {
        let fake = FakeTmux::new();
        let mut project = test_project();
        make_launchable(&mut project);
        setup_healthy_session(&fake, &project);

        restart(&fake, &project, Some("alpha")).or_panic("restart_single_agent");
    }

    #[test]
    fn restart_rejects_missing_agent_cwd_before_respawn() {
        let fake = FakeTmux::new();
        let base = tempfile::tempdir().or_panic("restart_rejects_missing_agent_cwd_before_respawn");
        let mut project = test_project();
        project.root = base.path().to_path_buf();
        project.agents[0].cwd = base.path().join("missing-cwd");
        setup_healthy_session(&fake, &project);

        let error = restart(&fake, &project, Some("alpha"))
            .err_or_panic("restart_rejects_missing_agent_cwd_before_respawn: expected Err");

        assert!(matches!(
            error.downcast_ref::<KiraMuxError>(),
            Some(KiraMuxError::ConfigValidation(
                ConfigError::AgentCwdNotFound { .. }
            ))
        ));
    }

    #[test]
    fn restart_unknown_agent_fails() {
        let fake = FakeTmux::new();
        let project = test_project();
        setup_healthy_session(&fake, &project);

        let err = restart(&fake, &project, Some("nonexistent"))
            .err_or_panic("restart_unknown_agent_fails: expected Err");
        assert!(matches!(
            err.downcast_ref::<KiraMuxError>(),
            Some(KiraMuxError::UnknownAgentId(_))
        ));
    }

    #[test]
    fn restart_absent_session_fails() {
        let fake = FakeTmux::new();
        let project = test_project();

        let err = restart(&fake, &project, None)
            .err_or_panic("restart_absent_session_fails: expected Err");
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
        fake.set_session_opt(&session, SESSION_CONFIG_FINGERPRINT, "wrong");
        fake.set_session_opt(&session, SESSION_PROJECT_ID, &project.id);

        let err = restart(&fake, &project, None)
            .err_or_panic("restart_drifted_session_fails: expected Err");
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

        kill(&fake, &project).or_panic("kill_removes_session");
        assert!(
            !fake
                .session_exists(&session_name(&project))
                .or_panic("kill_removes_session")
        );
    }

    #[test]
    fn kill_absent_session_succeeds() {
        let fake = FakeTmux::new();
        let project = test_project();

        kill(&fake, &project).or_panic("kill_absent_session_succeeds");
    }

    #[test]
    fn kill_refuses_untagged_session_name_collision() {
        let fake = FakeTmux::new();
        let project = test_project();
        let session = session_name(&project);
        fake.add_session(&session);

        let error = kill(&fake, &project)
            .err_or_panic("kill_refuses_untagged_session_name_collision: expected Err");

        assert!(matches!(
            error.downcast_ref::<KiraMuxError>(),
            Some(KiraMuxError::Drifted {
                reason: WorkspaceDriftReason::ProjectMetadataMismatch,
                ..
            })
        ));
        assert!(
            fake.session_exists(&session)
                .or_panic("kill_refuses_untagged_session_name_collision")
        );
    }

    #[test]
    fn attach_refuses_untagged_session_name_collision() {
        let fake = FakeTmux::new();
        let project = test_project();
        let session = session_name(&project);
        fake.add_session(&session);

        let error = attach(&fake, &project)
            .err_or_panic("attach_refuses_untagged_session_name_collision: expected Err");

        assert!(matches!(
            error.downcast_ref::<KiraMuxError>(),
            Some(KiraMuxError::Drifted {
                reason: WorkspaceDriftReason::ProjectMetadataMismatch,
                ..
            })
        ));
    }

    #[test]
    fn kill_allows_owned_session_with_fingerprint_drift() {
        let fake = FakeTmux::new();
        let project = test_project();
        let session = session_name(&project);
        setup_healthy_session(&fake, &project);
        fake.set_session_opt(&session, SESSION_CONFIG_FINGERPRINT, "stale");

        kill(&fake, &project).or_panic("kill_allows_owned_session_with_fingerprint_drift");

        assert!(
            !fake
                .session_exists(&session)
                .or_panic("kill_allows_owned_session_with_fingerprint_drift")
        );
    }

    #[test]
    fn attach_maps_vanished_session_to_session_absent() {
        let fake = FakeTmux::new();
        let project = test_project();
        setup_healthy_session(&fake, &project);
        // Ownership check passes; interactive attach then races the session away.
        fake.set_vanish_before_attach(true);

        let error = attach(&fake, &project)
            .err_or_panic("attach_maps_vanished_session_to_session_absent: expected Err");
        assert!(
            matches!(
                error.downcast_ref::<KiraMuxError>(),
                Some(KiraMuxError::SessionAbsent)
            ),
            "vanished session after ownership check must be SessionAbsent, got: {error}"
        );
        assert!(
            !fake
                .session_exists(&session_name(&project))
                .or_panic("attach_maps_vanished_session_to_session_absent")
        );
    }

    #[test]
    fn attach_propagates_error_when_session_still_present() {
        let fake = FakeTmux::new();
        let project = test_project();
        setup_healthy_session(&fake, &project);
        fake.set_fail_attach(true);

        let error = attach(&fake, &project)
            .err_or_panic("attach_propagates_error_when_session_still_present: expected Err");
        assert!(
            error.downcast_ref::<KiraMuxError>().is_none(),
            "hard attach failure with live session must not become SessionAbsent, got: {error}"
        );
        assert!(
            fake.session_exists(&session_name(&project))
                .or_panic("attach_propagates_error_when_session_still_present")
        );
    }

    #[test]
    fn kill_succeeds_when_session_vanishes_during_kill() {
        let fake = FakeTmux::new();
        let project = test_project();
        setup_healthy_session(&fake, &project);
        fake.set_vanish_before_kill(true);

        kill(&fake, &project).or_panic("kill_succeeds_when_session_vanishes_during_kill");
        assert!(
            !fake
                .session_exists(&session_name(&project))
                .or_panic("kill_succeeds_when_session_vanishes_during_kill")
        );
    }

    #[test]
    fn launch_sets_command_metadata() {
        let fake = FakeTmux::new();
        let mut project = test_project();
        make_launchable(&mut project);

        let outcome = start(&fake, &project, false).or_panic("launch_sets_command_metadata");
        assert_eq!(outcome, StartOutcome::Healthy);

        let val = fake
            .get_pane_option("%0", PANE_AGENT_COMMAND)
            .or_panic("launch_sets_command_metadata");
        assert_eq!(val.as_deref(), Some("echo"));
    }

    #[test]
    fn launch_sets_path_basename() {
        let fake = FakeTmux::new();
        let mut project = test_project();
        make_launchable(&mut project);
        project.agents[0].command = Some("/usr/bin/codex".to_string());

        start(&fake, &project, false).or_panic("launch_sets_path_basename");

        let val = fake
            .get_pane_option("%0", PANE_AGENT_COMMAND)
            .or_panic("launch_sets_path_basename");
        assert_eq!(val.as_deref(), Some("codex"));
    }

    #[test]
    fn launch_sets_shell_sentinel() {
        let fake = FakeTmux::new();
        let mut project = test_project();
        make_launchable(&mut project);
        project.agents[0].mode = AgentMode::Shell;
        project.agents[0].command = None;
        project.agents[0].shell_command = Some("codex --full-auto".to_string());

        start(&fake, &project, false).or_panic("launch_sets_shell_sentinel");

        let val = fake
            .get_pane_option("%0", PANE_AGENT_COMMAND)
            .or_panic("launch_sets_shell_sentinel");
        assert_eq!(val.as_deref(), Some("__shell__"));
    }
}
