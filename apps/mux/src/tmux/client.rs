use std::borrow::Cow;
use std::env;
use std::io::Write;
use std::process::{Command, ExitStatus, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, bail};

use super::adapter::{
    PaneInfo, TmuxAdapter, WorkspacePaneSnapshot, WorkspaceSnapshot, WorkspaceWindowSnapshot,
};
use super::env_file::{ShellEnvFile, respawn_pane_args};
use super::error::TmuxError;
use super::metadata::{
    PANE_AGENT_ID, SESSION_CONFIG_FINGERPRINT, SESSION_PROFILE_ID, SESSION_PROJECT_ID, WINDOW_ROLE,
};
use super::parse::{
    command_error, is_missing_session_message, is_missing_target_message, is_no_server_message,
    map_spawn_error, normalize_args, parse_pane_line, stdout_lines,
};

const TEST_SOCKET_ENV: &str = "KIRA_MUX_TMUX_SOCKET_NAME";

/// Session metadata read in one `display-message` round-trip.
struct DisplayedSessionMetadata {
    fingerprint: Option<String>,
    project_id: Option<String>,
    profile_id: Option<String>,
}

static BUFFER_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
/// Concrete tmux subprocess client used by the runtime.
pub(crate) struct TmuxClient {
    tmux_bin: String,
    socket_name: Option<String>,
}

impl TmuxAdapter for TmuxClient {
    /// Check whether a tmux session currently exists.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be started, no server is running, or
    /// `has-session` fails for a reason other than a missing session.
    fn session_exists(&self, session_name: &str) -> Result<bool> {
        let output = self.output(["has-session", "-t", session_name])?;
        if output.status.success() {
            return Ok(true);
        }

        let message = command_error(&output);
        if is_missing_session_message(&message) {
            return Ok(false);
        }

        if is_no_server_message(&message) {
            return Err(TmuxError::NoServer(message).into());
        }

        Err(TmuxError::CommandFailure(message).into())
    }

