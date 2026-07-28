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

    let resolved = normalize_path(&expand_path(value, Some(project_root))?);

    // Absolute and home-anchored cwds are trusted as configured; only
    // root-relative values carry the containment contract.
    if !is_absolute_or_home(value) {
        ensure_cwd_inside_root(agent_id, &resolved, project_root)?;
    }

    if !resolved.exists() {
        return if resolution_mode == ResolutionMode::Deferred {
            Ok(resolved)
        } else {
            Err(ConfigError::AgentCwdNotFound {
                agent_id: agent_id.to_string(),
                path: resolved,
            })
        };
    }
    if !resolved.is_dir() {
        return Err(ConfigError::AgentCwdNotDirectory {
            agent_id: agent_id.to_string(),
            path: resolved,
        });
    }

    Ok(resolved)
}

fn is_absolute_or_home(value: &str) -> bool {
    Path::new(value).is_absolute() || value.starts_with("~/") || value == "~"
}

/// Reject a root-relative agent cwd that escapes the project root.
///
/// Decision order — each mechanism catches escapes the previous cannot:
/// 1. Lexical: the normalized path must sit under the configured
///    (non-canonical) root. Catches plain `..` escapes with no disk access, so
///    it also protects paths that do not exist yet.
/// 2. Symlink probe: when the path itself is a symlink, its target — canonical,
///    or `read_link` fallback for broken links — must stay under the canonical
///    root. Catches links pointing outside, even dangling ones.
/// 3. Canonical containment: when the path exists, its canonical form must stay
///    under the canonical root. Catches escapes through symlinked intermediate
///    directories that 1 and 2 cannot see. Skipped for paths absent from disk:
///    deferred resolution tolerates them, and runtime resolution rejects them
///    as `AgentCwdNotFound` right after this check.
fn ensure_cwd_inside_root(agent_id: &str, resolved: &Path, project_root: &Path) -> Result<()> {
    if !resolved.starts_with(project_root) {
        return Err(ConfigError::AgentCwdEscapesRoot {
            agent_id: agent_id.to_string(),
            path: resolved.to_path_buf(),
        });
    }

    if resolved
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
        && let Some(path) = check_symlink_escape(resolved, project_root)
    {
        return Err(ConfigError::AgentCwdEscapesRoot {
            agent_id: agent_id.to_string(),
            path,
        });
    }

    if resolved.exists() {
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
                path: resolved.to_path_buf(),
                source,
            })?;
        if !canonical.starts_with(&canonical_root) {
            return Err(ConfigError::AgentCwdEscapesRoot {
                agent_id: agent_id.to_string(),
                path: canonical,
            });
        }
    }

    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestOptionExt, TestResultExt};

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
    fn agent_cwd_lexical_escape_is_rejected_without_disk_access() {
        // Mechanism 1: `..` escapes must fail even in deferred mode where
        // nothing exists on disk yet.
        let base = tempfile::tempdir().or_panic("agent_cwd_lexical_escape");
        let root = base.path().join("missing-root");

        let error = resolve_agent_cwd("alpha", Some("../outside"), &root, ResolutionMode::Deferred)
            .err_or_panic("agent_cwd_lexical_escape: expected Err");

        assert!(matches!(error, ConfigError::AgentCwdEscapesRoot { .. }));
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
        fn agent_cwd_broken_symlink_escape_is_rejected_before_existence_checks() {
            // Mechanism 2: a dangling link (`exists()` false) must be caught
            // as an escape, not tolerated by deferred missing-path handling.
            let temp = setup_project_root_with_subdir();
            symlink(
                "/nonexistent/escape/target",
                temp.path().join("broken_link"),
            )
            .or_panic("agent_cwd_broken_symlink_escape");

            let error = resolve_agent_cwd(
                "alpha",
                Some("broken_link"),
                temp.path(),
                ResolutionMode::Deferred,
            )
            .err_or_panic("agent_cwd_broken_symlink_escape: expected Err");

            assert!(matches!(error, ConfigError::AgentCwdEscapesRoot { .. }));
        }

        #[test]
        fn agent_cwd_intermediate_symlink_escape_is_rejected() {
            // Mechanism 3: the cwd itself is a plain directory, but an
            // intermediate component is a link out of the root — only
            // canonical containment can see it.
            let temp = setup_project_root_with_subdir();
            let outside = tempfile::tempdir().or_panic("agent_cwd_intermediate_symlink_escape");
            std::fs::create_dir(outside.path().join("sub"))
                .or_panic("agent_cwd_intermediate_symlink_escape");
            symlink(outside.path(), temp.path().join("link"))
                .or_panic("agent_cwd_intermediate_symlink_escape");

            let error = resolve_agent_cwd(
                "alpha",
                Some("link/sub"),
                temp.path(),
                ResolutionMode::Runtime,
            )
            .err_or_panic("agent_cwd_intermediate_symlink_escape: expected Err");

            assert!(matches!(error, ConfigError::AgentCwdEscapesRoot { .. }));
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
