//! Failure parsing (§4.26 "Diagnosis"): turn a tool's raw output into a
//! small set of structured [`Failure`]s a diagnosis can reason about,
//! instead of pasting a wall of text into the model's context.
//!
//! Each parser is deliberately tolerant: real output carries colour
//! codes, interleaved stdout/stderr, progress bars and warnings mixed
//! with errors. A parser that finds nothing is not an error — the
//! dispatcher falls back to [`GenericParser`], and a run that exited
//! non-zero with no parsed failure still records that fact.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::command::{CommandKind, VerifyCommand};

/// A source location a failure points at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Location {
    pub file: PathBuf,
    pub line: Option<u32>,
    pub column: Option<u32>,
}

impl Location {
    pub fn new(file: impl Into<PathBuf>) -> Self {
        Self {
            file: file.into(),
            line: None,
            column: None,
        }
    }

    pub fn at(file: impl Into<PathBuf>, line: u32, column: u32) -> Self {
        Self {
            file: file.into(),
            line: Some(line),
            column: Some(column),
        }
    }
}

/// An `expected` vs `actual` extracted from an assertion failure, when the
/// tool's output makes them recoverable.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assertion {
    pub expected: Option<String>,
    pub actual: Option<String>,
    pub detail: Option<String>,
}

impl Assertion {
    fn is_empty(&self) -> bool {
        self.expected.is_none() && self.actual.is_none() && self.detail.is_none()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    /// The code did not compile / parse.
    CompileError,
    /// A test ran and its assertions did not hold.
    TestFailure,
    /// A test aborted (panic, uncaught exception, segfault).
    TestPanic,
    /// A linter rule fired.
    LintError,
    /// A type checker rejected the code.
    TypeError,
    /// A formatter would rewrite the code.
    FormatViolation,
    /// The command exceeded its time budget.
    Timeout,
    /// Non-zero exit with nothing more specific recovered.
    Unknown,
}

/// One distilled failure. `suspect_files` is left empty by the parsers —
/// `diagnose` fills it by cross-referencing with the change ledger and
/// the graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failure {
    pub kind: FailureKind,
    pub message: String,
    pub primary_location: Option<Location>,
    pub secondary_locations: Vec<Location>,
    pub assertion: Option<Assertion>,
    pub failing_test: Option<String>,
    pub suspect_files: Vec<PathBuf>,
}

impl Failure {
    pub fn new(kind: FailureKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            primary_location: None,
            secondary_locations: Vec::new(),
            assertion: None,
            failing_test: None,
            suspect_files: Vec::new(),
        }
    }

    fn with_location(mut self, loc: Location) -> Self {
        self.primary_location = Some(loc);
        self
    }

    fn with_test(mut self, test: impl Into<String>) -> Self {
        self.failing_test = Some(test.into());
        self
    }

    fn with_assertion(mut self, assertion: Assertion) -> Self {
        if !assertion.is_empty() {
            self.assertion = Some(assertion);
        }
        self
    }

    /// A short, stable string identifying "this same failure again",
    /// used by loop detection and the repair ledger. Deliberately drops
    /// line/column (they drift as edits are made) but keeps file, kind,
    /// test name and a normalized message head.
    pub fn fingerprint(&self) -> String {
        let file = self
            .primary_location
            .as_ref()
            .map(|l| l.file.to_string_lossy().into_owned())
            .unwrap_or_default();
        let test = self.failing_test.clone().unwrap_or_default();
        let msg_head: String = self
            .message
            .split_whitespace()
            .take(8)
            .collect::<Vec<_>>()
            .join(" ");
        format!("{:?}|{}|{}|{}", self.kind, file, test, msg_head)
    }
}

/// Raw command output handed to a parser.
#[derive(Debug, Clone)]
pub struct RawOutput<'a> {
    pub stdout: &'a str,
    pub stderr: &'a str,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
}

impl RawOutput<'_> {
    fn combined(&self) -> String {
        format!("{}\n{}", self.stdout, self.stderr)
    }
}

pub trait FailureParser {
    fn tool(&self) -> &'static str;
    fn parse(&self, raw: &RawOutput<'_>) -> Vec<Failure>;
}

/// Pick the parsers relevant to `command`, run them, and fall back to the
/// generic parser if the specific ones came up empty on a failing run.
pub fn parse_output(command: &VerifyCommand, raw: &RawOutput<'_>) -> Vec<Failure> {
    if raw.timed_out {
        return vec![Failure::new(
            FailureKind::Timeout,
            format!("`{}` exceeded its time budget", command.display()),
        )];
    }

    let mut failures = Vec::new();
    for parser in parsers_for(command) {
        failures.extend(parser.parse(raw));
        if !failures.is_empty() {
            break;
        }
    }

    if failures.is_empty() && raw.exit_code != Some(0) {
        failures = GenericParser.parse(raw);
    }
    if failures.is_empty() && raw.exit_code != Some(0) {
        failures.push(Failure::new(
            FailureKind::Unknown,
            format!(
                "`{}` exited with {}",
                command.display(),
                raw.exit_code
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "signal".into())
            ),
        ));
    }
    failures
}

