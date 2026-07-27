//! [`TmuxAdapter`] implementation for [`super::FakeTmux`].

use std::sync::atomic::Ordering;

use anyhow::{Result, bail};

use super::{FakeOp, FakePane, FakeTmux, FakeZoom, ok};
use crate::tmux::metadata::{
    PANE_AGENT_ID, SESSION_CONFIG_FINGERPRINT, SESSION_PROFILE_ID, SESSION_PROJECT_ID, WINDOW_ROLE,
};
use crate::tmux::{
    PaneInfo, TmuxAdapter, TmuxError, WindowGeometry, WorkspacePaneSnapshot, WorkspaceSnapshot,
    WorkspaceWindowSnapshot,
};

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

        let mut panes = if let Some(window_name) = window_name {
            let Some(window) = session.windows.get(window_name) else {
                return Err(TmuxError::MissingTarget(target.to_string()).into());
            };
            window.panes.iter().map(FakePane::info).collect()
        } else {
            let mut all = Vec::new();
            for window in session.windows.values() {
                for p in &window.panes {
                    all.push(p.info());
                }
            }
            all
        };
        if self.reverse_pane_listing_enabled() {
            panes.reverse();
        }
        Ok(panes)
    }

    fn split_window(&self, target: &str, _start_directory: &str) -> Result<String> {
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
        let pane_id = format!("%{}", window.panes.len());
        window.panes.push(FakePane::new(&pane_id, false));
        Ok(pane_id)
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
        self.ensure_deliverable(target_pane)?;
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
        self.ensure_deliverable(target_pane)?;
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
        self.ensure_deliverable(target_pane)?;
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
        if self.fail_capture_enabled() {
            return Err(TmuxError::CommandFailure("fake transient capture failure".into()).into());
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
