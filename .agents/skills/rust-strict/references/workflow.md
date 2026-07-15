# workflow

## Anchor pass

Before editing, read the effective contract from disk:

1. `AGENTS.md` — product scope and default commands
2. `Cargo.toml` / `apps/mux/Cargo.toml` — members, lints, deps
3. `rust-toolchain.toml` — stable `1.97.0`; nightly only for rustfmt
4. `.rustfmt.toml`, `clippy.toml`, `.cargo/config.toml`, `deny.toml`
5. `.github/workflows/ci.yml` and `justfile`

Do not invent MSRV, lint levels, or CI gates from memory.

## Local gates

Impact-scoped by default. Full gate (`just check` or equivalent):

```bash
cargo +nightly fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
cargo deny check advisories licenses sources
```

Aliases (`.cargo/config.toml`): `lint`, `lint-app`, `lint-pedantic`, `test-all`,
`doc-all`, `deny-all`.

CI (`.github/workflows/ci.yml`): separate jobs for fmt (nightly), clippy, doc,
deny, and test — all with `RUSTFLAGS` / `RUSTDOCFLAGS` = `-D warnings` where
applicable.

## Scope escalation

1. Focused: `cargo check -p kira-mux`, `cargo test -p kira-mux --lib <filter>`
2. Package: `--all-targets --all-features` on `kira-mux`
3. Workspace / release: full baseline above

Widen when the change touches workspace policy, deps, lint/toolchain, or
cross-module contracts (inspect ↔ send, fingerprint ↔ resolve).

## Reporting

Always state:

- exact command(s) run
- package / target / features
- pass/fail
- intentional gaps (e.g. “did not run deny”)
