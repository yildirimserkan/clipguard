//! Secret detection.
//!
//! Two layers:
//!   1. Known formats (cloud keys, tokens, PEM keys, JWT, credentials in URLs/assignments)
//!   2. Heuristic for generated passwords: one "word", 12-64 chars, all four character
//!      classes, a password-style symbol, decent entropy.
//!
//! Tuned for a developer clipboard: identifiers, paths, URLs, UUIDs and hex hashes
//! (commit SHAs, sha256 sums) must NOT be flagged.

use regex::RegexSet;
use std::sync::LazyLock;

const NAMES: &[&str] = &[
    "private key (PEM)",
    "AWS access key",
    "GitHub token",
    "GitLab token",
    "Slack token",
    "Google API key",
    "Stripe live key",
    "Anthropic API key",
    "OpenAI API key",
    "JWT",
    "Bearer token",
    "credentials in URL",
    "secret assignment",
    "Azure storage key",
];

static PATTERNS: LazyLock<RegexSet> = LazyLock::new(|| {
    RegexSet::new([
        r"-----BEGIN (?:[A-Z0-9]+ )*PRIVATE KEY-----",
        r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b",
        r"\b(?:ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{36}\b|\bgithub_pat_[A-Za-z0-9_]{60,}",
        r"\bglpat-[A-Za-z0-9_\-]{20,}",
        r"\bxox[abposr]-[A-Za-z0-9\-]{10,}",
        r"\bAIza[0-9A-Za-z_\-]{35}\b",
        r"\b(?:sk|rk)_live_[0-9A-Za-z]{20,}",
        r"\bsk-ant-[A-Za-z0-9_\-]{20,}",
        r"\bsk-(?:proj-)?[A-Za-z0-9_\-]{32,}",
        r"\beyJ[A-Za-z0-9_\-]{8,}\.eyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}",
        r"(?i)\bbearer\s+[A-Za-z0-9._~+/=\-]{20,}",
        r"\b[a-zA-Z][a-zA-Z0-9+.\-]*://[^\s:/@]+:[^\s/@]+@",
        r#"(?i)\b(?:password|passwd|pwd|secret|api[_\-]?key|access[_\-]?token|auth[_\-]?token|client[_\-]?secret)\b["']?\s*[:=]\s*["']?[^\s"',;]{6,}"#,
        r"AccountKey=[A-Za-z0-9+/=]{40,}",
    ])
    .expect("secret patterns compile")
});

/// Returns what kind of secret this looks like, or None.
pub fn detect(text: &str) -> Option<&'static str> {
    if let Some(i) = PATTERNS.matches(text).iter().next() {
        return Some(NAMES[i]);
    }
    if looks_like_password(text.trim()) {
        return Some("password-like string");
    }
    None
}

fn shannon_entropy(s: &str) -> f64 {
    let mut counts = std::collections::HashMap::new();
    for c in s.chars() {
        *counts.entry(c).or_insert(0usize) += 1;
    }
    let n = s.chars().count() as f64;
    counts.values().map(|&c| { let p = c as f64 / n; -p * p.log2() }).sum()
}

fn looks_like_password(s: &str) -> bool {
    let len = s.chars().count();
    if !(12..=64).contains(&len) || s.chars().any(char::is_whitespace) {
        return false;
    }
    if s.contains("://") || s.starts_with('/') || s.starts_with("./") || s.contains('\\') {
        return false;
    }
    let lower = s.chars().any(|c| c.is_ascii_lowercase());
    let upper = s.chars().any(|c| c.is_ascii_uppercase());
    let digit = s.chars().any(|c| c.is_ascii_digit());
    // '.', '_', '-', '/', ':' are everyday identifier/path chars -> not a password signal
    let pw_symbol = s.chars().any(|c| "!@#$%^&*()+=[]{}|;'\",<>?~`".contains(c));
    lower && upper && digit && pw_symbol && shannon_entropy(s) >= 3.0
}

#[cfg(test)]
mod tests {
    use super::detect;

    #[test]
    fn flags_real_secrets() {
        let hits = [
            "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXk...",
            "AKIAIOSFODNN7EXAMPLE",
            "ghp_aBcDeFgHiJkLmNoPqRsTuVwXyZ0123456789",
            "xoxb-123456789012-abcdefghijkl",
            "AIzaSyA1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q",
            "sk_live_51HxYzAbCdEfGhIjKlMnOpQr",
            "sk-ant-api03-AbCdEfGhIjKlMnOpQrStUvWx",
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U",
            "Authorization: Bearer abcdefghijklmnopqrstuvwxyz123456",
            "postgres://admin:S3cr3t!@db.internal:5432/app",
            "DB_PASSWORD=hunter2hunter2",
            "\"api_key\": \"0123456789abcdef\"",
            "Tr0ub4dor&3xKq9!",
            "q7#Vm2$LpZ8!wR4x",
        ];
        for h in hits {
            assert!(detect(h).is_some(), "should flag: {h}");
        }
    }

    #[test]
    fn ignores_everyday_dev_clips() {
        let misses = [
            "SELECT * FROM users WHERE id = 42;",
            "https://serkanyildirim.tr/blog/post-1",
            "/usr/local/bin/clipguard",
            "C:\\Users\\Serkan\\AppData\\Roaming",
            "550e8400-e29b-41d4-a716-446655440000",
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            "a94a8fe5ccb19ba61c4c0873d391e987982fbbd3",
            "MyService.GetUserById_V2",
            "getUserById",
            "npm install --save-dev typescript",
            "Merhaba, toplantı 15:00'te.",
            "password",
            "Set the password field to required",
        ];
        for m in misses {
            assert!(detect(m).is_none(), "false positive: {m} -> {:?}", detect(m));
        }
    }
}
