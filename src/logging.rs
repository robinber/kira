//! Logging initialization and secret redaction helpers.

use std::sync::Once;

use tracing_subscriber::EnvFilter;

/// Initialize tracing once for the current process.
///
/// `json_stdout` comes from the parsed CLI (see `Cli::wants_json`): when
/// the invocation prints machine-readable JSON, the default level drops to
/// `error` so WARN-level tracing cannot contaminate `2>&1` pipelines.
pub fn init_logging(json_stdout: bool) {
    static INIT: Once = Once::new();

    INIT.call_once(|| {
        let filter = EnvFilter::try_from_env("KIRA_MUX_LOG")
            .or_else(|_| EnvFilter::try_from_default_env())
            .unwrap_or_else(|_| EnvFilter::new(default_log_level(json_stdout)));

        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(false)
            .with_writer(std::io::stderr)
            .without_time()
            .try_init();
    });
}

/// Default level when neither `KIRA_MUX_LOG` nor `RUST_LOG` is set: `warn`
/// for humans, `error` for JSON invocations.
fn default_log_level(json_stdout: bool) -> &'static str {
    if json_stdout { "error" } else { "warn" }
}

/// Render an environment variable without exposing its raw value.
#[must_use]
pub(crate) fn redact_env_value(key: &str, value: &str) -> String {
    if value.is_empty() {
        format!("{key}=<empty>")
    } else {
        format!("{key}=<redacted:{} chars>", value.chars().count())
    }
}

#[cfg(test)]
mod tests {
    use super::{default_log_level, redact_env_value};

    #[test]
    fn default_log_level_matches_output_mode() {
        assert_eq!(default_log_level(false), "warn");
        assert_eq!(default_log_level(true), "error");
    }

    #[test]
    fn redact_env_value_hides_non_empty_values() {
        assert_eq!(
            redact_env_value("API_KEY", "sk-test"),
            "API_KEY=<redacted:7 chars>"
        );
        assert_eq!(redact_env_value("TOKEN", ""), "TOKEN=<empty>");
    }
}
