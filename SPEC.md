# SPEC — freshness at scale, branch 1: sweep priority + low-entropy guard — v0.13.0

Status: APPROVED (2026-07-11). Full spec:
docs/superpowers/specs/2026-07-11-freshness-at-scale-design.md

Two freshness-correctness fixes that feed the honesty envelope. Sweep order
stops being arbitrary: files carrying anchors reindex first inside the same
32-file budget, so staleness lands where memories live. Follow stops trusting
uniqueness alone: a trivial body (empty fn, delegating one-liner) is refused as
follow evidence and surfaces as `Stale{low_entropy}` instead of silently
re-pointing to the wrong twin.

| Item | Target | Summary |
|---|---|---|
| Sweep prioritization | v0.13.0 | stable-partition `changed` anchored-first before the budget cut; report shape unchanged |
| schema v5 `symbols.body_len` | v0.13.0 | normalization-buffer byte length beside the hash; additive ALTER, table_info self-gate, mtime_ns=0 refill |
| Low-entropy follow guard | v0.13.0 | both follow sites: unique match under measured threshold -> `Stale{low_entropy}`; NULL = legacy grace; heals when original returns |

Locked: hash recipe byte-identical (length is a read of the same buffer);
thresholds calibrated from real buffer lengths across all 11 grammars before
the constants are set, biased low (never misclassify a real body); stale not
invalidated; two-process release-binary dogfood mandatory for the migration.

## Task Implementation Checklist — freshness branch 1

- [ ] Calibration harness: print normalization-buffer lengths for trivial +
      real fixture bodies across all 11 grammars; pick both thresholds
- [ ] `ast_body_hashes`/`ast_body_hash_node` return (hash, len); all callers
- [ ] schema v5: ALTER + table_info self-gate + refill + reopen/refill tests
- [ ] `index_file_parsed` writes `body_len`
- [ ] Sweep prioritization + budget-boundary test
- [ ] Symbol-site guard + NULL grace + healing round-trip tests
- [ ] File-site guard (`files.size`) + tests
- [ ] Docs: `low_entropy` stale reason; README if reasons enumerated
- [ ] Full suite + clippy + QA gate + two-process dogfood
- [ ] Whole-branch review -> STOP for Ken -> PR

---

# SPEC — grammar wave 2 (Go, Java, Ruby, C#, Bash) — v0.12.0

Status: APPROVED (design, 2026-07-08). Full spec:
docs/superpowers/specs/2026-07-08-grammar-wave-2-design.md

Extend structural coverage 6 -> 11 grammars, purely additive: each new grammar
is an isolated extractor arm + fixtures, gated by the same I7 fixture and
hash-identity checks the first six passed. The lineage graph gains two honest
inheritance rels so Go embedding (`embeds`) and Ruby mixins (`mixin`) are labeled
for what they are, not fuzzed into `extends`.

| Item | Target | Summary |
|---|---|---|
| Go / Java / Ruby / C# / Bash | v0.12.0 | one extractor arm + I7 fixture + hash/name gates per grammar |
| inherits.rel widening | v0.12.0 | schema v4 adds `embeds`, `mixin`; migration drops+recreates the derived table |
| FQN disambiguation + low-entropy follow guard | v0.13.0 | DEFERRED riders (touch anchor/dedup + schema uniqueness) |

Locked: 5 grammars only (riders deferred); honest new rels via schema v4;
no extract.rs split; Go/Bash grammar crates pinned ABI-compatible with
tree-sitter 0.24.

---

# SPEC — lineage graph + live ledger + local event hook (design, next minors)

Status: APPROVED (M0 closed 2026-07-07). Full spec:
docs/superpowers/specs/2026-07-07-limpet-lineage-ledger-hook-design.md

Three free-core features. v0.9.0 stays portability; the lineage graph lands
v0.10.0 (the live ledger rides along); the event hook is a gated v1.1+ bet. Each
ships only if it feeds the honest receipt or the honesty envelope.

