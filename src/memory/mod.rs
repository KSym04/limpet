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

/// Upper bound on a memory body. A memory is a distilled conclusion, not a
/// document; anything larger is misuse, and an unbounded body is an
/// allocation and store-growth vector on both the write and import paths.
pub const MAX_BODY_BYTES: usize = 64 * 1024;

/// Quantize a confidence to 6 decimals. Confidence is a heuristic score, not
/// a measurement, and f64 chains like `c * 0.6 * 0.6 ...` accumulate last-ULP
/// noise that then serializes verbatim and breaks export roundtrips. Every
/// stored confidence passes through this so the stored value is always clean.
pub fn quantize_confidence(c: f64) -> f64 {
    (c.clamp(0.0, 1.0) * 1_000_000.0).round() / 1_000_000.0
}

/// Ceiling on typed confidence for unverified (`explicit`) memories. A caller
/// can type swagger, but an unverified claim must never reach the confidence a
/// `verified` fact earns (0.95), so it cannot outrank truth by asserting a high
/// number. Truth is earned with evidence, not typed.
const EXPLICIT_CONF_CAP: f64 = 0.85;

/// Ceiling an IMPORTED confidence may claim per source. Import is the second
/// write path into the store and must enforce the same trust policy as
/// `remember`: `verified` may reach the earned 0.95, `mined` stays at 0.5,
/// and an unverified explicit claim can never exceed [`EXPLICIT_CONF_CAP`],
/// no matter what a hand-edited or hostile export line asserts.
pub fn import_confidence_cap(source: &str) -> f64 {
    match source {
        "verified" => 0.95,
        "mined" => 0.5,
        _ => EXPLICIT_CONF_CAP,
    }
}

