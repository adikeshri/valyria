//! Developer tooling for the `valyria` workspace: codegen, schema export,
//! the crate-layering check, and release gates (docs/PLAN.md, xtask row).
//!
//! Today this implements `check-layering`, which is load-bearing: it is the
//! mechanism (not a convention) that enforces "a crate may only depend on
//! crates in lower layers" from the build plan's crate topology (§2).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone)]
struct CrateInfo {
    name: String,
    layer: u8,
    /// Names of workspace-internal crates listed under `[dependencies]`
    /// (never `[dev-dependencies]` — testkit and other dev-only deps are
    /// deliberately exempt from the layering rule).
    deps: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct CargoToml {
    package: Option<Package>,
    dependencies: Option<toml::value::Table>,
}

#[derive(Debug, Deserialize)]
struct Package {
    name: String,
    metadata: Option<Metadata>,
}

#[derive(Debug, Deserialize)]
struct Metadata {
    valyria: Option<ValyriaMeta>,
}

#[derive(Debug, Deserialize)]
struct ValyriaMeta {
    layer: u8,
}

#[derive(Debug, PartialEq, Eq)]
struct Violation {
    message: String,
}

/// Pure, unit-testable core: given the crate graph, find every edge that
/// violates "may only depend on crates in a lower or equal layer" plus any
/// dependency cycle among same-layer crates.
fn find_violations(crates: &BTreeMap<String, CrateInfo>) -> Vec<Violation> {
    let mut violations = Vec::new();

    for krate in crates.values() {
        for dep_name in &krate.deps {
            let Some(dep) = crates.get(dep_name) else {
                continue; // external crate, not part of the workspace topology
            };
            if dep.layer > krate.layer {
                violations.push(Violation {
                    message: format!(
                        "{} (layer {}) depends on {} (layer {}) — a crate may only depend on crates in the same or a lower layer",
                        krate.name, krate.layer, dep.name, dep.layer
                    ),
                });
            }
        }
    }

    // Cycle detection restricted to same-layer edges (cross-layer edges are
    // already strictly decreasing in layer number, so they cannot cycle).
    for layer_crates in group_by_layer(crates).values() {
        if let Some(cycle) = find_cycle(layer_crates, crates) {
            violations.push(Violation {
                message: format!("dependency cycle within a layer: {}", cycle.join(" -> ")),
            });
        }
    }

    violations
}

fn group_by_layer(crates: &BTreeMap<String, CrateInfo>) -> BTreeMap<u8, Vec<&str>> {
    let mut by_layer: BTreeMap<u8, Vec<&str>> = BTreeMap::new();
    for krate in crates.values() {
        by_layer.entry(krate.layer).or_default().push(&krate.name);
    }
    by_layer
}

fn find_cycle(names: &[&str], crates: &BTreeMap<String, CrateInfo>) -> Option<Vec<String>> {
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        Unvisited,
        InProgress,
        Done,
    }

    let mut marks: BTreeMap<&str, Mark> = names.iter().map(|n| (*n, Mark::Unvisited)).collect();
    let mut stack: Vec<String> = Vec::new();

    fn visit<'a>(
        node: &'a str,
        layer: u8,
        crates: &'a BTreeMap<String, CrateInfo>,
        marks: &mut BTreeMap<&'a str, Mark>,
        stack: &mut Vec<String>,
    ) -> Option<Vec<String>> {
        match marks.get(node) {
            Some(Mark::InProgress) => {
                stack.push(node.to_string());
                return Some(stack.clone());
            }
            Some(Mark::Done) => return None,
            _ => {}
        }
        marks.insert(node, Mark::InProgress);
        stack.push(node.to_string());
        if let Some(krate) = crates.get(node) {
            for dep in &krate.deps {
                if let Some(dep_krate) = crates.get(dep) {
                    if dep_krate.layer == layer {
                        if let Some(cycle) = visit(dep, layer, crates, marks, stack) {
                            return Some(cycle);
                        }
                    }
                }
            }
        }
        stack.pop();
        marks.insert(node, Mark::Done);
        None
    }

    for name in names {
        if let Some(krate) = crates.get(*name) {
            if let Some(cycle) = visit(name, krate.layer, crates, &mut marks, &mut stack) {
                return Some(cycle);
            }
        }
    }
    None
}