    /// Read session ownership plus the managed window and all pane metadata.
    ///
    /// A present workspace takes three tmux subprocesses regardless of pane
    /// count: `has-session`, one session metadata display, and one
    /// `list-panes` call with pane/window options embedded in its format.
    fn workspace_snapshot(
        &self,
        session_name: &str,
        window_name: &str,
    ) -> Result<Option<WorkspaceSnapshot>> {
        let exists = match self.session_exists(session_name) {
            Ok(exists) => exists,
            Err(error)
                if matches!(
                    error.downcast_ref::<TmuxError>(),
                    Some(TmuxError::NoServer(_))
                ) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        if !exists {
            return Ok(None);
        }

        let display_fmt = format!(
            "#{{{SESSION_CONFIG_FINGERPRINT}}}\t#{{{SESSION_PROJECT_ID}}}\t#{{{SESSION_PROFILE_ID}}}",
        );
        let display_output =
            self.output(["display-message", "-p", "-t", session_name, &display_fmt])?;
        if !display_output.status.success() {
            return Err(failed_tmux_status(session_name, &display_output));
        }
        let metadata = parse_display_message_line(&String::from_utf8_lossy(&display_output.stdout));

        let window_target = format!("{session_name}:{window_name}");
        let pane_fmt = format!(
            "#{{pane_id}}\t#{{pane_dead}}\t#{{pane_dead_status}}\t#{{{PANE_AGENT_ID}}}\t#{{{WINDOW_ROLE}}}",
        );
        let pane_output = self.output(["list-panes", "-t", &window_target, "-F", &pane_fmt])?;
        let window = if pane_output.status.success() {
            let parsed = stdout_lines(&pane_output)
                .iter()
                .map(|line| parse_workspace_pane_line(line))
                .collect::<Result<Vec<_>>>()?;
            let role = parsed.first().and_then(|(_, role)| role.clone());
            let panes = parsed.into_iter().map(|(pane, _)| pane).collect();
            Some(WorkspaceWindowSnapshot { role, panes })
        } else {
            let error = failed_tmux_status(&window_target, &pane_output);
            match error.downcast_ref::<TmuxError>() {
                Some(TmuxError::MissingTarget(_)) => None,
                Some(TmuxError::MissingSession(_) | TmuxError::NoServer(_)) => return Ok(None),
                Some(TmuxError::CommandFailure(_)) | None => return Err(error),
            }
        };

        Ok(Some(WorkspaceSnapshot {
            fingerprint: metadata.fingerprint,
            project_id: metadata.project_id,
            profile_id: metadata.profile_id,
            window,
        }))
    }

    /// Create a detached session with a single managed window sized for
    /// `pane_count`.
    fn create_detached_session(
        &self,
        session_name: &str,
        start_directory: &str,
        window_name: &str,
        pane_count: usize,
    ) -> Result<()> {
        let height = (pane_count * 2).max(24).to_string();
        self.run([
            "new-session",
            "-d",
            "-x",
            "200",
            "-y",
            &height,
            "-s",
            session_name,
            "-c",
            start_directory,
            "-n",
            window_name,
        ])
    }

    /// List panes for the target session or window.
    fn list_panes(&self, target: &str) -> Result<Vec<PaneInfo>> {
        let output = self.output([
            "list-panes",
            "-F",
            "#{pane_id}|#{pane_dead}|#{pane_dead_status}",
            "-t",
            target,
        ])?;
        if !output.status.success() {
            let message = command_error(&output);
            if is_missing_target_message(&message) {
                return Err(TmuxError::MissingTarget(target.to_string()).into());
            }
            bail!(message);
        }

        stdout_lines(&output)
            .into_iter()
            .map(|line| parse_pane_line(&line))
            .collect()
    }

    /// Split the target window, creating another pane in `start_directory`.
    fn split_window(&self, target: &str, start_directory: &str) -> Result<()> {
        self.run(["split-window", "-d", "-t", target, "-c", start_directory])
    }

    /// Apply a tmux layout preset to the target window.
    fn select_layout(&self, target: &str, layout: &str) -> Result<()> {
        self.run(["select-layout", "-t", target, layout])
    }

    /// Restart a pane with the provided working directory, env, and command.
    ///
    /// Environment values are delivered through a 0600 temp file sourced (and
    /// deleted) by the pane wrapper so they never appear in process argv.
    fn respawn_pane(
        &self,
        target: &str,
        start_directory: &str,
        env_overrides: &[(String, String)],
        command: &[String],
    ) -> Result<()> {
        let mut env_file = ShellEnvFile::create(env_overrides)?;
        let env_file_path = env_file.as_ref().map(ShellEnvFile::path_arg).transpose()?;
        let args = respawn_pane_args(target, start_directory, env_file_path.as_deref(), command);

        self.run(args)?;
        // The pane wrapper owns deletion from this point.
        if let Some(file) = &mut env_file {
            file.defuse();
        }
        Ok(())
    }

    /// Attach the current terminal to the target session.
    fn attach_session(&self, session_name: &str) -> Result<()> {
        self.run_interactive(["attach-session", "-t", session_name])
    }

    /// Switch the attached tmux client to another session.
    fn switch_client(&self, session_name: &str) -> Result<()> {
        self.run_interactive(["switch-client", "-t", session_name])
    }

    /// Kill the target session.
    fn kill_session(&self, session_name: &str) -> Result<()> {
        self.run(["kill-session", "-t", session_name])
    }

    /// Set a session-scoped tmux option.
    fn set_session_option(&self, target: &str, name: &str, value: &str) -> Result<()> {
        self.run(["set-option", "-q", "-t", target, name, value])
    }

    /// Read a session-scoped tmux option.
    fn get_session_option(&self, target: &str, name: &str) -> Result<Option<String>> {
        self.read_option(target, ["show-options", "-q", "-v", "-t", target, name])
    }

    /// Set a window-scoped tmux option.
    fn set_window_option(&self, target: &str, name: &str, value: &str) -> Result<()> {
        self.run(["set-option", "-w", "-q", "-t", target, name, value])
    }

    /// Set a pane-scoped tmux option.
    fn set_pane_option(&self, target: &str, name: &str, value: &str) -> Result<()> {
        self.run(["set-option", "-p", "-q", "-t", target, name, value])
    }

    /// Read a pane-scoped tmux option.
    fn get_pane_option(&self, target: &str, name: &str) -> Result<Option<String>> {
        self.read_option(
            target,
            ["show-options", "-p", "-q", "-v", "-t", target, name],
        )
    }

    /// Paste literal text into a pane via a temporary tmux buffer.
    fn paste_text(&self, target_pane: &str, text: &str) -> Result<()> {
        let seq = BUFFER_SEQ.fetch_add(1, Ordering::Relaxed);
        let buffer_name = format!("kira_mux_send_{}", std::process::id());
        let buffer_ref = format!("{buffer_name}_{seq}");
        self.run_with_stdin(["load-buffer", "-b", &buffer_ref, "-"], text.as_bytes())?;
        // Type missing-target failures so submit can map them to DeadPane
        // instead of an untyped exit 1 (generic `run` only bails with stderr).
        let result = self.run_on_target(
            target_pane,
            [
                "paste-buffer",
                "-p",
                "-r",
                "-t",
                target_pane,
                "-b",
                &buffer_ref,
                "-d",
            ],
        );
        if result.is_err() {
            let _ = self.run(["delete-buffer", "-b", &buffer_ref]);
        }
        result
    }

    /// Send named keys (e.g. `Enter`) to a pane. Not for prompt text: without
    /// `-l`, tmux translates arguments that match key names into keypresses.
    fn send_keys(&self, target_pane: &str, keys: &[&str]) -> Result<()> {
        let mut args = vec!["send-keys", "-t", target_pane];
        args.extend_from_slice(keys);
        self.run_on_target(target_pane, args)
    }

    /// Type literal text into a pane. `-l` disables key-name lookup and `--`
    /// stops flag parsing, so prompts like `Enter` or `-x` arrive as text.
    fn send_text(&self, target_pane: &str, text: &str) -> Result<()> {
        let text = escape_trailing_semicolon(text);
        self.run_on_target(
            target_pane,
            ["send-keys", "-l", "-t", target_pane, "--", text.as_ref()],
        )
    }

    /// Capture the visible and scrollback content of a pane, returning at
    /// most `history_limit` lines (the last N lines of the captured output).
    fn capture_pane(&self, pane_id: &str, history_limit: usize) -> Result<String> {
        let capped = i64::try_from(history_limit).unwrap_or(i64::MAX);
        let start_line = -capped;
        let output = self.output([
            "capture-pane",
            "-p",
            "-J",
            "-t",
            pane_id,
            "-S",
            &start_line.to_string(),
        ])?;
        if !output.status.success() {
            // Typed like list_panes so wait/capture callers can classify a
            // vanished pane instead of seeing an opaque transport failure.
            return Err(failed_tmux_status(pane_id, &output));
        }
        let raw = String::from_utf8_lossy(&output.stdout);
        // tmux pads the visible area with empty lines below content, which
        // inflates the line count and can push useful scrollback (especially
        // from dead panes) past the limit. Strip only that trailing padding;
        // interior blank lines are genuine transcript content.
        let mut lines: Vec<&str> = raw.lines().collect();
        while lines.last().is_some_and(|line| line.is_empty()) {
            lines.pop();
        }

        if lines.len() > history_limit {
            Ok(lines[lines.len() - history_limit..].join("\n") + "\n")
        } else {
            Ok(lines.join("\n") + "\n")
        }
    }
}

impl TmuxClient {
    /// Build a client and pick up the test socket from
    /// `KIRA_MUX_TMUX_SOCKET_NAME` when set.
    pub(crate) fn from_env(tmux_bin: impl Into<String>) -> Self {
        Self {
            tmux_bin: tmux_bin.into(),
            socket_name: env::var(TEST_SOCKET_ENV)
                .ok()
                .and_then(|value| non_empty(&value)),
        }
    }