| # | Feature | Target | Summary |
|---|---|---|---|
| M1 | AST lineage graph | v0.11.0 | inheritance + resolved call edges -> bounded up/down lineage in `map` |
| M2 | Live token ledger | DEFERRED | built + bench-gated in 0.11.0; meta.ledger dropped the bench to 3.8x (under 4x), so reverted; stays in admin/stats/UI |
| M3 | Local event hook | v1.1+ bet | opt-in exec hook on memory transitions; local `check` gate |

## Core Architecture

| Layer | Responsibility |
|---|---|
| index (extract.rs) | new inheritance extraction, all 6 grammars (extends / implements / impl-trait); bare-name parents, resolved read-time |
| store | additive `inherits` table (child_fqn, parent_name, rel, file), schema bump 2->3, per-file reindex lifecycle mirrors `calls` |
| index/graph.rs | read-time name resolver + bounded BFS lineage (depth + node caps, visited-set); ancestors / descendants / callers, each edge labeled unique/ambiguous/unresolved |
| map tool | additive `lineage` field for symbol targets only (file targets unchanged); existing `symbols`/`calls`/`memories` unchanged |
| ledger | existing meta_kv ledger surfaced per-call in the envelope + per-session delta; sink unchanged, estimate-labeled |
| event hook | opt-in local exec hook (`.limpet/hooks.toml`), event JSON on stdin, no network; fires post-commit, cannot corrupt memory |

Interplay: M1's resolved call edges strengthen the `fan_in` signal the
cost_to_learn spec (below) reads from the `calls` table.

## INVARIANTS

- I-G1: call/inherit edges store bare names; endpoints resolve against the
  current symbol table at query time. No stored resolution to rot.
- I-G2: lineage traversal is bounded (depth cap + node cap + visited-set);
  truncation disclosed via `meta.completeness`. No silent clip.
- I-G3: every edge endpoint labeled unique / ambiguous / unresolved; ambiguity
  never collapsed to a guess.
- I-Z1: zero baked-in network anywhere in the core; `serve` stays stdio-only;
  the event hook shells out to a local command, opens no connection itself.
- I-L2 (carried): negative savings shown, never floored.
- I-L5 (carried): ledger/hook bugs cannot corrupt memory; fired outside the
  content transaction, after commit.
- Ledger sink stays `meta_kv`; never `.limpet/memory.jsonl`, never the network.

## ATTACK SURFACE

- Cyclic / diamond inheritance -> visited-set + depth cap.
- Deep or wide call fan-out -> node cap + disclosed truncation.
- Ambiguous name resolution -> all candidates labeled, none guessed.
- Malformed supertype syntax -> extractor skips the edge, no panic.
- Large legacy repos -> reuses the existing 512KB/8MB degradation ladder;
  traversal is read-only over indexed rows, O(log n) per hop.

## Task Implementation Checklist

M1 — lineage graph (v0.11.0):
- [ ] store: `inherits` table + indexes + schema bump 2->3 + migration test
- [ ] index: per-file `inherits` delete/reinsert at the 3 reindex sites
- [ ] extract.rs: inheritance capture for php/js/ts/py/rs/cpp (+ unit tests each)
- [ ] index/graph.rs: read-time resolver (0/1/N labeled) + bounded BFS lineage
- [ ] tools: `map` returns additive `lineage`; tool schema + README + docs_in_sync
- [ ] bench: fixture inheritance chain + lineage questions; ratio >= 4x sub-gate
- [ ] tests green; dogfood; NO RELEASE until Ken tests

M2 — live ledger (v0.11.0):
- [ ] serve: per-session `SessionLedger`, reset at serve start
- [ ] recall envelope: additive `meta.ledger` {served,baseline,saved,reads_avoided,cumulative_saved,estimate}
- [ ] assert sink is meta_kv; memory.jsonl untouched; negative not floored
- [ ] tests green; dogfood

