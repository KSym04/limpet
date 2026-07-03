# SPEC — `limpet update` self-updater

## Core Architecture

`limpet update` fetches the latest published release binary for the current
platform, verifies its SHA-256 against the published checksum, and atomically
replaces the running executable in place. Network is touched **only** on this
explicit command; index / recall / serve never touch the network.

```
limpet update            check -> download -> verify -> self-replace, or "already latest"
limpet update --check    report only: newer? print target version. exit 0 = up to date, 10 = update available
```

Flow:

```
current = env!("CARGO_PKG_VERSION")
remote  = GET api.github.com/repos/KSym04/limpet/releases/latest -> tag_name (strip leading 'v')
if remote <= current: print "already on latest (x)"; exit 0
asset   = resolve_asset(OS, ARCH)            # raw binary, not the archive
bin     = GET <release>/download/<asset>
sha     = GET <release>/download/<asset>.sha256
verify sha256(bin) == sha  (else abort, touch nothing)
self_replace(bin)                            # atomic; Windows-safe
print "updated {current} -> {remote}. restart Claude Code to reload the MCP server."
```

## State / Data Model

No persistent state. Downloads land in a temp dir and are discarded after the
atomic replace. The only mutated artifact is the on-disk executable at
`std::env::current_exe()`.

Asset name map (raw binaries, added to the release workflow):

| OS      | ARCH    | asset                          |
|---------|---------|--------------------------------|
| macos   | aarch64 | `limpet-aarch64-apple-darwin`  |
| macos   | x86_64  | `limpet-x86_64-apple-darwin`   |
| linux   | x86_64  | `limpet-x86_64-unknown-linux-gnu` |
| windows | x86_64  | `limpet-x86_64-pc-windows-msvc.exe` |

Each asset ships a sibling `<asset>.sha256` (`<hex>  <name>` format).

## INVARIANTS

- I1: never write the executable on checksum mismatch, short read, or HTTP != 200.
- I2: never downgrade — refuse if `remote <= current` (unless `--force`, not in v1).
- I3: replace is atomic — no window where the binary on disk is truncated.
- I4: no network on any command other than `update` / `update --check`.

## ATTACK SURFACE

- MITM / tampered asset -> HTTPS (rustls) + published SHA-256 verified before install.
- Downgrade attack -> I2 refuses non-greater remote.
- Wrong-arch asset -> `resolve_asset` hard-fails when OS/ARCH has no mapping.
- Partial download -> read to end, length + checksum checked before replace (I1/I3).
- Running MCP server stays on the old binary in memory -> POST message tells the
  user to restart Claude Code (same gotcha seen during the 0.3.0 hang debug).

## TECH STACK DEPS

- `ureq` (default-features off, `tls` = rustls) — blocking HTTPS, no tokio/hyper.
- `self_replace` — cross-platform atomic self-replace.
- `serde_json` (already present) — parse the releases API response.
- `sha2` (already present) — checksum verify.
- Release workflow additionally uploads raw `limpet-<target>` binaries + `.sha256`.

## Task Implementation Checklist

- [x] Cargo.toml: add `ureq` (rustls), `self-replace`; bump version 0.3.0 -> 0.4.0
- [x] `src/update.rs`: `run(check: bool)`, version compare, asset resolve, download, verify, self-replace
- [x] `src/main.rs`: wire `update` subcommand (+ `--check`), add to HELP
- [x] `.github/workflows/release.yml`: upload raw binary + `.sha256` per target
- [x] `src/skill.md`: document `/limpet update`
- [x] `server.json`: version 0.4.0
- [x] Build + `cargo test --locked` green
- [ ] Branch -> PR -> merge -> tag v0.4.0

## Security hardening: no-secret-leak guarantee

Threat: an agent calls `remember` with a credential in the body or evidence.
It would persist to the local store and later leak through
`admin export` -> `.limpet/memory.jsonl` -> `git push`.

Control: `src/secrets.rs::detect` runs on the write path in `memory::remember`.
It refuses (hard error, no write) any body or evidence output containing a
provider-specific credential: AWS/ASIA keys, GitHub tokens, Slack tokens,
OpenAI/Stripe secret keys, Google API keys, PEM private-key blocks, and JWTs.
High-precision (prefix + length + charset) to avoid tripping on prose. No new
dependency.

Out of scope for v1 (documented, not yet built):
- Release-binary signing (updater trusts a same-origin GitHub checksum; a
  release-signing key would defend a compromised release).
- Entropy-based generic secret detection (higher false-positive rate).
