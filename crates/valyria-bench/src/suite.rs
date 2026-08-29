//! The built-in offline fixture suite. Every task here runs against the
//! deterministic fake model with the network down — it is the
//! orchestration regression guard §7 calls the "fake-model agent tests",
//! packaged as gradeable benchmark tasks with executable oracles.
//!
//! Coverage is one task per §4.30 category. The `dependency_work` task
//! edits a plain `deps.txt` rather than a real manifest **on purpose** —
//! a real `Cargo.toml` would make `Verifying` discover and run `cargo
//! test` against a throwaway crate, which is neither hermetic nor the
//! point of this suite.

use serde_json::json;
use valyria_runtime_fake::{Scenario, ScriptedTurn};

use crate::oracle::{
    CommandSucceeds, FileContains, FileExists, FileLacks, MaxFilesChanged, Oracle, ReportVerified,
    TaskCompleted,
};
use crate::task::{BenchTask, RepoSpec, TaskCategory};

fn tool(name: &str, arguments: serde_json::Value) -> ScriptedTurn {
    ScriptedTurn::ToolCall {
        name: name.into(),
        arguments,
    }
}

fn finish(summary: &str) -> ScriptedTurn {
    ScriptedTurn::Finish {
        summary: summary.into(),
    }
}

fn exact_edit(path: &str, anchor: &str, replacement: &str) -> ScriptedTurn {
    tool(
        "edit_file",
        json!({
            "path": path,
            "precondition": "any",
            "strategy": { "type": "exact_replacement", "anchor": anchor, "replacement": replacement },
        }),
    )
}

fn scenario(name: &str, turns: Vec<ScriptedTurn>) -> Scenario {
    Scenario {
        name: name.into(),
        turns,
    }
}

fn all_of(oracles: Vec<Box<dyn Oracle>>) -> crate::oracle::All {
    crate::oracle::All::of(oracles)
}

/// The whole offline suite.
pub fn fixture_suite() -> Vec<BenchTask> {
    vec![
        feature_add_function(),
        bugfix_verified(),
        debugging_repair_loop(),
        refactor_rename(),
        test_creation(),
        dependency_work(),
        exploration_readonly(),
    ]
}

fn feature_add_function() -> BenchTask {
    let before = "pub fn existing(a: i32) -> i32 {\n    a\n}\n";
    let after = "pub fn existing(a: i32) -> i32 {\n    a\n}\n\npub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n";
    BenchTask::new(
        "feature_add_function",
        TaskCategory::Feature,
        "add an `add(a, b)` function to src/lib.rs",
        RepoSpec::new().file("src/lib.rs", before),
        scenario(
            "feature_add_function",
            vec![
                tool("read_file", json!({ "path": "src/lib.rs" })),
                exact_edit("src/lib.rs", before, after),
                finish("added add(a, b)"),
            ],
        ),
        all_of(vec![
            Box::new(TaskCompleted),
            Box::new(FileContains::new(
                "src/lib.rs",
                "pub fn add(a: i32, b: i32)",
            )),
        ]),
    )
}

fn bugfix_verified() -> BenchTask {
    BenchTask::new(
        "bugfix_verified",
        TaskCategory::BugFix,
        "make check.sh pass by setting ANSWER=42",
        RepoSpec::new().file("src/answer.txt", "ANSWER=0\n").file(
            "check.sh",
            "#!/bin/sh\ngrep -q 'ANSWER=42' src/answer.txt\n",
        ),
        scenario(
            "bugfix_verified",
            vec![
                tool("read_file", json!({ "path": "src/answer.txt" })),
                exact_edit("src/answer.txt", "ANSWER=0\n", "ANSWER=42\n"),
                finish("set the answer to 42"),
            ],
        ),
        all_of(vec![
            Box::new(FileContains::new("src/answer.txt", "ANSWER=42")),
            Box::new(ReportVerified),
            Box::new(CommandSucceeds::new("sh", ["check.sh"])),
        ]),
    )
}

