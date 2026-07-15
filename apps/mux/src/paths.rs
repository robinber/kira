//! XDG Base Directory path resolution for kira-mux config and state.

use std::env;
use std::path::PathBuf;

use anyhow::{Result, anyhow};

/// XDG-derived filesystem locations used by kira-mux.
#[derive(Debug, Clone)]
#[expect(
    clippy::struct_field_names,
    reason = "the field names follow the standard XDG config, state, and data home terminology"
)]
pub struct AppPaths {
    config_home: PathBuf,
    state_home: PathBuf,
    data_home: PathBuf,
}

impl AppPaths {
    /// Build an `AppPaths` from explicit XDG base directories.
    #[must_use]
    pub fn new(config_home: PathBuf, state_home: PathBuf, data_home: PathBuf) -> Self {
        Self {
            config_home,
            state_home,
            data_home,
        }
    }

    /// Resolve XDG base directories from the current environment.
    ///
    /// # Errors
    ///
    /// Returns an error when `HOME` is unavailable and an XDG fallback path
    /// is required.
    pub fn from_env() -> Result<Self> {
        Ok(Self::new(
            xdg_home("XDG_CONFIG_HOME", ".config")?,
            xdg_home("XDG_STATE_HOME", ".local/state")?,
            xdg_home("XDG_DATA_HOME", ".local/share")?,
        ))
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

    /// Return the kira-mux state directory.
    #[must_use]
    pub fn state_dir(&self) -> PathBuf {
        self.state_home.join("kira-mux")
    }

    /// Return the kira-mux data directory.
    #[must_use]
    pub fn data_dir(&self) -> PathBuf {
        self.data_home.join("kira-mux")
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
