use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use super::error::ConfigError;
use super::model::{
    GlobalConfig, ProjectFile, ProjectFileRaw, ProjectIdOnly, ResolutionMode,
    default_session_prefix, default_shell, default_tmux_bin, default_window_name,
};
use super::resolve::{resolve_project, validate_global_config};
use crate::model::ResolvedProject;
use crate::paths::AppPaths;

type Result<T> = std::result::Result<T, ConfigError>;

/// A project file or profile that failed discovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProjectConfigFailure {
    /// Path of the project TOML that failed.
    pub path: PathBuf,
    /// Best-effort project id (from partial parse, or empty when unknown).
    pub project_id: Option<String>,
    /// Profile id when a specific profile failed; `None` for whole-file errors.
    pub profile_id: Option<String>,
    /// Display form of the underlying config error.
    pub error: String,
}

/// Outcome of scanning the XDG projects directory.
#[derive(Debug, Default)]
pub(crate) struct LoadedProjects {
    /// Successfully resolved project/profile combinations.
    pub projects: Vec<ResolvedProject>,
    /// Invalid files and profiles that could not be resolved.
    pub failures: Vec<ProjectConfigFailure>,
}

/// Load every project and profile discovered under the XDG config directory.
///
/// Invalid individual project files and profiles are collected in
/// [`LoadedProjects::failures`] (and still logged at warn) so callers such as
/// `list` can surface them without depending on log level.
///
/// # Errors
///
/// Returns an error when the global config or project directory cannot be
/// read, the global config is invalid, or multiple files define the same
/// project ID.
pub(crate) fn load_projects(
    paths: &AppPaths,
    resolution_mode: ResolutionMode,
) -> Result<LoadedProjects> {
    let global = load_global_config(&paths.global_config_path())?;
    let mut loaded = LoadedProjects::default();
    let mut ids = BTreeSet::new();

    for path in project_files(paths)? {
        let raw = match parse_project_raw(&path) {
            Ok(raw) => raw,
            Err(error) => {
                tracing::warn!(
                    path = %path.display(),
                    %error,
                    "skipping invalid project file"
                );
                loaded.failures.push(ProjectConfigFailure {
                    path: path.clone(),
                    project_id: best_effort_project_id(&path),
                    profile_id: None,
                    error: error.to_string(),
                });
                continue;
            }
        };

        if !ids.insert(raw.id.clone()) {
            return Err(ConfigError::DuplicateProjectId {
                id: raw.id,
                path: path.clone(),
            });
        }

        for pid in profile_ids(&raw) {
            let resolved_profile = resolve_profile(&raw, pid, &global, resolution_mode);
            match resolved_profile {
                Ok(project) => loaded.projects.push(project),
                Err(error) => {
                    tracing::warn!(
                        path = %path.display(),
                        profile_id = pid,
                        %error,
                        "skipping invalid profile"
                    );
                    loaded.failures.push(ProjectConfigFailure {
                        path: path.clone(),
                        project_id: Some(raw.id.clone()),
                        profile_id: Some(pid.to_string()),
                        error: error.to_string(),
                    });
                }
            }
        }
    }

    Ok(loaded)
}

/// Best-effort id from a broken file so list rows stay identifiable.
fn best_effort_project_id(path: &Path) -> Option<String> {
    project_id_from_file(path).ok().or_else(|| {
        path.file_stem()
            .and_then(|stem| stem.to_str())
            .map(str::to_string)
    })
}

/// Load one resolved project by ID and optional profile.
///
/// # Errors
///
/// Returns an error when configuration cannot be read or parsed, the project
/// or profile does not exist, or the selected project fails validation or
/// environment resolution.
pub(crate) fn load_project(
    paths: &AppPaths,
    project_id: &str,
    profile_id: Option<&str>,
    resolution_mode: ResolutionMode,
) -> Result<ResolvedProject> {
    let global = load_global_config(&paths.global_config_path())?;
    let raw = find_project_raw(paths, project_id)?;
    let effective_profile = resolve_profile_id(&raw, profile_id)?;
    resolve_profile(&raw, &effective_profile, &global, resolution_mode)
}

fn parse_project_raw(path: &Path) -> Result<ProjectFileRaw> {
    let source = fs::read_to_string(path).map_err(|source| ConfigError::FileRead {
        path: path.to_path_buf(),
        source,
    })?;
    let raw: ProjectFileRaw = toml::from_str(&source).map_err(|source| ConfigError::FileParse {
        path: path.to_path_buf(),
        source,
    })?;
    raw.validate_shape()?;

    Ok(raw)
}

fn resolve_profile(
    raw: &ProjectFileRaw,
    profile_id: &str,
    global: &GlobalConfig,
    resolution_mode: ResolutionMode,
) -> Result<ResolvedProject> {
    let project = select_profile(raw, profile_id)?;
    resolve_project(project, profile_id, global, resolution_mode)
}