/// Confidence policy per source (spec section 5).
fn default_confidence(source: &str, requested: Option<f64>) -> f64 {
    let base = match source {
        "verified" => 0.95,
        "mined" => 0.5,
        _ => requested.unwrap_or(0.8).min(EXPLICIT_CONF_CAP),
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
    /// Existing same-anchor entries that are clearly the same topic but assert a
    /// DIVERGENT value (a flipped number, an added/removed negation): candidate
    /// contradictions. Surfaced so the writer can `supersede` deliberately;
    /// never auto-superseded, never auto-linked. Empty is omitted from the wire.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub possible_conflicts: Vec<serde_json::Value>,
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
            // Accept a bare name or a full FQN, but never guess between
            // duplicates: two `push` methods in one file must surface as a
            // choice, not a silently-picked LIMIT 1 (audit 2026-07).
            let mut cstmt = store.conn.prepare(
                "SELECT DISTINCT fqn FROM symbols
                 WHERE (fqn = ?1 OR name = ?1) AND file = ?2 LIMIT 5",
            )?;
            let candidates: Vec<String> = cstmt
                .query_map(params![symbol, spec.file], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            if candidates.len() > 1 {
                bail!(
                    "symbol '{symbol}' is ambiguous in {}: matches {candidates:?}. \
                     Anchor with the full FQN instead.",
                    spec.file
                );
            }
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
/// Positional API kept deliberately; a params struct is staged in
/// .superpowers/sdd/rememberoptions-refactor.patch for a reviewed follow-up.
#[allow(clippy::too_many_arguments)]
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
    private: bool,
    origin: Option<&str>,
    force: bool,
) -> Result<RememberResult> {
    if !KINDS.contains(&kind) {
        bail!("unknown kind '{kind}' (expected one of {KINDS:?})");
    }
    if !SOURCES.contains(&source) {
        bail!("unknown source '{source}' (expected one of {SOURCES:?})");
    }
    // "verified" is earned, not claimed: without evidence it would mint a
    // 0.95-confidence fact with nothing to re-verify (audit 2026-07).
    if source == "verified" && evidence.is_none() {
        bail!("source 'verified' requires evidence {{command, output}}; pass the proof or use source 'explicit'");
    }
    // Origin is a dedup key, not free text: the scan flow stamps
    // `scan:git:<sha>` so a re-run is rejected here instead of relying on
    // the caller to check first (I-SC4).
    if let Some(o) = origin {
        if o.trim().is_empty() {
            bail!("origin must not be empty when provided");
        }
        if o.len() > 256 {
            bail!("origin is {} bytes; the limit is 256. An origin is a dedup key, not a payload.", o.len());
        }
        if let Some(kind) = crate::secrets::detect(o) {
            bail!("refusing to store memory: origin looks like a {kind}. Use a source reference, not a credential.");
        }
        let existing: Option<String> = store
            .conn
            .query_row("SELECT id FROM entries WHERE origin = ?1", [o], |r| r.get(0))
            .ok();
        if let Some(existing) = existing {
            bail!("duplicate origin '{o}': already stored as {existing}");
        }
    }
    if body.trim().is_empty() {
        bail!("body must not be empty");
    }
    if body.len() > MAX_BODY_BYTES {
        bail!("body is {} bytes; the limit is {MAX_BODY_BYTES}. A memory is a conclusion, not a document.", body.len());
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
        // The output is only ever persisted as a digest, but the COMMAND is
        // stored raw and exported verbatim into a git-committed JSONL: it is
        // the one evidence field that can leak a credential, so it gets the
        // same guard as the body. Empty is refused too: an unverifiable proof
        // command must not mint the verified ranking boost.
        if ev.command.trim().is_empty() {
            bail!("refusing to store memory: the evidence command is empty, so the fact could never be re-verified.");
        }
        if let Some(kind) = crate::secrets::detect(&ev.command) {
            bail!("refusing to store memory: the evidence command looks like it contains a {kind}. Reference the credential, never embed it.");
        }
    }
    let source = if evidence.is_some() { "verified" } else { source };
    let id = ulid();
    let now = now_iso();
    let conf = quantize_confidence(default_confidence(source, confidence));

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

    // Dedup enforcement (P3): a near-identical body already on one of the same
    // anchors is refused BEFORE the insert, naming the existing id, so a re-run
    // or a forgetful writer cannot silently stack duplicates. A correction with
    // a divergent value is exempt (that is a conflict, surfaced after the
    // write, and blocking it would freeze a past mistake in place). `force`
    // stores anyway; nothing existing is ever touched (I4: propose, never merge).
    if !force && !resolved.is_empty() {
        let fts_query = fts_escape(body);
        if !fts_query.is_empty() {
            let files: Vec<&str> = resolved.iter().map(|a| a.file.as_str()).collect();
            let placeholders = (0..files.len())
                .map(|i| format!("?{}", i + 2))
                .collect::<Vec<_>>()
                .join(",");
            // Superseded entries are dead to recall, so they are dead here
            // too: a dead twin must never refuse a new write (correcting the
            // correction would otherwise be blocked by the very entry the
            // correction killed), and a supersede hint naming a dead entry
            // would be a no-op.
            let sql = format!(
                "SELECT DISTINCT e.id, e.body
                 FROM entries_fts f
                 JOIN entries e ON e.rowid = f.rowid
                 JOIN anchors a ON a.entry_id = e.id
                 WHERE entries_fts MATCH ?1 AND e.status != 'superseded'
                   AND a.file IN ({placeholders})
                 ORDER BY rank LIMIT 5"
            );
            let mut params: Vec<&dyn rusqlite::types::ToSql> = vec![&fts_query];
            for f in &files {
                params.push(f);
            }
            let mut stmt = store.conn.prepare(&sql)?;
            let rows = stmt.query_map(&params[..], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            });
            if let Ok(rows) = rows {
                for (cid, cbody) in rows.flatten() {
                    if near_duplicate(body, &cbody) {
                        bail!(
                            "near-duplicate of existing memory {cid} on the same anchor; \
                             supersede {cid} if this replaces it (links: [{{target, rel: \"supersedes\"}}]), \
                             or pass force: true to store it as a distinct memory anyway"
                        );
                    }
                }
            }
        }
    }

    let tx = store.conn.unchecked_transaction()?;
    store.conn.execute(
        "INSERT INTO entries(id, kind, body, created_at, updated_at, source,
                             confidence, status, branch,
                             evidence_cmd, evidence_digest, evidence_ran_at,
                             private, origin)
         VALUES (?1,?2,?3,?4,?4,?5,?6,'active',?7,?8,?9,?10,?11,?12)",
        params![id, kind, body, now, source, conf, branch, ev_cmd, ev_digest, ev_at,
                private as i64, origin],
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
    // anchored to any of the same files. Split into plain duplicates (same
    // topic, redundant) and conflicts (same topic, DIVERGENT value) so the
    // writer sees a contradiction at write time instead of only at recall.
    let mut possible_duplicates = Vec::new();
    let mut possible_conflicts = Vec::new();
    if !anchors.is_empty() {
        let fts_query = fts_escape(body);
        if !fts_query.is_empty() {
            // Same status rule as the pre-insert check: only LIVE entries are
            // surfaced as duplicates or conflicts; a superseded twin is dead.
            let mut stmt = store.conn.prepare(
                "SELECT DISTINCT e.id, e.kind, e.body
                 FROM entries_fts f
                 JOIN entries e ON e.rowid = f.rowid
                 JOIN anchors a ON a.entry_id = e.id
                 WHERE entries_fts MATCH ?1 AND e.id != ?2
                   AND e.status != 'superseded'
                   AND a.file IN (SELECT file FROM anchors WHERE entry_id = ?2)
                 ORDER BY rank LIMIT 3",
            )?;
            let rows = stmt.query_map(params![fts_query, id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            });
            if let Ok(rows) = rows {
                for (cid, ckind, cbody) in rows.flatten() {
                    let preview: String = cbody.chars().take(120).collect();
                    possible_duplicates.push(serde_json::json!({
                        "id": cid,
                        "kind": ckind,
                        "body_preview": preview.clone(),
                    }));
                    if looks_like_conflict(body, &cbody) {
                        possible_conflicts.push(serde_json::json!({
                            "id": cid,
                            "body_preview": preview,
                            "hint": format!("to correct this, supersede {cid}"),
                        }));
                    }
                }
            }
        }
    }

    Ok(RememberResult { id, anchored, possible_duplicates, possible_conflicts })
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

/// Minimum token-set overlap for two same-anchor memories to be considered the
/// same topic (rather than two unrelated notes that share an anchor file). Below
/// this, a value difference is not a contradiction, just two different subjects.
const CONFLICT_MIN_OVERLAP: f64 = 0.4;

/// Significant word tokens in reading order (lowercased, length >= 3 chars,
/// non-pure-digit). The split is Unicode-aware so a CJK or accented body keeps
/// its own words instead of being judged by its ASCII scraps. Pure-digit
/// tokens are excluded here and compared separately by `numbers`, so a flipped
/// value shows up as a value divergence, not as extra tokens that would dilute
/// the overlap ratio. Dependency-free.
fn ordered_significant_tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 3 && !w.chars().all(|c| c.is_ascii_digit()))
        .map(|w| w.to_lowercase())
        .collect()
}

