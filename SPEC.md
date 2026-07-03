# SPEC — audit hardening (v0.7.2)

Three parallel audits (staleness engine, store/API, periphery) produced 32
findings; ~20 confirmed and fixed here, 4 deferred to the roadmap with
reasons, the rest discarded on verification. Governing outcomes:

## Semantics changes (deliberate)

- **Resurrection.** resolve_all re-resolves invalidated entries; restored
  code (branch switch, stash, rebase, sweep lag) heals them. Only
  `superseded` is final. README updated; golden tests cover restore-heals
  and superseded-stays.
- **File anchors follow moves** by content hash (one home = followed,
  many = ambiguous_anchor), symmetric with symbol anchors.
- **Stale confidence penalty applies once**, on the active->stale
  transition; it previously compounded on every tool call.
- **Relevance floor exempts flagged items** (I3 now actually holds) and is
  skipped when the top score is non-positive.
- **C++ renames follow**: the own-name node is resolved through the
  declarator chain and excluded from the body hash.

## Trust and honesty

- remember: `verified` requires evidence; ambiguous bare-name anchors are
  refused with the candidate FQN list; anchor files are reindexed before
  hashing so anchors are never born stale.
- Envelope: failed sweeps report `sweep_failed` instead of dirty:0;
  map/affected disclose limit-clipping; recall names the true omission
  cause (budget vs relevance_floor).
- (fqn, hash) existence check kills nondeterministic anchor flapping on
  duplicate FQNs; full FQN disambiguation deferred to the roadmap.
- Import merges links on skipped entries and counts dropped links.

## Concurrency and robustness

- index_file is transactional (no half-indexed files); version_guard and
  ledger_add are IMMEDIATE-transaction atomic; the ledger session base is
  in-memory per process.
- MCP stdin loop survives invalid UTF-8 and caps line size at 8MB.
- UI: read timeout + bounded request/header reads.
- validate_rel_path rejects empty paths and symlink escapes.
- Updater stages next to the target binary with create_new (no predictable
  world-writable temp path).
- Secret detector splits on =/:/{}/[] (env/YAML/JSON forms) and gates Slack
  tokens on variant letter + digit presence.

## New: limpet doctor

`limpet doctor` checks binary/registration/skill/store/version-stamp/index
and prints ok/FAIL lines; runs automatically after `limpet install` and
`limpet update`.

## Deferred to ROADMAP (with reasons)

- Repo-key collision fix -> 0.9 repo-identity rework (rekeying orphans
  every existing store; needs the migration path anyway).
- Full FQN disambiguation (trait impls, overloads, nested modules) ->
  grammar-wave milestone; schema-touching.
- Low-entropy follow guard (trivial duplicate bodies) -> needs a body-size
  column.
- ledger.q row growth: accepted debt, tiny rows, cleared by ledger_reset.

---

# SPEC — session savings ledger (v0.7.0)

## Positioning

limpet already saves tokens; it saves them **invisibly**. The user never sees
the file reads a recall replaced. This ledger turns the benchmark's 4.1x into
the user's own live receipt, using the SAME honest methodology the README
publishes (ceil(bytes/4), minimal file set, assumptions labeled). It measures;
it never inflates. A recall that costs MORE than the source it replaced shows
as a negative-saving, not a hidden zero.

## Core Architecture

```
per recall (tool_recall, after the pack is built):
  served   = sum over returned items of token_estimate(body) + ITEM_OVERHEAD
             (identical to what the client actually pays — same units as budget)
  baseline = SEARCH_OVERHEAD                                  # flat search round-trip
           + sum over DISTINCT files anchored by returned items of
               ceil(files.size / 4)                           # minimal-file-set read
  saved    = baseline - served                                # may be negative; not floored
  reads_avoided = count(distinct anchored files of ACTIVE returned items)

  accumulate into meta_kv (lifetime, string-encoded ints):
    ledger.recalls, ledger.distinct_queries (by fts_query hash seen this process),
    ledger.served, ledger.baseline, ledger.reads_avoided, ledger.since (set once)

session view = lifetime snapshot at serve boot (meta_kv ledger_session_*)
               subtracted from current lifetime. CLI one-shots show lifetime only.

surfacing (humans only, never the recall wire):
  admin {op:"ledger"}  -> full session + lifetime + method string
  admin {op:"ledger_reset"} -> zero lifetime, restamp since
  limpet stats         -> CLI one-shot of the same payload
  ui /api/ledger + header stat beside the graph
```

