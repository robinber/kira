//! Stable CLI exit-code contract against a real binary and tmux.

use crate::harness::*;

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
