//! Small shared helpers used across modules.

/// Return the final path segment of a command string.
///
/// Only `/` is treated as a separator, matching configured agent commands
/// (Unix-style paths, including absolute binary paths).
pub(crate) fn command_basename(cmd: &str) -> &str {
    cmd.rsplit('/').next().unwrap_or(cmd)
}

#[cfg(test)]
mod tests {
    use super::command_basename;

    #[test]
    fn command_basename_strips_directory_prefix() {
        assert_eq!(command_basename("/usr/bin/codex"), "codex");
        assert_eq!(command_basename("bin/claude"), "claude");
    }

    #[test]
    fn command_basename_leaves_bare_name() {
        assert_eq!(command_basename("opencode"), "opencode");
        assert_eq!(command_basename(""), "");
    }
}
