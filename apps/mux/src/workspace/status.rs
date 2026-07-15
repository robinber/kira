use anyhow::Result;

use crate::config::EnvResolutionMode;
use crate::inspector::{
    self, InspectedWorkspace, RawWorkspacePane, RawWorkspaceSnapshot, SharedTopology,
    WorkspaceTopology,
};
use crate::model::{
    AgentState, AgentStatus, ProjectState, ProjectStatus, ProjectSummary, ResolvedProject,
};
use crate::paths::AppPaths;
use crate::tmux::{
    PaneInfo, PaneSummary, TmuxAdapter, TmuxClient, TmuxError, WorkspaceSummarySnapshot,
};
use crate::workspace::session_name;

pub(crate) fn project_status(
    tmux: &dyn TmuxAdapter,
    project: &ResolvedProject,
) -> Result<ProjectStatus> {
    let (state, agents) = match inspector::inspect(tmux, project)? {
        WorkspaceTopology::Absent => (
            ProjectState::Stopped,
            offline_agent_statuses(project, AgentState::MissingPane),
        ),
        WorkspaceTopology::Healthy(w) => (ProjectState::Running, live_agent_statuses(&w)),
        WorkspaceTopology::Degraded(w) => (ProjectState::Degraded, live_agent_statuses(&w)),
        WorkspaceTopology::Drifted { .. } => (
            ProjectState::Drifted,
            offline_agent_statuses(project, AgentState::Error),
        ),
    };

    Ok(ProjectStatus {
        id: project.id.clone(),
        profile_id: project.profile_id.clone(),
        name: project.name.clone(),
        root: project.root.display().to_string(),
        state,
        agent_count: agents.len(),
        agents,
    })
}

pub(crate) fn load_project_summaries() -> Result<Vec<ProjectSummary>> {
    let paths = AppPaths::from_env()?;
    let projects = crate::config::load_projects(&paths, EnvResolutionMode::Deferred)?;

    let mut summaries = Vec::new();
    for project in projects {
        let tmux = TmuxClient::from_env(project.tmux_bin.clone());
        let state = match summarize_project(&tmux, &project) {
            Ok(state) => state,
            Err(error) => {
                tracing::warn!(
                    project_id = project.id.as_str(),
                    %error,
                    "failed to query project state, marking as error"
                );
                ProjectState::Error
            }
        };
        summaries.push(ProjectSummary {
            id: project.id,
            profile_id: project.profile_id,
            name: project.name,
            root: project.root.display().to_string(),
            state,
            agent_count: project.agents.len(),
        });
    }

    Ok(summaries)
}

fn summarize_project(tmux: &TmuxClient, project: &ResolvedProject) -> Result<ProjectState> {
    let session = session_name(project);
    match tmux.snapshot_summary(&session, &project.window_name) {
        Ok(snapshot) => Ok(project_state_from_snapshot(project, snapshot.as_ref())),
        Err(error) => match classified_summary_error(&error) {
            Some(state) => Ok(state),
            None => Err(error),
        },
    }
}

/// Classify a successful `snapshot_summary` payload for `list`.
///
/// `None` means no session (or no server) — stopped. A present snapshot is
/// fed through the shared topology classifier so list/status agree on drift.
fn project_state_from_snapshot(
    project: &ResolvedProject,
    snapshot: Option<&WorkspaceSummarySnapshot>,
) -> ProjectState {
    let Some(snap) = snapshot else {
        return ProjectState::Stopped;
    };

    let shared = inspector::classify_snapshot(
        project,
        &RawWorkspaceSnapshot {
            fingerprint: snap.fingerprint.as_deref(),
            project_id: snap.project_id.as_deref(),
            profile_id: snap.profile_id.as_deref(),
            window_role: snap.window_role.as_deref(),
            panes: snap
                .panes
                .iter()
                .map(|pane: &PaneSummary| RawWorkspacePane {
                    agent_id: pane.agent_id.as_deref(),
                    pane_dead: pane.pane_dead,
                })
                .collect(),
        },
    );

    match shared {
        SharedTopology::Healthy { .. } => ProjectState::Running,
        SharedTopology::Degraded { .. } => ProjectState::Degraded,
        SharedTopology::Drifted { .. } => ProjectState::Drifted,
    }
}

/// Map typed tmux failures from `snapshot_summary` to a list state.
///
/// Returns `None` when the error is not classifiable (transport / generic
/// command failure) so the caller can surface `ProjectState::Error` instead
/// of lying with a false Drifted.
fn classified_summary_error(error: &anyhow::Error) -> Option<ProjectState> {
    match error.downcast_ref::<TmuxError>() {
        Some(TmuxError::NoServer(_) | TmuxError::MissingSession(_)) => Some(ProjectState::Stopped),
        Some(TmuxError::MissingTarget(_)) => Some(ProjectState::Drifted),
        Some(TmuxError::CommandFailure(_)) | None => None,
    }
}

fn live_agent_statuses(workspace: &InspectedWorkspace) -> Vec<AgentStatus> {
    workspace
        .panes
        .iter()
        .map(|managed| AgentStatus {
            id: managed.agent.id.clone(),
            state: agent_state_from_pane(&managed.pane),
            label: Some(managed.agent.label.clone()),
            command: managed
                .agent
                .command
                .clone()
                .or_else(|| managed.agent.shell_command.clone()),
            pane_id: Some(managed.pane.pane_id.clone()),
        })
        .collect()
}

