//! Post-send wait: poll pane captures until the agent's output settles.
//!
//! `send --wait` treats pane stillness as a proxy for "the agent finished",
//! in two phases: first the pane must *change* from the post-submit baseline
//! (the response started), then it must stop changing for a full stability
//! window. Stability without observed activity never counts as success — a
//! pane that sits quiet while the model thinks must not be reported as done
//! with only the prompt echo captured.

#[cfg(test)]
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::error::KiraMuxError;
use crate::tmux::{TmuxAdapter, TmuxError};

/// Lines of pane history compared per poll and returned on success. Sized
/// for a full agent reply; intentionally a constant, not CLI surface.
const WAIT_CAPTURE_LINES: usize = 200;

/// Tuning for the stability poll. Production uses [`WaitOptions::default`];
/// tests inject tiny durations (and optional virtual time) so timeout paths
/// run without wall-clock sleeps.
pub(crate) struct WaitOptions {
    /// Delay between pane captures.
    pub(crate) poll_interval: Duration,
    /// How long the pane must stay unchanged (after activity) to be stable.
    pub(crate) stability_window: Duration,
    /// Hard cap on the whole wait. Kept below typical caller-side tool
    /// timeouts so kira-mux fails first with a useful error.
    pub(crate) hard_timeout: Duration,
    /// Time source. Wall clock in production; virtual for deterministic tests.
    clock: WaitClock,
}

enum WaitClock {
    Wall,
    /// Each [`WaitOptions::sleep`] advances elapsed time without blocking.
    #[cfg(test)]
    Virtual(Mutex<Duration>),
}

impl Default for WaitOptions {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_millis(500),
            stability_window: Duration::from_secs(3),
            hard_timeout: Duration::from_mins(10),
            clock: WaitClock::Wall,
        }
    }
}

impl WaitOptions {
    /// Test knobs: millisecond-scale timings with a virtual clock so timeout
    /// and stability paths advance without real sleeps.
    #[cfg(test)]
    fn virtual_time(
        poll_interval: Duration,
        stability_window: Duration,
        hard_timeout: Duration,
    ) -> Self {
        Self {
            poll_interval,
            stability_window,
            hard_timeout,
            clock: WaitClock::Virtual(Mutex::new(Duration::ZERO)),
        }
    }

    fn elapsed(&self, wall_start: Instant) -> Duration {
        match &self.clock {
            WaitClock::Wall => wall_start.elapsed(),
            #[cfg(test)]
            WaitClock::Virtual(elapsed) => match elapsed.lock() {
                Ok(guard) => *guard,
                Err(poisoned) => *poisoned.into_inner(),
            },
        }
    }

    fn sleep(&self, duration: Duration) {
        match &self.clock {
            WaitClock::Wall => std::thread::sleep(duration),
            #[cfg(test)]
            WaitClock::Virtual(elapsed) => {
                let mut guard = match elapsed.lock() {
                    Ok(guard) => guard,
                    Err(poisoned) => poisoned.into_inner(),
                };
                *guard = guard.saturating_add(duration);
            }
        }
    }
}

/// Block until `pane_id` shows output activity and then stabilizes; return
/// the final capture (same text shape as `capture`).
///
/// Call this with the pane id already resolved by `send` so the baseline is
/// taken immediately after submit, without a second `inspect` round-trip.
///
/// # Errors
///
/// [`KiraMuxError::PaneDiedDuringWait`] when the pane is dead at entry or
/// dies/vanishes mid-wait (frozen dead content must never read as "stable").
/// [`KiraMuxError::WaitTimeout`] when the hard timeout elapses first.
pub(crate) fn wait_on_pane(
    tmux: &dyn TmuxAdapter,
    agent_id: &str,
    pane_id: &str,
    options: &WaitOptions,
) -> Result<String> {
    if pane_is_dead(tmux, pane_id)? {
        return Err(KiraMuxError::PaneDiedDuringWait(agent_id.to_string()).into());
    }

    let wall_start = Instant::now();
    // Baseline after the full submit sequence: prompt echo is already
    // rendered, so any change from here on is response activity.
    let mut last = capture_or_died(tmux, agent_id, pane_id)?;
    let mut activity_seen = false;
    let mut last_change = options.elapsed(wall_start);

    loop {
        let now = options.elapsed(wall_start);
        if now >= options.hard_timeout {
            return Err(KiraMuxError::WaitTimeout {
                agent_id: agent_id.to_string(),
                last_capture: last,
            }
            .into());
        }
        let remaining = options.hard_timeout.saturating_sub(now);
        options.sleep(options.poll_interval.min(remaining));

        if pane_is_dead(tmux, pane_id)? {
            return Err(KiraMuxError::PaneDiedDuringWait(agent_id.to_string()).into());
        }

        let current = capture_or_died(tmux, agent_id, pane_id)?;
        if current != last {
            last = current;
            activity_seen = true;
            last_change = options.elapsed(wall_start);
        } else if activity_seen
            && options.elapsed(wall_start).saturating_sub(last_change) >= options.stability_window
        {
            return Ok(last);
        }
    }
}

