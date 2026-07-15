//! Logging initialization and secret redaction helpers.

use std::sync::Once;

use tracing_subscriber::EnvFilter;

/// Initialize tracing once for the current process.
pub fn init_logging() {
    static INIT: Once = Once::new();

    INIT.call_once(|| {
        let filter = EnvFilter::try_from_env("KIRA_MUX_LOG")
            .or_else(|_| EnvFilter::try_from_default_env())
            .unwrap_or_else(|_| {
                let level = default_log_level(std::env::args());
                EnvFilter::new(level)
            });

        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(false)
            .with_writer(std::io::stderr)
            .without_time()
            .try_init();
    });
}

/// Return the default log level when neither `KIRA_MUX_LOG` nor `RUST_LOG` is
/// set by the operator. When `--json` appears in the process arguments, the
/// default drops to `error` so WARN-level tracing does not contaminate
/// machine-readable stdout for callers that merge stderr into stdout (`2>&1`).
///
/// Arguments after a `--` separator are positional values (e.g. a prompt that
/// happens to contain `--json`) and are not scanned.
fn default_log_level<I>(args: I) -> &'static str
where
    I: IntoIterator<Item = String>,
{
    if args
        .into_iter()
        .take_while(|a| a != "--")
        .any(|a| a == "--json")
    {
        "error"
    } else {
        "warn"
    }
}

/// Render an environment variable without exposing its raw value.
#[must_use]
pub fn redact_env_value(key: &str, value: &str) -> String {
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
    fn default_log_level_warn_without_json_flag() {
        let args = vec![
            "kira-mux".to_string(),
            "status".to_string(),
            "demo".to_string(),
        ];
        assert_eq!(default_log_level(args), "warn");
    }

    #[test]
    fn default_log_level_error_with_json_flag() {
        let args = vec![
            "kira-mux".to_string(),
            "status".to_string(),
            "demo".to_string(),
            "--json".to_string(),
        ];
        assert_eq!(default_log_level(args), "error");
    }

    #[test]
    fn default_log_level_error_with_json_flag_anywhere_in_args() {
        let args = vec![
            "kira-mux".to_string(),
            "status".to_string(),
            "kira".to_string(),
            "--json".to_string(),
            "--profile".to_string(),
            "pool-1".to_string(),
        ];
        assert_eq!(default_log_level(args), "error");
    }

    #[test]
    fn default_log_level_ignores_json_after_double_dash() {
        let args = vec![
            "kira-mux".to_string(),
            "send".to_string(),
            "demo".to_string(),
            "alpha".to_string(),
            "--".to_string(),
            "--json".to_string(),
        ];
        assert_eq!(default_log_level(args), "warn");
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