fn debugging_repair_loop() -> BenchTask {
    // First edit is deliberately wrong (41), so `Verifying` fails and the
    // task must go Diagnosing -> Repairing -> Verifying before COMPLETED.
    BenchTask::new(
        "debugging_repair_loop",
        TaskCategory::Debugging,
        "make check.sh pass",
        RepoSpec::new().file("src/answer.txt", "ANSWER=0\n").file(
            "check.sh",
            "#!/bin/sh\ngrep -q 'ANSWER=42' src/answer.txt\n",
        ),
        scenario(
            "debugging_repair_loop",
            vec![
                exact_edit("src/answer.txt", "ANSWER=0\n", "ANSWER=41\n"),
                finish("done (but it isn't)"),
                exact_edit("src/answer.txt", "ANSWER=41\n", "ANSWER=42\n"),
                finish("actually fixed it now"),
            ],
        ),
        all_of(vec![
            Box::new(FileContains::new("src/answer.txt", "ANSWER=42")),
            Box::new(ReportVerified),
            Box::new(CommandSucceeds::new("sh", ["check.sh"])),
        ]),
    )
}

fn refactor_rename() -> BenchTask {
    BenchTask::new(
        "refactor_rename",
        TaskCategory::Refactor,
        "rename `oldname` to `newname` in src/math.rs",
        RepoSpec::new().file(
            "src/math.rs",
            "pub fn oldname(x: i32) -> i32 {\n    x * 2\n}\n",
        ),
        scenario(
            "refactor_rename",
            vec![
                tool("read_file", json!({ "path": "src/math.rs" })),
                exact_edit(
                    "src/math.rs",
                    "pub fn oldname(x: i32) -> i32 {",
                    "pub fn newname(x: i32) -> i32 {",
                ),
                finish("renamed oldname -> newname"),
            ],
        ),
        all_of(vec![
            Box::new(TaskCompleted),
            Box::new(FileContains::new("src/math.rs", "pub fn newname")),
            Box::new(FileLacks::new("src/math.rs", "oldname")),
        ]),
    )
}

fn test_creation() -> BenchTask {
    let test_src = "#[test]\nfn doubles() {\n    assert_eq!(4, 2 * 2);\n}\n";
    BenchTask::new(
        "test_creation",
        TaskCategory::TestCreation,
        "add a test file under tests/",
        RepoSpec::new().file("src/lib.rs", "pub fn double(x: i32) -> i32 { x * 2 }\n"),
        scenario(
            "test_creation",
            vec![
                tool(
                    "write_file",
                    json!({ "path": "tests/double.rs", "content": test_src, "precondition": "any" }),
                ),
                finish("added tests/double.rs"),
            ],
        ),
        all_of(vec![
            Box::new(TaskCompleted),
            Box::new(FileExists::new("tests/double.rs")),
            Box::new(FileContains::new("tests/double.rs", "fn doubles()")),
        ]),
    )
}

fn dependency_work() -> BenchTask {
    BenchTask::new(
        "dependency_work",
        TaskCategory::DependencyWork,
        "add the serde dependency to deps.txt",
        RepoSpec::new().file("deps.txt", "# runtime dependencies\nanyhow = \"1\"\n"),
        scenario(
            "dependency_work",
            vec![
                tool("read_file", json!({ "path": "deps.txt" })),
                exact_edit(
                    "deps.txt",
                    "anyhow = \"1\"\n",
                    "anyhow = \"1\"\nserde = \"1\"\n",
                ),
                finish("added serde"),
            ],
        ),
        all_of(vec![
            Box::new(TaskCompleted),
            Box::new(FileContains::new("deps.txt", "serde = \"1\"")),
        ]),
    )
}

fn exploration_readonly() -> BenchTask {
    BenchTask::new(
        "exploration_readonly",
        TaskCategory::Exploration,
        "look around the repository and report back",
        RepoSpec::new()
            .file("src/lib.rs", "pub fn a() {}\n")
            .file("README.md", "# fixture\n"),
        scenario(
            "exploration_readonly",
            vec![
                tool("list_directory", json!({ "path": "." })),
                tool("read_file", json!({ "path": "src/lib.rs" })),
                tool("read_file", json!({ "path": "README.md" })),
                finish("it's a two-file fixture crate"),
            ],
        ),
        all_of(vec![
            Box::new(TaskCompleted),
            Box::new(MaxFilesChanged { max: 0 }),
        ]),
    )
}
