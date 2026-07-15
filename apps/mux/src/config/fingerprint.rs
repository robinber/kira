use std::collections::BTreeMap;
use std::path::Path;

use sha2::{Digest, Sha256};

use super::model::{AgentMode, Layout, RemainOnExit};

/// Sanitized fingerprint material for one agent.
///
/// Intentionally excludes `capabilities`, `prompt_template`, and `groups`.
/// These fields do not affect tmux pane topology (session/window/pane
/// structure), so including them would cause false-positive drift detection
/// when users change cosmetic agent metadata that does not require a
/// workspace restart.
///
/// Env values are reduced to key names and resolution strategy
/// so that resolved secrets never enter the fingerprint.
#[derive(Debug, Clone)]
pub(crate) struct FingerprintAgentMaterial {
    pub id: String,
    pub label: String,
    pub mode: AgentMode,
    pub command: Option<String>,
    pub shell_command: Option<String>,
    pub args: Vec<String>,
    pub cwd: String,
    pub env: BTreeMap<String, EnvFingerprint>,
}

/// How a single env entry is represented in the fingerprint.
#[derive(Debug, Clone)]
pub(crate) enum EnvFingerprint {
    /// A literal value — only the key name matters, not the value.
    Literal,
    /// An environment reference — the reference target is included.
    Reference(String),
}

/// All material that determines a project fingerprint.
#[derive(Clone, Copy)]
pub(crate) struct FingerprintInput<'a> {
    pub project_id: &'a str,
    pub profile_id: &'a str,
    pub root: &'a Path,
    pub layout: Layout,
    pub main_pane_ratio: u8,
    pub window_name: &'a str,
    pub default_shell: &'a str,
    pub remain_on_exit: RemainOnExit,
    pub agents: &'a [FingerprintAgentMaterial],
}

pub(crate) fn compute_fingerprint(input: FingerprintInput<'_>) -> String {
    let mut material = String::new();
    material.push_str("project_id=");
    material.push_str(input.project_id);
    material.push('\n');
    material.push_str("profile_id=");
    material.push_str(input.profile_id);
    material.push('\n');
    material.push_str("root=");
    material.push_str(&input.root.display().to_string());
    material.push('\n');
    material.push_str("layout=");
    material.push_str(input.layout.as_str());
    material.push('\n');
    material.push_str("main_pane_ratio=");
    material.push_str(&input.main_pane_ratio.to_string());
    material.push('\n');
    material.push_str("window_name=");
    material.push_str(input.window_name);
    material.push('\n');
    material.push_str("default_shell=");
    material.push_str(input.default_shell);
    material.push('\n');
    material.push_str("remain_on_exit=");
    material.push_str(input.remain_on_exit.as_str());
    material.push('\n');

    for agent in input.agents {
        material.push_str("agent.id=");
        material.push_str(&agent.id);
        material.push('\n');
        material.push_str("agent.label=");
        material.push_str(&agent.label);
        material.push('\n');
        material.push_str("agent.mode=");
        material.push_str(agent.mode.as_str());
        material.push('\n');
        match agent.mode {
            AgentMode::Direct => {
                material.push_str("agent.command=");
                material.push_str(agent.command.as_deref().unwrap_or_default());
                material.push('\n');
            }
            AgentMode::Shell => {
                material.push_str("agent.shell_command=");
                material.push_str(agent.shell_command.as_deref().unwrap_or_default());
                material.push('\n');
            }
        }
        material.push_str("agent.cwd=");
        material.push_str(&agent.cwd);
        material.push('\n');

        for arg in &agent.args {
            material.push_str("agent.arg=");
            material.push_str(arg);
            material.push('\n');
        }

        for (key, fingerprint) in &agent.env {
            material.push_str("agent.env.");
            material.push_str(key);
            material.push('=');
            match fingerprint {
                EnvFingerprint::Literal => material.push_str("literal"),
                EnvFingerprint::Reference(target) => {
                    material.push('$');
                    material.push_str(target);
                }
            }
            material.push('\n');
        }
    }

    hex::encode(Sha256::digest(material.as_bytes()))
}

