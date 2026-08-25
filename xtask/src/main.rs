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

fn main() -> Result<()> {
    let cmd = std::env::args().nth(1);
    match cmd.as_deref() {
        Some("check-layering") => check_layering(),
        Some(other) => bail!("unknown xtask command: {other}"),
        None => {
            println!("usage: cargo xtask <check-layering>");
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
