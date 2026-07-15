//! Prompt delivery and pane capture for agent workspaces.

mod capture;
mod policy;
mod resolve;
mod send;

pub(crate) use capture::capture_output;
pub(crate) use send::send_prompt;
