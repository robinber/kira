# AGENTS.md

Machine-facing contract for coding agents working in this repository.

## Product

Kira is a **local tmux multi-agent workspace** CLI (`kira-mux`).

In scope:

- XDG config (global + per-project TOML, templates, profiles, groups)
- tmux session / window / pane lifecycle (`open`, `start`, `attach`, `kill`, `restart`)
- prompt send (including `send --wait`), pane capture
- status, list, agents; contextual project target `.`

Keep the product small. Prefer a clear CLI over new subsystems.

## Load order

1. This file.
2. [`.agents/skills/rust-strict/SKILL.md`](.agents/skills/rust-strict/SKILL.md)
   before any Rust change, review, or verification claim. Prefer that skill’s
   module map and error/exit details over duplicating them here.
3. Code next to the module you edit.

## Package facts

- Single package at repo root: `kira-mux` (`src/`, `tests/`).
- Edition `2024`, Rust `1.97.0` (`rust-toolchain.toml`).
- Lint policy lives in root `Cargo.toml` `[lints.*]` — do not weaken it.
- Nightly only for `cargo +nightly fmt`; otherwise use the pinned stable toolchain.

## Working rules

- Make the smallest change that satisfies the request.
- Self-check: would a senior engineer call this overcomplicated? If yes, simplify.
- **Enforced** in non-test code (package + clippy deny): `unsafe`, `unwrap`,
  `expect`, `todo!`, `unimplemented!`, `dbg!`.
- **Repository policy** (not a separate `panic` lint): avoid `panic!` in
  non-test code; prefer typed errors.
- Typed domain errors (`thiserror`) where exit codes / callers match; `anyhow`
  at I/O, orchestration, and binary edges.
- Secrets stay out of logs and fingerprints.

## Exit codes

Stable mapping in `src/main.rs` (also documented in the README):

| Code | Meaning |
|---|---|
| 0 | success |
| 1 | untyped / unexpected error |
| 2 | config / validation / unknown agent\|group / kill aborted |
| 3 | missing dependency (e.g. tmux) |
| 4 | workspace drifted |
| 5 | session absent |
| 6 | dead pane, pane died during wait, degraded |
| 7 | `send --wait` hard timeout |

## Commands

```bash
cargo +nightly fmt --all
cargo +nightly fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
cargo deny check advisories licenses sources bans
just check
```

Only claim a command passed if you ran it and checked its output.

## `kira-mux` map

Authoritative layout: [rust-strict module map](.agents/skills/rust-strict/SKILL.md).
Short form:

- `src/cli/` — clap surface
- `src/app/` — command handlers
- `src/config/` — XDG load / resolve / validate / fingerprint
- `src/tmux/` — adapter, client, parse, paste, env files
- `src/workspace/` — session lifecycle
- `src/inspector.rs` — topology classify + live inspect
- `src/agent_io/` — send / capture / wait / policy
- `src/model/` — resolved project + status types
- `src/prompt/`, `output.rs`, `error.rs`, `paths.rs`, `logging.rs`
- `tests/cli/` — real-tmux integration harness (`main`, `harness`, scenario modules)
