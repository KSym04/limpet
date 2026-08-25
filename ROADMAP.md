# limpet roadmap

Versions are indicative: features earn their tag, they are not scheduled to a
calendar. The spine through everything below is one rule: **every feature
must feed one of the receipts (bench, ledger) or the honesty envelope.**
Anything that cannot show its number in `limpet stats` or flag its own
staleness does not ship. That discipline is the product.

The wedge is one capability no adjacent tool has: limpet notices when its own
context goes stale. A vector index or a hand-written architecture doc returns
confident answers about code that moved on, and time-based decay does not fix
that: age is not truth, and a schedule cannot name which memory an edit broke.
A deterministic AST-hash anchor is the only thing that flags the lie. Everything below deepens that edge or it does
not ship.

**Current focus (post-0.16): a personal tool.** 0.16 shipped the refinement
loop, and with it the owner's decision that limpet is built for one person's
own work: adoption is no longer a goal, so uptake is not a measure of
anything here. Feature work continues under the same evidence gates below,
unchanged. The next milestone is the 1.0 stability contract, and its value is
plain from that framing: the store holds the owner's own memory, so the
format and the API it depends on have to outlive the releases.

## Shipped

Delivered releases in brief (full detail lives in git history, `limpet stats`,
and the store's own memory). The roadmap below this point is what is NOT yet
built.

- **v0.9.0: portable repo identity.** The per-repo store is keyed by git-remote
  identity, not a path slug, so memory follows the project across clones, moves,
  and renames. Closed a silent path-collision data-loss seam.
- **v0.10.0: statusline doctor advisory.** `limpet doctor` reports how the
  statusline is wired (ok when it delegates to the binary, warn on a hand-rolled
  store query that will drift, note with the exact line when unwired), so the
  segment can never break silently.
- **v0.11.0: AST lineage graph.** `map` on a symbol returns ancestors,
  descendants, and callers in one call, each edge labeled unique / ambiguous /
  unresolved and resolved read-time so nothing rots (additive `inherits` table).
  The per-recall envelope ledger was built, bench-failed at 3.8x under the 4x
  gate, and dropped; the receipt stays free in `admin {op:"ledger"}`, `limpet
  stats`, and the UI. Revisit only if a richer-pack bench proves it fits.
- **v0.12.0: grammar wave 2.** Go (`embeds`), Java, Ruby (`mixin`), C#, and Bash
  bring coverage to eleven grammars, each gated by the I7 fixture and the
  hash-identity checks. Adding the ABI-15 grammars (Go, Bash) bumped the vendored
  tree-sitter core to 0.25; the original six (ABI 14) keep loading unchanged.
- **v0.13.0: sweep priority + low-entropy follow guard.** Files carrying
  anchors reindex first inside the unchanged 32-file sweep budget, so staleness
  lands where memories live. Rename/move following is evidence-gated: schema v5
  stores each symbol's normalization-buffer length beside its hash, and a
  unique match under a measured floor (calibrated across all 11 grammars)
  surfaces as `stale:low_entropy` instead of silently re-pointing the anchor at
  a trivial twin; it heals the moment the original returns. On pre-v5 stores
  the guard hardens progressively as the sweep refills.
- **v0.14.0: the truth layer.** Verification became a first-class signal on
  both paths: `verified` evidence earns a ranking boost only while its anchor
  is live (rotten proof loses the boost), typed confidence is capped below
  what proof earns, value-divergent writes surface `possible_conflicts`,
  near-identical bodies are refused with the supersede path named (`force`
  overrides; corrections never refused), and archival shelves a memory without
  deleting it while staleness keeps tracking the code underneath. The adoption
  bridge shipped in the same binary: `limpet demo` (self-verifying lifecycle
  proof, CI smoke on every platform), `limpet seed` (MEMORY.md ingest as
  `mined`), `import --path`, wider default ignores, and a hot-path panic
  ratchet in CI.
- **v0.15.0: freshness at scale, part 2.** Twins stopped sharing one identity.
  Schema v7 gives every symbol a `disamb` discriminator (trait impl path,
  receiver, parameter list, generic arity), an anchor addresses an exact twin
  with an `@<disamb>` suffix spelled the way the source writes it, and the
  slot-first resolution ladder closes the twin-masking false-Fresh hole,
  including a scope-respell step so a relabeled namespace is followed instead
  of false-staled. Recall items now name the task terms they matched (capped,
  omitted when empty; the 4.0x bench gate held at 4.2x). Removed-file purges
  became transactional. The watcher lag bench ran on the release binary
  (synthetic repos of 2k/10k/50k files, edit batches of 1/32/100): anchored
  staleness lands in ONE sweep call at every size and batch width, so sweep
  prioritization holds and no watcher is needed for correctness; a quiet
  50k-file repo pays 1.2s per sweep call (p50, against a 250 ms bar), so the
  FS-event watcher graduates from "unbuilt unless proven needed" to a
  designed backlog item, gated on this bench (see the bets table).
- **v0.16.0: the refinement loop.** The staleness engine already closed the
  detect half (rot flagged deterministically) and 0.14 closed the write-time
  half (conflicts surfaced, duplicates refused); this closes re-verification,
  landing before the v1.0 freeze because it changes the tool API. `admin
  {op:"reverify"}` accepts a fresh run of a verify_queue item's proving
  command: evidence digest and timestamp re-stamped, every anchor re-bound to
  the current code (refused, never guessed, when one no longer resolves), and
  the entry returned to active as verified. Healing became confidence-neutral:
  schema v8 stores the pre-stale confidence and refunds it when the reason
  evaporates, so a branch switch costs a memory nothing (decay once per
  reason). `admin {op:"consolidate"}` lists same-anchor, high-overlap
  clusters worth distilling into one superseding entry, and never merges
  anything itself. `limpet doctor` now names `limpet serve` processes running
  a code image older than the installed binary, so a post-update session that
  suddenly errors is diagnosed in one line instead of debugged.
- **v0.16.1: audit follow-ups.** The 2026-08-17 status audit's one security
  defect and its cluster of unkept claims, closed. Secret detection stopped
  depending on a credential being whitespace-delimited: sentence punctuation
  ends a token, edges are peeled under a constant contraction budget (linear
  time, DoS-proof, verified by timed tests), and the same classifier rules
  apply unchanged. The ledger's `session` block is counted as recalls are
  served, never inferred from a boot snapshot, and surfaces that serve no
  recalls (the `stats` CLI, the UI) drop the key instead of publishing a
  permanent zero. CI now gates clippy with warnings denied, `cargo audit`
  against a live advisory DB, and the declared MSRV read from Cargo.toml.
  Docs claims are tied to code by test: the `admin` op schema must equal the
  dispatcher, and the README's own enumerating rows are read instead of the
  whole file.

## v1.0: the stability contract (not features)

- Store schema, JSONL export format, and tool API frozen, with documented
  migration guarantees; the version guard extends to schema migrations.
- Signed release binaries (minisign), so `limpet update` verifies a maintainer
  signature rather than a same-origin checksum. Deprioritized in the
  personal-tool phase (owner decision, 2026-08-15): it returns to scope only
  if distribution ever matters again, and the 1.0 tag does not wait for it.
- Security review of the three choke points: path validation, parameterized
  SQL, secret detection.
- Docs restructured around the two receipts: benchmark and live ledger.

1.0 means one thing: your memory is safe to depend on for years.

## v1.1+: the bets (each gated on evals, not vibes)

| Bet | Gate before it ships |
|---|---|
| FS-event watcher: cut the quiet-repo sweep cost at scale (an OS-event feed marks dirty files so quiet sweeps stop paying full stat cost). Staleness LATENCY does not need it: the 0.15 lag bench measured one sweep call at every size. | `bench/lag_bench.py` quiet-repo per-call p50 < 250 ms at 50k files, with staleness latency still 1 call and 0 integrity findings |
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
