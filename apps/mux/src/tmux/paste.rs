//! Generic paste-then-submit helpers built on the [`TmuxAdapter`] primitives.
//!
//! These helpers know nothing about agents or submit behavior; they handle
//! the readiness-check + paste + Enter sequence that any caller pasting
//! text into a TUI pane needs.

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
    if !text.is_empty() {
        let baseline = tmux.capture_pane(pane_id, 50).ok();
        tmux.paste_text(pane_id, text)?;
        if let Some(baseline) = baseline {
            wait_for_paste_receipt(tmux, pane_id, &baseline);
        }
    }
    tmux.send_keys(pane_id, &["Enter"])?;
    Ok(())
}

/// Poll `capture_pane` until content differs from `baseline`, confirming
/// the TUI received and rendered the pasted content. Best-effort: returns
/// silently after [`PASTE_RECEIPT_TIMEOUT`].
fn wait_for_paste_receipt(tmux: &dyn TmuxAdapter, pane_id: &str, baseline: &str) {
    let deadline = Instant::now() + PASTE_RECEIPT_TIMEOUT;
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

        paste_then_submit_text(&fake, "%0", "hello").or_panic();

        let ops = fake.ops();
        let paste_idx = ops
            .iter()
            .position(|op| matches!(op, FakeOp::PasteText { text, .. } if text == "hello"))
            .or_panic();
        let enter_idx = ops
            .iter()
            .position(|op| matches!(op, FakeOp::SendKeys { keys, .. } if keys == &vec!["Enter".to_string()]))
            .or_panic();
        assert!(
            paste_idx < enter_idx,
            "paste must precede enter (paste={paste_idx}, enter={enter_idx})"
        );
    }

    #[test]
    fn paste_then_submit_proceeds_when_capture_pane_fails() {
        let fake = FakeTmux::new();

        paste_then_submit_text(&fake, "%0", "hello").or_panic();

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

        paste_then_submit_text(&fake, "%0", "").or_panic();

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
