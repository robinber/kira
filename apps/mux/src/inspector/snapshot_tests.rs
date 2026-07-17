use super::{WorkspaceTopology, inspect};
use crate::error::WorkspaceDriftReason;
use crate::model::ResolvedProject;
use crate::test_support::{FakeTmux, TestResultExt, setup_healthy_session, test_project};
use crate::tmux::TmuxError;
use crate::tmux::metadata::{
    PANE_AGENT_ID, SESSION_CONFIG_FINGERPRINT, SESSION_PROFILE_ID, SESSION_PROJECT_ID, WINDOW_ROLE,
    WINDOW_ROLE_AGENTS,
};
use crate::workspace::session_name;

#[test]
fn inspect_treats_no_tmux_server_as_absent() {
    let fake = FakeTmux::new();
    fake.set_no_server(true);

    let topology = inspect(&fake, &test_project()).or_panic();

    assert!(matches!(topology, WorkspaceTopology::Absent));
}

#[test]
fn inspect_propagates_generic_snapshot_failure() {
    let fake = FakeTmux::new();
    fake.set_workspace_snapshot_error(TmuxError::CommandFailure("snapshot failed".into()));

    let error = inspect(&fake, &test_project()).err_or_panic();

    assert!(matches!(
        error.downcast_ref::<TmuxError>(),
        Some(TmuxError::CommandFailure(message)) if message == "snapshot failed"
    ));
}

#[test]
fn inspect_reports_missing_managed_window() {
    let fake = FakeTmux::new();
    let project = test_project();
    add_session_metadata(&fake, &project);

    assert_drift(&fake, &project, &WorkspaceDriftReason::ManagedWindowMissing);
}

#[test]
fn session_metadata_drift_precedes_missing_managed_window() {
    let fake = FakeTmux::new();
    let project = test_project();
    add_session_metadata(&fake, &project);
    fake.set_session_opt(&session_name(&project), SESSION_CONFIG_FINGERPRINT, "wrong");

    assert_drift(&fake, &project, &WorkspaceDriftReason::FingerprintMismatch);
}

#[test]
fn inspect_reports_project_metadata_mismatch() {
    let fake = FakeTmux::new();
    let project = test_project();
    setup_healthy_session(&fake, &project);
    fake.set_session_opt(&session_name(&project), SESSION_PROJECT_ID, "other-project");

    assert_drift(
        &fake,
        &project,
        &WorkspaceDriftReason::ProjectMetadataMismatch,
    );
}

#[test]
fn inspect_reports_profile_metadata_mismatch() {
    let fake = FakeTmux::new();
    let project = test_project();
    setup_healthy_session(&fake, &project);
    fake.set_session_opt(&session_name(&project), SESSION_PROFILE_ID, "other-profile");

    assert_drift(
        &fake,
        &project,
        &WorkspaceDriftReason::ProfileMetadataMismatch,
    );
}

#[test]
fn inspect_reports_window_metadata_mismatch() {
    let fake = FakeTmux::new();
    let project = test_project();
    setup_healthy_session(&fake, &project);
    fake.set_window_opt(
        &session_name(&project),
        &project.window_name,
        WINDOW_ROLE,
        "other-role",
    );

    assert_drift(
        &fake,
        &project,
        &WorkspaceDriftReason::WindowMetadataMismatch,
    );
}

#[test]
fn inspect_reports_missing_pane_metadata() {
    let fake = FakeTmux::new();
    let project = test_project();
    add_session_metadata(&fake, &project);
    let session = session_name(&project);
    fake.add_window(&session, &project.window_name);
    fake.set_window_opt(
        &session,
        &project.window_name,
        WINDOW_ROLE,
        WINDOW_ROLE_AGENTS,
    );
    for (index, agent) in project.agents.iter().enumerate() {
        fake.add_pane(&session, &project.window_name, &format!("%{index}"), false);
        if index > 0 {
            fake.set_pane_opt(
                &session,
                &project.window_name,
                index,
                PANE_AGENT_ID,
                &agent.id,
            );
        }
    }

    assert_drift(&fake, &project, &WorkspaceDriftReason::PaneMetadataMissing);
}

#[test]
fn inspect_orders_panes_by_config_and_preserves_exit_metadata() {
    let fake = FakeTmux::new();
    let project = test_project();
    add_session_metadata(&fake, &project);
    let session = session_name(&project);
    fake.add_window(&session, &project.window_name);
    fake.set_window_opt(
        &session,
        &project.window_name,
        WINDOW_ROLE,
        WINDOW_ROLE_AGENTS,
    );
    fake.add_pane(&session, &project.window_name, "%9", true);
    fake.set_pane_opt(&session, &project.window_name, 0, PANE_AGENT_ID, "beta");
    fake.set_pane_dead_status("%9", 137);
    fake.add_pane(&session, &project.window_name, "%3", false);
    fake.set_pane_opt(&session, &project.window_name, 1, PANE_AGENT_ID, "alpha");

    let topology = inspect(&fake, &project).or_panic();
    let WorkspaceTopology::Degraded(workspace) = topology else {
        panic!("expected degraded workspace");
    };

    assert_eq!(workspace.panes[0].agent.id, "alpha");
    assert_eq!(workspace.panes[0].pane.pane_id, "%3");
    assert!(!workspace.panes[0].pane.pane_dead);
    assert_eq!(workspace.panes[0].pane.pane_dead_status, None);
    assert_eq!(workspace.panes[1].agent.id, "beta");
    assert_eq!(workspace.panes[1].pane.pane_id, "%9");
    assert!(workspace.panes[1].pane.pane_dead);
    assert_eq!(workspace.panes[1].pane.pane_dead_status, Some(137));
}

fn add_session_metadata(fake: &FakeTmux, project: &ResolvedProject) {
    let session = session_name(project);
    fake.add_session(&session);
    fake.set_session_opt(&session, SESSION_CONFIG_FINGERPRINT, &project.fingerprint);
    fake.set_session_opt(&session, SESSION_PROJECT_ID, &project.id);
    fake.set_session_opt(&session, SESSION_PROFILE_ID, &project.profile_id);
}

fn assert_drift(fake: &FakeTmux, project: &ResolvedProject, expected: &WorkspaceDriftReason) {
    let topology = inspect(fake, project).or_panic();
    let WorkspaceTopology::Drifted { reason } = topology else {
        panic!("expected drift reason {expected:?}");
    };
    assert_eq!(&reason, expected);
}