M3 — local event hook (v0.10.0, gate ruling at kickoff):
- [ ] event emitter (memory.remembered/stale/contradicted, index.completed), post-commit
- [ ] opt-in `.limpet/hooks.toml` exec hook; event JSON on stdin; bounded timeout
- [ ] local `limpet check` exit codes; no network opened (assert stdio-only)
- [ ] tests green; dogfood

---

# SPEC - /limpet scan: seed memory from history + private flag (approved, next minor)

Status: APPROVED DESIGN. Full spec: docs/superpowers/specs/2026-07-04-limpet-scan-design.md

## Core Architecture

Scan orchestration = skill layer only (src/skill.md). Binary gains ONLY
private-memory support. No new CLI subcommand, no network.

| Layer | Responsibility |
|---|---|
| skill (src/skill.md) | quality pre-check -> harvest in SUBAGENT (raw git/docs never hit main context) -> curate to kinds+anchors -> two-tier review gate -> `remember` -> honest report |
| remember tool | new optional `private` bool (default false) + `origin` string (scan:git:<sha> etc.); duplicate origin rejected naming existing id |
| store | additive `private` + `origin` columns (origin indexed), schema bump, version_guard as-is |
| admin export | withholds private items; reports "N private withheld" |
| ui / status | private badge; private count |

Depth modes: `light` default (merges+tags+README), `deep` full source set.
Volume cap 25/scan, value-ranked; input caps 100 merges / 200 subjects /
bounded doc reads. Idempotency ENFORCED by origin dedup in binary;
recall-check trims proposals first. Review gate: high-confidence tier =
one block, reject-by-exception; borderline = item-level; private = ALWAYS
item-level. Thin history: pre-check flags, scope shrinks, report says so
plainly. Global assistant memory: explicit in-run confirm, always private.

## INVARIANTS

- I-SC1: nothing written to store before user approves its batch.
- I-SC2: a private memory never appears in memory.jsonl output.
- I-SC3: credential filter applies to seeded bodies unchanged; `private` is
  not a bypass.
- I-SC4: re-running scan on a warm store adds only gaps, never duplicates;
  enforced by binary-side origin uniqueness, not prompt discipline.
- I-SC5: skill degrades silently when a source is absent (shallow clone,
  no docs, no memory dir); never blocks on a missing source.
- I-SC6: private candidates never enter a bulk approval; item-level only.
- I-SC7: scan report never overstates yield; thin harvest reported as thin.

## ATTACK SURFACE

- Junk flood: heuristic-free curation bar (reject anything derivable from a
  quick code read) + 25 cap + review gate.
- Private leak: export exclusion tested; import path unaffected (exports
  never carry private items).
- Prompt-injectable source content (commit bodies, docs): review gate is the
  human checkpoint before any write; harvest subagent output is candidates
  only, never executed instructions.
- Origin forgery via import: origin column is local-store metadata; export
  never carries private items and import re-validates as today.
- Rubber-stamp fatigue: two-tier gate keeps decisions ~2 blocks + borderline
  handful; private exempt from bulk.

## Task Implementation Checklist

- [x] store: `private` + `origin` columns + schema bump + migration test
- [x] store: origin uniqueness check; duplicate rejected naming existing id
- [x] tools: remember accepts `private` + `origin`; tool schema updated
- [x] export: exclusion + withheld count + test
- [x] ui: private badge; status: private count
- [x] src/skill.md: /limpet scan Arguments entry + flow section (light/deep,
      pre-check, subagent harvest, two-tier gate, origin stamping)
- [x] README: "Seeding from history" + tool param table
- [x] tests green incl. remember-private roundtrip + origin dedup
- [ ] dogfood on fresh repo (not limpet: store warm); NO RELEASE until Ken
      tests

---

# SPEC — cost_to_learn + authority-weighted recall (proposed, ~v0.9)

Status: DESIGN. Gated on the recall_eval precision suite and the bench like
every ranking change; not yet implemented.

## The idea

