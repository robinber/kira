use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// When environment-variable placeholders are resolved during config loading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvResolutionMode {
    /// Keep `$VARS` unresolved until runtime use.
    Deferred,
    /// Resolve `$VARS` during config loading.
    Runtime,
}

/// Supported tmux pane layouts for a workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Layout {
    /// Let kira-mux choose a layout automatically.
    #[default]
    Auto,
    /// Arrange panes side by side.
    SideBySide,
    /// Arrange panes in a vertical stack.
    Stacked,
    /// Use tmux's main-vertical layout.
    MainLeft,
    /// Use tmux's main-horizontal layout.
    MainTop,
    /// Use tmux's tiled grid layout.
    Grid,
}

impl Layout {
    /// Returns the config/display name (e.g. "side-by-side"), not the tmux
    /// layout engine name.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::SideBySide => "side-by-side",
            Self::Stacked => "stacked",
            Self::MainLeft => "main-left",
            Self::MainTop => "main-top",
            Self::Grid => "grid",
        }
    }
}

/// Launch mode for an agent pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AgentMode {
    /// Execute the configured command directly.
    #[default]
    Direct,
    /// Run the configured shell command through the default shell.
    Shell,
}

impl AgentMode {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Shell => "shell",
        }
    }
}

/// Policy for keeping panes open after the child process exits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum RemainOnExit {
    /// Never keep exited panes open.
    Off,
    /// Keep only panes that exited with failure.
    #[default]
    Failed,
    /// Always keep exited panes open.
    On,
}

impl RemainOnExit {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Failed => "failed",
            Self::On => "on",
        }
    }
}

/// Global kira-mux defaults loaded from the user config file.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct GlobalConfig {
    /// Prefix used when deriving tmux session names.
    pub session_prefix: String,
    /// Default layout for projects that do not set one.
    pub default_layout: Layout,
    /// Default main-pane ratio for supported layouts.
    pub main_pane_ratio: u8,
    /// Default tmux window name.
    pub window_name: String,
    /// Default shell for shell-mode agents.
    pub default_shell: String,
    /// Default pane retention policy after exit.
    pub remain_on_exit: RemainOnExit,
    /// tmux executable name or path.
    pub tmux_bin: String,
    /// Reusable agent templates available to projects.
    pub agent_templates: Vec<AgentTemplate>,
}

impl GlobalConfig {
    fn with_defaults() -> Self {
        Self {
            session_prefix: default_session_prefix(),
            default_layout: Layout::Auto,
            main_pane_ratio: default_main_pane_ratio(),
            window_name: default_window_name(),
            default_shell: default_shell(),
            remain_on_exit: RemainOnExit::Failed,
            tmux_bin: default_tmux_bin(),
            agent_templates: Vec::new(),
        }
    }
}

impl Default for GlobalConfig {
    fn default() -> Self {
        Self::with_defaults()
    }
}

/// Reusable agent template from global config.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentTemplate {
    /// Template name referenced by project agents.
    pub name: String,
    /// Optional display label override.
    #[serde(default)]
    pub label: Option<String>,
    /// Optional launch mode override.
    #[serde(default)]
    pub mode: Option<AgentMode>,
    /// Optional direct command override.
    #[serde(default)]
    pub command: Option<String>,
    /// Optional shell command override.
    #[serde(default)]
    pub shell_command: Option<String>,
    /// Extra command-line arguments.
    #[serde(default)]
    pub args: Vec<String>,
    /// Optional working directory override.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Environment overrides applied to the agent.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Capability tags advertised by the agent.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Optional prompt template for send operations.
    #[serde(default)]
    pub prompt_template: Option<String>,
}

/// Internal project shape used before full resolution.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProjectFile {
    /// Stable project ID.
    pub id: String,
    /// Optional display name.
    #[serde(default)]
    pub name: Option<String>,
    /// Project root path as configured.
    pub root: String,
    /// Optional layout override.
    #[serde(default)]
    pub layout: Option<Layout>,
    /// Optional main-pane ratio override.
    #[serde(default)]
    pub main_pane_ratio: Option<u8>,
    /// Optional window-name override.
    #[serde(default)]
    pub window_name: Option<String>,
    /// Agent definitions for the selected shape/profile.
    pub agents: Vec<ProjectAgent>,
    /// Named agent groups.
    #[serde(default)]
    pub groups: BTreeMap<String, Vec<String>>,
    /// Optional orchestration settings.
    #[serde(default)]
    pub orchestration: Option<OrchestrationConfig>,
}

