#!/usr/bin/env python3
"""Token savings benchmark for limpet. Stdlib only, fully local.

Methodology (also described in README.md):

For each benchmark question an agent typically re-answers every session,
we compare two ways of getting the answer:

  without limpet:  the agent locates and reads the minimal set of files
                   that contain (or fail to contain) the answer. Cost =
                   tokens(files) + a fixed 300-token allowance for the
                   two search round trips (grep/glob) needed to find them.
                   This UNDERSTATES real exploration cost: agents usually
                   read more than the minimal set.

  with limpet:     one recall() call. Cost = tokens of the full JSON
                   response envelope the agent receives.

Tokens are estimated as ceil(bytes / 4), the standard approximation, and
applied identically to both sides.

Questions marked not_in_code=True have answers that exist in no file
(decisions, history, tribal knowledge). File reading cannot answer them at
any token price; they are excluded from the savings ratio and reported
separately, because "infinite savings" would be marketing math.

Run:  python3 bench/token_savings.py [path-to-limpet-binary]
Exits nonzero if the measured savings ratio drops below GATE_RATIO (regression gate).
"""

import json
import math
import os
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURE = os.path.join(HERE, "fixture-repo")
SEARCH_OVERHEAD_TOKENS = 300
GATE_RATIO = 4.0


def tokens_of_text(text: str) -> int:
    return math.ceil(len(text.encode("utf-8")) / 4)


def tokens_of_files(paths):
    total = 0
    for rel in paths:
        with open(os.path.join(FIXTURE, rel), "rb") as f:
            total += math.ceil(len(f.read()) / 4)
    return total


MEMORIES = [
    dict(kind="decision", body="batch size is 50 because shared hosts kill requests over 30 seconds; the queue exists so a full catalog scan survives across many requests", anchors=[{"file": "src/scan/queue.php", "symbol": "seed"}]),
    dict(kind="insight", body="scan_batch skips draft products on purpose; treating that as a bug and removing the check floods reports with unsellable items", anchors=[{"file": "src/scan/scanner.php", "symbol": "scan_batch"}]),
    dict(kind="fact", body="health score subtracts 10 per critical and 2 per warning, floor at zero", anchors=[{"file": "src/scan/scanner.php", "symbol": "health_score"}]),
    dict(kind="insight", body="csv export uses semicolon delimiter plus BOM because Excel in EU locales splits on semicolon and mangles UTF-8 without BOM", anchors=[{"file": "src/export/csv.php", "symbol": "export"}]),
    dict(kind="decision", body="reports write to the uploads dir, never the plugin dir, because managed hosts mount plugin dirs read-only", anchors=[{"file": "src/export/csv.php", "symbol": "export"}]),
    dict(kind="fact", body="download tokens expire after 12 hours and are HMAC signed with the auth salt", anchors=[{"file": "src/auth/session.php", "symbol": "issue_download_token"}]),
    dict(kind="episode", body="tried streaming the csv export in chunks, reverted: WP output buffering on shared hosts broke chunked responses and produced empty files", anchors=[{"file": "src/export/csv.php", "symbol": "export"}]),
    dict(kind="insight", body="image probe uses HEAD with 5 second timeout; some CDNs return 403 for HEAD so a 403 gets re-probed by the nightly cron before being trusted as critical", anchors=[{"file": "src/scan/rules.php", "symbol": "probe_image"}]),
    dict(kind="intent", body="tools/cron.py exists to stop transient CDN failures from lingering as critical issues; it re-verifies failed image urls nightly", anchors=[{"file": "tools/cron.py", "symbol": "reverify_images"}]),
    dict(kind="episode", body="renaming check_product broke a customer site that hooked it via reflection; public method names in FeedScanner are frozen until v2", anchors=[{"file": "src/scan/scanner.php", "symbol": "check_product"}]),
    dict(kind="fact", body="frontend polls progress every 4 seconds; lowering it caused admin-ajax rate limiting on wp engine", anchors=[{"file": "assets/app.ts", "symbol": "pollLoop"}]),
    dict(kind="decision", body="score card thresholds are 80 and 50 to match Google Merchant account health bands, not arbitrary", anchors=[{"file": "admin/dashboard.php", "symbol": "render_score"}]),
]

QUESTIONS = [
    dict(q="why is the batch size 50 and why is there a queue at all", files=["src/scan/queue.php", "admin/dashboard.php"], not_in_code=True),
    dict(q="why does the scanner skip draft products, is that a bug", files=["src/scan/scanner.php"], not_in_code=True),
    dict(q="how is the health score computed", files=["src/scan/scanner.php"], not_in_code=False),
    dict(q="why semicolon delimiter and BOM in the csv export", files=["src/export/csv.php"], not_in_code=True),
    dict(q="where do report files get written and why", files=["src/export/csv.php"], not_in_code=True),
    dict(q="how long are download tokens valid", files=["src/auth/session.php"], not_in_code=False),
    dict(q="has anyone tried streaming the csv export", files=["src/export/csv.php"], not_in_code=True),
    dict(q="can I rename check_product in the scanner", files=["src/scan/scanner.php"], not_in_code=True),
    dict(q="what does the nightly cron actually exist for", files=["tools/cron.py"], not_in_code=True),
    dict(q="how often does the dashboard poll progress and can I lower it", files=["assets/app.ts"], not_in_code=True),
]


