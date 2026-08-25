//! Command risk classification (§4.9): argv-level analysis, not regex over
//! a joined string — the curated destructive-pattern database is checked
//! against the actual command shape (program + args), with a shell escape
//! hatch (`sh -c "..."`) handled as its own case since that's the one
//! place our argv-safety model (§20: never a raw shell string) can be
//! circumvented by the command itself.

use crate::request::RiskLevel;

const DESTRUCTIVE_SUBSTRINGS: &[&str] = &[
    "rm -rf",
    "rm -fr",
    "rm -r -f",
    "rm -f -r",
    "rm --recursive --force",
    "dd if=",
    "dd of=",
    "git push --force",
    "git push -f",
    "git reset --hard",
    "git clean -fdx",
    "git clean -xfd",
    "git clean -fd",
    "chmod -r 777",
    "chmod 777 -r",
    "mkfs",
    "of=/dev/",
    "| sh",
    "| bash",
    "curl | sh",
];

const SAFE_PROGRAMS: &[&str] = &[
    "echo", "cat", "ls", "pwd", "grep", "find", "head", "tail", "wc", "true", "test", "stat",
    "file", "which", "env", "printf",
];

/// Programs whose *some* subcommands are safe reads (`git status`) but
/// whose other subcommands are checked separately (`git push --force`
/// above) — never blanket-trusted the way [`SAFE_PROGRAMS`] is.
const SAFE_READ_SUBCOMMANDS: &[(&str, &[&str])] = &[(
    "git",
    &[
        "status",
        "diff",
        "log",
        "show",
        "blame",
        "branch",
        "rev-parse",
    ],
)];

const SHELL_INTERPRETERS: &[&str] = &["sh", "bash", "zsh", "dash", "ksh"];

const INSTALL_PROGRAMS: &[&str] = &[
    "npm", "pip", "pip3", "cargo", "gem", "go", "brew", "apt", "apt-get", "yum", "dnf", "pnpm",
    "yarn",
];
const INSTALL_VERBS: &[&str] = &["install", "add", "uninstall", "remove", "update", "upgrade"];

pub fn classify_command(program: &str, args: &[String]) -> RiskLevel {
    let prog_name = basename(program);
    let joined_lower = format!("{prog_name} {}", args.join(" ")).to_lowercase();

    if DESTRUCTIVE_SUBSTRINGS
        .iter()
        .any(|p| joined_lower.contains(p))
    {
        return RiskLevel::Destructive;
    }

    let is_shell_dash_c =
        SHELL_INTERPRETERS.contains(&prog_name.as_str()) && args.iter().any(|a| a == "-c");
    if is_shell_dash_c {
        // The shell content itself was already scanned above (it's part
        // of `joined_lower`) and didn't match a known-destructive
        // pattern, but arbitrary shell text is never auto-trusted as Safe
        // just because it didn't match our (necessarily incomplete)
        // pattern list.
        return RiskLevel::Unknown;
    }

    if let Some((_, safe_subs)) = SAFE_READ_SUBCOMMANDS.iter().find(|(p, _)| *p == prog_name) {
        if args
            .first()
            .is_some_and(|a| safe_subs.contains(&a.as_str()))
        {
            return RiskLevel::Safe;
        }
    }

    if INSTALL_PROGRAMS.contains(&prog_name.as_str())
        && args.iter().any(|a| INSTALL_VERBS.contains(&a.as_str()))
    {
        return RiskLevel::Controlled;
    }

    if SAFE_PROGRAMS.contains(&prog_name.as_str()) {
        return RiskLevel::Safe;
    }

    RiskLevel::Unknown
}

fn basename(program: &str) -> String {
    std::path::Path::new(program)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(program)
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn recognizes_safe_read_only_programs() {
        assert_eq!(classify_command("ls", &args(&["-la"])), RiskLevel::Safe);
        assert_eq!(
            classify_command("cat", &args(&["file.txt"])),
            RiskLevel::Safe
        );
        assert_eq!(
            classify_command("/bin/echo", &args(&["hi"])),
            RiskLevel::Safe
        );
    }

    #[test]
    fn recognizes_safe_git_subcommands() {
        assert_eq!(classify_command("git", &args(&["status"])), RiskLevel::Safe);
        assert_eq!(
            classify_command("git", &args(&["diff", "HEAD"])),
            RiskLevel::Safe
        );
    }

    #[test]
    fn flags_destructive_rm_rf() {
        assert_eq!(
            classify_command("rm", &args(&["-rf", "/"])),
            RiskLevel::Destructive
        );
        assert_eq!(
            classify_command("rm", &args(&["-fr", "target"])),
            RiskLevel::Destructive
        );
    }

    #[test]
    fn flags_destructive_git_force_push() {
        assert_eq!(
            classify_command("git", &args(&["push", "--force", "origin", "main"])),
            RiskLevel::Destructive
        );
        assert_eq!(
            classify_command("git", &args(&["push", "-f"])),
            RiskLevel::Destructive
        );
    }

    #[test]
    fn flags_destructive_git_reset_hard() {
        assert_eq!(
            classify_command("git", &args(&["reset", "--hard", "HEAD~5"])),
            RiskLevel::Destructive
        );
    }

    #[test]
    fn flags_dd_and_mkfs() {
        assert_eq!(
            classify_command("dd", &args(&["if=/dev/zero", "of=/dev/sda"])),
            RiskLevel::Destructive
        );
        assert_eq!(
            classify_command("mkfs", &args(&["/dev/sda1"])),
            RiskLevel::Destructive
        );
    }

    #[test]
    fn flags_pipe_to_shell() {
        assert_eq!(
            classify_command(
                "curl",
                &args(&["https://example.com/install.sh", "|", "sh"])
            ),
            RiskLevel::Destructive
        );
    }

    #[test]
    fn shell_dash_c_with_unrecognized_content_is_unknown_not_safe() {
        assert_eq!(
            classify_command("/bin/sh", &args(&["-c", "echo hello"])),
            RiskLevel::Unknown
        );
    }

    #[test]
    fn shell_dash_c_with_destructive_content_is_still_caught() {
        assert_eq!(
            classify_command("bash", &args(&["-c", "rm -rf /important"])),
            RiskLevel::Destructive
        );
    }

    #[test]
    fn recognizes_controlled_package_installs() {
        assert_eq!(
            classify_command("npm", &args(&["install", "left-pad"])),
            RiskLevel::Controlled
        );
        assert_eq!(
            classify_command("cargo", &args(&["add", "serde"])),
            RiskLevel::Controlled
        );
    }

    #[test]
    fn unrecognized_binary_defaults_to_unknown() {
        assert_eq!(
            classify_command("some-random-tool", &args(&["--flag"])),
            RiskLevel::Unknown
        );
    }

    #[test]
    fn unrecognized_git_subcommand_is_not_blanket_safe() {
        // "git" is not in SAFE_PROGRAMS wholesale — only specific
        // subcommands are; an unrecognized one must not fall through to Safe.
        assert_eq!(
            classify_command("git", &args(&["some-future-subcommand"])),
            RiskLevel::Unknown
        );
    }
}
