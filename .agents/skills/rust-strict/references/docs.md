# docs

## Expectations

- Public items need rustdoc (`missing_docs` is deny at workspace level).
- Internal `pub(crate)` items need brief docs when subtle; skip ceremony on
  obvious glue.
- Prefer contract-focused prose: purpose, invariants, errors.

## CI gate

```bash
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
# or: cargo doc-all with RUSTDOCFLAGS set
```

Failures include broken/private intra-doc links and bare URLs.

## Style

- Document `Errors` when a public function returns `Result` with meaningful
  failure modes.
- Document `Panics` only if something can panic (prefer not in production code).
- Keep examples minimal and accurate if present; this CLI crate rarely needs
  doctest examples on private modules.

## Exceptions

- One-line getters/pass-throughs may rely on the parent type’s docs.
- Generated or trivial private helpers need only what keeps the code readable.
