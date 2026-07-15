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
    #[error("failed to parse config file {path}: {source}")]
    FileParse {
        /// Path that failed to parse.
        path: PathBuf,
        /// Underlying TOML parse error.
        source: toml::de::Error,
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
}
