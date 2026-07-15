use std::env;
use std::io::Write;
use std::process::{Command, ExitStatus, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Result, bail};

use super::adapter::{PaneInfo, TmuxAdapter};
use super::env_file::{ShellEnvFile, forwarded_env_pairs_from, respawn_pane_args};
use super::error::TmuxError;
use super::metadata::{
    PANE_AGENT_ID, SESSION_CONFIG_FINGERPRINT, SESSION_PROFILE_ID, SESSION_PROJECT_ID, WINDOW_ROLE,
};
use super::parse::{
    command_error, is_missing_session_message, is_no_server_message, map_spawn_error,
    normalize_args, parse_pane_line, stdout_lines,
};

const TEST_SOCKET_ENV: &str = "KIRA_MUX_TMUX_SOCKET_NAME";

#[derive(Debug, Clone)]
pub(crate) struct PaneSummary {
    pub pane_dead: bool,
    pub agent_id: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct WorkspaceSummarySnapshot {
    pub fingerprint: Option<String>,
    pub project_id: Option<String>,
    pub profile_id: Option<String>,
    pub window_role: Option<String>,
    pub panes: Vec<PaneSummary>,
}

static BUFFER_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
/// Concrete tmux subprocess client used by the runtime and integration tests.
pub struct TmuxClient {
    tmux_bin: String,
    socket_name: Option<String>,
}

impl TmuxAdapter for TmuxClient {
    fn session_exists(&self, session_name: &str) -> Result<bool> {
        self.session_exists(session_name)
    }

    fn create_detached_session(
        &self,
        session_name: &str,
        start_directory: &str,
        window_name: &str,
        pane_count: usize,
    ) -> Result<()> {
        self.create_detached_session(session_name, start_directory, window_name, pane_count)
    }

    fn list_panes(&self, target: Option<&str>) -> Result<Vec<PaneInfo>> {
        self.list_panes(target)
    }

    fn split_window(&self, target: &str, start_directory: &str) -> Result<()> {
        self.split_window(target, start_directory)
    }

    fn select_layout(&self, target: &str, layout: &str) -> Result<()> {
        self.select_layout(target, layout)
    }

    fn respawn_pane(
        &self,
        target: &str,
        start_directory: &str,
        env_overrides: &[(String, String)],
        command: &[String],
    ) -> Result<()> {
        self.respawn_pane(target, start_directory, env_overrides, command)
    }

    fn attach_session(&self, session_name: &str) -> Result<()> {
        self.attach_session(session_name)
    }

    fn switch_client(&self, session_name: &str) -> Result<()> {
        self.switch_client(session_name)
    }

    fn kill_session(&self, session_name: &str) -> Result<()> {
        self.kill_session(session_name)
    }

    fn set_session_option(&self, target: &str, name: &str, value: &str) -> Result<()> {
        self.set_session_option(target, name, value)
    }

    fn get_session_option(&self, target: &str, name: &str) -> Result<Option<String>> {
        self.get_session_option(target, name)
    }

    fn set_window_option(&self, target: &str, name: &str, value: &str) -> Result<()> {
        self.set_window_option(target, name, value)
    }

    fn get_window_option(&self, target: &str, name: &str) -> Result<Option<String>> {
        self.get_window_option(target, name)
    }

    fn set_pane_option(&self, target: &str, name: &str, value: &str) -> Result<()> {
        self.set_pane_option(target, name, value)
    }

    fn get_pane_option(&self, target: &str, name: &str) -> Result<Option<String>> {
        self.get_pane_option(target, name)
    }

    fn paste_text(&self, target_pane: &str, text: &str) -> Result<()> {
        self.paste_text(target_pane, text)
    }

    fn send_keys(&self, target_pane: &str, keys: &[&str]) -> Result<()> {
        self.send_keys(target_pane, keys)
    }

    fn capture_pane(&self, pane_id: &str, history_limit: usize) -> Result<String> {
        self.capture_pane(pane_id, history_limit)
    }
}

impl TmuxClient {
    /// Build a client for the given tmux binary without a socket override.
    pub fn new(tmux_bin: impl Into<String>) -> Self {
        Self {
            tmux_bin: tmux_bin.into(),
            socket_name: None,
        }
    }

