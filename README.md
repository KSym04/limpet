# 🐚 limpet

**small shell. long memory.**

[![CI](https://github.com/KSym04/limpet/actions/workflows/ci.yml/badge.svg)](https://github.com/KSym04/limpet/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-green.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-stable-orange.svg)](https://rustup.rs)
[![Platforms](https://img.shields.io/badge/macOS%20%7C%20Linux%20%7C%20Windows-supported-lightgrey.svg)](https://github.com/KSym04/limpet/actions/workflows/ci.yml)
[![100% local](https://img.shields.io/badge/network%20calls-zero-blue.svg)](SECURITY.md)

**Memory for AI coding agents that goes stale when your code does.** Everything your agent learns about a codebase (decisions, verified facts, failed approaches, gotchas) survives every session, anchored to the actual code it describes, and flips to `stale` with the reason attached the moment that code moves on. One Rust binary, one SQLite file, standard MCP, zero network calls.

<p align="center">
  <img src="docs/limpet-demo.svg" alt="animated demo: recall answers from memory, an edit flips it to stale with the reason, a revert heals it" width="860">
  <br>
  <em>the anchor lifecycle, real output: recall answers instantly, an edit flips the memory stale, a revert heals it</em>
</p>

The failure mode that kills AI coding assistants is not forgetting; it is remembering something that is no longer true. Vector stores trust similarity, time-decay tools trust the calendar, markdown notes trust forever, so an agent that still believes last month's version of a function is not unhelpful, it is confidently wrong. limpet anchors each memory to a normalized AST hash of the code it describes: rename or move the code and the memory follows, edit it and the memory goes visibly stale with a reason, revert and it heals. Noticing its own staleness is the entire premise, and no other open memory layer does it.

## ⏱️ 30 seconds to running

```bash
curl -fsSL https://raw.githubusercontent.com/KSym04/limpet/main/install.sh | bash
```

(Windows: `irm https://raw.githubusercontent.com/KSym04/limpet/main/install.ps1 | iex`; full install options [below](#-install).)

Restart Claude Code, type `/limpet` in any project, and just work:

> **session 1:** "the scanner batch size is 50 because shared hosts kill long requests" → stored as a `decision`, anchored to the function that uses it
>
> **session 30, fresh context:** *you:* why is the batch size 50? → *agent:* one `recall`, ~350 tokens, full answer. No file spelunking, no re-teaching your own codebase.
>
> **you edit that function** → the memory flips to `stale: body_edited` everywhere it appears. Nothing ever pretends to be current when it is not.

## 🎯 Why limpet, in four checkable claims

- **It knows when it is wrong.** Memories anchor to AST hashes, follow renames and file moves, go stale on real edits with the reason attached, and heal on revert. → [the anchor lifecycle](#-the-anchor-lifecycle)
- **It shows you what it saved.** Every recall is priced against the file reads it replaced: 4.0x fewer tokens on the benchmark, and a live per-project ledger via `limpet stats`. → [the receipts](#-the-receipts-token-savings-measured)
- **It anchors the whole repository.** Eleven grammars for symbol-level anchoring; every other file (templates, styles, configs) anchorable at file level. → [whole repo indexed](#-whole-repo-indexed-thin-on-purpose)
- **It never lies by omission.** Every response carries an honesty envelope: matched vs returned, what was dropped and why, how fresh the index is, how much is stale. The benchmark gate has killed limpet's own features when they crossed that line.

It is not a vector database, a code-search engine, or a call-graph oracle; it is the layer that remembers *why*, tied to the code, and tells you when the why no longer holds. → [what limpet is not](#-what-limpet-is-not) · design principles with the scars that earned them: [PHILOSOPHY.md](PHILOSOPHY.md)

## 🧠 Why this exists

Code indexers answer "what is where". The expensive knowledge is not in any file:

- why the batch size is 50
- which refactor was tried and rolled back, and what broke
- which method names are frozen because customers hook them
- what that weird cron job actually protects against

Agents re-derive or re-ask this every session, burning tokens, or worse, they guess. limpet gives that knowledge the same lifecycle engineers give it:

```
valid -> code changed -> stale (reason attached, confidence drops) -> re-verified or superseded
```

Three properties make it work, and nobody else combines them: **anchored** (AST body hashes, not line numbers or paths), **honest** (the envelope on every response; nothing truncates silently), **evidenced** (a fact can carry the command that proved it, and hands it back when the anchor goes stale).

## 👥 Who is this for

| You are | limpet gives you |
|---|---|
| **Solo dev with an AI agent** | The first session spends tens of thousands of tokens learning your codebase; afterward it is one `recall` away. Survives `/clear`, compaction, new machines. |
| **A team** | `admin export` writes `.limpet/memory.jsonl` for git; teammates import after pulling. Onboarding knowledge travels with the repo and, unlike a wiki, flags itself when the code moves on. `affected` shows which documented decisions a diff just put at risk. |
| **Template-heavy stack** (Rails, Laravel, Vue, any CMS theme) | Every file is anchorable, so "this layout is locked to 480px" pins to the actual stylesheet and goes stale when someone edits it, exactly what symbol-only indexers cannot reach. |
| **Open-source maintainer** | `intent`/`decision` memories answer "why is this weird code here" before the PR that "fixes" it lands; `episode` memories stop the third contributor from re-attempting the refactor that already broke twice. |
| **Agent builder** | A model-agnostic memory backend behind standard MCP: one binary, zero network, inspectable SQLite. Nothing to explain to a security review. |

**Who should skip it:** throwaway scripts, repos you touch once, teams that do not use AI agents. Memory pays off only when questions repeat.

## 🧰 The six tools

| Tool | What it does |
|---|---|
| `recall` | Task description in, token-budgeted ranked memory pack out. Stale and contradicted items are always flagged, never hidden. Verified facts outrank unverified claims at equal relevance; an item without a `source` field is an unverified claim, not a proof. |
| `remember` | Store a memory: `fact`, `decision`, `episode`, `insight`, or `intent`. Anchor it to code. Attach evidence to make it verified. A near-identical body on the same anchor is refused (naming the existing id to supersede; `force: true` overrides), and a same-anchor memory asserting a divergent value comes back as `possible_conflicts` so contradictions are caught at write time. `private: true` keeps a memory local (recalled here, withheld from export); `origin` makes writes idempotent for seeding flows. |
| `map` | Structural outline of a file or symbol plus every memory attached to it. For a symbol target it also returns `lineage` (ancestors, descendants, callers) with each edge labeled `unique`/`ambiguous`/`unresolved`. Code and knowledge in one answer. |
| `affected` | What does my uncommitted diff touch: symbols, memories now at risk, and decisions constraining the code being edited. |
| `verify_queue` | Verified facts whose anchored code changed, each with the exact command that originally proved it. |
| `admin` | index, status, forget, archive / restore, export / import (guarded), ledger / ledger_reset (the savings receipt). Archive shelves a memory without deleting it: hidden from recall while its staleness keeps tracking the code, restored with its current truthful status, and still exported (flagged) so nothing hidden is ever lost. Export reports `private_withheld` so callers know how many memories stayed local. |

Every response is wrapped in the honesty envelope:

```json
{
  "data": [ ... ],
  "meta": {
    "freshness": { "indexed_at": "2026-07-03T10:12:44Z", "dirty": 0 },
    "completeness": { "matched": 7, "returned": 3, "omitted_reason": "budget" },
    "staleness": { "stale": 1, "contradicted": 0 }
  }
}
```

## ⚓ The anchor lifecycle

```
code change            anchor resolution         memory becomes
---------------------  ------------------------  -----------------------------
reformat / comments    same normalized AST hash  active (untouched)
rename symbol          body found under new FQN  active, anchor follows
move file              body found in new file    active, anchor follows
edit function body     hash differs at FQN       stale (body_edited), conf drops
delete symbol          body found nowhere        invalidated (kept as history)
duplicate bodies       multiple matches          stale (ambiguous_anchor)
edit anchored file     file content hash differs stale (file_edited), conf drops
delete anchored file   file row gone             invalidated (kept as history)
lose SOME anchors      others still resolve      stale (anchor_lost), never killed
move a trivial body    unique match, too small   stale (low_entropy), never re-pointed
```

A multi-anchor memory dies only when **every** anchor dies. Losing one anchor while others still resolve degrades it to `stale:anchor_lost` so the surviving knowledge stays usable. And `remember` refuses an anchor it cannot resolve, loudly, at write time: no memory is ever born dead.

Rename-following is evidence-gated: a unique match on a trivial body (an empty function, a bare delegation stub, a near-empty file) is refused as follow evidence and surfaces as `stale:low_entropy` instead of silently re-pointing the anchor at a look-alike twin, and it heals the moment the original code returns.

Staleness is also symmetric: revert the code (a rolled-back experiment, a `git checkout`) and the memory heals back to active on the next call, because the anchor hash matches again. No re-verification ritual for changes that un-happened. The same applies to invalidation: a branch switch, `git stash`, or mid-rebase state that makes files vanish briefly is not a death sentence: when the code comes back and the anchors resolve, the memory recovers. The only final state is `superseded`, which records a deliberate human decision rather than filesystem churn.

Contradictions are explicit links: when a new memory contradicts an old one, both stay visible with the conflict flagged until one `supersedes` the other. History is never silently overwritten.

## 🔬 Staleness models, compared

Every memory tool eventually faces the same question: is this still true? Four answers dominate the market, and none of them can see a silent code change.

| Staleness model | Typical of | What it cannot see |
|---|---|---|
| Time-based decay: memories age toward "needs review" on a schedule | memory tools with decay policies | Age is not truth. A calendar cannot name which memory a refactor broke five minutes ago, and it nags about stable knowledge that never changed. |
| LLM-judged invalidation: a model decides new input contradicts an old fact | conversation memory layers | Fires only when something new is said. Code that drifts between sessions triggers nothing, so the memory stays confidently wrong. |
| Embedding similarity | vector stores and RAG pipelines | A contradiction and a near-duplicate look identical to cosine similarity. |
| Manual curation: "review your learnings periodically" | review-bot knowledge bases, wikis | A human garbage-collecting prose, on human time, after the damage. |
| **Deterministic AST anchors (limpet)** | | Staleness is a fact computed from the code itself: the exact memory, the exact reason (`body_edited`, `anchor_lost`, `low_entropy`), on the next tool call after the edit. Reverting the code heals it. |

The first four expire knowledge by guessing. limpet checks.

## 🪙 The receipts: token savings, measured

Persistent memory also happens to be dramatically cheaper than re-derivation. Where the savings come from:

1. **Answers travel as conclusions, not source.** A memory is the 30-token distilled answer; the files it was learned from are thousands of tokens. Reading code to re-derive a known fact pays the full price every single session. Recall pays once per question.
2. **The worst spend is exploration that cannot succeed.** Why a batch size is 50, which refactor was rolled back, which API is frozen: not in any file. Without memory the agent greps, reads, and still does not know, so the tokens bought nothing. With memory these are the cheapest questions of all.
3. **Responses are budget-packed and noise-cut.** `recall` takes a token budget, packs best-first, drops the low-relevance tail, and reports what it omitted. You spend what you allowed, never what happened to match.
4. **No re-teaching after context loss.** Compaction, `/clear`, new session: the knowledge survives outside the context window and comes back at recall prices, not re-derivation prices.

Measured with a reproducible benchmark, seeded with 12 memories over a realistic 9-file fixture service, asking 10 questions an agent typically re-answers every session:

```
question                                                   files+grep   recall   ratio  in code?
----------------------------------------------------------------------------------------------------
why is the batch size 50 and why is there a queue at all         1929      367    5.3x  no (answer only in memory)
why does the scanner skip draft products, is that a bug          1630      377    4.3x  no (answer only in memory)
how is the health score computed                                 1630      352    4.6x  yes
why semicolon delimiter and BOM in the csv export                1327      361    3.7x  no (answer only in memory)
where do report files get written and why                        1327      371    3.6x  no (answer only in memory)
how long are download tokens valid                               1023      167    6.1x  yes
has anyone tried streaming the csv export                        1327      369    3.6x  no (answer only in memory)
can I rename check_product in the scanner                        1630      377    4.3x  no (answer only in memory)
what does the nightly cron actually exist for                     803      317    2.5x  no (answer only in memory)
how often does the dashboard poll progress and can I lower it    1072      340    3.2x  no (answer only in memory)
----------------------------------------------------------------------------------------------------
TOTAL                                                           13698     3398    4.0x
```

**4.0x fewer tokens (75% saved) across the benchmark.** Reproduce it yourself:

```bash
cargo build --release
python3 bench/token_savings.py
```

And the number is not just a benchmark: limpet keeps **your own receipt**. Every recall is priced against its file-reading counterfactual with the same methodology, and `limpet stats` (or `admin {op:"ledger"}`, or the UI header) shows session and lifetime savings: tokens saved, reads avoided, recalls gross and distinct. Negative savings are shown, never floored, and anchorless memories count zero baseline, so the figure is a conservative floor, not marketing.

<p align="center">
  <img src="docs/statusline-savings.svg" alt="terminal statusline segment showing 9 active memories and 134k tokens saved" width="420">
  <br>
  <em>the receipt, live in a Claude Code statusline: active memories and lifetime tokens saved, read straight from the store</em>
</p>

Methodology, stated so the number can be checked rather than believed:

- "Without limpet" cost is the **minimal** file set containing the answer plus a flat 300 tokens for search round trips. Real agents read more than the minimal set, so real savings are higher.
- Tokens are estimated as ceil(bytes/4) on both sides identically.
- 8 of the 10 questions are marked "no" above: their answers exist in **no file at any token price** (decisions, history, tribal knowledge). File reading gets you the code but not the answer. We still charge limpet full price against the file-reading cost instead of claiming infinite savings.
- The script is a regression gate: it exits nonzero if savings drop below 4x.
- Fixture files are 58 to 179 lines. Real source files run several times larger, and the "without" side grows with file size while a recall response does not.

## 🗺️ Visual memory

```bash
limpet ui --port 9748
```

The UI is its own command, separate from the MCP server. `limpet serve` (stdio) is what Claude Code launches for you automatically after `limpet install`; nothing listens on a port until you start `limpet ui` yourself. If http://127.0.0.1:9748 refuses connections, the MCP server is not broken; the UI just is not running.

<p align="center">
  <img src="docs/limpet-ui.png" alt="limpet visual memory graph: memories colored by health, clamped to code symbols" width="820">
  <br>
  <em>limpet ui: green memories are trustworthy, amber went stale when their code changed, squares are the symbols they clamp onto</em>
</p>

Open http://127.0.0.1:9748 for a live force-directed view of the knowledge graph: memories sized by confidence and colored by health (green active, amber stale, red invalidated), clamped to the files and symbols they describe, with contradiction and supersession edges drawn. The "needs attention" filter shows exactly what went stale and why, with the re-verify command one click away. Where other tools visualize code structure, limpet visualizes what your agent knows and whether it is still true. Served by the same single binary, bound to 127.0.0.1 only.

## 📦 Install

**One line.** The installer downloads the latest release binary for your platform, verifies its sha256, installs it, and registers it with Claude Code:

macOS / Linux:

```bash
curl -fsSL https://raw.githubusercontent.com/KSym04/limpet/main/install.sh | bash
```

Windows (PowerShell):

```powershell
irm https://raw.githubusercontent.com/KSym04/limpet/main/install.ps1 | iex
```

Then restart Claude Code and type `/limpet` in any project. That is the whole setup. (`LIMPET_INSTALL_DIR` overrides the install location; `LIMPET_VERSION` pins a release tag.)

Prefer not to pipe a script into your shell? Every step below is the manual equivalent. Prebuilt binaries ship for Apple Silicon macOS, x86_64 Linux, and x86_64 Windows on the [latest release](https://github.com/KSym04/limpet/releases/latest); every asset has a published sha256; verify it before you trust the binary.

**macOS (Apple Silicon)**

```bash
curl -fsSLO https://github.com/KSym04/limpet/releases/latest/download/limpet-aarch64-apple-darwin.tar.gz
curl -fsSLO https://github.com/KSym04/limpet/releases/latest/download/limpet-aarch64-apple-darwin.tar.gz.sha256
shasum -a 256 -c limpet-aarch64-apple-darwin.tar.gz.sha256
tar xzf limpet-aarch64-apple-darwin.tar.gz          # extracts a single `limpet` binary
sudo install -m 755 limpet /usr/local/bin/limpet
limpet install
```

macOS may quarantine a downloaded binary; if it refuses to run, clear the flag with `xattr -d com.apple.quarantine /usr/local/bin/limpet`.

**Linux (x86_64)**

```bash
curl -fsSLO https://github.com/KSym04/limpet/releases/latest/download/limpet-x86_64-unknown-linux-gnu.tar.gz
curl -fsSLO https://github.com/KSym04/limpet/releases/latest/download/limpet-x86_64-unknown-linux-gnu.tar.gz.sha256
sha256sum -c limpet-x86_64-unknown-linux-gnu.tar.gz.sha256
tar xzf limpet-x86_64-unknown-linux-gnu.tar.gz      # extracts a single `limpet` binary
sudo install -m 755 limpet /usr/local/bin/limpet    # or ~/.local/bin if it is on your PATH
limpet install
```

**Windows (x86_64, PowerShell)**

```powershell
Invoke-WebRequest https://github.com/KSym04/limpet/releases/latest/download/limpet-x86_64-pc-windows-msvc.zip -OutFile limpet.zip
Invoke-WebRequest https://github.com/KSym04/limpet/releases/latest/download/limpet-x86_64-pc-windows-msvc.zip.sha256 -OutFile limpet.zip.sha256
# compare the two hashes; they must match
(Get-FileHash limpet.zip -Algorithm SHA256).Hash
Get-Content limpet.zip.sha256
Expand-Archive limpet.zip -DestinationPath "$env:LOCALAPPDATA\limpet"   # extracts limpet.exe
# add the folder to PATH once, then restart the terminal
[Environment]::SetEnvironmentVariable('Path', $env:Path + ";$env:LOCALAPPDATA\limpet", 'User')
limpet install
```

**With Rust** ([rustup.rs](https://rustup.rs)), the path for Intel macs, ARM Linux, and any platform without a prebuilt binary:

```bash
cargo install limpet
limpet install
```

Restart Claude Code. Done. (`limpet install --dry-run` previews the exact config changes first; `limpet uninstall` reverses them.)

**Update later** with `limpet update`: it fetches the latest release binary for your platform, verifies it against the published sha256, and atomically replaces the running executable. `limpet update --check` reports whether a newer version exists without installing it. This is the only command that touches the network. Restart Claude Code afterward so the MCP server reloads onto the new binary.

## 🚀 Day-to-day usage

`/limpet` in any project indexes the code, recalls everything already known (stale items flagged), and switches the session to memory-first mode. From there the agent stores what it learns as it learns it; the [30-second story](#-30-seconds-to-running) above is the whole loop.

Everyday commands:

| Command | Does |
|---|---|
| `/limpet` | index + recall + memory-first mode for the session |
| `/limpet status` | counts and anything needing attention |
| `/limpet review` | re-verify stale facts using their stored proof commands |
| `/limpet export` | write `.limpet/memory.jsonl` to commit and share with the team |
| `limpet stats` | the token-savings receipt: session + lifetime, methodology included |
| `limpet doctor` | one-screen setup diagnosis; also runs automatically after install and update |
| `limpet ui` | knowledge graph at http://127.0.0.1:9748, all projects in one view |
| `limpet statusline` | the statusline segment (memories + tokens saved), read-only and instant |
| `limpet hook` | one-line SessionStart brief for Claude Code hooks, read-only |
| `limpet update` | self-update to the latest release, checksum-verified (the only networked command) |
| `limpet demo` | the anchor lifecycle (active → stale → healed) on a throwaway repo, self-verifying |
| `limpet seed <file>` | ingest an existing MEMORY.md / CLAUDE.md into anchored memory; re-runs are no-ops |

Data lives under `~/.local/share/limpet/`, one SQLite store per repository. Teammates run `limpet import` after pulling the JSONL (`--path <repo-relative file>` imports a different export).

**Keep your MEMORY.md.** `limpet seed MEMORY.md` chunks a plain notes file into individual memories: bullets and paragraphs become entries, a chunk that names a repo file is anchored to it (so it goes stale when that file changes), and everything seeds as `mined` (lower trust than verified facts until re-confirmed). Re-running on an unchanged file is a no-op. A **reworded** note is different: its old wording is already stored, so the write-path dedup refuses the new one and the report says so; re-run with `--force` to store the new wording too, then supersede the old from the agent side.

**Statusline on any platform.** `limpet statusline --root <project dir>` prints the shell segment (`| 🐚 13 · ↑32k tokens saved`, with the count hyperlinking to the project's graph when the UI is running) or nothing at all: it opens the store strictly read-only, never writes, and always exits 0, so it can sit in a prompt safely. Because the rendering lives in the binary, the same one-liner works from a bash statusline on macOS/Linux and a PowerShell or cmd statusline on Windows, with no sqlite3 CLI and no bash required:

```powershell
# inside a Claude Code statusline.ps1
$limpetSeg = & limpet statusline --root $projectDir
```

The simplest setup is to let Claude Code call the binary directly as its whole statusline; add to `~/.claude/settings.json`:

```json
{
  "statusLine": {
    "type": "command",
    "command": "limpet statusline --root \"$CLAUDE_PROJECT_DIR\""
  }
}
```

Already have a custom statusline? Append the binary's output as one segment, but do **not** query `store.db` with `sqlite3` yourself. The per-repo store key scheme changes between versions (it moved from a path hash to a portable git-remote identity in v0.9.0), so a hand-rolled query silently stops matching and the segment just disappears. Only `limpet statusline` tracks the current scheme. Run `limpet doctor` to check how your statusline is wired: it reports `ok` when it delegates to the binary, `warn` when it hand-rolls a store query that will drift, and the exact line to add when nothing is wired.

Toggle it off with `/limpet statusline` (writes `~/.claude/.limpet-statusline-off`; the command honors the flag).

**Auto-recall at session start.** Without any hook, memory-first behavior depends on the agent remembering to type `/limpet`. With a SessionStart hook, every new session in an already-indexed project opens with a one-line brief injected into context ("This project has limpet memory: 13 active memories, 2 stale…") and the agent recalls before it reads. Add to `~/.claude/settings.json`:

```json
{
  "hooks": {
    "SessionStart": [
      {
        "hooks": [
          { "type": "command", "command": "PATH=\"$HOME/.local/bin:$HOME/.cargo/bin:$PATH\" limpet hook" }
        ]
      }
    ]
  }
}
```

`limpet hook` prints nothing when the project has no store (sessions outside indexed repos stay clean), opens the store strictly read-only, and always exits 0. The explicit PATH prefix matters: hooks run in a non-login shell that often lacks `~/.local/bin` and `~/.cargo/bin`, and a silently-missing binary is exactly the kind of failure this command is designed never to surface. On Windows use `%USERPROFILE%\AppData\Local\Programs\limpet\limpet.exe hook`.

### Seeding a project

`/limpet scan` cold-starts a repository's memory from what already exists. `light` mode (default) harvests merge commits, tags, and the README; `deep` adds all docs, long-body commits, and the assistant's project memory directory. A preflight recall catches any candidates the store already answers, so re-runs only add gaps. The harvest runs in a subagent to keep raw git output and doc dumps out of the main context; only the curated candidate table comes back. You approve candidates in two tiers before anything is written: high-confidence items as a single block (reject-by-exception), borderlines one at a time. Each approved memory is stamped with an `origin` so duplicates are caught on future scans, and private-source items are stored with `private: true` so they are never exported.

## 🌳 Whole repo indexed, thin on purpose

**Every file in the repository is indexed and anchorable.** Files with a shipped grammar (PHP, JavaScript, TypeScript, Python, Rust, C/C++, Go, Java, Ruby, C#, Bash) get full symbol extraction: functions, classes, imports, name-based call references labeled `syntactic`, and inheritance edges (extends, implements, impl_trait, embeds, mixin) surfaced through `map` lineage. Every other file (`.twig`, `.scss`, `.vue`, `.blade.php`, `.erb`, `.md`, `.yml`, configs, anything) gets a file-level node with a content hash, so a memory can anchor to it and go `stale:file_edited` the moment it changes. On template-heavy stacks that is where the knowledge worth remembering actually lives.

Legacy encodings degrade gracefully: a grammar-matched file that is not valid UTF-8 (CP949 or UTF-16 source in an old C++ engine, say) keeps its file-level anchor instead of disappearing from the index. A grammar can only ever upgrade a file, never make it less anchorable.

What the walk skips, deliberately: everything in `.gitignore`, everything in an optional `.limpetignore` (gitignore syntax, works even outside a git repo), and a built-in set of near-universally generated or vendored trees (`node_modules`, `vendor`, `target`, `dist`, `build`, `__pycache__`, `.venv`/`venv`, Python caches, `.next`/`.nuxt`/`.svelte-kit`, `Pods`, `.gradle`, `bower_components`, `coverage`, `.terraform`), hidden junk, `*.min.*` assets, and files over 8MB. Source files over 512KB are indexed at file level but never parsed for symbols: the cap protects tree-sitter from generated bundles, and a giant hand-written translation unit stays anchorable instead of vanishing. Those bounds are what keep a large vendored dependency tree with no `.gitignore` from pegging your CPU; use `.limpetignore` to opt out anything else.

There is no LSP, no type inference, and no claim of a publishable call graph: the index exists to give memory anchor points, invalidation, and recall locality. Every shipped grammar has fixture coverage in the test suite; languages are added when they can be tested, not when they pad a number.

Freshness model: every tool call runs a bounded incremental sweep (changed files reparse in milliseconds via tree-sitter). Queries never block on indexing; anything still dirty is listed in the envelope.

## ⚙️ Per-repo config (`.limpet.json`)

An optional `.limpet.json` at the repository root tunes two things. It is a plain, size-bounded lookup table validated against the shipped grammars; a malformed file fails an explicit `index` loudly instead of being silently ignored.

```json
{
  "extensions": { "inc": "cpp", "module": "php" },
  "auto_import": true
}
```

- **`extensions`** maps a filename suffix to one of the shipped grammars (`php`, `js`, `ts`, `py`, `rust`, `cpp`, `go`, `java`, `rb`, `cs`, `bash`), so template-heavy and legacy stacks get full symbol extraction on extensions the built-in table does not know. The longest matching suffix wins, so a specific `blade.php` overrides a generic `php`.
- **`auto_import`** (default `true`) seeds a brand-new store from a committed `.limpet/memory.jsonl` on the first index, so a teammate who clones the repo gets the shared memory with no extra step. It runs once, only on a fresh store, through the same guarded path as `limpet import`.

**Portable identity.** A repository's store is keyed by its git `origin` remote when it has one, falling back to the canonical path. Move a checkout or re-clone it and the memory follows; two different repositories can no longer collide onto one store. Existing stores migrate automatically on first open, and a store is never mis-claimed: an ambiguous legacy store is left in place rather than reassigned.

## 🔒 Security posture

- **Local by default.** Indexing, recall, memory, and the UI make no network calls, ever: no telemetry, no API keys, no cloud. The one exception is `limpet update`, which you invoke explicitly; it fetches a checksum-verified release binary over HTTPS and sends nothing but a `limpet/<version>` User-Agent. The UI binds 127.0.0.1 and serves one embedded page, GET only.
- **Secrets never persist.** `remember` scans every body and evidence output and refuses to store anything shaped like a credential (cloud access keys, provider tokens, PEM private-key blocks, JWTs), so a secret cannot reach the local store or a shared `.limpet/memory.jsonl`.
- **Private memories stay local.** A memory stored with `private: true` is recalled normally within the project but is never included in `export` output, so sensitive context cannot reach a shared `.limpet/memory.jsonl`.
- **No shell interpolation.** External commands (git only) run with argument arrays; no string ever reaches a shell.
- **Path validation.** Every file path arriving over MCP is checked against the repository root; absolute paths and traversal are rejected at a single choke point.
- **Parameterized SQL only.** No query in the codebase concatenates user input.
- **Malformed input survives.** The JSON-RPC loop answers parse errors and handler panics with JSON-RPC errors and keeps serving.
- `install` edits only its own `mcpServers.limpet` entry and refuses to touch config it does not recognize; `uninstall` reverses exactly that.
- **Import is a guarded path.** A `.limpet/memory.jsonl` pulled from a teammate is untrusted input, so `import` enforces the same rules as `remember`: secrets are rejected (they can never enter the store, even from a peer), bodies are size-capped, confidence is clamped, future-dated entries cannot poison the merge, and imported anchor hashes are re-resolved against your local code so a forged hash cannot fake freshness. Rejected lines are counted, never silently applied.
- **Broken setup? `limpet doctor`.** It checks the binary, the Claude Code registration (including a moved-binary mismatch), the skill file, the store, its version stamp, and the index, printing ok/FAIL per line. It runs automatically after `install` and `update`.

## 🚫 What limpet is not

- Not a generic AI memory store, vector database, or RAG pipeline. No embeddings, no similarity guesswork: anchors are deterministic AST hashes, and staleness is a fact, not a score. Code-unaware memory stores never notice when the code moves on; noticing is limpet's entire premise.
- Not a code search engine. Your agent already has grep.
- Not a call-graph oracle. Call edges are syntactic and labeled as such.
- Not a cloud memory platform. No account, no sync, no server.
- Retrieval quality tracks what gets written: short, specific, anchored memories recall well, and the tool schemas steer agents toward exactly that.

## 🧭 Roadmap

See [ROADMAP.md](ROADMAP.md) for what has shipped (portable repo identity, the statusline doctor, the structural lineage graph, grammar wave 2 with eleven languages, and the 0.13 freshness pass: anchored-first sweep priority plus the evidence-gated low-entropy follow guard), what this release carries (the 0.14 truth layer: verification as a ranking signal, contradictions surfaced and duplicates refused at write time), and what is next (FQN disambiguation, the refinement loop that closes the re-verification cycle, then the 1.0 stability contract). One rule governs all of it: a feature ships only if it feeds a receipt (`limpet stats`, the benchmark, rework-avoided) or the honesty envelope.

## ⚖️ Reliance and license

limpet is an aid to judgment, not a substitute for it. Its freshness and
confidence signals are heuristics computed from code structure, not proof:
an "active" memory is limpet's best evidence that stored knowledge still
holds, never a guarantee that it is correct or complete. Verify anything you
would not want to be wrong about before you rely on it, especially in
security-sensitive or production changes. The whole design philosophy is to
surface uncertainty rather than hide it ([PHILOSOPHY.md](PHILOSOPHY.md)), and
that only protects you if you read the flags it gives you.

The software is provided "as is", without warranty of any kind, and the
authors carry no liability for its use, as set out in the license below.

Contributions are welcome and are accepted under the same MIT license as the
project (inbound = outbound): by opening a pull request you agree your
contribution may be distributed under these terms. See
[CONTRIBUTING.md](CONTRIBUTING.md).

## 📄 License

MIT. See [LICENSE](LICENSE). The MIT permission notice includes an explicit
disclaimer of warranty and limitation of liability; those clauses are the
legally operative protection for both users and authors.
