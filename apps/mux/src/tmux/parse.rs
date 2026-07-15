use std::process::Output;

use anyhow::{Context, Result};

use super::adapter::PaneInfo;

pub(super) fn stdout_lines(output: &Output) -> Vec<String> {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect()
}

pub(super) fn parse_pane_line(line: &str) -> Result<PaneInfo> {
    let mut parts = line.splitn(3, '|');
    let pane_id = parts.next().context("missing pane_id")?.to_string();
    let pane_dead = parts.next().context("missing pane_dead")? == "1";
    let pane_dead_status = parts.next().and_then(|value| {
        if value.is_empty() {
            None
        } else {
            value.parse().ok()
        }
    });

    Ok(PaneInfo {
        pane_id,
        pane_dead,
        pane_dead_status,
    })
}

pub(super) fn command_error(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if stderr.is_empty() {
        format!("tmux command failed with status {}", output.status)
    } else {
        stderr
    }
}

pub(super) fn normalize_args<I, S>(args: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    args.into_iter()
        .map(|arg| arg.as_ref().to_string())
        .collect()
}

pub(super) fn is_missing_session_message(message: &str) -> bool {
    message.starts_with("can't find session") || message.starts_with("session not found")
}

/// Match tmux errors for a missing window or pane target (a missing session
/// also counts — the target's session component no longer resolves).
pub(super) fn is_missing_target_message(message: &str) -> bool {
    is_missing_session_message(message)
        || message.starts_with("can't find window")
        || message.starts_with("can't find pane")
        || message.starts_with("window not found")
}

pub(super) fn is_no_server_message(message: &str) -> bool {
    message.starts_with("no server running on ")
        || (message.starts_with("error connecting to ")
            && (message.ends_with("(No such file or directory)")
                || message.ends_with("(Connection refused)")))
}

