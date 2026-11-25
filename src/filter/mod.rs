use regex::Regex;
use std::sync::LazyLock;

struct Rule {
    re: Regex,
    named: bool,
}

const NAMED: &[&str] = &[
    r#"(?i)((?:export\s+)?([\w]*(?:secret|password|passwd|token|api_key|apikey|auth|credential|private_key)[\w]*)\s*=\s*)("[^"]*"|'[^']*'|\S+)"#,
    r#"(?im)((?:^|\s)(--?[\w-]*(?:secret|password|passwd|token|api-key|api_key|apikey|auth|credential|private-key)[\w-]*)(?:[ \t]+|=))('[^']*'|"[^"]*"|[^\s'"-][^\s'"]*)"#,
    r#"(?i)("([\w.-]*(?:secret|password|passwd|token|api_key|apikey|api-key|credential|private_key)[\w.-]*)"\s*:\s*)("[^"]*")"#,
    r#"(?im)(^[ \t]*-?[ \t]*([\w.-]*(?:secret|password|passwd|token|api_key|apikey|api-key|credential|private_key)[\w.-]*)[ \t]*:[ \t]+)('[^']*'|"[^"]*"|\S+)"#,
];

const UNNAMED: &[&str] = &[
    r"(?i)(bearer\s+)[a-zA-Z0-9\-._~+/]{12,}=*",
    r"(?i)(authorization:\s*basic\s+)[a-zA-Z0-9+/]+=*",
    r"()(\b(?:AKIA|ASIA)[A-Z0-9]{16}\b)",
    r"()(\b(?:gh[pousr]_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{40,}|glpat-[A-Za-z0-9_-]{20,}|sk-(?:ant-|proj-)?[A-Za-z0-9_-]{20,}|xox[abprs]-[A-Za-z0-9-]{10,}|[sr]k_(?:live|test)_[A-Za-z0-9]{16,}|AIza[0-9A-Za-z_-]{35}|npm_[A-Za-z0-9]{36})\b)",
    r#"(?i)((?:key|token|secret|password)\s*[:=]\s*)['"]?[a-zA-Z0-9+/\-_]{40,}['"]?"#,
    r#"(?i)(\bcurl\b[^\n]*?\s(?:-u[ \t]*|--user[= \t]+)['"]?[^\s:'"]+:)([^\s'"]+)"#,
    r"(?i)(\b[a-z][a-z0-9+.-]*://[^\s:/@]+:)([^\s@/]+)(@)",
    r"()(eyJ[a-zA-Z0-9\-_]+\.eyJ[a-zA-Z0-9\-_]+\.[a-zA-Z0-9\-_]+)",
    r"(?i)(-----BEGIN\s+\w*\s*PRIVATE KEY-----)([\s\S]*?)(-----END\s+\w*\s*PRIVATE KEY-----)",
];

static RULES: LazyLock<Vec<Rule>> = LazyLock::new(|| {
    let named = NAMED.iter().map(|p| (p, true));
    let unnamed = UNNAMED.iter().map(|p| (p, false));
    named
        .chain(unnamed)
        .map(|(p, named)| Rule { re: Regex::new(p).unwrap(), named })
        .collect()
});

const REDACTED: &str = "[REDACTED]";

/// Values that look like config flags rather than secrets. When a secret-named
/// assignment (e.g. `AUTH_ENABLED=true`, `TOKEN_COUNT=5`) carries one of these,
/// redacting it would destroy harmless data, so it is left intact.
static NON_SECRET_VALUE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)^['"]?(true|false|yes|no|on|off|none|null|\d+(?:\.\d+)?)['"]?$"#).unwrap()
});

const SECRET_WORDS: &[&str] = &[
    "secret", "password", "passwd", "token", "apikey", "api_key", "auth", "credential", "private_key",
];

fn is_secret_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase().replace('-', "_").replace("author", "").replace("tokeniz", "");
    if ["file", "path", "stdin", "env"].iter().any(|s| name.ends_with(s)) {
        return false;
    }
    SECRET_WORDS.iter().any(|w| name.contains(w))
}

