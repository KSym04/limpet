//! The six MCP tools. Every handler: sweep first, resolve anchors, execute,
//! wrap in the honesty envelope. Git is invoked with argument arrays only;
//! no shell string is ever built from user input.

use crate::envelope::{build_meta, envelope, Completeness};
use crate::index::{self, SweepReport};
use crate::memory::{self, anchor, recall};
use crate::store::Store;
use anyhow::{anyhow, bail, Context, Result};

use serde_json::{json, Value};
use std::path::Path;
use std::process::Command;

/// Dispatch a tool call. Returns the full envelope value.
pub fn dispatch(store: &mut Store, root: &Path, name: &str, args: &Value) -> Result<Value> {
    // A stale code image must not touch the store at all (issue #9): every
    // call sweeps and resolves, which are writes, so the guard gates
    // everything rather than pretending reads from a half-current image
    // are trustworthy.
    store.version_guard()?;
    // Repo config loads ONCE per dispatch and threads into every index
    // touch below, so one call cannot index files under two different
    // rule sets. Hot-path policy: a broken .limpet.json degrades to the
    // built-in grammar table here; the explicit `index` command
    // (full_index) is where it fails loudly.
    let ext = crate::config::RepoConfig::load(root).unwrap_or_default().extensions;
    // Freshness first (I6): bounded sweep + anchor resolution on every call.
    // A failed sweep must not be reported as "dirty: 0"; that asserts a
    // freshness that was never checked (audit 2026-07).
    let sweep = match index::sweep(store, root, &ext) {
        Ok(s) => s,
        Err(_) => SweepReport { sweep_failed: true, ..SweepReport::default() },
    };
    let _ = anchor::resolve_all(store);

    match name {
        "recall" => tool_recall(store, &sweep, args),
        "remember" => tool_remember(store, root, &sweep, args, &ext),
        "map" => tool_map(store, &sweep, args),
        "affected" => tool_affected(store, root, &sweep),
        "verify_queue" => tool_verify_queue(store, &sweep),
        "admin" => tool_admin(store, root, &sweep, args),
        other => bail!("unknown tool '{other}'"),
    }
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing required string argument '{key}'"))
}

fn tool_recall(store: &Store, sweep: &SweepReport, args: &Value) -> Result<Value> {
    let task = str_arg(args, "task")?;
    let working_set: Vec<String> = args
        .get("working_set")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                // The index stores `/`; a Windows agent sends `\`.
                .map(crate::util::normalize_rel)
                .collect()
        })
        .unwrap_or_default();
    let budget = args
        .get("budget_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(1200) as usize;

    let result = recall::recall(store, task, &working_set, budget)?;

    // Savings ledger (spec v0.7.0): price the pack against its file-reading
    // counterfactual and accumulate. Observational only (I-L5): a failure
    // here must never fail the recall.
    let cost = recall::recall_cost(&result.items, |f| {
        store
            .conn
            .query_row("SELECT size FROM files WHERE path = ?1", [f], |r| r.get(0))
            .ok()
    });
    let query_hash = {
        use sha2::{Digest, Sha256};
        let d = Sha256::digest(memory::fts_escape(task).as_bytes());
        d[..8].iter().map(|b| format!("{b:02x}")).collect::<String>()
    };
    let _ = store.ledger_add(cost.served, cost.baseline, cost.reads_avoided, &query_hash);

    let stale = result
        .items
        .iter()
        .filter(|i| i.flags.iter().any(|f| f.starts_with("stale:")))
        .count();
    let contradicted = result
        .items
        .iter()
        .filter(|i| i.flags.iter().any(|f| f.starts_with("contradicted-by:")))
        .count();
    // Compact wire shape: every token here is paid on every recall, so
    // defaults are omitted and floats are rounded. Honesty fields (flags,
    // status when not active) always survive compaction.
    let items: Vec<Value> = result
        .items
        .iter()
        .map(|i| {
            let mut obj = serde_json::Map::new();
            obj.insert("id".into(), json!(i.id));
            obj.insert("kind".into(), json!(i.kind));
            obj.insert("body".into(), json!(i.body));
            obj.insert("conf".into(), json!((i.confidence * 100.0).round() / 100.0));
            if i.source != "explicit" {
                obj.insert("source".into(), json!(i.source));
            }
            if i.status != "active" {
                obj.insert("status".into(), json!(i.status));
            }
            if !i.anchors.is_empty() {
                obj.insert("anchors".into(), json!(i.anchors));
            }
            if !i.flags.is_empty() {
                obj.insert("flags".into(), json!(i.flags));
            }
            obj.insert(
                "on".into(),
                json!(i.created_at.get(..10).unwrap_or(&i.created_at)),
            );
            Value::Object(obj)
        })
        .collect();

    let meta = build_meta(
        store,
        sweep,
        Completeness {
            matched: result.matched,
            returned: result.returned,
            // Name the TRUE cause of omission: the relevance floor and the
            // token budget are different things (audit 2026-07).
            omitted_reason: if result.returned < result.matched {
                let budget_cut = result.matched - result.returned - result.cut_by_floor;
                match (result.cut_by_floor > 0, budget_cut > 0) {
                    (true, true) => Some("budget+relevance_floor"),
                    (true, false) => Some("relevance_floor"),
                    _ => Some("budget"),
                }
            } else {
                None
            },
        },
        stale,
        contradicted,
    );
    // The ledger is deliberately NOT in this response. Re-tested at 0.11.0:
    // a full meta.ledger block per recall added ~279 tokens across the bench
    // and dropped the overall ratio to 3.8x, under the 4x gate, even with
    // the lineage questions lifting the denominator (M1). The receipt is for
    // humans; it lives in admin {op:"ledger"}, `limpet stats`, and the UI,
    // where reading it costs the agent nothing.
    Ok(envelope(Value::Array(items), meta))
}

