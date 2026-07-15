use anyhow::Result;
use serde::Serialize;

use crate::model::{AgentInfo, AgentRunState, AgentsOutput, ProjectStatus, ProjectSummary};

/// Print any `--json` payload with one shared policy: compact single-line
/// JSON on stdout.
pub(crate) fn print_json<T: Serialize>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string(value)?);
    Ok(())
}

pub(crate) fn print_list(summaries: &[ProjectSummary], json: bool) -> Result<()> {
    if json {
        print_json(&summaries)?;
    } else {
        for row in summaries {
            println!(
                "{:<24} {:<20} {:<10} {:>2} agents  {}",
                display_id(&row.id, &row.profile_id),
                row.name,
                row.state,
                row.agent_count,
                row.root,
            );
        }
    }

    Ok(())
}

pub(crate) fn print_status(status: &ProjectStatus, json: bool) -> Result<()> {
    if json {
        print_json(status)?;
    } else {
        println!("Project: {} ({})", status.name, status.id);
        if status.profile_id != "default" {
            println!("Profile: {}", status.profile_id);
        }
        println!("Root:    {}", status.root);
        println!("State:   {}", status.state);
        println!();
        for agent in &status.agents {
            println!(
                "  {:<28} {}",
                agent_display_name(&agent.id, agent.label.as_deref()),
                agent.state
            );
        }
    }

    Ok(())
}

pub(crate) fn print_agents_table(output: &AgentsOutput) {
    print!("Project: {}", output.project);
    if let Some(ref profile) = output.profile {
        print!("  (profile: {profile})");
    }
    println!();
    println!();
    println!(
        "{:<28} {:<10} {:<10} {:<22} GROUPS",
        "AGENT", "COMMAND", "STATE", "CAPABILITIES"
    );
    println!("{}", "\u{2500}".repeat(80));
    for agent in &output.agents {
        let caps = agent.capabilities.join(", ");
        let groups = agent.groups.join(", ");
        println!(
            "{:<28} {:<10} {:<10} {:<22} {}",
            agent_display_name(&agent.id, Some(&agent.label)),
            agent.command,
            agent.state,
            caps,
            groups,
        );
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct AgentCapabilitiesOutput {
    pub agent: String,
    pub label: String,
    pub capabilities: Vec<String>,
    pub state: AgentRunState,
}

impl From<&AgentInfo> for AgentCapabilitiesOutput {
    fn from(agent: &AgentInfo) -> Self {
        Self {
            agent: agent.id.clone(),
            label: agent.label.clone(),
            capabilities: agent.capabilities.clone(),
            state: agent.state,
        }
    }
}

pub(crate) fn print_agent_capabilities(agent: &AgentInfo) {
    println!(
        "Agent: {}",
        agent_display_name(&agent.id, Some(&agent.label))
    );
    println!("State: {}", agent.state);
    println!(
        "Capabilities: {}",
        if agent.capabilities.is_empty() {
            "(none)".to_string()
        } else {
            agent.capabilities.join(", ")
        }
    );
}

#[derive(Debug, Serialize)]
pub(crate) struct GroupMemberOutput {
    pub id: String,
    pub state: AgentRunState,
}

#[derive(Debug, Serialize)]
pub(crate) struct GroupOutput {
    pub group: String,
    pub members: Vec<GroupMemberOutput>,
}

impl GroupOutput {
    pub(crate) fn new(group_name: &str, members: &[&AgentInfo]) -> Self {
        Self {
            group: group_name.to_string(),
            members: members
                .iter()
                .map(|a| GroupMemberOutput {
                    id: a.id.clone(),
                    state: a.state,
                })
                .collect(),
        }
    }
}

pub(crate) fn print_group(group_name: &str, members: &[&AgentInfo]) {
    println!("Group: {group_name}");
    for agent in members {
        println!(
            "  {:<28} {}",
            agent_display_name(&agent.id, Some(&agent.label)),
            agent.state
        );
    }
}

fn display_id(project_id: &str, profile_id: &str) -> String {
    format!("{project_id}/{profile_id}")
}

/// Text display for an agent: `id` alone when the label matches, otherwise
/// `id (label)` so config labels are visible without losing the stable id.
fn agent_display_name(id: &str, label: Option<&str>) -> String {
    match label {
        Some(label) if label != id => format!("{id} ({label})"),
        _ => id.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{agent_display_name, display_id};

    #[test]
    fn agent_display_name_omits_redundant_label() {
        assert_eq!(agent_display_name("alpha", Some("alpha")), "alpha");
        assert_eq!(agent_display_name("alpha", None), "alpha");
    }

    #[test]
    fn agent_display_name_includes_distinct_label() {
        assert_eq!(agent_display_name("alpha", Some("Coder")), "alpha (Coder)");
    }

    #[test]
    fn display_id_joins_project_and_profile() {
        assert_eq!(display_id("demo", "default"), "demo/default");
        assert_eq!(display_id("demo", "pool-1"), "demo/pool-1");
    }

    #[test]
    fn list_line_includes_project_name() {
        // Keep the text list columns aligned with print_list.
        let line = format!(
            "{:<24} {:<20} {:<10} {:>2} agents  {}",
            display_id("my-app", "default"),
            "My App",
            "running",
            2,
            "/tmp/demo",
        );
        assert!(line.contains("My App"));
        assert!(line.contains("my-app/default"));
        assert!(line.contains("running"));
    }
}
