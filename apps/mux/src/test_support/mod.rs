use std::collections::BTreeMap;
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
use crate::tmux::{PaneInfo, TmuxAdapter, TmuxError};

pub(crate) struct FakeTmux {
    sessions: Mutex<BTreeMap<String, FakeSession>>,
    ops: Mutex<Vec<FakeOp>>,
    fail_paste: AtomicBool,
    fail_send_keys: AtomicBool,
    fail_respawn: AtomicBool,
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

pub(crate) trait TestResultExt<T, E> {
    fn or_panic(self) -> T;
    fn err_or_panic(self) -> E;
}

impl<T, E: std::fmt::Debug> TestResultExt<T, E> for std::result::Result<T, E> {
    #[track_caller]
    fn or_panic(self) -> T {
        match self {
            Ok(value) => value,
            Err(error) => panic!("expected Ok(..), got Err({error:?})"),
        }
    }

    #[track_caller]
    fn err_or_panic(self) -> E {
        match self {
            Ok(_) => panic!("expected Err(..), got Ok(..)"),
            Err(err) => err,
        }
    }
}

pub(crate) trait TestOptionExt<T> {
    fn or_panic(self) -> T;
}

impl<T> TestOptionExt<T> for Option<T> {
    #[track_caller]
    fn or_panic(self) -> T {
        self.unwrap_or_else(|| panic!("expected Some(..), got None"))
    }
}

struct FakeSession {
    options: BTreeMap<String, String>,
    windows: BTreeMap<String, FakeWindow>,
}

struct FakeWindow {
    options: BTreeMap<String, String>,
    panes: Vec<FakePane>,
}

struct FakePane {
    pane_id: String,
    options: BTreeMap<String, String>,
    dead: bool,
    content: String,
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
}

impl FakeTmux {
    pub(crate) fn new() -> Self {
        Self {
            sessions: Mutex::new(BTreeMap::new()),
            ops: Mutex::new(Vec::new()),
            fail_paste: AtomicBool::new(false),
            fail_send_keys: AtomicBool::new(false),
            fail_respawn: AtomicBool::new(false),
        }
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
        session.windows.insert(
            window.to_string(),
            FakeWindow {
                options: BTreeMap::new(),
                panes: Vec::new(),
            },
        );
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
        window.panes.push(FakePane {
            pane_id: pane_id.to_string(),
            options: BTreeMap::new(),
            dead,
            content: String::new(),
        });
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

    pub(crate) fn set_pane_content(&self, pane_id: &str, content: &str) {
        let mut sessions = ok(self.sessions.lock(), "fake tmux sessions mutex poisoned");
        for session in sessions.values_mut() {
            for window in session.windows.values_mut() {
                for pane in &mut window.panes {
                    if pane.pane_id == pane_id {
                        pane.content = content.to_string();
                        return;
                    }
                }
            }
        }
    }

    fn record_text_op(&self, op: FakeOp, pane_id: &str, text: &str) {
        ok(self.ops.lock(), "fake tmux ops mutex poisoned").push(op);
        // Mirror the pasted/typed text into pane content so readiness waits
        // observe the change, matching a live TUI accepting input.
        let appended = format!(
            "{text}\nfake agent accepted the prompt and is streaming a response                  with enough visible output below the pasted text that the pane can                  never be mistaken for a pending input area by any verifier\n"
        );
        let mut sessions = ok(self.sessions.lock(), "fake tmux sessions mutex poisoned");
        for session in sessions.values_mut() {
            for window in session.windows.values_mut() {
                for pane in &mut window.panes {
                    if pane.pane_id == pane_id {
                        pane.content.push_str(&appended);
                    }
                }
            }
        }
    }
}

impl TmuxAdapter for FakeTmux {
    fn session_exists(&self, session_name: &str) -> Result<bool> {
        let sessions = ok(self.sessions.lock(), "fake tmux sessions mutex poisoned");
        Ok(sessions.contains_key(session_name))
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
        let sessions = ok(self.sessions.lock(), "fake tmux sessions mutex poisoned");
        let (session_name, window_name) = if let Some((s, w)) = target.split_once(':') {
            (s, Some(w))
        } else {
            (target, None)
        };

        // Mirror the real client: a missing session or window is a typed
        // MissingTarget error, not an empty result.
        let Some(session) = sessions.get(session_name) else {
            return Err(TmuxError::MissingTarget(target.to_string()).into());
        };

        if let Some(window_name) = window_name {
            let Some(window) = session.windows.get(window_name) else {
                return Err(TmuxError::MissingTarget(target.to_string()).into());
            };
            Ok(window
                .panes
                .iter()
                .map(|p| PaneInfo {
                    pane_id: p.pane_id.clone(),
                    pane_dead: p.dead,
                    pane_dead_status: if p.dead { Some(1) } else { None },
                })
                .collect())
        } else {
            let mut all = Vec::new();
            for window in session.windows.values() {
                for p in &window.panes {
                    all.push(PaneInfo {
                        pane_id: p.pane_id.clone(),
                        pane_dead: p.dead,
                        pane_dead_status: if p.dead { Some(1) } else { None },
                    });
                }
            }
            Ok(all)
        }
    }

