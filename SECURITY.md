# Security Policy

limpet reads your source code and writes MCP registration into your agent
configuration. That is its entire job, and it is designed so that nothing
beyond that is possible.

## Guarantees, enforced in code and tests

- **No network.** The codebase contains no HTTP client, no DNS, no sockets
  except the visual memory server, which binds 127.0.0.1 only, answers GET
  only, and serves one embedded document. There is no telemetry, no update
  check, no cloud. Verify: `grep -rn "reqwest\|TcpStream::connect" src/`.
- **No shell.** The only external process is `git`, invoked with argument
  arrays. No user input ever reaches a shell interpreter.
- **Path confinement.** Every file path arriving over MCP passes one
  validation choke point (`util::validate_rel_path`) that rejects absolute
  paths and traversal. Covered by unit tests and by integration tests that
  attack the running server.
- **Parameterized SQL only.** No query concatenates user input.
- **Hostile input survives.** The JSON-RPC loop answers malformed lines and
  handler panics with JSON-RPC errors and keeps serving; the test suite
  includes a fuzz pass of hostile input.
- **Config writes are surgical.** `limpet install` upserts exactly one key
  (`mcpServers.limpet`) and refuses to touch a config file whose shape it
  does not recognize. `--dry-run` previews the exact change. `uninstall`
  removes exactly that key.
- **Your data stays yours.** One SQLite file per repository under your
  local data directory. Export is plain JSONL you can read line by line.

## Reporting a vulnerability

Open a private security advisory on GitHub
(Security tab, "Report a vulnerability") or email the maintainer. Please
include a reproduction. You will get an acknowledgement within 72 hours.
Fixes for confirmed issues in the current release ship as a patch release
with a changelog entry crediting the reporter (unless anonymity is asked).

## Scope

In scope: anything reachable through the MCP stdio interface, the local UI
server, the CLI, or the store file format.

Out of scope: attacks requiring an already-compromised local account, and
the security of the coding agent connected to limpet.
