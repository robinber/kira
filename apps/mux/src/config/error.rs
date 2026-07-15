use std::path::PathBuf;

use thiserror::Error;

/// Errors raised while reading, validating, or resolving configuration.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// Reading a config file from disk failed.
    #[error("failed to read config file {path}: {source}")]
    FileRead {
        /// Path that failed to read.
        path: PathBuf,
        /// Underlying I/O error.
        source: std::io::Error,
    },
    /// Parsing a TOML config file failed.
    ///
    /// `reason` is a value-free diagnostic with a location when available and
    /// never includes TOML source excerpts or values, which may contain
    /// secrets.
    #[error("failed to parse config file {path}: {reason}")]
    FileParse {
        /// Path that failed to parse.
        path: PathBuf,
        /// Safe diagnostic category plus line/column when known.
        reason: String,
    },
    /// Canonicalizing or expanding a path failed.
    #[error("failed to resolve path {path}: {source}")]
    PathResolution {
        /// Path that failed to resolve.
        path: PathBuf,
        /// Underlying I/O error.
        source: std::io::Error,
    },
    /// No project file matched the requested project ID.
    #[error("unknown project id: {0}")]
    UnknownProjectId(String),
    /// A config identifier contains a character that corrupts tmux target
    /// syntax or option formats.
    #[error("{kind} contains forbidden character {ch:?}: {id:?}")]
    InvalidIdentifierChar {
        /// Kind of identifier that contained the character.
        kind: &'static str,
        /// Offending identifier value.
        id: String,
        /// The forbidden character.
        ch: char,
    },
    /// The HOME directory could not be resolved from the environment.
    #[error("HOME is not set")]
    HomeDirUnavailable,
    /// A project ID was empty.
    #[error("project id cannot be empty")]
    EmptyProjectId,
    /// A project root was empty.
    #[error("project root cannot be empty")]
    EmptyProjectRoot,
    /// A project root was relative to the process working directory.
    ///
    /// Relative roots make session identity depend on where `kira-mux` is
    /// invoked, so the same config can target different tmux sessions.
    #[error(
        "project root must be absolute or start with ~/ (got {0:?}); \
         relative paths follow process CWD and break session identity"
    )]
    RelativeProjectRoot(String),
    /// A project defined no agents.
    #[error("project must define at least one agent")]
    NoAgents,
    /// The configured project root does not exist.
    #[error("project root does not exist: {0}")]
    ProjectRootNotFound(PathBuf),
    /// The configured project root is not a directory.
    #[error("project root is not a directory: {0}")]
    ProjectRootNotDirectory(PathBuf),
    /// `main_pane_ratio` falls outside the supported range.
    #[error("main_pane_ratio must be between 30 and 70")]
    MainPaneRatioOutOfRange,
    /// Two agents share the same ID.
    #[error("duplicate agent id: {0}")]
    DuplicateAgentId(String),
    /// An agent referenced an unknown template.
    #[error("unknown agent template: {0}")]
    UnknownTemplate(String),
    /// An agent template name was empty.
    #[error("agent template name cannot be empty")]
    EmptyTemplateName,
    /// Two agent templates share the same name.
    #[error("duplicate agent template: {0}")]
    DuplicateTemplate(String),
    /// A direct-mode agent omitted its command.
    #[error("agent {agent_id} requires a command in direct mode")]
    MissingCommand {
        /// Agent missing its direct command.
        agent_id: String,
    },
    /// A shell-mode agent omitted its shell command.
    #[error("agent {agent_id} requires a shell_command in shell mode")]
    MissingShellCommand {
        /// Agent missing its shell command.
        agent_id: String,
    },
    /// A shell-mode agent set `args`, which are only used in direct mode.
    #[error(
        "agent {agent_id}: args are not used in shell mode (fold flags into shell_command, or use mode = \"direct\")"
    )]
    ShellArgsNotSupported {
        /// Agent that set unused args.
        agent_id: String,
    },
    /// An environment placeholder referenced an unset variable.
    #[error("agent {agent_id} references missing environment variable {var_name}")]
    UnresolvedEnvVar {
        /// Agent containing the unresolved placeholder.
        agent_id: String,
        /// Missing environment variable name.
        var_name: String,
    },
    /// Multiple project files resolved to the same project ID.
    #[error("duplicate project id: {id} (in {path})")]
    DuplicateProjectId {
        /// Duplicate project ID.
        id: String,
        /// Path of the conflicting project file.
        path: PathBuf,
    },
    /// An agent ID was empty.
    #[error("agent id cannot be empty")]
    EmptyAgentId,
    /// A group name was empty.
    #[error("group name cannot be empty")]
    EmptyGroupName,
    /// A group defined no members.
    #[error("group {group:?} is empty")]
    EmptyGroup {
        /// Group name with no members.
        group: String,
    },
    /// A group listed the same agent more than once.
    #[error("group {group:?} contains duplicate agent {agent:?}")]
    DuplicateAgentInGroup {
        /// Group name with the duplicate member.
        group: String,
        /// Duplicate agent ID.
        agent: String,
    },
    /// A group referenced an unknown agent.
    #[error("group {group:?} references unknown agent {agent:?}")]
    UnknownAgentInGroup {
        /// Group name with the bad reference.
        group: String,
        /// Unknown agent ID.
        agent: String,
    },
    /// A config mixed top-level fields with `[profiles]`.
    #[error("project config mixes top-level workspace fields with [profiles]")]
    MixedConfigShape,
    /// A `[profiles]` table was present but empty.
    #[error("[profiles] is present but empty")]
    EmptyProfiles,
    /// The requested profile ID does not exist.
    #[error("unknown profile: {id}")]
    UnknownProfile {
        /// Unknown profile ID.
        id: String,
    },
    /// Multiple profiles exist and no profile was selected.
    #[error("project {project_id} requires a profile selection (available: {available}). Use --profile <id>", available = available.join(", "))]
    ProfileRequired {
        /// Project that requires explicit profile selection.
        project_id: String,
        /// Available profile IDs.
        available: Vec<String>,
    },
    /// An agent `cwd` field was empty.
    #[error("agent {agent_id}: cwd cannot be empty")]
    EmptyAgentCwd {
        /// Agent with the empty cwd.
        agent_id: String,
    },
    /// An agent `cwd` resolved outside the project root.
    #[error("agent {agent_id}: cwd escapes project root: {path}")]
    AgentCwdEscapesRoot {
        /// Agent with the escaping cwd.
        agent_id: String,
        /// Escaping resolved path.
        path: PathBuf,
    },
    /// An agent `cwd` path does not exist.
    #[error("agent {agent_id}: cwd does not exist: {path}")]
    AgentCwdNotFound {
        /// Agent with the missing cwd.
        agent_id: String,
        /// Missing resolved path.
        path: PathBuf,
    },
    /// An agent `cwd` path is not a directory.
    #[error("agent {agent_id}: cwd is not a directory: {path}")]
    AgentCwdNotDirectory {
        /// Agent with the non-directory cwd.
        agent_id: String,
        /// Non-directory resolved path.
        path: PathBuf,
    },
    /// One or more project files or profiles failed to load during discovery.
    ///
    /// Emitted by `list` after printing structured `config_error` rows so
    /// automation can fail without losing the per-entry diagnostics.
    #[error("{count} project config(s) failed to load (details in list output)")]
    ProjectConfigLoadFailures {
        /// Number of failed project files / profiles.
        count: usize,
    },
}

