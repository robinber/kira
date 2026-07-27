//! Capture recent pane history for a resolved agent.
//!
//! Resolves the live managed pane, then returns text (or a JSON-ready
//! payload) for `kira-mux capture`.
//!
//! Agents whose TUI runs on the tmux alternate screen (Claude Code, Grok
//! Build, …) accumulate no tmux history: plain `capture-pane` depth is
//! capped at the visible frame regardless of the requested line count, and
//! those TUIs keep their transcript internally without ever scrolling the
//! terminal. Deep capture recovers that transcript by temporarily zooming
//! the pane and growing the window so the TUI repaints more of it, then
//! restoring the window exactly as found.

use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use serde::Serialize;

use super::resolve::resolve_managed_pane;
use crate::model::ResolvedProject;
use crate::tmux::{PaneInfo, TmuxAdapter, WindowGeometry};

/// Ceiling on the temporary window height (tmux rejects absurd sizes, and
/// content beyond what a TUI repaints at this height is unrecoverable anyway).
const DEEP_CAPTURE_MAX_HEIGHT: usize = 1000;
/// Delay between captures while waiting for the TUI to repaint after the
/// resize.
const DEEP_CAPTURE_REDRAW_POLL: Duration = Duration::from_millis(300);
/// Bound on the repaint wait, for a TUI that never repaints (the enlarged
/// frame shows nothing new) or never stops redrawing (spinners).
const DEEP_CAPTURE_REDRAW_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Serialize)]
pub(crate) struct PaneCapture {
    pub project_id: String,
    pub profile_id: String,
    pub agent_id: String,
    pub pane_id: String,
    pub pane_dead: bool,
    pub pane_dead_status: Option<i32>,
    /// Whether the pane program was on the tmux alternate screen (no tmux
    /// history: plain capture depth is capped at the visible frame).
    pub alternate_on: bool,
    /// Whether the resize-based deep capture produced `output`. `false` with
    /// `alternate_on` means depth is limited to the visible frame.
    pub deep_capture: bool,
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
    let (pane, _agent, _topology) = resolve_managed_pane(tmux, project, agent_id)?;
    let (output, deep_capture) = capture_with_depth(tmux, &pane, lines)?;

    Ok(PaneCapture {
        project_id: project.id.clone(),
        profile_id: project.profile_id.clone(),
        agent_id: agent_id.to_string(),
        pane_id: pane.pane_id,
        pane_dead: pane.pane_dead,
        pane_dead_status: pane.pane_dead_status,
        alternate_on: pane.alternate_on,
        deep_capture,
        lines,
        output,
    })
}

/// Capture up to `lines` from `pane`, deepening through the alternate screen
/// when needed. Returns the text and whether deep capture produced it.
///
/// Deep-capture failures fall back to the plain (visible-frame) capture with
/// a warning rather than failing the whole command: truncated output plus a
/// diagnostic beats no output.
fn capture_with_depth(
    tmux: &dyn TmuxAdapter,
    pane: &PaneInfo,
    lines: usize,
) -> Result<(String, bool)> {
    if wants_deep_capture(pane, lines) {
        match deep_capture(tmux, &pane.pane_id, lines) {
            Ok(output) => return Ok((output, true)),
            Err(error) => tracing::warn!(
                pane = %pane.pane_id,
                %error,
                "deep capture failed; agent runs on the tmux alternate screen, \
                 so output is limited to the visible frame"
            ),
        }
    }
    Ok((tmux.capture_pane(&pane.pane_id, lines)?, false))
}

/// Deep capture applies only where it can help: an alternate-screen pane
/// whose visible frame is shallower than the request, with a live process to
/// repaint after the resize (a dead pane's frame is frozen).
fn wants_deep_capture(pane: &PaneInfo, lines: usize) -> bool {
    pane.alternate_on && !pane.pane_dead && lines > pane.pane_height
}

/// Best-effort depth upgrade for the final `send --wait` capture.
///
/// Re-reads the pane state (alternate-screen mode can change during a long
/// generation), deepens when useful, and on any failure returns the converged
/// capture unchanged — the wait already succeeded, so this must never turn a
/// success into an error.
pub(crate) fn deepen_wait_capture(
    tmux: &dyn TmuxAdapter,
    pane_id: &str,
    lines: usize,
    converged: String,
) -> String {
    let pane = tmux
        .list_panes(pane_id)
        .ok()
        .and_then(|panes| panes.into_iter().find(|pane| pane.pane_id == pane_id));
    let Some(pane) = pane else { return converged };
    if !wants_deep_capture(&pane, lines) {
        return converged;
    }
    match deep_capture(tmux, &pane.pane_id, lines) {
        Ok(output) => output,
        Err(error) => {
            tracing::warn!(
                pane = %pane_id,
                %error,
                "deep capture after wait failed; output is limited to the \
                 visible frame (agent runs on the tmux alternate screen)"
            );
            converged
        }
    }
}

