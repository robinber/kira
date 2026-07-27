//! Shared unit-test fixtures: `FakeTmux`, `test_project`, and panic helpers.
//!
//! All unwrap-style helpers take a context string so failures name the
//! operation under test (and still use `#[track_caller]` for the call site):
//! - free functions [`ok`] / [`err`] / [`some`]
//! - extension methods [`TestResultExt::or_panic`] / [`err_or_panic`]
//! - [`TestOptionExt::or_panic`] for `Option`

use std::collections::{BTreeMap, VecDeque};
use std::fmt::Display;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Result, bail};

use crate::config::{AgentMode, Layout, RemainOnExit};
use crate::model::{ResolvedAgent, ResolvedProject};
use crate::tmux::metadata::{
    PANE_AGENT_ID, SESSION_CONFIG_FINGERPRINT, SESSION_PROFILE_ID, SESSION_PROJECT_ID, WINDOW_ROLE,
    WINDOW_ROLE_AGENTS,
};
use crate::tmux::{
    PaneInfo, TmuxAdapter, TmuxError, WindowGeometry, WorkspacePaneSnapshot, WorkspaceSnapshot,
    WorkspaceWindowSnapshot,
};

/// Default fake window/pane geometry (mirrors a small real workspace).
const FAKE_WINDOW_WIDTH: usize = 200;
const FAKE_WINDOW_HEIGHT: usize = 24;

pub(crate) struct FakeTmux {
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
}

#[track_caller]
pub(crate) fn ok<T, E>(result: std::result::Result<T, E>, context: impl Display) -> T
where
    E: Display,
{
    result.unwrap_or_else(|err| panic!("{context}: {err}"))
}

#[track_caller]
pub(crate) fn err<T, E>(result: std::result::Result<T, E>, context: impl Display) -> E {
    match result {
        Ok(_) => panic!("{context}"),
        Err(err) => err,
    }
}

#[track_caller]
pub(crate) fn some<T>(value: Option<T>, context: impl Display) -> T {
    value.unwrap_or_else(|| panic!("{context}"))
}

/// Extension helpers for `Result` in tests (same semantics as [`ok`] /
/// [`err`]).
pub(crate) trait TestResultExt<T, E> {
    fn or_panic(self, context: impl Display) -> T
    where
        E: Display;
    fn err_or_panic(self, context: impl Display) -> E;
}

impl<T, E> TestResultExt<T, E> for std::result::Result<T, E> {
    #[track_caller]
    fn or_panic(self, context: impl Display) -> T
    where
        E: Display,
    {
        ok(self, context)
    }

    #[track_caller]
    fn err_or_panic(self, context: impl Display) -> E {
        err(self, context)
    }
}

/// Extension helper for `Option` in tests (same semantics as [`some`]).
pub(crate) trait TestOptionExt<T> {
    fn or_panic(self, context: impl Display) -> T;
}

impl<T> TestOptionExt<T> for Option<T> {
    #[track_caller]
    fn or_panic(self, context: impl Display) -> T {
        some(self, context)
    }
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
    /// Pane id the window is zoomed on, when zoomed.
    zoomed_pane: Option<String>,
    /// Mirrors the tmux side effect of `resize-window`: a window-local
    /// `window-size manual` that stays until explicitly unset.
    size_option_set: bool,
}

impl FakeWindow {
    fn new() -> Self {
        Self {
            options: BTreeMap::new(),
            panes: Vec::new(),
            width: FAKE_WINDOW_WIDTH,
            height: FAKE_WINDOW_HEIGHT,
            zoomed_pane: None,
            size_option_set: false,
        }
    }
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
        pane_id: String,
        width: usize,
        height: usize,
    },
    ToggleZoom {
        pane_id: String,
    },
    UnsetWindowSizeOption {
        pane_id: String,
    },
}

