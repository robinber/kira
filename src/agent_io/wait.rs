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
//! Known agent TUIs additionally expose a busy marker: a stable interrupt
//! hint (e.g. "esc to interrupt") near the pane bottom while a turn runs.
//! While a marker is visible the wait never converges, even on a frozen
//! frame, and once one has been seen the quiet windows are floored by a
//! longer post-busy window — agents like Claude Code drop the marker and
//! look fully idle for a while when they background long tool calls, then
//! wake up to finish the reply.
//!
//! Capture-based convergence has known limits: activity perfectly aliased by
//! the poll interval is invisible, an idle monotonic counter never settles,
//! a reply that pauses longer than the active quiet window is cut short, and
//! a model that stays visually silent past the submission-only window is
//! reported done with only the submission echo captured. Busy markers narrow
//! but do not close the backgrounded-work gap: an idle-looking pane whose
//! agent wakes up later than the post-busy window still converges early, and
//! a narrow pane can truncate the marker out of the status line.
//!
//! Alternate-screen TUIs add a depth limit: their panes accumulate no tmux
//! history, so every frame observed here is capped at the visible pane
//! height. That is fine for convergence (frames still change and settle),
//! but the converged capture returned to the caller can miss the head of a
//! long reply — `send --wait` deepens that final capture via
//! [`super::deep_capture::deepen_wait_capture`] after this loop succeeds.

use std::collections::VecDeque;
#[cfg(test)]
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::Result;

use super::resolve::{find_pane, or_unavailable};
use super::send::WaitSeed;
use crate::error::KiraMuxError;
use crate::tmux::{TmuxAdapter, normalize_search_text, prompt_fragments};

const RECENT_FRAME_LIMIT: usize = 8;

/// Trailing frame lines scanned for busy markers: enough to span agent
/// status bars and spinner rows, small enough to skip transcript content
/// that merely mentions a marker phrase.
const BUSY_MARKER_SCAN_LINES: usize = 15;

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
    /// Floor applied to every quiet window once a busy marker has been seen:
    /// agents that background work drop the marker and look idle before the
    /// reply lands, so the marker's disappearance buys extra patience.
    post_busy_quiet_window: Duration,
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
            post_busy_quiet_window: Duration::from_secs(15),
            hard_timeout: Duration::from_mins(10),
            clock: WaitClock::Wall,
        }
    }
}

/// Env var selecting a wait profile — a test seam like
/// `KIRA_MUX_TMUX_SOCKET_NAME`. `fast` shrinks every window so the
/// exit-code contract (including the hard timeout → exit 7) is
/// exercisable end-to-end without production waits.
const WAIT_PROFILE_ENV: &str = "KIRA_MUX_WAIT_PROFILE";

impl WaitOptions {
    /// Production tuning, unless `KIRA_MUX_WAIT_PROFILE=fast` selects the
    /// integration-test profile.
    ///
    /// # Errors
    ///
    /// A set-but-unknown profile is a configuration error (exit 2): a typo
    /// while using the seam must not silently fall back to the ten-minute
    /// production timeout.
    pub(crate) fn from_env() -> Result<Self> {
        match std::env::var(WAIT_PROFILE_ENV) {
            Ok(profile) if profile == "fast" => Ok(Self::fast_profile()),
            Ok(profile) if profile.is_empty() => Ok(Self::default()),
            Ok(profile) => Err(KiraMuxError::ConfigValidation(
                crate::config::ConfigError::UnknownWaitProfile(profile),
            )
            .into()),
            Err(_) => Ok(Self::default()),
        }
    }

