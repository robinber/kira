// Readiness is operator-managed: these tests only assert dead-pane /
// topology gates. There is intentionally no “wait until TUI idle” path.
use super::*;
use crate::test_support::{FakeOp, TestResultExt, setup_session_with_dead_panes};
use crate::workspace::session_name;

#[test]
fn send_prompt_generic_agent_sends_paste_then_single_enter() {
    let fake = crate::test_support::FakeTmux::new();
    let project = crate::test_support::test_project();
    crate::test_support::setup_healthy_session(&fake, &project);

    send_prompt(&fake, &project, "alpha", "hello world", false)
        .or_panic("send_prompt_generic_agent_sends_paste_then_single_enter");

    let ops = fake.ops();
    assert_eq!(ops.len(), 2, "expected paste + 1 Enter, got: {ops:?}");
    assert_eq!(
        ops[0],
        FakeOp::PasteText {
            pane_id: "%0".to_string(),
            text: "hello world".to_string(),
        }
    );
    assert_eq!(
        ops[1],
        FakeOp::SendKeys {
            pane_id: "%0".to_string(),
            keys: vec!["Enter".to_string()],
        }
    );
}

#[test]
fn send_prompt_codex_agent_sends_paste_then_double_enter() {
    let fake = crate::test_support::FakeTmux::new();
    let mut project = crate::test_support::test_project();
    project.agents[0].command = Some("codex".to_string());
    crate::test_support::setup_healthy_session(&fake, &project);

    send_prompt(&fake, &project, "alpha", "review this", false)
        .or_panic("send_prompt_codex_agent_sends_paste_then_double_enter");

    let ops = fake.ops();
    assert_eq!(ops.len(), 3, "expected paste + 2 Enters, got: {ops:?}");
    assert_eq!(
        ops[0],
        FakeOp::PasteText {
            pane_id: "%0".to_string(),
            text: "review this".to_string(),
        }
    );
    assert_eq!(
        ops[1],
        FakeOp::SendKeys {
            pane_id: "%0".to_string(),
            keys: vec!["Enter".to_string()],
        }
    );
    assert_eq!(
        ops[2],
        FakeOp::SendKeys {
            pane_id: "%0".to_string(),
            keys: vec!["Enter".to_string()],
        }
    );
}

#[test]
fn send_prompt_opencode_agent_types_literal_text() {
    let fake = crate::test_support::FakeTmux::new();
    let mut project = crate::test_support::test_project();
    project.agents[0].command = Some("opencode".to_string());
    crate::test_support::setup_healthy_session(&fake, &project);

    // A prompt matching a tmux key name must arrive as literal text, not
    // as a keypress.
    send_prompt(&fake, &project, "alpha", "Enter", false)
        .or_panic("send_prompt_opencode_agent_types_literal_text");

    let ops = fake.ops();
    assert_eq!(
        ops[0],
        FakeOp::SendText {
            pane_id: "%0".to_string(),
            text: "Enter".to_string(),
        },
        "prompt text must go through the literal-text channel, got: {ops:?}"
    );
    assert!(
        ops[1..]
            .iter()
            .all(|op| matches!(op, FakeOp::SendKeys { keys, .. } if keys == &["Enter"])),
        "remaining ops must be Enter submits, got: {ops:?}"
    );
}

#[test]
fn send_prompt_reads_pane_metadata_for_submit_behavior() {
    let fake = crate::test_support::FakeTmux::new();
    let project = crate::test_support::test_project();
    crate::test_support::setup_healthy_session(&fake, &project);
    let session = session_name(&project);
    fake.set_pane_opt(
        &session,
        &project.window_name,
        0,
        PANE_AGENT_COMMAND,
        "codex",
    );

    send_prompt(&fake, &project, "alpha", "hello", false)
        .or_panic("send_prompt_reads_pane_metadata_for_submit_behavior");

    let ops = fake.ops();
    assert_eq!(ops.len(), 3, "expected paste + 2 Enters, got: {ops:?}");
    assert_eq!(
        ops[0],
        FakeOp::PasteText {
            pane_id: "%0".to_string(),
            text: "hello".to_string(),
        }
    );
    assert_eq!(
        ops[1],
        FakeOp::SendKeys {
            pane_id: "%0".to_string(),
            keys: vec!["Enter".to_string()],
        }
    );
    assert_eq!(
        ops[2],
        FakeOp::SendKeys {
            pane_id: "%0".to_string(),
            keys: vec!["Enter".to_string()],
        }
    );
}

