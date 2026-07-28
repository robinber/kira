//! In-memory [`TmuxAdapter`] used by unit tests.
//!
//! Layout: this module owns the fake state and test knobs; [`adapter`]
//! implements [`TmuxAdapter`]; self-tests live in [`tests`].

mod adapter;
#[cfg(test)]
mod conformance;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use anyhow::{Result, bail};

use super::{ok, some};
use crate::tmux::{PaneInfo, TmuxError};

pub(super) const FAKE_WINDOW_WIDTH: usize = 200;
pub(super) const FAKE_WINDOW_HEIGHT: usize = 24;

pub(crate) struct FakeTmux {
    /// Unique fake server socket path, so per-window deep-capture locks are
    /// isolated between `FakeTmux` instances (and between parallel tests).
    /// The tempdir keeps lock sidecar files out of the repo and is removed
    /// on drop.
    socket_dir: tempfile::TempDir,
    sessions: Mutex<BTreeMap<String, FakeSession>>,
    ops: Mutex<Vec<FakeOp>>,
    workspace_snapshot_error: Mutex<Option<TmuxError>>,
    fail_paste: AtomicBool,
    fail_send_keys: AtomicBool,
    fail_respawn: AtomicBool,
    /// When set, paste/send delivery ops return typed `MissingTarget` so
    /// submit can exercise the mid-delivery vanished-pane race.
    fail_delivery_missing_target: AtomicBool,
    /// When set, paste/send delivery ops report that the tmux server stopped.
    fail_delivery_no_server: AtomicBool,
    /// When set, a successful `respawn_pane` marks the pane dead immediately
    /// so post-launch health checks observe an instant exit.
    respawn_exits_immediately: AtomicBool,
    /// One-shot: next `attach_session` / `switch_client` fails with a generic
    /// status while the session remains present (hard attach error, not
    /// `SessionAbsent`). Consumed on use.
    fail_attach: AtomicBool,
    /// Next attach/switch removes the session then fails with a status-only
    /// error, simulating an interactive race → lifecycle maps to
    /// `SessionAbsent`.
    vanish_before_attach: AtomicBool,
    /// Next `kill_session` removes the session (if present) and returns
    /// `MissingSession`, simulating a vanish between existence check and kill.
    vanish_before_kill: AtomicBool,
    /// One-shot: next `kill_session` fails with a generic `CommandFailure`
    /// while the session remains present (hard kill error, not a vanish).
    fail_kill: AtomicBool,
    no_server: AtomicBool,
    /// When set, `capture_pane` fails with a generic `CommandFailure` while
    /// the pane stays live — a transient capture failure, unlike the typed
    /// vanished-target errors.
    fail_capture: AtomicBool,
    /// Content the fake "agent" renders below every delivered text. `None`
    /// (the default) echoes only the text itself.
    delivery_response: Mutex<Option<String>>,
    /// Next pane index for [`FakeTmux::alloc_pane_id`] — server-lifetime
    /// monotonic like real tmux pane ids.
    next_pane_id: AtomicUsize,
    /// When set, `list_panes` returns panes in reversed order so tests can
    /// prove callers never depend on listing order for pane identity.
    reverse_pane_listing: AtomicBool,
    /// Countdown of `capture_pane` calls before the fake server stops
    /// (flipping `no_server`), simulating tmux server loss mid-wait.
    server_stops_after_captures: Mutex<Option<usize>>,
    /// Scripted pane relocation: after N `window_geometry` calls, move the
    /// pane to another window — simulates an operator `move-pane` between
    /// the deep-capture lock probe and the under-lock geometry read.
    relocate_after_geometry_reads: Mutex<Option<FakeRelocation>>,
}

struct FakeRelocation {
    pane_id: String,
    to_session: String,
    to_window: String,
    remaining_reads: usize,
}
struct FakeSession {
    options: BTreeMap<String, String>,
    windows: BTreeMap<String, FakeWindow>,
}

