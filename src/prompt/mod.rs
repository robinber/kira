//! Prompt template rendering for agent panes.
//!
//! Builds context from project + topology, then substitutes `{{variables}}`.

mod context;
mod render;

pub(crate) use context::{PromptContext, extract_agent_state};
pub(crate) use render::{lint_template, render};
