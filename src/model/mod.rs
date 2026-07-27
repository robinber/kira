//! Domain types for resolved projects, agents, and workspace status.
//!
//! Runtime config after resolve, plus DTOs shared by CLI text/JSON output.

pub(crate) mod project;
pub(crate) mod status;

pub(crate) use project::{ResolvedAgent, ResolvedProject};
pub(crate) use status::{
    AgentInfo, AgentRunState, AgentState, AgentStatus, AgentsOutput, PaneLiveness, ProjectState,
    ProjectStatus, ProjectSummary, build_agents_output,
};
