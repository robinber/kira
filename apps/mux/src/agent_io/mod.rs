//! Prompt delivery and pane capture for agent workspaces.

mod capture;
mod policy;
mod resolve;
mod send;
mod wait;

pub(crate) use capture::capture_output;
pub(crate) use send::{
    DEFAULT_WAIT_CAPTURE_LINES, DeliveredPrompt, send_prompt, send_prompt_for_wait,
};
pub(crate) use wait::{WaitOptions, wait_on_pane};
