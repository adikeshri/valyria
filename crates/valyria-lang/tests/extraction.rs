//! The per-language extraction corpus.
//!
//! Each language gets a small but realistic source file and assertions on
//! what came out of it. This is the suite that catches a grammar upgrade
//! silently renaming a node kind — the failure mode where a repository
//! quietly indexes with zero symbols and every downstream ranking answer
//! becomes subtly wrong without anything erroring.

use std::path::Path;

use valyria_lang::{FileFacts, LanguageRegistry, SymbolKind};

fn facts_for(path: &str, source: &str) -> FileFacts {
    let registry = LanguageRegistry::with_builtin_languages().unwrap();
    registry
        .extract_facts(Path::new(path), source)
        .unwrap_or_else(|| panic!("no language claimed {path}"))
        .unwrap_or_else(|e| panic!("extraction failed for {path}: {e}"))
}

fn paths_of_kind(facts: &FileFacts, kind: SymbolKind) -> Vec<&str> {
    facts
        .symbols
        .iter()
        .filter(|s| s.kind == kind)
        .map(|s| s.symbol_path.as_str())
        .collect()
}

fn find<'a>(facts: &'a FileFacts, path: &str) -> &'a valyria_lang::Symbol {
    facts
        .symbols
        .iter()
        .find(|s| s.symbol_path == path)
        .unwrap_or_else(|| {
            let all: Vec<&str> = facts
                .symbols
                .iter()
                .map(|s| s.symbol_path.as_str())
                .collect();
            panic!("no symbol `{path}`; found {all:?}")
        })
}

const RUST: &str = r#"use std::collections::HashMap;
use crate::{a, b};

/// Parses things.
/// Second line.
pub struct Parser {
    pos: usize,
}

impl Parser {
    /// Parse the input.
    pub fn parse(&self, input: &str) -> bool {
        helper(input);
        true
    }
}

pub trait Visit {
    fn visit(&self);
}

pub mod inner {
    pub fn nested() {}
}

pub const LIMIT: usize = 10;
pub type Alias = u32;

fn helper(s: &str) -> bool { s.is_empty() }

#[cfg(test)]
mod tests {
    #[test]
    fn works() { assert!(true); }
}
"#;

#[test]
fn rust_symbols_carry_impl_qualified_paths() {
    let facts = facts_for("src/parser.rs", RUST);

    // The method is `Parser::parse`, not a bare `parse`: the `impl` block
    // is not itself a symbol, so the query supplies the prefix explicitly.
    assert_eq!(
        paths_of_kind(&facts, SymbolKind::Method),
        ["Parser::parse", "Visit::visit"]
    );
    assert_eq!(paths_of_kind(&facts, SymbolKind::Struct), ["Parser"]);
    assert_eq!(paths_of_kind(&facts, SymbolKind::Trait), ["Visit"]);
    assert_eq!(paths_of_kind(&facts, SymbolKind::TypeAlias), ["Alias"]);
    assert_eq!(paths_of_kind(&facts, SymbolKind::Constant), ["LIMIT"]);
    assert_eq!(
        paths_of_kind(&facts, SymbolKind::Module),
        ["inner", "tests"]
    );
    assert!(!facts.has_parse_errors);
}

#[test]
fn rust_nested_module_functions_are_path_qualified() {
    let facts = facts_for("src/parser.rs", RUST);
    assert!(paths_of_kind(&facts, SymbolKind::Function).contains(&"inner::nested"));
}

#[test]
fn rust_field_paths_are_not_double_prefixed() {
    // `struct_item` is a container, so containment already supplies
    // `Parser::`. A regression here shows up as `Parser::Parser::pos`.
    let facts = facts_for("src/parser.rs", RUST);
    assert_eq!(paths_of_kind(&facts, SymbolKind::Field), ["Parser::pos"]);
}