Amendment (found in dogfooding): the spec originally put a 2-int
meta.ledger block on every recall. The bench regression gate caught it,
4.1x -> 3.9x, under the 4x floor: even a tiny receipt on the hot path makes
the product worse at the thing the receipt measures. Removed. The agent
never pays for the ledger; humans read it where reading is free.

Amendment 2: the session base lives in meta_kv, so "session" = since the
last server boot on this store (last-boot-wins), not per-process. Simpler,
and correct for the one-server-per-project norm.

## State / Data Model

No schema change, no SCHEMA_VERSION bump: all counters live in `meta_kv`
(k TEXT PK, v TEXT), numeric values encoded as decimal strings, missing key
reads as 0. `since` is an ISO stamp set on first recall. The serve loop stamps
`ledger_session_*` = current lifetime once at boot; the difference is "this
session". No new table, no migration, no dispatch signature change beyond
`tool_recall` doing its own meta_kv accumulation.

Constants (one home, next to token_estimate):
- ITEM_OVERHEAD_TOKENS = 30 (already exists in recall.rs; reuse, do not fork)
- SEARCH_OVERHEAD_TOKENS = 300 (matches bench/token_savings.py exactly)

## INVARIANTS

- I-L1: `served` is computed the same way the budget packer counts, so the
  ledger's "served" can never disagree with what the client was charged.
- I-L2: `saved` is never floored or clamped. A verbose memory that costs more
  than its source file shows a real negative; hiding it would be the same
  dishonesty the anchors refuse (nothing stale is hidden; no anti-saving is
  hidden either).
- I-L3: token_estimate is the ONLY sizing function, applied identically to
  served and baseline (byte-for-byte parity with the published bench).
- I-L4: anchorless memories (pure decisions/episodes not in any file)
  contribute 0 baseline bytes. This UNDERSTATES savings on exactly the
  questions where limpet helps most, the same conservative bias the README
  already documents. Never estimate a file that does not exist.
- I-L5: the ledger is observational only. It never changes recall ranking,
  packing, or any status. A bug in the ledger can misreport a number; it can
  never corrupt memory.
- I-L6: every ledger payload carries a `method` string stating its
  assumptions, so the number is checkable rather than believed.

## ATTACK SURFACE

- Double counting: re-issuing the identical query re-charges `saved`. Real
  (the agent really would have paid each recall) but re-reads are not new
  knowledge. Mitigation: track `distinct_queries` by fts_query hash and report
  BOTH gross and distinct; hide neither.
- Gaming the ratio by seeding huge files: baseline scales with real file
  sizes, so a bogus giant file inflates "saved". Accept it: the bench has the
  same property, and the file has to actually exist and be indexed. The number
  is a floor-honest estimate, marketed as such, not a guarantee.
- Ledger write on the hot recall path: two meta_kv upserts per recall, both
  tiny, inside the existing transaction scope. Negligible next to FTS + pack.
- Concurrency: two serve processes on one store both accumulate into lifetime.
  Additive and correct for lifetime; session views are per-process snapshots so
  they stay independent. No lost-update guard needed (upserts are last-writer,
  and the counters are monotonic increments computed from read-modify-write
  under the store's single connection).
- Version guard interaction: ledger writes are writes, so a stale-image server
  is already refused before it can mis-accumulate (I-V1 covers this for free).

## TECH STACK DEPS

- None new. Reuses token_estimate, meta_kv, the envelope, the UI route table.

## Task Implementation Checklist

- [x] recall.rs: SEARCH_OVERHEAD_TOKENS + pure `recall_cost` with unit tests
      (served parity, distinct-file dedup, stale-avoids-nothing, negative
      survives, anchorless-zero, empty-pack-zero)
- [x] store.rs: Ledger struct, ledger_add/read/reset/since + session base
      helpers over meta_kv; unit test incl. distinct-query dedup and
      negative saving
- [x] tools.rs tool_recall: compute + accumulate (observational; failure
      never fails the recall). Wire block REMOVED per amendment: bench gate
      caught the cost
- [x] tools.rs tool_admin: `ledger` / `ledger_reset` ops; ledger_payload
      with method string; admin schema enum updated
- [x] mcp.rs serve boot: session base stamp
- [x] main.rs: `limpet stats` (+ HELP), version-guarded
- [x] ui.rs /api/ledger + header stat with methodology tooltip
- [x] README: "your own receipt" note with the conservative-floor caveats
- [x] version 0.7.0; 65 tests green with --locked; bench gate holds at 4.1x
      AFTER the amendment (and correctly failed at 3.9x before it); dogfood:
      real recall accumulated saved=456, admin ledger / limpet stats / API
      all agree
- [ ] PR -> CI -> merge -> tag v0.7.0 -> pipeline

---

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
