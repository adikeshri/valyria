//! Tooling discovery (§4.26): probe a workspace for the build / test /
//! lint / format commands its own maintainers actually use, then confirm
//! each one by executing a cheap probe before trusting it.
//!
//! Discovery itself is a pure filesystem read — no process is spawned
//! until [`validate`] runs. That split keeps the "what commands might this
//! repo have" question a fast, deterministic, unit-testable function, and
//! isolates the one genuinely environment-dependent step (is `cargo`
//! actually on this machine?) behind an injectable [`ProbeRunner`].

use std::collections::BTreeMap;
use std::path::Path;

use async_trait::async_trait;

use crate::command::{CommandKind, CommandSource, VerifyCommand};

/// The result of scanning a workspace: every candidate command, plus notes
/// about what was and wasn't found (surfaced to the user, not just
/// dropped).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiscoveryReport {
    pub candidates: Vec<VerifyCommand>,
    pub notes: Vec<String>,
}

impl DiscoveryReport {
    pub fn is_empty(&self) -> bool {
        self.candidates.is_empty()
    }

    pub fn by_kind(&self, kind: CommandKind) -> impl Iterator<Item = &VerifyCommand> {
        self.candidates.iter().filter(move |c| c.kind == kind)
    }

    /// Best candidate for a kind: highest source confidence wins, ties
    /// broken by "fewer args" (the more general invocation).
    pub fn best(&self, kind: CommandKind) -> Option<&VerifyCommand> {
        self.by_kind(kind).max_by(|a, b| {
            a.source
                .confidence()
                .cmp(&b.source.confidence())
                .then(b.args.len().cmp(&a.args.len()))
        })
    }

    fn push(&mut self, command: VerifyCommand) {
        let id = command.identity();
        if let Some(existing) = self
            .candidates
            .iter_mut()
            .find(|c| c.identity() == id && c.kind == command.kind)
        {
            // Keep the higher-confidence source when the same argv turns
            // up twice (CI workflow beats a manifest guess).
            if command.source.confidence() > existing.source.confidence() {
                existing.source = command.source;
            }
            return;
        }
        self.candidates.push(command);
    }
}

/// Scan `root` for verification tooling. Reads files; spawns nothing.
pub fn scan(root: &Path) -> DiscoveryReport {
    let mut report = DiscoveryReport::default();

    scan_cargo(root, &mut report);
    scan_node(root, &mut report);
    scan_python(root, &mut report);
    scan_go(root, &mut report);
    scan_make(root, &mut report);
    scan_just(root, &mut report);
    scan_config_files(root, &mut report);
    scan_conventions(root, &mut report);
    scan_ci_workflows(root, &mut report);

    if report.is_empty() {
        report
            .notes
            .push("no manifest, CI workflow or script convention found".into());
    }
    report
}

fn exists(root: &Path, rel: &str) -> bool {
    root.join(rel).exists()
}

fn read(root: &Path, rel: &str) -> Option<String> {
    std::fs::read_to_string(root.join(rel)).ok()
}

// --- Rust / Cargo ---------------------------------------------------------

fn scan_cargo(root: &Path, report: &mut DiscoveryReport) {
    if !exists(root, "Cargo.toml") {
        return;
    }
    let src = CommandSource::Manifest {
        file: "Cargo.toml".into(),
    };
    let workspace = read(root, "Cargo.toml")
        .map(|c| c.contains("[workspace]"))
        .unwrap_or(false);
    let scope: &[&str] = if workspace { &["--workspace"] } else { &[] };

    report.push(VerifyCommand::new(
        CommandKind::Build,
        "cargo",
        std::iter::once("build").chain(scope.iter().copied()),
        src.clone(),
    ));
    report.push(VerifyCommand::new(
        CommandKind::Test,
        "cargo",
        std::iter::once("test").chain(scope.iter().copied()),
        src.clone(),
    ));
    report.push(VerifyCommand::new(
        CommandKind::Lint,
        "cargo",
        ["clippy"]
            .into_iter()
            .chain(scope.iter().copied())
            .chain(["--", "-Dwarnings"]),
        src.clone(),
    ));
    report.push(VerifyCommand::new(
        CommandKind::Format,
        "cargo",
        ["fmt", "--", "--check"],
        src,
    ));
}

