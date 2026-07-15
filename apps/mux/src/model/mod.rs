//! Domain types for resolved projects, agents, and workspace status.

pub(crate) mod project;
pub(crate) mod status;

pub use project::{ResolvedAgent, ResolvedProject};
pub(crate) use status::{
    AgentInfo, AgentRunState, AgentState, AgentStatus, AgentsOutput, ProjectStatus,
    build_agents_output,
};
pub use status::{ProjectState, ProjectSummary};