/// Classify an env value for fingerprinting.
pub(crate) fn env_fingerprint(value: &str) -> EnvFingerprint {
    if let Some(reference) = value.strip_prefix('$') {
        EnvFingerprint::Reference(reference.to_string())
    } else {
        EnvFingerprint::Literal
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    struct Fixture {
        project_id: String,
        profile_id: String,
        root: PathBuf,
        layout: Layout,
        main_pane_ratio: u8,
        window_name: String,
        default_shell: String,
        remain_on_exit: RemainOnExit,
        agents: Vec<FingerprintAgentMaterial>,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                project_id: "demo-project".into(),
                profile_id: "default".into(),
                root: PathBuf::from("/tmp/demo"),
                layout: Layout::SideBySide,
                main_pane_ratio: 50,
                window_name: "main".into(),
                default_shell: "/bin/bash".into(),
                remain_on_exit: RemainOnExit::Failed,
                agents: vec![agent_alpha()],
            }
        }

        fn input(&self) -> FingerprintInput<'_> {
            FingerprintInput {
                project_id: &self.project_id,
                profile_id: &self.profile_id,
                root: &self.root,
                layout: self.layout,
                main_pane_ratio: self.main_pane_ratio,
                window_name: &self.window_name,
                default_shell: &self.default_shell,
                remain_on_exit: self.remain_on_exit,
                agents: &self.agents,
            }
        }
    }

    fn agent_alpha() -> FingerprintAgentMaterial {
        FingerprintAgentMaterial {
            id: "alpha".into(),
            label: "Alpha".into(),
            mode: AgentMode::Direct,
            command: Some("cmd-alpha".into()),
            shell_command: Some("shell-alpha".into()),
            args: vec!["--one".into(), "--two".into()],
            cwd: "/tmp/alpha".into(),
            env: BTreeMap::new(),
        }
    }

    fn agent_beta() -> FingerprintAgentMaterial {
        FingerprintAgentMaterial {
            id: "beta".into(),
            label: "Beta".into(),
            mode: AgentMode::Direct,
            command: Some("cmd-beta".into()),
            shell_command: Some("shell-beta".into()),
            args: vec!["--three".into()],
            cwd: "/tmp/beta".into(),
            env: BTreeMap::new(),
        }
    }

    // --- env_fingerprint helper ---

    #[test]
    fn env_fingerprint_dollar_prefix_is_reference() {
        match env_fingerprint("$HOME") {
            EnvFingerprint::Reference(target) => assert_eq!(target, "HOME"),
            EnvFingerprint::Literal => panic!("expected Reference, got Literal"),
        }
    }

    #[test]
    fn env_fingerprint_plain_string_is_literal() {
        assert!(matches!(
            env_fingerprint("plain-value"),
            EnvFingerprint::Literal
        ));
    }

    #[test]
    fn env_fingerprint_empty_string_is_literal() {
        assert!(matches!(env_fingerprint(""), EnvFingerprint::Literal));
    }

    // --- determinism ---

    #[test]
    fn same_input_produces_same_fingerprint() {
        let fx = Fixture::new();
        assert_eq!(
            compute_fingerprint(fx.input()),
            compute_fingerprint(fx.input())
        );
    }

    // --- top-level field sensitivity ---

    #[test]
    fn project_id_affects_fingerprint() {
        let mut fx = Fixture::new();
        let before = compute_fingerprint(fx.input());
        fx.project_id = "other-project".into();
        assert_ne!(before, compute_fingerprint(fx.input()));
    }

    #[test]
    fn profile_id_affects_fingerprint() {
        let mut fx = Fixture::new();
        let before = compute_fingerprint(fx.input());
        fx.profile_id = "other-profile".into();
        assert_ne!(before, compute_fingerprint(fx.input()));
    }

    #[test]
    fn root_affects_fingerprint() {
        let mut fx = Fixture::new();
        let before = compute_fingerprint(fx.input());
        fx.root = PathBuf::from("/tmp/other-root");
        assert_ne!(before, compute_fingerprint(fx.input()));
    }

    #[test]
    fn layout_affects_fingerprint() {
        let mut fx = Fixture::new();
        let before = compute_fingerprint(fx.input());
        fx.layout = Layout::Stacked;
        assert_ne!(before, compute_fingerprint(fx.input()));
    }

    #[test]
    fn main_pane_ratio_affects_fingerprint() {
        let mut fx = Fixture::new();
        let before = compute_fingerprint(fx.input());
        fx.main_pane_ratio = 75;
        assert_ne!(before, compute_fingerprint(fx.input()));
    }

    #[test]
    fn window_name_affects_fingerprint() {
        let mut fx = Fixture::new();
        let before = compute_fingerprint(fx.input());
        fx.window_name = "other-window".into();
        assert_ne!(before, compute_fingerprint(fx.input()));
    }

    #[test]
    fn default_shell_affects_fingerprint() {
        let mut fx = Fixture::new();
        let before = compute_fingerprint(fx.input());
        fx.default_shell = "/usr/bin/zsh".into();
        assert_ne!(before, compute_fingerprint(fx.input()));
    }

    #[test]
    fn remain_on_exit_affects_fingerprint() {
        let mut fx = Fixture::new();
        let before = compute_fingerprint(fx.input());
        fx.remain_on_exit = RemainOnExit::On;
        assert_ne!(before, compute_fingerprint(fx.input()));
    }

    // --- per-agent field sensitivity ---

    #[test]
    fn agent_id_affects_fingerprint() {
        let mut fx = Fixture::new();
        let before = compute_fingerprint(fx.input());
        fx.agents[0].id = "alpha-renamed".into();
        assert_ne!(before, compute_fingerprint(fx.input()));
    }

    #[test]
    fn agent_label_affects_fingerprint() {
        let mut fx = Fixture::new();
        let before = compute_fingerprint(fx.input());
        fx.agents[0].label = "Alpha Prime".into();
        assert_ne!(before, compute_fingerprint(fx.input()));
    }

    #[test]
    fn agent_mode_affects_fingerprint() {
        let mut fx = Fixture::new();
        let before = compute_fingerprint(fx.input());
        fx.agents[0].mode = AgentMode::Shell;
        assert_ne!(before, compute_fingerprint(fx.input()));
    }

    #[test]
    fn agent_command_affects_fingerprint_in_direct_mode() {
        let mut fx = Fixture::new();
        // fixture starts in AgentMode::Direct
        let before = compute_fingerprint(fx.input());
        fx.agents[0].command = Some("cmd-alpha-v2".into());
        assert_ne!(before, compute_fingerprint(fx.input()));
    }

    #[test]
    fn agent_shell_command_affects_fingerprint_in_shell_mode() {
        let mut fx = Fixture::new();
        fx.agents[0].mode = AgentMode::Shell;
        let before = compute_fingerprint(fx.input());
        fx.agents[0].shell_command = Some("shell-alpha-v2".into());
        assert_ne!(before, compute_fingerprint(fx.input()));
    }

    #[test]
    fn agent_cwd_affects_fingerprint() {
        let mut fx = Fixture::new();
        let before = compute_fingerprint(fx.input());
        fx.agents[0].cwd = "/tmp/alpha-v2".into();
        assert_ne!(before, compute_fingerprint(fx.input()));
    }

    #[test]
    fn agent_args_affect_fingerprint() {
        let mut fx = Fixture::new();
        let before = compute_fingerprint(fx.input());
        fx.agents[0].args.push("--extra".into());
        assert_ne!(before, compute_fingerprint(fx.input()));
    }

    // --- mode-aware inclusion ---

    #[test]
    fn direct_mode_ignores_shell_command() {
        let mut fx = Fixture::new();
        // fixture starts in AgentMode::Direct; shell_command is preset.
        let before = compute_fingerprint(fx.input());
        fx.agents[0].shell_command = Some("totally-different-shell-cmd".into());
        assert_eq!(before, compute_fingerprint(fx.input()));
    }

    #[test]
    fn shell_mode_ignores_command() {
        let mut fx = Fixture::new();
        fx.agents[0].mode = AgentMode::Shell;
        let before = compute_fingerprint(fx.input());
        fx.agents[0].command = Some("totally-different-direct-cmd".into());
        assert_eq!(before, compute_fingerprint(fx.input()));
    }

    // --- env secret redaction (critical invariant) ---

    #[test]
    fn literal_env_values_are_redacted_from_fingerprint() {
        let mut fx_a = Fixture::new();
        fx_a.agents[0]
            .env
            .insert("SECRET".into(), env_fingerprint("alpha-real-password"));

        let mut fx_b = Fixture::new();
        fx_b.agents[0]
            .env
            .insert("SECRET".into(), env_fingerprint("beta-other-secret"));

        assert_eq!(
            compute_fingerprint(fx_a.input()),
            compute_fingerprint(fx_b.input())
        );
    }

    // --- reference target sensitivity ---

    #[test]
    fn reference_target_affects_fingerprint() {
        let mut fx_a = Fixture::new();
        fx_a.agents[0]
            .env
            .insert("TOKEN".into(), env_fingerprint("$VAR_A"));

        let mut fx_b = Fixture::new();
        fx_b.agents[0]
            .env
            .insert("TOKEN".into(), env_fingerprint("$VAR_B"));

        assert_ne!(
            compute_fingerprint(fx_a.input()),
            compute_fingerprint(fx_b.input())
        );
    }

    // --- BTreeMap insertion order ---

    #[test]
    fn btreemap_insertion_order_does_not_affect_fingerprint() {
        let mut fx_a = Fixture::new();
        fx_a.agents[0]
            .env
            .insert("ALPHA".into(), EnvFingerprint::Literal);
        fx_a.agents[0]
            .env
            .insert("BETA".into(), EnvFingerprint::Literal);
        fx_a.agents[0]
            .env
            .insert("GAMMA".into(), EnvFingerprint::Literal);

        let mut fx_b = Fixture::new();
        fx_b.agents[0]
            .env
            .insert("GAMMA".into(), EnvFingerprint::Literal);
        fx_b.agents[0]
            .env
            .insert("BETA".into(), EnvFingerprint::Literal);
        fx_b.agents[0]
            .env
            .insert("ALPHA".into(), EnvFingerprint::Literal);

        assert_eq!(
            compute_fingerprint(fx_a.input()),
            compute_fingerprint(fx_b.input())
        );
    }

    // --- agent slice ordering ---

    #[test]
    fn agent_slice_order_affects_fingerprint() {
        let mut fx_a = Fixture::new();
        fx_a.agents = vec![agent_alpha(), agent_beta()];

        let mut fx_b = Fixture::new();
        fx_b.agents = vec![agent_beta(), agent_alpha()];

        assert_ne!(
            compute_fingerprint(fx_a.input()),
            compute_fingerprint(fx_b.input())
        );
    }

    // --- empty agents ---

    #[test]
    fn empty_agents_produces_valid_hex_fingerprint() {
        let mut fx = Fixture::new();
        fx.agents.clear();
        let fingerprint = compute_fingerprint(fx.input());
        assert_eq!(fingerprint.len(), 64);
        assert!(fingerprint.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
