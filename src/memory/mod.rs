//! Memory layer: entries, anchors-on-write, links, contradiction checks,
//! and confidence decay.

pub mod anchor;
pub mod recall;

use crate::index::now_iso;
use crate::store::Store;
use crate::util::ulid;
use anyhow::{bail, Context, Result};
use rusqlite::params;

pub const KINDS: [&str; 5] = ["fact", "decision", "episode", "insight", "intent"];
pub const SOURCES: [&str; 3] = ["explicit", "mined", "verified"];

/// Confidence policy per source (spec section 5).
fn default_confidence(source: &str, requested: Option<f64>) -> f64 {
    let base = match source {
        "verified" => 0.95,
        "mined" => 0.5,
        _ => requested.unwrap_or(0.8),
    };
    match source {
        "mined" => base.min(0.5),
        _ => base.clamp(0.0, 1.0),
    }
}

#[derive(Debug, serde::Deserialize)]
pub struct AnchorSpec {
    pub file: String,
    #[serde(default)]
    pub symbol: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
pub struct Evidence {
    pub command: String,
    pub output: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct LinkSpec {
    pub target: String,
    pub rel: String,
}

#[derive(Debug, serde::Serialize)]
pub struct RememberResult {
    pub id: String,
    pub anchored: usize,
    /// Existing entries whose bodies look similar and share an anchor file.
    /// Surfaced, never auto-merged (invariant I4).
    pub possible_duplicates: Vec<serde_json::Value>,
}

/// A fully resolved anchor, ready to insert. Built before any row is
/// written so an unresolvable anchor aborts the whole `remember` with
/// nothing persisted (invariant I-A) instead of reporting a phantom
/// `anchored` count (invariant I-B).
struct ResolvedAnchor {
    file: String,
    symbol_fqn: Option<String>,
    hash: String,
    context_hint: Option<String>,
}

fn resolve_anchor(store: &Store, spec: &AnchorSpec) -> Result<ResolvedAnchor> {
    match &spec.symbol {
        Some(symbol) => {
            // Accept a bare name or a full FQN.
            let row: Option<(String, String, String)> = store
                .conn
                .query_row(
                    "SELECT fqn, file, body_hash FROM symbols
                     WHERE (fqn = ?1 OR name = ?1) AND file = ?2 LIMIT 1",
                    params![symbol, spec.file],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .ok();
            let Some((sym_fqn, file, hash)) = row else {
                let mut near_stmt = store.conn.prepare(
                    "SELECT fqn FROM symbols WHERE file = ?1 ORDER BY fqn LIMIT 10",
                )?;
                let near: Vec<String> = near_stmt
                    .query_map([&spec.file], |r| r.get(0))?
                    .collect::<rusqlite::Result<_>>()?;
                bail!(
                    "symbol '{symbol}' not found in {} (known there: {near:?})",
                    spec.file
                );
            };
            let hint: Option<String> = store
                .conn
                .query_row(
                    "SELECT parent_fqn FROM symbols WHERE fqn = ?1 LIMIT 1",
                    [&sym_fqn],
                    |r| r.get(0),
                )
                .ok()
                .flatten();
            Ok(ResolvedAnchor { file, symbol_fqn: Some(sym_fqn), hash, context_hint: hint })
        }
        None => {
            // File-level anchor: the file must be indexed, and its current
            // content hash is stored so edits flip the memory to stale.
            let hash: Option<String> = store
                .conn
                .query_row("SELECT hash FROM files WHERE path = ?1", [&spec.file], |r| {
                    r.get(0)
                })
                .ok();
            let Some(hash) = hash else {
                bail!(
                    "cannot anchor to '{}': file is not in the index. Either the \
                     path is wrong, or the file is excluded by .gitignore, \
                     .limpetignore, the 512KB size cap, or the minified-asset \
                     skip. Fix that, run admin op=index, and retry.",
                    spec.file
                );
            };
            Ok(ResolvedAnchor {
                file: spec.file.clone(),
                symbol_fqn: None,
                hash,
                context_hint: None,
            })
        }
    }
}

/// Store a memory entry. Anchors are resolved against the live index at
/// write time so the entry is born with valid hashes; the entry plus all
/// its anchors and links persist atomically, or not at all.
pub fn remember(
    store: &Store,
    kind: &str,
    body: &str,
    source: &str,
    confidence: Option<f64>,
    anchors: &[AnchorSpec],
    evidence: Option<&Evidence>,
    links: &[LinkSpec],
    branch: Option<&str>,
) -> Result<RememberResult> {
    if !KINDS.contains(&kind) {
        bail!("unknown kind '{kind}' (expected one of {KINDS:?})");
    }
    if !SOURCES.contains(&source) {
        bail!("unknown source '{source}' (expected one of {SOURCES:?})");
    }
    if body.trim().is_empty() {
        bail!("body must not be empty");
    }
    // Never persist a credential: it would live in the local store and could
    // later leak through `admin export` -> .limpet/memory.jsonl -> git.
    if let Some(kind) = crate::secrets::detect(body) {
        bail!("refusing to store memory: body looks like a {kind}. Remove the secret and describe it instead.");
    }
    if let Some(ev) = evidence {
        if let Some(kind) = crate::secrets::detect(&ev.output) {
            bail!("refusing to store memory: evidence output looks like a {kind}. Redact it before storing.");
        }
    }
    let source = if evidence.is_some() { "verified" } else { source };
    let id = ulid();
    let now = now_iso();
    let conf = default_confidence(source, confidence);

    // Resolve every anchor before writing anything: one bad anchor aborts
    // the whole call with a loud error instead of a half-anchored entry.
    let resolved: Vec<ResolvedAnchor> = anchors
        .iter()
        .map(|spec| resolve_anchor(store, spec))
        .collect::<Result<_>>()?;

    let (ev_cmd, ev_digest, ev_at) = match evidence {
        Some(ev) => {
            use sha2::{Digest, Sha256};
            let d = Sha256::digest(ev.output.as_bytes());
            let digest: String = d[..16].iter().map(|b| format!("{b:02x}")).collect();
            (Some(ev.command.clone()), Some(digest), Some(now.clone()))
        }
        None => (None, None, None),
    };

    let tx = store.conn.unchecked_transaction()?;
    store.conn.execute(
        "INSERT INTO entries(id, kind, body, created_at, updated_at, source,
                             confidence, status, branch,
                             evidence_cmd, evidence_digest, evidence_ran_at)
         VALUES (?1,?2,?3,?4,?4,?5,?6,'active',?7,?8,?9,?10)",
        params![id, kind, body, now, source, conf, branch, ev_cmd, ev_digest, ev_at],
    )?;

    let mut anchored = 0usize;
    for a in &resolved {
        store.conn.execute(
            "INSERT INTO anchors(entry_id, file, symbol_fqn, ast_body_hash, context_hint)
             VALUES (?1,?2,?3,?4,?5)",
            params![id, a.file, a.symbol_fqn, a.hash, a.context_hint],
        )?;
        anchored += 1;
    }

    for l in links {
        add_link(store, &id, &l.target, &l.rel)?;
    }
    tx.commit()?;

    // Near-duplicate surfacing: top FTS matches on this body among entries
    // anchored to any of the same files.
    let mut possible_duplicates = Vec::new();
    if !anchors.is_empty() {
        let fts_query = fts_escape(body);
        if !fts_query.is_empty() {
            let mut stmt = store.conn.prepare(
                "SELECT DISTINCT e.id, e.kind, substr(e.body, 1, 120)
                 FROM entries_fts f
                 JOIN entries e ON e.rowid = f.rowid
                 JOIN anchors a ON a.entry_id = e.id
                 WHERE entries_fts MATCH ?1 AND e.id != ?2
                   AND a.file IN (SELECT file FROM anchors WHERE entry_id = ?2)
                 ORDER BY rank LIMIT 3",
            )?;
            let rows = stmt.query_map(params![fts_query, id], |r| {
                Ok(serde_json::json!({
                    "id": r.get::<_, String>(0)?,
                    "kind": r.get::<_, String>(1)?,
                    "body_preview": r.get::<_, String>(2)?,
                }))
            });
            if let Ok(rows) = rows {
                possible_duplicates = rows.flatten().collect();
            }
        }
    }

    Ok(RememberResult { id, anchored, possible_duplicates })
}

/// Turn free text into a safe FTS5 OR-query of bare terms.
/// FTS5 syntax characters are stripped, never interpolated.
pub fn fts_escape(text: &str) -> String {
    let terms: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|t| t.len() >= 3)
        .take(12)
        .map(|t| format!("\"{}\"", t.replace('"', "")))
        .collect();
    terms.join(" OR ")
}

pub fn add_link(store: &Store, src: &str, dst: &str, rel: &str) -> Result<()> {
    if !["supports", "contradicts", "supersedes"].contains(&rel) {
        bail!("unknown link rel '{rel}'");
    }
    let exists: bool = store
        .conn
        .query_row("SELECT EXISTS(SELECT 1 FROM entries WHERE id = ?1)", [dst], |r| {
            r.get(0)
        })
        .context("checking link target")?;
    if !exists {
        bail!("link target '{dst}' does not exist");
    }
    store.conn.execute(
        "INSERT OR IGNORE INTO links(src, dst, rel) VALUES (?1,?2,?3)",
        params![src, dst, rel],
    )?;
    if rel == "supersedes" {
        store.conn.execute(
            "UPDATE entries SET status = 'superseded', updated_at = ?2 WHERE id = ?1",
            params![dst, now_iso()],
        )?;
    }
    Ok(())
}

pub fn forget(store: &Store, id: &str) -> Result<bool> {
    let n = store
        .conn
        .execute("DELETE FROM entries WHERE id = ?1", [id])?;
    Ok(n > 0)
}

/// Age-based confidence decay, applied lazily at read time:
/// `c = c0 * 0.5^(age_days / 180)`, floored at 0.2.
pub fn decayed_confidence(c0: f64, created_at: &str, now: &str) -> f64 {
    let age_days = days_between(created_at, now).max(0.0);
    (c0 * 0.5_f64.powf(age_days / 180.0)).max(0.2)
}

fn days_between(a: &str, b: &str) -> f64 {
    match (parse_iso_secs(a), parse_iso_secs(b)) {
        (Some(x), Some(y)) => (y - x) as f64 / 86_400.0,
        _ => 0.0,
    }
}

/// Parse `YYYY-MM-DDTHH:MM:SSZ` to Unix seconds. Returns None on any
/// deviation; callers treat that as age zero (no decay) rather than
/// guessing.
pub fn parse_iso_secs(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    // days_from_civil, Howard Hinnant.
    let yy = if mo <= 2 { y - 1 } else { y };
    let era = if yy >= 0 { yy } else { yy - 399 } / 400;
    let yoe = yy - era * 400;
    let mp = if mo > 2 { mo - 3 } else { mo + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + h * 3600 + mi * 60 + sec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decay_halves_at_180_days() {
        let c = decayed_confidence(0.8, "2026-01-01T00:00:00Z", "2026-06-30T00:00:00Z");
        assert!((c - 0.4).abs() < 0.01, "expected ~0.4, got {c}");
        let same = decayed_confidence(0.8, "2026-01-01T00:00:00Z", "2026-01-01T00:00:00Z");
        assert!((same - 0.8).abs() < 1e-9);
        let floor = decayed_confidence(0.8, "2020-01-01T00:00:00Z", "2026-01-01T00:00:00Z");
        assert!((floor - 0.2).abs() < 1e-9);
    }

    #[test]
    fn fts_escape_strips_syntax() {
        let q = fts_escape("why NEAR(\"weird\") AND syntax-here?");
        assert!(!q.contains("NEAR("));
        assert!(q.contains("\"weird\""));
        assert!(q.contains("\"syntax\""));
    }
}