A lesson that cost a production outage should outrank a trivial note in
recall and should be harder to let rot. But self-reported importance is
gameable, so authority is EARNED FROM STRUCTURE, PageRank-style: a memory's
weight comes mostly from the code and memory graph around it, not from what
it claims about itself. `cost_to_learn` is the single human input, coarse
and bounded, and it can only tilt ranking within an already relevant,
already fresh result set. It can never resurrect a stale memory, reorder
past an honesty flag, or override the freshness signals.

## State / Data Model

Two new `entries` columns (schema v2, lazy migration; `version_guard`
already gates cross-version writes):

- `cost_to_learn TEXT` — coarse bucket, NOT a number (numbers invite
  inflation): one of `trivial` (default/unset), `hours`, `days`, `incident`.
  Set on `remember`; maps to a small fixed weight (0.0 / 0.3 / 0.6 / 1.0).
- `survived_changes INTEGER NOT NULL DEFAULT 0` — incremented in
  `resolve_all` each time an anchor is `Followed` (the code moved/renamed and
  the memory tracked it) or stayed `Fresh` across a sweep that reindexed its
  file. Earned, not settable by the caller. This is "survived N refactors".

Structural signals read at recall time (no new storage):

- `fan_in` — how central the anchored code is: callers of the anchored
  symbol from the `calls` table plus the count of OTHER memories anchored to
  the same file/symbol. High fan-in = the memory describes load-bearing code.
- `evidenced` — `source = 'verified'` (already exists): a lesson with a proof
  command outranks an unproven claim.

## Authority formula (all inputs normalized 0..1)

```
authority = 0.35 * cost_bucket        # the one human input, bounded
          + 0.30 * norm(fan_in)       # earned: centrality in the code graph
          + 0.20 * norm(survived)     # earned: durability across change
          + 0.15 * evidenced          # earned: has a proof command
```

Note 65% of authority is earned from structure the author cannot directly
set. `cost_to_learn` tilts, it does not decide.

## Recall integration

Current score:
`0.45*text + 0.25*proximity + 0.20*confidence + 0.10*recency` + kind nudge.

Proposed: carve a bounded authority term without letting it dominate
relevance (a costly lesson about the wrong topic must still lose to a
relevant one):

`0.40*text + 0.22*proximity + 0.18*confidence + 0.08*recency + 0.12*authority`

Authority ALSO orders `verify_queue`: a stale `incident`-cost verified fact
is the most urgent thing to re-prove and sorts to the top.

## INVARIANTS

- I-A1: authority is a tie-breaker within relevant+fresh results; it never
  moves a stale/contradicted/invalidated item above its flag, never changes
  status, never alters confidence decay.
- I-A2: no single self-reported field exceeds 35% of authority; the majority
  is earned from structure the caller cannot set.
