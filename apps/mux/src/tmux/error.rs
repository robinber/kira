use thiserror::Error;

/// Errors returned by tmux command execution and target resolution.
#[derive(Debug, Error)]
pub enum TmuxError {
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
