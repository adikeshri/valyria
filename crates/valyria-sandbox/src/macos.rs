//! macOS confinement via Seatbelt (`sandbox-exec`). Deliberately broad on
//! reads and restrictive on writes — see [`SandboxProfile`]'s docs for why
//! — verified against a real running profile (not guessed syntax):
//! `deny default` plus `file-read*`/`sysctl-read`/`mach-lookup` is the
//! minimum a program needs just to start and exit cleanly, confirmed by
//! actually running `/bin/echo` and `/bin/sh` under generated profiles
//! during development of this module.
//!
//! One hard-won detail: every path embedded in the profile must be
//! **canonicalized** first. `/tmp` is a symlink to `/private/tmp` on
//! macOS, and Seatbelt's `subpath` matches against the resolved path, not
//! the one the caller wrote — a profile written against `/tmp/...` allows
//! nothing.

use std::path::Path;

use valyria_process::CommandSpec;

use crate::confinement::Confinement;
use crate::error::{Result, SandboxError};
use crate::launcher::ProcessLauncher;
use crate::profile::SandboxProfile;

const SANDBOX_EXEC_PATH: &str = "/usr/bin/sandbox-exec";

pub struct SeatbeltLauncher {
    sandbox_exec: String,
}

impl SeatbeltLauncher {
    /// Only returns a launcher if `sandbox-exec` is actually present —
    /// confinement is never claimed on the strength of "this is usually
    /// macOS", only on the strength of the mechanism being verified
    /// present.
    pub fn detect() -> Option<Self> {
        if Path::new(SANDBOX_EXEC_PATH).exists() {
            Some(Self {
                sandbox_exec: SANDBOX_EXEC_PATH.to_string(),
            })
        } else {
            None
        }
    }
}

impl ProcessLauncher for SeatbeltLauncher {
    fn confinement_level(&self) -> Confinement {
        // Network confinement is per-profile (depends on `allow_network`),
        // so the launcher's *baseline* guaranteed level is filesystem-only;
        // §21 network policy layers on top per invocation via
        // `SandboxProfile::allow_network`.
        Confinement::Filesystem
    }

    fn wrap(&self, spec: CommandSpec, profile: &SandboxProfile) -> Result<CommandSpec> {
        let mut canon_writes = Vec::with_capacity(profile.allow_write.len());
        for path in &profile.allow_write {
            // The path may not exist yet (a file about to be created);
            // canonicalize the deepest existing ancestor instead, exactly
            // as `valyria-vfs::WorkspaceRoot::resolve` does, then it's a
            // subpath rule so the not-yet-existing tail is covered too.
            canon_writes.push(canonicalize_for_subpath(path)?);
        }

        let profile_text = render_profile(&canon_writes, profile.allow_network);

        let mut wrapped = CommandSpec::new(self.sandbox_exec.clone(), spec.cwd.clone())
            .arg("-p")
            .arg(profile_text)
            .arg("--")
            .arg(spec.program.clone());
        wrapped.args.extend(spec.args.iter().cloned());
        wrapped.env = spec.env;
        wrapped.timeout = spec.timeout;
        wrapped.idle_timeout = spec.idle_timeout;
        wrapped.max_output_bytes = spec.max_output_bytes;

        Ok(wrapped)
    }
}

fn canonicalize_for_subpath(path: &Path) -> Result<std::path::PathBuf> {
    let mut ancestor = path.to_path_buf();
    loop {
        if ancestor.exists() {
            return std::fs::canonicalize(&ancestor).map_err(|e| SandboxError::Canonicalize {
                path: ancestor.display().to_string(),
                source: e,
            });
        }
        if !ancestor.pop() {
            return Ok(path.to_path_buf()); // nothing exists at all; use as-is
        }
    }
}

fn render_profile(allow_write: &[std::path::PathBuf], allow_network: bool) -> String {
    let mut lines = vec![
        "(version 1)".to_string(),
        "(deny default)".to_string(),
        "(allow process-exec)".to_string(),
        "(allow process-fork)".to_string(),
        "(allow file-read*)".to_string(),
        "(allow sysctl-read)".to_string(),
        "(allow mach-lookup)".to_string(),
        "(allow signal (target self))".to_string(),
    ];

    for path in allow_write {
        lines.push(format!(
            "(allow file-write* (subpath {}))",
            sexpr_string(path)
        ));
    }

    if allow_network {
        lines.push("(allow network*)".to_string());
    }

    lines.join("\n")
}

