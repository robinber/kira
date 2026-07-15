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
                "{:<24} {:<10} {:>2} agents  {}",
                display_id(&row.id, &row.profile_id),
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
            println!("  {:<16} {}", agent.id, agent.state);
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
        "{:<12} {:<10} {:<10} {:<26} GROUPS",
        "AGENT", "COMMAND", "STATE", "CAPABILITIES"
    );
    println!("{}", "\u{2500}".repeat(70));
    for agent in &output.agents {
        let caps = agent.capabilities.join(", ");
        let groups = agent.groups.join(", ");
        println!(
            "{:<12} {:<10} {:<10} {:<26} {}",
            agent.id, agent.command, agent.state, caps, groups,
        );
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct AgentCapabilitiesOutput {
    pub agent: String,
    pub capabilities: Vec<String>,
    pub state: AgentRunState,
}

impl From<&AgentInfo> for AgentCapabilitiesOutput {
    fn from(agent: &AgentInfo) -> Self {
        Self {
            agent: agent.id.clone(),
            capabilities: agent.capabilities.clone(),
            state: agent.state,
        }
    }
}

pub(crate) fn print_agent_capabilities(agent: &AgentInfo) {
    println!("Agent: {}", agent.id);
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
        println!("  {:<12} {}", agent.id, agent.state);
    }
}

fn display_id(project_id: &str, profile_id: &str) -> String {
    format!("{project_id}/{profile_id}")
}
