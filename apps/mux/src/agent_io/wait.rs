//! Post-send wait: poll pane captures until the agent's output converges.
//!
//! There is no portable "done" signal across interactive agent TUIs. Kira
//! therefore observes the pane in three phases: submission acknowledgement
//! (the screen must durably move off the pre-submit image — a transient
//! redraw that reverts does not count), visible production, then settling.
//! Every distinct normalized frame resets settling, including cyclic spinner
//! frames. Frame history sizes the quiet window: durable production evidence
//! settles fastest, weak production waits longer, and a pane that never
//! changed again after the acknowledgement waits longest — a one-frame reply
//! and a silently thinking model are indistinguishable from captures alone.
//!
//! Capture-based convergence has known limits: activity perfectly aliased by
//! the poll interval is invisible, an idle monotonic counter never settles,
//! a reply that pauses longer than the active quiet window is cut short, and
//! a model that stays visually silent past the submission-only window is
//! reported done with only the submission echo captured.

use std::collections::VecDeque;
#[cfg(test)]
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::Result;

use super::send::{WAIT_CAPTURE_LINES, WaitSeed};
use crate::error::KiraMuxError;
use crate::tmux::{TmuxAdapter, TmuxError};

const RECENT_FRAME_LIMIT: usize = 8;

/// Tuning for the stability poll. Production uses [`WaitOptions::default`];
/// tests inject tiny durations (and optional virtual time) so timeout paths
/// run without wall-clock sleeps.
pub(crate) struct WaitOptions {
    /// Delay between pane captures.
    pub(crate) poll_interval: Duration,
    /// Micro-stability fallback when the rendered prompt cannot be found.
    submission_stability: Duration,
    /// Bound on the submission phase when the TUI keeps redrawing.
    submission_timeout: Duration,
    /// Quiet period after durable production evidence.
    normal_quiet_window: Duration,
    /// Conservative quiet period when production was seen but stayed weak.
    low_confidence_quiet_window: Duration,
    /// Most conservative quiet period when nothing changed after the
    /// submission acknowledgement: a one-frame reply and a silently thinking
    /// model look identical, so betting on "done" needs the longest odds.
    submission_only_quiet_window: Duration,
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
            submission_stability: Duration::from_secs(1),
            submission_timeout: Duration::from_secs(3),
            normal_quiet_window: Duration::from_secs(5),
            low_confidence_quiet_window: Duration::from_secs(10),
            submission_only_quiet_window: Duration::from_secs(30),
            hard_timeout: Duration::from_mins(10),
            clock: WaitClock::Wall,
        }
    }
}

impl WaitOptions {
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

struct SubmissionState {
    last_change: Duration,
    activity_seen: bool,
    /// First frame containing the rendered prompt. It must either survive the
    /// next poll or be followed by another non-baseline frame before the
    /// submission is acknowledged.
    prompt_candidate: Option<String>,
}

impl SubmissionState {
    fn new(last_change: Duration) -> Self {
        Self {
            last_change,
            activity_seen: false,
            prompt_candidate: None,
        }
    }
}

#[derive(Clone, Copy)]
struct SubmissionObservation<'a> {
    changed: bool,
    frame: &'a str,
    pre_submit: &'a str,
    prompt_visible: bool,
    observed_at: Duration,
}

enum SubmissionDecision {
    Pending,
    Acknowledged { production_seen: bool },
}

fn observe_submission(
    state: &mut SubmissionState,
    observation: SubmissionObservation<'_>,
    options: &WaitOptions,
) -> SubmissionDecision {
    let mut acknowledged = false;
    let mut production_seen = false;
    if observation.changed {
        state.last_change = observation.observed_at;
        if observation.frame == observation.pre_submit {
            state.activity_seen = false;
            state.prompt_candidate = None;
        } else if state.prompt_candidate.is_some() {
            acknowledged = true;
            production_seen = true;
        } else {
            state.activity_seen = true;
            if observation.prompt_visible {
                state.prompt_candidate = Some(observation.frame.to_string());
            }
        }
    } else if state.prompt_candidate.as_deref() == Some(observation.frame) {
        acknowledged = true;
    }

    let prompt_pending = state.prompt_candidate.is_some();
    let generically_stable = !prompt_pending
        && state.activity_seen
        && observation.frame != observation.pre_submit
        && observation.observed_at.saturating_sub(state.last_change)
            >= options.submission_stability;
    let redraw_timeout = !prompt_pending
        && state.activity_seen
        && observation.frame != observation.pre_submit
        && observation.observed_at >= options.submission_timeout;

    if acknowledged || generically_stable || redraw_timeout {
        SubmissionDecision::Acknowledged { production_seen }
    } else {
        SubmissionDecision::Pending
    }
}