fn tool_remember(
    store: &Store,
    root: &Path,
    sweep: &SweepReport,
    args: &Value,
    ext: &std::collections::HashMap<String, crate::index::lang::Lang>,
) -> Result<Value> {
    let kind = str_arg(args, "kind")?;
    let body = str_arg(args, "body")?;
    let source = args.get("source").and_then(Value::as_str).unwrap_or("explicit");
    let confidence = args.get("confidence").and_then(Value::as_f64);

    let mut anchors: Vec<memory::AnchorSpec> = match args.get("anchors") {
        Some(v) => serde_json::from_value(v.clone()).context("parsing anchors")?,
        None => Vec::new(),
    };
    // Store `/` regardless of what separator the caller's OS uses, so the
    // anchor matches the walker's `/`-keyed rows.
    for a in &mut anchors {
        a.file = crate::util::normalize_rel(&a.file);
    }
    for a in &anchors {
        crate::util::validate_rel_path(root, &a.file)?;
        // A memory must anchor to the file's CURRENT content: under sweep
        // budget starvation the target could still be dirty, minting an
        // anchor with the pre-edit hash that flips stale on the very next
        // sweep (audit 2026-07). Anchors are few, so this is cheap.
        let _ = index::index_file(store, root, &a.file, ext);
    }
    let evidence: Option<memory::Evidence> = match args.get("evidence") {
        Some(v) if !v.is_null() => Some(serde_json::from_value(v.clone()).context("parsing evidence")?),
        _ => None,
    };
    let links: Vec<memory::LinkSpec> = match args.get("links") {
        Some(v) => serde_json::from_value(v.clone()).context("parsing links")?,
        None => Vec::new(),
    };
    let private = args.get("private").and_then(Value::as_bool).unwrap_or(false);
    let origin = args.get("origin").and_then(Value::as_str);
    let branch = current_branch(root);

    let result = memory::remember(
        store,
        kind,
        body,
        source,
        confidence,
        &anchors,
        evidence.as_ref(),
        &links,
        branch.as_deref(),
        private,
        origin,
    )?;
    let meta = build_meta(
        store,
        sweep,
        Completeness { matched: 1, returned: 1, omitted_reason: None },
        0,
        0,
    );
    Ok(envelope(serde_json::to_value(&result)?, meta))
}