- I-A3: authority is fully decomposable. `recall` (behind a verbose flag) and
  `map` return the per-factor breakdown so "why is this ranked here" is
  answerable with numbers, never magic (the advisor's "why 82 not 96" test).
- I-A4: ranking changes ship only if recall_eval precision holds or improves
  AND the token bench gate holds.

## ATTACK SURFACE

- Gaming via `cost_to_learn: incident` on everything: capped at 35% and
  useless without relevance + freshness; inflating it uniformly cancels out.
- Fan-in gaming by over-anchoring a memory to many files: fan_in counts
  distinct REFERENCING code/memories, not a memory's own anchor list, so a
  memory cannot inflate its own centrality.
- survived_changes farming by trivial edits: only `Followed` (real
  rename/move) and genuine reindex-survival increment it, not cosmetic
  reformat no-ops.

## Task Implementation Checklist (when promoted from DESIGN)

- [ ] schema v2 + lazy migration; SCHEMA_VERSION bump; version_guard note
- [ ] remember: accept `cost_to_learn` bucket (validated enum); tool schema
- [ ] resolve_all: increment survived_changes on Followed / fresh-through-change
- [ ] recall: compute fan_in + authority; rebalanced score; verbose breakdown
- [ ] verify_queue: order by authority
- [ ] recall_eval: add cases proving a high-authority lesson outranks a
      trivial one AT EQUAL relevance, and does NOT outrank a more relevant one
- [ ] bench gate holds; recall_eval precision holds or improves

---

# SPEC — security + Windows hardening (v0.7.3)

Two parallel audits (adversarial security, Windows correctness) plus a
`cargo audit` advisory scan (clean, 123 deps). The security audit's headline:
`import` was a second, UNGUARDED write path into the store. The Windows
audit's headline: `canonicalize()` yields `\\?\` verbatim paths that break
the `/`-based index for every subdirectory, and CI never caught it because
tests use non-canonicalized `TempDir` roots.

## Security: consolidate import behind remember's guards

`import_jsonl` treats `.limpet/memory.jsonl` as UNTRUSTED (it arrives via
`git pull`). It now enforces what `remember` enforces:

- **Secrets rejected.** Body and evidence scanned via `secrets::detect`; a
  credential-bearing line is counted in `ImportReport.rejected`, never
  inserted. Restores the secrets.rs invariant on the import path.
- **Future timestamps rejected.** A `9999-...` `updated_at` would win the LWW
  merge against every honest later update forever; future or unparseable
  stamps are rejected.
- **Bounded line reads** (1 MiB) — no OOM from one giant line.
- **Confidence clamped** to [0,1] — an imported `1e300` can no longer pin a
  hostile memory to the top of recall.
- **Body size capped** at `MAX_BODY_BYTES` (64 KiB), enforced on both the
  remember and import paths.
- **Anchor hashes re-resolved** against the LOCAL index: a forged
  `ast_body_hash` cannot fake freshness against code this machine lacks.

Other security fixes:
- Updater caps the download at 128 MiB (OOM before checksum).
- Secret detector splits on `@` and `/` (catches `user:pass@host` shapes).
- Documented residual, by-design: the updater checksum is same-origin, not a
  signature; a compromised release is out of scope until signed builds (1.0).

## Windows: the verbatim-path root cause

- `util::canonicalize_plain` strips the `\\?\` (and `\\?\UNC\`) prefix;
  `root_from`, `install`, and `doctor` use it so stored roots join cleanly
  with `/`-separated rels.
- `util::normalize_rel` converts `\` to `/` at every tool boundary (anchors,
  `map` target, recall `working_set`) so a Windows agent's `src\foo.rs`
  matches the walker's `/`-keyed rows.
- `doctor` freshness compares canonically (verbatim/case/separator safe), so
  a correct Windows install no longer FAILs.
- `install` registers a non-verbatim command (spawnable by Claude Code).
- `validate_rel_path` rejects Windows reserved device names (NUL/CON/COM1...)
  now that a non-verbatim root would honor them.
- `uninstall` prints the real data dir (APPDATA\limpet), not a Unix path.

## INVARIANTS

- I-S1: no write path (remember OR import) admits a secret, an over-cap body,
  or an out-of-range confidence.
- I-S2: an imported anchor's freshness is judged against local code, never a
  self-asserted hash.
- I-W1: a repo-relative path round-trips identically regardless of the
  caller's separator or the platform's canonical form.

## Task Implementation Checklist

- [x] store.rs import_jsonl: secrets/future/bounds/clamp/anchor-reresolve;
      ImportReport.rejected
- [x] memory/mod.rs: MAX_BODY_BYTES on the remember path
- [x] update.rs: capped download
- [x] secrets.rs: @ / split
- [x] util.rs: canonicalize_plain, normalize_rel, reserved-device check
- [x] main.rs: root_from/install/doctor canonicalize_plain; uninstall wording
- [x] tools.rs: normalize anchors, map target, working_set
- [x] tests: import rejects secret/future/clamp; anchor re-resolve; oversize
      body; normalize_rel; backslash validation
- [x] cargo audit clean; 87 tests green; bench holds; import-secret dogfood
- [ ] PR -> CI (incl. windows-latest) -> merge -> tag v0.7.3 -> pipeline
