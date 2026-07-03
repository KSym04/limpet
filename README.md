# limpet

**small shell. long memory.**

Memory that clamps onto your code. Code moves, memory follows. Code changes, memory says so.

limpet is a memory-first code intelligence MCP server for AI coding agents. Everything your agent learns about a project (decisions, verified facts, failed approaches, gotchas, intent) is stored as durable memory, anchored to the actual code it describes, and automatically flagged the moment that code changes. One Rust binary, one SQLite file, 100% local.

The name is the mechanism: a limpet clamps to one spot and returns to it after every tide. Memories here clamp onto AST-hashed symbols, follow them through renames and file moves, and go visibly stale when the code underneath them actually changes.

## Why this exists

Code indexers answer "what is where" and answer it well. But the expensive knowledge is not in any file:

- why the batch size is 50
- which refactor was tried and rolled back, and what broke
- which method names are frozen because customers hook them
- what that weird cron job actually protects against

Agents re-derive or re-ask this every session, burning tokens, or worse, they guess. Session notes and markdown memory files rot silently because nothing connects them to the code they describe. limpet's answer: make memory a first-class store with three properties nobody else combines:

1. **Anchored.** Memories attach to symbols through normalized AST body hashes, not line numbers or file paths. Rename a function or move a file and the memory follows. Edit the function body and the memory flips to stale with a reason.
2. **Honest.** Every response carries a metadata envelope: how fresh the index is, how many results matched vs how many were returned, and how much of what you got is stale or contradicted. There is no code path that truncates silently.
3. **Evidenced.** A fact can carry the command that proved it. When its anchor goes stale, limpet hands the agent the exact command to re-verify it.

## Token savings, measured

Where the savings actually come from:

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

## The six tools

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

## The anchor lifecycle

```
code change            anchor resolution         memory becomes
---------------------  ------------------------  -----------------------------
reformat / comments    same normalized AST hash  active (untouched)
rename symbol          body found under new FQN  active, anchor follows
move file              body found in new file    active, anchor follows
edit function body     hash differs at FQN       stale (body_edited), conf drops
delete symbol          body found nowhere        invalidated (kept as history)
duplicate bodies       multiple matches          stale (ambiguous_anchor)
```

Contradictions are explicit links: when a new memory contradicts an old one, both stay visible with the conflict flagged until one `supersedes` the other. History is never silently overwritten.

## Visual memory

```bash
limpet ui --port 9748
```

Open http://127.0.0.1:9748 for a live force-directed view of the knowledge graph: memories sized by confidence and colored by health (green active, amber stale, red invalidated), clamped to the files and symbols they describe, with contradiction and supersession edges drawn. The "needs attention" filter shows exactly what went stale and why, with the re-verify command one click away. Where other tools visualize code structure, limpet visualizes what your agent knows and whether it is still true. Served by the same single binary, bound to 127.0.0.1 only.

## Install

Requires the Rust toolchain ([rustup.rs](https://rustup.rs)) until prebuilt binaries ship.

```bash
cargo install --git https://github.com/KSym04/limpet limpet
limpet install
```

The first command builds and places the `limpet` binary in `~/.cargo/bin`. The second registers it with Claude Code (user scope, `~/.claude.json`); add `--dry-run` to preview the exact config change before it is written. Restart Claude Code and say "index this project".

From a clone instead:

```bash
git clone https://github.com/KSym04/limpet
cd limpet
cargo install --path .
limpet install
```

Verify the setup any time:

```bash
limpet index --root /path/to/your/repo   # one-off index, prints counts
limpet status --root /path/to/your/repo  # index + memory counts
```

Data lives under `~/.local/share/limpet/`, one SQLite store per repository. `limpet uninstall` removes the Claude Code registration and touches nothing else.

Team sharing without binary blobs: `limpet export` writes `.limpet/memory.jsonl`, plain text, diffable, and git-mergeable. Teammates run `limpet import`.

## Thin index, on purpose

limpet ships tree-sitter grammars for PHP, JavaScript, TypeScript, Python, and Rust. The index extracts symbols, imports, and name-based call references labeled `syntactic`. There is no LSP, no type inference, and no claim of a publishable call graph: the index exists to give memory anchor points, invalidation, and recall locality. Every shipped grammar has fixture coverage in the test suite; languages are added when they can be tested, not when they pad a number.

Freshness model: every tool call runs a bounded incremental sweep (changed files reparse in milliseconds via tree-sitter). Queries never block on indexing; anything still dirty is listed in the envelope.

## Security posture

- **100% local.** No network calls anywhere in the codebase, no telemetry, no API keys, no cloud. The UI binds 127.0.0.1 and serves one embedded page, GET only.
- **No shell interpolation.** External commands (git only) run with argument arrays; no string ever reaches a shell.
- **Path validation.** Every file path arriving over MCP is checked against the repository root; absolute paths and traversal are rejected at a single choke point.
- **Parameterized SQL only.** No query in the codebase concatenates user input.
- **Malformed input survives.** The JSON-RPC loop answers parse errors and handler panics with JSON-RPC errors and keeps serving.
- `install` edits only its own `mcpServers.limpet` entry and refuses to touch config it does not recognize; `uninstall` reverses exactly that.

## What limpet is not

- Not a code search engine. Your agent already has grep.
- Not a call-graph oracle. Call edges are syntactic and labeled as such.
- Not a cloud memory platform. No account, no sync, no server.
- Retrieval quality tracks what gets written: short, specific, anchored memories recall well, and the tool schemas steer agents toward exactly that.

## Roadmap

- SessionEnd/Stop hook helpers for automatic episode mining from transcripts
- More grammars (Go, Java, Ruby, C#, C/C++, Bash), each with fixture coverage before shipping
- FS-event watcher (notify) to replace the on-call sweep on very large repos
- Optional embedding reranking behind a feature flag, only if recall evals prove it earns its size

## License

MIT
