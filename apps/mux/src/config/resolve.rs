use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::path::{Path, PathBuf};

use super::error::ConfigError;
use crate::domain::{ResolvedAgent, ResolvedProject};

type Result<T> = std::result::Result<T, ConfigError>;
use super::fingerprint::{
    FingerprintAgentMaterial, FingerprintInput, compute_fingerprint, env_fingerprint,
};
use super::model::{
    AgentMode, AgentTemplate, EnvResolutionMode, GlobalConfig, Layout, ProjectAgent, ProjectFile,
};

pub(crate) fn resolve_project(
    project: ProjectFile,
    profile_id: &str,
    global: &GlobalConfig,
    env_mode: EnvResolutionMode,
) -> Result<ResolvedProject> {
    validate_project_shape(&project)?;

    if profile_id.contains('\t') {
        return Err(ConfigError::TabInIdentifier {
            kind: "profile id",
            id: profile_id.to_string(),
        });
    }

    let (root, layout, main_pane_ratio, window_name, name) =
        resolve_workspace_defaults(&project, global)?;
    let template_map = build_template_map(&global.agent_templates)?;
    let (agents, fingerprint_agents, seen_agents) =
        resolve_agents(project.agents, &template_map, &root, env_mode)?;

    validate_groups(&project.groups, &seen_agents)?;

    let fingerprint = compute_fingerprint(FingerprintInput {
        project_id: &project.id,
        profile_id,
        root: &root,
        layout,
        main_pane_ratio,
        window_name: &window_name,
        default_shell: &global.default_shell,
        remain_on_exit: global.remain_on_exit,
        agents: &fingerprint_agents,
    });

    Ok(ResolvedProject {
        id: project.id,
        profile_id: profile_id.to_string(),
        name,
        root,
        layout,
        main_pane_ratio,
        window_name,
        session_prefix: global.session_prefix.clone(),
        default_shell: global.default_shell.clone(),
        remain_on_exit: global.remain_on_exit,
        tmux_bin: global.tmux_bin.clone(),
        agents,
        fingerprint,
        groups: project.groups,
        orchestration: project.orchestration,
    })
}

fn resolve_workspace_defaults(
    project: &ProjectFile,
    global: &GlobalConfig,
) -> Result<(PathBuf, Layout, u8, String, String)> {
    let root = normalize_project_root(&project.root)?;
    let layout = project.layout.unwrap_or(global.default_layout);
    let main_pane_ratio = project.main_pane_ratio.unwrap_or(global.main_pane_ratio);
    let window_name = project
        .window_name
        .clone()
        .unwrap_or_else(|| global.window_name.clone());
    let name = project.name.clone().unwrap_or_else(|| project.id.clone());

    if !(30..=70).contains(&main_pane_ratio) {
        return Err(ConfigError::MainPaneRatioOutOfRange);
    }

    Ok((root, layout, main_pane_ratio, window_name, name))
}

fn resolve_agents(
    agents: Vec<ProjectAgent>,
    template_map: &BTreeMap<String, &AgentTemplate>,
    root: &Path,
    env_mode: EnvResolutionMode,
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

        let (agent, material) = resolve_single_agent(agent, template, root, env_mode)?;
        resolved.push(agent);
        fingerprint_materials.push(material);
    }

    Ok((resolved, fingerprint_materials, seen))
}

