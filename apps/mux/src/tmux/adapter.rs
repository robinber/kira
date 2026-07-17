use anyhow::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
/// Summary of a tmux pane returned by `list-panes`.
#[expect(
    clippy::struct_field_names,
    reason = "field names mirror tmux's pane_* format variables"
)]
pub(crate) struct PaneInfo {
    /// Pane target ID such as `%1`.
    pub(crate) pane_id: String,
    /// Whether tmux reports the pane process as exited.
    pub(crate) pane_dead: bool,
    /// Exit status recorded by tmux when the pane is dead.
    pub(crate) pane_dead_status: Option<i32>,
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

pub(crate) trait TmuxAdapter {
    fn session_exists(&self, session_name: &str) -> Result<bool>;
    fn workspace_snapshot(
        &self,
        session_name: &str,
        window_name: &str,
    ) -> Result<Option<WorkspaceSnapshot>>;
    fn create_detached_session(
        &self,
        session_name: &str,
        start_directory: &str,
        window_name: &str,
        pane_count: usize,
    ) -> Result<()>;
    fn list_panes(&self, target: &str) -> Result<Vec<PaneInfo>>;
    fn split_window(&self, target: &str, start_directory: &str) -> Result<()>;
    fn select_layout(&self, target: &str, layout: &str) -> Result<()>;
    fn respawn_pane(
        &self,
        target: &str,
        start_directory: &str,
        env_overrides: &[(String, String)],
        command: &[String],
    ) -> Result<()>;
    fn attach_session(&self, session_name: &str) -> Result<()>;
    fn switch_client(&self, session_name: &str) -> Result<()>;
    fn kill_session(&self, session_name: &str) -> Result<()>;
    fn set_session_option(&self, target: &str, name: &str, value: &str) -> Result<()>;
    fn get_session_option(&self, target: &str, name: &str) -> Result<Option<String>>;
    fn set_window_option(&self, target: &str, name: &str, value: &str) -> Result<()>;
    fn set_pane_option(&self, target: &str, name: &str, value: &str) -> Result<()>;
    fn get_pane_option(&self, target: &str, name: &str) -> Result<Option<String>>;
    fn paste_text(&self, target_pane: &str, text: &str) -> Result<()>;
    /// Send named tmux keys (e.g. `Enter`, `C-c`) to a pane.
    fn send_keys(&self, target_pane: &str, keys: &[&str]) -> Result<()>;
    /// Type `text` into a pane literally, never interpreting it as key names
    /// or `send-keys` flags.
    fn send_text(&self, target_pane: &str, text: &str) -> Result<()>;
    fn capture_pane(&self, pane_id: &str, history_limit: usize) -> Result<String>;
}
