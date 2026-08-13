//! Per-language extraction coverage (invariant I7): every shipped grammar
//! proves it can extract a function, a class with a method, an import, and
//! a call, with correct FQNs after indexing.
//!
//! Every grammar ALSO pins the full `(parents, name, kind)` tuple of every
//! symbol it extracts, both on the flat fixture and on a nested one
//! (invariant I-F3). `names()` alone is blind to scope: a symbol can move from
//! `<file>.f` to `<file>.Scope.f` while every name assertion stays green, and
//! that silent respelling false-stales every anchor a user already stored.
//! `fqns()` is what turns such a move into a test failure.

use limpet::index::{self, extract, lang::Lang};
use limpet::memory::anchor;
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

/// Every symbol as `"<parents joined with '.'>|<name>|<kind>"`, sorted.
///
/// The persisted FQN is `<file>.<parents>.<name>`, so `parents` is exactly the
/// half of the spelling that `names()` cannot see. Sorting makes the pin
/// independent of walk order, so a pure traversal-order change does not churn
/// the fixtures while a scope change still fails loudly.
fn fqns(facts: &extract::FileFacts) -> Vec<String> {
    let mut rows: Vec<String> = facts
        .symbols
        .iter()
        .map(|s| format!("{}|{}|{}", s.parents.join("."), s.name, s.kind))
        .collect();
    rows.sort();
    rows
}

/// Extract `src` as `lang` and return the sorted FQN pin for it.
fn fqns_of(lang: Lang, src: &str) -> Vec<String> {
    fqns(&extract::extract(lang, src).unwrap())
}

/// Every symbol as `"<parents>|<name>|<kind>|<disamb or ->"`, sorted.
///
/// `fqns_of` deliberately stops at the spelling, so it cannot tell a scope
/// segment from a discriminator: a fix that puts `class << self` into the FQN
/// and a fix that puts it into `disamb` are indistinguishable to it as long as
/// the row counts match. Where a pin exists to prove WHICH of the two a shape
/// takes, it asserts through this instead.
fn slots_of(lang: Lang, src: &str) -> Vec<String> {
    let facts = extract::extract(lang, src).unwrap();
    let mut rows: Vec<String> = facts
        .symbols
        .iter()
        .map(|s| {
            format!(
                "{}|{}|{}|{}",
                s.parents.join("."),
                s.name,
                s.kind,
                s.disamb.as_deref().unwrap_or("-")
            )
        })
        .collect();
    rows.sort();
    rows
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
    assert_eq!(
        fqns(&facts),
        ["ScanQueue|push|method", "|ScanQueue|class", "|top_level|function"]
    );
    assert!(facts.imports.iter().any(|i| i.contains("Mailer")), "{:?}", facts.imports);
    assert!(facts
        .calls
        .iter()
        .any(|c| c.name == "top_level" && c.callee == "helper_call"));
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
    assert_eq!(
        fqns(&facts),
        ["ScanQueue|push|method", "|ScanQueue|class", "|topLevel|function"]
    );
    assert_eq!(facts.imports, vec!["./helper.js"]);
    assert!(facts
        .calls
        .iter()
        .any(|c| c.name == "topLevel" && c.callee == "helper"));
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
    assert_eq!(
        fqns(&facts),
        ["ScanQueue|push|method", "|ScanQueue|class", "|topLevel|function"]
    );
    assert_eq!(facts.imports, vec!["./helper"]);
    assert!(facts
        .calls
        .iter()
        .any(|c| c.name == "topLevel" && c.callee == "helper"));
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
    assert_eq!(
        fqns(&facts),
        ["ScanQueue|push|method", "|ScanQueue|class", "|top_level|function"]
    );
    assert!(facts.imports.iter().any(|i| i == "os"));
    assert!(facts
        .calls
        .iter()
        .any(|c| c.name == "top_level" && c.callee == "helper_call"));
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
    assert_eq!(
        fqns(&facts),
        ["ScanQueue|push|method", "|ScanQueue|class", "|top_level|function"]
    );
    assert!(facts.imports.iter().any(|i| i.contains("HashMap")));
    assert!(facts
        .calls
        .iter()
        .any(|c| c.name == "top_level" && c.callee == "helper_call"));
}

#[test]
fn cpp_extraction() {
    let src = r#"
#include <vector>
#include "GLGaeaClient.h"

int top_level(int x) {
    helper_call(x);
    return x + 1;
}

class ScanQueue {
public:
    void push(int item) {
        this->validate(item);
    }
};

void GLGaeaClient::GetSkinChar(int id) {
    Lookup::Find(id);
}
"#;
    let facts = extract::extract(Lang::Cpp, src).unwrap();
    assert_eq!(names(&facts, "function"), vec!["top_level"]);
    assert_eq!(names(&facts, "class"), vec!["ScanQueue"]);
    // `void GLGaeaClient::GetSkinChar(...)` is an out-of-line member of
    // GLGaeaClient, so it is a method, not a file-scope function. Its FQN
    // already scoped under the qualifier (file.GLGaeaClient.GetSkinChar);
    // kind now agrees with the FQN instead of contradicting it.
    assert_eq!(names(&facts, "method"), vec!["push", "GetSkinChar"]);
    // I-F3 pin: the qualifier `GLGaeaClient` IS part of the spelling. Without
    // this tuple assertion the whole suite stays green while the FQN moves.
    assert_eq!(
        fqns(&facts),
        [
            "GLGaeaClient|GetSkinChar|method",
            "ScanQueue|push|method",
            "|ScanQueue|class",
            "|top_level|function",
        ]
    );
    assert!(facts.imports.iter().any(|i| i == "vector"), "{:?}", facts.imports);
    assert!(facts.imports.iter().any(|i| i == "GLGaeaClient.h"), "{:?}", facts.imports);
    assert!(facts
        .calls
        .iter()
        .any(|c| c.name == "top_level" && c.callee == "helper_call"));
    // The call fact carries the FULL caller scope, qualifier included, so the
    // persisted caller_fqn equals the caller's own symbols.fqn.
    assert!(facts
        .calls
        .iter()
        .any(|c| c.parents == ["GLGaeaClient"]
            && c.name == "GetSkinChar"
            && c.callee == "Find"),
        "{:?}", facts.calls);
    assert!(facts
        .calls
        .iter()
        .any(|c| c.parents.is_empty()
            && c.name == "top_level"
            && c.callee == "helper_call"),
        "a file-scope caller carries no parents: {:?}", facts.calls);
}

