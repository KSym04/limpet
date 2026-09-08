# SPEC: the brain view, v0.17.0 (ui restyle)

Status: IN PROGRESS 2026-09-08. Source: the owner's ask that the visual
memory graph read as a brain, validated on 2026-09-08 with three throwaway
mockups over the real 601-node all-projects graph (neuron rendering alone did
not read as a brain; a brain-silhouette containment layout did, at 601 and
at 248 nodes; the combined variant encoded semantics in position and read
worse). This ports the silhouette layout into the shipped `src/ui.html`. It
is a restyle of one display surface: no tool, store, export, or route
changes, so the v1.0 contract in STABILITY.md is untouched. It ships as
0.17.0 together with the stability contract's code half.

## INVARIANTS

- I-B1: the health encoding is byte-identical to 0.16. Memory fill colours
  (`#3fb96f` active, `#e0a437` stale, `#e05252` invalidated, `#6b7885`
  superseded, `#8296a8` unknown), the 2px `#d8e1ea` verified ring, the
  dashed `#7f8ea3` private ring, the `7 + conf * 8` radius rule, the code
  square, and the four relation strokes (contradicts red dashed, supersedes
  blue, supports green, anchor neutral) are unchanged in value. The
  silhouette may change WHERE a node sits, never what its health looks like.
- I-B2: the settle/idle contract holds. A settled, idle tab does zero
  per-frame work beyond the calm check; every motion is finite by
  construction (the simulation stops at CALM_FRAMES of low energy, and a
  hard budget of MAX_TICKS_PER_WAKE ticks per wake forces calm if energy
  never settles; measured settle is 556 ticks at 604 nodes against a 4000
  budget); a poll that returns the
  same visible set never wakes the layout; a poll that adds nodes rescales
  positions but never resets the user's pan or zoom. `fitView` runs only on
  load, project switch, and filter change.
- I-B3: determinism. No `Math.random` anywhere in the file. Every per-node
  quantity (initial position, lobe seed) derives from a hash of the node id,
  so two loads of identical data settle to the same picture.
- I-B4: the surface stays closed and read-only. No new routes, no new
  fetches, GET only, one embedded file, zero external references (the only
  URL in the file is the SVG namespace of the favicon). The I-S5 ui tests
  are untouched and stay green; a new `tests/ui_html.rs` pins single-file,
  no-external-ref (absolute and scheme-relative), no-`Math.random`, and a
  `node --check` syntax pass on the extracted script body when node is on
  PATH (a missing node fails the test under `CI`, and only skips on a
  developer machine).
- I-B5: the silhouette is honest. Its scale follows the VISIBLE node count;
  below 40 visible nodes the footer says the shape needs density instead of
  pretending; a single-project view draws no lobes; the all-projects view
  spreads stores over the nine lobes largest first (keyed by the id prefix
  the server already emits, never by display name, so two stores that
  share a name are distinct in the map; past nine stores a lobe is shared)
  and labels each store's centroid with its project name (stores under
  five visible nodes get a smaller label, and only once node labels are
  showing). A node fatter than the silhouette's thinnest region (the
  brainstem, about 0.19 units) is held by its centre with its containment
  radius capped at 0.09 * S; at 51 or more visible nodes the cap exceeds
  the largest radius and nothing changes, below that a high-confidence
  memory routed into the stem may show its rim over the line rather than
  being shoved every tick.
- I-B6: docs truth. The footer legend, the README "Visual memory" section,
  its caption, and `docs/limpet-ui.png` describe what the binary draws. The
  screenshot is taken from the shipped binary on the limpet project's own
  store (no other project names in a public asset).
- Carried: I3, BENCH >= 4.0x, CONF, POS, panic ratchet, docs_in_sync,
  display surfaces read-only, migrations no-op on re-run, I-A1..I-A7,
  I-S1..I-S6.

## SURFACES

- `src/ui.html` only, in the script block plus the footer markup and one
  footer CSS rule (`flex-wrap`). Header, stats, filters, project select,
  detail panel, pan/zoom, click hit-test, the `esc()` HTML escaper, and the three fetch paths
  are unchanged. The isotropic centering force is replaced by: a signed-
  distance containment along a hand-authored side-view brain polygon
  (Catmull-Rom smoothed, SDF grid built once at load), a cortex bias for
  memory nodes and an interior bias for code nodes, a short-range softened
  repulsion kernel with a cutoff, per-store lobe attraction in the
  all-projects view and a weak isotropic pull in a project view. The
  silhouette, cortex band, and two fissure lines are drawn under the graph.
  Lobe labels sit at each store's centroid, counter-scaled to 11px (9px and
  only when zoomed in for stores under five visible nodes). Polls are tagged
  with the project they were issued for: a response for a project the user
  has since left is discarded, and a switch that lands during a poll is
  queued instead of dropped.
- `tests/ui_html.rs` (new): the I-B4 guards.
- `README.md`: the Visual memory section, caption, and command-table row;
  `docs/limpet-ui.png` regenerated.
- `ROADMAP.md`: 0.17.0 folded into Shipped; `SPEC.md` marked shipped at the
  tag.

## ATTACK SURFACE / HAZARDS

- Perf: repulsion runs on a uniform grid (cell = cutoff, 3x3 neighbourhood)
  with the cutoff at 0.25 * S floored at 60 px. Measured on the shipped page
  at 604 nodes (B4 receipts): 0.42 ms per tick (was 0.83 with the plain
  O(n^2) loop at a 0.55 * S cutoff), 0.18 ms per draw, 556 ticks / 2.4 s to
  settle (was 930 / 7.1 s; three ticks per frame while hot, two while warm,
  one cooling, presettle 100 ticks), nearest-neighbour p10 up 9.7%. The
  grid alone at the old cutoff was SLOWER (1.33 vs 1.08 ms): the tighter
  cutoff is what makes it pay, and a 60 px floor keeps a 13-node view from
  clumping (p10 fell 33% without it).
- View theft: a 5s poll that changes the visible count must not refit the
  view under the user (I-B2). Positions scale by the ratio; the view stays.
- Lobe flapping: recomputing lobe order from counts on every poll would
  migrate a whole store to another lobe when counts cross. The lobe map is
  built once per page load or project switch and only ever grows.
- Reduced motion: synchronous settle is capped (600 ticks, about 0.4 s at
  604 nodes at the measured 0.66 ms presettle tick) so the page never
  stalls.
- Containment and settle (B4): forces alone could not hold the outline
  (F.out 3.0 still left 133 rims over it) and a projection alone left
  energy humps; the shipped page uses both: F.out 3.0 plus a deterministic
  projection after integration that moves an overshooting rim back along
  the field gradient (max 3 iterations, outward velocity dropped). Result 0
  rims outside at 604 / 190 / 13 nodes, unchanged after 1200 further ticks.
  Residual energy is a slow collective creep of interior memories toward
  the cortex, not wall chatter: cool-phase damping 0.5 once energy drops
  below 0.1 * N keeps the settled state under the threshold through 1000
  further ticks with a 2.2x margin (the mockup's 9% margin failed the
  200-tick check at 190 nodes).
- Label legibility: labels are gated on on-screen spacing rather than a
  fixed zoom, so 601 nodes at fit scale draw no labels and a zoomed view
  draws them; lobe labels carry a dark halo.
- The all-projects payload prefixes every id with `<store key>:`; a single
  project payload does not. The lobe key is `id.slice(0, id.indexOf(":"))`
  only in the all view; store keys never contain a colon.
- The SDF build runs once at page load (~16k cells x 216 segments); it is
  synchronous and measured at well under 50 ms.

## Task Implementation Checklist: 0.17.0 brain view

- [x] B1 SPEC section (this) on branch `feat/brain-ui-0.17` off the
      stability branch (0af07df)
- [x] B2 RED first: `tests/ui_html.rs` pins no external refs, no
      `Math.random`, single script block, and `node --check` on the script
      body; the `Math.random` guard failed on 0.16's file (2 passed / 1
      failed at branch base), green after B3
- [ ] B3 port the silhouette layout into `src/ui.html`: forces core,
      silhouette + cortex band + fissures drawn under the graph, per-store
      lobes with labels, density note, footer legend and CSS; keep fetch,
      poll, select, and server keys; `fitView` policy per I-B2; lobe map
      persistence per the hazard above
- [x] B3 port landed (src/ui.html +500/-71, guard tests 3/3, node --check
      ok). Live on the release binary against a COPY of the real stores:
      604-node all view settles deterministic (position checksum identical
      across two loads), idle settled tab 0 draws / 0 ticks in 2 s, two
      unchanged polls left view and S bit-identical, filter click refits,
      two stores named `limpet` took two lobes, sparse 13-node view shows the
      density note. NOT yet met, handed to B4: 184 of 604 rims overshoot the
      outline (occipital/cerebellum bulge), settle margin 9%, brainstem
      clipped at fit scale.
- [x] B3b (found by B3's live check, fixed RED-first in `tests/ui_http.rs`
      + `src/ui.rs`): every browser request to `limpet ui` paid the full 5 s
      read timeout since the 2026-07 hardening (measured 5.0 s per request
      on the shipped 0.16.0 AND 0.16.1 binaries): each header line got a
      fresh inner BufReader over the take that swallowed the remaining
      header bytes, and the next read blocked on an empty socket. The route
      tests never saw it because they shut down their write half and handed
      the drain an EOF. Second defect from the same test file: an over-cap
      request line was truncated at 8 KB and SERVED 200 instead of refused;
      now 414. Fix, after the review round: one shared BufReader read
      through `read_line_by_deadline`, which arms what is left of a 5 s
      wall-clock deadline before EVERY recv (a plain `read_line` loops
      inside one call and only sees the per-recv timeout, so a client
      dripping one byte per 250 ms held the thread ~15 s: caught by the new
      drip test, which FAILED against the first fix), plus a write timeout
      for a client that never reads its response. Receipts: ui_http 4/4
      green (browser-shaped keep-alive answered in ms, drip answered at the
      deadline, 414 at exactly the cap in one write, cap-minus-one routed
      200; the first two were RED at 5.0068 s and a 200 on a 9 KB line),
      stability 15/15, ledger_session 5/5, docs_in_sync 7/7, clippy
      --all-targets 0 warnings
