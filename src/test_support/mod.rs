//! Shared unit-test fixtures: `FakeTmux`, `test_project`, and panic helpers.
//!
//! All unwrap-style helpers take a context string so failures name the
//! operation under test (and still use `#[track_caller]` for the call site):
//! - free functions [`ok`] / [`err`] / [`some`]
//! - extension methods [`TestResultExt::or_panic`] / [`err_or_panic`]
//! - [`TestOptionExt::or_panic`] for `Option`

mod assert;
mod fake_tmux;
mod fixtures;

pub(crate) use assert::{TestOptionExt, TestResultExt, err, ok, some};
pub(crate) use fake_tmux::{FakeOp, FakeTmux};
pub(crate) use fixtures::{
    setup_healthy_session, setup_session_with_dead_panes, test_agent, test_project,
};