fn parsers_for(command: &VerifyCommand) -> Vec<Box<dyn FailureParser>> {
    let prog = command.program.as_str();
    let joined = command.display().to_ascii_lowercase();
    let mut out: Vec<Box<dyn FailureParser>> = Vec::new();

    match command.kind {
        CommandKind::Test if prog == "cargo" || joined.contains("cargo test") => {
            out.push(Box::new(LibtestParser));
            out.push(Box::new(CargoParser));
        }
        CommandKind::Test if prog == "pytest" || joined.contains("pytest") => {
            out.push(Box::new(PytestParser))
        }
        CommandKind::Test if prog == "go" || joined.contains("go test") => {
            out.push(Box::new(GoTestParser))
        }
        CommandKind::Test => {
            // npm/yarn/pnpm/jest/vitest and script conventions.
            out.push(Box::new(JestParser));
            out.push(Box::new(PytestParser));
            out.push(Box::new(LibtestParser));
        }
        CommandKind::Build => {
            out.push(Box::new(CargoParser));
            out.push(Box::new(GoBuildParser));
            out.push(Box::new(TscParser));
        }
        CommandKind::Typecheck => {
            out.push(Box::new(TscParser));
            out.push(Box::new(MypyParser));
        }
        CommandKind::Lint => {
            out.push(Box::new(CargoParser));
            out.push(Box::new(EslintParser));
        }
        CommandKind::Format => out.push(Box::new(FormatParser)),
    }
    out
}

// --- cargo / rustc (JSON and human) ----------------------------------

#[derive(Debug)]
pub struct CargoParser;

impl FailureParser for CargoParser {
    fn tool(&self) -> &'static str {
        "cargo"
    }

    fn parse(&self, raw: &RawOutput<'_>) -> Vec<Failure> {
        let mut out = self.parse_json(raw);
        if out.is_empty() {
            out = self.parse_human(&raw.combined());
        }
        out
    }
}

impl CargoParser {
    fn parse_json(&self, raw: &RawOutput<'_>) -> Vec<Failure> {
        let mut out = Vec::new();
        for line in raw.stdout.lines().chain(raw.stderr.lines()) {
            let line = line.trim();
            if !line.starts_with('{') {
                continue;
            }
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            if v.get("reason").and_then(|r| r.as_str()) != Some("compiler-message") {
                continue;
            }
            let msg = &v["message"];
            if msg.get("level").and_then(|l| l.as_str()) != Some("error") {
                continue;
            }
            let text = msg
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("compile error")
                .to_string();
            let mut failure = Failure::new(FailureKind::CompileError, text);
            if let Some(spans) = msg.get("spans").and_then(|s| s.as_array()) {
                let primary = spans
                    .iter()
                    .find(|s| s.get("is_primary").and_then(|b| b.as_bool()) == Some(true))
                    .or_else(|| spans.first());
                if let Some(span) = primary {
                    if let Some(loc) = span_to_location(span) {
                        failure.primary_location = Some(loc);
                    }
                }
                for span in spans.iter().skip(1) {
                    if let Some(loc) = span_to_location(span) {
                        failure.secondary_locations.push(loc);
                    }
                }
            }
            out.push(failure);
        }
        out
    }

    fn parse_human(&self, text: &str) -> Vec<Failure> {
        let mut out = Vec::new();
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            let t = line.trim_start();
            let is_err = t.starts_with("error[") || t == "error" || t.starts_with("error:");
            if !is_err {
                continue;
            }
            let message = t.trim_start_matches("error").trim_start_matches(':').trim();
            let mut failure = Failure::new(FailureKind::CompileError, message);
            // The next `  --> path:line:col` line, if any.
            for follow in lines.iter().skip(i + 1).take(4) {
                if let Some(loc) = parse_arrow_location(follow) {
                    failure.primary_location = Some(loc);
                    break;
                }
            }
            out.push(failure);
        }
        out
    }
}

fn span_to_location(span: &serde_json::Value) -> Option<Location> {
    let file = span.get("file_name").and_then(|f| f.as_str())?;
    let line = span
        .get("line_start")
        .and_then(|l| l.as_u64())
        .map(|l| l as u32);
    let col = span
        .get("column_start")
        .and_then(|c| c.as_u64())
        .map(|c| c as u32);
    Some(Location {
        file: PathBuf::from(file),
        line,
        column: col,
    })
}

