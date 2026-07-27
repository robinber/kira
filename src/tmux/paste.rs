//! Generic deliver-then-submit helpers built on the [`TmuxAdapter`] primitives.
//!
//! These helpers know nothing about agents or submit behavior; they handle
//! the readiness-check + delivery + Enter sequence that any caller putting
//! text into a TUI pane needs, whether the text arrives via paste buffer or
//! literal send-keys.

use std::time::{Duration, Instant};

use anyhow::Result;

use super::adapter::TmuxAdapter;

const PASTE_RECEIPT_TIMEOUT: Duration = Duration::from_secs(3);
const PASTE_RECEIPT_POLL_INTERVAL: Duration = Duration::from_millis(50);
const PASTE_RECEIPT_STABILIZATION: Duration = Duration::from_millis(50);

/// Paste `text` into `pane_id` and submit a single `Enter`.
///
/// Captures a baseline first so the readiness wait can detect when the
/// pasted text has rendered. On capture failure (which is best-effort), the
/// paste proceeds without the readiness wait. Errors from the paste itself
/// abort the sequence and propagate to the caller.
pub(crate) fn paste_then_submit_text(
    tmux: &dyn TmuxAdapter,
    pane_id: &str,
    text: &str,
) -> Result<()> {
    deliver_then_submit(tmux, pane_id, text, |tmux| tmux.paste_text(pane_id, text))
}

/// Type `text` into `pane_id` via literal send-keys and submit a single
/// `Enter`, with the same receipt wait as [`paste_then_submit_text`] so the
/// Enter cannot race a TUI that has not rendered the text yet.
pub(crate) fn send_then_submit_text(
    tmux: &dyn TmuxAdapter,
    pane_id: &str,
    text: &str,
) -> Result<()> {
    deliver_then_submit(tmux, pane_id, text, |tmux| tmux.send_text(pane_id, text))
}

fn deliver_then_submit(
    tmux: &dyn TmuxAdapter,
    pane_id: &str,
    text: &str,
    deliver: impl FnOnce(&dyn TmuxAdapter) -> Result<()>,
) -> Result<()> {
    if !text.is_empty() {
        let baseline = tmux.capture_pane(pane_id, 50).ok();
        deliver(tmux)?;
        if let Some(baseline) = baseline {
            wait_for_render_change(tmux, pane_id, &baseline, PASTE_RECEIPT_TIMEOUT);
        }
    }
    tmux.send_keys(pane_id, &["Enter"])?;
    Ok(())
}

/// Poll `capture_pane` until content differs from `baseline`, confirming
/// the TUI received and rendered newly delivered input. Best-effort:
/// returns silently once `timeout` elapses.
pub(crate) fn wait_for_render_change(
    tmux: &dyn TmuxAdapter,
    pane_id: &str,
    baseline: &str,
    timeout: Duration,
) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        std::thread::sleep(PASTE_RECEIPT_POLL_INTERVAL);
        if let Ok(current) = tmux.capture_pane(pane_id, 50)
            && current != baseline
        {
            std::thread::sleep(PASTE_RECEIPT_STABILIZATION);
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{FakeOp, FakeTmux, TestOptionExt, TestResultExt};

    #[test]
    fn paste_then_submit_records_paste_then_enter() {
        let fake = FakeTmux::new();
        fake.add_session("s");
        fake.add_window("s", "w");
        fake.add_pane("s", "w", "%0", false);

        paste_then_submit_text(&fake, "%0", "hello")
            .or_panic("paste_then_submit_records_paste_then_enter");

        let ops = fake.ops();
        let paste_idx = ops
            .iter()
            .position(|op| matches!(op, FakeOp::PasteText { text, .. } if text == "hello"))
            .or_panic("paste_then_submit_records_paste_then_enter");
        let enter_idx = ops
            .iter()
            .position(|op| matches!(op, FakeOp::SendKeys { keys, .. } if keys == &vec!["Enter".to_string()]))
            .or_panic("paste_then_submit_records_paste_then_enter");
        assert!(
            paste_idx < enter_idx,
            "paste must precede enter (paste={paste_idx}, enter={enter_idx})"
        );
    }

    #[test]
    fn send_then_submit_records_send_text_then_enter() {
        let fake = FakeTmux::new();
        fake.add_session("s");
        fake.add_window("s", "w");
        fake.add_pane("s", "w", "%0", false);

        send_then_submit_text(&fake, "%0", "hello")
            .or_panic("send_then_submit_records_send_text_then_enter");

        let ops = fake.ops();
        let send_idx = ops
            .iter()
            .position(|op| matches!(op, FakeOp::SendText { text, .. } if text == "hello"))
            .or_panic("send_then_submit_records_send_text_then_enter");
        let enter_idx = ops
            .iter()
            .position(|op| matches!(op, FakeOp::SendKeys { keys, .. } if keys == &vec!["Enter".to_string()]))
            .or_panic("send_then_submit_records_send_text_then_enter");
        assert!(
            send_idx < enter_idx,
            "send_text must precede enter (send={send_idx}, enter={enter_idx})"
        );
    }

    #[test]
    fn paste_then_submit_proceeds_when_capture_pane_fails() {
        let fake = FakeTmux::new();

        paste_then_submit_text(&fake, "%0", "hello")
            .or_panic("paste_then_submit_proceeds_when_capture_pane_fails");

        let ops = fake.ops();
        assert!(
            ops.iter()
                .any(|op| matches!(op, FakeOp::PasteText { text, .. } if text == "hello")),
            "paste must still happen when capture_pane fails"
        );
        assert!(
            ops.iter().any(|op| matches!(op, FakeOp::SendKeys { keys, .. } if keys == &vec!["Enter".to_string()])),
            "enter must still be sent when capture_pane fails"
        );
    }

    #[test]
    fn paste_then_submit_with_empty_text_skips_paste_but_sends_enter() {
        let fake = FakeTmux::new();

        paste_then_submit_text(&fake, "%0", "")
            .or_panic("paste_then_submit_with_empty_text_skips_paste_but_sends_enter");

        let ops = fake.ops();
        assert!(
            !ops.iter().any(|op| matches!(op, FakeOp::PasteText { .. })),
            "no paste expected for empty text"
        );
        assert!(
            ops.iter()
                .any(|op| matches!(op, FakeOp::SendKeys { keys, .. } if keys == &vec!["Enter".to_string()])),
            "enter should still be sent"
        );
    }
}