#[expect(
    clippy::too_many_lines,
    reason = "the resolution pipeline keeps precedence, validation, and fingerprint material synchronized field by field"
)]
fn resolve_single_agent(
    agent: ProjectAgent,
    template: Option<&AgentTemplate>,
    root: &Path,
    env_mode: EnvResolutionMode,
) -> Result<(ResolvedAgent, FingerprintAgentMaterial)> {
    let label = agent
        .label
        .clone()
        .or_else(|| template.map(template_label))
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
    )?;

    let mut unresolved_env = template.map(|item| item.env.clone()).unwrap_or_default();
    unresolved_env.extend(agent.env.clone());

    validate_agent(
        &agent.id,
        mode,
        command.as_deref(),
        shell_command.as_deref(),
    )?;

    let fingerprint_material = FingerprintAgentMaterial {
        id: agent.id.clone(),
        label: label.clone(),
        mode,
        command: command.clone(),
        shell_command: shell_command.clone(),
        args: args.clone(),
        cwd: cwd.display().to_string(),
        env: unresolved_env
            .iter()
            .map(|(key, value)| (key.clone(), env_fingerprint(value)))
            .collect(),
    };

    let env = match env_mode {
        EnvResolutionMode::Deferred => unresolved_env,
        EnvResolutionMode::Runtime => resolve_env_map(&agent.id, unresolved_env)?,
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

    let orchestrator_prompt_template = agent
        .orchestrator_prompt_template
        .clone()
        .or_else(|| template.and_then(|item| item.orchestrator_prompt_template.clone()));

    if let Some(ref tmpl) = orchestrator_prompt_template {
        let unknowns = crate::prompt::lint_orchestrator_template(tmpl);
        if !unknowns.is_empty() {
            tracing::warn!(
                "agent {} orchestrator_prompt_template has unknown variable(s): {}",
                agent.id,
                unknowns.join(", ")
            );
        }
    }

    if orchestrator_prompt_template.is_some()
        && !capabilities.iter().any(|cap| cap == "orchestrator")
    {
        tracing::warn!(
            "agent {} orchestrator_prompt_template is ignored unless capability 'orchestrator' is present",
            agent.id
        );
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
        orchestrator_prompt_template,
    };

    Ok((resolved, fingerprint_material))
}

pub(crate) fn validate_global_config(global: &GlobalConfig) -> Result<()> {
    if !(30..=70).contains(&global.main_pane_ratio) {
        return Err(ConfigError::MainPaneRatioOutOfRange);
    }

    let _ = build_template_map(&global.agent_templates)?;
    Ok(())
}

fn validate_project_shape(project: &ProjectFile) -> Result<()> {
    if project.id.trim().is_empty() {
        return Err(ConfigError::EmptyProjectId);
    }
    if project.id.contains('\t') {
        return Err(ConfigError::TabInIdentifier {
            kind: "project id",
            id: project.id.clone(),
        });
    }
    if project.root.trim().is_empty() {
        return Err(ConfigError::EmptyProjectRoot);
    }
    if project.agents.is_empty() {
        return Err(ConfigError::NoAgents);
    }
    for agent in &project.agents {
        if agent.id.trim().is_empty() {
            return Err(ConfigError::EmptyAgentId);
        }
        if agent.id.contains('\t') {
            return Err(ConfigError::TabInIdentifier {
                kind: "agent id",
                id: agent.id.clone(),
            });
        }
    }

    Ok(())
}

fn validate_groups(
    groups: &BTreeMap<String, Vec<String>>,
    known_agents: &BTreeSet<String>,
) -> Result<()> {
    for (group_name, members) in groups {
        if group_name.trim().is_empty() {
            return Err(ConfigError::EmptyGroupName);
        }
        if members.is_empty() {
            return Err(ConfigError::EmptyGroup {
                group: group_name.clone(),
            });
        }
        let mut seen = BTreeSet::new();
        for member in members {
            if !seen.insert(member) {
                return Err(ConfigError::DuplicateAgentInGroup {
                    group: group_name.clone(),
                    agent: member.clone(),
                });
            }
            if !known_agents.contains(member) {
                return Err(ConfigError::UnknownAgentInGroup {
                    group: group_name.clone(),
                    agent: member.clone(),
                });
            }
        }
    }
    Ok(())
}

fn validate_agent(
    agent_id: &str,
    mode: AgentMode,
    command: Option<&str>,
    shell_command: Option<&str>,
) -> Result<()> {
    match mode {
        AgentMode::Direct if command.is_none_or(str::is_empty) => {
            Err(ConfigError::MissingCommand {
                agent_id: agent_id.to_string(),
            })
        }
        AgentMode::Shell if shell_command.is_none_or(str::is_empty) => {
            Err(ConfigError::MissingShellCommand {
                agent_id: agent_id.to_string(),
            })
        }
        _ => Ok(()),
    }
}

fn normalize_project_root(root: &str) -> Result<PathBuf> {
    let expanded = expand_path(root, None)?;

    if !expanded.exists() {
        return Err(ConfigError::ProjectRootNotFound(expanded));
    }
    if !expanded.is_dir() {
        return Err(ConfigError::ProjectRootNotDirectory(expanded));
    }

    expanded
        .canonicalize()
        .map_err(|source| ConfigError::PathResolution {
            path: expanded.clone(),
            source,
        })
}

