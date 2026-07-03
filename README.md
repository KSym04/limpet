# 🐚 limpet

**small shell. long memory.**

[![CI](https://github.com/KSym04/limpet/actions/workflows/ci.yml/badge.svg)](https://github.com/KSym04/limpet/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-green.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-stable-orange.svg)](https://rustup.rs)
[![Platforms](https://img.shields.io/badge/macOS%20%7C%20Linux%20%7C%20Windows-supported-lightgrey.svg)](https://github.com/KSym04/limpet/actions/workflows/ci.yml)
[![100% local](https://img.shields.io/badge/network%20calls-zero-blue.svg)](SECURITY.md)

**Persistent engineering memory for AI coding agents.** AI forgets everything between sessions. limpet remembers what your agent learned about a codebase, and it knows when that knowledge stops being true.

<p align="center">
  <img src="docs/limpet-ui.png" alt="limpet visual memory graph: memories colored by health, clamped to code symbols" width="820">
  <br>
  <em>limpet ui: green memories are trustworthy, amber went stale when their code changed, squares are the symbols they clamp onto</em>
</p>

Every coding agent runs the same loop: read code, reason, answer, forget. Next session it pays the full price again. And anything it did write down (session notes, markdown memory files) quietly rots, because nothing connects those notes to the code they describe. limpet replaces that loop with a knowledge lifecycle:

```
read -> reason -> remember -> verify -> flag stale when code changes -> re-verify -> reuse
```

Everything your agent learns about a project (decisions, verified facts, failed approaches, gotchas, intent) is stored as durable memory, anchored to the actual code it describes, and automatically flagged the moment that code changes. Ships as an MCP server, so any MCP-capable agent can use it: one Rust binary, one SQLite file, 100% local.

The name is the mechanism: a limpet clamps to one spot and returns to it after every tide. Memories here clamp onto AST-hashed symbols, follow them through renames and file moves, and go visibly stale when the code underneath them actually changes.

## 🧠 Why this exists

Code indexers answer "what is where" and answer it well. But the expensive knowledge is not in any file:

- why the batch size is 50
- which refactor was tried and rolled back, and what broke
- which method names are frozen because customers hook them
- what that weird cron job actually protects against

Agents re-derive or re-ask this every session, burning tokens, or worse, they guess. And the failure mode that actually kills AI coding assistants is not forgetting; it is remembering something that is no longer true. An agent that still believes last month's version of a function is not unhelpful, it is confidently wrong, and every generic memory store on the market will feed it that lie forever, because in those systems knowledge is written once and trusted forever.

limpet's premise is that a memory about code is only trustworthy while that code still matches it. So knowledge here has a lifecycle, the same one engineers give it:

```
valid -> code changed -> stale (reason attached, confidence drops) -> re-verified or superseded
```

That takes three properties nobody else combines:

1. **Anchored.** Memories attach to symbols through normalized AST body hashes, not line numbers or file paths. Rename a function or move a file and the memory follows. Edit the function body and the memory flips to stale with a reason.
2. **Honest.** Every response carries a metadata envelope: how fresh the index is, how many results matched vs how many were returned, and how much of what you got is stale or contradicted. There is no code path that truncates silently.
3. **Evidenced.** A fact can carry the command that proved it. When its anchor goes stale, limpet hands the agent the exact command to re-verify it.

## 👥 Who is this for

**Solo developers working with an AI agent.** The first session spends tens of thousands of tokens learning your codebase; every session after that, the knowledge is one `recall` away. Survives `/clear`, compaction, and new machines. No re-teaching your own project, and no agent confidently repeating last month's truth about a function you rewrote yesterday.

**Teams.** `admin export` writes `.limpet/memory.jsonl` for git; teammates import after pulling. Onboarding knowledge ("why the batch size is 50", "customers hook these method names, they are frozen") travels with the repo, and unlike a wiki it flags itself when the code moves on. `affected` tells a committer which documented decisions their diff just put at risk.

**Template-heavy web stacks** (WordPress/Timber, Laravel Blade, Rails ERB, Vue). Every file is anchorable, so "the hero block is locked to 480px by the design system" pins to the actual `.twig` or `.scss` file and goes stale when someone edits it. On these stacks that is where most of the knowledge worth keeping lives.

**Open-source maintainers.** `intent` and `decision` memories answer "why is this weird code here" before the PR that "fixes" it lands; `episode` memories stop the third contributor from re-attempting the refactor that already broke things twice.

**Agent builders.** A model-agnostic memory backend behind a standard MCP interface: one binary, zero network calls, inspectable SQLite. Nothing to explain to a security review.

**Who should skip it:** throwaway scripts, repos you touch once, teams that do not use AI agents. Memory pays off only when questions repeat.

## 🧰 The six tools

| Tool | What it does |
|---|---|
| `recall` | Task description in, token-budgeted ranked memory pack out. Stale and contradicted items are always flagged, never hidden. |
| `remember` | Store a memory: `fact`, `decision`, `episode`, `insight`, or `intent`. Anchor it to code. Attach evidence to make it verified. |
| `map` | Structural outline of a file or symbol plus every memory attached to it. Code and knowledge in one answer. |
| `affected` | What does my uncommitted diff touch: symbols, memories now at risk, and decisions constraining the code being edited. |
| `verify_queue` | Verified facts whose anchored code changed, each with the exact command that originally proved it. |
| `admin` | index, status, forget, export, import. |

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
```

A multi-anchor memory dies only when **every** anchor dies. Losing one anchor while others still resolve degrades it to `stale:anchor_lost` so the surviving knowledge stays usable. And `remember` refuses an anchor it cannot resolve, loudly, at write time: no memory is ever born dead.

Staleness is also symmetric: revert the code (a rolled-back experiment, a `git checkout`) and the memory heals back to active on the next call, because the anchor hash matches again. No re-verification ritual for changes that un-happened. Invalidation is one-way by design: a memory whose every anchor was deleted stays invalidated as history even if the files come back; store a fresh memory instead.

Contradictions are explicit links: when a new memory contradicts an old one, both stay visible with the conflict flagged until one `supersedes` the other. History is never silently overwritten.

## 🪙 The receipts: token savings, measured

Persistent memory also happens to be dramatically cheaper than re-derivation. Where the savings come from:

1. **Answers travel as conclusions, not source.** A memory is the 30-token distilled answer; the files it was learned from are thousands of tokens. Reading code to re-derive a known fact pays the full price every single session. Recall pays once per question.
2. **The worst spend is exploration that cannot succeed.** Why a batch size is 50, which refactor was rolled back, which API is frozen: not in any file. Without memory the agent greps, reads, and still does not know, so the tokens bought nothing. With memory these are the cheapest questions of all.
3. **Responses are budget-packed and noise-cut.** `recall` takes a token budget, packs best-first, drops the low-relevance tail, and reports what it omitted. You spend what you allowed, never what happened to match.
4. **No re-teaching after context loss.** Compaction, `/clear`, new session: the knowledge survives outside the context window and comes back at recall prices, not re-derivation prices.

Measured with a reproducible benchmark, seeded with 12 memories over a realistic 9-file fixture plugin, asking 10 questions an agent typically re-answers every session:

```
question                                                   files+grep   recall   ratio  in code?
------------------------------------------------------------------------------------------------
why is the batch size 50 and why is there a queue at all         1929      363    5.3x  no
why does the scanner skip draft products, is that a bug          1630      373    4.4x  no
how is the health score computed                                 1630      349    4.7x  yes
why semicolon delimiter and BOM in the csv export                1327      361    3.7x  no
where do report files get written and why                        1327      356    3.7x  no
how long are download tokens valid                               1023      165    6.2x  yes
has anyone tried streaming the csv export                        1327      369    3.6x  no
can I rename check_product in the scanner                        1630      373    4.4x  no
what does the nightly cron actually exist for                     803      313    2.6x  no
how often does the dashboard poll progress, can I lower it       1072      336    3.2x  no
------------------------------------------------------------------------------------------------
TOTAL                                                           13698     3373    4.1x
```

**4.1x fewer tokens (75.6% saved) across the benchmark.** Reproduce it yourself:

```bash
cargo build --release
python3 bench/token_savings.py
```

Methodology, stated so the number can be checked rather than believed:

- "Without limpet" cost is the **minimal** file set containing the answer plus a flat 300 tokens for search round trips. Real agents read more than the minimal set, so real savings are higher.
- Tokens are estimated as ceil(bytes/4) on both sides identically.
- 8 of the 10 questions are marked "no" above: their answers exist in **no file at any token price** (decisions, history, tribal knowledge). File reading gets you the code but not the answer. We still charge limpet full price against the file-reading cost instead of claiming infinite savings.
- The script is a regression gate: it exits nonzero if savings drop below 4x.
- Fixture files are 58 to 179 lines. Real plugin files run several times larger, and the "without" side grows with file size while a recall response does not.

## 🗺️ Visual memory

```bash
limpet ui --port 9748
```

The UI is its own command, separate from the MCP server. `limpet serve` (stdio) is what Claude Code launches for you automatically after `limpet install`; nothing listens on a port until you start `limpet ui` yourself. If http://127.0.0.1:9748 refuses connections, the MCP server is not broken; the UI just is not running.

Open http://127.0.0.1:9748 for a live force-directed view of the knowledge graph: memories sized by confidence and colored by health (green active, amber stale, red invalidated), clamped to the files and symbols they describe, with contradiction and supersession edges drawn. The "needs attention" filter shows exactly what went stale and why, with the re-verify command one click away. Where other tools visualize code structure, limpet visualizes what your agent knows and whether it is still true. Served by the same single binary, bound to 127.0.0.1 only.

## 📦 Install

**Prebuilt binary** (macOS, Linux, Windows): download from the [latest release](https://github.com/KSym04/limpet/releases/latest), verify the sha256, put `limpet` on your PATH, then:

```bash
limpet install
```

**With Rust** ([rustup.rs](https://rustup.rs)):

```bash
cargo install --git https://github.com/KSym04/limpet limpet
limpet install
```

Restart Claude Code. Done. (`limpet install --dry-run` previews the exact config changes first; `limpet uninstall` reverses them.)

**Update later** with `limpet update`: it fetches the latest release binary for your platform, verifies it against the published sha256, and atomically replaces the running executable. `limpet update --check` reports whether a newer version exists without installing it. This is the only command that touches the network. Restart Claude Code afterward so the MCP server reloads onto the new binary.

## 🚀 Usage in 60 seconds

**1. In any project, type `/limpet`.** It indexes the code, recalls everything already known (stale items flagged), and switches the session to memory-first mode.

**2. Just work.** The agent now stores what it learns as it learns it:

> "The scanner batch size is 50 because shared hosts kill long requests" becomes a `decision`, anchored to the function that uses it.

**3. Next session, in a fresh context, ask anything it ever learned:**

> you: why is the batch size 50?
> agent: (one `recall` call, ~350 tokens) shared hosts kill requests over 30 seconds; the queue exists so a full scan survives across requests.

No file spelunking, no re-explaining your own codebase.

**4. Change the code and memory reacts.** Edit that function and the memory flips to `stale: body_edited` everywhere it appears. Rename or move the function and the memory follows it silently. Nothing ever pretends to be current when it is not.

Everyday commands:

| Command | Does |
|---|---|
| `/limpet` | index + recall + memory-first mode for the session |
| `/limpet status` | counts and anything needing attention |
| `/limpet review` | re-verify stale facts using their stored proof commands |
| `/limpet export` | write `.limpet/memory.jsonl` to commit and share with the team |
| `limpet ui` | knowledge graph at http://127.0.0.1:9748, all projects in one view |
| `limpet update` | self-update to the latest release, checksum-verified (the only networked command) |

Data lives under `~/.local/share/limpet/`, one SQLite store per repository. Teammates run `limpet import` after pulling the JSONL.

## 🌳 Whole repo indexed, thin on purpose

**Every file in the repository is indexed and anchorable.** Files with a shipped grammar (PHP, JavaScript, TypeScript, Python, Rust) get full symbol extraction: functions, classes, imports, and name-based call references labeled `syntactic`. Every other file (`.twig`, `.scss`, `.vue`, `.blade.php`, `.md`, `.yml`, configs, anything) gets a file-level node with a content hash, so a memory can anchor to it and go `stale:file_edited` the moment it changes. On template-heavy stacks (WordPress/Timber, Rails, Laravel) that is where the knowledge worth remembering actually lives.

What the walk skips, deliberately: everything in `.gitignore`, everything in an optional `.limpetignore` (gitignore syntax, works even outside a git repo), `node_modules`/`vendor`/`target`/`dist`/`build`, hidden files, `*.min.*` assets, and files over 512KB. Those bounds are what keep a full WordPress install from pegging your CPU; use `.limpetignore` to opt out anything else.

There is no LSP, no type inference, and no claim of a publishable call graph: the index exists to give memory anchor points, invalidation, and recall locality. Every shipped grammar has fixture coverage in the test suite; languages are added when they can be tested, not when they pad a number.

Freshness model: every tool call runs a bounded incremental sweep (changed files reparse in milliseconds via tree-sitter). Queries never block on indexing; anything still dirty is listed in the envelope.

## 🔒 Security posture

- **Local by default.** Indexing, recall, memory, and the UI make no network calls, ever: no telemetry, no API keys, no cloud. The one exception is `limpet update`, which you invoke explicitly; it fetches a checksum-verified release binary over HTTPS and sends nothing but a `limpet/<version>` User-Agent. The UI binds 127.0.0.1 and serves one embedded page, GET only.
- **Secrets never persist.** `remember` scans every body and evidence output and refuses to store anything shaped like a credential (cloud access keys, provider tokens, PEM private-key blocks, JWTs), so a secret cannot reach the local store or a shared `.limpet/memory.jsonl`.
- **No shell interpolation.** External commands (git only) run with argument arrays; no string ever reaches a shell.
- **Path validation.** Every file path arriving over MCP is checked against the repository root; absolute paths and traversal are rejected at a single choke point.
- **Parameterized SQL only.** No query in the codebase concatenates user input.
- **Malformed input survives.** The JSON-RPC loop answers parse errors and handler panics with JSON-RPC errors and keeps serving.
- `install` edits only its own `mcpServers.limpet` entry and refuses to touch config it does not recognize; `uninstall` reverses exactly that.

## 🚫 What limpet is not

- Not a generic AI memory store, vector database, or RAG pipeline. No embeddings, no similarity guesswork: anchors are deterministic AST hashes, and staleness is a fact, not a score. Code-unaware memory stores never notice when the code moves on; noticing is limpet's entire premise.
- Not a code search engine. Your agent already has grep.
- Not a call-graph oracle. Call edges are syntactic and labeled as such.
- Not a cloud memory platform. No account, no sync, no server.
- Retrieval quality tracks what gets written: short, specific, anchored memories recall well, and the tool schemas steer agents toward exactly that.

## 🧭 Roadmap

- SessionEnd/Stop hook helpers for automatic episode mining from transcripts
- More grammars (Go, Java, Ruby, C#, C/C++, Bash), each with fixture coverage before shipping
- FS-event watcher (notify) to replace the on-call sweep on very large repos
- Optional embedding reranking behind a feature flag, only if recall evals prove it earns its size
- Signed release binaries (minisign) so `limpet update` verifies a maintainer signature, not just a same-origin checksum

## 📄 License

MIT
