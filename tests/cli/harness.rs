//! Shared sandbox and assertion helpers for real-tmux integration tests.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// Global config written into every test bed: deterministic shell, default
/// prefix, keep failed panes so dead-pane states are observable.
pub(crate) const GLOBAL_CONFIG: &str = r#"session_prefix = "kira"
default_layout = "auto"
window_name = "agents"
remain_on_exit = "failed"
default_shell = "/bin/sh"
"#;

/// One long-lived generic agent; `cat` echoes delivered prompts back into
/// the pane so send/capture round-trips are observable.
pub(crate) const CAT_AGENT: &str = r#"[[agents]]
id = "alpha"
mode = "shell"
shell_command = "exec cat"
"#;

static NEXT_SOCKET: AtomicUsize = AtomicUsize::new(0);

/// Isolated sandbox for one test: its own tmux server, config home, and
/// project root.
pub(crate) struct TestBed {
    pub(crate) config_home: tempfile::TempDir,
    pub(crate) project_root: tempfile::TempDir,
    pub(crate) socket: String,
}

impl TestBed {
    pub(crate) fn new() -> Self {
        let socket = format!(
            "kira-it-{}-{}",
            std::process::id(),
            NEXT_SOCKET.fetch_add(1, Ordering::Relaxed)
        );
        let bed = Self {
            config_home: make_tempdir("config home"),
            project_root: make_tempdir("project root"),
            socket,
        };
        write_file(&bed.projects_dir().join(".keep"), "");
        write_file(
            &bed.config_home.path().join("kira-mux/config.toml"),
            GLOBAL_CONFIG,
        );
        bed
    }

    pub(crate) fn projects_dir(&self) -> PathBuf {
        self.config_home.path().join("kira-mux/projects")
    }

    pub(crate) fn root(&self) -> String {
        self.project_root.path().display().to_string()
    }

    /// Write the test project (`id = "it"`) with the given `[[agents]]`
    /// snippet; call again to simulate config drift.
    pub(crate) fn write_project(&self, agents_toml: &str) {
        let contents = format!(
            "id = \"it\"\nname = \"Integration\"\nroot = \"{}\"\n\n{agents_toml}",
            self.root()
        );
        write_file(&self.projects_dir().join("it.toml"), &contents);
    }

    /// Run the compiled `kira-mux` binary against this bed's sandbox.
    pub(crate) fn kira(&self, args: &[&str]) -> Output {
        self.kira_with_env(args, &[])
    }

    /// Run the CLI with an explicit process current directory.
    pub(crate) fn kira_from(&self, current_dir: &std::path::Path, args: &[&str]) -> Output {
        self.kira_with_env_from(args, &[], Some(current_dir))
    }

    /// Like [`Self::kira`], with extra process environment entries (e.g. host
    /// values for `$VAR` agent env references).
    pub(crate) fn kira_with_env(&self, args: &[&str], extra_env: &[(&str, &str)]) -> Output {
        self.kira_with_env_from(args, extra_env, None)
    }

    pub(crate) fn kira_with_env_from(
        &self,
        args: &[&str],
        extra_env: &[(&str, &str)],
        current_dir: Option<&std::path::Path>,
    ) -> Output {
        let mut command = self.kira_command(args);
        if let Some(current_dir) = current_dir {
            command.current_dir(current_dir);
        }
        for (key, value) in extra_env {
            command.env(key, value);
        }
        run(&mut command)
    }