/// `  --> src/lib.rs:12:5`
fn parse_arrow_location(line: &str) -> Option<Location> {
    let rest = line.trim_start().strip_prefix("-->")?.trim();
    parse_path_line_col(rest)
}

/// `path:line:col` or `path:line` → Location.
fn parse_path_line_col(s: &str) -> Option<Location> {
    let s = s.trim();
    let mut parts = s.rsplitn(3, ':');
    let third = parts.next()?;
    let second = parts.next();
    let first = parts.next();
    match (first, second, third.parse::<u32>().ok()) {
        // path:line:col
        (Some(path), Some(line), Some(col)) => {
            if let Ok(line) = line.parse::<u32>() {
                return Some(Location::at(path, line, col));
            }
            None
        }
        // path:line   (third parsed as col but there's no first)
        (None, Some(path), Some(line)) => Some(Location {
            file: PathBuf::from(path),
            line: Some(line),
            column: None,
        }),
        _ => None,
    }
}

// --- rust libtest (`cargo test` runner output) ---------------------

#[derive(Debug)]
pub struct LibtestParser;

impl FailureParser for LibtestParser {
    fn tool(&self) -> &'static str {
        "libtest"
    }

    fn parse(&self, raw: &RawOutput<'_>) -> Vec<Failure> {
        let text = raw.combined();
        let lines: Vec<&str> = text.lines().collect();
        let mut out = Vec::new();

        for (i, line) in lines.iter().enumerate() {
            let t = line.trim();
            // `test tests::foo ... FAILED`
            let Some(name) = t
                .strip_prefix("test ")
                .and_then(|r| r.strip_suffix(" ... FAILED"))
            else {
                continue;
            };
            let name = name.trim();
            let mut failure =
                Failure::new(FailureKind::TestFailure, format!("test `{name}` failed"))
                    .with_test(name);

            // Look for the matching `---- <name> stdout ----` block and a
            // panic line within it.
            if let Some(block_start) = lines.iter().position(|l| {
                l.trim().starts_with("---- ") && l.contains(name) && l.contains("stdout")
            }) {
                for bl in lines.iter().skip(block_start + 1).take(12) {
                    let bt = bl.trim();
                    if bt.starts_with("---- ") {
                        break;
                    }
                    if let Some(p) = parse_rust_panic(bt) {
                        if let Some(loc) = p.1 {
                            failure.primary_location = Some(loc);
                        }
                        failure.message = format!("test `{name}` panicked: {}", p.0);
                        failure.kind = FailureKind::TestPanic;
                    }
                    if let Some(a) = parse_rust_assert(bt) {
                        failure = failure.with_assertion(a);
                    }
                }
            }
            let _ = i;
            out.push(failure);
        }

        // `test result: FAILED. 1 passed; 2 failed` with no per-test line
        // parsed (e.g. output truncation) — still record something.
        if out.is_empty() && lines.iter().any(|l| l.contains("test result: FAILED")) {
            out.push(Failure::new(
                FailureKind::TestFailure,
                "one or more tests failed (per-test detail not recovered)",
            ));
        }
        out
    }
}

/// `thread 'x' panicked at 'msg', src/lib.rs:42:9`  (older format) or
/// `thread 'x' panicked at src/lib.rs:42:9:` then message on next line
/// (1.72+). Returns `(message, location)`.
fn parse_rust_panic(line: &str) -> Option<(String, Option<Location>)> {
    let rest = line.strip_prefix("thread ")?;
    let rest = rest.split_once("panicked at ")?.1;
    // new format: `src/lib.rs:42:9:`
    if let Some(loc_str) = rest.strip_suffix(':') {
        if let Some(loc) = parse_path_line_col(loc_str) {
            return Some((String::new(), Some(loc)));
        }
    }
    // old format: `'msg', src/lib.rs:42:9`
    if let Some((msg, loc)) = rest.rsplit_once(", ") {
        let msg = msg.trim().trim_matches('\'').to_string();
        return Some((msg, parse_path_line_col(loc)));
    }
    Some((rest.trim().to_string(), None))
}

/// `assertion `left == right` failed` followed by `  left: `1``, `  right:
/// `2``, collapsed here from whatever single line we were handed.
fn parse_rust_assert(line: &str) -> Option<Assertion> {
    if let Some(rest) = line.strip_prefix("left: ") {
        return Some(Assertion {
            actual: Some(rest.trim().to_string()),
            ..Default::default()
        });
    }
    if let Some(rest) = line.strip_prefix("right: ") {
        return Some(Assertion {
            expected: Some(rest.trim().to_string()),
            ..Default::default()
        });
    }
    if line.starts_with("assertion ") && line.contains("failed") {
        return Some(Assertion {
            detail: Some(line.to_string()),
            ..Default::default()
        });
    }
    None
}

