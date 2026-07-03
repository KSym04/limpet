//! End-to-end MCP protocol test: spawn the real binary, speak JSON-RPC
//! over stdio, exercise initialize, tools/list, remember, recall, map,
//! and malformed-input resilience.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use tempfile::TempDir;

struct Server {
    child: Child,
    reader: BufReader<std::process::ChildStdout>,
}

impl Server {
    fn start(root: &std::path::Path, data_dir: &std::path::Path) -> Server {
        let bin = env!("CARGO_BIN_EXE_limpet");
        let mut child = Command::new(bin)
            .args(["serve", "--root"])
            .arg(root)
            .env("LIMPET_DATA_DIR", data_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn limpet serve");
        let stdout = child.stdout.take().expect("stdout piped");
        Server { child, reader: BufReader::new(stdout) }
    }

    fn send_raw(&mut self, line: &str) {
        let stdin = self.child.stdin.as_mut().expect("stdin piped");
        writeln!(stdin, "{line}").unwrap();
        stdin.flush().unwrap();
    }

    fn request(&mut self, msg: Value) -> Value {
        self.send_raw(&msg.to_string());
        let mut line = String::new();
        self.reader.read_line(&mut line).unwrap();
        serde_json::from_str(&line).expect("well-formed json response")
    }

    fn call_tool(&mut self, id: u64, name: &str, args: Value) -> Value {
        let resp = self.request(json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": { "name": name, "arguments": args }
        }));
        let text = resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("tool response missing text: {resp}"));
        serde_json::from_str(text).expect("tool text payload is json")
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn full_protocol_session() {
    let repo = TempDir::new().unwrap();
    let data = TempDir::new().unwrap();
    std::fs::write(
        repo.path().join("feed.py"),
        "def build_feed(products):\n    return [serialize(p) for p in products]\n",
    )
    .unwrap();

    let mut srv = Server::start(repo.path(), data.path());

    // initialize
    let init = srv.request(json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": { "protocolVersion": "2025-06-18", "capabilities": {} }
    }));
    assert_eq!(init["result"]["serverInfo"]["name"], "limpet");
    srv.send_raw(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }).to_string());

    // tools/list: exactly the six tools.
    let list = srv.request(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }));
    let tools: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(tools, vec!["recall", "remember", "map", "affected", "verify_queue", "admin"]);

    // index via admin
    let indexed = srv.call_tool(3, "admin", json!({ "op": "index" }));
    assert_eq!(indexed["data"]["index"]["files"], 1);
    assert!(indexed["meta"]["freshness"]["indexed_at"].is_string(), "envelope always present");

    // remember anchored to the indexed symbol
    let remembered = srv.call_tool(4, "remember", json!({
        "kind": "insight",
        "body": "build_feed silently drops products without prices, by design",
        "anchors": [{ "file": "feed.py", "symbol": "build_feed" }]
    }));
    assert_eq!(remembered["data"]["anchored"], 1);

    // recall finds it, envelope reports completeness
    let recalled = srv.call_tool(5, "recall", json!({
        "task": "why are some products missing from the feed"
    }));
    let items = recalled["data"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert!(items[0]["body"].as_str().unwrap().contains("drops products"));
    assert_eq!(recalled["meta"]["completeness"]["matched"], 1);
    assert_eq!(recalled["meta"]["completeness"]["returned"], 1);
    assert!(recalled["meta"]["staleness"]["stale"].is_number());

    // map merges structure and memory
    let mapped = srv.call_tool(6, "map", json!({ "target": "feed.py" }));
    assert_eq!(mapped["data"]["symbols"][0]["name"], "build_feed");
    assert_eq!(mapped["data"]["memories"].as_array().unwrap().len(), 1);

    // malformed json: error response, server keeps working
    srv.send_raw("{this is not json");
    let mut line = String::new();
    srv.reader.read_line(&mut line).unwrap();
    let err: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(err["error"]["code"], -32700);

    // unknown method
    let unknown = srv.request(json!({ "jsonrpc": "2.0", "id": 7, "method": "nope/nope" }));
    assert_eq!(unknown["error"]["code"], -32601);

    // still alive
    let pong = srv.request(json!({ "jsonrpc": "2.0", "id": 8, "method": "ping" }));
    assert!(pong["result"].is_object());

    // path traversal in anchors is rejected as a tool error
    let evil = srv.call_tool(9, "remember", json!({
        "kind": "fact", "body": "evil",
        "anchors": [{ "file": "../../etc/passwd" }]
    }));
    assert!(
        evil["error"].as_str().unwrap_or_default().contains("traversal"),
        "expected traversal rejection, got: {evil}"
    );
}