- [x] B4 optimize with receipts (headless Chromium, 1440x900, real store
      copy: all 604, limpet 190, sunoku 13). Kept: grid repulsion with a
      0.25 * S cutoff floored at 60 px; 3/2/1 ticks per frame by energy and
      presettle 100; F.out 3.0 plus the post-integration projection; cool
      damping 0.5 below 0.1 * N; fitView on the real polygon bbox with an
      18 px rim pad (brainstem no longer clipped). Receipts, baseline ->
      final: outside 184 -> 0 (max overshoot 11.05 -> 0 px), tick 0.83 ->
      0.42 ms, draw 0.18 ms, settle 930 -> 556 ticks and 7086 -> 2449 ms,
      p10 16.9 -> 18.6 px, energy after calm 13.7 max vs 30.2 threshold at
      604 and 6.0 vs 9.5 at 190 (baseline FAILED at 190: 53 of 200 ticks
      above), deterministic checksum identical on two loads, idle 0 ticks /
      0 draws in 2 s, server latency 0.001 to 0.08 s per endpoint. Rejected
      with figures: grid at 0.55 * S (slower), cutoff 0.35 * S (149 rims
      outside), no px floor (13-node p10 -33%), F.out without projection
      (133 rims), projection without F.out (humps), global damping 0.6/0.7,
      early cool switch. Post-B4 controller edits: per-wake tick budget
      (MAX_TICKS_PER_WAKE 4000) so I-B2 is finite by construction, footer
      lobe wording, reduced-motion comment
- [x] B5 docs: README Visual memory section, caption, alt text, and roadmap
      pointer rewritten (0.16.1 folded into shipped, 0.17.0 named as this
      release, next = the 1.0 tag after soak); `docs/limpet-ui.png` retaken
      from the 0.17.0 release binary on a copy of the limpet store (190
      nodes, 1180x820, no other project's name in the asset); ROADMAP
      0.17.0 Shipped entry with the B4 numbers and the v1.0 section
      narrowed to what the tag still waits for; docs_in_sync 7/7 green.
      Review corrections applied: lobe wording (nine lobes, largest first,
      shared past nine) in README, ROADMAP, I-B5, and the footer; "costs
      nothing" -> "does no per-frame work" (a settled tab still redraws once
      per 5 s poll); `esc()` named as the HTML escaper; the unbalanced
      parenthesis in the roadmap pointer
- [x] B6 adversarial review, two rounds, read-only agents with line-cited
      repros. Round 1 (ui.rs fix, both test files, docs): 19 findings, all
      addressed: wall-clock request deadline + write timeout (the deadline's
      first form FAILED the new drip test at 15.27 s, replaced by a
      fill_buf-driven reader that re-arms before every recv), 414 pinned at
      exactly the cap in one write plus a cap-minus-one routed case, early
      child-exit detection in spawn, scheme-relative and CI-node guards in
      ui_html, lobe wording (nine lobes, shared past nine) in README /
      ROADMAP / I-B5 / footer, "costs nothing" corrected, `esc()`
      clarified, a stray parenthesis, B1/B2 ticked; two findings were
      pre-existing on main and fixed anyway (write timeout, drip). Round 2
      (final ui.html, JS correctness + honesty envelope, 47 verified-ok
      items): 6 findings, all fixed: project switch during an in-flight
      poll was dropped and the stale payload merged under the new view
      (IMPORTANT; polls now tagged and the switch queued), a filter click
      before the first payload consumed its refit, a node fatter than the
      brainstem was shoved every tick (radius cap), lobe-label gating
      undocumented, a false gradAt comment, ledger fetch untagged
- [x] B7 QA (2026-09-08 22:24 to 22:25 local, on the final tree with
      version 0.17.0 synced in Cargo.toml, server.json, Cargo.lock): release
      build 0 (`limpet 0.17.0`), tests 404 passed / 0 failed across 19
      suites, clippy --all-targets 0 warnings, panic ratchet 0, bench 4.2x
      overall / 5.4x lineage (gate 4.0x), demo 0, cargo audit 0, cargo
      package 0, mcp-publisher validate ok, em-dash 0 authored files with
      the named `.limpet/memory.jsonl` exclusion at 5. Browser checks on the
      release binary against a store COPY: B4's receipts (all 604 / limpet
      190 / sparse 13: 0 rims outside, settle 556 ticks, idle 0 ticks and 0
      draws in 2 s, deterministic checksum, 0 console errors) plus the
      controller's own load of the limpet view for the README asset (190
      nodes, calm at 588 ticks, 0 errors) and the 4 http tests over the
      real socket path
- [ ] B8 ship: version sync 0.17.0, PR, 3-OS CI green, merge,
      /deploy-limpet, post-release verification

---

# SPEC: the stability contract, v1.0 (code half)

Status: IN PROGRESS 2026-08-25. Source: the v1.0 roadmap milestone plus the
0.16.1 audit's deferred table. This builds the ENGINEERING half of the
contract: the guards, wire format, and tests that make the store safe to
depend on for years. It does NOT tag 1.0, does not bump any version, and
does not build the signed-binary half (deprioritized in the personal-tool
phase). Tagging 1.0 is the owner's call once this has soaked.

## INVARIANTS

- I-S1: an older binary never writes to a store whose stamped schema_version
  exceeds its own SCHEMA_VERSION. `Store::open` refuses loudly, naming both
  versions and the fix, BEFORE any migration runs. Read-only display
  surfaces (statusline, hook) never run this check: they open read-only,
  print nothing, and exit 0 on any problem, unchanged.
- I-S2: the schema_version stamp is monotonic on disk. No code path lowers
  it: migrations stamp only forward, and re-running the chain on an
  already-migrated store leaves the stamp untouched.
- I-S3: the JSONL export names its format. The first line is a header
  record `{"limpet_export": 1, "schema_version": N}`; import accepts a
  missing header (every pre-1.0 file) unchanged, skips the header line as
  data, and refuses the whole FILE loudly, naming both versions, when the
  header claims a format newer than the binary supports. Per-line entry
  guards and LWW semantics are byte-for-byte unchanged.
- I-S4: one migration chain. `open` and `open_in_memory` run the same list
  through one function, and a test asserts the two paths produce identical
  schemas (sqlite_master SQL compared), so the chain cannot fork again.
- I-S5: `limpet ui` is proven read-only and closed. Tests pin: an unknown
  `?project=` key is refused (never opens an arbitrary path, proven against
  a planted decoy store outside the data dir), unknown routes 404, and a
  full request sweep leaves the store's logical content identical (every
  real table dumped whole; file bytes are not the assertion because WAL
  bookkeeping moves them). Scope: read-only is proven for current-schema
  stores; on an older store the ui migrates forward like every read-write
  surface, by design.
- I-S6: every SQL statement in the tree binds values through parameters;
  the only string interpolation in SQL text is compile-time constants.
  Verified by review sweep, recorded here, and the secrets choke point
  carries the 0.16.1 linear-time guarantee.
- Carried: I3, BENCH >= 4.0x, CONF, POS, panic ratchet, docs_in_sync,
  display surfaces read-only, migrations no-op on re-run, I-A1..I-A7.

## ATTACK SURFACE / HAZARDS

- The downgrade rewrite: 0.16.1 and earlier, an older binary opening a
  newer store re-runs its own chain (each migration self-gated on table
  state, so no DDL fires) and stamps its OWN SCHEMA_VERSION over the newer
  stamp. The store then reads as old while carrying new columns, and the
  next new binary re-stamps forward: the flip-flop is silent. I-S1/I-S2
  close it.
- A future export format read by an old import: measured, not assumed. A
  0.16-and-earlier importer hits the header's missing `id` and ABORTS the
  whole import transactionally ("entry missing id"): safe, nothing partial,
  but unfriendly; the remedy is the current binary. The graceful refusal
  naming both formats only exists going FORWARD (a 1.x binary reading a
  2.x file).
- `?project=` on the UI is attacker-adjacent input on a localhost socket;
  resolve_project must stay an exact-match lookup against enumerated store
  keys, never a path component.
- The schema guard must not brick a LEGITIMATE downgrade rescue: refusal
  names the exact remedy (run a current binary, or export from one), and
  read paths (recall over an old serve) still work because the guard sits
  on `open`'s migrating path... refusal IS the behavior there too; the
  error text carries the way out. Display surfaces stay silent by design.

## Task Implementation Checklist: v1.0 code half

- [x] S1 schema guard + monotonic stamp (I-S1, I-S2): RED first (both tests
      failed at branch base: the open succeeded and the stamp was rewritten
      down), then `schema_guard` (refuses before any migration, names both
      versions and the remedy) and `stamp_schema_version` (SQL-layer
      forward-only WHERE clause) replacing the seven hand-copied stamps
- [x] S2 export header + import format gate (I-S3): RED first (header and
      future-format tests failed), then the header line on export and the
      header/format branch on import. Two existing tests that parsed export
      line 0 as an entry were updated to find the entry line (suite at S2
      completion: 390 passed / 0 failed; the final tally lives in S6)
- [x] S3 shared migration chain `run_migrations` called by both open paths,
      with a sqlite_master equality test pinning the two schemas identical
      (I-S4)
- [x] S4 ui hardening tests (I-S5): unknown and traversal-shaped
      `?project=` keys refused with no filesystem effect, unknown routes
      404, and a full request sweep (hostile inputs included) leaves the
      store's logical content identical (entries + meta_kv dump compared;
      file bytes are not the assertion because WAL checkpoints move them)
- [x] S5 SQL parameter sweep: every statement binds through parameters; the
      only dynamic SQL text in the tree is the generated placeholder list
      for remember's near-dup IN clause (src/memory/mod.rs:450-453), values
      bound. STABILITY.md written at the repo root: frozen surfaces, the
      guards enforcing them, and the explicit non-promises (I-S6)
- [x] S6 QA, with one honest caveat. Pre-review gate (all green, local):
      release build 0, suite 393 passed / 0 failed, clippy --all-targets 0
      warnings, ratchet ok, bench true-exit 0 at 4.2x / 5.4x, demo 0, cargo
      audit 0, em-dash 0 authored. Whole-branch adversarial review ran (18
      confirmed findings, all fixed, round record below) and the fix suite
      passed 15/15. Then the machine entered the documented Kaspersky kavd
      exec semi-wedge (every fresh process launch stalls 10-22 s wall at ~0
      CPU; `limpet demo` measured real 22.2 s / user 0.04 s), which expires
      the ui tests' readiness windows: 3 ui tests in stability.rs AND the
      untouched ledger_session ui test (green on this diff's base in
      three-OS CI) fail locally on child-startup timeouts. That is the
      machine, not the tree: the post-fix full local tally reads 394/3 with
      exactly those wedge-shaped failures. CI (no Kaspersky, 3 OSes) is the
      authoritative gate for this branch; the wedge clears only by the
      owner restarting Kaspersky or rebooting
