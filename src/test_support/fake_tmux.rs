//! In-memory [`TmuxAdapter`] used by unit tests.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Result, bail};

use super::{TestResultExt, ok, some};
use crate::tmux::metadata::{
    PANE_AGENT_ID, SESSION_CONFIG_FINGERPRINT, SESSION_PROFILE_ID, SESSION_PROJECT_ID, WINDOW_ROLE,
};
use crate::tmux::{
    PaneInfo, TmuxAdapter, TmuxError, WindowGeometry, WorkspacePaneSnapshot, WorkspaceSnapshot,
    WorkspaceWindowSnapshot,
};

/// Default fake window/pane geometry (mirrors a small real workspace).
const FAKE_WINDOW_WIDTH: usize = 200;
const FAKE_WINDOW_HEIGHT: usize = 24;

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
    no_server: AtomicBool,
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

struct FakeZoom {
    pane_id: String,
    prior_height: usize,
}

struct FakePane {
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
    fn new(pane_id: &str, dead: bool) -> Self {
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

    fn info(&self) -> PaneInfo {
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
            no_server: AtomicBool::new(false),
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
        window.panes.push(FakePane::new(pane_id, dead));
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
        // observe the change, matching a live TUI accepting input.
        let appended = format!(
            "{text}\nfake agent accepted the prompt and is streaming a response                  with enough visible output below the pasted text that the pane can                  never be mistaken for a pending input area by any verifier\n"
        );
        let mut sessions = ok(self.sessions.lock(), "fake tmux sessions mutex poisoned");
        if let Some(pane) = Self::find_pane_mut(&mut sessions, pane_id) {
            pane.content.push_str(&appended);
        }
    }
}

impl TmuxAdapter for FakeTmux {
    fn session_exists(&self, session_name: &str) -> Result<bool> {
        if self.no_server.load(Ordering::Relaxed) {
            return Err(TmuxError::NoServer("no server running on fake socket".into()).into());
        }
        let sessions = ok(self.sessions.lock(), "fake tmux sessions mutex poisoned");
        Ok(sessions.contains_key(session_name))
    }

    fn workspace_snapshot(
        &self,
        session_name: &str,
        window_name: &str,
    ) -> Result<Option<WorkspaceSnapshot>> {
        if self.no_server.load(Ordering::Relaxed) {
            return Ok(None);
        }
        if let Some(error) = ok(
            self.workspace_snapshot_error.lock(),
            "fake tmux snapshot error mutex poisoned",
        )
        .take()
        {
            return Err(error.into());
        }
        let sessions = ok(self.sessions.lock(), "fake tmux sessions mutex poisoned");
        let Some(session) = sessions.get(session_name) else {
            return Ok(None);
        };
        let window = session.windows.get(window_name).map(|window| {
            let panes = window
                .panes
                .iter()
                .map(|pane| WorkspacePaneSnapshot {
                    pane: pane.info(),
                    agent_id: pane.options.get(PANE_AGENT_ID).cloned(),
                })
                .collect();
            WorkspaceWindowSnapshot {
                role: window.options.get(WINDOW_ROLE).cloned(),
                panes,
            }
        });

        Ok(Some(WorkspaceSnapshot {
            fingerprint: session.options.get(SESSION_CONFIG_FINGERPRINT).cloned(),
            project_id: session.options.get(SESSION_PROJECT_ID).cloned(),
            profile_id: session.options.get(SESSION_PROFILE_ID).cloned(),
            window,
        }))
    }

    fn create_detached_session(
        &self,
        name: &str,
        _start_directory: &str,
        window_name: &str,
        _pane_count: usize,
    ) -> Result<()> {
        self.add_session(name);
        self.add_window(name, window_name);
        self.add_pane(name, window_name, "%0", false);
        Ok(())
    }

