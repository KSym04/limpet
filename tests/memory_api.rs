//! Memory API behavior: writes, links, contradiction surfacing, supersede
//! semantics, JSONL round-trip.

use limpet::index;
use limpet::memory::{self, AnchorSpec, LinkSpec};
use limpet::store::Store;
use std::fs;
use tempfile::TempDir;

fn seeded_store(root: &std::path::Path) -> Store {
    fs::write(
        root.join("cache.py"),
        "def cache_get(key):\n    return store.lookup(key)\n",
    )
    .unwrap();
    let store = Store::open_in_memory().unwrap();
    index::full_index(&store, root).unwrap();
    store
}

#[test]
fn remember_anchors_and_reports_duplicates() {
    let dir = TempDir::new().unwrap();
    let store = seeded_store(dir.path());

    let first = memory::remember(
        &store,
        "insight",
        "cache_get returns None on miss, never raises",
        "explicit",
        None,
        &[AnchorSpec { file: "cache.py".into(), symbol: Some("cache_get".into()) }],
        None,
        &[],
        Some("main"),
    )
    .unwrap();
    assert_eq!(first.anchored, 1);
    assert!(first.possible_duplicates.is_empty());

    // Near-identical body on the same anchor: surfaced, not merged.
    let second = memory::remember(
        &store,
        "insight",
        "cache_get returns None when the key misses",
        "explicit",
        None,
        &[AnchorSpec { file: "cache.py".into(), symbol: Some("cache_get".into()) }],
        None,
        &[],
        Some("main"),
    )
    .unwrap();
    assert!(
        second.possible_duplicates.iter().any(|d| d["id"] == first.id.as_str()),
        "duplicate must be surfaced: {:?}",
        second.possible_duplicates
    );
    let count: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM entries", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 2, "no silent merge (I4)");
}

#[test]
fn unknown_symbol_fails_with_suggestions() {
    let dir = TempDir::new().unwrap();
    let store = seeded_store(dir.path());
    let err = memory::remember(
        &store,
        "fact",
        "something",
        "explicit",
        None,
        &[AnchorSpec { file: "cache.py".into(), symbol: Some("nonexistent_fn".into()) }],
        None,
        &[],
        None,
    )
    .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("nonexistent_fn"));
    assert!(msg.contains("cache.cache_get"), "must suggest known symbols: {msg}");
}

#[test]
fn unresolvable_anchor_fails_loud_and_writes_nothing() {
    let dir = TempDir::new().unwrap();
    let store = seeded_store(dir.path());

    // File anchor to a file limpet never indexed: loud error, no phantom
    // "anchored" count, and no orphan entry left behind.
    let err = memory::remember(
        &store,
        "insight",
        "hero block is 480px",
        "explicit",
        None,
        &[
            AnchorSpec { file: "cache.py".into(), symbol: Some("cache_get".into()) },
            AnchorSpec { file: "templates/interior.twig".into(), symbol: None },
        ],
        None,
        &[],
        None,
    )
    .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("templates/interior.twig"), "error must name the bad anchor: {msg}");
    assert!(msg.contains("not in the index"), "error must say why: {msg}");

    let entries: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM entries", [], |r| r.get(0))
        .unwrap();
    let anchors: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM anchors", [], |r| r.get(0))
        .unwrap();
    assert_eq!((entries, anchors), (0, 0), "failed remember must persist nothing");
}

#[test]
fn failed_symbol_anchor_leaves_no_orphan_entry() {
    let dir = TempDir::new().unwrap();
    let store = seeded_store(dir.path());
    let _ = memory::remember(
        &store,
        "fact",
        "something",
        "explicit",
        None,
        &[AnchorSpec { file: "cache.py".into(), symbol: Some("nonexistent_fn".into()) }],
        None,
        &[],
        None,
    )
    .unwrap_err();
    let entries: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM entries", [], |r| r.get(0))
        .unwrap();
    assert_eq!(entries, 0, "failed remember must not leave an orphan entry");
}

