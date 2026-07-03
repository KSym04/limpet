//! Visual memory: a local web view of the knowledge graph.
//!
//! Design intent: competitors visualize code structure; limpet visualizes
//! knowledge health. The graph shows memories, what they clamp onto, and
//! their honesty state (active, stale, invalidated, superseded) at a
//! glance, plus contradiction and supersession relations.
//!
//! Security posture: binds 127.0.0.1 only, GET only, serves exactly one
//! embedded HTML document and two JSON endpoints built from parameterized
//! queries. No filesystem paths are ever derived from the request, so
//! traversal is structurally impossible. No external network access.

use crate::store::Store;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::Path;

const UI_HTML: &str = include_str!("ui.html");

pub fn serve_ui(root: &Path, port: u16) -> Result<()> {
    let db_path = Store::default_db_path(root);
    let store = Store::open(&db_path)?;
    let listener = TcpListener::bind(("127.0.0.1", port))
        .with_context(|| format!("binding 127.0.0.1:{port}"))?;
    println!("limpet ui on http://127.0.0.1:{port} (local only, Ctrl-C to stop)");

    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let mut reader = BufReader::new(match stream.try_clone() {
            Ok(s) => s,
            Err(_) => continue,
        });
        let mut request_line = String::new();
        if reader.read_line(&mut request_line).is_err() {
            continue;
        }
        // Drain headers; nothing in them is trusted or used.
        let mut line = String::new();
        while reader.read_line(&mut line).is_ok() {
            if line == "\r\n" || line == "\n" || line.is_empty() {
                break;
            }
            line.clear();
        }

        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or("");
        let path = parts.next().unwrap_or("/");

        let (status, ctype, body) = if method != "GET" {
            ("405 Method Not Allowed", "text/plain", "GET only".to_string())
        } else {
            match path {
                "/" => ("200 OK", "text/html; charset=utf-8", UI_HTML.to_string()),
                "/api/graph" => match graph_json(&store, root) {
                    Ok(v) => ("200 OK", "application/json", v.to_string()),
                    Err(e) => (
                        "500 Internal Server Error",
                        "application/json",
                        json!({ "error": e.to_string() }).to_string(),
                    ),
                },
                _ => ("404 Not Found", "text/plain", "not found".to_string()),
            }
        };

        let _ = write!(
            stream,
            "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.flush();
    }
    Ok(())
}

/// Build the graph payload: memory entries, the files/symbols they clamp
/// onto, anchor edges, and inter-memory links.
pub fn graph_json(store: &Store, root: &Path) -> Result<Value> {
    let mut nodes: Vec<Value> = Vec::new();
    let mut edges: Vec<Value> = Vec::new();

    let mut estmt = store.conn.prepare(
        "SELECT id, kind, body, status, stale_reason, source, confidence,
                created_at, evidence_cmd
         FROM entries",
    )?;
    let entries: Vec<Value> = estmt
        .query_map([], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "type": "memory",
                "kind": r.get::<_, String>(1)?,
                "body": r.get::<_, String>(2)?,
                "status": r.get::<_, String>(3)?,
                "stale_reason": r.get::<_, Option<String>>(4)?,
                "source": r.get::<_, String>(5)?,
                "conf": (r.get::<_, f64>(6)? * 100.0).round() / 100.0,
                "on": r.get::<_, String>(7)?,
                "reverify": r.get::<_, Option<String>>(8)?,
            }))
        })?
        .collect::<rusqlite::Result<_>>()?;
    nodes.extend(entries);

    let mut seen_targets = std::collections::HashSet::new();
    let mut astmt = store.conn.prepare(
        "SELECT entry_id, file, symbol_fqn FROM anchors",
    )?;
    let anchor_rows: Vec<(String, String, Option<String>)> = astmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (entry_id, file, symbol) in anchor_rows {
        let target = symbol.clone().unwrap_or_else(|| file.clone());
        if seen_targets.insert(target.clone()) {
            nodes.push(json!({
                "id": target,
                "type": if symbol.is_some() { "symbol" } else { "file" },
                "file": file,
            }));
        }
        edges.push(json!({ "from": entry_id, "to": target, "rel": "anchor" }));
    }

    let mut lstmt = store.conn.prepare("SELECT src, dst, rel FROM links")?;
    let link_rows: Vec<(String, String, String)> = lstmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (src, dst, rel) in link_rows {
        edges.push(json!({ "from": src, "to": dst, "rel": rel }));
    }

    let counts = |status: &str| -> i64 {
        store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM entries WHERE status = ?1",
                [status],
                |r| r.get(0),
            )
            .unwrap_or(0)
    };

    Ok(json!({
        "project": root.file_name().map(|s| s.to_string_lossy().into_owned()),
        "indexed_at": store.kv_get("indexed_at").ok().flatten(),
        "stats": {
            "active": counts("active"),
            "stale": counts("stale"),
            "invalidated": counts("invalidated"),
            "superseded": counts("superseded"),
        },
        "nodes": nodes,
        "edges": edges,
    }))
}
