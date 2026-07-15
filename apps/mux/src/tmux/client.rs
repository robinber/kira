use std::borrow::Cow;
use std::env;
use std::io::Write;
use std::process::{Command, ExitStatus, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Result, bail};

use super::adapter::{PaneInfo, TmuxAdapter};
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

#[derive(Debug, Clone)]
pub(crate) struct PaneSummary {
    pub pane_dead: bool,
    pub agent_id: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct WorkspaceSummarySnapshot {
    pub fingerprint: Option<String>,
    pub project_id: Option<String>,
    pub profile_id: Option<String>,
    pub window_role: Option<String>,
    pub panes: Vec<PaneSummary>,
}

/// Session/window metadata read in one `display-message` round-trip.
struct DisplayedWindowMetadata {
    fingerprint: Option<String>,
    project_id: Option<String>,
    profile_id: Option<String>,
    window_role: Option<String>,
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

    /// Read a window-scoped tmux option.
    fn get_window_option(&self, target: &str, name: &str) -> Result<Option<String>> {
        self.read_option(
            target,
            ["show-options", "-w", "-q", "-v", "-t", target, name],
        )
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
        let result = self.run([
            "paste-buffer",
            "-p",
            "-r",
            "-t",
            target_pane,
            "-b",
            &buffer_ref,
            "-d",
        ]);
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
        self.run(args)
    }

    /// Type literal text into a pane. `-l` disables key-name lookup and `--`
    /// stops flag parsing, so prompts like `Enter` or `-x` arrive as text.
    fn send_text(&self, target_pane: &str, text: &str) -> Result<()> {
        let text = escape_trailing_semicolon(text);
        self.run(["send-keys", "-l", "-t", target_pane, "--", text.as_ref()])
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
        bail!(command_error(&output));
    }

    pub(crate) fn snapshot_summary(
        &self,
        session_name: &str,
        window_name: &str,
    ) -> Result<Option<WorkspaceSummarySnapshot>> {
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

        let window_target = format!("{session_name}:{window_name}");

        let display_fmt = format!(
            "#{{{SESSION_CONFIG_FINGERPRINT}}}\t#{{{SESSION_PROJECT_ID}}}\t#{{{SESSION_PROFILE_ID}}}\t#{{{WINDOW_ROLE}}}",
        );
        let display_output =
            self.output(["display-message", "-p", "-t", &window_target, &display_fmt])?;
        if !display_output.status.success() {
            // Missing window/target is drift (caller maps); transport / command
            // failures must not become an empty snapshot that classifies as
            // FingerprintMismatch.
            return Err(failed_tmux_status(&window_target, &display_output));
        }
        let metadata = parse_display_message_line(&String::from_utf8_lossy(&display_output.stdout));

        let pane_fmt = format!("#{{pane_dead}}\t#{{{PANE_AGENT_ID}}}");
        let pane_output = self.output(["list-panes", "-t", &window_target, "-F", &pane_fmt])?;
        if !pane_output.status.success() {
            return Err(failed_tmux_status(&window_target, &pane_output));
        }
        let panes = stdout_lines(&pane_output)
            .iter()
            .map(|line| parse_pane_summary_line(line))
            .collect();

        Ok(Some(WorkspaceSummarySnapshot {
            fingerprint: metadata.fingerprint,
            project_id: metadata.project_id,
            profile_id: metadata.profile_id,
            window_role: metadata.window_role,
            panes,
        }))
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

/// Escape a trailing `;` so tmux does not treat the final argument as a
/// command separator.
fn escape_trailing_semicolon(text: &str) -> Cow<'_, str> {
    match text.strip_suffix(';') {
        Some(stripped) => Cow::Owned(format!("{stripped}\\;")),
        None => Cow::Borrowed(text),
    }
}

fn parse_display_message_line(raw: &str) -> DisplayedWindowMetadata {
    let line = raw.trim();
    let mut parts = line.splitn(4, '\t');
    DisplayedWindowMetadata {
        fingerprint: parts.next().and_then(non_empty),
        project_id: parts.next().and_then(non_empty),
        profile_id: parts.next().and_then(non_empty),
        window_role: parts.next().and_then(non_empty),
    }
}

fn parse_pane_summary_line(line: &str) -> PaneSummary {
    let mut parts = line.splitn(2, '\t');
    let pane_dead = parts.next().unwrap_or("0") == "1";
    let agent_id = parts.next().and_then(non_empty);
    PaneSummary {
        pane_dead,
        agent_id,
    }
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
    use std::os::unix::process::ExitStatusExt;
    use std::process::{ExitStatus, Output};

    use super::{
        escape_trailing_semicolon, failed_tmux_status, parse_display_message_line,
        parse_pane_summary_line,
    };
    use crate::tmux::TmuxError;

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
        let metadata = parse_display_message_line("fp\tproj\tprof\tagents\n");

        assert_eq!(metadata.fingerprint.as_deref(), Some("fp"));
        assert_eq!(metadata.project_id.as_deref(), Some("proj"));
        assert_eq!(metadata.profile_id.as_deref(), Some("prof"));
        assert_eq!(metadata.window_role.as_deref(), Some("agents"));
    }

    #[test]
    fn parse_display_message_line_maps_empty_fields_to_none() {
        let metadata = parse_display_message_line("\t\t\t\n");

        assert_eq!(metadata.fingerprint, None);
        assert_eq!(metadata.project_id, None);
        assert_eq!(metadata.profile_id, None);
        assert_eq!(metadata.window_role, None);
    }

    #[test]
    fn parse_pane_summary_line_reads_dead_flag_and_agent() {
        let pane = parse_pane_summary_line("1\talpha");
        assert!(pane.pane_dead);
        assert_eq!(pane.agent_id.as_deref(), Some("alpha"));

        let pane = parse_pane_summary_line("0\t");
        assert!(!pane.pane_dead);
        assert_eq!(pane.agent_id, None);
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
}