    /// Base `kira-mux` command wired to this bed's isolated tmux server.
    pub(crate) fn kira_command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_kira-mux"));
        command
            .args(args)
            .env("XDG_CONFIG_HOME", self.config_home.path())
            .env("KIRA_MUX_TMUX_SOCKET_NAME", &self.socket)
            // Second-scale wait windows so `send --wait` scenarios (incl.
            // the exit-7 hard timeout) run inside test deadlines. Scripted
            // agents must emit within the fast profile's quiet windows.
            .env("KIRA_MUX_WAIT_PROFILE", "fast")
            .env("HOME", self.config_home.path())
            .env("SHELL", "/bin/sh")
            // Keep the server's socket inside the bed's tempdir so no tmux
            // state outlives the test, and a surrounding tmux session (a
            // developer running the suite inside tmux) is never visible.
            .env("TMUX_TMPDIR", self.config_home.path())
            .env_remove("TMUX");
        command
    }

    /// Run kira with a test-side deadline: a hung command is killed and its
    /// partial output returned, so a blocking-command regression (e.g. a
    /// `send --wait` that never converges) fails the suite fast instead of
    /// stalling on kira's internal 10-minute hard timeout.
    pub(crate) fn kira_within(&self, deadline: Duration, args: &[&str]) -> Output {
        let mut command = self.kira_command(args);
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => panic!("failed to spawn {command:?}: {error}"),
        };
        let started = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if started.elapsed() >= deadline => {
                    let _ = child.kill();
                    break;
                }
                // std has no blocking wait-with-timeout: poll with a short nap.
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                Err(error) => panic!("failed to poll kira-mux: {error}"),
            }
        }
        match child.wait_with_output() {
            Ok(output) => output,
            Err(error) => panic!("failed to collect kira-mux output: {error}"),
        }
    }

    /// Run raw tmux against this bed's isolated server, for asserting on
    /// server state the CLI does not expose.
    pub(crate) fn tmux(&self, args: &[&str]) -> Output {
        let mut command = Command::new("tmux");
        command
            .arg("-L")
            .arg(&self.socket)
            .args(args)
            .env("XDG_CONFIG_HOME", self.config_home.path())
            .env("HOME", self.config_home.path())
            .env("SHELL", "/bin/sh")
            .env("TMUX_TMPDIR", self.config_home.path())
            .env_remove("TMUX");
        run(&mut command)
    }

    /// Poll `status --json` until the project state matches. Transient
    /// non-JSON output (command still racing the workspace) polls again
    /// instead of failing.
    pub(crate) fn wait_for_state(&self, expected: &str) -> serde_json::Value {
        wait_until(&format!("project state `{expected}`"), || {
            let output = self.kira(&["status", "it", "--json"]);
            let value: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
            (value["state"] == expected).then_some(value)
        })
    }

    /// Poll `capture` until the pane output contains `needle`.
    pub(crate) fn wait_for_capture(&self, agent_id: &str, needle: &str) -> String {
        wait_until(
            &format!("capture of `{agent_id}` to contain {needle:?}"),
            || {
                let output = self.kira(&["capture", "it", agent_id]);
                let text = stdout_of(&output);
                (output.status.success() && text.contains(needle)).then_some(text)
            },
        )
    }
}

impl Drop for TestBed {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .args(["-L", &self.socket, "kill-server"])
            .env("XDG_CONFIG_HOME", self.config_home.path())
            .env("HOME", self.config_home.path())
            .env("SHELL", "/bin/sh")
            .env("TMUX_TMPDIR", self.config_home.path())
            .env_remove("TMUX")
            .output();
    }
}

pub(crate) fn make_tempdir(what: &str) -> tempfile::TempDir {
    match tempfile::tempdir() {
        Ok(dir) => dir,
        Err(error) => panic!("failed to create {what}: {error}"),
    }
}

pub(crate) fn write_file(path: &std::path::Path, contents: &str) {
    if let Some(parent) = path.parent()
        && let Err(error) = fs::create_dir_all(parent)
    {
        panic!("failed to create {}: {error}", parent.display());
    }
    if let Err(error) = fs::write(path, contents) {
        panic!("failed to write {}: {error}", path.display());
    }
}

pub(crate) fn run(command: &mut Command) -> Output {
    match command.output() {
        Ok(output) => output,
        Err(error) => panic!("failed to run {command:?}: {error}"),
    }
}

pub(crate) fn exit_code(output: &Output) -> i32 {
    output.status.code().unwrap_or(-1)
}

pub(crate) fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

pub(crate) fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

pub(crate) fn assert_success(output: &Output, what: &str) {
    assert!(
        output.status.success(),
        "{what} failed (exit {}): stdout={:?} stderr={:?}",
        exit_code(output),
        stdout_of(output),
        stderr_of(output),
    );
}

pub(crate) fn parse_json(output: &Output) -> serde_json::Value {
    match serde_json::from_slice(&output.stdout) {
        Ok(value) => value,
        Err(error) => panic!(
            "expected JSON on stdout, got {:?} (stderr={:?}): {error}",
            stdout_of(output),
            stderr_of(output),
        ),
    }
}

pub(crate) fn wait_until<T>(what: &str, poll: impl Fn() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(value) = poll() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

pub(crate) fn make_executable(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Err(error) = fs::set_permissions(path, fs::Permissions::from_mode(0o755)) {
        panic!("failed to chmod {}: {error}", path.display());
    }
}

/// Resolve the managed session name (it embeds a hash) via list-sessions.
pub(crate) fn managed_session_name(bed: &TestBed) -> String {
    let sessions = bed.tmux(&["list-sessions", "-F", "#{session_name}"]);
    assert_success(&sessions, "list-sessions");
    stdout_of(&sessions).trim().to_string()
}

/// Full window-state fingerprint a deep capture must leave untouched:
/// size, zoom, active pane, and the exact pane layout.
pub(crate) fn window_state(bed: &TestBed, window: &str) -> String {
    let state = bed.tmux(&[
        "display-message",
        "-p",
        "-t",
        window,
        "#{window_width}x#{window_height} zoomed=#{window_zoomed_flag} \
         active=#{pane_id} layout=#{window_layout}",
    ]);
    assert_success(&state, "window state read");
    stdout_of(&state).trim().to_string()
}