/// Internal project agent definition before template expansion.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProjectAgent {
    /// Stable agent ID.
    pub id: String,
    /// Optional template reference.
    #[serde(default)]
    pub template: Option<String>,
    /// Optional display label.
    #[serde(default)]
    pub label: Option<String>,
    /// Optional launch mode override.
    #[serde(default)]
    pub mode: Option<AgentMode>,
    /// Optional direct command.
    #[serde(default)]
    pub command: Option<String>,
    /// Optional shell command.
    #[serde(default)]
    pub shell_command: Option<String>,
    /// Optional argument list.
    #[serde(default)]
    pub args: Option<Vec<String>>,
    /// Optional working directory.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Environment overrides.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Optional capability list.
    #[serde(default)]
    pub capabilities: Option<Vec<String>>,
    /// Optional prompt template.
    #[serde(default)]
    pub prompt_template: Option<String>,
}

/// Profile-specific overrides inside a profiled project file.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProfileDef {
    /// Optional profile label for future UI use.
    #[serde(default)]
    #[allow(
        dead_code,
        reason = "the label remains accepted for profile-file compatibility"
    )]
    pub label: Option<String>,
    /// Optional layout override for the profile.
    #[serde(default)]
    pub layout: Option<Layout>,
    /// Optional main-pane ratio override for the profile.
    #[serde(default)]
    pub main_pane_ratio: Option<u8>,
    /// Agent definitions for the profile.
    pub agents: Vec<ProjectAgent>,
}

/// Raw project file before shape selection.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProjectFileRaw {
    /// Stable project ID.
    pub id: String,
    /// Optional display name.
    #[serde(default)]
    pub name: Option<String>,
    /// Project root path as configured.
    pub root: String,
    /// Optional top-level layout.
    #[serde(default)]
    pub layout: Option<Layout>,
    /// Optional top-level main-pane ratio.
    #[serde(default)]
    pub main_pane_ratio: Option<u8>,
    /// Optional top-level window name.
    #[serde(default)]
    pub window_name: Option<String>,
    /// Top-level agent list for non-profiled projects.
    #[serde(default)]
    pub agents: Option<Vec<ProjectAgent>>,
    /// Profile map for profiled projects.
    #[serde(default)]
    pub profiles: Option<BTreeMap<String, ProfileDef>>,
    /// Optional group definitions.
    #[serde(default)]
    pub groups: Option<BTreeMap<String, Vec<String>>>,
    /// Optional orchestration settings.
    #[serde(default)]
    pub orchestration: Option<OrchestrationConfig>,
}

impl ProjectFileRaw {
    /// Validate that the raw project uses exactly one supported config shape.
    pub(crate) fn validate_shape(&self) -> Result<(), crate::config::ConfigError> {
        if let Some(profiles) = &self.profiles {
            if profiles.is_empty() {
                return Err(crate::config::ConfigError::EmptyProfiles);
            }
            if self.layout.is_some() || self.main_pane_ratio.is_some() || self.agents.is_some() {
                return Err(crate::config::ConfigError::MixedConfigShape);
            }
            Ok(())
        } else {
            let agents = self.agents.as_deref().unwrap_or_default();
            if agents.is_empty() {
                return Err(crate::config::ConfigError::NoAgents);
            }
            Ok(())
        }
    }
}

/// Minimal TOML shape used when scanning project IDs.
#[derive(Debug, Deserialize)]
pub(crate) struct ProjectIdOnly {
    /// Stable project ID.
    pub id: String,
}

/// Default tmux session-name prefix.
pub(crate) fn default_session_prefix() -> String {
    "ai".to_string()
}

/// Default main-pane ratio.
pub(crate) fn default_main_pane_ratio() -> u8 {
    50
}

/// Default tmux window name.
pub(crate) fn default_window_name() -> String {
    "agents".to_string()
}

