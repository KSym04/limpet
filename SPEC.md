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
