//! Resolve raw project/template config into runtime `ResolvedProject` values.

mod agents;
mod paths;
mod validate;

use std::path::PathBuf;

use agents::{build_template_map, resolve_agents};

use super::error::ConfigError;
use super::fingerprint::{FingerprintInput, compute_fingerprint};
use super::model::{GlobalConfig, Layout, ProjectFile, ResolutionMode};
use crate::model::ResolvedProject;

type Result<T> = std::result::Result<T, ConfigError>;

pub(crate) use paths::normalize_project_root;
pub(crate) use validate::{validate_global_config, validate_main_pane_ratio};

pub(crate) fn resolve_project(
    project: ProjectFile,
    profile_id: &str,
    global: &GlobalConfig,
    resolution_mode: ResolutionMode,
) -> Result<ResolvedProject> {
    validate::validate_project_shape(&project)?;
    validate::validate_identifier("profile id", profile_id)?;

    let (root, layout, main_pane_ratio, window_name, name) =
        resolve_workspace_defaults(&project, global, resolution_mode)?;
    validate::validate_identifier("window name", &window_name)?;
    let template_map = build_template_map(&global.agent_templates)?;
    let (agents, fingerprint_agents, seen_agents) =
        resolve_agents(project.agents, &template_map, &root, resolution_mode)?;

    validate::validate_groups(&project.groups, &seen_agents)?;

    let mut resolved = ResolvedProject {
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
        fingerprint: String::new(),
        groups: project.groups,
    };
    resolved.fingerprint = compute_fingerprint(FingerprintInput::from_project(
        &resolved,
        &fingerprint_agents,
    ));
    Ok(resolved)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::{GlobalConfig, ProjectAgent, ProjectFile};
    use crate::test_support::TestResultExt;

    /// The one orchestration-level test: `resolve_project` wires validation,
    /// defaults, agent resolution, and the fingerprint together.
    #[test]
    fn resolve_project_produces_a_complete_fingerprinted_project() {
        let root = tempfile::tempdir().or_panic("resolve_project root");
        let project = ProjectFile {
            id: "demo".to_string(),
            name: None,
            root: root.path().display().to_string(),
            layout: None,
            main_pane_ratio: None,
            window_name: None,
            agents: vec![ProjectAgent {
                id: "alpha".to_string(),
                template: None,
                label: None,
                mode: None,
                command: Some("echo".to_string()),
                shell_command: None,
                args: None,
                cwd: None,
                env: std::collections::BTreeMap::new(),
                capabilities: None,
                prompt_template: None,
                submit: None,
                text_delivery: None,
            }],
            groups: std::collections::BTreeMap::new(),
        };

        let resolved = resolve_project(
            project,
            "default",
            &GlobalConfig::default(),
            ResolutionMode::Deferred,
        )
        .or_panic("resolve_project");

        assert_eq!(resolved.id, "demo");
        assert_eq!(resolved.name, "demo", "name defaults to the id");
        assert_eq!(resolved.agents.len(), 1);
        assert_eq!(
            resolved.agents[0].label, "alpha",
            "label falls back to the id"
        );
        assert!(
            !resolved.fingerprint.is_empty(),
            "the fingerprint must be computed from the constructed project"
        );
    }
}