- [ ] S7 PR on green CI; NO version bump, NO tag; 1.0 tagging is the
      owner's tap after soak

## S6 whole-branch review round (2026-08-25)

Four dimensions (schema-guard, wire-format, ui-tests, docs-truth), every
finding re-verified by an adversarial refuter with live repros. 19 raised, 1
refuted, 18 confirmed. All fixed in this round:

- IMPORTANT: I-S1 held only at open. A live `serve` handle kept writing
  after a newer binary migrated the store forward through `limpet ui`
  (which never stamps code_version), demonstrated end-to-end. Fixed:
  `version_guard`, which every tool call runs, now also refuses when the
  schema stamp exceeds the binary's SCHEMA_VERSION, read with the same CAST
  coercion the monotonic stamp uses (a second finding: a strict Rust parse
  disagreed with SQL CAST on stamps like '9x'). Pinned by a live-handle
  test.
- IMPORTANT: a failed bootstrap auto-import burned the one shot
  (`indexed_at` stamps before the import runs), so a 0.16 teammate hitting
  a headered export would silently never receive the shared memory even
  after upgrading. Fixed: a `bootstrap_import_pending` marker makes the
  next index retry until an import succeeds; pinned by a fail-repair-retry
  test.
- IMPORTANT x3, test blindness: the traversal test could not catch a
  request-string-pathing resolve_project (fixed: planted decoy store
  outside the data dir, key `../outside` must refuse); content_dump was
  blind to archived/anchors/links/inherits (fixed: generic every-real-table
  dump, rows sorted); the refused-open test proved nothing about migration
  order (fixed: a dropped v8 column must stay dropped through the refusal,
  which only holds when the guard precedes the chain).
- Header lines carrying entry data were silently dropped uncounted (fixed:
  only an id-less line is a header; a marker riding on an entry imports as
  data), and a non-integer marker fabricated format 9223372036854775807 in
  the refusal (fixed: honest "unrecognized marker" refusal). Both pinned.
- Test hygiene: ui children now die via a Drop guard instead of leaking on
  assert failure; spawn retries three fresh ports against the bind-race.
- Docs: I-S5 and the schema_guard comment overclaimed ("bytes identical",
  "byte-for-byte untouched"); both scoped to logical content with the WAL
  caveat. STABILITY.md's refusal claims re-scoped and now true via the
  version_guard extension; its SQL sweep sentence names both
  runtime-assembled sites. ROADMAP's signed-binary line carries the owner's
  deprioritization so the 1.0 tag cannot falsify it. S2's suite receipt
  dated.
- Refuted (1): the S2 "390 passed" receipt as drift; it reconciles as a
  point-in-time record, kept with an explicit date instead.

---

# SPEC: audit follow-ups, v0.16.1

Status: IN PROGRESS 2026-08-18. Source: the 2026-08-17 status audit (six
assessors plus an adversarial critic, gates re-run live on 2499752). The audit
found the codebase healthy and every gate green (344 tests, clippy 1 known
MSRV lint, bench 4.33x true gate, 12 assets published) and surfaced one
security defect plus a cluster of claims the repo makes and does not keep.
This spec closes the defect and the false claims. It does NOT open v1.0 work
(schema-guard extension, JSONL format version, rework counter, ui.rs tests):
those are recorded at the end as the v1.0 backlog, unbuilt.

## INVARIANTS

- I-A1: secret detection never depends on a credential being delimited by
  whitespace or the 2026-07 punctuation set. Sentence punctuation ends a
  token, and leading/trailing punctuation is trimmed before any length or
  charset gate, so `AKIA...` at the end of a sentence, in markdown emphasis,
  or in parentheses classifies exactly as the bare token does. Detection stays
  high-precision: ordinary prose about tokens must still store cleanly (the
  existing negative tests are the floor, not the ceiling).
- I-A2: the trim is a boundary rule, never a charset relaxation. A trimmed
  token is classified by the SAME provider rules; no rule loosens its length
  or charset test to absorb punctuation, so `AKIA` + 16 alnum + `X` is still
  not a key.
- I-A3: every receipt a doc names exists in code. `rework-avoided` has no
  counter, no field, and no op, so the claim goes; the two real receipts
  (bench, ledger) stay. Docs may not assert a capability the binary does not
  carry.
- I-A4: the `session` block in the ledger payload reports THIS process. A
  surface that serves recalls counts them as it serves them; a surface that
  structurally serves none (the `stats` CLI, `limpet ui`) drops the key rather
  than publishing a permanent zero or a copy of lifetime. Lifetime figures are
  unchanged by this fix. It binds EVERY surface that serves the payload: the
  review round found `limpet ui` publishing the same copy.
- I-A5: CI gates what the local QA gate gates. Clippy runs with warnings
  denied, `cargo audit` runs at a pinned tool version against an advisory DB
  tracking upstream, and the declared MSRV is built, so a "green CI" claim
  covers the same surface the release checklist claims. The DB tracks upstream
  deliberately: a commit-frozen DB would have read green straight through
  RUSTSEC-2026-0204, the advisory that created the job. Any lint deliberately
  kept carries an inline `#[allow]` with its reason, never an unexplained
  warning.
- I-A6: the agent-facing instruction file (`src/skill.md`) names every tool op
  the shipped binary carries. A headline feature unreachable through the
  documented flow is a shipped regression, not a docs nit. Enforced, not
  remembered: `tests/docs_in_sync.rs` reads the shipped `admin` op enum and
  asserts skill.md names each one.
- I-A7: the em-dash sweep is byte-accurate over TRACKED files of every type
  (`LC_ALL=C git grep $'\xe2\x80\x94'`), never an extension-filtered grep. The
  0.14 and 0.16 receipts read "em-dash 0" while `install.sh` carried four: a
  filtered sweep is a fabricated green. One exclusion is sanctioned and must
  be NAMED in every receipt that uses it: `.limpet/memory.jsonl` holds
  exported memory bodies, store data rather than authored text, and rewriting
  stored bodies to satisfy a style sweep would falsify the data; the receipt
  therefore reports authored files at zero and that file's own count beside
  it, never a silent global zero.
- Carried: I3, BENCH >= 4.0x, CONF, POS, hot-path panic ratchet, docs_in_sync,
  display surfaces read-only, migrations no-op on re-run.

## SURFACES

- `secrets::detect`: the boundary is a CLASS, not a list. A character ends a
  candidate token unless it is alphanumeric, `-`, `_`, or `.`, which covers
  every Unicode punctuation and symbol (smart quotes, ellipsis, dashes,
  fullwidth and ideographic marks) plus the ASCII characters an enumerated set
  kept missing (`&` `%` `$` `^`, the URL query-string surface). `-` and `_`
  stay unsplit because credential bodies embed them; `.` stays unsplit because
  a JWT is three dot-joined segments, and is peeled at the token EDGES instead,
  which reaches the sentence period without touching interior segments.
  Classification then runs in two peels: pass 1 keeps `-`/`_` so a 39-char
  `AIza` key ending in one is not trimmed THROUGH the exact `n == 39` gate;
  pass 2 peels them too and only runs on a token pass 1 already rejected.
  Applies on every write path that already scans: `remember` body + evidence,
  `reverify` command + output, `import` per line.
- `tools::ledger_payload`: `session` stopped being inferred (lifetime minus a
  boot snapshot) and became COUNTED: `ledger_add`, the single sink every
  recall already passes through, tallies an in-process accumulator beside the
  shared meta_kv counters, so `session` can never go negative and never
  mirrors lifetime. The `stats` CLI arm and `src/ui.rs` `/api/ledger` serve no
  recalls, so both drop the `session` key entirely (`ui.html` reads `lifetime`
  alone, so the panel is unchanged on screen); `serve` keeps reporting its own
  honest tally. `ledger_session_start` survives as the `ledger_reset` guard:
  the wiping process must not report a session larger than the lifetime it
  just cleared.
- `src/index/lang.rs`: `map_or` becomes `is_none_or`, unblocked by the
  declared MSRV. The floor is 1.86, NOT 1.82: the ladder was run rung by rung
  and 1.82/1.83/1.84 fail on the edition2024 gate (rustls -> zeroize 1.9.0)
  and 1.85 fails on icu_* 2.2 / idna_adapter 1.2.2 (ureq -> url -> idna). No
  MSRV comment existed to delete, and the justification was already false:
  `src/store.rs:226` calls `is_none_or` today, so the tree required 1.82+
  before this change.
- `.github/workflows/ci.yml`: adds clippy (`-D warnings`), `cargo audit`, and
  an MSRV build job to the existing 3-OS matrix.
- Docs: `src/skill.md` (`/limpet review` drives `admin {op:"reverify"}` and
  reports `{op:"consolidate"}` candidates), `README.md` roadmap pointer,
  `ROADMAP.md` current-focus and spine rule, `install.sh` output strings.

## ATTACK SURFACE / HAZARDS

- The audit's proof of the defect: a credential followed by `.` became a
  21-byte token, missing the `n == 20` AWS gate, the `n == 39` Google gate,
  and the all-alphanumeric GitHub gate. It was then stored AND written to
  `.limpet/memory.jsonl`, which is git-tracked against a public remote, while
  `src/secrets.rs:3-5`, `SECURITY.md`, and `README.md:403` promised the
  opposite. Reproduced end-to-end over MCP stdio against the shipped binary.
- Trim widens what reaches the classifier, so the false-POSITIVE risk rises:
  prose like "rotate the sk-key." must still store. Every negative test in the
  existing suite is re-run, and new prose negatives are added alongside the
  new positives.
- Widening the split set cannot be done by adding `-` or `_`: Slack, OpenAI,
  Stripe, and GitHub PAT bodies embed them, and splitting there would break
  detection instead of fixing it.
- The committed `.limpet/memory.jsonl` is 44 days and 46 entries stale (72
  lines against 118 live). Refreshing it publishes real memory bodies to a
  public repo, so the re-export is prepared, scanned with the FIXED binary,
  and held for the owner's explicit tap. Not automatic.

