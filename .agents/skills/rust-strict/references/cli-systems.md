# cli-systems

## Binary shape

```
main.rs          logging + kira_mux::run() + exit_code_for_error
lib.rs           pub fn run() → clap parse → app::run
cli/             clap types only
app/             command handlers (load context, call workspace/agent_io)
output.rs        stdout text / JSON
```

Keep `main.rs` thin. Do not put domain logic in the binary target.

## Streams

| Stream | Content |
|---|---|
| stdout | successful user data (`list`, `status`, `capture`, `--json`) |
| stderr | errors, warnings, interactive prompts (`confirm_kill`), tracing |

Do not print diagnostics to stdout when `--json` is in play (logging already
drops default level to `error` when `--json` appears before `--`).

## Exit codes

Map domain errors in `main.rs` (see `references/errors.md`). Preserve stable
codes when changing variants: scripts rely on 2/3/4/5/6.

## Testing CLI behavior

- Unit-test pure handlers and exit mapping without spawning the process.
- Prefer library-level tests with FakeTmux over full process integration unless
  adding an explicit real-tmux harness.
