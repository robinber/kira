//! Agent + template merge into `ResolvedAgent` and fingerprint material.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::path::Path;

use super::super::error::ConfigError;
use super::super::fingerprint::{EnvValue, FingerprintAgentMaterial, classify_env_value};
use super::super::model::{AgentOverrides, AgentTemplate, ProjectAgent, ResolutionMode};
use super::paths::resolve_agent_cwd;
use super::validate::validate_agent;
use crate::model::ResolvedAgent;

type Result<T> = std::result::Result<T, ConfigError>;

pub(super) fn resolve_agents(
    agents: Vec<ProjectAgent>,
    template_map: &BTreeMap<String, &AgentTemplate>,
    root: &Path,
    resolution_mode: ResolutionMode,
) -> Result<(
    Vec<ResolvedAgent>,
    Vec<FingerprintAgentMaterial>,
    BTreeSet<String>,
)> {
    let mut seen = BTreeSet::new();
    let mut resolved = Vec::new();
    let mut fingerprint_materials = Vec::new();

    for agent in agents {
        if !seen.insert(agent.id.clone()) {
            return Err(ConfigError::DuplicateAgentId(agent.id));
        }

        let template = match agent.template.as_ref() {
            Some(name) => Some(
                template_map
                    .get(name)
                    .copied()
                    .ok_or_else(|| ConfigError::UnknownTemplate(name.clone()))?,
            ),
            None => None,
        };

        let (agent, material) = resolve_single_agent(agent, template, root, resolution_mode)?;
        resolved.push(agent);
        fingerprint_materials.push(material);
    }

    Ok((resolved, fingerprint_materials, seen))
}

pub(super) fn resolve_single_agent(
    agent: ProjectAgent,
    template: Option<&AgentTemplate>,
    root: &Path,
    resolution_mode: ResolutionMode,
) -> Result<(ResolvedAgent, FingerprintAgentMaterial)> {
    let merged = match template {
        Some(template) => agent.overrides().or(&template.overrides()),
        None => agent.overrides(),
    };
    let AgentOverrides {
        label,
        mode,
        command,
        shell_command,
        args,
        cwd,
        env: unresolved_env,
        capabilities,
        prompt_template,
        submit,
        text_delivery,
    } = merged;

    let label = label
        // A label-less template still names its panes after itself.
        .or_else(|| template.map(|item| item.name.clone()))
        .filter(|label| !label.is_empty())
        .unwrap_or_else(|| agent.id.clone());
    let mode = mode.unwrap_or_default();
    let args = args.unwrap_or_default();
    let cwd = resolve_agent_cwd(&agent.id, cwd.as_deref(), root, resolution_mode)?;

    validate_agent(
        &agent.id,
        mode,
        command.as_deref(),
        shell_command.as_deref(),
        &args,
    )?;

    let env = match resolution_mode {
        ResolutionMode::Deferred => unresolved_env.clone(),
        ResolutionMode::Runtime => resolve_env_map(&agent.id, unresolved_env.clone())?,
    };

    if let Some(ref tmpl) = prompt_template {
        let unknowns = crate::prompt::lint_template(tmpl);
        if !unknowns.is_empty() {
            tracing::warn!(
                "agent {} prompt_template has unknown variable(s): {}",
                agent.id,
                unknowns.join(", ")
            );
        }
    }

    let resolved = ResolvedAgent {
        id: agent.id,
        label,
        mode,
        command,
        shell_command,
        args,
        cwd,
        env,
        capabilities: capabilities.unwrap_or_default(),
        prompt_template,
        submit,
        text_delivery,
    };
    let fingerprint_material = FingerprintAgentMaterial::from_agent(&resolved, &unresolved_env);

    Ok((resolved, fingerprint_material))
}

