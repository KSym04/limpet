// I-F3 differential corpus. Included by `if3_dump.rs` (both trees) and by
// `if3_disamb.rs` (branch only) so the two dumps can never drift apart.
//
// Every fixture exists to exercise a specific `parents.push` arm or
// discriminator-assignment arm of `extract::walk`, including the arms that must
// NOT fire (unbracketed PHP namespace, C# file-scoped namespace, declaration
// only Rust `mod x;`, string-named ambient TS modules, Ruby `class << self`).

use limpet::index::extract;
use limpet::index::lang::Lang;

/// (grammar label, language, fixture label, source).
fn corpus() -> Vec<(&'static str, Lang, &'static str, &'static str)> {
    vec![
        // ================================================================ php
        (
            "php",
            Lang::Php,
            "flat",
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
    public static function make() {}
}

interface Runner {
    public function run();
}

trait Loggable {
    public function log($m) {
        write_log($m);
    }
}
"#,
        ),
        (
            "php",
            Lang::Php,
            "ns_unbracketed",
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
        (
            "php",
            Lang::Php,
            "ns_bracketed",
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
        (
            "php",
            Lang::Php,
            "enums",
            r#"<?php
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

class Holder {
    public function label() {}
}
"#,
        ),
        (
            "php",
            Lang::Php,
            "enum_in_ns",
            r#"<?php
namespace Cards {
    enum Suit: string {
        case Hearts = 'H';
        public function label(): string { return 'h'; }
    }
}
"#,
        ),
        (
            "php",
            Lang::Php,
            "nested_fn",
            r#"<?php
function outer() {
    function inner() {
        deep();
    }
    inner();
}
"#,
        ),
        // ================================================================= js
        (
            "js",
            Lang::Js,
            "flat",
            r#"
import { helper } from './helper.js';

function topLevel(x) {
    helper(x);
    return x + 1;
}

function* gen() {
    yield 1;
}

class ScanQueue {
    constructor(cap) {
        this.cap = cap;
    }
    push(item) {
        this.validate(item);
    }
    static make(cap) {
        return new ScanQueue(cap);
    }
}
"#,
        ),
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
    static get kind() {
        return 'box';
    }
    static set kind(k) {
        this._k = k;
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
"#,
        ),
        // ================================================================= ts
        (
            "ts",
            Lang::Ts,
            "flat",
            r#"
import { helper } from './helper';

function topLevel(x: number): number {
    helper(x);
    return x + 1;
}

abstract class Base {
    abstract go(): void;
}

class ScanQueue extends Base {
    go(): void {}
    push(item: string): void {
        this.validate(item);
    }
}
"#,
        ),
        (
            "ts",
            Lang::Ts,
            "accessors",
            r#"
class Box {
    get value(): number {
        return this._v;
    }
    set value(v: number) {
        this._v = v;
    }
}
"#,
        ),
        (
            "ts",
            Lang::Ts,
            "namespaces",
            r#"
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
"#,
        ),
        (
            "ts",
            Lang::Ts,
            "ambient",
            r#"
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
"#,
        ),
        (
            "ts",
            Lang::Ts,
            "nested",
            r#"
function wrapper(): void {
    function nested(): void {}
}

function* gen(): Iterator<number> {
    yield 1;
}
"#,
        ),
        // ================================================================= py
        (
            "py",
            Lang::Py,
            "flat",
            r#"
import os
from app.services import mailer

def top_level(x):
    helper_call(x)
    return x + 1

class ScanQueue:
    def push(self, item):
        self.validate(item)

    @staticmethod
    def make(cap):
        return ScanQueue()

    @classmethod
    def build(cls, cap):
        return cls()
"#,
        ),
        (
            "py",
            Lang::Py,
            "property",
            r#"
class Box:
    @property
    def value(self):
        return self._v

    @value.setter
    def value(self, v):
        self._v = v

    @value.deleter
    def value(self):
        del self._v
"#,
        ),
        (
            "py",
            Lang::Py,
            "overload",
            r#"
from typing import overload

class Reader:
    @overload
    def read(self, n: int) -> bytes: ...

    @overload
    def read(self) -> bytes: ...

    def read(self, n=None):
        return b""
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

async def coro(x):
    return x
"#,
        ),
        // =============================================================== rust
        (
            "rust",
            Lang::Rust,
            "flat",
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

const K: u32 = 1;
static S: u32 = 2;
"#,
        ),
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
    pub const AK: u32 = 1;
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
        (
            "rust",
            Lang::Rust,
            "impls",
            r#"
struct T;

trait Speak {
    fn go(&self);
}

trait Walk {
    fn go(&self);
}

impl T {
    fn go(&self) {
        fn helper() {}
        helper();
    }
}

impl Speak for T {
    fn go(&self) {}
}

impl Walk for T {
    fn go(&self) {}
}

enum E {
    A,
}

impl E {
    fn e(&self) {}
    const EK: u32 = 3;
}

struct Wrap<T>(T);

impl<T> Speak for Wrap<T> {
    fn go(&self) {}
}
"#,
        ),
        (
            "rust",
            Lang::Rust,
            "nested_fn",
            r#"
fn outer() {
    fn inner() {
        fn deeper() {}
        deeper();
    }
    inner();
}
"#,
        ),
        // ================================================================ cpp
        (
            "cpp",
            Lang::Cpp,
            "flat",
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

struct S {
    void m() {}
};

enum Color { RED, GREEN };
"#,
        ),
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
};
void ns_free(int x) {
    helper(x);
}
}

void ns::A::inline_m(int x) {
    helper(x);
}

ns::A::~A() {}

void GLGaeaClient::GetSkinChar(int id) {
    Lookup::Find(id);
}

int plain(int x) {
    return x;
}
"#,
        ),
        (
            "cpp",
            Lang::Cpp,
            "nested_types",
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

namespace deep {
namespace inner {
void nested_free() {}
}
}
"#,
        ),
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
void pick<int>(int x) {}

