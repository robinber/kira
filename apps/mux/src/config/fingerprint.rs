use std::collections::BTreeMap;
use std::path::Path;

use sha2::{Digest, Sha256};

use super::model::{AgentMode, Layout, RemainOnExit};

/// Sanitized fingerprint material for one agent.
///
/// Intentionally excludes `label`, `capabilities`, `prompt_template`, and
/// `groups`. These fields do not affect tmux pane topology
/// (session/window/pane structure), so including them would cause
/// false-positive drift detection when users change cosmetic agent metadata
/// that does not require a workspace restart.
///
/// Env entries:
/// - **Literal** values are hashed so secrets never appear in fingerprint
///   material, but editing the config value still changes the fingerprint
///   (session becomes **drifted**).
/// - **`$VAR` references** fingerprint only the variable *name*. Changing the
///   host environment value does **not** drift the session: `start` reuses
///   healthy panes without re-injecting env. Use **`restart`** to re-resolve
///   references and re-apply them to panes.
#[derive(Debug, Clone)]
pub(crate) struct FingerprintAgentMaterial {
    pub id: String,
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
    /// A literal value, reduced to a SHA-256 digest of the value bytes.
    /// Digest changes when the configured literal changes → drift.
    Literal(String),
    /// An environment reference — only the target name is stored (e.g. `HOME`
    /// for `$HOME`). Host value rotation does not affect the fingerprint;
    /// operators must `restart` to refresh pane env.
    Reference(String),
}

/// How a raw env value from config should be interpreted.
///
/// Shared by fingerprinting and runtime resolution so the `$`-prefix
/// classification cannot drift between the two.
pub(crate) enum EnvValue<'a> {
    /// A literal value used as-is.
    Literal(&'a str),
    /// A `$NAME` reference resolved from the process environment.
    Reference(&'a str),
}

/// Classify a raw env value from config.
pub(crate) fn classify_env_value(value: &str) -> EnvValue<'_> {
    match value.strip_prefix('$') {
        Some(reference) => EnvValue::Reference(reference),
        None => EnvValue::Literal(value),
    }
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
    push_field(&mut material, "project_id", input.project_id);
    push_field(&mut material, "profile_id", input.profile_id);
    push_field(&mut material, "root", &input.root.display().to_string());
    push_field(&mut material, "layout", input.layout.as_str());
    push_field(
        &mut material,
        "main_pane_ratio",
        &input.main_pane_ratio.to_string(),
    );
    push_field(&mut material, "window_name", input.window_name);
    push_field(&mut material, "default_shell", input.default_shell);
    push_field(
        &mut material,
        "remain_on_exit",
        input.remain_on_exit.as_str(),
    );

    for agent in input.agents {
        push_field(&mut material, "agent.id", &agent.id);
        push_field(&mut material, "agent.mode", agent.mode.as_str());
        match agent.mode {
            AgentMode::Direct => {
                push_field(
                    &mut material,
                    "agent.command",
                    agent.command.as_deref().unwrap_or_default(),
                );
                // args are only passed to the process in direct mode; hashing
                // them in shell mode would cause false drift.
                for arg in &agent.args {
                    push_field(&mut material, "agent.arg", arg);
                }
            }
            AgentMode::Shell => {
                push_field(
                    &mut material,
                    "agent.shell_command",
                    agent.shell_command.as_deref().unwrap_or_default(),
                );
            }
        }
        push_field(&mut material, "agent.cwd", &agent.cwd);

        for (key, fingerprint) in &agent.env {
            let value = match fingerprint {
                EnvFingerprint::Literal(digest) => format!("literal:{digest}"),
                EnvFingerprint::Reference(target) => format!("${target}"),
            };
            push_field(&mut material, &format!("agent.env.{key}"), &value);
        }
    }

    hex::encode(Sha256::digest(material.as_bytes()))
}

/// Append one `key=value` line, escaping `\` and newlines in the value so
/// two different inputs can never produce identical material.
fn push_field(material: &mut String, key: &str, value: &str) {
    material.push_str(key);
    material.push('=');
    for ch in value.chars() {
        match ch {
            '\\' => material.push_str("\\\\"),
            '\n' => material.push_str("\\n"),
            other => material.push(other),
        }
    }
    material.push('\n');
}