    fn read_option<I, S>(&self, target: &str, args: I) -> Result<Option<String>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let output = self.output(args)?;
        if !output.status.success() {
            return Err(failed_tmux_status(target, &output));
        }

        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if stdout.is_empty() {
            Ok(None)
        } else {
            Ok(Some(stdout))
        }
    }

    fn run<I, S>(&self, args: I) -> Result<()>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let output = self.output(args)?;
        if output.status.success() {
            return Ok(());
        }

        bail!(command_error(&output));
    }

    /// Like [`Self::run`], but classifies missing session/window/pane as
    /// typed [`TmuxError`] so callers can map vanished targets without
    /// parsing stderr strings.
    fn run_on_target<I, S>(&self, target: &str, args: I) -> Result<()>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let output = self.output(args)?;
        if output.status.success() {
            return Ok(());
        }

        Err(failed_tmux_status(target, &output))
    }

    fn run_interactive<I, S>(&self, args: I) -> Result<()>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let args = normalize_args(args);
        let status = self.status(&args)?;
        if status.success() {
            return Ok(());
        }

        bail!("tmux command failed with status {status}");
    }

    fn output<I, S>(&self, args: I) -> Result<Output>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let args = normalize_args(args);
        let mut command = self.command(&args);
        command
            .output()
            .map_err(|e| map_spawn_error(e, &self.tmux_bin))
    }

    fn status(&self, args: &[String]) -> Result<ExitStatus> {
        let mut command = self.command(args);
        command
            .status()
            .map_err(|e| map_spawn_error(e, &self.tmux_bin))
    }

    fn command(&self, args: &[String]) -> Command {
        let command_name = args.first().map_or("unknown", String::as_str);
        tracing::debug!(
            tmux_bin = self.tmux_bin.as_str(),
            socket = self.socket_name.as_deref().unwrap_or("default"),
            command = command_name,
            "running tmux command"
        );

        let mut command = Command::new(&self.tmux_bin);
        if let Some(socket_name) = &self.socket_name {
            command.arg("-L").arg(socket_name);
        }

        for arg in args {
            command.arg(arg);
        }

        command
    }

    fn run_with_stdin<I, S>(&self, args: I, stdin_data: &[u8]) -> Result<()>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let args = normalize_args(args);
        let mut command = self.command(&args);
        command.stdin(Stdio::piped());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|e| map_spawn_error(e, &self.tmux_bin))?;
        if let Some(mut stdin) = child.stdin.take()
            && let Err(error) = stdin.write_all(stdin_data)
        {
            drop(stdin);
            let _ = child.kill();
            let _ = child.wait();
            return Err(anyhow::Error::new(error).context("failed to write to tmux stdin"));
        }
        let output = child
            .wait_with_output()
            .map_err(|e| map_spawn_error(e, &self.tmux_bin))?;
        if output.status.success() {
            return Ok(());
        }
        Err(failed_tmux_stdin_status(&output))
    }
}