fn offline_agent_statuses(project: &ResolvedProject, state: AgentState) -> Vec<AgentStatus> {
    project
        .agents
        .iter()
        .map(|agent| AgentStatus {
            id: agent.id.clone(),
            state,
            label: Some(agent.label.clone()),
            command: agent
                .command
                .clone()
                .or_else(|| agent.shell_command.clone()),
            pane_id: None,
        })
        .collect()
}

fn agent_state_from_pane(pane: &PaneInfo) -> AgentState {
    if !pane.pane_dead {
        AgentState::Running
    } else if pane.pane_dead_status == Some(0) {
        AgentState::ExitedClean
    } else {
        AgentState::ExitedFailed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tmux::metadata::WINDOW_ROLE_AGENTS;

    #[test]
    fn classified_summary_error_maps_missing_target_to_drifted() {
        let error = TmuxError::MissingTarget("s:agents".into()).into();
        assert_eq!(
            classified_summary_error(&error),
            Some(ProjectState::Drifted)
        );
    }

    #[test]
    fn classified_summary_error_maps_missing_session_to_stopped() {
        let error = TmuxError::MissingSession("s".into()).into();
        assert_eq!(
            classified_summary_error(&error),
            Some(ProjectState::Stopped)
        );
    }

    #[test]
    fn classified_summary_error_maps_no_server_to_stopped() {
        let error = TmuxError::NoServer("no server running on /tmp/tmux".into()).into();
        assert_eq!(
            classified_summary_error(&error),
            Some(ProjectState::Stopped)
        );
    }

    #[test]
    fn classified_summary_error_leaves_command_failure_unclassified() {
        let error = TmuxError::CommandFailure("server unexpectedly closed".into()).into();
        assert_eq!(classified_summary_error(&error), None);
    }

    #[test]
    fn classified_summary_error_leaves_untyped_errors_unclassified() {
        let error = anyhow::anyhow!("io transport glitch");
        assert_eq!(classified_summary_error(&error), None);
    }

    #[test]
    fn project_state_from_snapshot_none_is_stopped() {
        let project = crate::test_support::test_project();
        assert_eq!(
            project_state_from_snapshot(&project, None),
            ProjectState::Stopped
        );
    }

    #[test]
    fn project_state_from_snapshot_healthy_is_running() {
        let project = crate::test_support::test_project();
        let snap = WorkspaceSummarySnapshot {
            fingerprint: Some(project.fingerprint.clone()),
            project_id: Some(project.id.clone()),
            profile_id: Some(project.profile_id.clone()),
            window_role: Some(WINDOW_ROLE_AGENTS.to_string()),
            panes: vec![
                PaneSummary {
                    pane_dead: false,
                    agent_id: Some("alpha".into()),
                },
                PaneSummary {
                    pane_dead: false,
                    agent_id: Some("beta".into()),
                },
            ],
        };
        assert_eq!(
            project_state_from_snapshot(&project, Some(&snap)),
            ProjectState::Running
        );
    }

    #[test]
    fn project_state_from_snapshot_dead_pane_is_degraded() {
        let project = crate::test_support::test_project();
        let snap = WorkspaceSummarySnapshot {
            fingerprint: Some(project.fingerprint.clone()),
            project_id: Some(project.id.clone()),
            profile_id: Some(project.profile_id.clone()),
            window_role: Some(WINDOW_ROLE_AGENTS.to_string()),
            panes: vec![
                PaneSummary {
                    pane_dead: true,
                    agent_id: Some("alpha".into()),
                },
                PaneSummary {
                    pane_dead: false,
                    agent_id: Some("beta".into()),
                },
            ],
        };
        assert_eq!(
            project_state_from_snapshot(&project, Some(&snap)),
            ProjectState::Degraded
        );
    }

    #[test]
    fn project_state_from_snapshot_empty_default_is_drifted_not_error() {
        // A *successful* empty payload (e.g. untagged session) is real drift.
        // Command failures must not reach here as Default — that was the bug.
        let project = crate::test_support::test_project();
        let snap = WorkspaceSummarySnapshot::default();
        assert_eq!(
            project_state_from_snapshot(&project, Some(&snap)),
            ProjectState::Drifted
        );
    }

    #[test]
    fn project_state_from_snapshot_fingerprint_mismatch_is_drifted() {
        let project = crate::test_support::test_project();
        let snap = WorkspaceSummarySnapshot {
            fingerprint: Some("wrong".into()),
            project_id: Some(project.id.clone()),
            profile_id: Some(project.profile_id.clone()),
            window_role: Some(WINDOW_ROLE_AGENTS.to_string()),
            panes: vec![
                PaneSummary {
                    pane_dead: false,
                    agent_id: Some("alpha".into()),
                },
                PaneSummary {
                    pane_dead: false,
                    agent_id: Some("beta".into()),
                },
            ],
        };
        assert_eq!(
            project_state_from_snapshot(&project, Some(&snap)),
            ProjectState::Drifted
        );
    }
}