fn select_profile(raw: &ProjectFileRaw, profile_id: &str) -> Result<ProjectFile> {
    // Only layout, ratio, and agents vary between the flat and profiled
    // shapes; everything else always comes from the top level.
    let (layout, main_pane_ratio, agents) = match &raw.profiles {
        Some(profiles) => {
            let profile = profiles
                .get(profile_id)
                .ok_or_else(|| ConfigError::UnknownProfile {
                    id: profile_id.to_string(),
                })?;
            (
                profile.layout,
                profile.main_pane_ratio,
                profile.agents.clone(),
            )
        }
        None => (
            raw.layout,
            raw.main_pane_ratio,
            raw.agents.clone().unwrap_or_default(),
        ),
    };

    Ok(ProjectFile {
        id: raw.id.clone(),
        name: raw.name.clone(),
        root: raw.root.clone(),
        layout,
        main_pane_ratio,
        window_name: raw.window_name.clone(),
        agents,
        groups: raw.groups.clone().unwrap_or_default(),
    })
}

fn profile_ids(raw: &ProjectFileRaw) -> Vec<&str> {
    match &raw.profiles {
        Some(profiles) => profiles.keys().map(String::as_str).collect(),
        None => vec!["default"],
    }
}

fn resolve_profile_id(raw: &ProjectFileRaw, requested: Option<&str>) -> Result<String> {
    if let Some(profiles) = &raw.profiles {
        let id = match requested {
            Some(id) => id.to_string(),
            None if profiles.len() == 1 => profiles
                .keys()
                .next()
                .cloned()
                .ok_or(ConfigError::EmptyProfiles)?,
            None => {
                return Err(ConfigError::ProfileRequired {
                    project_id: raw.id.clone(),
                    available: profiles.keys().cloned().collect(),
                });
            }
        };
        if !profiles.contains_key(&id) {
            return Err(ConfigError::UnknownProfile { id });
        }
        Ok(id)
    } else {
        let id = requested.unwrap_or("default");
        if id != "default" {
            return Err(ConfigError::UnknownProfile { id: id.to_string() });
        }
        Ok("default".to_string())
    }
}

fn load_global_config(path: &Path) -> Result<GlobalConfig> {
    if !path.exists() {
        return Ok(GlobalConfig::default());
    }

    let source = fs::read_to_string(path).map_err(|source| ConfigError::FileRead {
        path: path.to_path_buf(),
        source,
    })?;
    let mut config: GlobalConfig =
        toml::from_str(&source).map_err(|source| ConfigError::FileParse {
            path: path.to_path_buf(),
            source,
        })?;

    if config.session_prefix.is_empty() {
        config.session_prefix = default_session_prefix();
    }
    if config.window_name.is_empty() {
        config.window_name = default_window_name();
    }
    if config.default_shell.is_empty() {
        config.default_shell = default_shell();
    }
    if config.tmux_bin.is_empty() {
        config.tmux_bin = default_tmux_bin();
    }

    validate_global_config(&config)?;
    Ok(config)
}

fn project_files(paths: &AppPaths) -> Result<Vec<PathBuf>> {
    if !paths.projects_dir().exists() {
        return Ok(Vec::new());
    }

    let mut files = Vec::new();
    let dir = paths.projects_dir();
    for entry in fs::read_dir(&dir).map_err(|source| ConfigError::FileRead {
        path: dir.clone(),
        source,
    })? {
        let entry = entry.map_err(|source| ConfigError::FileRead {
            path: dir.clone(),
            source,
        })?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("toml") {
            files.push(path);
        }
    }

    files.sort();
    Ok(files)
}

/// Locate and fully parse the single project file matching `project_id`.
fn find_project_raw(paths: &AppPaths, project_id: &str) -> Result<ProjectFileRaw> {
    let mut matched = None;

    for path in project_files(paths)? {
        match project_id_from_file(&path) {
            Ok(id) if id == project_id => {
                if matched.replace(path.clone()).is_some() {
                    return Err(ConfigError::DuplicateProjectId {
                        id: project_id.to_string(),
                        path,
                    });
                }
            }
            Err(error) if path.file_stem().and_then(|stem| stem.to_str()) == Some(project_id) => {
                return Err(error);
            }
            Ok(_) | Err(_) => {}
        }
    }

    let path = matched.ok_or_else(|| ConfigError::UnknownProjectId(project_id.to_string()))?;
    parse_project_raw(&path)
}

fn project_id_from_file(path: &Path) -> Result<String> {
    let source = fs::read_to_string(path).map_err(|source| ConfigError::FileRead {
        path: path.to_path_buf(),
        source,
    })?;
    let project: ProjectIdOnly =
        toml::from_str(&source).map_err(|source| ConfigError::FileParse {
            path: path.to_path_buf(),
            source,
        })?;
    Ok(project.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{err, ok};

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

        let err = err(resolve_profile_id(&raw, None), "profile should be required");

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

        let profile = ok(resolve_profile_id(&raw, None), "resolve sole profile");

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

        let profile = ok(resolve_profile_id(&raw, None), "resolve flat profile");

        assert_eq!(profile, "default");
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
        assert_eq!(loaded.failures[0].profile_id.as_deref(), Some("default"));
        assert!(
            loaded.failures[0].error.contains("absolute")
                || loaded.failures[0].error.contains("relative"),
            "got: {}",
            loaded.failures[0].error
        );
    }
}