pub(super) fn resolve_env_map(
    agent_id: &str,
    env_map: BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>> {
    let mut resolved = BTreeMap::new();

    for (key, value) in env_map {
        let resolved_value = match classify_env_value(&value) {
            EnvValue::Reference(reference) => {
                env::var(reference).map_err(|_source| ConfigError::UnresolvedEnvVar {
                    agent_id: agent_id.to_string(),
                    var_name: reference.to_string(),
                })?
            }
            EnvValue::Literal(_) => value,
        };
        resolved.insert(key, resolved_value);
    }

    Ok(resolved)
}

pub(super) fn build_template_map(
    templates: &[AgentTemplate],
) -> Result<BTreeMap<String, &AgentTemplate>> {
    let mut by_name = BTreeMap::new();

    for template in templates {
        if template.name.trim().is_empty() {
            return Err(ConfigError::EmptyTemplateName);
        }
        if by_name.insert(template.name.clone(), template).is_some() {
            return Err(ConfigError::DuplicateTemplate(template.name.clone()));
        }
    }

    Ok(by_name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::fingerprint::{FingerprintInput, compute_fingerprint};
    use crate::config::model::Layout;
    use crate::test_support::TestResultExt;

    #[test]
    fn resolve_env_map_reports_missing_environment_variable() {
        let variable = "KIRA_MUX_TEST_MISSING_ENV_RESTRICTION_7E3D2C";
        assert!(
            env::var_os(variable).is_none(),
            "reserved test variable must remain unset"
        );
        let env_map = BTreeMap::from([("TOKEN".to_string(), format!("${variable}"))]);

        let error = resolve_env_map("alpha", env_map)
            .err_or_panic("resolve_env_map_reports_missing_environment_variable: expected Err");
        let display = error.to_string();
        let ConfigError::UnresolvedEnvVar { agent_id, var_name } = error else {
            panic!("expected unresolved environment variable error");
        };

        assert_eq!(agent_id, "alpha");
        assert_eq!(var_name, variable);
        assert_eq!(
            display,
            format!("agent alpha references missing environment variable {variable}")
        );
    }

    #[test]
    fn empty_label_falls_back_to_agent_id() {
        let agent = ProjectAgent {
            id: "alpha".to_string(),
            template: None,
            label: Some(String::new()),
            mode: None,
            command: Some("echo".to_string()),
            shell_command: None,
            args: None,
            cwd: None,
            env: BTreeMap::new(),
            capabilities: None,
            prompt_template: None,
            submit: None,
            text_delivery: None,
        };

        let (resolved, _material) = resolve_single_agent(
            agent,
            None,
            Path::new("/tmp/kira-test-root"),
            ResolutionMode::Deferred,
        )
        .or_panic("empty_label_falls_back_to_agent_id");

        assert_eq!(
            resolved.label, "alpha",
            "empty label must fall back to the id, not render as `alpha ()`"
        );
    }

    #[test]
    fn agent_submit_and_text_delivery_override_template() {
        use crate::config::{SubmitPolicy, TextDelivery};

        let template = AgentTemplate {
            name: "coder".to_string(),
            label: None,
            mode: None,
            command: Some("my-agent".to_string()),
            shell_command: None,
            args: None,
            cwd: None,
            env: BTreeMap::new(),
            capabilities: None,
            prompt_template: None,
            submit: Some(SubmitPolicy::Double),
            text_delivery: Some(TextDelivery::SendKeys),
        };
        let agent = ProjectAgent {
            id: "alpha".to_string(),
            template: Some("coder".to_string()),
            label: None,
            mode: None,
            command: None,
            shell_command: None,
            args: None,
            cwd: None,
            env: BTreeMap::new(),
            capabilities: None,
            prompt_template: None,
            submit: Some(SubmitPolicy::Single),
            text_delivery: None,
        };

        let (resolved, _material) = resolve_single_agent(
            agent,
            Some(&template),
            Path::new("/tmp/kira-test-root"),
            ResolutionMode::Deferred,
        )
        .or_panic("agent_submit_and_text_delivery_override_template");

        assert_eq!(resolved.submit, Some(SubmitPolicy::Single));
        assert_eq!(resolved.text_delivery, Some(TextDelivery::SendKeys));
    }

    #[test]
    fn submit_and_text_delivery_do_not_affect_fingerprint() {
        use crate::config::{SubmitPolicy, TextDelivery};

        let root = Path::new("/tmp/kira-test-root");
        let base = || ProjectAgent {
            id: "alpha".to_string(),
            template: None,
            label: Some("Alpha".to_string()),
            mode: None,
            command: Some("echo".to_string()),
            shell_command: None,
            args: None,
            cwd: None,
            env: BTreeMap::new(),
            capabilities: Some(vec!["review".to_string()]),
            prompt_template: Some("{{user_prompt}}".to_string()),
            submit: None,
            text_delivery: None,
        };

        let mut with_defaults = base();
        with_defaults.submit = None;
        with_defaults.text_delivery = None;

        let mut with_overrides = base();
        with_overrides.submit = Some(SubmitPolicy::Double);
        with_overrides.text_delivery = Some(TextDelivery::SendKeys);
        with_overrides.label = Some("Other Label".to_string());
        with_overrides.capabilities = Some(vec!["impl".to_string()]);
        with_overrides.prompt_template = Some("review: {{user_prompt}}".to_string());

        let (resolved_defaults, material_defaults) =
            resolve_single_agent(with_defaults, None, root, ResolutionMode::Deferred)
                .or_panic("submit_and_text_delivery_do_not_affect_fingerprint");
        let (resolved_overrides, material_overrides) =
            resolve_single_agent(with_overrides, None, root, ResolutionMode::Deferred)
                .or_panic("submit_and_text_delivery_do_not_affect_fingerprint");

        assert_ne!(resolved_defaults.submit, resolved_overrides.submit);
        assert_ne!(
            resolved_defaults.text_delivery,
            resolved_overrides.text_delivery
        );

        let fingerprint = |material: &FingerprintAgentMaterial| {
            compute_fingerprint(FingerprintInput {
                project_id: "demo",
                profile_id: "default",
                root,
                layout: Layout::Auto,
                main_pane_ratio: 50,
                window_name: "agents",
                default_shell: "/bin/sh",
                remain_on_exit: crate::config::RemainOnExit::Failed,
                agents: std::slice::from_ref(material),
            })
        };

        assert_eq!(
            fingerprint(&material_defaults),
            fingerprint(&material_overrides),
            "send-time and cosmetic fields must not drift the fingerprint"
        );
    }
}
