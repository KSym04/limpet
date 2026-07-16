//! Recall: ranked, budget-packed memory retrieval.
//!
//! Score = 0.45 * bm25_norm + 0.25 * proximity + 0.20 * decayed confidence
//!       + 0.10 * recency, then a small kind nudge. Stale and contradicted
//!       entries are never filtered by score; they are flagged (I3).

use crate::index::now_iso;
use crate::memory::{decayed_confidence, fts_escape, parse_iso_secs};
use crate::store::Store;
use crate::util::token_estimate;
use anyhow::Result;
use std::collections::{HashMap, HashSet};

#[derive(Debug, serde::Serialize)]
pub struct RecallItem {
    pub id: String,
    pub kind: String,
    pub body: String,
    pub source: String,
    pub status: String,
    pub confidence: f64,
    pub score: f64,
    pub created_at: String,
    pub anchors: Vec<String>,
    /// "stale:<reason>", "contradicted-by:<id>", "reverify:<command>"
    pub flags: Vec<String>,
    /// Repo-relative files this item anchors to; feeds the savings ledger's
    /// baseline. Not serialized: the wire shape is built in tools.rs.
    #[serde(skip)]
    pub anchor_files: Vec<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct RecallResult {
    pub items: Vec<RecallItem>,
    pub matched: usize,
    pub returned: usize,
    /// Items dropped by the relevance floor (as opposed to the token
    /// budget), so the envelope can state the true omission reason.
    pub cut_by_floor: usize,
}

/// Per-item fixed overhead in the packed response (labels, flags, ids),
/// counted against the budget alongside the body itself.
const ITEM_OVERHEAD_TOKENS: usize = 30;

pub fn recall(
    store: &Store,
    task: &str,
    working_set: &[String],
    budget_tokens: usize,
) -> Result<RecallResult> {
    let now = now_iso();
    let fts_query = fts_escape(task);

    // Candidates: FTS matches, plus everything anchored near the working
    // set (an agent's current files matter even when vocabulary differs).
    let mut candidate_ids: HashSet<String> = HashSet::new();
    if !fts_query.is_empty() {
        let mut stmt = store.conn.prepare(
            "SELECT e.id FROM entries_fts f JOIN entries e ON e.rowid = f.rowid
             WHERE entries_fts MATCH ?1 ORDER BY rank LIMIT 200",
        )?;
        for id in stmt.query_map([&fts_query], |r| r.get::<_, String>(0))? {
            candidate_ids.insert(id?);
        }
    }
    if !working_set.is_empty() {
        let mut stmt = store
            .conn
            .prepare("SELECT DISTINCT entry_id FROM anchors WHERE file = ?1")?;
        for f in working_set {
            for id in stmt.query_map([f], |r| r.get::<_, String>(0))? {
                candidate_ids.insert(id?);
            }
        }
    }
    if candidate_ids.is_empty() {
        return Ok(RecallResult { items: vec![], matched: 0, returned: 0, cut_by_floor: 0 });
    }

    // BM25 rank per candidate (lower rank value = better in FTS5; normalize
    // to 0..1 where 1 is best).
    let mut bm25: HashMap<String, f64> = HashMap::new();
    if !fts_query.is_empty() {
        let mut stmt = store.conn.prepare(
            "SELECT e.id, rank FROM entries_fts f JOIN entries e ON e.rowid = f.rowid
             WHERE entries_fts MATCH ?1",
        )?;
        let scores: Vec<(String, f64)> = stmt
            .query_map([&fts_query], |r| Ok((r.get(0)?, r.get::<_, f64>(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        if !scores.is_empty() {
            let min = scores.iter().map(|(_, s)| *s).fold(f64::INFINITY, f64::min);
            let max = scores.iter().map(|(_, s)| *s).fold(f64::NEG_INFINITY, f64::max);
            for (id, s) in scores {
                // FTS5 rank is negative-better; invert and min-max.
                let norm = if (max - min).abs() < 1e-12 { 1.0 } else { (max - s) / (max - min) };
                bm25.insert(id, norm);
            }
        }
    }

    // One-hop import neighborhood of the working set for proximity scoring.
    let mut neighborhood: HashSet<String> = HashSet::new();
    if !working_set.is_empty() {
        let mut fwd = store
            .conn
            .prepare("SELECT target FROM imports WHERE file = ?1")?;
        // ESCAPE '\' with %/_ escaped in the stem: a caller-supplied file
        // name containing LIKE metacharacters must not widen the match.
        let mut rev = store.conn.prepare(
            "SELECT file FROM imports WHERE target LIKE '%' || ?1 || '%' ESCAPE '\\'",
        )?;
        for f in working_set {
            for t in fwd.query_map([f], |r| r.get::<_, String>(0))? {
                neighborhood.insert(t?);
            }
            let stem = std::path::Path::new(f)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| f.clone())
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_");
            for t in rev.query_map([&stem], |r| r.get::<_, String>(0))? {
                neighborhood.insert(t?);
            }
        }
    }
    let ws_set: HashSet<&String> = working_set.iter().collect();
    let ws_dirs: HashSet<String> = working_set
        .iter()
        .filter_map(|f| f.split('/').next().map(str::to_string))
        .collect();

    let now_secs = parse_iso_secs(&now).unwrap_or(0);
    let mut items: Vec<RecallItem> = Vec::new();

    for id in &candidate_ids {
        let row = store.conn.query_row(
            "SELECT kind, body, source, status, confidence, created_at,
                    stale_reason, evidence_cmd
             FROM entries WHERE id = ?1
               AND NOT EXISTS (SELECT 1 FROM archived a WHERE a.entry_id = entries.id)",
            [id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, f64>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, Option<String>>(6)?,
                    r.get::<_, Option<String>>(7)?,
                ))
            },
        );
        let Ok((kind, body, source, status, conf0, created_at, stale_reason, ev_cmd)) = row
        else {
            continue;
        };
        if status == "superseded" {
            continue; // History stays queryable via admin, not recall.
        }

        let mut astmt = store.conn.prepare(
            "SELECT file, symbol_fqn FROM anchors WHERE entry_id = ?1",
        )?;
        let anchor_rows: Vec<(String, Option<String>)> = astmt
            .query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;

        let proximity = anchor_rows
            .iter()
            .map(|(file, _)| {
                if ws_set.contains(file) {
                    1.0
                } else if neighborhood.contains(file) {
                    0.6
                } else if file
                    .split('/')
                    .next()
                    .map(|d| ws_dirs.contains(d))
                    .unwrap_or(false)
                {
                    0.3
                } else {
                    0.0
                }
            })
            .fold(0.0_f64, f64::max);

        let confidence = decayed_confidence(conf0, &created_at, &now);
        let recency = match parse_iso_secs(&created_at) {
            Some(c) => {
                let age_days = ((now_secs - c) as f64 / 86_400.0).max(0.0);
                (1.0 - age_days / 365.0).clamp(0.0, 1.0)
            }
            None => 0.5,
        };
        let text_score = bm25.get(id).copied().unwrap_or(0.0);

        let mut score =
            0.45 * text_score + 0.25 * proximity + 0.20 * confidence + 0.10 * recency;
        score += match kind.as_str() {
            "fact" | "decision" => 0.05,
            "episode" => -0.05,
            _ => 0.0,
        };
        // Verification is a ranking signal, not just a label: an unverified
        // claim must never tie a proven fact. `verified` (evidence on file)
        // earns a boost; `explicit` (someone typed it, nothing re-runnable) is
        // downranked so truth wins ties. `mined` is imported, lower-trust.
        // The boost is earned by LIVE evidence: once the anchor rots (stale /
        // invalidated) the proof is rotten too, so the boost dies with it and
        // fresh knowledge outranks it (the I-A1 principle: no signal ranks rot
        // above fresh). The item still surfaces, still flagged (I3).
        score += match source.as_str() {
            "verified" if status == "active" => 0.10,
            "verified" => 0.0,
            "mined" => -0.05,
            _ => -0.05, // explicit / unverified
        };

        let mut flags = Vec::new();
        if status == "stale" {
            flags.push(format!(
                "stale:{}",
                stale_reason.as_deref().unwrap_or("unknown")
            ));
            if source == "verified" {
                if let Some(cmd) = &ev_cmd {
                    flags.push(format!("reverify:{cmd}"));
                }
            }
        }
        if status == "invalidated" {
            flags.push(format!(
                "invalidated:{}",
                stale_reason.as_deref().unwrap_or("anchor_deleted")
            ));
        }
        let mut cstmt = store.conn.prepare(
            "SELECT src FROM links WHERE dst = ?1 AND rel = 'contradicts'
             UNION SELECT dst FROM links WHERE src = ?1 AND rel = 'contradicts'",
        )?;
        for c in cstmt.query_map([id], |r| r.get::<_, String>(0))? {
            flags.push(format!("contradicted-by:{}", c?));
        }

        items.push(RecallItem {
            id: id.clone(),
            kind,
            body,
            source,
            status,
            confidence,
            score,
            created_at,
            anchors: anchor_rows
                .iter()
                .map(|(f, s)| s.clone().unwrap_or_else(|| f.clone()))
                .collect(),
            anchor_files: anchor_rows.iter().map(|(f, _)| f.clone()).collect(),
            flags,
        });
    }

    items.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    let matched = items.len();

    // Relevance cutoff: items scoring far below the best match are noise
    // that costs the agent tokens. Two carve-outs keep it honest
    // (audit 2026-07): flagged items (stale/contradicted) are exempt, so
    // the cutoff can never silently hide exactly what I3 promises to show;
    // and a non-positive top score disables the cutoff entirely, because
    // top*0.35 > top when top < 0 would drop every item including the best.
    let mut cut_by_floor = 0usize;
    if let Some(top) = items.first().map(|i| i.score) {
        if top > 0.0 {
            let floor = top * 0.35;
            let before = items.len();
            items.retain(|i| i.score >= floor || !i.flags.is_empty());
            cut_by_floor = before - items.len();
        }
    }

    // Greedy budget packing, best first. No silent truncation: the caller
    // receives matched vs returned counts for the honesty envelope.
    let mut packed = Vec::new();
    let mut spent = 0usize;
    for item in items {
        let cost = token_estimate(&item.body) + ITEM_OVERHEAD_TOKENS;
        if spent + cost > budget_tokens && !packed.is_empty() {
            continue;
        }
        if spent + cost <= budget_tokens || packed.is_empty() {
            spent += cost;
            packed.push(item);
        }
    }
    let returned = packed.len();
    Ok(RecallResult { items: packed, matched, returned, cut_by_floor })
}

/// Flat cost of the search round trips the agent skips by recalling.
/// MUST match bench/token_savings.py, which charges the same constant on
/// the "without limpet" side (I-L3: identical methodology, both directions).
pub const SEARCH_OVERHEAD_TOKENS: usize = 300;

#[derive(Debug, Default, PartialEq)]
pub struct RecallCost {
    /// What the pack actually costs the client, counted exactly like the
    /// budget packer above (I-L1): body estimate + per-item overhead.
    pub served: i64,
    /// Search overhead + ceil(size/4) over the DISTINCT files the returned
    /// items anchor to: the minimal file set a memory-less agent reads.
    pub baseline: i64,
    /// Distinct anchored files of ACTIVE items: reads recall made unnecessary.
    /// Flagged items do not count; the agent is told to verify those.
    pub reads_avoided: i64,
}

/// Price one recall against its file-reading counterfactual. Pure: file
/// sizes come from the caller. Anchorless memories contribute zero baseline
/// (I-L4), deliberately understating savings on not-in-any-file questions.
pub fn recall_cost(items: &[RecallItem], file_size: impl Fn(&str) -> Option<i64>) -> RecallCost {
    let mut cost = RecallCost::default();
    if items.is_empty() {
        return cost;
    }
    let mut files: HashSet<&str> = HashSet::new();
    let mut active_files: HashSet<&str> = HashSet::new();
    for item in items {
        cost.served += (token_estimate(&item.body) + ITEM_OVERHEAD_TOKENS) as i64;
        for f in &item.anchor_files {
            files.insert(f);
            if item.status == "active" {
                active_files.insert(f);
            }
        }
    }
    cost.baseline = SEARCH_OVERHEAD_TOKENS as i64
        + files
            .iter()
            .filter_map(|f| file_size(f))
            .map(|sz| (sz.max(0) + 3) / 4)
            .sum::<i64>();
    cost.reads_avoided = active_files.len() as i64;
    cost
}

#[cfg(test)]
mod cost_tests {
    use super::*;

    fn item(body: &str, status: &str, files: &[&str]) -> RecallItem {
        RecallItem {
            id: "01TEST".into(),
            kind: "fact".into(),
            body: body.into(),
            source: "explicit".into(),
            status: status.into(),
            confidence: 0.8,
            score: 1.0,
            created_at: "2026-07-03T00:00:00Z".into(),
            anchors: files.iter().map(|f| f.to_string()).collect(),
            anchor_files: files.iter().map(|f| f.to_string()).collect(),
            flags: vec![],
        }
    }

    #[test]
    fn served_matches_packer_counting_exactly() {
        let items = vec![item("abcdefgh", "active", &["a.py"])]; // 8 bytes -> 2 tokens
        let c = recall_cost(&items, |_| Some(4000));
        assert_eq!(c.served, (2 + ITEM_OVERHEAD_TOKENS) as i64);
        assert_eq!(c.baseline, (SEARCH_OVERHEAD_TOKENS + 1000) as i64);
        assert_eq!(c.reads_avoided, 1);
    }

    #[test]
    fn distinct_files_counted_once_and_stale_items_avoid_nothing() {
        let items = vec![
            item("x", "active", &["a.py", "b.py"]),
            item("y", "stale", &["a.py", "c.py"]),
        ];
        let c = recall_cost(&items, |_| Some(400)); // 100 tokens each
        // baseline: 300 + 3 distinct files * 100
        assert_eq!(c.baseline, 600);
        // reads_avoided: only the ACTIVE item's files
        assert_eq!(c.reads_avoided, 2);
    }

    #[test]
    fn anchorless_contributes_zero_baseline_and_negatives_are_real() {
        // A verbose anchorless memory: baseline is just the search overhead,
        // served exceeds it, saved goes negative and stays negative (I-L2).
        let long_body = "z".repeat(4000); // 1000 tokens
        let items = vec![item(&long_body, "active", &[])];
        let c = recall_cost(&items, |_| None);
        assert_eq!(c.baseline, SEARCH_OVERHEAD_TOKENS as i64);
        assert!(c.baseline - c.served < 0, "negative saving must survive");
        assert_eq!(c.reads_avoided, 0);
    }

    #[test]
    fn empty_pack_costs_and_saves_nothing() {
        let c = recall_cost(&[], |_| Some(1_000_000));
        assert_eq!(c, RecallCost::default());
    }
}
