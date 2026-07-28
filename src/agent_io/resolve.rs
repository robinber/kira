//! Resolve a live managed pane for send / capture under topology rules.
//!
//! Shares the inspector drift contract: a healthy workspace yields a pane
//! id, drifted/absent/dead targets become typed domain errors.

use anyhow::Result;

use crate::error::{KiraMuxError, WorkspaceDriftReason};
use crate::inspector::{self, WorkspaceTopology};
use crate::model::{ResolvedAgent, ResolvedProject};
use crate::tmux::{PaneInfo, TmuxAdapter, TmuxError};

/// Map a vanished-target failure (killed pane/window/session or stopped
/// server) to the caller's typed meaning; other errors pass through. The
/// single `is_target_unavailable` seam every pane-addressed op shares.
pub(super) fn or_unavailable<T>(
    result: Result<T>,
    on_unavailable: impl FnOnce() -> Result<T>,
) -> Result<T> {
    match result {
        Err(error) if TmuxError::is_target_unavailable(&error) => on_unavailable(),
        other => other,
    }
}

/// Send/capture-side classification, shared by every pane-addressed op: a
/// target that vanished mid-operation is the typed
/// [`KiraMuxError::DeadPane`] (exit 6), not an untyped transport failure.
pub(super) fn or_dead_pane<T>(agent_id: &str, result: Result<T>) -> Result<T> {
    or_unavailable(result, || {
        Err(KiraMuxError::DeadPane(agent_id.to_string()).into())
    })
}

/// `list_panes` then find by id — the shared list-then-find idiom for
/// re-reading one pane's state.
pub(super) fn find_pane(tmux: &dyn TmuxAdapter, pane_id: &str) -> Result<Option<PaneInfo>> {
    Ok(tmux
        .list_panes(pane_id)?
        .into_iter()
        .find(|pane| pane.pane_id == pane_id))
}

/// Resolve the live managed pane for `agent_id` under the **same topology
/// contract** as [`inspector::inspect`].
///
/// Healthy and degraded workspaces both resolve: dead panes are returned so
/// callers can decide whether the operation is allowed (`send` rejects them;
/// `capture` allows them). Drifted and absent sessions fail with typed
/// [`KiraMuxError`] variants.
///
/// The inspected topology is returned alongside the pane so callers can
/// reuse it (e.g. for the prompt context) instead of running a second
/// inspection that could observe a different workspace state.
pub(super) fn resolve_managed_pane<'a>(
    tmux: &dyn TmuxAdapter,
    project: &'a ResolvedProject,
    agent_id: &str,
) -> Result<(PaneInfo, &'a ResolvedAgent, WorkspaceTopology)> {
    let agent = project
        .agents
        .iter()
        .find(|a| a.id == agent_id)
        .ok_or_else(|| KiraMuxError::UnknownAgentId(agent_id.to_string()))?;

    let topology = inspector::inspect(tmux, project)?;
    let pane = match &topology {
        WorkspaceTopology::Absent => return Err(KiraMuxError::SessionAbsent.into()),
        WorkspaceTopology::Drifted { reason } => {
            return Err(KiraMuxError::Drifted {
                project_id: project.id.clone(),
                reason: reason.clone(),
            }
            .into());
        }
        WorkspaceTopology::Healthy(workspace) | WorkspaceTopology::Degraded(workspace) => {
            // inspect() pairs every configured agent when topology is live;
            // MissingManagedPane is a defensive fallback only.
            workspace
                .panes
                .iter()
                .find(|mp| mp.agent.id == agent_id)
                .map(|mp| mp.pane.clone())
                .ok_or_else(|| KiraMuxError::Drifted {
                    project_id: project.id.clone(),
                    reason: WorkspaceDriftReason::MissingManagedPane(agent_id.to_string()),
                })?
        }
    };

    tracing::debug!(
        agent = agent_id,
        pane = %pane.pane_id,
        pane_dead = pane.pane_dead,
        "managed pane resolved"
    );
    Ok((pane, agent, topology))
}

#[cfg(test)]
mod tests;
