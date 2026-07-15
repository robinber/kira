# lints

## Where policy lives

| Layer | Location |
|---|---|
| Workspace | root `Cargo.toml` `[workspace.lints]` |
| Crate inherit | `apps/mux/Cargo.toml` → `[lints] workspace = true` |
| Clippy knobs | `clippy.toml` (MSRV, thresholds, `doc-valid-idents`) |
| CI | `RUSTFLAGS=-D warnings`, `RUSTDOCFLAGS=-D warnings` |

## Current floor (do not weaken)

**Rust / rustdoc (deny):** `unsafe_code`, `missing_docs`, `elided_lifetimes_in_paths`,
`unused_lifetimes`, `unused_macro_rules`, `unused_qualifications`, rustdoc link/URL lints.

**Clippy (deny):** `dbg_macro`, `expect_used`, `todo`, `unimplemented`, `unwrap_used`,
plus `correctness` / `suspicious` groups at deny.

**Clippy (warn, including pedantic):** complexity/perf/style/pedantic and many
individual pedantic-style lints listed in the workspace manifest. CI `-D warnings`
makes those warnings fail CI when enabled via `RUSTFLAGS`.

## Suppressions

- Prefer fix over allow.
- New `#[allow]` / `#[expect]` need smallest scope and `reason = "..."`.
- Do not broaden an existing suppression silently.
- Temporary allowances need a cleanup path.

## Ratchet

Tighten deliberately at workspace level. Trial stricter lints narrowly before
raising workspace deny. Runtime first; tests second.

Optional local check: `cargo lint-pedantic` (pedantic as deny) — not required by CI.
