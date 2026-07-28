//! Interactive prompts for destructive CLI operations (e.g. kill confirm).

use std::io::{self, BufRead, Write};

use anyhow::Result;

use crate::error::KiraMuxError;

/// Prompt the user for kill confirmation. Returns `Ok(())` if confirmed,
/// or `Err(KiraMuxError::KillAborted)` if declined.
///
/// # Errors
///
/// [`KiraMuxError::KillAborted`] when the answer is anything but `y`/`yes`
/// — including an empty read from a closed stdin (script/non-TTY), which
/// must decline rather than default to destruction.
pub(crate) fn confirm_kill(project_id: &str) -> Result<()> {
    confirm_kill_with(project_id, io::stdin().lock(), io::stderr())
}

fn confirm_kill_with(
    project_id: &str,
    mut input: impl BufRead,
    mut prompt: impl Write,
) -> Result<()> {
    write!(prompt, "Kill managed tmux session for {project_id}? [y/N] ")?;
    prompt.flush()?;

    let mut answer = String::new();
    input.read_line(&mut answer)?;
    let normalized = answer.trim().to_ascii_lowercase();
    if normalized != "y" && normalized != "yes" {
        return Err(KiraMuxError::KillAborted.into());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn confirm(input: &str) -> Result<()> {
        confirm_kill_with("demo", input.as_bytes(), Vec::new())
    }

    fn assert_aborts(input: &str) {
        let Err(error) = confirm(input) else {
            panic!("{input:?} must abort")
        };
        assert!(matches!(
            error.downcast_ref::<KiraMuxError>(),
            Some(KiraMuxError::KillAborted)
        ));
    }

    #[test]
    fn yes_answers_confirm() {
        for input in ["y\n", "Y\n", "yes\n", "YES\n"] {
            if let Err(error) = confirm(input) {
                panic!("{input:?} must confirm, got: {error}");
            }
        }
    }

    #[test]
    fn anything_else_aborts() {
        for input in ["n\n", "no\n", "maybe\n", "\n"] {
            assert_aborts(input);
        }
    }

    #[test]
    fn closed_stdin_declines_instead_of_defaulting_to_destruction() {
        // Non-TTY / script contract: an empty read (EOF) is a decline.
        assert_aborts("");
    }
}
