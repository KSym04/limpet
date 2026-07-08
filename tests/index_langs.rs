//! Per-language extraction coverage (invariant I7): every shipped grammar
//! proves it can extract a function, a class with a method, an import, and
//! a call, with correct FQNs after indexing.

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
    assert_eq!(names(&facts, "function"), vec!["top_level", "GetSkinChar"]);
    assert_eq!(names(&facts, "class"), vec!["ScanQueue"]);
    assert_eq!(names(&facts, "method"), vec!["push"]);
    assert!(facts.imports.iter().any(|i| i == "vector"), "{:?}", facts.imports);
    assert!(facts.imports.iter().any(|i| i == "GLGaeaClient.h"), "{:?}", facts.imports);
    assert!(facts
        .calls
        .iter()
        .any(|(scope, callee)| scope == "top_level" && callee == "helper_call"));
    assert!(facts
        .calls
        .iter()
        .any(|(scope, callee)| scope == "GetSkinChar" && callee == "Find"),
        "{:?}", facts.calls);
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
    assert!(facts.imports.iter().any(|i| i.contains("fmt")), "{:?}", facts.imports);
    assert!(facts.calls.iter().any(|(scope, callee)| scope == "Speak" && callee == "bark"));
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
    assert!(facts.imports.iter().any(|i| i.contains("Mailer")), "{:?}", facts.imports);
    assert!(facts.calls.iter().any(|(scope, callee)| scope == "speak" && callee == "bark"));
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
    assert!(facts.imports.iter().any(|i| i.contains("mailer")), "{:?}", facts.imports);
    assert!(facts.calls.iter().any(|(scope, callee)| scope == "speak" && callee == "bark"),
        "speak -> bark call edge missing: {:?}", facts.calls);
    assert!(facts.inherits.iter().any(|i| i.name=="Dog" && i.parent_name=="Animal" && i.rel=="extends"), "{:?}", facts.inherits);
    assert!(facts.inherits.iter().any(|i| i.name=="Dog" && i.parent_name=="Walkable" && i.rel=="mixin"), "{:?}", facts.inherits);
    assert!(!facts.calls.iter().any(|(_, c)| c == "name" || c == "greeting"),
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
    assert!(facts.imports.iter().any(|i| i.contains("App.Services")), "{:?}", facts.imports);
    assert!(facts.calls.iter().any(|(scope, callee)| scope == "Speak" && callee == "Bark"));
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
    assert!(facts.calls.iter().any(|(scope, callee)| scope == "greet" && callee == "hello_world"), "{:?}", facts.calls);
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
