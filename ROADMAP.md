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

## v0.11.0 - structural lineage (context, not just the symbol)

- **AST lineage graph.** `map` on a symbol returns its structural neighborhood in
  one call: ancestors (what it extends or implements, the trait it satisfies),
  descendants (subclasses, implementors, callees), and callers, instead of the
  agent grepping and reading five to ten files to reconstruct inheritance and
  blast radius by hand. Inheritance edges are extracted for all six grammars;
  call and inherit edges store bare names and resolve read-time against the
  current symbol table, so nothing rots and every endpoint is labeled unique,
  ambiguous, or unresolved (an edge is never guessed). Traversal is depth and
  node capped with disclosed truncation. Ships with an additive schema migration
  (the `inherits` table). This is the query that makes the syntactic call graph
  pay for itself: the old v1.1 "reverse debugging" bet, earned. Gated on the
  bench, the fixture gains an inheritance chain and lineage-only questions, and
  the 4x ratio must hold with those questions net positive.
- **Live ledger in the envelope** (rides along). Every `recall` reports this
  call's savings (served, baseline, saved, reads-avoided) plus the running
  total, in `meta.ledger`, labeled an estimate (baseline understates by design;
  negative is shown, never floored). The receipt stops being a separate
  `limpet stats` trip and becomes instant per-call feedback. The sink stays the
  local ledger: nothing is written to the memory export, nothing leaves the
  machine.
- Boundary held: name resolution, not type resolution; ambiguity is disclosed,
  not resolved. limpet stays memory context, not a call-graph oracle.

## v0.12.0 — grammar wave 2

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

## v0.13.0 — freshness at scale

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