fn edge_json(e: &crate::index::graph::Edge) -> Value {
    json!({ "fqn": e.fqn, "rel": e.rel, "resolved": e.resolved, "depth": e.depth })
}

fn tool_map(store: &Store, sweep: &SweepReport, args: &Value) -> Result<Value> {
    let target_raw = str_arg(args, "target")?;
    // A path target may arrive with `\`; a symbol FQN never contains one, so
    // normalizing is safe for both and fixes `map src\util.rs` on Windows.
    let target_norm = crate::util::normalize_rel(target_raw);
    let target = target_norm.as_str();

    // Try file view first, then symbol view.
    let file_symbols: Vec<Value> = {
        let mut stmt = store.conn.prepare(
            "SELECT fqn, name, kind, start_line, end_line FROM symbols
             WHERE file = ?1 ORDER BY start_line",
        )?;
        let rows: Vec<Value> = stmt
            .query_map([target], |r| {
                Ok(json!({
                    "fqn": r.get::<_, String>(0)?,
                    "name": r.get::<_, String>(1)?,
                    "kind": r.get::<_, String>(2)?,
                    "lines": [r.get::<_, i64>(3)?, r.get::<_, i64>(4)?],
                }))
            })?
            .collect::<rusqlite::Result<_>>()?;
        rows
    };

    let (scope_desc, symbols, files): (String, Vec<Value>, Vec<String>) = if !file_symbols
        .is_empty()
    {
        (format!("file {target}"), file_symbols, vec![target.to_string()])
    } else {
        let mut stmt = store.conn.prepare(
            "SELECT fqn, name, kind, file, start_line, end_line FROM symbols
             WHERE fqn = ?1 OR name = ?1 ORDER BY fqn LIMIT 20",
        )?;
        let rows: Vec<(Value, String)> = stmt
            .query_map([target], |r| {
                let file: String = r.get(3)?;
                Ok((
                    json!({
                        "fqn": r.get::<_, String>(0)?,
                        "name": r.get::<_, String>(1)?,
                        "kind": r.get::<_, String>(2)?,
                        "file": file.clone(),
                        "lines": [r.get::<_, i64>(4)?, r.get::<_, i64>(5)?],
                    }),
                    file,
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let files = rows.iter().map(|(_, f)| f.clone()).collect();
        (
            format!("symbol {target}"),
            rows.into_iter().map(|(v, _)| v).collect(),
            files,
        )
    };

    if symbols.is_empty() {
        let meta = build_meta(
            store,
            sweep,
            Completeness { matched: 0, returned: 0, omitted_reason: None },
            0,
            0,
        );
        return Ok(envelope(
            json!({ "error": format!("nothing indexed matches '{target}'") }),
            meta,
        ));
    }

    // Syntactic call edges touching the scope.
    let mut calls_out: Vec<Value> = Vec::new();
    {
        let mut stmt = store.conn.prepare(
            "SELECT caller_fqn, callee_name FROM calls WHERE file = ?1 LIMIT 100",
        )?;
        for f in &files {
            for row in stmt.query_map([f], |r| {
                Ok(json!({
                    "caller": r.get::<_, String>(0)?,
                    "callee": r.get::<_, String>(1)?,
                    "confidence": "syntactic",
                }))
            })? {
                calls_out.push(row?);
            }
        }
    }

    // Attached memories for the scope.
    let mut memories: Vec<Value> = Vec::new();
    {
        let mut stmt = store.conn.prepare(
            "SELECT DISTINCT e.id, e.kind, e.body, e.status, e.stale_reason
             FROM entries e JOIN anchors a ON a.entry_id = e.id
             WHERE (a.file = ?1 OR a.symbol_fqn = ?1) AND e.status != 'superseded'
             ORDER BY e.id DESC LIMIT 20",
        )?;
        let mut seen = std::collections::HashSet::new();
        for key in files.iter().chain(std::iter::once(&target.to_string())) {
            for row in stmt.query_map([key], |r| {
                Ok(json!({
                    "id": r.get::<_, String>(0)?,
                    "kind": r.get::<_, String>(1)?,
                    "body": r.get::<_, String>(2)?,
                    "status": r.get::<_, String>(3)?,
                    "stale_reason": r.get::<_, Option<String>>(4)?,
                }))
            })? {
                let row = row?;
                if seen.insert(row["id"].as_str().unwrap_or_default().to_string()) {
                    memories.push(row);
                }
            }
        }
    }

    let stale = memories
        .iter()
        .filter(|m| m["status"].as_str() == Some("stale"))
        .count();

    // Additive lineage for SYMBOL targets only (D2). Computed for the first
    // matched symbol fqn; the payload names its own target so an ambiguous
    // target string is self-disclosing. Read-only, bounded (I-G2).
    let lineage_val = if scope_desc.starts_with("symbol ") {
        let target_fqn = symbols
            .first()
            .and_then(|s| s.get("fqn"))
            .and_then(Value::as_str)
            .unwrap_or(target);
        match crate::index::graph::lineage(store, target_fqn, Default::default()) {
            Ok(l) => json!({
                "target": l.target,
                "ancestors": l.ancestors.iter().map(edge_json).collect::<Vec<_>>(),
                "descendants": l.descendants.iter().map(edge_json).collect::<Vec<_>>(),
                "callers": l.callers.iter().map(edge_json).collect::<Vec<_>>(),
                "truncated": l.truncated,
                "unresolved_count": l.unresolved_count,
            }),
            // Lineage is best-effort context; a failure never fails `map`.
            Err(_) => Value::Null,
        }
    } else {
        Value::Null
    };

    // True totals, so a clipped list is disclosed instead of silently
    // presented as complete (envelope invariant; audit 2026-07).
    let sym_total: i64 = store.conn.query_row(
        "SELECT COUNT(*) FROM symbols WHERE file = ?1 OR fqn = ?1 OR name = ?1",
        [target],
        |r| r.get(0),
    )?;
    let mut calls_total = 0i64;
    for f in &files {
        calls_total += store.conn.query_row(
            "SELECT COUNT(*) FROM calls WHERE file = ?1",
            [f],
            |r| r.get::<_, i64>(0),
        )?;
    }
    let returned = symbols.len() + memories.len() + calls_out.len();
    let matched = sym_total as usize + calls_total as usize + memories.len();
    let meta = build_meta(
        store,
        sweep,
        Completeness {
            matched: matched.max(returned),
            returned,
            omitted_reason: if matched > returned { Some("limit") } else { None },
        },
        stale,
        0,
    );
    let mut data = json!({
        "scope": scope_desc,
        "symbols": symbols,
        "calls": calls_out,
        "memories": memories,
    });
    if !lineage_val.is_null() {
        data.as_object_mut().unwrap().insert("lineage".into(), lineage_val);
    }
    Ok(envelope(data, meta))
}

fn tool_affected(store: &Store, root: &Path, sweep: &SweepReport) -> Result<Value> {
    let changed = git_changed_files(root)?;

    let mut impacted_symbols: Vec<Value> = Vec::new();
    let mut touched_memories: Vec<Value> = Vec::new();
    {
        let mut sym_stmt = store.conn.prepare(
            "SELECT fqn, name, kind FROM symbols WHERE file = ?1 LIMIT 50",
        )?;
        let mut mem_stmt = store.conn.prepare(
            "SELECT DISTINCT e.id, e.kind, e.body, e.status, e.stale_reason
             FROM entries e JOIN anchors a ON a.entry_id = e.id
             WHERE a.file = ?1 AND e.status != 'superseded'",
        )?;
        let mut seen = std::collections::HashSet::new();
        for f in &changed {
            for row in sym_stmt.query_map([f], |r| {
                Ok(json!({
                    "fqn": r.get::<_, String>(0)?,
                    "name": r.get::<_, String>(1)?,
                    "kind": r.get::<_, String>(2)?,
                    "file": f,
                }))
            })? {
                impacted_symbols.push(row?);
            }
            for row in mem_stmt.query_map([f], |r| {
                Ok(json!({
                    "id": r.get::<_, String>(0)?,
                    "kind": r.get::<_, String>(1)?,
                    "body": r.get::<_, String>(2)?,
                    "status": r.get::<_, String>(3)?,
                    "stale_reason": r.get::<_, Option<String>>(4)?,
                    "file": f,
                }))
            })? {
                let row = row?;
                if seen.insert(row["id"].as_str().unwrap_or_default().to_string()) {
                    touched_memories.push(row);
                }
            }
        }
    }

    let stale = touched_memories
        .iter()
        .filter(|m| m["status"].as_str() == Some("stale"))
        .count();
    let decisions: Vec<&Value> = touched_memories
        .iter()
        .filter(|m| m["kind"].as_str() == Some("decision"))
        .collect();
    // Disclose symbol clipping (LIMIT 50 per changed file) instead of
    // reporting the clipped view as complete (audit 2026-07).
    let mut sym_total = 0i64;
    for f in &changed {
        sym_total += store.conn.query_row(
            "SELECT COUNT(*) FROM symbols WHERE file = ?1",
            [f],
            |r| r.get::<_, i64>(0),
        )?;
    }
    let returned = changed.len() + impacted_symbols.len() + touched_memories.len();
    let matched = changed.len() + sym_total as usize + touched_memories.len();
    let meta = build_meta(
        store,
        sweep,
        Completeness {
            matched: matched.max(returned),
            returned,
            omitted_reason: if matched > returned { Some("limit") } else { None },
        },
        stale,
        0,
    );
    Ok(envelope(
        json!({
            "changed_files": changed,
            "impacted_symbols": impacted_symbols,
            "memories_on_changed_code": touched_memories,
            "constraining_decisions": decisions,
        }),
        meta,
    ))
}

fn tool_verify_queue(store: &Store, sweep: &SweepReport) -> Result<Value> {
    let mut stmt = store.conn.prepare(
        "SELECT id, body, evidence_cmd, evidence_ran_at, stale_reason
         FROM entries
         WHERE source = 'verified' AND status = 'stale'
         ORDER BY evidence_ran_at ASC",
    )?;
    let queue: Vec<Value> = stmt
        .query_map([], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "body": r.get::<_, String>(1)?,
                "reverify_command": r.get::<_, Option<String>>(2)?,
                "evidence_from": r.get::<_, Option<String>>(3)?,
                "stale_reason": r.get::<_, Option<String>>(4)?,
            }))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let n = queue.len();
    let meta = build_meta(
        store,
        sweep,
        Completeness { matched: n, returned: n, omitted_reason: None },
        n,
        0,
    );
    Ok(envelope(json!(queue), meta))
}

fn tool_admin(store: &mut Store, root: &Path, sweep: &SweepReport, args: &Value) -> Result<Value> {
    let op = str_arg(args, "op")?;
    let data = match op {
        "index" => {
            let (report, imported) = index::index_and_bootstrap(store, root)?;
            let resolved = anchor::resolve_all(store)?;
            json!({ "index": report, "anchors": resolved, "imported": imported })
        }
        "status" => {
            let files: i64 =
                store.conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))?;
            let symbols: i64 =
                store.conn.query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))?;
            let entries: i64 =
                store.conn.query_row("SELECT COUNT(*) FROM entries", [], |r| r.get(0))?;
            let private: i64 = store
                .conn
                .query_row("SELECT COUNT(*) FROM entries WHERE private = 1", [], |r| r.get(0))?;
            let by_status: Vec<Value> = {
                let mut stmt = store.conn.prepare(
                    "SELECT status, COUNT(*) FROM entries GROUP BY status",
                )?;
                let rows: Vec<Value> = stmt
                    .query_map([], |r| {
                        Ok(json!({
                            "status": r.get::<_, String>(0)?,
                            "count": r.get::<_, i64>(1)?
                        }))
                    })?
                    .collect::<rusqlite::Result<_>>()?;
                rows
            };
            json!({
                "files": files, "symbols": symbols, "entries": entries,
                "private": private,
                "entries_by_status": by_status,
                "root": root.to_string_lossy(),
            })
        }
        "forget" => {
            let id = str_arg(args, "id")?;
            let deleted = memory::forget(store, id)?;
            json!({ "deleted": deleted, "id": id })
        }
        "export" => {
            let default_path = root.join(".limpet/memory.jsonl");
            let path = match args.get("path").and_then(Value::as_str) {
                Some(p) => crate::util::validate_rel_path(root, p)?,
                None => default_path,
            };
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let mut f = std::fs::File::create(&path)?;
            let report = store.export_jsonl(&mut f)?;
            json!({ "exported": report.exported, "private_withheld": report.private_withheld, "path": path.to_string_lossy() })
        }
        "import" => {
            let default_path = root.join(".limpet/memory.jsonl");
            let path = match args.get("path").and_then(Value::as_str) {
                Some(p) => crate::util::validate_rel_path(root, p)?,
                None => default_path,
            };
            let f = std::fs::File::open(&path)
                .with_context(|| format!("opening {}", path.display()))?;
            let mut r = std::io::BufReader::new(f);
            let report = store.import_jsonl(&mut r)?;
            json!({ "import": report, "path": path.to_string_lossy() })
        }
        "ledger" => ledger_payload(store),
        "ledger_reset" => {
            store.ledger_reset()?;
            store.ledger_session_start()?;
            json!({ "reset": true })
        }
        other => bail!("unknown admin op '{other}' (index|status|forget|export|import|ledger|ledger_reset)"),
    };
    let meta = build_meta(
        store,
        sweep,
        Completeness { matched: 1, returned: 1, omitted_reason: None },
        0,
        0,
    );
    Ok(envelope(data, meta))
}

