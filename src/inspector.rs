//! Topology classification and live inspection for managed workspaces.
//!
//! `inspect` reads tmux state; `classify_*` maps snapshots to healthy,
//! degraded, drifted, or absent — the single topology truth for pane I/O.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;

use crate::error::{KiraMuxError, WorkspaceDriftReason};
use crate::model::{ResolvedAgent, ResolvedProject};
use crate::tmux::metadata::WINDOW_ROLE_AGENTS;
use crate::tmux::{
    PaneInfo, TmuxAdapter, TmuxError, WorkspacePaneSnapshot, WorkspaceSnapshot,
    WorkspaceWindowSnapshot,
};
use crate::workspace::session_name;

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

/// Shared classification used by inspect, status, and agent listing.
#[derive(Debug)]
pub(crate) enum SharedTopology {
    Healthy { ordered_pane_indexes: Vec<usize> },
    Degraded { ordered_pane_indexes: Vec<usize> },
    Drifted { reason: WorkspaceDriftReason },
}

/// Classify a bulk [`WorkspaceSnapshot`] against the resolved project contract.
pub(crate) fn classify_snapshot(
    project: &ResolvedProject,
    snapshot: &WorkspaceSnapshot,
) -> SharedTopology {
    if let Some(reason) = classify_session_metadata(
        project,
        snapshot.fingerprint.as_deref(),
        snapshot.project_id.as_deref(),
        snapshot.profile_id.as_deref(),
    ) {
        return SharedTopology::Drifted { reason };
    }

    let Some(window) = &snapshot.window else {
        return SharedTopology::Drifted {
            reason: WorkspaceDriftReason::ManagedWindowMissing,
        };
    };

    if let Some(reason) = classify_window_shape(project, window.role.as_deref(), window.panes.len())
    {
        return SharedTopology::Drifted { reason };
    }

    classify_managed_panes(project, &window.panes)
}