/// Filter sensitive content from a string. Returns the filtered version.
/// Preserves the key/label portion and only replaces the secret value.
pub fn redact(input: &str) -> String {
    let mut output = input.to_string();
    for rule in RULES.iter() {
        output = rule
            .re
            .replace_all(&output, |caps: &regex::Captures| {
                let whole = caps.get(0).map_or("", |m| m.as_str());
                if rule.named {
                    let name = caps.get(2).map_or("", |m| m.as_str());
                    let value = caps.get(3).map_or("", |m| m.as_str());
                    if !is_secret_name(name) || NON_SECRET_VALUE.is_match(value) {
                        return whole.to_string();
                    }
                    let prefix = caps.get(1).map_or("", |m| m.as_str());
                    return format!("{prefix}{REDACTED}");
                }
                let prefix = caps.get(1).map_or("", |m| m.as_str());
                let suffix = caps.get(3).map_or("", |m| m.as_str());
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
    fn aws_key_requires_word_boundary() {
        // A real key on a boundary is redacted; an AKIA-prefixed substring inside a
        // longer alphanumeric blob is not a key and must be left intact.
        let key = redact("key=AKIAIOSFODNN7EXAMPLE");
        assert!(!key.contains("AKIAIOSFODNN7EXAMPLE"));
        let embedded = "XAKIAIOSFODNN7EXAMPLE0000";
        assert_eq!(redact(embedded), embedded);
    }

    #[test]
    fn leaves_secret_named_flags_and_numbers() {
        // Secret-ish names with plain flag/number values are not secrets.
        for input in [
            "AUTH_ENABLED=true",
            "TOKEN_COUNT=5",
            "export PASSWORD_REQUIRED=false",
            "API_KEY_VERSION=2.1",
        ] {
            assert_eq!(redact(input), input, "should not redact: {input}");
        }
    }

    #[test]
    fn still_redacts_real_secret_with_value() {
        let input = "SECRET_KEY=abc123def456ghi";
        let result = redact(input);
        assert!(result.contains("SECRET_KEY="));
        assert!(result.contains(REDACTED));
        assert!(!result.contains("abc123def456ghi"));
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
    fn redacts_space_separated_secret_flags() {
        for (input, expected) in [
            ("docker login -u me --password hunter2", "docker login -u me --password [REDACTED]"),
            ("--api-key 'abc def'", "--api-key [REDACTED]"),
            ("gh auth login --with-token --token xyz", "gh auth login --with-token --token [REDACTED]"),
            ("cli --client-secret=s3cr3t", "cli --client-secret=[REDACTED]"),
        ] {
            assert_eq!(redact(input), expected, "{input}");
        }
    }

    #[test]
    fn leaves_secret_flags_without_a_value() {
        for input in [
            "docker login --password-stdin -u me",
            "tool --token-file ~/.token",
            "tool --no-auth --verbose",
        ] {
            assert_eq!(redact(input), input, "{input}");
        }
    }

    #[test]
    fn leaves_names_that_only_look_secret() {
        for input in [
            "AUTHOR=jane",
            "git commit --author \"Jane <jane@example.com>\"",
            "git log --author=jane",
            "content, tokenize='unicode61'",
            "regex patterns strip bearer tokens, AWS keys",
        ] {
            assert_eq!(redact(input), input, "{input}");
        }
    }

    #[test]
    fn redacts_url_credentials_for_any_scheme() {
        for (input, expected) in [
            ("git clone https://me:tok3n@github.com/x/y", "git clone https://me:[REDACTED]@github.com/x/y"),
            ("postgres://app.user-1:pw@db:5432/x", "postgres://app.user-1:[REDACTED]@db:5432/x"),
            ("ftp://anon:mail@host/", "ftp://anon:[REDACTED]@host/"),
        ] {
            assert_eq!(redact(input), expected, "{input}");
        }
    }

    #[test]
    fn leaves_urls_without_password() {
        for input in [
            "ssh://git@github.com:22/x/y",
            "git@github.com:org/repo.git",
            "https://example.com:8443/path",
        ] {
            assert_eq!(redact(input), input, "{input}");
        }
    }

    #[test]
    fn redacts_provider_tokens() {
        for token in [
            "ghp_0123456789abcdefghijABCDEFGHIJ012345",
            "github_pat_11ABCDEFG0123456789_abcdefghijklmnopqrstuvwxyzABCDEF",
            "glpat-abcdefghij0123456789",
            "sk-ant-api03-abcdefghijklmnopqrstuvwxyz",
            "sk-proj-abcdefghijklmnopqrstuvwxyz",
            "xoxb-1234567890-abcdefghij",
            "sk_live_abcdefghijklmnop1234",
            "AIzaSyA0123456789abcdefghijklmnopqrstuv",
            "npm_abcdefghijklmnopqrstuvwxyz0123456789",
        ] {
            let result = redact(&format!("echo {token} done"));
            assert_eq!(result, "echo [REDACTED] done", "{token}");
        }
    }

    #[test]
    fn leaves_words_that_contain_token_prefixes() {
        let input = "task-runner disk-usage ask-me";
        assert_eq!(redact(input), input);
    }

    #[test]
    fn redacts_basic_auth_header() {
        let result = redact("Authorization: Basic dXNlcjpwYXNz");
        assert_eq!(result, "Authorization: Basic [REDACTED]");
    }

    #[test]
    fn redacts_curl_user_password() {
        assert_eq!(redact("curl -u admin:s3cret https://x"), "curl -u admin:[REDACTED] https://x");
        assert_eq!(redact("curl --user 'admin:s3cret' https://x"), "curl --user 'admin:[REDACTED]' https://x");
    }

    #[test]
    fn redacts_json_and_yaml_secret_keys() {
        assert_eq!(
            redact(r#"{"user": "me", "password": "hunter2"}"#),
            r#"{"user": "me", "password": [REDACTED]}"#
        );
        assert_eq!(redact("db:\n  password: hunter2\n  host: x"), "db:\n  password: [REDACTED]\n  host: x");
        assert_eq!(redact("  api_token: 'abc'"), "  api_token: [REDACTED]");
    }

    #[test]
    fn leaves_log_lines_that_mention_secret_words() {
        for input in [
            "Refreshing token: done",
            "Invalid password: try again",
            "token: true",
            r#"{"token_count": "5"}"#,
        ] {
            assert_eq!(redact(input), input, "{input}");
        }
    }

    #[test]
    fn leaves_public_key_alone() {
        // Public keys should not be redacted
        let input = "-----BEGIN PUBLIC KEY-----\nMIIBIjANBg...\n-----END PUBLIC KEY-----";
        assert_eq!(redact(input), input);
    }
}