/// The full ledger payload: session + lifetime + the method string that
/// makes the number checkable rather than believed (I-L6).
pub fn ledger_payload(store: &Store) -> Value {
    let lifetime = store.ledger_read();
    let session = lifetime.diff(&store.ledger_session_base());
    json!({
        "session": {
            "recalls": session.recalls,
            "served_tokens": session.served,
            "baseline_tokens": session.baseline,
            "saved_tokens": session.saved(),
            "reads_avoided": session.reads_avoided,
        },
        "lifetime": {
            "recalls": lifetime.recalls,
            "distinct_queries": lifetime.distinct_queries,
            "served_tokens": lifetime.served,
            "baseline_tokens": lifetime.baseline,
            "saved_tokens": lifetime.saved(),
            "reads_avoided": lifetime.reads_avoided,
            "since": store.ledger_since(),
        },
        "method": "tokens = ceil(bytes/4) on both sides; baseline = 300-token \
                   search overhead + the DISTINCT files returned memories anchor \
                   to (minimal file set); anchorless memories add zero baseline, \
                   understating savings on questions no file answers; negatives \
                   are shown, never floored; gross recalls and distinct queries \
                   both reported. Same methodology as bench/token_savings.py.",
    })
}

/// Changed files: committed-vs-HEAD is not needed here; we want the working
/// tree delta an agent is editing right now. Argument arrays only.
fn git_changed_files(root: &Path) -> Result<Vec<String>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["status", "--porcelain", "-z", "--untracked-files=all"])
        .output();
    let Ok(out) = out else {
        return Ok(Vec::new()); // Not a git repo or git absent: empty, honest.
    };
    if !out.status.success() {
        return Ok(Vec::new());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut files = Vec::new();
    for rec in text.split('\0') {
        if rec.len() > 3 {
            let path = &rec[3..];
            if !path.is_empty() {
                files.push(path.to_string());
            }
        }
    }
    files.sort();
    files.dedup();
    Ok(files)
}

