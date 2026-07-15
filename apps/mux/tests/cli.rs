//! Real-tmux integration tests for the `kira-mux` binary.
//!
//! Every test gets its own tmux server (unique `-L` socket) and its own XDG
//! config home, so tests run in parallel and leave nothing behind —
//! [`TestBed`] kills its server on drop, even when an assertion panics.
//!
//! Scope: only what `FakeTmux` cannot guarantee — the fidelity of the real
//! tmux client (send/capture semantics, session metadata, error messages)
//! and the end-to-end exit-code contract. Logic coverage lives in the unit
//! suite. Assertions poll with a generous timeout instead of sleeping a
//! fixed amount, so the suite is fast locally and tolerant on loaded CI
//! runners.
#![cfg(unix)]
#![allow(
    unused_crate_dependencies,
    reason = "integration test target uses only a subset of the package dependencies"
)]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// Global config written into every test bed: deterministic shell, default
/// prefix, keep failed panes so dead-pane states are observable.
const GLOBAL_CONFIG: &str = r#"session_prefix = "kira"
default_layout = "auto"
window_name = "agents"
remain_on_exit = "failed"
default_shell = "/bin/sh"
"#;

/// One long-lived generic agent; `cat` echoes delivered prompts back into
/// the pane so send/capture round-trips are observable.
const CAT_AGENT: &str = r#"[[agents]]
id = "alpha"
mode = "shell"
shell_command = "exec cat"
"#;

static NEXT_SOCKET: AtomicUsize = AtomicUsize::new(0);

/// Isolated sandbox for one test: its own tmux server, config home, and
/// project root.
struct TestBed {
    config_home: tempfile::TempDir,
    project_root: tempfile::TempDir,
    socket: String,
}

