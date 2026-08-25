//! Secret redaction (§29 diagnosis, §49 security): applied to anything
//! entering model context, logs, or an error surfaced to the client.
//!
//! Two detection strategies, combined: known credential *shapes* (AWS keys,
//! bearer tokens, private key blocks, generic `KEY=value` assignments) via
//! regex, and generic high-entropy tokens that don't match a known shape
//! but look secret-like anyway. Prefer over-redaction to under-redaction —
//! a false positive costs a debugging inconvenience; a false negative can
//! leak a credential into a model prompt or a log file.

use std::sync::LazyLock;

use regex::Regex;

const REDACTED: &str = "[REDACTED]";

struct Pattern {
    name: &'static str,
    regex: Regex,
}

static PATTERNS: LazyLock<Vec<Pattern>> = LazyLock::new(|| {
    vec![
        Pattern {
            name: "aws_access_key",
            regex: Regex::new(r"\b(AKIA|ASIA)[0-9A-Z]{16}\b").unwrap(),
        },
        Pattern {
            name: "private_key_block",
            regex: Regex::new(
                r"-----BEGIN (?:RSA |EC |OPENSSH |DSA |)PRIVATE KEY-----[\s\S]*?-----END (?:RSA |EC |OPENSSH |DSA |)PRIVATE KEY-----",
            )
            .unwrap(),
        },
        Pattern {
            name: "bearer_token",
            regex: Regex::new(r"(?i)\bbearer\s+[A-Za-z0-9\-_.~+/]{16,}=*").unwrap(),
        },
        Pattern {
            name: "github_token",
            regex: Regex::new(r"\bgh[pousr]_[A-Za-z0-9]{36,}\b").unwrap(),
        },
        Pattern {
            name: "generic_key_assignment",
            // e.g. API_KEY=..., token: "...", secret = '...'
            regex: Regex::new(
                r#"(?i)\b([A-Z_]*(?:KEY|TOKEN|SECRET|PASSWORD)[A-Z_]*)\s*[:=]\s*['"]?([A-Za-z0-9\-_./+]{12,})['"]?"#,
            )
            .unwrap(),
        },
    ]
});

/// Redact known credential shapes from `text`, returning the redacted text
/// and the names of every pattern that matched (for logging/telemetry —
/// never log the matched value itself).
pub fn redact(text: &str) -> (String, Vec<&'static str>) {
    let mut out = text.to_string();
    let mut hit: Vec<&'static str> = Vec::new();

    for pattern in PATTERNS.iter() {
        if pattern.regex.is_match(&out) {
            hit.push(pattern.name);
            out = pattern
                .regex
                .replace_all(&out, |caps: &regex::Captures| {
                    // For the key=value pattern, keep the key name visible
                    // ("API_KEY=[REDACTED]") since the key name itself
                    // isn't secret and is useful for debugging.
                    if caps.len() > 1 {
                        if let Some(key) = caps.get(1) {
                            return format!("{}={}", key.as_str(), REDACTED);
                        }
                    }
                    REDACTED.to_string()
                })
                .into_owned();
        }
    }

    (out, hit)
}

/// Shannon entropy of the byte distribution, in bits per byte. High-entropy
/// tokens (roughly > 4.0 for base64/hex-like alphabets) are candidates for
/// "looks like a secret even though we don't recognize its shape".
pub fn shannon_entropy(s: &str) -> f64 {
    if s.is_empty() {
        return 0.0;
    }
    let mut counts = [0u32; 256];
    for b in s.bytes() {
        counts[b as usize] += 1;
    }
    let len = s.len() as f64;
    counts
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = c as f64 / len;
            -p * p.log2()
        })
        .sum()
}

/// Heuristic: does this look like an opaque secret token even without
/// matching a known pattern? Long, high-entropy, alphanumeric-with-symbols
/// runs are flagged. Natural-language phrases can have surprisingly high
/// per-character entropy too, so a digit-count check is required as well —
/// real tokens (base64/hex/API-key alphabets) mix digits into an otherwise
/// letter-heavy string in a way prose essentially never does.
pub fn looks_like_secret(token: &str) -> bool {
    if token.len() < 20 || token.len() > 512 {
        return false;
    }
    let is_token_charset = token
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+' | '/' | '='));
    let digit_count = token.chars().filter(|c| c.is_ascii_digit()).count();
    is_token_charset && digit_count >= 2 && shannon_entropy(token) > 4.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_aws_access_key() {
        let (out, hits) = redact("export AWS_ACCESS_KEY_ID=AKIAABCDEFGHIJKLMNOP");
        assert!(!out.contains("AKIAABCDEFGHIJKLMNOP"));
        assert!(hits.contains(&"aws_access_key") || hits.contains(&"generic_key_assignment"));
    }

    #[test]
    fn redacts_private_key_block() {
        let key =
            "-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQ==\n-----END RSA PRIVATE KEY-----";
        let (out, hits) = redact(key);
        assert!(!out.contains("MIIEowIBAAKCAQ"));
        assert!(hits.contains(&"private_key_block"));
    }

    #[test]
    fn redacts_bearer_token() {
        let (out, _) = redact("Authorization: Bearer sk-abcdefghijklmnopqrstuvwx1234567890");
        assert!(!out.contains("sk-abcdefghijklmnopqrstuvwx1234567890"));
    }

    #[test]
    fn redacts_generic_key_assignment_but_keeps_key_name() {
        let (out, hits) = redact(r#"DATABASE_PASSWORD = "hunter2superlongpassword123""#);
        assert!(hits.contains(&"generic_key_assignment"));
        assert!(out.contains("DATABASE_PASSWORD="));
        assert!(!out.contains("hunter2superlongpassword123"));
    }

    #[test]
    fn leaves_ordinary_text_untouched() {
        let (out, hits) = redact("fn main() { println!(\"hello world\"); }");
        assert_eq!(out, "fn main() { println!(\"hello world\"); }");
        assert!(hits.is_empty());
    }

    #[test]
    fn entropy_distinguishes_random_from_repetitive() {
        assert!(shannon_entropy("aaaaaaaaaaaaaaaa") < shannon_entropy("kX9mQ2pL7vN4zR8w"));
    }

    #[test]
    fn looks_like_secret_flags_long_random_tokens() {
        assert!(looks_like_secret("kX9mQ2pL7vN4zR8wT3jH6bY1"));
        assert!(!looks_like_secret("the_quick_brown_fox_jumps"));
        assert!(!looks_like_secret("short"));
    }
}
