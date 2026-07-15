use super::context::PromptContext;

type ContextAccessor = fn(&PromptContext) -> &str;

/// Single source of truth for template variables: name → accessor.
/// `render` and `lint_template` both consult this table, so adding a
/// variable is a one-line change.
const PROMPT_VARIABLES: &[(&str, ContextAccessor)] = &[
    ("user_prompt", |ctx| &ctx.user_prompt),
    ("agent_name", |ctx| &ctx.agent_name),
    ("project_name", |ctx| &ctx.project_name),
    ("active_agents", |ctx| &ctx.active_agents),
    ("agent_states", |ctx| &ctx.agent_states),
];

pub(crate) fn render(template: &str, ctx: &PromptContext) -> String {
    let mut result = String::with_capacity(template.len());
    let mut rest = template;

    while let Some(start) = rest.find("{{") {
        result.push_str(&rest[..start]);
        if let Some(end) = rest[start..].find("}}") {
            let var = rest[start + 2..start + end].trim();
            let replacement = PROMPT_VARIABLES
                .iter()
                .find(|(name, _)| *name == var)
                .map(|(_, get)| get(ctx));
            match replacement {
                Some(val) => result.push_str(val),
                None => result.push_str(&rest[start..start + end + 2]),
            }
            rest = &rest[start + end + 2..];
        } else {
            result.push_str(&rest[start..]);
            return result;
        }
    }
    result.push_str(rest);
    result
}

pub(crate) fn lint_template(template: &str) -> Vec<String> {
    let mut unknowns = Vec::new();
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        match rest[start..].find("}}") {
            Some(end) => {
                let var = rest[start + 2..start + end].trim();
                if !PROMPT_VARIABLES.iter().any(|(name, _)| *name == var) {
                    unknowns.push(format!("{{{{{var}}}}}"));
                }
                rest = &rest[start + end + 2..];
            }
            None => break,
        }
    }
    unknowns
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full_context() -> PromptContext {
        PromptContext {
            user_prompt: "deploy the service".into(),
            agent_name: "coder".into(),
            project_name: "my-project".into(),
            active_agents: "coder, reviewer, tester".into(),
            agent_states: "coder:idle, reviewer:busy, tester:idle".into(),
        }
    }

    #[test]
    fn renders_all_variables() {
        let template = concat!(
            "Agent {{agent_name}} in {{project_name}}: {{user_prompt}}. ",
            "Active: {{active_agents}}. States: {{agent_states}}."
        );
        let ctx = full_context();
        let result = render(template, &ctx);

        assert_eq!(
            result,
            concat!(
                "Agent coder in my-project: deploy the service. ",
                "Active: coder, reviewer, tester. ",
                "States: coder:idle, reviewer:busy, tester:idle."
            )
        );
    }

    #[test]
    fn unknown_variable_left_as_is() {
        let ctx = PromptContext::minimal("coder", "proj", "hello");
        let result = render("{{unknown}} and {{also_unknown}}", &ctx);
        assert_eq!(result, "{{unknown}} and {{also_unknown}}");
    }

    #[test]
    fn no_template_vars_returns_unchanged() {
        let ctx = full_context();
        let plain = "just some plain text with no variables";
        assert_eq!(render(plain, &ctx), plain);
    }

    #[test]
    fn multiple_occurrences_of_same_var() {
        let ctx = PromptContext::minimal("coder", "proj", "hello");
        let result = render("{{agent_name}} is {{agent_name}}", &ctx);
        assert_eq!(result, "coder is coder");
    }

    #[test]
    fn user_prompt_with_braces_not_confused() {
        let ctx = PromptContext::minimal("coder", "proj", "use {{custom_var}} in config");
        let template = "Prompt: {{user_prompt}}";
        let result = render(template, &ctx);
        assert_eq!(result, "Prompt: use {{custom_var}} in config");
    }

    #[test]
    fn user_prompt_with_known_var_not_reexpanded() {
        let ctx = PromptContext::minimal("coder", "proj", "check {{agent_name}} status");
        let template = "Prompt: {{user_prompt}}";
        let result = render(template, &ctx);
        assert_eq!(
            result, "Prompt: check {{agent_name}} status",
            "injected values must not be re-expanded"
        );
    }

    #[test]
    fn lint_template_detects_unknown_vars() {
        let unknowns = lint_template("{{user_prompt}} {{tyop}}");
        assert_eq!(unknowns, vec!["{{tyop}}"]);
    }

    #[test]
    fn lint_template_known_vars_only_returns_empty() {
        let unknowns = lint_template(
            "{{user_prompt}} {{agent_name}} {{project_name}} {{active_agents}} {{agent_states}}",
        );
        assert!(unknowns.is_empty());
    }

    #[test]
    fn lint_template_no_vars_returns_empty() {
        let unknowns = lint_template("just plain text");
        assert!(unknowns.is_empty());
    }

    #[test]
    fn render_trims_spaces_in_delimiters() {
        let ctx = PromptContext::minimal("coder", "proj", "hello");
        assert_eq!(render("{{ user_prompt }}", &ctx), "hello");
    }

    #[test]
    fn render_trims_multiple_spaces_in_delimiters() {
        let ctx = PromptContext::minimal("coder", "proj", "hello");
        assert_eq!(render("{{  user_prompt  }}", &ctx), "hello");
    }

    #[test]
    fn render_trims_tabs_in_delimiters() {
        let ctx = PromptContext::minimal("coder", "proj", "hello");
        assert_eq!(render("{{\tuser_prompt}}", &ctx), "hello");
    }

    #[test]
    fn render_trims_asymmetric_whitespace_in_delimiters() {
        let ctx = PromptContext::minimal("coder", "proj", "hello");
        assert_eq!(render("{{ user_prompt}}", &ctx), "hello");
    }

    #[test]
    fn lint_template_trims_whitespace_in_known_var() {
        let unknowns = lint_template("{{ user_prompt }}");
        assert!(unknowns.is_empty());
    }

    #[test]
    fn lint_template_trims_whitespace_in_unknown_var() {
        let unknowns = lint_template("{{ tyop }}");
        assert_eq!(unknowns, vec!["{{tyop}}"]);
    }
}