impl TestBed {
    fn new() -> Self {
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

    fn projects_dir(&self) -> PathBuf {
        self.config_home.path().join("kira-mux/projects")
    }

    fn root(&self) -> String {
        self.project_root.path().display().to_string()
    }

    /// Write the test project (`id = "it"`) with the given `[[agents]]`
    /// snippet; call again to simulate config drift.
    fn write_project(&self, agents_toml: &str) {
        let contents = format!(
            "id = \"it\"\nname = \"Integration\"\nroot = \"{}\"\n\n{agents_toml}",
            self.root()
        );
        write_file(&self.projects_dir().join("it.toml"), &contents);
    }

    /// Run the compiled `kira-mux` binary against this bed's sandbox.
    fn kira(&self, args: &[&str]) -> Output {
        self.kira_with_env(args, &[])
    }

    /// Like [`Self::kira`], with extra process environment entries (e.g. host
    /// values for `$VAR` agent env references).
    fn kira_with_env(&self, args: &[&str], extra_env: &[(&str, &str)]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_kira-mux"));
        command
            .args(args)
            .env("XDG_CONFIG_HOME", self.config_home.path())
            .env("KIRA_MUX_TMUX_SOCKET_NAME", &self.socket)
            .env("HOME", self.config_home.path())
            .env("SHELL", "/bin/sh")
            // Keep the server's socket inside the bed's tempdir so no tmux
            // state outlives the test, and a surrounding tmux session (a
            // developer running the suite inside tmux) is never visible.
            .env("TMUX_TMPDIR", self.config_home.path())
            .env_remove("TMUX");
        for (key, value) in extra_env {
            command.env(key, value);
        }
        run(&mut command)
    }

    /// Run raw tmux against this bed's isolated server, for asserting on
    /// server state the CLI does not expose.
    fn tmux(&self, args: &[&str]) -> Output {
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
    fn wait_for_state(&self, expected: &str) -> serde_json::Value {
        wait_until(&format!("project state `{expected}`"), || {
            let output = self.kira(&["status", "it", "--json"]);
            let value: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
            (value["state"] == expected).then_some(value)
        })
    }

    /// Poll `capture` until the pane output contains `needle`.
    fn wait_for_capture(&self, agent_id: &str, needle: &str) -> String {
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

fn make_tempdir(what: &str) -> tempfile::TempDir {
    match tempfile::tempdir() {
        Ok(dir) => dir,
        Err(error) => panic!("failed to create {what}: {error}"),
    }
}

fn write_file(path: &std::path::Path, contents: &str) {
    if let Some(parent) = path.parent()
        && let Err(error) = fs::create_dir_all(parent)
    {
        panic!("failed to create {}: {error}", parent.display());
    }
    if let Err(error) = fs::write(path, contents) {
        panic!("failed to write {}: {error}", path.display());
    }
}

fn run(command: &mut Command) -> Output {
    match command.output() {
        Ok(output) => output,
        Err(error) => panic!("failed to run {command:?}: {error}"),
    }
}

fn exit_code(output: &Output) -> i32 {
    output.status.code().unwrap_or(-1)
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn assert_success(output: &Output, what: &str) {
    assert!(
        output.status.success(),
        "{what} failed (exit {}): stdout={:?} stderr={:?}",
        exit_code(output),
        stdout_of(output),
        stderr_of(output),
    );
}

fn parse_json(output: &Output) -> serde_json::Value {
    match serde_json::from_slice(&output.stdout) {
        Ok(value) => value,
        Err(error) => panic!(
            "expected JSON on stdout, got {:?} (stderr={:?}): {error}",
            stdout_of(output),
            stderr_of(output),
        ),
    }
}

fn wait_until<T>(what: &str, poll: impl Fn() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(value) = poll() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

#[test]
fn start_creates_live_session_and_status_reports_running() {
    let bed = TestBed::new();
    bed.write_project(&format!(
        "{CAT_AGENT}\n[[agents]]\nid = \"beta\"\nmode = \"shell\"\nshell_command = \"exec cat\"\n"
    ));

    assert_success(&bed.kira(&["start", "it"]), "start");

    let status = bed.wait_for_state("running");
    let agents = status["agents"].as_array().map_or(0, Vec::len);
    assert_eq!(agents, 2, "expected 2 agents in status: {status}");
    assert_eq!(status["agents"][0]["state"], "running", "got: {status}");
    assert_eq!(status["agents"][1]["state"], "running", "got: {status}");

    // The session really exists, on this bed's isolated server only.
    let sessions = bed.tmux(&["list-sessions", "-F", "#{session_name}"]);
    assert_success(&sessions, "list-sessions");
    let names: Vec<String> = stdout_of(&sessions).lines().map(str::to_owned).collect();
    assert_eq!(names.len(), 1, "expected exactly one session: {names:?}");
    assert!(
        names[0].starts_with("kira-it-default-"),
        "unexpected session name: {names:?}"
    );

    // list goes through the snapshot_summary path (session options +
    // list-panes against the real server).
    let list = bed.kira(&["list", "--json"]);
    assert_success(&list, "list");
    assert_eq!(parse_json(&list)[0]["state"], "running");
}

#[test]
fn start_twice_is_idempotent() {
    let bed = TestBed::new();
    bed.write_project(CAT_AGENT);

    assert_success(&bed.kira(&["start", "it"]), "first start");
    assert_success(&bed.kira(&["start", "it"]), "second start");

    let sessions = bed.tmux(&["list-sessions", "-F", "#{session_name}"]);
    assert_eq!(
        stdout_of(&sessions).lines().count(),
        1,
        "second start must reuse the session, not create another"
    );
    bed.wait_for_state("running");
}

#[test]
fn kill_removes_the_session_and_repeating_kill_is_a_noop() {
    let bed = TestBed::new();
    bed.write_project(CAT_AGENT);
    assert_success(&bed.kira(&["start", "it"]), "start");

    assert_success(&bed.kira(&["kill", "it", "--yes"]), "kill");
    assert_eq!(bed.wait_for_state("stopped")["state"], "stopped");

    let second = bed.kira(&["kill", "it", "--yes"]);
    assert_success(&second, "second kill");
    assert!(
        stderr_of(&second).contains("already stopped"),
        "got stderr: {:?}",
        stderr_of(&second)
    );
}

#[test]
fn kill_refuses_an_untagged_same_name_session() {
    let bed = TestBed::new();
    bed.write_project(CAT_AGENT);
    assert_success(&bed.kira(&["start", "it"]), "start");
    bed.wait_for_state("running");

    let sessions = bed.tmux(&["list-sessions", "-F", "#{session_name}"]);
    assert_success(&sessions, "list managed session");
    let managed_name = stdout_of(&sessions).trim().to_string();
    assert!(
        !managed_name.is_empty(),
        "managed session name must be present"
    );

    assert_success(&bed.kira(&["kill", "it", "--yes"]), "initial kill");
    assert_success(
        &bed.tmux(&[
            "new-session",
            "-d",
            "-s",
            &managed_name,
            "-x",
            "80",
            "-y",
            "24",
        ]),
        "create same-name foreign session",
    );

    let kill = bed.kira(&["kill", "it", "--yes"]);
    assert_eq!(
        exit_code(&kill),
        4,
        "kill must classify the untagged collision as drift: {:?}",
        stderr_of(&kill)
    );
    let sessions = bed.tmux(&["list-sessions", "-F", "#{session_name}"]);
    assert_success(&sessions, "list foreign session after refused kill");
    assert_eq!(
        stdout_of(&sessions).trim(),
        managed_name,
        "the untagged same-name session must remain alive"
    );
}

#[test]
fn kill_succeeds_after_the_project_root_disappears() {
    let bed = TestBed::new();
    let agent_cwd = bed.project_root.path().join("agent-cwd");
    if let Err(error) = fs::create_dir(&agent_cwd) {
        panic!("failed to create explicit agent cwd: {error}");
    }
    bed.write_project(&format!("{CAT_AGENT}cwd = \"agent-cwd\"\n"));
    assert_success(&bed.kira(&["start", "it"]), "start");
    bed.wait_for_state("running");

    // Deleting the project root must not strand the session: kill resolves
    // the project from config alone and never needs the directory back.
    if let Err(error) = fs::remove_dir_all(bed.project_root.path()) {
        panic!("failed to remove project root: {error}");
    }
    assert_success(
        &bed.kira(&["kill", "it", "--yes"]),
        "kill after root removal",
    );
    bed.wait_for_state("stopped");

    // Relaunching into the missing root must fail loudly (typed config
    // validation), not create broken panes.
    let start = bed.kira(&["start", "it"]);
    assert_eq!(
        exit_code(&start),
        2,
        "start into a deleted root must exit 2, stderr: {:?}",
        stderr_of(&start)
    );
}

#[test]
fn status_and_list_report_stopped_before_any_start() {
    let bed = TestBed::new();
    bed.write_project(CAT_AGENT);

    let status = bed.kira(&["status", "it", "--json"]);
    assert_success(&status, "status");
    let value = parse_json(&status);
    assert_eq!(value["state"], "stopped", "got: {value}");
    assert_eq!(value["agents"][0]["state"], "missing_pane", "got: {value}");

    let list = bed.kira(&["list", "--json"]);
    assert_success(&list, "list");
    assert_eq!(parse_json(&list)[0]["state"], "stopped");
}

#[test]
fn list_json_surfaces_invalid_project_configs_and_exits_2() {
    let bed = TestBed::new();
    bed.write_project(CAT_AGENT);

    // Unknown field — deny_unknown_fields rejects the whole file.
    write_file(
        &bed.projects_dir().join("mystery.toml"),
        r#"
id = "mystery"
root = "/tmp/mystery"
nope = true

[[agents]]
id = "alpha"
command = "echo"
"#,
    );

    // Malformed TOML — no usable body.
    write_file(
        &bed.projects_dir().join("garbage.toml"),
        "id = [\nnot toml\n",
    );

    let list = bed.kira(&["list", "--json"]);
    assert_eq!(
        exit_code(&list),
        2,
        "list must exit 2 when configs are broken, stderr: {:?}",
        stderr_of(&list)
    );

    // stdout is still one valid JSON document with both healthy and broken rows.
    let value = parse_json(&list);
    let rows = value
        .as_array()
        .unwrap_or_else(|| panic!("list --json must be an array, got: {value}"));

    let good = rows.iter().find(|row| row["id"] == "it");
    assert!(
        good.is_some_and(|row| row["state"] == "stopped"),
        "valid project still listed, got: {value}"
    );

    let mystery = rows.iter().find(|row| row["id"] == "mystery");
    assert!(
        mystery.is_some_and(|row| {
            row["state"] == "config_error"
                && row["error"].as_str().is_some_and(|e| !e.is_empty())
                && row["path"]
                    .as_str()
                    .is_some_and(|p| p.ends_with("mystery.toml"))
        }),
        "unknown-field project must surface as config_error, got: {value}"
    );

    let garbage = rows.iter().find(|row| {
        row["state"] == "config_error"
            && row["path"]
                .as_str()
                .is_some_and(|p| p.ends_with("garbage.toml"))
    });
    assert!(
        garbage.is_some(),
        "malformed TOML must surface as config_error, got: {value}"
    );

    // stderr carries the aggregate message; details stay on stdout JSON.
    assert!(
        stderr_of(&list).contains("failed to load"),
        "got stderr: {:?}",
        stderr_of(&list)
    );
}

#[test]
fn init_writes_config_files_and_never_clobbers_them() {
    let bed = TestBed::new();

    assert_success(&bed.kira(&["init"]), "init");
    let example = bed.projects_dir().join("example.toml");
    assert!(example.exists(), "init must write the example project");

    write_file(&example, "# customized\n");
    assert_success(&bed.kira(&["init"]), "second init");
    let kept = match fs::read_to_string(&example) {
        Ok(contents) => contents,
        Err(error) => panic!("failed to read example project: {error}"),
    };
    assert_eq!(
        kept, "# customized\n",
        "init without --force must keep files"
    );
}

// ---------------------------------------------------------------------------
// Exit-code contract
// ---------------------------------------------------------------------------

#[test]
fn send_to_absent_session_exits_5() {
    let bed = TestBed::new();
    bed.write_project(CAT_AGENT);

    let send = bed.kira(&["send", "it", "alpha", "hello"]);
    assert_eq!(
        exit_code(&send),
        5,
        "absent session must exit 5, stderr: {:?}",
        stderr_of(&send)
    );
}

#[test]
fn unknown_agent_and_unknown_project_exit_2() {
    let bed = TestBed::new();
    bed.write_project(CAT_AGENT);

    let send = bed.kira(&["send", "it", "nope", "hello"]);
    assert_eq!(exit_code(&send), 2, "unknown agent id must exit 2");

    let status = bed.kira(&["status", "nope"]);
    assert_eq!(exit_code(&status), 2, "unknown project id must exit 2");
}

#[test]
fn missing_tmux_binary_exits_3() {
    let bed = TestBed::new();
    bed.write_project(CAT_AGENT);
    write_file(
        &bed.config_home.path().join("kira-mux/config.toml"),
        "tmux_bin = \"/nonexistent/kira-mux-it-tmux\"\n",
    );

    let start = bed.kira(&["start", "it"]);
    assert_eq!(
        exit_code(&start),
        3,
        "missing tmux binary must exit 3, stderr: {:?}",
        stderr_of(&start)
    );
}

#[test]
fn config_drift_shows_in_status_and_list_and_send_exits_4() {
    let bed = TestBed::new();
    bed.write_project(CAT_AGENT);
    assert_success(&bed.kira(&["start", "it"]), "start");
    bed.wait_for_state("running");

    // Topology-affecting config change after launch: the stored fingerprint
    // no longer matches the resolved project.
    bed.write_project(
        "[[agents]]\nid = \"alpha\"\nmode = \"shell\"\nshell_command = \"exec cat -u\"\n",
    );

    assert_eq!(bed.wait_for_state("drifted")["state"], "drifted");

    let list = bed.kira(&["list", "--json"]);
    assert_success(&list, "list");
    assert_eq!(parse_json(&list)[0]["state"], "drifted");

    let send = bed.kira(&["send", "it", "alpha", "hello"]);
    assert_eq!(
        exit_code(&send),
        4,
        "send into a drifted workspace must exit 4, stderr: {:?}",
        stderr_of(&send)
    );
}

#[test]
fn dead_pane_degrades_workspace_send_exits_6_capture_still_works() {
    let bed = TestBed::new();
    bed.write_project(&format!(
        "{CAT_AGENT}\n[[agents]]\nid = \"omega\"\nmode = \"shell\"\nshell_command = \"exit 7\"\n"
    ));

    // Post-launch health check must observe omega's immediate exit and
    // surface the degraded exit code (issue #13).
    let start = bed.kira(&["start", "it"]);
    assert_eq!(
        exit_code(&start),
        6,
        "start must exit 6 when an agent dies immediately, stderr: {:?}",
        stderr_of(&start)
    );
    let status = bed.wait_for_state("degraded");
    assert_eq!(
        status["agents"][1]["state"], "exited_failed",
        "got: {status}"
    );

    let send_dead = bed.kira(&["send", "it", "omega", "hello"]);
    assert_eq!(
        exit_code(&send_dead),
        6,
        "send to a dead pane must exit 6, stderr: {:?}",
        stderr_of(&send_dead)
    );

    // Contract: send rejects dead panes, capture allows them (post-mortem).
    let capture = bed.kira(&["capture", "it", "omega", "--json"]);
    assert_success(&capture, "capture of dead pane");
    assert_eq!(parse_json(&capture)["pane_dead"], true);

    // A live pane inside a degraded workspace still accepts prompts.
    assert_success(
        &bed.kira(&["send", "it", "alpha", "still alive"]),
        "send to live pane in degraded workspace",
    );
}

#[test]
fn restart_revives_dead_agent_once_its_command_succeeds() {
    let bed = TestBed::new();
    let ready_flag = format!("{}/ready", bed.root());
    bed.write_project(&format!(
        "[[agents]]\nid = \"solo\"\nmode = \"shell\"\nshell_command = \"[ -f {ready_flag} ] && exec cat || exit 7\"\n"
    ));

    let start = bed.kira(&["start", "it"]);
    assert_eq!(
        exit_code(&start),
        6,
        "start must exit 6 on immediate failure, stderr: {:?}",
        stderr_of(&start)
    );
    bed.wait_for_state("degraded");

    write_file(std::path::Path::new(&ready_flag), "");
    assert_success(&bed.kira(&["restart", "it", "solo"]), "restart");
    bed.wait_for_state("running");
}

#[test]
fn env_reference_host_value_rotation_requires_restart() {
    // Contract (#16): `$VAR` fingerprints the name only. Host value rotation
    // does not drift the session; healthy `start` keeps the old injection.
    // `restart` re-resolves and re-applies.
    let bed = TestBed::new();
    bed.write_project(
        r#"[[agents]]
id = "alpha"
mode = "shell"
shell_command = "printf 'token=%s\n' \"$TOKEN\"; exec cat"
env = { TOKEN = "$KIRA_IT_TOKEN" }
"#,
    );

    assert_success(
        &bed.kira_with_env(&["start", "it"], &[("KIRA_IT_TOKEN", "one")]),
        "start with token=one",
    );
    bed.wait_for_state("running");
    bed.wait_for_capture("alpha", "token=one");

    // Same config, rotated host value — start must not refresh the pane.
    assert_success(
        &bed.kira_with_env(&["start", "it"], &[("KIRA_IT_TOKEN", "two")]),
        "start with token=two (healthy no-op)",
    );
    assert_eq!(bed.wait_for_state("running")["state"], "running");
    let still_one = bed.kira(&["capture", "it", "alpha"]);
    assert_success(&still_one, "capture after no-op start");
    let text = stdout_of(&still_one);
    assert!(
        text.contains("token=one"),
        "pane must still show the original injection, got: {text:?}"
    );
    assert!(
        !text.contains("token=two"),
        "start must not re-inject rotated $VAR values, got: {text:?}"
    );

    assert_success(
        &bed.kira_with_env(&["restart", "it", "alpha"], &[("KIRA_IT_TOKEN", "two")]),
        "restart applies token=two",
    );
    bed.wait_for_state("running");
    bed.wait_for_capture("alpha", "token=two");
}

#[test]
fn start_exits_6_for_missing_executable() {
    let bed = TestBed::new();
    bed.write_project(
        r#"[[agents]]
id = "ghost"
mode = "direct"
command = "/nonexistent/kira-mux-missing-agent-bin"
"#,
    );

    let start = bed.kira(&["start", "it"]);
    assert_eq!(
        exit_code(&start),
        6,
        "missing executable must degrade start, stderr: {:?}",
        stderr_of(&start)
    );
    let status = bed.wait_for_state("degraded");
    assert_eq!(
        status["agents"][0]["state"], "exited_failed",
        "got: {status}"
    );

    // Repair path must also report degraded, not a false success.
    let again = bed.kira(&["start", "it"]);
    assert_eq!(
        exit_code(&again),
        6,
        "repair start must stay degraded, stderr: {:?}",
        stderr_of(&again)
    );
}

// ---------------------------------------------------------------------------
// Send/capture fidelity against real tmux
// ---------------------------------------------------------------------------

#[test]
fn send_and_capture_roundtrip_through_paste() {
    let bed = TestBed::new();
    bed.write_project(CAT_AGENT);
    assert_success(&bed.kira(&["start", "it"]), "start");
    bed.wait_for_state("running");

    assert_success(
        &bed.kira(&["send", "it", "alpha", "hello from kira integration"]),
        "send",
    );
    bed.wait_for_capture("alpha", "hello from kira integration");
}

#[test]
fn send_keys_agents_receive_hostile_text_verbatim() {
    // A command whose basename is `opencode` selects the send-keys -l
    // delivery path — the layer where unescaped trailing `;`, leading
    // dashes, or key names like `Enter` historically corrupted prompts.
    let bed = TestBed::new();
    let script = bed.project_root.path().join("opencode");
    write_file(&script, "#!/bin/sh\nexec cat\n");
    make_executable(&script);
    bed.write_project(&format!(
        "[[agents]]\nid = \"oc\"\ncommand = \"{}\"\n",
        script.display()
    ));

    assert_success(&bed.kira(&["start", "it"]), "start");
    bed.wait_for_state("running");

    let hostile = "-l -- Enter Escape C-c kill-server;";
    assert_success(&bed.kira(&["send", "it", "oc", "--", hostile]), "send");
    let captured = bed.wait_for_capture("oc", hostile);
    assert!(
        !captured.contains("\\;"),
        "escape must not leak into the pane: {captured:?}"
    );
    // The pane (and the server) survived text that looks like tmux commands.
    bed.wait_for_state("running");
}

#[test]
fn capture_honors_line_limit_and_strips_screen_padding() {
    let bed = TestBed::new();
    bed.write_project(
        "[[agents]]\nid = \"alpha\"\nmode = \"shell\"\nshell_command = \"seq 1 200; exec cat\"\n",
    );
    assert_success(&bed.kira(&["start", "it"]), "start");
    bed.wait_for_capture("alpha", "200");

    let capture = bed.kira(&["capture", "it", "alpha", "--lines", "5", "--json"]);
    assert_success(&capture, "capture --lines 5");
    let value = parse_json(&capture);
    assert_eq!(value["lines"], 5);
    assert_eq!(value["pane_dead"], false);
    let output = value["output"]
        .as_str()
        .map_or_else(String::new, str::to_owned);
    let lines: Vec<&str> = output.lines().collect();
    assert_eq!(
        lines,
        vec!["196", "197", "198", "199", "200"],
        "capture must return the last 5 content lines, without the blank \
         screen padding real tmux appends below them"
    );
}

#[test]
fn agents_list_reports_live_runtime_state() {
    let bed = TestBed::new();
    bed.write_project(CAT_AGENT);
    assert_success(&bed.kira(&["start", "it"]), "start");
    bed.wait_for_state("running");

    let agents = bed.kira(&["agents", "list", "it", "--json"]);
    assert_success(&agents, "agents list");
    let value = parse_json(&agents);
    assert_eq!(value["agents"][0]["id"], "alpha", "got: {value}");
    assert_eq!(value["agents"][0]["state"], "running", "got: {value}");
}

fn make_executable(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Err(error) = fs::set_permissions(path, fs::Permissions::from_mode(0o755)) {
        panic!("failed to chmod {}: {error}", path.display());
    }
}
