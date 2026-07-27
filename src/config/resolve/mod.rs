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
    validate::validate_identifier("session prefix", &global.session_prefix)?;

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
    use std::collections::BTreeMap;
    use std::env;
    use std::path::Path;

    use super::agents::{resolve_env_map, resolve_single_agent};
    use super::paths::{
        check_symlink_escape, normalize_project_root, require_stable_project_root,
        resolve_agent_cwd,
    };
    use super::validate::{validate_agent, validate_identifier};
    use super::*;
    use crate::config::error::ConfigError;
    use crate::config::fingerprint::FingerprintAgentMaterial;
    use crate::config::model::{AgentMode, AgentTemplate, ProjectAgent, ResolutionMode};
    use crate::test_support::{TestOptionExt, TestResultExt};

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
            let error = validate_identifier("agent id", id)
                .err_or_panic("forbidden_identifier_chars_are_rejected: expected Err");
            let ConfigError::InvalidIdentifierChar { kind, id: got, ch } = error else {
                panic!("expected InvalidIdentifierChar for {id:?}");
            };
            assert_eq!(kind, "agent id");
            assert_eq!(got, id);
            assert_eq!(ch, expected_ch);
        }

        validate_identifier("agent id", "plain-id_09")
            .or_panic("forbidden_identifier_chars_are_rejected");
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
        .err_or_panic("shell_mode_rejects_nonempty_args: expected Err");
        assert!(matches!(
            error,
            ConfigError::ShellArgsNotSupported { agent_id } if agent_id == "worker"
        ));
    }

    #[test]
    fn shell_mode_allows_empty_args() {
        validate_agent("worker", AgentMode::Shell, None, Some("npm test"), &[])
            .or_panic("shell_mode_allows_empty_args");
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
        .or_panic("direct_mode_allows_args");
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

        let before = normalize_project_root(&configured, ResolutionMode::Deferred)
            .or_panic("project_root_identity_survives_directory_deletion");
        if let Err(error) = std::fs::remove_dir(&root) {
            panic!("failed to remove workdir: {error}");
        }
        let after = normalize_project_root(&configured, ResolutionMode::Deferred)
            .or_panic("project_root_identity_survives_directory_deletion");

        assert_eq!(
            before, after,
            "resolved root (and thus the derived session name) must be \
             identical before and after the directory disappears"
        );
    }

    #[test]
    fn project_root_rejects_relative_paths() {
        for root in [".", "relative", "../sibling", "tmp/project"] {
            let error = normalize_project_root(root, ResolutionMode::Deferred)
                .err_or_panic("project_root_rejects_relative_paths: expected Err");
            assert!(
                matches!(
                    &error,
                    ConfigError::RelativeProjectRoot(got) if got == root
                ),
                "expected RelativeProjectRoot for {root:?}, got {error:?}"
            );
        }
    }

    #[test]
    fn absolute_project_root_is_accepted_by_stability_gate() {
        let base = match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(error) => panic!("failed to create tempdir: {error}"),
        };
        let configured = base.path().display().to_string();
        assert!(
            Path::new(&configured).is_absolute(),
            "temp paths must be absolute for this test"
        );

        require_stable_project_root(&configured)
            .or_panic("absolute_project_root_is_accepted_by_stability_gate");
    }

    #[test]
    fn home_relative_project_root_is_accepted() {
        // HOME may be unset in some environments; expand_path would fail then.
        // We only assert the stability gate accepts the form.
        require_stable_project_root("~/projects/demo")
            .or_panic("home_relative_project_root_is_accepted");
        require_stable_project_root("~").or_panic("home_relative_project_root_is_accepted");
    }

    #[test]
    fn deferred_resolution_tolerates_missing_root_and_explicit_agent_cwd() {
        let base = tempfile::tempdir()
            .or_panic("deferred_resolution_tolerates_missing_root_and_explicit_agent_cwd");
        let missing_root = base.path().join("missing-root");
        let root = normalize_project_root(
            &missing_root.display().to_string(),
            ResolutionMode::Deferred,
        )
        .or_panic("deferred_resolution_tolerates_missing_root_and_explicit_agent_cwd");

        let cwd = resolve_agent_cwd("alpha", Some("subdir"), &root, ResolutionMode::Deferred)
            .or_panic("deferred_resolution_tolerates_missing_root_and_explicit_agent_cwd");

        assert_eq!(cwd, missing_root.join("subdir"));
    }

    #[test]
    fn runtime_resolution_rejects_missing_project_root() {
        let base = tempfile::tempdir().or_panic("runtime_resolution_rejects_missing_project_root");
        let missing_root = base.path().join("missing-root");

        let error =
            normalize_project_root(&missing_root.display().to_string(), ResolutionMode::Runtime)
                .err_or_panic("runtime_resolution_rejects_missing_project_root: expected Err");

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

    #[cfg(unix)]
    mod symlink_escape_tests {
        use std::os::unix::fs::symlink;

        use super::*;

        fn setup_project_root_with_subdir() -> tempfile::TempDir {
            let temp = tempfile::tempdir().or_panic("setup_project_root_with_subdir");
            std::fs::create_dir(temp.path().join("subdir"))
                .or_panic("setup_project_root_with_subdir");
            temp
        }

        #[test]
        fn check_symlink_escape_fallback_on_broken_symlink() {
            let temp = setup_project_root_with_subdir();
            let link = temp.path().join("broken_link");
            symlink("/nonexistent/escape/target", &link)
                .or_panic("check_symlink_escape_fallback_on_broken_symlink");
            let result = check_symlink_escape(&link, temp.path());
            assert!(
                result.is_some(),
                "expected escape detection via read_link fallback"
            );
            let escaped = result.or_panic("check_symlink_escape_fallback_on_broken_symlink");
            assert!(escaped.starts_with("/nonexistent"));
        }

        #[test]
        fn check_symlink_escape_detects_relative_escape() {
            let temp = setup_project_root_with_subdir();
            let link = temp.path().join("subdir/escape_link");
            symlink("../../..", &link).or_panic("check_symlink_escape_detects_relative_escape");
            let result = check_symlink_escape(&link, temp.path());
            assert!(result.is_some(), "expected relative escape detection");
        }

        #[test]
        fn check_symlink_escape_nested_relative_escape() {
            let temp = setup_project_root_with_subdir();
            let subdir = temp.path().join("subdir");
            std::fs::create_dir(subdir.join("nested"))
                .or_panic("check_symlink_escape_nested_relative_escape");
            let link = subdir.join("nested/deep_escape");
            symlink("../../../..", &link).or_panic("check_symlink_escape_nested_relative_escape");
            let result = check_symlink_escape(&link, temp.path());
            assert!(
                result.is_some(),
                "expected nested relative escape detection"
            );
        }

        #[test]
        fn check_symlink_escape_detects_absolute_escape() {
            let temp = setup_project_root_with_subdir();
            let escape_target =
                tempfile::tempdir().or_panic("check_symlink_escape_detects_absolute_escape");
            let link = temp.path().join("link");
            symlink(escape_target.path(), &link)
                .or_panic("check_symlink_escape_detects_absolute_escape");
            let result = check_symlink_escape(&link, temp.path());
            assert!(result.is_some(), "expected escape detection");
            let escaped = result.or_panic("check_symlink_escape_detects_absolute_escape");
            assert!(
                !escaped.starts_with(temp.path()),
                "escaped path should be outside project root"
            );
        }

        #[test]
        fn check_symlink_escape_returns_none_when_canonical_inside_root() {
            let temp = tempfile::tempdir()
                .or_panic("check_symlink_escape_returns_none_when_canonical_inside_root");
            std::fs::create_dir_all(temp.path().join("a/b"))
                .or_panic("check_symlink_escape_returns_none_when_canonical_inside_root");
            let link = temp.path().join("a/b/link");
            symlink("..", &link)
                .or_panic("check_symlink_escape_returns_none_when_canonical_inside_root");
            let project_root = temp
                .path()
                .canonicalize()
                .or_panic("check_symlink_escape_returns_none_when_canonical_inside_root");
            assert!(check_symlink_escape(&link, &project_root).is_none());
        }

        #[test]
        fn project_root_identity_survives_broken_configured_symlink() {
            let temp = tempfile::tempdir()
                .or_panic("project_root_identity_survives_broken_configured_symlink");
            let target = temp.path().join("target");
            let link = temp.path().join("project-link");
            std::fs::create_dir(&target)
                .or_panic("project_root_identity_survives_broken_configured_symlink");
            symlink(&target, &link)
                .or_panic("project_root_identity_survives_broken_configured_symlink");
            let configured = link.display().to_string();

            let before = normalize_project_root(&configured, ResolutionMode::Deferred)
                .or_panic("project_root_identity_survives_broken_configured_symlink");
            std::fs::remove_dir(&target)
                .or_panic("project_root_identity_survives_broken_configured_symlink");
            let after = normalize_project_root(&configured, ResolutionMode::Deferred)
                .or_panic("project_root_identity_survives_broken_configured_symlink");

            assert_eq!(before, after);
            assert_eq!(after, link);
        }
    }
}
