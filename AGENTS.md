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

1. This file — product contract, package facts, exit codes, module map.
2. Shared Rust skill **rust-strict** (v1.2.0+) before any Rust change, review, or
   verification claim. One canonical checkout; Claude/Grok paths are symlinks:

   | Tool | Path |
   |---|---|
   | Codex (canonical submodule) | [`.agents/skills/rust-strict/SKILL.md`](.agents/skills/rust-strict/SKILL.md) |
   | Claude Code | [`.claude/skills/rust-strict/SKILL.md`](.claude/skills/rust-strict/SKILL.md) → symlink |
   | Grok | [`.grok/skills/rust-strict/SKILL.md`](.grok/skills/rust-strict/SKILL.md) → symlink |

   Source: https://github.com/robinber/agent-skills-rust (pin tag, currently `v1.2.0`).
3. Code next to the module you edit.

When documents disagree, stop and surface the conflict. Do not silently choose
the interpretation that permits more work.

## Package facts

| Fact | Value |
|---|---|
| Layout | single package at repo root: `kira-mux` (`src/`, `tests/`) |
| Edition / MSRV | `2024` / `1.97.0` (`rust-toolchain.toml` + package metadata) |
| Nightly | **only** `cargo +nightly fmt` (unstable rustfmt options) |
| Lint floor | root `Cargo.toml` `[lints.*]` + `clippy.toml` — do not weaken |
| Convenience | cargo aliases (`lint`, `test-all`, `doc-all`, `deny-all`); `just check` |
| CI | `.github/workflows/ci.yml` (fmt, clippy, doc, deny, test lanes) |
| Drift profile | rust-strict defaults (800 / 1000 LOC, ≤ 6 params) |

### Lint floor (do not weaken)

- `unsafe_code = deny`, `missing_docs = deny` (package)
- clippy deny: `unwrap_used`, `expect_used`, `panic`, `todo`, `unimplemented`,
  `dbg_macro`
- `clippy.toml`: `allow-panic-in-tests = true`; `too-many-arguments-threshold = 6`
  (integration harness helpers outside `#[test]` may use a scoped
  `clippy::panic` allow — see `tests/cli/harness.rs`)
- groups: `correctness` / `suspicious` deny; pedantic and others **warn** at package level
- CI: `RUSTFLAGS=-D warnings`, `RUSTDOCFLAGS=-D warnings`
- Optional: `cargo lint-pedantic` (pedantic as deny) — not required by CI

### Feature matrix

Default package features. Prefer explicit `-p kira-mux` when focusing. Repo
commands often use `--workspace --all-features`; that matches this single-package
layout — still report the exact selection used.

## Working rules

- Make the smallest change that satisfies the request.
- Self-check: would a senior engineer call this overcomplicated? If yes, simplify.
- **Enforced** in non-test code (package + clippy deny): `unsafe`, `unwrap`,
  `expect`, `panic!`, `todo!`, `unimplemented!`, `dbg!`.
- Prefer typed errors over panics; test-only `panic!` is allowed via
  `allow-panic-in-tests`.
- Typed domain errors (`thiserror`) where exit codes / callers match; `anyhow`
  at I/O, orchestration, and binary edges.
- Default new items to `pub(crate)` unless the binary/tests need them public.
- Public today: `run`, `KiraMuxError`, `WorkspaceDriftReason`,
  `config::ConfigError`, `logging::init_logging`, `output::StdoutClosed`,
  `output::is_stdout_closed` (binary exit-code / EPIPE edge).
- Keep `main.rs` thin: init logging, call `kira_mux::run()`, map exit codes.
- This crate is **not** an async service: do not introduce an async runtime,
  streams, or public `async fn` traits unless the task explicitly requires it.
- Match module boundaries: `cli` → `app` → `workspace` / `agent_io` → `tmux` /
  `config`.
- **One topology truth:** pane I/O must not invent a second drift contract next
  to `inspector::inspect` / `classify_snapshot`.

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

Error layers: `ConfigError` (config), `KiraMuxError` (domain), `TmuxError`
(tmux), `anyhow` at binary/glue edges.

## Secrets and logging

- Secrets stay out of logs, fingerprints, and process argv.
- Use `tracing` for diagnostics; user-facing success data on stdout via
  `output.rs`.
- Default log level warn; `KIRA_MUX_LOG` / `RUST_LOG` override; `--json` lowers
  default to error.
- Redact env with `logging::redact_env_value` when logging launch env.
- Fingerprint hashes literal env values; pane env via `tmux/env_file`, never on
  tmux argv.

## Module map (`src/`)

| Path | Role |
|---|---|
| `cli/` | clap surface |
| `app/` | command handlers |
| `config/` | XDG load / resolve / validate / fingerprint |
| `tmux/` | adapter, client, parse, paste, env files |
| `workspace/` | session lifecycle, layout, status summaries |
| `inspector.rs` | topology classify + live inspect |
| `agent_io/` | send / capture / pane resolve / submit policy |
| `model/` | resolved project + status DTOs |
| `prompt/` | template render + context |
| `output.rs` | text / JSON printing |
| `error.rs` | `KiraMuxError` + drift reasons |
| `main.rs` | logging init + exit-code map |
| `tests/cli/` | real-tmux integration harness |

## Domain duplication hotspots

Search before adding a third copy of:

- prompt template parse/render
- path expand / symlink / root escape checks
- config validation and env classification (`$VAR` vs literal)
- fingerprint field materialization
- tmux failure classification (`failed_tmux_status`, missing-target maps)
- JSON/text list/status rendering
- topology/drift classification (`inspect` vs list summary vs send resolve)

## Pressure zones (approx.)

Treat as large / high-churn surfaces: `test_support/fake_tmux/`, `tmux/client`,
`inspector.rs`, `workspace/lifecycle.rs`, `config/load.rs`,
`config/resolve/` (`mod`/`agents`/`paths`/`validate`), `agent_io/send.rs`,
`agent_io/deep_capture.rs`, `tests/cli/` (harness + scenarios).

### Near 800 LOC (do not grow casually)

| Path | Approx. LOC | Guidance |
|---|---:|---|
| `test_support/fake_tmux/mod.rs` | ~785 | **Headroom thin.** New knobs, scripted faults, or helpers must land in a focused sibling (`adapter`, new submodule, or `tests`) — do not keep stacking fields/methods on `mod.rs` past the 800 pressure line. Prefer extract-first when a change would push it over. |

## Critical surfaces (tests when touched)

- config load / resolve / fingerprint
- inspect + drift reasons
- workspace lifecycle and list error mapping
- agent_io resolve / send / capture
- exit-code mapping for new domain errors

Unit tests next to modules; FakeTmux helpers in `test_support` (`#[cfg(test)]`).
Prefer deterministic setup over real tmux unless writing explicit integration
tests. Prefer coordination over new `sleep`s in tests.

## Verification commands

```bash
cargo +nightly fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
cargo deny check advisories licenses sources bans
just check
```

Impact-scoped iteration is fine; report exact package, target, feature set, and
gaps. Only claim a command passed if you ran it and checked its output.

## Deviations from rust-strict defaults

- Nightly is required for `cargo fmt` in this repo (not optional).
- Pedantic is package-level **warn**, not a global deny (optional
  `cargo lint-pedantic`).
- Single package; `--workspace` is used in CI/aliases for consistency.
