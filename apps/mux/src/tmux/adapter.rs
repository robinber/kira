use anyhow::Result;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
/// Summary of a tmux pane returned by `list-panes`.
pub struct PaneInfo {
    /// Pane target ID such as `%1`.
    pub pane_id: String,
    /// Owning window target ID such as `@3`.
    pub window_id: String,
    /// Whether tmux reports the pane process as exited.
    pub pane_dead: bool,
    /// Exit status recorded by tmux when the pane is dead.
    pub pane_dead_status: Option<i32>,
}

pub(crate) trait TmuxAdapter {
    fn session_exists(&self, session_name: &str) -> Result<bool>;
    fn create_detached_session(
        &self,
        session_name: &str,
        start_directory: &str,
        window_name: &str,
        pane_count: usize,
    ) -> Result<()>;
    fn list_panes(&self, target: Option<&str>) -> Result<Vec<PaneInfo>>;
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
    fn get_window_option(&self, target: &str, name: &str) -> Result<Option<String>>;
    fn set_pane_option(&self, target: &str, name: &str, value: &str) -> Result<()>;
    fn get_pane_option(&self, target: &str, name: &str) -> Result<Option<String>>;
    fn paste_text(&self, target_pane: &str, text: &str) -> Result<()>;
    fn send_keys(&self, target_pane: &str, keys: &[&str]) -> Result<()>;
    fn capture_pane(&self, pane_id: &str, history_limit: usize) -> Result<String>;
}