## Task Implementation Checklist: 0.16.1

- [x] T1 src/secrets.rs: punctuation-boundary fix per I-A1/I-A2; unit tests
      covering each provider at end-of-sentence, in parentheses, in markdown
      emphasis, and after a comma already covered; prose negatives re-run and
      extended; body + evidence + import paths proven through the public API.
      RED first: 11 of 18 tests failed at HEAD and `remember` returned a
      successful RememberResult for a body carrying a period-terminated AWS
      key, the leak reproduced through the public API. Review then found two
      CRITICAL holes in the first fix, both real, both closed: an
      `is_ascii_punctuation` trim ate the `-`/`_` a 39-char Google key can end
      on (a DETECTION REGRESSION against HEAD), and an ASCII-only rule let
      Unicode punctuation walk past. classify_token is byte-for-byte unchanged
      (I-A2 holds)
- [x] T2 docs truth pass: src/skill.md names reverify + consolidate (I-A6),
      README roadmap pointer moves to 0.16 shipped / v1.0 next and drops the
      rework-avoided claim (I-A3), ROADMAP current focus replaced with the
      personal-tool phase and the spine rule drops the third receipt,
      install.sh em dashes removed (I-A7). Review caught the v1.0 backlog line
      still naming the three-receipt set; fixed. Controller additions:
      skill.md now routes an archived entry through restore-then-reverify,
      states what `import` actually enforces, and makes `working_set` explicit
      on the recall steps per the T5 measurement
- [x] T3 CI + toolchain: declare `rust-version` (verified by building on that
      exact toolchain), `cargo update -p crossbeam-epoch` to clear
      RUSTSEC-2026-0204, is_none_or, clippy `-D warnings` + `cargo audit` +
      MSRV jobs in ci.yml (I-A5). MSRV proved by ladder at 1.86; lockfile diff
      is 2 insertions / 2 deletions, crossbeam-epoch alone; clippy went into
      the 3-OS matrix rather than an ubuntu-only job because the tree carries
      real `#[cfg(windows)]` / `#[cfg(unix)]` code a single-OS lint never
      expands
- [x] T4 session honesty: `session` is now counted per recall by `ledger_add`
      in the serving process, never inferred from a boot snapshot; the `stats`
      CLI serves no recalls and drops the `session` key, with
      tests/ledger_session.rs proving the CLI and ui drops, the per-handle
      tally, cross-server isolation, and that a reset in another process can
      never produce negative counts (5/5 red on a HEAD clone) (I-A4).
      Review found `limpet ui` publishing the same lie at src/ui.rs:216;
      closed by dropping the key (ui.html never read it, confirmed by grep)
      with a test that spawns the real `ui` arm and asserts no session block
- [x] T5 measurement (no code): ten real recalls against the live 118-entry
      store, judged for usefulness, recorded below. The audit's own finding was
      that every quality number to date is synthetic (bench/fixture-repo is a
      self-graded exam); this is the first honest read on real data
- [x] T6 QA (2026-08-25, all receipts from real runs on the fixed tree):
      release build exit 0, test 382 passed / 0 failed across 16 targets,
      clippy --all-targets 0 warnings, panic ratchet ok, bench true-exit 0 at
      4.2x overall / 5.4x lineage, demo exit 0, cargo audit exit 0 (1226
      advisories loaded), em-dash sweep 0 over authored tracked files with
      the named `.limpet/memory.jsonl` exclusion at 5 (I-A7). Whole-branch
      adversarial review: five dimensions + per-finding refutation, 11
      confirmed findings all fixed (round record below). Two-process dogfood
      on the release binary: index in one serve process, a second serve then
      proved the period-terminated AWS key refused end-to-end, prose stored,
      recall + honest session block, reverify bogus-id refusal, consolidate
      reachable, and the stats CLI publishing lifetime with no session key;
      11/11 checks green
- [ ] T7 Ship: version sync, PR, merge on green CI, /deploy-limpet 0.16.1

## T6 whole-branch review round (2026-08-25)

Five-dimension adversarial review (secrets, ledger-session, ci-toolchain,
docs-truth, seams), every finding independently re-verified by a refuting
agent against the working tree. 14 findings raised, 3 refuted with receipts,
11 confirmed (10 distinct; the quadratic surfaced twice). All confirmed
findings fixed in this round:

- CRITICAL `classify_candidate` was O(n^2) on `prefix + alnum-run +
  trailing -/_ run` (measured 4x per doubling; ~627 ms per 64 KB detect;
  ~31 s for 50 import lines; evidence/reverify paths uncapped). Fixed with
  `CONTRACTION_BUDGET = 64`: the trim and emphasis passes already take
  maximal runs in one step, so real decoration resolves in a handful of
  contractions and the loop is now O(budget * n). Perf test pins the timed
  shapes; a positive test pins that decorated credentials still classify.
  Import now runs its O(1) size gate before the O(n) secret scan.
- SPEC prose described a session-base-stamping stats arm that was never
  shipped; rewritten to the counted model (I-A4 wording, SURFACES, T4).
- I-A7 read 0 only under a silent exclusion: `.limpet/memory.jsonl` carries
  5 em-dash lines in exported memory bodies. The invariant now sanctions
  exactly that one exclusion by name, receipts must report it.
- `src/mcp.rs` boot comment still described the removed baseline-snapshot
  model; rewritten.
- CI msrv job hard-pinned 1.86.0, one-directional vs `rust-version`;
  toolchain now read from Cargo.toml, drift caught both directions.
- `docs_in_sync` guards strengthened: the schema op enum is now asserted
  equal to the `tool_admin` dispatch arms (scraped from source), and the
  README op guard reads the `admin` row's own words (mutation-tested; the
  old bare `contains` let six of eleven ops survive deletion).
- skill.md and README stated the doctor server-image advisory
  unconditionally; both now name it macOS/Linux (`#[cfg(unix)]`, needs
  `ps`).
- `.cargo/audit.toml` and `tests/ledger_session.rs` were untracked while
  ci.yml and CONTRIBUTING.md referenced them; added to the commit.

Owner tap, not code: no branch protection on main, so the new CI jobs are
advisory at the merge boundary. One command makes them required:
`gh api -X PUT repos/KSym04/limpet/branches/main/protection` with
required_status_checks contexts `test (ubuntu-latest)`, `test
(macos-latest)`, `test (windows-latest)`, `audit`, `msrv` (session
permissions blocked repo-settings writes; deliberate).

## T5 result: the first non-synthetic quality number

Ten questions were written and timestamped BEFORE any memory body was read,
then run twice through `limpet serve` over stdio against the live 118-entry
store (64 live, 35 active / 29 stale at run time). Result is bimodal, and the
variable is one argument:

| Run | Hit (yes) | Usable (yes+partial) | Precision | Noise | Misleading |
|---|---|---|---|---|---|
| Cold, no `working_set` | 40% | 90% | 17/54 = 31.5% | 64.8% | 2/54 = 3.7% |
| With `working_set` | 90% | 100% | 33/51 = 64.7% | 29.4% | 3/51 = 5.9% (strict) |

Mechanism, read off `src/memory/recall.rs:252-270`: score = 0.45 text + 0.25
proximity + 0.20 confidence + 0.10 recency, plus kind and source bonuses. With
no working_set, proximity is identically 0 for every candidate, deleting a
quarter of the signal. FTS matched 61 of 64 live entries for essentially every
question, so retrieval discriminates almost nothing and the scorer does all the
work. Cold, six high-confidence verified facts filled 36 of 54 slots (67%)
regardless of topic, and 12 of 54 returned items carried no significant task
term at all.

The cold misses were RANKING failures, not corpus gaps: the literal answers to
Q4 and Q7 were ACTIVE in the store and absent from the pack, and both are
`kind=insight, source=explicit`, the combination the bonuses penalize by 0.20
against `fact + verified`. Both came back at rank 3 and rank 1 with a
working_set.

Staleness earned its keep: 0 stale items were returned without their flag, 9 of
11 distinct stale bodies audited against source are still TRUE, and the 2 that
had rotted into falsehood (the ROADMAP receipt-set memory and the pre-0.16
refinement-gap audit) both surfaced flagged, for the right mechanical reason.

Honest reading: in the mode an agent actually works in (files open,
working_set passed) recall is genuinely useful, and this is the first quality
number this project has that is not self-graded. The cold path is weak, and the
published 4.33x bench does not measure any of this.

Acted on here: `src/skill.md` now makes `working_set` explicit on both recall
steps. NOT acted on, recorded as a bet: rebalancing the kind and source bonuses
so explicit insights stop being buried is a scoring change and needs the bench
plus `recall_eval` as its gate, not a patch release.

## Deferred to v1.0 (found by this audit, NOT built here)

| Gap | Evidence | Why deferred |
|---|---|---|
| version_guard ignores schema | `src/store.rs:615` reads `code_version` only; migrations run first at `:443-451` and stamp SCHEMA_VERSION unconditionally, so an older binary opening a newer store rewrites the stamp downward | Behaviour change to the open path; belongs with the frozen-schema contract |
| JSONL export carries no format version | `src/store.rs:663-748` writes no version key; import (`:787-790`) reads only id + updated_at | Wire-format change; must land WITH the freeze, not before it |
| `rework-avoided` receipt does not exist | Named `ROADMAP.md:5`, asserted `README.md:423`, absent from `Ledger` (`src/store.rs:1173-1179`) and `ledger_payload` | 0.16.1 removes the false claim; building the counter is a feature |
| Migration chain hand-duplicated | `src/store.rs:444-451` vs `:460-467`, no test asserts they agree | Needs a shared migration list plus a test; touches every migration |
| `src/ui.rs` has zero tests | 390 lines, written security posture, no test in src/ or tests/ references `ui::` | The v1.0 item already promises a choke-point security review |
| verify_queue shows only `source='verified'` | `src/tools.rs:514`; 28 of 29 stale entries invisible | NOT a bug: an unverified entry has no `evidence_cmd` to hand out, so the queue is the re-provable set by design. The real gap is a stale-listing surface. Owner decision, recorded not built |

---

# SPEC: the refinement loop, v0.16.0

