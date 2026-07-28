//! Human-readable and JSON stdout formatting for CLI commands.

use std::io::{self, Write};

use anyhow::Result;
use serde::Serialize;
use thiserror::Error;

use crate::agent_io::PaneCapture;
use crate::model::{
    AgentCapabilitiesOutput, AgentInfo, AgentsOutput, GroupOutput, ProjectStatus, ProjectSummary,
};

/// Stdout's reader closed the pipe (e.g. `kira-mux list | head`).
///
/// Only minted by this module when writing to process stdout. Other
/// `BrokenPipe` sources (tmux child stdin, etc.) must not become exit 0.
#[derive(Debug, Error)]
#[error("stdout closed by reader")]
pub struct StdoutClosed;

/// Write through `out`, mapping only *this* write's broken pipe to
/// [`StdoutClosed`].
fn write_formatted(
    out: &mut dyn Write,
    f: impl FnOnce(&mut dyn Write) -> io::Result<()>,
) -> Result<()> {
    match f(out).and_then(|()| out.flush()) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Err(StdoutClosed.into()),
        Err(error) => Err(anyhow::Error::new(error).context("failed to write to stdout")),
    }
}

fn write_stdout(f: impl FnOnce(&mut dyn Write) -> io::Result<()>) -> Result<()> {
    let mut out = io::stdout().lock();
    write_formatted(&mut out, f)
}

/// Print any `--json` payload with one shared policy: compact single-line
/// JSON on stdout. Private on purpose: every command reaches it through
/// its `print_*` view function, so the human-vs-JSON branch point lives in
/// this module only.
fn print_json<T: Serialize>(value: &T) -> Result<()> {
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
        write_stdout(|out| write_list_text(out, summaries))?;
    }
    Ok(())
}

fn write_list_text(out: &mut dyn Write, summaries: &[ProjectSummary]) -> io::Result<()> {
    for row in summaries {
        writeln!(out, "{}", list_line(row))?;
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
        write_stdout(|out| write_status_text(out, status))?;
    }
    Ok(())
}

fn write_status_text(out: &mut dyn Write, status: &ProjectStatus) -> io::Result<()> {
    writeln!(out, "Project: {} ({})", status.name, status.id)?;
    if status.profile_id != crate::config::DEFAULT_PROFILE_ID {
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
}

pub(crate) fn print_agents(output: &AgentsOutput, json: bool) -> Result<()> {
    if json {
        print_json(output)
    } else {
        print_agents_table(output)
    }
}

fn print_agents_table(output: &AgentsOutput) -> Result<()> {
    write_stdout(|out| write_agents_table(out, output))
}

fn write_agents_table(out: &mut dyn Write, output: &AgentsOutput) -> io::Result<()> {
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
}

pub(crate) fn print_agent_capabilities(agent: &AgentInfo, json: bool) -> Result<()> {
    if json {
        print_json(&AgentCapabilitiesOutput::from(agent))
    } else {
        write_stdout(|out| write_agent_capabilities(out, agent))
    }
}

fn write_agent_capabilities(out: &mut dyn Write, agent: &AgentInfo) -> io::Result<()> {
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
}

pub(crate) fn print_group(group_name: &str, members: &[&AgentInfo], json: bool) -> Result<()> {
    if json {
        print_json(&GroupOutput::new(group_name, members))
    } else {
        write_stdout(|out| write_group(out, group_name, members))
    }
}

/// Print a pane capture: the full JSON payload, or just the pane text.
pub(crate) fn print_capture(capture: &PaneCapture, json: bool) -> Result<()> {
    if json {
        print_json(capture)
    } else {
        print_pane_text(&capture.output)
    }
}

fn write_group(out: &mut dyn Write, group_name: &str, members: &[&AgentInfo]) -> io::Result<()> {
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
}

/// Print captured pane text on stdout, guaranteeing a trailing newline.
pub(crate) fn print_pane_text(output: &str) -> Result<()> {
    write_stdout(|out| write_pane_text(out, output))
}

fn write_pane_text(out: &mut dyn Write, output: &str) -> io::Result<()> {
    write!(out, "{output}")?;
    if !output.ends_with('\n') {
        writeln!(out)?;
    }
    Ok(())
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

/// True when `error` is (or wraps) a [`StdoutClosed`] from this module.
///
/// The binary maps this to exit 0 so pipelines like `kira-mux list | head`
/// do not look like hard failures when the reader closes early.
#[must_use]
pub fn is_stdout_closed(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.downcast_ref::<StdoutClosed>().is_some())
}

#[cfg(test)]
mod tests {
    use std::io::{self, Write};

    use super::{
        StdoutClosed, agent_display_name, display_id, is_stdout_closed, list_line, write_formatted,
        write_pane_text,
    };
    use crate::model::{ProjectState, ProjectSummary};

    struct FailingWriter {
        kind: io::ErrorKind,
    }

    impl Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(self.kind, "injected write failure"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct CountingWriter {
        bytes: Vec<u8>,
        fail_after: usize,
    }

    impl Write for CountingWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if self.bytes.len() >= self.fail_after {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed"));
            }
            self.bytes.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

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
    fn write_formatted_maps_broken_pipe_to_stdout_closed() {
        let err = match write_formatted(
            &mut FailingWriter {
                kind: io::ErrorKind::BrokenPipe,
            },
            |w| write!(w, "x"),
        ) {
            Ok(()) => panic!("expected error"),
            Err(error) => error,
        };
        assert!(is_stdout_closed(&err), "got {err:?}");
        assert!(err.downcast_ref::<StdoutClosed>().is_some());
    }

    #[test]
    fn write_formatted_preserves_other_io_errors() {
        let err = match write_formatted(
            &mut FailingWriter {
                kind: io::ErrorKind::PermissionDenied,
            },
            |w| write!(w, "x"),
        ) {
            Ok(()) => panic!("expected error"),
            Err(error) => error,
        };
        assert!(!is_stdout_closed(&err));
        assert!(err.to_string().contains("failed to write to stdout"));
    }

    #[test]
    fn is_stdout_closed_ignores_unrelated_broken_pipe() {
        let err = anyhow::Error::new(io::Error::new(io::ErrorKind::BrokenPipe, "tmux stdin"))
            .context("failed to write to tmux stdin");
        assert!(!is_stdout_closed(&err));
    }

    #[test]
    fn write_pane_text_adds_trailing_newline_when_missing() {
        let mut buf = Vec::new();
        write_pane_text(&mut buf, "hello").expect_ok();
        assert_eq!(buf, b"hello\n");
    }

    #[test]
    fn write_pane_text_preserves_existing_trailing_newline() {
        let mut buf = Vec::new();
        write_pane_text(&mut buf, "hello\n").expect_ok();
        assert_eq!(buf, b"hello\n");
    }

    trait ExpectOk {
        fn expect_ok(self);
    }

    impl ExpectOk for io::Result<()> {
        fn expect_ok(self) {
            if let Err(error) = self {
                panic!("io error: {error}");
            }
        }
    }

    #[test]
    fn write_formatted_maps_mid_stream_broken_pipe() {
        let mut out = CountingWriter {
            bytes: Vec::new(),
            fail_after: 0,
        };
        let err = match write_formatted(&mut out, |w| write_pane_text(w, "hello")) {
            Ok(()) => panic!("expected error"),
            Err(error) => error,
        };
        assert!(is_stdout_closed(&err));
    }
}
