//! Prompt template context built from project identity and live topology.

use crate::inspector::WorkspaceTopology;
use crate::model::{AgentRunState, ResolvedProject, build_agents_output};

#[derive(Debug, Clone)]
pub(crate) struct PromptContext {
    pub user_prompt: String,
    pub agent_name: String,
    pub project_name: String,
    pub active_agents: String,
    pub agent_states: String,
}

impl PromptContext {
    /// Context with only the identity fields set; test convenience since
    /// production callers always render from an inspected topology.
    #[cfg(test)]
    pub(crate) fn minimal(agent_id: &str, project_name: &str, user_prompt: &str) -> Self {
        Self {
            user_prompt: user_prompt.to_owned(),
            agent_name: agent_id.to_owned(),
            project_name: project_name.to_owned(),
            active_agents: String::new(),
            agent_states: String::new(),
        }
    }
}

pub(crate) fn extract_agent_state(
    topology: &WorkspaceTopology,
    project: &ResolvedProject,
) -> (String, String) {
    match topology {
        WorkspaceTopology::Healthy(_) | WorkspaceTopology::Degraded(_) => {
            // Project from the same view `agents list` serves, so the
            // vocabulary injected into prompts cannot drift from the CLI's.
            let output = build_agents_output(project, topology);
            let mut active: Vec<String> = Vec::new();
            let mut states: Vec<String> = Vec::with_capacity(output.agents.len());

            for agent in &output.agents {
                if agent.state == AgentRunState::Running {
                    if agent.capabilities.is_empty() {
                        active.push(agent.id.clone());
                    } else {
                        active.push(format!("{} ({})", agent.id, agent.capabilities.join(", ")));
                    }
                }

                let group_str = if agent.groups.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", agent.groups.join(", "))
                };
                states.push(format!("{}:{}{}", agent.id, agent.state, group_str));
            }

            (active.join(", "), states.join(", "))
        }
        WorkspaceTopology::Absent => ("(workspace absent)".into(), String::new()),
        WorkspaceTopology::Drifted { .. } => ("(workspace drifted)".into(), String::new()),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use super::*;
    use crate::inspector::{InspectedWorkspace, ManagedPane, WorkspaceTopology};
    use crate::model::{ResolvedAgent, ResolvedProject};
    use crate::tmux::PaneInfo;

    fn make_agent(id: &str, capabilities: Vec<String>) -> ResolvedAgent {
        let mut agent = crate::test_support::test_project().agents.remove(0);
        agent.id = id.to_string();
        agent.label = id.to_string();
        agent.capabilities = capabilities;
        agent
    }

    fn make_project(
        agents: Vec<ResolvedAgent>,
        groups: BTreeMap<String, Vec<String>>,
    ) -> ResolvedProject {
        let mut project = crate::test_support::test_project();
        project.root = PathBuf::from("/tmp/test");
        project.fingerprint = "fp".to_string();
        project.agents = agents;
        project.groups = groups;
        project
    }

    fn healthy_topology(agents: &[ResolvedAgent]) -> WorkspaceTopology {
        let panes = agents
            .iter()
            .enumerate()
            .map(|(i, agent)| ManagedPane {
                pane: PaneInfo {
                    pane_id: format!("%{i}"),
                    pane_dead: false,
                    pane_dead_status: None,
                    alternate_on: false,
                    pane_height: 24,
                },
                agent: agent.clone(),
            })
            .collect();
        WorkspaceTopology::Healthy(InspectedWorkspace { panes })
    }

    #[test]
    fn minimal_context_has_empty_live_fields() {
        let ctx = PromptContext::minimal("coder", "my-project", "fix the bug");

        assert_eq!(ctx.agent_name, "coder");
        assert_eq!(ctx.project_name, "my-project");
        assert_eq!(ctx.user_prompt, "fix the bug");
        assert_eq!(ctx.active_agents, "");
        assert_eq!(ctx.agent_states, "");
    }

    #[test]
    fn extract_agent_state_with_capabilities() {
        let agents = vec![
            make_agent("coder", vec!["rust".to_string(), "typescript".to_string()]),
            make_agent(
                "reviewer",
                vec!["code-review".to_string(), "security".to_string()],
            ),
        ];
        let project = make_project(agents.clone(), BTreeMap::new());
        let topology = healthy_topology(&agents);

        let (active, _states) = extract_agent_state(&topology, &project);

        assert!(active.contains("coder (rust, typescript)"), "got: {active}");
        assert!(
            active.contains("reviewer (code-review, security)"),
            "got: {active}"
        );
    }

    #[test]
    fn extract_agent_state_no_capabilities_no_parens() {
        let agents = vec![make_agent("coder", vec![]), make_agent("reviewer", vec![])];
        let project = make_project(agents.clone(), BTreeMap::new());
        let topology = healthy_topology(&agents);

        let (active, _states) = extract_agent_state(&topology, &project);

        assert!(!active.contains('('), "should not contain parens: {active}");
        assert!(active.contains("coder"), "got: {active}");
        assert!(active.contains("reviewer"), "got: {active}");
    }

    #[test]
    fn extract_agent_state_with_groups() {
        let agents = vec![make_agent("coder", vec![]), make_agent("reviewer", vec![])];
        let mut groups = BTreeMap::new();
        groups.insert("implementation".to_string(), vec!["coder".to_string()]);
        groups.insert("review".to_string(), vec!["reviewer".to_string()]);
        let project = make_project(agents.clone(), groups);
        let topology = healthy_topology(&agents);

        let (_active, states) = extract_agent_state(&topology, &project);

        assert!(states.contains("[implementation]"), "got: {states}");
        assert!(states.contains("[review]"), "got: {states}");
    }

    #[test]
    fn extract_agent_state_mixed() {
        let agents = vec![
            make_agent("coder", vec!["rust".to_string()]),
            make_agent("reviewer", vec![]),
        ];
        let mut groups = BTreeMap::new();
        groups.insert("review".to_string(), vec!["reviewer".to_string()]);
        let project = make_project(agents.clone(), groups);
        let topology = healthy_topology(&agents);

        let (active, states) = extract_agent_state(&topology, &project);

        assert!(active.contains("coder (rust)"), "got: {active}");
        assert!(active.contains("reviewer"), "got: {active}");
        assert!(
            !active.contains("reviewer ("),
            "reviewer should have no parens: {active}"
        );
        assert!(states.contains("[review]"), "got: {states}");
        assert!(
            !states.contains("coder ["),
            "coder should have no group brackets: {states}"
        );
    }

    #[test]
    fn extract_agent_state_degraded_dead_pane_not_active() {
        let agents = vec![make_agent("coder", vec![]), make_agent("reviewer", vec![])];
        let panes = vec![
            ManagedPane {
                pane: PaneInfo {
                    pane_id: "%0".to_string(),
                    pane_dead: false,
                    pane_dead_status: None,
                    alternate_on: false,
                    pane_height: 24,
                },
                agent: agents[0].clone(),
            },
            ManagedPane {
                pane: PaneInfo {
                    pane_id: "%1".to_string(),
                    pane_dead: true,
                    pane_dead_status: Some(1),
                    alternate_on: false,
                    pane_height: 24,
                },
                agent: agents[1].clone(),
            },
        ];
        let topology = WorkspaceTopology::Degraded(InspectedWorkspace { panes });
        let project = make_project(agents, BTreeMap::new());

        let (active, states) = extract_agent_state(&topology, &project);

        assert_eq!(
            active, "coder",
            "dead agent must not appear in active_agents"
        );
        assert!(states.contains("coder:running"), "got: {states}");
        assert!(states.contains("reviewer:dead"), "got: {states}");
    }
}