fn classify_managed_panes(
    project: &ResolvedProject,
    panes: &[WorkspacePaneSnapshot],
) -> SharedTopology {
    let configured_agent_ids = project
        .agents
        .iter()
        .map(|agent| agent.id.as_str())
        .collect::<BTreeSet<_>>();
    let mut pane_indexes_by_agent = BTreeMap::<&str, usize>::new();

    for (index, pane) in panes.iter().enumerate() {
        let Some(agent_id) = pane.agent_id.as_deref() else {
            return SharedTopology::Drifted {
                reason: WorkspaceDriftReason::PaneMetadataMissing,
            };
        };

        if !configured_agent_ids.contains(agent_id) {
            return SharedTopology::Drifted {
                reason: WorkspaceDriftReason::UnknownManagedAgentId(agent_id.to_string()),
            };
        }

        if pane_indexes_by_agent.insert(agent_id, index).is_some() {
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
        .any(|index| panes[*index].pane.pane_dead)
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
    if fingerprint == Some(project.fingerprint.as_str()) {
        classify_session_ownership(project, project_id, profile_id)
    } else {
        Some(WorkspaceDriftReason::FingerprintMismatch)
    }
}

/// Classify only the metadata that proves a session belongs to a project and
/// profile. Destructive commands use this subset so config fingerprint drift
/// does not prevent cleanup of an otherwise owned session.
pub(crate) fn classify_session_ownership(
    project: &ResolvedProject,
    project_id: Option<&str>,
    profile_id: Option<&str>,
) -> Option<WorkspaceDriftReason> {
    if project_id != Some(project.id.as_str()) {
        Some(WorkspaceDriftReason::ProjectMetadataMismatch)
    } else if profile_id != Some(project.profile_id.as_str()) {
        Some(WorkspaceDriftReason::ProfileMetadataMismatch)
    } else {
        None
    }
}

/// Confirm a live session is owned by `project` (project id + profile only).
///
/// Uses the bulk [`TmuxAdapter::workspace_snapshot`] path — the same session
/// metadata source as [`inspect`] / `list` — so ownership cannot disagree with
/// topology classification. Fingerprint drift is intentionally ignored so
/// `kill` can still clean up a session after config changes.
///
/// When the session vanishes between an existence check and this call, returns
/// [`TmuxError::MissingSession`] (same surface as the former per-option reads).
pub(crate) fn ensure_session_owned(
    tmux: &dyn TmuxAdapter,
    project: &ResolvedProject,
) -> Result<()> {
    let session = session_name(project);
    let Some(snapshot) = tmux.workspace_snapshot(&session, &project.window_name)? else {
        return Err(TmuxError::MissingSession(session).into());
    };
    if let Some(reason) = classify_session_ownership(
        project,
        snapshot.project_id.as_deref(),
        snapshot.profile_id.as_deref(),
    ) {
        return Err(KiraMuxError::Drifted {
            project_id: project.id.clone(),
            reason,
        }
        .into());
    }
    Ok(())
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
    pane_indexes_by_agent: &BTreeMap<&str, usize>,
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
    window: &WorkspaceWindowSnapshot,
    ordered_pane_indexes: Vec<usize>,
) -> InspectedWorkspace {
    InspectedWorkspace {
        panes: ordered_pane_indexes
            .into_iter()
            .enumerate()
            .map(|(agent_index, pane_index)| ManagedPane {
                pane: window.panes[pane_index].pane.clone(),
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
    let snapshot = match tmux.workspace_snapshot(&session, &project.window_name) {
        Ok(snapshot) => snapshot,
        // Typed races between the existence check and the metadata reads
        // classify as the state a re-inspection would report, so every
        // inspect() consumer (status, list, agents, send/capture) agrees
        // instead of surfacing a generic transport error.
        Err(error) => match error.downcast_ref::<TmuxError>() {
            Some(TmuxError::NoServer(_) | TmuxError::MissingSession(_)) => {
                return Ok(WorkspaceTopology::Absent);
            }
            // Coarse by necessity: the client already softens list-panes
            // MissingTarget into `window: None`, so an escaping
            // MissingTarget comes from the session-scoped metadata read and
            // window absence is the closest drift reason available.
            Some(TmuxError::MissingTarget(_)) => {
                return Ok(WorkspaceTopology::Drifted {
                    reason: WorkspaceDriftReason::ManagedWindowMissing,
                });
            }
            Some(TmuxError::CommandFailure(_)) | None => return Err(error),
        },
    };
    let Some(snapshot) = snapshot else {
        return Ok(WorkspaceTopology::Absent);
    };
    let shared = classify_snapshot(project, &snapshot);

    let (ordered_pane_indexes, degraded) = match shared {
        SharedTopology::Healthy {
            ordered_pane_indexes,
        } => (ordered_pane_indexes, false),
        SharedTopology::Degraded {
            ordered_pane_indexes,
        } => (ordered_pane_indexes, true),
        SharedTopology::Drifted { reason } => {
            return Ok(WorkspaceTopology::Drifted { reason });
        }
    };
    let Some(window) = snapshot.window.as_ref() else {
        return Ok(WorkspaceTopology::Drifted {
            reason: WorkspaceDriftReason::ManagedWindowMissing,
        });
    };
    let workspace = build_inspected_workspace(project, window, ordered_pane_indexes);
    if degraded {
        Ok(WorkspaceTopology::Degraded(workspace))
    } else {
        Ok(WorkspaceTopology::Healthy(workspace))
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
mod snapshot_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::WorkspaceDriftReason;
    use crate::test_support::{FakeTmux, TestResultExt, setup_healthy_session, test_project};

    /// Typed snapshot for classifier unit tests (no `FakeTmux`).
    /// Mutate one field after construction when a test needs a single
    /// deviation.
    fn snapshot(project: &ResolvedProject, panes: &[(Option<&str>, bool)]) -> WorkspaceSnapshot {
        WorkspaceSnapshot {
            fingerprint: Some(project.fingerprint.clone()),
            project_id: Some(project.id.clone()),
            profile_id: Some(project.profile_id.clone()),
            window: Some(WorkspaceWindowSnapshot {
                role: Some(WINDOW_ROLE_AGENTS.to_string()),
                panes: panes
                    .iter()
                    .enumerate()
                    .map(|(index, (agent_id, pane_dead))| WorkspacePaneSnapshot {
                        pane: PaneInfo {
                            pane_id: format!("%{index}"),
                            pane_dead: *pane_dead,
                            pane_dead_status: pane_dead.then_some(1),
                            alternate_on: false,
                            pane_height: 24,
                        },
                        agent_id: agent_id.map(str::to_string),
                    })
                    .collect(),
            }),
        }
    }

    #[test]
    fn inspect_absent_session() {
        let fake = FakeTmux::new();
        let project = test_project();
        let result = inspect(&fake, &project).or_panic("inspect_absent_session");
        assert!(matches!(result, WorkspaceTopology::Absent));
    }

    #[test]
    fn inspect_healthy_session() {
        let fake = FakeTmux::new();
        let project = test_project();
        setup_healthy_session(&fake, &project);
        let result = inspect(&fake, &project).or_panic("inspect_healthy_session");
        assert!(matches!(result, WorkspaceTopology::Healthy(_)));
    }

    #[test]
    fn inspect_degraded_with_dead_pane() {
        let fake = FakeTmux::new();
        let project = test_project();
        crate::test_support::setup_session_with_dead_panes(&fake, &project, &[1]);

        let result = inspect(&fake, &project).or_panic("inspect_degraded_with_dead_pane");
        assert!(matches!(result, WorkspaceTopology::Degraded(_)));
    }

    #[test]
    fn shared_classifier_reports_healthy_workspace() {
        let project = test_project();
        let result = classify_snapshot(
            &project,
            &snapshot(&project, &[(Some("alpha"), false), (Some("beta"), false)]),
        );

        assert!(matches!(result, SharedTopology::Healthy { .. }));
    }

    #[test]
    fn shared_classifier_reports_degraded_workspace() {
        let project = test_project();
        let result = classify_snapshot(
            &project,
            &snapshot(&project, &[(Some("alpha"), false), (Some("beta"), true)]),
        );

        assert!(matches!(result, SharedTopology::Degraded { .. }));
    }

    #[test]
    fn shared_classifier_reports_fingerprint_mismatch() {
        let project = test_project();
        let mut snap = snapshot(&project, &[(Some("alpha"), false), (Some("beta"), false)]);
        snap.fingerprint = Some("wrong-fingerprint".to_string());
        let result = classify_snapshot(&project, &snap);

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
        let mut snap = snapshot(&project, &[(Some("alpha"), false), (Some("beta"), false)]);
        snap.project_id = Some("other-project".to_string());
        let result = classify_snapshot(&project, &snap);

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
        let mut snap = snapshot(&project, &[(Some("alpha"), false), (Some("beta"), false)]);
        snap.profile_id = Some("other-profile".to_string());
        let result = classify_snapshot(&project, &snap);

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
        let mut snap = snapshot(&project, &[(Some("alpha"), false), (Some("beta"), false)]);
        if let Some(window) = snap.window.as_mut() {
            window.role = Some("other-role".to_string());
        }
        let result = classify_snapshot(&project, &snap);

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
        let result = classify_snapshot(&project, &snapshot(&project, &[(Some("alpha"), false)]));

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
        let result = classify_snapshot(
            &project,
            &snapshot(&project, &[(Some("alpha"), false), (None, false)]),
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
        let result = classify_snapshot(
            &project,
            &snapshot(
                &project,
                &[(Some("alpha"), false), (Some("unknown"), false)],
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
        let result = classify_snapshot(
            &project,
            &snapshot(&project, &[(Some("alpha"), false), (Some("alpha"), false)]),
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
        let pane_indexes_by_agent = BTreeMap::from([("alpha", 0usize)]);

        let result = order_managed_pane_indexes(&project, &pane_indexes_by_agent);

        assert!(matches!(
            result,
            Err(WorkspaceDriftReason::MissingManagedPane(_))
        ));
    }
}
