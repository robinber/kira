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
/// How long an unrelated frame change (spinner, clock, badge) must persist
/// without the delivered text appearing before the receipt wait gives up on
/// the strong fragment signal and releases on the weak delta signal.
const RECEIPT_DELTA_GRACE: Duration = Duration::from_millis(250);

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
            let outcome =
                wait_for_text_receipt(tmux, pane_id, &baseline, text, PASTE_RECEIPT_TIMEOUT);
            tracing::debug!(pane_id, ?outcome, "text delivery receipt");
        }
    }
    tmux.send_keys(pane_id, &["Enter"])?;
    Ok(())
}

/// How a [`wait_for_text_receipt`] poll concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReceiptOutcome {
    /// A fragment of the delivered text rendered in the pane.
    Fragment,
    /// Only frame changes unrelated to the text were observed for the whole
    /// grace window.
    Delta,
    /// Nothing observable changed before the timeout (e.g. a pane that
    /// never echoes input).
    Timeout,
}

/// Poll `capture_pane` until the delivered text visibly rendered, then let
/// the frame stabilize before the caller submits Enter.
///
/// Two signals, strongest first: a [`prompt_fragments`] match against the
/// whitespace-normalized capture proves the text arrived; a bare
/// frame-vs-baseline delta (spinners, clocks, unrelated repaints) only
/// releases after it has persisted for [`RECEIPT_DELTA_GRACE`] without the
/// fragment showing up. Best-effort: returns silently once `timeout`
/// elapses (e.g. panes that never echo input).
fn wait_for_text_receipt(
    tmux: &dyn TmuxAdapter,
    pane_id: &str,
    baseline: &str,
    text: &str,
    timeout: Duration,
) -> ReceiptOutcome {
    let fragments = prompt_fragments(text);
    let baseline_normalized = normalize_search_text(baseline);
    let deadline = Instant::now() + timeout;
    let mut delta_since: Option<Instant> = None;
    while Instant::now() < deadline {
        std::thread::sleep(PASTE_RECEIPT_POLL_INTERVAL);
        let Ok(current) = tmux.capture_pane(pane_id, 50) else {
            continue;
        };
        let now = Instant::now();
        let current_normalized = normalize_search_text(&current);
        if fragments.iter().any(|fragment| {
            !baseline_normalized.contains(fragment.as_str())
                && current_normalized.contains(fragment.as_str())
        }) {
            std::thread::sleep(PASTE_RECEIPT_STABILIZATION);
            return ReceiptOutcome::Fragment;
        }
        if current != baseline {
            let since = *delta_since.get_or_insert(now);
            if now >= since + RECEIPT_DELTA_GRACE {
                return ReceiptOutcome::Delta;
            }
        }
    }
    ReceiptOutcome::Timeout
}

/// Collapse all whitespace runs to single spaces so containment checks
/// survive TUI reflow and tmux line wrapping.
pub(crate) fn normalize_search_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Short head/tail probes of `rendered_prompt` (whitespace-normalized) used
/// to recognize the prompt inside a pane capture.
pub(crate) fn prompt_fragments(rendered_prompt: &str) -> Vec<String> {
    const FRAGMENT_CHARS: usize = 64;

    let normalized = normalize_search_text(rendered_prompt);
    let chars: Vec<char> = normalized.chars().collect();
    if chars.is_empty() {
        return Vec::new();
    }
    if chars.len() <= FRAGMENT_CHARS {
        return vec![normalized];
    }

    vec![
        chars.iter().take(FRAGMENT_CHARS).collect(),
        chars
            .iter()
            .skip(chars.len().saturating_sub(FRAGMENT_CHARS))
            .collect(),
    ]
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
    fn receipt_releases_on_fragment_render() {
        let fake = FakeTmux::new();
        fake.add_session("s");
        fake.add_window("s", "w");
        fake.add_pane("s", "w", "%0", false);
        fake.set_pane_content("%0", "idle composer");
        let baseline = fake.capture_pane("%0", 50).or_panic("baseline");
        fake.set_pane_content("%0", "idle composer\n> please review the diff");

        let outcome = wait_for_text_receipt(
            &fake,
            "%0",
            &baseline,
            "please review the diff",
            Duration::from_secs(1),
        );

        assert_eq!(outcome, ReceiptOutcome::Fragment);
    }

    #[test]
    fn receipt_ignores_spinner_delta_until_grace_elapses() {
        let fake = FakeTmux::new();
        fake.add_session("s");
        fake.add_window("s", "w");
        fake.add_pane("s", "w", "%0", false);
        fake.set_pane_content("%0", "spinner |");
        let baseline = fake.capture_pane("%0", 50).or_panic("baseline");
        fake.set_pane_content("%0", "spinner /");

        let start = Instant::now();
        let outcome = wait_for_text_receipt(
            &fake,
            "%0",
            &baseline,
            "please review the diff",
            Duration::from_secs(2),
        );

        assert_eq!(outcome, ReceiptOutcome::Delta);
        assert!(
            start.elapsed() >= RECEIPT_DELTA_GRACE,
            "a bare frame delta must not release before the grace window"
        );
    }

    #[test]
    fn receipt_times_out_when_nothing_renders() {
        let fake = FakeTmux::new();
        fake.add_session("s");
        fake.add_window("s", "w");
        fake.add_pane("s", "w", "%0", false);
        fake.set_pane_content("%0", "hidden input");
        let baseline = fake.capture_pane("%0", 50).or_panic("baseline");

        let outcome = wait_for_text_receipt(
            &fake,
            "%0",
            &baseline,
            "secret text never echoed",
            Duration::from_millis(200),
        );

        assert_eq!(outcome, ReceiptOutcome::Timeout);
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