struct FakeWindow {
    options: BTreeMap<String, String>,
    panes: Vec<FakePane>,
    width: usize,
    height: usize,
    /// Zoom state: the zoomed pane and its pre-zoom height (restored on
    /// unzoom, mirroring tmux returning panes to their layout size).
    zoomed_pane: Option<FakeZoom>,
    /// Active pane id; defaults to the first pane. Zooming a pane makes it
    /// active (and unzooming does not switch back), mirroring tmux.
    active_pane: Option<String>,
    /// Window-local `window-size` value. `resize-window` forces `manual`,
    /// mirroring tmux; `None` inherits the global option.
    size_option: Option<String>,
}

impl FakeWindow {
    fn new() -> Self {
        Self {
            options: BTreeMap::new(),
            panes: Vec::new(),
            width: FAKE_WINDOW_WIDTH,
            height: FAKE_WINDOW_HEIGHT,
            zoomed_pane: None,
            active_pane: None,
            size_option: None,
        }
    }

    fn active_pane_id(&self) -> String {
        self.active_pane.clone().unwrap_or_else(|| {
            self.panes
                .first()
                .map(|pane| pane.pane_id.clone())
                .unwrap_or_default()
        })
    }
}

pub(super) struct FakeZoom {
    pane_id: String,
    prior_height: usize,
}

pub(super) struct FakePane {
    pane_id: String,
    options: BTreeMap<String, String>,
    dead: bool,
    dead_status: Option<i32>,
    /// Whether the fake pane program is on the alternate screen. Captures of
    /// such panes are capped at `height` lines, mirroring real tmux (no
    /// history) — the full `content` plays the TUI's internal transcript.
    alternate_on: bool,
    /// Visible pane height; tracks the window height through `resize_window`.
    height: usize,
    content: String,
    /// Scripted capture sequence: each `capture_pane` call pops the front
    /// into `content`, so tests can drive a changing pane deterministically.
    queued_contents: VecDeque<String>,
    /// When set, the pane is marked dead once this many `capture_pane`
    /// calls have been observed — simulates a crash mid-wait.
    dies_after_captures: Option<usize>,
    /// When set, the pane is removed from the session once this many
    /// `capture_pane` calls have been observed — simulates a killed window
    /// (`MissingTarget` on subsequent `list_panes` / capture).
    removed_after_captures: Option<usize>,
}

impl FakePane {
    pub(super) fn new(pane_id: &str, dead: bool) -> Self {
        Self {
            pane_id: pane_id.to_string(),
            options: BTreeMap::new(),
            dead,
            dead_status: dead.then_some(1),
            alternate_on: false,
            height: FAKE_WINDOW_HEIGHT,
            content: String::new(),
            queued_contents: VecDeque::new(),
            dies_after_captures: None,
            removed_after_captures: None,
        }
    }

