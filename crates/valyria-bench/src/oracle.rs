//! Executable oracles (§4.30). An oracle answers one question — "did the
//! task actually achieve its objective?" — from evidence on disk and the
//! completion report, never from the model's own say-so (D4). The most
//! important one is [`CommandSucceeds`]: it runs a real command in the
//! finished workspace and passes iff it exits 0 ("the tests pass").

use std::path::{Path, PathBuf};
use std::process::Command;

use valyria_types::AgentState;
use valyria_verify::{CompletionReport, ReportStatus};

/// The result of one oracle check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OracleVerdict {
    pub passed: bool,
    pub detail: String,
}

impl OracleVerdict {
    pub fn pass(detail: impl Into<String>) -> Self {
        Self {
            passed: true,
            detail: detail.into(),
        }
    }
    pub fn fail(detail: impl Into<String>) -> Self {
        Self {
            passed: false,
            detail: detail.into(),
        }
    }
}

/// Everything an oracle is allowed to look at.
pub struct OracleContext<'a> {
    pub workspace: &'a Path,
    pub report: &'a CompletionReport,
    pub final_state: AgentState,
    /// Workspace-relative paths whose content hash changed over the run.
    pub files_changed: &'a [String],
}

impl OracleContext<'_> {
    fn read(&self, rel: &str) -> Option<String> {
        std::fs::read_to_string(self.workspace.join(rel)).ok()
    }
}

pub trait Oracle: Send + Sync {
    fn name(&self) -> String;
    fn check(&self, ctx: &OracleContext<'_>) -> OracleVerdict;
}

// --- concrete oracles -------------------------------------------------

/// The task reached `COMPLETED` (not `FAILED` / `CANCELLED` / stuck).
pub struct TaskCompleted;
impl Oracle for TaskCompleted {
    fn name(&self) -> String {
        "task_completed".into()
    }
    fn check(&self, ctx: &OracleContext<'_>) -> OracleVerdict {
        if ctx.final_state == AgentState::Completed {
            OracleVerdict::pass("task reached COMPLETED")
        } else {
            OracleVerdict::fail(format!("final state was {}", ctx.final_state))
        }
    }
}

/// The completion report's status is `Verified` — a broad test-tier run
/// passed and nothing in the log is failing.
pub struct ReportVerified;
impl Oracle for ReportVerified {
    fn name(&self) -> String {
        "report_verified".into()
    }
    fn check(&self, ctx: &OracleContext<'_>) -> OracleVerdict {
        match ctx.report.status {
            ReportStatus::Verified => OracleVerdict::pass("completion report: Verified"),
            other => OracleVerdict::fail(format!("completion report: {other:?}")),
        }
    }
}

/// `path` exists and contains `needle`.
pub struct FileContains {
    pub path: String,
    pub needle: String,
}
impl FileContains {
    pub fn new(path: impl Into<String>, needle: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            needle: needle.into(),
        }
    }
}
impl Oracle for FileContains {
    fn name(&self) -> String {
        format!("file_contains({}, {:?})", self.path, self.needle)
    }
    fn check(&self, ctx: &OracleContext<'_>) -> OracleVerdict {
        match ctx.read(&self.path) {
            None => OracleVerdict::fail(format!("{} does not exist", self.path)),
            Some(c) if c.contains(&self.needle) => {
                OracleVerdict::pass(format!("{} contains {:?}", self.path, self.needle))
            }
            Some(_) => OracleVerdict::fail(format!("{} lacks {:?}", self.path, self.needle)),
        }
    }
}

/// `path` exists and does **not** contain `needle` (e.g. the old symbol
/// name after a rename).
pub struct FileLacks {
    pub path: String,
    pub needle: String,
}
impl FileLacks {
    pub fn new(path: impl Into<String>, needle: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            needle: needle.into(),
        }
    }
}
impl Oracle for FileLacks {
    fn name(&self) -> String {
        format!("file_lacks({}, {:?})", self.path, self.needle)
    }
    fn check(&self, ctx: &OracleContext<'_>) -> OracleVerdict {
        match ctx.read(&self.path) {
            None => OracleVerdict::fail(format!("{} does not exist", self.path)),
            Some(c) if c.contains(&self.needle) => {
                OracleVerdict::fail(format!("{} still contains {:?}", self.path, self.needle))
            }
            Some(_) => OracleVerdict::pass(format!("{} free of {:?}", self.path, self.needle)),
        }
    }
}