fn resolve_agent_cwd(agent_id: &str, raw: Option<&str>, project_root: &Path) -> Result<PathBuf> {
    let Some(value) = raw else {
        return Ok(project_root.to_path_buf());
    };

    if value.trim().is_empty() {
        return Err(ConfigError::EmptyAgentCwd {
            agent_id: agent_id.to_string(),
        });
    }

    let expanded = expand_path(value, Some(project_root))?;
    let resolved = normalize_path(&expanded);

    let is_absolute_or_home =
        PathBuf::from(value).is_absolute() || value.starts_with("~/") || value == "~";

    if !is_absolute_or_home && !resolved.starts_with(project_root) {
        return Err(ConfigError::AgentCwdEscapesRoot {
            agent_id: agent_id.to_string(),
            path: resolved,
        });
    }

    if !is_absolute_or_home
        && resolved
            .symlink_metadata()
            .is_ok_and(|m| m.file_type().is_symlink())
        && let Some(path) = check_symlink_escape(&resolved, project_root)
    {
        return Err(ConfigError::AgentCwdEscapesRoot {
            agent_id: agent_id.to_string(),
            path,
        });
    }

    if !resolved.exists() {
        return Err(ConfigError::AgentCwdNotFound {
            agent_id: agent_id.to_string(),
            path: resolved,
        });
    }
    if !resolved.is_dir() {
        return Err(ConfigError::AgentCwdNotDirectory {
            agent_id: agent_id.to_string(),
            path: resolved,
        });
    }

    if !is_absolute_or_home {
        let canonical = resolved
            .canonicalize()
            .map_err(|source| ConfigError::PathResolution {
                path: resolved.clone(),
                source,
            })?;
        if !canonical.starts_with(project_root) {
            return Err(ConfigError::AgentCwdEscapesRoot {
                agent_id: agent_id.to_string(),
                path: canonical,
            });
        }
    }

    Ok(resolved)
}

fn resolve_env_map(
    agent_id: &str,
    env_map: BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>> {
    let mut resolved = BTreeMap::new();

    for (key, value) in env_map {
        if let Some(reference) = value.strip_prefix('$') {
            let resolved_value =
                env::var(reference).map_err(|_source| ConfigError::UnresolvedEnvVar {
                    agent_id: agent_id.to_string(),
                    var_name: reference.to_string(),
                })?;
            resolved.insert(key, resolved_value);
        } else {
            resolved.insert(key, value);
        }
    }

    Ok(resolved)
}

fn build_template_map(templates: &[AgentTemplate]) -> Result<BTreeMap<String, &AgentTemplate>> {
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

fn template_label(template: &AgentTemplate) -> String {
    template
        .label
        .clone()
        .unwrap_or_else(|| template.name.clone())
}

fn expand_path(value: &str, project_root: Option<&Path>) -> Result<PathBuf> {
    if let Some(rest) = value.strip_prefix("~/") {
        return Ok(home_dir()?.join(rest));
    }

    if value == "~" {
        return home_dir();
    }

    let path = PathBuf::from(value);
    if path.is_absolute() {
        Ok(normalize_path(&path))
    } else if let Some(root) = project_root {
        Ok(normalize_path(&root.join(path)))
    } else {
        let cwd = env::current_dir().map_err(|source| ConfigError::PathResolution {
            path: PathBuf::from("."),
            source,
        })?;
        Ok(normalize_path(&cwd.join(path)))
    }
}

/// Normalizes `.` and `..` components in-place. Parent traversals above the
/// root are clamped (silently dropped), not rejected.
fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();

    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }

    normalized
}

fn check_symlink_escape(path: &Path, project_root: &Path) -> Option<PathBuf> {
    match path.canonicalize() {
        Ok(canonical) if !canonical.starts_with(project_root) => Some(canonical),
        Err(_) => std::fs::read_link(path).ok().and_then(|target| {
            let effective = if target.is_absolute() {
                normalize_path(&target)
            } else {
                normalize_path(&path.parent().unwrap_or(project_root).join(target))
            };
            (!effective.starts_with(project_root)).then_some(effective)
        }),
        Ok(_) => None,
    }
}

fn map_home_dir_error(_source: anyhow::Error) -> ConfigError {
    ConfigError::FileRead {
        path: PathBuf::from("~"),
        source: std::io::Error::new(std::io::ErrorKind::NotFound, "HOME is not set"),
    }
}

