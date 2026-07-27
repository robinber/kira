//! Resolve the contextual project target `.` from the process CWD.
//!
//! Matches registered project roots by physical path; deepest root wins.

use std::path::{Path, PathBuf};

use super::{DiscoveredProject, discover_projects};
use crate::config::error::ConfigError;
use crate::config::model::ResolutionMode;
use crate::config::resolve::normalize_project_root;
use crate::paths::AppPaths;

type Result<T> = std::result::Result<T, ConfigError>;

struct ProjectCandidate {
    id: String,
    path: PathBuf,
    depth: usize,
}

pub(super) fn find_project_path(paths: &AppPaths, directory: &Path) -> Result<PathBuf> {
    let canonical_directory =
        directory
            .canonicalize()
            .map_err(|source| ConfigError::PathResolution {
                path: directory.to_path_buf(),
                source,
            })?;
    let mut candidates = Vec::new();

    for record in discover_projects(paths)? {
        // Broken files and root-less identities cannot participate in
        // contextual matching; list / explicit-id lookups surface their
        // errors instead.
        let (path, id, root) = match record {
            DiscoveredProject::Identified {
                path,
                id,
                root: Some(root),
            } => (path, id, root),
            DiscoveredProject::Identified {
                path,
                id,
                root: None,
            } => {
                tracing::debug!(
                    path = %path.display(),
                    project_id = id.as_str(),
                    "skipping project config without a root for contextual matching"
                );
                continue;
            }
            DiscoveredProject::Broken { path, error } => {
                tracing::debug!(
                    path = %path.display(),
                    %error,
                    "skipping project config that cannot be located contextually"
                );
                continue;
            }
        };

        let root = match normalize_project_root(&root, ResolutionMode::Deferred) {
            Ok(root) => root,
            Err(error) => {
                tracing::debug!(
                    project_id = id.as_str(),
                    %error,
                    "skipping project with an invalid contextual root"
                );
                continue;
            }
        };
        let canonical_root = match root.canonicalize() {
            Ok(root) => root,
            Err(error) => {
                tracing::debug!(
                    project_id = id.as_str(),
                    root = %root.display(),
                    %error,
                    "skipping project whose contextual root is unavailable"
                );
                continue;
            }
        };

        if canonical_directory.starts_with(&canonical_root) {
            candidates.push(ProjectCandidate {
                id,
                path,
                depth: canonical_root.components().count(),
            });
        }
    }

    let Some(deepest) = candidates.iter().map(|candidate| candidate.depth).max() else {
        return Err(ConfigError::NoProjectForDirectory {
            directory: canonical_directory,
            projects_dir: paths.projects_dir(),
        });
    };
    let mut deepest_candidates = candidates
        .into_iter()
        .filter(|candidate| candidate.depth == deepest)
        .collect::<Vec<_>>();

    if deepest_candidates.len() > 1 {
        let mut project_ids = deepest_candidates
            .iter()
            .map(|candidate| candidate.id.clone())
            .collect::<Vec<_>>();
        project_ids.sort();
        return Err(ConfigError::AmbiguousProjectForDirectory {
            directory: canonical_directory,
            project_ids,
        });
    }

    deepest_candidates.pop().map_or_else(
        || {
            Err(ConfigError::NoProjectForDirectory {
                directory: canonical_directory,
                projects_dir: paths.projects_dir(),
            })
        },
        |candidate| Ok(candidate.path),
    )
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::test_support::TestResultExt;

    struct Fixture {
        _config_home: tempfile::TempDir,
        roots: tempfile::TempDir,
        paths: AppPaths,
    }

    impl Fixture {
        fn new() -> Self {
            let config_home = tempfile::tempdir().or_panic("new");
            let roots = tempfile::tempdir().or_panic("new");
            let paths = AppPaths::new(config_home.path().to_path_buf());
            fs::create_dir_all(paths.projects_dir()).or_panic("new");
            Self {
                _config_home: config_home,
                roots,
                paths,
            }
        }

        fn root(&self, relative: &str) -> PathBuf {
            let path = self.roots.path().join(relative);
            fs::create_dir_all(&path).or_panic("root");
            path
        }

        fn project_file(&self, name: &str, contents: &str) -> PathBuf {
            let path = self.paths.projects_dir().join(name);
            fs::write(&path, contents).or_panic("project_file");
            path
        }

        fn register(&self, file_name: &str, project_id: &str, root: &Path) -> PathBuf {
            self.project_file(
                file_name,
                &format!(
                    "id = {project_id:?}\nroot = {:?}\n",
                    root.display().to_string()
                ),
            )
        }
    }

    #[test]
    fn selects_project_containing_nested_directory() {
        let fixture = Fixture::new();
        let root = fixture.root("project");
        let nested = root.join("src/deep");
        fs::create_dir_all(&nested).or_panic("selects_project_containing_nested_directory");
        let expected = fixture.register("project.toml", "project", &root);

        let selected = find_project_path(&fixture.paths, &nested)
            .or_panic("selects_project_containing_nested_directory");

        assert_eq!(selected, expected);
    }

    #[test]
    fn deepest_registered_root_wins() {
        let fixture = Fixture::new();
        let parent = fixture.root("workspace");
        let nested_root = parent.join("nested");
        fs::create_dir_all(&nested_root).or_panic("deepest_registered_root_wins");
        fixture.register("parent.toml", "parent", &parent);
        let expected = fixture.register("nested.toml", "nested", &nested_root);

        let selected = find_project_path(&fixture.paths, &nested_root)
            .or_panic("deepest_registered_root_wins");

        assert_eq!(selected, expected);
    }

    #[test]
    fn same_effective_root_is_ambiguous() {
        let fixture = Fixture::new();
        let root = fixture.root("project");
        fixture.register("alpha.toml", "alpha", &root);
        fixture.register("beta.toml", "beta", &root);

        let error = find_project_path(&fixture.paths, &root)
            .err_or_panic("same_effective_root_is_ambiguous: expected Err");

        assert!(matches!(
            error,
            ConfigError::AmbiguousProjectForDirectory {
                project_ids,
                ..
            } if project_ids == ["alpha", "beta"]
        ));
    }

    #[test]
    fn reports_when_no_registered_root_contains_directory() {
        let fixture = Fixture::new();
        let registered = fixture.root("registered");
        let outside = fixture.root("outside");
        fixture.register("registered.toml", "registered", &registered);

        let error = find_project_path(&fixture.paths, &outside)
            .err_or_panic("reports_when_no_registered_root_contains_directory: expected Err");

        assert!(matches!(
            error,
            ConfigError::NoProjectForDirectory { directory, .. }
                if directory == outside.canonicalize().or_panic("reports_when_no_registered_root_contains_directory")
        ));
    }

    #[test]
    fn unrelated_malformed_config_does_not_block_valid_match() {
        let fixture = Fixture::new();
        let root = fixture.root("project");
        fixture.project_file("broken.toml", "not = [valid");
        let expected = fixture.register("project.toml", "project", &root);

        let selected = find_project_path(&fixture.paths, &root)
            .or_panic("unrelated_malformed_config_does_not_block_valid_match");

        assert_eq!(selected, expected);
    }

    #[test]
    fn duplicate_project_ids_remain_an_error() {
        let fixture = Fixture::new();
        let first = fixture.root("first");
        let second = fixture.root("second");
        fixture.register("first.toml", "duplicate", &first);
        fixture.register("second.toml", "duplicate", &second);

        let error = find_project_path(&fixture.paths, &first)
            .err_or_panic("duplicate_project_ids_remain_an_error: expected Err");

        assert!(matches!(
            error,
            ConfigError::DuplicateProjectId { id, .. } if id == "duplicate"
        ));
    }

    #[cfg(unix)]
    #[test]
    fn configured_symlink_root_matches_physical_directory() {
        use std::os::unix::fs::symlink;

        let fixture = Fixture::new();
        let physical_root = fixture.root("physical");
        let nested = physical_root.join("src");
        fs::create_dir_all(&nested).or_panic("configured_symlink_root_matches_physical_directory");
        let symlink_root = fixture.roots.path().join("linked");
        symlink(&physical_root, &symlink_root)
            .or_panic("configured_symlink_root_matches_physical_directory");
        let expected = fixture.register("project.toml", "project", &symlink_root);

        let selected = find_project_path(&fixture.paths, &nested)
            .or_panic("configured_symlink_root_matches_physical_directory");

        assert_eq!(selected, expected);
        let selected_contents = fs::read_to_string(selected)
            .or_panic("configured_symlink_root_matches_physical_directory");
        assert!(
            selected_contents.contains(&symlink_root.display().to_string()),
            "selection must preserve the configured symlink root"
        );
    }
}