enum WaitPhase {
    Submitting(SubmissionState),
    Settling { threshold_seen: bool },
}

/// A novel frame with its production context. Starts as the pending
/// candidate of the latest change; becomes material once it survives a poll.
struct MaterialEvent {
    frame: String,
    after_prior_activity: bool,
}

/// Tracks only enough history to distinguish durable novel frames from a
/// short cycle. Visible changes are handled separately and always reset the
/// quiet timer.
struct FrameTracker {
    recent: VecDeque<String>,
    pending: Option<MaterialEvent>,
    material: VecDeque<MaterialEvent>,
    changed_before: bool,
}

impl FrameTracker {
    fn new(baseline: String) -> Self {
        let mut recent = VecDeque::with_capacity(RECENT_FRAME_LIMIT);
        recent.push_back(baseline);
        Self {
            recent,
            pending: None,
            material: VecDeque::with_capacity(RECENT_FRAME_LIMIT),
            changed_before: false,
        }
    }

    fn reset(&mut self, baseline: String) {
        self.recent.clear();
        self.recent.push_back(baseline);
        self.pending = None;
        self.material.clear();
        self.changed_before = false;
    }

    fn observe_change(&mut self, frame: &str) {
        let cyclic = self.recent.iter().any(|recent| recent == frame);
        if cyclic {
            self.pending = None;
            self.material.retain(|event| event.frame != frame);
        } else {
            self.pending = Some(MaterialEvent {
                frame: frame.to_string(),
                after_prior_activity: self.changed_before,
            });
        }
        self.changed_before = true;
        push_bounded(&mut self.recent, frame.to_string());
    }

    fn observe_stable(&mut self, frame: &str) {
        if let Some(pending) = self.pending.take()
            && pending.frame == frame
        {
            push_bounded(&mut self.material, pending);
        }
    }

    fn has_strong_evidence(&self) -> bool {
        self.material.len() >= 2 || self.material.iter().any(|event| event.after_prior_activity)
    }

    fn quiet_window(&self, options: &WaitOptions, production_seen: bool) -> Duration {
        if self.has_strong_evidence() {
            options.normal_quiet_window
        } else if production_seen {
            options.low_confidence_quiet_window
        } else {
            options.submission_only_quiet_window
        }
    }
}

fn push_bounded<T>(items: &mut VecDeque<T>, item: T) {
    if items.len() == RECENT_FRAME_LIMIT {
        items.pop_front();
    }
    items.push_back(item);
}

