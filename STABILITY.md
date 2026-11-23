# The stability contract

limpet holds its owner's memory, so the store has to outlive the releases
that write it. This file names what is stable, what enforces it, and what is
deliberately not promised. The contract's code shipped ahead of the 1.0 tag;
the tag is the owner's declaration that it has soaked.

## What is stable

**The store schema.** Migrations are forward-only, additive, self-gating on
actual table state, and true no-ops on re-run. Two guards enforce the
direction: `Store::open` refuses, loudly and before any migration runs, when
the store's stamped `schema_version` is newer than the binary
(`schema_guard`), and the stamp itself can never move backward at the SQL
layer (`stamp_schema_version` writes only forward). A refused open names
both versions and the remedy. Before these guards, an older binary silently
re-stamped a newer store downward; that class of corruption is closed.

**The JSONL export.** Every export opens with a header line naming its wire
format: `{"limpet_export": 1, "schema_version": N}`. Import accepts
headerless files (every pre-1.0 export) unchanged, skips the header as
non-data, and refuses a whole file that claims a format newer than the
binary supports, naming both formats. The format number bumps only when an
old importer could misread the new shape; additive per-line fields do not
qualify, because unknown fields were always ignored.

**The tool API.** The six MCP tools (`recall`, `remember`, `map`,
`affected`, `verify_queue`, `admin`) and the `admin` op set are the surface
agents program against. Additions are fair game; renames, removals, and
semantic changes to existing parameters are breaking and get a major
version. `tests/docs_in_sync.rs` pins the op enum to the dispatcher and to
the docs, so the advertised surface cannot drift from the shipped one.

**Write-path safety.** `version_guard`, which every tool call runs, refuses
writes when the store's `code_version` was stamped by a newer binary OR its
schema stamp exceeds what the running binary knows. The schema half matters
for long-lived handles: a `serve` that passed the open-time guard while the
store was old is stopped at its next tool call if a newer binary has
migrated the store forward in the meantime. Secret detection runs on every
write path (remember body and evidence, reverify command and output, import
per line) in guaranteed linear time. Every SQL statement binds values
through parameters; the only runtime-assembled SQL text in the tree is a
generated placeholder list for one IN clause and one WHERE fragment chosen
from compile-time constants, with all values bound in both (swept
2026-08-25).

**The store identity.** A repository keeps its store across a failing
`git` binary (the remote is read from `.git/config`), provider SSH host
aliases, and moves or re-clones of the checkout. The config fallback never
overrides git's `safe.directory` ownership check: a repository git refuses
as owned by someone else, or (on unix) git metadata owned by another
account, keys by path exactly as before. When the key does change
(a remote moved hosts), the old store is never deleted, renamed, or
written: `limpet doctor`, the SessionStart hook, and `admin` status
(`other_stores`) report it, and `limpet adopt` merges it through the same
guarded import path as a teammate's export (private and archived state
preserved), stamping a watermark so a re-run is a no-op. A store recorded
under a different remote is never merged unless named with `--from <dir>`.

**Upgrading to 0.18.** Two kinds of store written by 0.17 are not the
store 0.18 opens for the same repository:

- a path-keyed store (`path-<dir>-*`) created while `git` itself was
  failing (on macOS, exit 69 until the Xcode licence is accepted; git
  missing from PATH): 0.18 reads the remote from `.git/config` and opens
  the `git-<host>-<org>-<repo>-*` store instead;
- a store under a provider alias key only (for example
  `git-ssh-bitbucket-org-<org>-<repo>-*`, created after the remote moved to
  `ssh.bitbucket.org`): 0.18 opens the canonical `git-bitbucket-org-*`
  store.

In both cases the target store may hold fewer memories or not exist yet.
Nothing is lost and the SessionStart hook names the other store, but
recall does not see those memories until they are merged. After updating, run `limpet doctor` in each active
repository; if it reports another store, run
`limpet adopt --root <repo> --dry-run` to preview and then
`limpet adopt --root <repo>` to merge. `limpet update` cannot print this
for you on the 0.17 to 0.18 step, because its post-update check runs in the
old binary.

## What is not promised

- The UI's layout, endpoints, and JSON shapes. The UI is a local viewer,
  read-only by test on current-schema stores (on an older store it migrates
  forward like every read-write surface), and free to change.
- CLI human-readable output (`stats`, `doctor`, `demo` text). Scripts should
  consume the MCP tools or the JSONL export, not screen text.
- Benchmark numbers. The 4x gate is a floor the release process enforces,
  not a per-query guarantee.
- Ranking internals. Recall scoring weights may be rebalanced behind the
  bench and eval gates; staleness semantics (what gets flagged, and that it
  gets flagged deterministically) are the stable part.

## Reading this as a user

If a newer limpet migrated a store forward, older binaries refuse it at open
and at every tool call, and tell you to upgrade. If a newer limpet exported
a file in a future format, current binaries refuse the import whole, naming
both formats (binaries from before the header existed abort the import
loudly and transactionally on the header line, and the bootstrap import
retries on the next index once the binary is current). Nothing in these
paths guesses, truncates, or migrates backward. Your memory's worst case is
a loud refusal naming the fix, never silent loss.