// --- pytest --------------------------------------------------------

#[derive(Debug)]
pub struct PytestParser;

impl FailureParser for PytestParser {
    fn tool(&self) -> &'static str {
        "pytest"
    }

    fn parse(&self, raw: &RawOutput<'_>) -> Vec<Failure> {
        let text = raw.combined();
        let mut out = Vec::new();

        for line in text.lines() {
            let t = line.trim();
            // Summary line: `FAILED tests/test_x.py::test_y - AssertionError: ...`
            let Some(rest) = t.strip_prefix("FAILED ") else {
                continue;
            };
            let (nodeid, detail) = match rest.split_once(" - ") {
                Some((a, b)) => (a.trim(), Some(b.trim().to_string())),
                None => (rest.trim(), None),
            };
            let (file, test) = match nodeid.split_once("::") {
                Some((f, t)) => (Some(f.to_string()), Some(t.replace("::", "."))),
                None => (None, None),
            };
            let mut failure = Failure::new(
                FailureKind::TestFailure,
                detail.clone().unwrap_or_else(|| format!("{nodeid} failed")),
            );
            failure.failing_test = test;
            if let Some(f) = file {
                failure.primary_location = Some(Location::new(f));
            }
            if let Some(d) = detail {
                failure.assertion = Some(Assertion {
                    detail: Some(d),
                    ..Default::default()
                });
            }
            out.push(failure);
        }

        // `path:line: AssertionError` traceback tails add locations to the
        // matching failure when the summary didn't carry a line.
        for line in text.lines() {
            if let Some(loc) = pytest_traceback_location(line.trim()) {
                if let Some(f) = out.iter_mut().find(|f| {
                    f.primary_location
                        .as_ref()
                        .map(|l| l.line.is_none() && loc.file.ends_with(&l.file))
                        .unwrap_or(false)
                }) {
                    f.primary_location = Some(loc);
                }
            }
        }
        out
    }
}

/// `tests/test_x.py:12: AssertionError`
fn pytest_traceback_location(line: &str) -> Option<Location> {
    let (path_line, tail) = line.split_once(": ")?;
    if !tail
        .chars()
        .next()
        .map(|c| c.is_ascii_uppercase())
        .unwrap_or(false)
    {
        return None;
    }
    let (path, line_no) = path_line.rsplit_once(':')?;
    Some(Location {
        file: PathBuf::from(path),
        line: line_no.trim().parse().ok(),
        column: None,
    })
}

// --- go test / go build -----------------------------------------

#[derive(Debug)]
pub struct GoTestParser;

impl FailureParser for GoTestParser {
    fn tool(&self) -> &'static str {
        "go test"
    }

    fn parse(&self, raw: &RawOutput<'_>) -> Vec<Failure> {
        let text = raw.combined();
        let lines: Vec<&str> = text.lines().collect();
        let mut out = Vec::new();

        for (i, line) in lines.iter().enumerate() {
            let t = line.trim();
            let Some(rest) = t.strip_prefix("--- FAIL: ") else {
                continue;
            };
            let name = rest.split_whitespace().next().unwrap_or(rest).to_string();
            let mut failure =
                Failure::new(FailureKind::TestFailure, format!("test `{name}` failed"))
                    .with_test(&name);
            // Following indented `    foo_test.go:20: msg` lines.
            for follow in lines.iter().skip(i + 1).take(8) {
                let ft = follow.trim();
                if ft.starts_with("--- FAIL") || ft.starts_with("=== RUN") || ft == "FAIL" {
                    break;
                }
                if let Some((loc, msg)) = go_detail_line(ft) {
                    failure.primary_location = Some(loc);
                    failure.assertion = Some(Assertion {
                        detail: Some(msg),
                        ..Default::default()
                    });
                    break;
                }
            }
            out.push(failure);
        }

        // Build errors: `./foo.go:10:2: undefined: Bar` — only at column
        // zero, so an indented `foo_test.go:20: expected …` detail line
        // under a `--- FAIL` block is not double-counted as a compile
        // error.
        for line in &lines {
            if line.starts_with(char::is_whitespace) {
                continue;
            }
            if let Some(f) = go_build_error(line.trim()) {
                out.push(f);
            }
        }
        out
    }
}

#[derive(Debug)]
pub struct GoBuildParser;

impl FailureParser for GoBuildParser {
    fn tool(&self) -> &'static str {
        "go build"
    }

    fn parse(&self, raw: &RawOutput<'_>) -> Vec<Failure> {
        raw.combined()
            .lines()
            .filter_map(|l| go_build_error(l.trim()))
            .collect()
    }
}

