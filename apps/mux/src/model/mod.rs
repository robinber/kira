//! Domain types for resolved projects, agents, and workspace status.

pub(crate) mod project;
pub(crate) mod status;

pub(crate) use project::{ResolvedAgent, ResolvedProject};
pub(crate) use status::{
    AgentInfo, AgentRunState, AgentState, AgentStatus, AgentsOutput, ProjectState, ProjectStatus,
    ProjectSummary, build_agents_output,
};