    fn list_panes(&self, target: &str) -> Result<Vec<PaneInfo>> {
        if self.no_server.load(Ordering::Relaxed) {
            return Err(TmuxError::NoServer("no server running on fake socket".into()).into());
        }
        let sessions = ok(self.sessions.lock(), "fake tmux sessions mutex poisoned");

        // Real tmux accepts pane ids (`%0`) as list-panes targets; post-launch
        // health checks rely on that.
        if target.starts_with('%') {
            for session in sessions.values() {
                for window in session.windows.values() {
                    if let Some(pane) = window.panes.iter().find(|pane| pane.pane_id == target) {
                        return Ok(vec![pane.info()]);
                    }
                }
            }
            return Err(TmuxError::MissingTarget(target.to_string()).into());
        }

        let (session_name, window_name) = if let Some((s, w)) = target.split_once(':') {
            (s, Some(w))
        } else {
            (target, None)
        };

        // Mirror the real client classifier: missing session vs missing window.
        let Some(session) = sessions.get(session_name) else {
            return Err(TmuxError::MissingSession(target.to_string()).into());
        };

        if let Some(window_name) = window_name {
            let Some(window) = session.windows.get(window_name) else {
                return Err(TmuxError::MissingTarget(target.to_string()).into());
            };
            Ok(window.panes.iter().map(FakePane::info).collect())
        } else {
            let mut all = Vec::new();
            for window in session.windows.values() {
                for p in &window.panes {
                    all.push(p.info());
                }
            }
            Ok(all)
        }
    }

    fn split_window(&self, target: &str, _start_directory: &str) -> Result<()> {
        if self.no_server.load(Ordering::Relaxed) {
            return Err(TmuxError::NoServer("no server running on fake socket".into()).into());
        }
        let mut sessions = ok(self.sessions.lock(), "fake tmux sessions mutex poisoned");
        let Some((session_name, window_name)) = target.split_once(':') else {
            return Err(TmuxError::CommandFailure(
                "split_window requires session:window target".into(),
            )
            .into());
        };
        let Some(session) = sessions.get_mut(session_name) else {
            return Err(TmuxError::MissingSession(target.to_string()).into());
        };
        let Some(window) = session.windows.get_mut(window_name) else {
            return Err(TmuxError::MissingTarget(target.to_string()).into());
        };
        let idx = window.panes.len();
        window.panes.push(FakePane::new(&format!("%{idx}"), false));
        Ok(())
    }

    fn select_layout(&self, target: &str, _: &str) -> Result<()> {
        if self.no_server.load(Ordering::Relaxed) {
            return Err(TmuxError::NoServer("no server running on fake socket".into()).into());
        }
        let sessions = ok(self.sessions.lock(), "fake tmux sessions mutex poisoned");
        if target.starts_with('%') {
            return if Self::find_pane(&sessions, target).is_some() {
                Ok(())
            } else {
                Err(TmuxError::MissingTarget(target.to_string()).into())
            };
        }
        let (session_name, window_name) = Self::parse_target(target);
        let Some(session) = sessions.get(&session_name) else {
            return Err(TmuxError::MissingSession(target.to_string()).into());
        };
        if let Some(window_name) = window_name
            && !session.windows.contains_key(&window_name)
        {
            return Err(TmuxError::MissingTarget(target.to_string()).into());
        }
        Ok(())
    }

    fn respawn_pane(
        &self,
        target: &str,
        start_directory: &str,
        env_overrides: &[(String, String)],
        command: &[String],
    ) -> Result<()> {
        if self.no_server.load(Ordering::Relaxed) {
            return Err(TmuxError::NoServer("no server running on fake socket".into()).into());
        }
        if self.fail_respawn.load(Ordering::Relaxed) {
            bail!("fake tmux respawn_pane failure");
        }

        // Revive by default (mirrors a successful respawn); optional flag
        // simulates a process that dies before the post-launch health window.
        let leave_dead = self.respawn_exits_immediately.load(Ordering::Relaxed);
        let mut sessions = ok(self.sessions.lock(), "fake tmux sessions mutex poisoned");
        for session in sessions.values_mut() {
            for window in session.windows.values_mut() {
                if let Some(pane) = window.panes.iter_mut().find(|pane| pane.pane_id == target) {
                    pane.dead = leave_dead;
                    pane.dead_status = leave_dead.then_some(1);
                    drop(sessions);
                    ok(self.ops.lock(), "fake tmux ops mutex poisoned").push(FakeOp::RespawnPane {
                        pane_id: target.to_string(),
                        cwd: start_directory.to_string(),
                        env: env_overrides.to_vec(),
                        command: command.to_vec(),
                    });
                    return Ok(());
                }
            }
        }
        Err(TmuxError::MissingTarget(target.to_string()).into())
    }

