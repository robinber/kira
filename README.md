# Kira

[![CI](https://github.com/robinber/kira/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/robinber/kira/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](./LICENSE)
[![Rust MSRV](https://img.shields.io/badge/rust-1.97.0%2B-orange.svg)](./rust-toolchain.toml)

**Kira is a local tmux multi-agent workspace tool.**  
Configure agents in TOML, open a session, send prompts, watch panes, take over.

No Postgres. No workflow engine. No skill-driven orchestrator.

> The experimental multi-agent workflow stack (event log, canonical memory,
> orchestrator skills) lives frozen at
> [`robinber/kira-archive`](https://github.com/robinber/kira-archive).

## Quick start

**Prerequisites:** Rust `1.97.0` (see `rust-toolchain.toml`), nightly rustfmt,
`tmux` 3.3+, [`cargo-deny`](https://github.com/EmbarkStudios/cargo-deny).

```bash
git clone https://github.com/robinber/kira
cd kira
cargo install --path apps/mux

kira-mux init
# edit ~/.config/kira-mux/projects/example.toml → set root + agents

kira-mux open example
kira-mux send example assistant "review the auth module"
kira-mux capture example assistant --lines 80
kira-mux status example
kira-mux kill example --yes
```

## Commands

| Command | Purpose |
|---|---|
| `init` | Write default XDG config |
| `open` / `start` / `attach` | Workspace lifecycle |
| `list` / `status` / `agents` | Inspect projects and panes |
| `send` / `capture` | Deliver a prompt / read pane output |
| `restart` / `kill` | Repair or stop a session |

## Quality baseline

Same strict Rust floor as before:

- workspace lints: `unsafe` denied, `unwrap`/`expect`/`todo` denied, pedantic on
- `cargo +nightly fmt`, clippy `-D warnings`, rustdoc `-D warnings`
- `cargo deny` advisories / licenses / sources
- `just check` runs the full gate

```bash
just check
# or
cargo +nightly fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
cargo deny check advisories licenses sources
```

Agent coding policy: [`.agents/skills/rust-strict/`](.agents/skills/rust-strict/)
and [`AGENTS.md`](AGENTS.md).

## Layout

```text
apps/mux/     kira-mux binary (config, tmux, workspace, agent I/O)
examples/     sample project configs
```

## License

MIT. See [`LICENSE`](./LICENSE).