/// Capture for the wait loop: a pane that vanishes between the liveness
/// check and the capture surfaces as [`KiraMuxError::PaneDiedDuringWait`],
/// matching [`pane_is_dead`], instead of a transport error.
fn capture_or_died(tmux: &dyn TmuxAdapter, agent_id: &str, pane_id: &str) -> Result<String> {
    match tmux.capture_pane(pane_id, WAIT_CAPTURE_LINES) {
        Err(error) if TmuxError::is_missing_target(&error) => {
            Err(KiraMuxError::PaneDiedDuringWait(agent_id.to_string()).into())
        }
        other => other,
    }
}

/// A vanished pane (killed window / missing target) counts as dead. Session
/// loss is also treated as dead for the wait loop so callers get a typed
/// exit 6 rather than an untyped transport failure.
fn pane_is_dead(tmux: &dyn TmuxAdapter, pane_id: &str) -> Result<bool> {
    match tmux.list_panes(pane_id) {
        Ok(panes) => Ok(panes
            .iter()
            .find(|pane| pane.pane_id == pane_id)
            .is_none_or(|pane| pane.pane_dead)),
        Err(error) if TmuxError::is_missing_target(&error) => Ok(true),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ResolvedProject;
    use crate::test_support::TestResultExt;

    /// Resolve then wait — covers topology gates at wait entry in tests.
    fn wait_for_stable_output(
        tmux: &dyn TmuxAdapter,
        project: &ResolvedProject,
        agent_id: &str,
        options: &WaitOptions,
    ) -> Result<String> {
        let (pane, _agent, _topology) =
            super::super::resolve::resolve_managed_pane(tmux, project, agent_id)?;
        wait_on_pane(tmux, agent_id, &pane.pane_id, options)
    }

    /// Millisecond-scale knobs so timeout paths stay fast; the stability
    /// window spans several polls, mirroring the production ratio. Virtual
    /// time means these tests do not burn wall-clock sleeps.
    fn fast_options() -> WaitOptions {
        WaitOptions::virtual_time(
            Duration::from_millis(1),
            Duration::from_millis(20),
            Duration::from_millis(250),
        )
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

        assert!(
            matches!(
                err.downcast_ref::<KiraMuxError>(),
                Some(KiraMuxError::PaneDiedDuringWait(id)) if id == "alpha"
            ),
            "post-send wait must use PaneDiedDuringWait, not send-time DeadPane, got: {err}"
        );
    }

    #[test]
    fn wait_fails_when_pane_dies_mid_wait() {
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        crate::test_support::setup_healthy_session(&fake, &project);
        fake.queue_pane_contents("%0", &["prompt echo", "streaming...", "str"]);
        fake.set_pane_dies_after_captures("%0", 3);
        // Stability window is unreachable; death must win before hard timeout.
        let options = WaitOptions::virtual_time(
            Duration::from_millis(1),
            Duration::from_secs(30),
            Duration::from_millis(250),
        );

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
    fn wait_fails_when_pane_vanishes_mid_wait() {
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        crate::test_support::setup_healthy_session(&fake, &project);
        // Baseline capture, then the pane is gone (killed window): list_panes
        // returns MissingTarget, which must map to PaneDiedDuringWait.
        fake.queue_pane_contents("%0", &["prompt echo"]);
        fake.set_pane_removed_after_captures("%0", 1);
        let options = WaitOptions::virtual_time(
            Duration::from_millis(1),
            Duration::from_secs(30),
            Duration::from_millis(250),
        );

        let err = wait_for_stable_output(&fake, &project, "alpha", &options).err_or_panic();

        assert!(
            matches!(
                err.downcast_ref::<KiraMuxError>(),
                Some(KiraMuxError::PaneDiedDuringWait(id)) if id == "alpha"
            ),
            "vanished pane must be typed PaneDiedDuringWait (exit 6), got: {err}"
        );
    }

    #[test]
    fn wait_on_pane_skips_resolve_and_uses_given_pane_id() {
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        crate::test_support::setup_healthy_session(&fake, &project);
        fake.queue_pane_contents(
            "%0",
            &["prompt echo", "prompt echo\nreply", "prompt echo\nreply"],
        );

        // No project inspect path: direct pane wait after a successful send.
        let output = wait_on_pane(&fake, "alpha", "%0", &fast_options()).or_panic();
        assert_eq!(output, "prompt echo\nreply");
    }

    #[test]
    fn capture_between_liveness_checks_maps_vanished_pane_to_died() {
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        crate::test_support::setup_healthy_session(&fake, &project);

        // Pane gone at capture time (vanished after the liveness check):
        // the typed MissingTarget must surface as PaneDiedDuringWait.
        let err = capture_or_died(&fake, "alpha", "%99").err_or_panic();

        assert!(
            matches!(
                err.downcast_ref::<KiraMuxError>(),
                Some(KiraMuxError::PaneDiedDuringWait(id)) if id == "alpha"
            ),
            "capture of a vanished pane must map to PaneDiedDuringWait, got: {err}"
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
