//! Dual-impl contract tests: the same operation sequences run against
//! [`FakeTmux`] and, when a tmux binary is available, a real
//! [`TmuxClient`] on an isolated throwaway server — pinning the fake's
//! semantics to real tmux instead of to comments.
//!
//! Every test asserts the fake unconditionally and repeats the identical
//! sequence against the real server. The real half is mandatory by
//! default (see the canary); environments that genuinely cannot run tmux
//! opt out explicitly with `KIRA_MUX_SKIP_TMUX_CONFORMANCE=1` rather
//! than degrading silently.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use super::FakeTmux;
use crate::test_support::TestResultExt;
use crate::tmux::{TmuxAdapter, TmuxClient, TmuxError};

const TMUX_BIN: &str = "tmux";
const SKIP_ENV: &str = "KIRA_MUX_SKIP_TMUX_CONFORMANCE";

static SOCKET_SEQ: AtomicUsize = AtomicUsize::new(0);

fn conformance_skipped() -> bool {
    std::env::var_os(SKIP_ENV).is_some_and(|value| value == "1")
}

/// A real client on an isolated socket, its server killed on drop.
struct RealServer {
    client: TmuxClient,
    socket: String,
}

impl Drop for RealServer {
    fn drop(&mut self) {
        let _ = std::process::Command::new(TMUX_BIN)
            .args(["-L", &self.socket, "kill-server"])
            .output();
    }
}

/// `None` when the explicit skip is set or no usable tmux binary is on
/// PATH.
fn real_server() -> Option<RealServer> {
    if conformance_skipped() {
        return None;
    }
    let socket = format!(
        "kira-conformance-{}-{}",
        std::process::id(),
        SOCKET_SEQ.fetch_add(1, Ordering::Relaxed)
    );
    let client = TmuxClient::with_socket(TMUX_BIN, &socket);
    match client.session_exists("probe") {
        // A fresh socket answers "no server": the binary works.
        Ok(_) => Some(RealServer { client, socket }),
        Err(error)
            if matches!(
                error.downcast_ref::<TmuxError>(),
                Some(TmuxError::NoServer(_))
            ) =>
        {
            Some(RealServer { client, socket })
        }
        Err(_) => None,
    }
}

fn tmp() -> String {
    std::env::temp_dir().display().to_string()
}

fn error_kind(error: &anyhow::Error) -> &'static str {
    match error.downcast_ref::<TmuxError>() {
        Some(TmuxError::MissingTarget(_)) => "missing_target",
        Some(TmuxError::MissingSession(_)) => "missing_session",
        Some(TmuxError::NoServer(_)) => "no_server",
        Some(TmuxError::CommandFailure(_)) => "command_failure",
        None => "untyped",
    }
}

/// Seed + split-reported pane ids: unique, monotonic, and identical
/// between impls on a fresh server (real tmux starts a fresh server's pane
/// ids at `%0`, exactly like a fresh [`FakeTmux`]).
fn observe_id_sequence(tmux: &dyn TmuxAdapter) -> Vec<String> {
    tmux.create_detached_session("conf", &tmp(), "w", 3)
        .or_panic("conformance: create session");
    let mut ids: Vec<String> = tmux
        .list_panes("conf:w")
        .or_panic("conformance: seed listing")
        .into_iter()
        .map(|pane| pane.pane_id)
        .collect();
    ids.push(
        tmux.split_window("conf:w", &tmp())
            .or_panic("conformance: first split"),
    );
    ids.push(
        tmux.split_window("conf:w", &tmp())
            .or_panic("conformance: second split"),
    );
    assert_eq!(
        tmux.list_panes("conf:w")
            .or_panic("conformance: final listing")
            .len(),
        3
    );
    ids
}

/// Canary: the dual-impl half of this suite is mandatory by default — the
/// unit CI job installs tmux explicitly for it. Without this test, a
/// missing binary would silently degrade every conformance test to
/// fake-only assertions while staying green. Environments that cannot run
/// tmux set `KIRA_MUX_SKIP_TMUX_CONFORMANCE=1` to opt out loudly.
#[test]
fn real_tmux_is_available_for_conformance() {
    if conformance_skipped() {
        return;
    }
    assert!(
        real_server().is_some(),
        "no usable tmux binary: dual-impl conformance is not running \
         (set {SKIP_ENV}=1 to opt out explicitly)"
    );
}

