use crate::config::AgentMode;
use crate::model::ResolvedAgent;
use crate::tmux::metadata::PANE_COMMAND_SHELL;
use crate::util::command_basename;

const DOUBLE_ENTER_TOOLS: &[&str] = &["codex", "claude", "opencode", "qwen", "grok"];
const SEND_KEYS_TEXT_TOOLS: &[&str] = &["opencode"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SubmitBehavior {
    SingleEnter,
    DoubleEnter,
}

pub(crate) fn infer_submit_behavior(
    agent: &ResolvedAgent,
    pane_command: Option<&str>,
) -> SubmitBehavior {
    match effective_basename(agent, pane_command) {
        Some(name) if DOUBLE_ENTER_TOOLS.contains(&name) => SubmitBehavior::DoubleEnter,
        _ if pane_command == Some(PANE_COMMAND_SHELL)
            && shell_command_needs_double_enter(agent) =>
        {
            SubmitBehavior::DoubleEnter
        }
        _ => SubmitBehavior::SingleEnter,
    }
}

fn effective_basename<'a>(
    agent: &'a ResolvedAgent,
    pane_command: Option<&'a str>,
) -> Option<&'a str> {
    pane_command
        .filter(|cmd| *cmd != PANE_COMMAND_SHELL)
        .or_else(|| {
            if agent.mode != AgentMode::Direct {
                return None;
            }
            agent.command.as_deref().map(command_basename)
        })
}

fn shell_command_needs_double_enter(agent: &ResolvedAgent) -> bool {
    agent
        .shell_command
        .as_deref()
        .is_some_and(|command| contains_tool(command, DOUBLE_ENTER_TOOLS))
}

fn contains_tool(command: &str, tools: &[&str]) -> bool {
    command
        .split(|ch: char| {
            ch.is_whitespace()
                || matches!(
                    ch,
                    '\'' | '"' | '`' | ';' | '&' | '|' | '(' | ')' | '<' | '>'
                )
        })
        .map(command_basename)
        .any(|token| tools.contains(&token))
}

