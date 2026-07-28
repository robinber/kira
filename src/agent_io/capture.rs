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

use anyhow::Result;
use serde::Serialize;

use super::deep_capture::{
    DEEP_CAPTURE_MAX_HEIGHT, DeepCaptureOptions, DeepCaptureStatus, deep_capture_with_status,
};
use super::resolve::{or_dead_pane, resolve_managed_pane};
use crate::model::ResolvedProject;
use crate::tmux::{PaneInfo, TmuxAdapter};

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
    options: &DeepCaptureOptions,
) -> Result<PaneCapture> {
    let (pane, _agent, _topology) = resolve_managed_pane(tmux, project, agent_id)?;
    let (output, status) = or_dead_pane(agent_id, capture_with_depth(tmux, &pane, lines, options))?;

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
        match deep_capture_with_status(tmux, &pane.pane_id, lines, options) {
            (Some(output), status) => return Ok((output, status)),
            (None, status) => status,
        }
    };
    Ok((tmux.capture_pane(&pane.pane_id, lines)?, status))
}

#[cfg(test)]
mod tests;