fn go_detail_line(line: &str) -> Option<(Location, String)> {
    // `foo_test.go:20: expected 1, got 2`
    let (path_line, msg) = line.split_once(": ")?;
    let (path, rest) = path_line.split_once(':')?;
    if !path.ends_with(".go") {
        return None;
    }
    let (line_no, _col) = match rest.split_once(':') {
        Some((l, c)) => (l, Some(c)),
        None => (rest, None),
    };
    Some((
        Location {
            file: PathBuf::from(path),
            line: line_no.trim().parse().ok(),
            column: None,
        },
        msg.trim().to_string(),
    ))
}

fn go_build_error(line: &str) -> Option<Failure> {
    // `./foo.go:10:2: undefined: Bar`  or  `foo.go:10:2: ...`
    if !line.contains(".go:") {
        return None;
    }
    let (loc_str, msg) = line.split_once(": ")?;
    let loc = parse_path_line_col(loc_str.trim_start_matches("./"))?;
    Some(Failure::new(FailureKind::CompileError, msg.trim()).with_location(loc))
}

// --- jest / vitest --------------------------------------------

#[derive(Debug)]
pub struct JestParser;

impl FailureParser for JestParser {
    fn tool(&self) -> &'static str {
        "jest"
    }

    fn parse(&self, raw: &RawOutput<'_>) -> Vec<Failure> {
        let text = raw.combined();
        let lines: Vec<&str> = text.lines().collect();
        let mut out = Vec::new();

        for (i, line) in lines.iter().enumerate() {
            let t = line.trim();
            // `● Suite name › test name`  (bullet may be ● or ✕)
            let Some(rest) = t
                .strip_prefix("● ")
                .or_else(|| t.strip_prefix("✕ "))
                .or_else(|| t.strip_prefix("FAIL "))
            else {
                continue;
            };
            if rest.is_empty() {
                continue;
            }
            let test = rest.rsplit(" › ").next().unwrap_or(rest).trim().to_string();
            let mut failure =
                Failure::new(FailureKind::TestFailure, format!("`{rest}` failed")).with_test(&test);
            let mut assertion = Assertion::default();
            for follow in lines.iter().skip(i + 1).take(20) {
                let ft = follow.trim();
                if let Some(exp) = ft.strip_prefix("Expected: ") {
                    assertion.expected = Some(exp.trim().to_string());
                }
                if let Some(act) = ft.strip_prefix("Received: ") {
                    assertion.actual = Some(act.trim().to_string());
                }
                if let Some(loc) = jest_at_location(ft) {
                    failure.primary_location = Some(loc);
                    break;
                }
            }
            failure = failure.with_assertion(assertion);
            out.push(failure);
        }
        out
    }
}

/// `at Object.<anonymous> (src/foo.test.js:10:5)` or `at src/foo.test.js:10:5`
fn jest_at_location(line: &str) -> Option<Location> {
    let rest = line.strip_prefix("at ")?;
    let inside = if let Some(open) = rest.rfind('(') {
        rest[open + 1..].trim_end_matches(')')
    } else {
        rest
    };
    if inside.contains("node_modules") {
        return None;
    }
    parse_path_line_col(inside)
}

// --- tsc ------------------------------------------------------

#[derive(Debug)]
pub struct TscParser;

impl FailureParser for TscParser {
    fn tool(&self) -> &'static str {
        "tsc"
    }

    fn parse(&self, raw: &RawOutput<'_>) -> Vec<Failure> {
        let text = raw.combined();
        let mut out = Vec::new();
        for line in text.lines() {
            let t = line.trim();
            // `src/foo.ts(12,5): error TS2322: Type '...'`
            let Some((loc_part, rest)) = t.split_once("): error TS") else {
                continue;
            };
            let Some((path, rowcol)) = loc_part.split_once('(') else {
                continue;
            };
            let mut nums = rowcol.split(',');
            let line_no = nums.next().and_then(|n| n.trim().parse().ok());
            let col = nums.next().and_then(|n| n.trim().parse().ok());
            let message = rest
                .split_once(": ")
                .map(|(_code, m)| m)
                .unwrap_or(rest)
                .to_string();
            out.push(
                Failure::new(FailureKind::TypeError, message).with_location(Location {
                    file: PathBuf::from(path),
                    line: line_no,
                    column: col,
                }),
            );
        }
        out
    }
}

// --- mypy ---------------------------------------------------

#[derive(Debug)]
pub struct MypyParser;

