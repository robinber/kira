/// Session option storing the configured project ID.
pub(crate) const SESSION_PROJECT_ID: &str = "@kira_mux_project_id";
/// Session option storing the active profile ID.
pub(crate) const SESSION_PROFILE_ID: &str = "@kira_mux_profile_id";
/// Session option storing the resolved config fingerprint.
pub(crate) const SESSION_CONFIG_FINGERPRINT: &str = "@kira_mux_config_fingerprint";
/// Window option marking kira-mux-managed windows.
pub(crate) const WINDOW_ROLE: &str = "@kira_mux_window_role";
/// Window role value for the main agents window.
pub(crate) const WINDOW_ROLE_AGENTS: &str = "agents";
/// Pane option storing the owning agent ID.
pub(crate) const PANE_AGENT_ID: &str = "@kira_mux_agent_id";
/// Pane option storing the launched agent command.
pub(crate) const PANE_AGENT_COMMAND: &str = "@kira_mux_agent_command";
/// Sentinel stored in [`PANE_AGENT_COMMAND`] for shell-mode agents.
pub(crate) const PANE_COMMAND_SHELL: &str = "__shell__";
