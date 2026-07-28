//! Workspace lifecycle, list, init, and contextual project target.

use std::fs;

use crate::harness::*;

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
    let name = managed_session_name(&bed);
    assert!(
        !name.contains('\n'),
        "expected exactly one session: {name:?}"
    );
    assert!(
        name.starts_with("kira-it-default-"),
        "unexpected session name: {name:?}"
    );

    // list goes through the bulk workspace snapshot path (session options +
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

    let managed_name = managed_session_name(&bed);
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
fn contextual_project_target_drives_workspace_from_nested_directory() {
    let bed = TestBed::new();
    bed.write_project(CAT_AGENT);
    let nested = bed.project_root.path().join("src/nested");
    if let Err(error) = fs::create_dir_all(&nested) {
        panic!("failed to create contextual nested directory: {error}");
    }

    assert_success(&bed.kira_from(&nested, &["start", "."]), "contextual start");

    let status = bed.kira_from(&nested, &["status", ".", "--json"]);
    assert_success(&status, "contextual status");
    let status = parse_json(&status);
    assert_eq!(status["id"], "it", "got: {status}");
    assert_eq!(status["root"], bed.root(), "got: {status}");

    let agents = bed.kira_from(&nested, &["agents", "list", ".", "--json"]);
    assert_success(&agents, "contextual agents list");
    assert_eq!(parse_json(&agents)["agents"][0]["id"], "alpha");

    // An explicit ID must resolve the same session that contextual start made.
    assert_success(&bed.kira(&["start", "it"]), "explicit idempotent start");
    let sessions = bed.tmux(&["list-sessions", "-F", "#{session_name}"]);
    assert_success(&sessions, "list contextual sessions");
    assert_eq!(
        stdout_of(&sessions).lines().count(),
        1,
        "contextual and explicit targets must share one session"
    );

    assert_success(
        &bed.kira_from(&nested, &["send", ".", "alpha", "contextual hello"]),
        "contextual send",
    );
    wait_until("contextual capture to contain delivered prompt", || {
        let output = bed.kira_from(&nested, &["capture", ".", "alpha"]);
        let text = stdout_of(&output);
        (output.status.success() && text.contains("contextual hello")).then_some(text)
    });

    assert_success(
        &bed.kira_from(&nested, &["restart", ".", "alpha"]),
        "contextual restart",
    );
    assert_success(
        &bed.kira_from(&nested, &["kill", ".", "--yes"]),
        "contextual kill",
    );
    let stopped = bed.kira_from(&nested, &["status", ".", "--json"]);
    assert_success(&stopped, "contextual stopped status");
    assert_eq!(parse_json(&stopped)["state"], "stopped");
}

#[test]
fn contextual_project_target_reports_zero_match() {
    let bed = TestBed::new();
    bed.write_project(CAT_AGENT);
    let outside = make_tempdir("outside contextual root");

    let status = bed.kira_from(outside.path(), &["status", "."]);

    assert_eq!(
        exit_code(&status),
        2,
        "zero contextual match must exit 2, stderr: {:?}",
        stderr_of(&status)
    );
    assert!(
        stderr_of(&status).contains("no registered project contains current directory"),
        "got stderr: {:?}",
        stderr_of(&status)
    );
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
    assert!(
        garbage.is_some_and(|row| row["id"] == "garbage" && row.get("profile_id").is_none()),
        "whole-file failure must keep the filename id and omit profile_id, got: {value}"
    );

    // stderr carries the aggregate message; details stay on stdout JSON.
    assert!(
        stderr_of(&list).contains("failed to load"),
        "got stderr: {:?}",
        stderr_of(&list)
    );
}

#[test]
fn list_redacts_toml_source_secrets_from_config_diagnostics() {
    // Malformed literal env value: toml Display embeds the source line, which
    // would leak the secret into list/json and warn logs if left unredacted.
    const SENTINEL: &str = "super-secret-value-do-not-leak";

    let bed = TestBed::new();
    bed.write_project(CAT_AGENT);
    write_file(
        &bed.projects_dir().join("leaky.toml"),
        &format!("env = {{ TOKEN = \"{SENTINEL}\n"),
    );

    // Force warn logs so the skip-path tracing::warn is visible even if
    // defaults change; secrets must still be absent from stderr.
    let list_json = bed.kira_with_env(
        &["list", "--json"],
        &[("KIRA_MUX_LOG", "warn"), ("RUST_LOG", "warn")],
    );
    assert_eq!(
        exit_code(&list_json),
        2,
        "list --json must exit 2, stderr: {:?}",
        stderr_of(&list_json)
    );

    let json_out = stdout_of(&list_json);
    let json_err = stderr_of(&list_json);
    assert!(
        !json_out.contains(SENTINEL),
        "list --json stdout must not contain secret: {json_out}"
    );
    assert!(
        !json_err.contains(SENTINEL),
        "list --json stderr must not contain secret: {json_err}"
    );

    let value = parse_json(&list_json);
    let rows = value
        .as_array()
        .unwrap_or_else(|| panic!("list --json must be an array, got: {value}"));
    let leaky = rows.iter().find(|row| {
        row["state"] == "config_error"
            && row["path"]
                .as_str()
                .is_some_and(|p| p.ends_with("leaky.toml"))
    });
    let leaky = leaky.unwrap_or_else(|| panic!("leaky.toml must surface as config_error: {value}"));
    let error = leaky["error"]
        .as_str()
        .unwrap_or_else(|| panic!("config_error row needs error string: {leaky}"));
    assert!(!error.is_empty(), "error diagnostic must remain actionable");
    assert!(
        !error.contains(SENTINEL),
        "structured error must not contain secret: {error}"
    );

    let list_text = bed.kira_with_env(&["list"], &[("KIRA_MUX_LOG", "warn"), ("RUST_LOG", "warn")]);
    assert_eq!(
        exit_code(&list_text),
        2,
        "list must exit 2, stderr: {:?}",
        stderr_of(&list_text)
    );
    let text_out = stdout_of(&list_text);
    let text_err = stderr_of(&list_text);
    assert!(
        !text_out.contains(SENTINEL),
        "list stdout must not contain secret: {text_out}"
    );
    assert!(
        !text_err.contains(SENTINEL),
        "list stderr must not contain secret: {text_err}"
    );
    assert!(
        text_out.contains("config_error") || text_err.contains("failed to load"),
        "text mode must still surface the failure: stdout={text_out:?} stderr={text_err:?}"
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
