use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Serialize;

use crate::config::{AgentMode, Layout, RemainOnExit};

/// Fully resolved project configuration ready for tmux workspace management.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedProject {
    /// Stable project ID from config.
    pub id: String,
    /// Active profile ID.
    pub profile_id: String,
    /// Human-friendly project name.
    pub name: String,
    /// Canonical project root directory.
    pub root: PathBuf,
    /// Requested workspace layout.
    pub layout: Layout,
    /// Primary-pane ratio for supported layouts.
    pub main_pane_ratio: u8,
    /// Managed tmux window name.
    pub window_name: String,
    /// Prefix used when deriving the tmux session name.
    pub session_prefix: String,
    /// Default shell for shell-mode agents.
    pub default_shell: String,
    /// Pane exit retention policy.
    pub remain_on_exit: RemainOnExit,
    /// tmux executable name or path.
    pub tmux_bin: String,
    /// Fully resolved agent definitions.
    pub agents: Vec<ResolvedAgent>,
    /// Deterministic fingerprint of the resolved project contract.
    pub fingerprint: String,
    /// Named agent groups keyed by group name.
    pub groups: BTreeMap<String, Vec<String>>,
}

impl ResolvedProject {
    /// Returns the groups that contain the given agent.
    #[must_use]
    pub fn groups_for(&self, agent_id: &str) -> Vec<String> {
        self.groups
            .iter()
            .filter(|(_, members)| members.iter().any(|m| m == agent_id))
            .map(|(name, _)| name.clone())
            .collect()
    }
}

/// Fully resolved agent configuration ready for pane creation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedAgent {
    /// Stable agent ID from config.
    pub id: String,
    /// Human-friendly agent label.
    pub label: String,
    /// Launch mode used for the pane.
    pub mode: AgentMode,
    /// Direct command, when running in direct mode.
    pub command: Option<String>,
    /// Shell command, when running in shell mode.
    pub shell_command: Option<String>,
    /// Extra command-line arguments.
    pub args: Vec<String>,
    /// Working directory for the launched process.
    pub cwd: PathBuf,
    /// Resolved environment overrides.
    pub env: BTreeMap<String, String>,
    /// Declared agent capabilities.
    pub capabilities: Vec<String>,
    /// Optional prompt template applied before send.
    pub prompt_template: Option<String>,
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::test_support::test_project;

    fn project_with_groups(groups: BTreeMap<String, Vec<String>>) -> super::ResolvedProject {
        let mut project = test_project();
        project.groups = groups;
        project
    }

    #[test]
    fn groups_for_returns_every_group_containing_the_agent_sorted_by_name() {
        let mut groups = BTreeMap::new();
        groups.insert(
            "backend".to_owned(),
            vec!["alpha".to_owned(), "beta".to_owned()],
        );
        groups.insert("reviewers".to_owned(), vec!["alpha".to_owned()]);
        groups.insert("ops".to_owned(), vec!["beta".to_owned()]);
        let project = project_with_groups(groups);

        // BTreeMap iteration is key-ordered, so membership is returned sorted
        // by group name: "alpha" is in "backend" and "reviewers".
        assert_eq!(
            project.groups_for("alpha"),
            vec!["backend".to_owned(), "reviewers".to_owned()]
        );
        // "beta" is in "backend" and "ops".
        assert_eq!(
            project.groups_for("beta"),
            vec!["backend".to_owned(), "ops".to_owned()]
        );
    }

    #[test]
    fn groups_for_returns_empty_when_agent_is_in_no_group() {
        let mut groups = BTreeMap::new();
        groups.insert("backend".to_owned(), vec!["alpha".to_owned()]);
        let project = project_with_groups(groups);

        assert!(project.groups_for("gamma").is_empty());
    }

    #[test]
    fn groups_for_returns_empty_for_project_without_groups() {
        // `test_project()` has an empty `groups` map by default.
        let project = test_project();

        assert!(project.groups_for("alpha").is_empty());
    }
}
