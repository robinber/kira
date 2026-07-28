//! Shape and identifier validation for project/global config.

use std::collections::{BTreeMap, BTreeSet};

use super::super::error::ConfigError;
use super::super::model::{AgentMode, GlobalConfig, ProjectFile};
use super::agents::build_template_map;

type Result<T> = std::result::Result<T, ConfigError>;

/// Non-whitespace characters rejected in identifiers that end up in tmux
/// session names or target syntax (`session:window.pane`). All Unicode
/// whitespace is rejected separately because tmux option reads are trimmed.
const FORBIDDEN_IDENTIFIER_CHARS: &[char] = &[':', '.'];

pub(super) fn validate_identifier(kind: &'static str, id: &str) -> Result<()> {
    if let Some(ch) = id
        .chars()
        .find(|ch| ch.is_whitespace() || FORBIDDEN_IDENTIFIER_CHARS.contains(ch))
    {
        return Err(ConfigError::InvalidIdentifierChar {
            kind,
            id: id.to_string(),
            ch,
        });
    }
    Ok(())
}

pub(crate) fn validate_main_pane_ratio(ratio: u8) -> Result<()> {
    if (30..=70).contains(&ratio) {
        Ok(())
    } else {
        Err(ConfigError::MainPaneRatioOutOfRange)
    }
}

pub(crate) fn validate_global_config(global: &GlobalConfig) -> Result<()> {
    validate_main_pane_ratio(global.main_pane_ratio)?;

    let _ = build_template_map(&global.agent_templates)?;
    Ok(())
}

pub(super) fn validate_project_shape(project: &ProjectFile) -> Result<()> {
    if project.id.trim().is_empty() {
        return Err(ConfigError::EmptyProjectId);
    }
    validate_identifier("project id", &project.id)?;
    if project.root.trim().is_empty() {
        return Err(ConfigError::EmptyProjectRoot);
    }
    if project.agents.is_empty() {
        return Err(ConfigError::NoAgents);
    }
    for agent in &project.agents {
        if agent.id.trim().is_empty() {
            return Err(ConfigError::EmptyAgentId);
        }
        validate_identifier("agent id", &agent.id)?;
    }

    Ok(())
}

pub(super) fn validate_groups(
    groups: &BTreeMap<String, Vec<String>>,
    known_agents: &BTreeSet<String>,
) -> Result<()> {
    for (group_name, members) in groups {
        if group_name.trim().is_empty() {
            return Err(ConfigError::EmptyGroupName);
        }
        if members.is_empty() {
            return Err(ConfigError::EmptyGroup {
                group: group_name.clone(),
            });
        }
        let mut seen = BTreeSet::new();
        for member in members {
            if !seen.insert(member) {
                return Err(ConfigError::DuplicateAgentInGroup {
                    group: group_name.clone(),
                    agent: member.clone(),
                });
            }
            if !known_agents.contains(member) {
                return Err(ConfigError::UnknownAgentInGroup {
                    group: group_name.clone(),
                    agent: member.clone(),
                });
            }
        }
    }
    Ok(())
}

pub(super) fn validate_agent(
    agent_id: &str,
    mode: AgentMode,
    command: Option<&str>,
    shell_command: Option<&str>,
    args: &[String],
) -> Result<()> {
    match mode {
        AgentMode::Direct if command.is_none_or(str::is_empty) => {
            Err(ConfigError::MissingCommand {
                agent_id: agent_id.to_string(),
            })
        }
        AgentMode::Shell if shell_command.is_none_or(str::is_empty) => {
            Err(ConfigError::MissingShellCommand {
                agent_id: agent_id.to_string(),
            })
        }
        // Launch only passes args in direct mode; rejecting here keeps config
        // honest instead of silently ignoring shell-mode args.
        AgentMode::Shell if !args.is_empty() => Err(ConfigError::ShellArgsNotSupported {
            agent_id: agent_id.to_string(),
        }),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::error::ConfigError;
    use crate::config::model::AgentMode;
    use crate::test_support::TestResultExt;

    #[test]
    fn forbidden_identifier_chars_are_rejected() {
        for (id, expected_ch) in [
            ("a:b", ':'),
            ("a.b", '.'),
            ("a\tb", '\t'),
            ("a\nb", '\n'),
            ("a\rb", '\r'),
            // Padded ids round-trip through trimmed tmux options and would
            // report permanent drift.
            (" alpha", ' '),
            ("a b", ' '),
            ("a ", ' '),
            ("a\u{00a0}", '\u{00a0}'),
        ] {
            let error = validate_identifier("agent id", id)
                .err_or_panic("forbidden_identifier_chars_are_rejected: expected Err");
            let ConfigError::InvalidIdentifierChar { kind, id: got, ch } = error else {
                panic!("expected InvalidIdentifierChar for {id:?}");
            };
            assert_eq!(kind, "agent id");
            assert_eq!(got, id);
            assert_eq!(ch, expected_ch);
        }

        validate_identifier("agent id", "plain-id_09")
            .or_panic("forbidden_identifier_chars_are_rejected");
    }

    #[test]
    fn shell_mode_rejects_nonempty_args() {
        let error = validate_agent(
            "worker",
            AgentMode::Shell,
            None,
            Some("npm test"),
            &["--watch".to_string()],
        )
        .err_or_panic("shell_mode_rejects_nonempty_args: expected Err");
        assert!(matches!(
            error,
            ConfigError::ShellArgsNotSupported { agent_id } if agent_id == "worker"
        ));
    }

    #[test]
    fn shell_mode_allows_empty_args() {
        validate_agent("worker", AgentMode::Shell, None, Some("npm test"), &[])
            .or_panic("shell_mode_allows_empty_args");
    }

    #[test]
    fn direct_mode_allows_args() {
        validate_agent(
            "coder",
            AgentMode::Direct,
            Some("codex"),
            None,
            &["--full-auto".to_string()],
        )
        .or_panic("direct_mode_allows_args");
    }
}
