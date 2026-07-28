//! Load global and per-project TOML configs from XDG paths.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::{env, fs};

use serde::Deserialize;
use serde::de::DeserializeOwned;

use super::error::ConfigError;
use super::model::{
    GlobalConfig, ProjectFile, ProjectFileRaw, ResolutionMode, default_session_prefix,
    default_shell, default_tmux_bin, default_window_name,
};
use super::resolve::{resolve_project, validate_global_config};
use crate::model::ResolvedProject;
use crate::paths::AppPaths;

mod target;

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

    for record in discover_projects(paths)? {
        let (path, project_id) = match record {
            DiscoveredProject::Broken { path, error } => {
                tracing::warn!(
                    path = %path.display(),
                    %error,
                    "skipping invalid project file"
                );
                loaded.failures.push(ProjectConfigFailure {
                    project_id: file_stem_id(&path),
                    path,
                    profile_id: None,
                    error: error.to_string(),
                });
                continue;
            }
            DiscoveredProject::Identified { path, id, root: _ } => (path, id),
        };

        let raw = match parse_project_raw(&path) {
            Ok(raw) => raw,
            Err(error) => {
                tracing::warn!(
                    path = %path.display(),
                    %error,
                    "skipping invalid project file"
                );
                loaded.failures.push(ProjectConfigFailure {
                    path,
                    project_id: Some(project_id),
                    profile_id: None,
                    error: error.to_string(),
                });
                continue;
            }
        };

        for pid in profile_ids(&raw) {
            let resolved_profile =
                select_profile(&raw, Some(pid)).and_then(|(profile_id, project)| {
                    resolve_project(project, &profile_id, &global, resolution_mode)
                });
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

/// Minimal identity parsed during discovery: enough for duplicate
/// detection, explicit-id lookup, and contextual root matching, tolerant of
/// everything else in the file.
#[derive(Debug, Deserialize)]
struct ProjectIdentity {
    id: String,
    /// Any TOML type is tolerated so a mistyped `root` cannot poison the
    /// identity (the full parse reports the real error); only a string
    /// participates in contextual matching.
    #[serde(default)]
    root: Option<toml::Value>,
}

/// One projects-dir file as seen by the shared discovery pass.
pub(super) enum DiscoveredProject {
    /// Identity parsed; full validation may still fail later.
    Identified {
        path: PathBuf,
        id: String,
        root: Option<String>,
    },
    /// The identity itself could not be parsed.
    Broken { path: PathBuf, error: ConfigError },
}

/// The single projects-dir scan shared by every entry point (`list`,
/// explicit id, contextual `.`): sorted file order, one identity parse per
/// file, and one duplicate policy — two files claiming the same id abort
/// discovery regardless of which project the caller wanted.
pub(super) fn discover_projects(paths: &AppPaths) -> Result<Vec<DiscoveredProject>> {
    let mut ids = BTreeSet::new();
    let mut records = Vec::new();

    for path in project_files(paths)? {
        match parse_project_file::<ProjectIdentity>(&path) {
            Ok(identity) => {
                if !ids.insert(identity.id.clone()) {
                    return Err(ConfigError::DuplicateProjectId {
                        id: identity.id,
                        path,
                    });
                }
                records.push(DiscoveredProject::Identified {
                    path,
                    id: identity.id,
                    root: identity
                        .root
                        .as_ref()
                        .and_then(toml::Value::as_str)
                        .map(str::to_string),
                });
            }
            Err(error) => records.push(DiscoveredProject::Broken { path, error }),
        }
    }

    Ok(records)
}

/// File-stem id for rows whose identity could not be parsed at all.
fn file_stem_id(path: &Path) -> Option<String> {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .map(str::to_string)
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
    resolve_loaded_project(&raw, profile_id, &global, resolution_mode)
}

/// Load the project whose registered root contains the process current
/// directory.
///
/// # Errors
///
/// Returns an error when the current directory cannot be resolved, no unique
/// registered root contains it, or the selected project/profile is invalid.
pub(crate) fn load_project_from_current_directory(
    paths: &AppPaths,
    profile_id: Option<&str>,
    resolution_mode: ResolutionMode,
) -> Result<ResolvedProject> {
    let directory = env::current_dir().map_err(|source| ConfigError::PathResolution {
        path: PathBuf::from("."),
        source,
    })?;
    load_project_from_directory(paths, &directory, profile_id, resolution_mode)
}

fn load_project_from_directory(
    paths: &AppPaths,
    directory: &Path,
    profile_id: Option<&str>,
    resolution_mode: ResolutionMode,
) -> Result<ResolvedProject> {
    let global = load_global_config(&paths.global_config_path())?;
    let path = target::find_project_path(paths, directory)?;
    let raw = parse_project_raw(&path)?;
    resolve_loaded_project(&raw, profile_id, &global, resolution_mode)
}

fn resolve_loaded_project(
    raw: &ProjectFileRaw,
    profile_id: Option<&str>,
    global: &GlobalConfig,
    resolution_mode: ResolutionMode,
) -> Result<ResolvedProject> {
    let (profile_id, project) = select_profile(raw, profile_id)?;
    resolve_project(project, &profile_id, global, resolution_mode)
}

fn parse_project_raw(path: &Path) -> Result<ProjectFileRaw> {
    let raw: ProjectFileRaw = parse_project_file(path)?;
    raw.validate_shape()?;

    Ok(raw)
}

fn parse_project_file<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let source = fs::read_to_string(path).map_err(|source| ConfigError::FileRead {
        path: path.to_path_buf(),
        source,
    })?;
    toml::from_str(&source)
        .map_err(|error| ConfigError::file_parse(path.to_path_buf(), &source, &error))
}

/// Profile id used by flat (profile-less) project files.
pub(crate) const DEFAULT_PROFILE_ID: &str = "default";

/// Validate `requested` against the file shape and return the selected
/// `(profile_id, ProjectFile)` pair — the only way to obtain a
/// per-profile view, so a flat file cannot silently accept a bogus
/// profile id and the two halves of selection cannot disagree.
fn select_profile(raw: &ProjectFileRaw, requested: Option<&str>) -> Result<(String, ProjectFile)> {
    // Only layout, ratio, and agents vary between the flat and profiled
    // shapes; everything else always comes from the top level.
    let (profile_id, layout, main_pane_ratio, agents) = if let Some(profiles) = &raw.profiles {
        {
            let profile_id = match requested {
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
            let profile = profiles
                .get(&profile_id)
                .ok_or_else(|| ConfigError::UnknownProfile {
                    id: profile_id.clone(),
                })?;
            (
                profile_id,
                profile.layout,
                profile.main_pane_ratio,
                profile.agents.clone(),
            )
        }
    } else {
        {
            let profile_id = requested.unwrap_or(DEFAULT_PROFILE_ID);
            if profile_id != DEFAULT_PROFILE_ID {
                return Err(ConfigError::UnknownProfile {
                    id: profile_id.to_string(),
                });
            }
            (
                profile_id.to_string(),
                raw.layout,
                raw.main_pane_ratio,
                raw.agents.clone().unwrap_or_default(),
            )
        }
    };

    Ok((
        profile_id,
        ProjectFile {
            id: raw.id.clone(),
            name: raw.name.clone(),
            root: raw.root.clone(),
            layout,
            main_pane_ratio,
            window_name: raw.window_name.clone(),
            agents,
            groups: raw.groups.clone().unwrap_or_default(),
        },
    ))
}

fn profile_ids(raw: &ProjectFileRaw) -> Vec<&str> {
    match &raw.profiles {
        Some(profiles) => profiles.keys().map(String::as_str).collect(),
        None => vec![DEFAULT_PROFILE_ID],
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
    let mut config: GlobalConfig = toml::from_str(&source)
        .map_err(|error| ConfigError::file_parse(path.to_path_buf(), &source, &error))?;

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
///
/// A declared id always wins; a broken file whose *filename* matches the
/// requested id surfaces its parse error only when no file declares the id,
/// so the operator sees why their lookup failed instead of "unknown id".
fn find_project_raw(paths: &AppPaths, project_id: &str) -> Result<ProjectFileRaw> {
    let mut matched = None;
    let mut broken_stem_match = None;

    for record in discover_projects(paths)? {
        match record {
            DiscoveredProject::Identified { path, id, root: _ } if id == project_id => {
                matched = Some(path);
            }
            DiscoveredProject::Broken { path, error }
                if path.file_stem().and_then(|stem| stem.to_str()) == Some(project_id) =>
            {
                broken_stem_match = Some(error);
            }
            DiscoveredProject::Identified { .. } | DiscoveredProject::Broken { .. } => {}
        }
    }

    match (matched, broken_stem_match) {
        (Some(path), _) => parse_project_raw(&path),
        (None, Some(error)) => Err(error),
        (None, None) => Err(ConfigError::UnknownProjectId(project_id.to_string())),
    }
}

#[cfg(test)]
mod tests;
