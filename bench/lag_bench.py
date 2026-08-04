#!/usr/bin/env python3
"""FS-event watcher lag bench for limpet. Stdlib only, fully local.

Answers the ROADMAP watcher question with numbers: is the per-call bounded
sweep (stat pass + discover walk on EVERY tool call) fast enough, and does
anchored-file prioritization keep staleness latency at one tool call, so
that a filesystem-event watcher stays unbuilt?

Methodology, three measurements kept strictly separate:

  1. Quiet-repo per-call sweep overhead: on a fully indexed repo with zero
     pending changes, time N recall calls end to end over stdio JSON-RPC
     and report p50/p95 wall milliseconds. Nothing is dirty, so this
     isolates the O(repo) cost paid on every call: the discover() walk plus
     the stat pass over every known file.

  2. Anchored-staleness latency in TOOL CALLS, not seconds: modify the body
     of K files (K in 1, 32, 100), including the anchored function body of
     min(4, K) files that carry memories, then issue recall calls and count
     how many calls it takes until every touched anchored file's memory
     reports stale. The sweep sorts anchored files first, so this should be
     exactly 1 call at every K.

  3. Dirty-backlog drain: modify 500 unanchored files at once (the scale of
     a branch switch), then count recall calls until the honesty envelope
     reports dirty=0. Expected: ceil(500 / observed_reindex_batch) calls.
     The mean per-call wall time during the drain, against the quiet p50,
     shows the incremental reindex cost per call. Informational, not gated.

Synthetic repos are generated deterministically from counters (no wall
clock, no randomness in content): 60 percent Python, 30 percent Rust,
10 percent JavaScript, nested directories up to 5 levels deep, small
realistic bodies with globally unique function names. Every "touch"
rewrites the file with a bumped revision counter that changes a string
literal inside the anchored function body (flips the normalized AST body
hash) and changes the file length (consecutive revisions always differ in
size, so mtime granularity can never hide a touch).

Verdict criteria (exit 0 only if all hold):
  - anchored staleness latency == 1 call at every repo size and every K
  - quiet-repo p50 < 250 ms at 50000 files
  - no integrity failure (untouched anchored memories must stay fresh,
    seeded memories must all be recallable)
A run without the 50000-file size is INCOMPLETE and exits 1: it proves the
mechanics but cannot issue the verdict. Exit 2 means the harness itself
failed (protocol timeout, server death), not a gate result. Exit 3 means a
per-invocation time budget ran out with progress saved: rerun the same
phase to continue (see --budget-seconds).

Indexing runs through the binary's own bounded sweep (32 files per tool
call) until dirty=0, which reaches the same fully indexed store as a full
index but is naturally chunked and resumable between invocations.

Run everything:
  python3 bench/lag_bench.py [--binary PATH] [--sizes 2000,10000,50000]
                             [--scratch DIR]

Or one bounded phase at a time (requires --scratch so state persists;
each phase is resumable and idempotent per size):
  python3 bench/lag_bench.py --scratch DIR --sizes 50000 --phase generate
  python3 bench/lag_bench.py --scratch DIR --sizes 50000 --phase index
  python3 bench/lag_bench.py --scratch DIR --sizes 50000 --phase measure
  python3 bench/lag_bench.py --scratch DIR --sizes 2000,10000,50000 --phase verdict

The live limpet store is never touched: every server runs with
LIMPET_DATA_DIR pointing into the scratch directory, exactly like
bench/token_savings.py. The stdio protocol mirrors token_savings.py
byte-for-byte: newline-delimited JSON-RPC, a single initialize request,
no framing headers; the only addition is a deadline on every read.
"""

import argparse
import json
import math
import os
import queue
import shutil
import statistics
import subprocess
import sys
import tempfile
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))

# Verdict thresholds (the ROADMAP watcher gate).
QUIET_P50_LIMIT_MS = 250.0
GATE_SIZE = 50_000
ANCHOR_COUNT = 12
QUIET_CALLS = 10
LATENCY_KS = (1, 32, 100)
LATENCY_CALL_CAP = 20
BULK_CHANGE = 500
DRAIN_CALL_CAP = 80
RECALL_TASK = "lagbench receipt batch scaling decision"
RECALL_BUDGET = 20_000

# Read deadlines, seconds. Generous: a deadline is a hang detector, not a
# performance assertion; taxed environments make single calls legitimately
# slow (see read_open_tax_ms).
CALL_TIMEOUT_S = 300
INIT_TIMEOUT_S = 60
GIT_STEP_TIMEOUT_S = 120

MEMORY_KINDS = ("decision", "insight", "fact", "episode", "intent")

# Directory name stems for the nested layout; digits are appended, so none
# of these can collide with the walker's generated-dir skip list.
DIR_STEMS = ("core", "svc", "mod", "sub", "leaf")


class BenchProtocolError(RuntimeError):
    """The server hung or died; the bench cannot continue. Exit 2 class."""


def grammar_for(i: int) -> str:
    """Deterministic grammar mix: 60 percent py, 30 percent rs, 10 percent js."""
    slot = i % 10
    if slot < 6:
        return "py"
    if slot < 9:
        return "rs"
    return "js"


