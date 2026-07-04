---
name: limpet
description: Index the current project into limpet and work memory-first. Use when the user types /limpet, says "index this project", "limpet index", or at the start of substantive work in any repository when limpet tools are available.
---

# limpet: memory-first workflow

limpet stores what you learn about this codebase as durable memory anchored
to the code itself. Memories survive context loss and go visibly stale when
their code changes. Work with it in this order.

## On invocation

1. Call `admin` with `{"op": "index"}`. Report files, symbols, and anchor
   resolution counts in one line.
2. Call `recall` with the user's current goal as `task` (if they stated one)
   or `"project orientation: architecture, constraints, known gotchas"`.
   Present what came back, one line per memory. Show stale or contradicted
   flags exactly as returned; never present a flagged memory as current fact.
3. Call `verify_queue`. If it is non-empty, tell the user how many verified
   facts need re-proof and offer to run their reverify commands now.
4. Start the visual memory UI unless one is already running: if nothing is
   listening on 127.0.0.1:9748 (`lsof -nP -iTCP:9748 -sTCP:LISTEN`), run
   `limpet ui` as a background shell task and report the graph is live at
   http://127.0.0.1:9748. If the port is already in use, just report the
   URL. If the `limpet` binary is not on PATH, skip this step silently —
   never block the memory workflow on the UI.

## Standing behavior for the rest of the session

- **Before editing** a file you have not touched yet, call `map` on it.
  Attached memories are constraints: decisions and frozen APIs live there.
- **Before committing or summarizing changes**, call `affected` to see which
  memories your diff put at risk and which decisions bound the code you
  changed.
- **Recall before read.** Before grepping or reading a file to answer a
  question about this project, call `recall` with the question. If an
  active memory answers it, do not read the file at all.
- **Remember as you learn.** When you discover something durable, call
  `remember` immediately, anchored to the relevant symbol:
  - `decision`: a choice plus the reason and rejected alternatives
  - `fact`: verified behavior; include `evidence` (command + output) when a
    command proved it
  - `episode`: what was tried and failed, and why it failed
  - `insight`: a gotcha or non-obvious constraint
  - `intent`: what a module is for when the code cannot say
  Anchor to a symbol when one exists (survives renames and moves, gives
  precise staleness); anchor to the file (omit `symbol`) for templates,
  styles, and configs — every file in the repo is indexed and file anchors
  go stale when the file's content changes. If `remember` rejects an
  anchor, the path is wrong or the file is excluded by ignore rules; fix
  it rather than dropping the anchor.
  Write bodies short, specific, and standalone. Do not store anything
  derivable from a quick read of the code, and never store secrets,
  credentials, tokens, or personal data. limpet enforces this: `remember`
  rejects a body or evidence output that looks like a credential, so a
  secret can never reach the store or a shared `.limpet/memory.jsonl`.
- **Trust recall before re-deriving.** If recall answers the question, do
  not re-read files to confirm what an active memory already states. If the
  memory is flagged stale, verify against the code first, then update it:
  store the corrected entry with a `supersedes` link to the old one.

## Seeding a project: /limpet scan

Cold-start a repo's memory from what already exists. Depth: `light`
(default) harvests merge commits, tags, and the README; `deep` adds all
docs, long-body commits, and the assistant's project memory directory.

1. Preflight: `admin` `{"op": "status"}` plus a recall for existing
   coverage. A non-empty store means gap-fill mode: drop any candidate a
   recall already answers. If the limpet binary was updated during this
   session, the MCP server may still run the old image and will silently
   drop `private`/`origin` arguments — restart the session before
   seeding.
2. Quality pre-check, before curating: percentage of commits with
   non-empty bodies, merge count, docs present. Thin history gets said
   plainly upfront ("history thin, expect few candidates") and shrinks
   the harvest; never pad the yield to look useful.
3. Harvest in a subagent so raw git output and doc dumps never enter the
   main context; only the candidate table comes back. Bounded commands:
   `git log --merges -n 100 --format='%h|%s|%b'`, `git tag -l
   --sort=-creatordate` with messages, commits with non-empty bodies
   (`%b` carries the why), plus docs and, on `deep`, the assistant
   project memory dir (skip silently when absent). Global assistant
   memory only after an explicit in-run confirmation, and always private.
   Every proposed anchor path must be verified to exist (`ls`) before it
   enters the candidate table — harvesters guess module names, and
   `remember` rejects an unresolvable anchor loudly at write time.
4. Curate to limpet's bar: kind (decision/episode/insight/intent), anchor
   (symbol if identifiable, else file), short standalone body. Reject
   anything derivable from a quick read of the code; keep the why, drop
   the what-changed. Cap: 25 per scan, ranked by durable value.
5. Review gate, two tiers: high-confidence candidates as ONE block
   approved with reject-by-exception ("untick any to drop"); borderline
   candidates item by item. Private candidates are always item-level,
   never bulk. Nothing is written before approval.
6. Write each approved candidate via `remember` with an `origin` stamp
   (`scan:git:<sha>`, `scan:doc:<path>#<heading>`, `scan:mem:<file>`);
   private-source items also pass `private: true`. Origins are unique
   per candidate: two candidates from the same doc section need
   disambiguated stamps (`#section-death`, `#section-heal`), never a
   shared one. A duplicate-origin rejection means an earlier scan
   already stored it: count it as skipped and move on.
7. Report honestly: seeded by kind, skipped (including origin dups),
   private count, and the pre-check verdict.

## Arguments

- `/limpet` or `/limpet index`: run the invocation sequence above.
- `/limpet status`: call `admin` `{"op": "status"}` and `verify_queue`;
  report counts and anything needing attention.
- `/limpet review`: work through `verify_queue`; for each item run its
  reverify command, then update the memory with fresh evidence or supersede
  it if the fact changed.
- `/limpet ui`: start the visual memory UI (step 4 of the invocation
  sequence) without re-running the index/recall flow, and report the URL.
- `/limpet export`: call `admin` `{"op": "export"}` so the memory can be
  committed and shared with the team.
- `/limpet stats`: call `admin` `{"op": "ledger"}` and present the token
  savings receipt (session + lifetime: saved tokens, reads avoided, recalls
  gross vs distinct); note it is a conservative floor per the method string.
- `/limpet info`: the juicy session summary. Combine `admin` `{"op":
  "status"}` + `{"op": "ledger"}` into a short human brief: how many
  memories are active and healthy, what went stale and why, tokens saved
  this session and lifetime with the x-multiplier (baseline/served), file
  reads avoided, and the single most-recalled fact if evident. Two short
  paragraphs or a compact table, no raw JSON.
- `/limpet statusline`: toggle the limpet segment in the Claude Code
  statusline (`| 🐚 <active> ↑<saved>k tokens saved`). On by default;
  toggling creates or removes
  `${CLAUDE_CONFIG_DIR:-$HOME/.claude}/.limpet-statusline-off`, then report
  the new state. Rendering is `limpet statusline --root <dir>` — read-only,
  no server, no sweep, no writes, works on macOS/Linux/Windows alike.
- `/limpet update`: run `limpet update` in the shell to self-update to the
  latest release binary (checksum-verified, atomic). This is the only limpet
  command that uses the network. Report the old and new version, then tell the
  user to restart Claude Code so the MCP server reloads onto the new binary.
  Use `limpet update --check` to report whether a newer version exists without
  installing it.
- `/limpet scan` or `/limpet scan light|deep`: seed memory from git
  history, docs, and (deep) assistant memory, per the seeding section
  above. Idempotent: re-runs add only gaps.
