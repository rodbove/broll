use regex::Regex;
use std::sync::LazyLock;

/// Patterns that match common sensitive content.
static SENSITIVE_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        // Environment variable assignments with secret-like names
        r"(?i)(export\s+)?([\w]*(?:secret|password|passwd|token|api_key|apikey|auth|credential|private_key)[\w]*)\s*=\s*\S+",
        // Bearer tokens
        r"(?i)bearer\s+[a-zA-Z0-9\-._~+/]+=*",
        // AWS keys
        r"(?:AKIA|ASIA)[A-Z0-9]{16}",
        // Generic long hex/base64 secrets (40+ chars that look like keys)
        r#"(?i)(?:key|token|secret|password)\s*[:=]\s*['"]?[a-zA-Z0-9+/\-_]{40,}['"]?"#,
    ]
    .iter()
    .map(|p| Regex::new(p).unwrap())
    .collect()
});

const REDACTED: &str = "[REDACTED]";

/// Filter sensitive content from a string. Returns the filtered version.
pub fn redact(input: &str) -> String {
    let mut output = input.to_string();
    for pattern in SENSITIVE_PATTERNS.iter() {
        output = pattern.replace_all(&output, REDACTED).to_string();
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_export_secret() {
        let input = "export SECRET_KEY=abc123def456";
        assert_eq!(redact(input), REDACTED);
    }

    #[test]
    fn redacts_bearer_token() {
        let input = "Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.test";
        assert!(redact(input).contains(REDACTED));
    }

    #[test]
    fn leaves_normal_text() {
        let input = "ls -la /home/user";
        assert_eq!(redact(input), input);
    }
}