// --- Node / package.json ------------------------------------------------

fn scan_node(root: &Path, report: &mut DiscoveryReport) {
    let Some(raw) = read(root, "package.json") else {
        return;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&raw) else {
        report
            .notes
            .push("package.json present but did not parse as JSON".into());
        return;
    };
    let runner = node_runner(root);
    let scripts = json
        .get("scripts")
        .and_then(|s| s.as_object())
        .cloned()
        .unwrap_or_default();

    for (name, _) in &scripts {
        let Some(kind) = script_name_kind(name) else {
            continue;
        };
        let (program, mut args) = runner.clone();
        args.push(name.clone());
        report.push(VerifyCommand::new(
            kind,
            program,
            args,
            CommandSource::Script {
                file: "package.json".into(),
                name: name.clone(),
            },
        ));
    }

    // `npm test` works even without an explicit "test" script alias in
    // some setups; only assert it if the script is actually declared.
    if !scripts.contains_key("test") {
        report
            .notes
            .push("package.json has no `test` script".into());
    }
}

/// `(program, leading args)` for running a package script, picked from
/// whichever lockfile is present.
fn node_runner(root: &Path) -> (String, Vec<String>) {
    if exists(root, "pnpm-lock.yaml") {
        ("pnpm".into(), vec!["run".into()])
    } else if exists(root, "yarn.lock") {
        ("yarn".into(), vec![])
    } else if exists(root, "bun.lockb") {
        ("bun".into(), vec!["run".into()])
    } else {
        ("npm".into(), vec!["run".into()])
    }
}

fn script_name_kind(name: &str) -> Option<CommandKind> {
    let n = name.to_ascii_lowercase();
    if n == "test" || n.starts_with("test:") || n == "jest" || n == "vitest" {
        Some(CommandKind::Test)
    } else if n == "lint" || n.starts_with("lint:") || n == "eslint" {
        Some(CommandKind::Lint)
    } else if n == "typecheck" || n == "type-check" || n == "tsc" || n == "check-types" {
        Some(CommandKind::Typecheck)
    } else if n == "format" || n == "fmt" || n == "prettier" || n == "format:check" {
        Some(CommandKind::Format)
    } else if n == "build" {
        Some(CommandKind::Build)
    } else {
        None
    }
}

// --- Python ------------------------------------------------------------

fn scan_python(root: &Path, report: &mut DiscoveryReport) {
    let pyproject = read(root, "pyproject.toml");
    let has_py_project = pyproject.is_some()
        || exists(root, "setup.py")
        || exists(root, "setup.cfg")
        || exists(root, "tox.ini")
        || exists(root, "pytest.ini");
    if !has_py_project {
        return;
    }
    let src = CommandSource::Manifest {
        file: "pyproject.toml".into(),
    };
    report.push(VerifyCommand::new(
        CommandKind::Test,
        "pytest",
        Vec::<String>::new(),
        src.clone(),
    ));

    let py = pyproject.unwrap_or_default();
    if py.contains("[tool.ruff]") || exists(root, "ruff.toml") || exists(root, ".ruff.toml") {
        report.push(VerifyCommand::new(
            CommandKind::Lint,
            "ruff",
            ["check", "."],
            CommandSource::ConfigFile {
                file: "ruff.toml".into(),
            },
        ));
    }
    if py.contains("[tool.mypy]") || exists(root, "mypy.ini") || exists(root, ".mypy.ini") {
        report.push(VerifyCommand::new(
            CommandKind::Typecheck,
            "mypy",
            ["."],
            CommandSource::ConfigFile {
                file: "mypy.ini".into(),
            },
        ));
    }
    if py.contains("[tool.black]") {
        report.push(VerifyCommand::new(
            CommandKind::Format,
            "black",
            ["--check", "."],
            CommandSource::ConfigFile {
                file: "pyproject.toml".into(),
            },
        ));
    }
}

// --- Go --------------------------------------------------------------