    /// Build a client and pick up the test socket from
    /// `KIRA_MUX_TMUX_SOCKET_NAME` when set.
    pub fn from_env(tmux_bin: impl Into<String>) -> Self {
        Self {
            tmux_bin: tmux_bin.into(),
            socket_name: socket_name_from_env_vars(|key| env::var(key).ok()),
        }
    }

    /// Check whether a tmux session currently exists.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be started, no server is running, or
    /// `has-session` fails for a reason other than a missing session.
    pub fn session_exists(&self, session_name: &str) -> Result<bool> {
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
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be started or rejects the session
    /// creation command.
    pub fn create_detached_session(
        &self,
        session_name: &str,
        start_directory: &str,
        window_name: &str,
        pane_count: usize,
    ) -> Result<()> {
        let height = (pane_count * 2).max(24).to_string();
        let args: Vec<String> = vec![
            "new-session".to_string(),
            "-d".to_string(),
            "-x".to_string(),
            "200".to_string(),
            "-y".to_string(),
            height,
            "-s".to_string(),
            session_name.to_string(),
            "-c".to_string(),
            start_directory.to_string(),
            "-n".to_string(),
            window_name.to_string(),
        ];
        self.run(args)
    }

    /// List panes for one target, or all panes when `target` is `None`.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be started, rejects the query, or
    /// returns a pane row that cannot be parsed.
    pub fn list_panes(&self, target: Option<&str>) -> Result<Vec<PaneInfo>> {
        let mut args = vec![
            "list-panes".to_string(),
            "-F".to_string(),
            "#{pane_id}|#{window_id}|#{pane_dead}|#{pane_dead_status}".to_string(),
        ];
        if let Some(target) = target {
            args.push("-t".to_string());
            args.push(target.to_string());
        } else {
            args.push("-a".to_string());
        }

        let output = self.output(args)?;
        if !output.status.success() {
            bail!(command_error(&output));
        }

        stdout_lines(&output)
            .into_iter()
            .map(|line| parse_pane_line(&line))
            .collect()
    }

    /// Split the target window, creating another pane in `start_directory`.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be started or rejects the split.
    pub fn split_window(&self, target: &str, start_directory: &str) -> Result<()> {
        let args: Vec<String> = vec![
            "split-window".to_string(),
            "-d".to_string(),
            "-t".to_string(),
            target.to_string(),
            "-c".to_string(),
            start_directory.to_string(),
        ];
        self.run(args)
    }

    /// Build the resolved environment pair list for the tmux forwarding
    /// whitelist, reading values from the current process env.
    fn forwarded_env_pairs() -> Vec<(String, String)> {
        forwarded_env_pairs_from(|key| env::var(key).ok())
    }

    /// Apply a tmux layout preset to the target window.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be started or rejects the layout.
    pub fn select_layout(&self, target: &str, layout: &str) -> Result<()> {
        self.run(["select-layout", "-t", target, layout])
    }

    /// Restart a pane with the provided working directory, env, and command.
    ///
    /// # Errors
    ///
    /// Returns an error when the temporary environment file cannot be created
    /// or tmux cannot restart the pane.
    pub fn respawn_pane(
        &self,
        target: &str,
        start_directory: &str,
        env_overrides: &[(String, String)],
        command: &[String],
    ) -> Result<()> {
        let mut env_pairs = Self::forwarded_env_pairs();
        env_pairs.extend(env_overrides.iter().cloned());
        let env_file = ShellEnvFile::create(&env_pairs)?;
        let env_file_path = env_file.as_ref().map(ShellEnvFile::path_arg).transpose()?;
        let args = respawn_pane_args(target, start_directory, env_file_path.as_deref(), command);

        let result = self.run(args);
        if result.is_err()
            && let Some(file) = &env_file
        {
            file.remove_best_effort();
        }
        result
    }

    /// Attach the current terminal to the target session.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be started or rejects the attach.
    pub fn attach_session(&self, session_name: &str) -> Result<()> {
        self.run_interactive(["attach-session", "-t", session_name])
    }

    /// Switch the attached tmux client to another session.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be started or rejects the switch.
    pub fn switch_client(&self, session_name: &str) -> Result<()> {
        self.run_interactive(["switch-client", "-t", session_name])
    }

    /// Capture the visible and scrollback content of a pane, returning at
    /// most `history_limit` lines (the last N lines of the captured output).
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be started or rejects the capture.
    pub fn capture_pane(&self, pane_id: &str, history_limit: usize) -> Result<String> {
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
            bail!(command_error(&output));
        }
        let raw = String::from_utf8_lossy(&output.stdout);
        let all_lines: Vec<&str> = raw.lines().collect();

        // tmux pads the visible area with empty lines below content,
        // which inflates the line count and can cause useful scrollback
        // (especially from dead panes) to be truncated. Collapse runs
        // of consecutive empty lines to at most one before limiting.
        let mut lines: Vec<&str> = Vec::with_capacity(all_lines.len());
        let mut prev_empty = false;
        for line in &all_lines {
            if line.is_empty() {
                if !prev_empty {
                    lines.push(line);
                }
                prev_empty = true;
            } else {
                lines.push(line);
                prev_empty = false;
            }
        }

        if lines.len() > history_limit {
            Ok(lines[lines.len() - history_limit..].join("\n") + "\n")
        } else {
            Ok(lines.join("\n") + "\n")
        }
    }

