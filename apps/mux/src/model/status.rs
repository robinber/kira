use std::collections::BTreeMap;

use serde::Serialize;

use crate::domain::ResolvedProject;
use crate::inspector::{InspectedWorkspace, WorkspaceTopology};

/// Lifecycle state of a managed tmux workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectState {
    /// No managed session is running.
    Stopped,
    /// All managed panes are healthy.
    Running,
    /// The session exists but one or more panes are degraded.
    Degraded,
    /// The session exists but no longer matches the resolved config.
    Drifted,
    /// Status collection failed unexpectedly.
    Error,
}

/// Runtime state of a single agent pane inside a workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AgentState {
    /// The pane is running normally.
    Running,
    /// The pane exited with status 0.
    ExitedClean,
    /// The pane exited with a non-zero status.
    ExitedFailed,
    /// No pane could be matched for this agent.
    MissingPane,
    /// Agent status could not be determined.
    Error,
}

/// Lightweight project overview used by the CLI list command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectSummary {
    /// Stable project ID.
    pub id: String,
    /// Active profile ID.
    pub profile_id: String,
    /// Human-friendly project name.
    pub name: String,
    /// Display form of the project root.
    pub root: String,
    /// Current workspace state.
    pub state: ProjectState,
    /// Number of configured agents.
    pub agent_count: usize,
}

/// Full project status including per-agent state, used by the status command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ProjectStatus {
    /// Stable project ID.
    pub id: String,
    /// Active profile ID.
    pub profile_id: String,
    /// Human-friendly project name.
    pub name: String,
    /// Display form of the project root.
    pub root: String,
    /// Current workspace state.
    pub state: ProjectState,
    /// Number of configured agents.
    pub agent_count: usize,
    /// Per-agent status rows.
    pub agents: Vec<AgentStatus>,
}

/// Runtime status of a single agent within a project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct AgentStatus {
    /// Stable agent ID.
    pub id: String,
    /// Current agent pane state.
    pub state: AgentState,
    /// Human-friendly label from config.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Resolved launch command.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Matched tmux pane ID (e.g. `"%0"`), when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
}

/// Structured output for the `agents` command.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct AgentsOutput {
    /// Display name of the project.
    pub project: String,
    /// Active profile, or `None` for the default profile.
    pub profile: Option<String>,
    /// Agent rows included in the output.
    pub agents: Vec<AgentInfo>,
    /// Declared groups keyed by group name.
    pub groups: BTreeMap<String, Vec<String>>,
}

/// Per-agent info in the agents output.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct AgentInfo {
    /// Stable agent ID.
    pub id: String,
    /// Display command for the agent.
    pub command: String,
    /// Simplified runtime state for the agents view.
    pub state: AgentRunState,
    /// Matched tmux pane ID, when present.
    pub pane_id: Option<String>,
    /// Declared capabilities.
    pub capabilities: Vec<String>,
    /// Group memberships for the agent.
    pub groups: Vec<String>,
}

/// Runtime state of an agent for the agents command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum AgentRunState {
    /// A live pane is present for the agent.
    Running,
    /// A pane exists but is no longer running.
    Dead,
    /// No pane currently exists for the agent.
    Absent,
}