    fn split_window(&self, target: &str, _start_directory: &str) -> Result<()> {
        let mut sessions = ok(self.sessions.lock(), "fake tmux sessions mutex poisoned");
        let (session_name, window_name) = if let Some((s, w)) = target.split_once(':') {
            (s.to_string(), w.to_string())
        } else {
            bail!("split_window requires session:window target");
        };
        let session = some(
            sessions.get_mut(&session_name),
            format!("missing fake session '{session_name}'"),
        );
        let window = some(
            session.windows.get_mut(&window_name),
            format!("missing fake window '{window_name}' in session '{session_name}'"),
        );
        let idx = window.panes.len();
        window.panes.push(FakePane {
            pane_id: format!("%{idx}"),
            options: BTreeMap::new(),
            dead: false,
            content: String::new(),
        });
        Ok(())
    }

    fn select_layout(&self, _: &str, _: &str) -> Result<()> {
        Ok(())
    }

    fn respawn_pane(
        &self,
        target: &str,
        start_directory: &str,
        env_overrides: &[(String, String)],
        command: &[String],
    ) -> Result<()> {
        if self.fail_respawn.load(Ordering::Relaxed) {
            bail!("fake tmux respawn_pane failure");
        }
        ok(self.ops.lock(), "fake tmux ops mutex poisoned").push(FakeOp::RespawnPane {
            pane_id: target.to_string(),
            cwd: start_directory.to_string(),
            env: env_overrides.to_vec(),
            command: command.to_vec(),
        });
        Ok(())
    }

    fn attach_session(&self, _: &str) -> Result<()> {
        Ok(())
    }

    fn switch_client(&self, _: &str) -> Result<()> {
        Ok(())
    }

    fn kill_session(&self, name: &str) -> Result<()> {
        let mut sessions = ok(self.sessions.lock(), "fake tmux sessions mutex poisoned");
        sessions.remove(name);
        Ok(())
    }

    fn set_session_option(&self, target: &str, name: &str, value: &str) -> Result<()> {
        self.set_session_opt(target, name, value);
        Ok(())
    }

    fn get_session_option(&self, target: &str, name: &str) -> Result<Option<String>> {
        let sessions = ok(self.sessions.lock(), "fake tmux sessions mutex poisoned");
        let (session_name, _) = Self::parse_target(target);
        let Some(session) = sessions.get(&session_name) else {
            return Err(TmuxError::MissingSession(target.to_string()).into());
        };
        Ok(session.options.get(name).cloned())
    }

    fn set_window_option(&self, target: &str, name: &str, value: &str) -> Result<()> {
        let (session_name, window_name) = Self::parse_target(target);
        if let Some(wn) = window_name {
            self.set_window_opt(&session_name, &wn, name, value);
        }
        Ok(())
    }

    fn get_window_option(&self, target: &str, name: &str) -> Result<Option<String>> {
        let sessions = ok(self.sessions.lock(), "fake tmux sessions mutex poisoned");
        let (session_name, window_name) = Self::parse_target(target);
        // Mirror the real client: missing session/window is a typed error.
        let Some(session) = sessions.get(&session_name) else {
            return Err(TmuxError::MissingTarget(target.to_string()).into());
        };
        let Some(window_name) = window_name else {
            return Err(TmuxError::MissingTarget(target.to_string()).into());
        };
        let Some(window) = session.windows.get(&window_name) else {
            return Err(TmuxError::MissingTarget(target.to_string()).into());
        };
        Ok(window.options.get(name).cloned())
    }

    fn set_pane_option(&self, target: &str, name: &str, value: &str) -> Result<()> {
        let mut sessions = ok(self.sessions.lock(), "fake tmux sessions mutex poisoned");
        for session in sessions.values_mut() {
            for window in session.windows.values_mut() {
                for pane in &mut window.panes {
                    if pane.pane_id == target {
                        pane.options.insert(name.to_string(), value.to_string());
                        return Ok(());
                    }
                }
            }
        }
        Ok(())
    }

    fn get_pane_option(&self, target: &str, name: &str) -> Result<Option<String>> {
        let sessions = ok(self.sessions.lock(), "fake tmux sessions mutex poisoned");
        for session in sessions.values() {
            for window in session.windows.values() {
                for pane in &window.panes {
                    if pane.pane_id == target {
                        return Ok(pane.options.get(name).cloned());
                    }
                }
            }
        }
        Ok(None)
    }

    fn paste_text(&self, target_pane: &str, text: &str) -> Result<()> {
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
        let sessions = ok(self.sessions.lock(), "fake tmux sessions mutex poisoned");
        for session in sessions.values() {
            for window in session.windows.values() {
                for pane in &window.panes {
                    if pane.pane_id == pane_id {
                        let lines: Vec<&str> = pane.content.lines().collect();
                        if lines.len() > history_limit {
                            return Ok(lines[lines.len() - history_limit..].join("\n") + "\n");
                        }
                        return Ok(pane.content.clone());
                    }
                }
            }
        }
        bail!("pane not found: {pane_id}")
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
        session_prefix: "ai".to_string(),
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
        let env = vec![("FOO".to_string(), "bar".to_string())];
        let command = vec![
            "codex".to_string(),
            "--profile".to_string(),
            "fast".to_string(),
        ];
        fake.respawn_pane("%0", "/tmp", &env, &command).or_panic();

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
}
