//! Tmux workspace lifecycle for project-scoped agent sessions.

mod identity;
mod launch;
mod lifecycle;
mod status;

pub(crate) use identity::{session_name, window_target};
pub(crate) use lifecycle::{StartOutcome, attach, kill, restart, start};
pub(crate) use status::{load_project_summaries, project_status};
