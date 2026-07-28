//! Tmux workspace lifecycle for project-scoped agent sessions.
//!
//! Create or repair sessions, attach, restart panes, kill, and summarize
//! live state for `status` / `list`.

mod identity;
mod launch;
mod lifecycle;
mod status;

pub(crate) use identity::{session_name, window_target};
pub(crate) use lifecycle::{KillOutcome, StartOutcome, attach, kill, restart, start};
pub(crate) use status::{load_project_summaries, project_status};
