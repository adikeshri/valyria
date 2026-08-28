//! Minimal, dependency-free flag parsing. The command surface here is
//! intentionally small — full ergonomics (`--help` generation, shell
//! completions, subcommand trees) land with the rest of the CLI in Phase
//! 10; Phase 3 only needs enough to drive and observe the walking
//! skeleton.

use std::path::PathBuf;

use valyria_types::PermissionMode;

#[derive(Debug, Default)]
pub struct ParsedArgs {
    pub positional: Vec<String>,
    pub workspace: Option<PathBuf>,
    pub scenario: Option<PathBuf>,
    pub permission_mode: Option<PermissionMode>,
    pub events: bool,
    pub allow: bool,
    pub deny: bool,
    /// `--plan`: run `Planning` as a model-authored, validated plan
    /// (Phase 8) instead of the pass-through.
    pub plan: bool,
}

pub fn parse(raw: &[String]) -> Result<ParsedArgs, String> {
    let mut parsed = ParsedArgs::default();
    let mut i = 0;
    while i < raw.len() {
        match raw[i].as_str() {
            "--workspace" => {
                i += 1;
                let v = raw.get(i).ok_or("--workspace needs a value")?;
                parsed.workspace = Some(PathBuf::from(v));
            }
            "--scenario" => {
                i += 1;
                let v = raw.get(i).ok_or("--scenario needs a value")?;
                parsed.scenario = Some(PathBuf::from(v));
            }
            "--permission-mode" => {
                i += 1;
                let v = raw.get(i).ok_or("--permission-mode needs a value")?;
                parsed.permission_mode = Some(match v.as_str() {
                    "manual" => PermissionMode::Manual,
                    "assisted" => PermissionMode::Assisted,
                    "autonomous" => PermissionMode::Autonomous,
                    other => return Err(format!("unknown --permission-mode `{other}`")),
                });
            }
            "--events" => parsed.events = true,
            "--plan" => parsed.plan = true,
            "--allow" => parsed.allow = true,
            "--deny" => parsed.deny = true,
            other => parsed.positional.push(other.to_string()),
        }
        i += 1;
    }
    Ok(parsed)
}

pub fn resolve_workspace(parsed: &ParsedArgs) -> PathBuf {
    parsed
        .workspace
        .clone()
        .unwrap_or_else(|| std::env::current_dir().expect("current directory must be readable"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn parses_flags_and_positionals() {
        let parsed = parse(&args("run \"add a function\" --workspace /tmp/ws --events")).unwrap();
        assert_eq!(parsed.positional, vec!["run", "\"add", "a", "function\""]);
        assert_eq!(parsed.workspace, Some(PathBuf::from("/tmp/ws")));
        assert!(parsed.events);
    }

    #[test]
    fn permission_mode_parses_known_values() {
        let parsed = parse(&args("run x --permission-mode autonomous")).unwrap();
        assert_eq!(parsed.permission_mode, Some(PermissionMode::Autonomous));
    }

    #[test]
    fn plan_flag_is_off_by_default_and_opt_in() {
        assert!(!parse(&args("run x")).unwrap().plan);
        assert!(parse(&args("run x --plan")).unwrap().plan);
    }

    #[test]
    fn unknown_permission_mode_errors() {
        let err = parse(&args("run x --permission-mode bogus")).unwrap_err();
        assert!(err.contains("bogus"));
    }

    #[test]
    fn missing_flag_value_errors() {
        let err = parse(&args("run x --workspace")).unwrap_err();
        assert!(err.contains("--workspace"));
    }
}