    fn attach_session(&self, session_name: &str) -> Result<()> {
        self.attach_or_switch(session_name)
    }

    fn switch_client(&self, session_name: &str) -> Result<()> {
        self.attach_or_switch(session_name)
    }

    fn kill_session(&self, name: &str) -> Result<()> {
        if self.no_server.load(Ordering::Relaxed) {
            return Err(TmuxError::NoServer("no server running on fake socket".into()).into());
        }
        let mut sessions = ok(self.sessions.lock(), "fake tmux sessions mutex poisoned");
        if self.vanish_before_kill.swap(false, Ordering::Relaxed) {
            sessions.remove(name);
            return Err(TmuxError::MissingSession(name.to_string()).into());
        }
        if sessions.remove(name).is_none() {
            return Err(TmuxError::MissingSession(name.to_string()).into());
        }
        Ok(())
    }

    fn set_session_option(&self, target: &str, name: &str, value: &str) -> Result<()> {
        if self.no_server.load(Ordering::Relaxed) {
            return Err(TmuxError::NoServer("no server running on fake socket".into()).into());
        }
        let mut sessions = ok(self.sessions.lock(), "fake tmux sessions mutex poisoned");
        let (session_name, _) = Self::parse_target(target);
        let Some(session) = sessions.get_mut(&session_name) else {
            return Err(TmuxError::MissingSession(target.to_string()).into());
        };
        session.options.insert(name.to_string(), value.to_string());
        Ok(())
    }

    fn set_window_option(&self, target: &str, name: &str, value: &str) -> Result<()> {
        if self.no_server.load(Ordering::Relaxed) {
            return Err(TmuxError::NoServer("no server running on fake socket".into()).into());
        }
        let mut sessions = ok(self.sessions.lock(), "fake tmux sessions mutex poisoned");
        let (session_name, window_name) = Self::parse_target(target);
        let Some(session) = sessions.get_mut(&session_name) else {
            return Err(TmuxError::MissingSession(target.to_string()).into());
        };
        let Some(window_name) = window_name else {
            return Err(TmuxError::CommandFailure(
                "set_window_option requires session:window target".into(),
            )
            .into());
        };
        let Some(window) = session.windows.get_mut(&window_name) else {
            return Err(TmuxError::MissingTarget(target.to_string()).into());
        };
        // Keep the modeled window-size policy in sync so restore-by-value
        // (deep capture) is observable through window_geometry.
        if name == "window-size" {
            window.size_option = Some(value.to_string());
        }
        window.options.insert(name.to_string(), value.to_string());
        Ok(())
    }

    fn set_pane_option(&self, target: &str, name: &str, value: &str) -> Result<()> {
        if self.no_server.load(Ordering::Relaxed) {
            return Err(TmuxError::NoServer("no server running on fake socket".into()).into());
        }
        let mut sessions = ok(self.sessions.lock(), "fake tmux sessions mutex poisoned");
        let Some(pane) = Self::find_pane_mut(&mut sessions, target) else {
            return Err(TmuxError::MissingTarget(target.to_string()).into());
        };
        pane.options.insert(name.to_string(), value.to_string());
        Ok(())
    }

    fn get_pane_option(&self, target: &str, name: &str) -> Result<Option<String>> {
        if self.no_server.load(Ordering::Relaxed) {
            return Err(TmuxError::NoServer("no server running on fake socket".into()).into());
        }
        let sessions = ok(self.sessions.lock(), "fake tmux sessions mutex poisoned");
        let Some(pane) = Self::find_pane(&sessions, target) else {
            return Err(TmuxError::MissingTarget(target.to_string()).into());
        };
        Ok(pane.options.get(name).cloned())
    }