impl FakeTmux {
    pub(crate) fn new() -> Self {
        Self {
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

    fn with_window_of_pane_mut(&self, pane_id: &str, apply: impl FnOnce(&mut FakeWindow)) -> bool {
        let mut sessions = ok(self.sessions.lock(), "fake tmux sessions mutex poisoned");
        for session in sessions.values_mut() {
            for window in session.windows.values_mut() {
                if window.panes.iter().any(|pane| pane.pane_id == pane_id) {
                    apply(window);
                    return true;
                }
            }
        }
        false
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

    fn get_session_option(&self, target: &str, name: &str) -> Result<Option<String>> {
        if self.no_server.load(Ordering::Relaxed) {
            return Err(TmuxError::NoServer("no server running on fake socket".into()).into());
        }
        let sessions = ok(self.sessions.lock(), "fake tmux sessions mutex poisoned");
        let (session_name, _) = Self::parse_target(target);
        let Some(session) = sessions.get(&session_name) else {
            return Err(TmuxError::MissingSession(target.to_string()).into());
        };
        Ok(session.options.get(name).cloned())
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
        for session in sessions.values() {
            for window in session.windows.values() {
                if window.panes.iter().any(|pane| pane.pane_id == pane_id) {
                    return Ok(WindowGeometry {
                        width: window.width,
                        height: window.height,
                        zoomed: window.zoomed_pane.is_some(),
                        pane_active: window
                            .zoomed_pane
                            .as_deref()
                            .is_none_or(|zoomed| zoomed == pane_id),
                        size_option_set: window.size_option_set,
                    });
                }
            }
        }
        Err(TmuxError::MissingTarget(pane_id.to_string()).into())
    }

    fn resize_window(&self, pane_id: &str, width: usize, height: usize) -> Result<()> {
        if self.no_server.load(Ordering::Relaxed) {
            return Err(TmuxError::NoServer("no server running on fake socket".into()).into());
        }
        let found = self.with_window_of_pane_mut(pane_id, |window| {
            window.width = width;
            window.height = height;
            window.size_option_set = true;
            // Approximation: every pane tracks the window height (exact for
            // the zoomed pane, close enough for layout panes in tests).
            for pane in &mut window.panes {
                pane.height = height;
            }
        });
        if !found {
            return Err(TmuxError::MissingTarget(pane_id.to_string()).into());
        }
        ok(self.ops.lock(), "fake tmux ops mutex poisoned").push(FakeOp::ResizeWindow {
            pane_id: pane_id.to_string(),
            width,
            height,
        });
        Ok(())
    }

    fn toggle_pane_zoom(&self, pane_id: &str) -> Result<()> {
        if self.no_server.load(Ordering::Relaxed) {
            return Err(TmuxError::NoServer("no server running on fake socket".into()).into());
        }
        let found = self.with_window_of_pane_mut(pane_id, |window| {
            window.zoomed_pane = if window.zoomed_pane.is_some() {
                None
            } else {
                Some(pane_id.to_string())
            };
        });
        if !found {
            return Err(TmuxError::MissingTarget(pane_id.to_string()).into());
        }
        ok(self.ops.lock(), "fake tmux ops mutex poisoned").push(FakeOp::ToggleZoom {
            pane_id: pane_id.to_string(),
        });
        Ok(())
    }

    fn unset_window_size_option(&self, pane_id: &str) -> Result<()> {
        if self.no_server.load(Ordering::Relaxed) {
            return Err(TmuxError::NoServer("no server running on fake socket".into()).into());
        }
        let found = self.with_window_of_pane_mut(pane_id, |window| {
            window.size_option_set = false;
        });
        if !found {
            return Err(TmuxError::MissingTarget(pane_id.to_string()).into());
        }
        ok(self.ops.lock(), "fake tmux ops mutex poisoned").push(FakeOp::UnsetWindowSizeOption {
            pane_id: pane_id.to_string(),
        });
        Ok(())
    }
}

pub(crate) fn test_project() -> ResolvedProject {
    ResolvedProject {
        id: "test".to_string(),
        profile_id: "default".to_string(),
        name: "Test".to_string(),
        root: PathBuf::from("/tmp/test-project"),
        layout: Layout::Auto,
        main_pane_ratio: 50,
        window_name: "agents".to_string(),
        session_prefix: "kira".to_string(),
        default_shell: "/bin/sh".to_string(),
        remain_on_exit: RemainOnExit::Failed,
        tmux_bin: "tmux".to_string(),
        agents: vec![
            ResolvedAgent {
                id: "alpha".to_string(),
                label: "Alpha".to_string(),
                mode: AgentMode::Direct,
                command: Some("echo".to_string()),
                shell_command: None,
                args: vec![],
                cwd: PathBuf::from("/tmp/test-project"),
                env: BTreeMap::new(),
                capabilities: vec![],
                prompt_template: None,
                submit: None,
                text_delivery: None,
            },
            ResolvedAgent {
                id: "beta".to_string(),
                label: "Beta".to_string(),
                mode: AgentMode::Direct,
                command: Some("echo".to_string()),
                shell_command: None,
                args: vec![],
                cwd: PathBuf::from("/tmp/test-project"),
                env: BTreeMap::new(),
                capabilities: vec![],
                prompt_template: None,
                submit: None,
                text_delivery: None,
            },
        ],
        fingerprint: "abc123".to_string(),
        groups: BTreeMap::new(),
    }
}

pub(crate) fn setup_healthy_session(fake: &FakeTmux, project: &ResolvedProject) {
    setup_session_with_dead_panes(fake, project, &[]);
}

/// Set up a fully-tagged managed session whose panes at `dead_pane_indexes`
/// are dead. An empty slice yields a healthy session.
pub(crate) fn setup_session_with_dead_panes(
    fake: &FakeTmux,
    project: &ResolvedProject,
    dead_pane_indexes: &[usize],
) {
    let session = crate::workspace::session_name(project);
    fake.add_session(&session);
    fake.set_session_opt(&session, SESSION_CONFIG_FINGERPRINT, &project.fingerprint);
    fake.set_session_opt(&session, SESSION_PROJECT_ID, &project.id);
    fake.set_session_opt(&session, SESSION_PROFILE_ID, &project.profile_id);
    fake.add_window(&session, &project.window_name);
    fake.set_window_opt(
        &session,
        &project.window_name,
        WINDOW_ROLE,
        WINDOW_ROLE_AGENTS,
    );

    for (i, agent) in project.agents.iter().enumerate() {
        let pane_id = format!("%{i}");
        fake.add_pane(
            &session,
            &project.window_name,
            &pane_id,
            dead_pane_indexes.contains(&i),
        );
        fake.set_pane_opt(&session, &project.window_name, i, PANE_AGENT_ID, &agent.id);
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
