//! Start, repair, attach, restart, and kill managed workspaces.
//!
//! Ownership checks and topology guards keep foreign sessions out of scope.

use anyhow::{Result, bail};

use super::launch::{TopologyGuard, apply_layout, respawn_agent, verify_panes_survived_launch};
use super::{session_name, window_target};
use crate::config::ConfigError;
use crate::error::KiraMuxError;
use crate::inspector::{self, ManagedPane, WorkspaceTopology};
use crate::model::{ResolvedAgent, ResolvedProject};
use crate::tmux::TmuxAdapter;
use crate::tmux::metadata::{
    PANE_AGENT_ID, SESSION_CONFIG_FINGERPRINT, SESSION_PROFILE_ID, SESSION_PROJECT_ID, WINDOW_ROLE,
    WINDOW_ROLE_AGENTS,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StartOutcome {
    Healthy,
    Degraded,
}

pub(crate) fn start(
    tmux: &dyn TmuxAdapter,
    project: &ResolvedProject,
    attach_after: bool,
) -> Result<StartOutcome> {
    let session = session_name(project);
    tracing::debug!(
        project_id = project.id.as_str(),
        session,
        "starting workspace"
    );

    let outcome = match inspector::inspect(tmux, project)? {
        WorkspaceTopology::Absent => create(tmux, project, &session)?,
        WorkspaceTopology::Healthy(_) => StartOutcome::Healthy,
        WorkspaceTopology::Degraded(workspace) => repair(tmux, project, &workspace.panes)?,
        WorkspaceTopology::Drifted { reason } => {
            return Err(KiraMuxError::Drifted {
                project_id: project.id.clone(),
                reason,
            }
            .into());
        }
    };

    if attach_after {
        attach_to_session(tmux, &session)?;
    }

    Ok(outcome)
}

pub(crate) fn attach(tmux: &dyn TmuxAdapter, project: &ResolvedProject) -> Result<()> {
    let session = session_name(project);
    tracing::debug!(
        project_id = project.id.as_str(),
        session,
        "attaching workspace"
    );

    if !tmux.session_exists(&session)? {
        return Err(KiraMuxError::SessionAbsent.into());
    }

    inspector::ensure_session_owned(tmux, project)?;
    attach_to_session(tmux, &session)
}

pub(crate) fn restart(
    tmux: &dyn TmuxAdapter,
    project: &ResolvedProject,
    agent_id: Option<&str>,
) -> Result<StartOutcome> {
    let session = session_name(project);
    tracing::debug!(
        project_id = project.id.as_str(),
        session,
        agent_id,
        "restarting workspace panes"
    );

    let panes = match inspector::inspect(tmux, project)? {
        WorkspaceTopology::Absent => return Err(KiraMuxError::SessionAbsent.into()),
        WorkspaceTopology::Healthy(w) | WorkspaceTopology::Degraded(w) => w.panes,
        WorkspaceTopology::Drifted { reason } => {
            return Err(KiraMuxError::Drifted {
                project_id: project.id.clone(),
                reason,
            }
            .into());
        }
    };

    restart_managed_panes(tmux, project, &panes, agent_id)
}

/// What `kill` did, so the app layer reports without re-deriving
/// session names or re-checking existence itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KillOutcome {
    /// No session existed; nothing to do.
    AlreadyStopped,
    /// The owned session was confirmed and removed.
    Killed,
}

/// Kill the managed session. `confirm` runs only for a live session that
/// passed the ownership check, so users are never prompted about sessions
/// kira does not own; return an error from it to abort.
pub(crate) fn kill(
    tmux: &dyn TmuxAdapter,
    project: &ResolvedProject,
    confirm: impl FnOnce(&str) -> Result<()>,
) -> Result<KillOutcome> {
    let session = session_name(project);
    tracing::debug!(
        project_id = project.id.as_str(),
        session,
        "killing workspace"
    );

    if !tmux.session_exists(&session)? {
        return Ok(KillOutcome::AlreadyStopped);
    }

    inspector::ensure_session_owned(tmux, project)?;
    confirm(&project.id)?;
    if let Err(error) = tmux.kill_session(&session) {
        // The session may have died between the existence check and the
        // kill; the goal is reached either way.
        if tmux.session_exists(&session)? {
            return Err(error);
        }
    }
    Ok(KillOutcome::Killed)
}

