//! Tmux subprocess client, output parsing, and session metadata.

mod adapter;
mod client;
mod env_file;
mod error;
pub(crate) mod metadata;
mod parse;
mod paste;

pub use adapter::PaneInfo;
pub(crate) use adapter::TmuxAdapter;
pub use client::TmuxClient;
pub use error::TmuxError;
pub(crate) use paste::paste_then_submit_text;
