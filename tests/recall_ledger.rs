//! Tests for M2: per-call savings ledger surfaced in the recall envelope.
//! I-L2: saved is never floored. I-L5: ledger cannot fail the recall.
//! Sink: after recall, export JSONL contains no ledger keys.

use limpet::index;
use limpet::memory::{self, AnchorSpec};
use limpet::store::Store;
use tempfile::TempDir;

/// Seed one anchored memory so recall returns at least one item.
fn seed_one_memory(store: &Store, root: &std::path::Path) {
    std::fs::write(
        root.join("why.py"),
        "def why():\n    return 42\n",
    )
    .unwrap();
    index::full_index(store, root).unwrap();
    memory::remember(
        store,
        "fact",
        "why returns 42 because the answer to everything",
        "explicit",
        None,
        &[AnchorSpec { file: "why.py".into(), symbol: Some("why".into()) }],
        None,
        &[],
        None,
        false,
        None,
    )
    .unwrap();
}

#[test]
fn recall_envelope_carries_honest_ledger() {
    let dir = TempDir::new().unwrap();
    let store = Store::open_in_memory().unwrap();
    seed_one_memory(&store, dir.path());
    let args = serde_json::json!({ "task": "why" });
    let resp = limpet::tools::dispatch_for_test("recall", &store, &args).unwrap();
    let led = &resp["meta"]["ledger"];
    assert!(!led.is_null(), "recall meta carries a ledger block");
    assert_eq!(led["estimate"], true);
    for k in ["served", "baseline", "saved", "reads_avoided", "cumulative_saved"] {
        assert!(led.get(k).is_some(), "ledger has {k}");
    }
    // saved == baseline - served, never floored (I-L2).
    assert_eq!(
        led["saved"].as_i64().unwrap(),
        led["baseline"].as_i64().unwrap() - led["served"].as_i64().unwrap()
    );
}

#[test]
fn recall_sink_untouched_after_recall() {
    let dir = TempDir::new().unwrap();
    let store = Store::open_in_memory().unwrap();
    seed_one_memory(&store, dir.path());
    // Trigger a recall so the ledger accumulates.
    let args = serde_json::json!({ "task": "why" });
    limpet::tools::dispatch_for_test("recall", &store, &args).unwrap();
    // Export to JSONL (in-memory: write to a Vec).
    let mut out: Vec<u8> = Vec::new();
    store.export_jsonl(&mut out).unwrap();
    let text = String::from_utf8(out).unwrap();
    // The ledger lives only in meta_kv; it must never bleed into the export.
    assert!(
        !text.contains("\"ledger\""),
        "export must not contain ledger key, got: {text}"
    );
    assert!(
        !text.contains("reads_avoided"),
        "export must not contain reads_avoided, got: {text}"
    );
    assert!(
        !text.contains("cumulative_saved"),
        "export must not contain cumulative_saved, got: {text}"
    );
}
