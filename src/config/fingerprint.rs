//! Deterministic config fingerprint for workspace drift detection.

use std::collections::BTreeMap;
use std::path::Path;

use sha2::{Digest, Sha256};

use super::model::{AgentMode, Layout, RemainOnExit};
use crate::model::{ResolvedAgent, ResolvedProject};

/// Sanitized fingerprint material for one agent.
///
/// Intentionally excludes `label`, `capabilities`, `prompt_template`,
/// `submit`, `text_delivery`, and `groups`. These fields do not affect tmux
/// pane topology (session/window/pane structure), so including them would
/// cause false-positive drift detection when users change cosmetic or
/// send-time agent metadata that does not require a workspace restart.
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

impl FingerprintAgentMaterial {
    /// Build material from a resolved agent plus its **unresolved** env map
    /// (fingerprints hash `$VAR` references by name, never host values).
    ///
    /// Exhaustive destructuring, no `..` rest pattern: adding a
    /// [`ResolvedAgent`] field is a compile error here until the field gets
    /// an explicit include/exclude decision.
    pub(crate) fn from_agent(
        agent: &ResolvedAgent,
        unresolved_env: &BTreeMap<String, String>,
    ) -> Self {
        let ResolvedAgent {
            id,
            // Cosmetic display name; no pane-topology impact.
            label: _,
            mode,
            command,
            shell_command,
            args,
            cwd,
            // Hashed from `unresolved_env` so references stay by-name.
            env: _,
            // Send-time metadata; no pane-topology impact.
            capabilities: _,
            prompt_template: _,
            submit: _,
            text_delivery: _,
        } = agent;
        Self {
            id: id.clone(),
            mode: *mode,
            command: command.clone(),
            shell_command: shell_command.clone(),
            args: args.clone(),
            cwd: cwd.display().to_string(),
            env: unresolved_env
                .iter()
                .map(|(key, value)| (key.clone(), env_fingerprint(value)))
                .collect(),
        }
    }
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

impl<'a> FingerprintInput<'a> {
    /// Build fingerprint input from the resolved project (its `fingerprint`
    /// field still unset) plus the pre-sanitized per-agent material.
    ///
    /// Exhaustive destructuring, no `..` rest pattern: adding a
    /// [`ResolvedProject`] field is a compile error here until the field
    /// gets an explicit include/exclude decision.
    pub(crate) fn from_project(
        project: &'a ResolvedProject,
        agents: &'a [FingerprintAgentMaterial],
    ) -> Self {
        let ResolvedProject {
            id,
            profile_id,
            // Display-only name; no topology impact.
            name: _,
            root,
            layout,
            main_pane_ratio,
            window_name,
            // Session identity is the *lookup key* for drift comparison,
            // not hashed content — changing the prefix addresses a
            // different session rather than drifting the old one.
            session_prefix: _,
            default_shell,
            remain_on_exit,
            // Transport binary choice; does not change pane topology.
            tmux_bin: _,
            // Hashed via the sanitized per-agent material instead.
            agents: _,
            // The output being computed.
            fingerprint: _,
            // Send-time grouping; no topology impact.
            groups: _,
        } = project;
        Self {
            project_id: id,
            profile_id,
            root,
            layout: *layout,
            main_pane_ratio: *main_pane_ratio,
            window_name,
            default_shell,
            remain_on_exit: *remain_on_exit,
            agents,
        }
    }
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
mod tests;