fn attach_to_session(tmux: &dyn TmuxAdapter, session: &str) -> Result<()> {
    let result = if std::env::var_os("TMUX").is_some() {
        tmux.switch_client(session)
    } else {
        tmux.attach_session(session)
    };
    match result {
        Ok(()) => Ok(()),
        Err(error) => {
            // Interactive attach/switch only report a status code. If the
            // session vanished between ownership check and attach, surface the
            // stable SessionAbsent outcome instead of a generic exit 1.
            if !tmux.session_exists(session)? {
                return Err(KiraMuxError::SessionAbsent.into());
            }
            Err(error)
        }
    }
}

fn create(
    tmux: &dyn TmuxAdapter,
    project: &ResolvedProject,
    session: &str,
) -> Result<StartOutcome> {
    validate_launch_paths(project, project.agents.iter())?;

    let root = project.root.display().to_string();
    let window_target = window_target(session, &project.window_name);

    tmux.create_detached_session(session, &root, &project.window_name, project.agents.len())?;
    let mut guard = TopologyGuard::new(tmux, session);
    let setup = (|| -> Result<Vec<String>> {
        tmux.set_session_option(session, SESSION_PROJECT_ID, &project.id)?;
        tmux.set_session_option(session, SESSION_PROFILE_ID, &project.profile_id)?;
        tmux.set_session_option(session, SESSION_CONFIG_FINGERPRINT, &project.fingerprint)?;
        tmux.set_window_option(&window_target, WINDOW_ROLE, WINDOW_ROLE_AGENTS)?;
        tmux.set_window_option(
            &window_target,
            "remain-on-exit",
            project.remain_on_exit.as_str(),
        )?;

        // Pane ids are collected explicitly — the fresh window's single
        // seed pane, then each id split-window reports — so agent binding
        // never depends on listing order (with even-vertical applied, real
        // tmux lists panes out of creation order). The interim layout only
        // helps subsequent splits fit; the session height reserves the
        // actual room.
        let mut pane_ids: Vec<String> = tmux
            .list_panes(&window_target)?
            .into_iter()
            .map(|pane| pane.pane_id)
            .collect();
        if pane_ids.len() != 1 {
            bail!(
                "fresh session window has {} panes, expected the single seed pane",
                pane_ids.len()
            );
        }
        for _ in pane_ids.len()..project.agents.len() {
            let pane_id = tmux.split_window(&window_target, &root)?;
            tmux.select_layout(&window_target, "even-vertical")?;
            pane_ids.push(pane_id);
        }

        // Postcondition on final tmux state, not on the constructed list: a
        // seed window with extra panes or a concurrent split must roll back
        // rather than commit a partially unmanaged topology.
        let listed = tmux.list_panes(&window_target)?.len();
        if pane_ids.len() != project.agents.len() || listed != project.agents.len() {
            bail!(
                "expected {} panes after window setup, found {listed}",
                project.agents.len()
            );
        }
        for (pane_id, agent) in pane_ids.iter().zip(project.agents.iter()) {
            tmux.set_pane_option(pane_id, PANE_AGENT_ID, &agent.id)?;
        }

        apply_layout(tmux, project, &window_target)?;

        Ok(pane_ids)
    })();
    let pane_ids = match setup {
        Ok(pane_ids) => pane_ids,
        Err(error) => {
            guard.mark_failed(error.to_string());
            return Err(error);
        }
    };

    guard.commit();

    Ok(launch_all(
        tmux,
        project,
        "create",
        pane_ids
            .iter()
            .map(String::as_str)
            .zip(project.agents.iter()),
    ))
}