fn scan_go(root: &Path, report: &mut DiscoveryReport) {
    if !exists(root, "go.mod") {
        return;
    }
    let src = CommandSource::Manifest {
        file: "go.mod".into(),
    };
    report.push(VerifyCommand::new(
        CommandKind::Build,
        "go",
        ["build", "./..."],
        src.clone(),
    ));
    report.push(VerifyCommand::new(
        CommandKind::Test,
        "go",
        ["test", "./..."],
        src.clone(),
    ));
    report.push(VerifyCommand::new(
        CommandKind::Lint,
        "go",
        ["vet", "./..."],
        src,
    ));
    report.push(VerifyCommand::new(
        CommandKind::Format,
        "gofmt",
        ["-l", "."],
        CommandSource::Manifest {
            file: "go.mod".into(),
        },
    ));
}

// --- Makefile / justfile --------------------------------------------

fn scan_make(root: &Path, report: &mut DiscoveryReport) {
    let Some(contents) = read(root, "Makefile").or_else(|| read(root, "makefile")) else {
        return;
    };
    for target in parse_make_targets(&contents) {
        if let Some(kind) = target_name_kind(&target) {
            report.push(VerifyCommand::new(
                kind,
                "make",
                [target.clone()],
                CommandSource::Script {
                    file: "Makefile".into(),
                    name: target,
                },
            ));
        }
    }
}

fn parse_make_targets(contents: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in contents.lines() {
        // A target line: `name:` at column 0, not a variable assignment,
        // not a `.PHONY` directive body.
        if line.starts_with([' ', '\t', '#']) {
            continue;
        }
        let Some((head, _)) = line.split_once(':') else {
            continue;
        };
        let head = head.trim();
        if head.is_empty() || head.contains('=') || head.starts_with('.') || head.contains(' ') {
            continue;
        }
        out.push(head.to_string());
    }
    out
}

fn scan_just(root: &Path, report: &mut DiscoveryReport) {
    let Some(contents) = read(root, "justfile").or_else(|| read(root, "Justfile")) else {
        return;
    };
    for recipe in parse_just_recipes(&contents) {
        if let Some(kind) = target_name_kind(&recipe) {
            report.push(VerifyCommand::new(
                kind,
                "just",
                [recipe.clone()],
                CommandSource::Script {
                    file: "justfile".into(),
                    name: recipe,
                },
            ));
        }
    }
}

fn parse_just_recipes(contents: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in contents.lines() {
        if line.starts_with([' ', '\t', '#', '@']) {
            continue;
        }
        let Some((head, _)) = line.split_once(':') else {
            continue;
        };
        // recipe head can carry params: `test args="":`
        let name = head.split_whitespace().next().unwrap_or("").trim();
        if name.is_empty() || name.contains('=') {
            continue;
        }
        out.push(name.to_string());
    }
    out
}

fn target_name_kind(name: &str) -> Option<CommandKind> {
    match name.to_ascii_lowercase().as_str() {
        "test" | "tests" | "check" => Some(CommandKind::Test),
        "lint" | "clippy" | "vet" => Some(CommandKind::Lint),
        "build" | "compile" => Some(CommandKind::Build),
        "fmt" | "format" | "format-check" | "fmt-check" => Some(CommandKind::Format),
        "typecheck" | "types" => Some(CommandKind::Typecheck),
        _ => None,
    }
}

// --- tool config files ---------------------------------------------

fn scan_config_files(root: &Path, report: &mut DiscoveryReport) {
    const ESLINT: &[&str] = &[
        ".eslintrc",
        ".eslintrc.js",
        ".eslintrc.cjs",
        ".eslintrc.json",
        ".eslintrc.yml",
        ".eslintrc.yaml",
        "eslint.config.js",
        "eslint.config.mjs",
    ];
    if ESLINT.iter().any(|f| exists(root, f)) && !has_command(report, "eslint") {
        report.push(VerifyCommand::new(
            CommandKind::Lint,
            "eslint",
            ["."],
            CommandSource::ConfigFile {
                file: ".eslintrc".into(),
            },
        ));
    }

    const PRETTIER: &[&str] = &[
        ".prettierrc",
        ".prettierrc.json",
        ".prettierrc.yml",
        ".prettierrc.yaml",
        ".prettierrc.js",
        "prettier.config.js",
    ];
    if PRETTIER.iter().any(|f| exists(root, f)) && !has_command(report, "prettier") {
        report.push(VerifyCommand::new(
            CommandKind::Format,
            "prettier",
            ["--check", "."],
            CommandSource::ConfigFile {
                file: ".prettierrc".into(),
            },
        ));
    }

    if exists(root, "tsconfig.json") && !has_kind(report, CommandKind::Typecheck) {
        report.push(VerifyCommand::new(
            CommandKind::Typecheck,
            "tsc",
            ["--noEmit"],
            CommandSource::ConfigFile {
                file: "tsconfig.json".into(),
            },
        ));
    }
}