#[test]
fn pane_id_sequences_match_real_tmux() {
    let fake = FakeTmux::new();
    let fake_ids = observe_id_sequence(&fake);
    assert_eq!(fake_ids, ["%0", "%1", "%2"]);

    if let Some(real) = real_server() {
        assert_eq!(observe_id_sequence(&real.client), fake_ids);
    }
}

/// Killing a session must not recycle pane ids while the server lives —
/// tmux ids are server-lifetime unique. A keeper session holds the real
/// server open across the kill.
fn observe_ids_across_recreation(tmux: &dyn TmuxAdapter) -> Vec<String> {
    tmux.create_detached_session("keep", &tmp(), "w", 1)
        .or_panic("conformance: keeper");
    tmux.create_detached_session("conf", &tmp(), "w", 1)
        .or_panic("conformance: first session");
    let first = tmux
        .split_window("conf:w", &tmp())
        .or_panic("conformance: split before kill");
    tmux.kill_session("conf").or_panic("conformance: kill");
    tmux.create_detached_session("conf2", &tmp(), "w", 1)
        .or_panic("conformance: recreated session");
    let reseed = tmux
        .list_panes("conf2:w")
        .or_panic("conformance: reseed listing")
        .into_iter()
        .map(|pane| pane.pane_id)
        .collect::<Vec<_>>();
    let mut ids = vec![first];
    ids.extend(reseed);
    ids
}

#[test]
fn pane_ids_are_never_reused_while_the_server_lives() {
    let fake = FakeTmux::new();
    let fake_ids = observe_ids_across_recreation(&fake);
    assert_eq!(fake_ids, ["%2", "%3"]);

    if let Some(real) = real_server() {
        assert_eq!(observe_ids_across_recreation(&real.client), fake_ids);
    }
}

#[test]
fn capture_shape_has_no_trailing_padding_and_one_newline() {
    let fake = FakeTmux::new();
    fake.add_session("conf");
    fake.add_window("conf", "w");
    fake.add_pane("conf", "w", "%0", false);
    fake.set_pane_content("%0", "hello\n\n\n");
    let captured = fake
        .capture_pane("%0", 50)
        .or_panic("conformance: fake capture");
    assert_eq!(captured, "hello\n");

    if let Some(real) = real_server() {
        real.client
            .create_detached_session("conf", &tmp(), "w", 1)
            .or_panic("conformance: real session");
        let pane = real
            .client
            .list_panes("conf:w")
            .or_panic("conformance: real listing")
            .remove(0)
            .pane_id;
        real.client
            .respawn_pane(
                &pane,
                &tmp(),
                &[],
                &[
                    "sh".to_string(),
                    "-c".to_string(),
                    "printf 'hello\\n\\n\\n'; sleep 30".to_string(),
                ],
            )
            .or_panic("conformance: respawn");
        let deadline = Instant::now() + Duration::from_secs(5);
        let captured = loop {
            let captured = real
                .client
                .capture_pane(&pane, 50)
                .or_panic("conformance: real capture");
            if captured.contains("hello") || Instant::now() >= deadline {
                break captured;
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        assert_eq!(
            captured, "hello\n",
            "real capture must strip trailing blank padding down to one newline"
        );
    }
}

#[test]
fn vanished_target_classifications_match_real_tmux() {
    // Absolute expectations, not just fake==real: a shared
    // misclassification of both impls must fail too.
    let expected = [
        "missing_target",
        "missing_target",
        "missing_target",
        "missing_session",
    ];

    let fake = FakeTmux::new();
    fake.add_session("conf");
    fake.add_window("conf", "w");
    assert_eq!(observe_error_kinds(&fake), expected);

    if let Some(real) = real_server() {
        real.client
            .create_detached_session("conf", &tmp(), "w", 1)
            .or_panic("conformance: real session");
        assert_eq!(observe_error_kinds(&real.client), expected);
    }
}

fn observe_error_kinds(tmux: &dyn TmuxAdapter) -> Vec<&'static str> {
    let paste = tmux
        .paste_text("%99", "x")
        .err_or_panic("conformance: paste to missing pane must fail");
    let capture = tmux
        .capture_pane("%99", 50)
        .err_or_panic("conformance: capture of missing pane must fail");
    let window = tmux
        .list_panes("conf:absent")
        .err_or_panic("conformance: listing a missing window must fail");
    let session = tmux
        .kill_session("absent")
        .err_or_panic("conformance: killing a missing session must fail");
    vec![
        error_kind(&paste),
        error_kind(&capture),
        error_kind(&window),
        error_kind(&session),
    ]
}
