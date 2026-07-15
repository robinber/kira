use crate::inspector::WorkspaceTopology;
use crate::model::ResolvedProject;

#[derive(Debug, Clone)]
pub(crate) struct PromptContext {
    pub user_prompt: String,
    pub agent_name: String,
    pub project_name: String,
    pub active_agents: String,
    pub agent_states: String,
}

impl PromptContext {
    pub(crate) fn resolve(
        user_prompt: String,
        agent_name: String,
        project_name: String,
        active_agents: String,
        agent_states: String,
    ) -> Self {
        Self {
            user_prompt,
            agent_name,
            project_name,
            active_agents,
            agent_states,
        }
    }

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
        WorkspaceTopology::Healthy(ws) | WorkspaceTopology::Degraded(ws) => {
            let active: Vec<String> = project
                .agents
                .iter()
                .filter_map(|agent| {
                    let alive = ws
                        .panes
                        .iter()
                        .any(|p| p.agent.id == agent.id && !p.pane.pane_dead);
                    if alive {
                        if agent.capabilities.is_empty() {
                            Some(agent.id.clone())
                        } else {
                            Some(format!("{} ({})", agent.id, agent.capabilities.join(", ")))
                        }
                    } else {
                        None
                    }
                })
                .collect();

            let states: Vec<String> = project
                .agents
                .iter()
                .map(|agent| {
                    let alive = ws
                        .panes
                        .iter()
                        .any(|p| p.agent.id == agent.id && !p.pane.pane_dead);
                    let groups = project.groups_for(&agent.id);
                    let group_str = if groups.is_empty() {
                        String::new()
                    } else {
                        format!(" [{}]", groups.join(", "))
                    };
                    format!(
                        "{}:{}{}",
                        agent.id,
                        if alive { "running" } else { "dead" },
                        group_str
                    )
                })
                .collect();

            (active.join(", "), states.join(", "))
        }
        _ => ("(workspace absent)".into(), String::new()),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use super::*;
    use crate::config::{AgentMode, Layout, RemainOnExit};
    use crate::inspector::{InspectedWorkspace, ManagedPane, WorkspaceTopology};
    use crate::model::{ResolvedAgent, ResolvedProject};
    use crate::tmux::PaneInfo;

    fn make_agent(id: &str, capabilities: Vec<String>) -> ResolvedAgent {
        ResolvedAgent {
            id: id.to_string(),
            label: id.to_string(),
            mode: AgentMode::Direct,
            command: Some("echo".to_string()),
            shell_command: None,
            args: vec![],
            cwd: PathBuf::from("/tmp/test"),
            env: BTreeMap::new(),
            capabilities,
            prompt_template: None,
        }
    }

    fn make_project(
        agents: Vec<ResolvedAgent>,
        groups: BTreeMap<String, Vec<String>>,
    ) -> ResolvedProject {
        ResolvedProject {
            id: "test".to_string(),
            profile_id: "default".to_string(),
            name: "Test".to_string(),
            root: PathBuf::from("/tmp/test"),
            layout: Layout::Auto,
            main_pane_ratio: 50,
            window_name: "agents".to_string(),
            session_prefix: "ai".to_string(),
            default_shell: "/bin/sh".to_string(),
            remain_on_exit: RemainOnExit::Failed,
            tmux_bin: "tmux".to_string(),
            agents,
            fingerprint: "fp".to_string(),
            groups,
        }
    }

    fn healthy_topology(agents: &[ResolvedAgent]) -> WorkspaceTopology {
        let panes = agents
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
                    window_id: "@w-agents".to_string(),
                    pane_dead: false,
                    pane_dead_status: None,
                },
                agent: agents[0].clone(),
            },
            ManagedPane {
                pane: PaneInfo {
                    pane_id: "%1".to_string(),
                    window_id: "@w-agents".to_string(),
                    pane_dead: true,
                    pane_dead_status: Some(1),
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
