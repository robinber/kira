use sha2::{Digest, Sha256};

use crate::domain::ResolvedProject;

/// Derive the managed tmux session name for a resolved project/profile pair.
#[must_use]
pub(crate) fn session_name(project: &ResolvedProject) -> String {
    let digest = Sha256::digest(project.root.display().to_string().as_bytes());
    let hex = hex::encode(digest);
    format!(
        "{}-{}-{}-{}",
        project.session_prefix,
        project.id,
        project.profile_id,
        &hex[..8]
    )
}

pub(crate) fn window_target(session: &str, window_name: &str) -> String {
    format!("{session}:{window_name}")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{session_name, window_target};
    use crate::test_support::{TestOptionExt as _, test_project};

    #[test]
    fn session_name_uses_prefix_id_profile_and_root_hash_prefix() {
        // `test_project()` has prefix="ai", id="test", profile="default" and
        // root="/tmp/test-project". sha256("/tmp/test-project") starts with
        // "b71e47fd", and the session name uses only the first 8 hex chars.
        let project = test_project();

        assert_eq!(session_name(&project), "ai-test-default-b71e47fd");
    }

    #[test]
    fn session_name_hash_suffix_differs_for_different_roots() {
        let mut a = test_project();
        let mut b = test_project();
        a.root = PathBuf::from("/tmp/project-a");
        b.root = PathBuf::from("/tmp/project-b");

        let name_a = session_name(&a);
        let name_b = session_name(&b);
        assert_ne!(name_a, name_b);

        // Only the trailing 8-char hash segment can differ: prefix, id and
        // profile are identical across the two projects.
        let suffix_a = name_a.rsplit('-').next().or_panic();
        let suffix_b = name_b.rsplit('-').next().or_panic();
        assert_eq!(suffix_a.len(), 8);
        assert_eq!(suffix_b.len(), 8);
        assert_ne!(suffix_a, suffix_b);
    }

    #[test]
    fn window_target_joins_session_and_window_with_a_colon() {
        assert_eq!(
            window_target("ai-test-default-b71e47fd", "agents"),
            "ai-test-default-b71e47fd:agents"
        );
    }

    #[test]
    fn window_target_uses_the_projects_session_and_window_name() {
        let project = test_project();
        let session = session_name(&project);

        assert_eq!(
            window_target(&session, &project.window_name),
            "ai-test-default-b71e47fd:agents"
        );
    }
}
