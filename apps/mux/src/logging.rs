//! Logging initialization and secret redaction helpers.

use std::sync::Once;

use tracing_subscriber::EnvFilter;

/// Initialize tracing once for the current process.
pub fn init_logging() {
    static INIT: Once = Once::new();

    INIT.call_once(|| {
        let filter = EnvFilter::try_from_env("KIRA_MUX_LOG")
            .or_else(|_| EnvFilter::try_from_env("AI_MUX_LOG"))
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

/// Return the default log level when neither `KIRA_MUX_LOG`, legacy
/// `AI_MUX_LOG`, nor `RUST_LOG` is set by the operator. When `--json` appears
/// in the process arguments, the default drops to `error` so WARN-level tracing
/// does not contaminate machine-readable stdout for callers that merge stderr
/// into stdout (`2>&1`).
fn default_log_level<I>(args: I) -> &'static str
where
    I: IntoIterator<Item = String>,
{
    if args.into_iter().any(|a| a == "--json") {
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

/// Redact obvious secrets before pane capture content is persisted.
#[must_use]
pub fn redact_persisted_capture_content(content: &str) -> String {
    redact_url_like_values(&redact_sensitive_assignments(content))
}

fn redact_sensitive_assignments(text: &str) -> String {
    let chars = text.chars().collect::<Vec<_>>();
    let mut output = String::with_capacity(text.len());
    let mut index = 0;
    while index < chars.len() {
        if matches!(chars[index], '=' | ':')
            && let Some(key) = key_before_separator(&chars, index)
            && is_sensitive_key(&key)
        {
            output.push(chars[index]);
            index += 1;
            while index < chars.len() && matches!(chars[index], ' ' | '\t') {
                output.push(chars[index]);
                index += 1;
            }
            index = redact_value(&chars, index, &mut output);
            continue;
        }
        output.push(chars[index]);
        index += 1;
    }
    output
}

fn key_before_separator(chars: &[char], separator_index: usize) -> Option<String> {
    let mut key_end = separator_index;
    while key_end > 0 && chars[key_end - 1].is_whitespace() {
        key_end -= 1;
    }
    if key_end > 0 && matches!(chars[key_end - 1], '"' | '\'') {
        key_end -= 1;
    }
    while key_end > 0 && chars[key_end - 1].is_whitespace() {
        key_end -= 1;
    }
    let mut key_start = key_end;
    while key_start > 0 && is_key_char(chars[key_start - 1]) {
        key_start -= 1;
    }
    (key_start < key_end).then(|| chars[key_start..key_end].iter().collect())
}

fn is_sensitive_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    lower == "key"
        || lower.contains("database_url")
        || lower.contains("api_key")
        || lower.contains("token")
        || lower.contains("secret")
        || lower.contains("password")
        || lower.ends_with("_key")
        || lower.ends_with("-key")
}

fn redact_value(chars: &[char], mut index: usize, output: &mut String) -> usize {
    if let Some(quote @ ('\'' | '"')) = chars.get(index).copied() {
        output.push(quote);
        output.push_str("<redacted>");
        index += 1;
        while index < chars.len() {
            let current = chars[index];
            index += 1;
            if current == quote {
                output.push(quote);
                break;
            }
        }
        return index;
    }
    output.push_str("<redacted>");
    while index < chars.len() && !is_unquoted_value_delimiter(chars[index]) {
        index += 1;
    }
    index
}

fn redact_url_like_values(text: &str) -> String {
    let chars = text.chars().collect::<Vec<_>>();
    let mut output = String::with_capacity(text.len());
    let mut index = 0;
    while index < chars.len() {
        if url_scheme_at(&chars, index) {
            output.push_str("<redacted:url>");
            index += 1;
            while index < chars.len() && !is_url_delimiter(chars[index]) {
                index += 1;
            }
        } else {
            output.push(chars[index]);
            index += 1;
        }
    }
    output
}

fn url_scheme_at(chars: &[char], index: usize) -> bool {
    if !chars.get(index).is_some_and(char::is_ascii_alphabetic) {
        return false;
    }
    let mut cursor = index + 1;
    while cursor < chars.len()
        && (chars[cursor].is_ascii_alphanumeric() || matches!(chars[cursor], '+' | '-' | '.'))
    {
        cursor += 1;
    }
    cursor + 2 < chars.len()
        && chars[cursor] == ':'
        && chars[cursor + 1] == '/'
        && chars[cursor + 2] == '/'
}

fn is_key_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-')
}

fn is_unquoted_value_delimiter(ch: char) -> bool {
    ch.is_whitespace() || matches!(ch, ',' | ';')
}

fn is_url_delimiter(ch: char) -> bool {
    ch.is_whitespace() || matches!(ch, '"' | '\'' | ',' | ';' | ')' | ']' | '}' | '<' | '>')
}

#[cfg(test)]
mod tests {
    use super::{default_log_level, redact_persisted_capture_content};

    #[test]
    fn default_log_level_warn_without_json_flag() {
        let args = vec![
            "kira-mux".to_string(),
            "query".to_string(),
            "messages".to_string(),
            "kira".to_string(),
        ];
        assert_eq!(default_log_level(args), "warn");
    }

    #[test]
    fn default_log_level_error_with_json_flag() {
        let args = vec![
            "kira-mux".to_string(),
            "query".to_string(),
            "messages".to_string(),
            "kira".to_string(),
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
    fn persisted_capture_redacts_secret_assignments_and_urls() {
        let input = "DATABASE_URL=postgres://user:pass@db.local/app\nAPI_KEY=\"sk-test\"\nsee https://example.invalid/path\nplain";

        let redacted = redact_persisted_capture_content(input);

        assert_eq!(
            redacted,
            "DATABASE_URL=<redacted>\nAPI_KEY=\"<redacted>\"\nsee <redacted:url>\nplain"
        );
    }

    #[test]
    fn persisted_capture_preserves_non_sensitive_text() {
        let input = "status: ok\nplain output\n";

        assert_eq!(redact_persisted_capture_content(input), input);
    }
}