def rel_dir(i: int) -> str:
    """Deterministic nested directory, 0 to 5 levels deep."""
    depth = i % 6
    parts = []
    v = i
    for level in range(depth):
        parts.append(f"{DIR_STEMS[level]}{v % 7}")
        v //= 7
    return "/".join(parts)


def rel_path(i: int) -> str:
    d = rel_dir(i)
    name = f"unit_{i:06d}.{grammar_for(i)}"
    return f"{d}/{name}" if d else name


def file_body(i: int, rev: int) -> str:
    """Deterministic file content for unit i at revision rev.

    rev feeds a string literal INSIDE fn_<i> (an identity leaf in the AST
    body hash, so symbol anchors flip stale) and a trailing pad line whose
    length changes on every consecutive revision (so file size always
    changes and mtime granularity can never mask a touch).
    """
    tag = f"{i:06d}"
    mult = (i + rev) % 97 + 1
    label = f"batch-{tag}-r{rev}"
    pad = "x" * (rev % 7 + 1)
    kind = grammar_for(i)
    if kind == "py":
        return (
            f'"""Unit {tag}: batch metrics for feed {i}."""\n'
            "\n"
            f"def fn_{tag}(x):\n"
            f'    """Scale the feed batch for unit {tag}."""\n'
            f"    total = x * {mult}\n"
            f'    label = "{label}"\n'
            "    return total, label\n"
            "\n"
            f"def aux_{tag}(items):\n"
            f'    """Count truthy items for unit {tag}."""\n'
            "    return sum(1 for it in items if it)\n"
            "\n"
            f"# pad: {pad}\n"
        )
    if kind == "rs":
        return (
            f"//! Unit {tag}: batch metrics for feed {i}.\n"
            "\n"
            f"pub fn fn_{tag}(x: i64) -> (i64, String) {{\n"
            f"    let total = x * {mult};\n"
            f'    let label = String::from("{label}");\n'
            "    (total, label)\n"
            "}\n"
            "\n"
            f"pub fn aux_{tag}(items: &[i64]) -> i64 {{\n"
            "    items.iter().filter(|v| **v > 0).count() as i64\n"
            "}\n"
            "\n"
            f"// pad: {pad}\n"
        )
    return (
        f"// Unit {tag}: batch metrics for feed {i}.\n"
        "\n"
        f"export function fn_{tag}(x) {{\n"
        f"  const total = x * {mult};\n"
        f'  const label = "{label}";\n'
        "  return { total, label };\n"
        "}\n"
        "\n"
        f"export function aux_{tag}(items) {{\n"
        "  return items.filter(Boolean).length;\n"
        "}\n"
        "\n"
        f"// pad: {pad}\n"
    )


def p50(samples):
    return statistics.median(samples)


def p95(samples):
    ordered = sorted(samples)
    idx = min(len(ordered) - 1, math.ceil(0.95 * len(ordered)) - 1)
    return ordered[idx]


