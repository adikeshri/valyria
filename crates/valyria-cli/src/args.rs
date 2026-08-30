//! Minimal, dependency-free flag parsing. The CLI stays a thin protocol
//! client (D11) — it dispatches into `valyria_protocol::Client` and holds
//! no orchestration logic — so the parser only needs to be good enough to
//! route a subcommand and its flags, not a full ergonomics layer.

use std::path::PathBuf;

use valyria_types::PermissionMode;

#[derive(Debug, Default)]
pub struct ParsedArgs {
    pub positional: Vec<String>,
    pub workspace: Option<PathBuf>,
    pub scenario: Option<PathBuf>,
    pub permission_mode: Option<PermissionMode>,
    pub events: bool,
    pub json: bool,
    pub dry_run: bool,
    pub allow: bool,
    pub deny: bool,
    /// `--plan`: run `Planning` as a model-authored, validated plan
    /// (Phase 8) instead of the pass-through.
    pub plan: bool,
    /// `--scope <memory|cache|tasks|logs>` for `valyria clean`.
    pub scope: Option<String>,
    /// `--connect <path>`: talk to a running daemon over its Unix socket
    /// instead of running the runtime in-process. Same `Client` trait,
    /// pure backend swap (D11).
    pub connect: Option<PathBuf>,
    /// `--socket <path>` for `valyria serve`.
    pub socket: Option<PathBuf>,
    /// `--auth-token-file <path>`: for `valyria serve`, read a per-daemon
    /// auth token from this file (G10); for `--connect`, present it. When
    /// unset the daemon relies on the peer-uid check alone.
    pub auth_token_file: Option<PathBuf>,
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
            "--scope" => {
                i += 1;
                let v = raw.get(i).ok_or("--scope needs a value")?;
                parsed.scope = Some(v.clone());
            }
            "--connect" => {
                i += 1;
                let v = raw.get(i).ok_or("--connect needs a socket path")?;
                parsed.connect = Some(PathBuf::from(v));
            }
            "--socket" => {
                i += 1;
                let v = raw.get(i).ok_or("--socket needs a path")?;
                parsed.socket = Some(PathBuf::from(v));
            }
            "--auth-token-file" => {
                i += 1;
                let v = raw.get(i).ok_or("--auth-token-file needs a path")?;
                parsed.auth_token_file = Some(PathBuf::from(v));
            }
            "--events" => parsed.events = true,
            "--json" => parsed.json = true,
            "--dry-run" => parsed.dry_run = true,
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

    #[test]
    fn phase10_flags_parse() {
        let p = parse(&args(
            "clean --scope cache --dry-run --json --connect /tmp/v.sock",
        ))
        .unwrap();
        assert_eq!(p.scope.as_deref(), Some("cache"));
        assert!(p.dry_run);
        assert!(p.json);
        assert_eq!(p.connect, Some(PathBuf::from("/tmp/v.sock")));
    }

    #[test]
    fn serve_socket_flag_parses() {
        let p = parse(&args("serve --socket /run/valyria.sock")).unwrap();
        assert_eq!(p.socket, Some(PathBuf::from("/run/valyria.sock")));
    }
}
