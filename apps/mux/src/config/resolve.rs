use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::path::{Path, PathBuf};

use super::error::ConfigError;
use crate::model::{ResolvedAgent, ResolvedProject};

type Result<T> = std::result::Result<T, ConfigError>;
use super::fingerprint::{
    EnvValue, FingerprintAgentMaterial, FingerprintInput, classify_env_value, compute_fingerprint,
    env_fingerprint,
};
use super::model::{
    AgentMode, AgentTemplate, GlobalConfig, Layout, ProjectAgent, ProjectFile, ResolutionMode,
};

/// Non-whitespace characters rejected in identifiers that end up in tmux
/// session names or target syntax (`session:window.pane`). All Unicode
/// whitespace is rejected separately because tmux option reads are trimmed.
const FORBIDDEN_IDENTIFIER_CHARS: &[char] = &[':', '.'];

fn validate_identifier(kind: &'static str, id: &str) -> Result<()> {
    if let Some(ch) = id
        .chars()
        .find(|ch| ch.is_whitespace() || FORBIDDEN_IDENTIFIER_CHARS.contains(ch))
    {
        return Err(ConfigError::InvalidIdentifierChar {
            kind,
            id: id.to_string(),
            ch,
        });
    }
    Ok(())
}

pub(crate) fn validate_main_pane_ratio(ratio: u8) -> Result<()> {
    if (30..=70).contains(&ratio) {
        Ok(())
    } else {
        Err(ConfigError::MainPaneRatioOutOfRange)
    }
}

pub(crate) fn resolve_project(
    project: ProjectFile,
    profile_id: &str,
    global: &GlobalConfig,
    resolution_mode: ResolutionMode,
) -> Result<ResolvedProject> {
    validate_project_shape(&project)?;
    validate_identifier("profile id", profile_id)?;
    validate_identifier("session prefix", &global.session_prefix)?;

    let (root, layout, main_pane_ratio, window_name, name) =
        resolve_workspace_defaults(&project, global, resolution_mode)?;
    validate_identifier("window name", &window_name)?;
    let template_map = build_template_map(&global.agent_templates)?;
    let (agents, fingerprint_agents, seen_agents) =
        resolve_agents(project.agents, &template_map, &root, resolution_mode)?;

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
    })
}

fn resolve_workspace_defaults(
    project: &ProjectFile,
    global: &GlobalConfig,
    resolution_mode: ResolutionMode,
) -> Result<(PathBuf, Layout, u8, String, String)> {
    let root = normalize_project_root(&project.root, resolution_mode)?;
    let layout = project.layout.unwrap_or(global.default_layout);
    let main_pane_ratio = project.main_pane_ratio.unwrap_or(global.main_pane_ratio);
    let window_name = project
        .window_name
        .clone()
        .unwrap_or_else(|| global.window_name.clone());
    let name = project.name.clone().unwrap_or_else(|| project.id.clone());

    validate_main_pane_ratio(main_pane_ratio)?;

    Ok((root, layout, main_pane_ratio, window_name, name))
}

