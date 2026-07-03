//! MCP server: JSON-RPC 2.0 over stdio, one JSON object per line.
//!
//! Implements initialize, ping, tools/list, tools/call. Malformed input
//! and handler errors produce JSON-RPC errors; the loop never dies on bad
//! input.

use crate::store::Store;
use crate::tools;
use anyhow::Result;
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::PathBuf;

pub const PROTOCOL_VERSION: &str = "2025-06-18";

pub fn serve(root: PathBuf) -> Result<()> {
    let db_path = Store::default_db_path(&root);
    let mut store = Store::open(&db_path)?;
    // Session baseline for the savings ledger: "this session" = lifetime
    // minus this stamp. Last server boot owns the session view.
    let _ = store.ledger_session_start();

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => {
                write_msg(
                    &mut out,
                    &json!({
                        "jsonrpc": "2.0", "id": Value::Null,
                        "error": { "code": -32700, "message": "parse error" }
                    }),
                )?;
                continue;
            }
        };
        let id = msg.get("id").cloned().unwrap_or(Value::Null);
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");

        // Notifications get no response.
        if method.starts_with("notifications/") {
            continue;
        }

        let response = match method {
            "initialize" => json!({
                "jsonrpc": "2.0", "id": id,
                "result": {
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": { "tools": {} },
                    "serverInfo": {
                        "name": "limpet",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }
            }),
            "ping" => json!({ "jsonrpc": "2.0", "id": id, "result": {} }),
            "tools/list" => json!({
                "jsonrpc": "2.0", "id": id,
                "result": { "tools": tools::tool_schemas() }
            }),
            "tools/call" => {
                let name = msg
                    .pointer("/params/name")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let empty = json!({});
                let args = msg.pointer("/params/arguments").unwrap_or(&empty);
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    tools::dispatch(&mut store, &root, name, args)
                }));
                match outcome {
                    Ok(Ok(env_val)) => json!({
                        "jsonrpc": "2.0", "id": id,
                        "result": {
                            "content": [{
                                "type": "text",
                                "text": serde_json::to_string(&env_val)?
                            }]
                        }
                    }),
                    Ok(Err(e)) => json!({
                        "jsonrpc": "2.0", "id": id,
                        "result": {
                            "content": [{
                                "type": "text",
                                "text": json!({ "error": e.to_string() }).to_string()
                            }],
                            "isError": true
                        }
                    }),
                    Err(_) => json!({
                        "jsonrpc": "2.0", "id": id,
                        "error": { "code": -32603, "message": "internal error (handler panicked)" }
                    }),
                }
            }
            _ => json!({
                "jsonrpc": "2.0", "id": id,
                "error": { "code": -32601, "message": format!("method not found: {method}") }
            }),
        };
        write_msg(&mut out, &response)?;
    }
    Ok(())
}

fn write_msg(out: &mut impl Write, v: &Value) -> Result<()> {
    let s = serde_json::to_string(v)?;
    writeln!(out, "{s}")?;
    out.flush()?;
    Ok(())
}
