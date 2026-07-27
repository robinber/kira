//! Build `status` / `list` summaries from config + live inspection.

use anyhow::Result;

use crate::config::ResolutionMode;
use crate::inspector::{self, InspectedWorkspace, WorkspaceTopology};
use crate::model::{
    AgentState, AgentStatus, PaneLiveness, ProjectState, ProjectStatus, ProjectSummary,
    ResolvedProject,
};
use crate::paths::AppPaths;
use crate::tmux::{TmuxAdapter, TmuxClient};

pub(crate) fn project_status(
    tmux: &dyn TmuxAdapter,
    project: &ResolvedProject,
) -> Result<ProjectStatus> {
    let topology = inspector::inspect(tmux, project)?;
    let state = ProjectState::from(&topology);
    let agents = match &topology {
        WorkspaceTopology::Absent => offline_agent_statuses(project, AgentState::MissingPane),
        WorkspaceTopology::Healthy(w) | WorkspaceTopology::Degraded(w) => live_agent_statuses(w),
        WorkspaceTopology::Drifted { .. } => offline_agent_statuses(project, AgentState::Error),
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

/// One state per project for `list`: the same `inspect()` classification
/// `status` uses, projected through the shared topology→state mapping.
/// Unclassifiable failures propagate so the caller reports `Error` instead
/// of a false Stopped/Drifted.
fn summarize_project(tmux: &dyn TmuxAdapter, project: &ResolvedProject) -> Result<ProjectState> {
    Ok(ProjectState::from(&inspector::inspect(tmux, project)?))
}

fn live_agent_statuses(workspace: &InspectedWorkspace) -> Vec<AgentStatus> {
    workspace
        .panes
        .iter()
        .map(|managed| AgentStatus {
            id: managed.agent.id.clone(),
            state: PaneLiveness::from_pane(&managed.pane).into(),
            label: Some(managed.agent.label.clone()),
            command: managed.agent.display_command(),
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
            command: agent.display_command(),
            pane_id: None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        FakeTmux, TestResultExt, setup_healthy_session, setup_session_with_dead_panes, test_project,
    };
    use crate::tmux::TmuxError;

    #[test]
    fn summarize_absent_session_is_stopped() {
        let fake = FakeTmux::new();
        let project = test_project();

        let state = summarize_project(&fake, &project).or_panic("summarize_absent");

        assert_eq!(state, ProjectState::Stopped);
    }

    #[test]
    fn summarize_healthy_session_is_running() {
        let fake = FakeTmux::new();
        let project = test_project();
        setup_healthy_session(&fake, &project);

        let state = summarize_project(&fake, &project).or_panic("summarize_healthy");

        assert_eq!(state, ProjectState::Running);
    }

    #[test]
    fn summarize_dead_pane_is_degraded() {
        let fake = FakeTmux::new();
        let project = test_project();
        setup_session_with_dead_panes(&fake, &project, &[0]);

        let state = summarize_project(&fake, &project).or_panic("summarize_degraded");

        assert_eq!(state, ProjectState::Degraded);
    }

    #[test]
    fn summarize_untagged_session_is_drifted() {
        // A session that exists but carries no kira metadata is drift, not
        // an error — the old snapshot-path test pinned this and the shared
        // classifier must keep it.
        let fake = FakeTmux::new();
        let project = test_project();
        let session = crate::workspace::session_name(&project);
        fake.add_session(&session);
        fake.add_window(&session, &project.window_name);
        fake.add_pane(&session, &project.window_name, "%0", false);

        let state = summarize_project(&fake, &project).or_panic("summarize_untagged");

        assert_eq!(state, ProjectState::Drifted);
    }

    #[test]
    fn vanished_server_race_classifies_as_stopped_for_list_and_status() {
        // NoServer surfacing as a snapshot *error* (not the Ok(None) path)
        // must classify like absence on both sides.
        let fake = FakeTmux::new();
        let project = test_project();
        setup_healthy_session(&fake, &project);

        fake.set_workspace_snapshot_error(TmuxError::NoServer("no server".into()));
        assert_eq!(
            summarize_project(&fake, &project).or_panic("list side"),
            ProjectState::Stopped
        );

        fake.set_workspace_snapshot_error(TmuxError::NoServer("no server".into()));
        assert_eq!(
            project_status(&fake, &project)
                .or_panic("status side")
                .state,
            ProjectState::Stopped
        );
    }

    #[test]
    fn vanished_session_race_classifies_identically_for_list_and_status() {
        // Issue #83: a session vanishing between the existence check and the
        // metadata read made `list` report Stopped while `status` surfaced a
        // generic error. Both go through inspect() now.
        let fake = FakeTmux::new();
        let project = test_project();
        setup_healthy_session(&fake, &project);

        fake.set_workspace_snapshot_error(TmuxError::MissingSession("gone".into()));
        assert_eq!(
            summarize_project(&fake, &project).or_panic("list side"),
            ProjectState::Stopped
        );

        fake.set_workspace_snapshot_error(TmuxError::MissingSession("gone".into()));
        let status = project_status(&fake, &project).or_panic("status side");
        assert_eq!(status.state, ProjectState::Stopped);
    }

    #[test]
    fn vanished_window_race_classifies_as_drifted_for_list_and_status() {
        let fake = FakeTmux::new();
        let project = test_project();
        setup_healthy_session(&fake, &project);

        fake.set_workspace_snapshot_error(TmuxError::MissingTarget("s:agents".into()));
        assert_eq!(
            summarize_project(&fake, &project).or_panic("list side"),
            ProjectState::Drifted
        );

        fake.set_workspace_snapshot_error(TmuxError::MissingTarget("s:agents".into()));
        assert_eq!(
            project_status(&fake, &project)
                .or_panic("status side")
                .state,
            ProjectState::Drifted
        );
    }

    #[test]
    fn command_failure_propagates_for_list_and_status() {
        // Unclassifiable transport failures must stay errors (list renders
        // Error at the caller) rather than lying with Stopped/Drifted.
        let fake = FakeTmux::new();
        let project = test_project();
        setup_healthy_session(&fake, &project);

        fake.set_workspace_snapshot_error(TmuxError::CommandFailure(
            "server unexpectedly closed".into(),
        ));
        summarize_project(&fake, &project).err_or_panic("list side: expected Err");

        fake.set_workspace_snapshot_error(TmuxError::CommandFailure(
            "server unexpectedly closed".into(),
        ));
        project_status(&fake, &project).err_or_panic("status side: expected Err");
    }
}