#[test]
fn rust_doc_comments_attach_to_the_definition_below_them() {
    let facts = facts_for("src/parser.rs", RUST);
    let parser = find(&facts, "Parser");
    assert_eq!(
        parser.doc.as_deref(),
        Some("/// Parses things.\n/// Second line.")
    );
    assert_eq!(
        find(&facts, "Parser::parse").doc.as_deref(),
        Some("/// Parse the input.")
    );
    // A symbol with no comment above it gets no doc, rather than
    // inheriting the nearest one.
    assert_eq!(find(&facts, "helper").doc, None);
}

#[test]
fn rust_signatures_stop_at_the_body() {
    let facts = facts_for("src/parser.rs", RUST);
    assert_eq!(
        find(&facts, "Parser::parse").signature,
        "pub fn parse(&self, input: &str) -> bool"
    );
    assert_eq!(find(&facts, "Parser").signature, "pub struct Parser");
}

#[test]
fn rust_imports_keep_the_brace_group_intact() {
    let facts = facts_for("src/parser.rs", RUST);
    let paths: Vec<&str> = facts.imports.iter().map(|i| i.raw_path.as_str()).collect();
    assert_eq!(paths, ["std::collections::HashMap", "crate::{a, b}"]);
}

#[test]
fn rust_calls_record_their_calling_function() {
    let facts = facts_for("src/parser.rs", RUST);
    let call = facts.calls.iter().find(|c| c.name == "helper").unwrap();
    assert_eq!(call.enclosing_symbol_path.as_deref(), Some("Parser::parse"));
}

#[test]
fn rust_test_attribute_is_detected() {
    let facts = facts_for("src/parser.rs", RUST);
    assert_eq!(facts.tests.len(), 1);
    assert_eq!(facts.tests[0].name, "works");
    // The test's identity matches its symbol's, so the graph can join them.
    assert_eq!(facts.tests[0].symbol_path, "tests::works");
}

const PYTHON: &str = r#"import os
from typing import List
from . import sibling

LIMIT = 10

class Parser:
    def parse(self, text):
        return helper(text)

    @staticmethod
    def make():
        return Parser()

def helper(text):
    return os.path.join(text)

def test_parse():
    assert Parser().parse("x")
"#;

#[test]
fn python_methods_are_qualified_by_their_class() {
    let facts = facts_for("app/parser.py", PYTHON);
    assert_eq!(
        paths_of_kind(&facts, SymbolKind::Method),
        ["Parser.parse", "Parser.make"]
    );
    assert_eq!(paths_of_kind(&facts, SymbolKind::Class), ["Parser"]);
}

#[test]
fn python_a_decorated_method_is_one_symbol_not_two() {
    // `@staticmethod def make()` matches both the decorated-definition
    // pattern and the bare function one; they collapse to a single
    // symbol whose span covers the decorator.
    let facts = facts_for("app/parser.py", PYTHON);
    let make: Vec<_> = facts.symbols.iter().filter(|s| s.name == "make").collect();
    assert_eq!(make.len(), 1);
    assert_eq!(make[0].kind, SymbolKind::Method);
    assert!(make[0].signature.starts_with("@staticmethod"));
}

#[test]
fn python_module_level_assignments_are_constants() {
    let facts = facts_for("app/parser.py", PYTHON);
    assert_eq!(paths_of_kind(&facts, SymbolKind::Constant), ["LIMIT"]);
}

#[test]
fn python_imports_cover_plain_from_and_relative_forms() {
    let facts = facts_for("app/parser.py", PYTHON);
    let paths: Vec<&str> = facts.imports.iter().map(|i| i.raw_path.as_str()).collect();
    assert_eq!(paths, ["os", "typing", "."]);
}

#[test]
fn python_tests_are_found_by_naming_convention() {
    let facts = facts_for("app/parser.py", PYTHON);
    assert_eq!(facts.tests.len(), 1);
    assert_eq!(facts.tests[0].name, "test_parse");
}

const GO: &str = r#"package main

import (
    "fmt"
    "github.com/x/y"
)

type Parser struct {
    pos int
}

type Visitor interface {
    Visit()
}

type Alias = int

const Limit = 10
var global = 1

func (p *Parser) Parse(s string) bool {
    fmt.Println(s)
    return helper(s)
}

func helper(s string) bool { return s == "" }

