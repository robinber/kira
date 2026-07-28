//! Send/capture/wait fidelity against real tmux (including deep capture).

use std::time::{Duration, Instant};

use crate::harness::*;

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
fn send_wait_survives_prompt_echo_before_delayed_answer() {
    let bed = TestBed::new();
    let script = bed.project_root.path().join("wait-agent");
    write_file(
        &script,
        "#!/bin/sh\nwhile IFS= read -r line; do\n  sleep 1\n  printf 'answer chunk: %s\\n' \"$line\"\n  sleep 0.5\n  printf 'answer final: WAIT_OK\\n'\ndone\n",
    );
    make_executable(&script);
    bed.write_project(&format!(
        "[[agents]]\nid = \"alpha\"\ncommand = \"{}\"\n",
        script.display()
    ));
    assert_success(&bed.kira(&["start", "it"]), "start");
    bed.wait_for_state("running");

    let started = Instant::now();
    let waited = bed.kira_within(
        Duration::from_mins(2),
        &["send", "it", "alpha", "race probe", "--wait"],
    );

    assert_success(&waited, "send --wait");
    let output = stdout_of(&waited);
    assert!(
        output.contains("answer chunk: race probe") && output.contains("answer final: WAIT_OK"),
        "wait must capture the full delayed reply, got: {output:?}"
    );
    // The quiet timer must restart after the delayed answer: under the fast
    // wait profile the final line prints at ~1.5 s and the shortest window
    // after durable production is 1.5 s, so an early return sits well below
    // this bound.
    assert!(
        started.elapsed() >= Duration::from_millis(2900),
        "wait returned before the post-answer quiet window elapsed"
    );
}

#[test]
fn send_lines_without_wait_is_rejected() {
    let bed = TestBed::new();
    bed.write_project(CAT_AGENT);
    // Clap rejects this before any project load / tmux work.
    let rejected = bed.kira(&["send", "it", "alpha", "hello", "--lines", "50"]);
    assert_eq!(
        exit_code(&rejected),
        2,
        "send --lines without --wait must exit 2, stderr: {:?}",
        stderr_of(&rejected)
    );
    let stderr = stderr_of(&rejected);
    assert!(
        stderr.contains("--wait") || stderr.contains("wait"),
        "error should mention --wait dependency, got: {stderr:?}"
    );
}

#[test]
fn send_wait_zero_lines_is_rejected() {
    let bed = TestBed::new();
    bed.write_project(CAT_AGENT);
    // Zero would empty every capture and stall wait until the hard timeout.
    let rejected = bed.kira(&["send", "it", "alpha", "hello", "--wait", "--lines", "0"]);
    assert_eq!(
        exit_code(&rejected),
        2,
        "send --wait --lines 0 must exit 2 before project/tmux work, stderr: {:?}",
        stderr_of(&rejected)
    );
    let stderr = stderr_of(&rejected);
    assert!(
        stderr.contains("at least 1") || stderr.contains("zero"),
        "error should reject zero lines, got: {stderr:?}"
    );
}

