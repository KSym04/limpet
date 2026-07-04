//! Recall evaluation harness (invariant I8): precision@3 on a seeded
//! store, stale surfacing, working-set proximity, and budget packing.

use limpet::index;
use limpet::memory::{self, recall, AnchorSpec};
use limpet::store::Store;
use std::fs;
use tempfile::TempDir;

/// Seeded mini-project: scanner + exporter + auth, with 12 memories.
fn seed() -> (TempDir, Store, Vec<(String, &'static str)>) {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("scan")).unwrap();
    fs::create_dir_all(root.join("export")).unwrap();
    fs::create_dir_all(root.join("auth")).unwrap();
    fs::write(
        root.join("scan/scanner.py"),
        "def scan_products(batch):\n    return [check(p) for p in batch]\n\ndef check(p):\n    return p.ok\n",
    )
    .unwrap();
    fs::write(
        root.join("export/csv_export.py"),
        "def export_csv(rows):\n    return write_file(rows)\n",
    )
    .unwrap();
    fs::write(
        root.join("auth/session.py"),
        "def verify_nonce(token):\n    return token.valid\n",
    )
    .unwrap();
    let store = Store::open_in_memory().unwrap();
    index::full_index(&store, root).unwrap();

    let mut ids = Vec::new();
    let mut add = |kind: &str,
                   body: &'static str,
                   file: Option<&str>,
                   symbol: Option<&str>| {
        let anchors: Vec<AnchorSpec> = file
            .map(|f| {
                vec![AnchorSpec {
                    file: f.to_string(),
                    symbol: symbol.map(str::to_string),
                }]
            })
            .unwrap_or_default();
        let r = memory::remember(&store, kind, body, "explicit", None, &anchors, None, &[], None, false, None)
            .unwrap();
        ids.push((r.id, body));
    };

    add("fact", "scan_products processes items in batches of 50, larger batches hit the memory cap", Some("scan/scanner.py"), Some("scan_products"));
    add("decision", "batch size 50 chosen because shared hosts kill requests over 30s", Some("scan/scanner.py"), Some("scan_products"));
    add("insight", "csv export must quote fields containing semicolons, Excel splits them otherwise", Some("export/csv_export.py"), Some("export_csv"));
    add("fact", "verify_nonce rejects tokens older than 12 hours", Some("auth/session.py"), Some("verify_nonce"));
    add("intent", "the scan module exists to pre-validate product feeds before Google Merchant sees them", Some("scan/scanner.py"), None);
    add("episode", "tried streaming the csv export, reverted: WP output buffering broke chunked responses", Some("export/csv_export.py"), Some("export_csv"));
    add("insight", "product check treats missing gtin as warning not error", Some("scan/scanner.py"), Some("check"));
    add("fact", "session tokens are stored hashed, never plaintext", Some("auth/session.py"), None);
    add("decision", "exports write to uploads dir because plugin dir is read-only on managed hosts", Some("export/csv_export.py"), None);
    add("insight", "scanner skips draft products on purpose, do not fix as a bug", Some("scan/scanner.py"), Some("scan_products"));
    add("episode", "renaming check() to validate() broke third party hooks, rolled back", Some("scan/scanner.py"), Some("check"));
    add("fact", "nonce verification adds 3ms per request, measured", Some("auth/session.py"), Some("verify_nonce"));

    (dir, store, ids)
}

fn top_ids(store: &Store, task: &str, ws: &[String], k: usize) -> Vec<String> {
    recall::recall(store, task, ws, 100_000)
        .unwrap()
        .items
        .into_iter()
        .take(k)
        .map(|i| i.id)
        .collect()
}

#[test]
fn precision_at_3_on_seeded_queries() {
    let (_dir, store, ids) = seed();
    let id_of = |body: &str| {
        ids.iter()
            .find(|(_, b)| *b == body)
            .map(|(i, _)| i.clone())
            .unwrap()
    };

    // Each query names its single must-hit memory; it must appear in top 3.
    let cases: Vec<(&str, String)> = vec![
        (
            "why is the scan batch size 50",
            id_of("batch size 50 chosen because shared hosts kill requests over 30s"),
        ),
        (
            "csv export excel semicolon problem",
            id_of("csv export must quote fields containing semicolons, Excel splits them otherwise"),
        ),
        (
            "how long are nonce tokens valid",
            id_of("verify_nonce rejects tokens older than 12 hours"),
        ),
        (
            "why does the scanner skip draft products",
            id_of("scanner skips draft products on purpose, do not fix as a bug"),
        ),
        (
            "what happened when someone renamed check",
            id_of("renaming check() to validate() broke third party hooks, rolled back"),
        ),
        (
            "what is the scan module for",
            id_of("the scan module exists to pre-validate product feeds before Google Merchant sees them"),
        ),
    ];

    let mut hits = 0;
    let total = cases.len();
    for (query, expected) in &cases {
        let top = top_ids(&store, query, &[], 3);
        if top.contains(expected) {
            hits += 1;
        } else {
            eprintln!("MISS query={query} expected={expected} got={top:?}");
        }
    }
    assert_eq!(hits, total, "precision@3 must be {total}/{total}, got {hits}/{total}");
}

#[test]
fn working_set_pulls_local_memories_up() {
    let (_dir, store, _ids) = seed();
    // Vague task; the working set decides what matters.
    let ws = vec!["export/csv_export.py".to_string()];
    let out = recall::recall(&store, "fix the output formatting", &ws, 100_000).unwrap();
    assert!(!out.items.is_empty());
    let top = &out.items[0];
    assert!(
        top.anchors.iter().any(|a| a.contains("csv_export") || a.contains("export")),
        "top memory should be anchored near the working set, got {:?} ({})",
        top.anchors,
        top.body
    );
}

#[test]
fn budget_packs_greedy_and_reports_omissions() {
    let (_dir, store, _ids) = seed();
    let full = recall::recall(&store, "scan products batches feeds", &[], 100_000).unwrap();
    assert!(full.matched >= 3, "seed should match several: {}", full.matched);

    let tight = recall::recall(&store, "scan products batches feeds", &[], 120).unwrap();
    assert!(tight.returned < full.matched, "budget must cut the list");
    assert!(tight.returned >= 1, "never return nothing when something matched");
    assert_eq!(tight.matched, full.matched, "matched count must not shrink with budget");

    // Best-first: the packed head equals the unpacked head.
    assert_eq!(tight.items[0].id, full.items[0].id);
}

#[test]
fn empty_store_returns_empty_not_error() {
    let dir = TempDir::new().unwrap();
    let store = Store::open_in_memory().unwrap();
    index::full_index(&store, dir.path()).unwrap();
    let out = recall::recall(&store, "anything at all", &[], 1000).unwrap();
    assert_eq!(out.matched, 0);
    assert_eq!(out.returned, 0);
}