def anchored_indices(n: int):
    """ANCHOR_COUNT file indices spread evenly across the repo."""
    spread = sorted({(j * n) // ANCHOR_COUNT for j in range(ANCHOR_COUNT)})
    if len(spread) != ANCHOR_COUNT:
        raise RuntimeError(f"anchored index spread collided at n={n}")
    return spread


class Server:
    """One limpet MCP server over stdio JSON-RPC, store confined to data_dir.

    Every read has a deadline: a reader thread feeds a queue, request()
    waits with a timeout, and a timeout kills the child and raises with the
    last request echoed plus the child's stderr tail. Never blocks forever.
    """

    def __init__(self, binary: str, root: str, data_dir: str, stderr_log: str):
        self.stderr_log = stderr_log
        self.stderr_fh = open(stderr_log, "wb")
        self.proc = subprocess.Popen(
            [binary, "serve", "--root", root],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=self.stderr_fh,
            env={**os.environ, "LIMPET_DATA_DIR": data_dir},
            text=True,
        )
        self.next_id = 0
        self.last_request = ""
        self.lines: "queue.Queue[str]" = queue.Queue()
        self.reader = threading.Thread(target=self._read_loop, daemon=True)
        self.reader.start()

    def _read_loop(self) -> None:
        for line in self.proc.stdout:
            self.lines.put(line)
        self.lines.put("")  # EOF sentinel

    def _stderr_tail(self) -> str:
        try:
            self.stderr_fh.flush()
            with open(self.stderr_log, "r", encoding="utf-8",
                      errors="replace") as f:
                return "".join(f.readlines()[-30:])
        except OSError:
            return "(stderr log unreadable)"

    def _die(self, why: str) -> BenchProtocolError:
        try:
            self.proc.kill()
        except Exception:
            pass
        return BenchProtocolError(
            f"{why}\nlast request: {self.last_request}\n"
            f"server stderr tail:\n{self._stderr_tail()}"
        )

    def request(self, method: str, params=None, timeout_s: int = CALL_TIMEOUT_S):
        self.next_id += 1
        msg = {"jsonrpc": "2.0", "id": self.next_id, "method": method}
        if params is not None:
            msg["params"] = params
        self.last_request = json.dumps(msg)
        try:
            self.proc.stdin.write(self.last_request + "\n")
            self.proc.stdin.flush()
        except (BrokenPipeError, OSError) as e:
            raise self._die(f"server stdin closed while sending '{method}': {e}")
        try:
            line = self.lines.get(timeout=timeout_s)
        except queue.Empty:
            raise self._die(f"no response to '{method}' within {timeout_s}s")
        if not line:
            raise self._die(f"server closed stdout during '{method}'")
        return json.loads(line)

    def call_tool(self, name: str, arguments, timeout_s: int = CALL_TIMEOUT_S):
        resp = self.request(
            "tools/call", {"name": name, "arguments": arguments}, timeout_s
        )
        if "error" in resp:
            raise BenchProtocolError(f"tool '{name}' rpc error: {resp['error']}")
        text = resp["result"]["content"][0]["text"]
        return json.loads(text)

    def close(self) -> None:
        try:
            self.proc.stdin.close()
            self.proc.wait(timeout=10)
        except Exception:
            self.proc.kill()
        finally:
            self.stderr_fh.close()


def size_paths(scratch: str, n: int) -> dict:
    base = os.path.join(scratch, f"size-{n}")
    return {
        "base": base,
        "repo": os.path.join(base, "repo"),
        "data": os.path.join(base, "data"),
        "logs": os.path.join(base, "logs"),
        "state": os.path.join(base, "state.json"),
    }


def load_state(paths: dict) -> dict:
    if os.path.exists(paths["state"]):
        with open(paths["state"], "r", encoding="utf-8") as f:
            return json.load(f)
    return {}


def save_state(paths: dict, state: dict) -> None:
    tmp = paths["state"] + ".tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(state, f, indent=1)
    os.replace(tmp, paths["state"])


def make_prog(n: int):
    t_start = time.perf_counter()

    def prog(msg: str) -> None:
        print(f"  [{n}] +{time.perf_counter() - t_start:7.1f}s  {msg}",
              flush=True)

    return prog


def start_server(binary: str, paths: dict, prog) -> Server:
    prog("spawning server (LIMPET_DATA_DIR confined to scratch)")
    srv = Server(binary, paths["repo"], paths["data"],
                 os.path.join(paths["logs"], "server-stderr.log"))
    # Same handshake as token_savings.py: one initialize request, newline
    # JSON, nothing else.
    srv.request(
        "initialize",
        {"protocolVersion": "2025-06-18", "capabilities": {}},
        timeout_s=INIT_TIMEOUT_S,
    )
    prog("initialize handshake ok")
    return srv


# --------------------------------------------------------------------------
# git seeding: best effort. The store is keyed off the root PATH (see
# token_savings.py, whose fixture repo is not its own git repo), so git is
# realism, not a requirement. Some monitored environments tax unlink and
# rename syscalls by hundreds of ms each, which makes git object writes
# (one rename per object) pathologically slow, so git seeding is probed
# first and every step gets a hard timeout with a documented no-git
# fallback instead of a silent stall. The bench measurements themselves
# only write, stat, and walk, which stay fast under that tax.
# --------------------------------------------------------------------------

def fs_op_tax_ms(base: str) -> float:
    """Mean rename+unlink cost in ms, measured on a few probe files."""
    probe_dir = os.path.join(base, ".fs-probe")
    os.makedirs(probe_dir, exist_ok=True)
    n = 3
    t0 = time.perf_counter()
    for i in range(n):
        a = os.path.join(probe_dir, f"a{i}")
        b = os.path.join(probe_dir, f"b{i}")
        with open(a, "w", encoding="utf-8") as f:
            f.write("probe")
        os.rename(a, b)
        os.unlink(b)
    os.rmdir(probe_dir)
    return (time.perf_counter() - t0) * 1000 / (2 * n)


def read_open_tax_ms(base: str) -> float:
    """Mean read-open cost in ms. Security-monitored environments have been
    observed to stall EVERY read-open by ~hundreds of ms with no caching;
    that pollutes wall-clock columns (index_ms, drain_mean_ms, and any
    phase that re-reads touched files) while leaving call-count metrics
    (staleness latency, drain calls) and the stat-only quiet sweep valid.
    Measured and reported so a run is honest about its environment."""
    probe_dir = os.path.join(base, ".read-probe")
    os.makedirs(probe_dir, exist_ok=True)
    p = os.path.join(probe_dir, "r0")
    with open(p, "w", encoding="utf-8") as f:
        f.write("probe")
    n = 3
    t0 = time.perf_counter()
    for _ in range(n):
        with open(p, "rb") as f:
            f.read()
    tax = (time.perf_counter() - t0) * 1000 / n
    os.unlink(p)
    os.rmdir(probe_dir)
    return tax


def try_git_seed(repo: str, log_path: str, prog) -> bool:
    tax = fs_op_tax_ms(repo)
    if tax > 50:
        prog(f"unlink/rename tax detected ({tax:.0f} ms per op); git object "
             f"writes would crawl, so continuing WITHOUT git (path-keyed "
             f"store, see docstring)")
        return False
    env = {
        **os.environ,
        "GIT_CONFIG_GLOBAL": "/dev/null",
        "GIT_CONFIG_SYSTEM": "/dev/null",
        "GIT_TERMINAL_PROMPT": "0",
    }
    steps = [
        ["init", "-q"],
        ["add", "-A"],
        ["-c", "user.name=bench", "-c", "user.email=bench@example.invalid",
         "-c", "commit.gpgsign=false", "commit", "-qm", "seed"],
    ]
    with open(log_path, "ab") as log:
        for args in steps:
            log.write(f"$ git {' '.join(args)}\n".encode())
            log.flush()
            try:
                result = subprocess.run(
                    ["git", "-C", repo] + args,
                    env=env, stdout=log, stderr=subprocess.STDOUT,
                    timeout=GIT_STEP_TIMEOUT_S,
                )
            except subprocess.TimeoutExpired:
                prog(f"git {args[0]} timed out after {GIT_STEP_TIMEOUT_S}s; "
                     f"continuing WITHOUT git (path-keyed store, see docstring)")
                return False
            if result.returncode != 0:
                prog(f"git {args[0]} exited {result.returncode}; "
                     f"continuing WITHOUT git (path-keyed store, see docstring)")
                return False
    return True


# --------------------------------------------------------------------------
# Phases
# --------------------------------------------------------------------------

def phase_generate(scratch: str, n: int) -> None:
    prog = make_prog(n)
    paths = size_paths(scratch, n)
    prog(f"PHASE generate: start ({n} files, 60/30/10 py/rs/js, "
         f"nested to 5 levels)")
    if os.path.exists(paths["repo"]):
        # Overwrite in place rather than rmtree: the generator is
        # deterministic over the same file set, and mass unlinks are the
        # one operation monitored environments tax brutally. The store is
        # a handful of files, so IT is reset by deletion (a fresh repo
        # must never inherit a previous run's memories).
        prog("existing repo found; overwriting all files in place (rev 0)")
        if os.path.exists(paths["data"]):
            shutil.rmtree(paths["data"], ignore_errors=True)
    for d in (paths["repo"], paths["data"], paths["logs"]):
        os.makedirs(d, exist_ok=True)

    made_dirs = set()
    for i in range(n):
        rel = rel_path(i)
        d = os.path.dirname(rel)
        if d and d not in made_dirs:
            os.makedirs(os.path.join(paths["repo"], d), exist_ok=True)
            made_dirs.add(d)
        with open(os.path.join(paths["repo"], rel), "w", encoding="utf-8") as f:
            f.write(file_body(i, 0))
        if (i + 1) % 10_000 == 0:
            prog(f"generated {i + 1}/{n} files")
    prog(f"generated {n}/{n} files")

    prog("git seed: init + add + commit (hard timeouts, no-git fallback)")
    git_ok = try_git_seed(
        paths["repo"], os.path.join(paths["logs"], "git.log"), prog
    )
    prog(f"git seed {'ok' if git_ok else 'SKIPPED'}")
    save_state(paths, {"size": n, "git_ok": git_ok})
    prog("PHASE generate: done")


class PhaseIncomplete(RuntimeError):
    """The per-invocation time budget ran out mid-phase; state is saved and
    a rerun of the same phase continues where this one stopped. Exit 3."""


def phase_index(binary: str, scratch: str, n: int, budget_s: int) -> None:
    """Index the repo through the binary's own bounded sweep, one 32-file
    batch per tool call, until the envelope reports dirty=0. Identical end
    state to a full index, but naturally chunked: the phase saves progress
    and can be rerun to continue, so no single invocation is unbounded.
    """
    prog = make_prog(n)
    paths = size_paths(scratch, n)
    state = load_state(paths)
    if not state:
        raise RuntimeError(f"size {n}: run --phase generate first")
    if state.get("index_done"):
        prog("PHASE index: already complete, nothing to do")
        return
    prog("PHASE index: start (sweep-drain, 32 files per call, resumable)")
    tax = read_open_tax_ms(paths["base"])
    state["index_read_tax_ms"] = round(tax, 1)
    prog(f"environment read-open tax: {tax:.1f} ms per open"
         + (" (WALL-CLOCK COLUMNS POLLUTED)" if tax > 20 else ""))

    calls = state.get("index_calls", 0)
    elapsed = state.get("index_elapsed_s", 0.0)
    t_budget = time.perf_counter()
    srv = start_server(binary, paths, prog)
    try:
        while True:
            t0 = time.perf_counter()
            env = srv.call_tool("verify_queue", {}, timeout_s=300)
            elapsed += time.perf_counter() - t0
            calls += 1
            fresh = env["meta"]["freshness"]
            dirty = fresh.get("dirty", 0)
            if calls % 20 == 0 or dirty == 0:
                prog(f"sweep call {calls}: dirty={dirty} "
                     f"(indexing elapsed {elapsed:.1f}s)")
            if dirty == 0:
                break
            if time.perf_counter() - t_budget > budget_s:
                state["index_calls"] = calls
                state["index_elapsed_s"] = elapsed
                save_state(paths, state)
                raise PhaseIncomplete(
                    f"size {n}: index budget ({budget_s}s) spent at "
                    f"dirty={dirty} after {calls} calls; rerun --phase index"
                )
        status = srv.call_tool("admin", {"op": "status"})["data"]
        state.update(
            files=status["files"], symbols=status["symbols"],
            index_ms=int(elapsed * 1000),
            index_calls=calls, index_elapsed_s=elapsed, index_done=True,
        )
        prog(f"indexed {status['files']} files, {status['symbols']} symbols "
             f"in {calls} sweep calls, {elapsed:.1f}s total")
    finally:
        srv.close()
    save_state(paths, state)
    prog("PHASE index: done")


def item_is_stale(item: dict) -> bool:
    if item.get("status") == "stale":
        return True
    return any(f.startswith("stale:") for f in item.get("flags", []))


def recall_status(srv: Server, working_set=None):
    """One recall call; returns (envelope, id_to_item, wall_ms)."""
    args = {"task": RECALL_TASK, "budget_tokens": RECALL_BUDGET}
    if working_set:
        args["working_set"] = working_set
    t0 = time.perf_counter_ns()
    env = srv.call_tool("recall", args)
    ms = (time.perf_counter_ns() - t0) / 1e6
    by_id = {item["id"]: item for item in env["data"]}
    return env, by_id, ms


def drain_dirty(srv: Server, cap: int):
    """Recall until the envelope reports dirty=0. Returns (calls, per-call
    ms list, first observed reindex batch size)."""
    calls = 0
    times = []
    first_batch = None
    while calls < cap:
        env, _, ms = recall_status(srv)
        calls += 1
        times.append(ms)
        fresh = env["meta"]["freshness"]
        if first_batch is None and "reindexed_now" in fresh:
            first_batch = fresh["reindexed_now"]
        if fresh.get("dirty", 0) == 0:
            return calls, times, first_batch
    raise BenchProtocolError(f"dirty backlog did not drain within {cap} calls")


def pick_plain_indices(n: int, anchored: set, start: int, count: int):
    """count unanchored indices scanning forward from start, wrapping."""
    out = []
    i = start % n
    while len(out) < count:
        if i not in anchored:
            out.append(i)
        i = (i + 1) % n
        if i == start % n and len(out) < count:
            raise RuntimeError("not enough unanchored files")
    return out


def touch_files(repo: str, indices, revs: dict) -> None:
    """Rewrite each file with a bumped revision: new mtime, new size, new
    string literal inside the anchored function body."""
    for i in indices:
        revs[i] = revs.get(i, 0) + 1
        with open(os.path.join(repo, rel_path(i)), "w", encoding="utf-8") as f:
            f.write(file_body(i, revs[i]))


def phase_measure(binary: str, scratch: str, n: int, budget_s: int) -> None:
    """Checkpointed measurement: seed -> quiet -> K tests -> bulk drain.

    Progress persists in state.json between sub-steps, so a spent time
    budget (PhaseIncomplete, exit 3) is resumed by rerunning the phase.
    Call counts survive server restarts because staleness and the dirty
    backlog live in the store and the filesystem, not in server memory.
    """
    prog = make_prog(n)
    paths = size_paths(scratch, n)
    state = load_state(paths)
    if not state.get("index_done"):
        raise RuntimeError(f"size {n}: run --phase index first")
    if state.get("measured"):
        prog("PHASE measure: already complete, nothing to do")
        return
    step = state.get("m_step", "seed")
    prog(f"PHASE measure: start (at sub-step '{step}')")
    integrity: list = state.get("integrity", [])
    state["integrity"] = integrity
    tax = read_open_tax_ms(paths["base"])
    state["measure_read_tax_ms"] = round(tax, 1)
    prog(f"environment read-open tax: {tax:.1f} ms per open"
         + (" (WALL-CLOCK COLUMNS POLLUTED)" if tax > 20 else ""))
    if state["files"] != n and not any("expected" in i for i in integrity):
        integrity.append(f"indexed {state['files']} files, expected {n}")

    anch = anchored_indices(n)
    revs = {int(k): v for k, v in state.get("m_revs", {}).items()}
    mem = {int(k): v for k, v in state.get("m_mem", {}).items()}
    t_budget = time.perf_counter()

    def budget_left() -> float:
        return budget_s - (time.perf_counter() - t_budget)

    def checkpoint(next_step: str) -> None:
        nonlocal step
        step = next_step
        state["m_step"] = next_step
        state["m_revs"] = {str(k): v for k, v in revs.items()}
        state["m_mem"] = {str(k): v for k, v in mem.items()}
        save_state(paths, state)

    def need(seconds: float, what: str) -> None:
        if budget_left() < seconds:
            checkpoint(step)
            raise PhaseIncomplete(
                f"size {n}: measure budget ({budget_s}s) spent before "
                f"'{what}'; rerun --phase measure to continue"
            )

    srv = start_server(binary, paths, prog)
    try:
        if step == "seed":
            prog(f"seeding {ANCHOR_COUNT} anchored memories "
                 f"({len(mem)} already done)")
            for j, i in enumerate(anch):
                if i in mem:
                    continue
                need(150, f"seed memory {j}")
                body = (
                    f"lagbench receipt {j}: batch scaling decision for "
                    f"fn_{i:06d}, the multiplier follows feed cadence "
                    f"variant {j} and must not be flattened without "
                    f"re-measuring throughput"
                )
                out = srv.call_tool("remember", {
                    "kind": MEMORY_KINDS[j % len(MEMORY_KINDS)],
                    "body": body,
                    "anchors": [{"file": rel_path(i), "symbol": f"fn_{i:06d}"}],
                })
                data = out.get("data", {})
                if "id" not in data or data.get("anchored") != 1:
                    raise BenchProtocolError(
                        f"remember failed for fn_{i:06d}: {out}"
                    )
                mem[i] = data["id"]
                checkpoint("seed")  # one memory at a time survives budget death
            need(150, "seed verification")
            prog("seeded; verifying all memories recallable and fresh")
            _, by_id, _ = recall_status(srv)
            missing = [i for i in anch if mem[i] not in by_id]
            if missing:
                integrity.append(f"seeded memories not recallable: {missing}")
            pre_stale = [i for i in anch
                         if mem[i] in by_id and item_is_stale(by_id[mem[i]])]
            if pre_stale:
                integrity.append(
                    f"memories stale before any touch: {pre_stale}"
                )
            checkpoint("quiet")

        if step == "quiet":
            # 1. Quiet-repo per-call sweep overhead.
            quiet = state.get("quiet_partial", [])
            prog(f"quiet phase: timing {QUIET_CALLS} recall calls on an "
                 f"unchanged repo ({len(quiet)} already done)")
            while len(quiet) < QUIET_CALLS:
                need(150, f"quiet call {len(quiet) + 1}")
                env, _, ms = recall_status(srv)
                quiet.append(round(ms, 2))
                fresh = env["meta"]["freshness"]
                if fresh.get("dirty", 0) != 0 or "reindexed_now" in fresh:
                    integrity.append(
                        f"quiet call {len(quiet)} was not quiet: {fresh}"
                    )
                state["quiet_partial"] = quiet
                checkpoint("quiet")
            state["quiet_p50"] = p50(quiet)
            state["quiet_p95"] = p95(quiet)
            state["quiet_ms"] = quiet
            prog(f"quiet done: p50 {state['quiet_p50']:.1f} ms, "
                 f"p95 {state['quiet_p95']:.1f} ms")
            checkpoint("k1")

        # 2. Anchored-staleness latency per K. Each K test consumes its own
        # fresh anchored memories so no test starts already stale:
        #   K=1   -> anch[0]
        #   K=32  -> anch[1..5]
        #   K=100 -> anch[5..9]
        # anch[9..12] are never touched: the false-stale control.
        alloc = {1: anch[0:1], 32: anch[1:5], 100: anch[5:9]}
        controls = anch[9:12]
        state.setdefault("latency", {})
        k_steps = {1: ("k1", "k32"), 32: ("k32", "k100"), 100: ("k100", "bulk")}
        for k in LATENCY_KS:
            step_name, next_step = k_steps[k]
            if step != step_name:
                continue
            need(200, f"K={k} test")
            touched_anchored = alloc[k]
            if state.get(f"k{k}_started"):
                # A previous invocation died inside this K test, so the
                # anchored files may already be touched and their memories
                # stale, which would fake a 1-call latency. Deterministic
                # content makes recovery exact: rewrite each anchored file
                # at its pre-touch revision (the stored anchor hash), let
                # the sweep re-resolve, and the memories heal to active.
                prog(f"K={k} restarted after an interruption; reverting "
                     f"anchored files to their pre-touch revision to heal")
                for i in touched_anchored:
                    with open(os.path.join(paths["repo"], rel_path(i)),
                              "w", encoding="utf-8") as f:
                        f.write(file_body(i, revs.get(i, 0)))
                drain_dirty(srv, DRAIN_CALL_CAP)
                _, by_id, _ = recall_status(srv)
                still_stale = [i for i in touched_anchored
                               if mem.get(i) in by_id
                               and item_is_stale(by_id[mem[i]])]
                if still_stale:
                    integrity.append(
                        f"K={k} unrecoverable after interruption: anchors "
                        f"{still_stale} stale even after revert-heal"
                    )
                else:
                    integrity.append(
                        f"K={k} test restarted after an interruption "
                        f"(recovered by revert-heal, result trustworthy)"
                    )
            state[f"k{k}_started"] = True
            checkpoint(step_name)  # persist the started flag before touching
            plain_start = n // 3 + sum(kk for kk in LATENCY_KS if kk < k)
            plain = pick_plain_indices(
                n, set(anch), plain_start, k - len(touched_anchored)
            )
            touch_files(paths["repo"], touched_anchored + plain, revs)
            prog(f"K={k}: touched {len(touched_anchored)} anchored + "
                 f"{len(plain)} plain files; counting recall calls to stale")
            ws = [rel_path(i) for i in touched_anchored]
            calls = 0
            stale_seen = 0
            while calls < LATENCY_CALL_CAP:
                _, by_id, _ = recall_status(srv, working_set=ws)
                calls += 1
                stale_seen = sum(
                    1 for i in touched_anchored
                    if mem[i] in by_id and item_is_stale(by_id[mem[i]])
                )
                if stale_seen == len(touched_anchored):
                    break
            if stale_seen != len(touched_anchored):
                calls = LATENCY_CALL_CAP + 1  # cap exceeded, recorded as fail
            state["latency"][str(k)] = calls
            prog(f"K={k}: all touched anchored memories stale after "
                 f"{calls} call(s)")
            # False-stale control: untouched anchored memories stay fresh.
            _, by_id, _ = recall_status(srv)
            false_stale = [i for i in controls
                           if mem[i] in by_id and item_is_stale(by_id[mem[i]])]
            if false_stale:
                integrity.append(
                    f"K={k} flipped untouched anchors stale: {false_stale}"
                )
            # Drain any remaining backlog so the next test starts quiet.
            drain_calls, _, _ = drain_dirty(srv, DRAIN_CALL_CAP)
            prog(f"K={k}: backlog drained in {drain_calls} follow-up call(s)")
            checkpoint(next_step)

        if step == "bulk":
            # 3. Bulk-change drain (branch-switch scale), unanchored only.
            bulk_n = min(BULK_CHANGE, n - ANCHOR_COUNT)
            if not state.get("bulk_touched"):
                need(60 + bulk_n * tax / 1000 * 2, "bulk touch")
                bulk = pick_plain_indices(n, set(anch), n // 2, bulk_n)
                touch_files(paths["repo"], bulk, revs)
                state["bulk_n"] = bulk_n
                state["bulk_touched"] = True
                prog(f"bulk change: touched {bulk_n} unanchored files")
                checkpoint("bulk")
            calls = state.get("bulk_drain_calls", 0)
            times = state.get("bulk_drain_times", [])
            batch = state.get("drain_batch")
            prog(f"draining bulk backlog (resumed at {calls} calls)"
                 if calls else "draining bulk backlog")
            while True:
                if calls >= DRAIN_CALL_CAP:
                    raise BenchProtocolError(
                        f"dirty backlog did not drain within "
                        f"{DRAIN_CALL_CAP} calls"
                    )
                if budget_left() < 150:
                    state["bulk_drain_calls"] = calls
                    state["bulk_drain_times"] = times
                    state["drain_batch"] = batch
                    checkpoint("bulk")
                    raise PhaseIncomplete(
                        f"size {n}: measure budget spent mid-drain at "
                        f"{calls} calls; rerun --phase measure to continue"
                    )
                env, _, ms = recall_status(srv)
                calls += 1
                times.append(round(ms, 2))
                fresh = env["meta"]["freshness"]
                if batch is None and "reindexed_now" in fresh:
                    batch = fresh["reindexed_now"]
                if fresh.get("dirty", 0) == 0:
                    break
            state["drain_calls"] = calls
            state["drain_mean_ms"] = statistics.mean(times)
            state["drain_batch"] = batch
            state["drain_expected"] = (
                math.ceil(state["bulk_n"] / batch) if batch else None
            )
            prog(f"drain done: dirty=0 after {calls} calls (reindex batch "
                 f"{batch}, mean {state['drain_mean_ms']:.1f} ms/call, "
                 f"quiet p50 {state['quiet_p50']:.1f} ms)")
            checkpoint("done")
    finally:
        srv.close()
    state["measured"] = True
    save_state(paths, state)
    prog("PHASE measure: done")


def phase_verdict(scratch: str, sizes) -> int:
    results = []
    integrity = []
    for n in sizes:
        paths = size_paths(scratch, n)
        state = load_state(paths)
        if not state.get("measured"):
            print(f"size {n}: not measured yet (state at {paths['state']})",
                  file=sys.stderr)
            return 2
        results.append(state)
        for note in state.get("integrity", []):
            integrity.append(f"size {n}: {note}")

    print("\n" + "=" * 100)
    print("LAG BENCH VERDICT")
    print("=" * 100)
    print(f"{'files':>7} {'symbols':>8} {'git':>4} {'index_ms':>9} "
          f"{'quiet_p50_ms':>13} {'quiet_p95_ms':>13} "
          f"{'lat_K1':>7} {'lat_K32':>8} {'lat_K100':>9} "
          f"{'drain_calls':>12} {'drain_exp':>10}")
    print("-" * 100)
    for r in results:
        lat = {int(k): v for k, v in r["latency"].items()}
        fmt_lat = {k: (f">{LATENCY_CALL_CAP}" if lat[k] > LATENCY_CALL_CAP
                       else str(lat[k])) for k in LATENCY_KS}
        print(f"{r['files']:>7} {r['symbols']:>8} "
              f"{'yes' if r.get('git_ok') else 'no':>4} {r['index_ms']:>9} "
              f"{r['quiet_p50']:>13.1f} {r['quiet_p95']:>13.1f} "
              f"{fmt_lat[1]:>7} {fmt_lat[32]:>8} {fmt_lat[100]:>9} "
              f"{r['drain_calls']:>12} {str(r['drain_expected']):>10}")
    print("-" * 100)
    for r in results:
        it = r.get("index_read_tax_ms")
        mt = r.get("measure_read_tax_ms")
        if (it and it > 20) or (mt and mt > 20):
            print(f"WARNING size {r['size']}: environment read-open tax "
                  f"{it} ms (index) / {mt} ms (measure) per open; index_ms "
                  f"and drain timings are environment-polluted. Call-count "
                  f"metrics and the stat-only quiet sweep remain valid.")

    # Criterion 1: anchored latency == 1 call at every size and every K.
    all_lat = [int(v) for r in results for v in r["latency"].values()]
    lat_ok = all(v == 1 for v in all_lat)
    lat_measured = f"max={max(all_lat)} call(s)" if all_lat else "none"

    # Criterion 2: quiet p50 < 250 ms at 50k files.
    gate_row = next((r for r in results if r["size"] == GATE_SIZE), None)
    if gate_row is None:
        p50_ok = None
        p50_measured = f"NOT MEASURED ({GATE_SIZE} not in --sizes)"
    else:
        p50_ok = gate_row["quiet_p50"] < QUIET_P50_LIMIT_MS
        p50_measured = f"{gate_row['quiet_p50']:.1f} ms"

    # Criterion 3: bench integrity (false stales, unquiet quiet phase, etc).
    integ_ok = not integrity

    def verdict(ok):
        if ok is None:
            return "INCOMPLETE"
        return "PASS" if ok else "FAIL"

    print(f"{'criterion':<52} {'threshold':<14} {'measured':<26} result")
    print(f"{'anchored staleness latency, every size, every K':<52} "
          f"{'== 1 call':<14} {lat_measured:<26} {verdict(lat_ok)}")
    print(f"{'quiet-repo per-call sweep p50 at 50000 files':<52} "
          f"{'< 250 ms':<14} {p50_measured:<26} {verdict(p50_ok)}")
    print(f"{'bench integrity (no false stales, quiet is quiet)':<52} "
          f"{'0 findings':<14} {f'{len(integrity)} findings':<26} "
          f"{verdict(integ_ok)}")
    for note in integrity:
        print(f"  integrity: {note}")

    overall = bool(lat_ok) and bool(p50_ok) and integ_ok
    label = "PASS" if overall else ("INCOMPLETE" if p50_ok is None else "FAIL")
    print(f"\nOVERALL: {label}")
    if overall:
        print("Watcher verdict: the bounded per-call sweep meets the gate; "
              "the FS-event watcher stays unbuilt.")
    elif p50_ok is None:
        print("Run with the 50000-file size included to issue the verdict.")
    else:
        print("Watcher verdict: gate missed; the FS-event watcher becomes a "
              "designed backlog item with this bench as its gate.")
    return 0 if overall else 1


def main() -> int:
    ap = argparse.ArgumentParser(description="limpet FS-event watcher lag bench")
    ap.add_argument("--binary", default=os.path.abspath(
        os.path.join(HERE, "..", "target", "release", "limpet")))
    ap.add_argument("--sizes", default="2000,10000,50000",
                    help="comma-separated repo sizes (default 2000,10000,50000)")
    ap.add_argument("--scratch", default=None,
                    help="scratch dir for repos, stores, and phase state "
                         "(default: temp dir, auto-removed; a given dir is "
                         "kept and enables phased runs)")
    ap.add_argument("--phase", default="all",
                    choices=["all", "generate", "index", "measure", "verdict"],
                    help="run one bounded phase (requires --scratch) or all")
    ap.add_argument("--budget-seconds", type=int, default=400,
                    help="per-invocation time budget for the index and "
                         "measure phases; a spent budget saves progress and "
                         "exits 3 so a rerun continues (default 400)")
    args = ap.parse_args()

    sizes = [int(s) for s in args.sizes.split(",") if s.strip()]
    if args.phase != "all" and args.scratch is None:
        print("--phase requires --scratch so state persists between phases",
              file=sys.stderr)
        return 2
    needs_binary = args.phase in ("all", "index", "measure")
    if needs_binary and not os.path.exists(args.binary):
        print(f"binary not found: {args.binary}\n"
              f"build first: rustup run stable cargo build --release",
              file=sys.stderr)
        return 2

    own_scratch = args.scratch is None
    scratch = args.scratch or tempfile.mkdtemp(prefix="limpet-lagbench-")
    os.makedirs(scratch, exist_ok=True)
    print(f"binary:  {args.binary}")
    print(f"scratch: {scratch}")
    print(f"sizes:   {sizes}")
    print(f"phase:   {args.phase}", flush=True)

    # A single "all" run gets an effectively unbounded budget: the caller
    # asked for everything in one invocation and can bound it externally.
    budget = args.budget_seconds if args.phase != "all" else 10 ** 9
    try:
        if args.phase in ("all", "generate"):
            for n in sizes:
                print(f"\n=== repo size {n}: generate ===", flush=True)
                phase_generate(scratch, n)
        if args.phase in ("all", "index"):
            for n in sizes:
                print(f"\n=== repo size {n}: index ===", flush=True)
                phase_index(args.binary, scratch, n, budget)
        if args.phase in ("all", "measure"):
            for n in sizes:
                print(f"\n=== repo size {n}: measure ===", flush=True)
                phase_measure(args.binary, scratch, n, budget)
        if args.phase in ("all", "verdict"):
            return phase_verdict(scratch, sizes)
        return 0
    except PhaseIncomplete as e:
        print(f"\nPHASE INCOMPLETE (rerun to continue): {e}", flush=True)
        return 3
    except BenchProtocolError as e:
        print(f"\nHARNESS FAILURE: {e}", file=sys.stderr)
        return 2
    finally:
        if own_scratch:
            shutil.rmtree(scratch, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
