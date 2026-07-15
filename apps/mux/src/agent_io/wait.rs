//! Post-send wait: poll pane captures until the agent's output settles.
//!
//! `send --wait` treats pane stillness as a proxy for "the agent finished",
//! in two phases: first the pane must *change* from the post-submit baseline
//! (the response started), then it must stop changing for a full stability
//! window. Stability without observed activity never counts as success — a
//! pane that sits quiet while the model thinks must not be reported as done
//! with only the prompt echo captured.

use std::time::{Duration, Instant};

use anyhow::Result;

use super::resolve::resolve_managed_pane;
use crate::error::KiraMuxError;
use crate::model::ResolvedProject;
use crate::tmux::TmuxAdapter;

/// Lines of pane history compared per poll and returned on success. Sized
/// for a full agent reply; intentionally a constant, not CLI surface.
const WAIT_CAPTURE_LINES: usize = 200;

/// Tuning for the stability poll. Production uses [`WaitOptions::default`];
/// tests inject tiny durations so timeout paths run in milliseconds.
pub(crate) struct WaitOptions {
    /// Delay between pane captures.
    pub(crate) poll_interval: Duration,
    /// How long the pane must stay unchanged (after activity) to be stable.
    pub(crate) stability_window: Duration,
    /// Hard cap on the whole wait. Kept below typical caller-side tool
    /// timeouts so kira-mux fails first with a useful error.
    pub(crate) hard_timeout: Duration,
}

impl Default for WaitOptions {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_millis(500),
            stability_window: Duration::from_secs(3),
            hard_timeout: Duration::from_mins(10),
        }
    }
}

/// Block until `agent_id`'s pane shows output activity and then stabilizes;
/// return the final capture (same text shape as `capture`).
///
/// # Errors
///
/// Fails fast with the same errors as `send` (absent session, drift, unknown
/// agent, dead pane). During the wait: [`KiraMuxError::PaneDiedDuringWait`]
/// when the pane dies — a dead pane's frozen content must never read as
/// "stable" — and [`KiraMuxError::WaitTimeout`] (carrying the last capture)
/// when the hard timeout elapses first.
pub(crate) fn wait_for_stable_output(
    tmux: &dyn TmuxAdapter,
    project: &ResolvedProject,
    agent_id: &str,
    options: &WaitOptions,
) -> Result<String> {
    let (pane, _agent, _topology) = resolve_managed_pane(tmux, project, agent_id)?;
    if pane.pane_dead {
        return Err(KiraMuxError::DeadPane(agent_id.to_string()).into());
    }

    let deadline = Instant::now() + options.hard_timeout;
    // Baseline after the full submit sequence: prompt echo is already
    // rendered, so any change from here on is response activity.
    let mut last = tmux.capture_pane(&pane.pane_id, WAIT_CAPTURE_LINES)?;
    let mut activity_seen = false;
    let mut last_change = Instant::now();

    loop {
        let now = Instant::now();
        if now >= deadline {
            return Err(KiraMuxError::WaitTimeout {
                agent_id: agent_id.to_string(),
                last_capture: last,
            }
            .into());
        }
        std::thread::sleep(options.poll_interval.min(deadline - now));

        if pane_is_dead(tmux, &pane.pane_id)? {
            return Err(KiraMuxError::PaneDiedDuringWait(agent_id.to_string()).into());
        }

        let current = tmux.capture_pane(&pane.pane_id, WAIT_CAPTURE_LINES)?;
        if current != last {
            last = current;
            activity_seen = true;
            last_change = Instant::now();
        } else if activity_seen && last_change.elapsed() >= options.stability_window {
            return Ok(last);
        }
    }
}