#[test]
fn send_prompt_falls_back_without_pane_metadata() {
    let fake = crate::test_support::FakeTmux::new();
    let mut project = crate::test_support::test_project();
    project.agents[0].command = Some("codex".to_string());
    crate::test_support::setup_healthy_session(&fake, &project);

    send_prompt(&fake, &project, "alpha", "hello", false)
        .or_panic("send_prompt_falls_back_without_pane_metadata");

    let ops = fake.ops();
    assert_eq!(ops.len(), 3, "expected paste + 2 Enters, got: {ops:?}");
}

#[test]
fn send_prompt_dead_pane_fails() {
    let fake = crate::test_support::FakeTmux::new();
    let project = crate::test_support::test_project();
    setup_session_with_dead_panes(&fake, &project, &[0]);

    let err = send_prompt(&fake, &project, "alpha", "hello", false)
        .err_or_panic("send_prompt_dead_pane_fails: expected Err");
    assert!(matches!(
        err.downcast_ref::<KiraMuxError>(),
        Some(KiraMuxError::DeadPane(id)) if id == "alpha"
    ));
    assert!(fake.ops().is_empty());
}

#[test]
fn send_prompt_absent_session_fails() {
    let fake = crate::test_support::FakeTmux::new();
    let project = crate::test_support::test_project();
    let err = send_prompt(&fake, &project, "alpha", "hello", false)
        .err_or_panic("send_prompt_absent_session_fails: expected Err");
    assert!(matches!(
        err.downcast_ref::<KiraMuxError>(),
        Some(KiraMuxError::SessionAbsent)
    ));
}

#[test]
fn send_prompt_propagates_paste_failure() {
    let fake = crate::test_support::FakeTmux::new();
    let project = crate::test_support::test_project();
    crate::test_support::setup_healthy_session(&fake, &project);
    fake.set_fail_paste(true);

    let err = send_prompt(&fake, &project, "alpha", "hello", false)
        .err_or_panic("send_prompt_propagates_paste_failure: expected Err");
    // Generic transport failures stay untyped (exit 1), not DeadPane.
    assert!(
        err.downcast_ref::<KiraMuxError>().is_none(),
        "generic paste failure must not map to DeadPane, got: {err}"
    );
}

#[test]
fn send_prompt_propagates_send_keys_failure() {
    let fake = crate::test_support::FakeTmux::new();
    let project = crate::test_support::test_project();
    crate::test_support::setup_healthy_session(&fake, &project);
    fake.set_fail_send_keys(true);

    let result = send_prompt(&fake, &project, "alpha", "hello", false);
    assert!(result.is_err());
}

#[test]
fn send_prompt_maps_missing_target_mid_submit_to_dead_pane() {
    let fake = crate::test_support::FakeTmux::new();
    let project = crate::test_support::test_project();
    crate::test_support::setup_healthy_session(&fake, &project);
    // Pane is live at resolve time; delivery ops then report MissingTarget
    // (pane killed between the liveness gate and paste/send-keys).
    fake.set_fail_delivery_missing_target(true);

    let err = send_prompt(&fake, &project, "alpha", "hello", false)
        .err_or_panic("send_prompt_maps_missing_target_mid_submit_to_dead_pane: expected Err");
    assert!(
        matches!(
            err.downcast_ref::<KiraMuxError>(),
            Some(KiraMuxError::DeadPane(id)) if id == "alpha"
        ),
        "vanished pane during submit must be typed DeadPane (exit 6), got: {err}"
    );
}

#[test]
fn send_prompt_maps_missing_target_on_send_text_path_to_dead_pane() {
    let fake = crate::test_support::FakeTmux::new();
    let mut project = crate::test_support::test_project();
    project.agents[0].command = Some("opencode".to_string());
    crate::test_support::setup_healthy_session(&fake, &project);
    fake.set_fail_delivery_missing_target(true);

    let err = send_prompt(&fake, &project, "alpha", "hello", false).err_or_panic(
        "send_prompt_maps_missing_target_on_send_text_path_to_dead_pane: expected Err",
    );
    assert!(
        matches!(
            err.downcast_ref::<KiraMuxError>(),
            Some(KiraMuxError::DeadPane(id)) if id == "alpha"
        ),
        "vanished pane on send-text delivery must be typed DeadPane, got: {err}"
    );
}

