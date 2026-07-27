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
