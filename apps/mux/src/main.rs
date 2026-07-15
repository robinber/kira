//! Binary entrypoint for the `kira-mux` CLI.
//!
//! Initializes logging, delegates to the library, and maps typed errors to
//! process exit codes.
#![allow(
    unused_crate_dependencies,
    reason = "thin binary delegates dependency use to the kira_mux library target"
)]

use std::process::ExitCode;

fn main() -> ExitCode {
    kira_mux::logging::init_logging();

    match kira_mux::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::debug!("application error: {error:?}");
            eprintln!("{error}");
            exit_code_for_error(&error)
        }
    }
}

fn exit_code_for_error(error: &anyhow::Error) -> ExitCode {
    use kira_mux::AiMuxError;
    use kira_mux::config::ConfigError;

    if error.downcast_ref::<ConfigError>().is_some() {
        return ExitCode::from(2);
    }

    match error.downcast_ref::<AiMuxError>() {
        Some(
            AiMuxError::UnknownProjectId(_)
            | AiMuxError::UnknownAgentId(_)
            | AiMuxError::UnknownGroupName(_)
            | AiMuxError::MissingArgument(_)
            | AiMuxError::InvalidOrchestratorProfile { .. }
            | AiMuxError::OrchestratorAgentMismatch { .. }
            | AiMuxError::AgentNotOrchestrator { .. }
            | AiMuxError::OrchestratorShellModeUnsupported { .. }
            | AiMuxError::OrchestratorPaneActive { .. }
            | AiMuxError::ConfigValidation(_)
            | AiMuxError::KillAborted,
        ) => ExitCode::from(2),
        Some(AiMuxError::MissingDependency(_)) => ExitCode::from(3),
        Some(AiMuxError::Drifted { .. }) => ExitCode::from(4),
        Some(AiMuxError::SessionAbsent) => ExitCode::from(5),
        Some(AiMuxError::Degraded(_)) => ExitCode::from(6),
        None => ExitCode::FAILURE,
    }
}

#[cfg(test)]
mod tests {
    use std::process::ExitCode;

    use kira_mux::AiMuxError;

    use super::exit_code_for_error;

    #[test]
    fn degraded_maps_to_exit_code_6() {
        let err = anyhow::Error::new(AiMuxError::Degraded("demo".into()));
        assert_eq!(exit_code_for_error(&err), ExitCode::from(6));
    }

    #[test]
    fn drifted_maps_to_exit_code_4() {
        let err = anyhow::Error::new(AiMuxError::Drifted {
            project_id: "demo".into(),
            reason: kira_mux::WorkspaceDriftReason::FingerprintMismatch,
        });
        assert_eq!(exit_code_for_error(&err), ExitCode::from(4));
    }
}
