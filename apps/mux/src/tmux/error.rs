use thiserror::Error;

/// Errors returned by tmux command execution and target resolution.
#[derive(Debug, Error)]
pub(crate) enum TmuxError {
    /// No tmux server is currently running.
    #[error("{0}")]
    NoServer(String),
    /// The requested tmux session does not exist.
    #[error("tmux session not found: {0}")]
    MissingSession(String),
    /// The requested tmux session/window/pane target does not exist.
    #[error("tmux target not found: {0}")]
    MissingTarget(String),
    /// tmux returned a non-success exit status.
    #[error("tmux command failed: {0}")]
    CommandFailure(String),
}

impl TmuxError {
    /// True when `error` is a typed missing-session/window/pane failure, as
    /// opposed to a transport or server error.
    pub(crate) fn is_missing_target(error: &anyhow::Error) -> bool {
        matches!(
            error.downcast_ref::<Self>(),
            Some(Self::MissingTarget(_) | Self::MissingSession(_))
        )
    }

    /// True when a previously resolved target can no longer be addressed,
    /// including when its last pane disappearing stopped the tmux server.
    pub(crate) fn is_target_unavailable(error: &anyhow::Error) -> bool {
        matches!(
            error.downcast_ref::<Self>(),
            Some(Self::NoServer(_) | Self::MissingTarget(_) | Self::MissingSession(_))
        )
    }
}