#[test]
fn send_prompt_maps_server_loss_mid_submit_to_dead_pane() {
    let fake = crate::test_support::FakeTmux::new();
    let project = crate::test_support::test_project();
    crate::test_support::setup_healthy_session(&fake, &project);
    // Removing the only pane also stops an isolated tmux server, which
    // reports NoServer instead of MissingTarget on the next command.
    fake.set_fail_delivery_no_server(true);

    let err = send_prompt(&fake, &project, "alpha", "hello", false)
        .err_or_panic("send_prompt_maps_server_loss_mid_submit_to_dead_pane: expected Err");
    assert!(
        matches!(
            err.downcast_ref::<KiraMuxError>(),
            Some(KiraMuxError::DeadPane(id)) if id == "alpha"
        ),
        "server loss during submit must be typed DeadPane (exit 6), got: {err}"
    );
}

#[test]
fn send_prompt_empty_skips_paste() {
    let fake = crate::test_support::FakeTmux::new();
    let project = crate::test_support::test_project();
    crate::test_support::setup_healthy_session(&fake, &project);

    send_prompt(&fake, &project, "alpha", "", false).or_panic("send_prompt_empty_skips_paste");

    let ops = fake.ops();
    assert_eq!(ops.len(), 1, "expected only Enter, got: {ops:?}");
    assert_eq!(
        ops[0],
        FakeOp::SendKeys {
            pane_id: "%0".to_string(),
            keys: vec!["Enter".to_string()],
        }
    );
}

#[test]
fn send_prompt_with_template_renders_context() {
    let fake = crate::test_support::FakeTmux::new();
    let mut project = crate::test_support::test_project();
    project.agents[0].prompt_template =
        Some("Agent {{agent_name}} in {{project_name}}: {{user_prompt}}".to_string());
    crate::test_support::setup_healthy_session(&fake, &project);

    send_prompt(&fake, &project, "alpha", "hello world", false)
        .or_panic("send_prompt_with_template_renders_context");

    let ops = fake.ops();
    assert!(
        ops.iter().any(|op| matches!(
            op,
            FakeOp::PasteText { text, .. }
                if text == "Agent Alpha in Test: hello world"
        )),
        "expected rendered template (label as agent_name) in paste, got: {ops:?}"
    );
}

#[test]
fn send_prompt_without_template_sends_raw() {
    let fake = crate::test_support::FakeTmux::new();
    let project = crate::test_support::test_project();
    crate::test_support::setup_healthy_session(&fake, &project);

    send_prompt(&fake, &project, "alpha", "raw prompt", false)
        .or_panic("send_prompt_without_template_sends_raw");

    let ops = fake.ops();
    assert!(
        ops.iter().any(|op| matches!(
            op,
            FakeOp::PasteText { text, .. }
                if text == "raw prompt"
        )),
        "expected raw prompt in paste, got: {ops:?}"
    );
}

#[test]
fn send_prompt_no_template_bypasses_rendering() {
    let fake = crate::test_support::FakeTmux::new();
    let mut project = crate::test_support::test_project();
    project.agents[0].prompt_template =
        Some("Agent {{agent_name}} in {{project_name}}: {{user_prompt}}".to_string());
    crate::test_support::setup_healthy_session(&fake, &project);

    send_prompt(&fake, &project, "alpha", "raw prompt", true)
        .or_panic("send_prompt_no_template_bypasses_rendering");

    let ops = fake.ops();
    assert!(
        ops.iter().any(|op| matches!(
            op,
            FakeOp::PasteText { text, .. }
                if text == "raw prompt"
        )),
        "expected raw prompt (no template rendering) in paste, got: {ops:?}"
    );
}

#[test]
fn send_prompt_delivers_exactly_once() {
    let fake = crate::test_support::FakeTmux::new();
    let project = crate::test_support::test_project();
    crate::test_support::setup_healthy_session(&fake, &project);

    send_prompt(&fake, &project, "alpha", "one message", false)
        .or_panic("send_prompt_delivers_exactly_once");

    let ops = fake.ops();
    let paste_count = ops
        .iter()
        .filter(|op| matches!(op, FakeOp::PasteText { .. }))
        .count();
    let enter_count = ops
        .iter()
        .filter(|op| matches!(op, FakeOp::SendKeys { keys, .. } if keys == &["Enter"]))
        .count();
    assert_eq!(paste_count, 1, "expected exactly one paste, got: {ops:?}");
    assert_eq!(enter_count, 1, "expected exactly one Enter, got: {ops:?}");
}

