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
cargo install --path .
```

**First run**

```bash
kira-mux examples   # usage recipes
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

# Deliver /clear to an agent UI that supports it (no prompt template):
kira-mux send --clear example assistant

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
| `examples` | Print usage recipes (no config or tmux side effects) |
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

## Configuration

Files live under XDG: `~/.config/kira-mux/` by default
(`$XDG_CONFIG_HOME/kira-mux/` when set and absolute).

### Global config (`config.toml`)

Written by `kira-mux init`. Keys (all optional with defaults):

| Key | Role | Default (approx.) |
|---|---|---|
| `session_prefix` | Prefix for derived tmux session names | `kira` |
| `default_layout` | Layout when a project omits one | `auto` |
| `main_pane_ratio` | Main-pane ratio for supported layouts (30–70) | `50` |
| `window_name` | Managed window name | `agents` |
| `default_shell` | Shell for `mode = "shell"` agents | `/bin/sh` |
| `remain_on_exit` | Pane retention after exit (`off` / `failed` / `on`) | `failed` |
| `tmux_bin` | tmux executable name or path | `tmux` |
| `agent_templates` | Reusable agent blueprints (see below) | `[]` |

### Agent templates

Reusable defaults referenced by project agents via `template = "name"`.
Project fields override the template when set.

```toml
# ~/.config/kira-mux/config.toml
[[agent_templates]]
name = "codex"
label = "Codex"
command = "codex"
args = ["-a", "never", "-s", "danger-full-access"]
capabilities = ["rust", "impl"]
# Optional send-time overrides (also valid on project agents):
# submit = "double"            # single | double
# text_delivery = "paste"      # paste | send-keys
# prompt_template = "…"
# mode / shell_command / cwd / env as needed
```

```toml
# ~/.config/kira-mux/projects/my-app.toml
[[agents]]
id = "coder"
template = "codex"
label = "Coder"   # overrides template label
```

### Project sketch

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
- Optional per-agent (or template) send overrides when the basename heuristic
  is wrong: `submit = "single" | "double"`, `text_delivery = "paste" | "send-keys"`
- **Default submit heuristics** (when `submit` / `text_delivery` are unset):
  basenames `codex`, `claude`, `opencode`, `qwen`, `grok` get **double Enter**;
  `opencode` uses literal `send-keys` for multi-line text (others use paste)
- `root` must be absolute or `~/...` (not process-CWD-relative) so session
  identity stays stable no matter where you invoke `kira-mux`
- Agent `cwd` may still be relative to `root`
- Profiles (`[profiles.<name>]`) select alternate agent layouts for the same
  project when you need more than one workspace shape
- Named `groups` map group name → agent ids (for listing / prompt context)

### Exit codes

Scripts should treat these as stable:

| Code | Meaning |
|---|---|
| **0** | Success (also used when stdout hits a broken pipe, e.g. `… \| head`) |
| **1** | Untyped / unexpected error (`anyhow` edge) |
| **2** | Config / validation / unknown agent or group / kill aborted / list has `config_error` rows |
| **3** | Missing dependency (e.g. tmux binary not found) |
| **4** | Workspace **drifted** (fingerprint or topology mismatch) |
| **5** | Session **absent** |
| **6** | Dead pane, pane died during wait, or degraded launch |
| **7** | `send --wait` hard timeout (~10 min); last capture on stderr |

### JSON state vocabularies

`status --json` and `agents --json` both describe panes but use slightly
different agent-state strings (historical). Do not assume one field set maps
1:1 onto the other without reading the schemas:

| Surface | Examples of agent/pane state |
|---|---|
| `status` | `exited_clean`, `exited_failed`, `missing_pane`, … |
| `agents` | `dead`, `absent`, live capability fields, … |

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
- `capabilities`, `groups`, `prompt_template`, `submit`, `text_delivery`
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
   Use `send --clear <project> <agent>` to deliver a literal `/clear` (no
   prompt template) when you want a fresh agent context from the CLI.
5. Read the reply with `kira-mux capture …`, or use `send … --wait` to block
   until the pane settles and print the capture on stdout (agent-to-agent).

If you `start` + `send` immediately on a brand-new interactive agent, the prompt
can land in the wrong UI. That is expected with this contract, not a silent
bug.

### Waiting for a reply: `send --wait`

`send --wait` polls the pane after delivery and prints the captured output on
stdout once it converges. The condition is **pane convergence, not a formal
done signal**. Kira captures the screen before submit, waits for the pane to
durably move off that image (submission acknowledgement — a transient redraw
that reverts does not count), then waits for visible production to settle.
Every distinct normalized frame resets settling (including spinner frames);
the quiet window is sized to the evidence: 5 s after durable production, 10 s
for weak production, and 30 s when nothing changed after the acknowledgement
(a one-frame reply and a silently thinking model look identical). One final
identical poll confirms the result. These are internal heuristics, not CLI
timing flags. Use `send --wait --lines <N>` to widen the capture window
(default **200**, minimum **1** — zero is rejected because every capture would
be empty and wait could only fail at the hard timeout). Plain
`capture --lines` defaults to **30**.

