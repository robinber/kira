use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::{ExitStatus, Output};

use super::{
    TmuxClient, escape_trailing_semicolon, failed_tmux_status, failed_tmux_stdin_status,
    parse_display_message_line, parse_workspace_pane_line,
};
use crate::test_support::{TestOptionExt, TestResultExt};
use crate::tmux::{PaneInfo, TmuxAdapter, TmuxError};

fn failed_output(stderr: &str) -> Output {
    Output {
        status: ExitStatus::from_raw(256),
        stdout: Vec::new(),
        stderr: stderr.as_bytes().to_vec(),
    }
}

#[test]
fn escape_trailing_semicolon_escapes_final_separator() {
    assert_eq!(escape_trailing_semicolon("echo hi;"), "echo hi\\;");
}

#[test]
fn escape_trailing_semicolon_leaves_interior_semicolons() {
    assert_eq!(escape_trailing_semicolon("a; b"), "a; b");
    assert_eq!(escape_trailing_semicolon("plain"), "plain");
}

#[test]
fn parse_display_message_line_splits_tab_fields() {
    let metadata = parse_display_message_line("fp\tproj\tprof\n");

    assert_eq!(metadata.fingerprint.as_deref(), Some("fp"));
    assert_eq!(metadata.project_id.as_deref(), Some("proj"));
    assert_eq!(metadata.profile_id.as_deref(), Some("prof"));
}

#[test]
fn parse_display_message_line_maps_empty_fields_to_none() {
    let metadata = parse_display_message_line("\t\t\n");

    assert_eq!(metadata.fingerprint, None);
    assert_eq!(metadata.project_id, None);
    assert_eq!(metadata.profile_id, None);
}

#[test]
fn parse_workspace_pane_line_reads_full_metadata() {
    let (pane, role) = parse_workspace_pane_line("%5\t1\t137\talpha\tagents")
        .or_panic("parse_workspace_pane_line_reads_full_metadata");

    assert_eq!(pane.pane.pane_id, "%5");
    assert!(pane.pane.pane_dead);
    assert_eq!(pane.pane.pane_dead_status, Some(137));
    assert_eq!(pane.agent_id.as_deref(), Some("alpha"));
    assert_eq!(role.as_deref(), Some("agents"));
}

#[test]
fn parse_workspace_pane_line_maps_empty_options_to_none() {
    let (pane, role) = parse_workspace_pane_line("%5\t0\t\t\t")
        .or_panic("parse_workspace_pane_line_maps_empty_options_to_none");

    assert!(!pane.pane.pane_dead);
    assert_eq!(pane.pane.pane_dead_status, None);
    assert_eq!(pane.agent_id, None);
    assert_eq!(role, None);
}

#[test]
fn workspace_snapshot_uses_three_commands_independent_of_pane_count() {
    for pane_count in [1, 12] {
        let (temp, client, log_path) = scripted_tmux(pane_count);

        let snapshot = client
            .workspace_snapshot("session", "agents")
            .or_panic("workspace_snapshot_uses_three_commands_independent_of_pane_count")
            .or_panic("workspace_snapshot_uses_three_commands_independent_of_pane_count");

        assert_eq!(
            snapshot
                .window
                .or_panic("workspace_snapshot_uses_three_commands_independent_of_pane_count")
                .panes
                .len(),
            pane_count,
            "unexpected pane count for script under {}",
            temp.path().display()
        );
        let calls = fs::read_to_string(&log_path)
            .or_panic("workspace_snapshot_uses_three_commands_independent_of_pane_count");
        assert_eq!(
            calls.lines().count(),
            3,
            "snapshot command count must not grow with {pane_count} panes: {calls}"
        );
    }
}

#[test]
fn workspace_snapshot_reports_a_missing_window() {
    let (_temp, client, _log_path) = scripted_tmux_with_list_failure("can't find window: agents");

    let snapshot = client
        .workspace_snapshot("session", "agents")
        .or_panic("workspace_snapshot_reports_a_missing_window")
        .or_panic("workspace_snapshot_reports_a_missing_window");

    assert_eq!(snapshot.window, None);
}