fn load_workspace_crates(workspace_root: &Path) -> Result<BTreeMap<String, CrateInfo>> {
    let crates_dir = workspace_root.join("crates");
    let mut out = BTreeMap::new();

    let mut manifest_paths: Vec<PathBuf> = fs::read_dir(&crates_dir)
        .with_context(|| format!("reading {}", crates_dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path().join("Cargo.toml"))
        .filter(|p| p.exists())
        .collect();
    manifest_paths.sort();

    for manifest_path in manifest_paths {
        let text = fs::read_to_string(&manifest_path)
            .with_context(|| format!("reading {}", manifest_path.display()))?;
        let parsed: CargoToml = toml::from_str(&text)
            .with_context(|| format!("parsing {}", manifest_path.display()))?;

        let Some(package) = parsed.package else {
            continue;
        };
        let Some(layer) = package.metadata.and_then(|m| m.valyria).map(|v| v.layer) else {
            // Not every crate declares a layer yet (e.g. xtask, cli-adjacent
            // tooling); skip rather than fail so the check stays useful
            // during incremental scaffolding.
            continue;
        };

        let deps = parsed
            .dependencies
            .map(|t| t.keys().cloned().collect())
            .unwrap_or_default();

        out.insert(
            package.name.clone(),
            CrateInfo {
                name: package.name,
                layer,
                deps,
            },
        );
    }

    Ok(out)
}

fn check_layering() -> Result<()> {
    let workspace_root = workspace_root()?;
    let crates = load_workspace_crates(&workspace_root)?;
    if crates.is_empty() {
        bail!("no crates with `package.metadata.valyria.layer` found under crates/");
    }

    let violations = find_violations(&crates);
    if violations.is_empty() {
        println!("layering OK — {} crates checked", crates.len());
        Ok(())
    } else {
        eprintln!("layering violations:");
        for v in &violations {
            eprintln!("  - {}", v.message);
        }
        bail!("{} layering violation(s)", violations.len());
    }
}

fn workspace_root() -> Result<PathBuf> {
    let mut dir = std::env::current_dir()?;
    loop {
        if dir.join("Cargo.toml").exists() && dir.join("crates").is_dir() {
            return Ok(dir);
        }
        if !dir.pop() {
            bail!("could not locate workspace root (no crates/ + Cargo.toml ancestor)");
        }
    }
}

/// Where the exported protocol schema lives, relative to the workspace root.
const PROTOCOL_SCHEMA_DIR: &str = "docs/protocol";

/// Write `docs/protocol/*.schema.json` + `version.txt` from the live
/// `valyria-protocol` types (§4.27). Run this after any deliberate wire
/// change; `check-protocol` gates that it was run.
fn export_schema() -> Result<()> {
    let root = workspace_root()?;
    let dir = root.join(PROTOCOL_SCHEMA_DIR);
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    for (name, contents) in valyria_protocol::export_schema() {
        let path = dir.join(name);
        fs::write(&path, contents).with_context(|| format!("writing {}", path.display()))?;
        println!("wrote {}", path.display());
    }
    Ok(())
}

/// CI gate: the committed schema must match what the current types
/// generate. A mismatch means either the schema export was not re-run
/// (`cargo xtask schema`) or the change is breaking and
/// `PROTOCOL_VERSION` must be bumped.
fn check_protocol() -> Result<()> {
    let root = workspace_root()?;
    let dir = root.join(PROTOCOL_SCHEMA_DIR);
    let mut stale = Vec::new();

    for (name, expected) in valyria_protocol::export_schema() {
        let path = dir.join(name);
        let actual = fs::read_to_string(&path).unwrap_or_default();
        if actual != expected {
            stale.push(name);
        }
    }

    if stale.is_empty() {
        println!(
            "protocol schema OK — {} (docs/protocol/ matches the live types)",
            valyria_protocol::PROTOCOL_VERSION
        );
        Ok(())
    } else {
        eprintln!("protocol schema is out of date:");
        for name in &stale {
            eprintln!("  - docs/protocol/{name}");
        }
        eprintln!();
        eprintln!("the wire types changed. Then, in order:");
        eprintln!("  1. if the change is breaking, bump PROTOCOL_VERSION in");
        eprintln!("     crates/valyria-protocol/src/version.rs (see its doc comment);");
        eprintln!("  2. run `cargo xtask schema` and commit docs/protocol/.");
        bail!("{} schema file(s) out of date", stale.len());
    }
}

/// Where the committed benchmark baseline lives, relative to the root.
const BENCH_BASELINE: &str = "docs/bench/baseline.json";

/// Run the offline fixture benchmark suite (`valyria-bench`). With
/// `--bless`, (over)write `docs/bench/baseline.json`; otherwise diff the
/// fresh run against the committed baseline and fail on any regression —
/// the Phase 11 "benchmark baseline recorded" gate.
fn bench(bless: bool) -> Result<()> {
    use valyria_bench::{compare, fixture_suite, BenchReport, BenchRunner};

    let root = workspace_root()?;
    let baseline_path = root.join(BENCH_BASELINE);

    let rt = tokio::runtime::Runtime::new().context("starting tokio runtime")?;
    let report = rt
        .block_on(BenchRunner::new().run_suite(&fixture_suite()))
        .map_err(|e| anyhow::anyhow!("bench suite errored ({}): {e}", e.code()))?;
    print!("{}", report.render_table());

    if bless {
        if let Some(parent) = baseline_path.parent() {
            fs::create_dir_all(parent).ok();
        }
        fs::write(
            &baseline_path,
            format!("{}\n", report.stabilized().to_json_pretty()),
        )
        .with_context(|| format!("writing {}", baseline_path.display()))?;
        println!("blessed baseline -> {}", baseline_path.display());
        return Ok(());
    }

    if !report.all_passed() {
        bail!("bench suite has failing tasks (see table above)");
    }

    let baseline_src = fs::read_to_string(&baseline_path).with_context(|| {
        format!(
            "reading {} (run `cargo xtask bench --bless` to create it)",
            baseline_path.display()
        )
    })?;
    let baseline = BenchReport::from_json(&baseline_src)
        .with_context(|| format!("parsing {}", baseline_path.display()))?;
    let cmp = compare(&baseline, &report);
    print!("{}", cmp.render());
    if !cmp.is_clean() {
        bail!("benchmark regression against {BENCH_BASELINE} (bless it if intentional)");
    }
    println!("bench OK — no regression against {BENCH_BASELINE}");
    Ok(())
}

/// Aggregate release gate (§52 / PLAN Phase 11): every machine-checkable
/// gate in one run, with a summary table. Non-zero exit if any fails.
type Gate = (&'static str, Box<dyn Fn() -> Result<()>>);

fn release_gates() -> Result<()> {
    let root = workspace_root()?;
    let gates: Vec<Gate> = vec![
        ("crate layering", Box::new(check_layering)),
        ("protocol schema compat", Box::new(check_protocol)),
        ("benchmark baseline", Box::new(|| bench(false))),
        (
            "acceptance mapping doc",
            Box::new(move || {
                let p = root.join("docs/ACCEPTANCE.md");
                if p.exists() {
                    Ok(())
                } else {
                    bail!("missing {}", p.display())
                }
            }),
        ),
    ];

    let mut failed = Vec::new();
    println!("release gates\n");
    for (name, run) in &gates {
        match run() {
            Ok(()) => println!("  [pass] {name}"),
            Err(e) => {
                println!("  [FAIL] {name}: {e}");
                failed.push(*name);
            }
        }
    }
    println!();
    if failed.is_empty() {
        println!("all {} release gates pass", gates.len());
        Ok(())
    } else {
        bail!(
            "{} release gate(s) failed: {}",
            failed.len(),
            failed.join(", ")
        );
    }
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("check-layering") => check_layering(),
        Some("schema") => export_schema(),
        Some("check-protocol") => check_protocol(),
        Some("bench") => bench(args.iter().any(|a| a == "--bless")),
        Some("release-gates") => release_gates(),
        Some(other) => bail!("unknown xtask command: {other}"),
        None => {
            println!(
                "usage: cargo xtask <check-layering|schema|check-protocol|bench [--bless]|release-gates>"
            );
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The committed `docs/protocol/` schema must already match the live
    /// wire types — the same check CI runs, run locally so a stale commit
    /// is caught before push.
    #[test]
    fn committed_protocol_schema_is_current() {
        check_protocol().expect("run `cargo xtask schema` and commit docs/protocol/");
    }

    fn krate(name: &str, layer: u8, deps: &[&str]) -> (String, CrateInfo) {
        (
            name.to_string(),
            CrateInfo {
                name: name.to_string(),
                layer,
                deps: deps.iter().map(|s| s.to_string()).collect(),
            },
        )
    }

    #[test]
    fn allows_same_or_lower_layer_deps() {
        let crates: BTreeMap<String, CrateInfo> = [
            krate("valyria-types", 0, &[]),
            krate("valyria-util", 0, &[]),
            krate("valyria-store", 0, &["valyria-types", "valyria-util"]),
            krate("valyria-vfs", 1, &["valyria-store"]),
        ]
        .into_iter()
        .collect();

        assert!(find_violations(&crates).is_empty());
    }

    #[test]
    fn catches_upward_dependency() {
        // deliberate violation: a layer-0 crate depending on a layer-1 crate
        let crates: BTreeMap<String, CrateInfo> = [
            krate("valyria-types", 0, &["valyria-vfs"]),
            krate("valyria-vfs", 1, &[]),
        ]
        .into_iter()
        .collect();

        let violations = find_violations(&crates);
        assert_eq!(violations.len(), 1);
        assert!(violations[0].message.contains("valyria-types"));
        assert!(violations[0].message.contains("valyria-vfs"));
    }

    #[test]
    fn catches_same_layer_cycle() {
        let crates: BTreeMap<String, CrateInfo> = [krate("a", 0, &["b"]), krate("b", 0, &["a"])]
            .into_iter()
            .collect();

        let violations = find_violations(&crates);
        assert!(violations.iter().any(|v| v.message.contains("cycle")));
    }

    #[test]
    fn ignores_external_crates() {
        let crates: BTreeMap<String, CrateInfo> =
            [krate("valyria-types", 0, &["serde", "tokio", "thiserror"])]
                .into_iter()
                .collect();

        assert!(find_violations(&crates).is_empty());
    }
}