#[test]
fn send_wait_with_lines_still_captures_full_reply() {
    let bed = TestBed::new();
    let script = bed.project_root.path().join("wait-agent-lines");
    write_file(
        &script,
        "#!/bin/sh\nwhile IFS= read -r line; do\n  sleep 1\n  printf 'answer chunk: %s\\n' \"$line\"\n  sleep 0.5\n  printf 'answer final: LINES_OK\\n'\ndone\n",
    );
    make_executable(&script);
    bed.write_project(&format!(
        "[[agents]]\nid = \"alpha\"\ncommand = \"{}\"\n",
        script.display()
    ));
    assert_success(&bed.kira(&["start", "it"]), "start");
    bed.wait_for_state("running");

    let waited = bed.kira_within(
        Duration::from_mins(2),
        &[
            "send",
            "it",
            "alpha",
            "lines probe",
            "--wait",
            "--lines",
            "80",
        ],
    );
    assert_success(&waited, "send --wait --lines");
    let output = stdout_of(&waited);
    assert!(
        output.contains("answer chunk: lines probe") && output.contains("answer final: LINES_OK"),
        "wait --lines must still capture the full delayed reply, got: {output:?}"
    );
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
fn capture_deepens_alternate_screen_tui_and_restores_window() {
    // Mini alternate-screen TUI, faithful to real agent TUIs (Claude Code,
    // Grok Build): it keeps a 100-line transcript internally, repaints only
    // the visible tail, and never scrolls the terminal — so tmux history
    // stays empty and plain capture is capped at the pane height.
    let bed = TestBed::new();
    let script = bed.project_root.path().join("mini-alt-tui");
    write_file(
        &script,
        r#"#!/bin/sh
printf '\033[?1049h'
total=100
repaint() {
  rows=$(stty size < /dev/tty 2>/dev/null | cut -d' ' -f1)
  [ -n "$rows" ] || rows=24
  printf '\033[2J\033[H'
  start=$((total - rows + 2))
  [ "$start" -lt 1 ] && start=1
  i=$start
  while [ "$i" -le "$total" ]; do
    printf 'transcript line %s\n' "$i"
    i=$((i + 1))
  done
}
trap repaint WINCH
repaint
while :; do sleep 1; done
"#,
    );
    make_executable(&script);
    // Multi-pane workspace (the product's normal topology): the TUI is the
    // second, initially inactive pane, so deep capture must zoom it, then
    // hand the active pane back.
    bed.write_project(&format!(
        "{CAT_AGENT}\n[[agents]]\nid = \"tui\"\ncommand = \"{}\"\n",
        script.display()
    ));
    assert_success(&bed.kira(&["start", "it"]), "start");
    bed.wait_for_state("running");

    // Shallow capture (10 ≤ pane height): stays plain and proves the pane is
    // genuinely depth-capped — the transcript head is unreachable.
    let shallow = wait_until("alt-screen TUI to paint its tail", || {
        let output = bed.kira(&["capture", "it", "tui", "--lines", "10"]);
        let text = stdout_of(&output);
        (output.status.success() && text.contains("transcript line 100")).then_some(text)
    });
    assert!(
        !shallow.contains("transcript line 1\n"),
        "the visible frame must not reach the transcript head: {shallow:?}"
    );

    // Snapshot the pre-capture window state the restore must reproduce.
    let window = format!("{}:agents", managed_session_name(&bed));
    let before = window_state(&bed, &window);

    // Deep request: the resize-based capture must recover the full transcript.
    let deep = bed.kira(&["capture", "it", "tui", "--lines", "200", "--json"]);
    assert_success(&deep, "deep capture");
    let value = parse_json(&deep);
    assert_eq!(value["alternate_on"], true, "got: {value}");
    assert_eq!(value["deep_capture"], true, "got: {value}");
    assert_eq!(value["deep_capture_status"], "completed", "got: {value}");
    assert_eq!(value["depth_request_clamped"], false, "got: {value}");
    assert!(
        value["pane_height"].as_u64().is_some_and(|h| h > 0),
        "pane_height must be reported, got: {value}"
    );
    let output = value["output"]
        .as_str()
        .map_or_else(String::new, str::to_owned);
    assert!(
        output.contains("transcript line 1\n") && output.contains("transcript line 100"),
        "deep capture must recover the whole transcript, got head: {:?}",
        output.lines().take(3).collect::<Vec<_>>()
    );

    // The window must come back exactly as before: size, zoom, active pane,
    // and the multi-pane layout, with no leftover window-local `window-size`
    // override.
    assert_eq!(
        window_state(&bed, &window),
        before,
        "deep capture must restore size, zoom, active pane, and layout"
    );
    let size_option = bed.tmux(&["show-options", "-w", "-t", &window, "window-size"]);
    assert_eq!(
        stdout_of(&size_option).trim(),
        "",
        "deep capture must not leave a window-size manual override behind"
    );
}

#[test]
fn send_wait_deepens_alternate_screen_reply_and_restores_window() {
    // The exact #69 route, end to end: an alternate-screen TUI that reads a
    // prompt from stdin and answers with a reply taller than the pane.
    // `send --wait` must print the WHOLE reply (deepened final capture), and
    // leave the window exactly as found.
    let bed = TestBed::new();
    let script = bed.project_root.path().join("mini-wait-tui");
    write_file(
        &script,
        r#"#!/bin/sh
printf '\033[?1049h'
total=0
header=""
repaint() {
  rows=$(stty size < /dev/tty 2>/dev/null | cut -d' ' -f1)
  [ -n "$rows" ] || rows=24
  printf '\033[2J\033[H'
  [ -n "$header" ] && printf '%s\n' "$header"
  start=$((total - rows + 3))
  [ "$start" -lt 1 ] && start=1
  i=$start
  while [ "$i" -le "$total" ]; do
    printf 'reply line %s\n' "$i"
    i=$((i + 1))
  done
}
trap repaint WINCH
repaint
# A trapped WINCH (e.g. a layout resize) can interrupt read and make it
# fail on dash without consuming anything: retry until a real prompt
# arrives, and echo it into the reply so the test proves the reply is a
# response to the sent prompt, not a generic paint.
prompt=""
until [ -n "$prompt" ]; do
  IFS= read -r prompt || prompt=""
done
header="reply to: $prompt"
total=100
repaint
while :; do sleep 1; done
"#,
    );
    make_executable(&script);
    bed.write_project(&format!(
        "[[agents]]\nid = \"tui\"\ncommand = \"{}\"\n",
        script.display()
    ));
    assert_success(&bed.kira(&["start", "it"]), "start");
    bed.wait_for_state("running");
    let window = format!("{}:agents", managed_session_name(&bed));
    let before = window_state(&bed, &window);

    let waited = bed.kira_within(
        Duration::from_mins(3),
        &[
            "send",
            "it",
            "tui",
            "answer tall",
            "--wait",
            "--lines",
            "200",
        ],
    );
    assert_success(&waited, "send --wait to alt-screen TUI");
    let output = stdout_of(&waited);
    assert!(
        output.contains("reply to: answer tall"),
        "the reply must provably respond to the sent prompt (the TUI \
         consumed it), got head: {:?}",
        output.lines().take(3).collect::<Vec<_>>()
    );
    assert!(
        output.contains("reply line 1\n") && output.contains("reply line 100"),
        "wait must return the whole reply through the alternate screen, \
         got head: {:?}",
        output.lines().take(3).collect::<Vec<_>>()
    );

    assert_eq!(
        window_state(&bed, &window),
        before,
        "the deepened final capture must restore the window exactly"
    );
    let size_option = bed.tmux(&["show-options", "-w", "-t", &window, "window-size"]);
    assert_eq!(
        stdout_of(&size_option).trim(),
        "",
        "no window-size override may survive the deepened wait capture"
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
