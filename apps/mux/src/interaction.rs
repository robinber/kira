use std::io::{self, Write};

use anyhow::Result;

use crate::error::AiMuxError;

/// Prompt the user for kill confirmation. Returns `Ok(())` if confirmed,
/// or `Err(AiMuxError::KillAborted)` if declined.
pub(crate) fn confirm_kill(project_id: &str) -> Result<()> {
    eprint!("Kill managed tmux session for {project_id}? [y/N] ");
    io::stderr().flush()?;

    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    let normalized = answer.trim().to_ascii_lowercase();
    if normalized != "y" && normalized != "yes" {
        return Err(AiMuxError::KillAborted.into());
    }

    Ok(())
}