fn has_command(report: &DiscoveryReport, program: &str) -> bool {
    report.candidates.iter().any(|c| c.program == program)
}

fn has_kind(report: &DiscoveryReport, kind: CommandKind) -> bool {
    report.candidates.iter().any(|c| c.kind == kind)
}

// --- repo-root conventions ---------------------------------------

fn scan_conventions(root: &Path, report: &mut DiscoveryReport) {
    for (file, kind) in [
        ("test.sh", CommandKind::Test),
        ("verify.sh", CommandKind::Test),
        ("scripts/test.sh", CommandKind::Test),
        ("check.sh", CommandKind::Test),
        ("lint.sh", CommandKind::Lint),
        ("build.sh", CommandKind::Build),
    ] {
        if exists(root, file) {
            report.push(VerifyCommand::new(
                kind,
                "sh",
                [file],
                CommandSource::Convention,
            ));
        }
    }
}

// --- CI workflows ------------------------------------------------

fn scan_ci_workflows(root: &Path, report: &mut DiscoveryReport) {
    let dir = root.join(".github").join("workflows");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    let mut files: Vec<_> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            matches!(
                p.extension().and_then(|e| e.to_str()),
                Some("yml") | Some("yaml")
            )
        })
        .collect();
    files.sort();

    for path in files {
        let Ok(contents) = std::fs::read_to_string(&path) else {
            continue;
        };
        let file = format!(
            ".github/workflows/{}",
            path.file_name().and_then(|n| n.to_str()).unwrap_or("?")
        );
        for run_line in extract_run_lines(&contents) {
            if let Some(cmd) = classify_ci_command(&run_line, &file) {
                report.push(cmd);
            }
        }
    }
}

/// Pull the command text out of every `run:` step in a workflow file,
/// including the first line of a `run: |` block. Deliberately shallow —
/// full YAML parsing is not worth a dependency here, and CI `run:` steps
/// are overwhelmingly single commands.
fn extract_run_lines(contents: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_block: Option<usize> = None; // indent of the `run: |` key

    for raw in contents.lines() {
        let indent = raw.len() - raw.trim_start().len();
        let line = raw.trim();

        if let Some(block_indent) = in_block {
            if line.is_empty() {
                continue;
            }
            if indent > block_indent {
                push_command_words(line, &mut out);
                continue;
            }
            in_block = None;
        }

        // A workflow step is a YAML list item: `- run: ...`.
        let line = line.strip_prefix("- ").unwrap_or(line).trim();

        if let Some(rest) = line.strip_prefix("run:") {
            let rest = rest.trim();
            if rest == "|" || rest == ">" || rest.is_empty() || rest == "|-" || rest == ">-" {
                in_block = Some(indent);
            } else {
                push_command_words(rest, &mut out);
            }
        }
    }
    out
}

fn push_command_words(line: &str, out: &mut Vec<String>) {
    // Split a `a && b` / `a ; b` chain into individual commands.
    let normalized = line.replace("&&", ";").replace("||", ";");
    for part in normalized.split(';') {
        let part = part.trim().trim_start_matches("- ").trim();
        if !part.is_empty() {
            out.push(part.to_string());
        }
    }
}