Status: SHIPPED 2026-08-15 (PR #28 merged on green 3-platform CI, tag
v0.16.0; GitHub release 12 assets, crates.io 0.16.0, MCP registry isLatest
all verified same day; local binary self-updated). Personal-tool phase:
gates unchanged, adoption pressure dropped by owner decision.

Closes the re-verification half of refinement: a flagged memory gets a
first-class path back to trusted. Must land before v1.0 because reverify
changes the tool API and the API freezes at 1.0.

## INVARIANTS

- I-R1: reverify never guesses. An anchor that cannot re-resolve against the
  CURRENT index refuses the whole op naming the anchor, and so does a slot
  holding more than one distinct body or resolving in more than one file
  (which body the evidence proves is unknowable); no half-reverified entry
  exists. A slot found whole in exactly ONE other file follows the rename
  and repairs anchors.file.
- I-R2: confidence refund is decay-once-per-reason. The pre-stale value is
  stored on the active->stale transition ONLY, HEAL restores exactly it, and
  it clears after either consumer. Reverify is a fresh proof, not a heal: it
  pays max(stored refund, the verified earn 0.95). A stale entry re-staling
  never re-stores the penalized value. The refund is a future confidence, so
  import caps it per source exactly like confidence itself. Every
  confidence write stays ROUND(...,6) (CONF).
- I-R1b: reverify applies remember's whole evidence policy (secret scan on
  command AND output, non-empty both, output digested never stored), and an
  entry of ANY source gains verified through fresh evidence, remember's own
  rule, stated in the tool description.
- I-R3: any state change replicating through LWW bumps updated_at strictly
  (bump_updated_at), and the new column travels the JSONL wire additively
  (omitted when NULL; old binaries ignore it; import clamps to [0,1]).
- I-R4: consolidation LISTS, never writes. The distilled entry and its
  supersedes links go through the existing guarded write paths.
- I-R5: doctor advisories never flip the ok flag (0.10 contract); the
  stale-image check is best-effort, silent on any parse failure, and
  read-only surfaces stay guard-free.
- I-R6: schema v8 is additive (conf_before_stale REAL), pragma self-gated,
  true no-op on re-run, no refill (no derived data changed).
- Carried: I3, BENCH >= 4.0x, CONF, POS, hot-path panic ratchet.

## STATE / DATA MODEL

- entries.conf_before_stale REAL NULL (v8). Written on active->stale with
  the pre-penalty value; read+cleared on heal (resolve_all) and reverify.
- Export line: "conf_before_stale": <f64> omitted when NULL.
- No new tables. verify_queue query unchanged.

## SURFACES

