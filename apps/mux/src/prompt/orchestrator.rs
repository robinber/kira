//! Optional orchestrator launch-template helpers kept for config compatibility.

/// Variables recognised in `orchestrator_prompt_template`.
const ORCHESTRATOR_TEMPLATE_VARS: &[&str] = &[
    "objective",
    "orchestrator_envelope",
    "orchestrator_rules",
    "project_id",
    "profile",
    "orchestrator_profile",
    "orchestrator_agent_id",
    "thread",
    "trace_id",
    "mux_bin",
];

/// Return unknown `{{var}}` placeholders in an orchestrator template.
pub(crate) fn lint_orchestrator_template(template: &str) -> Vec<String> {
    let mut unknowns = Vec::new();
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        match rest[start..].find("}}") {
            Some(end) => {
                let var = rest[start + 2..start + end].trim();
                if !ORCHESTRATOR_TEMPLATE_VARS.contains(&var) {
                    unknowns.push(format!("{{{{{var}}}}}"));
                }
                rest = &rest[start + end + 2..];
            }
            None => break,
        }
    }
    unknowns
}