fn classify_ci_command(line: &str, file: &str) -> Option<VerifyCommand> {
    let words = shell_words(line);
    if words.is_empty() {
        return None;
    }
    let program = words[0].clone();
    let args: Vec<String> = words[1..].to_vec();
    let joined = line.to_ascii_lowercase();

    let kind = if joined.contains("clippy") || program == "eslint" || joined.contains(" vet") {
        CommandKind::Lint
    } else if joined.contains("fmt") || joined.contains("prettier") || joined.contains("--check") {
        CommandKind::Format
    } else if program == "mypy" || joined.contains("tsc") || joined.contains("typecheck") {
        CommandKind::Typecheck
    } else if joined.contains("test") || program == "pytest" || joined.contains("go test") {
        CommandKind::Test
    } else if joined.contains("build") {
        CommandKind::Build
    } else {
        return None;
    };

    // Only trust commands whose program we recognize as a real tool
    // runner — skips `echo`, `cd`, `actions/checkout` shims, etc.
    const KNOWN: &[&str] = &[
        "cargo", "go", "npm", "pnpm", "yarn", "bun", "pytest", "python", "python3", "make", "just",
        "mypy", "ruff", "tsc", "eslint", "prettier", "gofmt", "sh", "bash",
    ];
    if !KNOWN.contains(&program.as_str()) {
        return None;
    }

    Some(VerifyCommand::new(
        kind,
        program,
        args,
        CommandSource::CiWorkflow { file: file.into() },
    ))
}

/// Minimal shell-ish word split: whitespace, honoring single and double
/// quotes. Not a full parser — CI `run:` lines are simple, and anything
/// exotic just falls through to "not recognized".
fn shell_words(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for ch in line.chars() {
        match quote {
            Some(q) if ch == q => quote = None,
            Some(_) => cur.push(ch),
            None if ch == '"' || ch == '\'' => quote = Some(ch),
            None if ch.is_whitespace() => {
                if !cur.is_empty() {
                    words.push(std::mem::take(&mut cur));
                }
            }
            None => cur.push(ch),
        }
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    words
}

// --- validation by execution -------------------------------------

/// Runs a cheap probe of a program to confirm it exists and is runnable
/// on this machine. Injectable so discovery's tests never depend on which
/// toolchains happen to be installed on the test host.
#[async_trait]
pub trait ProbeRunner: Send + Sync {
    /// Run `program` with `args` (a version/help flag) and report whether
    /// it launched and exited without a spawn error. A non-zero exit is
    /// still "the program exists".
    async fn probe(&self, program: &str, args: &[String]) -> ProbeOutcome;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// The program launched (any exit code).
    Runnable,
    /// The program could not be spawned (not on PATH, not executable).
    Missing { reason: String },
}

/// The outcome of confirming a discovery report against a real machine.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ValidatedTooling {
    pub validated: Vec<VerifyCommand>,
    pub rejected: Vec<(VerifyCommand, String)>,
}

impl ValidatedTooling {
    pub fn by_kind(&self, kind: CommandKind) -> impl Iterator<Item = &VerifyCommand> {
        self.validated.iter().filter(move |c| c.kind == kind)
    }

    pub fn best(&self, kind: CommandKind) -> Option<&VerifyCommand> {
        self.by_kind(kind).max_by_key(|c| c.source.confidence())
    }

    pub fn contains(&self, command: &VerifyCommand) -> bool {
        self.validated
            .iter()
            .any(|c| c.identity() == command.identity())
    }
}

/// The probe args used to confirm a given program is present. `sh`/`bash`
/// take `-c true`; everything else takes a version flag.
fn probe_args(program: &str) -> Vec<String> {
    match program {
        "sh" | "bash" => vec!["-c".into(), "true".into()],
        "gofmt" => vec!["-h".into()],
        "make" => vec!["--version".into()],
        _ => vec!["--version".into()],
    }
}

/// Confirm every candidate in `report` against `runner`, deduplicating
/// probes by program (probing `cargo` once covers `cargo test`, `cargo
/// clippy`, …).
pub async fn validate<R: ProbeRunner + ?Sized>(
    report: &DiscoveryReport,
    runner: &R,
) -> ValidatedTooling {
    let mut program_outcomes: BTreeMap<String, ProbeOutcome> = BTreeMap::new();
    let mut out = ValidatedTooling::default();

    for command in &report.candidates {
        let outcome = if let Some(cached) = program_outcomes.get(&command.program) {
            cached.clone()
        } else {
            let outcome = runner
                .probe(&command.program, &probe_args(&command.program))
                .await;
            program_outcomes.insert(command.program.clone(), outcome.clone());
            outcome
        };
        match outcome {
            ProbeOutcome::Runnable => out.validated.push(command.clone()),
            ProbeOutcome::Missing { reason } => out.rejected.push((command.clone(), reason)),
        }
    }
    out
}

