use anyhow::{Result, bail};

use crate::domain::{ResolvedAgent, ResolvedProject};
use crate::error::{AiMuxError, WorkspaceDriftReason};
use crate::inspector;
use crate::tmux::metadata::{PANE_AGENT_ID, WINDOW_ROLE, WINDOW_ROLE_AGENTS};
use crate::tmux::{PaneInfo, TmuxAdapter};
use crate::workspace::{session_name, window_target};

pub(super) fn resolve_managed_pane(
    tmux: &dyn TmuxAdapter,
    project: &ResolvedProject,
    agent_id: &str,
) -> Result<(PaneInfo, ResolvedAgent)> {
    let agent = project
        .agents
        .iter()
        .find(|a| a.id == agent_id)
        .ok_or_else(|| AiMuxError::UnknownAgentId(agent_id.to_string()))?;

    let session = session_name(project);
    if !inspector::session_exists(tmux, &session)? {
        return Err(AiMuxError::SessionAbsent.into());
    }

    let window_target = window_target(&session, &project.window_name);
    let Ok(window_role) = tmux.get_window_option(&window_target, WINDOW_ROLE) else {
        return Err(AiMuxError::Drifted {
            project_id: project.id.clone(),
            reason: WorkspaceDriftReason::ManagedWindowMissing,
        }
        .into());
    };
    if window_role.as_deref() != Some(WINDOW_ROLE_AGENTS) {
        return Err(AiMuxError::Drifted {
            project_id: project.id.clone(),
            reason: WorkspaceDriftReason::WindowMetadataMismatch,
        }
        .into());
    }

    let panes = tmux.list_panes(Some(&window_target))?;

    let mut matches: Vec<PaneInfo> = Vec::new();
    for pane in panes {
        if let Some(id) = tmux.get_pane_option(&pane.pane_id, PANE_AGENT_ID)?
            && id == agent_id
        {
            matches.push(pane);
        }
    }

    match matches.len() {
        0 => bail!("pane for agent '{agent_id}' not found in session"),
        1 => match matches.into_iter().next() {
            Some(pane) => Ok((pane, agent.clone())),
            None => bail!("managed pane disappeared during resolution"),
        },
        n => bail!("agent '{agent_id}' is not uniquely resolvable: {n} panes match"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{err, ok};

    #[test]
    fn resolve_pane_absent_session() {
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        let err = err(
            resolve_managed_pane(&fake, &project, "alpha"),
            "resolve_managed_pane should fail when the session is absent",
        );
        assert!(matches!(
            err.downcast_ref::<AiMuxError>(),
            Some(AiMuxError::SessionAbsent)
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
            err.downcast_ref::<AiMuxError>(),
            Some(AiMuxError::UnknownAgentId(_))
        ));
    }

    #[test]
    fn resolve_pane_found() {
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        crate::test_support::setup_healthy_session(&fake, &project);
        let (pane, agent) = ok(
            resolve_managed_pane(&fake, &project, "alpha"),
            "resolve_managed_pane should find the healthy managed pane",
        );
        assert_eq!(pane.pane_id, "%0");
        assert_eq!(agent.id, "alpha");
    }

    #[test]
    fn resolve_pane_fails_on_drifted_session_with_renamed_window() {
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        let session = session_name(&project);

        fake.add_session(&session);
        fake.set_session_opt(&session, "@kira_mux_config_fingerprint", "wrong");
        fake.set_session_opt(&session, "@kira_mux_project_id", &project.id);
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
                err.downcast_ref::<AiMuxError>(),
                Some(AiMuxError::Drifted {
                    reason: WorkspaceDriftReason::WindowMetadataMismatch,
                    ..
                })
            ),
            "expected Drifted/WindowMetadataMismatch, got: {err}"
        );
    }

    #[test]
    fn resolve_pane_duplicate_agent_id_fails() {
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
            err.to_string().contains("not uniquely"),
            "expected non-unique error, got: {err}"
        );
    }

    #[test]
    fn resolve_pane_no_metadata_fails() {
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
        fake.add_pane(&session, &project.window_name, "%0", false);
        fake.add_pane(&session, &project.window_name, "%1", false);

        let err = err(
            resolve_managed_pane(&fake, &project, "alpha"),
            "resolve_managed_pane should fail when pane metadata is missing",
        );
        assert!(
            err.to_string().contains("not found"),
            "expected not-found error, got: {err}"
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

        let (pane, agent) = ok(
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
                err.downcast_ref::<AiMuxError>(),
                Some(AiMuxError::Drifted {
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
        fake.set_session_opt(
            &session,
            "@kira_mux_config_fingerprint",
            &project.fingerprint,
        );
        fake.set_session_opt(&session, "@kira_mux_project_id", &project.id);

        let err = err(
            resolve_managed_pane(&fake, &project, "alpha"),
            "resolve_managed_pane should fail when the managed window is missing",
        );
        assert!(
            matches!(
                err.downcast_ref::<AiMuxError>(),
                Some(AiMuxError::Drifted {
                    reason: WorkspaceDriftReason::WindowMetadataMismatch,
                    ..
                })
            ),
            "expected Drifted/WindowMetadataMismatch, got: {err}"
        );
    }

    #[test]
    fn resolve_pane_fails_on_empty_window_role() {
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
        fake.set_window_opt(&session, &project.window_name, "@kira_mux_window_role", "");

        let err = err(
            resolve_managed_pane(&fake, &project, "alpha"),
            "resolve_managed_pane should fail on an empty window role",
        );
        assert!(
            matches!(
                err.downcast_ref::<AiMuxError>(),
                Some(AiMuxError::Drifted {
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
                orchestrator_prompt_template: None,
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
            let (pane, agent) = ok(
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