/// Reduce an env value to its fingerprint representation.
pub(crate) fn env_fingerprint(value: &str) -> EnvFingerprint {
    match classify_env_value(value) {
        EnvValue::Reference(target) => EnvFingerprint::Reference(target.to_string()),
        EnvValue::Literal(literal) => {
            EnvFingerprint::Literal(hex::encode(Sha256::digest(literal.as_bytes())))
        }
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
            EnvFingerprint::Literal(_) => panic!("expected Reference, got Literal"),
        }
    }

    #[test]
    fn env_fingerprint_plain_string_is_hashed_literal() {
        match env_fingerprint("plain-value") {
            EnvFingerprint::Literal(digest) => {
                assert_eq!(digest.len(), 64);
                assert!(
                    !digest.contains("plain-value"),
                    "literal value must not appear in the fingerprint material"
                );
            }
            EnvFingerprint::Reference(_) => panic!("expected Literal, got Reference"),
        }
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
    fn agent_args_affect_fingerprint_in_direct_mode() {
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

    #[test]
    fn shell_mode_ignores_args() {
        // Launch ignores args in shell mode, so the fingerprint must too.
        let mut fx = Fixture::new();
        fx.agents[0].mode = AgentMode::Shell;
        let before = compute_fingerprint(fx.input());
        fx.agents[0].args.push("--extra".into());
        assert_eq!(before, compute_fingerprint(fx.input()));
    }

    // --- env value sensitivity without exposure ---

    #[test]
    fn literal_env_value_change_affects_fingerprint() {
        let mut fx_a = Fixture::new();
        fx_a.agents[0]
            .env
            .insert("SECRET".into(), env_fingerprint("alpha-real-password"));

        let mut fx_b = Fixture::new();
        fx_b.agents[0]
            .env
            .insert("SECRET".into(), env_fingerprint("beta-other-secret"));

        assert_ne!(
            compute_fingerprint(fx_a.input()),
            compute_fingerprint(fx_b.input()),
            "editing a literal env value must be detected as drift"
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

    #[test]
    fn reference_host_value_rotation_does_not_affect_fingerprint() {
        // Config stays `TOKEN = "$KIRA_TOKEN"`. Only the process env changes.
        // Fingerprint material never sees the resolved secret, so rotation of
        // the host value alone must not mark the workspace drifted.
        let mut fx = Fixture::new();
        fx.agents[0]
            .env
            .insert("TOKEN".into(), env_fingerprint("$KIRA_TOKEN"));
        let first = compute_fingerprint(fx.input());
        let second = compute_fingerprint(fx.input());
        assert_eq!(
            first, second,
            "same $VAR reference must yield a stable fingerprint regardless \
             of whatever value the host currently has for that name"
        );
        // Sanity: still a reference form in material, not a hashed secret.
        match fx.agents[0].env.get("TOKEN") {
            Some(EnvFingerprint::Reference(name)) => assert_eq!(name, "KIRA_TOKEN"),
            other => panic!("expected Reference, got {other:?}"),
        }
    }

    // --- material ambiguity ---

    #[test]
    fn newline_in_value_cannot_forge_another_field() {
        let mut fx_a = Fixture::new();
        fx_a.agents[0].args = vec!["--a\nagent.arg=--b".into()];

        let mut fx_b = Fixture::new();
        fx_b.agents[0].args = vec!["--a".into(), "--b".into()];

        assert_ne!(
            compute_fingerprint(fx_a.input()),
            compute_fingerprint(fx_b.input()),
            "an embedded newline must not collide with two separate args"
        );
    }

    // --- BTreeMap insertion order ---

    #[test]
    fn btreemap_insertion_order_does_not_affect_fingerprint() {
        let mut fx_a = Fixture::new();
        fx_a.agents[0]
            .env
            .insert("ALPHA".into(), env_fingerprint("one"));
        fx_a.agents[0]
            .env
            .insert("BETA".into(), env_fingerprint("two"));
        fx_a.agents[0]
            .env
            .insert("GAMMA".into(), env_fingerprint("three"));

        let mut fx_b = Fixture::new();
        fx_b.agents[0]
            .env
            .insert("GAMMA".into(), env_fingerprint("three"));
        fx_b.agents[0]
            .env
            .insert("BETA".into(), env_fingerprint("two"));
        fx_b.agents[0]
            .env
            .insert("ALPHA".into(), env_fingerprint("one"));

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