impl ConfigError {
    /// Build a [`Self::FileParse`] that is safe to log and print.
    ///
    /// The TOML crate's default `Display` embeds the offending source line.
    /// That can leak secrets from malformed literal env values into `list`,
    /// `list --json`, and default warn logs. This constructor keeps the
    /// location when available, without trusting parser messages that may
    /// embed literal values.
    pub(crate) fn file_parse(path: PathBuf, input: &str, error: &toml::de::Error) -> Self {
        Self::FileParse {
            path,
            reason: safe_toml_parse_reason(error, input),
        }
    }
}

/// Format a TOML parse error without source excerpts or literal values.
fn safe_toml_parse_reason(error: &toml::de::Error, input: &str) -> String {
    let message = "invalid TOML configuration";
    match error.span() {
        Some(span) if !input.is_empty() => {
            let (line, column) = line_column_at(input.as_bytes(), span.start);
            format!("{message} (at line {line}, column {column})")
        }
        _ => message.to_string(),
    }
}

/// Convert a byte offset into 1-based line and column (column counts chars).
fn line_column_at(input: &[u8], byte_offset: usize) -> (usize, usize) {
    if input.is_empty() {
        return (1, 1);
    }

    let safe_index = byte_offset.min(input.len().saturating_sub(1));
    let column_offset = byte_offset.saturating_sub(safe_index);

    let line_start = input[..safe_index]
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(0, |nl| nl + 1);

    let mut line = 1usize;
    for &b in &input[..line_start] {
        if b == b'\n' {
            line += 1;
        }
    }

    let column = std::str::from_utf8(&input[line_start..=safe_index])
        .map_or_else(|_| safe_index - line_start + 1, |s| s.chars().count())
        + column_offset;

    (line, column.max(1))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{ConfigError, line_column_at, safe_toml_parse_reason};

    const SENTINEL: &str = "super-secret-value-do-not-leak";

    #[test]
    fn safe_toml_parse_reason_omits_source_excerpt_and_secret() {
        let input = format!("env = {{ TOKEN = \"{SENTINEL}\n");
        let Err(error) = toml::from_str::<toml::Table>(&input) else {
            panic!("expected parse failure");
        };

        let raw = error.to_string();
        assert!(
            raw.contains(SENTINEL),
            "precondition: toml Display must embed the source line, got: {raw}"
        );

        let reason = safe_toml_parse_reason(&error, &input);
        assert!(
            !reason.contains(SENTINEL),
            "safe reason must not include the secret: {reason}"
        );
        assert!(
            !reason.contains('|'),
            "safe reason must not look like a source excerpt: {reason}"
        );
        assert!(
            reason.contains("line") && reason.contains("column"),
            "location should be preserved: {reason}"
        );

        let display = ConfigError::file_parse("/tmp/leak.toml".into(), &input, &error).to_string();
        assert!(
            !display.contains(SENTINEL),
            "FileParse Display must not leak secret: {display}"
        );
        assert!(
            display.contains("failed to parse config file"),
            "got: {display}"
        );
    }

    #[test]
    fn safe_toml_parse_reason_omits_literal_embedded_in_message() {
        const NUMERIC_SENTINEL: &str = "987654321012345678";

        let input = format!("env = {{ TOKEN = {NUMERIC_SENTINEL} }}");
        let Err(error) = toml::from_str::<BTreeMap<String, BTreeMap<String, String>>>(&input)
        else {
            panic!("expected env value type failure");
        };

        let raw_message = error.message();
        assert!(
            raw_message.contains(NUMERIC_SENTINEL),
            "precondition: TOML message must embed the invalid value, got: {raw_message}"
        );

        let reason = safe_toml_parse_reason(&error, &input);
        assert!(
            !reason.contains(NUMERIC_SENTINEL),
            "safe reason must not include the invalid value: {reason}"
        );
        assert!(
            reason.contains("invalid TOML configuration"),
            "diagnostic category should remain actionable: {reason}"
        );
        assert!(
            reason.contains("line") && reason.contains("column"),
            "location should be preserved: {reason}"
        );
    }

    #[test]
    fn line_column_at_counts_newlines() {
        let input = b"a\nbc\nd";
        assert_eq!(line_column_at(input, 0), (1, 1));
        assert_eq!(line_column_at(input, 2), (2, 1));
        assert_eq!(line_column_at(input, 4), (2, 3));
        assert_eq!(line_column_at(input, 5), (3, 1));
    }
}
