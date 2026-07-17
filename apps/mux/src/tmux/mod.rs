//! Tmux subprocess client, output parsing, and session metadata.

mod adapter;
mod client;
mod env_file;
mod error;
pub(crate) mod metadata;
mod parse;
mod paste;

#[cfg(test)]
pub(crate) use adapter::WorkspacePaneSnapshot;
pub(crate) use adapter::{PaneInfo, TmuxAdapter, WorkspaceSnapshot, WorkspaceWindowSnapshot};
pub(crate) use client::TmuxClient;
pub(crate) use error::TmuxError;
pub(crate) use paste::paste_then_submit_text;
