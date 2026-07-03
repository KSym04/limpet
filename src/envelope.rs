//! The honesty envelope (invariant I2).
//!
//! Every tool response is `{ "data": ..., "meta": ... }`. The meta block
//! always states how fresh the index is, how complete the answer is, and
//! how much of what was returned is stale or contradicted. There is no
//! code path that truncates without saying so.

use crate::index::SweepReport;
use crate::store::Store;
use serde_json::{json, Value};

pub struct Completeness {
    pub matched: usize,
    pub returned: usize,
    pub omitted_reason: Option<&'static str>,
}

pub fn build_meta(
    store: &Store,
    sweep: &SweepReport,
    completeness: Completeness,
    stale: usize,
    contradicted: usize,
) -> Value {
    let indexed_at = store
        .kv_get("indexed_at")
        .ok()
        .flatten()
        .unwrap_or_else(|| "never".to_string());

    // Compact but never silent: empty lists collapse to counts of zero,
    // non-empty lists are shown (capped) so nothing is hidden.
    let mut freshness = serde_json::Map::new();
    freshness.insert("indexed_at".into(), json!(indexed_at));
    if sweep.sweep_failed {
        // Freshness is unknown, not clean; say so (audit 2026-07).
        freshness.insert("sweep_failed".into(), json!(true));
    }
    if !sweep.reindexed.is_empty() {
        freshness.insert("reindexed_now".into(), json!(sweep.reindexed.len()));
    }
    freshness.insert("dirty".into(), json!(sweep.dirty.len()));
    if !sweep.dirty.is_empty() {
        freshness.insert(
            "dirty_files".into(),
            json!(sweep.dirty.iter().take(10).collect::<Vec<_>>()),
        );
    }
    if !sweep.failed.is_empty() {
        freshness.insert(
            "failed_files".into(),
            json!(sweep.failed.iter().take(10).collect::<Vec<_>>()),
        );
    }

    let mut completeness_obj = serde_json::Map::new();
    completeness_obj.insert("matched".into(), json!(completeness.matched));
    completeness_obj.insert("returned".into(), json!(completeness.returned));
    if let Some(reason) = completeness.omitted_reason {
        completeness_obj.insert("omitted_reason".into(), json!(reason));
    }

    json!({
        "freshness": Value::Object(freshness),
        "completeness": Value::Object(completeness_obj),
        "staleness": { "stale": stale, "contradicted": contradicted },
    })
}

pub fn envelope(data: Value, meta: Value) -> Value {
    json!({ "data": data, "meta": meta })
}
