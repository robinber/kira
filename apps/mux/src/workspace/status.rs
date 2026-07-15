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
use crate::tmux::{PaneInfo, TmuxAdapter, TmuxClient};
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
    let snapshot = tmux.snapshot_summary(&session, &project.window_name)?;

    let Some(snap) = snapshot else {
        return Ok(ProjectState::Stopped);
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
                .map(|pane| RawWorkspacePane {
                    agent_id: pane.agent_id.as_deref(),
                    pane_dead: pane.pane_dead,
                })
                .collect(),
        },
    );

    Ok(match shared {
        SharedTopology::Healthy { .. } => ProjectState::Running,
        SharedTopology::Degraded { .. } => ProjectState::Degraded,
        SharedTopology::Drifted { .. } => ProjectState::Drifted,
    })
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
