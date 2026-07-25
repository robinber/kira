//! Human-readable and JSON stdout formatting for CLI commands.

use std::io::{self, Write};

use anyhow::{Context, Result};
use serde::Serialize;

use crate::model::{AgentInfo, AgentRunState, AgentsOutput, ProjectStatus, ProjectSummary};

/// Write to stdout, mapping I/O failures (including broken pipes) into
/// `anyhow`.
fn write_stdout(f: impl FnOnce(&mut dyn Write) -> io::Result<()>) -> Result<()> {
    let mut out = io::stdout().lock();
    f(&mut out).context("failed to write to stdout")
}

/// Print any `--json` payload with one shared policy: compact single-line
/// JSON on stdout.
pub(crate) fn print_json<T: Serialize>(value: &T) -> Result<()> {
    let payload = serde_json::to_string(value)?;
    write_stdout(|out| {
        writeln!(out, "{payload}")?;
        Ok(())
    })
}

pub(crate) fn print_list(summaries: &[ProjectSummary], json: bool) -> Result<()> {
    if json {
        print_json(&summaries)?;
    } else {
        write_stdout(|out| {
            for row in summaries {
                writeln!(out, "{}", list_line(row))?;
            }
            Ok(())
        })?;
    }

    Ok(())
}

/// One text row of `list`: id/profile, name, state, agent count, root.
///
/// Config failures append path + error on following indented lines so text
/// mode stays consistent with the JSON `path` / `error` fields.
fn list_line(row: &ProjectSummary) -> String {
    let primary = format!(
        "{:<24} {:<20} {:<12} {:>2} agents  {}",
        display_id(&row.id, &row.profile_id),
        if row.name.is_empty() { "-" } else { &row.name },
        row.state,
        row.agent_count,
        if row.root.is_empty() { "-" } else { &row.root },
    );
    match (&row.path, &row.error) {
        (Some(path), Some(error)) => format!("{primary}\n  path:  {path}\n  error: {error}"),
        (Some(path), None) => format!("{primary}\n  path:  {path}"),
        (None, Some(error)) => format!("{primary}\n  error: {error}"),
        (None, None) => primary,
    }
}

pub(crate) fn print_status(status: &ProjectStatus, json: bool) -> Result<()> {
    if json {
        print_json(status)?;
    } else {
        write_stdout(|out| {
            writeln!(out, "Project: {} ({})", status.name, status.id)?;
            if status.profile_id != "default" {
                writeln!(out, "Profile: {}", status.profile_id)?;
            }
            writeln!(out, "Root:    {}", status.root)?;
            writeln!(out, "State:   {}", status.state)?;
            writeln!(out)?;
            for agent in &status.agents {
                writeln!(
                    out,
                    "  {:<28} {}",
                    agent_display_name(&agent.id, agent.label.as_deref()),
                    agent.state
                )?;
            }
            Ok(())
        })?;
    }

    Ok(())
}

pub(crate) fn print_agents_table(output: &AgentsOutput) -> Result<()> {
    write_stdout(|out| {
        write!(out, "Project: {}", output.project)?;
        if let Some(ref profile) = output.profile {
            write!(out, "  (profile: {profile})")?;
        }
        writeln!(out)?;
        writeln!(out)?;
        writeln!(
            out,
            "{:<28} {:<10} {:<10} {:<22} GROUPS",
            "AGENT", "COMMAND", "STATE", "CAPABILITIES"
        )?;
        writeln!(out, "{}", "\u{2500}".repeat(80))?;
        for agent in &output.agents {
            let caps = agent.capabilities.join(", ");
            let groups = agent.groups.join(", ");
            writeln!(
                out,
                "{:<28} {:<10} {:<10} {:<22} {}",
                agent_display_name(&agent.id, Some(&agent.label)),
                agent.command,
                agent.state,
                caps,
                groups,
            )?;
        }
        Ok(())
    })
}

#[derive(Debug, Serialize)]
pub(crate) struct AgentCapabilitiesOutput {
    pub agent: String,
    pub label: String,
    pub capabilities: Vec<String>,
    pub state: AgentRunState,
}

impl From<&AgentInfo> for AgentCapabilitiesOutput {
    fn from(agent: &AgentInfo) -> Self {
        Self {
            agent: agent.id.clone(),
            label: agent.label.clone(),
            capabilities: agent.capabilities.clone(),
            state: agent.state,
        }
    }
}

pub(crate) fn print_agent_capabilities(agent: &AgentInfo) -> Result<()> {
    write_stdout(|out| {
        writeln!(
            out,
            "Agent: {}",
            agent_display_name(&agent.id, Some(&agent.label))
        )?;
        writeln!(out, "State: {}", agent.state)?;
        writeln!(
            out,
            "Capabilities: {}",
            if agent.capabilities.is_empty() {
                "(none)".to_string()
            } else {
                agent.capabilities.join(", ")
            }
        )?;
        Ok(())
    })
}

