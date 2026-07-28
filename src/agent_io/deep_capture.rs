//! Deep capture: zoom/resize an alternate-screen pane so the TUI repaints
//! more of its internal transcript, then restore the window exactly as found.
//!
//! Used by plain `capture` and the final `send --wait` deepen step.

use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use serde::Serialize;

use super::lock;
use crate::tmux::{PaneInfo, TmuxAdapter, WindowGeometry};

/// Ceiling on the temporary window height (tmux rejects absurd sizes, and
/// content beyond what a TUI repaints at this height is unrecoverable anyway).
pub(crate) const DEEP_CAPTURE_MAX_HEIGHT: usize = 1000;

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
    pub(crate) fn fast() -> Self {
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
pub(crate) enum DeepOutcome {
    /// Deepened output: geometry ran and a repaint was observed.
    Deepened(String),
    /// The pane already spans a tall-enough window; plain capture is
    /// already full-depth.
    NothingToDeepen,
    /// Another capture owns the window lock.
    Busy,
}

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
    options: &DeepCaptureOptions,
) -> String {
    let pane = match super::resolve::find_pane(tmux, pane_id) {
        Ok(pane) => pane,
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
    match deep_capture_with_status(tmux, &pane.pane_id, lines, options) {
        (Some(output), _) => output,
        (None, _) => converged,
    }
}

/// Run a deep capture and fold its outcome into `(deepened output, status)`,
/// owning the shared lock-contention and failure diagnostics so both the
/// capture command and the wait path report them identically.
pub(super) fn deep_capture_with_status(
    tmux: &dyn TmuxAdapter,
    pane_id: &str,
    lines: usize,
    options: &DeepCaptureOptions,
) -> (Option<String>, DeepCaptureStatus) {
    match deep_capture(tmux, pane_id, lines, options) {
        Ok(DeepOutcome::Deepened(output)) => (Some(output), DeepCaptureStatus::Completed),
        Ok(DeepOutcome::NothingToDeepen) => (None, DeepCaptureStatus::NotNeeded),
        Ok(DeepOutcome::Busy) => {
            tracing::warn!(
                pane = %pane_id,
                "another capture owns this window's deep-capture lock; \
                 output is limited to the visible frame"
            );
            (None, DeepCaptureStatus::Busy)
        }
        Err(error) => {
            tracing::warn!(
                pane = %pane_id,
                %error,
                "deep capture failed; agent runs on the tmux alternate \
                 screen, so output is limited to the visible frame"
            );
            (None, DeepCaptureStatus::Unavailable)
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
pub(crate) fn deep_capture(
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
    let captured = resize_and_capture(tmux, pane_id, &geometry, &baseline, lines, options);
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

fn resize_and_capture(
    tmux: &dyn TmuxAdapter,
    pane_id: &str,
    geometry: &WindowGeometry,
    baseline: &str,
    lines: usize,
    options: &DeepCaptureOptions,
) -> Result<String> {
    // Same one-line derivations as the caller (which needs them for flow
    // control and restore) — cheaper than threading two more parameters.
    let target_height = lines.min(DEEP_CAPTURE_MAX_HEIGHT);
    let need_resize = geometry.height < target_height;
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
