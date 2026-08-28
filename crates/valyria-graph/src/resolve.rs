//! Binding references to definitions.
//!
//! This is the honest part of the graph. Without type information, an
//! import string and a callee name are all the runtime has, so resolution
//! is heuristic — and the heuristics are written here, in pure functions
//! over plain data, so they can be tested exhaustively and so the
//! confidence attached to each edge means something specific.
//!
//! The rule throughout: narrow as far as the evidence allows, then record
//! what is left. Never silently pick one of several equally good
//! candidates, and never drop a reference just because it points outside
//! the repository.

use std::collections::{BTreeMap, BTreeSet};

use valyria_index::{CallRecord, ImportRecord, SymbolRecord};
use valyria_lang::SymbolKind;

use crate::model::Confidence;

/// The outcome of resolving one reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution<T> {
    /// Bound to one or more targets in the repository.
    Resolved {
        targets: Vec<T>,
        confidence: Confidence,
    },
    /// Points outside the repository (a third-party crate, the standard
    /// library) — a real fact, just not one with a node on the other end.
    External,
}

/// Resolve an import path to a file in the repository.
///
/// Handles the two shapes every ecosystem's imports fall into:
///
/// - **Relative** (`./util`, `../shared/log`): resolved against the
///   importing file's directory, then extension-completed. Unambiguous
///   when it hits.
/// - **Dotted or slashed module paths** (`std::collections::HashMap`,
///   `app.parser`, `github.com/org/repo/pkg`, `com.example.Parser`):
///   normalized to slash-separated segments and matched as a *suffix* of
///   a repository path. Suffix rather than prefix matching is what makes
///   `github.com/org/repo/internal/parse` find `internal/parse.go`
///   without the runtime needing to know the module's declared name.
///
/// The longest suffix match wins; ties are reported as ambiguous rather
/// than resolved arbitrarily.
pub fn resolve_import(importer: &str, raw_path: &str, files: &FileLookup) -> Resolution<String> {
    let cleaned = raw_path.trim();
    if cleaned.is_empty() {
        return Resolution::External;
    }

    if cleaned.starts_with('.') && (cleaned.contains('/') || cleaned == "." || cleaned == "..") {
        if let Some(target) = resolve_relative(importer, cleaned, files) {
            return Resolution::Resolved {
                targets: vec![target],
                confidence: Confidence::Likely,
            };
        }
        // A relative import that resolves to nothing is a broken import,
        // not an external dependency — but the graph has no way to say
        // that yet, and calling it external at least keeps the fact.
        return Resolution::External;
    }

    let segments = normalize_module_path(cleaned);
    if segments.is_empty() {
        return Resolution::External;
    }

    // Two nested searches, most specific interpretation first.
    //
    // The outer loop drops trailing segments, because a module path often
    // ends in an *item* rather than a file: `crate::parser::Parser` names
    // a type in `parser.rs`, and no amount of suffix matching on the full
    // path will find it. The inner loop then takes progressively shorter
    // suffixes of what remains, because the leading segments are usually a
    // crate, package or module-root name that appears in no file path
    // (`crate::`, `github.com/org/repo/`).
    for drop_trailing in 0..segments.len() {
        let head = &segments[..segments.len() - drop_trailing];
        for take in (1..=head.len()).rev() {
            let candidate = head[head.len() - take..].join("/");
            let matches = files.by_suffix(&candidate);
            match matches.len() {
                0 => continue,
                1 => {
                    return Resolution::Resolved {
                        targets: vec![matches[0].clone()],
                        confidence: Confidence::Likely,
                    }
                }
                _ => {
                    return Resolution::Resolved {
                        targets: matches,
                        confidence: Confidence::Ambiguous,
                    }
                }
            }
        }
    }

    Resolution::External
}

fn resolve_relative(importer: &str, raw: &str, files: &FileLookup) -> Option<String> {
    let mut parts: Vec<&str> = importer.split('/').collect();
    parts.pop(); // drop the file name, leaving its directory

    for segment in raw.split('/') {
        match segment {
            "." | "" => {}
            ".." => {
                parts.pop()?;
            }
            other => parts.push(other),
        }
    }

    let joined = parts.join("/");
    files.complete(&joined)
}

