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

use super::lock;
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

/// Machine-readable outcome of the depth strategy for one capture, exposed
/// through `capture --json` so scripted callers do not depend on stderr
/// warnings (suppressed at the default `--json` log level).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DeepCaptureStatus {
    /// Normal-screen or dead pane: depth comes from real tmux history.
    NotApplicable,
    /// The visible frame already satisfies the request.
    NotNeeded,
    /// The zoom/resize ran and a repaint of the enlarged frame was observed.
    Completed,
    /// Another capture currently owns the window lock; retry later.
    Busy,
    /// Deepening was attempted but could not complete (zoom conflict,
    /// repaint timeout, …): output is capped at the visible frame.
    Unavailable,
}

/// What one deep-capture attempt produced.
enum DeepOutcome {
    /// Deepened output: geometry ran and a repaint was observed.
    Deepened(String),
    /// The pane already spans a tall-enough window; plain capture is
    /// already full-depth.
    NothingToDeepen,
    /// Another capture owns the window lock.
    Busy,
}

#[derive(Debug, Serialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "JSON DTO: each bool is a stable field of the capture contract, not a state machine"
)]
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
    /// Visible pane height at resolve time — the plain-capture depth ceiling
    /// when `alternate_on` is set.
    pub pane_height: usize,
    /// Whether `output` came from a completed deep capture. Kept alongside
    /// [`Self::deep_capture_status`] for compatibility
    /// (`deep_capture == (deep_capture_status == "completed")`).
    pub deep_capture: bool,
    /// Depth-strategy outcome; see [`DeepCaptureStatus`].
    pub deep_capture_status: DeepCaptureStatus,
    /// Whether the request exceeds what deep capture can ever deliver for
    /// this pane: on an alternate-screen live pane, content beyond the
    /// height ceiling (1000 rows) is unreachable regardless of the outcome —
    /// including when the pane already spans a ceiling-height window and the
    /// status reads `not_needed`.
    pub depth_request_clamped: bool,
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
    let (output, status) = or_dead_pane(
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
        pane_height: pane.pane_height,
        deep_capture: status == DeepCaptureStatus::Completed,
        deep_capture_status: status,
        // Request-based, not outcome-based: a `not_needed` on a pane already
        // spanning a ceiling-height window must not read as "fully
        // satisfied" when the request goes beyond the ceiling.
        depth_request_clamped: pane.alternate_on
            && !pane.pane_dead
            && lines > pane.pane_height.max(DEEP_CAPTURE_MAX_HEIGHT),
        lines,
        output,
    })
}

