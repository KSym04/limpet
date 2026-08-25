//! The `session` block reports THIS process, never a copy of lifetime and
//! never a figure another process moved (I-A4, v0.16.1).
//!
//! Session used to be `lifetime - a snapshot this process took at boot`.
//! The lifetime counters live in the store's `meta_kv` rows, which every
//! process on that store shares, so the subtraction was only ever right
//! when exactly one process was running: a second `limpet serve` (a second
//! project window, a second editor) had its recalls credited to the first
//! server's session, a cold `limpet stats` diffed against zero and reprinted
//! lifetime under the `session` key, and `ledger_reset` from any process
//! drove every other process's counts NEGATIVE. Counts are tallies of
//! things that happened; the "negatives are shown, never floored" rule is
//! about saved_tokens, and nothing can serve -3 recalls.
//!
//! So session is now COUNTED, not inferred: `ledger_add` (the single
//! writer, one production caller in `tool_recall`) adds each committed
//! recall to an in-process cell, and `ledger_payload` publishes that cell.
//! These tests pin the three claims that mechanism has to keep: another
//! process's recalls never land here, a reset never yields a negative
//! count, and a surface that serves no recalls at all publishes no session
//! block rather than a permanent zero.

use limpet::store::Store;
use limpet::tools;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tempfile::TempDir;

/// The db path the CLI resolves for `root` under `data_dir`. Mirrors
/// `Store::default_db_path` WITHOUT mutating this process's environment:
/// `LIMPET_DATA_DIR` is global state and the test binary is threaded, so a
/// `set_var` here would race every other test. `repo_key` canonicalizes the
/// root itself, matching what `root_from` hands the CLI.
fn db_path_for(data_dir: &Path, root: &Path) -> PathBuf {
    data_dir.join(limpet::util::repo_key(root)).join("store.db")
}

/// Run the real binary's `stats` arm against an isolated store and parse
/// its stdout.
fn cli_stats(root: &Path, data_dir: &Path) -> Value {
    let out = Command::new(env!("CARGO_BIN_EXE_limpet"))
        .args(["stats", "--root"])
        .arg(root)
        .env("LIMPET_DATA_DIR", data_dir)
        .output()
        .expect("spawn limpet stats");
    assert!(
        out.status.success(),
        "limpet stats exited {:?}: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("stats prints json")
}

fn i64_at(v: &Value, block: &str, key: &str) -> i64 {
    v[block][key]
        .as_i64()
        .unwrap_or_else(|| panic!("{block}.{key} missing or not an integer in {v}"))
}

/// Every figure that counts occurrences rather than tokens. Savings may be
/// negative by design (I-L2); these may not, in either block.
const COUNT_KEYS: [&str; 3] = ["recalls", "distinct_queries", "reads_avoided"];

fn assert_no_negative_counts(payload: &Value, whose: &str) {
    for block in ["session", "lifetime"] {
        let Some(obj) = payload.get(block).and_then(Value::as_object) else {
            continue; // A surface that serves nothing drops the block.
        };
        for key in COUNT_KEYS {
            if let Some(n) = obj.get(key).and_then(Value::as_i64) {
                assert!(
                    n >= 0,
                    "{whose} reports {block}.{key} = {n}: a process cannot serve a \
                     negative count ({payload})"
                );
            }
        }
    }
}

#[test]
fn stats_cli_publishes_no_session_it_did_not_serve() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().join("repo");
    std::fs::create_dir_all(&root).expect("mkdir repo");
    let data = tmp.path().join("data");

    // Seed a lifetime ledger the way real recalls do, then close the store
    // so the CLI opens it cold, exactly like a fresh shell would.
    let db = db_path_for(&data, &root);
    {
        let store = Store::open(&db).expect("open seed store");
        store.ledger_add(100, 700, 2, "q1").expect("recall 1");
        store.ledger_add(50, 350, 1, "q1").expect("recall 2, repeat query");
        store.ledger_add(80, 80, 0, "q2").expect("recall 3, zero saving");
    }

    let stats = cli_stats(&root, &data);

    // Self-check the fixture: if the key derivation drifted, the CLI would
    // have opened an empty store and the assertion below would pass for the
    // wrong reason.
    assert_eq!(i64_at(&stats, "lifetime", "recalls"), 3, "CLI read the seeded store: {stats}");

    // Lifetime is the whole receipt here, and it is untouched.
    assert_eq!(i64_at(&stats, "lifetime", "distinct_queries"), 2);
    assert_eq!(i64_at(&stats, "lifetime", "served_tokens"), 230);
    assert_eq!(i64_at(&stats, "lifetime", "baseline_tokens"), 1130);
    assert_eq!(i64_at(&stats, "lifetime", "saved_tokens"), 900);
    assert_eq!(i64_at(&stats, "lifetime", "reads_avoided"), 3);
    assert!(stats["lifetime"]["since"].is_string(), "lifetime keeps its since stamp: {stats}");

    // A `stats` process serves no recalls: recalls come from `serve`. Its
    // session is therefore all zeros by construction, permanently, so the
    // key is dropped the way `ui` drops it. Through 0.16.0 this arm printed
    // lifetime under the `session` key, the exact claim I-A4 forbids.
    assert!(
        stats.get("session").is_none(),
        "stats must publish no session block, not a copy of lifetime and not a \
         permanent zero: {stats}"
    );

    // Reading is read-only: a second cold process sees the same lifetime.
    let again = cli_stats(&root, &data);
    assert_eq!(again["lifetime"], stats["lifetime"], "stats must not mutate the ledger");
    assert!(again.get("session").is_none());
}

