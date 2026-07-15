# Kira

[![CI](https://github.com/robinber/kira/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/robinber/kira/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](./LICENSE)
[![Rust MSRV](https://img.shields.io/badge/rust-1.97.0%2B-orange.svg)](./rust-toolchain.toml)

**Kira is a local tmux multi-agent workspace tool.**

Define coding agents in TOML, open a managed tmux session, send prompts, capture
pane output, and take over any pane with the muscle memory you already have.

No daemon. No cloud. No database. Just your machine, tmux, and the agents you
already run.

## Why

Most agent runners hide workers behind opaque processes. When something goes
sideways, you cannot see the pane, scroll back, or type into the session.

Kira does the opposite:

- **tmux is the UI.** Each agent is a real pane you can attach to, watch, and
  hijack.
- **Config is local and boring.** XDG TOML under `~/.config/kira-mux/`.
- **The CLI is small.** Launch, inspect, send, capture, restart, kill.

## Quick start

**Prerequisites**

- Rust `1.97.0` (pinned in [`rust-toolchain.toml`](./rust-toolchain.toml))
- Nightly rustfmt: `rustup toolchain install nightly --profile minimal --component rustfmt`
- [`tmux`](https://github.com/tmux/tmux) 3.3+
- [`cargo-deny`](https://github.com/EmbarkStudios/cargo-deny) (for the full quality gate)

**Install**

```bash
git clone https://github.com/robinber/kira
cd kira
cargo install --path apps/mux
```

**First run**

```bash
kira-mux init
# edit ~/.config/kira-mux/projects/example.toml
# set `root` to a real project path and adjust agents

kira-mux open example
kira-mux send example assistant "review the auth module"
kira-mux capture example assistant --lines 80
kira-mux status example
kira-mux kill example --yes
```

See [`examples/solo-coder/`](./examples/solo-coder) for a ready-made project
config.

## Commands

| Command | Purpose |
|---|---|
| `init` | Write default XDG config |
| `open` | Create or repair the workspace and attach |
| `start` | Create or repair without attaching |
| `attach` | Attach to an existing session |
| `list` | List configured projects |
| `status` | Live workspace / agent state |
| `agents` | Inspect agents (list, capabilities, groups) |
| `send` | Deliver a prompt to an agent pane |
| `capture` | Capture recent pane output |
| `restart` | Restart one agent, or all panes |
| `kill` | Tear down the managed session |

## Configuration sketch

`~/.config/kira-mux/projects/my-app.toml`:

```toml
id = "my-app"
name = "My App"
root = "~/projects/my-app"
layout = "side-by-side"

[[agents]]
id = "coder"
label = "Coder"
command = "codex"

[[agents]]
id = "tests"
label = "Tests"
mode = "shell"
shell_command = "npm test -- --watch"
```

- `mode = "direct"` (default) runs `command` (+ optional `args`)
- `mode = "shell"` runs `shell_command` through the configured shell
  (`args` are not used in shell mode and are rejected at config load)
- `root` must be absolute or `~/...` (not process-CWD-relative) so session
  identity stays stable no matter where you invoke `kira-mux`
- Agent `cwd` may still be relative to `root`
- Profiles (`[profiles.<name>]`) select alternate agent layouts for the same
  project when you need more than one workspace shape

### What causes workspace drift

Each managed session stores a **config fingerprint**. Commands like `status`,
`send`, `restart`, and `list` compare the live session to the resolved project.
A mismatch is reported as **drifted** (exit code 4 for commands that fail on
drift) — fix by `kill` then `start`/`open`, or align config with the running
workspace.

**Included in the fingerprint** (changing these drifts a live session):

- project id, profile id, root path
- layout, main pane ratio, window name
- default shell, remain-on-exit
- per agent: id, mode, command / shell_command / args (mode-aware), cwd, env
  (literal values are hashed; `$VAR` references fingerprint the variable name)

**Excluded on purpose** (cosmetic / non-topology — no drift):

- project `name`, agent `label`
- `capabilities`, `groups`, `prompt_template`
- `session_prefix`, `tmux_bin` — changing the prefix renames the session, so
  the old workspace shows as **stopped** (not drifted); `tmux_bin` only
  changes how tmux is invoked

Literal env values never appear in the fingerprint material; only digests do.

### Invalid project files in `list`

Broken project TOML (parse errors, unknown fields, failed validation) is **not**
silently skipped. `list` / `list --json` includes a row with
`state = "config_error"` plus `path` and `error` fields. Exit code **2** when
any such row is present. Diagnostics live in the list output itself (stdout),
so `--json` does not depend on log level or merging stderr.

## Layout

```text
apps/mux/       kira-mux CLI (config, tmux, workspace, agent I/O)
examples/       sample project configs
.agents/        agent coding contracts (rust-strict)
```

## Development

Quality floor is intentional and strict:

- workspace lints: `unsafe` denied, `unwrap` / `expect` / `todo` denied, pedantic on
- `cargo +nightly fmt`, clippy `-D warnings`, rustdoc `-D warnings`
- `cargo deny` for advisories, licenses, and sources
- integration tests drive the compiled binary against real tmux servers
  (`apps/mux/tests/cli.rs`, needs `tmux` on `PATH`; each test uses an
  isolated socket, so your own tmux sessions are never touched)
- CI on push and pull requests (see [`.github/workflows/ci.yml`](./.github/workflows/ci.yml))

```bash
just check   # requires https://github.com/casey/just — the recipes only wrap the commands below
# or
cargo +nightly fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
cargo deny check advisories licenses sources
```

Agent coding policy: [`AGENTS.md`](./AGENTS.md) and
[`.agents/skills/rust-strict/`](./.agents/skills/rust-strict/).

## Status

Kira is early, single-maintainer software. The CLI and config schema may still
change. Issues and small PRs are welcome; large redesigns should start as a
discussion.

## License

MIT. See [`LICENSE`](./LICENSE).
