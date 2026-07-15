//! Tmux subprocess client, output parsing, and session metadata.

mod adapter;
mod client;
mod env_file;
mod error;
pub(crate) mod metadata;
mod parse;
mod paste;

pub(crate) use adapter::{PaneInfo, TmuxAdapter};
pub(crate) use client::{PaneSummary, TmuxClient, WorkspaceSummarySnapshot};
pub(crate) use error::TmuxError;
pub(crate) use paste::paste_then_submit_text;
