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
use super::send::or_dead_pane;
use crate::model::ResolvedProject;
use crate::tmux::{PaneInfo, TmuxAdapter, WindowGeometry};

/// Ceiling on the temporary window height (tmux rejects absurd sizes, and
/// content beyond what a TUI repaints at this height is unrecoverable anyway).
const DEEP_CAPTURE_MAX_HEIGHT: usize = 1000;

/// Repaint-wait tuning. Production uses [`DeepCaptureOptions::default`];
/// tests inject tiny durations so timeout paths run without long sleeps.
pub(crate) struct DeepCaptureOptions {
    /// Delay between captures while waiting for the TUI to repaint after the
    /// zoom/resize.
    redraw_poll: Duration,
    /// Bound on the repaint wait. Hitting it without ever observing a change
    /// is a deep-capture failure: reporting the baseline frame as deepened
    /// would recreate the silent truncation this feature removes.
    redraw_timeout: Duration,
}

impl Default for DeepCaptureOptions {
    fn default() -> Self {
        Self {
            redraw_poll: Duration::from_millis(300),
            redraw_timeout: Duration::from_secs(5),
        }
    }
}

#[cfg(test)]
impl DeepCaptureOptions {
    /// Tiny durations so tests exercise the timeout path without real waits.
    fn fast() -> Self {
        Self {
            redraw_poll: Duration::from_millis(1),
            redraw_timeout: Duration::from_millis(20),
        }
    }
}

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
    /// Whether `output` came from a completed deep capture: the zoom/resize
    /// ran and a repaint of the enlarged frame was observed. `false` with
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
    let (output, deep_capture) = or_dead_pane(
        agent_id,
        capture_with_depth(tmux, &pane, lines, &DeepCaptureOptions::default()),
    )?;

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
/// when needed. Returns the text and whether a completed deep capture
/// produced it.
///
/// Deep-capture failures fall back to the plain (visible-frame) capture with
/// a warning rather than failing the whole command: truncated output plus a
/// diagnostic beats no output.
fn capture_with_depth(
    tmux: &dyn TmuxAdapter,
    pane: &PaneInfo,
    lines: usize,
    options: &DeepCaptureOptions,
) -> Result<(String, bool)> {
    if wants_deep_capture(pane, lines) {
        match deep_capture(tmux, &pane.pane_id, lines, options) {
            Ok(Some(output)) => return Ok((output, true)),
            // Nothing to deepen (the pane already spans a tall-enough
            // window): the plain capture below is already full-depth.
            Ok(None) => {}
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
    let pane = match tmux.list_panes(pane_id) {
        Ok(panes) => panes.into_iter().find(|pane| pane.pane_id == pane_id),
        Err(error) => {
            // Still fail-open, but never silently: the converged capture may
            // be depth-capped and the caller deserves the diagnostic.
            tracing::warn!(
                pane = %pane_id,
                %error,
                "could not re-read pane state after wait; skipping deep \
                 capture, output may be limited to the visible frame"
            );
            None
        }
    };
    let Some(pane) = pane else { return converged };
    if !wants_deep_capture(&pane, lines) {
        return converged;
    }
    match deep_capture(tmux, &pane.pane_id, lines, &DeepCaptureOptions::default()) {
        Ok(Some(output)) => output,
        Ok(None) => converged,
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

/// Zoom the pane and/or grow its window so the TUI repaints more of its
/// internal transcript, capture, then restore the window exactly as found
/// (size, zoom, active pane, and the window-local `window-size` value that
/// `resize-window` forces to `manual`).
///
/// Returns `Ok(None)` when there is nothing to deepen (the pane already
/// spans a tall-enough window), so the caller's plain capture is already
/// full-depth. In a multi-pane layout the window being tall enough is *not*
/// sufficient — the pane is shorter than the window — so zooming alone (no
/// resize) is a valid deepening step.
///
/// Known limit: two concurrent deep captures on the same window race on the
/// saved geometry; the last restore wins.
fn deep_capture(
    tmux: &dyn TmuxAdapter,
    pane_id: &str,
    lines: usize,
    options: &DeepCaptureOptions,
) -> Result<Option<String>> {
    let geometry = tmux.window_geometry(pane_id)?;
    // Exactly the requested lines: a frame taller than the capture window
    // would push the head of a top-anchored transcript (Grok Build pins its
    // input box at the bottom of the enlarged frame) past the last-N-lines
    // cut. TUI chrome rows count toward the budget, as in plain captures.
    let target_height = lines.min(DEEP_CAPTURE_MAX_HEIGHT);
    if geometry.zoomed && !geometry.pane_active {
        // Unzooming someone else's zoom to grow this pane is too intrusive.
        bail!("window is zoomed on another pane");
    }
    let need_zoom = !geometry.zoomed;
    let need_resize = geometry.height < target_height;
    if !need_zoom && !need_resize {
        // Already zoomed on this pane in a tall-enough window: the visible
        // frame is the best any geometry change could do.
        return Ok(None);
    }

    // The pre-change frame is the baseline for repaint detection, captured
    // before any geometry mutation.
    let baseline = tmux.capture_pane(pane_id, lines)?;
    if need_zoom {
        tmux.toggle_pane_zoom(pane_id)?;
    }
    let captured = resize_and_capture(
        tmux,
        pane_id,
        &geometry,
        &baseline,
        target_height,
        need_resize,
        lines,
        options,
    );
    if let Err(error) = restore_window(tmux, pane_id, &geometry, need_zoom, need_resize) {
        tracing::warn!(
            pane = %pane_id,
            %error,
            "failed to restore window after deep capture"
        );
    }
    captured.map(Some)
}

#[expect(
    clippy::too_many_arguments,
    reason = "internal step of deep_capture; a one-off context struct would only rename the coupling"
)]
fn resize_and_capture(
    tmux: &dyn TmuxAdapter,
    pane_id: &str,
    geometry: &WindowGeometry,
    baseline: &str,
    target_height: usize,
    need_resize: bool,
    lines: usize,
    options: &DeepCaptureOptions,
) -> Result<String> {
    if need_resize {
        tmux.resize_window(&geometry.window_id, geometry.width, target_height)?;
    }
    // Stillness alone cannot mean "repainted" — the TUI may not have handled
    // SIGWINCH yet. Settled means the capture moved off the baseline and
    // then held for one poll. Never observing a change by the bound is a
    // failure: reporting the baseline as deepened would silently truncate.
    // (A transcript that genuinely fits the old frame also lands here; the
    // fallback plain capture returns the same content, so nothing is lost.)
    let started = Instant::now();
    let mut last: Option<String> = None;
    loop {
        std::thread::sleep(options.redraw_poll);
        let current = tmux.capture_pane(pane_id, lines)?;
        let repainted = current != baseline;
        if repainted && last.as_deref() == Some(current.as_str()) {
            return Ok(current);
        }
        if started.elapsed() >= options.redraw_timeout {
            if repainted {
                // Still animating (spinner): the latest frame is the best
                // stable answer available.
                return Ok(current);
            }
            bail!("TUI did not repaint the enlarged frame in time");
        }
        last = Some(current);
    }
}

/// Undo the resize/zoom, restore the window-local `window-size` value, and
/// re-select the originally active pane.
///
/// Steps are independent and all attempted even when one fails (the first
/// error is reported): every restore op addresses the *window*, which
/// survives the observed pane vanishing mid-capture.
fn restore_window(
    tmux: &dyn TmuxAdapter,
    pane_id: &str,
    geometry: &WindowGeometry,
    zoom_toggled: bool,
    resized: bool,
) -> Result<()> {
    let mut first_error = None;
    let mut note = |result: Result<()>| {
        if let Err(error) = result
            && first_error.is_none()
        {
            first_error = Some(error);
        }
    };

    if resized {
        note(tmux.resize_window(&geometry.window_id, geometry.width, geometry.height));
    }
    if zoom_toggled {
        // A window target resolves to the active pane — the zoomed pane —
        // so unzoom works even if the captured pane is gone.
        note(tmux.toggle_pane_zoom(&geometry.window_id));
    }
    if resized {
        // `resize-window` forced the window-local value to `manual`; put
        // back what was there (a specific value, or inherit-from-global).
        note(match &geometry.size_option {
            Some(value) => tmux.set_window_option(&geometry.window_id, "window-size", value),
            None => tmux.unset_window_size_option(&geometry.window_id),
        });
    }
    if zoom_toggled && geometry.active_pane_id != pane_id {
        // Zooming an inactive pane made it active; unzooming does not
        // switch back.
        note(tmux.select_pane(&geometry.active_pane_id));
    }

    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
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

    /// Fake window id handed out by `FakeTmux::window_geometry` for the
    /// standard test project.
    fn test_window_id(project: &ResolvedProject) -> String {
        format!(
            "{}:{}",
            crate::workspace::session_name(project),
            project.window_name
        )
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

        // Restore ops address the window, not the captured pane: the pane
        // can vanish mid-capture while the window survives.
        let window = test_window_id(&project);
        assert_eq!(
            fake.ops(),
            vec![
                FakeOp::ToggleZoom {
                    target: "%0".into()
                },
                FakeOp::ResizeWindow {
                    target: window.clone(),
                    width: 200,
                    height: 200,
                },
                FakeOp::ResizeWindow {
                    target: window.clone(),
                    width: 200,
                    height: 24,
                },
                FakeOp::ToggleZoom {
                    target: window.clone(),
                },
                FakeOp::UnsetWindowSizeOption { target: window },
            ],
            "deep capture must zoom, grow, then restore in reverse order"
        );
    }

    #[test]
    fn capture_output_zooms_without_resize_when_window_already_tall() {
        // Multi-pane layout on a tall terminal: the window already exceeds
        // the requested depth but the pane does not — zoom alone must deepen.
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        crate::test_support::setup_healthy_session(&fake, &project);
        setup_alt_screen_transcript(&fake);
        let session = crate::workspace::session_name(&project);
        fake.set_window_height(&session, &project.window_name, 80);

        let capture = ok(
            capture_output(&fake, &project, "alpha", 40),
            "zoom-only deepening should succeed",
        );
        assert!(capture.deep_capture);
        let lines: Vec<&str> = capture.output.lines().collect();
        assert_eq!(
            lines.len(),
            40,
            "zoom must raise the pane to the window height, got {} lines",
            lines.len()
        );
        assert_eq!(lines[0], "line 61");
        assert!(
            !fake.ops().iter().any(|op| matches!(
                op,
                FakeOp::ResizeWindow { .. } | FakeOp::UnsetWindowSizeOption { .. }
            )),
            "a tall-enough window must not be resized (nor its window-size \
             policy touched), got: {:?}",
            fake.ops()
        );
        assert!(
            fake.ops()
                .iter()
                .filter(|op| matches!(op, FakeOp::ToggleZoom { .. }))
                .count()
                == 2,
            "zoom-only deepening must still zoom and unzoom, got: {:?}",
            fake.ops()
        );
    }

    #[test]
    fn capture_output_falls_back_when_tui_never_repaints() {
        // A transcript that fits the old frame never changes the capture:
        // the settle loop must fail (not report the baseline as deepened)
        // and the fallback plain capture returns the same content honestly.
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        crate::test_support::setup_healthy_session(&fake, &project);
        fake.set_pane_alternate_on("%0", true);
        fake.set_pane_height("%0", 10);
        fake.set_pane_content("%0", "short\ntranscript");

        let (pane, _, _) = ok(
            resolve_managed_pane(&fake, &project, "alpha"),
            "resolve should succeed",
        );
        let (output, deep) = ok(
            capture_with_depth(&fake, &pane, 200, &DeepCaptureOptions::fast()),
            "fallback capture should succeed",
        );
        assert!(!deep, "an unobserved repaint must not claim deep_capture");
        assert_eq!(output, "short\ntranscript");
        let window = test_window_id(&project);
        assert!(
            fake.ops().iter().any(|op| matches!(
                op,
                FakeOp::ResizeWindow { target, height: 24, .. } if *target == window
            )),
            "the window must still be restored after the failed settle, got: {:?}",
            fake.ops()
        );
    }

    #[test]
    fn capture_output_preserves_existing_window_size_policy() {
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        crate::test_support::setup_healthy_session(&fake, &project);
        setup_alt_screen_transcript(&fake);
        let session = crate::workspace::session_name(&project);
        fake.set_window_size_option(&session, &project.window_name, "latest");

        let capture = ok(
            capture_output(&fake, &project, "alpha", 200),
            "deep capture should succeed",
        );
        assert!(capture.deep_capture);
        assert_eq!(
            fake.window_size_option(&session, &project.window_name)
                .as_deref(),
            Some("latest"),
            "a pre-existing window-size policy must be restored by value, \
             not left as the manual forced by resize-window"
        );
        assert!(
            !fake
                .ops()
                .iter()
                .any(|op| matches!(op, FakeOp::UnsetWindowSizeOption { .. })),
            "restore must set the saved value back, not unset it, got: {:?}",
            fake.ops()
        );
    }

    #[test]
    fn capture_output_restores_original_active_pane() {
        // Zooming an inactive pane makes it active and unzoom does not
        // switch back — restore must re-select the original active pane.
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        crate::test_support::setup_healthy_session(&fake, &project);
        fake.set_pane_alternate_on("%1", true);
        fake.set_pane_height("%1", 10);
        let content = (1..=100)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        fake.set_pane_content("%1", &content);

        let capture = ok(
            capture_output(&fake, &project, "beta", 200),
            "deep capture of the inactive pane should succeed",
        );
        assert!(capture.deep_capture);
        let session = crate::workspace::session_name(&project);
        assert_eq!(
            fake.active_pane(&session, &project.window_name).as_deref(),
            Some("%0"),
            "the original active pane must be re-selected after unzoom"
        );
        assert!(
            fake.ops()
                .iter()
                .any(|op| matches!(op, FakeOp::SelectPane { pane_id } if pane_id == "%0")),
            "restore must select the original active pane, got: {:?}",
            fake.ops()
        );
    }

    #[test]
    fn deep_capture_restores_window_when_pane_vanishes_mid_capture() {
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        crate::test_support::setup_healthy_session(&fake, &project);
        setup_alt_screen_transcript(&fake);
        // Baseline capture survives; the pane is gone by the settle polls.
        fake.set_pane_removed_after_captures("%0", 2);

        let error = err(
            deep_capture(&fake, "%0", 200, &DeepCaptureOptions::fast()),
            "deep capture must fail when the pane vanishes",
        );
        assert!(
            crate::tmux::TmuxError::is_target_unavailable(&error),
            "expected a missing-target failure, got: {error}"
        );
        // Window-addressed restore must still run and succeed.
        let window = test_window_id(&project);
        assert!(
            fake.ops().iter().any(|op| matches!(
                op,
                FakeOp::ResizeWindow { target, height: 24, .. } if *target == window
            )) && fake.ops().iter().any(|op| matches!(
                op,
                FakeOp::UnsetWindowSizeOption { target } if *target == window
            )),
            "restore must address the surviving window, got: {:?}",
            fake.ops()
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