/// A loopback port nothing is listening on. Binding to 0 and dropping the
/// listener leaves a small reuse race, which is why the caller retries the
/// connect rather than trusting the first attempt.
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind ephemeral port")
        .local_addr()
        .expect("local addr")
        .port()
}

/// One GET against the local UI, returning the response body. The write
/// half is shut down after the request so the server's bounded header
/// drain sees EOF immediately instead of waiting out its 5s read timeout.
fn ui_get(port: u16, path: &str) -> Option<String> {
    let addr = format!("127.0.0.1:{port}").parse().ok()?;
    let mut sock = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).ok()?;
    sock.set_read_timeout(Some(Duration::from_secs(15))).ok()?;
    write!(sock, "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").ok()?;
    sock.flush().ok()?;
    let _ = sock.shutdown(Shutdown::Write);
    let mut raw = String::new();
    sock.read_to_string(&mut raw).ok()?;
    let (_, body) = raw.split_once("\r\n\r\n")?;
    Some(body.to_string())
}

#[test]
fn ui_ledger_endpoint_publishes_no_session_it_did_not_serve() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().join("repo");
    std::fs::create_dir_all(&root).expect("mkdir repo");
    let data = tmp.path().join("data");

    // Same seed as the CLI case, so the two surfaces are compared on one
    // ledger rather than on two differently shaped fixtures.
    let db = db_path_for(&data, &root);
    {
        let store = Store::open(&db).expect("open seed store");
        store.ledger_add(100, 700, 2, "q1").expect("recall 1");
        store.ledger_add(50, 350, 1, "q1").expect("recall 2, repeat query");
        store.ledger_add(80, 80, 0, "q2").expect("recall 3, zero saving");
    }

    let port = free_port();
    let mut child = Command::new(env!("CARGO_BIN_EXE_limpet"))
        .args(["ui", "--root"])
        .arg(&root)
        .args(["--port", &port.to_string()])
        .env("LIMPET_DATA_DIR", &data)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn limpet ui");

    let mut body = None;
    for _ in 0..50 {
        if let Some(b) = ui_get(port, "/api/ledger") {
            body = Some(b);
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();

    let body = body.expect("UI answered /api/ledger");
    let v: Value =
        serde_json::from_str(&body).unwrap_or_else(|e| panic!("ledger body is not json ({e}): {body}"));

    // Self-check the fixture: an empty store would satisfy the session
    // assertion below for the wrong reason.
    assert_eq!(i64_at(&v, "lifetime", "recalls"), 3, "UI read the seeded store: {v}");
    assert_eq!(i64_at(&v, "lifetime", "served_tokens"), 230);
    assert_eq!(i64_at(&v, "lifetime", "baseline_tokens"), 1130);
    assert_eq!(i64_at(&v, "lifetime", "saved_tokens"), 900);
    assert_eq!(i64_at(&v, "lifetime", "reads_avoided"), 3);

    // A `ui` process serves no recalls, and this endpoint opens a fresh
    // store per request, so there is no honest session figure to publish.
    // Through 0.16.0 it published lifetime under the `session` key: the
    // exact claim I-A4 forbids. The block is gone, not zeroed, because a
    // permanently-zero panel is noise rather than a receipt.
    assert!(
        v.get("session").is_none(),
        "UI must publish no session block, not a copy of lifetime: {v}"
    );
}

#[test]
fn payload_session_counts_only_the_recalls_this_handle_served() {
    let tmp = TempDir::new().expect("tempdir");
    let db = tmp.path().join("store.db");

    // Two handles on one store: separate connections, separate session
    // cells, one shared set of lifetime rows. Same shape as two servers.
    let mine = Store::open(&db).expect("open handle A");
    let theirs = Store::open(&db).expect("open handle B");

    theirs.ledger_add(100, 700, 2, "before").expect("other handle's recall");

    let cold = tools::ledger_payload(&mine);
    assert_eq!(i64_at(&cold, "session", "recalls"), 0, "not my work: {cold}");
    assert_eq!(i64_at(&cold, "session", "saved_tokens"), 0);
    assert_eq!(i64_at(&cold, "lifetime", "recalls"), 1, "shared lifetime sees it");
    assert_eq!(i64_at(&cold, "lifetime", "saved_tokens"), 600);

    // One recall served here, and only that one shows in my session, with
    // the exact figures ledger_add committed.
    mine.ledger_add(10, 500, 1, "mine").expect("this handle's recall");
    let warm = tools::ledger_payload(&mine);
    assert_eq!(i64_at(&warm, "session", "recalls"), 1);
    assert_eq!(i64_at(&warm, "session", "served_tokens"), 10);
    assert_eq!(i64_at(&warm, "session", "baseline_tokens"), 500);
    assert_eq!(i64_at(&warm, "session", "saved_tokens"), 490);
    assert_eq!(i64_at(&warm, "session", "reads_avoided"), 1);
    assert_eq!(i64_at(&warm, "lifetime", "recalls"), 2);
    assert_eq!(i64_at(&warm, "lifetime", "saved_tokens"), 1090);

    // The other handle keeps counting its own work, not mine.
    assert_eq!(i64_at(&tools::ledger_payload(&theirs), "session", "recalls"), 1);
}

/// A live `limpet serve` child, driven over its stdio JSON-RPC line
/// protocol. Two of these against one store is the real thing the session
/// block has to survive: two OS processes, two connections, one ledger.
struct Server {
    child: Child,
    reader: BufReader<std::process::ChildStdout>,
    next_id: u64,
}

impl Server {
    fn start(root: &Path, data_dir: &Path) -> Server {
        let mut child = Command::new(env!("CARGO_BIN_EXE_limpet"))
            .args(["serve", "--root"])
            .arg(root)
            .env("LIMPET_DATA_DIR", data_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn limpet serve");
        let stdout = child.stdout.take().expect("stdout piped");
        let mut srv = Server { child, reader: BufReader::new(stdout), next_id: 1 };
        let init = srv.request(json!({
            "jsonrpc": "2.0", "id": 0, "method": "initialize",
            "params": { "protocolVersion": "2025-06-18", "capabilities": {} }
        }));
        assert_eq!(init["result"]["serverInfo"]["name"], "limpet", "server initialized");
        srv
    }

    fn request(&mut self, msg: Value) -> Value {
        let stdin = self.child.stdin.as_mut().expect("stdin piped");
        writeln!(stdin, "{msg}").expect("write request");
        stdin.flush().expect("flush request");
        let mut line = String::new();
        self.reader.read_line(&mut line).expect("read response");
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("bad response ({e}): {line}"))
    }

    /// A tool call, unwrapped to the tool's own JSON payload.
    fn call(&mut self, name: &str, args: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let resp = self.request(json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": { "name": name, "arguments": args }
        }));
        let text = resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("tool response missing text: {resp}"));
        serde_json::from_str(text).expect("tool text payload is json")
    }

    /// This server's own ledger view (the `data` half of the envelope).
    fn ledger(&mut self) -> Value {
        self.call("admin", json!({ "op": "ledger" }))["data"].clone()
    }

    fn recall(&mut self, task: &str) {
        let r = self.call("recall", json!({ "task": task }));
        assert!(r["data"].is_array(), "recall returned a data array: {r}");
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A repo with one indexed symbol and one anchored memory, so recalls
/// return a real pack and the ledger figures are real rather than zeros.
fn seeded_repo(root: &Path, data: &Path) -> Server {
    std::fs::create_dir_all(root).expect("mkdir repo");
    std::fs::write(
        root.join("feed.py"),
        "def build_feed(products):\n    return [serialize(p) for p in products]\n",
    )
    .expect("write fixture file");
    let mut srv = Server::start(root, data);
    assert_eq!(srv.call("admin", json!({ "op": "index" }))["data"]["index"]["files"], 1);
    let remembered = srv.call(
        "remember",
        json!({
            "kind": "insight",
            "body": "build_feed silently drops products without prices, by design",
            "anchors": [{ "file": "feed.py", "symbol": "build_feed" }]
        }),
    );
    assert_eq!(remembered["data"]["anchored"], 1, "fixture memory is anchored: {remembered}");
    srv
}

#[test]
fn a_second_servers_recalls_never_land_in_this_servers_session() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().join("repo");
    let data = tmp.path().join("data");

    let mut a = seeded_repo(&root, &data);
    let mut b = Server::start(&root, &data);

    // A serves one recall. B, a separate process on the same store, serves
    // two: the exact "second project window / second editor" case.
    a.recall("why are some products missing from the feed");
    b.recall("what does build_feed drop");
    b.recall("feed serialization behaviour");

    let a_view = a.ledger();
    let b_view = b.ledger();

    // Self-check the fixture: the shared lifetime must have seen all three,
    // or the session assertions below would pass for the wrong reason.
    assert_eq!(
        i64_at(&a_view, "lifetime", "recalls"),
        3,
        "A's lifetime sees B's recalls (shared meta_kv): {a_view}"
    );
    assert_eq!(i64_at(&b_view, "lifetime", "recalls"), 3, "same store, same lifetime: {b_view}");

    // The claim under test: session is what THIS process served. Through
    // 0.16.0 A reported 3 here, having diffed the shared counters against
    // its own boot snapshot.
    assert_eq!(
        i64_at(&a_view, "session", "recalls"),
        1,
        "A served exactly one recall; B's two are not A's session: {a_view}"
    );
    assert_eq!(
        i64_at(&b_view, "session", "recalls"),
        2,
        "B served exactly two recalls: {b_view}"
    );

    // Real figures, not zeros: the anchored memory makes the pack cost and
    // the file-reading baseline both nonzero, so session tokens are a
    // receipt and not a vacuous pass.
    assert!(
        i64_at(&a_view, "session", "served_tokens") > 0
            && i64_at(&a_view, "session", "baseline_tokens") > 0,
        "A's recall priced a real pack: {a_view}"
    );

    // A's session cannot move because B kept working.
    b.recall("anything else about the feed");
    let a_after = a.ledger();
    assert_eq!(
        i64_at(&a_after, "session", "recalls"),
        1,
        "A's session moved when only B served: {a_after}"
    );
    assert_eq!(i64_at(&a_after, "lifetime", "recalls"), 4, "lifetime did move: {a_after}");
    assert_no_negative_counts(&a_after, "server A");
}

