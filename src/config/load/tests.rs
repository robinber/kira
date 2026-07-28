use super::*;
use crate::test_support::{err, ok};

fn contextual_fixture() -> (tempfile::TempDir, tempfile::TempDir, AppPaths) {
    let config_home = ok(tempfile::tempdir(), "create config home");
    let project_root = ok(tempfile::tempdir(), "create project root");
    let paths = AppPaths::new(config_home.path().to_path_buf());
    ok(
        fs::create_dir_all(paths.projects_dir()),
        "create projects directory",
    );
    (config_home, project_root, paths)
}

fn write_contextual_project(paths: &AppPaths, file_name: &str, contents: &str) {
    ok(
        fs::write(paths.projects_dir().join(file_name), contents),
        "write contextual project",
    );
}

#[test]
fn multi_profile_project_requires_explicit_profile_even_when_default_exists() {
    let parsed: std::result::Result<ProjectFileRaw, _> = toml::from_str(
        r#"
id = "demo"
root = "/tmp/demo"

[profiles.default]
[[profiles.default.agents]]
id = "assistant"

[profiles.work]
[[profiles.work.agents]]
id = "worker"
"#,
    );
    let raw = ok(parsed, "parse project");

    let err = err(select_profile(&raw, None), "profile should be required");

    match err {
        ConfigError::ProfileRequired {
            project_id,
            available,
        } => {
            assert_eq!(project_id, "demo");
            assert_eq!(available, vec!["default".to_string(), "work".to_string()]);
        }
        other => panic!("expected ProfileRequired, got {other:?}"),
    }
}

#[test]
fn single_profile_project_auto_selects_sole_profile() {
    let parsed: std::result::Result<ProjectFileRaw, _> = toml::from_str(
        r#"
id = "demo"
root = "/tmp/demo"

[profiles.work]
[[profiles.work.agents]]
id = "worker"
"#,
    );
    let raw = ok(parsed, "parse project");

    let (profile, _project) = ok(select_profile(&raw, None), "resolve sole profile");

    assert_eq!(profile, "work");
}

#[test]
fn flat_project_uses_implicit_default_profile() {
    let parsed: std::result::Result<ProjectFileRaw, _> = toml::from_str(
        r#"
id = "demo"
root = "/tmp/demo"

[[agents]]
id = "assistant"
"#,
    );
    let raw = ok(parsed, "parse project");

    let (profile, _project) = ok(select_profile(&raw, None), "resolve flat profile");

    assert_eq!(profile, DEFAULT_PROFILE_ID);

    let err = err(
        select_profile(&raw, Some("nope")),
        "a flat file must reject non-default profile ids",
    );
    assert!(matches!(err, ConfigError::UnknownProfile { ref id } if id == "nope"));
}

#[test]
fn contextual_and_explicit_targets_resolve_identical_project_identity() {
    let (_config_home, project_root, paths) = contextual_fixture();
    write_contextual_project(
        &paths,
        "demo.toml",
        &format!(
            "id = \"demo\"\nroot = {:?}\n\n[[agents]]\nid = \"assistant\"\ncommand = \"echo\"\n",
            project_root.path().display().to_string()
        ),
    );

    let explicit = ok(
        load_project(&paths, "demo", None, ResolutionMode::Deferred),
        "load explicit project",
    );
    let contextual = ok(
        load_project_from_directory(&paths, project_root.path(), None, ResolutionMode::Deferred),
        "load contextual project",
    );

    assert_eq!(contextual, explicit);
}

#[test]
fn contextual_target_applies_profile_selection_after_project_selection() {
    let (_config_home, project_root, paths) = contextual_fixture();
    write_contextual_project(
        &paths,
        "demo.toml",
        &format!(
            r#"id = "demo"
root = {:?}

[profiles.default]
[[profiles.default.agents]]
id = "assistant"
command = "echo"

[profiles.work]
[[profiles.work.agents]]
id = "worker"
command = "echo"
"#,
            project_root.path().display().to_string()
        ),
    );

    let missing_profile = err(
        load_project_from_directory(&paths, project_root.path(), None, ResolutionMode::Deferred),
        "contextual multi-profile project should require a profile",
    );
    assert!(matches!(
        missing_profile,
        ConfigError::ProfileRequired { project_id, .. } if project_id == "demo"
    ));

    let selected = ok(
        load_project_from_directory(
            &paths,
            project_root.path(),
            Some("work"),
            ResolutionMode::Deferred,
        ),
        "load contextual work profile",
    );
    assert_eq!(selected.id, "demo");
    assert_eq!(selected.profile_id, "work");
    assert_eq!(selected.agents[0].id, "worker");
}

