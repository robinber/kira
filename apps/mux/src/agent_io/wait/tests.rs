use super::super::send::DeliveredPrompt;
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
    let seed = WaitSeed {
        delivered: DeliveredPrompt {
            pane_id: pane.pane_id,
            rendered: "prompt echo".to_string(),
        },
        pre_submit: "ready".to_string(),
    };
    wait_on_pane(tmux, agent_id, &seed, options)
}

/// Millisecond-scale knobs so timeout paths stay fast; each quiet window
/// spans several polls, mirroring the production ratios. Virtual time
/// means these tests do not burn wall-clock sleeps.
fn fast_options() -> WaitOptions {
    WaitOptions {
        poll_interval: Duration::from_millis(1),
        submission_stability: Duration::from_millis(3),
        submission_timeout: Duration::from_millis(8),
        normal_quiet_window: Duration::from_millis(10),
        low_confidence_quiet_window: Duration::from_millis(20),
        submission_only_quiet_window: Duration::from_millis(40),
        hard_timeout: Duration::from_millis(250),
        clock: WaitClock::Virtual(Mutex::new(Duration::ZERO)),
    }
}

#[test]
fn wait_returns_final_capture_once_pane_stabilizes_after_activity() {
    let fake = crate::test_support::FakeTmux::new();
    let project = crate::test_support::test_project();
    crate::test_support::setup_healthy_session(&fake, &project);
    // The pre-submit baseline is carried by WaitSeed; captures start with
    // the submission echo, then the pane streams and freezes.
    fake.queue_pane_contents(
        "%0",
        &[
            "prompt echo",
            "prompt echo\nthinking...",
            "prompt echo\nthinking...\nanswer: 42",
        ],
    );

    let options = fast_options();
    let output = wait_for_stable_output(&fake, &project, "alpha", &options).or_panic();

    assert_eq!(output, "prompt echo\nthinking...\nanswer: 42");
    assert!(
        options.elapsed(Instant::now()) < Duration::from_millis(40),
        "a frame after the prompt acknowledgement must count as production"
    );
}

