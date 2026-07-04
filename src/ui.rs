//! Visual memory: a local web view of the knowledge graph.
//!
//! Design intent: competitors visualize code structure; limpet visualizes
//! knowledge health. The graph shows memories, what they clamp onto, and
//! their honesty state (active, stale, invalidated, superseded) at a
//! glance, plus contradiction and supersession relations.
//!
//! Security posture: binds 127.0.0.1 only, GET only, serves exactly one
//! embedded HTML document and JSON endpoints built from parameterized
//! queries. The project selector accepts only keys that exactly match an
//! enumerated store directory, so no filesystem path is ever derived from
//! request input. No external network access.

use crate::store::Store;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};

const UI_HTML: &str = include_str!("ui.html");

/// Base directory holding one store per indexed repository.
fn data_dir() -> PathBuf {
    // default_db_path is <data_dir>/<repo_key>/store.db; peel two levels
    // off a probe path to recover <data_dir> without duplicating the
    // resolution logic.
    let probe = Store::default_db_path(Path::new("."));
    probe
        .parent()
        .and_then(|p| p.parent())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Every indexed project: (repo_key, project_root, store path).
/// Enumerated from disk on each call so newly indexed projects appear
/// without restarting the UI.
fn list_projects() -> Vec<(String, String, PathBuf)> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(data_dir()) else {
        return out;
    };
    for entry in entries.flatten() {
        let db = entry.path().join("store.db");
        if !db.is_file() {
            continue;
        }
        let key = entry.file_name().to_string_lossy().into_owned();
        let root = Store::open(&db)
            .ok()
            .and_then(|s| s.kv_get("project_root").ok().flatten())
            .unwrap_or_else(|| key.clone());
        out.push((key, root, db));
    }
    out.sort_by(|a, b| a.1.cmp(&b.1));
    out
}

/// Resolve a ?project= query value to a store, strictly by exact match
/// against the enumerated keys. Anything else is rejected. Only the
/// matched store is opened; `list_projects` would open every store just
/// to validate one key, which this endpoint is polled too often to afford.
fn resolve_project(query: Option<&str>, default_root: &Path) -> Result<(Store, PathBuf)> {
    match query {
        Some(key) => {
            // The db path is built from the enumerated directory entry, never
            // from the request string, preserving the no-path-from-input rule.
            let db = std::fs::read_dir(data_dir())
                .ok()
                .into_iter()
                .flatten()
                .flatten()
                .find(|e| e.file_name().to_string_lossy() == key)
                .map(|e| e.path().join("store.db"))
                .filter(|db| db.is_file())
                .with_context(|| format!("unknown project key '{key}'"))?;
            let store = Store::open(&db)?;
            let root = store
                .kv_get("project_root")
                .ok()
                .flatten()
                .unwrap_or_else(|| key.to_string());
            Ok((store, PathBuf::from(root)))
        }
        None => {
            let db = Store::default_db_path(default_root);
            Ok((Store::open(&db)?, default_root.to_path_buf()))
        }
    }
}

pub fn serve_ui(root: &Path, port: u16) -> Result<()> {
    // Fail fast if the default project cannot open at all.
    let _ = Store::open(&Store::default_db_path(root))?;
    let default_key = crate::util::repo_key(root);
    let listener = TcpListener::bind(("127.0.0.1", port))
        .with_context(|| format!("binding 127.0.0.1:{port}"))?;
    println!("limpet ui on http://127.0.0.1:{port} (local only, Ctrl-C to stop)");

    // One thread per connection, hard-capped. A single-threaded accept loop
    // stalls real requests behind browser preconnect sockets: Chrome opens
    // speculative idle connections that sit silent until the 5s read timeout,
    // serializing every API call behind them (observed 2026-07). The cap
    // keeps a hostile local client from spawning unbounded threads; excess
    // connections are dropped and the browser simply retries.
    const MAX_CONNS: usize = 32;
    let live = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        if live.fetch_add(1, std::sync::atomic::Ordering::SeqCst) >= MAX_CONNS {
            live.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            continue;
        }
        let live = std::sync::Arc::clone(&live);
        let root = root.to_path_buf();
        let default_key = default_key.clone();
        std::thread::spawn(move || {
            handle_conn(stream, &root, &default_key);
            live.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        });
    }
    Ok(())
}