#[test]
fn contextual_target_surfaces_selected_projects_full_parse_error() {
    let (_config_home, project_root, paths) = contextual_fixture();
    write_contextual_project(
        &paths,
        "demo.toml",
        &format!(
            "id = \"demo\"\nroot = {:?}\nunknown = true\n",
            project_root.path().display().to_string()
        ),
    );

    let error = err(
        load_project_from_directory(&paths, project_root.path(), None, ResolutionMode::Deferred),
        "selected invalid project must fail full parsing",
    );

    assert!(matches!(error, ConfigError::FileParse { .. }));
}

#[test]
fn duplicate_id_detection_is_uniform_across_entry_points() {
    // File A is fully valid; file B declares the same id but fails full
    // parse (unknown field). The old per-entry checks disagreed: `list`
    // only counted fully-parsed files and missed this duplicate.
    let config_home = ok(tempfile::tempdir(), "config home");
    let projects = config_home.path().join("kira-mux/projects");
    ok(fs::create_dir_all(&projects), "projects dir");

    ok(
        fs::write(
            projects.join("a.toml"),
            r#"
id = "dup"
root = "/tmp/dup-a"

[[agents]]
id = "alpha"
command = "echo"
"#,
        ),
        "write a",
    );
    ok(
        fs::write(
            projects.join("b.toml"),
            r#"
id = "dup"
root = "/tmp/dup-b"
nope = true

[[agents]]
id = "alpha"
command = "echo"
"#,
        ),
        "write b",
    );

    let paths = AppPaths::new(config_home.path().to_path_buf());

    let list_err = err(
        load_projects(&paths, ResolutionMode::Deferred),
        "list entry: expected duplicate error",
    );
    assert!(
        matches!(list_err, ConfigError::DuplicateProjectId { ref id, .. } if id == "dup"),
        "got: {list_err}"
    );

    let explicit_err = err(
        find_project_raw(&paths, "dup"),
        "explicit entry: expected duplicate error",
    );
    assert!(
        matches!(explicit_err, ConfigError::DuplicateProjectId { ref id, .. } if id == "dup"),
        "got: {explicit_err}"
    );

    let unrelated_err = err(
        find_project_raw(&paths, "unrelated"),
        "unrelated explicit lookup: duplicates abort discovery globally",
    );
    assert!(
        matches!(unrelated_err, ConfigError::DuplicateProjectId { ref id, .. } if id == "dup"),
        "got: {unrelated_err}"
    );

    let contextual_err = err(
        target::find_project_path(&paths, config_home.path()),
        "contextual entry: expected duplicate error",
    );
    assert!(
        matches!(contextual_err, ConfigError::DuplicateProjectId { ref id, .. } if id == "dup"),
        "got: {contextual_err}"
    );
}

#[test]
fn mistyped_root_does_not_poison_the_declared_identity() {
    // `root = 42` must not knock the file out of discovery: its id
    // still counts for duplicate detection, and explicit lookup
    // surfaces the real parse error instead of UnknownProjectId.
    let config_home = ok(tempfile::tempdir(), "config home");
    let projects = config_home.path().join("kira-mux/projects");
    ok(fs::create_dir_all(&projects), "projects dir");

    ok(
        fs::write(
            projects.join("bad-root.toml"),
            r#"
id = "wanted"
root = 42

[[agents]]
id = "alpha"
command = "echo"
"#,
        ),
        "write bad root",
    );

    let paths = AppPaths::new(config_home.path().to_path_buf());
    let error = err(
        find_project_raw(&paths, "wanted"),
        "expected the full-parse error",
    );
    assert!(
        !matches!(error, ConfigError::UnknownProjectId(_)),
        "declared id must stay discoverable, got: {error}"
    );
}

#[test]
fn explicit_lookup_prefers_declared_id_over_broken_stem_match() {
    // A broken file named `wanted.toml` must not shadow a healthy file
    // that properly declares `id = "wanted"`.
    let config_home = ok(tempfile::tempdir(), "config home");
    let projects = config_home.path().join("kira-mux/projects");
    ok(fs::create_dir_all(&projects), "projects dir");

    ok(
        fs::write(projects.join("wanted.toml"), "id = [\nnot = toml\n"),
        "write broken",
    );
    ok(
        fs::write(
            projects.join("real.toml"),
            r#"
id = "wanted"
root = "/tmp/wanted"

[[agents]]
id = "alpha"
command = "echo"
"#,
        ),
        "write real",
    );

    let paths = AppPaths::new(config_home.path().to_path_buf());
    let raw = ok(find_project_raw(&paths, "wanted"), "declared id must win");
    assert_eq!(raw.id, "wanted");
}