impl FailureParser for MypyParser {
    fn tool(&self) -> &'static str {
        "mypy"
    }

    fn parse(&self, raw: &RawOutput<'_>) -> Vec<Failure> {
        let text = raw.combined();
        let mut out = Vec::new();
        for line in text.lines() {
            let t = line.trim();
            // `foo.py:10: error: Incompatible return value type ...`
            let Some((path_line, rest)) = t.split_once(": error: ") else {
                continue;
            };
            let (path, line_no) = match path_line.split_once(':') {
                Some((p, l)) => (p, l.trim().parse().ok()),
                None => (path_line, None),
            };
            out.push(
                Failure::new(FailureKind::TypeError, rest.trim()).with_location(Location {
                    file: PathBuf::from(path),
                    line: line_no,
                    column: None,
                }),
            );
        }
        out
    }
}

// --- eslint -----------------------------------------------

#[derive(Debug)]
pub struct EslintParser;

impl FailureParser for EslintParser {
    fn tool(&self) -> &'static str {
        "eslint"
    }

    fn parse(&self, raw: &RawOutput<'_>) -> Vec<Failure> {
        let text = raw.combined();
        let lines: Vec<&str> = text.lines().collect();
        let mut out = Vec::new();
        let mut current_file: Option<&str> = None;

        for line in &lines {
            let raw_line = line;
            let t = line.trim();
            if t.is_empty() {
                continue;
            }
            // A file header: an absolute or relative path on its own line.
            if (raw_line.starts_with('/')
                || raw_line.starts_with("./")
                || t.contains(".js")
                || t.contains(".ts"))
                && !t.starts_with(char::is_numeric)
                && t.split_whitespace().count() == 1
            {
                current_file = Some(t);
                continue;
            }
            // `  12:5  error  Message text  rule-name`
            let mut parts = t.split_whitespace();
            let rowcol = parts.next().unwrap_or("");
            let sev = parts.next().unwrap_or("");
            if sev != "error" {
                continue;
            }
            let Some((row, col)) = rowcol.split_once(':') else {
                continue;
            };
            let rest: Vec<&str> = parts.collect();
            let (message, rule) = match rest.split_last() {
                Some((rule, msg)) => (msg.join(" "), Some(*rule)),
                None => (String::new(), None),
            };
            let mut failure = Failure::new(
                FailureKind::LintError,
                match rule {
                    Some(r) => format!("{message} ({r})"),
                    None => message,
                },
            );
            if let Some(f) = current_file {
                failure.primary_location = Some(Location {
                    file: PathBuf::from(f),
                    line: row.parse().ok(),
                    column: col.parse().ok(),
                });
            }
            out.push(failure);
        }
        out
    }
}

// --- formatters ---------------------------------------------

#[derive(Debug)]
pub struct FormatParser;

impl FailureParser for FormatParser {
    fn tool(&self) -> &'static str {
        "formatter"
    }

    fn parse(&self, raw: &RawOutput<'_>) -> Vec<Failure> {
        if raw.exit_code == Some(0) {
            return Vec::new();
        }
        let text = raw.combined();
        let mut files = Vec::new();
        for line in text.lines() {
            let t = line.trim();
            // `Diff in /path/to/file.rs at line 3:` (rustfmt)
            if let Some(rest) = t.strip_prefix("Diff in ") {
                if let Some(path) = rest.split_whitespace().next() {
                    files.push(path.to_string());
                }
            }
            // gofmt / prettier --check: a bare filename per line.
            else if !t.is_empty()
                && !t.contains(' ')
                && (t.ends_with(".go")
                    || t.ends_with(".ts")
                    || t.ends_with(".js")
                    || t.ends_with(".rs"))
            {
                files.push(t.to_string());
            }
        }
        let mut failure = Failure::new(
            FailureKind::FormatViolation,
            if files.is_empty() {
                "formatter would reformat some files".to_string()
            } else {
                format!("formatter would reformat {} file(s)", files.len())
            },
        );
        failure.secondary_locations = files.iter().map(Location::new).collect();
        failure.primary_location = files.first().map(Location::new);
        vec![failure]
    }
}

// --- generic fallback -------------------------------------

#[derive(Debug)]
pub struct GenericParser;