- admin {op:"reverify", id, command, output}: entry must exist, not
  superseded/archived/invalidated; command+output non-empty; command
  secret-scanned (remember's evidence rules). Effects, one tx: every anchor
  re-resolves against the current index (symbol anchors re-read their slot
  hash, file anchors the file hash; any failure refuses, I-R1);
  evidence_cmd/digest/ran_at re-stamped; source='verified';
  confidence = the verified earn (0.95) or the stored refund when higher,
  quantized (a fresh proof is never worth less than a new verified fact;
  found by the dogfood draining a pre-v8 item to the 0.5 floor);
  conf_before_stale=NULL; status='active'; stale_reason=NULL;
  bump_updated_at. Returns {id, anchors_rebound, confidence}.
- admin {op:"consolidate"}: read-only candidate clusters. Group
  non-superseded, non-archived entries by anchored (file, symbol_fqn) with
  >=2 members; pairwise token jaccard >= 0.5 forms a cluster; caps: 10
  clusters, 6 members each, disclosed via truncated flag. Returns
  {anchor, ids, kinds, previews (120ch), mean_overlap}.
- doctor: advisory "server images" check (unix only): `ps -axo
  pid=,etime=,command=` rows containing `limpet serve`, elapsed parsed from
  etime; a process older than the installed binary's mtime is a stale image
  -> note listing PIDs + restart hint. ok/warn/note only.

## ATTACK SURFACE / HAZARDS

- Forged reverify: output is digested, never trusted as proof of execution;
  same trust model as remember's evidence. Secret scan on command; oversize
  output refused (MAX_BODY_BYTES).
- Reverify on a twin: anchors re-read THEIR slot (fqn+disamb exact), never
  a wildcard; a vanished slot refuses (I-R1).
- Refund poisoning via import: conf_before_stale clamped to [0,1] and
  quantized on import; a 1e300 cannot park a future refund above cap.
- Consolidation spam: caps + read-only.
- Penalty CASE ordering: conf_before_stale must be SET in the same UPDATE
  that penalizes, reading the pre-update confidence (SQLite reads old row
  values within one UPDATE), and only WHEN status='active'.

## Task Implementation Checklist: 0.16.0

- [x] T2 store.rs: schema v8 (entries.conf_before_stale ALTER, self-gate,
      SCHEMA_V1 DDL, SCHEMA_VERSION 7->8, version tests, reopen no-op test);
      export/import carry the field (clamp+quantize on import)
- [x] T2 anchor.rs resolve_all: stale transition stores pre-penalty value;
      heal transition refunds + clears; branch-switch round-trip test
      proving confidence-neutral; re-stale-while-stale keeps stored value
- [x] T1 tools.rs + memory: admin reverify op per SURFACES + tool schema +
      README; tests: drain-own-queue shape (stale verified fact reverifies
      to active with refunded confidence + new digest), refusal on
      unresolvable anchor, refusal on superseded/archived/invalidated,
      secret in command refused, LWW bump proven
- [x] T3 tools.rs: admin consolidate op + tests (cluster found on
      same-anchor high-overlap episodes; unrelated bodies excluded; caps)
- [x] T4 main.rs doctor: stale server-image advisory + fixture-free unit
      test for the etime parser; never flips ok
- [x] Docs: README (verify_queue -> reverify loop, consolidate, doctor
      note), ROADMAP (0.16 -> Shipped), tool schema text, docs_in_sync
- [x] QA (2026-08-15): 15/15 suites (13 refinement tests), clippy 0 (known
      MSRV lint only), ratchet ok, bench 4.2x/5.4x, demo exit 0, em-dash 0.
      Two-process v7->v8 dogfood on a live-store copy: schema 8, statuses
      unchanged, REAL queue item drained over stdio (mcp-publisher validate
      re-run, entry active at 0.95, queue 3->2), consolidate clean.
      Whole-branch adversarial review (25 agents): 9 confirmed findings = 5
      distinct defects, ALL FIXED same day: per-source cap on imported
      refunds (laundering closed), multi-body slot + multi-home refusals in
      the reverify rebind (plus single-home rename follow), LWW refund-wash
      preservation, consolidate twin self-pair dedupe + whole-cluster
      mean_overlap + group caps + anchors_elsewhere, in-tx status guard,
      output secret scan, doctor wording, docs_in_sync coverage. 1 refuted
      (git-mv dead-end: fqn embeds the path, so the scenario cannot occur).
- [x] Ship: 0.16.0 synced (Cargo.toml + server.json + Cargo.lock), PR #28
      merged on green CI, tag v0.16.0 pushed 2026-08-15; release workflow 12
      assets, crates.io 0.16.0, MCP registry isLatest True, local binary
      self-updated, all verified

---

# SPEC: freshness at scale, part 2, v0.15.0

Status: SHIPPED 2026-08-13 (PR #27 merged, tag v0.15.0; GitHub release 12
assets, crates.io 0.15.0, MCP registry isLatest all verified same day; local
binary updated). Full design:
docs/superpowers/specs/2026-07-21-freshness-scale-2-design.md

FQN disambiguation via an additive `disamb` discriminator (schema v7:
symbols.disamb + anchors.disamb, NULL-safe, no UNIQUE) + a slot-first anchor
resolution ladder that closes the twin-masking false-Fresh hole; NINE
sanctioned FQN spelling changes (S1-S9 below); the FS-event-watcher lag bench
and verdict; P5 matched-terms behind the 4.0x bench gate; removed-file purge
transactionality.

Key invariants: I-F1 (no twin masking), I-F2 (legacy anchors byte-exact),
I-F3 (spelling byte-identical outside S1-S9, kind outside K1-K3, no symbol
lost, runtime-proven), I-F4 (v7 no-op re-run, race-safe), I-F5 (recall wire
untouched by disamb; P5 gated), I-F6 (entropy guards unweakened), I-F7 (set
semantics only) + carried I3/BENCH/CONF/POS.

I-F2 amendment (final review round, deliberate): a legacy NULL-disamb anchor
whose fqn is still indexed IN ITS OWN FILE while its body is not reads
Stale{body_edited} BEFORE the scope-respell step. The state is point-in-time
ambiguous (in-place edit with a preserved copy vs a twin-split respell), and
the ladder picks the honest failure: a wrong follow serves an edited body as
fresh forever, a stale is visible and recoverable. Pinned in
tests/anchor_golden.rs (a_surviving_fqn... and an_in_place_edit...).

## Task Implementation Checklist: 0.15.0

- [x] T1 store.rs: schema v7 (symbols.disamb + anchors.disamb ALTERs,
      pragma self-gate, duplicate-column race tolerated, mtimes zeroed once,
      SCHEMA_V1 fresh DDL updated, SCHEMA_VERSION 6->7, 4 hardcoded version
      tests updated, reopen-idempotence test)
- [x] T2 extract.rs: SymbolFact.disamb + push_sym signature; per-grammar
      disamb (Rust impl trait, Go receiver, C++/Java/C# param text); scope
      fixes (Rust mod_item, PHP bracketed namespace, C++ qualified
      out-of-line); fixtures per change verified via to_sexp; I-F3 runtime
      proof across all 11 grammars
- [x] T2b extract.rs: close every REMAINING shape that filed two symbols in
      one slot, adversarial-review follow-up. Scope fixes: C# block
      `namespace_declaration` (file-scoped form deliberately untouched), TS
      `internal_module`, Java `enum_declaration`/`record_declaration`, PHP
      `enum_declaration`. New discriminators: Ruby receiver (`self.` /
      `<<self.` / `#`), Python + JS/TS parameter list (property/setter and
      get/set pairs), C# `type_parameters` (generic arity), C++ enclosing
      `template_declaration` parameters and `template_function` arguments.
      Totality: no NULL slot in a space where a twin can carry one (Rust
      inherent `impl`, Go package `func`, Ruby instance `#`), because
      anchors.disamb NULL also means "legacy" and took the 0.14 ladder.
      Recovery: C++ `reference_declarator`/`parenthesized_declarator` have NO
      declarator field, so every reference-returning definition extracted
      nothing at all. `caller_fqn` now walks back to the innermost SYMBOL
      frame so a module-level call keeps the `<file>` sentinel instead of
      naming a scope with no row. Corpus scan (10,286 files / 399k symbols /
      3.06M calls): 0 symbols lost, 87 strictly new, 0 phantom callers -- but
      that corpus was C and C++ only, so it proved NOTHING about Java, C#,
      TypeScript, PHP, Ruby or Python. I-F3 re-proven properly under T2d.
- [x] T2d I-F3 restated and re-proven with the COMPLETE sanctioned list
      (S1-S9, K1-K3, R1-R2) over a 43-fixture, 11-grammar differential run
      against the 2ab0afb baseline from one byte-identical harness. 45
      spelling changes, all attributable, 0 unattributed, 0 rows lost
      (re-measured 2026-08-04 with S5 widened to identifier-named `module`
      nodes; delta vs the 2026-07-25 run = the three Beta.* rows only). New
      permanent pins in tests/index_langs.rs for every newly sanctioned scope
      plus the negative bounds (C# file-scoped namespace, TS string-named
      ambient module, Rust `mod x;`, PHP unbracketed namespace, Ruby
      `class << self`, C++ `::f` and anonymous namespace).
- [x] T2c anchor.rs: the scope-respell step (step 4 of the shipped 6-step
      ladder). An FQN whose body is still in its file under the same last
      segment under an EXTENDED scope chain is a respelled SCOPE, not a moved
      symbol: follow it (gated for legacy NULL anchors by the same-file
      fqn-survival stale, see the I-F2 amendment above). Without it every scope
      fix above false-stales the anchors already pointing at those FQNs
      (measured 16.3% permanent: low_entropy under the follow floor,
      ambiguous_anchor on any duplicated body) for zero code change.
      store.rs: one-time `v7_content_refill` marker so a store that reached v7
      under an earlier build still gets its single reindex.
- [x] T3 index/mod.rs: persist disamb into symbols insert; purge tx fix (G)
- [x] T4 memory/mod.rs + tools.rs: resolve_anchor DISTINCT (fqn, disamb) +
      @disamb spec parsing (last-@ split, verbatim retry) + anchors INSERT
      disamb + tool description + README
- [x] T5 anchor.rs: slot-first ladder (6 steps: slot-fresh, slot-edited,
      slot-respell, scope-respell, fqn-survives stale, hunt) + legacy NULL
      path + opportunistic backfill + follow rewrites disamb + twin-masking
      test + trait-rename follow test + golden additions
- [x] T6 bench/lag_bench.py: synthetic repos 2k/10k/50k, walk/stat/reindex
      cost separation, staleness latency in calls, drain; run on the release
      binary 2026-08-07. VERDICT: anchored staleness latency 1 sweep call at
      every size and edit batch (K1/K32/K100), integrity 0 findings; quiet
      per-call p50 156/372/1206 ms at 2k/10k/50k vs the 250 ms bar, so the
      FS-event watcher becomes a designed backlog item gated on this bench.
      Recorded in ROADMAP (0.15 Shipped entry + bets table) + README
      receipts section.
- [x] T7 P5 matched terms: per-item `matched` string (task terms found in the
      body, cap 3, task order, omitted when empty), priced into both the
      packer and recall_cost (ITEM_OVERHEAD parity). GATE HELD on the rebuilt
      release binary: 4.2x overall / 5.4x lineage (a first 4.1x reading was a
      stale-binary artifact and was discarded); emission proven on the real
      serve path via stdio JSON-RPC ("matched":"sweep prioritization
      anchored"). 3 unit + 1 wire test.
- [x] T8 docs: README (disamb + @spec whitespace-forgiven wording, matched
      semantics, lag-bench receipt in the receipts section), ROADMAP (0.15 ->
      Shipped with the measured watcher verdict + FS-event watcher added to
      the bets table with its bench gate); main.rs untouched
- [x] T9 QA (final run 2026-08-13, post review-round fixes): 14/14 suites
      green (cargo test --locked exit 0), clippy 0 (known MSRV map_or lint
      only), panic ratchet ok, bench 4.2x overall / 5.4x lineage on the
      rebuilt release binary, demo exit 0, em-dash sweep 0 files.
      Two-process dogfoods on copies of the REAL store: v6 -> v7 (72 real
      entries, statuses unchanged, refill once, serve recall emitted matched
      terms on the wire, reopen no-op) AND refill generation 1 -> 2 (109
      entries, 29 stale before == 29 stale after, marker 1 -> 2, reopen
      no-op). Adversarial reviews: task-level rounds during the branch plus
      TWO whole-branch workflows (2026-08-04; 2026-08-07 24-agent round, 8
      confirmed findings all fixed, section above).
- [x] Ship: version 0.15.0 synced (Cargo.toml + server.json + Cargo.lock),
      PR #27 merged on green 3-platform CI, tag v0.15.0 pushed 2026-08-13;
      release workflow 12 assets, crates.io 0.15.0, MCP registry isLatest
      True, local binary self-updated, all verified

FINAL REVIEW ROUND (2026-08-07, 24-agent adversarial workflow: 19 raw
findings, 8 confirmed after two-lens verification, 1 refuted, 10 minor; ALL
confirmed + actionable minors FIXED):
- [x] BLOCKER anchor.rs: respell step converted an in-place edit of a legacy
      NULL-disamb anchor's symbol into Followed when the old body survived
      in-file under a deeper scope. Fixed: same-file fqn-survival stale gate
      (I-F2 amendment above) + flipped/added golden pins.
- [x] MAJOR store.rs: v7 refill marker frozen at its first claim; extractor
      changes after the claim never re-fired it. Fixed:
      CONTENT_REFILL_GENERATION (now 2) guarded upsert + generation test.
- [x] MAJOR extract.rs: JS/TS static/instance members of one name shared one
      slot. Fixed: `static` prefixes the discriminator + pin.
- [x] MAJOR extract.rs: Java enum-constant bodies filed constant overrides
      into the enum's slot. Fixed: constant-name marker flows down + pin.
      Java anonymous-class members got a `new <Type>.` marker + pin; JS
      named-object-literal methods got a declarator marker + pin.
- [x] MAJOR extract.rs: nested symbols with their own parameter list
      DISCARDED the inherited twin discriminator. Fixed: compose_disamb
      (inherited prefixes own, all param-list grammars) + pin.
- [x] MAJOR store.rs import: NULL disamb re-resolved as a wildcard and could
      adopt the twin's hash. Fixed: `disamb IS ?2` exact, twin-free-only
      adoption for slot-less anchors + deliberate-NULL round-trip test.
- [x] MAJOR store.rs import: an old-binary peer's re-export stripped local
      anchor slots via LWW replace. Fixed: preserved_slots carry-over when
      the incoming anchor names the same (file, fqn) slot-lessly + test.
- [x] MAJOR docs: "@disamb spelled as the source writes it" was false for
      whitespace. Fixed both ways: the spec parser canonicalizes the suffix
      through collapse_ws, and README/tool docs say whitespace is forgiven.
- [x] minors: sig_text for Go receiver + Rust trait-path discriminators
      (formatting-only edits no longer churn slots); HashSet dedup in
      significant_terms; @-named bash symbol colliding with a slot spec is
      refused loudly; K1-K2 -> K1-K3 in two SPEC lines; ladder step count
      corrected; matched-absence wording fixed in README + recall tool
      description.
- Refuted (1): "is_scope_extension vacuous for pre-v7 spellings"; the
  mechanical trace was right but the blocker fix removes the reachable harm.

Deferred out of 0.15 (whole-branch reviews 2026-08-04 + 2026-08-07,
recorded not fixed):
- C# type-level generic arity: `class C<T>` / `class C<T,U>` share one NULL
  slot (methods got `type_parameters`, type rows did not). 0.16 candidate.
- C++ `union_specifier` is neither a symbol arm nor a type scope, so union
  member functions extract file-scoped. Pre-existing, not a 0.15 regression.
- map/lineage reads are disamb-blind: twin slots sharing an FQN merge into
  one lineage target undisclosed. 0.16 candidate alongside the refinement
  loop's map work.
- Two SAME-type Java anonymous classes with identical bodies in one scope
  share one slot (the `new <Type>.` marker cannot separate them); inline
  (never-bound) JS object literals keep no declarator marker. Both
  pre-existing twin spaces narrowed, not closed, by this round.
- JS `get x()` beside a plain method `x()` (legal parse, degenerate at
  runtime) share `()`. Not worth a discriminator.
- T2b's "totality" claim is therefore scoped: every twin axis with a STABLE
  spellable discriminator is closed; the residuals above are the measured
  remainder.

---

# SPEC: Truth-Layer (Slice A), v0.14.0

Status: SHIPPED 2026-07-17 (tag v0.14.0; crates.io, MCP registry, GitHub release
12 assets all verified 2026-07-21). Source: `IMPROVEMENTS-TRUTH-LAYER.md` (P0 + P1),
triaged against real code + limpet memory. Ships **0.14.0** (feature = minor). Collides
with roadmap 0.15 write-back (P1 == "anchor-collision surfacing at write"), so roadmap is
reconciled in this drop. Adoption (demo/seed) and hardening (panic audit) are OUT of
scope; separate later drops (Slice B, Slice C).

Doctrine (limpet honesty scars): **flag and propose, never silently delete or merge.**

## INVARIANTS (must not regress)

| ID | Invariant | Source |
|----|-----------|--------|
| I3 | Nothing flagged (stale/contradicted/unverified) is ever score-hidden by the relevance floor. | mem 01KWM94SDB |
| BENCH | `bench/token_savings.py` overall ratio stays >= 4.0x. Per-item token additions are the known killer (ledger died at 3.8x). | mem 01KX05ESTY |
| CONF | Every confidence write passes through `quantize_confidence` (6-dp). | mem 01KWPA1G5S |
| HONEST | verified > unverified on TIES; a far-more-relevant unverified memory still ranks (text_score dominates). | P0 acceptance |
| POS | Roadmap/README contrast mechanisms, never competitor names. | mem 01KXABHGYP |
| I-F3 | No symbol the 0.14 extractor produced is lost, and FQN spelling (`parents` + `name`) is byte-identical pre/post outside the nine sanctioned changes S1-S9; `kind` is byte-identical outside K1-K3. Runtime-proven across all 11 grammars against the pre-branch baseline, never asserted from node-types.json. A tenth spelling change, a fourth kind rule, or one lost symbol is a breach. | 0.15 T2/T2b |

### I-F3: the complete sanctioned set (measured, not collected)

I-F3 originally named THREE scope fixes. The adversarial-review round added
five more scopes and one naming change, so the three-item wording was false.
This is the measured list. Every row names the grammar and the vendored-grammar
node kind; every row with a sibling form that must NOT take the change names it,
because the cheapest way to breach I-F3 is for an arm to grow one node kind
wider. Full rationale and the measurement of record:
docs/superpowers/specs/2026-07-21-freshness-scale-2-design.md.

| # | Grammar | Node kind | Spelling change | Negative bound |
|---|---------|-----------|-----------------|----------------|
| S1 | Rust | `mod_item` with a `body` | one FQN segment, no symbol row | `mod x;` pushes nothing |
| S2 | PHP | `namespace_definition` with a `body` | one segment, no symbol row | `namespace A;` pushes nothing |
| S3 | C++ | `function_definition` reaching `qualified_identifier` | every `scope` segment becomes a parent, raw source text | `::f` has no `scope` field; anonymous `namespace` has no name |
| S4 | C# | `namespace_declaration` with a `body` | the dotted name as ONE segment | `file_scoped_namespace_declaration` pushes nothing |
| S5 | TS | `internal_module` or identifier-named `module` with a `body` | one segment, no symbol row | string-named `module "x" { }` never scopes: it carries a `body` too, so the `name` kind (`string`), not body presence, is the bound; neither node kind exists in the JS grammar |
| S6 | Java | `enum_declaration` | `class` row + scopes members | -- |
| S7 | Java | `record_declaration` | `class` row + scopes members | -- |
| S8 | PHP | `enum_declaration` | `class` row + scopes members | -- |
| S9 | C++ | `template_function` under `qualified_identifier` | name is the template's `name` (`f`), not the template id (`f<int>`); arguments move to `disamb` | -- |

`kind` moves only under K1-K3, all from `parents.is_empty()` becoming
`fn_kind(tdepth)` where `tdepth` counts TYPE scopes only; K3 names the one
place a segment COUNTS as a type scope:

| # | Rule | Effect |
|---|------|--------|
| K1 | module/namespace frame is not a type scope | a free function inside a C++ `namespace` (or Rust `mod`, PHP/C# namespace, TS `internal_module`) is `function`, not `method` |
| K2 | enclosing FUNCTION frame is not a type scope | a nested `def`/`fn` is `function`, not `method` |
| K3 | C++ qualifier segments ARE type scopes | an out-of-line qualified member definition (`void A::run()`) labels `method`, matching its in-class spelling |

Two C++ recoveries ADD rows without respelling any existing one (so they cannot
breach I-F3, but they shift later ordinals in their file):
R1 `reference_declarator`/`parenthesized_declarator` (fieldless wrappers,
previously extracted nothing), R2 unqualified `template_function` (ditto).

Measured over a 43-fixture corpus covering every `parents.push` arm in BOTH
trees (re-measured 2026-08-04 after S5 widened to identifier-named `module`
nodes; the widening added exactly the three `Beta.*` rows, nothing else moved):
218 baseline symbols, 231 branch symbols, 45 spelling changes ALL attributable,
0 unattributed, 7 kind-only changes, 13 rows added, 0 rows lost; JS, Go and
Bash byte-identical. Pins live in `tests/index_langs.rs` under the
`I-F3 sanctioned-scope pins` banner.

## ATTACK SURFACE / HAZARDS

- **P0.b bench death.** A `verified:` field on EVERY item = the ledger mistake (reverted
  at 3.8x). MITIGATION: emit a single `unverified` flag ONLY on explicit-source items
  (flags array already omitted-when-empty, survives compaction, only on the untrusted
  subset). verified already emits `source:"verified"`, mined emits `source:"mined"`
  (tools.rs:112 emits source when != explicit); the ONLY marker gap is explicit. Close
  exactly that. Gate on BENCH before commit.
- **P0.a floor interaction.** New source term is additive; must not push a flagged item
  below the floor. I3 exempts flagged items; verify the exemption still fires.
- **P0.c over-derivation.** Full derivation (corroboration/staleness) is roadmap-0.15
  territory. Scope here = a hard CAP only.
- **P1 false conflicts.** Duplicate-vs-conflict split on a similarity ratio; too low a
  threshold spams. Tunable, documented.

## CORE ARCHITECTURE

- **P0.a** `src/memory/recall.rs` (~6 lines): after the kind boost (recall.rs:206), add
  `source` term: `verified => 0.10`, `mined => -0.05`, `_ => -0.05`. `source` already
  destructured (:159), unused in score today. Verify I3 exemption intact.
- **P0.b** `src/tools.rs` (~4 lines): in `tool_recall` (:112), when `i.source ==
  "explicit"`, push `"unverified"` into that item's `flags`. NO per-item field.
- **P0.c** `src/memory/mod.rs` (1 line): line 34 `_ => requested.unwrap_or(0.8)` →
  `.min(EXPLICIT_CONF_CAP)` with `EXPLICIT_CONF_CAP = 0.85`. Already quantized at :231.
- **P1** `src/memory/mod.rs` + `src/tools.rs`: split `possible_duplicates` (mod.rs:278)
  by body similarity: near-identical = duplicate, high-overlap-divergent = conflict. Add
  `possible_conflicts: Vec<Value>` to `RememberResult` (:67), each `{id, body, hint}`. NO
  auto-supersede/link. Surface only.
- **TEST** `tests/recall_quality.rs`: land drop-in + add verified-wins case (P0.a) +
  conflict-surfaced case (P1).
- **ROADMAP** `ROADMAP.md`/`README.md`: reconcile numbering (Ken picks), honor POS.

## DESIGN CHANGE (2026-07-17, bench-driven)

P0.b was specced as a per-item `unverified` flag. MEASURED: it dropped
`bench/token_savings.py` 4.0x → 3.8x (served 3386 → 3583, +197 tok), the exact
ledger failure (mem 01KX05ESTY). Root cause: you cannot pay a per-item marker on
the COMMON source type (explicit is the default). Pivot: mark the EXCEPTION, not
the default. verified/mined already self-identify via `source` (cheap, rare); a
MISSING `source` = unverified, documented in the recall tool description. Net bench
after pivot: **4.1x** (P0.a ranking floored a marginal explicit item, served 3325).

## SCOPE (Ken, 2026-07-17): ALL of A+B+C in 0.14, no phasing

Personal tool, can't wait for adoption. Honesty caveat held: the 129-site panic
audit (C item 5) lands incrementally behind the ratchet; everything else ships in
the 0.14 release.

## TASK IMPLEMENTATION CHECKLIST

Slice A (truth layer):
- [x] P0.a: source ranking term in recall.rs (verified +0.10, mined/explicit -0.05)
- [x] P0.b: provenance convention (verified self-IDs via source; missing source =
      unverified) + documented in recall tool description. NO per-item marker (bench).
- [x] P0.c: `EXPLICIT_CONF_CAP = 0.85` cap in default_confidence
- [x] `tests/recall_quality.rs` landed + compiles + 5 green (incl. col9/col10 guard)
- [x] `bench/token_savings.py` = 4.1x (>= 4.0 gate) after pivot
- [x] P1: `possible_conflicts` + value-divergence classifier + surface in tool_remember
      + docs + 2 tests (divergence->conflict, restatement->not)
- [x] P2a: ALREADY BUILT, envelope.rs:42 emits dirty count + 10-file sample cap
- [x] P2b: widened `discover()` hard-skip (Python/JS/iOS/etc generated trees) + test +
      README updated
- [x] P3: pre-insert near-dup refuse (jaccard>=0.9 + same numbers + same negation;
      corrections/conflicts exempt; blocking a correction would freeze col9/col10) +
      `force` tool param + 3 TDD tests + 42 call sites migrated. Gates re-verified:
      13/13 suites, clippy 0, ratchet ok, bench 4.1x all answers intact, demo exit 0.
- [x] P4 archival DONE (2026-07-17): 5/5 TDD tests green first pass (hide/restore
      with truthful status, export/import round trip, verify_queue exclusion, loud
      nonsense refusals, forget cleanup + status count). Schema v6 additive sidecar;
      4 version-assertion tests updated 5->6; admin archive/restore ops + tool schema
      + README + ROADMAP + docs_in_sync. Design as below. Gates: 14/14 suites,
      clippy 0, bench 4.1x, demo exit 0.
      Original design note: NOT a status value. The
      entries.status CHECK would need a core-table rebuild to admit 'archived';
      instead a SIDECAR table `archived(entry_id PK, archived_at)` = additive
      CREATE TABLE IF NOT EXISTS (v3 precedent), SCHEMA_VERSION 5->6. Archival
      gates VISIBILITY ONLY: recall/verify_queue/map exclude flagged ids; sweep +
      resolve_all UNTOUCHED so status keeps tracking reality underneath; restore =
      delete sidecar row, original (current, truthful) status reappears for free.
      admin ops archive/restore; export carries `archived: true` (omitted when
      false); import re-applies the flag on added/updated lines; forget cleans the
      sidecar. Old binaries reading a new export simply ignore the field (entry
      imports visible; acceptable degradation). INVARIANT: archival is a USER
      action, not an honesty flag; I3 does not apply (like superseded).
- [x] P5: `matched` field per recalled item (query∩body significant tokens); BENCH RISK
      -> DEFERRED out of 0.14; shipped in 0.15 T7 with the 4.0x gate held at 4.2x

Slice B (adoption):
- [x] `src/demo.rs` drop-in + wire main.rs; `cargo run -- demo` exits 0 (verified)
- [x] `src/seed.rs` drop-in + `source:"mined"` per P0 note + wire main.rs; idempotent
      (seeded 3 -> 3 unchanged, verified)
- [x] `import --path` forwarding + HELP hint; repo-relative works, absolute correctly
      blocked by validate_rel_path security boundary (verified)

Slice C (hardening):
- [x] `scripts/check_hotpath_panics.sh` + baseline landed; passes at baseline (verified)
- [x] CI wiring (`.github/workflows/ci.yml`): panic check + `cargo run -- demo`
- [x] "129-site audit" was a PHANTOM: those counts were `#[cfg(test)]` unwraps. Fixed
      the ratchet to exclude test modules; true hot-path = 4 sites. Hardened the 2 in
      util.rs (clock, utf8); left tools.rs:415 + graph.rs:50 (invariant-proven).

QA (2026-07-17, 7-dimension adversarial workflow: 20 raw findings, 15 refuted, 5
confirmed, ALL FIXED):
- [x] MAJOR recall.rs: stale-verified outranked fresh-explicit (+0.10 boost survived
      evidence rot). Fixed: boost gated on status=="active" (I-A1 principle) + test.
- [x] MAJOR seed.rs: reworded note dead-ended in opaque "rejected" counter after P3.
      Fixed: separate refused_near_duplicate counter + printed fix hint + --force flag;
      live smoke: refused with hint -> --force seeds 1.
- [x] MINOR demo.rs: ScratchDir could silently reuse a crashed run's dir (flaky CI).
      Fixed: create_dir (fails-on-exists) + suffix retry, never create_dir_all.
- [x] MINOR README: seed "idempotently" overpromised post-P3. Fixed: no-op re-runs vs
      reworded-refusal + --force documented.
- [x] MINOR docs_in_sync: demo/seed missing from CLI guard. Fixed: added.

QA ROUND 2 (full workflow result, 49 agents: 21 raw, 14 confirmed after adversarial
verify, ALL FIXED):
- [x] MAJOR store.rs import_jsonl bypassed the truth layer: forged verified-without-
      evidence minted the +0.10 boost at conf 1.0; unknown source ABORTED whole import
      at schema CHECK. Fixed: SOURCES validation + verified-requires-evidence reject +
      per-source confidence caps (import_confidence_cap) + TDD test (5-line hostile
      export fixture).
- [x] MAJOR mod.rs dedup/conflict queries had no status filter: a SUPERSEDED twin
      refused correcting-the-correction and got no-op supersede hints. Fixed: status
      != 'superseded' on both queries + TDD test (A superseded by B, re-assert A).
- [x] MAJOR demo.rs store lived INSIDE fixture root; store_exclude_dir = grandparent
      = root, so step-1 index indexed NOTHING (passed via tool_remember's defensive
      reindex). Fixed: repo/ + store/ siblings under scratch + map assertion that
      scan_batch is actually in the symbol table.
- [x] MAJOR seed.rs --kind unvalidated (all chunks rejected, exit 0) + fenced code
      blocks chunked as prose. Fixed: up-front KINDS bail + fence state machine
      (``` and ~~~, unclosed swallows to EOF) + 3 in-module tests.
- [x] MINOR classifier trio: numbers() now extracts EMBEDDED digit runs (col9/col10,
      the motivating case, now surfaces as conflict + test); Unicode-aware tokens
      (CJK bodies no longer judged by ASCII scraps + test); bigram adjacency check
      (reversed same-vocabulary claim is a correction, not refused + test).
- [x] MINOR seed looks_like_path: +11 note/template extensions (.md .yml .toml .twig
      .scss .vue .sh .sql etc) + test.
- [x] MINOR baseline: +5 hot-path files pinned at 0 (mcp, config, secrets, fqn, lang).
- [x] MINOR main.rs doc header: ui/stats added, import --path, seed --force.
- [x] MINOR README pointer: 0.14 = "what this release carries", not "shipped".
- [x] MINOR demo write_fixture comment no longer claims a nonexistent mtime call.
- NOTE unverified (verifiers died on API overload): security flagged ScratchDir
      temp-path squatting; the create_dir-fails-on-exists fix already covers it.

WHOLE-BRANCH REVIEW (2026-07-17, 74 agents, gates GREEN, 21 confirmed findings, ALL
FIXED; the seam pass earned its keep again):
- [x] BLOCKER: remember never secret-scanned evidence.command (the one raw-persisted,
      git-exported evidence field). Fixed: same guard as body + empty refused + test.
- [x] BLOCKER: no .gitattributes; windows-latest autocrlf would CRLF the ratchet
      script ('set -euo pipefail\r') and redden CI on first push. Fixed: .gitattributes
      pinning *.sh + baseline to LF.
- [x] MAJOR: archival never propagated to already-synced peers (archive/restore did
      not bump updated_at; LWW skipped the line both directions, contradicting the
      code comment). Fixed: strictly-monotonic bump_updated_at (max(now, cur+1s)) in
      one tx + both-direction propagation test. GUARD COLLISION found by the test:
      the +1s stamp tripped import's future-timestamp poison guard; resolved with a
      bounded 3600s skew allowance (peers' clocks skew anyway; an hour cannot poison
      the merge durably; 9999-poison still rejected, test held).
- [x] MAJOR: import validated source but not kind/status/body/link-rel; one bad line
      aborted the WHOLE batch at the schema CHECK (incl. first-run bootstrap). Fixed:
      per-line rejects + links_dropped for bad rels + test.
- [x] MAJOR test gaps: cap now pinned DIRECTLY on the wire (conf <= 0.85); negation
      dimension of both classifiers pinned ('no timeout' correction stores + conflicts).
- [x] MINOR x14: forget wrapped in one tx; hook brief + statusline exclude archived
      (with pre-v6 read-only fallback); ratchet FAILs on missing baseline file; seed
      strict arg parser (flag values never eaten as the file, '=' form and unknown
      flags refused loudly, extra positionals refused, absolutized read-error, three
      doc surfaces identical); map/affected archival exclusion pinned by test; both
      new test suites moved store OUTSIDE repo root (the demo trap) + layout guard
      assert; README 'shipped' vs 'this release carries' honesty fix (round 2 QA).

Ship:
- [x] Roadmap/README reconciled: 0.14 = the truth layer (Ken's pick), freshness-at-
      scale-2 -> 0.15, refinement loop -> 0.16 (anchor-collision bullet marked pulled
      forward); README six-tools table + CLI table + seed paragraph + pointer updated
- [x] Final gates 2026-07-17 (post QA round 2): 13/13 suites (17 recall_quality/
      memory_api truth tests), clippy 0, ratchet ok (15 files pinned), bench 4.1x,
      lineage 5.4x, demo exit 0 + index self-verification, em-dash sweep clean
- [x] Whole-branch review (playbook item 14) before merge (74 agents, 21 confirmed
      findings, all fixed; section above)
- [x] Version 0.13.0 -> 0.14.0 via `/deploy-limpet` QA gate: PR #26 merged, tag
      v0.14.0 pushed, crates.io 0.14.0, MCP registry isLatest, GitHub release 12
      assets, local binary 0.14.0 (asset count re-listed and confirmed 2026-07-21)

---

# SPEC: freshness at scale, branch 1: sweep priority + low-entropy guard, v0.13.0

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

## Task Implementation Checklist: freshness branch 1

- [x] Calibration harness: print normalization-buffer lengths for trivial +
      real fixture bodies across all 11 grammars; pick both thresholds
      (BODY=124, FILE=34; max trivial 123 vs min real 265)
- [x] `ast_body_hashes`/`ast_body_hash_node` return (hash, len); all callers
- [x] schema v5: ALTER + table_info self-gate + refill + reopen/refill tests
- [x] `index_file_parsed` writes `body_len`
- [x] Sweep prioritization + budget-boundary test
- [x] Symbol-site guard + NULL grace + healing round-trip tests
- [x] File-site guard (`files.size`) + tests
- [x] Docs: `low_entropy` stale reason; README fate table + prose
- [x] Full suite (163) + clippy clean + bench 4.0x/5.4x + two-process dogfood
      (real v4 store migrated, inherits intact, anchored files refilled first,
      guard fired live stale:low_entropy and healed on restore)
- [ ] Whole-branch review -> PR -> merge -> /deploy-limpet 0.13.0 (Ken pre-authorized 2026-07-11)

---

# SPEC: grammar wave 2 (Go, Java, Ruby, C#, Bash), v0.12.0

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

# SPEC: lineage graph + live ledger + local event hook (design, next minors)

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

M1: lineage graph (v0.11.0):
- [ ] store: `inherits` table + indexes + schema bump 2->3 + migration test
- [ ] index: per-file `inherits` delete/reinsert at the 3 reindex sites
- [ ] extract.rs: inheritance capture for php/js/ts/py/rs/cpp (+ unit tests each)
- [ ] index/graph.rs: read-time resolver (0/1/N labeled) + bounded BFS lineage
- [ ] tools: `map` returns additive `lineage`; tool schema + README + docs_in_sync
- [ ] bench: fixture inheritance chain + lineage questions; ratio >= 4x sub-gate
- [ ] tests green; dogfood; NO RELEASE until Ken tests

M2: live ledger (v0.11.0):
- [ ] serve: per-session `SessionLedger`, reset at serve start
- [ ] recall envelope: additive `meta.ledger` {served,baseline,saved,reads_avoided,cumulative_saved,estimate}
- [ ] assert sink is meta_kv; memory.jsonl untouched; negative not floored
- [ ] tests green; dogfood

M3: local event hook (v0.10.0, gate ruling at kickoff):
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

# SPEC: cost_to_learn + authority-weighted recall (proposed, ~v0.9)

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

- `cost_to_learn TEXT`: coarse bucket, NOT a number (numbers invite
  inflation): one of `trivial` (default/unset), `hours`, `days`, `incident`.
  Set on `remember`; maps to a small fixed weight (0.0 / 0.3 / 0.6 / 1.0).
- `survived_changes INTEGER NOT NULL DEFAULT 0`: incremented in
  `resolve_all` each time an anchor is `Followed` (the code moved/renamed and
  the memory tracked it) or stayed `Fresh` across a sweep that reindexed its
  file. Earned, not settable by the caller. This is "survived N refactors".

Structural signals read at recall time (no new storage):

- `fan_in`: how central the anchored code is: callers of the anchored
  symbol from the `calls` table plus the count of OTHER memories anchored to
  the same file/symbol. High fan-in = the memory describes load-bearing code.
- `evidenced`: `source = 'verified'` (already exists): a lesson with a proof
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

# SPEC: security + Windows hardening (v0.7.3)

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
- **Bounded line reads** (1 MiB): no OOM from one giant line.
- **Confidence clamped** to [0,1]: an imported `1e300` can no longer pin a
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