#[test]
fn workspace_snapshot_treats_a_vanished_session_as_absent() {
    let (_temp, client, _log_path) = scripted_tmux_with_list_failure("can't find session: session");

    let snapshot = client
        .workspace_snapshot("session", "agents")
        .or_panic("workspace_snapshot_treats_a_vanished_session_as_absent");

    assert_eq!(snapshot, None);
}

#[test]
fn workspace_snapshot_treats_a_stopped_server_as_absent() {
    let (_temp, client, _log_path) =
        scripted_tmux_with_list_failure("no server running on /tmp/tmux-1000/default");

    let snapshot = client
        .workspace_snapshot("session", "agents")
        .or_panic("workspace_snapshot_treats_a_stopped_server_as_absent");

    assert_eq!(snapshot, None);
}

#[test]
fn workspace_snapshot_propagates_a_generic_list_panes_failure() {
    // Use a token that cannot match missing-session/target/no-server
    // classifiers (and avoids shell metacharacters in the fake script).
    const MSG: &str = "kira-test-generic-list-panes-failure";
    let (_temp, client, _log_path) = scripted_tmux_with_list_failure(MSG);

    let error = client
        .workspace_snapshot("session", "agents")
        .err_or_panic("workspace_snapshot_propagates_a_generic_list_panes_failure: expected Err");

    match error.downcast_ref::<TmuxError>() {
        Some(TmuxError::CommandFailure(message)) if message == MSG => {}
        other => panic!("expected CommandFailure({MSG:?}), got {other:?} ({error})"),
    }
}

#[test]
fn workspace_snapshot_propagates_a_display_message_failure() {
    let (temp, client, log_path) =
        scripted_tmux_with_actions("printf '%s\\n' 'display failed' >&2\nexit 1", "exit 0");

    let error = client
        .workspace_snapshot("session", "agents")
        .err_or_panic("workspace_snapshot_propagates_a_display_message_failure: expected Err");

    assert!(matches!(
        error.downcast_ref::<TmuxError>(),
        Some(TmuxError::CommandFailure(message)) if message == "display failed"
    ));
    let calls = fs::read_to_string(&log_path)
        .or_panic("workspace_snapshot_propagates_a_display_message_failure");
    assert_eq!(
        calls.lines().count(),
        2,
        "list-panes should not run after display-message fails under {}: {calls}",
        temp.path().display()
    );
}

#[test]
fn failed_tmux_status_maps_missing_window_to_missing_target() {
    let error = failed_tmux_status("s:agents", &failed_output("can't find window: agents"));
    assert!(matches!(
        error.downcast_ref::<TmuxError>(),
        Some(TmuxError::MissingTarget(_))
    ));
}

#[test]
fn failed_tmux_status_maps_missing_session_to_missing_session() {
    let error = failed_tmux_status("s:agents", &failed_output("can't find session: s"));
    assert!(matches!(
        error.downcast_ref::<TmuxError>(),
        Some(TmuxError::MissingSession(_))
    ));
}

#[test]
fn failed_tmux_status_maps_generic_failure_to_command_failure() {
    let error = failed_tmux_status("s:agents", &failed_output("server unexpectedly closed"));
    assert!(matches!(
        error.downcast_ref::<TmuxError>(),
        Some(TmuxError::CommandFailure(_))
    ));
}

#[test]
fn failed_tmux_status_maps_no_server() {
    let error = failed_tmux_status(
        "s:agents",
        &failed_output("no server running on /tmp/tmux-1000/default"),
    );
    assert!(matches!(
        error.downcast_ref::<TmuxError>(),
        Some(TmuxError::NoServer(_))
    ));
}

#[test]
fn list_panes_maps_no_server_through_shared_classifier() {
    let (_temp, client, _log_path) =
        scripted_tmux_with_list_failure("no server running on /tmp/tmux-1000/default");

    let error = list_panes_after_publish(&client, "%0")
        .err_or_panic("list_panes_maps_no_server_through_shared_classifier: expected Err");
    assert!(
        matches!(
            error.downcast_ref::<TmuxError>(),
            Some(TmuxError::NoServer(_))
        ),
        "list_panes must type no-server like other tmux paths, got: {error}"
    );
}