    pub(super) fn info(&self) -> PaneInfo {
        PaneInfo {
            pane_id: self.pane_id.clone(),
            pane_dead: self.dead,
            pane_dead_status: self.dead_status,
            alternate_on: self.alternate_on,
            pane_height: self.height,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FakeOp {
    PasteText {
        pane_id: String,
        text: String,
    },
    SendKeys {
        pane_id: String,
        keys: Vec<String>,
    },
    SendText {
        pane_id: String,
        text: String,
    },
    RespawnPane {
        pane_id: String,
        cwd: String,
        env: Vec<(String, String)>,
        command: Vec<String>,
    },
    ResizeWindow {
        target: String,
        width: usize,
        height: usize,
    },
    ToggleZoom {
        target: String,
    },
    UnsetWindowSizeOption {
        target: String,
    },
    UnzoomWindow {
        target: String,
    },
    SelectPane {
        pane_id: String,
    },
}

impl FakeTmux {
    pub(crate) fn new() -> Self {
        Self {
            socket_dir: ok(tempfile::tempdir(), "fake tmux socket tempdir"),
            sessions: Mutex::new(BTreeMap::new()),
            ops: Mutex::new(Vec::new()),
            workspace_snapshot_error: Mutex::new(None),
            fail_paste: AtomicBool::new(false),
            fail_send_keys: AtomicBool::new(false),
            fail_respawn: AtomicBool::new(false),
            fail_delivery_missing_target: AtomicBool::new(false),
            fail_delivery_no_server: AtomicBool::new(false),
            respawn_exits_immediately: AtomicBool::new(false),
            fail_attach: AtomicBool::new(false),
            vanish_before_attach: AtomicBool::new(false),
            vanish_before_kill: AtomicBool::new(false),
            fail_kill: AtomicBool::new(false),
            no_server: AtomicBool::new(false),
            fail_capture: AtomicBool::new(false),
            delivery_response: Mutex::new(None),
            next_pane_id: AtomicUsize::new(0),
            reverse_pane_listing: AtomicBool::new(false),
            server_stops_after_captures: Mutex::new(None),
            relocate_after_geometry_reads: Mutex::new(None),
        }
    }

    /// Move `pane_id` to `to_session:to_window` once `reads` calls to
    /// `window_geometry` have been served.
    pub(crate) fn set_pane_relocated_after_geometry_reads(
        &self,
        pane_id: &str,
        to_session: &str,
        to_window: &str,
        reads: usize,
    ) {
        *ok(
            self.relocate_after_geometry_reads.lock(),
            "fake tmux relocation mutex poisoned",
        ) = Some(FakeRelocation {
            pane_id: pane_id.to_string(),
            to_session: to_session.to_string(),
            to_window: to_window.to_string(),
            remaining_reads: reads,
        });
    }

    fn note_geometry_served(&self) {
        let mut slot = ok(
            self.relocate_after_geometry_reads.lock(),
            "fake tmux relocation mutex poisoned",
        );
        let Some(relocation) = slot.as_mut() else {
            return;
        };
        relocation.remaining_reads = relocation.remaining_reads.saturating_sub(1);
        if relocation.remaining_reads > 0 {
            return;
        }
        let Some(relocation) = slot.take() else {
            return;
        };
        let mut sessions = ok(self.sessions.lock(), "fake tmux sessions mutex poisoned");
        let mut moved = None;
        'find: for session in sessions.values_mut() {
            for window in session.windows.values_mut() {
                if let Some(idx) = window
                    .panes
                    .iter()
                    .position(|pane| pane.pane_id == relocation.pane_id)
                {
                    moved = Some(window.panes.remove(idx));
                    break 'find;
                }
            }
        }
        if let (Some(pane), Some(window)) = (
            moved,
            sessions
                .get_mut(&relocation.to_session)
                .and_then(|session| session.windows.get_mut(&relocation.to_window)),
        ) {
            window.panes.push(pane);
        }
    }

    pub(crate) fn set_workspace_snapshot_error(&self, error: TmuxError) {
        *ok(
            self.workspace_snapshot_error.lock(),
            "fake tmux snapshot error mutex poisoned",
        ) = Some(error);
    }

    pub(crate) fn set_no_server(&self, no_server: bool) {
        self.no_server.store(no_server, Ordering::Relaxed);
    }

    /// Reverse `list_panes` ordering so callers that depend on listing
    /// order for pane identity fail loudly in tests.
    pub(crate) fn set_reverse_pane_listing(&self, reverse: bool) {
        self.reverse_pane_listing.store(reverse, Ordering::Relaxed);
    }

    /// Make `capture_pane` fail transiently (generic `CommandFailure`,
    /// pane stays live), for best-effort-capture tests.
    pub(crate) fn set_fail_capture(&self, fail: bool) {
        self.fail_capture.store(fail, Ordering::Relaxed);
    }

    /// Declare the single frame the fake "agent" renders below every
    /// delivered text (delivery is one atomic append). Tests whose
    /// assertions depend on visible agent output at delivery time state
    /// that frame here; multi-frame streams (production evidence over
    /// several polls) stay on [`FakeTmux::queue_pane_contents`].
    pub(crate) fn set_delivery_response(&self, response: &str) {
        *ok(
            self.delivery_response.lock(),
            "fake tmux delivery response mutex poisoned",
        ) = Some(response.to_string());
    }

    fn fail_capture_enabled(&self) -> bool {
        self.fail_capture.load(Ordering::Relaxed)
    }

    pub(super) fn reverse_pane_listing_enabled(&self) -> bool {
        self.reverse_pane_listing.load(Ordering::Relaxed)
    }

    pub(crate) fn set_fail_paste(&self, fail: bool) {
        self.fail_paste.store(fail, Ordering::Relaxed);
    }

    pub(crate) fn set_fail_send_keys(&self, fail: bool) {
        self.fail_send_keys.store(fail, Ordering::Relaxed);
    }

    pub(crate) fn set_fail_respawn(&self, fail: bool) {
        self.fail_respawn.store(fail, Ordering::Relaxed);
    }

    /// Simulate a pane that vanishes between resolve and paste/send-keys.
    pub(crate) fn set_fail_delivery_missing_target(&self, fail: bool) {
        self.fail_delivery_missing_target
            .store(fail, Ordering::Relaxed);
    }

    /// Simulate an isolated tmux server stopping with its last pane.
    pub(crate) fn set_fail_delivery_no_server(&self, fail: bool) {
        self.fail_delivery_no_server.store(fail, Ordering::Relaxed);
    }

    /// Stop the fake server after `captures` calls to `capture_pane`:
    /// subsequent `list_panes`/`capture_pane` calls report `NoServer`,
    /// simulating the tmux server dying mid-wait.
    pub(crate) fn set_server_stops_after_captures(&self, captures: usize) {
        *ok(
            self.server_stops_after_captures.lock(),
            "fake tmux server-stop mutex poisoned",
        ) = Some(captures);
    }

    fn note_capture_served(&self) {
        let mut remaining = ok(
            self.server_stops_after_captures.lock(),
            "fake tmux server-stop mutex poisoned",
        );
        if let Some(count) = remaining.take() {
            let count = count.saturating_sub(1);
            if count == 0 {
                self.no_server.store(true, Ordering::Relaxed);
            } else {
                *remaining = Some(count);
            }
        }
    }

    /// Guard every pane-addressed delivery op the way the real client's
    /// `run_on_target` does: a stopped server or unknown pane id is a typed
    /// error, never a silent success.
    fn ensure_deliverable(&self, target_pane: &str) -> Result<()> {
        if self.no_server.load(Ordering::Relaxed) {
            return Err(TmuxError::NoServer("no server running on fake socket".into()).into());
        }
        let sessions = ok(self.sessions.lock(), "fake tmux sessions mutex poisoned");
        if Self::find_pane(&sessions, target_pane).is_none() {
            return Err(TmuxError::MissingTarget(target_pane.to_string()).into());
        }
        Ok(())
    }

    fn delivery_failure(&self, target_pane: &str) -> Option<anyhow::Error> {
        if self.fail_delivery_no_server.load(Ordering::Relaxed) {
            Some(TmuxError::NoServer("no server running on fake socket".to_string()).into())
        } else if self.fail_delivery_missing_target.load(Ordering::Relaxed) {
            Some(TmuxError::MissingTarget(target_pane.to_string()).into())
        } else {
            None
        }
    }

    /// Shared attach/switch path. Matches real interactive clients: failures
    /// are status-only (no typed `TmuxError`). Lifecycle re-checks existence
    /// afterward to mint `SessionAbsent` when the session is gone.
    fn attach_or_switch(&self, session_name: &str) -> Result<()> {
        if self.no_server.load(Ordering::Relaxed) {
            bail!("tmux command failed with status 1");
        }
        let mut sessions = ok(self.sessions.lock(), "fake tmux sessions mutex poisoned");
        if self.vanish_before_attach.swap(false, Ordering::Relaxed) {
            sessions.remove(session_name);
            bail!("tmux command failed with status 1");
        }
        let present = sessions.contains_key(session_name);
        drop(sessions);
        if !present {
            bail!("tmux command failed with status 1");
        }
        if self.fail_attach.swap(false, Ordering::Relaxed) {
            bail!("tmux command failed with status 1");
        }
        Ok(())
    }

    pub(crate) fn set_respawn_exits_immediately(&self, exits: bool) {
        self.respawn_exits_immediately
            .store(exits, Ordering::Relaxed);
    }

    /// One-shot: next attach/switch fails while the session still exists.
    pub(crate) fn set_fail_attach(&self, fail: bool) {
        self.fail_attach.store(fail, Ordering::Relaxed);
    }

    /// Next attach/switch drops the session then fails (interactive race).
    pub(crate) fn set_vanish_before_attach(&self, vanish: bool) {
        self.vanish_before_attach.store(vanish, Ordering::Relaxed);
    }

    /// Next kill drops the session and returns `MissingSession` (kill race).
    pub(crate) fn set_vanish_before_kill(&self, vanish: bool) {
        self.vanish_before_kill.store(vanish, Ordering::Relaxed);
    }

    /// One-shot: next kill fails while the session still exists.
    pub(crate) fn set_fail_kill(&self, fail: bool) {
        self.fail_kill.store(fail, Ordering::Relaxed);
    }

    pub(crate) fn ops(&self) -> Vec<FakeOp> {
        ok(self.ops.lock(), "fake tmux ops mutex poisoned").clone()
    }

    pub(crate) fn add_session(&self, name: &str) {
        let mut sessions = ok(self.sessions.lock(), "fake tmux sessions mutex poisoned");
        sessions.insert(
            name.to_string(),
            FakeSession {
                options: BTreeMap::new(),
                windows: BTreeMap::new(),
            },
        );
    }

    pub(crate) fn add_window(&self, session: &str, window: &str) {
        let mut sessions = ok(self.sessions.lock(), "fake tmux sessions mutex poisoned");
        let session = some(
            sessions.get_mut(session),
            format!("missing fake session '{session}'"),
        );
        session
            .windows
            .insert(window.to_string(), FakeWindow::new());
    }

    pub(crate) fn add_pane(&self, session: &str, window: &str, pane_id: &str, dead: bool) {
        // Keep allocator ids ahead of explicitly chosen ones so splits can
        // never collide with a fixture pane.
        if let Some(index) = pane_id
            .strip_prefix('%')
            .and_then(|digits| digits.parse::<usize>().ok())
        {
            self.next_pane_id.fetch_max(index + 1, Ordering::Relaxed);
        }
        let mut sessions = ok(self.sessions.lock(), "fake tmux sessions mutex poisoned");
        // Loud collision detection: pane ids are server-lifetime unique on
        // real tmux, so a duplicate here is a fixture bug (or an
        // allocator/fixture race) that must abort instead of aliasing.
        assert!(
            Self::find_pane(&sessions, pane_id).is_none(),
            "duplicate fake pane id {pane_id}: ids must be unique across all sessions"
        );
        let session_name = session;
        let window_name = window;
        let session = some(
            sessions.get_mut(session),
            format!("missing fake session '{session}'"),
        );
        let window = some(
            session.windows.get_mut(window_name),
            format!("missing fake window '{window_name}' in session '{session_name}'"),
        );
        window.panes.push(FakePane::new(pane_id, dead));
    }

    /// Server-lifetime-unique pane id, mirroring real tmux: ids are
    /// monotonic per server and never reused, across every session and
    /// window this fake owns.
    pub(super) fn alloc_pane_id(&self) -> String {
        format!("%{}", self.next_pane_id.fetch_add(1, Ordering::Relaxed))
    }

    pub(crate) fn set_session_opt(&self, session: &str, key: &str, value: &str) {
        let mut sessions = ok(self.sessions.lock(), "fake tmux sessions mutex poisoned");
        let session = some(
            sessions.get_mut(session),
            format!("missing fake session '{session}'"),
        );
        session.options.insert(key.to_string(), value.to_string());
    }

    pub(crate) fn set_window_opt(&self, session: &str, window: &str, key: &str, value: &str) {
        let mut sessions = ok(self.sessions.lock(), "fake tmux sessions mutex poisoned");
        let session_name = session;
        let window_name = window;
        let session = some(
            sessions.get_mut(session),
            format!("missing fake session '{session}'"),
        );
        let window = some(
            session.windows.get_mut(window_name),
            format!("missing fake window '{window_name}' in session '{session_name}'"),
        );
        window.options.insert(key.to_string(), value.to_string());
    }

    pub(crate) fn set_pane_opt(
        &self,
        session: &str,
        window: &str,
        pane_idx: usize,
        key: &str,
        value: &str,
    ) {
        let mut sessions = ok(self.sessions.lock(), "fake tmux sessions mutex poisoned");
        let session_name = session;
        let window_name = window;
        let session = some(
            sessions.get_mut(session),
            format!("missing fake session '{session}'"),
        );
        let window = some(
            session.windows.get_mut(window_name),
            format!("missing fake window '{window_name}' in session '{session_name}'"),
        );
        window.panes[pane_idx]
            .options
            .insert(key.to_string(), value.to_string());
    }

    fn parse_target(target: &str) -> (String, Option<String>) {
        if let Some((session, window)) = target.split_once(':') {
            (session.to_string(), Some(window.to_string()))
        } else {
            (target.to_string(), None)
        }
    }

    fn find_pane<'a>(
        sessions: &'a BTreeMap<String, FakeSession>,
        pane_id: &str,
    ) -> Option<&'a FakePane> {
        for session in sessions.values() {
            for window in session.windows.values() {
                if let Some(pane) = window.panes.iter().find(|pane| pane.pane_id == pane_id) {
                    return Some(pane);
                }
            }
        }
        None
    }

