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

/// Base directory holding one store per indexed repository. Calls the
/// store's own base resolution directly: these endpoints are polled too
/// often to afford a git subprocess or a migration probe per request.
fn data_dir() -> PathBuf {
    Store::data_base()
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

/// How a line read ended.
enum LineEnd {
    /// A `\n` was read (the line includes it).
    Newline,
    /// The socket closed; `buf` holds whatever arrived before it.
    Eof,
    /// `cap` bytes were read without a `\n`; the rest of the line is still
    /// in the socket.
    Cap,
}

/// Read one line into `buf`, never more than `cap` bytes, giving up when
/// `deadline` passes. Every recv is armed with the time left and the
/// deadline is re-checked between recvs, so a client dripping one byte per
/// call cannot stretch a single line past it (a plain `read_line` loops
/// inside one call and only ever sees the per-recv timeout).
fn read_line_by_deadline(
    reader: &mut BufReader<std::net::TcpStream>,
    stream: &std::net::TcpStream,
    deadline: std::time::Instant,
    cap: usize,
    buf: &mut Vec<u8>,
) -> std::io::Result<LineEnd> {
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return Err(std::io::Error::from(std::io::ErrorKind::TimedOut));
        }
        stream.set_read_timeout(Some(left))?;
        let avail = reader.fill_buf()?;
        if avail.is_empty() {
            return Ok(LineEnd::Eof);
        }
        let room = cap.saturating_sub(buf.len());
        let window = &avail[..avail.len().min(room)];
        let (n, hit) = match window.iter().position(|&b| b == b'\n') {
            Some(i) => (i + 1, true),
            None => (window.len(), false),
        };
        buf.extend_from_slice(&window[..n]);
        reader.consume(n);
        if hit {
            return Ok(LineEnd::Newline);
        }
        if buf.len() >= cap {
            return Ok(LineEnd::Cap);
        }
    }
}

/// Serve one HTTP connection: bounded read, route, respond, close.
fn handle_conn(mut stream: std::net::TcpStream, root: &Path, default_key: &str) {
    // Timeouts plus hard byte/line caps keep one stuck or hostile local
    // client from holding a thread or growing memory without bound (audit
    // 2026-07). A read timeout alone is per recv, so a client dripping one
    // byte per call stays inside it forever; `deadline` bounds the whole
    // request on the wall clock and `read_line_by_deadline` re-arms what is
    // left of it before EVERY recv. The write timeout covers a client that
    // sends a valid request and never reads the response. The cloned reader
    // shares the socket, so options set on `stream` apply to it too.
    const REQUEST_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);
    const MAX_REQ_LINE: usize = 8 * 1024;
    const MAX_HEADER_LINES: usize = 100;
    let deadline = std::time::Instant::now() + REQUEST_DEADLINE;
    let _ = stream.set_write_timeout(Some(REQUEST_DEADLINE));
    // ONE buffered reader for the whole request, so bytes already pulled
    // off the socket are never lost between lines. Through 0.16.1 every
    // line got a fresh inner BufReader over a Take: it drained the rest of
    // the headers into a buffer it then dropped, the next read found an
    // empty socket, and every browser request (write half left open, as
    // browsers do) paid the full 5 s read timeout. The route tests never
    // saw it because they shut down their write half and handed the drain
    // an EOF; tests/ui_http.rs now sends a browser-shaped request.
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    });
    let mut raw_line: Vec<u8> = Vec::new();
    let request_line = match read_line_by_deadline(&mut reader, &stream, deadline, MAX_REQ_LINE, &mut raw_line) {
        // Deadline, timeout, or socket error before a request line: nothing
        // to answer.
        Err(_) => return,
        Ok(LineEnd::Eof) if raw_line.is_empty() => return,
        Ok(LineEnd::Cap) => {
            // The cap cut the line. Refuse it outright: a truncated request
            // line must never be routed as if it were the one the client sent.
            let body = "request line too long";
            let _ = write!(
                stream,
                "HTTP/1.1 414 URI Too Long\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.flush();
            return;
        }
        // A complete line, or a partial one at EOF (a client that closed
        // right after the request line), is routed as it was received.
        Ok(_) => match String::from_utf8(std::mem::take(&mut raw_line)) {
            Ok(l) => l,
            Err(_) => return,
        },
    };
    // Drain headers (bounded); nothing in them is trusted or used. A cut
    // header line counts as one header and the rest of it as the next, so
    // the total is bounded by MAX_HEADER_LINES x MAX_REQ_LINE and, before
    // that, by the deadline.
    let mut header_count = 0usize;
    loop {
        raw_line.clear();
        match read_line_by_deadline(&mut reader, &stream, deadline, MAX_REQ_LINE, &mut raw_line) {
            // Deadline or EOF mid-headers: answer what was read, then close.
            Err(_) | Ok(LineEnd::Eof) => break,
            Ok(_) => {
                if raw_line == b"\r\n" || raw_line == b"\n" {
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
                    Ok((store, _)) => {
                        // Lifetime only (I-A4). `session` means "the recalls
                        // THIS process served", and a `ui` process serves
                        // none: recalls come from `serve`. This endpoint also
                        // opens a fresh store per request, so its session
                        // base is always zero and the block could only ever
                        // be lifetime under a second name. ui.html reads
                        // `lifetime` alone, so dropping the key changes
                        // nothing on screen and stops the endpoint claiming
                        // work it never did.
                        let mut payload = crate::tools::ledger_payload(&store);
                        if let Some(obj) = payload.as_object_mut() {
                            obj.remove("session");
                        }
                        ("200 OK", "application/json", payload.to_string())
                    }
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
