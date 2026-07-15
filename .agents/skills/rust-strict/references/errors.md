# errors

## Layers

| Type | Module | Use |
|---|---|---|
| `ConfigError` | `config/error.rs` | TOML load, validation, resolve, paths |
| `KiraMuxError` | `error.rs` | domain outcomes mapped to exit codes |
| `TmuxError` | `tmux/error.rs` | missing session/target, no server, command failure |
| `anyhow::Error` | edges | context, glue, binary `run()` |

`KiraMuxError::ConfigValidation` wraps `ConfigError`. Prefer returning the
typed error early; convert at the boundary with `?` / `.into()`.

## Policy

- No `unwrap` / `expect` / `panic!` / `todo!` / `unimplemented!` outside tests
  (workspace clippy denials).
- Prefer typed variants over `bail!("string")` when:
  - exit codes should distinguish the case, or
  - callers match on the error.
- Tests may use `test_support::{or_panic, ok, err}` instead of raw unwrap.

## Exit codes (`main.rs`)

| Code | Meaning |
|---|---|
| 0 | success |
| 2 | config / validation / unknown agent|group / kill aborted |
| 3 | missing dependency (e.g. tmux binary) |
| 4 | workspace drifted |
| 5 | session absent |
| 6 | dead pane or degraded launch |
| 1 | other (`anyhow` / untyped) |

New user-visible failure modes should get a `KiraMuxError` variant **and** an
exit-code arm when scripts need to distinguish them.

## Construction

- Fallible construction: `try_*` / `build` / `Result`-returning resolve paths.
- Do not add fallible `new` that panics on bad input.
