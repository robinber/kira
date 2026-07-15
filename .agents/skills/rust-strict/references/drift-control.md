# drift-control

Use when work risks maintainability debt: large files, duplicated helpers,
broad suppressions, second topology/error paths, or missing tests on critical
surfaces.

## No net-new debt

Passing tests are not enough — the touched surface should be no worse after the
change.

Before editing:

- size: `wc -l <path>`
- similar logic: search before adding parse / path / env / fingerprint / error map
- active findings only if the task or a maintained tracker points at them

## Size gates

| Condition | Behavior |
|---|---|
| \> 1000 LOC | No feature growth without extract/split (bugfix/test-only ok) |
| \> 800 LOC | Pressure zone; narrow additions |
| Deep / long function | Prefer extract over another nested branch |

Do not drive drive-by refactors of unrelated code just to satisfy a number.

Pressure zones today often include: `inspector.rs`, `config/resolve.rs`,
`tmux/client.rs`, `test_support/mod.rs`, `config/fingerprint.rs`,
`workspace/lifecycle.rs`.

## Duplication gates

Search before adding a third copy of:

- prompt template parse/render
- path expand / symlink / root escape checks
- config validation and env classification (`$VAR` vs literal)
- fingerprint field materialization
- tmux failure classification (`failed_tmux_status`, missing-target maps)
- JSON/text list/status rendering
- topology/drift classification (`inspect` vs list summary vs send resolve)

Two copies can be transitional. A third needs a shared helper or an explicit
divergence reason.

## Single topology truth

`inspector::inspect` / `classify_snapshot` own workspace topology. `send` /
`capture` resolve panes through that contract. Do not reintroduce a lighter
parallel check that can disagree on fingerprint or pane identity.

List uses a batched snapshot for speed, but failures must not become false
`Drifted` (empty snapshot default is forbidden).

## Suppression gates

New `#[allow]` / `#[expect]`:

- narrowest scope
- `reason = "..."` outside tests
- no silent broadening
- temporary ⇒ cleanup path

## Test gates

When touching critical logic, add focused tests for:

- config load / resolve / fingerprint
- inspect + drift reasons
- workspace lifecycle and list error mapping
- agent_io resolve / send / capture
- exit-code mapping for new domain errors

## Report checklist

For non-trivial changes, note:

- large files touched
- new duplication / params / suppressions
- commands run and remaining gaps
