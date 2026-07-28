//! Self-tests for `FakeTmux` scripted behaviour.

use super::*;
use crate::test_support::TestResultExt;
use crate::tmux::TmuxAdapter;

#[test]
fn respawn_pane_records_operation() {
    let fake = FakeTmux::new();
    fake.add_session("s");
    fake.add_window("s", "agents");
    fake.add_pane("s", "agents", "%0", false);
    let env = vec![("FOO".to_string(), "bar".to_string())];
    let command = vec![
        "codex".to_string(),
        "--profile".to_string(),
        "fast".to_string(),
    ];
    fake.respawn_pane("%0", "/tmp", &env, &command)
        .or_panic("respawn_pane_records_operation");

    let ops = fake.ops();
    assert_eq!(
        ops,
        vec![FakeOp::RespawnPane {
            pane_id: "%0".to_string(),
            cwd: "/tmp".to_string(),
            env: vec![("FOO".to_string(), "bar".to_string())],
            command: vec![
                "codex".to_string(),
                "--profile".to_string(),
                "fast".to_string(),
            ],
        }]
    );
}

#[test]
fn respawn_pane_unknown_returns_missing_target() {
    let fake = FakeTmux::new();
    let error = fake
        .respawn_pane("%99", "/tmp", &[], &["true".into()])
        .err_or_panic("respawn_pane_unknown_returns_missing_target: expected Err");
    assert!(matches!(
        error.downcast_ref::<TmuxError>(),
        Some(TmuxError::MissingTarget(_))
    ));
    assert!(fake.ops().is_empty());
}

#[test]
fn delivery_ops_reject_vanished_pane() {
    // Real tmux refuses paste/send to a pane that does not exist; the fake
    // must too, or submit/wait tests validate impossible states.
    let fake = FakeTmux::new();
    fake.add_session("s");
    fake.add_window("s", "w");

    let paste = fake
        .paste_text("%0", "x")
        .err_or_panic("delivery_ops_reject_vanished_pane: paste");
    assert!(matches!(
        paste.downcast_ref::<TmuxError>(),
        Some(TmuxError::MissingTarget(_))
    ));
    let send_text = fake
        .send_text("%0", "x")
        .err_or_panic("delivery_ops_reject_vanished_pane: send_text");
    assert!(matches!(
        send_text.downcast_ref::<TmuxError>(),
        Some(TmuxError::MissingTarget(_))
    ));
    let send_keys = fake
        .send_keys("%0", &["Enter"])
        .err_or_panic("delivery_ops_reject_vanished_pane: send_keys");
    assert!(matches!(
        send_keys.downcast_ref::<TmuxError>(),
        Some(TmuxError::MissingTarget(_))
    ));
    assert!(fake.ops().is_empty());
}

#[test]
fn delivered_text_echoes_minimally_unless_a_response_is_declared() {
    // The fake must not ship a universal always-converges response frame:
    // tests that need visible agent output declare it explicitly.
    let fake = FakeTmux::new();
    fake.add_session("s");
    fake.add_window("s", "w");
    fake.add_pane("s", "w", "%0", false);

    fake.paste_text("%0", "hello")
        .or_panic("delivered_text_echoes_minimally: paste");
    let echoed = fake
        .capture_pane("%0", 50)
        .or_panic("delivered_text_echoes_minimally: capture");
    assert!(echoed.contains("hello"), "got: {echoed}");
    assert!(
        !echoed.contains("streaming a response"),
        "no implicit agent response frame: {echoed}"
    );

    fake.set_delivery_response("agent replied with a full answer");
    fake.paste_text("%0", "second prompt")
        .or_panic("delivered_text_echoes_minimally: paste 2");
    let responded = fake
        .capture_pane("%0", 50)
        .or_panic("delivered_text_echoes_minimally: capture 2");
    assert!(
        responded.contains("agent replied with a full answer"),
        "declared response frame must render: {responded}"
    );
}

#[test]
fn vanished_pane_wins_over_transient_capture_knob() {
    let fake = FakeTmux::new();
    fake.add_session("s");
    fake.add_window("s", "w");
    fake.set_fail_capture(true);

    let error = fake
        .capture_pane("%0", 50)
        .err_or_panic("vanished_pane_wins_over_transient_capture_knob: expected Err");
    assert!(
        matches!(
            error.downcast_ref::<TmuxError>(),
            Some(TmuxError::MissingTarget(_))
        ),
        "a vanished pane must classify as MissingTarget, got: {error}"
    );
}

#[test]
fn stopped_server_reports_no_sessions() {
    let fake = FakeTmux::new();
    fake.add_session("s");
    fake.set_no_server(true);

    let exists = fake
        .session_exists("s")
        .or_panic("stopped_server_reports_no_sessions");
    assert!(
        !exists,
        "a stopped server must report absence, not an error"
    );
}