func TestParse(t *testing.T) {
    helper("x")
}
"#;

#[test]
fn go_methods_are_qualified_by_their_receiver_type() {
    let facts = facts_for("cmd/parser.go", GO);
    assert_eq!(
        paths_of_kind(&facts, SymbolKind::Method),
        ["Visitor.Visit", "Parser.Parse"]
    );
}

#[test]
fn go_type_declarations_pick_the_specific_kind_over_the_catch_all() {
    // Every one of these is a `type_declaration`, matched by both the
    // specific pattern and the type-alias catch-all. A regression shows up
    // as `Parser` being reported as a type alias.
    let facts = facts_for("cmd/parser.go", GO);
    assert_eq!(paths_of_kind(&facts, SymbolKind::Struct), ["Parser"]);
    assert_eq!(paths_of_kind(&facts, SymbolKind::Interface), ["Visitor"]);
    assert_eq!(paths_of_kind(&facts, SymbolKind::TypeAlias), ["Alias"]);
}

#[test]
fn go_consts_and_vars_are_distinguished() {
    let facts = facts_for("cmd/parser.go", GO);
    assert_eq!(paths_of_kind(&facts, SymbolKind::Constant), ["Limit"]);
    assert_eq!(paths_of_kind(&facts, SymbolKind::Variable), ["global"]);
}

#[test]
fn go_import_paths_are_unquoted() {
    let facts = facts_for("cmd/parser.go", GO);
    let paths: Vec<&str> = facts.imports.iter().map(|i| i.raw_path.as_str()).collect();
    assert_eq!(paths, ["fmt", "github.com/x/y"]);
}

#[test]
fn go_tests_are_found_by_the_toolchain_naming_convention() {
    let facts = facts_for("cmd/parser.go", GO);
    assert_eq!(facts.tests.len(), 1);
    assert_eq!(facts.tests[0].name, "TestParse");
}

const JAVA: &str = r#"package com.example;

import java.util.List;

/** A parser. */
public class Parser {
    private int pos;

    public Parser() { this.pos = 0; }

    public boolean parse(String input) {
        return helper(input);
    }

    static class Inner {
        void deep() {}
    }
}

class ParserTest {
    @Test
    public void parsesInput() {
        new Parser().parse("x");
    }
}
"#;

#[test]
fn java_nested_classes_produce_fully_qualified_member_paths() {
    let facts = facts_for("src/Parser.java", JAVA);
    assert!(paths_of_kind(&facts, SymbolKind::Class).contains(&"Parser.Inner"));
    assert!(paths_of_kind(&facts, SymbolKind::Method).contains(&"Parser.Inner.deep"));
}

#[test]
fn java_constructors_are_methods() {
    let facts = facts_for("src/Parser.java", JAVA);
    assert!(paths_of_kind(&facts, SymbolKind::Method).contains(&"Parser.Parser"));
}

#[test]
fn java_block_doc_comments_attach() {
    let facts = facts_for("src/Parser.java", JAVA);
    assert_eq!(
        find(&facts, "Parser").doc.as_deref(),
        Some("/** A parser. */")
    );
}

#[test]
fn java_tests_are_found_by_annotation() {
    let facts = facts_for("src/Parser.java", JAVA);
    assert_eq!(facts.tests.len(), 1);
    assert_eq!(facts.tests[0].symbol_path, "ParserTest.parsesInput");
}

const JAVASCRIPT: &str = r#"import { readFile } from "fs";
const path = require("path");

export function parse(input) {
  return helper(input);
}

const arrow = (x) => helper(x);
const NOT_A_FUNCTION = 42;

class Parser {
  parse(input) {
    return helper(input);
  }
}

function helper(s) { return s; }

test("parses input", () => {
  parse("x");
});
"#;

#[test]
fn javascript_arrow_consts_are_functions_not_variables() {
    let facts = facts_for("src/parser.js", JAVASCRIPT);
    let functions = paths_of_kind(&facts, SymbolKind::Function);
    assert!(functions.contains(&"arrow"));
    // A const that is not function-valued is not a symbol at all: every
    // local `let i = 0` would otherwise flood the index.
    assert!(!facts.symbols.iter().any(|s| s.name == "NOT_A_FUNCTION"));
}