    fn paste_text(&self, target_pane: &str, text: &str) -> Result<()> {
        if let Some(error) = self.delivery_failure(target_pane) {
            return Err(error);
        }
        if self.fail_paste.load(Ordering::Relaxed) {
            bail!("fake tmux paste_text failure");
        }
        self.record_text_op(
            FakeOp::PasteText {
                pane_id: target_pane.to_string(),
                text: text.to_string(),
            },
            target_pane,
            text,
        );
        Ok(())
    }

    fn send_keys(&self, target_pane: &str, keys: &[&str]) -> Result<()> {
        if let Some(error) = self.delivery_failure(target_pane) {
            return Err(error);
        }
        if self.fail_send_keys.load(Ordering::Relaxed) {
            bail!("fake tmux send_keys failure");
        }
        ok(self.ops.lock(), "fake tmux ops mutex poisoned").push(FakeOp::SendKeys {
            pane_id: target_pane.to_string(),
            keys: keys.iter().map(ToString::to_string).collect(),
        });
        Ok(())
    }

    fn send_text(&self, target_pane: &str, text: &str) -> Result<()> {
        if let Some(error) = self.delivery_failure(target_pane) {
            return Err(error);
        }
        if self.fail_send_keys.load(Ordering::Relaxed) {
            bail!("fake tmux send_text failure");
        }
        self.record_text_op(
            FakeOp::SendText {
                pane_id: target_pane.to_string(),
                text: text.to_string(),
            },
            target_pane,
            text,
        );
        Ok(())
    }

    fn capture_pane(&self, pane_id: &str, history_limit: usize) -> Result<String> {
        if self.no_server.load(Ordering::Relaxed) {
            return Err(TmuxError::NoServer("no server running on fake socket".into()).into());
        }
        let mut sessions = ok(self.sessions.lock(), "fake tmux sessions mutex poisoned");
        for session in sessions.values_mut() {
            for window in session.windows.values_mut() {
                if let Some(idx) = window.panes.iter().position(|pane| pane.pane_id == pane_id) {
                    let pane = &mut window.panes[idx];
                    if let Some(next) = pane.queued_contents.pop_front() {
                        pane.content = next;
                    }
                    if let Some(remaining) = &mut pane.dies_after_captures {
                        *remaining = remaining.saturating_sub(1);
                        if *remaining == 0 {
                            pane.dead = true;
                            pane.dead_status = Some(1);
                        }
                    }
                    let remove_now = if let Some(remaining) = &mut pane.removed_after_captures {
                        *remaining = remaining.saturating_sub(1);
                        *remaining == 0
                    } else {
                        false
                    };
                    // An alternate-screen pane has no tmux history: capture
                    // depth is capped at the visible pane height, mirroring
                    // real tmux. The full content plays the TUI's internal
                    // transcript, reachable only after a resize.
                    let depth = if pane.alternate_on {
                        history_limit.min(pane.height)
                    } else {
                        history_limit
                    };
                    let lines: Vec<&str> = pane.content.lines().collect();
                    let content = if lines.len() > depth {
                        lines[lines.len() - depth..].join("\n") + "\n"
                    } else {
                        pane.content.clone()
                    };
                    if remove_now {
                        window.panes.remove(idx);
                        // Mirror tmux: removing the zoomed pane auto-unzooms
                        // the window, and a removed active pane hands the
                        // active slot to a survivor.
                        if window
                            .zoomed_pane
                            .as_ref()
                            .is_some_and(|zoom| zoom.pane_id == pane_id)
                        {
                            window.zoomed_pane = None;
                        }
                        if window.active_pane.as_deref() == Some(pane_id) {
                            window.active_pane =
                                window.panes.first().map(|pane| pane.pane_id.clone());
                        }
                    }
                    self.note_capture_served();
                    return Ok(content);
                }
            }
        }
        Err(TmuxError::MissingTarget(pane_id.to_string()).into())
    }