#[test]
fn wait_times_out_when_pane_never_changes() {
    let fake = crate::test_support::FakeTmux::new();
    let project = crate::test_support::test_project();
    crate::test_support::setup_healthy_session(&fake, &project);
    // Quiet from the start: no submission echo or response activity.
    fake.set_pane_content("%0", "ready");

    let err = wait_for_stable_output(&fake, &project, "alpha", &fast_options()).err_or_panic();

    assert!(
        matches!(
            err.downcast_ref::<KiraMuxError>(),
            Some(KiraMuxError::WaitTimeout { agent_id, last_capture })
                if agent_id == "alpha" && last_capture == "ready"
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
    // Quiet windows are unreachable; death must win before hard timeout.
    let options = WaitOptions {
        normal_quiet_window: Duration::from_secs(30),
        low_confidence_quiet_window: Duration::from_secs(30),
        submission_only_quiet_window: Duration::from_secs(30),
        ..fast_options()
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
fn wait_fails_when_pane_vanishes_mid_wait() {
    let fake = crate::test_support::FakeTmux::new();
    let project = crate::test_support::test_project();
    crate::test_support::setup_healthy_session(&fake, &project);
    // Baseline capture, then the pane is gone (killed window): list_panes
    // returns MissingTarget, which must map to PaneDiedDuringWait.
    fake.queue_pane_contents("%0", &["prompt echo"]);
    fake.set_pane_removed_after_captures("%0", 1);
    let options = WaitOptions {
        normal_quiet_window: Duration::from_secs(30),
        low_confidence_quiet_window: Duration::from_secs(30),
        submission_only_quiet_window: Duration::from_secs(30),
        ..fast_options()
    };

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
    let seed = WaitSeed {
        delivered: DeliveredPrompt {
            pane_id: "%0".to_string(),
            rendered: "prompt echo".to_string(),
        },
        pre_submit: "ready".to_string(),
    };
    let output = wait_on_pane(&fake, "alpha", &seed, &fast_options()).or_panic();
    assert_eq!(output, "prompt echo\nreply");
}

#[test]
fn delayed_answer_after_prompt_echo_is_not_returned_early() {
    let fake = crate::test_support::FakeTmux::new();
    let project = crate::test_support::test_project();
    crate::test_support::setup_healthy_session(&fake, &project);
    // 25 echo-only polls put the silence past the low-confidence window
    // (20 polls): only the submission-only window may cover an
    // echo-acknowledged pane, so the late answer must still be captured.
    let mut frames = vec!["prompt echo"; 25];
    frames.push("prompt echo\nanswer after silent thinking");
    fake.queue_pane_contents("%0", &frames);

    let output = wait_for_stable_output(&fake, &project, "alpha", &fast_options()).or_panic();

    assert_eq!(output, "prompt echo\nanswer after silent thinking");
}

#[test]
fn one_frame_response_waits_for_the_submission_only_window() {
    let fake = crate::test_support::FakeTmux::new();
    let project = crate::test_support::test_project();
    crate::test_support::setup_healthy_session(&fake, &project);
    fake.queue_pane_contents("%0", &["prompt echo\nanswer: 42"]);
    let options = fast_options();

    let output = wait_for_stable_output(&fake, &project, "alpha", &options).or_panic();

    assert_eq!(output, "prompt echo\nanswer: 42");
    // A pane that never changes again after the submission
    // acknowledgement is indistinguishable from a silently thinking
    // model: convergence must wait out the submission-only window, not
    // the shorter low-confidence one.
    let elapsed = options.elapsed(Instant::now());
    assert!(
        elapsed >= Duration::from_millis(41),
        "one-frame reply converged after only {elapsed:?}"
    );
}

#[test]
fn prompt_rendered_after_submission_timeout_still_uses_submission_only_window() {
    let fake = crate::test_support::FakeTmux::new();
    let project = crate::test_support::test_project();
    crate::test_support::setup_healthy_session(&fake, &project);
    let mut frames = vec!["ready"; 10];
    frames.push("prompt echo");
    fake.queue_pane_contents("%0", &frames);
    let options = fast_options();

    let output = wait_for_stable_output(&fake, &project, "alpha", &options).or_panic();

    assert_eq!(output, "prompt echo");
    assert!(
        options.elapsed(Instant::now()) >= Duration::from_millis(52),
        "a late prompt echo must not be demoted to weak production"
    );
}

#[test]
fn placeholder_submission_uses_generic_stability_fallback() {
    let fake = crate::test_support::FakeTmux::new();
    let project = crate::test_support::test_project();
    crate::test_support::setup_healthy_session(&fake, &project);
    fake.queue_pane_contents(
        "%0",
        &[
            "[Pasted text #1]",
            "[Pasted text #1]",
            "[Pasted text #1]",
            "[Pasted text #1]",
            "[Pasted text #1]\nanswer",
        ],
    );
    // No prompt fragment ever appears, so the placeholder echo must exit
    // the submission phase through the micro-stability path. With the
    // submission timeout pushed past the hard timeout, that fallback is
    // the only route to success — this pins the generically_stable
    // branch, which no other test reaches.
    let options = WaitOptions {
        submission_timeout: Duration::from_millis(300),
        ..fast_options()
    };

    let output = wait_for_stable_output(&fake, &project, "alpha", &options).or_panic();

    assert_eq!(output, "[Pasted text #1]\nanswer");
}

#[test]
fn swallowed_prompt_reverting_to_pre_submit_times_out() {
    let fake = crate::test_support::FakeTmux::new();
    let project = crate::test_support::test_project();
    crate::test_support::setup_healthy_session(&fake, &project);
    // The pane renders the exact prompt, then the TUI discards it and
    // redraws the pre-submit screen. Even the prompt accelerator must
    // require durability instead of reporting the idle pane as success.
    fake.queue_pane_contents("%0", &["prompt echo", "ready"]);

    let err = wait_for_stable_output(&fake, &project, "alpha", &fast_options()).err_or_panic();

    assert!(
        matches!(
            err.downcast_ref::<KiraMuxError>(),
            Some(KiraMuxError::WaitTimeout { agent_id, last_capture })
                if agent_id == "alpha" && last_capture == "ready"
        ),
        "a reverted submission must not converge, got: {err}"
    );
}

#[test]
fn cyclic_spinner_keeps_resetting_settling() {
    let fake = crate::test_support::FakeTmux::new();
    let project = crate::test_support::test_project();
    crate::test_support::setup_healthy_session(&fake, &project);
    let mut frames = vec!["prompt echo"];
    for index in 0..30 {
        frames.push(if index % 2 == 0 {
            "prompt echo\nworking /"
        } else {
            "prompt echo\nworking -"
        });
    }
    frames.push("prompt echo\nanswer complete");
    fake.queue_pane_contents("%0", &frames);

    let output = wait_for_stable_output(&fake, &project, "alpha", &fast_options()).or_panic();

    assert_eq!(output, "prompt echo\nanswer complete");
}

#[test]
fn unique_late_redraw_cancels_confirmation() {
    let fake = crate::test_support::FakeTmux::new();
    let project = crate::test_support::test_project();
    crate::test_support::setup_healthy_session(&fake, &project);
    let answer = "prompt echo\nchunk\nanswer";
    let redraw = "prompt echo\nchunk\nanswer\nusage: 12 tokens";
    let mut frames = vec!["prompt echo", "prompt echo\nchunk", answer];
    frames.extend(std::iter::repeat_n(answer, 10));
    frames.push(redraw);
    fake.queue_pane_contents("%0", &frames);

    let output = wait_for_stable_output(&fake, &project, "alpha", &fast_options()).or_panic();

    assert_eq!(output, redraw);
}

#[test]
fn repeated_spinner_frames_are_not_material_evidence() {
    let mut tracker = FrameTracker::new("prompt".to_string());
    tracker.observe_change("working /");
    tracker.observe_stable("working /");
    tracker.observe_change("working -");
    tracker.observe_stable("working -");
    assert!(tracker.has_strong_evidence());

    tracker.observe_change("working /");
    tracker.observe_change("working -");

    assert!(!tracker.has_strong_evidence());
}

#[test]
fn recurring_frame_is_conservatively_removed_from_material_evidence() {
    let mut tracker = FrameTracker::new("prompt".to_string());
    tracker.observe_change("chunk");
    tracker.observe_change("answer");
    tracker.observe_stable("answer");
    assert!(tracker.has_strong_evidence());

    // Captures cannot prove whether this is a cosmetic flicker or a slow
    // cycle. Prefer the longer quiet window: extra latency is safer than a
    // truncated response.
    tracker.observe_change("notification flash");
    tracker.observe_change("answer");

    assert!(!tracker.has_strong_evidence());
}

#[test]
fn frame_normalization_ignores_trailing_redraw_whitespace() {
    assert_eq!(
        normalize_frame("answer   \nstatus\t\n\n"),
        normalize_frame("answer\nstatus")
    );
}

#[test]
fn prompt_detection_tolerates_wrapping() {
    let prompt = "review the authentication module and report only concrete findings";
    let fragments = prompt_fragments(prompt);
    assert!(prompt_appeared(
        "agent ready",
        &normalize_search_text(
            "agent ready review the authentication\nmodule and report only concrete findings",
        ),
        &fragments,
    ));
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
fn wait_fails_when_server_stops_mid_wait() {
    let fake = crate::test_support::FakeTmux::new();
    let project = crate::test_support::test_project();
    crate::test_support::setup_healthy_session(&fake, &project);
    fake.queue_pane_contents("%0", &["prompt echo"]);
    // Server gone after the second capture: the next liveness check sees
    // NoServer, which must classify as death (typed exit 6), mirroring
    // the send-side `is_target_unavailable` mapping.
    fake.set_server_stops_after_captures(2);

    let err = wait_for_stable_output(&fake, &project, "alpha", &fast_options()).err_or_panic();

    assert!(
        matches!(
            err.downcast_ref::<KiraMuxError>(),
            Some(KiraMuxError::PaneDiedDuringWait(id)) if id == "alpha"
        ),
        "server loss mid-wait must be typed PaneDiedDuringWait, got: {err}"
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