    /// Paste literal text into a pane via a temporary tmux buffer.
    ///
    /// # Errors
    ///
    /// Returns an error when the temporary buffer cannot be loaded or pasted
    /// into the target pane.
    pub fn paste_text(&self, target_pane: &str, text: &str) -> Result<()> {
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

    /// Send literal key names to a pane.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be started or rejects the key input.
    pub fn send_keys(&self, target_pane: &str, keys: &[&str]) -> Result<()> {
        let mut args = vec![
            "send-keys".to_string(),
            "-t".to_string(),
            target_pane.to_string(),
        ];
        for key in keys {
            args.push((*key).to_string());
        }
        self.run(args)
    }

    /// Kill the target session.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be started or rejects the kill.
    pub fn kill_session(&self, session_name: &str) -> Result<()> {
        self.run(["kill-session", "-t", session_name])
    }

    /// Set a session-scoped tmux option.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be started or rejects the option.
    pub fn set_session_option(&self, target: &str, name: &str, value: &str) -> Result<()> {
        self.run(["set-option", "-q", "-t", target, name, value])
    }

    /// Read a session-scoped tmux option.
    ///
    /// # Errors
    ///
    /// Returns an error when the session is missing or the tmux option query
    /// fails.
    pub fn get_session_option(&self, target: &str, name: &str) -> Result<Option<String>> {
        if !self.session_exists(target)? {
            return Err(TmuxError::MissingSession(target.to_string()).into());
        }
        self.read_option(["show-options", "-q", "-v", "-t", target, name])
    }

    /// Set a window-scoped tmux option.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be started or rejects the option.
    pub fn set_window_option(&self, target: &str, name: &str, value: &str) -> Result<()> {
        self.run(["set-option", "-w", "-q", "-t", target, name, value])
    }

    /// Read a window-scoped tmux option.
    ///
    /// # Errors
    ///
    /// Returns an error when the target is missing or the tmux option query
    /// fails.
    pub fn get_window_option(&self, target: &str, name: &str) -> Result<Option<String>> {
        if !self.target_exists(target)? {
            return Err(TmuxError::MissingTarget(target.to_string()).into());
        }
        self.read_option(["show-options", "-w", "-q", "-v", "-t", target, name])
    }

    /// Set a pane-scoped tmux option.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be started or rejects the option.
    pub fn set_pane_option(&self, target: &str, name: &str, value: &str) -> Result<()> {
        self.run(["set-option", "-p", "-q", "-t", target, name, value])
    }

    /// Read a pane-scoped tmux option.
    ///
    /// # Errors
    ///
    /// Returns an error when the target is missing or the tmux option query
    /// fails.
    pub fn get_pane_option(&self, target: &str, name: &str) -> Result<Option<String>> {
        if !self.target_exists(target)? {
            return Err(TmuxError::MissingTarget(target.to_string()).into());
        }
        self.read_option(["show-options", "-p", "-q", "-v", "-t", target, name])
    }

    fn read_option<I, S>(&self, args: I) -> Result<Option<String>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let output = self.output(args)?;
        if !output.status.success() {
            return Ok(None);
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
        let args = args
            .into_iter()
            .map(|arg| arg.as_ref().to_string())
            .collect::<Vec<_>>();
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
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(stdin_data)?;
        }
        let output = child
            .wait_with_output()
            .map_err(|e| map_spawn_error(e, &self.tmux_bin))?;
        if output.status.success() {
            return Ok(());
        }
        bail!(command_error(&output));
    }