#[test]
fn cpp_non_utf8_source_keeps_file_level_row() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    fs::write(root.join("good.cpp"), "int ok(int a) { return a + 1; }\n").unwrap();
    // CP949-encoded comment bytes (legacy Korean engine source): a grammar
    // match must degrade to a file-level row, never drop the file (I-N1).
    let mut cp949 = b"// ".to_vec();
    cp949.extend_from_slice(&[0xB0, 0xA1, 0xB3, 0xAA, 0xB4, 0xD9]);
    cp949.extend_from_slice(b"\nint legacy(int a) { return a + 1; }\n");
    fs::write(root.join("legacy.cpp"), &cp949).unwrap();

    let store = Store::open_in_memory().unwrap();
    let report = index::full_index(&store, root).unwrap();
    assert_eq!(report.files, 2);
    assert!(report.failed.is_empty(), "decode fallback is not a failure: {:?}", report.failed);

    let (lang, hash): (Option<String>, String) = store
        .conn
        .query_row("SELECT lang, hash FROM files WHERE path = 'legacy.cpp'", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(lang, None, "undecodable source is file-level, not cpp");
    assert!(!hash.is_empty());
    let syms: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM symbols WHERE file = 'good.cpp'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(syms, 1, "UTF-8 sibling still gets symbols");
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
    let s0 = index::sweep(&store, root, &Default::default()).unwrap();
    assert!(s0.reindexed.is_empty() && s0.dirty.is_empty() && s0.removed.is_empty());

    // Touch one file with new content and a bumped mtime.
    std::thread::sleep(std::time::Duration::from_millis(20));
    fs::write(root.join("a.py"), "def alpha():\n    return 42\n").unwrap();
    let s1 = index::sweep(&store, root, &Default::default()).unwrap();
    assert_eq!(s1.reindexed, vec!["a.py"]);

    // Delete the other: sweep purges it.
    fs::remove_file(root.join("b.py")).unwrap();
    let s2 = index::sweep(&store, root, &Default::default()).unwrap();
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

#[test]
fn go_extraction() {
    let src = r#"
package main

import "fmt"

type Animal struct{}

type Dog struct {
    Animal
}

func (d Dog) Speak() string {
    return bark()
}

func bark() string {
    fmt.Println("woof")
    return "woof"
}
"#;
    let facts = extract::extract(Lang::Go, src).unwrap();
    assert!(names(&facts, "function").contains(&"bark".to_string()), "{:?}", facts.symbols);
    assert!(names(&facts, "method").contains(&"Speak".to_string()), "{:?}", facts.symbols);
    assert!(names(&facts, "class").contains(&"Dog".to_string()), "{:?}", facts.symbols);
    // Go methods are declared at file scope, so the receiver never becomes a
    // parent segment; the receiver type lives in `disamb` instead.
    assert_eq!(
        fqns(&facts),
        ["|Animal|class", "|Dog|class", "|Speak|method", "|bark|function"]
    );
    assert!(facts.imports.iter().any(|i| i.contains("fmt")), "{:?}", facts.imports);
    assert!(facts.calls.iter().any(|c| c.name == "Speak" && c.callee == "bark"));
    // Go embedding surfaces as an `embeds` inherit edge.
    assert!(
        facts
            .inherits
            .iter()
            .any(|i| i.name == "Dog" && i.parent_name == "Animal" && i.rel == "embeds"),
        "{:?}",
        facts.inherits
    );
}

#[test]
fn java_extraction() {
    let src = r#"
package app;
import app.services.Mailer;

interface Pet {}

class Animal {}

class Dog extends Animal implements Pet {
    public String speak() {
        return bark();
    }
}
"#;
    let facts = extract::extract(Lang::Java, src).unwrap();
    assert!(names(&facts, "class").contains(&"Dog".to_string()), "{:?}", facts.symbols);
    assert!(names(&facts, "method").contains(&"speak".to_string()), "{:?}", facts.symbols);
    // `package app;` is NOT a parent segment: the file path already scopes the
    // FQN, so adding it would double-scope every Java symbol.
    assert_eq!(
        fqns(&facts),
        ["Dog|speak|method", "|Animal|class", "|Dog|class", "|Pet|class"]
    );
    assert!(facts.imports.iter().any(|i| i.contains("Mailer")), "{:?}", facts.imports);
    assert!(facts.calls.iter().any(|c| c.name == "speak" && c.callee == "bark"));
    assert!(facts.inherits.iter().any(|i| i.name=="Dog" && i.parent_name=="Animal" && i.rel=="extends"), "{:?}", facts.inherits);
    assert!(facts.inherits.iter().any(|i| i.name=="Dog" && i.parent_name=="Pet" && i.rel=="implements"), "{:?}", facts.inherits);
}

#[test]
fn java_hash_is_cosmetic_invariant_and_edit_sensitive() {
    let a = r#"
class Foo {
    public String speak() {
        return bark();
    }
}
"#;
    // cosmetic: extra whitespace + a comment, same semantics
    let b = r#"
class Foo {
    // a comment
    public String speak() {

        return  bark() ;
    }
}
"#;
    // semantic: changed return value
    let c = r#"
class Foo {
    public String speak() {
        return woof();
    }
}
"#;

    let hash_of = |src: &str| {
        let facts = extract::extract(Lang::Java, src).unwrap();
        let sym = facts
            .symbols
            .iter()
            .find(|s| s.name == "speak")
            .unwrap_or_else(|| panic!("no speak symbol in: {src}"));
        anchor::ast_body_hash(Lang::Java, src, sym.byte_range).unwrap()
    };

    let ha = hash_of(a);
    let hb = hash_of(b);
    let hc = hash_of(c);

    assert_eq!(ha, hb, "cosmetic change (whitespace/comment) must not alter body_hash");
    assert_ne!(ha, hc, "semantic edit must alter body_hash");
}

#[test]
fn go_hash_is_cosmetic_invariant_and_edit_sensitive() {
    let a = "package main\nfunc bark() string { return \"woof\" }\n";
    let b = "package main\n// a comment\nfunc bark()  string  {  return \"woof\"  }\n";
    let c = "package main\nfunc bark() string { return \"bark\" }\n";

    let hash_of = |src: &str| {
        let facts = extract::extract(Lang::Go, src).unwrap();
        let sym = facts
            .symbols
            .iter()
            .find(|s| s.name == "bark")
            .unwrap_or_else(|| panic!("no bark symbol in: {src}"));
        anchor::ast_body_hash(Lang::Go, src, sym.byte_range).unwrap()
    };

    let ha = hash_of(a);
    let hb = hash_of(b);
    let hc = hash_of(c);

    assert_eq!(ha, hb, "cosmetic change (whitespace/comment) must not alter body_hash");
    assert_ne!(ha, hc, "semantic edit must alter body_hash");
}

#[test]
fn java_interface_extends() {
    let src = "interface Walkable {}\ninterface Runner extends Walkable {}\n";
    let facts = extract::extract(Lang::Java, src).unwrap();
    assert!(
        facts.inherits.iter().any(|i| i.name == "Runner"
            && i.parent_name == "Walkable"
            && i.rel == "extends"),
        "interface extends must produce an extends edge: {:?}",
        facts.inherits
    );
}

#[test]
fn ruby_extraction() {
    let src = r#"
require 'mailer'

module Walkable
end

class Animal
end

class Dog < Animal
  include Walkable
  def speak(name)
    greeting = "hi"
    bark()
  end
end
"#;
    let facts = extract::extract(Lang::Ruby, src).unwrap();
    assert!(names(&facts, "class").contains(&"Dog".to_string()), "{:?}", facts.symbols);
    assert!(names(&facts, "method").contains(&"speak".to_string()), "{:?}", facts.symbols);
    assert_eq!(
        fqns(&facts),
        ["Dog|speak|method", "|Animal|class", "|Dog|class", "|Walkable|class"]
    );
    assert!(facts.imports.iter().any(|i| i.contains("mailer")), "{:?}", facts.imports);
    assert!(facts.calls.iter().any(|c| c.name == "speak" && c.callee == "bark"),
        "speak -> bark call edge missing: {:?}", facts.calls);
    assert!(facts.inherits.iter().any(|i| i.name=="Dog" && i.parent_name=="Animal" && i.rel=="extends"), "{:?}", facts.inherits);
    assert!(facts.inherits.iter().any(|i| i.name=="Dog" && i.parent_name=="Walkable" && i.rel=="mixin"), "{:?}", facts.inherits);
    assert!(!facts.calls.iter().any(|c| c.callee == "name" || c.callee == "greeting"),
        "local vars and params must not be recorded as calls: {:?}", facts.calls);
}

#[test]
fn ruby_hash_is_cosmetic_invariant_and_edit_sensitive() {
    // baseline: speak method
    let a = r#"
class Dog
  def speak
    bark
  end
end
"#;
    // cosmetic: extra blank line + comment, same semantics
    let b = r#"
class Dog
  # says woof
  def speak

    bark
  end
end
"#;
    // semantic: changed callee
    let c = r#"
class Dog
  def speak
    woof
  end
end
"#;

    let hash_of = |src: &str| {
        let facts = extract::extract(Lang::Ruby, src).unwrap();
        let sym = facts
            .symbols
            .iter()
            .find(|s| s.name == "speak")
            .unwrap_or_else(|| panic!("no speak symbol in: {src}"));
        anchor::ast_body_hash(Lang::Ruby, src, sym.byte_range).unwrap()
    };

    let ha = hash_of(a);
    let hb = hash_of(b);
    let hc = hash_of(c);

    assert_eq!(ha, hb, "cosmetic change (whitespace/comment) must not alter body_hash");
    assert_ne!(ha, hc, "semantic edit must alter body_hash");
}

#[test]
fn csharp_extraction() {
    let src = r#"
using App.Services;

interface IPet {}

class Animal {}

class Dog : Animal, IPet {
    public string Speak() {
        return Bark();
    }
}
"#;
    let facts = extract::extract(Lang::CSharp, src).unwrap();
    assert!(names(&facts, "class").contains(&"Dog".to_string()), "{:?}", facts.symbols);
    assert!(names(&facts, "method").contains(&"Speak".to_string()), "{:?}", facts.symbols);
    assert_eq!(
        fqns(&facts),
        ["Dog|Speak|method", "|Animal|class", "|Dog|class", "|IPet|class"]
    );
    assert!(facts.imports.iter().any(|i| i.contains("App.Services")), "{:?}", facts.imports);
    assert!(facts.calls.iter().any(|c| c.name == "Speak" && c.callee == "Bark"));
    // base-list: both entries recorded as extends (class vs interface not
    // distinguished syntactically; resolved read-time, labeled).
    assert!(facts.inherits.iter().any(|i| i.name=="Dog" && i.parent_name=="Animal" && i.rel=="extends"), "{:?}", facts.inherits);
    assert!(facts.inherits.iter().any(|i| i.name=="Dog" && i.parent_name=="IPet" && i.rel=="extends"), "{:?}", facts.inherits);
}

#[test]
fn csharp_hash_is_cosmetic_invariant_and_edit_sensitive() {
    // baseline: Speak method
    let a = r#"
class Dog {
    public string Speak() {
        return Bark();
    }
}
"#;
    // cosmetic: extra whitespace + comment, same semantics
    let b = r#"
class Dog {
    // says woof
    public string Speak() {

        return  Bark() ;
    }
}
"#;
    // semantic: changed callee
    let c = r#"
class Dog {
    public string Speak() {
        return Woof();
    }
}
"#;

    let hash_of = |src: &str| {
        let facts = extract::extract(Lang::CSharp, src).unwrap();
        let sym = facts
            .symbols
            .iter()
            .find(|s| s.name == "Speak")
            .unwrap_or_else(|| panic!("no Speak symbol in: {src}"));
        anchor::ast_body_hash(Lang::CSharp, src, sym.byte_range).unwrap()
    };

    let ha = hash_of(a);
    let hb = hash_of(b);
    let hc = hash_of(c);

    assert_eq!(ha, hb, "cosmetic change (whitespace/comment) must not alter body_hash");
    assert_ne!(ha, hc, "semantic edit must alter body_hash");
}

#[test]
fn bash_extraction() {
    let src = r#"#!/bin/bash
source ./helpers.sh

greet() {
    hello_world
}
"#;
    let facts = extract::extract(Lang::Bash, src).unwrap();
    assert!(names(&facts, "function").contains(&"greet".to_string()), "{:?}", facts.symbols);
    assert_eq!(fqns(&facts), ["|greet|function"]);
    assert!(facts.calls.iter().any(|c| c.name == "greet" && c.callee == "hello_world"), "{:?}", facts.calls);
    assert!(facts.imports.iter().any(|i| i.contains("helpers.sh")), "{:?}", facts.imports);
    // Bash produces no inheritance edges.
    assert!(facts.inherits.is_empty(), "{:?}", facts.inherits);
}

#[test]
fn bash_hash_is_cosmetic_invariant_and_edit_sensitive() {
    // baseline: greet function
    let a = "#!/bin/bash\ngreet() {\n    hello_world\n}\n";
    // cosmetic: extra blank line + comment, same semantics
    let b = "#!/bin/bash\n# greets the world\ngreet() {\n\n    hello_world\n}\n";
    // semantic: changed callee
    let c = "#!/bin/bash\ngreet() {\n    goodbye_world\n}\n";

    let hash_of = |src: &str| {
        let facts = extract::extract(Lang::Bash, src).unwrap();
        let sym = facts
            .symbols
            .iter()
            .find(|s| s.name == "greet")
            .unwrap_or_else(|| panic!("no greet symbol in: {src}"));
        anchor::ast_body_hash(Lang::Bash, src, sym.byte_range).unwrap()
    };

    let ha = hash_of(a);
    let hb = hash_of(b);
    let hc = hash_of(c);

    assert_eq!(ha, hb, "cosmetic change (whitespace/comment) must not alter body_hash");
    assert_ne!(ha, hc, "semantic edit must alter body_hash");
}

// ---------------------------------------------------------------------------
// I-F3: FQN spelling pins on a NESTED shape, one per shipped grammar.
//
// A flat fixture cannot detect a scoping change: every symbol sits at
// `parents = []` there, so a new scope node can start pushing without moving a
// single assertion. Each test below feeds its grammar a shape with real
// nesting (a type in a type, a function in a function, a module/namespace
// scope) and pins the ENTIRE sorted `(parents, name, kind)` list. Adding a
// scope, dropping one, or respelling one is now a test failure with a diff
// that names the symbol that moved.
//
// These lists were not transcribed from node-types.json. They are the runtime
// output of the pre/post differential harness whose result is recorded in
// .superpowers/sdd/review-T2b-c.diff, where every changed spelling is
// attributed to one of the three sanctioned scope fixes.
// ---------------------------------------------------------------------------

#[test]
fn fqn_pin_php_nested() {
    let src = r#"<?php
function outer() {
    function inner() {
        deep();
    }
    inner();
}

interface Runner {}

trait Loggable {
    public function log($m) {
        write_log($m);
    }
}
"#;
    assert_eq!(
        fqns_of(Lang::Php, src),
        [
            "Loggable|log|method",
            "outer|inner|function",
            "|Loggable|class",
            "|Runner|class",
            "|outer|function",
        ]
    );
}

#[test]
fn fqn_pin_php_namespace_scoping() {
    // SANCTIONED FIX 2. Bracketed `namespace A { }` is the only form that can
    // appear more than once per file, so it is the only collision class, and
    // it is the only form that scopes.
    let bracketed = r#"<?php
namespace A {
    class Job {
        public function run() {}
    }
    function helper() {}
}
namespace B {
    class Job {
        public function run() {}
    }
    function helper() {}
}
"#;
    assert_eq!(
        fqns_of(Lang::Php, bracketed),
        [
            "A.Job|run|method",
            "A|Job|class",
            "A|helper|function",
            "B.Job|run|method",
            "B|Job|class",
            "B|helper|function",
        ],
        "two same-named classes in one file must not share an FQN"
    );

    // Unbracketed `namespace A;` scopes the whole file and cannot repeat, so
    // it stays invisible: adding a segment here would respell every symbol in
    // every namespaced PHP file for no disambiguation gain.
    let unbracketed = r#"<?php
namespace App\Core;

class Job {
    public function run() {
        go();
    }
}

function helper() {}
"#;
    assert_eq!(
        fqns_of(Lang::Php, unbracketed),
        ["Job|run|method", "|Job|class", "|helper|function"]
    );
}

#[test]
fn fqn_pin_js_nested() {
    let src = r#"
class Outer {
    run() {
        function inner() {
            deep();
        }
        return inner();
    }
}

function wrapper() {
    function nested() {}
    return nested;
}

function* gen() {
    yield 1;
}
"#;
    assert_eq!(
        fqns_of(Lang::Js, src),
        [
            "Outer.run|inner|function",
            "Outer|run|method",
            "wrapper|nested|function",
            "|Outer|class",
            "|gen|function",
            "|wrapper|function",
        ]
    );
}

#[test]
fn fqn_pin_ts_nested() {
    let src = r#"
abstract class Base {
    abstract go(): void;
}

class Outer extends Base {
    go(): void {
        function inner(): void {}
        inner();
    }
}

function wrapper(): void {
    function nested(): void {}
}

function* gen(): Iterator<number> {
    yield 1;
}
"#;
    assert_eq!(
        fqns_of(Lang::Ts, src),
        [
            "Outer.go|inner|function",
            "Outer|go|method",
            "wrapper|nested|function",
            "|Base|class",
            "|Outer|class",
            "|gen|function",
            "|wrapper|function",
        ]
    );
}

#[test]
fn fqn_pin_py_nested() {
    let src = r#"
class Outer:
    class Inner:
        def deep(self):
            pass

    def method(self):
        def helper():
            pass
        return helper()

def free():
    def inner():
        pass
    return inner
"#;
    assert_eq!(
        fqns_of(Lang::Py, src),
        [
            "Outer.Inner|deep|method",
            "Outer.method|helper|method",
            "Outer|Inner|class",
            "Outer|method|method",
            "free|inner|function",
            "|Outer|class",
            "|free|function",
        ]
    );
}

#[test]
fn fqn_pin_rust_mod_scoping() {
    // SANCTIONED FIX 1. `mod` emits no symbol row of its own but DOES scope:
    // without it, `a::f`, `b::f`, `b::deep::f` and the crate-root `f` all
    // collapse onto one FQN. Declaration-only `mod c;` has no body and
    // contributes nothing.
    let src = r#"
mod a {
    pub fn f() {}
    pub struct S;
    impl S {
        pub fn m(&self) {}
    }
}

mod b {
    pub fn f() {}
    pub mod deep {
        pub fn f() {}
    }
}

mod c;

fn f() {}
"#;
    let rows = fqns_of(Lang::Rust, src);
    assert_eq!(
        rows,
        [
            "a.S|m|method",
            "a|S|class",
            "a|f|function",
            "b.deep|f|function",
            "b|f|function",
            "|f|function",
        ]
    );
    assert!(
        !rows.iter().any(|r| r.ends_with("|a|class")
            || r.ends_with("|b|class")
            || r.ends_with("|c|class")),
        "mod is a scope, never a symbol row: {rows:?}"
    );
}

#[test]
fn fqn_pin_rust_nested() {
    let src = r#"
struct T;

trait Speak {}

impl Speak for T {
    fn go(&self) {}
}

impl T {
    fn go(&self) {
        fn helper() {}
        helper();
    }
}

fn outer() {
    fn inner() {}
    inner();
}

enum E {
    A,
}

impl E {
    fn e(&self) {}
}

const K: u32 = 1;
static S: u32 = 2;
"#;
    // `impl` contributes the TYPE name, not an `impl` segment, so both `go`
    // methods land on `T.go` and are told apart by `disamb`, never by FQN.
    assert_eq!(
        fqns_of(Lang::Rust, src),
        [
            "E|e|method",
            "T.go|helper|method",
            "T|go|method",
            "T|go|method",
            "outer|inner|function",
            "|E|class",
            "|K|const",
            "|Speak|class",
            "|S|const",
            "|T|class",
            "|outer|function",
        ]
    );
}

#[test]
fn fqn_pin_cpp_qualified_out_of_line() {
    // SANCTIONED FIX 3. An out-of-line `void ns::A::inline_m(...)` is a member
    // of `ns::A`; every qualifier segment becomes a parent, so the definition
    // shares an FQN with the in-class declaration instead of squatting at file
    // scope. A destructor takes the same path.
    let src = r#"
namespace ns {
class A {
public:
    void inline_m(int x);
    ~A();
};
void ns_free(int x) {
    helper(x);
}
}

void ns::A::inline_m(int x) {
    helper(x);
}

ns::A::~A() {}

int plain(int x) {
    return x;
}
"#;
    assert_eq!(
        fqns_of(Lang::Cpp, src),
        [
            "ns.A|inline_m|method",
            "ns.A|~A|method",
            "ns|A|class",
            "ns|ns_free|function",
            "|plain|function",
        ]
    );
}

#[test]
fn fqn_pin_cpp_nested() {
    let src = r#"
class Outer {
public:
    class Inner {
    public:
        void deep() {}
    };
    void run() {
        helper();
    }
};

struct S {
    void m() {}
};

enum Color { RED, GREEN };
"#;
    assert_eq!(
        fqns_of(Lang::Cpp, src),
        [
            "Outer.Inner|deep|method",
            "Outer|Inner|class",
            "Outer|run|method",
            "S|m|method",
            "|Color|class",
            "|Outer|class",
            "|S|class",
        ]
    );
}

#[test]
fn fqn_pin_go_nested() {
    let src = r#"
package main

type (
    A struct{}
    B struct{}
)

type Walker interface {
    Walk() string
}

func (a A) String() string {
    return helperA()
}

func (b *B) String() string {
    return helperB()
}

func free() string {
    return ""
}
"#;
    // Both `String` methods share the file-scope FQN by design: Go declares
    // methods at file scope, and the receiver type separates them in `disamb`.
    assert_eq!(
        fqns_of(Lang::Go, src),
        [
            "|A|class",
            "|B|class",
            "|String|method",
            "|String|method",
            "|Walker|class",
            "|free|function",
        ]
    );
}

#[test]
fn fqn_pin_java_nested() {
    let src = r#"
class Outer {
    class Inner {
        public void deep() {
            go();
        }
    }

    interface Cb {
        void call();
    }

    public void run() {
        helper();
    }

    enum Color {
        RED;
        public void tint() {}
    }
}
"#;
    // `enum Color` IS a type scope and emits a class row of its own, exactly
    // like `class Inner`: `tint` scopes to `Outer.Color`, not to `Outer`. The
    // earlier spelling put every nested enum's methods in their PARENT's
    // namespace, so two enums in one class with a same-named method shared one
    // FQN and one `()` slot and masked each other's edits. `record` behaves
    // identically (covered by disamb_java_enum_and_record_scope_their_methods).
    assert_eq!(
        fqns_of(Lang::Java, src),
        [
            "Outer.Cb|call|method",
            "Outer.Color|tint|method",
            "Outer.Inner|deep|method",
            "Outer|Cb|class",
            "Outer|Color|class",
            "Outer|Inner|class",
            "Outer|run|method",
            "|Outer|class",
        ]
    );
}

#[test]
fn fqn_pin_ruby_nested() {
    let src = r#"
module Outer
  class Inner
    def deep
      go
    end
  end

  def mod_method
  end
end

class C
  def self.build
    new
  end

  def outer
    def inner
    end
  end
end

def free_fn
  def nested_fn
  end
end
"#;
    // A `module` emits a class row AND scopes (its `def`s are real methods),
    // unlike Rust `mod` and PHP/C++ `namespace`, which emit no row.
    assert_eq!(
        fqns_of(Lang::Ruby, src),
        [
            "C.outer|inner|method",
            "C|build|method",
            "C|outer|method",
            "Outer.Inner|deep|method",
            "Outer|Inner|class",
            "Outer|mod_method|method",
            "free_fn|nested_fn|function",
            "|C|class",
            "|Outer|class",
            "|free_fn|function",
        ]
    );
}

#[test]
fn fqn_pin_csharp_nested() {
    let src = r#"
namespace App.Core {
    class Outer {
        class Inner {
            public void Deep() {
                Go();
            }
        }

        public void Run() {
            Helper();
        }
    }

    struct S {
        public void M() {}
    }

    interface IX {
        void Go();
    }
}
"#;
    // Block `namespace App.Core { ... }` IS a scope, on PHP's exact rule: it is
    // the only C# namespace form a file can legally repeat, so it is the only
    // one that can collide, and two `namespace X { class Options { ... } }`
    // blocks in one file otherwise produced one FQN and one `()` slot per
    // method. The dotted name enters as a SINGLE segment, matching the source
    // spelling. `namespace A;` (file_scoped_namespace_declaration) is a
    // different grammar node and stays invisible, so every modern C# file keeps
    // the FQNs it already had; that half is pinned by
    // fqn_pin_csharp_file_scoped_namespace_is_not_a_scope.
    assert_eq!(
        fqns_of(Lang::CSharp, src),
        [
            "App.Core.IX|Go|method",
            "App.Core.Outer.Inner|Deep|method",
            "App.Core.Outer|Inner|class",
            "App.Core.Outer|Run|method",
            "App.Core.S|M|method",
            "App.Core|IX|class",
            "App.Core|Outer|class",
            "App.Core|S|class",
        ]
    );
}

#[test]
fn fqn_pin_csharp_file_scoped_namespace_is_not_a_scope() {
    let src = r#"
namespace App.Core;

class Outer {
    public void Run() {
        Helper();
    }
}
"#;
    // The negative half of the namespace rule. A file-scoped namespace cannot
    // repeat inside its file, so scoping it would respell every symbol in every
    // modern C# file for no collision it could ever prevent. Pinned so the
    // block-namespace arm cannot quietly grow to cover this form too.
    assert_eq!(
        fqns_of(Lang::CSharp, src),
        ["Outer|Run|method", "|Outer|class"]
    );
}

#[test]
fn fqn_pin_bash_nested() {
    let src = r#"#!/bin/bash
outer() {
    inner() {
        deep_call
    }
    inner
}

function other {
    outer
}
"#;
    assert_eq!(
        fqns_of(Lang::Bash, src),
        ["outer|inner|function", "|other|function", "|outer|function"]
    );
}

// ---------------------------------------------------------------------------
// I-F3 sanctioned-scope pins.
//
// Each of the nine sanctioned FQN spelling changes gets a pin here, and every
// one that has a NEGATIVE bound (a sibling form that must NOT scope) pins that
// half too. A future arm that grows to cover the negative form respells live
// FQNs and false-stales the anchors pointing at them, which is exactly the
// failure the `fqns()` pin exists to turn into a red test.
//
//   S1 Rust  `mod_item` (body)               fqn_pin_rust_mod_scoping
//   S2 PHP   `namespace_definition` (body)   fqn_pin_php_namespace_scoping
//   S3 C++   qualifier segments              fqn_pin_cpp_qualified_out_of_line
//   S4 C#    `namespace_declaration` (body)  fqn_pin_csharp_nested + below
//   S5 TS    `internal_module` (body)        fqn_pin_ts_internal_module_scoping
//   S6 Java  `enum_declaration`              fqn_pin_java_enum_and_record_scoping
//   S7 Java  `record_declaration`            fqn_pin_java_enum_and_record_scoping
//   S8 PHP   `enum_declaration`              fqn_pin_php_enum_scoping
//   S9 C++   qualified `template_function`   fqn_pin_cpp_template_shapes
// ---------------------------------------------------------------------------

#[test]
fn fqn_pin_php_enum_scoping() {
    // S8. A PHP 8.1 `enum` is a type with a body exactly like a class: it emits
    // a class row AND scopes its methods. Without it, two enums in one file gave
    // `label` ONE file-scoped FQN and one `()` slot, so an identical body in
    // either masked an edit to the other, and `map Suit` found nothing.
    let src = r#"<?php
enum Suit: string {
    case Hearts = 'H';
    public function label(): string {
        return tr($this->value);
    }
}

enum Rank: int {
    case Ace = 1;
    public function label(): string {
        return num($this->value);
    }
}
"#;
    assert_eq!(
        fqns_of(Lang::Php, src),
        [
            "Rank|label|method",
            "Suit|label|method",
            "|Rank|class",
            "|Suit|class",
        ],
        "two enums in one file must not share one method FQN"
    );

    // The enum arm composes with the bracketed-namespace arm rather than
    // replacing it: both segments are present, in source order.
    let in_ns = r#"<?php
namespace Cards {
    enum Suit: string {
        case Hearts = 'H';
        public function label(): string { return 'h'; }
    }
}
"#;
    assert_eq!(
        fqns_of(Lang::Php, in_ns),
        ["Cards.Suit|label|method", "Cards|Suit|class"]
    );
}

#[test]
fn fqn_pin_ts_internal_module_scoping() {
    // S5. `namespace A { }` is `internal_module`; the legacy `module A { }` is
    // a distinct `module` node with the same shape (name field + body), and
    // both are TypeScript's exact analogue of PHP's bracketed namespace: a file
    // can legally hold several, so two same-named classes in two namespaces
    // otherwise produced one FQN and one slot per method. String-named ambient
    // modules also carry a body, so the `name` kind is what keeps them out.
    let src = r#"
namespace Alpha {
    export class Options {
        validate(): void {
            check();
        }
    }
    export function helper(): void {}
}

module Beta {
    export class Options {
        validate(): void {
            check();
        }
    }
    export function helper(): void {}
}

namespace Outer.Inner {
    export function deep(): void {}
}
"#;
    assert_eq!(
        fqns_of(Lang::Ts, src),
        [
            "Alpha.Options|validate|method",
            "Alpha|Options|class",
            "Alpha|helper|function",
            "Beta.Options|validate|method",
            "Beta|Options|class",
            "Beta|helper|function",
            "Outer.Inner|deep|function",
        ],
        "two same-named classes in two namespaces must not share an FQN"
    );

    // A dotted `namespace Outer.Inner` enters as ONE segment spelled the way the
    // source spells it, matching the C# block-namespace rule, and neither form
    // emits a symbol row of its own.
    assert!(
        !fqns_of(Lang::Ts, src)
            .iter()
            .any(|r| r.ends_with("|Alpha|class")
                || r.ends_with("|Beta|class")
                || r.contains("|Outer.Inner|")),
        "internal_module is a scope, never a symbol row"
    );
}

#[test]
fn fqn_pin_ts_ambient_module_is_not_a_scope() {
    // The negative half of S5, and the only shape that can tell the two apart:
    // a STRING-named `declare module "x" { }` is the grammar's `module` node,
    // NOT `internal_module`, so it contributes no segment, while an
    // identifier-named `declare namespace Amb { }` IS `internal_module` and does.
    // A member signature emits no row, so the pin rides on the nested CLASS.
    let src = r#"
declare module "ext-a";

declare module "ext-b" {
    class Ext {
        run(): void;
    }
}

declare namespace Amb {
    class Inner {
        run(): void;
    }
}

function afterAmbient(): void {
    go();
}
"#;
    assert_eq!(
        fqns_of(Lang::Ts, src),
        ["Amb|Inner|class", "|Ext|class", "|afterAmbient|function"],
        "a string-named ambient module must not scope, an identifier-named one must"
    );
}

#[test]
fn fqn_pin_java_enum_and_record_scoping() {
    // S6 + S7. `enum` and `record` are types with bodies exactly like `class`.
    // Leaving them out scoped their methods to the ENCLOSING class, so the two
    // nested enums below shared `Holder.tint` with each other AND with Holder's
    // own `tint`: three rows, one FQN, one `()` slot, mutual masking.
    let src = r#"
enum Color {
    RED, GREEN;
    public void tint() {
        shade();
    }
}

record Point(int x, int y) {
    public int sum() {
        return x + y;
    }
}

class Holder {
    public void tint() {}

    enum Inner {
        A;
        public void tint() {}
    }

    enum Other {
        B;
        public void tint() {}
    }

    record Pair(int a, int b) {
        public int sum() {
            return a + b;
        }
    }
}
"#;
    assert_eq!(
        fqns_of(Lang::Java, src),
        [
            "Color|tint|method",
            "Holder.Inner|tint|method",
            "Holder.Other|tint|method",
            "Holder.Pair|sum|method",
            "Holder|Inner|class",
            "Holder|Other|class",
            "Holder|Pair|class",
            "Holder|tint|method",
            "Point|sum|method",
            "|Color|class",
            "|Holder|class",
            "|Point|class",
        ]
    );
}

#[test]
fn fqn_pin_csharp_block_namespaces_separate_twins() {
    // S4, the collision case the single-block pin cannot show: two blocks in one
    // file, each with an `Options.Validate`. Without the segment both rows spell
    // `Options.Validate` and take the same `()` slot.
    let two_blocks = r#"
namespace App.Core {
    class Options {
        public void Validate() {
            Go();
        }
    }
}

namespace App.Edge {
    class Options {
        public void Validate() {
            Go();
        }
    }
}
"#;
    assert_eq!(
        fqns_of(Lang::CSharp, two_blocks),
        [
            "App.Core.Options|Validate|method",
            "App.Core|Options|class",
            "App.Edge.Options|Validate|method",
            "App.Edge|Options|class",
        ]
    );

    // Nested blocks stack, one segment per block, each spelled as written.
    let nested = r#"
namespace Outer {
    namespace Inner {
        class C {
            public void M() {
                Go();
            }
        }
    }
}
"#;
    assert_eq!(
        fqns_of(Lang::CSharp, nested),
        ["Outer.Inner.C|M|method", "Outer.Inner|C|class"]
    );
}

#[test]
fn fqn_pin_ruby_singleton_class_is_not_a_scope() {
    // NEGATIVE bound on the Ruby receiver discriminator. `class << self` twins
    // every `def` inside it against the instance method of the same name, but it
    // is NOT an FQN segment: `map C.build` must still find every half. The split
    // lives entirely in `disamb`, so this asserts through `slots_of` -- an
    // FQN-only pin cannot tell the two designs apart.
    let src = r#"
class C
  def build
    new
  end

  def self.build
    new
  end

  class << self
    def made
      new
    end

    def build
      new
    end
  end
end
"#;
    assert_eq!(
        fqns_of(Lang::Ruby, src),
        [
            "C|build|method",
            "C|build|method",
            "C|build|method",
            "C|made|method",
            "|C|class",
        ],
        "class << self must not add an FQN segment"
    );
    assert_eq!(
        slots_of(Lang::Ruby, src),
        [
            "C|build|method|#",
            "C|build|method|<<self.",
            "C|build|method|self.",
            "C|made|method|<<self.",
            "|C|class|-",
        ],
        "the three `build` twins must occupy three distinct slots"
    );
}

#[test]
fn fqn_pin_cpp_template_shapes() {
    // S3 + S9. A template qualifier enters as one RAW segment (`Box<T>`), and a
    // qualified explicit specialization takes the template's NAME (`f`), not the
    // whole template id (`f<int>`) it used to be spelled with; the arguments move
    // into `disamb` so `N.f` stays the FQN a reader would write while the
    // specialization keeps its own slot.
    let src = r#"
namespace N {
template <typename T>
void f(T x) {}
}

template <>
void N::f<int>(int x) {
    sink(x);
}

template <class T>
class Box {
public:
    void run();
};

template <class T>
void Box<T>::run() {}
"#;
    assert_eq!(
        fqns_of(Lang::Cpp, src),
        [
            "Box<T>|run|method",
            "N|f|function",
            "N|f|method",
            "|Box|class",
        ]
    );

    // The negative bound on the qualifier rule: a global-scope qualifier (`::g`)
    // has no `scope` field, so it contributes no segment, and an ANONYMOUS
    // namespace has no name, so it contributes none either.
    let unqualified = r#"
void ::global_fn(int x) {
    sink(x);
}

namespace {
void anon_fn() {
    sink(0);
}
}
"#;
    assert_eq!(
        fqns_of(Lang::Cpp, unqualified),
        ["|anon_fn|function", "|global_fn|function"]
    );
}

/// Every symbol as `"<ordinal>|<parents>|<name>|<kind>|<start>..<end>"` in
/// EXTRACTION order, unsorted.
///
/// `fqns()` sorts, so it is blind to the two things `ordinal` decides: which
/// `ast_body_hashes` entry pairs with which symbol (`index::index_file_parsed`
/// zips them by position) and which of several same-FQN rows a new anchor
/// takes (`memory::resolve_anchor` breaks ties with `ORDER BY ordinal`). A
/// traversal-order change would repoint anchors at a different twin with every
/// name assertion still green, and adding a symbol class shifts every later
/// ordinal in the file. Byte ranges ride along because they are the other
/// input to the body hash.
fn ordered_pin(lang: Lang, src: &str) -> Vec<String> {
    extract::extract(lang, src)
        .unwrap()
        .symbols
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let (a, b) = s.byte_range;
            format!("{i}|{}|{}|{}|{a}..{b}", s.parents.join("."), s.name, s.kind)
        })
        .collect()
}