template <class T>
class Box {
public:
    void run();
    T get() const;
};

template <class T>
void Box<T>::run() {}

template <class T>
T Box<T>::get() const {
    return T();
}
"#,
        ),
        (
            "cpp",
            Lang::Cpp,
            "qualified_template",
            r#"
namespace N {
template <typename T>
void f(T x) {}
}

template <>
void N::f<int>(int x) {
    sink(x);
}

void ::global_fn(int x) {
    sink(x);
}

namespace {
void anon_fn() {
    sink(0);
}
}
"#,
        ),
        (
            "cpp",
            Lang::Cpp,
            "operators",
            r#"
class Buffer {
public:
    operator bool() const;
    Buffer& operator=(const Buffer& o);
    char& at(int i);
    const char& at(int i) const;
    void go() &;
    void go() &&;
    void quiet() noexcept;
    void loud();
};

Buffer::operator bool() const {
    return true;
}

Buffer& Buffer::operator=(const Buffer& o) {
    return *this;
}

char& Buffer::at(int i) {
    return data[i];
}

const char& Buffer::at(int i) const {
    return data[i];
}

int (paren_fn)(int x) {
    return x;
}
"#,
        ),
        // ================================================================= go
        (
            "go",
            Lang::Go,
            "flat",
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

func (d *Dog) Rename(n string) {
    d.name = n
}

func bark() string {
    fmt.Println("woof")
    return "woof"
}
"#,
        ),
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
    return ""
}
"#,
        ),
        // =============================================================== ruby
        (
            "ruby",
            Lang::Ruby,
            "flat",
            r##"
require 'mailer'

module Walkable
  def walk
  end
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
"##,
        ),
        (
            "ruby",
            Lang::Ruby,
            "singleton",
            r##"
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

obj = Object.new
class << obj
  def only_here
  end
end
"##,
        ),
        (
            "ruby",
            Lang::Ruby,
            "nested",
            r##"
module Outer
  class Inner
    def deep
      go
    end
  end

  def mod_method
  end
end

def free_fn
  def nested_fn
  end
end
"##,
        ),
        // =============================================================== java
        (
            "java",
            Lang::Java,
            "flat",
            r#"
package app;
import app.services.Mailer;

interface Pet {
    void speak();
}

class Animal {}

class Dog extends Animal implements Pet {
    public String speak() {
        return bark();
    }
    public String speak(int times) {
        return bark();
    }
}
"#,
        ),
        (
            "java",
            Lang::Java,
            "enum_record",
            r#"
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
}
"#,
        ),
        // ============================================================= csharp
        (
            "csharp",
            Lang::CSharp,
            "flat",
            r#"
using App.Services;

interface IPet {
    void Speak();
}

interface IA {
    void M(int a);
}

interface IB {
    void M(int a);
}

class Animal {}

class Dog : Animal, IPet, IA, IB {
    public string Speak() {
        return Bark();
    }
    public string Speak(int times) {
        return Bark();
    }
    void IA.M(int a) {}
    void IB.M(int a) {}
    public void G<T>() {}
    public void G<T, U>() {}
}

struct Point {
    public void M() {}
}
"#,
        ),
        (
            "csharp",
            Lang::CSharp,
            "ns_block",
            r#"
namespace App.Core {
    class Options {
        public void Validate() {
            Go();
        }
    }

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
}

namespace App.Edge {
    class Options {
        public void Validate() {
            Go();
        }
    }
}
"#,
        ),
        (
            "csharp",
            Lang::CSharp,
            "records",
            r#"
record Point(int X, int Y) {
    public int Sum() {
        return X + Y;
    }
}

record Pair(int A, int B) {
    public int Sum() {
        return A + B;
    }
}
"#,
        ),
        (
            "csharp",
            Lang::CSharp,
            "ns_nested_blocks",
            r#"
namespace Outer {
    namespace Inner {
        class C {
            public void M() {
                Go();
            }
        }
    }
}
"#,
        ),
        (
            "csharp",
            Lang::CSharp,
            "ns_filescoped",
            r#"
namespace App.Core;

class Options {
    public void Validate() {
        Go();
    }
}
"#,
        ),
        // =============================================================== bash
        (
            "bash",
            Lang::Bash,
            "flat",
            r#"#!/bin/bash
source ./helpers.sh

greet() {
    hello_world
}

function other {
    greet
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
"#,
        ),
    ]
}