pub(super) fn needs_send_keys_for_text(agent: &ResolvedAgent, pane_command: Option<&str>) -> bool {
    match effective_basename(agent, pane_command) {
        Some(name) => SEND_KEYS_TEXT_TOOLS.contains(&name),
        None if pane_command == Some(PANE_COMMAND_SHELL) => agent
            .shell_command
            .as_deref()
            .is_some_and(|command| contains_tool(command, SEND_KEYS_TEXT_TOOLS)),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use super::*;

    fn test_agent(mode: AgentMode, command: Option<&str>) -> ResolvedAgent {
        test_agent_with_id("test", mode, command)
    }

    fn test_agent_with_id(id: &str, mode: AgentMode, command: Option<&str>) -> ResolvedAgent {
        ResolvedAgent {
            id: id.to_string(),
            label: "Test".to_string(),
            mode,
            command: command.map(String::from),
            shell_command: None,
            args: vec![],
            cwd: PathBuf::from("/tmp"),
            env: BTreeMap::new(),
            capabilities: vec![],
            prompt_template: None,
        }
    }

    #[test]
    fn codex_gets_double_enter() {
        let agent = test_agent(AgentMode::Direct, Some("codex"));
        assert_eq!(
            infer_submit_behavior(&agent, None),
            SubmitBehavior::DoubleEnter
        );
    }

    #[test]
    fn codex_with_path_gets_double_enter() {
        let agent = test_agent(AgentMode::Direct, Some("/usr/local/bin/codex"));
        assert_eq!(
            infer_submit_behavior(&agent, None),
            SubmitBehavior::DoubleEnter
        );
    }

    #[test]
    fn qwen_gets_double_enter() {
        let agent = test_agent(AgentMode::Direct, Some("qwen"));
        assert_eq!(
            infer_submit_behavior(&agent, None),
            SubmitBehavior::DoubleEnter
        );
    }

    #[test]
    fn claude_gets_double_enter() {
        let agent = test_agent(AgentMode::Direct, Some("claude"));
        assert_eq!(
            infer_submit_behavior(&agent, None),
            SubmitBehavior::DoubleEnter
        );
    }

    #[test]
    fn opencode_gets_double_enter() {
        let agent = test_agent(AgentMode::Direct, Some("opencode"));
        assert_eq!(
            infer_submit_behavior(&agent, None),
            SubmitBehavior::DoubleEnter
        );
    }

    #[test]
    fn opencode_uses_send_keys_for_text() {
        let agent = test_agent(AgentMode::Direct, Some("opencode"));
        assert!(needs_send_keys_for_text(&agent, None));
    }

    #[test]
    fn pane_metadata_opencode_uses_send_keys_for_text() {
        let agent = test_agent(AgentMode::Direct, Some("my-tool"));
        assert!(needs_send_keys_for_text(&agent, Some("opencode")));
    }

    #[test]
    fn codex_keeps_paste_for_text() {
        let agent = test_agent(AgentMode::Direct, Some("codex"));
        assert!(!needs_send_keys_for_text(&agent, None));
    }

    #[test]
    fn pane_metadata_shell_sentinel_uses_shell_command_for_send_keys_text() {
        let mut agent = test_agent_with_id("opencode", AgentMode::Shell, None);
        agent.shell_command =
            Some("ssh -t root@example 'cd /root/kira && opencode --model test'".to_string());
        assert!(needs_send_keys_for_text(&agent, Some("__shell__")));
    }

    #[test]
    fn generic_command_gets_single_enter() {
        let agent = test_agent(AgentMode::Direct, Some("my-tool"));
        assert_eq!(
            infer_submit_behavior(&agent, None),
            SubmitBehavior::SingleEnter
        );
    }

    #[test]
    fn shell_mode_gets_single_enter() {
        let mut agent = test_agent(AgentMode::Shell, None);
        agent.shell_command = Some("codex --full-auto".to_string());
        assert_eq!(
            infer_submit_behavior(&agent, None),
            SubmitBehavior::SingleEnter
        );
    }

    #[test]
    fn no_command_gets_single_enter() {
        let agent = test_agent(AgentMode::Direct, None);
        assert_eq!(
            infer_submit_behavior(&agent, None),
            SubmitBehavior::SingleEnter
        );
    }

    #[test]
    fn pane_metadata_overrides_config() {
        let agent = test_agent(AgentMode::Direct, Some("echo"));
        assert_eq!(
            infer_submit_behavior(&agent, Some("codex")),
            SubmitBehavior::DoubleEnter
        );
    }

    #[test]
    fn pane_metadata_generic_gets_single_enter() {
        let agent = test_agent(AgentMode::Direct, Some("codex"));
        assert_eq!(
            infer_submit_behavior(&agent, Some("my-tool")),
            SubmitBehavior::SingleEnter
        );
    }

    #[test]
    fn pane_metadata_shell_sentinel_uses_resolved_direct_command() {
        let agent = test_agent(AgentMode::Direct, Some("codex"));
        assert_eq!(
            infer_submit_behavior(&agent, Some("__shell__")),
            SubmitBehavior::DoubleEnter
        );
    }

    #[test]
    fn pane_metadata_shell_sentinel_uses_resolved_shell_command() {
        let mut agent = test_agent_with_id("opus", AgentMode::Shell, None);
        agent.shell_command =
            Some("ssh -t root@example 'cd /root/kira && claude --model opus'".to_string());
        assert_eq!(
            infer_submit_behavior(&agent, Some("__shell__")),
            SubmitBehavior::DoubleEnter
        );
    }

    #[test]
    fn pane_metadata_shell_sentinel_keeps_generic_shell_single_enter() {
        let mut agent = test_agent_with_id("worker", AgentMode::Shell, None);
        agent.shell_command = Some("ssh -t root@example 'cd /root/kira && bash'".to_string());
        assert_eq!(
            infer_submit_behavior(&agent, Some("__shell__")),
            SubmitBehavior::SingleEnter
        );
    }
}
