use anyhow::Result;
use serde::Serialize;

use super::resolve::resolve_managed_pane;
use crate::model::ResolvedProject;
use crate::tmux::TmuxAdapter;

#[derive(Debug, Serialize)]
pub(crate) struct PaneCapture {
    pub project_id: String,
    pub profile_id: String,
    pub agent_id: String,
    pub pane_id: String,
    pub pane_dead: bool,
    pub pane_dead_status: Option<i32>,
    /// Requested line limit, not the number of lines actually returned.
    pub lines: usize,
    pub output: String,
}

pub(crate) fn capture_output(
    tmux: &dyn TmuxAdapter,
    project: &ResolvedProject,
    agent_id: &str,
    lines: usize,
) -> Result<PaneCapture> {
    let (pane, _agent) = resolve_managed_pane(tmux, project, agent_id)?;
    let output = tmux.capture_pane(&pane.pane_id, lines)?;

    Ok(PaneCapture {
        project_id: project.id.clone(),
        profile_id: project.profile_id.clone(),
        agent_id: agent_id.to_string(),
        pane_id: pane.pane_id,
        pane_dead: pane.pane_dead,
        pane_dead_status: pane.pane_dead_status,
        lines,
        output,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{err, ok};

    #[test]
    fn capture_output_returns_content() {
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        crate::test_support::setup_healthy_session(&fake, &project);
        fake.set_pane_content("%0", "some output here");

        let capture = ok(
            capture_output(&fake, &project, "alpha", 30),
            "capture_output should succeed for a healthy pane",
        );
        assert_eq!(capture.agent_id, "alpha");
        assert_eq!(capture.pane_id, "%0");
        assert_eq!(capture.output, "some output here");
        assert_eq!(capture.project_id, "test");
        assert!(!capture.pane_dead);
    }

    #[test]
    fn capture_output_dead_pane_allowed() {
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        crate::test_support::setup_session_with_dead_panes(&fake, &project, &[0]);
        fake.set_pane_content("%0", "dead pane output");

        let capture = ok(
            capture_output(&fake, &project, "alpha", 30),
            "capture_output should succeed for a dead pane",
        );
        assert!(capture.pane_dead);
        assert_eq!(capture.output, "dead pane output");
    }

    #[test]
    fn capture_output_absent_session_fails() {
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        let err = err(
            capture_output(&fake, &project, "alpha", 30),
            "capture_output should fail when the session is absent",
        );
        assert!(matches!(
            err.downcast_ref::<crate::error::KiraMuxError>(),
            Some(crate::error::KiraMuxError::SessionAbsent)
        ));
    }

    #[test]
    fn capture_output_respects_line_limit() {
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        crate::test_support::setup_healthy_session(&fake, &project);

        let content = (1..=50)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        fake.set_pane_content("%0", &content);

        let capture = ok(
            capture_output(&fake, &project, "alpha", 5),
            "capture_output should succeed with a line limit",
        );
        let lines: Vec<&str> = capture.output.lines().collect();
        assert_eq!(lines.len(), 5, "expected 5 lines, got: {lines:?}");
        assert_eq!(lines[0], "line 46");
        assert_eq!(lines[4], "line 50");
    }
}