class Server:
    def __init__(self, binary, root, data_dir):
        self.proc = subprocess.Popen(
            [binary, "serve", "--root", root],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            env={**os.environ, "LIMPET_DATA_DIR": data_dir},
            text=True,
        )
        self.next_id = 0

    def request(self, method, params=None):
        self.next_id += 1
        msg = {"jsonrpc": "2.0", "id": self.next_id, "method": method}
        if params is not None:
            msg["params"] = params
        self.proc.stdin.write(json.dumps(msg) + "\n")
        self.proc.stdin.flush()
        return json.loads(self.proc.stdout.readline())

    def call_tool(self, name, arguments):
        resp = self.request("tools/call", {"name": name, "arguments": arguments})
        text = resp["result"]["content"][0]["text"]
        return text, json.loads(text)

    def close(self):
        self.proc.stdin.close()
        self.proc.wait(timeout=10)


def main():
    binary = sys.argv[1] if len(sys.argv) > 1 else os.path.join(
        HERE, "..", "target", "release", "limpet"
    )
    binary = os.path.abspath(binary)
    if not os.path.exists(binary):
        print(f"binary not found: {binary}\nbuild first: cargo build --release", file=sys.stderr)
        return 2

    with tempfile.TemporaryDirectory() as data_dir:
        srv = Server(binary, FIXTURE, data_dir)
        srv.request("initialize", {"protocolVersion": "2025-06-18", "capabilities": {}})
        _, indexed = srv.call_tool("admin", {"op": "index"})
        n_files = indexed["data"]["index"]["files"]
        n_symbols = indexed["data"]["index"]["symbols"]

        for m in MEMORIES:
            _, out = srv.call_tool("remember", m)
            if "error" in out:
                print(f"seed failed: {out['error']}", file=sys.stderr)
                return 2

        rows = []
        for case in QUESTIONS:
            raw, parsed = srv.call_tool(
                "recall", {"task": case["q"], "budget_tokens": 300}
            )
            with_tokens = tokens_of_text(raw)
            without_tokens = tokens_of_files(case["files"]) + SEARCH_OVERHEAD_TOKENS
            answered = len(parsed["data"]) > 0
            rows.append(dict(
                q=case["q"],
                with_t=with_tokens,
                without_t=without_tokens,
                ratio=without_tokens / with_tokens,
                answered=answered,
                not_in_code=case["not_in_code"],
            ))
        srv.close()

    print(f"\nfixture: {n_files} files, {n_symbols} symbols indexed; "
          f"{len(MEMORIES)} memories seeded\n")
    print(f"{'question':<58} {'files+grep':>10} {'recall':>8} {'ratio':>7}  in code?")
    print("-" * 100)
    for r in rows:
        marker = "no (answer only in memory)" if r["not_in_code"] else "yes"
        print(f"{r['q'][:57]:<58} {r['without_t']:>10} {r['with_t']:>8} "
              f"{r['ratio']:>6.1f}x  {marker}")

    unanswered = [r for r in rows if not r["answered"]]
    if unanswered:
        print(f"\nFAIL: {len(unanswered)} questions returned no memories", file=sys.stderr)
        return 1

    total_with = sum(r["with_t"] for r in rows)
    total_without = sum(r["without_t"] for r in rows)
    overall = total_without / total_with
    derivable = [r for r in rows if not r["not_in_code"]]
    derivable_ratio = (
        sum(r["without_t"] for r in derivable) / sum(r["with_t"] for r in derivable)
        if derivable else 0.0
    )
    n_memory_only = sum(1 for r in rows if r["not_in_code"])

    print("-" * 100)
    print(f"{'TOTAL':<58} {total_without:>10} {total_with:>8} {overall:>6.1f}x")
    print(f"\ntoken estimate: ceil(bytes/4), applied identically to both sides")
    print(f"overall savings: {overall:.1f}x fewer tokens across all {len(rows)} questions")
    print(f"code-derivable questions only: {derivable_ratio:.1f}x "
          f"({len(derivable)} questions)")
    print(f"questions unanswerable from code at any token price: {n_memory_only} "
          f"of {len(rows)} (answered from memory alone)")

    if overall < GATE_RATIO:
        print(f"\nREGRESSION: overall ratio {overall:.1f}x under gate {GATE_RATIO}x",
              file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
