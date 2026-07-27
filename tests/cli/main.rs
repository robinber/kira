//! Real-tmux integration tests for the `kira-mux` binary.
//!
//! Every test gets its own tmux server (unique `-L` socket) and its own XDG
//! config home, so tests run in parallel and leave nothing behind —
//! [`harness::TestBed`] kills its server on drop, even when an assertion
//! panics.
//!
//! Scope: only what `FakeTmux` cannot guarantee — the fidelity of the real
//! tmux client (send/capture semantics, session metadata, error messages)
//! and the end-to-end exit-code contract. Logic coverage lives in the unit
//! suite. Assertions poll with a generous timeout instead of sleeping a
//! fixed amount, so the suite is fast locally and tolerant on loaded CI
//! runners.
#![cfg(unix)]
#![allow(
    unused_crate_dependencies,
    reason = "integration test target uses only a subset of the package dependencies"
)]

mod exit_codes;
mod harness;
mod lifecycle;
mod send_capture;
