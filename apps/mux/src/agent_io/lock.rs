//! Per-window mutual exclusion for deep captures.
//!
//! Two concurrent deep captures of panes in the same window would race on
//! the saved geometry: the second snapshots the first one's *temporary*
//! zoomed/enlarged state and "restores" it permanently. A non-blocking OS
//! file lock excludes them: the sidecar lock file lives next to the tmux
//! server socket (same 0700 per-user directory), namespaced by window id,
//! so it names the window uniquely across servers. The lock releases when
//! the file handle drops — including on process death — so stale-lock
//! recovery needs no PID probing, lease, or daemon. The sidecar file itself
//! is never deleted: unlinking a locked path would let a new opener lock a
//! fresh inode while the old holder still runs.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::PathBuf;

use anyhow::{Context, Result};

/// Held lock: dropping it (or dying with it) releases the window.
pub(super) struct WindowLock {
    _file: File,
}

/// Try to become the only deep capture for `window_id` on the server at
/// `socket_path`. Returns `None` when another capture currently owns the
/// window — callers fall back to a plain capture instead of waiting.
pub(super) fn try_lock_window(socket_path: &str, window_id: &str) -> Result<Option<WindowLock>> {
    let path = lock_path(socket_path, window_id);
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("failed to open deep-capture lock {}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(WindowLock { _file: file })),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(error)) => Err(anyhow::Error::new(error).context(format!(
            "failed to lock deep-capture lock {}",
            path.display()
        ))),
    }
}

/// `<socket_path>-kira-mux-deep-<window_id>.lock` — window ids (`@N`) are
/// plain filename characters, and the socket's parent directory is the
/// user-owned tmux directory.
fn lock_path(socket_path: &str, window_id: &str) -> PathBuf {
    PathBuf::from(format!("{socket_path}-kira-mux-deep-{window_id}.lock"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestOptionExt, ok};

    #[test]
    fn lock_is_exclusive_per_window_and_released_on_drop() {
        let dir = ok(tempfile::tempdir(), "tempdir for lock test");
        let socket = dir.path().join("socket").display().to_string();

        let first = ok(try_lock_window(&socket, "@1"), "first lock attempt")
            .or_panic("first lock should be acquired");
        assert!(
            ok(try_lock_window(&socket, "@1"), "contended lock attempt").is_none(),
            "the same window must be busy while the first lock is held"
        );
        // A different window on the same server is independent.
        ok(try_lock_window(&socket, "@2"), "other-window lock attempt")
            .or_panic("another window should lock independently");

        drop(first);
        ok(try_lock_window(&socket, "@1"), "post-drop lock attempt")
            .or_panic("dropping the lock must release the window");
    }
}