#[test]
fn load_projects_collects_malformed_and_unknown_field_files() {
    let config_home = match tempfile::tempdir() {
        Ok(dir) => dir,
        Err(error) => panic!("config home: {error}"),
    };
    let projects = config_home.path().join("kira-mux/projects");
    if let Err(error) = fs::create_dir_all(&projects) {
        panic!("projects dir: {error}");
    }

    if let Err(error) = fs::write(
        projects.join("good.toml"),
        r#"
id = "good"
name = "Good"
root = "/tmp/good"

[[agents]]
id = "alpha"
command = "echo"
"#,
    ) {
        panic!("write good: {error}");
    }

    if let Err(error) = fs::write(projects.join("broken-toml.toml"), "id = [\nnot = toml\n") {
        panic!("write broken toml: {error}");
    }

    if let Err(error) = fs::write(
        projects.join("unknown-field.toml"),
        r#"
id = "mystery"
root = "/tmp/mystery"
nope = true

[[agents]]
id = "alpha"
command = "echo"
"#,
    ) {
        panic!("write unknown field: {error}");
    }

    let paths = AppPaths::new(config_home.path().to_path_buf());
    let loaded = match load_projects(&paths, ResolutionMode::Deferred) {
        Ok(loaded) => loaded,
        Err(error) => panic!("load: {error}"),
    };

    assert_eq!(loaded.projects.len(), 1, "only good project resolves");
    assert_eq!(loaded.projects[0].id, "good");
    assert_eq!(
        loaded.failures.len(),
        2,
        "malformed + unknown field must surface: {:?}",
        loaded.failures
    );
    assert!(
        loaded
            .failures
            .iter()
            .any(|f| f.path.ends_with("broken-toml.toml")),
        "got: {:?}",
        loaded.failures
    );
    assert!(
        loaded
            .failures
            .iter()
            .any(|f| f.path.ends_with("unknown-field.toml")
                && f.project_id.as_deref() == Some("mystery")),
        "unknown-field should still expose best-effort id: {:?}",
        loaded.failures
    );
}

#[test]
fn load_projects_malformed_toml_diagnostics_omit_source_secrets() {
    const SENTINEL: &str = "super-secret-value-do-not-leak";

    let config_home = match tempfile::tempdir() {
        Ok(dir) => dir,
        Err(error) => panic!("config home: {error}"),
    };
    let projects = config_home.path().join("kira-mux/projects");
    if let Err(error) = fs::create_dir_all(&projects) {
        panic!("projects dir: {error}");
    }

    let body = format!("env = {{ TOKEN = \"{SENTINEL}\n");
    if let Err(error) = fs::write(projects.join("leaky.toml"), &body) {
        panic!("write leaky: {error}");
    }

    let paths = AppPaths::new(config_home.path().to_path_buf());
    let loaded = match load_projects(&paths, ResolutionMode::Deferred) {
        Ok(loaded) => loaded,
        Err(error) => panic!("load: {error}"),
    };

    assert_eq!(loaded.projects.len(), 0);
    assert_eq!(loaded.failures.len(), 1, "got: {:?}", loaded.failures);
    let failure = &loaded.failures[0];
    assert!(
        failure.path.ends_with("leaky.toml"),
        "got path: {}",
        failure.path.display()
    );
    assert!(
        !failure.error.contains(SENTINEL),
        "failure error must not include secret: {}",
        failure.error
    );
    assert!(
        failure.error.contains("failed to parse")
            || failure.error.contains("line")
            || failure.error.contains("TOML")
            || failure.error.contains("toml")
            || !failure.error.is_empty(),
        "diagnostics must remain non-empty and actionable: {}",
        failure.error
    );
}

#[test]
fn load_projects_collects_invalid_profile_resolution() {
    let config_home = match tempfile::tempdir() {
        Ok(dir) => dir,
        Err(error) => panic!("config home: {error}"),
    };
    let projects = config_home.path().join("kira-mux/projects");
    if let Err(error) = fs::create_dir_all(&projects) {
        panic!("projects dir: {error}");
    }

    // Relative root is rejected at resolve time (#15).
    if let Err(error) = fs::write(
        projects.join("bad-root.toml"),
        r#"
id = "bad-root"
name = "Bad Root"
root = "relative/path"

[[agents]]
id = "alpha"
command = "echo"
"#,
    ) {
        panic!("write bad root: {error}");
    }

    let paths = AppPaths::new(config_home.path().to_path_buf());
    let loaded = match load_projects(&paths, ResolutionMode::Deferred) {
        Ok(loaded) => loaded,
        Err(error) => panic!("load: {error}"),
    };

    assert!(loaded.projects.is_empty());
    assert_eq!(loaded.failures.len(), 1);
    assert_eq!(loaded.failures[0].project_id.as_deref(), Some("bad-root"));
    assert_eq!(
        loaded.failures[0].profile_id.as_deref(),
        Some(DEFAULT_PROFILE_ID)
    );
    assert!(
        loaded.failures[0].error.contains("absolute")
            || loaded.failures[0].error.contains("relative"),
        "got: {}",
        loaded.failures[0].error
    );
}