    fn window_geometry(&self, pane_id: &str) -> Result<WindowGeometry> {
        if self.no_server.load(Ordering::Relaxed) {
            return Err(TmuxError::NoServer("no server running on fake socket".into()).into());
        }
        let sessions = ok(self.sessions.lock(), "fake tmux sessions mutex poisoned");
        let mut geometry = None;
        'find: for (session_name, session) in sessions.iter() {
            for (window_name, window) in &session.windows {
                if window.panes.iter().any(|pane| pane.pane_id == pane_id) {
                    geometry = Some(WindowGeometry {
                        // Fake window id: a `session:window` target, which
                        // the window-addressed fake ops resolve like tmux
                        // resolves `@N`.
                        window_id: format!("{session_name}:{window_name}"),
                        socket_path: self.socket_path(),
                        width: window.width,
                        height: window.height,
                        zoomed: window.zoomed_pane.is_some(),
                        pane_active: window.active_pane_id() == pane_id,
                        active_pane_id: window.active_pane_id(),
                        size_option: window.size_option.clone(),
                    });
                    break 'find;
                }
            }
        }
        drop(sessions);
        match geometry {
            Some(geometry) => {
                self.note_geometry_served();
                Ok(geometry)
            }
            None => Err(TmuxError::MissingTarget(pane_id.to_string()).into()),
        }
    }

    fn resize_window(&self, target: &str, width: usize, height: usize) -> Result<()> {
        if self.no_server.load(Ordering::Relaxed) {
            return Err(TmuxError::NoServer("no server running on fake socket".into()).into());
        }
        let found = self.with_window_mut(target, |window| {
            window.width = width;
            window.height = height;
            // Mirrors tmux: an explicit resize forces the local policy.
            window.size_option = Some("manual".to_string());
            // Approximation: every pane tracks the window height (exact for
            // the zoomed pane, close enough for layout panes in tests).
            for pane in &mut window.panes {
                pane.height = height;
            }
        });
        if !found {
            return Err(TmuxError::MissingTarget(target.to_string()).into());
        }
        ok(self.ops.lock(), "fake tmux ops mutex poisoned").push(FakeOp::ResizeWindow {
            target: target.to_string(),
            width,
            height,
        });
        Ok(())
    }

    fn toggle_pane_zoom(&self, target: &str) -> Result<()> {
        if self.no_server.load(Ordering::Relaxed) {
            return Err(TmuxError::NoServer("no server running on fake socket".into()).into());
        }
        let found = self.with_window_mut(target, |window| {
            if let Some(zoom) = window.zoomed_pane.take() {
                // Unzoom: the pane returns to its layout height; the active
                // pane does NOT switch back (tmux semantics).
                if let Some(pane) = window
                    .panes
                    .iter_mut()
                    .find(|pane| pane.pane_id == zoom.pane_id)
                {
                    pane.height = zoom.prior_height;
                }
            } else {
                // Zoom: a `%` target zooms that pane; a window target
                // resolves to the active pane (tmux semantics). The zoomed
                // pane spans the window and becomes active.
                let pane_id = if target.starts_with('%') {
                    target.to_string()
                } else {
                    window.active_pane_id()
                };
                let window_height = window.height;
                if let Some(pane) = window.panes.iter_mut().find(|pane| pane.pane_id == pane_id) {
                    window.zoomed_pane = Some(FakeZoom {
                        pane_id: pane_id.clone(),
                        prior_height: pane.height,
                    });
                    pane.height = window_height;
                    window.active_pane = Some(pane_id);
                }
            }
        });
        if !found {
            return Err(TmuxError::MissingTarget(target.to_string()).into());
        }
        ok(self.ops.lock(), "fake tmux ops mutex poisoned").push(FakeOp::ToggleZoom {
            target: target.to_string(),
        });
        Ok(())
    }