/// Zoom the pane, grow its window so the TUI repaints more of its internal
/// transcript, capture, then restore the window exactly as found (size, zoom,
/// and the `window-size` option that `resize-window` forces to `manual`).
///
/// Known limit: two concurrent deep captures on the same window race on the
/// saved geometry; the last restore wins.
fn deep_capture(tmux: &dyn TmuxAdapter, pane_id: &str, lines: usize) -> Result<String> {
    let geometry = tmux.window_geometry(pane_id)?;
    // Exactly the requested lines: a frame taller than the capture window
    // would push the head of a top-anchored transcript (Grok Build pins its
    // input box at the bottom of the enlarged frame) past the last-N-lines
    // cut. TUI chrome rows count toward the budget, as in plain captures.
    let target_height = lines.min(DEEP_CAPTURE_MAX_HEIGHT);
    if geometry.height >= target_height {
        // Window is already tall enough: the visible frame is the best any
        // resize could do.
        return tmux.capture_pane(pane_id, lines);
    }
    if geometry.zoomed && !geometry.pane_active {
        // Unzooming someone else's zoom to grow this pane is too intrusive.
        bail!("window is zoomed on another pane");
    }

    let zoom_toggled = !geometry.zoomed;
    if zoom_toggled {
        tmux.toggle_pane_zoom(pane_id)?;
    }
    let captured = resize_and_capture(tmux, pane_id, &geometry, target_height, lines);
    if let Err(error) = restore_window(tmux, pane_id, &geometry, zoom_toggled) {
        tracing::warn!(
            pane = %pane_id,
            %error,
            "failed to restore window after deep capture"
        );
    }
    captured
}

fn resize_and_capture(
    tmux: &dyn TmuxAdapter,
    pane_id: &str,
    geometry: &WindowGeometry,
    target_height: usize,
    lines: usize,
) -> Result<String> {
    // The pre-resize frame is the baseline: stillness alone cannot mean
    // "repainted" because the TUI may not have handled SIGWINCH yet — settled
    // means the capture moved off the baseline and then held for one poll.
    // A TUI whose repaint shows nothing new (or that never stops animating)
    // hits the bound and the latest frame is returned.
    let baseline = tmux.capture_pane(pane_id, lines)?;
    tmux.resize_window(pane_id, geometry.width, target_height)?;
    let started = Instant::now();
    let mut last: Option<String> = None;
    loop {
        std::thread::sleep(DEEP_CAPTURE_REDRAW_POLL);
        let current = tmux.capture_pane(pane_id, lines)?;
        let repainted_and_stable = current != baseline && last.as_deref() == Some(current.as_str());
        if repainted_and_stable || started.elapsed() >= DEEP_CAPTURE_REDRAW_TIMEOUT {
            return Ok(current);
        }
        last = Some(current);
    }
}