/// Build structured agents output from a resolved project and its workspace
/// topology.
pub(crate) fn build_agents_output(
    project: &ResolvedProject,
    topology: &WorkspaceTopology,
) -> AgentsOutput {
    let workspace: Option<&InspectedWorkspace> = match topology {
        WorkspaceTopology::Healthy(ws) | WorkspaceTopology::Degraded(ws) => Some(ws),
        WorkspaceTopology::Absent | WorkspaceTopology::Drifted { .. } => None,
    };

    let agents = project
        .agents
        .iter()
        .map(|agent| {
            let matched_pane =
                workspace.and_then(|ws| ws.panes.iter().find(|mp| mp.agent.id == agent.id));

            let (state, pane_id) = match matched_pane {
                Some(mp) if !mp.pane.pane_dead => {
                    (AgentRunState::Running, Some(mp.pane.pane_id.clone()))
                }
                Some(mp) => (AgentRunState::Dead, Some(mp.pane.pane_id.clone())),
                None if workspace.is_some() => (AgentRunState::Dead, None),
                None => (AgentRunState::Absent, None),
            };

            let command = agent
                .command
                .clone()
                .or_else(|| agent.shell_command.clone())
                .unwrap_or_default();

            AgentInfo {
                id: agent.id.clone(),
                command,
                state,
                pane_id,
                capabilities: agent.capabilities.clone(),
                groups: project.groups_for(&agent.id),
            }
        })
        .collect();

    let profile = if project.profile_id == "default" {
        None
    } else {
        Some(project.profile_id.clone())
    };

    AgentsOutput {
        project: project.name.clone(),
        profile,
        agents,
        groups: project.groups.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentMode;
    use crate::inspector::{InspectedWorkspace, ManagedPane};
    use crate::test_support::{TestResultExt, test_project};
    use crate::tmux::PaneInfo;

    fn healthy_workspace(project: &ResolvedProject) -> InspectedWorkspace {
        InspectedWorkspace {
            panes: project
                .agents
                .iter()
                .enumerate()
                .map(|(i, agent)| ManagedPane {
                    pane: PaneInfo {
                        pane_id: format!("%{i}"),
                        window_id: "@w-agents".to_string(),
                        pane_dead: false,
                        pane_dead_status: None,
                    },
                    agent: agent.clone(),
                })
                .collect(),
        }
    }

    #[test]
    fn agents_output_healthy_workspace() {
        let project = test_project();
        let ws = healthy_workspace(&project);
        let topology = WorkspaceTopology::Healthy(ws);

        let output = build_agents_output(&project, &topology);

        assert_eq!(output.agents.len(), 2);
        assert_eq!(output.agents[0].id, "alpha");
        assert_eq!(output.agents[0].state, AgentRunState::Running);
        assert_eq!(output.agents[0].pane_id, Some("%0".to_string()));
        assert_eq!(output.agents[1].id, "beta");
        assert_eq!(output.agents[1].state, AgentRunState::Running);
        assert_eq!(output.agents[1].pane_id, Some("%1".to_string()));
    }

    #[test]
    fn agents_output_absent_workspace() {
        let project = test_project();
        let topology = WorkspaceTopology::Absent;

        let output = build_agents_output(&project, &topology);

        assert_eq!(output.agents.len(), 2);
        for agent in &output.agents {
            assert_eq!(agent.state, AgentRunState::Absent);
            assert_eq!(agent.pane_id, None);
        }
    }

    #[test]
    fn agents_output_serializes_to_json() {
        let project = test_project();
        let topology = WorkspaceTopology::Absent;

        let output = build_agents_output(&project, &topology);
        let json = serde_json::to_string(&output).or_panic();

        assert!(json.contains("\"project\""));
        assert!(json.contains("\"agents\""));
        assert!(json.contains("\"absent\""));
        assert!(json.contains("\"alpha\""));
        assert!(json.contains("\"beta\""));
    }

    #[test]
    fn agents_output_includes_capabilities() {
        let mut project = test_project();
        project.agents[0].capabilities = vec!["code".to_string(), "web".to_string()];

        let ws = healthy_workspace(&project);
        let topology = WorkspaceTopology::Healthy(ws);

        let output = build_agents_output(&project, &topology);

        assert_eq!(output.agents[0].capabilities, vec!["code", "web"]);
        assert!(output.agents[1].capabilities.is_empty());
    }

    #[test]
    fn agents_output_includes_groups() {
        let mut project = test_project();
        project.groups.insert(
            "research".to_string(),
            vec!["alpha".to_string(), "beta".to_string()],
        );
        project
            .groups
            .insert("code".to_string(), vec!["alpha".to_string()]);

        let topology = WorkspaceTopology::Absent;
        let output = build_agents_output(&project, &topology);

        assert_eq!(output.groups.len(), 2);
        assert!(output.groups.contains_key("research"));
        assert!(output.groups.contains_key("code"));

        let alpha = &output.agents[0];
        assert!(alpha.groups.contains(&"research".to_string()));
        assert!(alpha.groups.contains(&"code".to_string()));

        let beta = &output.agents[1];
        assert!(beta.groups.contains(&"research".to_string()));
        assert!(!beta.groups.contains(&"code".to_string()));
    }

    #[test]
    fn agents_output_default_profile_is_none() {
        let project = test_project();
        let topology = WorkspaceTopology::Absent;
        let output = build_agents_output(&project, &topology);
        assert_eq!(output.profile, None);
    }

    #[test]
    fn agents_output_non_default_profile() {
        let mut project = test_project();
        project.profile_id = "work".to_string();

        let topology = WorkspaceTopology::Absent;
        let output = build_agents_output(&project, &topology);
        assert_eq!(output.profile, Some("work".to_string()));
    }

    #[test]
    fn agents_output_degraded_workspace_with_dead_pane() {
        let project = test_project();
        let ws = InspectedWorkspace {
            panes: vec![
                ManagedPane {
                    pane: PaneInfo {
                        pane_id: "%0".to_string(),
                        window_id: "@w-agents".to_string(),
                        pane_dead: false,
                        pane_dead_status: None,
                    },
                    agent: project.agents[0].clone(),
                },
                ManagedPane {
                    pane: PaneInfo {
                        pane_id: "%1".to_string(),
                        window_id: "@w-agents".to_string(),
                        pane_dead: true,
                        pane_dead_status: Some(1),
                    },
                    agent: project.agents[1].clone(),
                },
            ],
        };
        let topology = WorkspaceTopology::Degraded(ws);

        let output = build_agents_output(&project, &topology);
        assert_eq!(output.agents[0].state, AgentRunState::Running);
        assert_eq!(output.agents[1].state, AgentRunState::Dead);
        assert_eq!(output.agents[1].pane_id, Some("%1".to_string()));
    }

    #[test]
    fn agents_output_drifted_workspace() {
        let project = test_project();
        let topology = WorkspaceTopology::Drifted {
            reason: crate::error::WorkspaceDriftReason::FingerprintMismatch,
        };

        let output = build_agents_output(&project, &topology);

        for agent in &output.agents {
            assert_eq!(agent.state, AgentRunState::Absent);
            assert_eq!(agent.pane_id, None);
        }
    }

    #[test]
    fn agent_status_serializes_extended_fields() {
        let status = AgentStatus {
            id: "opus".to_string(),
            state: AgentState::Running,
            label: Some("Claude Opus 4.6".to_string()),
            command: Some("claude --model claude-opus-4-6".to_string()),
            pane_id: Some("%0".to_string()),
        };
        let json: serde_json::Value = serde_json::to_value(&status).or_panic();
        assert_eq!(json["id"], "opus");
        assert_eq!(json["state"], "running");
        assert_eq!(json["label"], "Claude Opus 4.6");
        assert_eq!(json["command"], "claude --model claude-opus-4-6");
        assert_eq!(json["pane_id"], "%0");
    }

    #[test]
    fn agent_status_serializes_missing_agent() {
        let status = AgentStatus {
            id: "kimi".to_string(),
            state: AgentState::MissingPane,
            label: Some("Kimi K2".to_string()),
            command: Some("openrouter kimi-k2".to_string()),
            pane_id: None,
        };
        let json: serde_json::Value = serde_json::to_value(&status).or_panic();
        assert_eq!(json["state"], "missing_pane");
        assert!(json["pane_id"].is_null());
    }

    #[test]
    fn agents_output_command_from_shell_command() {
        let mut project = test_project();
        project.agents[0].command = None;
        project.agents[0].mode = AgentMode::Shell;
        project.agents[0].shell_command = Some("bash -c 'echo hello'".to_string());

        let topology = WorkspaceTopology::Absent;
        let output = build_agents_output(&project, &topology);

        assert_eq!(output.agents[0].command, "bash -c 'echo hello'");
    }
}