/// Renders a path as a Seatbelt s-expression string literal, escaping the
/// two characters (`"` and `\`) that would otherwise break out of it.
fn sexpr_string(path: &Path) -> String {
    let escaped = path
        .display()
        .to_string()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    format!("\"{escaped}\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_sandbox_exec_on_a_real_mac() {
        // This test only makes sense on macOS, which is the only place
        // this module compiles.
        assert!(SeatbeltLauncher::detect().is_some());
    }

    #[test]
    fn profile_denies_by_default_and_allows_only_named_write_paths() {
        let profile = render_profile(&[std::path::PathBuf::from("/private/tmp/x")], false);
        assert!(profile.contains("(deny default)"));
        assert!(profile.contains("(allow file-write* (subpath \"/private/tmp/x\"))"));
        assert!(!profile.contains("network"));
    }

    #[test]
    fn network_allow_only_present_when_requested() {
        let with_net = render_profile(&[], true);
        let without_net = render_profile(&[], false);
        assert!(with_net.contains("(allow network*)"));
        assert!(!without_net.contains("network"));
    }

    #[test]
    fn escapes_quotes_in_paths() {
        let s = sexpr_string(Path::new("/tmp/weird\"name"));
        assert_eq!(s, "\"/tmp/weird\\\"name\"");
    }

    #[tokio::test]
    async fn end_to_end_write_outside_profile_is_blocked() {
        let launcher = SeatbeltLauncher::detect().expect("sandbox-exec must exist on macOS CI");
        let workdir = tempfile::tempdir().unwrap();
        let allowed = workdir.path().join("allowed");
        let denied = workdir.path().join("denied");
        std::fs::create_dir_all(&allowed).unwrap();
        std::fs::create_dir_all(&denied).unwrap();

        let profile = SandboxProfile::new().allow_write(&allowed);

        let spec = CommandSpec::new("/bin/sh", workdir.path())
            .arg("-c")
            .arg(format!("echo ok > {}/f.txt", denied.display()));
        let wrapped = launcher.wrap(spec, &profile).unwrap();

        let result = valyria_process::run(&wrapped, valyria_util::CancellationToken::new())
            .await
            .unwrap();

        assert!(!result.success(), "write outside the profile must fail");
        assert!(!denied.join("f.txt").exists());
    }

    #[tokio::test]
    async fn end_to_end_write_inside_profile_succeeds() {
        let launcher = SeatbeltLauncher::detect().expect("sandbox-exec must exist on macOS CI");
        let workdir = tempfile::tempdir().unwrap();
        let allowed = workdir.path().join("allowed");
        std::fs::create_dir_all(&allowed).unwrap();

        let profile = SandboxProfile::new().allow_write(&allowed);

        let spec = CommandSpec::new("/bin/sh", workdir.path())
            .arg("-c")
            .arg(format!("echo ok > {}/f.txt", allowed.display()));
        let wrapped = launcher.wrap(spec, &profile).unwrap();

        let result = valyria_process::run(&wrapped, valyria_util::CancellationToken::new())
            .await
            .unwrap();

        assert!(result.success(), "stderr: {}", result.stderr.text);
        assert_eq!(
            std::fs::read_to_string(allowed.join("f.txt"))
                .unwrap()
                .trim(),
            "ok"
        );
    }

    #[tokio::test]
    async fn end_to_end_network_denied_by_default() {
        let launcher = SeatbeltLauncher::detect().expect("sandbox-exec must exist on macOS CI");
        let workdir = tempfile::tempdir().unwrap();
        let profile = SandboxProfile::new(); // allow_network defaults to false

        let spec = CommandSpec::new("/usr/bin/curl", workdir.path()).args([
            "-s",
            "-m",
            "3",
            "-o",
            "/dev/null",
            "-w",
            "%{http_code}",
            "https://example.com",
        ]);
        let wrapped = launcher.wrap(spec, &profile).unwrap();

        let result = valyria_process::run(&wrapped, valyria_util::CancellationToken::new())
            .await
            .unwrap();

        assert_ne!(
            result.stdout.text.trim(),
            "200",
            "network should have been denied"
        );
    }
}