/// Capture up to `lines` from `pane`, deepening through the alternate screen
/// when needed. Returns the text and the depth-strategy outcome.
///
/// Deep-capture failures (and lock contention) fall back to the plain
/// (visible-frame) capture with a warning rather than failing the whole
/// command: truncated output plus a diagnostic beats no output.
fn capture_with_depth(
    tmux: &dyn TmuxAdapter,
    pane: &PaneInfo,
    lines: usize,
    options: &DeepCaptureOptions,
) -> Result<(String, DeepCaptureStatus)> {
    let status = if !pane.alternate_on || pane.pane_dead {
        DeepCaptureStatus::NotApplicable
    } else if lines <= pane.pane_height {
        DeepCaptureStatus::NotNeeded
    } else {
        match deep_capture(tmux, &pane.pane_id, lines, options) {
            Ok(DeepOutcome::Deepened(output)) => return Ok((output, DeepCaptureStatus::Completed)),
            Ok(DeepOutcome::NothingToDeepen) => DeepCaptureStatus::NotNeeded,
            Ok(DeepOutcome::Busy) => {
                tracing::warn!(
                    pane = %pane.pane_id,
                    "another capture owns this window's deep-capture lock; \
                     output is limited to the visible frame"
                );
                DeepCaptureStatus::Busy
            }
            Err(error) => {
                tracing::warn!(
                    pane = %pane.pane_id,
                    %error,
                    "deep capture failed; agent runs on the tmux alternate \
                     screen, so output is limited to the visible frame"
                );
                DeepCaptureStatus::Unavailable
            }
        }
    };
    Ok((tmux.capture_pane(&pane.pane_id, lines)?, status))
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
        Ok(DeepOutcome::Deepened(output)) => output,
        Ok(DeepOutcome::NothingToDeepen) => converged,
        Ok(DeepOutcome::Busy) => {
            tracing::warn!(
                pane = %pane_id,
                "another capture owns this window's deep-capture lock; \
                 output is limited to the visible frame"
            );
            converged
        }
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
/// [`DeepOutcome::NothingToDeepen`] means the pane already spans a
/// tall-enough window, so the caller's plain capture is already full-depth.
/// In a multi-pane layout the window being tall enough is *not* sufficient —
/// the pane is shorter than the window — so zooming alone (no resize) is a
/// valid deepening step.
///
/// Concurrent deep captures of the same window are excluded by a per-window
/// file lock ([`lock::try_lock_window`]): without it, the second capture
/// would snapshot the first one's temporary geometry and restore it
/// permanently. Contention yields [`DeepOutcome::Busy`] immediately — no
/// waiting.
fn deep_capture(
    tmux: &dyn TmuxAdapter,
    pane_id: &str,
    lines: usize,
    options: &DeepCaptureOptions,
) -> Result<DeepOutcome> {
    // First read only names the window (id + socket) for the lock; the
    // authoritative geometry snapshot happens under the lock, because this
    // one may observe another capture's temporary state.
    let probe = tmux.window_geometry(pane_id)?;
    let Some(_lock) = lock::try_lock_window(&probe.socket_path, &probe.window_id)? else {
        return Ok(DeepOutcome::Busy);
    };
    let geometry = tmux.window_geometry(pane_id)?;
    if geometry.window_id != probe.window_id || geometry.socket_path != probe.socket_path {
        // The pane moved to another window between the probe and the lock:
        // the lock we hold does not cover the window we would mutate, so the
        // exclusion guarantee is void. Fail closed without touching anything.
        bail!(
            "pane moved from window {} to {} while acquiring the deep-capture lock",
            probe.window_id,
            geometry.window_id
        );
    }

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
        return Ok(DeepOutcome::NothingToDeepen);
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
    // The lock stays held through the full restore attempt below.
    if let Err(error) = restore_window(tmux, pane_id, &geometry, need_zoom, need_resize) {
        tracing::warn!(
            pane = %pane_id,
            %error,
            "failed to restore window after deep capture"
        );
    }
    captured.map(DeepOutcome::Deepened)
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
    //
    // Residual limits, same epistemic class as wait convergence: a spinner
    // tick that lands before the SIGWINCH repaint and then holds for a poll
    // reads as the repaint, and a TUI that animates until the bound returns
    // its latest (repainted but non-quiescent) frame as deepened.
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
        // Conditional-and-atomic: tmux auto-unzooms when the zoomed pane's
        // process is removed, so restoration must not assume its own zoom is
        // still in effect — and a client-side check-then-toggle would race
        // the same removal. `unzoom_window` folds both into one server-side
        // command; when zoomed, a window target resolves to the zoomed pane,
        // so it works even if the captured pane is gone.
        note(tmux.unzoom_window(&geometry.window_id));
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
        assert_eq!(
            capture.deep_capture_status,
            DeepCaptureStatus::NotApplicable
        );
        assert_eq!(capture.pane_height, 24);
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
        assert_eq!(capture.deep_capture_status, DeepCaptureStatus::Completed);
        assert!(!capture.depth_request_clamped);
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
                FakeOp::UnzoomWindow {
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
                .any(|op| matches!(op, FakeOp::ToggleZoom { .. }))
                && fake
                    .ops()
                    .iter()
                    .any(|op| matches!(op, FakeOp::UnzoomWindow { .. })),
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
        let (output, status) = ok(
            capture_with_depth(&fake, &pane, 200, &DeepCaptureOptions::fast()),
            "fallback capture should succeed",
        );
        assert_eq!(
            status,
            DeepCaptureStatus::Unavailable,
            "an unobserved repaint must not claim a completed deep capture"
        );
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
        // Its removal auto-unzooms the window (tmux semantics).
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
        // The removal already unzoomed the window: restoration's conditional
        // unzoom must be a no-op, never a re-zoom of the surviving pane.
        let session = crate::workspace::session_name(&project);
        assert!(
            !fake.window_is_zoomed(&session, &project.window_name),
            "restore must not re-zoom a window that tmux already unzoomed"
        );
        assert_eq!(
            fake.ops()
                .iter()
                .filter(|op| matches!(op, FakeOp::ToggleZoom { .. }))
                .count(),
            1,
            "only the initial zoom may toggle, got: {:?}",
            fake.ops()
        );
    }

    #[test]
    fn deep_capture_clamps_request_to_max_height() {
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        crate::test_support::setup_healthy_session(&fake, &project);
        setup_alt_screen_transcript(&fake);

        let capture = ok(
            capture_output(&fake, &project, "alpha", 2000),
            "over-cap deep capture should still succeed",
        );
        assert!(capture.deep_capture);
        assert!(
            capture.depth_request_clamped,
            "a request beyond the cap must be reported as clamped"
        );
        let window = test_window_id(&project);
        assert!(
            fake.ops().iter().any(|op| matches!(
                op,
                FakeOp::ResizeWindow { target, height, .. }
                    if *target == window && *height == DEEP_CAPTURE_MAX_HEIGHT
            )),
            "a request beyond the cap must resize to exactly the cap, got: {:?}",
            fake.ops()
        );
        assert!(
            fake.ops().iter().any(|op| matches!(
                op,
                FakeOp::ResizeWindow { target, height: 24, .. } if *target == window
            )),
            "restore must still return to the original height, got: {:?}",
            fake.ops()
        );
    }

    #[test]
    fn capture_output_reports_busy_when_window_lock_is_held() {
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        crate::test_support::setup_healthy_session(&fake, &project);
        setup_alt_screen_transcript(&fake);

        // Another process deep-captures the same window: hold its lock.
        let held = ok(
            lock::try_lock_window(&fake.socket_path(), &test_window_id(&project)),
            "external lock attempt should not error",
        );
        assert!(held.is_some(), "external lock should be acquired");

        let capture = ok(
            capture_output(&fake, &project, "alpha", 200),
            "contended capture must fall back, not fail",
        );
        assert_eq!(capture.deep_capture_status, DeepCaptureStatus::Busy);
        assert!(!capture.deep_capture);
        // Viewport-limited fallback, and no geometry mutation at all.
        assert_eq!(capture.output.lines().count(), 10);
        assert!(
            !fake.ops().iter().any(|op| matches!(
                op,
                FakeOp::ToggleZoom { .. }
                    | FakeOp::ResizeWindow { .. }
                    | FakeOp::UnzoomWindow { .. }
                    | FakeOp::UnsetWindowSizeOption { .. }
            )),
            "a busy window must not be touched, got: {:?}",
            fake.ops()
        );
    }

    #[test]
    fn capture_output_shallow_and_dead_statuses() {
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        crate::test_support::setup_healthy_session(&fake, &project);
        setup_alt_screen_transcript(&fake);

        let shallow = ok(
            capture_output(&fake, &project, "alpha", 5),
            "shallow capture should succeed",
        );
        assert_eq!(shallow.deep_capture_status, DeepCaptureStatus::NotNeeded);

        let fake_dead = crate::test_support::FakeTmux::new();
        crate::test_support::setup_session_with_dead_panes(&fake_dead, &project, &[0]);
        fake_dead.set_pane_alternate_on("%0", true);
        fake_dead.set_pane_content("%0", "frozen");
        let dead = ok(
            capture_output(&fake_dead, &project, "alpha", 200),
            "dead-pane capture should succeed",
        );
        assert_eq!(dead.deep_capture_status, DeepCaptureStatus::NotApplicable);
    }

    #[test]
    fn deep_capture_status_serializes_snake_case() {
        for (status, expected) in [
            (DeepCaptureStatus::NotApplicable, "\"not_applicable\""),
            (DeepCaptureStatus::NotNeeded, "\"not_needed\""),
            (DeepCaptureStatus::Completed, "\"completed\""),
            (DeepCaptureStatus::Busy, "\"busy\""),
            (DeepCaptureStatus::Unavailable, "\"unavailable\""),
        ] {
            let json = ok(serde_json::to_string(&status), "status should serialize");
            assert_eq!(json, expected);
        }
    }

    #[test]
    fn capture_output_fails_closed_when_pane_moves_window_during_lock() {
        // The pane moves to another window between the lock probe and the
        // under-lock geometry read: the held lock does not cover the window
        // that would be mutated, so deepening must fail closed (fallback,
        // zero geometry ops) instead of mutating an unlocked window.
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        crate::test_support::setup_healthy_session(&fake, &project);
        setup_alt_screen_transcript(&fake);
        let session = crate::workspace::session_name(&project);
        fake.add_window(&session, "other");
        fake.set_pane_relocated_after_geometry_reads("%0", &session, "other", 1);

        let capture = ok(
            capture_output(&fake, &project, "alpha", 200),
            "relocated-pane capture must fall back, not fail",
        );
        assert_eq!(capture.deep_capture_status, DeepCaptureStatus::Unavailable);
        assert!(
            !fake.ops().iter().any(|op| matches!(
                op,
                FakeOp::ToggleZoom { .. }
                    | FakeOp::ResizeWindow { .. }
                    | FakeOp::UnzoomWindow { .. }
                    | FakeOp::UnsetWindowSizeOption { .. }
            )),
            "no geometry may be touched under a mismatched lock, got: {:?}",
            fake.ops()
        );
    }

    #[test]
    fn capture_output_reports_clamp_even_when_nothing_to_deepen() {
        // A pane already zoomed in a ceiling-height window: nothing to
        // deepen, but a request beyond the ceiling is still unsatisfiable —
        // the JSON must not read as "fully served".
        let fake = crate::test_support::FakeTmux::new();
        let project = crate::test_support::test_project();
        crate::test_support::setup_healthy_session(&fake, &project);
        setup_alt_screen_transcript(&fake);
        let session = crate::workspace::session_name(&project);
        fake.set_window_height(&session, &project.window_name, DEEP_CAPTURE_MAX_HEIGHT);
        ok(
            fake.toggle_pane_zoom("%0"),
            "zooming the pane should succeed",
        );

        let capture = ok(
            capture_output(&fake, &project, "alpha", 2000),
            "at-ceiling capture should succeed",
        );
        assert_eq!(capture.deep_capture_status, DeepCaptureStatus::NotNeeded);
        assert!(
            capture.depth_request_clamped,
            "a request beyond the ceiling must be reported as clamped even \
             without a geometry change"
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
        assert_eq!(capture.deep_capture_status, DeepCaptureStatus::Unavailable);
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
