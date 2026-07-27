//! Path expansion, normalization, and agent cwd resolution.

use std::env;
use std::path::{Path, PathBuf};

use super::super::error::ConfigError;
use super::super::model::ResolutionMode;

type Result<T> = std::result::Result<T, ConfigError>;

pub(crate) fn normalize_project_root(
    root: &str,
    resolution_mode: ResolutionMode,
) -> Result<PathBuf> {
    // Session names hash the project root. Resolving relative roots against
    // process CWD would make the same XDG config target different sessions
    // depending on where kira-mux is invoked — reject that footgun.
    require_stable_project_root(root)?;

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

/// Project roots must be absolute or `~/...` so resolution never depends on
/// the process current directory. Agent `cwd` may still be relative to root.
pub(super) fn require_stable_project_root(root: &str) -> Result<()> {
    if root == "~" || root.starts_with("~/") {
        return Ok(());
    }
    if Path::new(root).is_absolute() {
        return Ok(());
    }
    Err(ConfigError::RelativeProjectRoot(root.to_string()))
}

pub(super) fn resolve_agent_cwd(
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
pub(super) fn expand_path(value: &str, project_root: Option<&Path>) -> Result<PathBuf> {
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
pub(super) fn normalize_path(path: &Path) -> PathBuf {
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

pub(super) fn check_symlink_escape(path: &Path, project_root: &Path) -> Option<PathBuf> {
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

pub(super) fn home_dir() -> Result<PathBuf> {
    crate::paths::home_dir().map_err(|_source| ConfigError::HomeDirUnavailable)
}