#[test]
fn delivery_ops_reject_stopped_server() {
    let fake = FakeTmux::new();
    fake.add_session("s");
    fake.add_window("s", "w");
    fake.add_pane("s", "w", "%0", false);
    fake.set_no_server(true);

    let error = fake
        .paste_text("%0", "x")
        .err_or_panic("delivery_ops_reject_stopped_server: expected Err");
    assert!(matches!(
        error.downcast_ref::<TmuxError>(),
        Some(TmuxError::NoServer(_))
    ));
    assert!(fake.ops().is_empty());
}

#[test]
fn kill_session_missing_returns_missing_session() {
    let fake = FakeTmux::new();
    let error = fake
        .kill_session("gone")
        .err_or_panic("kill_session_missing_returns_missing_session: expected Err");
    assert!(matches!(
        error.downcast_ref::<TmuxError>(),
        Some(TmuxError::MissingSession(_))
    ));
}

#[test]
fn split_window_missing_window_returns_missing_target() {
    let fake = FakeTmux::new();
    fake.add_session("s");
    let error = fake
        .split_window("s:agents", "/tmp")
        .err_or_panic("split_window_missing_window_returns_missing_target: expected Err");
    assert!(matches!(
        error.downcast_ref::<TmuxError>(),
        Some(TmuxError::MissingTarget(_))
    ));
}

#[test]
fn set_session_option_missing_returns_missing_session() {
    let fake = FakeTmux::new();
    let error = fake
        .set_session_option("gone", "@k", "v")
        .err_or_panic("set_session_option_missing_returns_missing_session: expected Err");
    assert!(matches!(
        error.downcast_ref::<TmuxError>(),
        Some(TmuxError::MissingSession(_))
    ));
}

#[test]
fn attach_session_missing_is_status_only_not_typed() {
    let fake = FakeTmux::new();
    let error = fake
        .attach_session("gone")
        .err_or_panic("attach_session_missing_is_status_only_not_typed: expected Err");
    // Real interactive attach inherits stderr and only reports a status;
    // typed MissingSession/NoServer would overstate the transport contract.
    assert!(
        error.downcast_ref::<TmuxError>().is_none(),
        "interactive attach must stay status-only, got typed: {error}"
    );
    assert!(
        error
            .to_string()
            .contains("tmux command failed with status"),
        "expected status-only message, got: {error}"
    );
}

#[test]
fn switch_client_missing_is_status_only_not_typed() {
    let fake = FakeTmux::new();
    let error = fake
        .switch_client("gone")
        .err_or_panic("switch_client_missing_is_status_only_not_typed: expected Err");
    assert!(
        error.downcast_ref::<TmuxError>().is_none(),
        "interactive switch must stay status-only, got typed: {error}"
    );
}

#[test]
fn select_layout_missing_window_returns_missing_target() {
    let fake = FakeTmux::new();
    fake.add_session("s");
    let error = fake
        .select_layout("s:agents", "even-vertical")
        .err_or_panic("select_layout_missing_window_returns_missing_target: expected Err");
    assert!(matches!(
        error.downcast_ref::<TmuxError>(),
        Some(TmuxError::MissingTarget(_))
    ));
}

#[test]
fn list_panes_missing_session_returns_missing_session() {
    let fake = FakeTmux::new();
    let error = fake
        .list_panes("gone:agents")
        .err_or_panic("list_panes_missing_session_returns_missing_session: expected Err");
    assert!(matches!(
        error.downcast_ref::<TmuxError>(),
        Some(TmuxError::MissingSession(_))
    ));
}

#[test]
fn set_pane_option_no_server_returns_no_server() {
    let fake = FakeTmux::new();
    fake.add_session("s");
    fake.add_window("s", "agents");
    fake.add_pane("s", "agents", "%0", false);
    fake.set_no_server(true);

    let error = fake
        .set_pane_option("%0", "@k", "v")
        .err_or_panic("set_pane_option_no_server_returns_no_server: expected Err");
    assert!(matches!(
        error.downcast_ref::<TmuxError>(),
        Some(TmuxError::NoServer(_))
    ));
}

#[test]
fn get_pane_option_no_server_returns_no_server() {
    let fake = FakeTmux::new();
    fake.add_session("s");
    fake.add_window("s", "agents");
    fake.add_pane("s", "agents", "%0", false);
    fake.set_no_server(true);

    let error = fake
        .get_pane_option("%0", "@k")
        .err_or_panic("get_pane_option_no_server_returns_no_server: expected Err");
    assert!(matches!(
        error.downcast_ref::<TmuxError>(),
        Some(TmuxError::NoServer(_))
    ));
}

#[test]
fn set_pane_option_missing_returns_missing_target() {
    let fake = FakeTmux::new();
    let error = fake
        .set_pane_option("%99", "@k", "v")
        .err_or_panic("set_pane_option_missing_returns_missing_target: expected Err");
    assert!(matches!(
        error.downcast_ref::<TmuxError>(),
        Some(TmuxError::MissingTarget(_))
    ));
}
