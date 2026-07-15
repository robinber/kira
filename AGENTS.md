# AGENTS.md

Machine-facing contract for coding agents working in this repository.

## Product

Kira is a **local tmux multi-agent workspace** CLI (`kira-mux`).

In scope:

- XDG config (global + per-project TOML)
- tmux session / window / pane lifecycle
- prompt send and pane capture
- status, list, agents, restart, kill

Keep the product small. Prefer a clear CLI over new subsystems.

## Load order

1. This file.
2. [`.agents/skills/rust-strict/SKILL.md`](.agents/skills/rust-strict/SKILL.md)
   before any Rust change, review, or verification claim.
3. Code next to the module you edit.

## Workspace facts

- Cargo workspace, `resolver = "3"`, edition `2024`, Rust `1.97.0`.
- Single member: `apps/mux` (`kira-mux`).
- Lint policy lives in root `Cargo.toml` `[workspace.lints]` — do not weaken it.
- Nightly only for `cargo +nightly fmt`; otherwise use the pinned stable toolchain.

## Working rules

- Make the smallest change that satisfies the request.
- Self-check: would a senior engineer call this overcomplicated? If yes, simplify.
- Denied in non-test code (workspace lints): `unsafe`, `unwrap`, `expect`,
  `panic!`, `todo!`, `unimplemented!`, `dbg!`.
- `thiserror` in libraries, `anyhow` at the binary edge.
- Secrets stay out of logs and fingerprints.

## Commands

```bash
cargo +nightly fmt --all
cargo +nightly fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
cargo deny check advisories licenses sources
just check
```

Only claim a command passed if you ran it and checked its output.

## `kira-mux` map

- `src/cli/` — clap surface
- `src/app/` — command handlers
- `src/config/` — XDG load / resolve / validate
- `src/tmux/` — tmux adapter
- `src/workspace/` — session lifecycle
- `src/agent_io/` — send + capture
- `src/model/` — resolved project / status types