#[test]
fn send_prompt_returns_rendered_prompt_not_raw() {
    let fake = crate::test_support::FakeTmux::new();
    let mut project = crate::test_support::test_project();
    project.agents[0].prompt_template =
        Some("Agent {{agent_name}} in {{project_name}}: {{user_prompt}}".to_string());
    crate::test_support::setup_healthy_session(&fake, &project);

    let sent = send_prompt(&fake, &project, "alpha", "hello world", false)
        .or_panic("send_prompt_returns_rendered_prompt_not_raw");
    assert_eq!(
        sent.rendered, "Agent Alpha in Test: hello world",
        "send_prompt must return the rendered prompt, not the raw input"
    );
    assert_eq!(sent.pane_id, "%0");
}

#[test]
fn send_prompt_returns_raw_slash_command_unchanged() {
    let fake = crate::test_support::FakeTmux::new();
    let project = crate::test_support::test_project();
    crate::test_support::setup_healthy_session(&fake, &project);

    let sent = send_prompt(&fake, &project, "alpha", "/help", false)
        .or_panic("send_prompt_returns_raw_slash_command_unchanged");
    assert_eq!(sent.rendered, "/help");
    assert_eq!(sent.pane_id, "%0");
}

#[test]
fn send_prompt_clear_bypasses_template_like_cli_flag() {
    // Mirrors `send --clear`: literal "/clear" with no_template=true.
    let fake = crate::test_support::FakeTmux::new();
    let mut project = crate::test_support::test_project();
    project.agents[0].prompt_template = Some("Agent {{agent_name}}: {{user_prompt}}".to_string());
    crate::test_support::setup_healthy_session(&fake, &project);

    let sent = send_prompt(&fake, &project, "alpha", "/clear", true)
        .or_panic("send_prompt_clear_bypasses_template_like_cli_flag");
    assert_eq!(sent.rendered, "/clear");
    let ops = fake.ops();
    assert!(
        ops.iter().any(|op| matches!(
            op,
            FakeOp::PasteText { text, .. } if text == "/clear"
        )),
        "expected literal /clear paste, got: {ops:?}"
    );
}

#[test]
fn send_prompt_returns_rendered_slash_command() {
    let fake = crate::test_support::FakeTmux::new();
    let mut project = crate::test_support::test_project();
    project.agents[0].prompt_template = Some("/cmd {{user_prompt}}".to_string());
    crate::test_support::setup_healthy_session(&fake, &project);

    let sent = send_prompt(&fake, &project, "alpha", "args here", false)
        .or_panic("send_prompt_returns_rendered_slash_command");
    assert_eq!(sent.rendered, "/cmd args here");
    assert_eq!(sent.pane_id, "%0");
}

#[test]
fn send_prompt_for_wait_captures_before_delivery() {
    let fake = crate::test_support::FakeTmux::new();
    let project = crate::test_support::test_project();
    crate::test_support::setup_healthy_session(&fake, &project);
    fake.set_pane_content("%0", "idle before submit");

    let seed = send_prompt_for_wait(
        &fake,
        &project,
        "alpha",
        "hello world",
        false,
        DEFAULT_WAIT_CAPTURE_LINES,
    )
    .or_panic("send_prompt_for_wait_captures_before_delivery");

    assert_eq!(seed.delivered.pane_id, "%0");
    assert_eq!(seed.delivered.rendered, "hello world");
    assert_eq!(seed.pre_submit, "idle before submit\n");
    assert_eq!(seed.capture_lines, DEFAULT_WAIT_CAPTURE_LINES);
    assert!(
        fake.ops()
            .iter()
            .any(|op| matches!(op, FakeOp::PasteText { text, .. } if text == "hello world")),
        "observed delivery must still submit the prompt"
    );
}

#[test]
fn send_prompt_for_wait_honors_capture_line_limit() {
    let fake = crate::test_support::FakeTmux::new();
    let project = crate::test_support::test_project();
    crate::test_support::setup_healthy_session(&fake, &project);
    fake.set_pane_content("%0", "line1\nline2\nline3\nline4\n");

    let seed = send_prompt_for_wait(&fake, &project, "alpha", "hi", false, 2)
        .or_panic("send_prompt_for_wait_honors_capture_line_limit");

    assert_eq!(seed.pre_submit, "line3\nline4\n");
    assert_eq!(seed.capture_lines, 2);
}

