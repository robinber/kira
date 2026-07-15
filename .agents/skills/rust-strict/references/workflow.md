# workflow

## 1. Source-backed guidance
- Start from the workspace policy files: `Cargo.toml` (`[workspace.package]`, `[workspace.lints]`), `rust-toolchain.toml`, `.rustfmt.toml`, `clippy.toml`, `.cargo/config.toml`, `deny.toml`, and CI workflow files define the effective contract.
- Treat `rust-version` in `[workspace.package]` as the MSRV declaration. Update it deliberately and verify it against CI and the selected toolchain.
- Treat `rust-toolchain.toml` as the default toolchain contract. Do not infer nightly by default; use `+nightly` only for commands that explicitly require it, such as this repo's rustfmt configuration.
- Run `rustfmt` before broad verification so the diff reflects behavior, not formatting drift. Use the repository's `.rustfmt.toml` (edition 2024 baseline with import grouping).
- Check `.github/workflows/ci.yml` before widening scope; it encodes the project's canonical gates, `RUSTFLAGS=-D warnings`, `RUSTDOCFLAGS=-D warnings`, and split test lanes.
- Verify from the smallest relevant scope first: one package, one test target, one feature set. Escalate to `cargo check`, then `cargo test`, then workspace-wide or feature-complete commands only when the change affects shared code, feature gates, build scripts, or cross-crate behavior.

## 2. Local verification baseline
Default verification is impact-scoped. The five full-baseline gates are:

```bash
cargo +nightly fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
cargo deny check advisories licenses sources
cargo test --workspace --all-features
```

Cargo aliases (`.cargo/config.toml`) provide shorthand: `lint`, `lint-app`, `doc-all`, `deny-all`, `test-all`. The Justfile provides `fmt-check` and `check`.

Repository CI (`.github/workflows/ci.yml`) runs on push and pull requests: Rust format, clippy with `-D warnings`, rustdoc with `-D warnings`, cargo-deny, and workspace tests.

When reporting verification, copy the exact command shape and scope: package, workspace/member selection, target (`--lib`, `--bin`, tests), feature set, and whether doctests or dependency-policy checks were included.

## 3. Skill policy
- Always do an anchor pass over workspace policy files and CI before editing.
- Prefer the narrowest command that can fail for the change you made.
- Escalate in this order when needed: package scope, `--all-targets`, `--all-features`, then `--workspace`.
- Treat workspace-wide verification as mandatory when editing shared crates, root manifests, workspace dependencies, dependency policy, lint/toolchain policy, or feature plumbing.
- Keep MSRV, lint policy, deny policy, and CI expectations aligned; if one changes, check the others.

## 4. Allowed exceptions
- If the change is manifest-only or formatting-only, a focused manifest check plus `rustfmt` is enough unless CI policy says otherwise.
- If the workspace is very large, first verify the affected package and direct dependents, then widen only if the change crosses crate boundaries.
- For documentation-only edits, you may skip full test execution unless doctests or public API examples changed.
- If CI is the authoritative gate for a slow target, a local narrower check is acceptable as long as you clearly note the remaining gap.