    fn target_exists(&self, target: &str) -> Result<bool> {
        let output = self.output(["list-panes", "-t", target, "-F", "#{pane_id}"])?;
        Ok(output.status.success())
    }

    pub(crate) fn snapshot_summary(
        &self,
        session_name: &str,
        window_index: &str,
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

        let window_target = format!("{session_name}:{window_index}");

        let display_fmt = format!(
            "#{{{SESSION_CONFIG_FINGERPRINT}}}\t#{{{SESSION_PROJECT_ID}}}\t#{{{SESSION_PROFILE_ID}}}\t#{{{WINDOW_ROLE}}}",
        );
        let display_output =
            self.output(["display-message", "-p", "-t", &window_target, &display_fmt])?;
        if !display_output.status.success() {
            return Ok(Some(WorkspaceSummarySnapshot {
                fingerprint: None,
                project_id: None,
                profile_id: None,
                window_role: None,
                panes: vec![],
            }));
        }
        let (fingerprint, project_id, profile_id, window_role) =
            parse_display_message_line(&String::from_utf8_lossy(&display_output.stdout));

        let pane_fmt = format!("#{{pane_dead}}\t#{{{PANE_AGENT_ID}}}");
        let pane_output = self.output(["list-panes", "-t", &window_target, "-F", &pane_fmt])?;
        if !pane_output.status.success() {
            return Ok(Some(WorkspaceSummarySnapshot {
                fingerprint: None,
                project_id: None,
                profile_id: None,
                window_role: None,
                panes: vec![],
            }));
        }
        let panes = stdout_lines(&pane_output)
            .iter()
            .map(|line| parse_pane_summary_line(line))
            .collect();

        Ok(Some(WorkspaceSummarySnapshot {
            fingerprint,
            project_id,
            profile_id,
            window_role,
            panes,
        }))
    }
}

fn parse_display_message_line(
    raw: &str,
) -> (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
) {
    let line = raw.trim();
    let mut parts = line.splitn(4, '\t');
    let fingerprint = parts.next().and_then(non_empty);
    let project_id = parts.next().and_then(non_empty);
    let profile_id = parts.next().and_then(non_empty);
    let window_role = parts.next().and_then(non_empty);
    (fingerprint, project_id, profile_id, window_role)
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

fn socket_name_from_env_vars(mut get_env: impl FnMut(&str) -> Option<String>) -> Option<String> {
    get_env(TEST_SOCKET_ENV).and_then(|value| non_empty(&value))
}

#[cfg(test)]
mod tests {
    use super::{TEST_SOCKET_ENV, socket_name_from_env_vars};

    #[test]
    fn socket_env_reads_kira_mux_name() {
        let socket = socket_name_from_env_vars(|key| match key {
            TEST_SOCKET_ENV => Some("primary".to_string()),
            _ => None,
        });

        assert_eq!(socket.as_deref(), Some("primary"));
    }

    #[test]
    fn socket_env_ignores_empty_values() {
        let socket = socket_name_from_env_vars(|key| match key {
            TEST_SOCKET_ENV => Some(" ".to_string()),
            _ => None,
        });

        assert_eq!(socket, None);
    }

    #[test]
    fn socket_env_absent_when_unset() {
        let socket = socket_name_from_env_vars(|_| None);
        assert_eq!(socket, None);
    }
}