/// Block until the observed pane converges; return the final raw capture
/// (same text shape as `capture`).
///
/// # Errors
///
/// [`KiraMuxError::PaneDiedDuringWait`] when the pane is dead at entry or
/// dies/vanishes mid-wait (frozen dead content must never read as "stable").
/// [`KiraMuxError::WaitTimeout`] when the hard timeout elapses first.
pub(crate) fn wait_on_pane(
    tmux: &dyn TmuxAdapter,
    agent_id: &str,
    seed: &WaitSeed,
    options: &WaitOptions,
) -> Result<String> {
    if pane_is_dead(tmux, &seed.delivered.pane_id)? {
        return Err(KiraMuxError::PaneDiedDuringWait(agent_id.to_string()).into());
    }

    let wall_start = Instant::now();
    let pre_submit = normalize_frame(&seed.pre_submit);
    let prompt_fragments = prompt_fragments(&seed.delivered.rendered);
    let pre_submit_search = normalize_search_text(&seed.pre_submit);
    let mut last_capture = seed.pre_submit.clone();
    let mut last_frame = pre_submit.clone();
    let mut phase = WaitPhase::Submitting(SubmissionState::new(Duration::ZERO));
    let mut tracker = FrameTracker::new(pre_submit.clone());
    let mut candidate_seen = false;
    let mut production_seen = false;
    let mut last_visible_change = Duration::ZERO;

    loop {
        let now = options.elapsed(wall_start);
        if now >= options.hard_timeout {
            return Err(KiraMuxError::WaitTimeout {
                agent_id: agent_id.to_string(),
                last_capture,
            }
            .into());
        }
        let remaining = options.hard_timeout.saturating_sub(now);
        options.sleep(options.poll_interval.min(remaining));

        if pane_is_dead(tmux, &seed.delivered.pane_id)? {
            return Err(KiraMuxError::PaneDiedDuringWait(agent_id.to_string()).into());
        }

        let current = capture_or_died(tmux, agent_id, &seed.delivered.pane_id)?;
        let observed_at = options.elapsed(wall_start);
        // Byte-identical captures normalize identically: skip the allocation.
        let mut changed = false;
        if current != last_capture {
            let current_frame = normalize_frame(&current);
            if current_frame != last_frame {
                last_frame = current_frame;
                changed = true;
            }
        }
        last_capture = current;

        if let WaitPhase::Submitting(submission) = &mut phase {
            let prompt_visible = changed
                && prompt_appeared(
                    &pre_submit_search,
                    &normalize_search_text(&last_capture),
                    &prompt_fragments,
                );
            let observation = SubmissionObservation {
                changed,
                frame: &last_frame,
                pre_submit: &pre_submit,
                prompt_visible,
                observed_at,
            };
            if let SubmissionDecision::Acknowledged {
                production_seen: seen,
            } = observe_submission(submission, observation, options)
            {
                candidate_seen = true;
                production_seen = seen;
                tracker.reset(last_frame.clone());
                last_visible_change = observed_at;
                phase = WaitPhase::Settling {
                    threshold_seen: false,
                };
            }
            continue;
        }

        if changed {
            if last_frame == pre_submit {
                // Returning to the exact pre-submit image invalidates the
                // acknowledgement. Stay conservative and wait for a durable
                // submission transition instead of reporting the idle pane.
                candidate_seen = false;
                production_seen = false;
                tracker.reset(pre_submit.clone());
                phase = WaitPhase::Submitting(SubmissionState::new(observed_at));
                continue;
            }
            candidate_seen = true;
            production_seen = true;
            tracker.observe_change(&last_frame);
            last_visible_change = observed_at;
            phase = WaitPhase::Settling {
                threshold_seen: false,
            };
            continue;
        }

        tracker.observe_stable(&last_frame);
        if !candidate_seen {
            continue;
        }

        let quiet_window = tracker.quiet_window(options, production_seen);
        if observed_at.saturating_sub(last_visible_change) < quiet_window {
            continue;
        }

        if let WaitPhase::Settling { threshold_seen } = &mut phase {
            if *threshold_seen {
                return Ok(last_capture);
            }
            *threshold_seen = true;
        }
    }
}

fn normalize_frame(capture: &str) -> String {
    let mut lines: Vec<&str> = capture.lines().map(str::trim_end).collect();
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

fn normalize_search_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn prompt_fragments(rendered_prompt: &str) -> Vec<String> {
    const FRAGMENT_CHARS: usize = 64;

    let normalized = normalize_search_text(rendered_prompt);
    let chars: Vec<char> = normalized.chars().collect();
    if chars.is_empty() {
        return Vec::new();
    }
    if chars.len() <= FRAGMENT_CHARS {
        return vec![normalized];
    }

    vec![
        chars.iter().take(FRAGMENT_CHARS).collect(),
        chars
            .iter()
            .skip(chars.len().saturating_sub(FRAGMENT_CHARS))
            .collect(),
    ]
}

fn prompt_appeared(pre_submit: &str, current: &str, fragments: &[String]) -> bool {
    fragments.iter().any(|fragment| {
        !fragment.is_empty()
            && !pre_submit.contains(fragment.as_str())
            && current.contains(fragment.as_str())
    })
}

/// Capture for the wait loop: a pane that vanishes (or a tmux server that
/// stops) between the liveness check and the capture surfaces as
/// [`KiraMuxError::PaneDiedDuringWait`], matching [`pane_is_dead`], instead
/// of a transport error.
fn capture_or_died(tmux: &dyn TmuxAdapter, agent_id: &str, pane_id: &str) -> Result<String> {
    match tmux.capture_pane(pane_id, WAIT_CAPTURE_LINES) {
        Err(error) if TmuxError::is_target_unavailable(&error) => {
            Err(KiraMuxError::PaneDiedDuringWait(agent_id.to_string()).into())
        }
        other => other,
    }
}

/// A vanished pane (killed window / missing target), a lost session, or a
/// stopped tmux server all count as dead for the wait loop so callers get a
/// typed exit 6 rather than an untyped transport failure — the same
/// `is_target_unavailable` classification the send path uses.
fn pane_is_dead(tmux: &dyn TmuxAdapter, pane_id: &str) -> Result<bool> {
    match tmux.list_panes(pane_id) {
        Ok(panes) => Ok(panes
            .iter()
            .find(|pane| pane.pane_id == pane_id)
            .is_none_or(|pane| pane.pane_dead)),
        Err(error) if TmuxError::is_target_unavailable(&error) => Ok(true),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests;