#[test]
fn send_prompt_submit_override_forces_single_enter_for_codex() {
    let fake = crate::test_support::FakeTmux::new();
    let mut project = crate::test_support::test_project();
    project.agents[0].command = Some("codex".to_string());
    project.agents[0].submit = Some(crate::config::SubmitPolicy::Single);
    crate::test_support::setup_healthy_session(&fake, &project);

    send_prompt(&fake, &project, "alpha", "review this", false)
        .or_panic("send_prompt_submit_override_forces_single_enter_for_codex");

    let ops = fake.ops();
    assert_eq!(ops.len(), 2, "expected paste + 1 Enter, got: {ops:?}");
}

#[test]
fn send_prompt_text_delivery_override_forces_send_keys() {
    let fake = crate::test_support::FakeTmux::new();
    let mut project = crate::test_support::test_project();
    project.agents[0].text_delivery = Some(crate::config::TextDelivery::SendKeys);
    crate::test_support::setup_healthy_session(&fake, &project);

    send_prompt(&fake, &project, "alpha", "hello world", false)
        .or_panic("send_prompt_text_delivery_override_forces_send_keys");

    let ops = fake.ops();
    assert!(
        ops.iter()
            .any(|op| matches!(op, FakeOp::SendText { text, .. } if text == "hello world")),
        "text_delivery=send-keys must type literally, got: {ops:?}"
    );
    assert!(
        !ops.iter().any(|op| matches!(op, FakeOp::PasteText { .. })),
        "text_delivery=send-keys must not paste, got: {ops:?}"
    );
}

#[test]
fn send_for_wait_seeds_inferred_claude_busy_markers() {
    let fake = crate::test_support::FakeTmux::new();
    let mut project = crate::test_support::test_project();
    project.agents[0].command = Some("claude".to_string());
    crate::test_support::setup_healthy_session(&fake, &project);

    let seed = send_prompt_for_wait(
        &fake,
        &project,
        "alpha",
        "do the thing",
        true,
        DEFAULT_WAIT_CAPTURE_LINES,
    )
    .or_panic("send_for_wait_seeds_inferred_claude_busy_markers");

    assert_eq!(seed.busy_markers, vec!["esc to interrupt".to_string()]);
}

#[test]
fn send_for_wait_seeds_configured_markers_normalized() {
    let fake = crate::test_support::FakeTmux::new();
    let mut project = crate::test_support::test_project();
    project.agents[0].busy_markers = Some(vec!["  Custom MARKER ".to_string()]);
    crate::test_support::setup_healthy_session(&fake, &project);

    let seed = send_prompt_for_wait(
        &fake,
        &project,
        "alpha",
        "do the thing",
        true,
        DEFAULT_WAIT_CAPTURE_LINES,
    )
    .or_panic("send_for_wait_seeds_configured_markers_normalized");

    assert_eq!(seed.busy_markers, vec!["custom marker".to_string()]);
}

#[test]
fn send_for_wait_explicit_empty_markers_disable_inference() {
    let fake = crate::test_support::FakeTmux::new();
    let mut project = crate::test_support::test_project();
    project.agents[0].command = Some("claude".to_string());
    project.agents[0].busy_markers = Some(Vec::new());
    crate::test_support::setup_healthy_session(&fake, &project);

    let seed = send_prompt_for_wait(
        &fake,
        &project,
        "alpha",
        "do the thing",
        true,
        DEFAULT_WAIT_CAPTURE_LINES,
    )
    .or_panic("send_for_wait_explicit_empty_markers_disable_inference");

    assert!(seed.busy_markers.is_empty());
}

#[test]
fn send_for_wait_drops_markers_contained_in_the_prompt() {
    let fake = crate::test_support::FakeTmux::new();
    let mut project = crate::test_support::test_project();
    project.agents[0].command = Some("claude".to_string());
    crate::test_support::setup_healthy_session(&fake, &project);

    // The prompt echo would sit near the pane bottom and read as busy
    // forever: a marker phrase inside the prompt disables that marker.
    let seed = send_prompt_for_wait(
        &fake,
        &project,
        "alpha",
        "explain what Esc to Interrupt does in the TUI",
        true,
        DEFAULT_WAIT_CAPTURE_LINES,
    )
    .or_panic("send_for_wait_drops_markers_contained_in_the_prompt");

    assert!(seed.busy_markers.is_empty());
}