#[test]
fn list_panes_maps_missing_target_through_shared_classifier() {
    let (_temp, client, _log_path) = scripted_tmux_with_list_failure("can't find window: agents");

    let error = list_panes_after_publish(&client, "s:agents")
        .err_or_panic("list_panes_maps_missing_target_through_shared_classifier: expected Err");
    assert!(matches!(
        error.downcast_ref::<TmuxError>(),
        Some(TmuxError::MissingTarget(_))
    ));
}

#[test]
fn failed_tmux_stdin_status_maps_no_server() {
    let error = failed_tmux_stdin_status(&failed_output(
        "no server running on /tmp/tmux-1000/default",
    ));
    assert!(matches!(
        error.downcast_ref::<TmuxError>(),
        Some(TmuxError::NoServer(_))
    ));
}

#[test]
fn failed_tmux_stdin_status_leaves_generic_failure_untyped() {
    let error = failed_tmux_stdin_status(&failed_output("load-buffer failed"));
    assert!(error.downcast_ref::<TmuxError>().is_none());
    assert_eq!(error.to_string(), "load-buffer failed");
}

fn scripted_tmux(pane_count: usize) -> (tempfile::TempDir, TmuxClient, PathBuf) {
    let pane_lines = (0..pane_count)
        .map(|index| {
            format!("printf '%s\\t%s\\t\\t%s\\t%s\\n' '%{index}' '0' 'agent-{index}' 'agents'")
        })
        .collect::<Vec<_>>()
        .join("\n");
    scripted_tmux_with_actions("printf 'fp\\tproject\\tprofile\\n'", &pane_lines)
}

fn scripted_tmux_with_list_failure(message: &str) -> (tempfile::TempDir, TmuxClient, PathBuf) {
    let list_action = format!("printf '%s\\n' \"{message}\" >&2\nexit 1");
    scripted_tmux_with_actions("printf 'fp\\tproject\\tprofile\\n'", &list_action)
}

fn scripted_tmux_with_actions(
    display_action: &str,
    list_action: &str,
) -> (tempfile::TempDir, TmuxClient, PathBuf) {
    let temp = tempfile::tempdir().or_panic("scripted_tmux_with_actions");
    let script_path = temp.path().join("tmux");
    let pending_script_path = temp.path().join("tmux.pending");
    let log_path = temp.path().join("calls.log");
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncase \"$1\" in\n  has-session) exit 0 ;;\n  display-message)\n{display_action}\n    ;;\n  list-panes)\n{list_action}\n    ;;\nesac\n",
        log_path.display()
    );
    fs::write(&pending_script_path, script).or_panic("scripted_tmux_with_actions");
    let mut permissions = fs::metadata(&pending_script_path)
        .or_panic("scripted_tmux_with_actions")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&pending_script_path, permissions).or_panic("scripted_tmux_with_actions");
    // Publish the executable only after its writer is closed. This avoids
    // transient ETXTBSY failures on Linux runners when tests start it.
    fs::rename(&pending_script_path, &script_path).or_panic("scripted_tmux_with_actions");
    // Durably settle the rename before concurrent tests spawn the binary.
    let published = fs::File::open(&script_path).or_panic("scripted_tmux_with_actions");
    published.sync_all().or_panic("scripted_tmux_with_actions");
    drop(published);
    let client = TmuxClient {
        tmux_bin: script_path.display().to_string(),
        socket_name: None,
    };
    (temp, client, log_path)
}

/// Retry a few times when the kernel still reports ETXTBSY after publish.
fn list_panes_after_publish(client: &TmuxClient, target: &str) -> anyhow::Result<Vec<PaneInfo>> {
    let mut last_error = None;
    for attempt in 0..8 {
        match client.list_panes(target) {
            Ok(panes) => return Ok(panes),
            Err(error) => {
                let spawn_race = error.to_string().contains("failed to run tmux command");
                if spawn_race && attempt + 1 < 8 {
                    last_error = Some(error);
                    std::thread::sleep(std::time::Duration::from_millis(5 * (attempt + 1)));
                    continue;
                }
                return Err(error);
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("list_panes retries exhausted")))
}
