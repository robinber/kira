use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, bail};

const ENV_WRAPPER_SHELL: &str = "/bin/sh";
const ENV_WRAPPER_ARG0: &str = "kira-mux-env";
const ENV_WRAPPER_SCRIPT: &str =
    r#"env_file=$1; shift; . "$env_file" || exit $?; rm -f "$env_file" || exit $?; exec "$@""#;

static ENV_FILE_SEQ: AtomicU64 = AtomicU64::new(0);

/// A 0600 temp file carrying env values for one pane launch.
///
/// The file is deleted on drop unless [`ShellEnvFile::defuse`] was called,
/// which hands deletion ownership to the pane wrapper script.
pub(super) struct ShellEnvFile {
    path: PathBuf,
    handed_off: bool,
}

impl ShellEnvFile {
    pub(super) fn create(env_pairs: &[(String, String)]) -> Result<Option<Self>> {
        if env_pairs.is_empty() {
            return Ok(None);
        }

        Self::create_in(env_pairs, &env::temp_dir()).map(Some)
    }

    fn create_in(env_pairs: &[(String, String)], dir: &Path) -> Result<Self> {
        let contents = shell_env_file_contents(env_pairs);
        for _ in 0..100 {
            let seq = ENV_FILE_SEQ.fetch_add(1, Ordering::Relaxed);
            let path = dir.join(format!("kira-mux-env-{}-{seq}.sh", std::process::id()));
            let mut file = match OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
            {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("failed to create tmux env file at {}", path.display())
                    });
                }
            };

            if let Err(error) = fs::set_permissions(&path, fs::Permissions::from_mode(0o600)) {
                let _ = fs::remove_file(&path);
                return Err(error).with_context(|| {
                    format!("failed to restrict tmux env file at {}", path.display())
                });
            }

            if let Err(error) = file.write_all(contents.as_bytes()) {
                let _ = fs::remove_file(&path);
                return Err(error).with_context(|| {
                    format!("failed to write tmux env file at {}", path.display())
                });
            }

            if let Err(error) = file.flush() {
                let _ = fs::remove_file(&path);
                return Err(error).with_context(|| {
                    format!("failed to flush tmux env file at {}", path.display())
                });
            }

            return Ok(Self {
                path,
                handed_off: false,
            });
        }

        bail!(
            "failed to create a unique tmux env file in {}",
            dir.display()
        );
    }

    pub(super) fn path_arg(&self) -> Result<String> {
        self.path
            .to_str()
            .map(ToString::to_string)
            .ok_or_else(|| anyhow::anyhow!("tmux env file path is not valid UTF-8"))
    }

    /// Hand deletion ownership to the pane wrapper script; drop no longer
    /// removes the file.
    pub(super) fn defuse(&mut self) {
        self.handed_off = true;
    }
}

impl Drop for ShellEnvFile {
    fn drop(&mut self) {
        if self.handed_off {
            return;
        }
        match fs::remove_file(&self.path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(
                    path = %self.path.display(),
                    %error,
                    "failed to remove tmux env file after launch failure"
                );
            }
        }
    }
}

pub(super) fn respawn_pane_args(
    target: &str,
    start_directory: &str,
    env_file_path: Option<&str>,
    command: &[String],
) -> Vec<String> {
    let mut args = vec![
        "respawn-pane".to_string(),
        "-k".to_string(),
        "-t".to_string(),
        target.to_string(),
        "-c".to_string(),
        start_directory.to_string(),
    ];
    match env_file_path {
        Some(path) => args.extend(wrap_command_with_env_file(path, command)),
        None => args.extend(command.iter().cloned()),
    }
    args
}

fn wrap_command_with_env_file(env_file_path: &str, command: &[String]) -> Vec<String> {
    let mut wrapped = Vec::with_capacity(command.len() + 5);
    wrapped.push(ENV_WRAPPER_SHELL.to_string());
    wrapped.push("-c".to_string());
    wrapped.push(ENV_WRAPPER_SCRIPT.to_string());
    wrapped.push(ENV_WRAPPER_ARG0.to_string());
    wrapped.push(env_file_path.to_string());
    wrapped.extend(command.iter().cloned());
    wrapped
}

