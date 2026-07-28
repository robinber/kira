use super::*;
use crate::test_support::{err, ok};
use crate::tmux::metadata::{PANE_AGENT_ID, SESSION_CONFIG_FINGERPRINT};
use crate::workspace::session_name;

#[test]
fn resolve_vanished_session_race_maps_to_session_absent() {
    // The session vanishing between the existence check and the metadata
    // read must give send/capture the typed exit-5 SessionAbsent, not a
    // generic transport error (issue #83 classification in inspect()).
    let fake = crate::test_support::FakeTmux::new();
    let project = crate::test_support::test_project();
    crate::test_support::setup_healthy_session(&fake, &project);
    fake.set_workspace_snapshot_error(TmuxError::MissingSession("gone".into()));

    let err = err(
        resolve_managed_pane(&fake, &project, "alpha"),
        "resolve_managed_pane should classify the race as absence",
    );
    assert!(matches!(
        err.downcast_ref::<KiraMuxError>(),
        Some(KiraMuxError::SessionAbsent)
    ));
}

#[test]
fn resolve_pane_absent_session() {
    let fake = crate::test_support::FakeTmux::new();
    let project = crate::test_support::test_project();
    let err = err(
        resolve_managed_pane(&fake, &project, "alpha"),
        "resolve_managed_pane should fail when the session is absent",
    );
    assert!(matches!(
        err.downcast_ref::<KiraMuxError>(),
        Some(KiraMuxError::SessionAbsent)
    ));
}

#[test]
fn resolve_pane_unknown_agent() {
    let fake = crate::test_support::FakeTmux::new();
    let project = crate::test_support::test_project();
    crate::test_support::setup_healthy_session(&fake, &project);
    let err = err(
        resolve_managed_pane(&fake, &project, "nonexistent"),
        "resolve_managed_pane should fail for an unknown agent",
    );
    assert!(matches!(
        err.downcast_ref::<KiraMuxError>(),
        Some(KiraMuxError::UnknownAgentId(_))
    ));
}

#[test]
fn resolve_pane_found() {
    let fake = crate::test_support::FakeTmux::new();
    let project = crate::test_support::test_project();
    crate::test_support::setup_healthy_session(&fake, &project);
    let (pane, agent, _topology) = ok(
        resolve_managed_pane(&fake, &project, "alpha"),
        "resolve_managed_pane should find the healthy managed pane",
    );
    assert_eq!(pane.pane_id, "%0");
    assert_eq!(agent.id, "alpha");
}

#[test]
fn resolve_pane_allows_degraded_dead_pane() {
    let fake = crate::test_support::FakeTmux::new();
    let project = crate::test_support::test_project();
    crate::test_support::setup_session_with_dead_panes(&fake, &project, &[0]);

    let (pane, agent, _topology) = ok(
        resolve_managed_pane(&fake, &project, "alpha"),
        "resolve_managed_pane should return dead panes so callers can decide",
    );
    assert!(pane.pane_dead);
    assert_eq!(agent.id, "alpha");
}

#[test]
fn resolve_pane_fails_on_fingerprint_mismatch() {
    // The one drift passthrough kept at this layer: the taxonomy itself
    // is pinned by the classifier table in inspector.rs.
    let fake = crate::test_support::FakeTmux::new();
    let project = crate::test_support::test_project();
    crate::test_support::setup_healthy_session(&fake, &project);
    fake.set_session_opt(&session_name(&project), SESSION_CONFIG_FINGERPRINT, "wrong");

    let err = err(
        resolve_managed_pane(&fake, &project, "alpha"),
        "resolve_managed_pane should fail on fingerprint mismatch",
    );
    assert!(
        matches!(
            err.downcast_ref::<KiraMuxError>(),
            Some(KiraMuxError::Drifted {
                reason: WorkspaceDriftReason::FingerprintMismatch,
                ..
            })
        ),
        "expected Drifted/FingerprintMismatch, got: {err}"
    );
}

#[test]
fn resolve_pane_ignores_unmanaged_window_panes() {
    let fake = crate::test_support::FakeTmux::new();
    let project = crate::test_support::test_project();
    let session = session_name(&project);

    crate::test_support::setup_healthy_session(&fake, &project);

    fake.add_window(&session, "other-window");
    fake.add_pane(&session, "other-window", "%99", false);
    fake.set_pane_opt(&session, "other-window", 0, PANE_AGENT_ID, "alpha");

    let (pane, agent, _topology) = ok(
        resolve_managed_pane(&fake, &project, "alpha"),
        "resolve_managed_pane should ignore unmanaged windows",
    );
    assert_eq!(pane.pane_id, "%0");
    assert_eq!(agent.id, "alpha");
}

#[test]
fn resolve_pane_deterministic_with_many_agents() {
    let fake = crate::test_support::FakeTmux::new();
    let mut project = crate::test_support::test_project();

    for i in 2..5 {
        project
            .agents
            .push(crate::test_support::test_agent(&format!("agent-{i}")));
    }

    crate::test_support::setup_healthy_session(&fake, &project);

    let expected = [
        ("alpha", "%0"),
        ("beta", "%1"),
        ("agent-2", "%2"),
        ("agent-3", "%3"),
        ("agent-4", "%4"),
    ];

    for (agent_id, expected_pane_id) in expected {
        let (pane, agent, _topology) = ok(
            resolve_managed_pane(&fake, &project, agent_id),
            format!("resolve_managed_pane should find agent '{agent_id}'"),
        );
        assert_eq!(
            pane.pane_id, expected_pane_id,
            "wrong pane for agent {agent_id}"
        );
        assert_eq!(agent.id, agent_id);
    }
}
