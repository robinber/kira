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

    let outcome = start(&fake, &project, false).or_panic("start_creates_new_workspace_from_absent");
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
    let base =
        tempfile::tempdir().or_panic("start_repair_rejects_missing_project_root_before_respawn");
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

    let err =
        start(&fake, &project, false).err_or_panic("start_refuses_drifted_session: expected Err");
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

    let err =
        restart(&fake, &project, None).err_or_panic("restart_absent_session_fails: expected Err");
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

    let err =
        restart(&fake, &project, None).err_or_panic("restart_drifted_session_fails: expected Err");
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

    let outcome = kill(&fake, &project, |_| Ok(())).or_panic("kill_removes_session");
    assert_eq!(outcome, KillOutcome::Killed);
    assert!(
        !fake
            .session_exists(&session_name(&project))
            .or_panic("kill_removes_session")
    );
}

#[test]
fn kill_absent_session_reports_already_stopped_without_confirm() {
    let fake = FakeTmux::new();
    let project = test_project();

    let outcome = kill(&fake, &project, |_| {
        panic!("confirm must not run for an absent session")
    })
    .or_panic("kill_absent_session_reports_already_stopped_without_confirm");

    assert_eq!(outcome, KillOutcome::AlreadyStopped);
}

#[test]
fn kill_declined_confirm_leaves_session_alive() {
    let fake = FakeTmux::new();
    let project = test_project();
    setup_healthy_session(&fake, &project);

    let error = kill(&fake, &project, |_| Err(KiraMuxError::KillAborted.into()))
        .err_or_panic("kill_declined_confirm_leaves_session_alive: expected Err");

    assert!(matches!(
        error.downcast_ref::<KiraMuxError>(),
        Some(KiraMuxError::KillAborted)
    ));
    assert!(
        fake.session_exists(&session_name(&project))
            .or_panic("kill_declined_confirm_leaves_session_alive")
    );
}

#[test]
fn kill_refuses_untagged_session_name_collision() {
    let fake = FakeTmux::new();
    let project = test_project();
    let session = session_name(&project);
    fake.add_session(&session);

    let error = kill(&fake, &project, |_| {
        panic!("confirm must not run before ownership is proven")
    })
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

    kill(&fake, &project, |_| Ok(())).or_panic("kill_allows_owned_session_with_fingerprint_drift");

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

    kill(&fake, &project, |_| Ok(())).or_panic("kill_succeeds_when_session_vanishes_during_kill");
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
