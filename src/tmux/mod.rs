//! Tmux subprocess client, output parsing, and session metadata.
//!
//! Adapter trait + real client, paste helpers, env-file injection, and the
//! `@kira_mux_*` option namespace.

mod adapter;
mod client;
mod env_file;
mod error;
pub(crate) mod metadata;
mod parse;
mod paste;

pub(crate) use adapter::{
    PaneInfo, TmuxAdapter, WindowGeometry, WorkspacePaneSnapshot, WorkspaceSnapshot,
    WorkspaceWindowSnapshot,
};
pub(crate) use client::TmuxClient;
pub(crate) use error::TmuxError;
pub(crate) use paste::{
    normalize_search_text, paste_then_submit_text, prompt_fragments, send_then_submit_text,
};
