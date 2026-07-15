use std::collections::BTreeMap;

use serde::Deserialize;

/// How strictly runtime-only configuration is resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResolutionMode {
    /// Keep `$VARS` unresolved and tolerate paths that disappeared after
    /// launch.
    Deferred,
    /// Resolve `$VARS` and require launch paths to exist.
    Runtime,
}

/// Supported tmux pane layouts for a workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Layout {
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub(crate) enum AgentMode {
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub(crate) enum RemainOnExit {
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
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct GlobalConfig {
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
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentTemplate {
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
}

impl ProjectFileRaw {
    /// Validate that the raw project uses exactly one supported config shape.
    ///
    /// An empty agent list is not checked here — profile selection funnels
    /// every shape through `resolve_project`, which rejects it as `NoAgents`.
    pub(crate) fn validate_shape(&self) -> Result<(), crate::config::ConfigError> {
        if let Some(profiles) = &self.profiles {
            if profiles.is_empty() {
                return Err(crate::config::ConfigError::EmptyProfiles);
            }
            if self.layout.is_some() || self.main_pane_ratio.is_some() || self.agents.is_some() {
                return Err(crate::config::ConfigError::MixedConfigShape);
            }
        }
        Ok(())
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
    "kira".to_string()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_file_rejects_removed_orchestration_table() {
        let toml = r#"
id = "demo"
root = "/tmp/demo"
orchestration = { liveness = { stall_after_secs = 120 } }

[[agents]]
id = "a"
command = "codex"
"#;
        let err = toml::from_str::<ProjectFileRaw>(toml)
            .err()
            .unwrap_or_else(|| panic!("removed orchestration table must fail to parse"));
        assert!(
            err.to_string().contains("unknown field"),
            "expected an unknown-field error, got: {err}"
        );
    }

    #[test]
    fn global_config_defaults_parse() {
        let config: GlobalConfig = toml::from_str("").unwrap_or_else(|err| panic!("parse: {err}"));
        assert_eq!(config.session_prefix, "kira");
        assert_eq!(config.main_pane_ratio, 50);
    }
}