/// `path` exists at all.
pub struct FileExists {
    pub path: String,
}
impl FileExists {
    pub fn new(path: impl Into<String>) -> Self {
        Self { path: path.into() }
    }
}
impl Oracle for FileExists {
    fn name(&self) -> String {
        format!("file_exists({})", self.path)
    }
    fn check(&self, ctx: &OracleContext<'_>) -> OracleVerdict {
        if ctx.workspace.join(&self.path).exists() {
            OracleVerdict::pass(format!("{} exists", self.path))
        } else {
            OracleVerdict::fail(format!("{} missing", self.path))
        }
    }
}

/// At most `max` files changed over the run — a diff-size constraint.
pub struct MaxFilesChanged {
    pub max: usize,
}
impl Oracle for MaxFilesChanged {
    fn name(&self) -> String {
        format!("max_files_changed({})", self.max)
    }
    fn check(&self, ctx: &OracleContext<'_>) -> OracleVerdict {
        let n = ctx.files_changed.len();
        if n <= self.max {
            OracleVerdict::pass(format!("{n} file(s) changed (≤ {})", self.max))
        } else {
            OracleVerdict::fail(format!(
                "{n} files changed (> {}): {:?}",
                self.max, ctx.files_changed
            ))
        }
    }
}

/// None of `paths` was modified — the task stayed out of areas it had no
/// business touching.
pub struct PathsUntouched {
    pub paths: Vec<String>,
}
impl PathsUntouched {
    pub fn new<I: IntoIterator<Item = S>, S: Into<String>>(paths: I) -> Self {
        Self {
            paths: paths.into_iter().map(Into::into).collect(),
        }
    }
}
impl Oracle for PathsUntouched {
    fn name(&self) -> String {
        format!("paths_untouched({:?})", self.paths)
    }
    fn check(&self, ctx: &OracleContext<'_>) -> OracleVerdict {
        let hit: Vec<&String> = self
            .paths
            .iter()
            .filter(|p| ctx.files_changed.iter().any(|c| c == *p))
            .collect();
        if hit.is_empty() {
            OracleVerdict::pass("declared-off-limits paths untouched")
        } else {
            OracleVerdict::fail(format!("touched off-limits paths: {hit:?}"))
        }
    }
}

/// Run `program args...` in the finished workspace; pass iff it exits 0.
/// This is the executable oracle §4.30 is built around ("tests pass",
/// "specific tests newly pass"). It runs unsandboxed *on purpose* — it is
/// the grader, not the agent, and the workspace is a fixture the harness
/// itself authored.
pub struct CommandSucceeds {
    pub program: String,
    pub args: Vec<String>,
}
impl CommandSucceeds {
    pub fn new<I: IntoIterator<Item = S>, S: Into<String>>(
        program: impl Into<String>,
        args: I,
    ) -> Self {
        Self {
            program: program.into(),
            args: args.into_iter().map(Into::into).collect(),
        }
    }
}
impl Oracle for CommandSucceeds {
    fn name(&self) -> String {
        format!("command_succeeds({} {})", self.program, self.args.join(" "))
    }
    fn check(&self, ctx: &OracleContext<'_>) -> OracleVerdict {
        let workspace: PathBuf = ctx.workspace.to_path_buf();
        match Command::new(&self.program)
            .args(&self.args)
            .current_dir(&workspace)
            .output()
        {
            Ok(out) if out.status.success() => {
                OracleVerdict::pass(format!("`{}` exited 0", self.name()))
            }
            Ok(out) => OracleVerdict::fail(format!(
                "`{}` exited {:?}: {}",
                self.name(),
                out.status.code(),
                String::from_utf8_lossy(&out.stderr).trim()
            )),
            Err(e) => OracleVerdict::fail(format!("could not run `{}`: {e}", self.program)),
        }
    }
}

/// All sub-oracles must pass. The verdict detail concatenates each.
pub struct All(pub Vec<Box<dyn Oracle>>);

impl All {
    pub fn of(oracles: Vec<Box<dyn Oracle>>) -> Self {
        Self(oracles)
    }
}