/// Serve one HTTP connection: bounded read, route, respond, close.
fn handle_conn(mut stream: std::net::TcpStream, root: &Path, default_key: &str) {
    // Read timeout plus hard byte/line caps keep one stuck or hostile
    // local client from holding a thread or growing memory without
    // bound (audit 2026-07).
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
    const MAX_REQ_LINE: u64 = 8 * 1024;
    const MAX_HEADER_LINES: usize = 100;
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    });
    let mut request_line = String::new();
    {
        let mut limited = std::io::Read::take(std::io::Read::by_ref(&mut reader), MAX_REQ_LINE);
        let mut lr = BufReader::new(&mut limited);
        if lr.read_line(&mut request_line).is_err() || request_line.is_empty() {
            return;
        }
    }
    // Drain headers (bounded); nothing in them is trusted or used.
    let mut line = String::new();
    let mut header_count = 0usize;
    loop {
        line.clear();
        let mut limited = std::io::Read::take(std::io::Read::by_ref(&mut reader), MAX_REQ_LINE);
        let mut lr = BufReader::new(&mut limited);
        match lr.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                if line == "\r\n" || line == "\n" || line.is_empty() {
                    break;
                }
                header_count += 1;
                if header_count > MAX_HEADER_LINES {
                    break;
                }
            }
        }
    }

    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let full_path = parts.next().unwrap_or("/");
    let (path, query) = match full_path.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (full_path, None),
    };
    let project_param = query.and_then(|q| {
        q.split('&')
            .find_map(|kv| kv.strip_prefix("project="))
            .map(str::to_string)
    });

    let (status, ctype, body) = if method != "GET" {
        (
            "405 Method Not Allowed",
            "text/plain",
            "GET only".to_string(),
        )
    } else {
        match path {
            "/" => ("200 OK", "text/html; charset=utf-8", UI_HTML.to_string()),
            "/api/projects" => {
                let projects: Vec<Value> = list_projects()
                    .into_iter()
                    .map(|(key, root, _)| {
                        let name = Path::new(&root)
                            .file_name()
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_else(|| key.clone());
                        json!({
                            "key": key,
                            "name": name,
                            "root": root,
                            "default": key == default_key,
                        })
                    })
                    .collect();
                ("200 OK", "application/json", json!(projects).to_string())
            }
            "/api/graph" => {
                let result = if project_param.as_deref() == Some("all") {
                    all_projects_graph()
                } else {
                    resolve_project(project_param.as_deref(), root)
                        .and_then(|(store, proot)| graph_json(&store, &proot))
                };
                match result {
                    Ok(v) => ("200 OK", "application/json", v.to_string()),
                    Err(e) => (
                        "500 Internal Server Error",
                        "application/json",
                        json!({ "error": e.to_string() }).to_string(),
                    ),
                }
            }
            "/api/ledger" => {
                // "all" has no single store; fall back to the default
                // project so the panel always shows something real.
                let param = match project_param.as_deref() {
                    Some("all") | None => None,
                    p => p,
                };
                match resolve_project(param, root) {
                    Ok((store, _)) => (
                        "200 OK",
                        "application/json",
                        crate::tools::ledger_payload(&store).to_string(),
                    ),
                    Err(e) => (
                        "500 Internal Server Error",
                        "application/json",
                        json!({ "error": e.to_string() }).to_string(),
                    ),
                }
            }
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

/// Merged view: every indexed project's memory in one graph. Node ids are
/// namespaced with the project key so identical symbol names in different
/// repositories never collide, and every node carries its project name for
/// the detail panel.
fn all_projects_graph() -> Result<Value> {
    let mut nodes: Vec<Value> = Vec::new();
    let mut edges: Vec<Value> = Vec::new();
    let mut stats = serde_json::Map::new();
    for k in ["active", "stale", "invalidated", "superseded"] {
        stats.insert(k.to_string(), json!(0));
    }
    let mut latest_index: Option<String> = None;

    for (key, root, db) in list_projects() {
        let Ok(store) = Store::open(&db) else {
            continue;
        };
        let Ok(g) = graph_json(&store, Path::new(&root)) else {
            continue;
        };
        let project_name = g["project"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| key.clone());

        if let Some(at) = g["indexed_at"].as_str() {
            if latest_index.as_deref().map(|cur| at > cur).unwrap_or(true) {
                latest_index = Some(at.to_string());
            }
        }
        for k in ["active", "stale", "invalidated", "superseded"] {
            let add = g["stats"][k].as_i64().unwrap_or(0);
            let cur = stats[k].as_i64().unwrap_or(0);
            stats.insert(k.to_string(), json!(cur + add));
        }
        for n in g["nodes"].as_array().into_iter().flatten() {
            let mut n = n.clone();
            let raw_id = n["id"].as_str().unwrap_or_default().to_string();
            n["id"] = json!(format!("{key}:{raw_id}"));
            n["project"] = json!(project_name);
            nodes.push(n);
        }
        for e in g["edges"].as_array().into_iter().flatten() {
            let mut e = e.clone();
            let from = e["from"].as_str().unwrap_or_default().to_string();
            let to = e["to"].as_str().unwrap_or_default().to_string();
            e["from"] = json!(format!("{key}:{from}"));
            e["to"] = json!(format!("{key}:{to}"));
            edges.push(e);
        }
    }

    Ok(json!({
        "project": "all projects",
        "indexed_at": latest_index,
        "stats": Value::Object(stats),
        "nodes": nodes,
        "edges": edges,
    }))
}

/// Build the graph payload: memory entries, the files/symbols they clamp
/// onto, anchor edges, and inter-memory links.
pub fn graph_json(store: &Store, root: &Path) -> Result<Value> {
    let mut nodes: Vec<Value> = Vec::new();
    let mut edges: Vec<Value> = Vec::new();

    let mut estmt = store.conn.prepare(
        "SELECT id, kind, body, status, stale_reason, source, confidence,
                created_at, evidence_cmd, private
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
                "private": r.get::<_, i64>(9)? != 0,
            }))
        })?
        .collect::<rusqlite::Result<_>>()?;
    nodes.extend(entries);

    let mut seen_targets = std::collections::HashSet::new();
    let mut astmt = store
        .conn
        .prepare("SELECT entry_id, file, symbol_fqn FROM anchors")?;
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
