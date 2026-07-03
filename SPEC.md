# SPEC — store version guard (v0.6.1, issue #9)

`limpet update` swaps the binary on disk; a running `serve` keeps the old
code image. Two images on one store produced a spurious invalidation
(old-image hashless writes + mixed-version resolves). Fix: stamp and refuse.

## Core Architecture

```
Store::version_guard()          called first in tools::dispatch (gates every
                                MCP call incl. the sweep+resolve writes) and
                                before CLI `limpet index`
  no stamp / older stamp   -> kv_set code_version = CARGO_PKG_VERSION, proceed
  stamp == running         -> proceed
  stamp NEWER than running -> bail loudly, naming both versions + the fix
ver_tuple                       malformed stamp parses low, never outranks real
```

Deviation from issue #9 as filed: reads are NOT exempted. Every tool call
sweeps and resolves (writes), and a half-current image serving "reads" with
stale semantics is the same silent-corruption class. All-or-nothing error is
simpler and honest. Limitation: pre-guard images (<=0.6.0) cannot refuse;
protection covers all future image mixes.

## INVARIANTS

- I-V1: a process never writes to a store stamped by a newer version.
- I-V2: the refusal names both versions and the remediation.
- I-V3: malformed stamps never brick a store (parse low, get restamped).

## Task Implementation Checklist

- [x] store.rs: version_guard + ver_tuple + unit test (stamp, upgrade,
      refuse-newer, malformed)
- [x] tools.rs dispatch + main.rs index: guard call
- [x] update.rs: post-update message mentions old-image write refusal
- [x] version 0.6.1 (Cargo.toml, server.json, Cargo.lock)
- [x] live cross-version verify: 0.6.1 CLI and MCP both refuse a
      99.0.0-stamped store; unit suite 60 green
- [ ] PR -> CI -> merge -> tag v0.6.1 -> pipeline

---

# SPEC — C++ grammar + legacy-encoding fallback (v0.6.0, shipped)

Driven by a real target: a legacy MMO C++ engine (CP949-encoded source,
UTF-16 headers mixed in) whose knowledge currently anchors only at file
level. Goal: symbol anchors for C/C++ where the source is UTF-8, and a
guaranteed non-regression for files a grammar matches but cannot decode.

## Core Architecture

```
lang::detect            + Cpp: cpp cc cxx hpp hh hxx h c inl -> tree-sitter-cpp
index_file              read bytes ONCE
  no grammar            -> file-level row (raw-byte hash)          [unchanged]
  grammar + valid UTF-8 -> parse, symbols, imports, calls          [unchanged]
  grammar + NOT UTF-8   -> file-level row (raw-byte hash), 0 syms  [NEW]
                           (CP949/UTF-16 legacy source keeps its
                            anchorability instead of vanishing)

extract walk, Lang::Cpp:
  function_definition   -> function/method (method when inside class/struct)
                           name via declarator descent: function_declarator /
                           pointer / reference / parenthesized -> identifier |
                           field_identifier | destructor_name | operator_name |
                           qualified_identifier (rightmost name; out-of-line
                           GLGaeaClient::GetSkinChar anchors as GetSkinChar)
  class_specifier | struct_specifier | enum_specifier (named) -> class
  namespace_definition  -> parent scope push only (like Rust impl_item)
  preproc_include       -> import (path, <> and "" trimmed)
  call_expression       -> call edge; callee_name grows qualified_identifier

anchor is_identity_leaf += number_literal, char_literal, raw_string_literal
  (C++ numerics are `number_literal`; without this a body edit `a+1 -> a+2`
   would NOT change the hash and staleness would silently lie)
```

## INVARIANTS

- I7 (existing): no grammar ships without fixture coverage in
  tests/index_langs.rs. Cpp gets: function, class+method, include, call.
- I-N1: adding a grammar must never make any file LESS anchorable than
  v0.5.x file-level indexing. Decode failure degrades to the file-level
  row; it never drops the file. Regression test with real CP949 bytes.
- Golden hash properties (cosmetic-invariant, edit-sensitive) must hold
  for Cpp in tests/anchor_golden.rs hash_properties_hold_per_language.

## ATTACK SURFACE

- `.h` claimed by Cpp: plain C headers parse fine under tree-sitter-cpp;
  Objective-C headers will parse poorly -> parse_ok=0 path already isolates
  failures per file, no spread.
- Templates: template_declaration wraps function_definition; recursive walk
  finds the inner node, no special casing.
- Macro-heavy regions: tree-sitter-cpp error-recovers; worst case fewer
  symbols, file row always present.
- UTF-16 files contain interior NULs, from_utf8 fails -> fallback path, not
  a crash.

## TECH STACK DEPS

- + tree-sitter-cpp = "0.23" (matches the 0.23 grammar family, core 0.24).
  No other new deps.

## Task Implementation Checklist

- [ ] Cargo.toml: tree-sitter-cpp; version -> 0.6.0 (+ server.json, lock)
- [ ] lang.rs: Lang::Cpp, detect map, ts_language arm
- [ ] extract.rs: Cpp walk arm + cpp_declarator_name helper +
      callee_name qualified_identifier arm
- [ ] anchor.rs: is_identity_leaf += number_literal | char_literal |
      raw_string_literal
- [ ] index/mod.rs: byte-read refactor with non-UTF-8 -> file-level fallback
- [ ] tests: index_langs cpp fixture (incl. out-of-line method name);
      anchor_golden Cpp hash-properties case; CP949-bytes fallback test
- [ ] index/mod.rs: split MAX_PARSE_BYTES (512KB, symbol parse cap, degrades
      to file-level) from MAX_FILE_BYTES (8MB, walk skip): giant legacy
      translation units stay anchorable (I-N1 applied to size, not just
      encoding)
- [ ] README: grammar list + legacy-encoding note + size-bound wording
- [ ] cargo test --locked green; bench gate holds; dogfood on a CP949 fixture
