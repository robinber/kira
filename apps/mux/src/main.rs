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
    use kira_mux::KiraMuxError;
    use kira_mux::config::ConfigError;

    if error.downcast_ref::<ConfigError>().is_some() {
        return ExitCode::from(2);
    }

    match error.downcast_ref::<KiraMuxError>() {
        Some(
            KiraMuxError::UnknownAgentId(_)
            | KiraMuxError::UnknownGroupName(_)
            | KiraMuxError::ConfigValidation(_)
            | KiraMuxError::KillAborted,
        ) => ExitCode::from(2),
        Some(KiraMuxError::MissingDependency(_)) => ExitCode::from(3),
        Some(KiraMuxError::Drifted { .. }) => ExitCode::from(4),
        Some(KiraMuxError::SessionAbsent) => ExitCode::from(5),
        // Dead pane and post-launch degradation both mean the workspace exists
        // but is not fully healthy — scripts can treat exit 6 as "repair me".
        Some(KiraMuxError::DeadPane(_) | KiraMuxError::Degraded(_)) => ExitCode::from(6),
        None => ExitCode::FAILURE,
    }
}

#[cfg(test)]
mod tests {
    use std::process::ExitCode;

    use kira_mux::KiraMuxError;

    use super::exit_code_for_error;

    #[test]
    fn degraded_maps_to_exit_code_6() {
        let err = anyhow::Error::new(KiraMuxError::Degraded("demo".into()));
        assert_eq!(exit_code_for_error(&err), ExitCode::from(6));
    }

    #[test]
    fn drifted_maps_to_exit_code_4() {
        let err = anyhow::Error::new(KiraMuxError::Drifted {
            project_id: "demo".into(),
            reason: kira_mux::WorkspaceDriftReason::FingerprintMismatch,
        });
        assert_eq!(exit_code_for_error(&err), ExitCode::from(4));
    }

    #[test]
    fn dead_pane_maps_to_exit_code_6() {
        let err = anyhow::Error::new(KiraMuxError::DeadPane("alpha".into()));
        assert_eq!(exit_code_for_error(&err), ExitCode::from(6));
    }
}
