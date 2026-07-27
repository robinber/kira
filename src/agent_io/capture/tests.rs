//! Unit tests for plain and deep pane capture.

use super::super::deep_capture::{DeepCaptureOptions, deep_capture};
use super::super::lock;
use super::super::resolve::resolve_managed_pane;
use super::*;
use crate::model::ResolvedProject;
use crate::test_support::{FakeOp, err, ok};
use crate::tmux::TmuxAdapter;

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
