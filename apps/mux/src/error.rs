//! Domain errors surfaced to the CLI exit-code layer.

use std::fmt;

use thiserror::Error;

use crate::config::ConfigError;

/// Domain errors surfaced to the CLI exit-code layer.
#[derive(Debug, Error)]
pub enum AiMuxError {
    /// The requested project ID does not exist.
    #[error("unknown project id: {0}")]
    UnknownProjectId(String),
    /// The requested agent ID does not exist in the resolved project.
    #[error("unknown agent id: {0}")]
    UnknownAgentId(String),
    /// The requested group name does not exist in the resolved project.
    #[error("unknown group name: {0}")]
    UnknownGroupName(String),
    /// A required CLI argument was omitted after higher-level parsing.
    #[error("{0}")]
    MissingArgument(String),
    /// An orchestrator profile must resolve to exactly one agent in V1.
    #[error("orchestrator profile '{profile}' must contain exactly one agent, found {count}")]
    InvalidOrchestratorProfile {
        /// Profile that failed validation.
        profile: String,
        /// Number of resolved agents in that profile.
        count: usize,
    },
    /// The selected agent is not the only agent in the orchestrator profile.
    #[error(
        "orchestrator profile '{profile}' contains agent '{actual}', not requested agent '{requested}'"
    )]
    OrchestratorAgentMismatch {
        /// Profile being launched.
        profile: String,
        /// Requested agent ID.
        requested: String,
        /// Actual single agent ID in the profile.
        actual: String,
    },
    /// The selected agent is missing the required orchestrator capability.
    #[error("agent '{agent_id}' is not an orchestrator; add capability 'orchestrator'")]
    AgentNotOrchestrator {
        /// Agent that failed validation.
        agent_id: String,
    },
    /// V1 only supports direct-mode orchestrator launch.
    #[error(
        "agent '{agent_id}' uses shell mode; orchestrator start/restart supports direct mode only"
    )]
    OrchestratorShellModeUnsupported {
        /// Agent that failed validation.
        agent_id: String,
    },
    /// Start refused to replace a running orchestrator pane.
    #[error(
        "orchestrator pane for agent '{agent_id}' is already running; use restart to replace it"
    )]
    OrchestratorPaneActive {
        /// Agent whose pane is active.
        agent_id: String,
    },
    /// Project or global configuration failed validation.
    #[error("config validation error: {0}")]
    ConfigValidation(#[from] ConfigError),
    /// A required external dependency is missing from the host system.
    #[error("required dependency missing: {0}")]
    MissingDependency(String),
    /// The managed tmux session does not exist.
    #[error("session is absent")]
    SessionAbsent,
    /// The user declined a destructive kill operation.
    #[error("kill aborted")]
    KillAborted,
    /// Workspace launch completed with at least one failed pane.
    #[error("project {0} completed in degraded state")]
    Degraded(String),
    /// Live tmux state no longer matches the resolved project contract.
    #[error("workspace for project {project_id} is drifted: {reason}")]
    Drifted {
        /// Project whose workspace drifted.
        project_id: String,
        /// Specific drift classification.
        reason: WorkspaceDriftReason,
    },
}

/// Specific reason a workspace was classified as drifted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceDriftReason {
    /// Session metadata fingerprint no longer matches the resolved config.
    FingerprintMismatch,
    /// Session project metadata points at a different project.
    ProjectMetadataMismatch,
    /// Session profile metadata points at a different profile.
    ProfileMetadataMismatch,
    /// The managed window no longer exists.
    ManagedWindowMissing,
    /// Managed window metadata is missing or no longer matches.
    WindowMetadataMismatch,
    /// The number of panes no longer matches the configured agents.
    PaneCountMismatch,
    /// At least one managed pane is missing identifying metadata.
    PaneMetadataMissing,
    /// A pane references an unknown agent ID.
    UnknownManagedAgentId(String),
    /// Multiple panes claim the same agent ID.
    DuplicateManagedAgentId(String),
    /// A configured agent no longer has a corresponding pane.
    MissingManagedPane(String),
}

impl fmt::Display for WorkspaceDriftReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FingerprintMismatch => write!(f, "config fingerprint mismatch"),
            Self::ProjectMetadataMismatch => write!(f, "session project metadata mismatch"),
            Self::ProfileMetadataMismatch => write!(f, "session profile metadata mismatch"),
            Self::ManagedWindowMissing => write!(f, "managed window missing"),
            Self::WindowMetadataMismatch => write!(f, "managed window metadata mismatch"),
            Self::PaneCountMismatch => write!(f, "managed pane count mismatch"),
            Self::PaneMetadataMissing => write!(f, "pane metadata missing"),
            Self::UnknownManagedAgentId(id) => write!(f, "unknown managed agent id: {id}"),
            Self::DuplicateManagedAgentId(id) => write!(f, "duplicate managed agent id: {id}"),
            Self::MissingManagedPane(id) => write!(f, "missing managed pane for agent {id}"),
        }
    }
}
