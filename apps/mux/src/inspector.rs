use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;

use crate::domain::{ResolvedAgent, ResolvedProject};
use crate::error::WorkspaceDriftReason;
use crate::tmux::metadata::{
    PANE_AGENT_ID, SESSION_CONFIG_FINGERPRINT, SESSION_PROFILE_ID, SESSION_PROJECT_ID, WINDOW_ROLE,
    WINDOW_ROLE_AGENTS,
};
use crate::tmux::{PaneInfo, TmuxAdapter, TmuxError};
use crate::workspace::{session_name, window_target};

/// A managed pane paired with its resolved agent definition.
#[derive(Debug, Clone)]
pub(crate) struct ManagedPane {
    /// Live tmux pane metadata.
    pub(crate) pane: PaneInfo,
    /// Resolved agent assigned to the pane.
    pub(crate) agent: ResolvedAgent,
}

/// Ordered pane snapshot for a managed workspace.
#[derive(Debug, Clone)]
pub(crate) struct InspectedWorkspace {
    /// Managed panes in configured agent order.
    pub(crate) panes: Vec<ManagedPane>,
}

/// High-level topology classification for a workspace inspection.
#[derive(Debug, Clone)]
pub(crate) enum WorkspaceTopology {
    /// No matching session exists.
    Absent,
    /// Session metadata and pane health are consistent.
    Healthy(InspectedWorkspace),
    /// Session metadata is consistent, but one or more panes are degraded.
    Degraded(InspectedWorkspace),
    /// Session state drifted away from the resolved project contract.
    Drifted { reason: WorkspaceDriftReason },
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct RawWorkspacePane<'a> {
    pub(crate) agent_id: Option<&'a str>,
    pub(crate) pane_dead: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct RawWorkspaceSnapshot<'a> {
    pub(crate) fingerprint: Option<&'a str>,
    pub(crate) project_id: Option<&'a str>,
    pub(crate) profile_id: Option<&'a str>,
    pub(crate) window_role: Option<&'a str>,
    pub(crate) panes: Vec<RawWorkspacePane<'a>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SharedTopology {
    Healthy { ordered_pane_indexes: Vec<usize> },
    Degraded { ordered_pane_indexes: Vec<usize> },
    Drifted { reason: WorkspaceDriftReason },
}

pub(crate) fn classify_snapshot(
    project: &ResolvedProject,
    snapshot: &RawWorkspaceSnapshot<'_>,
) -> SharedTopology {
    if let Some(reason) = classify_session_metadata(
        project,
        snapshot.fingerprint,
        snapshot.project_id,
        snapshot.profile_id,
    ) {
        return SharedTopology::Drifted { reason };
    }

    if let Some(reason) = classify_window_shape(project, snapshot.window_role, snapshot.panes.len())
    {
        return SharedTopology::Drifted { reason };
    }

    classify_managed_panes(project, &snapshot.panes)
}