/// Map a failed tmux subprocess status into a typed error.
///
/// Missing targets stay distinguishable from generic command failures so
/// callers can classify drift vs hard errors.
fn failed_tmux_status(target: &str, output: &Output) -> anyhow::Error {
    let message = command_error(output);
    if is_missing_session_message(&message) {
        TmuxError::MissingSession(target.to_string()).into()
    } else if is_missing_target_message(&message) {
        TmuxError::MissingTarget(target.to_string()).into()
    } else if is_no_server_message(&message) {
        TmuxError::NoServer(message).into()
    } else {
        TmuxError::CommandFailure(message).into()
    }
}

/// Classify a failed stdin command without changing generic error semantics.
fn failed_tmux_stdin_status(output: &Output) -> anyhow::Error {
    let message = command_error(output);
    if is_no_server_message(&message) {
        TmuxError::NoServer(message).into()
    } else {
        anyhow::Error::msg(message)
    }
}

/// Escape a trailing `;` so tmux does not treat the final argument as a
/// command separator.
fn escape_trailing_semicolon(text: &str) -> Cow<'_, str> {
    match text.strip_suffix(';') {
        Some(stripped) => Cow::Owned(format!("{stripped}\\;")),
        None => Cow::Borrowed(text),
    }
}

fn parse_display_message_line(raw: &str) -> DisplayedSessionMetadata {
    let line = raw.trim();
    let mut parts = line.splitn(3, '\t');
    DisplayedSessionMetadata {
        fingerprint: parts.next().and_then(non_empty),
        project_id: parts.next().and_then(non_empty),
        profile_id: parts.next().and_then(non_empty),
    }
}

fn parse_workspace_pane_line(line: &str) -> Result<(WorkspacePaneSnapshot, Option<String>)> {
    let mut parts = line.splitn(5, '\t');
    let pane_id = parts.next().context("missing pane_id")?.to_string();
    let pane_dead = parts.next().context("missing pane_dead")? == "1";
    let pane_dead_status = parts.next().and_then(|value| {
        if value.is_empty() {
            None
        } else {
            value.parse().ok()
        }
    });
    let agent_id = parts.next().and_then(non_empty);
    let window_role = parts.next().and_then(non_empty);

    Ok((
        WorkspacePaneSnapshot {
            pane: PaneInfo {
                pane_id,
                pane_dead,
                pane_dead_status,
            },
            agent_id,
        },
        window_role,
    ))
}

