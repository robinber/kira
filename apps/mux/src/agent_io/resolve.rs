use anyhow::Result;

use crate::error::{KiraMuxError, WorkspaceDriftReason};
use crate::inspector::{self, WorkspaceTopology};
use crate::model::{ResolvedAgent, ResolvedProject};
use crate::tmux::{PaneInfo, TmuxAdapter};

/// Resolve the live managed pane for `agent_id` under the **same topology
/// contract** as [`inspector::inspect`].
///
/// Healthy and degraded workspaces both resolve: dead panes are returned so
/// callers can decide whether the operation is allowed (`send` rejects them;
/// `capture` allows them). Drifted and absent sessions fail with typed
/// [`KiraMuxError`] variants.
///
/// The inspected topology is returned alongside the pane so callers can
/// reuse it (e.g. for the prompt context) instead of running a second
/// inspection that could observe a different workspace state.
pub(super) fn resolve_managed_pane<'a>(
    tmux: &dyn TmuxAdapter,
    project: &'a ResolvedProject,
    agent_id: &str,
) -> Result<(PaneInfo, &'a ResolvedAgent, WorkspaceTopology)> {
    let agent = project
        .agents
        .iter()
        .find(|a| a.id == agent_id)
        .ok_or_else(|| KiraMuxError::UnknownAgentId(agent_id.to_string()))?;

    let topology = inspector::inspect(tmux, project)?;
    let pane = match &topology {
        WorkspaceTopology::Absent => return Err(KiraMuxError::SessionAbsent.into()),
        WorkspaceTopology::Drifted { reason } => {
            return Err(KiraMuxError::Drifted {
                project_id: project.id.clone(),
                reason: reason.clone(),
            }
            .into());
        }
        WorkspaceTopology::Healthy(workspace) | WorkspaceTopology::Degraded(workspace) => {
            // inspect() pairs every configured agent when topology is live;
            // MissingManagedPane is a defensive fallback only.
            workspace
                .panes
                .iter()
                .find(|mp| mp.agent.id == agent_id)
                .map(|mp| mp.pane.clone())
                .ok_or_else(|| KiraMuxError::Drifted {
                    project_id: project.id.clone(),
                    reason: WorkspaceDriftReason::MissingManagedPane(agent_id.to_string()),
                })?
        }
    };

    Ok((pane, agent, topology))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{err, ok};
    use crate::tmux::metadata::{WINDOW_ROLE, WINDOW_ROLE_AGENTS};
    use crate::workspace::session_name;

    fn tag_session_metadata(
        fake: &crate::test_support::FakeTmux,
        project: &ResolvedProject,
        session: &str,
    ) {
        fake.set_session_opt(
            session,
            "@kira_mux_config_fingerprint",
            &project.fingerprint,
        );
        fake.set_session_opt(session, "@kira_mux_project_id", &project.id);
        fake.set_session_opt(session, "@kira_mux_profile_id", &project.profile_id);
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
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        let session = session_name(&project);

        fake.add_session(&session);
        fake.set_session_opt(&session, "@kira_mux_config_fingerprint", "wrong");
        fake.set_session_opt(&session, "@kira_mux_project_id", &project.id);
        fake.set_session_opt(&session, "@kira_mux_profile_id", &project.profile_id);
        fake.add_window(&session, &project.window_name);
        fake.set_window_opt(
            &session,
            &project.window_name,
            WINDOW_ROLE,
            WINDOW_ROLE_AGENTS,
        );
        fake.add_pane(&session, &project.window_name, "%0", false);
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
    fn resolve_pane_fails_on_drifted_session_with_renamed_window() {
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        let session = session_name(&project);

        fake.add_session(&session);
        tag_session_metadata(&fake, &project, &session);
        fake.add_window(&session, "renamed-window");
        fake.add_pane(&session, "renamed-window", "%0", false);
        fake.set_pane_opt(&session, "renamed-window", 0, "@kira_mux_agent_id", "alpha");
        fake.add_pane(&session, "renamed-window", "%1", false);
        fake.set_pane_opt(&session, "renamed-window", 1, "@kira_mux_agent_id", "beta");

        let err = err(
            resolve_managed_pane(&fake, &project, "alpha"),
            "resolve_managed_pane should fail for a drifted session",
        );
        assert!(
            matches!(
                err.downcast_ref::<KiraMuxError>(),
                Some(KiraMuxError::Drifted {
                    reason: WorkspaceDriftReason::ManagedWindowMissing,
                    ..
                })
            ),
            "expected Drifted/ManagedWindowMissing, got: {err}"
        );
    }

    #[test]
    fn resolve_pane_duplicate_agent_id_fails() {
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        let session = session_name(&project);

        fake.add_session(&session);
        tag_session_metadata(&fake, &project, &session);
        fake.add_window(&session, &project.window_name);
        fake.set_window_opt(
            &session,
            &project.window_name,
            WINDOW_ROLE,
            WINDOW_ROLE_AGENTS,
        );
        fake.add_pane(&session, &project.window_name, "%0", false);
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
            "alpha",
        );

        let err = err(
            resolve_managed_pane(&fake, &project, "alpha"),
            "resolve_managed_pane should fail when agent IDs are duplicated",
        );
        assert!(
            matches!(
                err.downcast_ref::<KiraMuxError>(),
                Some(KiraMuxError::Drifted {
                    reason: WorkspaceDriftReason::DuplicateManagedAgentId(id),
                    ..
                }) if id == "alpha"
            ),
            "expected Drifted/DuplicateManagedAgentId, got: {err}"
        );
    }

    #[test]
    fn resolve_pane_no_metadata_fails() {
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        let session = session_name(&project);

        fake.add_session(&session);
        tag_session_metadata(&fake, &project, &session);
        fake.add_window(&session, &project.window_name);
        fake.set_window_opt(
            &session,
            &project.window_name,
            WINDOW_ROLE,
            WINDOW_ROLE_AGENTS,
        );
        fake.add_pane(&session, &project.window_name, "%0", false);
        fake.add_pane(&session, &project.window_name, "%1", false);

        let err = err(
            resolve_managed_pane(&fake, &project, "alpha"),
            "resolve_managed_pane should fail when pane metadata is missing",
        );
        assert!(
            matches!(
                err.downcast_ref::<KiraMuxError>(),
                Some(KiraMuxError::Drifted {
                    reason: WorkspaceDriftReason::PaneMetadataMissing,
                    ..
                })
            ),
            "expected Drifted/PaneMetadataMissing, got: {err}"
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
        fake.set_pane_opt(&session, "other-window", 0, "@kira_mux_agent_id", "alpha");

        let (pane, agent, _topology) = ok(
            resolve_managed_pane(&fake, &project, "alpha"),
            "resolve_managed_pane should ignore unmanaged windows",
        );
        assert_eq!(pane.pane_id, "%0");
        assert_eq!(agent.id, "alpha");
    }

    #[test]
    fn resolve_pane_fails_on_wrong_window_role() {
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        let session = session_name(&project);

        fake.add_session(&session);
        tag_session_metadata(&fake, &project, &session);
        fake.add_window(&session, &project.window_name);
        fake.set_window_opt(
            &session,
            &project.window_name,
            "@kira_mux_window_role",
            "wrong",
        );
        fake.add_pane(&session, &project.window_name, "%0", false);
        fake.set_pane_opt(
            &session,
            &project.window_name,
            0,
            "@kira_mux_agent_id",
            "alpha",
        );

        let err = err(
            resolve_managed_pane(&fake, &project, "alpha"),
            "resolve_managed_pane should fail on the wrong window role",
        );
        assert!(
            matches!(
                err.downcast_ref::<KiraMuxError>(),
                Some(KiraMuxError::Drifted {
                    reason: WorkspaceDriftReason::WindowMetadataMismatch,
                    ..
                })
            ),
            "expected Drifted/WindowMetadataMismatch, got: {err}"
        );
    }

    #[test]
    fn resolve_pane_fails_when_managed_window_missing() {
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        let session = session_name(&project);

        fake.add_session(&session);
        tag_session_metadata(&fake, &project, &session);

        let err = err(
            resolve_managed_pane(&fake, &project, "alpha"),
            "resolve_managed_pane should fail when the managed window is missing",
        );
        assert!(
            matches!(
                err.downcast_ref::<KiraMuxError>(),
                Some(KiraMuxError::Drifted {
                    reason: WorkspaceDriftReason::ManagedWindowMissing,
                    ..
                })
            ),
            "expected Drifted/ManagedWindowMissing, got: {err}"
        );
    }

    #[test]
    fn resolve_pane_fails_on_empty_window_role() {
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        let session = session_name(&project);

        fake.add_session(&session);
        tag_session_metadata(&fake, &project, &session);
        fake.add_window(&session, &project.window_name);
        fake.set_window_opt(&session, &project.window_name, "@kira_mux_window_role", "");

        let err = err(
            resolve_managed_pane(&fake, &project, "alpha"),
            "resolve_managed_pane should fail on an empty window role",
        );
        assert!(
            matches!(
                err.downcast_ref::<KiraMuxError>(),
                Some(KiraMuxError::Drifted {
                    reason: WorkspaceDriftReason::WindowMetadataMismatch,
                    ..
                })
            ),
            "expected Drifted/WindowMetadataMismatch for empty role, got: {err}"
        );
    }

    #[test]
    fn resolve_pane_deterministic_with_many_agents() {
        use std::collections::BTreeMap;
        use std::path::PathBuf;

        use crate::config::AgentMode;

        let fake = crate::test_support::FakeTmux::new();
        let mut project = crate::test_support::test_project();

        for i in 2..5 {
            project.agents.push(ResolvedAgent {
                id: format!("agent-{i}"),
                label: format!("Agent {i}"),
                mode: AgentMode::Direct,
                command: Some("echo".to_string()),
                shell_command: None,
                args: vec![],
                cwd: PathBuf::from("/tmp/test-project"),
                env: BTreeMap::new(),
                capabilities: vec![],
                prompt_template: None,
            });
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
}