    fn find_pane_mut<'a>(
        sessions: &'a mut BTreeMap<String, FakeSession>,
        pane_id: &str,
    ) -> Option<&'a mut FakePane> {
        for session in sessions.values_mut() {
            for window in session.windows.values_mut() {
                if let Some(pane) = window.panes.iter_mut().find(|pane| pane.pane_id == pane_id) {
                    return Some(pane);
                }
            }
        }
        None
    }

    fn with_pane_mut(&self, pane_id: &str, apply: impl FnOnce(&mut FakePane)) {
        let mut sessions = ok(self.sessions.lock(), "fake tmux sessions mutex poisoned");
        if let Some(pane) = Self::find_pane_mut(&mut sessions, pane_id) {
            apply(pane);
        }
    }

    pub(crate) fn set_pane_content(&self, pane_id: &str, content: &str) {
        self.with_pane_mut(pane_id, |pane| pane.content = content.to_string());
    }

    /// Put the fake pane program on the alternate screen: captures become
    /// capped at the pane height until the window is resized taller.
    pub(crate) fn set_pane_alternate_on(&self, pane_id: &str, alternate_on: bool) {
        self.with_pane_mut(pane_id, |pane| pane.alternate_on = alternate_on);
    }

    pub(crate) fn set_pane_height(&self, pane_id: &str, height: usize) {
        self.with_pane_mut(pane_id, |pane| pane.height = height);
    }

    /// Resolve a window from either a pane target (`%N`) or the fake window
    /// id (`session:window`, what [`Self::window_geometry`] hands out).
    fn with_window_mut(&self, target: &str, apply: impl FnOnce(&mut FakeWindow)) -> bool {
        let mut sessions = ok(self.sessions.lock(), "fake tmux sessions mutex poisoned");
        if target.starts_with('%') {
            for session in sessions.values_mut() {
                for window in session.windows.values_mut() {
                    if window.panes.iter().any(|pane| pane.pane_id == target) {
                        apply(window);
                        return true;
                    }
                }
            }
            return false;
        }
        let Some((session_name, window_name)) = target.split_once(':') else {
            return false;
        };
        if let Some(window) = sessions
            .get_mut(session_name)
            .and_then(|session| session.windows.get_mut(window_name))
        {
            apply(window);
            return true;
        }
        false
    }

    /// Read the window-local `window-size` value for assertions.
    pub(crate) fn window_size_option(&self, session: &str, window: &str) -> Option<String> {
        let sessions = ok(self.sessions.lock(), "fake tmux sessions mutex poisoned");
        sessions
            .get(session)
            .and_then(|session| session.windows.get(window))
            .and_then(|window| window.size_option.clone())
    }

    /// Fake tmux server socket path (unique per instance) — also usable by
    /// tests to contend on the deep-capture window lock.
    pub(crate) fn socket_path(&self) -> String {
        self.socket_dir
            .path()
            .join("fake-socket")
            .display()
            .to_string()
    }

    /// Read the zoom state of a window for assertions.
    pub(crate) fn window_is_zoomed(&self, session: &str, window: &str) -> bool {
        let sessions = ok(self.sessions.lock(), "fake tmux sessions mutex poisoned");
        sessions
            .get(session)
            .and_then(|session| session.windows.get(window))
            .is_some_and(|window| window.zoomed_pane.is_some())
    }

    /// Read the active pane id of a window for assertions.
    pub(crate) fn active_pane(&self, session: &str, window: &str) -> Option<String> {
        let sessions = ok(self.sessions.lock(), "fake tmux sessions mutex poisoned");
        sessions
            .get(session)
            .and_then(|session| session.windows.get(window))
            .map(FakeWindow::active_pane_id)
    }

    /// Force the window-local `window-size` value (e.g. a pre-existing
    /// `latest` policy the restore path must preserve).
    pub(crate) fn set_window_size_option(&self, session: &str, window: &str, value: &str) {
        let mut sessions = ok(self.sessions.lock(), "fake tmux sessions mutex poisoned");
        if let Some(window) = sessions
            .get_mut(session)
            .and_then(|session| session.windows.get_mut(window))
        {
            window.size_option = Some(value.to_string());
        }
    }

    /// Set the window height without touching pane heights or recording an
    /// op — models a multi-pane layout on a tall terminal, where panes are
    /// shorter than their window.
    pub(crate) fn set_window_height(&self, session: &str, window: &str, height: usize) {
        let mut sessions = ok(self.sessions.lock(), "fake tmux sessions mutex poisoned");
        if let Some(window) = sessions
            .get_mut(session)
            .and_then(|session| session.windows.get_mut(window))
        {
            window.height = height;
        }
    }

    pub(crate) fn set_pane_dead_status(&self, pane_id: &str, status: i32) {
        self.with_pane_mut(pane_id, |pane| {
            pane.dead = true;
            pane.dead_status = Some(status);
        });
    }

    /// Script the next `capture_pane` results for `pane_id`: each call pops
    /// one entry into the pane content; once drained, the content freezes on
    /// the last entry.
    pub(crate) fn queue_pane_contents(&self, pane_id: &str, contents: &[&str]) {
        self.with_pane_mut(pane_id, |pane| {
            pane.queued_contents
                .extend(contents.iter().map(ToString::to_string));
        });
    }

    /// Mark `pane_id` dead after `captures` calls to `capture_pane`,
    /// simulating an agent that crashes mid-wait.
    pub(crate) fn set_pane_dies_after_captures(&self, pane_id: &str, captures: usize) {
        self.with_pane_mut(pane_id, |pane| pane.dies_after_captures = Some(captures));
    }

    /// Remove `pane_id` after `captures` calls to `capture_pane`, simulating
    /// a killed window so subsequent `list_panes("%N")` returns
    /// `MissingTarget`.
    pub(crate) fn set_pane_removed_after_captures(&self, pane_id: &str, captures: usize) {
        self.with_pane_mut(pane_id, |pane| pane.removed_after_captures = Some(captures));
    }

    fn record_text_op(&self, op: FakeOp, pane_id: &str, text: &str) {
        ok(self.ops.lock(), "fake tmux ops mutex poisoned").push(op);
        // Mirror the pasted/typed text into pane content so readiness waits
        // observe the change, matching a live TUI accepting input. By
        // default only the text echoes — tests whose assertions depend on
        // the agent visibly responding must declare that frame via
        // [`FakeTmux::set_delivery_response`] instead of inheriting a
        // universal skeleton key tuned to satisfy every verifier.
        let response = ok(
            self.delivery_response.lock(),
            "fake tmux delivery response mutex poisoned",
        )
        .clone();
        let appended = match response {
            Some(response) => format!("{text}\n{response}\n"),
            None => format!("{text}\n"),
        };
        let mut sessions = ok(self.sessions.lock(), "fake tmux sessions mutex poisoned");
        if let Some(pane) = Self::find_pane_mut(&mut sessions, pane_id) {
            pane.content.push_str(&appended);
        }
    }
}