#[test]
fn a_reset_in_another_process_can_never_produce_negative_counts() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().join("repo");
    let data = tmp.path().join("data");

    // B is the older process. It works first, so the lifetime counters are
    // already nonzero when A boots: that is what made the old subtraction
    // go negative, because A's boot snapshot was 2 and B's reset then took
    // lifetime to 0 (0 - 2 = -2 recalls served, which is not a thing).
    let mut b = seeded_repo(&root, &data);
    b.recall("what does build_feed drop");
    b.recall("feed serialization behaviour");

    let mut a = Server::start(&root, &data);
    a.recall("why are some products missing from the feed");
    a.recall("which products are skipped");
    a.recall("does the feed report skipped products");
    assert_eq!(i64_at(&a.ledger(), "session", "recalls"), 3, "A served three");
    assert_eq!(i64_at(&a.ledger(), "lifetime", "recalls"), 5, "B's two are in lifetime");

    // B wipes the shared counters. Under the old subtraction A now reported
    // session.recalls = 0 - 2 = -2: a process claiming it served negative
    // two recalls. The "never floored" licence is about saved_tokens, and
    // it never licensed a negative tally of things that happened.
    let reset = b.call("admin", json!({ "op": "ledger_reset" }));
    assert_eq!(reset["data"]["reset"], true, "reset ran: {reset}");

    let a_view = a.ledger();
    assert_eq!(i64_at(&a_view, "lifetime", "recalls"), 0, "the wipe really happened: {a_view}");
    assert_eq!(
        i64_at(&a_view, "session", "recalls"),
        3,
        "A really did serve three recalls; a wipe elsewhere cannot un-serve them: {a_view}"
    );
    assert_no_negative_counts(&a_view, "server A after B's reset");

    // The resetting process starts its own tally over with the counters it
    // cleared, so it cannot report a session bigger than the lifetime it
    // just emptied.
    let b_view = b.ledger();
    assert_eq!(i64_at(&b_view, "session", "recalls"), 0, "B restarted its tally: {b_view}");
    assert_no_negative_counts(&b_view, "server B after its own reset");

    // Counting resumes from zero for both, still per process.
    a.recall("post-reset question");
    let a_post = a.ledger();
    assert_eq!(i64_at(&a_post, "lifetime", "recalls"), 1, "lifetime counts again: {a_post}");
    assert_eq!(i64_at(&a_post, "session", "recalls"), 4, "A's own tally is continuous: {a_post}");
    assert_eq!(i64_at(&b.ledger(), "session", "recalls"), 0, "B served nothing since its reset");
    assert_no_negative_counts(&a_post, "server A after the post-reset recall");
}