    fn unset_window_size_option(&self, target: &str) -> Result<()> {
        if self.no_server.load(Ordering::Relaxed) {
            return Err(TmuxError::NoServer("no server running on fake socket".into()).into());
        }
        let found = self.with_window_mut(target, |window| {
            window.size_option = None;
        });
        if !found {
            return Err(TmuxError::MissingTarget(target.to_string()).into());
        }
        ok(self.ops.lock(), "fake tmux ops mutex poisoned").push(FakeOp::UnsetWindowSizeOption {
            target: target.to_string(),
        });
        Ok(())
    }

    fn unzoom_window(&self, target: &str) -> Result<()> {
        if self.no_server.load(Ordering::Relaxed) {
            return Err(TmuxError::NoServer("no server running on fake socket".into()).into());
        }
        // Atomic under the sessions mutex, mirroring the single server-side
        // `if-shell` conditional: unzoom only when currently zoomed, never
        // zoom.
        let found = self.with_window_mut(target, |window| {
            if let Some(zoom) = window.zoomed_pane.take()
                && let Some(pane) = window
                    .panes
                    .iter_mut()
                    .find(|pane| pane.pane_id == zoom.pane_id)
            {
                pane.height = zoom.prior_height;
            }
        });
        if !found {
            return Err(TmuxError::MissingTarget(target.to_string()).into());
        }
        ok(self.ops.lock(), "fake tmux ops mutex poisoned").push(FakeOp::UnzoomWindow {
            target: target.to_string(),
        });
        Ok(())
    }