/// A vanished pane (killed window) counts as dead; a killed session already
/// surfaces as a typed error from `list_panes`.
fn pane_is_dead(tmux: &dyn TmuxAdapter, pane_id: &str) -> Result<bool> {
    let panes = tmux.list_panes(pane_id)?;
    Ok(panes
        .iter()
        .find(|pane| pane.pane_id == pane_id)
        .is_none_or(|pane| pane.pane_dead))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestResultExt;

    /// Millisecond-scale knobs so timeout paths stay fast; the stability
    /// window spans several polls, mirroring the production ratio.
    fn fast_options() -> WaitOptions {
        WaitOptions {
            poll_interval: Duration::from_millis(1),
            stability_window: Duration::from_millis(20),
            hard_timeout: Duration::from_millis(250),
        }
    }

    #[test]
    fn wait_returns_final_capture_once_pane_stabilizes_after_activity() {
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        crate::test_support::setup_healthy_session(&fake, &project);
        // Baseline pops the first entry; the pane then streams and freezes.
        fake.queue_pane_contents(
            "%0",
            &[
                "prompt echo",
                "prompt echo\nthinking...",
                "prompt echo\nthinking...\nanswer: 42",
            ],
        );

        let output = wait_for_stable_output(&fake, &project, "alpha", &fast_options()).or_panic();

        assert_eq!(output, "prompt echo\nthinking...\nanswer: 42");
    }

    #[test]
    fn wait_times_out_when_pane_never_changes() {
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        crate::test_support::setup_healthy_session(&fake, &project);
        // Quiet from the start: only the prompt echo, no response activity.
        // Stability without activity must not be reported as success.
        fake.set_pane_content("%0", "prompt echo");

        let err = wait_for_stable_output(&fake, &project, "alpha", &fast_options()).err_or_panic();

        assert!(
            matches!(
                err.downcast_ref::<KiraMuxError>(),
                Some(KiraMuxError::WaitTimeout { agent_id, last_capture })
                    if agent_id == "alpha" && last_capture == "prompt echo"
            ),
            "expected WaitTimeout carrying the last capture, got: {err}"
        );
    }

    #[test]
    fn wait_times_out_while_pane_keeps_changing() {
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        crate::test_support::setup_healthy_session(&fake, &project);
        let frames: Vec<String> = (0..2000).map(|i| format!("streaming line {i}")).collect();
        let frame_refs: Vec<&str> = frames.iter().map(String::as_str).collect();
        fake.queue_pane_contents("%0", &frame_refs);

        let err = wait_for_stable_output(&fake, &project, "alpha", &fast_options()).err_or_panic();

        assert!(
            matches!(
                err.downcast_ref::<KiraMuxError>(),
                Some(KiraMuxError::WaitTimeout { agent_id, .. }) if agent_id == "alpha"
            ),
            "expected WaitTimeout for a never-stable pane, got: {err}"
        );
    }

    #[test]
    fn wait_fails_fast_on_pane_dead_at_start() {
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        crate::test_support::setup_session_with_dead_panes(&fake, &project, &[0]);

        let err = wait_for_stable_output(&fake, &project, "alpha", &fast_options()).err_or_panic();

        assert!(matches!(
            err.downcast_ref::<KiraMuxError>(),
            Some(KiraMuxError::DeadPane(id)) if id == "alpha"
        ));
    }

    #[test]
    fn wait_fails_when_pane_dies_mid_wait() {
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        crate::test_support::setup_healthy_session(&fake, &project);
        fake.queue_pane_contents("%0", &["prompt echo", "streaming...", "str"]);
        fake.set_pane_dies_after_captures("%0", 3);
        // Success and timeout are both out of reach: death must win.
        let options = WaitOptions {
            poll_interval: Duration::from_millis(1),
            stability_window: Duration::from_secs(30),
            hard_timeout: Duration::from_secs(30),
        };

        let err = wait_for_stable_output(&fake, &project, "alpha", &options).err_or_panic();

        assert!(
            matches!(
                err.downcast_ref::<KiraMuxError>(),
                Some(KiraMuxError::PaneDiedDuringWait(id)) if id == "alpha"
            ),
            "a frozen dead pane must fail, not read as stable, got: {err}"
        );
    }

    #[test]
    fn wait_absent_session_fails() {
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();

        let err = wait_for_stable_output(&fake, &project, "alpha", &fast_options()).err_or_panic();

        assert!(matches!(
            err.downcast_ref::<KiraMuxError>(),
            Some(KiraMuxError::SessionAbsent)
        ));
    }
}
