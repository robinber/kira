# testing

## Layout in this repo

- Unit tests live in `#[cfg(test)]` modules next to production code.
- Shared fixtures: `apps/mux/src/test_support/` (FakeTmux, `test_project`,
  `setup_healthy_session`, `or_panic` helpers).
- No `tests/` integration crate yet; real-tmux coverage is optional/future.
- Binary exit-code tests live in `apps/mux/src/main.rs`.

## Policy

- Prefer FakeTmux over live tmux for unit tests.
- Prefer deterministic setup helpers over ad hoc session wiring in each test.
- Prefer coordination over new `thread::sleep` in tests. Production send/paste
  already uses short settles; do not add more sleeps without a clear need.
- Name filters explicitly:

  ```bash
  cargo test -p kira-mux --lib agent_io::resolve --all-features
  cargo test -p kira-mux --bin kira-mux
  ```

- Never claim “tests pass” without package, target, feature set, and filter.

## Critical surfaces

When touching these, add or update focused tests:

- `config/` resolve, validate, fingerprint, load shapes
- `inspector` topology classify + inspect
- `workspace` lifecycle (start/repair/restart/kill) and list summary mapping
- `agent_io` resolve/send/capture (including drift and dead-pane behavior)
- `main` exit-code mapping for new `KiraMuxError` variants
- `tmux` parse helpers and failure classification (`failed_tmux_status`)

## Exceptions

- Thin `main` glue covered by exit-code unit tests may not need more cases.
- If a gap remains, state it and run the narrowest command that still exercises
  the path.