impl Oracle for All {
    fn name(&self) -> String {
        let inner: Vec<String> = self.0.iter().map(|o| o.name()).collect();
        format!("all[{}]", inner.join(" & "))
    }
    fn check(&self, ctx: &OracleContext<'_>) -> OracleVerdict {
        let mut details = Vec::new();
        let mut all_passed = true;
        for o in &self.0 {
            let v = o.check(ctx);
            all_passed &= v.passed;
            details.push(format!(
                "[{}] {}",
                if v.passed { "ok" } else { "FAIL" },
                v.detail
            ));
        }
        OracleVerdict {
            passed: all_passed,
            detail: details.join("; "),
        }
    }
}

/// Sugar: `all![a, b, c]` → `All::of(vec![Box::new(a), ...])`.
#[macro_export]
macro_rules! all {
    ($($oracle:expr),+ $(,)?) => {
        $crate::oracle::All::of(vec![$(Box::new($oracle) as Box<dyn $crate::oracle::Oracle>),+])
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use valyria_types::TaskId;
    use valyria_verify::CompletionReport;

    fn ctx<'a>(
        ws: &'a Path,
        report: &'a CompletionReport,
        state: AgentState,
        changed: &'a [String],
    ) -> OracleContext<'a> {
        OracleContext {
            workspace: ws,
            report,
            final_state: state,
            files_changed: changed,
        }
    }

    fn empty_report() -> CompletionReport {
        CompletionReport::from_runs(TaskId::new(), &[], &[])
    }

    #[test]
    fn file_contains_and_lacks() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello world").unwrap();
        let rep = empty_report();
        let c = ctx(dir.path(), &rep, AgentState::Completed, &[]);

        assert!(FileContains::new("a.txt", "world").check(&c).passed);
        assert!(!FileContains::new("a.txt", "nope").check(&c).passed);
        assert!(!FileContains::new("missing.txt", "x").check(&c).passed);
        assert!(FileLacks::new("a.txt", "zzz").check(&c).passed);
        assert!(!FileLacks::new("a.txt", "hello").check(&c).passed);
    }

    #[test]
    fn task_completed_and_max_files_changed() {
        let dir = tempfile::tempdir().unwrap();
        let rep = empty_report();
        let changed = vec!["src/a.rs".to_string(), "src/b.rs".to_string()];

        assert!(
            TaskCompleted
                .check(&ctx(dir.path(), &rep, AgentState::Completed, &[]))
                .passed
        );
        assert!(
            !TaskCompleted
                .check(&ctx(dir.path(), &rep, AgentState::Failed, &[]))
                .passed
        );
        assert!(
            MaxFilesChanged { max: 2 }
                .check(&ctx(dir.path(), &rep, AgentState::Completed, &changed))
                .passed
        );
        assert!(
            !MaxFilesChanged { max: 1 }
                .check(&ctx(dir.path(), &rep, AgentState::Completed, &changed))
                .passed
        );
    }

    #[test]
    fn paths_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let rep = empty_report();
        let changed = vec!["src/a.rs".to_string()];
        let c = ctx(dir.path(), &rep, AgentState::Completed, &changed);
        assert!(PathsUntouched::new(["Cargo.lock"]).check(&c).passed);
        assert!(!PathsUntouched::new(["src/a.rs"]).check(&c).passed);
    }

    #[test]
    fn command_succeeds_runs_in_the_workspace() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("marker"), "").unwrap();
        let rep = empty_report();
        let c = ctx(dir.path(), &rep, AgentState::Completed, &[]);
        #[cfg(unix)]
        {
            assert!(
                CommandSucceeds::new("test", ["-f", "marker"])
                    .check(&c)
                    .passed
            );
            assert!(
                !CommandSucceeds::new("test", ["-f", "absent"])
                    .check(&c)
                    .passed
            );
        }
        assert!(
            !CommandSucceeds::new("definitely-not-a-real-binary-xyz", ["x"])
                .check(&c)
                .passed
        );
    }

    #[test]
    fn all_requires_every_sub_oracle() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hi").unwrap();
        let rep = empty_report();
        let c = ctx(dir.path(), &rep, AgentState::Completed, &[]);

        let good = All::of(vec![
            Box::new(TaskCompleted),
            Box::new(FileContains::new("a.txt", "hi")),
        ]);
        assert!(good.check(&c).passed);

        let bad = All::of(vec![
            Box::new(TaskCompleted),
            Box::new(FileContains::new("a.txt", "bye")),
        ]);
        let v = bad.check(&c);
        assert!(!v.passed);
        assert!(v.detail.contains("FAIL"));
    }
}
