//! Configuration loading, validation, and resolution for kira-mux projects.

mod error;
mod fingerprint;
mod load;
mod model;
mod resolve;

pub use error::ConfigError;
pub(crate) use load::{
    ProjectConfigFailure, load_project, load_project_from_current_directory, load_projects,
};
pub(crate) use model::{AgentMode, Layout, RemainOnExit, ResolutionMode};
