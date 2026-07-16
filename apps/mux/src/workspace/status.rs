use anyhow::Result;

use crate::config::ResolutionMode;
use crate::inspector::{self, InspectedWorkspace, SharedTopology, WorkspaceTopology};
use crate::model::{
    AgentState, AgentStatus, ProjectState, ProjectStatus, ProjectSummary, ResolvedProject,
};
use crate::paths::AppPaths;
use crate::tmux::{PaneInfo, TmuxAdapter, TmuxClient, TmuxError, WorkspaceSnapshot};
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
    let loaded = crate::config::load_projects(&paths, ResolutionMode::Deferred)?;

    let mut summaries = Vec::new();
    for project in loaded.projects {
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
            path: None,
            error: None,
        });
    }

    for failure in loaded.failures {
        summaries.push(summary_from_config_failure(failure));
    }

    // Stable order: valid projects first (file sort order from loader), then
    // failures by path / id so JSON diffs stay readable.
    summaries.sort_by(|a, b| {
        a.id.cmp(&b.id)
            .then_with(|| a.profile_id.cmp(&b.profile_id))
            .then_with(|| a.path.cmp(&b.path))
    });

    Ok(summaries)
}

fn summary_from_config_failure(failure: crate::config::ProjectConfigFailure) -> ProjectSummary {
    ProjectSummary {
        id: failure
            .project_id
            .unwrap_or_else(|| "<unknown>".to_string()),
        profile_id: failure.profile_id.unwrap_or_else(|| "-".to_string()),
        name: String::new(),
        root: String::new(),
        state: ProjectState::ConfigError,
        agent_count: 0,
        path: Some(failure.path.display().to_string()),
        error: Some(failure.error),
    }
}

fn summarize_project(tmux: &TmuxClient, project: &ResolvedProject) -> Result<ProjectState> {
    let session = session_name(project);
    match tmux.workspace_snapshot(&session, &project.window_name) {
        Ok(snapshot) => Ok(project_state_from_snapshot(project, snapshot.as_ref())),
        Err(error) => match classified_summary_error(&error) {
            Some(state) => Ok(state),
            None => Err(error),
        },
    }
}

/// Classify a successful workspace snapshot payload for `list`.
///
/// `None` means no session (or no server) — stopped. A present snapshot is
/// fed through the shared topology classifier so list/status agree on drift.
fn project_state_from_snapshot(
    project: &ResolvedProject,
    snapshot: Option<&WorkspaceSnapshot>,
) -> ProjectState {
    let Some(snap) = snapshot else {
        return ProjectState::Stopped;
    };

    let shared = inspector::classify_workspace_snapshot(project, snap);

    match shared {
        SharedTopology::Healthy { .. } => ProjectState::Running,
        SharedTopology::Degraded { .. } => ProjectState::Degraded,
        SharedTopology::Drifted { .. } => ProjectState::Drifted,
    }
}

/// Map typed tmux failures from workspace inspection to a list state.
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

/// Map tmux pane liveness to agent state.
///
/// Alive ⇒ [`AgentState::Running`] even if the tool is mid-setup. Kira does
/// not parse pane contents for readiness (operator-managed; see README).
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
    use crate::tmux::{WorkspacePaneSnapshot, WorkspaceWindowSnapshot};

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
        let snap = workspace_snapshot(&project, false);
        assert_eq!(
            project_state_from_snapshot(&project, Some(&snap)),
            ProjectState::Running
        );
    }

    #[test]
    fn project_state_from_snapshot_dead_pane_is_degraded() {
        let project = crate::test_support::test_project();
        let snap = workspace_snapshot(&project, true);
        assert_eq!(
            project_state_from_snapshot(&project, Some(&snap)),
            ProjectState::Degraded
        );
    }

    #[test]
    fn project_state_from_snapshot_missing_metadata_is_drifted_not_error() {
        // A successful but untagged payload is real drift. Command failures
        // must not become an empty snapshot that is misclassified as drift.
        let project = crate::test_support::test_project();
        let snap = WorkspaceSnapshot {
            fingerprint: None,
            project_id: None,
            profile_id: None,
            window: Some(WorkspaceWindowSnapshot {
                role: None,
                panes: Vec::new(),
            }),
        };
        assert_eq!(
            project_state_from_snapshot(&project, Some(&snap)),
            ProjectState::Drifted
        );
    }

    #[test]
    fn project_state_from_snapshot_fingerprint_mismatch_is_drifted() {
        let project = crate::test_support::test_project();
        let mut snap = workspace_snapshot(&project, false);
        snap.fingerprint = Some("wrong".into());
        assert_eq!(
            project_state_from_snapshot(&project, Some(&snap)),
            ProjectState::Drifted
        );
    }

    #[test]
    fn project_state_from_snapshot_missing_window_is_drifted() {
        let project = crate::test_support::test_project();
        let mut snap = workspace_snapshot(&project, false);
        snap.window = None;

        assert_eq!(
            project_state_from_snapshot(&project, Some(&snap)),
            ProjectState::Drifted
        );
    }

    fn workspace_snapshot(project: &ResolvedProject, first_pane_dead: bool) -> WorkspaceSnapshot {
        WorkspaceSnapshot {
            fingerprint: Some(project.fingerprint.clone()),
            project_id: Some(project.id.clone()),
            profile_id: Some(project.profile_id.clone()),
            window: Some(WorkspaceWindowSnapshot {
                role: Some(WINDOW_ROLE_AGENTS.to_string()),
                panes: vec![
                    WorkspacePaneSnapshot {
                        pane: PaneInfo {
                            pane_id: "%0".into(),
                            pane_dead: first_pane_dead,
                            pane_dead_status: first_pane_dead.then_some(1),
                        },
                        agent_id: Some("alpha".into()),
                    },
                    WorkspacePaneSnapshot {
                        pane: PaneInfo {
                            pane_id: "%1".into(),
                            pane_dead: false,
                            pane_dead_status: None,
                        },
                        agent_id: Some("beta".into()),
                    },
                ],
            }),
        }
    }
}
