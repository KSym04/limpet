# SPEC — whole-repo indexing + honest anchors (v0.5.0)

Field-tested on a Bedrock/Timber WordPress theme (451 tracked files): only
123 files indexed, memories anchored to `.twig`/`.scss` files died silently,
`remember` reported phantom `anchored` counts, one dead anchor nuked whole
multi-anchor memories. This spec fixes all four plus two latent bugs found
during the audit.

## Core Architecture

```
discover(root)                       every file passing walk bounds (was: lang-detected only)
  bounds unchanged: .gitignore, .limpetignore, hidden, dir blocklist,
                    512KB cap, *.min.* skip
index_file(rel)
  lang detected   -> parse, symbols + imports + calls + files row (as today)
  lang unknown    -> files row only: lang=NULL, parse_ok=1, hash=sha256(bytes)[..16]
                     (binary-safe: raw bytes, no UTF-8 requirement)

remember(anchors)                    ALL validation before any write, one tx
  symbol anchor   -> must resolve in symbols table (unchanged) else hard error
  file anchor     -> must exist in files table else hard error naming the file
                     and whether it exists on disk (=> excluded by bounds)
                     stores files.hash into anchors.ast_body_hash

resolve_all: file-level anchor fate
  file row gone            -> Invalidated
  anchor hash NULL (legacy)-> backfill current hash, Fresh
  hash == files.hash       -> Fresh
  hash != files.hash       -> Stale{file_edited}

resolve_all: entry status aggregation (was: worst anchor wins)
  all anchors Invalidated       -> invalidated, 'anchor_deleted'
  some Invalidated, some alive  -> stale, 'anchor_lost'
  any Stale                     -> stale, <reason>
  else                          -> active
```

## State / Data Model

No schema change. Existing columns absorb everything:

| column                 | new use                                            |
|------------------------|----------------------------------------------------|
| `files.lang`           | NULL for non-parsed (file-level-only) files        |
| `files.hash`           | already sha256[..16]; now compared for file anchors|
| `anchors.ast_body_hash`| file-level anchors: content hash (was always NULL) |
| `entries.stale_reason` | new values: `file_edited`, `anchor_lost`           |

Legacy stores (v0.4.0) migrate lazily: NULL file-anchor hashes are
backfilled on first `resolve_all`; unindexed files appear on next sweep.

## INVARIANTS

- I-A: `remember` is atomic — either the entry and ALL its anchors persist, or
  nothing does. `anchored` in the result equals anchors written, always.
- I-B: an anchor never resolves against a file limpet has not indexed;
  `remember` refuses it loudly at write time instead.
- I-C: one dead anchor never invalidates an entry that still has a live
  anchor; entry dies only when every anchor dies.
- I-D: file-level anchors participate in staleness — editing the file flips
  attached memories to `stale:file_edited`, deleting it invalidates them.
- I-E: walk bounds (size cap, ignore files, dir blocklist, minified skip)
  survive unchanged — indexing all extensions must not reopen the
  WordPress-tree CPU-peg (see memory 01KWJZ51DV).

## ATTACK SURFACE

- Binary files (images, fonts) now walk into `index_file` -> hash raw bytes,
  never `read_to_string`; 512KB cap bounds cost.
- Giant text configs (`package-lock.json` < 512KB) -> file row only, no
  parse; harmless.
- Repo with no `.gitignore` -> unchanged risk profile; bounds are the
  defense, not extension filtering (extension filter never guarded the walk
  anyway — walk visited every file, filter only dropped them post-stat).
- Legacy entry invalidated by OLD all-or-nothing rule stays invalidated
  (status write is one-way for invalidated). Documented: re-`remember` or
  `admin index` after upgrade will not resurrect; user re-seeds.
- FTS duplicate surfacing on file anchors unchanged (same `anchors.file`).

## TECH STACK DEPS

- No new crates. rusqlite `unchecked_transaction` for atomic remember.
- tree-sitter grammars untouched; `lang::detect` still gates parsing only.

## Task Implementation Checklist

- [x] `src/index/mod.rs`: `discover` drops lang filter; `index_file` handles
      unknown-lang path (raw-byte hash, files row, purge symbol rows);
      hidden(false) so .github/.gitignore are anchorable; `.limpet` dir
      blocklisted so the export never indexes itself
- [x] `src/memory/mod.rs`: validate-then-write in one tx; file anchors
      require files row, store content hash; loud errors
- [x] `src/memory/anchor.rs`: file-anchor hash compare + legacy backfill;
      per-anchor aggregation (I-C)
- [x] `src/memory/recall.rs`: invalidated flag no longer hardcodes
      `anchor_deleted`; use stored stale_reason
- [x] tests: golden transitions for file anchors (edit -> stale,
      delete -> invalidated, legacy NULL hash -> fresh+backfill);
      multi-anchor partial-death -> stale not invalidated;
      remember atomicity (failed anchor leaves zero rows);
      discover picks up .twig/.scss/.md/unknown extensions and hidden paths
- [x] `README.md`: serve vs ui note; document .limpetignore + whole-repo
      indexing; `src/skill.md`: anchor-to-any-file guidance + recall-before-read
- [x] version 0.4.0 -> 0.5.0 (Cargo.toml, Cargo.lock, server.json)
- [x] `cargo test --locked` green (55 tests); bench gate holds at 4.1x;
      dogfood verified over MCP stdio: 47/47 anchorable tracked files indexed,
      file_edited/anchor_lost/anchor_deleted transitions all observed live
- [ ] Branch feat/whole-repo-index -> PR -> merge -> tag v0.5.0