impl FailureParser for GenericParser {
    fn tool(&self) -> &'static str {
        "generic"
    }

    fn parse(&self, raw: &RawOutput<'_>) -> Vec<Failure> {
        if raw.exit_code == Some(0) {
            return Vec::new();
        }
        let text = raw.combined();
        // First line that looks like an error, else the last non-empty line.
        let error_line = text.lines().map(str::trim).find(|l| {
            let low = l.to_ascii_lowercase();
            low.starts_with("error")
                || low.contains("error:")
                || low.contains("exception")
                || low.contains("panic")
                || low.contains("failed")
        });
        let message = error_line
            .map(str::to_string)
            .or_else(|| {
                text.lines()
                    .map(str::trim)
                    .rev()
                    .find(|l| !l.is_empty())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "command failed with no diagnostic output".to_string());

        let mut failure = Failure::new(FailureKind::Unknown, message);
        // Any `path:line[:col]` token anywhere becomes a secondary loc.
        for line in text.lines() {
            for tok in line.split_whitespace() {
                let tok = tok.trim_matches(|c: char| "()[],\"'".contains(c));
                if tok.contains(':') && tok.contains('.') {
                    if let Some(loc) = parse_path_line_col(tok) {
                        if failure.primary_location.is_none() {
                            failure.primary_location = Some(loc);
                        } else if failure.secondary_locations.len() < 5 {
                            failure.secondary_locations.push(loc);
                        }
                    }
                }
            }
        }
        vec![failure]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::CommandSource;

    fn cmd(kind: CommandKind, program: &str, args: &[&str]) -> VerifyCommand {
        VerifyCommand::new(kind, program, args.to_vec(), CommandSource::Convention)
    }

    fn out<'a>(stdout: &'a str, stderr: &'a str, code: i32) -> RawOutput<'a> {
        RawOutput {
            stdout,
            stderr,
            exit_code: Some(code),
            timed_out: false,
        }
    }

    #[test]
    fn cargo_json_compile_error_gives_primary_location() {
        let json = r#"{"reason":"compiler-message","message":{"level":"error","message":"mismatched types","spans":[{"file_name":"src/lib.rs","line_start":10,"column_start":5,"is_primary":true}]}}"#;
        let failures = CargoParser.parse(&out(json, "", 101));
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].kind, FailureKind::CompileError);
        let loc = failures[0].primary_location.as_ref().unwrap();
        assert_eq!(loc.file, PathBuf::from("src/lib.rs"));
        assert_eq!(loc.line, Some(10));
        assert_eq!(loc.column, Some(5));
    }

    #[test]
    fn cargo_human_compile_error_reads_arrow_line() {
        let text = "error[E0308]: mismatched types\n  --> src/main.rs:4:20\n   |\n";
        let failures = CargoParser.parse(&out("", text, 101));
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].primary_location.as_ref().unwrap().line, Some(4));
    }

    #[test]
    fn libtest_failed_test_with_new_style_panic() {
        let text = "\
running 2 tests
test tests::adds ... ok
test tests::subtracts ... FAILED

failures:

---- tests::subtracts stdout ----
thread 'tests::subtracts' panicked at src/lib.rs:42:9:
assertion `left == right` failed
  left: 1
  right: 2

test result: FAILED. 1 passed; 1 failed
";
        let failures = LibtestParser.parse(&out(text, "", 101));
        assert_eq!(failures.len(), 1);
        let f = &failures[0];
        assert_eq!(f.failing_test.as_deref(), Some("tests::subtracts"));
        assert_eq!(f.kind, FailureKind::TestPanic);
        assert_eq!(f.primary_location.as_ref().unwrap().line, Some(42));
    }

    #[test]
    fn libtest_old_style_panic_message_and_loc() {
        let line = "thread 'x' panicked at 'boom', src/thing.rs:7:1";
        let (msg, loc) = parse_rust_panic(line).unwrap();
        assert_eq!(msg, "boom");
        assert_eq!(loc.unwrap().line, Some(7));
    }

    #[test]
    fn pytest_summary_line_parsed() {
        let text = "\
=================================== FAILURES ===================================
tests/test_math.py:8: AssertionError
=========================== short test summary info ============================
FAILED tests/test_math.py::test_add - assert 3 == 4
";
        let failures = PytestParser.parse(&out(text, "", 1));
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].failing_test.as_deref(), Some("test_add"));
        assert_eq!(
            failures[0].primary_location.as_ref().unwrap().file,
            PathBuf::from("tests/test_math.py")
        );
        assert_eq!(failures[0].primary_location.as_ref().unwrap().line, Some(8));
    }

    #[test]
    fn go_test_fail_with_detail_line() {
        let text = "\
=== RUN   TestAdd
--- FAIL: TestAdd (0.00s)
    math_test.go:15: expected 4, got 3
FAIL
FAIL	example.com/m	0.101s
";
        let failures = GoTestParser.parse(&out(text, "", 1));
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].failing_test.as_deref(), Some("TestAdd"));
        assert_eq!(
            failures[0].primary_location.as_ref().unwrap().line,
            Some(15)
        );
    }

    #[test]
    fn go_build_error_line() {
        let failures = GoBuildParser.parse(&out("", "./main.go:10:6: undefined: Foo\n", 1));
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].kind, FailureKind::CompileError);
        assert_eq!(
            failures[0].primary_location.as_ref().unwrap().file,
            PathBuf::from("main.go")
        );
    }

    #[test]
    fn jest_failure_with_expected_received_and_location() {
        let text = "\
  ● math › adds numbers

    expect(received).toBe(expected)

    Expected: 4
    Received: 3

      at Object.<anonymous> (src/math.test.js:6:19)
";
        let failures = JestParser.parse(&out(text, "", 1));
        assert_eq!(failures.len(), 1);
        let f = &failures[0];
        assert_eq!(f.failing_test.as_deref(), Some("adds numbers"));
        let a = f.assertion.as_ref().unwrap();
        assert_eq!(a.expected.as_deref(), Some("4"));
        assert_eq!(a.actual.as_deref(), Some("3"));
        assert_eq!(
            f.primary_location.as_ref().unwrap().file,
            PathBuf::from("src/math.test.js")
        );
    }

    #[test]
    fn tsc_error_line() {
        let text =
            "src/app.ts(12,5): error TS2322: Type 'string' is not assignable to type 'number'.";
        let failures = TscParser.parse(&out(text, "", 2));
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].kind, FailureKind::TypeError);
        let loc = failures[0].primary_location.as_ref().unwrap();
        assert_eq!(loc.line, Some(12));
        assert_eq!(loc.column, Some(5));
    }

    #[test]
    fn mypy_error_line() {
        let failures = MypyParser.parse(&out(
            "app.py:10: error: Incompatible return value type\n",
            "",
            1,
        ));
        assert_eq!(failures.len(), 1);
        assert_eq!(
            failures[0].primary_location.as_ref().unwrap().line,
            Some(10)
        );
    }

    #[test]
    fn eslint_stylish_output() {
        let text = "\
/repo/src/index.js
  3:10  error  'x' is assigned a value but never used  no-unused-vars
  5:1   error  Unexpected console statement             no-console
";
        let failures = EslintParser.parse(&out(text, "", 1));
        assert_eq!(failures.len(), 2);
        assert!(failures[0].message.contains("no-unused-vars"));
        assert_eq!(failures[0].primary_location.as_ref().unwrap().line, Some(3));
    }

    #[test]
    fn rustfmt_check_diff_lists_files() {
        let text = "Diff in /repo/src/lib.rs at line 3:\n-fn  x(){}\n+fn x() {}\n";
        let failures = FormatParser.parse(&out(text, "", 1));
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].kind, FailureKind::FormatViolation);
        assert_eq!(
            failures[0].primary_location.as_ref().unwrap().file,
            PathBuf::from("/repo/src/lib.rs")
        );
    }

    #[test]
    fn generic_parser_extracts_message_and_location() {
        let text =
            "Traceback (most recent call last):\n  File \"run.py\", line 3\nRuntimeError: nope\n";
        let failures = GenericParser.parse(&out("", text, 1));
        assert_eq!(failures.len(), 1);
        assert!(
            failures[0].message.to_lowercase().contains("error")
                || failures[0].message.contains("nope")
        );
    }

    #[test]
    fn generic_parser_is_silent_on_success() {
        assert!(GenericParser.parse(&out("all good", "", 0)).is_empty());
    }

    #[test]
    fn dispatch_falls_back_to_generic_on_unrecognized_failing_output() {
        let c = cmd(CommandKind::Test, "sh", &["test.sh"]);
        let failures = parse_output(&c, &out("", "boom: everything is on fire\n", 2));
        assert_eq!(failures.len(), 1);
        assert!(failures[0].message.contains("fire") || failures[0].kind == FailureKind::Unknown);
    }

    #[test]
    fn dispatch_reports_timeout() {
        let c = cmd(CommandKind::Test, "cargo", &["test"]);
        let raw = RawOutput {
            stdout: "",
            stderr: "",
            exit_code: None,
            timed_out: true,
        };
        let failures = parse_output(&c, &raw);
        assert_eq!(failures[0].kind, FailureKind::Timeout);
    }

    #[test]
    fn dispatch_on_success_returns_no_failures() {
        let c = cmd(CommandKind::Test, "cargo", &["test"]);
        assert!(parse_output(&c, &out("test result: ok. 3 passed", "", 0)).is_empty());
    }

    #[test]
    fn non_zero_exit_with_no_parse_still_yields_a_failure() {
        let c = cmd(CommandKind::Build, "cargo", &["build"]);
        let failures = parse_output(&c, &out("", "", 1));
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].kind, FailureKind::Unknown);
    }

    #[test]
    fn fingerprint_is_stable_across_line_moves() {
        let mut a = Failure::new(
            FailureKind::TestFailure,
            "assertion failed: values differ a lot here",
        )
        .with_test("tests::foo");
        a.primary_location = Some(Location::at("src/lib.rs", 10, 1));
        let mut b = a.clone();
        b.primary_location = Some(Location::at("src/lib.rs", 25, 4));
        assert_eq!(a.fingerprint(), b.fingerprint());
    }
}