    fn select_pane(&self, pane_id: &str) -> Result<()> {
        if self.no_server.load(Ordering::Relaxed) {
            return Err(TmuxError::NoServer("no server running on fake socket".into()).into());
        }
        let found = self.with_window_mut(pane_id, |window| {
            window.active_pane = Some(pane_id.to_string());
        });
        if !found {
            return Err(TmuxError::MissingTarget(pane_id.to_string()).into());
        }
        ok(self.ops.lock(), "fake tmux ops mutex poisoned").push(FakeOp::SelectPane {
            pane_id: pane_id.to_string(),
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tmux::TmuxAdapter;

    #[test]
    fn respawn_pane_records_operation() {
        let fake = FakeTmux::new();
        fake.add_session("s");
        fake.add_window("s", "agents");
        fake.add_pane("s", "agents", "%0", false);
        let env = vec![("FOO".to_string(), "bar".to_string())];
        let command = vec![
            "codex".to_string(),
            "--profile".to_string(),
            "fast".to_string(),
        ];
        fake.respawn_pane("%0", "/tmp", &env, &command)
            .or_panic("respawn_pane_records_operation");

        let ops = fake.ops();
        assert_eq!(
            ops,
            vec![FakeOp::RespawnPane {
                pane_id: "%0".to_string(),
                cwd: "/tmp".to_string(),
                env: vec![("FOO".to_string(), "bar".to_string())],
                command: vec![
                    "codex".to_string(),
                    "--profile".to_string(),
                    "fast".to_string(),
                ],
            }]
        );
    }

    #[test]
    fn respawn_pane_unknown_returns_missing_target() {
        let fake = FakeTmux::new();
        let error = fake
            .respawn_pane("%99", "/tmp", &[], &["true".into()])
            .err_or_panic("respawn_pane_unknown_returns_missing_target: expected Err");
        assert!(matches!(
            error.downcast_ref::<TmuxError>(),
            Some(TmuxError::MissingTarget(_))
        ));
        assert!(fake.ops().is_empty());
    }

    #[test]
    fn kill_session_missing_returns_missing_session() {
        let fake = FakeTmux::new();
        let error = fake
            .kill_session("gone")
            .err_or_panic("kill_session_missing_returns_missing_session: expected Err");
        assert!(matches!(
            error.downcast_ref::<TmuxError>(),
            Some(TmuxError::MissingSession(_))
        ));
    }

    #[test]
    fn split_window_missing_window_returns_missing_target() {
        let fake = FakeTmux::new();
        fake.add_session("s");
        let error = fake
            .split_window("s:agents", "/tmp")
            .err_or_panic("split_window_missing_window_returns_missing_target: expected Err");
        assert!(matches!(
            error.downcast_ref::<TmuxError>(),
            Some(TmuxError::MissingTarget(_))
        ));
    }

    #[test]
    fn set_session_option_missing_returns_missing_session() {
        let fake = FakeTmux::new();
        let error = fake
            .set_session_option("gone", "@k", "v")
            .err_or_panic("set_session_option_missing_returns_missing_session: expected Err");
        assert!(matches!(
            error.downcast_ref::<TmuxError>(),
            Some(TmuxError::MissingSession(_))
        ));
    }

    #[test]
    fn attach_session_missing_is_status_only_not_typed() {
        let fake = FakeTmux::new();
        let error = fake
            .attach_session("gone")
            .err_or_panic("attach_session_missing_is_status_only_not_typed: expected Err");
        // Real interactive attach inherits stderr and only reports a status;
        // typed MissingSession/NoServer would overstate the transport contract.
        assert!(
            error.downcast_ref::<TmuxError>().is_none(),
            "interactive attach must stay status-only, got typed: {error}"
        );
        assert!(
            error
                .to_string()
                .contains("tmux command failed with status"),
            "expected status-only message, got: {error}"
        );
    }

    #[test]
    fn switch_client_missing_is_status_only_not_typed() {
        let fake = FakeTmux::new();
        let error = fake
            .switch_client("gone")
            .err_or_panic("switch_client_missing_is_status_only_not_typed: expected Err");
        assert!(
            error.downcast_ref::<TmuxError>().is_none(),
            "interactive switch must stay status-only, got typed: {error}"
        );
    }

    #[test]
    fn select_layout_missing_window_returns_missing_target() {
        let fake = FakeTmux::new();
        fake.add_session("s");
        let error = fake
            .select_layout("s:agents", "even-vertical")
            .err_or_panic("select_layout_missing_window_returns_missing_target: expected Err");
        assert!(matches!(
            error.downcast_ref::<TmuxError>(),
            Some(TmuxError::MissingTarget(_))
        ));
    }

    #[test]
    fn list_panes_missing_session_returns_missing_session() {
        let fake = FakeTmux::new();
        let error = fake
            .list_panes("gone:agents")
            .err_or_panic("list_panes_missing_session_returns_missing_session: expected Err");
        assert!(matches!(
            error.downcast_ref::<TmuxError>(),
            Some(TmuxError::MissingSession(_))
        ));
    }

    #[test]
    fn set_pane_option_no_server_returns_no_server() {
        let fake = FakeTmux::new();
        fake.add_session("s");
        fake.add_window("s", "agents");
        fake.add_pane("s", "agents", "%0", false);
        fake.set_no_server(true);

        let error = fake
            .set_pane_option("%0", "@k", "v")
            .err_or_panic("set_pane_option_no_server_returns_no_server: expected Err");
        assert!(matches!(
            error.downcast_ref::<TmuxError>(),
            Some(TmuxError::NoServer(_))
        ));
    }

    #[test]
    fn get_pane_option_no_server_returns_no_server() {
        let fake = FakeTmux::new();
        fake.add_session("s");
        fake.add_window("s", "agents");
        fake.add_pane("s", "agents", "%0", false);
        fake.set_no_server(true);

        let error = fake
            .get_pane_option("%0", "@k")
            .err_or_panic("get_pane_option_no_server_returns_no_server: expected Err");
        assert!(matches!(
            error.downcast_ref::<TmuxError>(),
            Some(TmuxError::NoServer(_))
        ));
    }

    #[test]
    fn set_pane_option_missing_returns_missing_target() {
        let fake = FakeTmux::new();
        let error = fake
            .set_pane_option("%99", "@k", "v")
            .err_or_panic("set_pane_option_missing_returns_missing_target: expected Err");
        assert!(matches!(
            error.downcast_ref::<TmuxError>(),
            Some(TmuxError::MissingTarget(_))
        ));
    }
}