fn shell_env_file_contents(env_pairs: &[(String, String)]) -> String {
    let mut contents = String::from("# sourced by kira-mux pane launch wrapper\n");
    for (key, value) in env_pairs {
        contents.push_str("export ");
        contents.push_str(&shell_quote(&format!("{key}={value}")));
        contents.push('\n');
    }
    contents
}

fn shell_quote(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('\'');
    for ch in value.chars() {
        if ch == '\'' {
            quoted.push_str("'\\''");
        } else {
            quoted.push(ch);
        }
    }
    quoted.push('\'');
    quoted
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use super::{
        ShellEnvFile, respawn_pane_args, shell_env_file_contents, wrap_command_with_env_file,
    };
    use crate::test_support::TestResultExt;

    #[test]
    fn env_file_wrapper_argv_never_contains_resolved_secret_values() {
        let secret = "super-secret-token";
        let temp = tempfile::tempdir().or_panic();
        let env_file = ShellEnvFile::create_in(
            &[("KIRA_TEST_TOKEN".to_string(), secret.to_string())],
            temp.path(),
        )
        .or_panic();
        let env_file_path = env_file.path_arg().or_panic();
        let args = wrap_command_with_env_file(
            &env_file_path,
            &[
                "kira-mux".to_string(),
                "status".to_string(),
                "demo".to_string(),
            ],
        );

        assert!(
            args.iter().all(|arg| !arg.contains(secret)),
            "resolved secret must not be process argv: {args:?}"
        );
        assert!(
            fs::read_to_string(&env_file.path)
                .or_panic()
                .contains(secret),
            "env file should carry the secret for the pane wrapper"
        );
    }

    #[test]
    fn respawn_pane_argv_uses_env_file_path_without_env_flags_or_values() {
        let secret = "super-secret-token";
        let temp = tempfile::tempdir().or_panic();
        let env_file = ShellEnvFile::create_in(
            &[("KIRA_TEST_TOKEN".to_string(), secret.to_string())],
            temp.path(),
        )
        .or_panic();
        let env_file_path = env_file.path_arg().or_panic();
        let args = respawn_pane_args(
            "%0",
            "/tmp/project",
            Some(&env_file_path),
            &["kira-mux".to_string(), "status".to_string()],
        );

        assert!(
            args.iter().all(|arg| arg != "-e" && !arg.contains(secret)),
            "respawn argv must not expose env values: {args:?}"
        );
        assert!(
            args.iter().any(|arg| arg == &env_file_path),
            "respawn argv should include only the env file path for env delivery: {args:?}"
        );
    }

    #[test]
    fn env_file_uses_owner_only_permissions() {
        let temp = tempfile::tempdir().or_panic();
        let env_file = ShellEnvFile::create_in(
            &[("KIRA_TEST_TOKEN".to_string(), "value".to_string())],
            temp.path(),
        )
        .or_panic();
        let metadata = fs::metadata(&env_file.path).or_panic();

        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn env_file_removed_on_drop_unless_defused() {
        let temp = tempfile::tempdir().or_panic();
        let env_file = ShellEnvFile::create_in(
            &[("KIRA_TEST_TOKEN".to_string(), "value".to_string())],
            temp.path(),
        )
        .or_panic();
        let path = env_file.path.clone();
        assert!(path.exists());

        drop(env_file);
        assert!(!path.exists(), "dropped env file must be deleted");
    }

    #[test]
    fn defused_env_file_survives_drop() {
        let temp = tempfile::tempdir().or_panic();
        let mut env_file = ShellEnvFile::create_in(
            &[("KIRA_TEST_TOKEN".to_string(), "value".to_string())],
            temp.path(),
        )
        .or_panic();
        let path = env_file.path.clone();
        env_file.defuse();

        drop(env_file);
        assert!(path.exists(), "defused env file is owned by the wrapper");
    }

    #[test]
    fn env_file_contents_exports_values_with_shell_quoting() {
        let contents = shell_env_file_contents(&[
            ("KIRA_TEST_TOKEN".to_string(), "value:pa'ss".to_string()),
            ("KIRA_MODE".to_string(), "worker pool".to_string()),
        ]);

        assert!(contents.contains("export 'KIRA_TEST_TOKEN=value:pa'\\''ss'"));
        assert!(contents.contains("export 'KIRA_MODE=worker pool'"));
    }
}
