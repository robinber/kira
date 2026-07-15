---
name: rust-strict
description: >-
  Use when changing, reviewing, or verifying Rust in this repo (kira-mux):
  Cargo workspace policy, clippy/rustfmt/rustdoc/tests, errors, config/tmux
  code, or release-quality checks.
---

# Rust Strict

Strict workflow for the **kira-mux** workspace: a single CLI crate that manages
local tmux multi-agent workspaces. Prefer the smallest change that matches
existing patterns in `apps/mux`.

Load order: `AGENTS.md` → this skill → code next to the module you edit.

## This repository

| Fact | Value |
|---|---|
| Product | local tmux multi-agent CLI (`kira-mux`) |
| Layout | Cargo workspace, one member: `apps/mux` |
| Edition / MSRV | `2024` / `1.97.0` (`rust-toolchain.toml`) |
| Nightly | **only** `cargo +nightly fmt` |
| Errors | `thiserror` domain types; `anyhow` at CLI / I/O edges |
| Visibility | almost everything `pub(crate)`; thin public surface from `lib.rs` |
| Secrets | never in logs, fingerprints, or process argv (env files for pane env) |

### Module map (`apps/mux/src/`)

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

Keep the product small. Prefer a clear CLI over new subsystems.

## Activation checklist

Before editing, reviewing, or claiming verification:

1. Read `AGENTS.md` and this skill.
2. Anchor on policy files (below); do not invent toolchain or lint policy from memory.
3. Name the package/target/feature set you will touch (`kira-mux` / lib, bin, tests).
4. Classify: implementation, review, docs, deps/supply-chain, workspace policy, or release verification.
5. Pick the **narrowest** command that can fail for the change; record what it does not cover.

## Anchor files

| File | Contract |
|---|---|
| `Cargo.toml` | workspace package, members, `[workspace.lints]`, deps |
| `apps/mux/Cargo.toml` | package inherits `[lints] workspace = true` |
| `rust-toolchain.toml` | pinned stable `1.97.0` |
| `.rustfmt.toml` | edition 2024; run with `+nightly` |
| `clippy.toml` | MSRV, thresholds, `doc-valid-idents` |
| `.cargo/config.toml` | aliases: `lint`, `lint-app`, `lint-pedantic`, `test-all`, `doc-all`, `deny-all` |
| `deny.toml` | advisories / licenses / sources |
| `.github/workflows/ci.yml` | fmt, clippy, doc, deny, test lanes |
| `justfile` | `just check` = full gate |

## Operating model

- Smallest change that satisfies the request.
- Self-check: would a senior engineer call this overcomplicated? If yes, simplify.
- Discovery → implementation → verification; do not widen scope without evidence.
- Match existing module boundaries (`cli` → `app` → `workspace` / `agent_io` → `tmux` / `config`).
- One topology truth: pane I/O should not invent a second drift contract next to `inspector::inspect`.

## Hard gates (no net-new debt)

Before non-trivial runtime edits:

1. Check target file size and module responsibility.
2. Search for existing helpers before adding parsing, path, env, fingerprint, or error-mapping logic.
3. Do not expand known debt in the touched area without an explicit reason.

| Gate | Rule |
|---|---|
| File > 1000 LOC | No feature growth without extract/split first (bugfix/test-only ok) |
| File > 800 LOC | Pressure zone: narrow additions only |
| ≥ 6 params | Prefer a request/context/options struct |
| 3rd copy of a helper | Extract shared code or justify divergence |
| `#[allow]` / `#[expect]` | Smallest scope + `reason = "..."`; no silent broadening |

Current large files (approx.): `inspector.rs`, `config/resolve.rs`, `tmux/client.rs`,
`test_support`, `config/fingerprint.rs`, `workspace/lifecycle.rs` — treat as pressure zones.

Details: `references/drift-control.md`.

## Verification

Impact-scoped by default. Full static baseline (release / workspace policy / deps / broad API):

```bash
cargo +nightly fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
cargo deny check advisories licenses sources
```

Also available: `just check`, cargo aliases (`lint`, `test-all`, `doc-all`, `deny-all`).

| Change type | Prefer |
|---|---|
| Logic / API | `cargo clippy -p kira-mux …` + focused `cargo test -p kira-mux --lib <filter>` |
| Formatting only | `cargo +nightly fmt --all --check` |
| Public docs | `RUSTDOCFLAGS="-D warnings" cargo doc-all` |
| Deps / deny.toml | `cargo deny check advisories licenses sources` |
| Cross-cutting runtime | `cargo test --workspace --all-features` |

Rules:

- Only claim a command passed if you ran it and checked output.
- Report exact package, target, feature set, and intentional gaps.
- Do not treat a narrow filter as full suite proof.
- This workspace has a single member; still prefer explicit `-p kira-mux` when focusing.

