//! Shared resolved-project fixtures and managed-session setup.

use std::collections::BTreeMap;
use std::path::PathBuf;

use super::FakeTmux;
use crate::config::{AgentMode, Layout, RemainOnExit};
use crate::model::{ResolvedAgent, ResolvedProject};
use crate::tmux::metadata::{
    PANE_AGENT_ID, SESSION_CONFIG_FINGERPRINT, SESSION_PROFILE_ID, SESSION_PROJECT_ID, WINDOW_ROLE,
    WINDOW_ROLE_AGENTS,
};

/// Minimal direct-mode agent (`echo` in the test-project root, label
/// capitalized from the id). Mutate the returned value for the one
/// deviation a test needs instead of spelling every field — new
/// `ResolvedAgent` fields then only touch this builder.
pub(crate) fn test_agent(id: &str) -> ResolvedAgent {
    let mut chars = id.chars();
    let label = match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    };
    ResolvedAgent {
        id: id.to_string(),
        label,
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
        busy_markers: None,
    }
}

pub(crate) fn test_project() -> ResolvedProject {
    ResolvedProject {
        id: "test".to_string(),
        profile_id: crate::config::DEFAULT_PROFILE_ID.to_string(),
        name: "Test".to_string(),
        root: PathBuf::from("/tmp/test-project"),
        layout: Layout::Auto,
        main_pane_ratio: 50,
        window_name: "agents".to_string(),
        session_prefix: "kira".to_string(),
        default_shell: "/bin/sh".to_string(),
        remain_on_exit: RemainOnExit::Failed,
        tmux_bin: "tmux".to_string(),
        agents: vec![test_agent("alpha"), test_agent("beta")],
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