fn non_empty(s: &str) -> Option<String> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

#[cfg(test)]
mod tests {
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
    use crate::tmux::{TmuxAdapter, TmuxError};

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
        let (pane, role) = parse_workspace_pane_line("%5\t1\t137\talpha\tagents").or_panic();

        assert_eq!(pane.pane.pane_id, "%5");
        assert!(pane.pane.pane_dead);
        assert_eq!(pane.pane.pane_dead_status, Some(137));
        assert_eq!(pane.agent_id.as_deref(), Some("alpha"));
        assert_eq!(role.as_deref(), Some("agents"));
    }

    #[test]
    fn parse_workspace_pane_line_maps_empty_options_to_none() {
        let (pane, role) = parse_workspace_pane_line("%5\t0\t\t\t").or_panic();

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
                .or_panic()
                .or_panic();

            assert_eq!(
                snapshot.window.or_panic().panes.len(),
                pane_count,
                "unexpected pane count for script under {}",
                temp.path().display()
            );
            let calls = fs::read_to_string(&log_path).or_panic();
            assert_eq!(
                calls.lines().count(),
                3,
                "snapshot command count must not grow with {pane_count} panes: {calls}"
            );
        }
    }

    #[test]
    fn workspace_snapshot_reports_a_missing_window() {
        let (_temp, client, _log_path) =
            scripted_tmux_with_list_failure("can't find window: agents");

        let snapshot = client
            .workspace_snapshot("session", "agents")
            .or_panic()
            .or_panic();

        assert_eq!(snapshot.window, None);
    }

    #[test]
    fn workspace_snapshot_treats_a_vanished_session_as_absent() {
        let (_temp, client, _log_path) =
            scripted_tmux_with_list_failure("can't find session: session");

        let snapshot = client.workspace_snapshot("session", "agents").or_panic();

        assert_eq!(snapshot, None);
    }

    #[test]
    fn workspace_snapshot_treats_a_stopped_server_as_absent() {
        let (_temp, client, _log_path) =
            scripted_tmux_with_list_failure("no server running on /tmp/tmux-1000/default");

        let snapshot = client.workspace_snapshot("session", "agents").or_panic();

        assert_eq!(snapshot, None);
    }

    #[test]
    fn workspace_snapshot_propagates_a_generic_list_panes_failure() {
        let (_temp, client, _log_path) =
            scripted_tmux_with_list_failure("server unexpectedly closed");

        let error = client
            .workspace_snapshot("session", "agents")
            .err_or_panic();

        assert!(matches!(
            error.downcast_ref::<TmuxError>(),
            Some(TmuxError::CommandFailure(message)) if message == "server unexpectedly closed"
        ));
    }

    #[test]
    fn workspace_snapshot_propagates_a_display_message_failure() {
        let (temp, client, log_path) =
            scripted_tmux_with_actions("printf '%s\\n' 'display failed' >&2\nexit 1", "exit 0");

        let error = client
            .workspace_snapshot("session", "agents")
            .err_or_panic();

        assert!(matches!(
            error.downcast_ref::<TmuxError>(),
            Some(TmuxError::CommandFailure(message)) if message == "display failed"
        ));
        let calls = fs::read_to_string(&log_path).or_panic();
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
        let temp = tempfile::tempdir().or_panic();
        let script_path = temp.path().join("tmux");
        let pending_script_path = temp.path().join("tmux.pending");
        let log_path = temp.path().join("calls.log");
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncase \"$1\" in\n  has-session) exit 0 ;;\n  display-message)\n{display_action}\n    ;;\n  list-panes)\n{list_action}\n    ;;\nesac\n",
            log_path.display()
        );
        fs::write(&pending_script_path, script).or_panic();
        let mut permissions = fs::metadata(&pending_script_path).or_panic().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&pending_script_path, permissions).or_panic();
        // Publish the executable only after its writer is closed. This avoids
        // transient ETXTBSY failures on Linux runners when tests start it.
        fs::rename(&pending_script_path, &script_path).or_panic();
        let client = TmuxClient {
            tmux_bin: script_path.display().to_string(),
            socket_name: None,
        };
        (temp, client, log_path)
    }
}