/// Launch every `(pane_id, agent)` target, keep going past individual
/// failures, and report one outcome — the single degraded-launch policy
/// shared by create, repair, and restart. The app layer maps the outcome
/// to the user-facing warning and exit exactly once.
fn launch_all<'a>(
    tmux: &dyn TmuxAdapter,
    project: &ResolvedProject,
    op: &'static str,
    targets: impl IntoIterator<Item = (&'a str, &'a ResolvedAgent)>,
) -> StartOutcome {
    let mut any_launch_failed = false;
    let mut respawned = Vec::new();
    for (pane_id, agent) in targets {
        match respawn_agent(tmux, pane_id, project, agent) {
            Ok(()) => respawned.push((pane_id, agent)),
            Err(error) => {
                tracing::warn!(
                    project_id = project.id.as_str(),
                    agent_id = agent.id.as_str(),
                    op,
                    %error,
                    "agent launch failed, workspace degraded"
                );
                any_launch_failed = true;
            }
        }
    }
    for (agent, error) in verify_panes_survived_launch(tmux, &respawned) {
        tracing::warn!(
            project_id = project.id.as_str(),
            agent_id = agent.id.as_str(),
            op,
            %error,
            "agent launch failed, workspace degraded"
        );
        any_launch_failed = true;
    }
    if any_launch_failed {
        StartOutcome::Degraded
    } else {
        StartOutcome::Healthy
    }
}

fn repair(
    tmux: &dyn TmuxAdapter,
    project: &ResolvedProject,
    panes: &[ManagedPane],
) -> Result<StartOutcome> {
    validate_launch_paths(
        project,
        panes
            .iter()
            .filter(|managed| managed.pane.pane_dead)
            .map(|managed| &managed.agent),
    )?;

    Ok(launch_all(
        tmux,
        project,
        "repair",
        panes
            .iter()
            .filter(|managed| managed.pane.pane_dead)
            .map(|managed| (managed.pane.pane_id.as_str(), &managed.agent)),
    ))
}

fn restart_managed_panes(
    tmux: &dyn TmuxAdapter,
    project: &ResolvedProject,
    panes: &[ManagedPane],
    agent_id: Option<&str>,
) -> Result<StartOutcome> {
    if let Some(agent_id) = agent_id {
        let managed = panes
            .iter()
            .find(|pane| pane.agent.id == agent_id)
            .ok_or_else(|| KiraMuxError::UnknownAgentId(agent_id.to_string()))?;
        validate_launch_paths(project, std::iter::once(&managed.agent))?;
        return Ok(launch_all(
            tmux,
            project,
            "restart",
            std::iter::once((managed.pane.pane_id.as_str(), &managed.agent)),
        ));
    }

    validate_launch_paths(project, panes.iter().map(|managed| &managed.agent))?;

    Ok(launch_all(
        tmux,
        project,
        "restart",
        panes
            .iter()
            .map(|managed| (managed.pane.pane_id.as_str(), &managed.agent)),
    ))
}

fn validate_launch_paths<'a>(
    project: &ResolvedProject,
    agents: impl IntoIterator<Item = &'a ResolvedAgent>,
) -> Result<()> {
    if !project.root.exists() {
        return Err(
            KiraMuxError::ConfigValidation(ConfigError::ProjectRootNotFound(project.root.clone()))
                .into(),
        );
    }
    if !project.root.is_dir() {
        return Err(
            KiraMuxError::ConfigValidation(ConfigError::ProjectRootNotDirectory(
                project.root.clone(),
            ))
            .into(),
        );
    }

    for agent in agents {
        if !agent.cwd.exists() {
            return Err(
                KiraMuxError::ConfigValidation(ConfigError::AgentCwdNotFound {
                    agent_id: agent.id.clone(),
                    path: agent.cwd.clone(),
                })
                .into(),
            );
        }
        if !agent.cwd.is_dir() {
            return Err(
                KiraMuxError::ConfigValidation(ConfigError::AgentCwdNotDirectory {
                    agent_id: agent.id.clone(),
                    path: agent.cwd.clone(),
                })
                .into(),
            );
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests;