#[test]
fn file_anchor_stores_content_hash_at_write_time() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("style.scss"), ".a { color: red; }\n").unwrap();
    let store = seeded_store(dir.path());
    let r = memory::remember(
        &store,
        "insight",
        "brand red lives here, do not hardcode it elsewhere",
        "explicit",
        None,
        &[AnchorSpec { file: "style.scss".into(), symbol: None }],
        None,
        &[],
        None,
    )
    .unwrap();
    assert_eq!(r.anchored, 1);
    let (anchor_hash, file_hash): (Option<String>, String) = store
        .conn
        .query_row(
            "SELECT a.ast_body_hash, f.hash FROM anchors a
             JOIN files f ON f.path = a.file WHERE a.entry_id = ?1",
            [&r.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(anchor_hash.as_deref(), Some(file_hash.as_str()));
}

#[test]
fn kind_and_source_validation() {
    let dir = TempDir::new().unwrap();
    let store = seeded_store(dir.path());
    assert!(memory::remember(&store, "vibe", "x", "explicit", None, &[], None, &[], None).is_err());
    assert!(memory::remember(&store, "fact", "", "explicit", None, &[], None, &[], None).is_err());
    assert!(memory::remember(&store, "fact", "x", "psychic", None, &[], None, &[], None).is_err());
}

#[test]
fn mined_confidence_is_capped() {
    let dir = TempDir::new().unwrap();
    let store = seeded_store(dir.path());
    let r = memory::remember(&store, "episode", "tried X, failed", "mined", Some(0.9), &[], None, &[], None)
        .unwrap();
    let conf: f64 = store
        .conn
        .query_row("SELECT confidence FROM entries WHERE id = ?1", [&r.id], |x| x.get(0))
        .unwrap();
    assert!(conf <= 0.5, "mined memories cap at 0.5, got {conf}");
}

#[test]
fn contradiction_keeps_both_supersede_resolves() {
    let dir = TempDir::new().unwrap();
    let store = seeded_store(dir.path());
    let old = memory::remember(&store, "fact", "timeout is 30 seconds", "explicit", None, &[], None, &[], None)
        .unwrap();
    let new = memory::remember(
        &store,
        "fact",
        "timeout is 60 seconds since the pool rewrite",
        "explicit",
        None,
        &[],
        None,
        &[LinkSpec { target: old.id.clone(), rel: "contradicts".into() }],
        None,
    )
    .unwrap();

    // Both alive while contradiction stands.
    let statuses: Vec<String> = {
        let mut stmt = store
            .conn
            .prepare("SELECT status FROM entries ORDER BY id")
            .unwrap();
        stmt.query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    assert_eq!(statuses, vec!["active", "active"]);

    // Recall surfaces the conflict on both sides.
    let out = memory::recall::recall(&store, "what is the timeout", &[], 2000).unwrap();
    assert!(out.items.len() >= 2);
    for item in out.items.iter().filter(|i| i.id == old.id || i.id == new.id) {
        assert!(
            item.flags.iter().any(|f| f.starts_with("contradicted-by:")),
            "conflict must be visible on {}: {:?}",
            item.id,
            item.flags
        );
    }

    // Supersede ends the argument; old drops out of recall.
    memory::add_link(&store, &new.id, &old.id, "supersedes").unwrap();
    let (st,): (String,) = store
        .conn
        .query_row("SELECT status FROM entries WHERE id = ?1", [&old.id], |r| {
            Ok((r.get(0)?,))
        })
        .unwrap();
    assert_eq!(st, "superseded");
    let out2 = memory::recall::recall(&store, "what is the timeout", &[], 2000).unwrap();
    assert!(out2.items.iter().all(|i| i.id != old.id), "superseded must leave recall");
}

#[test]
fn link_to_missing_target_fails() {
    let dir = TempDir::new().unwrap();
    let store = seeded_store(dir.path());
    let r = memory::remember(&store, "fact", "x", "explicit", None, &[], None, &[], None).unwrap();
    assert!(memory::add_link(&store, &r.id, "01НЕСУЩЕСТВУЕТ", "supports").is_err());
    assert!(memory::add_link(&store, &r.id, &r.id, "invalid_rel").is_err());
}

#[test]
fn jsonl_roundtrip_is_lossless() {
    let dir = TempDir::new().unwrap();
    let store = seeded_store(dir.path());
    let a = memory::remember(
        &store,
        "decision",
        "chose sqlite over flat files for FTS5",
        "explicit",
        None,
        &[AnchorSpec { file: "cache.py".into(), symbol: None }],
        None,
        &[],
        Some("main"),
    )
    .unwrap();
    let _b = memory::remember(
        &store,
        "fact",
        "lookup is O(1)",
        "explicit",
        None,
        &[],
        Some(&memory::Evidence { command: "pytest -q".into(), output: "2 passed".into() }),
        &[LinkSpec { target: a.id.clone(), rel: "supports".into() }],
        None,
    )
    .unwrap();

    let mut exported = Vec::new();
    let n = store.export_jsonl(&mut exported).unwrap();
    assert_eq!(n, 2);

    let dir2 = TempDir::new().unwrap();
    let mut fresh = seeded_store(dir2.path());
    let report = fresh
        .import_jsonl(&mut std::io::BufReader::new(exported.as_slice()))
        .unwrap();
    assert_eq!(report.added, 2);
    assert_eq!(report.updated, 0);

    let mut re_exported = Vec::new();
    fresh.export_jsonl(&mut re_exported).unwrap();
    assert_eq!(
        String::from_utf8(exported).unwrap(),
        String::from_utf8(re_exported).unwrap(),
        "export -> import -> export must be byte-identical"
    );

    // Re-import of the same data is a no-op.
    let mut exported2 = Vec::new();
    fresh.export_jsonl(&mut exported2).unwrap();
    let report2 = fresh
        .import_jsonl(&mut std::io::BufReader::new(exported2.as_slice()))
        .unwrap();
    assert_eq!(report2.added, 0);
    assert_eq!(report2.skipped, 2);
}