fn home_dir() -> Result<PathBuf> {
    crate::paths::home_dir().map_err(map_home_dir_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestOptionExt, TestResultExt};

    fn base_project(root: &Path) -> ProjectFile {
        ProjectFile {
            id: "test".to_string(),
            name: None,
            root: root.to_string_lossy().into_owned(),
            layout: None,
            main_pane_ratio: None,
            window_name: None,
            agents: vec![ProjectAgent {
                id: "reviewer".to_string(),
                template: None,
                label: None,
                mode: None,
                command: Some("echo".to_string()),
                shell_command: None,
                args: None,
                cwd: None,
                env: BTreeMap::new(),
                capabilities: None,
                prompt_template: None,
                orchestrator_prompt_template: None,
            }],
            groups: BTreeMap::new(),
            orchestration: None,
        }
    }

    #[test]
    fn resolve_env_map_reports_missing_environment_variable() {
        let variable = "KIRA_MUX_TEST_MISSING_ENV_RESTRICTION_7E3D2C";
        assert!(
            env::var_os(variable).is_none(),
            "reserved test variable must remain unset"
        );
        let env_map = BTreeMap::from([("TOKEN".to_string(), format!("${variable}"))]);

        let error = resolve_env_map("alpha", env_map).err_or_panic();
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
    fn home_dir_error_preserves_existing_contract() {
        let error = map_home_dir_error(anyhow::anyhow!("ignored source"));
        let display = error.to_string();
        let ConfigError::FileRead { path, source } = error else {
            panic!("expected config file read error");
        };

        assert_eq!(path, PathBuf::from("~"));
        assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
        assert_eq!(source.to_string(), "HOME is not set");
        assert_eq!(display, "failed to read config file ~: HOME is not set");
    }

    // --- orchestrator_prompt_template resolution ---

    fn orchestrator_template() -> AgentTemplate {
        AgentTemplate {
            name: "codex-orchestrator".to_string(),
            label: None,
            mode: None,
            command: Some("codex".to_string()),
            shell_command: None,
            args: vec![],
            cwd: None,
            env: BTreeMap::new(),
            capabilities: vec!["orchestrator".to_string()],
            prompt_template: None,
            orchestrator_prompt_template: Some(
                "$orchestrator {{orchestrator_envelope}}".to_string(),
            ),
        }
    }

    #[test]
    fn orchestrator_prompt_template_inherits_from_template() {
        let temp = tempfile::tempdir().or_panic();
        let global = GlobalConfig {
            agent_templates: vec![orchestrator_template()],
            ..GlobalConfig::default()
        };
        let project = ProjectFile {
            id: "test".to_string(),
            name: None,
            root: temp.path().to_string_lossy().into_owned(),
            layout: None,
            main_pane_ratio: None,
            window_name: None,
            agents: vec![ProjectAgent {
                id: "orchestrator-1".to_string(),
                template: Some("codex-orchestrator".to_string()),
                label: None,
                mode: None,
                command: None,
                shell_command: None,
                args: None,
                cwd: None,
                env: BTreeMap::new(),
                capabilities: None,
                prompt_template: None,
                orchestrator_prompt_template: None,
            }],
            groups: BTreeMap::new(),
            orchestration: None,
        };
        let resolved =
            resolve_project(project, "default", &global, EnvResolutionMode::Deferred).or_panic();
        assert_eq!(
            resolved.agents[0].orchestrator_prompt_template.as_deref(),
            Some("$orchestrator {{orchestrator_envelope}}")
        );
    }

    #[test]
    fn orchestrator_prompt_template_agent_overrides_template() {
        let temp = tempfile::tempdir().or_panic();
        let global = GlobalConfig {
            agent_templates: vec![orchestrator_template()],
            ..GlobalConfig::default()
        };
        let project = ProjectFile {
            id: "test".to_string(),
            name: None,
            root: temp.path().to_string_lossy().into_owned(),
            layout: None,
            main_pane_ratio: None,
            window_name: None,
            agents: vec![ProjectAgent {
                id: "orchestrator-1".to_string(),
                template: Some("codex-orchestrator".to_string()),
                label: None,
                mode: None,
                command: None,
                shell_command: None,
                args: None,
                cwd: None,
                env: BTreeMap::new(),
                capabilities: None,
                prompt_template: None,
                orchestrator_prompt_template: Some("custom {{objective}}".to_string()),
            }],
            groups: BTreeMap::new(),
            orchestration: None,
        };
        let resolved =
            resolve_project(project, "default", &global, EnvResolutionMode::Deferred).or_panic();
        assert_eq!(
            resolved.agents[0].orchestrator_prompt_template.as_deref(),
            Some("custom {{objective}}")
        );
    }

    #[test]
    fn orchestrator_prompt_template_missing_resolves_to_none() {
        let temp = tempfile::tempdir().or_panic();
        let project = base_project(temp.path());
        let resolved = resolve_project(
            project,
            "default",
            &GlobalConfig::default(),
            EnvResolutionMode::Deferred,
        )
        .or_panic();
        assert!(resolved.agents[0].orchestrator_prompt_template.is_none());
    }

    #[test]
    fn orchestrator_prompt_template_does_not_change_fingerprint() {
        let temp = tempfile::tempdir().or_panic();
        let global = GlobalConfig::default();

        let project_without = base_project(temp.path());
        let resolved_without = resolve_project(
            project_without,
            "default",
            &global,
            EnvResolutionMode::Deferred,
        )
        .or_panic();

        let mut project_with = base_project(temp.path());
        project_with.agents[0].orchestrator_prompt_template =
            Some("{{orchestrator_envelope}}".to_string());
        let resolved_with = resolve_project(
            project_with,
            "default",
            &global,
            EnvResolutionMode::Deferred,
        )
        .or_panic();

        assert_eq!(
            resolved_without.fingerprint, resolved_with.fingerprint,
            "orchestrator_prompt_template must not affect the fingerprint"
        );
    }

    #[cfg(unix)]
    mod symlink_escape_tests {
        use std::os::unix::fs::symlink;

        use super::*;

        fn setup_project_root_with_subdir() -> tempfile::TempDir {
            let temp = tempfile::tempdir().or_panic();
            std::fs::create_dir(temp.path().join("subdir")).or_panic();
            temp
        }

        #[test]
        fn check_symlink_escape_fallback_on_broken_symlink() {
            let temp = setup_project_root_with_subdir();
            let link = temp.path().join("broken_link");
            symlink("/nonexistent/escape/target", &link).or_panic();
            let result = check_symlink_escape(&link, temp.path());
            assert!(
                result.is_some(),
                "expected escape detection via read_link fallback"
            );
            let escaped = result.or_panic();
            assert!(escaped.starts_with("/nonexistent"));
        }

        #[test]
        fn check_symlink_escape_detects_relative_escape() {
            let temp = setup_project_root_with_subdir();
            let link = temp.path().join("subdir/escape_link");
            symlink("../../..", &link).or_panic();
            let result = check_symlink_escape(&link, temp.path());
            assert!(result.is_some(), "expected relative escape detection");
        }

        #[test]
        fn check_symlink_escape_nested_relative_escape() {
            let temp = setup_project_root_with_subdir();
            let subdir = temp.path().join("subdir");
            std::fs::create_dir(subdir.join("nested")).or_panic();
            let link = subdir.join("nested/deep_escape");
            symlink("../../../..", &link).or_panic();
            let result = check_symlink_escape(&link, temp.path());
            assert!(
                result.is_some(),
                "expected nested relative escape detection"
            );
        }

        #[test]
        fn check_symlink_escape_detects_absolute_escape() {
            let temp = setup_project_root_with_subdir();
            let escape_target = tempfile::tempdir().or_panic();
            let link = temp.path().join("link");
            symlink(escape_target.path(), &link).or_panic();
            let result = check_symlink_escape(&link, temp.path());
            assert!(result.is_some(), "expected escape detection");
            let escaped = result.or_panic();
            assert!(
                !escaped.starts_with(temp.path()),
                "escaped path should be outside project root"
            );
        }

        #[test]
        fn check_symlink_escape_returns_none_when_canonical_inside_root() {
            let temp = tempfile::tempdir().or_panic();
            std::fs::create_dir_all(temp.path().join("a/b")).or_panic();
            let link = temp.path().join("a/b/link");
            symlink("..", &link).or_panic();
            let project_root = temp.path().canonicalize().or_panic();
            assert!(check_symlink_escape(&link, &project_root).is_none());
        }
    }
}