/// Set view of `ordered_significant_tokens`, for overlap ratios.
fn significant_tokens(text: &str) -> std::collections::HashSet<String> {
    ordered_significant_tokens(text).into_iter().collect()
}

/// Numeric values in the text: every MAXIMAL run of ascii digits, including
/// runs embedded inside tokens. "col9" vs "col10" is the motivating failure
/// (the col9/col10 case), and a standalone-token-only view would never see it.
fn numbers(text: &str) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    let mut run = String::new();
    for c in text.chars() {
        if c.is_ascii_digit() {
            run.push(c);
        } else if !run.is_empty() {
            out.insert(std::mem::take(&mut run));
        }
    }
    if !run.is_empty() {
        out.insert(run);
    }
    out
}

/// Whether the text carries a negation that could flip a claim's meaning.
/// Splits raw words (not `significant_tokens`, which drops words under 3
/// chars and would never see "no").
fn has_negation(text: &str) -> bool {
    const NEG: &[&str] = &["no", "not", "never", "cannot", "without", "disable", "disabled"];
    text.split(|c: char| !c.is_alphanumeric())
        .any(|w| NEG.contains(&w.to_lowercase().as_str()))
        || text.contains("n't")
}

/// Minimum token-set overlap for a same-anchor write to be refused as a
/// near-duplicate. Deliberately much higher than `CONFLICT_MIN_OVERLAP`:
/// refusing a write is a stronger act than surfacing a warning, so it demands
/// near-certainty that nothing new is being said.
const DUP_MIN_OVERLAP: f64 = 0.9;