    /// Second-scale windows keeping the production ordering and behavior
    /// classes (the scale factors differ per knob): scripted integration
    /// agents emit with >=1.5s margin under every quiet window using
    /// whole-second sleeps, and the hard timeout stays reachable inside a
    /// test deadline.
    fn fast_profile() -> Self {
        Self {
            poll_interval: Duration::from_millis(50),
            submission_stability: Duration::from_millis(150),
            submission_timeout: Duration::from_millis(750),
            normal_quiet_window: Duration::from_millis(2500),
            low_confidence_quiet_window: Duration::from_secs(3),
            submission_only_quiet_window: Duration::from_secs(4),
            post_busy_quiet_window: Duration::from_millis(3500),
            hard_timeout: Duration::from_secs(8),
            clock: WaitClock::Wall,
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

/// Facts carried out of a successful submission acknowledgement.
#[derive(Clone, Copy)]
struct Acknowledgement {
    production_seen: bool,
    via: &'static str,
}

enum SubmissionDecision {
    Pending,
    Acknowledged(Acknowledgement),
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
        SubmissionDecision::Acknowledged(Acknowledgement {
            production_seen,
            via: if acknowledged {
                "prompt-stable"
            } else if generically_stable {
                "generic-stability"
            } else {
                "redraw-timeout"
            },
        })
    } else {
        SubmissionDecision::Pending
    }
}

enum WaitPhase {
    Submitting(SubmissionState),
    Settling(SettlingState),
}

/// Post-acknowledgement convergence state. Lives only inside
/// [`WaitPhase::Settling`], so the settling facts cannot survive a revert
/// to the submission phase or leak into it.
struct SettlingState {
    /// Frame history since the acknowledgement; owns the quiet-window
    /// evidence so a revert drops it automatically with the variant.
    tracker: FrameTracker,
    /// Whether any production evidence has been observed (at or since the
    /// acknowledgement); selects the quiet-window class.
    production_seen: bool,
    /// A full quiet window has already elapsed once; the next stable poll
    /// past the window confirms convergence.
    threshold_seen: bool,
    /// When the frame last visibly changed.
    last_visible_change: Duration,
    /// Last quiet-window class logged, to log transitions exactly once.
    logged_quiet_class: Option<&'static str>,
    /// The first post-acknowledgement production change logs at debug;
    /// later ones drop to trace.
    post_ack_production_logged: bool,
    /// Busy-marker facts, owned here like the rest of the settling
    /// evidence so a revert drops them with the variant.
    busy_marker: BusyMarkerState,
}

/// Busy-marker observations folded into the settling state.
struct BusyMarkerState {
    /// A marker has been visible at some point in this settling; arms the
    /// post-busy quiet-window floor.
    seen: bool,
    /// Marker visibility on the previous poll, to log transitions once.
    visible: bool,
}

impl SettlingState {
    /// Quiet window for the current evidence, floored by the post-busy
    /// window once a busy marker has been seen.
    fn quiet_window(&self, options: &WaitOptions) -> (Duration, &'static str) {
        let (window, class) = self.tracker.quiet_window(options, self.production_seen);
        if self.busy_marker.seen && options.post_busy_quiet_window > window {
            (options.post_busy_quiet_window, "post-busy")
        } else {
            (window, class)
        }
    }
}

#[derive(Clone, Copy)]
struct SettlingObservation<'a> {
    changed: bool,
    frame: &'a str,
    pre_submit: &'a str,
    /// A busy marker is visible near the bottom of `frame`.
    busy_visible: bool,
    observed_at: Duration,
}

/// Reset the tracker on the acknowledged frame, log the transition, and
/// build the settling state that [`WaitPhase::Settling`] carries.
fn enter_settling(
    last_frame: &str,
    ack: Acknowledgement,
    busy_visible: bool,
    observed_at: Duration,
    options: &WaitOptions,
    agent_id: &str,
) -> SettlingState {
    let state = SettlingState {
        tracker: FrameTracker::new(last_frame.to_string()),
        // A visible busy marker is production evidence: the TUI is working.
        production_seen: ack.production_seen || busy_visible,
        threshold_seen: false,
        last_visible_change: observed_at,
        logged_quiet_class: None,
        post_ack_production_logged: false,
        busy_marker: BusyMarkerState {
            seen: busy_visible,
            visible: busy_visible,
        },
    };
    let (window, class) = state.quiet_window(options);
    tracing::debug!(
        agent = agent_id,
        elapsed = ?observed_at,
        production_seen = state.production_seen,
        via = ack.via,
        busy_visible,
        quiet_window = ?window,
        class,
        "submission acknowledged, settling"
    );
    SettlingState {
        logged_quiet_class: Some(class),
        ..state
    }
}

enum SettlingDecision {
    /// Keep settling.
    Pending,
    /// The frame reverted to the exact pre-submit image: the
    /// acknowledgement is invalidated and submission re-arms.
    Reverted,
    /// Stable past the quiet window twice: the output has converged.
    Converged,
}

/// Settling peer of [`observe_submission`]: fold one post-acknowledgement
/// poll into the settling state and decide whether the pane converged,
/// reverted, or needs more polls.
fn observe_settling(
    state: &mut SettlingState,
    observation: SettlingObservation<'_>,
    options: &WaitOptions,
    agent_id: &str,
) -> SettlingDecision {
    if observation.busy_visible != state.busy_marker.visible {
        tracing::debug!(
            agent = agent_id,
            elapsed = ?observation.observed_at,
            visible = observation.busy_visible,
            "busy marker visibility changed"
        );
        state.busy_marker.visible = observation.busy_visible;
    }
    if observation.busy_visible {
        state.busy_marker.seen = true;
    }
    if observation.changed {
        if observation.frame == observation.pre_submit {
            // Returning to the exact pre-submit image invalidates the
            // acknowledgement (the settling state, evidence included, is
            // dropped with the variant). Stay conservative and wait for a
            // durable submission transition instead of reporting the idle
            // pane.
            tracing::debug!(
                agent = agent_id,
                elapsed = ?observation.observed_at,
                "frame reverted to pre-submit image, re-waiting for submission"
            );
            return SettlingDecision::Reverted;
        }
        if state.post_ack_production_logged {
            tracing::trace!(agent = agent_id, elapsed = ?observation.observed_at, "frame changed");
        } else {
            // First production after acknowledgement logs at debug so
            // poll-aliased activity and one-frame replies are
            // distinguishable without trace volume.
            tracing::debug!(
                agent = agent_id,
                elapsed = ?observation.observed_at,
                "production evidence after acknowledgement"
            );
            state.post_ack_production_logged = true;
        }
        state.production_seen = true;
        state.tracker.observe_change(observation.frame);
        state.last_visible_change = observation.observed_at;
        state.threshold_seen = false;
        return SettlingDecision::Pending;
    }

    state.tracker.observe_stable(observation.frame);
    if observation.busy_visible {
        // The TUI says it is still working: a frozen frame with a visible
        // busy marker must never count toward any quiet window.
        state.production_seen = true;
        state.last_visible_change = observation.observed_at;
        state.threshold_seen = false;
        return SettlingDecision::Pending;
    }
    let (quiet_window, quiet_class) = state.quiet_window(options);
    if state.logged_quiet_class != Some(quiet_class) {
        tracing::debug!(
            agent = agent_id,
            elapsed = ?observation.observed_at,
            quiet_window = ?quiet_window,
            class = quiet_class,
            "settling quiet window changed"
        );
        state.logged_quiet_class = Some(quiet_class);
    }
    if observation
        .observed_at
        .saturating_sub(state.last_visible_change)
        < quiet_window
    {
        return SettlingDecision::Pending;
    }
    if state.threshold_seen {
        tracing::debug!(
            agent = agent_id,
            elapsed = ?observation.observed_at,
            "wait converged on stable output"
        );
        return SettlingDecision::Converged;
    }
    tracing::debug!(
        agent = agent_id,
        elapsed = ?observation.observed_at,
        quiet_window = ?quiet_window,
        class = quiet_class,
        production_seen = state.production_seen,
        "quiet window satisfied, confirming stability"
    );
    state.threshold_seen = true;
    SettlingDecision::Pending
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

    /// The quiet window matching the current evidence, with its class
    /// name for observability.
    fn quiet_window(
        &self,
        options: &WaitOptions,
        production_seen: bool,
    ) -> (Duration, &'static str) {
        if self.has_strong_evidence() {
            (options.normal_quiet_window, "normal")
        } else if production_seen {
            (options.low_confidence_quiet_window, "low-confidence")
        } else {
            (options.submission_only_quiet_window, "submission-only")
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
        tracing::debug!(agent = agent_id, "pane already dead at wait entry");
        return Err(KiraMuxError::PaneDiedDuringWait(agent_id.to_string()).into());
    }

    tracing::debug!(
        agent = agent_id,
        pane = %seed.delivered.pane_id,
        capture_lines = seed.capture_lines,
        "wait started"
    );
    let wall_start = Instant::now();
    let pre_submit = normalize_frame(&seed.pre_submit);
    let prompt_fragments = prompt_fragments(&seed.delivered.rendered);
    let pre_submit_search = normalize_search_text(&seed.pre_submit);
    let mut last_capture = seed.pre_submit.clone();
    let mut last_frame = pre_submit.clone();
    let mut phase = WaitPhase::Submitting(SubmissionState::new(Duration::ZERO));

    loop {
        let now = options.elapsed(wall_start);
        if now >= options.hard_timeout {
            tracing::debug!(
                agent = agent_id,
                elapsed = ?now,
                phase = match &phase {
                    WaitPhase::Submitting(_) => "submitting",
                    WaitPhase::Settling(_) => "settling",
                },
                "wait hard timeout"
            );
            return Err(KiraMuxError::WaitTimeout {
                agent_id: agent_id.to_string(),
                last_capture,
            }
            .into());
        }
        let remaining = options.hard_timeout.saturating_sub(now);
        options.sleep(options.poll_interval.min(remaining));

        if pane_is_dead(tmux, &seed.delivered.pane_id)? {
            tracing::debug!(
                agent = agent_id,
                elapsed = ?options.elapsed(wall_start),
                "pane died during wait"
            );
            return Err(KiraMuxError::PaneDiedDuringWait(agent_id.to_string()).into());
        }

        let current = capture_or_died(tmux, agent_id, &seed.delivered.pane_id, seed.capture_lines)?;
        let observed_at = options.elapsed(wall_start);
        let changed = frame_changed(&current, &last_capture, &mut last_frame);
        last_capture = current;
        let busy_visible = busy_marker_visible(&last_frame, &seed.busy_markers);

        phase = match phase {
            WaitPhase::Submitting(mut submission) => {
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
                match observe_submission(&mut submission, observation, options) {
                    SubmissionDecision::Pending => WaitPhase::Submitting(submission),
                    SubmissionDecision::Acknowledged(ack) => WaitPhase::Settling(enter_settling(
                        &last_frame,
                        ack,
                        busy_visible,
                        observed_at,
                        options,
                        agent_id,
                    )),
                }
            }
            WaitPhase::Settling(mut settling) => {
                let observation = SettlingObservation {
                    changed,
                    frame: &last_frame,
                    pre_submit: &pre_submit,
                    busy_visible,
                    observed_at,
                };
                match observe_settling(&mut settling, observation, options, agent_id) {
                    SettlingDecision::Pending => WaitPhase::Settling(settling),
                    SettlingDecision::Reverted => {
                        WaitPhase::Submitting(SubmissionState::new(observed_at))
                    }
                    SettlingDecision::Converged => return Ok(last_capture),
                }
            }
        };
    }
}

/// Normalize the capture and report whether the visible frame moved,
/// updating `last_frame` when it did.
fn frame_changed(current: &str, last_capture: &str, last_frame: &mut String) -> bool {
    // Byte-identical captures normalize identically: skip the allocation.
    if current == last_capture {
        return false;
    }
    let current_frame = normalize_frame(current);
    if current_frame == *last_frame {
        return false;
    }
    *last_frame = current_frame;
    true
}

/// True when any marker fragment appears (case-insensitively) in the bottom
/// [`BUSY_MARKER_SCAN_LINES`] lines of the normalized frame. Narrow panes
/// can truncate a marker out of the status line; a miss only falls back to
/// plain frame-diff convergence.
fn busy_marker_visible(frame: &str, markers: &[String]) -> bool {
    if markers.is_empty() {
        return false;
    }
    frame
        .lines()
        .rev()
        .take(BUSY_MARKER_SCAN_LINES)
        .any(|line| {
            let line = line.to_lowercase();
            markers.iter().any(|marker| line.contains(marker.as_str()))
        })
}

fn normalize_frame(capture: &str) -> String {
    let mut lines: Vec<&str> = capture.lines().map(str::trim_end).collect();
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
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
fn capture_or_died(
    tmux: &dyn TmuxAdapter,
    agent_id: &str,
    pane_id: &str,
    capture_lines: usize,
) -> Result<String> {
    or_unavailable(tmux.capture_pane(pane_id, capture_lines), || {
        Err(KiraMuxError::PaneDiedDuringWait(agent_id.to_string()).into())
    })
}

/// A vanished pane (killed window / missing target), a lost session, or a
/// stopped tmux server all count as dead for the wait loop so callers get a
/// typed exit 6 rather than an untyped transport failure — the same
/// `is_target_unavailable` classification the send path uses.
fn pane_is_dead(tmux: &dyn TmuxAdapter, pane_id: &str) -> Result<bool> {
    or_unavailable(
        find_pane(tmux, pane_id).map(|pane| pane.is_none_or(|pane| pane.pane_dead)),
        || Ok(true),
    )
}

#[cfg(test)]
mod tests;