A pane that dies or vanishes mid-wait (killed window, lost session, or a
stopped tmux server) fails with exit **6**; an internal hard timeout (~10 min)
aborts with exit **7** and writes the last capture to stderr (stdout stays
reserved for confirmed-stable output).

Known limits: activity perfectly synchronized with the 500 ms poll can be
invisible; a reply that pauses longer than the active quiet window is cut
short; a model that stays visually silent past the 30 s submission-only
window is reported done with only the echo captured; and an idle monotonic
counter (clock, watcher) never converges and reaches the hard timeout.

### Capture depth and alternate-screen TUIs

Some agent TUIs (Claude Code, Grok Build) run on the tmux **alternate
screen** and keep their transcript internally: tmux accumulates no history
for those panes, so a plain `capture-pane` can never return more than the
visible frame, no matter what `--lines` asks for. Others (Codex) write to the
normal screen and scroll into real tmux history.

When `capture` or the final `send --wait` capture targets an alternate-screen
pane and asks for more lines than the pane is tall, kira performs a **deep
capture**: it zooms the pane, temporarily grows the window (up to the
requested lines, capped at 1000 rows), lets the TUI repaint its transcript
into the taller frame, captures it, then restores the window exactly as
found — size, zoom, active pane, layout, and the window-local `window-size`
value. In a multi-pane layout whose window is already tall enough, the zoom
alone provides the depth and no resize happens. An attached client sees a
resize flicker while this runs — usually well under a second, bounded at
~5 s when the TUI never repaints. `send --wait` deepens only the final
capture, after convergence: the wait polls themselves never touch geometry.

If deep capture cannot run (for example the window is zoomed on another
pane), or the TUI never repaints the enlarged frame, kira falls back to the
visible-frame capture and logs a warning on stderr. Note that with `--json`
the default log level drops to `error`, so raise `KIRA_MUX_LOG=warn` to see
fallback warnings in scripted JSON flows — or check the JSON flags instead.

`capture --json` reports the depth context per capture: `alternate_on` (the
pane runs on the alternate screen), `pane_height` (the plain-capture depth
ceiling), `deep_capture` (the zoom/resize ran and a repaint of the enlarged
frame was observed), `depth_request_clamped` (the request exceeds what deep
capture can ever deliver for this pane — content beyond the 1000-row
ceiling is unreachable regardless of the outcome, including on
`not_needed`), and `deep_capture_status` — one of `not_applicable`
(normal-screen or dead pane), `not_needed` (no geometry change would deepen
further: the visible frame already covers the achievable depth),
`completed`, `busy` (another capture owns the window; retry later), or
`unavailable` (deepening failed; output is capped at the visible frame).

Concurrent deep captures of panes in the same window are serialized by a
per-window file lock (a sidecar next to the tmux server socket, released
automatically when the process exits): the contending capture does not
wait — it returns the visible-frame capture with `deep_capture_status:
"busy"` and a stderr warning.

Known limits: repaint detection is capture-based, with the same epistemic
caveats as wait convergence: a spinner frame that changes before the TUI
handles the resize can be mistaken for the repaint, and a TUI that never
stops animating returns its latest frame at the ~5 s bound. If the agent
process dies after wait convergence but during deepening, the (frozen)
converged output is still returned with exit 0 — the next command surfaces
the dead pane.

## Layout

```text
src/            kira-mux CLI (config, tmux, workspace, agent I/O)
tests/          real-tmux integration harness
examples/       sample project configs
.agents/        agent coding contracts (rust-strict)
```

## Development

Quality floor is intentional and strict:

- package lints: `unsafe` denied, `unwrap` / `expect` / `todo` denied, pedantic on
- `cargo +nightly fmt`, clippy `-D warnings`, rustdoc `-D warnings`
- `cargo deny` for advisories, licenses, and sources
- integration tests drive the compiled binary against real tmux servers
  (`tests/cli/`, needs `tmux` on `PATH`; each test uses an
  isolated socket, so your own tmux sessions are never touched)
- CI on push and pull requests (see [`.github/workflows/ci.yml`](./.github/workflows/ci.yml))

```bash
just check   # requires https://github.com/casey/just — the recipes only wrap the commands below
# or
cargo +nightly fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
cargo deny check advisories licenses sources bans
```

Agent coding policy: [`AGENTS.md`](./AGENTS.md) and
[`.agents/skills/rust-strict/`](./.agents/skills/rust-strict/).

## Status

Kira is early, single-maintainer software. The CLI and config schema may still
change. Issues and small PRs are welcome; large redesigns should start as a
discussion.

## License

MIT. See [`LICENSE`](./LICENSE).