/// Minimum bigram (adjacent-pair) overlap for a near-duplicate call. Word
/// order can invert meaning with identical vocabulary ("prefer sqlite over
/// postgres" vs the reverse); a set comparison is blind to that, so adjacency
/// must also agree before a write is refused.
const DUP_MIN_BIGRAM_OVERLAP: f64 = 0.6;

fn jaccard<T: std::hash::Hash + Eq>(
    a: &std::collections::HashSet<T>,
    b: &std::collections::HashSet<T>,
) -> f64 {
    let union = a.union(b).count() as f64;
    if union == 0.0 {
        return 0.0;
    }
    a.intersection(b).count() as f64 / union
}

/// A near-duplicate says the same thing about the same anchor: near-identical
/// significant tokens, in near-identical ORDER, with the same values (numbers,
/// negation). A divergent value at high overlap is a CORRECTION (see
/// `looks_like_conflict`), never a duplicate; a reversed claim with the same
/// vocabulary is also a correction. Both must store freely or a past mistake
/// becomes unfixable.
fn near_duplicate(new_body: &str, cand_body: &str) -> bool {
    let a_ord = ordered_significant_tokens(new_body);
    let b_ord = ordered_significant_tokens(cand_body);
    if a_ord.is_empty() || b_ord.is_empty() {
        return false;
    }
    let a: std::collections::HashSet<&String> = a_ord.iter().collect();
    let b: std::collections::HashSet<&String> = b_ord.iter().collect();
    if jaccard(&a, &b) < DUP_MIN_OVERLAP {
        return false;
    }
    // Adjacency check: same vocabulary in a different order is a different
    // claim. Bodies too short to form pairs fall back to the set verdict.
    if a_ord.len() >= 2 && b_ord.len() >= 2 {
        let a_bi: std::collections::HashSet<(&String, &String)> =
            a_ord.windows(2).map(|w| (&w[0], &w[1])).collect();
        let b_bi: std::collections::HashSet<(&String, &String)> =
            b_ord.windows(2).map(|w| (&w[0], &w[1])).collect();
        if jaccard(&a_bi, &b_bi) < DUP_MIN_BIGRAM_OVERLAP {
            return false;
        }
    }
    numbers(new_body) == numbers(cand_body) && has_negation(new_body) == has_negation(cand_body)
}

/// Two same-anchor memories conflict when they are clearly the same topic (token
/// overlap >= `CONFLICT_MIN_OVERLAP`) yet diverge on a value: a different number,
/// or an added/removed negation. This is deliberately conservative on the topic
/// side (avoid crying conflict on unrelated notes) and specific on the value
/// side (a flipped number is exactly the col9/col10 failure). Surfaced only;
/// the writer decides. Never merges.
fn looks_like_conflict(new_body: &str, cand_body: &str) -> bool {
    let a = significant_tokens(new_body);
    let b = significant_tokens(cand_body);
    if a.is_empty() || b.is_empty() {
        return false;
    }
    if jaccard(&a, &b) < CONFLICT_MIN_OVERLAP {
        return false;
    }
    let na = numbers(new_body);
    let nb = numbers(cand_body);
    let numbers_diverge = !na.is_empty() && !nb.is_empty() && na != nb;
    let negation_diverges = has_negation(new_body) != has_negation(cand_body);
    numbers_diverge || negation_diverges
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
    // One transaction: the archival sidecar has no FK cascade (additive
    // table, entries predate it), and a crash between the two DELETEs would
    // otherwise leave an orphaned flag row inflating the archived count.
    let tx = store.conn.unchecked_transaction()?;
    let n = tx.execute("DELETE FROM entries WHERE id = ?1", [id])?;
    tx.execute("DELETE FROM archived WHERE entry_id = ?1", [id])?;
    tx.commit()?;
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
