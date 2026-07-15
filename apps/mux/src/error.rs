//! Domain errors surfaced to the CLI exit-code layer.

use std::fmt;

use thiserror::Error;

use crate::config::ConfigError;

/// Domain errors surfaced to the CLI exit-code layer.
#[derive(Debug, Error)]
pub enum KiraMuxError {
    /// The requested agent ID does not exist in the resolved project.
    #[error("unknown agent id: {0}")]
    UnknownAgentId(String),
    /// The requested group name does not exist in the resolved project.
    #[error("unknown group name: {0}")]
    UnknownGroupName(String),
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
    /// An operation that requires a live pane targeted a dead pane.
    #[error("cannot send to dead pane for agent '{0}'")]
    DeadPane(String),
    /// The agent pane died while `send --wait` was polling for output.
    #[error("pane for agent '{0}' died while waiting for output")]
    PaneDiedDuringWait(String),
    /// `send --wait` hit the internal hard timeout before the pane went
    /// through the activity-then-stability cycle.
    #[error("timed out waiting for output from agent '{agent_id}' to stabilize")]
    WaitTimeout {
        /// Agent whose pane never stabilized.
        agent_id: String,
        /// Last captured pane content, surfaced on stderr by the CLI layer
        /// so callers can inspect partial output.
        last_capture: String,
    },
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
