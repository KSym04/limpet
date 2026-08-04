//! I-F3 differential harness, round 2.
//!
//! Dumps (grammar, fixture, byte_range, parents joined '.', name, kind) for
//! every symbol in a fixture corpus that exercises EVERY `parents.push` arm and
//! EVERY disamb-assignment site present in either tree, plus the forms that
//! must NOT scope.
//!
//! Compiles byte-identically against the pre-branch baseline (2ab0afb) and the
//! branch: it touches only `Sym.parents`, `Sym.name`, `Sym.kind` and
//! `Sym.byte_range`, all four of which exist on both sides. `byte_range` is the
//! JOIN KEY: it identifies the defining node independently of how the walk
//! spells its FQN, so a spelling change pairs up instead of showing as one
//! deletion plus one unrelated insertion.

use limpet::index::extract;
use limpet::index::lang::Lang;

/// (grammar label, lang, fixture label, source).
fn corpus() -> Vec<(&'static str, Lang, &'static str, &'static str)> {
    vec![
        // ================================================================ php
        (
            "php",
            Lang::Php,
            "base",
            r#"<?php
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
"#,
        ),
        // NEGATIVE: unbracketed `namespace A;` must NOT become an FQN segment.
        (
            "php",
            Lang::Php,
            "unbracketed_ns",
            r#"<?php
namespace App\Core;

class Job {
    public function run() {
        go();
    }
}

function helper() {}
"#,
        ),
        // POSITIVE: bracketed `namespace A { }` scopes.
        (
            "php",
            Lang::Php,
            "bracketed_ns",
            r#"<?php
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
"#,
        ),
        // POSITIVE: PHP 8.1 `enum` is a type scope and a symbol row.
        (
            "php",
            Lang::Php,
            "enums",
            r#"<?php
enum Status: string {
    case Draft = 'draft';
    case Live = 'live';

    public function label(): string {
        return ucfirst($this->value);
    }
}

enum Tier: int {
    case Free = 0;

    public function label(): string {
        return 'tier';
    }
}

class Holder {
    public function label(): string { return 'h'; }
}
"#,
        ),
        // Bracketed namespace WITH an enum inside: two scope layers at once.
        (
            "php",
            Lang::Php,
            "ns_enum",
            r#"<?php
namespace N1 {
    enum Status {
        case A;
        public function label(): string { return 'a'; }
    }
}
namespace N2 {
    enum Status {
        case A;
        public function label(): string { return 'b'; }
    }
}
"#,
        ),
        (
            "php",
            Lang::Php,
            "nested",
            r#"<?php
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
"#,
        ),
        // ================================================================= js
        (
            "js",
            Lang::Js,
            "base",
            r#"
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
"#,
        ),
        (
            "js",
            Lang::Js,
            "nested",
            r#"
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
"#,
        ),
        // Accessor pairs: same FQN, split only by the parameter list.
        (
            "js",
            Lang::Js,
            "accessors",
            r#"
class Box {
    get value() {
        return this._v;
    }
    set value(v) {
        this._v = v;
    }
    static get unit() {
        return 'px';
    }
    static set unit(u) {
        this._u = u;
    }
    plain(a, b) {
        return a + b;
    }
}
"#,
        ),
        // ================================================================= ts
        (
            "ts",
            Lang::Ts,
            "base",
            r#"
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
"#,
        ),
        (
            "ts",
            Lang::Ts,
            "nested",
            r#"
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
"#,
        ),
        (
            "ts",
            Lang::Ts,
            "accessors",
            r#"
class Box {
    private _v: number = 0;
    get value(): number {
        return this._v;
    }
    set value(v: number) {
        this._v = v;
    }
}
"#,
        ),
        // POSITIVE: `namespace A { }` (internal_module) and `module B { }`
        // (a distinct `module` node; scoped since the S5 widening, 53b5e6f).
        (
            "ts",
            Lang::Ts,
            "namespaces",
            r#"
namespace Alpha {
    export class Options {
        validate(): void {}
    }
    export function helper(): void {}
}

module Beta {
    export class Options {
        validate(): void {}
    }
    export function helper(): void {}
}

namespace Outer.Inner {
    export function deep(): void {}
}
"#,
        ),
        // NEGATIVE: a body-less ambient module declaration contributes nothing.
        (
            "ts",
            Lang::Ts,
            "ambient_module",
            r#"
declare module "side-effect";

function afterwards(): void {}
"#,
        ),
        // ================================================================= py
        (
            "py",
            Lang::Py,
            "base",
            r#"
import os
from app.services import mailer

def top_level(x):
    helper_call(x)
    return x + 1

class ScanQueue:
    def push(self, item):
        self.validate(item)
"#,
        ),
        (
            "py",
            Lang::Py,
            "nested",
            r#"
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
"#,
        ),
        // property / setter / overload set: one name, several definitions.
        (
            "py",
            Lang::Py,
            "properties",
            r#"
from typing import overload

class Cfg:
    @property
    def path(self):
        return self._p

    @path.setter
    def path(self, value):
        self._p = value

    @path.deleter
    def path(self):
        del self._p

    @staticmethod
    def build(a, b):
        return Cfg()

    @classmethod
    def of(cls, a):
        return cls()

@overload
def load(a: int) -> int: ...

@overload
def load(a: str) -> str: ...

def load(a):
    return a
"#,
        ),
        // =============================================================== rust
        (
            "rust",
            Lang::Rust,
            "base",
            r#"
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
"#,
        ),
        // POSITIVE `mod a { }` / nested `mod deep`; NEGATIVE `mod c;`.
        (
            "rust",
            Lang::Rust,
            "mods",
            r#"
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
"#,
        ),
        // Inherent impl vs trait impl on ONE type, same method name, plus a
        // nested helper, a const and a static inside each.
        (
            "rust",
            Lang::Rust,
            "impl_pairs",
            r#"
struct T;

trait Speak {
    fn go(&self);
}

trait Shout {
    fn go(&self);
}

impl Speak for T {
    fn go(&self) {
        fn helper() {}
        helper();
    }
}

impl Shout for T {
    fn go(&self) {
        fn helper() {}
        helper();
    }
}

impl T {
    const K: u32 = 1;
    fn go(&self) {
        fn helper() {}
        helper();
    }
}

impl std::fmt::Display for T {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        Ok(())
    }
}
"#,
        ),
        (
            "rust",
            Lang::Rust,
            "nested",
            r#"
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
"#,
        ),
        // Generic impl: the impl scope segment is the raw type text.
        (
            "rust",
            Lang::Rust,
            "generic_impl",
            r#"
struct Wrap<T>(T);

trait Peek {
    fn peek(&self);
}

impl<T> Peek for Wrap<T> {
    fn peek(&self) {}
}

impl<T> Wrap<T> {
    fn peek_inherent(&self) {}
}

mod inner {
    pub struct W;
    impl W {
        pub fn go(&self) {}
    }
    pub const C: u32 = 3;
}
"#,
        ),
        // ================================================================ cpp
        (
            "cpp",
            Lang::Cpp,
            "base",
            r#"
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
"#,
        ),
        // Out-of-line qualified definitions, destructor, nested qualifiers.
        (
            "cpp",
            Lang::Cpp,
            "qualified",
            r#"
namespace ns {
class A {
public:
    void inline_m(int x);
    ~A();
    class B {
    public:
        void deep();
    };
};
void ns_free(int x) {
    helper(x);
}
}

void ns::A::inline_m(int x) {
    helper(x);
}

ns::A::~A() {}

void ns::A::B::deep() {
    helper(0);
}

int plain(int x) {
    return x;
}
"#,
        ),
        (
            "cpp",
            Lang::Cpp,
            "nested",
            r#"
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
"#,
        ),
        // Templates: template_declaration parameters, template_function
        // arguments (explicit specialization), and a templated out-of-line
        // member definition whose qualifier is `A<T>`.
        (
            "cpp",
            Lang::Cpp,
            "templates",
            r#"
template <typename T>
void make() {}

template <typename T, typename U>
void make() {}

template <>
void f<int>(int x) {}

template <>
void f<double>(double x) {}

template <class T>
class A {
public:
    void run();
};

template <class T>
void A<T>::run() {}
"#,
        ),
        // Declarator shapes that the field-only descent used to drop entirely:
        // reference returns, parenthesized declarators, operators, and the
        // conversion operator (`operator_cast`).
        (
            "cpp",
            Lang::Cpp,
            "declarators",
            r#"
class Buffer {
public:
    char& at(int i) { return d[i]; }
    const char& at(int i) const { return d[i]; }
    Buffer& operator=(const Buffer& o) { return *this; }
    char& operator[](int i) { return d[i]; }
    const char& operator[](int i) const { return d[i]; }
    operator bool() const { return true; }
    const char* name() const { return "b"; }
    void n(int i) noexcept {}
    void n(int i) {}
    char* p;
    char d[8];
};

int (parenthesized)(int x) { return x; }

char& ref_returning(char* p) { return *p; }
"#,
        ),
        // ================================================================= go
        (
            "go",
            Lang::Go,
            "base",
            r#"
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
"#,
        ),
        // Value receiver vs pointer receiver, same method name; free function
        // sharing the name of a method.
        (
            "go",
            Lang::Go,
            "receivers",
            r#"
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

func String() string {
    return "free"
}

func free() string {
    return ""
}
"#,
        ),
        // A type declared INSIDE a function: `type_spec` under a function scope.
        (
            "go",
            Lang::Go,
            "local_type",
            r#"
package main

func outer() {
    type Local struct{}
    _ = Local{}
}

type Top struct{}
"#,
        ),
        // =============================================================== java
        (
            "java",
            Lang::Java,
            "base",
            r#"
package app;
import app.services.Mailer;

interface Pet {}

class Animal {}

class Dog extends Animal implements Pet {
    public String speak() {
        return bark();
    }
}
"#,
        ),
        (
            "java",
            Lang::Java,
            "nested",
            r#"
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
"#,
        ),
        // POSITIVE: top-level and nested `enum`, two enums with one method name.
        (
            "java",
            Lang::Java,
            "enums",
            r#"
enum Status {
    DRAFT, LIVE;

    public String label() {
        return name();
    }
}

enum Tier {
    FREE;

    public String label() {
        return "tier";
    }
}

class Holder {
    enum Inner {
        A;
        public String label() { return "i"; }
    }

    public String label() { return "h"; }
}
"#,
        ),
        // POSITIVE: `record`, top-level and nested, with a compact body.
        (
            "java",
            Lang::Java,
            "records",
            r#"
record Point(int x, int y) {
    int sum() {
        return x + y;
    }
}

record Span(int lo, int hi) {
    int sum() {
        return hi - lo;
    }
}

class Box {
    record Cell(int v) {
        int sum() { return v; }
    }

    int sum() { return 0; }
}
"#,
        ),
        (
            "java",
            Lang::Java,
            "overloads",
            r#"
class C {
    void m(int a) {}
    void m(String a) {}
    void m() {}
}
"#,
        ),
        // =============================================================== ruby
        (
            "ruby",
            Lang::Ruby,
            "base",
            r#"
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
"#,
        ),
        (
            "ruby",
            Lang::Ruby,
            "nested",
            r#"
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
"#,
        ),
        // `def self.m` / `class << self` / `class << obj` / instance `def m`,
        // all four spelling the SAME FQN and split only by the discriminator.
        (
            "ruby",
            Lang::Ruby,
            "singletons",
            r#"
class C
  def m
    inst
  end

  def self.m
    klass
  end

  class << self
    def m
      meta
    end

    def other
      x
    end
  end
end

obj = Object.new

class << obj
  def m
    singleton
  end
end
"#,
        ),
        // ============================================================= csharp
        (
            "csharp",
            Lang::CSharp,
            "base",
            r#"
using App.Services;

interface IPet {}

class Animal {}

class Dog : Animal, IPet {
    public string Speak() {
        return Bark();
    }
}
"#,
        ),
        // POSITIVE: block `namespace A { }`, two of them in one file.
        (
            "csharp",
            Lang::CSharp,
            "block_ns",
            r#"
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

namespace App.Data {
    class Outer {
        public void Run() {
            Helper();
        }
    }
}
"#,
        ),
        // NEGATIVE: file-scoped `namespace A;` must NOT become a segment.
        (
            "csharp",
            Lang::CSharp,
            "file_scoped_ns",
            r#"
namespace App.Data;

class Options {
    public void Validate() {
        Go();
    }
}

struct Pair {
    public void Swap() {}
}
"#,
        ),
        // Generic arity and explicit interface implementations.
        (
            "csharp",
            Lang::CSharp,
            "generics",
            r#"
interface IA { void M(int a); }
interface IB { void M(int a); }

class C : IA, IB {
    public void M<T>() {}
    public void M<T, U>() {}
    public void M(int a) {}
    void IA.M(int a) {}
    void IB.M(int a) {}
}
"#,
        ),
        // =============================================================== bash
        (
            "bash",
            Lang::Bash,
            "base",
            r#"#!/bin/bash
source ./helpers.sh

greet() {
    hello_world
}
"#,
        ),
        (
            "bash",
            Lang::Bash,
            "nested",
            r#"#!/bin/bash
outer() {
    inner() {
        deep_call
    }
    inner
}

function other {
    outer
}
"#,
        ),
    ]
}

fn main() {
    let mut lines: Vec<String> = Vec::new();
    for (grammar, lang, fixture, src) in corpus() {
        let facts = extract::extract(lang, src).unwrap_or_else(|e| {
            panic!("extract failed for {grammar}/{fixture}: {e}");
        });
        for s in &facts.symbols {
            lines.push(format!(
                "{}\t{}\t{:06}-{:06}\tparents={}\tname={}\tkind={}",
                grammar,
                fixture,
                s.byte_range.0,
                s.byte_range.1,
                s.parents.join("."),
                s.name,
                s.kind
            ));
        }
    }
    lines.sort();
    for l in lines {
        println!("{l}");
    }
}