/// Default shell path for shell-mode agents.
pub(crate) fn default_shell() -> String {
    "/bin/sh".to_string()
}

/// Default tmux executable name.
pub(crate) fn default_tmux_bin() -> String {
    "tmux".to_string()
}

/// Resolved orchestration settings for a project.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct OrchestrationConfig {
    /// Liveness timings for verified delivery and the stall watchdog.
    pub liveness: LivenessConfig,
}

impl OrchestrationConfig {
    /// Liveness settings that apply to `agent_id`.
    ///
    /// V1 resolves every agent to the global `[orchestration.liveness]`
    /// table. The accessor is the single resolution point so a later
    /// `[agents.<id>.liveness]` override table (the pool is heterogeneous:
    /// different agent CLIs have different latencies) can slot in without
    /// touching any call site.
    #[must_use]
    pub fn liveness_for(&self, _agent_id: &str) -> LivenessConfig {
        self.liveness
    }
}

/// Liveness timings for verified delivery and the stall watchdog,
/// configured through the optional `[orchestration.liveness]` table.
/// Absent table or fields fall back to the defaults below.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct LivenessConfig {
    /// Window after a verified landing within which the prompt must leave
    /// the pane's input area (delivery counted as ingested), in seconds.
    pub ingest_window_secs: u64,
    /// Age of a task's last durable activity past which the watchdog opens
    /// a stall episode, in seconds.
    pub stall_after_secs: u64,
    /// Window after a stall nudge without new durable activity past which
    /// the watchdog escalates to `needs_operator`, in seconds.
    pub escalate_after_secs: u64,
}

impl Default for LivenessConfig {
    fn default() -> Self {
        Self {
            ingest_window_secs: 15,
            stall_after_secs: 900,
            escalate_after_secs: 300,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orchestration_config_rejects_removed_engine_fields() {
        // The removed orchestration-engine settings must not parse anymore;
        // deny_unknown_fields makes stale configs fail loudly instead of
        // being silently ignored.
        for legacy in ["mode = \"automatic\"\n", "poll_interval_ms = 500\n"] {
            let err = toml::from_str::<OrchestrationConfig>(legacy)
                .err()
                .unwrap_or_else(|| panic!("removed engine field must fail to parse: {legacy}"));
            assert!(
                err.to_string().contains("unknown field"),
                "expected an unknown-field error for {legacy:?}, got: {err}"
            );
        }
    }

    #[test]
    fn liveness_defaults_apply_when_the_table_is_absent() {
        let config: OrchestrationConfig =
            toml::from_str("").unwrap_or_else(|err| panic!("parse: {err}"));
        assert_eq!(
            config.liveness,
            LivenessConfig {
                ingest_window_secs: 15,
                stall_after_secs: 900,
                escalate_after_secs: 300,
            }
        );
    }

    #[test]
    fn liveness_table_overrides_only_the_supplied_fields() {
        let config: OrchestrationConfig = toml::from_str("[liveness]\nstall_after_secs = 120\n")
            .unwrap_or_else(|err| panic!("parse: {err}"));
        assert_eq!(config.liveness.stall_after_secs, 120);
        assert_eq!(config.liveness.ingest_window_secs, 15);
        assert_eq!(config.liveness.escalate_after_secs, 300);
    }

    #[test]
    fn liveness_table_rejects_unknown_fields() {
        let err = toml::from_str::<OrchestrationConfig>("[liveness]\nstall_secs = 120\n")
            .err()
            .unwrap_or_else(|| panic!("unknown liveness field must fail to parse"));
        assert!(
            err.to_string().contains("stall_secs"),
            "expected the unknown field to be named, got: {err}"
        );
    }

    #[test]
    fn liveness_for_returns_the_global_settings_for_every_agent() {
        let config: OrchestrationConfig = toml::from_str("[liveness]\nescalate_after_secs = 60\n")
            .unwrap_or_else(|err| panic!("parse: {err}"));
        for agent in ["codex", "opus", "glm51", "opencode-1"] {
            assert_eq!(
                config.liveness_for(agent),
                config.liveness,
                "v1 resolves every agent to the global table (agent: {agent})"
            );
        }
    }
}