#[test]
fn javascript_covers_both_esm_and_commonjs_imports() {
    let facts = facts_for("src/parser.js", JAVASCRIPT);
    let paths: Vec<&str> = facts.imports.iter().map(|i| i.raw_path.as_str()).collect();
    assert_eq!(paths, ["fs", "path"]);
}

#[test]
fn javascript_tests_are_named_by_their_string_literal() {
    let facts = facts_for("src/parser.js", JAVASCRIPT);
    assert_eq!(facts.tests.len(), 1);
    assert_eq!(facts.tests[0].name, "parses input");
}

#[test]
fn javascript_top_level_calls_have_no_enclosing_symbol() {
    let facts = facts_for("src/parser.js", JAVASCRIPT);
    let top = facts.calls.iter().find(|c| c.name == "require").unwrap();
    assert_eq!(top.enclosing_symbol_path, None);
}

const TYPESCRIPT: &str = r#"import { readFile } from "fs";

export interface Options {
  depth: number;
  visit(): void;
}

export type Alias = string;
export enum Mode { Fast, Slow }

export class Parser {
  private pos: number = 0;
  parse(input: string): boolean {
    return helper(input);
  }
}

export abstract class Base {
  abstract run(): void;
}

function helper(s: string): boolean { return s === ""; }
"#;

#[test]
fn typescript_adds_interfaces_enums_and_type_aliases_to_the_shared_js_patterns() {
    let facts = facts_for("src/parser.ts", TYPESCRIPT);
    assert_eq!(paths_of_kind(&facts, SymbolKind::Interface), ["Options"]);
    assert_eq!(paths_of_kind(&facts, SymbolKind::Enum), ["Mode"]);
    assert_eq!(paths_of_kind(&facts, SymbolKind::TypeAlias), ["Alias"]);
    // The JavaScript patterns still work against the TypeScript grammar —
    // that is the whole reason the query files are concatenated.
    assert!(paths_of_kind(&facts, SymbolKind::Function).contains(&"helper"));
    assert!(paths_of_kind(&facts, SymbolKind::Class).contains(&"Parser"));
}

#[test]
fn typescript_interface_and_abstract_members_are_methods() {
    let facts = facts_for("src/parser.ts", TYPESCRIPT);
    let methods = paths_of_kind(&facts, SymbolKind::Method);
    assert!(methods.contains(&"Options.visit"));
    assert!(methods.contains(&"Base.run"));
    assert!(methods.contains(&"Parser.parse"));
}

#[test]
fn tsx_parses_jsx_that_the_typescript_grammar_would_reject() {
    // `<div>hi</div>` is a type assertion in `.ts` and an element in
    // `.tsx`; the two need different grammars, which is why TSX is a
    // separate provider rather than a flag.
    let facts = facts_for("src/App.tsx", "export const App = () => <div>hi</div>;\n");
    assert!(!facts.has_parse_errors);
    assert_eq!(paths_of_kind(&facts, SymbolKind::Function), ["App"]);
}

#[test]
fn a_syntactically_broken_file_still_yields_the_symbols_it_can() {
    // Partial facts beat no facts: the agent is often looking at a file
    // it is midway through editing.
    let facts = facts_for("src/broken.rs", "fn good() {}\nfn broken( {\n");
    assert!(facts.has_parse_errors);
    assert!(facts.symbols.iter().any(|s| s.name == "good"));
}

#[test]
fn an_empty_file_yields_no_facts_and_no_error() {
    let facts = facts_for("src/empty.rs", "");
    assert_eq!(facts, FileFacts::default());
}

#[test]
fn extraction_is_deterministic() {
    // Journal replay and the index drift check both compare extraction
    // output across runs, so a `HashMap` iteration order leaking into the
    // result would make both flaky.
    let a = facts_for("src/parser.rs", RUST);
    let b = facts_for("src/parser.rs", RUST);
    assert_eq!(a, b);
}