fn classify_managed_panes(
    project: &ResolvedProject,
    panes: &[RawWorkspacePane<'_>],
) -> SharedTopology {
    let configured_agent_ids = project
        .agents
        .iter()
        .map(|agent| agent.id.as_str())
        .collect::<BTreeSet<_>>();
    let mut pane_indexes_by_agent = BTreeMap::new();

    for (index, pane) in panes.iter().enumerate() {
        let Some(agent_id) = pane.agent_id else {
            return SharedTopology::Drifted {
                reason: WorkspaceDriftReason::PaneMetadataMissing,
            };
        };

        if !configured_agent_ids.contains(agent_id) {
            return SharedTopology::Drifted {
                reason: WorkspaceDriftReason::UnknownManagedAgentId(agent_id.to_string()),
            };
        }

        if pane_indexes_by_agent
            .insert(agent_id.to_string(), index)
            .is_some()
        {
            return SharedTopology::Drifted {
                reason: WorkspaceDriftReason::DuplicateManagedAgentId(agent_id.to_string()),
            };
        }
    }

    let ordered_pane_indexes = match order_managed_pane_indexes(project, &pane_indexes_by_agent) {
        Ok(ordered_pane_indexes) => ordered_pane_indexes,
        Err(reason) => return SharedTopology::Drifted { reason },
    };

    if ordered_pane_indexes
        .iter()
        .any(|index| panes[*index].pane_dead)
    {
        SharedTopology::Degraded {
            ordered_pane_indexes,
        }
    } else {
        SharedTopology::Healthy {
            ordered_pane_indexes,
        }
    }
}

fn classify_session_metadata(
    project: &ResolvedProject,
    fingerprint: Option<&str>,
    project_id: Option<&str>,
    profile_id: Option<&str>,
) -> Option<WorkspaceDriftReason> {
    if fingerprint != Some(project.fingerprint.as_str()) {
        Some(WorkspaceDriftReason::FingerprintMismatch)
    } else if project_id != Some(project.id.as_str()) {
        Some(WorkspaceDriftReason::ProjectMetadataMismatch)
    } else if profile_id != Some(project.profile_id.as_str()) {
        Some(WorkspaceDriftReason::ProfileMetadataMismatch)
    } else {
        None
    }
}

fn classify_window_shape(
    project: &ResolvedProject,
    window_role: Option<&str>,
    pane_count: usize,
) -> Option<WorkspaceDriftReason> {
    if window_role != Some(WINDOW_ROLE_AGENTS) {
        Some(WorkspaceDriftReason::WindowMetadataMismatch)
    } else if pane_count != project.agents.len() {
        Some(WorkspaceDriftReason::PaneCountMismatch)
    } else {
        None
    }
}

fn order_managed_pane_indexes(
    project: &ResolvedProject,
    pane_indexes_by_agent: &BTreeMap<String, usize>,
) -> std::result::Result<Vec<usize>, WorkspaceDriftReason> {
    project
        .agents
        .iter()
        .map(|agent| {
            pane_indexes_by_agent
                .get(agent.id.as_str())
                .copied()
                .ok_or_else(|| WorkspaceDriftReason::MissingManagedPane(agent.id.clone()))
        })
        .collect()
}

fn build_inspected_workspace(
    project: &ResolvedProject,
    panes: &[PaneInfo],
    ordered_pane_indexes: Vec<usize>,
) -> InspectedWorkspace {
    InspectedWorkspace {
        panes: ordered_pane_indexes
            .into_iter()
            .enumerate()
            .map(|(agent_index, pane_index)| ManagedPane {
                pane: panes[pane_index].clone(),
                agent: project.agents[agent_index].clone(),
            })
            .collect(),
    }
}

pub(crate) fn inspect(
    tmux: &dyn TmuxAdapter,
    project: &ResolvedProject,
) -> Result<WorkspaceTopology> {
    let session = session_name(project);

    if !session_exists(tmux, &session)? {
        return Ok(WorkspaceTopology::Absent);
    }

    let fingerprint = tmux.get_session_option(&session, SESSION_CONFIG_FINGERPRINT)?;
    let project_id = tmux.get_session_option(&session, SESSION_PROJECT_ID)?;
    let profile_id = tmux.get_session_option(&session, SESSION_PROFILE_ID)?;
    if let Some(reason) = classify_session_metadata(
        project,
        fingerprint.as_deref(),
        project_id.as_deref(),
        profile_id.as_deref(),
    ) {
        return Ok(WorkspaceTopology::Drifted { reason });
    }

    let window_target = window_target(&session, &project.window_name);
    // Keep managed-window transport failures outside the shared classifier so
    // inspect() preserves the exact lifecycle-facing ManagedWindowMissing reason.
    let Ok(window_role) = tmux.get_window_option(&window_target, WINDOW_ROLE) else {
        return Ok(WorkspaceTopology::Drifted {
            reason: WorkspaceDriftReason::ManagedWindowMissing,
        });
    };
    let Ok(panes) = tmux.list_panes(Some(&window_target)) else {
        return Ok(WorkspaceTopology::Drifted {
            reason: WorkspaceDriftReason::ManagedWindowMissing,
        });
    };
    if let Some(reason) = classify_window_shape(project, window_role.as_deref(), panes.len()) {
        return Ok(WorkspaceTopology::Drifted { reason });
    }

    let pane_agent_ids = panes
        .iter()
        .map(|pane| tmux.get_pane_option(&pane.pane_id, PANE_AGENT_ID))
        .collect::<Result<Vec<_>>>()?;

    let raw_panes = panes
        .iter()
        .zip(pane_agent_ids.iter())
        .map(|(pane, agent_id)| RawWorkspacePane {
            agent_id: agent_id.as_deref(),
            pane_dead: pane.pane_dead,
        })
        .collect::<Vec<_>>();
    let shared = classify_managed_panes(project, &raw_panes);

    match shared {
        SharedTopology::Healthy {
            ordered_pane_indexes,
        } => Ok(WorkspaceTopology::Healthy(build_inspected_workspace(
            project,
            &panes,
            ordered_pane_indexes,
        ))),
        SharedTopology::Degraded {
            ordered_pane_indexes,
        } => Ok(WorkspaceTopology::Degraded(build_inspected_workspace(
            project,
            &panes,
            ordered_pane_indexes,
        ))),
        SharedTopology::Drifted { reason } => Ok(WorkspaceTopology::Drifted { reason }),
    }
}

pub(crate) fn session_exists(tmux: &dyn TmuxAdapter, session: &str) -> Result<bool> {
    match tmux.session_exists(session) {
        Ok(exists) => Ok(exists),
        Err(error)
            if matches!(
                error.downcast_ref::<TmuxError>(),
                Some(TmuxError::NoServer(_))
            ) =>
        {
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::WorkspaceDriftReason;
    use crate::test_support::{FakeTmux, TestResultExt, setup_healthy_session, test_project};
    use crate::workspace::session_name;

    fn raw_snapshot<'a>(
        project: &'a ResolvedProject,
        panes: Vec<RawWorkspacePane<'a>>,
    ) -> RawWorkspaceSnapshot<'a> {
        RawWorkspaceSnapshot {
            fingerprint: Some(project.fingerprint.as_str()),
            project_id: Some(project.id.as_str()),
            profile_id: Some(project.profile_id.as_str()),
            window_role: Some(WINDOW_ROLE_AGENTS),
            panes,
        }
    }

    fn raw_pane(agent_id: Option<&str>, pane_dead: bool) -> RawWorkspacePane<'_> {
        RawWorkspacePane {
            agent_id,
            pane_dead,
        }
    }

    fn classify(project: &ResolvedProject, snapshot: &RawWorkspaceSnapshot<'_>) -> SharedTopology {
        classify_snapshot(project, snapshot)
    }

    fn session_name_for(project: &ResolvedProject) -> String {
        session_name(project)
    }

    #[test]
    fn inspect_absent_session() {
        let fake = FakeTmux::new();
        let project = test_project();
        let result = inspect(&fake, &project).or_panic();
        assert!(matches!(result, WorkspaceTopology::Absent));
    }

    #[test]
    fn inspect_healthy_session() {
        let fake = FakeTmux::new();
        let project = test_project();
        setup_healthy_session(&fake, &project);
        let result = inspect(&fake, &project).or_panic();
        assert!(matches!(result, WorkspaceTopology::Healthy(_)));
    }

    #[test]
    fn inspect_degraded_with_dead_pane() {
        let fake = FakeTmux::new();
        let project = test_project();
        let session = session_name_for(&project);

        fake.add_session(&session);
        fake.set_session_opt(
            &session,
            "@kira_mux_config_fingerprint",
            &project.fingerprint,
        );
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
        fake.add_pane(&session, &project.window_name, "%1", true);
        fake.set_pane_opt(
            &session,
            &project.window_name,
            1,
            "@kira_mux_agent_id",
            "beta",
        );

        let result = inspect(&fake, &project).or_panic();
        assert!(matches!(result, WorkspaceTopology::Degraded(_)));
    }

    #[test]
    fn inspect_drifted_fingerprint_mismatch() {
        let fake = FakeTmux::new();
        let project = test_project();
        let session = session_name_for(&project);

        fake.add_session(&session);
        fake.set_session_opt(
            &session,
            "@kira_mux_config_fingerprint",
            "wrong-fingerprint",
        );
        fake.set_session_opt(&session, "@kira_mux_project_id", &project.id);

        let result = inspect(&fake, &project).or_panic();
        assert!(matches!(
            result,
            WorkspaceTopology::Drifted {
                reason: WorkspaceDriftReason::FingerprintMismatch
            }
        ));
    }

    #[test]
    fn inspect_drifted_pane_count_mismatch() {
        let fake = FakeTmux::new();
        let project = test_project();
        let session = session_name_for(&project);

        fake.add_session(&session);
        fake.set_session_opt(
            &session,
            "@kira_mux_config_fingerprint",
            &project.fingerprint,
        );
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

        let result = inspect(&fake, &project).or_panic();
        assert!(matches!(
            result,
            WorkspaceTopology::Drifted {
                reason: WorkspaceDriftReason::PaneCountMismatch
            }
        ));
    }

    #[test]
    fn inspect_drifted_unknown_agent_id() {
        let fake = FakeTmux::new();
        let project = test_project();
        let session = session_name_for(&project);

        fake.add_session(&session);
        fake.set_session_opt(
            &session,
            "@kira_mux_config_fingerprint",
            &project.fingerprint,
        );
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
            "unknown-agent",
        );

        let result = inspect(&fake, &project).or_panic();
        assert!(matches!(
            result,
            WorkspaceTopology::Drifted {
                reason: WorkspaceDriftReason::UnknownManagedAgentId(_)
            }
        ));
    }

    #[test]
    fn inspect_drifted_duplicate_agent_id() {
        let fake = FakeTmux::new();
        let project = test_project();
        let session = session_name_for(&project);

        fake.add_session(&session);
        fake.set_session_opt(
            &session,
            "@kira_mux_config_fingerprint",
            &project.fingerprint,
        );
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
            "alpha",
        );

        let result = inspect(&fake, &project).or_panic();
        assert!(matches!(
            result,
            WorkspaceTopology::Drifted {
                reason: WorkspaceDriftReason::DuplicateManagedAgentId(_)
            }
        ));
    }

    #[test]
    fn shared_classifier_reports_healthy_workspace() {
        let project = test_project();
        let result = classify(
            &project,
            &raw_snapshot(
                &project,
                vec![
                    raw_pane(Some("alpha"), false),
                    raw_pane(Some("beta"), false),
                ],
            ),
        );

        assert!(matches!(result, SharedTopology::Healthy { .. }));
    }

    #[test]
    fn shared_classifier_reports_degraded_workspace() {
        let project = test_project();
        let result = classify(
            &project,
            &raw_snapshot(
                &project,
                vec![raw_pane(Some("alpha"), false), raw_pane(Some("beta"), true)],
            ),
        );

        assert!(matches!(result, SharedTopology::Degraded { .. }));
    }

    #[test]
    fn managed_pane_classifier_reports_healthy_workspace() {
        let project = test_project();
        let result = classify_managed_panes(
            &project,
            &[
                raw_pane(Some("alpha"), false),
                raw_pane(Some("beta"), false),
            ],
        );

        assert!(matches!(result, SharedTopology::Healthy { .. }));
    }

    #[test]
    fn managed_pane_classifier_reports_degraded_workspace() {
        let project = test_project();
        let result = classify_managed_panes(
            &project,
            &[raw_pane(Some("alpha"), false), raw_pane(Some("beta"), true)],
        );

        assert!(matches!(result, SharedTopology::Degraded { .. }));
    }

    #[test]
    fn shared_classifier_reports_fingerprint_mismatch() {
        let project = test_project();
        let result = classify(
            &project,
            &RawWorkspaceSnapshot {
                fingerprint: Some("wrong-fingerprint"),
                ..raw_snapshot(
                    &project,
                    vec![
                        raw_pane(Some("alpha"), false),
                        raw_pane(Some("beta"), false),
                    ],
                )
            },
        );

        assert!(matches!(
            result,
            SharedTopology::Drifted {
                reason: WorkspaceDriftReason::FingerprintMismatch
            }
        ));
    }

    #[test]
    fn shared_classifier_reports_project_metadata_mismatch() {
        let project = test_project();
        let result = classify(
            &project,
            &RawWorkspaceSnapshot {
                project_id: Some("other-project"),
                ..raw_snapshot(
                    &project,
                    vec![
                        raw_pane(Some("alpha"), false),
                        raw_pane(Some("beta"), false),
                    ],
                )
            },
        );

        assert!(matches!(
            result,
            SharedTopology::Drifted {
                reason: WorkspaceDriftReason::ProjectMetadataMismatch
            }
        ));
    }

    #[test]
    fn shared_classifier_reports_profile_metadata_mismatch() {
        let project = test_project();
        let result = classify(
            &project,
            &RawWorkspaceSnapshot {
                profile_id: Some("other-profile"),
                ..raw_snapshot(
                    &project,
                    vec![
                        raw_pane(Some("alpha"), false),
                        raw_pane(Some("beta"), false),
                    ],
                )
            },
        );

        assert!(matches!(
            result,
            SharedTopology::Drifted {
                reason: WorkspaceDriftReason::ProfileMetadataMismatch
            }
        ));
    }

    #[test]
    fn shared_classifier_reports_window_role_mismatch() {
        let project = test_project();
        let result = classify(
            &project,
            &RawWorkspaceSnapshot {
                window_role: Some("other-role"),
                ..raw_snapshot(
                    &project,
                    vec![
                        raw_pane(Some("alpha"), false),
                        raw_pane(Some("beta"), false),
                    ],
                )
            },
        );

        assert!(matches!(
            result,
            SharedTopology::Drifted {
                reason: WorkspaceDriftReason::WindowMetadataMismatch
            }
        ));
    }

    #[test]
    fn shared_classifier_reports_pane_count_mismatch() {
        let project = test_project();
        let result = classify(
            &project,
            &raw_snapshot(&project, vec![raw_pane(Some("alpha"), false)]),
        );

        assert!(matches!(
            result,
            SharedTopology::Drifted {
                reason: WorkspaceDriftReason::PaneCountMismatch
            }
        ));
    }

    #[test]
    fn shared_classifier_reports_missing_pane_metadata() {
        let project = test_project();
        let result = classify(
            &project,
            &raw_snapshot(
                &project,
                vec![raw_pane(Some("alpha"), false), raw_pane(None, false)],
            ),
        );

        assert!(matches!(
            result,
            SharedTopology::Drifted {
                reason: WorkspaceDriftReason::PaneMetadataMissing
            }
        ));
    }

    #[test]
    fn shared_classifier_reports_unknown_agent_id() {
        let project = test_project();
        let result = classify(
            &project,
            &raw_snapshot(
                &project,
                vec![
                    raw_pane(Some("alpha"), false),
                    raw_pane(Some("unknown"), false),
                ],
            ),
        );

        assert!(matches!(
            result,
            SharedTopology::Drifted {
                reason: WorkspaceDriftReason::UnknownManagedAgentId(_)
            }
        ));
    }

    #[test]
    fn shared_classifier_reports_duplicate_agent_id() {
        let project = test_project();
        let result = classify(
            &project,
            &raw_snapshot(
                &project,
                vec![
                    raw_pane(Some("alpha"), false),
                    raw_pane(Some("alpha"), false),
                ],
            ),
        );

        assert!(matches!(
            result,
            SharedTopology::Drifted {
                reason: WorkspaceDriftReason::DuplicateManagedAgentId(_)
            }
        ));
    }

    #[test]
    fn shared_classifier_reports_missing_managed_pane() {
        let project = test_project();
        let pane_indexes_by_agent = BTreeMap::from([(String::from("alpha"), 0usize)]);

        let result = order_managed_pane_indexes(&project, &pane_indexes_by_agent);

        assert!(matches!(
            result,
            Err(WorkspaceDriftReason::MissingManagedPane(_))
        ));
    }
}
