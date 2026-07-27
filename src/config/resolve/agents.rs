//! Agent + template merge into `ResolvedAgent` and fingerprint material.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::path::Path;

use super::super::error::ConfigError;
use super::super::fingerprint::{EnvValue, FingerprintAgentMaterial, classify_env_value};
use super::super::model::{AgentTemplate, ProjectAgent, ResolutionMode};
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
    let label = agent
        .label
        .clone()
        .or_else(|| template.map(template_label))
        .filter(|label| !label.is_empty())
        .unwrap_or_else(|| agent.id.clone());
    let mode = agent
        .mode
        .or_else(|| template.and_then(|item| item.mode))
        .unwrap_or_default();
    let command = agent
        .command
        .clone()
        .or_else(|| template.and_then(|item| item.command.clone()));
    let shell_command = agent
        .shell_command
        .clone()
        .or_else(|| template.and_then(|item| item.shell_command.clone()));
    let args = agent
        .args
        .clone()
        .unwrap_or_else(|| template.map(|item| item.args.clone()).unwrap_or_default());
    let cwd = resolve_agent_cwd(
        &agent.id,
        agent
            .cwd
            .as_deref()
            .or_else(|| template.and_then(|item| item.cwd.as_deref())),
        root,
        resolution_mode,
    )?;

    let mut unresolved_env = template.map(|item| item.env.clone()).unwrap_or_default();
    unresolved_env.extend(agent.env.clone());

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

    let capabilities = match &agent.capabilities {
        Some(caps) => caps.clone(),
        None => template
            .map(|item| item.capabilities.clone())
            .unwrap_or_default(),
    };
    let prompt_template = agent
        .prompt_template
        .clone()
        .or_else(|| template.and_then(|item| item.prompt_template.clone()));
    let submit = agent
        .submit
        .or_else(|| template.and_then(|item| item.submit));
    let text_delivery = agent
        .text_delivery
        .or_else(|| template.and_then(|item| item.text_delivery));

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
        capabilities,
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

pub(super) fn template_label(template: &AgentTemplate) -> String {
    template
        .label
        .clone()
        .unwrap_or_else(|| template.name.clone())
}
