//! Trait and DTOs for the tmux subprocess adapter.

use anyhow::Result;

/// Summary of a tmux pane returned by `list-panes`.
///
/// Field names mirror tmux's format variables (`pane_dead`, `alternate_on`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PaneInfo {
    /// Pane target ID such as `%1`.
    pub(crate) pane_id: String,
    /// Whether tmux reports the pane process as exited.
    pub(crate) pane_dead: bool,
    /// Exit status recorded by tmux when the pane is dead.
    pub(crate) pane_dead_status: Option<i32>,
    /// Whether the pane program is on the tmux alternate screen. Alternate
    /// screens accumulate no history: capture depth is capped at the visible
    /// frame no matter how many lines are requested.
    pub(crate) alternate_on: bool,
    /// Visible pane height in rows — the capture depth ceiling when
    /// `alternate_on` is set.
    pub(crate) pane_height: usize,
}

/// Window state saved before (and restored after) a deep capture resize.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WindowGeometry {
    /// Window width in columns.
    pub(crate) width: usize,
    /// Window height in rows.
    pub(crate) height: usize,
    /// Whether the window is currently zoomed on a pane.
    pub(crate) zoomed: bool,
    /// Whether the observed pane is the window's active pane (the zoomed
    /// pane when `zoomed` is set).
    pub(crate) pane_active: bool,
    /// Whether `window-size` was already set locally on the window before
    /// kira touched it (a later `resize-window` forces it to `manual`).
    pub(crate) size_option_set: bool,
}

/// Live pane metadata paired with its kira-mux agent assignment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkspacePaneSnapshot {
    /// Native tmux pane state.
    pub(crate) pane: PaneInfo,
    /// Agent ID stored in the pane-scoped kira-mux option.
    pub(crate) agent_id: Option<String>,
}

/// Managed-window data returned by a bulk workspace inspection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkspaceWindowSnapshot {
    /// Kira-mux role stored on the managed window.
    pub(crate) role: Option<String>,
    /// All panes in tmux order, including assignment and exit metadata.
    pub(crate) panes: Vec<WorkspacePaneSnapshot>,
}

/// Session and managed-window metadata read in a constant number of commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkspaceSnapshot {
    /// Resolved configuration fingerprint stored on the session.
    pub(crate) fingerprint: Option<String>,
    /// Project ID stored on the session.
    pub(crate) project_id: Option<String>,
    /// Profile ID stored on the session.
    pub(crate) profile_id: Option<String>,
    /// Managed window, or `None` when the session exists without that window.
    pub(crate) window: Option<WorkspaceWindowSnapshot>,
}

/// Subprocess-backed operations used by workspace lifecycle and agent I/O.
///
/// # Error semantics
///
/// Non-interactive methods that address a session, window, or pane should
/// surface transport failures as [`crate::tmux::TmuxError`] when the stderr
/// form is known:
///
/// | Condition | Typed error |
/// |---|---|
/// | No tmux server | [`NoServer`](crate::tmux::TmuxError::NoServer) |
/// | Session missing | [`MissingSession`](crate::tmux::TmuxError::MissingSession) |
/// | Window/pane missing | [`MissingTarget`](crate::tmux::TmuxError::MissingTarget) |
/// | Other non-zero status | [`CommandFailure`](crate::tmux::TmuxError::CommandFailure) |
///
/// Callers map those variants **in context** (send → dead pane, attach →
/// session absent, kill of an already-gone session → success, inspection →
/// drift). Do not collapse every missing target into one domain error here.
///
/// Interactive attach/switch keep the process terminal I/O path; a vanished
/// session race is re-checked at the lifecycle boundary after a failed
/// interactive command, not by inventing stderr from a status-only exit.
pub(crate) trait TmuxAdapter {
    /// Whether a session currently exists on the server.
    ///
    /// Missing sessions return `Ok(false)`. No server and other hard failures
    /// return typed [`crate::tmux::TmuxError`] values.
    fn session_exists(&self, session_name: &str) -> Result<bool>;
    /// Bulk read of session ownership plus managed window/pane metadata.
    ///
    /// Returns `Ok(None)` when the session (or server) is absent. A present
    /// session without the managed window yields `window: None` (not an error).
    fn workspace_snapshot(
        &self,
        session_name: &str,
        window_name: &str,
    ) -> Result<Option<WorkspaceSnapshot>>;
    /// Create a detached session whose first window is sized for `pane_count`.
    ///
    /// Not a target-bearing mutation of an existing object: failures stay
    /// generic command errors unless the implementation can type them.
    fn create_detached_session(
        &self,
        session_name: &str,
        start_directory: &str,
        window_name: &str,
        pane_count: usize,
    ) -> Result<()>;
    /// List panes for a session or window target.
    fn list_panes(&self, target: &str) -> Result<Vec<PaneInfo>>;
    /// Split a window, creating another pane in `start_directory`.
    fn split_window(&self, target: &str, start_directory: &str) -> Result<()>;
    /// Apply a named tmux layout to a window.
    fn select_layout(&self, target: &str, layout: &str) -> Result<()>;
    /// Restart a pane with cwd, env overrides, and command argv.
    fn respawn_pane(
        &self,
        target: &str,
        start_directory: &str,
        env_overrides: &[(String, String)],
        command: &[String],
    ) -> Result<()>;
    /// Attach the current client to a session (replaces the process).
    fn attach_session(&self, session_name: &str) -> Result<()>;
    /// Switch an existing client to another session.
    fn switch_client(&self, session_name: &str) -> Result<()>;
    /// Destroy a session and all of its windows/panes.
    fn kill_session(&self, session_name: &str) -> Result<()>;
    /// Set a session-scoped user option.
    fn set_session_option(&self, target: &str, name: &str, value: &str) -> Result<()>;
    /// Read a session-scoped user option.
    fn get_session_option(&self, target: &str, name: &str) -> Result<Option<String>>;
    /// Set a window-scoped user option.
    fn set_window_option(&self, target: &str, name: &str, value: &str) -> Result<()>;
    /// Set a pane-scoped user option.
    fn set_pane_option(&self, target: &str, name: &str, value: &str) -> Result<()>;
    /// Read a pane-scoped user option.
    fn get_pane_option(&self, target: &str, name: &str) -> Result<Option<String>>;
    /// Bracketed-paste `text` into a pane.
    fn paste_text(&self, target_pane: &str, text: &str) -> Result<()>;
    /// Send named tmux keys (e.g. `Enter`, `C-c`) to a pane.
    fn send_keys(&self, target_pane: &str, keys: &[&str]) -> Result<()>;
    /// Type `text` into a pane literally, never interpreting it as key names
    /// or `send-keys` flags.
    fn send_text(&self, target_pane: &str, text: &str) -> Result<()>;
    /// Capture recent pane history (at most `history_limit` lines).
    fn capture_pane(&self, pane_id: &str, history_limit: usize) -> Result<String>;
    /// Read the window geometry (and zoom/option state) for a pane's window.
    fn window_geometry(&self, pane_id: &str) -> Result<WindowGeometry>;
    /// Resize a pane's window to an explicit size (sets `window-size` to
    /// `manual` as a tmux side effect — see
    /// [`WindowGeometry::size_option_set`]).
    fn resize_window(&self, pane_id: &str, width: usize, height: usize) -> Result<()>;
    /// Toggle window zoom on a pane (`resize-pane -Z`).
    fn toggle_pane_zoom(&self, pane_id: &str) -> Result<()>;
    /// Remove the window-local `window-size` override so the window follows
    /// clients (or the session default size) again.
    fn unset_window_size_option(&self, pane_id: &str) -> Result<()>;
}
