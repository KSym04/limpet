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

## Standing behavior for the rest of the session

- **Before editing** a file you have not touched yet, call `map` on it.
  Attached memories are constraints: decisions and frozen APIs live there.
- **Before committing or summarizing changes**, call `affected` to see which
  memories your diff put at risk and which decisions bound the code you
  changed.
- **Remember as you learn.** When you discover something durable, call
  `remember` immediately, anchored to the relevant symbol:
  - `decision`: a choice plus the reason and rejected alternatives
  - `fact`: verified behavior; include `evidence` (command + output) when a
    command proved it
  - `episode`: what was tried and failed, and why it failed
  - `insight`: a gotcha or non-obvious constraint
  - `intent`: what a module is for when the code cannot say
  Write bodies short, specific, and standalone. Do not store anything
  derivable from a quick read of the code, and never store secrets,
  credentials, tokens, or personal data.
- **Trust recall before re-deriving.** If recall answers the question, do
  not re-read files to confirm what an active memory already states. If the
  memory is flagged stale, verify against the code first, then update it:
  store the corrected entry with a `supersedes` link to the old one.

## Arguments

- `/limpet` or `/limpet index`: run the invocation sequence above.
- `/limpet status`: call `admin` `{"op": "status"}` and `verify_queue`;
  report counts and anything needing attention.
- `/limpet review`: work through `verify_queue`; for each item run its
  reverify command, then update the memory with fresh evidence or supersede
  it if the fact changed.
- `/limpet export`: call `admin` `{"op": "export"}` so the memory can be
  committed and shared with the team.