/// The real [`ProbeRunner`]: spawns `program <version-flag>` in `cwd`
/// with a short timeout. Anything that launches — even with a non-zero
/// exit — counts as present; only a spawn failure means missing.
#[derive(Debug)]
pub struct ProcessProbeRunner {
    cwd: std::path::PathBuf,
    timeout: std::time::Duration,
}

impl ProcessProbeRunner {
    pub fn new(cwd: impl Into<std::path::PathBuf>) -> Self {
        Self {
            cwd: cwd.into(),
            timeout: std::time::Duration::from_secs(10),
        }
    }
}

#[async_trait]
impl ProbeRunner for ProcessProbeRunner {
    async fn probe(&self, program: &str, args: &[String]) -> ProbeOutcome {
        let env = valyria_process::EnvPolicy::inherit_filtered().build(&std::env::vars().collect());
        let spec = valyria_process::CommandSpec::new(program, self.cwd.clone())
            .args(args.to_vec())
            .env(env)
            .timeout(self.timeout);
        match valyria_process::run(&spec, valyria_util::CancellationToken::new()).await {
            Ok(_) => ProbeOutcome::Runnable,
            Err(e) => ProbeOutcome::Missing {
                reason: e.to_string(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use valyria_testkit::TempWorkspace;

    struct AllPresent;
    #[async_trait]
    impl ProbeRunner for AllPresent {
        async fn probe(&self, _program: &str, _args: &[String]) -> ProbeOutcome {
            ProbeOutcome::Runnable
        }
    }

    struct OnlyPresent(&'static [&'static str]);
    #[async_trait]
    impl ProbeRunner for OnlyPresent {
        async fn probe(&self, program: &str, _args: &[String]) -> ProbeOutcome {
            if self.0.contains(&program) {
                ProbeOutcome::Runnable
            } else {
                ProbeOutcome::Missing {
                    reason: format!("{program} not found"),
                }
            }
        }
    }

    #[test]
    fn cargo_manifest_yields_the_four_kinds() {
        let ws = TempWorkspace::new();
        ws.write("Cargo.toml", "[package]\nname = \"x\"\n");
        let report = scan(ws.path());
        for kind in [
            CommandKind::Build,
            CommandKind::Test,
            CommandKind::Lint,
            CommandKind::Format,
        ] {
            assert!(
                report.by_kind(kind).next().is_some(),
                "missing {kind} for a cargo project"
            );
        }
        let test = report.best(CommandKind::Test).unwrap();
        assert_eq!(test.program, "cargo");
        assert_eq!(test.args, vec!["test"]);
    }

    #[test]
    fn cargo_workspace_manifest_adds_workspace_flag() {
        let ws = TempWorkspace::new();
        ws.write("Cargo.toml", "[workspace]\nmembers = []\n");
        let report = scan(ws.path());
        let test = report.best(CommandKind::Test).unwrap();
        assert_eq!(test.args, vec!["test", "--workspace"]);
    }

    #[test]
    fn package_json_scripts_map_to_kinds_with_the_right_runner() {
        let ws = TempWorkspace::new();
        ws.write(
            "package.json",
            r#"{"scripts":{"test":"jest","lint":"eslint .","build":"tsc"}}"#,
        );
        ws.write("pnpm-lock.yaml", "");
        let report = scan(ws.path());
        let test = report.best(CommandKind::Test).unwrap();
        assert_eq!(test.program, "pnpm");
        assert_eq!(test.args, vec!["run", "test"]);
        assert!(report.by_kind(CommandKind::Lint).next().is_some());
    }

    #[test]
    fn go_module_yields_test_build_vet() {
        let ws = TempWorkspace::new();
        ws.write("go.mod", "module example.com/x\n\ngo 1.21\n");
        let report = scan(ws.path());
        assert_eq!(
            report.best(CommandKind::Test).unwrap().args,
            vec!["test", "./..."]
        );
        assert_eq!(
            report.best(CommandKind::Build).unwrap().args,
            vec!["build", "./..."]
        );
    }

    #[test]
    fn makefile_targets_become_make_invocations() {
        let ws = TempWorkspace::new();
        ws.write(
            "Makefile",
            "VERSION = 1\n\ntest:\n\tcargo test\n\nlint:\n\tcargo clippy\n\ndocs:\n\tmdbook build\n",
        );
        let report = scan(ws.path());
        let test = report.best(CommandKind::Test).unwrap();
        assert_eq!(test.program, "make");
        assert_eq!(test.args, vec!["test"]);
        // `docs` is not a recognized verification target.
        assert!(report
            .candidates
            .iter()
            .all(|c| c.args != vec!["docs".to_string()]));
    }

    #[test]
    fn justfile_recipes_are_discovered() {
        let ws = TempWorkspace::new();
        ws.write(
            "justfile",
            "test:\n    cargo test\n\nci: test\n    echo done\n",
        );
        let report = scan(ws.path());
        assert_eq!(report.best(CommandKind::Test).unwrap().program, "just");
    }

    #[test]
    fn script_convention_is_portable_sh_invocation() {
        let ws = TempWorkspace::new();
        ws.write("test.sh", "#!/bin/sh\nexit 0\n");
        let report = scan(ws.path());
        let test = report.best(CommandKind::Test).unwrap();
        assert_eq!(test.program, "sh");
        assert_eq!(test.args, vec!["test.sh"]);
        assert_eq!(test.source, CommandSource::Convention);
    }

    #[test]
    fn ci_workflow_run_steps_are_extracted_and_outrank_manifest() {
        let ws = TempWorkspace::new();
        ws.write("Cargo.toml", "[package]\nname=\"x\"\n");
        ws.mkdir(".github/workflows");
        ws.write(
            ".github/workflows/ci.yml",
            "jobs:\n  build:\n    steps:\n      - run: cargo test --all-features\n      - run: |\n          cargo clippy --workspace\n          echo done\n",
        );
        let report = scan(ws.path());
        let test = report.best(CommandKind::Test).unwrap();
        assert!(matches!(test.source, CommandSource::CiWorkflow { .. }));
        assert_eq!(test.args, vec!["test", "--all-features"]);
        let lint = report.best(CommandKind::Lint).unwrap();
        assert!(matches!(lint.source, CommandSource::CiWorkflow { .. }));
    }

    #[test]
    fn ci_workflow_ignores_non_tool_run_lines() {
        let ws = TempWorkspace::new();
        ws.mkdir(".github/workflows");
        ws.write(
            ".github/workflows/ci.yml",
            "steps:\n  - run: echo hello world\n  - run: cd frontend\n",
        );
        let report = scan(ws.path());
        assert!(report.is_empty());
    }

    #[test]
    fn empty_workspace_reports_nothing_with_a_note() {
        let ws = TempWorkspace::new();
        let report = scan(ws.path());
        assert!(report.is_empty());
        assert!(!report.notes.is_empty());
    }

    #[tokio::test]
    async fn validation_keeps_present_tools_and_rejects_missing_ones() {
        let ws = TempWorkspace::new();
        ws.write("Cargo.toml", "[package]\nname=\"x\"\n");
        ws.write("package.json", r#"{"scripts":{"test":"jest"}}"#);
        let report = scan(ws.path());

        let validated = validate(&report, &OnlyPresent(&["cargo"])).await;
        assert!(validated
            .by_kind(CommandKind::Test)
            .any(|c| c.program == "cargo"));
        assert!(validated.rejected.iter().any(|(c, _)| c.program == "npm"));
    }

    #[tokio::test]
    async fn validation_probes_each_program_only_once() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        struct Counting(Arc<AtomicUsize>);
        #[async_trait]
        impl ProbeRunner for Counting {
            async fn probe(&self, _p: &str, _a: &[String]) -> ProbeOutcome {
                self.0.fetch_add(1, Ordering::SeqCst);
                ProbeOutcome::Runnable
            }
        }

        let ws = TempWorkspace::new();
        ws.write("Cargo.toml", "[package]\nname=\"x\"\n"); // 4 cargo commands
        let report = scan(ws.path());
        let count = Arc::new(AtomicUsize::new(0));
        validate(&report, &Counting(count.clone())).await;
        // cargo + gofmt-free: only `cargo` and (format) `cargo` again →
        // one distinct program.
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn all_present_validates_everything() {
        let ws = TempWorkspace::new();
        ws.write("go.mod", "module x\n");
        let report = scan(ws.path());
        let validated = validate(&report, &AllPresent).await;
        assert_eq!(validated.rejected.len(), 0);
        assert!(!validated.validated.is_empty());
    }
}