/// Turn a language's module path into slash-separated segments.
/// `std::collections`, `app.parser`, and `app/parser` all become
/// `["app", "parser"]`-shaped input for suffix matching.
fn normalize_module_path(raw: &str) -> Vec<String> {
    // Rust brace groups (`std::collections::{HashMap, HashSet}`) name a
    // module plus several items; the module prefix is the useful part.
    let raw = raw.split('{').next().unwrap_or(raw);
    raw.replace("::", "/")
        .replace('.', "/")
        .split('/')
        .map(|s| s.trim().trim_matches(|c| c == ';' || c == ',').to_string())
        .filter(|s| !s.is_empty() && s != "*")
        .collect()
}

/// Repository paths, arranged for the lookups resolution needs.
#[derive(Debug, Default)]
pub struct FileLookup {
    paths: BTreeSet<String>,
    /// `src/app/parser` -> [`src/app/parser.rs`]: every path with its
    /// extension removed, so an import that names no extension can still
    /// find the file.
    stems: BTreeMap<String, Vec<String>>,
}

impl FileLookup {
    pub fn new(paths: impl IntoIterator<Item = String>) -> Self {
        let mut lookup = Self::default();
        for path in paths {
            let stem = strip_extension(&path);
            lookup.stems.entry(stem).or_default().push(path.clone());
            lookup.paths.insert(path);
        }
        lookup
    }

    pub fn contains(&self, path: &str) -> bool {
        self.paths.contains(path)
    }

    pub fn len(&self) -> usize {
        self.paths.len()
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    /// Complete an extensionless path to a real file, also trying the
    /// `<dir>/index`, `<dir>/mod` and `<dir>/__init__` conventions that
    /// stand in for "the module itself" across ecosystems.
    pub fn complete(&self, stem: &str) -> Option<String> {
        if self.paths.contains(stem) {
            return Some(stem.to_string());
        }
        if let Some(candidates) = self.stems.get(stem) {
            return candidates.first().cloned();
        }
        for convention in ["index", "mod", "__init__"] {
            let nested = format!("{stem}/{convention}");
            if let Some(candidates) = self.stems.get(&nested) {
                return candidates.first().cloned();
            }
        }
        None
    }

    /// Files that `suffix` could be naming, in two passes.
    ///
    /// First, files whose path (extension removed) ends at a segment
    /// boundary with `suffix` — `parser` finding `src/parser.rs`.
    ///
    /// Failing that, the suffix may name a *directory*: Go packages and
    /// Python packages are directories, not files. A conventional entry
    /// file (`index`, `mod`, `__init__`) stands in for the directory when
    /// one exists; otherwise every file in it is a target, because a
    /// package import really does depend on all of them.
    fn by_suffix(&self, suffix: &str) -> Vec<String> {
        let mut hits: Vec<String> = self
            .stems
            .iter()
            .filter(|(stem, _)| stem.as_str() == suffix || stem.ends_with(&format!("/{suffix}")))
            .flat_map(|(_, paths)| paths.iter().cloned())
            .collect();

        if hits.is_empty() {
            for convention in ["index", "mod", "__init__"] {
                let nested = format!("{suffix}/{convention}");
                hits.extend(
                    self.stems
                        .iter()
                        .filter(|(stem, _)| {
                            stem.as_str() == nested || stem.ends_with(&format!("/{nested}"))
                        })
                        .flat_map(|(_, paths)| paths.iter().cloned()),
                );
            }
        }

        if hits.is_empty() {
            hits.extend(
                self.paths
                    .iter()
                    .filter(|path| {
                        let Some((dir, _)) = path.rsplit_once('/') else {
                            // A root-level file's directory is empty,
                            // which must not match every suffix.
                            return false;
                        };
                        dir == suffix || dir.ends_with(&format!("/{suffix}"))
                    })
                    .cloned(),
            );
        }

        hits.sort();
        hits.dedup();
        hits
    }
}

fn strip_extension(path: &str) -> String {
    match path.rsplit_once('/') {
        Some((dir, name)) => match name.rsplit_once('.') {
            Some((stem, _)) if !stem.is_empty() => format!("{dir}/{stem}"),
            _ => path.to_string(),
        },
        None => match path.rsplit_once('.') {
            Some((stem, _)) if !stem.is_empty() => stem.to_string(),
            _ => path.to_string(),
        },
    }
}

/// Symbols arranged by name, for call resolution.
#[derive(Debug, Default)]
pub struct SymbolLookup {
    by_name: BTreeMap<String, Vec<SymbolRef>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolRef {
    pub path: String,
    pub symbol_path: String,
    pub kind: SymbolKind,
}

impl SymbolLookup {
    pub fn new(symbols: &[SymbolRecord]) -> Self {
        let mut by_name: BTreeMap<String, Vec<SymbolRef>> = BTreeMap::new();
        for symbol in symbols {
            // Only callable kinds: a struct named `parse` must not become
            // a candidate for the call `parse()`.
            if !matches!(
                symbol.kind,
                SymbolKind::Function | SymbolKind::Method | SymbolKind::Macro | SymbolKind::Test
            ) {
                continue;
            }
            by_name
                .entry(symbol.name.clone())
                .or_default()
                .push(SymbolRef {
                    path: symbol.path.clone(),
                    symbol_path: symbol.symbol_path.clone(),
                    kind: symbol.kind,
                });
        }
        Self { by_name }
    }