fn current_branch(root: &Path) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let b = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if b.is_empty() { None } else { Some(b) }
}

/// JSON Schemas for tools/list.
pub fn tool_schemas() -> Value {
    json!([
        {
            "name": "recall",
            "description": "Retrieve project memories relevant to a task. Returns a token-budgeted, ranked pack of facts, decisions, insights, episodes, and intents, each flagged if stale or contradicted. Always check meta.staleness and item flags before trusting a memory.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "task": { "type": "string", "description": "What you are working on, in a sentence." },
                    "working_set": { "type": "array", "items": { "type": "string" }, "description": "Repo-relative paths of files currently being worked on." },
                    "budget_tokens": { "type": "integer", "description": "Max tokens of memory to return (default 1200)." }
                },
                "required": ["task"]
            }
        },
        {
            "name": "remember",
            "description": "Store a durable memory. kinds: fact (verified behavior), decision (choice + why), episode (what worked/failed), insight (gotcha), intent (what a module is for). Anchor it to code so limpet can flag it when that code changes: symbol anchors track the function/class body, file anchors (no symbol) track the whole file's content; any indexed file works, including templates, styles, and configs. Every anchor must resolve or the call fails with the reason; nothing is stored half-anchored. Provide evidence {command, output} to make it a verified fact.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "enum": ["fact","decision","episode","insight","intent"] },
                    "body": { "type": "string", "description": "The memory itself. Short, specific, standalone." },
                    "anchors": { "type": "array", "items": { "type": "object", "properties": {
                        "file": { "type": "string", "description": "Repo-relative path." },
                        "symbol": { "type": "string", "description": "Function/class name or FQN in that file. Omit to anchor to the file itself (goes stale when the file's content changes)." }
                    }, "required": ["file"] } },
                    "evidence": { "type": "object", "properties": {
                        "command": { "type": "string" }, "output": { "type": "string" }
                    }, "required": ["command", "output"] },
                    "links": { "type": "array", "items": { "type": "object", "properties": {
                        "target": { "type": "string" },
                        "rel": { "type": "string", "enum": ["supports","contradicts","supersedes"] }
                    }, "required": ["target","rel"] } },
                    "source": { "type": "string", "enum": ["explicit","mined"] },
                    "confidence": { "type": "number" },
                    "private": { "type": "boolean", "description": "Keep this memory on this machine only: it is recalled locally but withheld from admin export / .limpet/memory.jsonl. Default false." },
                    "origin": { "type": "string", "description": "Optional dedup key naming the memory's source (e.g. scan:git:<sha>). A second remember with the same origin is rejected, which makes scan re-runs idempotent." }
                },
                "required": ["kind", "body"]
            }
        },
        {
            "name": "map",
            "description": "Structural view of a file or symbol: outline, imports, syntactic call edges, plus every memory attached to it. For a SYMBOL target it also returns lineage (ancestors, descendants, callers) with each edge labeled unique/ambiguous/unresolved. Code and knowledge in one answer.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "target": { "type": "string", "description": "Repo-relative file path, symbol name, or FQN." }
                },
                "required": ["target"]
            }
        },
        {
            "name": "affected",
            "description": "What does my current uncommitted change touch? Changed files, their symbols, memories anchored to them (now possibly stale), and decisions that constrain the code being edited.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "verify_queue",
            "description": "Verified facts whose anchored code changed. Each comes with the exact command that originally proved it; rerun it and update the memory.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "admin",
            "description": "Maintenance: op=index (full reindex), status (includes private count), forget (id), export / import (JSONL at .limpet/memory.jsonl for team sharing via git; private memories are withheld from export and counted in private_withheld), ledger (token-savings receipt: session + lifetime + methodology) / ledger_reset.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "op": { "type": "string", "enum": ["index","status","forget","export","import","ledger","ledger_reset"] },
                    "id": { "type": "string" },
                    "path": { "type": "string" }
                },
                "required": ["op"]
            }
        }
    ])
}

/// Test-only shim: call a tool handler directly, bypassing sweep/resolve.
/// Used by integration tests that build their own Store in a tempdir.
/// Not part of the public API; subject to removal without notice.
#[doc(hidden)]
pub fn dispatch_for_test(name: &str, store: &Store, args: &Value) -> Result<Value> {
    let sweep = SweepReport::default();
    match name {
        "map" => tool_map(store, &sweep, args),
        other => Err(anyhow!("unknown tool '{other}'")),
    }
}
