# limpet roadmap

Versions are indicative: features earn their tag, they are not scheduled to a
calendar. The spine through everything below is one rule — **every feature
must feed one of the receipts (bench, ledger, rework counter) or the honesty
envelope.** Anything that cannot show its number in `limpet stats` or flag its
own staleness does not ship. That discipline is the product.

The wedge is one capability no adjacent tool has: limpet notices when its own
context goes stale. A vector index or a hand-written architecture doc returns
confident answers about code that moved on; a deterministic AST-hash anchor is
the only thing that flags the lie. Everything below deepens that edge or it does
not ship.

## Shipped

Delivered releases in brief (full detail lives in git history, `limpet stats`,
and the store's own memory). The roadmap below this point is what is NOT yet
built.

- **v0.9.0 — portable repo identity.** The per-repo store is keyed by git-remote
  identity, not a path slug, so memory follows the project across clones, moves,
  and renames. Closed a silent path-collision data-loss seam.
- **v0.10.0 — statusline doctor advisory.** `limpet doctor` reports how the
  statusline is wired (ok when it delegates to the binary, warn on a hand-rolled
  store query that will drift, note with the exact line when unwired), so the
  segment can never break silently.
- **v0.11.0 — AST lineage graph.** `map` on a symbol returns ancestors,
  descendants, and callers in one call, each edge labeled unique / ambiguous /
  unresolved and resolved read-time so nothing rots (additive `inherits` table).
  The per-recall envelope ledger was built, bench-failed at 3.8x under the 4x
  gate, and dropped; the receipt stays free in `admin {op:"ledger"}`, `limpet
  stats`, and the UI. Revisit only if a richer-pack bench proves it fits.
- **v0.12.0 — grammar wave 2.** Go (`embeds`), Java, Ruby (`mixin`), C#, and Bash
  bring coverage to eleven grammars, each gated by the I7 fixture and the
  hash-identity checks. Adding the ABI-15 grammars (Go, Bash) bumped the vendored
  tree-sitter core to 0.25; the original six (ABI 14) keep loading unchanged.
- **v0.13.0 — sweep priority + low-entropy follow guard.** Files carrying
  anchors reindex first inside the unchanged 32-file sweep budget, so staleness
  lands where memories live. Rename/move following is evidence-gated: schema v5
  stores each symbol's normalization-buffer length beside its hash, and a
  unique match under a measured floor (calibrated across all 11 grammars)
  surfaces as `stale:low_entropy` instead of silently re-pointing the anchor at
  a trivial twin; it heals the moment the original returns. On pre-v5 stores
  the guard hardens progressively as the sweep refills.

## v0.14.0 — freshness at scale, part 2

- **Full FQN disambiguation** (deferred from grammar wave 2): trait impls, C++
  overloads, and nested modules currently share FQNs; the `(fqn, hash)`
  existence check shipped in 0.7.2 stops the flapping, but true uniqueness needs
  schema work.
- **FS-event watcher** (notify) replacing the on-call sweep for very large
  repositories — gated on evidence: build a lag bench on a genuinely large repo
  first; if sweep prioritization keeps anchored-file staleness latency
  acceptable, the watcher (and its per-platform risk surface) stays unbuilt.

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
| Authority-weighted recall: knowledge earns rank structurally (fan-in, refactors survived, verification), with `cost_to_learn` as one bounded human input (<=35% of authority); never overrides staleness. Full design in SPEC.md. | recall_eval precision holds or improves AND the token bench gate holds |
| Semantic recall (embedding rerank behind a feature flag) | Must beat FTS + proximity on the recall_eval precision suite; "only if it earns its size" |
| Episode mining from session transcripts (SessionEnd hook) | Mined entries are already capped at 0.5 confidence; the miner must show a >50% keep-rate under human review or it is noise |
| Local event hooks: an opt-in exec hook fires on memory transitions (remembered, went stale, contradicted) with the event as JSON on stdin, zero baked-in network, so you can wire limpet into your own CI, editor, or scripts | A concrete public consumer exists AND it feeds a checkable signal, e.g. a local `check` that exits nonzero when a diff contradicts an active decision |

## Standing non-goals

Unchanged from the README: not a code search engine, not a call-graph oracle,
not a cloud platform. Growth happens by deepening the memory layer, never by
becoming a worse version of an adjacent tool. Refusing scope is a feature, not a
gap. The named refusals:

- **Embeddings never decide freshness or identity.** Staleness is a
  deterministic fact from AST hashes; a similarity score cannot notice when code
  moves on, and noticing is the whole premise. Embedding rerank stays a gated,
  flag-guarded ranking bet (above), never the memory or freshness mechanism.
- **Not an agent orchestrator.** limpet feeds the architect / coder / reviewer /
  tester loop with fresh, anchored context; it never becomes the loop. The
  event-hook bet emits events for your own framework to react to, nothing more.
- **Not a few-shot or prompt-template engine.** Anchored `episode` and
  `decision` memories already carry how-and-why in context; a static
  gold-standard file is out of lane.
- **Not a hand-maintained architecture map.** The structural map is derived from
  the AST on every query, never hand-written where it silently rots. Static
  taste rules (stack, style, guardrails) stay in your CLAUDE.md or .cursorrules;
  limpet supplies the part that must stay true to the code.