pub(super) fn map_spawn_error(error: std::io::Error, tmux_bin: &str) -> anyhow::Error {
    if error.kind() == std::io::ErrorKind::NotFound {
        crate::error::KiraMuxError::MissingDependency(format!("tmux binary not found: {tmux_bin}"))
            .into()
    } else {
        anyhow::Error::new(error).context(format!("failed to run tmux command via {tmux_bin}"))
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::os::unix::process::ExitStatusExt;
    use std::process::{ExitStatus, Output};

    use super::{
        command_error, is_missing_session_message, is_missing_target_message, is_no_server_message,
        map_spawn_error, parse_pane_line, stdout_lines,
    };
    use crate::error::KiraMuxError;
    use crate::test_support::{TestOptionExt, TestResultExt};

    fn output(stdout: &str, stderr: &str, status_code: i32) -> Output {
        Output {
            status: ExitStatus::from_raw(status_code << 8),
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    #[test]
    fn stdout_lines_returns_empty_for_empty_stdout() {
        assert_eq!(stdout_lines(&output("", "", 0)), Vec::<String>::new());
    }

    #[test]
    fn stdout_lines_trims_multiline_stdout() {
        let lines = stdout_lines(&output("  first  \n\tsecond\t\nthird\n", "", 0));

        assert_eq!(lines, ["first", "second", "third"]);
    }

    #[test]
    fn stdout_lines_filters_blank_lines() {
        let lines = stdout_lines(&output("\n  \nfirst\n\t\n second \n", "", 0));

        assert_eq!(lines, ["first", "second"]);
    }

    #[test]
    fn parse_pane_line_parses_alive_pane() {
        let pane = parse_pane_line("%5|0|").or_panic();

        assert_eq!(pane.pane_id, "%5");
        assert!(!pane.pane_dead);
        assert_eq!(pane.pane_dead_status, None);
    }

    #[test]
    fn parse_pane_line_parses_dead_pane_with_exit_code() {
        let pane = parse_pane_line("%5|1|137").or_panic();

        assert_eq!(pane.pane_id, "%5");
        assert!(pane.pane_dead);
        assert_eq!(pane.pane_dead_status, Some(137));
    }

    #[test]
    fn parse_pane_line_parses_dead_pane_with_empty_status() {
        let pane = parse_pane_line("%5|1|").or_panic();

        assert!(pane.pane_dead);
        assert_eq!(pane.pane_dead_status, None);
    }

    #[test]
    fn parse_pane_line_ignores_non_numeric_dead_status() {
        let pane = parse_pane_line("%5|1|not-a-number").or_panic();

        assert!(pane.pane_dead);
        assert_eq!(pane.pane_dead_status, None);
    }

    #[test]
    fn parse_pane_line_preserves_empty_pane_id_field() {
        let pane = parse_pane_line("|0|").or_panic();

        assert_eq!(pane.pane_id, "");
        assert!(!pane.pane_dead);
    }

    #[test]
    fn parse_pane_line_rejects_missing_pane_dead_field() {
        let error = parse_pane_line("%5").err_or_panic();

        assert_eq!(error.to_string(), "missing pane_dead");
    }

    #[test]
    fn parse_pane_line_treats_status_remainder_as_opaque() {
        let pane = parse_pane_line("%5|1|137|extra").or_panic();

        assert!(pane.pane_dead);
        assert_eq!(pane.pane_dead_status, None);
    }

    #[test]
    fn is_missing_target_message_matches_window_pane_and_session() {
        assert!(is_missing_target_message("can't find window: agents"));
        assert!(is_missing_target_message("can't find pane: %7"));
        assert!(is_missing_target_message("can't find session: demo"));
        assert!(!is_missing_target_message("some other tmux error"));
    }

    #[test]
    fn command_error_returns_trimmed_stderr_when_present() {
        let output = output("", "  tmux failed\n", 42);

        assert_eq!(command_error(&output), "tmux failed");
    }

    #[test]
    fn command_error_uses_status_fallback_for_empty_stderr() {
        let output = output("", "", 42);

        assert_eq!(
            command_error(&output),
            format!("tmux command failed with status {}", output.status)
        );
    }

    #[test]
    fn command_error_uses_status_fallback_for_whitespace_stderr() {
        let output = output("", " \n\t", 42);

        assert_eq!(
            command_error(&output),
            format!("tmux command failed with status {}", output.status)
        );
    }

    #[test]
    fn is_missing_session_message_matches_cant_find_session() {
        assert!(is_missing_session_message("can't find session foo"));
    }

    #[test]
    fn is_missing_session_message_matches_session_not_found() {
        assert!(is_missing_session_message("session not found"));
    }

    #[test]
    fn is_missing_session_message_rejects_unrelated_message() {
        assert!(!is_missing_session_message("can't find window foo"));
    }

    #[test]
    fn is_no_server_message_matches_no_server_running() {
        assert!(is_no_server_message(
            "no server running on /tmp/tmux-501/default"
        ));
    }

    #[test]
    fn is_no_server_message_matches_missing_socket_connection_error() {
        assert!(is_no_server_message(
            "error connecting to /tmp/foo (No such file or directory)"
        ));
    }

    #[test]
    fn is_no_server_message_matches_refused_socket_connection_error() {
        assert!(is_no_server_message(
            "error connecting to /tmp/foo (Connection refused)"
        ));
    }

    #[test]
    fn is_no_server_message_rejects_permission_denied_connection_error() {
        assert!(!is_no_server_message(
            "error connecting to /tmp/foo (Permission denied)"
        ));
    }

    #[test]
    fn is_no_server_message_rejects_unrelated_message() {
        assert!(!is_no_server_message("server is already running"));
    }

    #[test]
    fn map_spawn_error_maps_not_found_to_missing_dependency() {
        let error = map_spawn_error(io::Error::from(io::ErrorKind::NotFound), "tmux_bin");
        let error = error.downcast_ref::<KiraMuxError>().or_panic();

        assert!(matches!(
            error,
            KiraMuxError::MissingDependency(message)
                if message == "tmux binary not found: tmux_bin"
        ));
    }

    #[test]
    fn map_spawn_error_wraps_other_io_errors_with_context() {
        let error = map_spawn_error(io::Error::from(io::ErrorKind::PermissionDenied), "tmux_bin");

        assert_eq!(error.to_string(), "failed to run tmux command via tmux_bin");
        assert_eq!(
            error.downcast_ref::<io::Error>().or_panic().kind(),
            io::ErrorKind::PermissionDenied
        );
    }
}