## Lint policy

Source of truth: root `Cargo.toml` `[workspace.lints]` + `clippy.toml`.

Already enforced here (do not weaken):

- `unsafe_code = deny`, `missing_docs = deny` (workspace)
- clippy: `unwrap_used`, `expect_used`, `todo`, `unimplemented`, `dbg_macro` = **deny**
- groups: `correctness`/`suspicious` deny; `pedantic` and others **warn** at workspace level
- CI: `RUSTFLAGS=-D warnings`, `RUSTDOCFLAGS=-D warnings`

Notes:

- New members must use `[lints] workspace = true`.
- Prefer fixing root causes over suppressions.
- Repo already runs pedantic at **warn**; do not propose global pedantic deny without a funded cleanup.
- `cargo lint-pedantic` exists for optional pedantic-as-deny checks.

Details: `references/lints.md`.

## Errors and exit codes

| Layer | Type | Role |
|---|---|---|
| Config | `config::ConfigError` | load / parse / validate / resolve |
| Domain | `KiraMuxError` | CLI-facing domain (drift, absent, dead pane, …) |
| Tmux | `tmux::TmuxError` | missing target/session, no server, command failure |
| Edge | `anyhow::Error` | glue, context, binary boundary |

Rules:

- No `unwrap` / `expect` / `panic!` / `todo!` / `unimplemented!` in non-test code.
- Prefer typed variants over string `bail!` when the CLI maps exit codes.
- `main.rs` maps `ConfigError` and `KiraMuxError` to exit codes (2–6); untyped failures → 1.
- Preserve actionable messages; never log secrets.

Details: `references/errors.md`, `references/cli-systems.md`.

## Visibility and API

- Default new items to `pub(crate)` unless the binary/tests need them public.
- Public today: `run`, `KiraMuxError`, `WorkspaceDriftReason`, `config::ConfigError`, `logging::init_logging`.
- Keep `main.rs` thin: init logging, call `kira_mux::run()`, map exit codes.
- Prefer explicit ownership; avoid gratuitous `clone` / `Arc<Mutex<_>>`.

This crate is not an async service: do not introduce async runtime, streams, or
public `async fn` traits unless the task explicitly requires it.

Details: `references/api-design.md`, `references/docs.md`.

## Logging and secrets

- Use `tracing` (not ad hoc `println!` for diagnostics). User-facing success data stays on stdout via `output.rs`.
- Default log level is warn; `KIRA_MUX_LOG` / `RUST_LOG` override; `--json` lowers default to error so stderr noise does not spoil machine output.
- Redact env values with `logging::redact_env_value` when logging launch env.
- Fingerprint hashes literal env values; never put secrets on tmux argv (use `tmux/env_file`).

## Tests

- Unit tests next to modules; FakeTmux + helpers in `test_support` (`#[cfg(test)]`).
- Prefer deterministic setup (`setup_healthy_session`, etc.) over real tmux unless writing an explicit integration harness.
- Prefer coordination over new `sleep`s in tests; production send/paste still uses short settles — do not add more without need.
- State exact filters: e.g. `cargo test -p kira-mux --lib inspector:: --all-features`.
- Critical surfaces needing tests when touched: fingerprint, resolve/validate, inspect/drift, lifecycle, send/capture resolve, exit-code map.

Details: `references/testing.md`.

## Dependencies

- Prefer workspace deps; keep the graph lean.
- `Cargo.toml` / `Cargo.lock` / `deny.toml` edits are supply-chain changes → run deny after.
- Do not re-enable unused feature flags (e.g. random clap/`tracing-subscriber` features) without a call site.
- Do not relax `deny.toml` without rationale.

## Review checklist

- Docs still accurate for any public item touched?
- No new hidden panics in non-test code?
- Errors typed where exit codes / callers care?
- Logs secret-safe? Fingerprint still excludes cosmetics?
- Topology / drift contract still single-sourced?
- Verification commands named with scope and gaps?
- Touched large files no worse than before?

## References

Load only what the task needs:

| File | When |
|---|---|
| `references/workflow.md` | anchor pass, CI, verification scope |
| `references/testing.md` | unit / FakeTmux / report discipline |
| `references/lints.md` | workspace lint structure, suppressions |
| `references/docs.md` | rustdoc / public items |
| `references/api-design.md` | types, constructors, surface size |
| `references/errors.md` | Result boundaries, panic policy |
| `references/cli-systems.md` | main / exit codes / stdout vs stderr |
| `references/drift-control.md` | file size, duplication, debt gates |

Keep this skill body short. Put deep material in `references/`, not here.
