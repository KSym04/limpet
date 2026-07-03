# limpet roadmap

Versions are indicative: features earn their tag, they are not scheduled to a
calendar. The spine through everything below is one rule — **every feature
must feed one of the receipts (bench, ledger, rework counter) or the honesty
envelope.** Anything that cannot show its number in `limpet stats` or flag its
own staleness does not ship. That discipline is the product.

## v0.8.0 — kill the every-task token leak

- **Snippet-by-symbol retrieval.** `map` returns one function body by FQN with
  line anchors, instead of the agent reading a 2,000-token file for a 30-line
  function. Today limpet saves on *repeated* questions; this saves on the
  first one, which widens who benefits from "multi-session users" to everyone.
- **Rework-avoided counter.** A third ledger dimension: stale flags that fired
  *before* an agent acted on a dead assumption. Navigation and memory savings
  have competitors; this metric structurally cannot be produced without
  code-anchored invalidation.
- **Ledger coverage for `map` and `affected`.** Snippet retrieval must feed
  the same receipt recalls do.

## v0.9.0 — portability (the seams users actually hit)

- **Repo identity by git remote, path fallback.** Today the store is keyed by
  absolute path: move or re-clone a repository and its memory is orphaned, and
  distinct paths can even collide onto one key (`/a/b` vs `/a-b`, 2026-07
  audit). Ships with a store migration path; the collision fix rides along
  because rekeying is only worth doing once.
- **Extension map in `.limpet.json`** (e.g. `.blade.php` → php, `.inc` → cpp)
  for template-heavy and legacy stacks.
- **Auto-import on first index** when a committed `.limpet/memory.jsonl`
  exists: a teammate clones the repo and the knowledge is just there.

## v0.10.0 — grammar wave 2

Go, Java, Ruby, C#, Bash. Each gated on the I7 fixture (function, class,
method, import, call), a golden hash-property case (cosmetic-invariant,
edit-sensitive), an identity-leaf audit, and an own-name-node check — the C++
lessons (`number_literal` missing from the hash identity set, the definition
name hiding in the declarator chain) say every grammar conceals at least one
node-shape surprise. The non-UTF-8 file-level fallback (I-N1) generalizes.

Riding along with this milestone, from the 2026-07 audit: full FQN
disambiguation (trait impls, C++ overloads, nested modules currently share
FQNs; the `(fqn, hash)` existence check shipped in 0.7.2 stops the flapping,
uniqueness needs schema work), and a low-entropy follow guard so trivial
duplicate bodies (empty functions, delegating one-liners) cannot be silently
followed to the wrong twin.

## v0.11.0 — freshness at scale

- **FS-event watcher** (notify) replacing the on-call sweep for very large
  repositories, where the 32-file sweep budget starts to lag.
- **Sweep prioritization:** files carrying anchors reindex first, so staleness
  is instant where it matters most.

## v1.0 — the stability contract (not features)

- Store schema, JSONL export format, and tool API frozen, with documented
  migration guarantees; the version guard extends to schema migrations.
- Signed release binaries (minisign), so `limpet update` verifies a maintainer
  signature rather than a same-origin checksum.
- Security review of the three choke points: path validation, parameterized
  SQL, secret detection.
- Docs restructured around the three receipts: benchmark, live ledger,
  rework-avoided.

1.0 means one thing: your memory is safe to depend on for years.

## v1.1+ — the bets (each gated on evals, not vibes)

| Bet | Gate before it ships |
|---|---|
| Semantic recall (embedding rerank behind a feature flag) | Must beat FTS + proximity on the recall_eval precision suite; "only if it earns its size" |
| Episode mining from session transcripts (SessionEnd hook) | Mined entries are already capped at 0.5 confidence; the miner must show a >50% keep-rate under human review or it is noise |
| Reverse debugging: given a failing symbol, return every episode and decision attached to it and its callers | The syntactic call graph exists; this is the query that makes it pay for itself |

## Standing non-goals

Unchanged from the README: not a code search engine, not a call-graph oracle,
not a cloud platform. Growth happens by deepening the memory layer, never by
becoming a worse version of an adjacent tool.