    pub fn candidates(&self, name: &str) -> &[SymbolRef] {
        self.by_name.get(name).map(|v| v.as_slice()).unwrap_or(&[])
    }
}

/// Resolve a call site to the definitions it could be reaching.
///
/// Narrowing runs in the order the evidence justifies:
///
/// 1. **A definition in the calling file.** Every language resolves a
///    local definition first, so if the caller's own file defines the
///    name, that is the answer.
/// 2. **A definition in a file the caller imports.** The import graph is
///    the only scoping information available without a type checker, and
///    it is usually enough.
/// 3. **Anything else with that name**, reported as ambiguous.
///
/// A name defined nowhere in the repository is external — a standard
/// library or third-party call — not an error.
pub fn resolve_call(
    caller_file: &str,
    call: &CallRecord,
    symbols: &SymbolLookup,
    imported_files: &BTreeSet<String>,
) -> Resolution<SymbolRef> {
    let candidates = symbols.candidates(&call.name);
    if candidates.is_empty() {
        return Resolution::External;
    }

    let local: Vec<SymbolRef> = candidates
        .iter()
        .filter(|c| c.path == caller_file)
        .cloned()
        .collect();
    if local.len() == 1 {
        return Resolution::Resolved {
            targets: local,
            confidence: Confidence::Likely,
        };
    }
    if local.len() > 1 {
        return Resolution::Resolved {
            targets: local,
            confidence: Confidence::Ambiguous,
        };
    }

    let imported: Vec<SymbolRef> = candidates
        .iter()
        .filter(|c| imported_files.contains(&c.path))
        .cloned()
        .collect();
    if imported.len() == 1 {
        return Resolution::Resolved {
            targets: imported,
            confidence: Confidence::Likely,
        };
    }
    if imported.len() > 1 {
        return Resolution::Resolved {
            targets: imported,
            confidence: Confidence::Ambiguous,
        };
    }

    if candidates.len() == 1 {
        return Resolution::Resolved {
            targets: candidates.to_vec(),
            confidence: Confidence::Likely,
        };
    }

    Resolution::Resolved {
        targets: candidates.to_vec(),
        confidence: Confidence::Ambiguous,
    }
}

/// The files a given file imports, resolved. Precomputed once per file so
/// call resolution's step 2 is a set lookup rather than a re-resolution
/// per call site.
pub fn imported_files(
    importer: &str,
    imports: &[ImportRecord],
    files: &FileLookup,
) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for import in imports {
        if let Resolution::Resolved { targets, .. } =
            resolve_import(importer, &import.raw_path, files)
        {
            out.extend(targets);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup(paths: &[&str]) -> FileLookup {
        FileLookup::new(paths.iter().map(|s| s.to_string()))
    }

    fn resolved(resolution: Resolution<String>) -> (Vec<String>, Confidence) {
        match resolution {
            Resolution::Resolved {
                targets,
                confidence,
            } => (targets, confidence),
            Resolution::External => panic!("expected a resolved import"),
        }
    }

    #[test]
    fn a_relative_import_resolves_against_the_importing_files_directory() {
        let files = lookup(&["src/app/main.js", "src/app/util.js"]);
        let (targets, _) = resolved(resolve_import("src/app/main.js", "./util", &files));
        assert_eq!(targets, ["src/app/util.js"]);
    }

    #[test]
    fn a_relative_import_can_walk_up_directories() {
        let files = lookup(&["src/app/main.js", "src/shared/log.js"]);
        let (targets, _) = resolved(resolve_import("src/app/main.js", "../shared/log", &files));
        assert_eq!(targets, ["src/shared/log.js"]);
    }

    #[test]
    fn a_relative_import_that_escapes_the_repository_is_external() {
        let files = lookup(&["main.js"]);
        assert_eq!(
            resolve_import("main.js", "../../outside", &files),
            Resolution::External
        );
    }

    #[test]
    fn a_directory_import_finds_the_conventional_entry_file() {
        let files = lookup(&["src/app/main.js", "src/app/util/index.js"]);
        let (targets, _) = resolved(resolve_import("src/app/main.js", "./util", &files));
        assert_eq!(targets, ["src/app/util/index.js"]);
    }

    #[test]
    fn a_rust_use_path_matches_a_module_file() {
        let files = lookup(&["src/lib.rs", "src/parser/lexer.rs"]);
        let (targets, _) = resolved(resolve_import("src/lib.rs", "crate::parser::lexer", &files));
        assert_eq!(targets, ["src/parser/lexer.rs"]);
    }

    #[test]
    fn a_rust_brace_group_resolves_by_its_module_prefix() {
        let files = lookup(&["src/lib.rs", "src/parser.rs"]);
        let (targets, _) = resolved(resolve_import(
            "src/lib.rs",
            "crate::parser::{Parser, Token}",
            &files,
        ));
        assert_eq!(targets, ["src/parser.rs"]);
    }

    #[test]
    fn a_rust_use_that_names_an_item_falls_back_to_its_module() {
        // `crate::parser::Parser` names a type, not a file; the longest
        // suffix that is a file (`parser`) is the right answer.
        let files = lookup(&["src/lib.rs", "src/parser.rs"]);
        let (targets, _) = resolved(resolve_import(
            "src/lib.rs",
            "crate::parser::Parser",
            &files,
        ));
        assert_eq!(targets, ["src/parser.rs"]);
    }

    #[test]
    fn a_go_module_path_matches_by_suffix_without_knowing_the_module_name() {
        let files = lookup(&["cmd/main.go", "internal/parse/parse.go"]);
        let (targets, _) = resolved(resolve_import(
            "cmd/main.go",
            "github.com/org/repo/internal/parse",
            &files,
        ));
        assert_eq!(targets, ["internal/parse/parse.go"]);
    }

    #[test]
    fn a_python_dotted_import_matches_a_module_file() {
        let files = lookup(&["app/main.py", "app/parser/lexer.py"]);
        let (targets, _) = resolved(resolve_import("app/main.py", "app.parser.lexer", &files));
        assert_eq!(targets, ["app/parser/lexer.py"]);
    }

    #[test]
    fn a_python_package_import_finds_its_init_file() {
        let files = lookup(&["app/main.py", "app/parser/__init__.py"]);
        let (targets, _) = resolved(resolve_import("app/main.py", "app.parser", &files));
        assert_eq!(targets, ["app/parser/__init__.py"]);
    }

    #[test]
    fn a_third_party_import_is_external_rather_than_forced_onto_something() {
        let files = lookup(&["src/lib.rs", "src/parser.rs"]);
        assert_eq!(
            resolve_import("src/lib.rs", "serde::Deserialize", &files),
            Resolution::External
        );
    }

    #[test]
    fn two_files_matching_one_import_are_reported_ambiguous_not_guessed() {
        let files = lookup(&["a/parser.rs", "b/parser.rs", "src/lib.rs"]);
        let (targets, confidence) = resolved(resolve_import("src/lib.rs", "parser", &files));
        assert_eq!(targets, ["a/parser.rs", "b/parser.rs"]);
        assert_eq!(confidence, Confidence::Ambiguous);
    }

    #[test]
    fn an_empty_import_path_is_external() {
        let files = lookup(&["a.rs"]);
        assert_eq!(resolve_import("a.rs", "   ", &files), Resolution::External);
    }

    fn symbol(path: &str, symbol_path: &str, name: &str) -> SymbolRecord {
        SymbolRecord {
            path: path.into(),
            name: name.into(),
            kind: SymbolKind::Function,
            symbol_path: symbol_path.into(),
            span: valyria_lang::Span {
                start_byte: 0,
                end_byte: 1,
                start_line: 1,
                end_line: 1,
            },
            name_span: valyria_lang::Span {
                start_byte: 0,
                end_byte: 1,
                start_line: 1,
                end_line: 1,
            },
            signature: String::new(),
            doc: None,
        }
    }

    fn call(name: &str) -> CallRecord {
        CallRecord {
            path: "src/caller.rs".into(),
            name: name.into(),
            enclosing_symbol_path: Some("run".into()),
            start_line: 1,
        }
    }

    #[test]
    fn a_call_prefers_a_definition_in_its_own_file() {
        let symbols = SymbolLookup::new(&[
            symbol("src/caller.rs", "helper", "helper"),
            symbol("src/other.rs", "helper", "helper"),
        ]);
        let imports = BTreeSet::from(["src/other.rs".to_string()]);

        let resolution = resolve_call("src/caller.rs", &call("helper"), &symbols, &imports);
        match resolution {
            Resolution::Resolved {
                targets,
                confidence,
            } => {
                assert_eq!(targets.len(), 1);
                assert_eq!(targets[0].path, "src/caller.rs");
                assert_eq!(confidence, Confidence::Likely);
            }
            Resolution::External => panic!("expected a resolved call"),
        }
    }

    #[test]
    fn a_call_falls_back_to_an_imported_file() {
        let symbols = SymbolLookup::new(&[
            symbol("src/other.rs", "helper", "helper"),
            symbol("src/unrelated.rs", "helper", "helper"),
        ]);
        let imports = BTreeSet::from(["src/other.rs".to_string()]);

        match resolve_call("src/caller.rs", &call("helper"), &symbols, &imports) {
            Resolution::Resolved {
                targets,
                confidence,
            } => {
                assert_eq!(targets[0].path, "src/other.rs");
                assert_eq!(confidence, Confidence::Likely);
            }
            Resolution::External => panic!("expected a resolved call"),
        }
    }

    #[test]
    fn a_call_with_several_equally_good_targets_is_ambiguous_not_arbitrary() {
        let symbols = SymbolLookup::new(&[
            symbol("src/a.rs", "helper", "helper"),
            symbol("src/b.rs", "helper", "helper"),
        ]);

        match resolve_call("src/caller.rs", &call("helper"), &symbols, &BTreeSet::new()) {
            Resolution::Resolved {
                targets,
                confidence,
            } => {
                assert_eq!(targets.len(), 2);
                assert_eq!(confidence, Confidence::Ambiguous);
            }
            Resolution::External => panic!("expected a resolved call"),
        }
    }

    #[test]
    fn a_call_into_the_standard_library_is_external() {
        let symbols = SymbolLookup::new(&[symbol("src/a.rs", "helper", "helper")]);
        assert_eq!(
            resolve_call(
                "src/caller.rs",
                &call("println"),
                &symbols,
                &BTreeSet::new()
            ),
            Resolution::External
        );
    }

    #[test]
    fn only_callable_symbols_are_call_targets() {
        // A struct named `parse` must not be a candidate for `parse()`.
        let mut struct_symbol = symbol("src/a.rs", "parse", "parse");
        struct_symbol.kind = SymbolKind::Struct;
        let symbols = SymbolLookup::new(&[struct_symbol]);

        assert_eq!(
            resolve_call("src/caller.rs", &call("parse"), &symbols, &BTreeSet::new()),
            Resolution::External
        );
    }

    #[test]
    fn file_lookup_strips_extensions_without_mangling_dotfiles() {
        assert_eq!(strip_extension("src/parser.rs"), "src/parser");
        assert_eq!(strip_extension("src/.gitignore"), "src/.gitignore");
        assert_eq!(strip_extension("Makefile"), "Makefile");
        assert_eq!(strip_extension("a.b/c"), "a.b/c");
    }

    #[test]
    fn module_paths_normalize_across_language_separators() {
        assert_eq!(
            normalize_module_path("std::collections"),
            ["std", "collections"]
        );
        assert_eq!(normalize_module_path("app.parser"), ["app", "parser"]);
        assert_eq!(
            normalize_module_path("org/repo/pkg"),
            ["org", "repo", "pkg"]
        );
        assert_eq!(normalize_module_path("a::b::*"), ["a", "b"]);
    }
}
