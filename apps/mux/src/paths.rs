//! XDG Base Directory path resolution for kira-mux config.

use std::env;
use std::path::PathBuf;

use anyhow::{Result, anyhow};

/// XDG-derived filesystem locations used by kira-mux.
#[derive(Debug, Clone)]
pub struct AppPaths {
    config_home: PathBuf,
}

impl AppPaths {
    /// Build an `AppPaths` from an explicit XDG config home.
    #[must_use]
    pub fn new(config_home: PathBuf) -> Self {
        Self { config_home }
    }

    /// Resolve XDG config home from the current environment.
    ///
    /// # Errors
    ///
    /// Returns an error when `HOME` is unavailable and an XDG fallback path
    /// is required.
    pub fn from_env() -> Result<Self> {
        Ok(Self::new(xdg_home("XDG_CONFIG_HOME", ".config")?))
    }

    /// Return the kira-mux config directory.
    #[must_use]
    pub fn config_dir(&self) -> PathBuf {
        self.config_home.join("kira-mux")
    }

    /// Return the directory that stores per-project config files.
    #[must_use]
    pub fn projects_dir(&self) -> PathBuf {
        self.config_dir().join("projects")
    }

    /// Return the global config file path.
    #[must_use]
    pub fn global_config_path(&self) -> PathBuf {
        self.config_dir().join("config.toml")
    }

    /// Return the path used by `init` for the example project file.
    #[must_use]
    pub fn example_project_path(&self) -> PathBuf {
        self.projects_dir().join("example.toml")
    }
}

fn xdg_home(var_name: &str, fallback_suffix: &str) -> Result<PathBuf> {
    match env::var_os(var_name) {
        Some(value) if !value.is_empty() => {
            let path = PathBuf::from(value);
            if path.is_absolute() {
                Ok(path)
            } else {
                Ok(home_dir()?.join(fallback_suffix))
            }
        }
        _ => Ok(home_dir()?.join(fallback_suffix)),
    }
}

pub(crate) fn home_dir() -> Result<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or_else(|| anyhow!("HOME is not set"))
}