#[test]
fn extraction_order_and_byte_ranges_are_pinned() {
    assert_eq!(
        ordered_pin(Lang::Rust, "fn a() {}\nstruct S;\nimpl S { fn m(&self) {} }\nmod q { fn z() {} }\n"),
        [
            "0||a|function|0..9",
            "1||S|class|10..19",
            "2|S|m|method|29..43",
            "3|q|z|function|54..63",
        ]
    );
    assert_eq!(
        ordered_pin(Lang::Py, "def a():\n    pass\n\nclass K:\n    def m(self):\n        pass\n"),
        ["0||a|function|0..17", "1||K|class|19..57", "2|K|m|method|32..57"]
    );
    // C++ carries the shapes most likely to shift: an out-of-line definition
    // pushes qualifier segments, and a reference return descends through a
    // fieldless declarator wrapper.
    assert_eq!(
        ordered_pin(
            Lang::Cpp,
            "class B {\n  int n() { return n_; }\n  int& r() { return n_; }\n};\nvoid B::go() {}\n"
        ),
        [
            "0||B|class|0..62",
            // The reference return: extracted at all only since the fieldless
            // declarator fallback, and it shifts every later ordinal in the file.
            "1|B|n|method|12..34",
            "2|B|r|method|37..60",
            "3|B|go|method|64..79",
        ]
    );
    assert_eq!(
        ordered_pin(Lang::Java, "class C {\n  enum E { X; void t() {} }\n  void m() {}\n}\n"),
        ["0||C|class|0..53", "1|C|E|class|12..37", "2|C.E|t|method|24..35", "3|C|m|method|40..51"]
    );
}

/// A comment between a reference sigil and its declarator is a named child in
/// the C++ grammar; the R1 fieldless-wrapper recovery must skip it, never
/// land the walk on it and extract nothing (whole-branch review 2026-08-04).
#[test]
fn cpp_reference_wrapper_survives_an_interleaved_comment() {
    let src = "struct B {\n    int v[4];\n    int& /* borrowed */ at(int i) { return v[i]; }\n};\n";
    assert_eq!(
        fqns_of(Lang::Cpp, src),
        ["B|at|method", "|B|class"],
        "the commented reference-returning method must still extract"
    );
}
