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

# Prefer `open` for interactive agents: attach, finish any first-run UI
# (trust directory, login, …), then detach and dispatch with `send`.
kira-mux open example
# …accept Codex/Claude/etc. prompts in the pane if this is a cold start…
# Ctrl-b d  (detach)

kira-mux send example assistant "review the auth module"
kira-mux capture example assistant --lines 80

# Agent-to-agent dispatch: block until the reply settles, print it on stdout.
kira-mux send example assistant "review the auth module" --wait
kira-mux status example
kira-mux kill example --yes
```

`start` (no attach) is fine once agents are already bootstrapped. On a cold
interactive first launch, use **`open`** (or `start` + `attach`) before the
first unattended `send` — see [Running vs input-ready](#running-vs-input-ready).

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
| `send` | Deliver a prompt to a **live** pane (not “agent ready”); `--wait` blocks until the reply settles |
| `capture` | Capture recent pane output |
| `restart` | Restart one agent, or all panes |
| `kill` | Tear down the managed session |

### Select the current project with `.`

Every command that accepts a project id also accepts the exact target `.`.
From anywhere inside a configured project root, Kira resolves `.` to that
project:

```bash
cd ~/projects/my-app/crates/api
kira-mux status .
kira-mux send . coder "review this crate"
kira-mux capture . coder --lines 80
```

Kira compares the physical current directory with configured project roots.
If roots are nested, the deepest matching root wins. No match, or equally
specific matches, is a configuration error (exit code 2); pass an explicit
project id to disambiguate. Profile selection works exactly as it does with an
explicit id, and the resolved project's configured id and root still determine
the tmux session identity.

`.` is a contextual project selector, not an arbitrary path argument. Other
paths and project ids keep their existing meaning.

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
  (see env rules below)

**Excluded on purpose** (cosmetic / non-topology — no drift):

- project `name`, agent `label`
- `capabilities`, `groups`, `prompt_template`
- `session_prefix`, `tmux_bin` — changing the prefix renames the session, so
  the old workspace shows as **stopped** (not drifted); `tmux_bin` only
  changes how tmux is invoked

### Agent `env`: literals vs `$VAR` references

| Config form | Example | Fingerprint | Refresh |
|---|---|---|---|
| **Literal** | `TOKEN = "secret"` | SHA-256 of the value (never raw) | Editing the value **drifts** the live session → `kill` then `start`/`open` |
| **Reference** | `TOKEN = "$KIRA_TOKEN"` | Variable **name** only (`KIRA_TOKEN`) | Changing the host env value does **not** drift. Healthy `start` reuses panes and keeps the old injection. Run **`restart`** (agent or all) to re-resolve and re-apply |

Secrets never appear in fingerprint material, tmux session options, or default logs (pane env is delivered via a short-lived env file, not argv).

### Invalid project files in `list`

Broken project TOML (parse errors, unknown fields, failed validation) is **not**
silently skipped. `list` / `list --json` includes a row with
`state = "config_error"` plus `path` and `error` fields. Exit code **2** when
any such row is present. Diagnostics live in the list output itself (stdout),
so `--json` does not depend on log level or merging stderr.

### Running vs input-ready

Kira reports agent state from **tmux pane liveness**, not from application
readiness.

| Term | Meaning in Kira |
|---|---|
| **`running`** | The pane process is alive (`pane_dead = 0`). |
| **Input-ready** | The agent TUI is past setup and will treat pasted text as a task. **Not detected** by Kira. |

So `status` / `agents` can show `running` while Codex is still on “Do you trust
this directory?”, a login screen, or another first-use dialog. `send` only
refuses **dead** panes; it will happily paste into a setup UI.

**Contract (operator-managed readiness):**

- There is **no** readiness config, poll, or tool-specific “done” detector.
- Cold start for interactive tools: use **`open`** (or attach), complete
  one-time bootstrap in the pane, detach, then use `send` / scripts.
- Headless automation assumes agents are already past that bootstrap (or uses
  non-interactive agent modes).

**Manual cold-start scenario (Codex-like tools)**

1. `kira-mux open <project>` — session starts; agent may show a trust/login UI.
2. In the attached pane, accept prompts until the normal chat/input is ready.
3. Detach (`Ctrl-b d`).
4. `kira-mux send <project> <agent> "…"` — task text goes to the agent, not setup.
5. Read the reply with `kira-mux capture …`, or use `send … --wait` to block
   until the pane settles and print the capture on stdout (agent-to-agent).

If you `start` + `send` immediately on a brand-new interactive agent, the prompt
can land in the wrong UI. That is expected with this contract, not a silent
bug.

### Waiting for a reply: `send --wait`

`send --wait` polls the pane after delivery and prints the captured output on
stdout once it settles. The condition is **pane stability, not a formal done
signal**, in two phases: the pane must first *change* after the submit (the
reply started), then stay unchanged for a few seconds. A pane that dies or
vanishes mid-wait (killed window / missing target) fails with exit **6**; an
internal hard timeout (~10 min, no CLI flag) aborts with exit **7** and writes
the last capture to stderr (stdout stays reserved for confirmed-stable output).

Known limits: a reply streamed with pauses longer than the stability window
can be cut short; a pane that keeps redrawing (clocks, watchers) never
stabilizes and hits the hard timeout; a reply that fully renders *before* the
post-submit baseline is rare but ends in the hard timeout as well (no activity
observed).

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