#[derive(Debug, Serialize)]
pub(crate) struct GroupMemberOutput {
    pub id: String,
    pub state: AgentRunState,
}

#[derive(Debug, Serialize)]
pub(crate) struct GroupOutput {
    pub group: String,
    pub members: Vec<GroupMemberOutput>,
}

impl GroupOutput {
    pub(crate) fn new(group_name: &str, members: &[&AgentInfo]) -> Self {
        Self {
            group: group_name.to_string(),
            members: members
                .iter()
                .map(|a| GroupMemberOutput {
                    id: a.id.clone(),
                    state: a.state,
                })
                .collect(),
        }
    }
}

pub(crate) fn print_group(group_name: &str, members: &[&AgentInfo]) -> Result<()> {
    write_stdout(|out| {
        writeln!(out, "Group: {group_name}")?;
        for agent in members {
            writeln!(
                out,
                "  {:<28} {}",
                agent_display_name(&agent.id, Some(&agent.label)),
                agent.state
            )?;
        }
        Ok(())
    })
}

/// Print captured pane text on stdout, guaranteeing a trailing newline.
pub(crate) fn print_pane_text(output: &str) -> Result<()> {
    write_stdout(|out| {
        write!(out, "{output}")?;
        if !output.ends_with('\n') {
            writeln!(out)?;
        }
        Ok(())
    })
}

fn display_id(project_id: &str, profile_id: &str) -> String {
    format!("{project_id}/{profile_id}")
}

/// Text display for an agent: `id` alone when the label matches, otherwise
/// `id (label)` so config labels are visible without losing the stable id.
fn agent_display_name(id: &str, label: Option<&str>) -> String {
    match label {
        Some(label) if label != id => format!("{id} ({label})"),
        _ => id.to_string(),
    }
}

/// True when `error` is (or wraps) a broken stdout/stderr pipe.
///
/// The binary maps this to exit 0 so pipelines like `kira-mux list | head`
/// do not look like hard failures when the reader closes early.
#[must_use]
pub fn is_broken_pipe(error: &anyhow::Error) -> bool {
    for cause in error.chain() {
        if let Some(io_error) = cause.downcast_ref::<io::Error>()
            && io_error.kind() == io::ErrorKind::BrokenPipe
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::{agent_display_name, display_id, is_broken_pipe, list_line, print_pane_text};
    use crate::model::{ProjectState, ProjectSummary};

    #[test]
    fn agent_display_name_omits_redundant_label() {
        assert_eq!(agent_display_name("alpha", Some("alpha")), "alpha");
        assert_eq!(agent_display_name("alpha", None), "alpha");
    }

    #[test]
    fn agent_display_name_includes_distinct_label() {
        assert_eq!(agent_display_name("alpha", Some("Coder")), "alpha (Coder)");
    }

    #[test]
    fn display_id_joins_project_and_profile() {
        assert_eq!(display_id("demo", "default"), "demo/default");
        assert_eq!(display_id("demo", "pool-1"), "demo/pool-1");
    }

    #[test]
    fn list_line_includes_project_name() {
        let line = list_line(&ProjectSummary {
            id: "my-app".to_string(),
            profile_id: "default".to_string(),
            name: "My App".to_string(),
            root: "/tmp/demo".to_string(),
            state: ProjectState::Running,
            agent_count: 2,
            path: None,
            error: None,
        });

        assert!(line.contains("my-app/default"), "got: {line}");
        assert!(line.contains("My App"), "got: {line}");
        assert!(line.contains("running"), "got: {line}");
        assert!(line.contains("2 agents  /tmp/demo"), "got: {line}");
    }

    #[test]
    fn list_line_surfaces_config_error_details() {
        let line = list_line(&ProjectSummary {
            id: "broken".to_string(),
            profile_id: "default".to_string(),
            name: String::new(),
            root: String::new(),
            state: ProjectState::ConfigError,
            agent_count: 0,
            path: Some("/cfg/projects/broken.toml".to_string()),
            error: Some("unknown field `nope`".to_string()),
        });

        assert!(line.contains("config_error"), "got: {line}");
        assert!(
            line.contains("path:  /cfg/projects/broken.toml"),
            "got: {line}"
        );
        assert!(line.contains("error: unknown field `nope`"), "got: {line}");
    }

    #[test]
    fn is_broken_pipe_detects_io_broken_pipe() {
        let err = anyhow::Error::new(io::Error::new(io::ErrorKind::BrokenPipe, "pipe"));
        assert!(is_broken_pipe(&err));
        let wrapped = err.context("failed to write to stdout");
        assert!(is_broken_pipe(&wrapped));
    }

    #[test]
    fn is_broken_pipe_rejects_other_errors() {
        let err = anyhow::Error::new(io::Error::other("nope"));
        assert!(!is_broken_pipe(&err));
    }

    #[test]
    fn print_pane_text_accepts_trailing_newline_input() {
        // Smoke: does not panic; success path only (real stdout).
        if let Err(error) = print_pane_text("hello\n") {
            panic!("write pane text: {error}");
        }
    }
}
