//! Allowlist-first environment construction (§20, §49): credential
//! isolation starts here. A spawned command's environment is never a raw
//! copy of the runtime's own — by default it inherits the caller's
//! environment *minus* anything credential-shaped, and callers can go
//! further and build a fully explicit environment instead.

use std::collections::HashMap;

/// `_`-delimited name segments that mark a variable as credential-shaped,
/// checked case-insensitively against each segment individually (not as a
/// raw substring) — `AWS_ACCESS_KEY_ID` has a `KEY` segment and is
/// flagged, while `PUBKEY_ALGORITHMS` does not contain a bare `KEY`
/// segment (`PUBKEY` is one word) and survives. Deliberately broad beyond
/// that: a false positive here costs a tool an env var it probably didn't
/// need; a false negative can leak a secret into a child process's
/// environment and from there into logs or captured stdout.
// Note: deliberately excludes the bare segment `PWD` — that's the
// ubiquitous, entirely benign "present working directory" shell variable,
// not a credential, and stripping it would break ordinary shell-tool
// expectations for a false-positive reason.
const DENY_SEGMENTS: &[&str] = &[
    "TOKEN",
    "SECRET",
    "PASSWORD",
    "CREDENTIAL",
    "CREDENTIALS",
    "KEY",
    "APIKEY",
    "PRIVATE",
];

/// Exact variable names stripped regardless of pattern — SSH agent
/// forwarding is a credential-equivalent capability, not a mere secret
/// value, so it's named explicitly rather than relying on a substring
/// match.
const DENY_EXACT: &[&str] = &["SSH_AUTH_SOCK", "AWS_SESSION_TOKEN"];

#[derive(Debug, Clone)]
pub struct EnvPolicy {
    inherit: bool,
    filter_credentials: bool,
    extra: HashMap<String, String>,
}

impl EnvPolicy {
    /// Inherit the runtime's own environment, stripping anything
    /// credential-shaped. The sensible default for most tool invocations.
    pub fn inherit_filtered() -> Self {
        Self {
            inherit: true,
            filter_credentials: true,
            extra: HashMap::new(),
        }
    }

    /// No inherited environment at all — the command sees only what is
    /// explicitly added via [`Self::with_var`]. Used for the highest-risk
    /// invocations (§21 sandboxed execution).
    pub fn strict() -> Self {
        Self {
            inherit: false,
            filter_credentials: false,
            extra: HashMap::new(),
        }
    }

    /// Explicit opt-out of filtering — inherits everything, unfiltered.
    /// Requires the caller to have already authorized this at the
    /// permission-engine level (`secret_access`); this type does not
    /// enforce that itself.
    pub fn inherit_unfiltered() -> Self {
        Self {
            inherit: true,
            filter_credentials: false,
            extra: HashMap::new(),
        }
    }

    pub fn with_var(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.extra.insert(key.into(), value.into());
        self
    }

    /// Build the final environment map from `source` (the vars to inherit
    /// from, when inheriting — tests pass a synthetic map; production
    /// callers pass `std::env::vars().collect()`).
    pub fn build(&self, source: &HashMap<String, String>) -> HashMap<String, String> {
        let mut out = HashMap::new();
        if self.inherit {
            for (k, v) in source {
                if !self.filter_credentials || !is_credential_shaped(k) {
                    out.insert(k.clone(), v.clone());
                }
            }
        }
        for (k, v) in &self.extra {
            out.insert(k.clone(), v.clone());
        }
        out
    }
}

fn is_credential_shaped(name: &str) -> bool {
    let upper = name.to_uppercase();
    if DENY_EXACT.contains(&upper.as_str()) {
        return true;
    }
    upper
        .split('_')
        .any(|segment| DENY_SEGMENTS.contains(&segment))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source() -> HashMap<String, String> {
        [
            ("PATH", "/usr/bin"),
            ("HOME", "/home/dev"),
            ("AWS_ACCESS_KEY_ID", "AKIA..."),
            ("AWS_SECRET_ACCESS_KEY", "shh"),
            ("DATABASE_PASSWORD", "hunter2"),
            ("GITHUB_TOKEN", "ghp_..."),
            ("SSH_AUTH_SOCK", "/tmp/ssh.sock"),
            ("MY_PUBKEY_ALGORITHMS", "not actually secret shaped"),
            ("PWD", "/home/dev/project"),
            ("EDITOR", "vim"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
    }

    #[test]
    fn strict_ignores_source_entirely() {
        let env = EnvPolicy::strict().with_var("FOO", "bar").build(&source());
        assert_eq!(env.len(), 1);
        assert_eq!(env.get("FOO").map(String::as_str), Some("bar"));
    }

    #[test]
    fn inherit_filtered_strips_credential_shaped_vars() {
        let env = EnvPolicy::inherit_filtered().build(&source());
        assert!(env.contains_key("PATH"));
        assert!(env.contains_key("HOME"));
        assert!(env.contains_key("EDITOR"));
        assert!(!env.contains_key("AWS_ACCESS_KEY_ID"));
        assert!(!env.contains_key("AWS_SECRET_ACCESS_KEY"));
        assert!(!env.contains_key("DATABASE_PASSWORD"));
        assert!(!env.contains_key("GITHUB_TOKEN"));
        assert!(!env.contains_key("SSH_AUTH_SOCK"));
    }

    #[test]
    fn substring_match_is_not_overbroad() {
        let env = EnvPolicy::inherit_filtered().build(&source());
        // Contains "KEY" as a substring in the middle, but not as its own
        // `_`-delimited segment — must survive filtering.
        assert!(env.contains_key("MY_PUBKEY_ALGORITHMS"));
    }

    #[test]
    fn pwd_is_not_treated_as_a_password() {
        let env = EnvPolicy::inherit_filtered().build(&source());
        assert!(env.contains_key("PWD"));
    }

    #[test]
    fn inherit_unfiltered_keeps_everything() {
        let env = EnvPolicy::inherit_unfiltered().build(&source());
        assert!(env.contains_key("AWS_ACCESS_KEY_ID"));
        assert!(env.contains_key("SSH_AUTH_SOCK"));
    }

    #[test]
    fn extra_vars_override_inherited_ones() {
        let env = EnvPolicy::inherit_filtered()
            .with_var("PATH", "/custom/bin")
            .build(&source());
        assert_eq!(env.get("PATH").map(String::as_str), Some("/custom/bin"));
    }
}
