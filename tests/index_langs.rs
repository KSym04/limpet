//! Per-language extraction coverage (invariant I7): every shipped grammar
//! proves it can extract a function, a class with a method, an import, and
//! a call, with correct FQNs after indexing.

use limpet::index::{self, extract, lang::Lang};
use limpet::store::Store;
use std::fs;
use tempfile::TempDir;

fn names(facts: &extract::FileFacts, kind: &str) -> Vec<String> {
    facts
        .symbols
        .iter()
        .filter(|s| s.kind == kind)
        .map(|s| s.name.clone())
        .collect()
}

#[test]
fn php_extraction() {
    let src = r#"<?php
use App\Services\Mailer;

function top_level($x) {
    helper_call($x);
    return $x + 1;
}

class ScanQueue {
    public function push($item) {
        $this->validate($item);
    }
}
"#;
    let facts = extract::extract(Lang::Php, src).unwrap();
    assert_eq!(names(&facts, "function"), vec!["top_level"]);
    assert_eq!(names(&facts, "class"), vec!["ScanQueue"]);
    assert_eq!(names(&facts, "method"), vec!["push"]);
    assert!(facts.imports.iter().any(|i| i.contains("Mailer")), "{:?}", facts.imports);
    assert!(facts
        .calls
        .iter()
        .any(|(scope, callee)| scope == "top_level" && callee == "helper_call"));
}

#[test]
fn js_extraction() {
    let src = r#"
import { helper } from './helper.js';

function topLevel(x) {
    helper(x);
    return x + 1;
}

class ScanQueue {
    push(item) {
        this.validate(item);
    }
}
"#;
    let facts = extract::extract(Lang::Js, src).unwrap();
    assert_eq!(names(&facts, "function"), vec!["topLevel"]);
    assert_eq!(names(&facts, "class"), vec!["ScanQueue"]);
    assert_eq!(names(&facts, "method"), vec!["push"]);
    assert_eq!(facts.imports, vec!["./helper.js"]);
    assert!(facts
        .calls
        .iter()
        .any(|(scope, callee)| scope == "topLevel" && callee == "helper"));
}

#[test]
fn ts_extraction() {
    let src = r#"
import { helper } from './helper';

function topLevel(x: number): number {
    helper(x);
    return x + 1;
}

class ScanQueue {
    push(item: string): void {
        this.validate(item);
    }
}
"#;
    let facts = extract::extract(Lang::Ts, src).unwrap();
    assert_eq!(names(&facts, "function"), vec!["topLevel"]);
    assert_eq!(names(&facts, "class"), vec!["ScanQueue"]);
    assert_eq!(names(&facts, "method"), vec!["push"]);
    assert_eq!(facts.imports, vec!["./helper"]);
    assert!(facts
        .calls
        .iter()
        .any(|(scope, callee)| scope == "topLevel" && callee == "helper"));
}

#[test]
fn py_extraction() {
    let src = r#"
import os
from app.services import mailer

def top_level(x):
    helper_call(x)
    return x + 1

class ScanQueue:
    def push(self, item):
        self.validate(item)
"#;
    let facts = extract::extract(Lang::Py, src).unwrap();
    assert_eq!(names(&facts, "function"), vec!["top_level"]);
    assert_eq!(names(&facts, "class"), vec!["ScanQueue"]);
    assert_eq!(names(&facts, "method"), vec!["push"]);
    assert!(facts.imports.iter().any(|i| i == "os"));
    assert!(facts
        .calls
        .iter()
        .any(|(scope, callee)| scope == "top_level" && callee == "helper_call"));
}

#[test]
fn rust_extraction() {
    let src = r#"
use std::collections::HashMap;

fn top_level(x: u32) -> u32 {
    helper_call(x);
    x + 1
}

struct ScanQueue;

impl ScanQueue {
    fn push(&self, item: String) {
        self.validate(item);
    }
}
"#;
    let facts = extract::extract(Lang::Rust, src).unwrap();
    assert_eq!(names(&facts, "function"), vec!["top_level"]);
    assert_eq!(names(&facts, "class"), vec!["ScanQueue"]);
    assert_eq!(names(&facts, "method"), vec!["push"]);
    assert!(facts.imports.iter().any(|i| i.contains("HashMap")));
    assert!(facts
        .calls
        .iter()
        .any(|(scope, callee)| scope == "top_level" && callee == "helper_call"));
}

#[test]
fn full_index_and_sweep_reindexes_only_changed() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    fs::write(root.join("a.py"), "def alpha():\n    return 1\n").unwrap();
    fs::write(root.join("b.py"), "def beta():\n    return 2\n").unwrap();

    let store = Store::open_in_memory().unwrap();
    let report = index::full_index(&store, root).unwrap();
    assert_eq!(report.files, 2);
    assert_eq!(report.symbols, 2);
    assert!(report.failed.is_empty());

    // FQN shape check.
    let fqn: String = store
        .conn
        .query_row("SELECT fqn FROM symbols WHERE name = 'alpha'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(fqn, "a.alpha");

    // No changes: sweep does nothing.
    let s0 = index::sweep(&store, root).unwrap();
    assert!(s0.reindexed.is_empty() && s0.dirty.is_empty() && s0.removed.is_empty());

    // Touch one file with new content and a bumped mtime.
    std::thread::sleep(std::time::Duration::from_millis(20));
    fs::write(root.join("a.py"), "def alpha():\n    return 42\n").unwrap();
    let s1 = index::sweep(&store, root).unwrap();
    assert_eq!(s1.reindexed, vec!["a.py"]);

    // Delete the other: sweep purges it.
    fs::remove_file(root.join("b.py")).unwrap();
    let s2 = index::sweep(&store, root).unwrap();
    assert_eq!(s2.removed, vec!["b.py"]);
    let n: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM symbols WHERE file = 'b.py'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0);
}

#[test]
fn broken_file_is_isolated() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    fs::write(root.join("good.py"), "def ok():\n    return 1\n").unwrap();
    // Tree-sitter is resilient; even garbage parses with ERROR nodes, so an
    // unreadable file (invalid UTF-8) is the isolation case that matters.
    fs::write(root.join("bad.py"), [0xFFu8, 0xFE, 0x00, 0x80]).unwrap();

    let store = Store::open_in_memory().unwrap();
    let report = index::full_index(&store, root).unwrap();
    let ok_syms: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM symbols WHERE file = 'good.py'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(ok_syms, 1, "good file must index despite bad sibling");
    assert!(report.files >= 1);
}