fn resolve_agents(
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

fn resolve_single_agent(
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

    let fingerprint_material = FingerprintAgentMaterial {
        id: agent.id.clone(),
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

    let env = match resolution_mode {
        ResolutionMode::Deferred => unresolved_env,
        ResolutionMode::Runtime => resolve_env_map(&agent.id, unresolved_env)?,
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
    };

    Ok((resolved, fingerprint_material))
}

pub(crate) fn validate_global_config(global: &GlobalConfig) -> Result<()> {
    validate_main_pane_ratio(global.main_pane_ratio)?;

    let _ = build_template_map(&global.agent_templates)?;
    Ok(())
}

fn validate_project_shape(project: &ProjectFile) -> Result<()> {
    if project.id.trim().is_empty() {
        return Err(ConfigError::EmptyProjectId);
    }
    validate_identifier("project id", &project.id)?;
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
        validate_identifier("agent id", &agent.id)?;
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
    args: &[String],
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
        // Launch only passes args in direct mode; rejecting here keeps config
        // honest instead of silently ignoring shell-mode args.
        AgentMode::Shell if !args.is_empty() => Err(ConfigError::ShellArgsNotSupported {
            agent_id: agent_id.to_string(),
        }),
        _ => Ok(()),
    }
}

fn normalize_project_root(root: &str, resolution_mode: ResolutionMode) -> Result<PathBuf> {
    let expanded = expand_path(root, None)?;

    if !expanded.exists() && resolution_mode == ResolutionMode::Runtime {
        return Err(ConfigError::ProjectRootNotFound(expanded));
    }
    if expanded.exists() && !expanded.is_dir() {
        return Err(ConfigError::ProjectRootNotDirectory(expanded));
    }

    // Keep the normalized configured path as the stable workspace identity.
    // Canonicalizing here would change the session hash when a configured
    // symlink becomes broken after launch, making the session impossible to
    // find for status or cleanup.
    Ok(expanded)
}

fn resolve_agent_cwd(
    agent_id: &str,
    raw: Option<&str>,
    project_root: &Path,
    resolution_mode: ResolutionMode,
) -> Result<PathBuf> {
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

    if !resolved.exists() && resolution_mode == ResolutionMode::Deferred {
        return Ok(resolved);
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
        let canonical_root =
            project_root
                .canonicalize()
                .map_err(|source| ConfigError::PathResolution {
                    path: project_root.to_path_buf(),
                    source,
                })?;
        let canonical = resolved
            .canonicalize()
            .map_err(|source| ConfigError::PathResolution {
                path: resolved.clone(),
                source,
            })?;
        if !canonical.starts_with(&canonical_root) {
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
    let canonical_root = project_root.canonicalize().ok()?;
    match path.canonicalize() {
        Ok(canonical) if !canonical.starts_with(&canonical_root) => Some(canonical),
        Err(_) => std::fs::read_link(path).ok().and_then(|target| {
            let effective = if target.is_absolute() {
                normalize_path(&target)
            } else {
                let parent = path.parent().unwrap_or(project_root);
                let resolved_parent = parent
                    .canonicalize()
                    .unwrap_or_else(|_| normalize_path(parent));
                normalize_path(&resolved_parent.join(target))
            };
            (!effective.starts_with(&canonical_root)).then_some(effective)
        }),
        Ok(_) => None,
    }
}

fn home_dir() -> Result<PathBuf> {
    crate::paths::home_dir().map_err(|_source| ConfigError::HomeDirUnavailable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestOptionExt, TestResultExt};

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
    fn forbidden_identifier_chars_are_rejected() {
        for (id, expected_ch) in [
            ("a:b", ':'),
            ("a.b", '.'),
            ("a\tb", '\t'),
            ("a\nb", '\n'),
            ("a\rb", '\r'),
            // Padded ids round-trip through trimmed tmux options and would
            // report permanent drift.
            (" alpha", ' '),
            ("a b", ' '),
            ("a ", ' '),
            ("a\u{00a0}", '\u{00a0}'),
        ] {
            let error = validate_identifier("agent id", id).err_or_panic();
            let ConfigError::InvalidIdentifierChar { kind, id: got, ch } = error else {
                panic!("expected InvalidIdentifierChar for {id:?}");
            };
            assert_eq!(kind, "agent id");
            assert_eq!(got, id);
            assert_eq!(ch, expected_ch);
        }

        validate_identifier("agent id", "plain-id_09").or_panic();
    }

    #[test]
    fn shell_mode_rejects_nonempty_args() {
        let error = validate_agent(
            "worker",
            AgentMode::Shell,
            None,
            Some("npm test"),
            &["--watch".to_string()],
        )
        .err_or_panic();
        assert!(matches!(
            error,
            ConfigError::ShellArgsNotSupported { agent_id } if agent_id == "worker"
        ));
    }

    #[test]
    fn shell_mode_allows_empty_args() {
        validate_agent("worker", AgentMode::Shell, None, Some("npm test"), &[]).or_panic();
    }

    #[test]
    fn direct_mode_allows_args() {
        validate_agent(
            "coder",
            AgentMode::Direct,
            Some("codex"),
            None,
            &["--full-auto".to_string()],
        )
        .or_panic();
    }

    #[test]
    fn project_root_identity_survives_directory_deletion() {
        let base = match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(error) => panic!("failed to create tempdir: {error}"),
        };
        let root = base.path().join("workdir");
        if let Err(error) = std::fs::create_dir(&root) {
            panic!("failed to create workdir: {error}");
        }
        let configured = root.display().to_string();

        let before = normalize_project_root(&configured, ResolutionMode::Deferred).or_panic();
        if let Err(error) = std::fs::remove_dir(&root) {
            panic!("failed to remove workdir: {error}");
        }
        let after = normalize_project_root(&configured, ResolutionMode::Deferred).or_panic();

        assert_eq!(
            before, after,
            "resolved root (and thus the derived session name) must be \
             identical before and after the directory disappears"
        );
    }

    #[test]
    fn deferred_resolution_tolerates_missing_root_and_explicit_agent_cwd() {
        let base = tempfile::tempdir().or_panic();
        let missing_root = base.path().join("missing-root");
        let root = normalize_project_root(
            &missing_root.display().to_string(),
            ResolutionMode::Deferred,
        )
        .or_panic();

        let cwd =
            resolve_agent_cwd("alpha", Some("subdir"), &root, ResolutionMode::Deferred).or_panic();

        assert_eq!(cwd, missing_root.join("subdir"));
    }

    #[test]
    fn runtime_resolution_rejects_missing_project_root() {
        let base = tempfile::tempdir().or_panic();
        let missing_root = base.path().join("missing-root");

        let error =
            normalize_project_root(&missing_root.display().to_string(), ResolutionMode::Runtime)
                .err_or_panic();

        assert!(matches!(error, ConfigError::ProjectRootNotFound(path) if path == missing_root));
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
        };

        let (resolved, _material) = resolve_single_agent(
            agent,
            None,
            Path::new("/tmp/kira-test-root"),
            ResolutionMode::Deferred,
        )
        .or_panic();

        assert_eq!(
            resolved.label, "alpha",
            "empty label must fall back to the id, not render as `alpha ()`"
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

        #[test]
        fn project_root_identity_survives_broken_configured_symlink() {
            let temp = tempfile::tempdir().or_panic();
            let target = temp.path().join("target");
            let link = temp.path().join("project-link");
            std::fs::create_dir(&target).or_panic();
            symlink(&target, &link).or_panic();
            let configured = link.display().to_string();

            let before = normalize_project_root(&configured, ResolutionMode::Deferred).or_panic();
            std::fs::remove_dir(&target).or_panic();
            let after = normalize_project_root(&configured, ResolutionMode::Deferred).or_panic();

            assert_eq!(before, after);
            assert_eq!(after, link);
        }
    }
}
