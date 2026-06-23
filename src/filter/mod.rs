use regex::Regex;
use std::sync::LazyLock;

/// Patterns that match common sensitive content.
/// Each pattern redacts only the secret value, preserving context where possible.
static SENSITIVE_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        // Environment variable assignments with secret-like names (handles quoted values)
        r#"(?i)((?:export\s+)?[\w]*(?:secret|password|passwd|token|api_key|apikey|auth|credential|private_key)[\w]*\s*=\s*)("[^"]*"|'[^']*'|\S+)"#,
        // Bearer tokens
        r"(?i)(bearer\s+)[a-zA-Z0-9\-._~+/]+=*",
        // AWS access keys
        r"()((?:AKIA|ASIA)[A-Z0-9]{16})",
        // Generic long hex/base64 secrets (40+ chars that look like keys)
        r#"(?i)((?:key|token|secret|password)\s*[:=]\s*)['"]?[a-zA-Z0-9+/\-_]{40,}['"]?"#,
        // Database connection strings with credentials
        r#"(?i)((?:mysql|postgres|postgresql|mongodb|redis|amqp)://\w+:)([^@\s]+)(@)"#,
        // JWT tokens (three base64 segments separated by dots)
        r"()(eyJ[a-zA-Z0-9\-_]+\.eyJ[a-zA-Z0-9\-_]+\.[a-zA-Z0-9\-_]+)",
        // Private key blocks
        r"(?i)(-----BEGIN\s+\w*\s*PRIVATE KEY-----)([\s\S]*?)(-----END\s+\w*\s*PRIVATE KEY-----)",
    ]
    .iter()
    .map(|p| Regex::new(p).unwrap())
    .collect()
});

const REDACTED: &str = "[REDACTED]";

/// Filter sensitive content from a string. Returns the filtered version.
/// Preserves the key/label portion and only replaces the secret value.
pub fn redact(input: &str) -> String {
    let mut output = input.to_string();
    for pattern in SENSITIVE_PATTERNS.iter() {
        output = pattern
            .replace_all(&output, |caps: &regex::Captures| {
                let prefix = caps.get(1).map(|m| m.as_str()).unwrap_or("");
                let suffix = caps.get(3).map(|m| m.as_str()).unwrap_or("");
                format!("{prefix}{REDACTED}{suffix}")
            })
            .to_string();
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_export_secret() {
        let input = "export SECRET_KEY=abc123def456";
        let result = redact(input);
        assert!(result.contains("SECRET_KEY="));
        assert!(result.contains(REDACTED));
        assert!(!result.contains("abc123def456"));
    }

    #[test]
    fn redacts_quoted_secret() {
        let input = r#"export PASSWORD="my secret value""#;
        let result = redact(input);
        assert!(result.contains("PASSWORD="));
        assert!(result.contains(REDACTED));
        assert!(!result.contains("my secret value"));
    }

    #[test]
    fn redacts_bearer_token() {
        let input = "Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.test";
        let result = redact(input);
        assert!(result.contains("Bearer"));
        assert!(result.contains(REDACTED));
    }

    #[test]
    fn redacts_jwt() {
        let input = "token: eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U";
        let result = redact(input);
        assert!(result.contains(REDACTED));
        assert!(!result.contains("eyJhbGci"));
    }

    #[test]
    fn redacts_database_url() {
        let input = "DATABASE_URL=postgres://admin:supersecret@localhost:5432/mydb";
        let result = redact(input);
        assert!(result.contains(REDACTED));
        assert!(!result.contains("supersecret"));
        // The URL structure must survive: scheme, user, '@', host all intact.
        assert_eq!(result, "DATABASE_URL=postgres://admin:[REDACTED]@localhost:5432/mydb");
    }

    #[test]
    fn redacts_aws_key() {
        let input = "aws_access_key_id = AKIAIOSFODNN7EXAMPLE";
        let result = redact(input);
        assert!(result.contains(REDACTED));
        assert!(!result.contains("AKIAIOSFODNN7EXAMPLE"));
    }

    #[test]
    fn leaves_normal_text() {
        let input = "ls -la /home/user";
        assert_eq!(redact(input), input);
    }

    #[test]
    fn leaves_non_secret_assignments() {
        let input = "export PATH=/usr/local/bin:/usr/bin";
        assert_eq!(redact(input), input);
    }

    #[test]
    fn redacts_private_key_block() {
        let input = "-----BEGIN RSA PRIVATE KEY-----\nMIIBogIBAAJBALRi...\n-----END RSA PRIVATE KEY-----";
        let result = redact(input);
        assert!(result.contains("-----BEGIN RSA PRIVATE KEY-----"));
        assert!(result.contains("-----END RSA PRIVATE KEY-----"));
        assert!(result.contains(REDACTED));
        assert!(!result.contains("MIIBogIBAAJBALRi"));
    }

    #[test]
    fn redacts_ec_private_key() {
        let input = "-----BEGIN EC PRIVATE KEY-----\nMHQCAQEE...\n-----END EC PRIVATE KEY-----";
        let result = redact(input);
        assert!(result.contains(REDACTED));
        assert!(!result.contains("MHQCAQEE"));
    }

    #[test]
    fn multiple_secrets_on_one_line() {
        let input = "SECRET_KEY=abc123 API_TOKEN=def456";
        let result = redact(input);
        assert!(!result.contains("abc123"));
        assert!(!result.contains("def456"));
        assert_eq!(result.matches(REDACTED).count(), 2);
    }

    #[test]
    fn preserves_unicode_content() {
        let input = "echo \"こんにちは世界\"";
        assert_eq!(redact(input), input);
    }

    #[test]
    fn unicode_around_secret() {
        let input = "# コメント\nexport SECRET_TOKEN=mysecretvalue123\n# 終わり";
        let result = redact(input);
        assert!(result.contains("コメント"));
        assert!(result.contains("終わり"));
        assert!(result.contains(REDACTED));
        assert!(!result.contains("mysecretvalue123"));
    }

    #[test]
    fn leaves_short_values_alone() {
        // "key = short" should NOT be redacted (value too short for generic pattern)
        let input = "key = short";
        assert_eq!(redact(input), input);
    }

    #[test]
    fn redacts_single_quoted_password() {
        let input = "export PASSWORD='my secret value'";
        let result = redact(input);
        assert!(result.contains(REDACTED));
        assert!(!result.contains("my secret value"));
    }

    #[test]
    fn redacts_mongodb_connection_string() {
        let input = "mongodb://root:p4ssw0rd@mongo.example.com:27017/admin";
        let result = redact(input);
        assert!(result.contains(REDACTED));
        assert!(!result.contains("p4ssw0rd"));
    }

    #[test]
    fn leaves_public_key_alone() {
        // Public keys should not be redacted
        let input = "-----BEGIN PUBLIC KEY-----\nMIIBIjANBg...\n-----END PUBLIC KEY-----";
        assert_eq!(redact(input), input);
    }
}
