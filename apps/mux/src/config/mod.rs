//! Configuration loading, validation, and resolution for kira-mux projects.

mod error;
mod fingerprint;
mod load;
mod model;
mod resolve;

pub use error::ConfigError;
pub use load::{load_project, load_projects};
pub use model::{AgentMode, AgentTemplate, EnvResolutionMode, GlobalConfig, Layout, RemainOnExit};