/// Undo the zoom/resize in reverse order, then drop the `window-size =
/// manual` override unless the window already had one before kira touched it.
fn restore_window(
    tmux: &dyn TmuxAdapter,
    pane_id: &str,
    geometry: &WindowGeometry,
    zoom_toggled: bool,
) -> Result<()> {
    tmux.resize_window(pane_id, geometry.width, geometry.height)?;
    if zoom_toggled {
        tmux.toggle_pane_zoom(pane_id)?;
    }
    if !geometry.size_option_set {
        tmux.unset_window_size_option(pane_id)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{FakeOp, err, ok};

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

    /// A 100-line internal transcript in an alternate-screen pane 10 rows
    /// tall: plain capture can only ever see the last 10 rows.
    fn setup_alt_screen_transcript(fake: &crate::test_support::FakeTmux) {
        fake.set_pane_alternate_on("%0", true);
        fake.set_pane_height("%0", 10);
        let content = (1..=100)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        fake.set_pane_content("%0", &content);
    }

    #[test]
    fn capture_output_normal_screen_never_resizes() {
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        crate::test_support::setup_healthy_session(&fake, &project);
        fake.set_pane_content("%0", "plain history");

        let capture = ok(
            capture_output(&fake, &project, "alpha", 200),
            "capture_output should succeed on a normal-screen pane",
        );
        assert!(!capture.alternate_on);
        assert!(!capture.deep_capture);
        assert!(
            fake.ops().is_empty(),
            "normal-screen capture must not touch window geometry, got: {:?}",
            fake.ops()
        );
    }

    #[test]
    fn capture_output_deepens_alternate_screen_pane_and_restores_window() {
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        crate::test_support::setup_healthy_session(&fake, &project);
        setup_alt_screen_transcript(&fake);

        let capture = ok(
            capture_output(&fake, &project, "alpha", 200),
            "deep capture should succeed",
        );
        assert!(capture.alternate_on);
        assert!(capture.deep_capture);
        let lines: Vec<&str> = capture.output.lines().collect();
        assert_eq!(
            lines.len(),
            100,
            "deep capture must recover the full transcript, got {} lines",
            lines.len()
        );
        assert_eq!(lines[0], "line 1");

        assert_eq!(
            fake.ops(),
            vec![
                FakeOp::ToggleZoom {
                    pane_id: "%0".into()
                },
                FakeOp::ResizeWindow {
                    pane_id: "%0".into(),
                    width: 200,
                    height: 200,
                },
                FakeOp::ResizeWindow {
                    pane_id: "%0".into(),
                    width: 200,
                    height: 24,
                },
                FakeOp::ToggleZoom {
                    pane_id: "%0".into()
                },
                FakeOp::UnsetWindowSizeOption {
                    pane_id: "%0".into()
                },
            ],
            "deep capture must zoom, grow, then restore in reverse order"
        );
    }

    #[test]
    fn capture_output_alt_screen_shallow_request_stays_plain() {
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        crate::test_support::setup_healthy_session(&fake, &project);
        setup_alt_screen_transcript(&fake);

        // Requested depth fits in the visible frame: nothing to deepen.
        let capture = ok(
            capture_output(&fake, &project, "alpha", 5),
            "shallow capture should succeed",
        );
        assert!(!capture.deep_capture);
        assert!(fake.ops().is_empty());
        assert_eq!(capture.output.lines().count(), 5);
    }

    #[test]
    fn capture_output_dead_alt_screen_pane_stays_plain() {
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        crate::test_support::setup_session_with_dead_panes(&fake, &project, &[0]);
        setup_alt_screen_transcript(&fake);

        // A dead pane cannot repaint after a resize: skip the whole dance.
        let capture = ok(
            capture_output(&fake, &project, "alpha", 200),
            "capture of dead alt-screen pane should succeed",
        );
        assert!(!capture.deep_capture);
        assert!(fake.ops().is_empty());
    }

    #[test]
    fn capture_output_falls_back_when_window_zoomed_on_another_pane() {
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        crate::test_support::setup_healthy_session(&fake, &project);
        setup_alt_screen_transcript(&fake);
        ok(
            fake.toggle_pane_zoom("%1"),
            "zooming the sibling pane should succeed",
        );

        let capture = ok(
            capture_output(&fake, &project, "alpha", 200),
            "capture should fall back, not fail",
        );
        assert!(
            !capture.deep_capture,
            "deep capture must not steal another pane's zoom"
        );
        // Viewport-limited fallback: the pane height caps the output.
        assert_eq!(capture.output.lines().count(), 10);
        assert!(
            !fake.ops().iter().any(|op| matches!(
                op,
                FakeOp::ResizeWindow { .. } | FakeOp::UnsetWindowSizeOption { .. }
            )),
            "fallback must leave the window untouched, got: {:?}",
            fake.ops()
        );
    }

    #[test]
    fn deepen_wait_capture_recovers_full_transcript() {
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        crate::test_support::setup_healthy_session(&fake, &project);
        setup_alt_screen_transcript(&fake);

        let deepened = deepen_wait_capture(&fake, "%0", 200, "viewport tail\n".to_string());
        assert_eq!(
            deepened.lines().count(),
            100,
            "wait deepening must recover the full transcript"
        );
    }

    #[test]
    fn deepen_wait_capture_returns_converged_on_normal_screen() {
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        crate::test_support::setup_healthy_session(&fake, &project);
        fake.set_pane_content("%0", "history is fine");

        let converged = "converged output\n".to_string();
        let deepened = deepen_wait_capture(&fake, "%0", 200, converged.clone());
        assert_eq!(deepened, converged);
        assert!(fake.ops().is_empty());
    }

    #[test]
    fn deepen_wait_capture_survives_vanished_pane() {
        let fake = crate::test_support::FakeTmux::new();

        let converged = "converged output\n".to_string();
        let deepened = deepen_wait_capture(&fake, "%9", 200, converged.clone());
        assert_eq!(
            deepened, converged,
            "a vanished pane must not turn a successful wait into a failure"
        );
    }
}
