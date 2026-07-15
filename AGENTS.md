# AGENTS.md

Machine-facing contract for coding agents in this repository.

## Product

Kira (product A) is a **local tmux multi-agent workspace** CLI.

In scope: XDG config, tmux sessions/panes, send/capture, status/restart/kill.  
Out of scope: Postgres, message bus, workflow engine, canonical memory,
skill-driven orchestrator, meta-monitoring.

Historical product-B code is archived at `robinber/kira-archive`.

## Load order

1. This file.
2. [`.agents/skills/rust-strict/SKILL.md`](.agents/skills/rust-strict/SKILL.md)
   before any Rust change, review, or verification claim.
3. Code next to the module you edit.

## Workspace facts

- Cargo workspace, `resolver = "3"`, edition `2024`, Rust `1.97.0`.
- Single member: `apps/mux` (`kira-mux`).
- Lint policy lives in root `Cargo.toml` `[workspace.lints]` — do not weaken it.
- Nightly only for `cargo +nightly fmt`; stable from `rust-toolchain.toml` otherwise.

## Working rules

- Smallest change that satisfies the request.
- Self-check: would a senior engineer call this overcomplicated? If yes, simplify.
- Denied in non-test code (workspace lints): `unsafe`, `unwrap`, `expect`,
  `panic!`, `todo!`, `unimplemented!`, `dbg!`.
- `thiserror` in libraries, `anyhow` at the binary/orchestration edge.
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

- `src/cli/` — clap surface (product A only)
- `src/app/` — command handlers
- `src/config/` — XDG load/resolve/validate
- `src/tmux/` — tmux adapter
- `src/workspace/` — session lifecycle
- `src/agent_io/` — send + capture
- `src/model/` — resolved project/status types

Do not reintroduce workflow, msgbus, or orchestrator command groups without an
explicit operator decision.
