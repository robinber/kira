# api-design

## Surface size

`kira-mux` is a CLI packaged as a library + thin binary. Keep the **public**
surface small:

- Prefer `pub(crate)` for modules and items used only inside the crate.
- Public today is intentionally limited (`run`, domain errors, config error,
  logging init). Do not expand it without a concrete consumer need.

## Shapes that match the codebase

- Domain enums with `thiserror` for matchable failures.
- Resolved types in `model/` after config resolution (not raw TOML structs).
- Adapter traits (`TmuxAdapter`) for testability; production `TmuxClient`.
- Options / request structs when parameter lists approach the clippy threshold
  (`too-many-arguments-threshold = 7` in `clippy.toml`).

## Naming

- Rust API Guidelines: `snake_case` functions, `UpperCamelCase` types.
- Domain words used in-repo: project, profile, agent, pane, fingerprint,
  topology, drift, degraded.

## Avoid

- Async public APIs or runtime crates unless the task requires them (this product
  is synchronous CLI + tmux subprocesses).
- Boolean soups in public helpers; prefer small enums for policy
  (`AgentMode`, `EnvResolutionMode`, `SubmitBehavior`).
- Encoding domain state as free-form strings when an enum already exists
  (`WorkspaceDriftReason`, `ProjectState`, …).
