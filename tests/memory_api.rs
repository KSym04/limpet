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
        false,
        None,
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
        false,
        None,
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
        false,
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
        false,
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
        false,
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
        false,
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
fn verified_without_evidence_is_refused() {
    let dir = TempDir::new().unwrap();
    let store = seeded_store(dir.path());
    let err = memory::remember(
        &store, "fact", "claims to be proven", "verified", None, &[], None, &[], None, false, None,
    )
    .unwrap_err();
    assert!(err.to_string().contains("evidence"), "{err}");
}

#[test]
fn ambiguous_bare_name_is_refused_with_candidates() {
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("two.py"),
        "class A:\n    def push(self):\n        return 1\n\nclass B:\n    def push(self):\n        return 2\n",
    )
    .unwrap();
    let store = seeded_store(dir.path());
    let err = memory::remember(
        &store,
        "insight",
        "push does a thing",
        "explicit",
        None,
        &[AnchorSpec { file: "two.py".into(), symbol: Some("push".into()) }],
        None,
        &[],
        None,
        false,
        None,
    )
    .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("ambiguous"), "{msg}");
    assert!(msg.contains("two.A.push") && msg.contains("two.B.push"), "must list candidates: {msg}");
}

#[test]
fn kind_and_source_validation() {
    let dir = TempDir::new().unwrap();
    let store = seeded_store(dir.path());
    assert!(memory::remember(&store, "vibe", "x", "explicit", None, &[], None, &[], None, false, None).is_err());
    assert!(memory::remember(&store, "fact", "", "explicit", None, &[], None, &[], None, false, None).is_err());
    assert!(memory::remember(&store, "fact", "x", "psychic", None, &[], None, &[], None, false, None).is_err());
}

#[test]
fn mined_confidence_is_capped() {
    let dir = TempDir::new().unwrap();
    let store = seeded_store(dir.path());
    let r = memory::remember(&store, "episode", "tried X, failed", "mined", Some(0.9), &[], None, &[], None, false, None)
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
    let old = memory::remember(&store, "fact", "timeout is 30 seconds", "explicit", None, &[], None, &[], None, false, None)
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
        false,
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
    let r = memory::remember(&store, "fact", "x", "explicit", None, &[], None, &[], None, false, None).unwrap();
    assert!(memory::add_link(&store, &r.id, "01НЕСУЩЕСТВУЕТ", "supports").is_err());
    assert!(memory::add_link(&store, &r.id, &r.id, "invalid_rel").is_err());
}

#[test]
fn import_rejects_secrets_future_dates_and_clamps_confidence() {
    use std::io::BufReader;
    let dir = TempDir::new().unwrap();
    let mut store = seeded_store(dir.path());

    // A hostile export: a secret-bearing body, a future-dated poison entry,
    // and an out-of-range confidence.
    let lines = concat!(
        r#"{"id":"01SECRET0000000000000000AA","kind":"insight","body":"deploy key ghp_1234567890abcdefghijklmnopqrstuvwxyz here","created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z","source":"explicit","confidence":0.8,"status":"active","anchors":[],"links":[]}"#, "\n",
        r#"{"id":"01FUTURE0000000000000000BB","kind":"fact","body":"benign but future dated","created_at":"9999-01-01T00:00:00Z","updated_at":"9999-01-01T00:00:00Z","source":"explicit","confidence":0.8,"status":"active","anchors":[],"links":[]}"#, "\n",
        r#"{"id":"01CLAMP00000000000000000CC","kind":"fact","body":"huge confidence","created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z","source":"explicit","confidence":1e300,"status":"active","anchors":[],"links":[]}"#, "\n",
    );
    let report = store
        .import_jsonl(&mut BufReader::new(lines.as_bytes()))
        .unwrap();
    assert_eq!(report.rejected, 2, "secret and future-date lines rejected");
    assert_eq!(report.added, 1, "only the clamp line is applied");

    let secret_present: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM entries WHERE id = '01SECRET0000000000000000AA'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(secret_present, 0, "secret-bearing entry must never enter the store");

    let conf: f64 = store
        .conn
        .query_row("SELECT confidence FROM entries WHERE id = '01CLAMP00000000000000000CC'", [], |r| r.get(0))
        .unwrap();
    assert!(conf <= 1.0, "confidence must be clamped, got {conf}");
}

#[test]
fn import_reresolves_anchor_hash_against_local_index() {
    use std::io::BufReader;
    let dir = TempDir::new().unwrap();
    let mut store = seeded_store(dir.path()); // seeds cache.py with cache_get

    // A forged-fresh anchor: claims a hash the attacker chose. On import it
    // must be replaced by cache_get's ACTUAL local hash (or left to resolve
    // honestly), never trusted verbatim.
    let line = r#"{"id":"01FORGED0000000000000000DD","kind":"fact","body":"cache_get behaviour","created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z","source":"explicit","confidence":0.8,"status":"active","anchors":[{"file":"cache.py","symbol_fqn":"cache.cache_get","ast_body_hash":"deadbeefdeadbeefdeadbeefdeadbeef","context_hint":null}],"links":[]}"#;
    store.import_jsonl(&mut BufReader::new(line.as_bytes())).unwrap();

    let (stored_hash, real_hash): (Option<String>, String) = store
        .conn
        .query_row(
            "SELECT a.ast_body_hash, s.body_hash FROM anchors a
             JOIN symbols s ON s.fqn = a.symbol_fqn
             WHERE a.entry_id = '01FORGED0000000000000000DD'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(stored_hash.as_deref(), Some(real_hash.as_str()), "imported hash must be re-resolved to local");
    assert_ne!(stored_hash.as_deref(), Some("deadbeefdeadbeefdeadbeefdeadbeef"), "forged hash must not survive");
}

#[test]
fn oversize_body_is_refused() {
    let dir = TempDir::new().unwrap();
    let store = seeded_store(dir.path());
    let huge = "x".repeat(70 * 1024);
    let err = memory::remember(&store, "fact", &huge, "explicit", None, &[], None, &[], None, false, None)
        .unwrap_err();
    assert!(err.to_string().contains("limit"), "{err}");
}

#[test]
fn penalized_confidence_stays_clean_and_roundtrips() {
    use limpet::index;
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    std::fs::write(root.join("s.py"), "def f():\n    return 1\n").unwrap();
    let store = Store::open_in_memory().unwrap();
    index::full_index(&store, root).unwrap();
    let a = memory::remember(
        &store, "fact", "f body", "explicit", Some(0.8),
        &[AnchorSpec { file: "s.py".into(), symbol: Some("f".into()) }], None, &[], None, false, None,
    )
    .unwrap();
    // Edit the body several times to drive repeated resolution/penalty.
    for body in ["return 2", "return 3", "return 4"] {
        std::thread::sleep(std::time::Duration::from_millis(15));
        std::fs::write(root.join("s.py"), format!("def f():\n    {body}\n")).unwrap();
        index::sweep(&store, root, &Default::default()).unwrap();
        memory::anchor::resolve_all(&store).unwrap();
    }
    let conf: f64 = store
        .conn
        .query_row("SELECT confidence FROM entries WHERE id = ?1", [&a.id], |r| r.get(0))
        .unwrap();
    // Clean to 6 decimals: no last-ULP tail, so serialize->parse is exact.
    assert_eq!(conf, (conf * 1e6).round() / 1e6, "stored confidence must be 6-decimal clean: {conf}");
    let s = serde_json::to_string(&conf).unwrap();
    let back: f64 = serde_json::from_str(&s).unwrap();
    assert_eq!(conf, back, "confidence must roundtrip through JSON exactly");
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
        false,
        None,
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
        false,
        None,
    )
    .unwrap();

    let mut exported = Vec::new();
    let n = store.export_jsonl(&mut exported).unwrap().exported;
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

#[test]
fn origin_dedup_rejects_second_write_naming_existing_id() {
    let dir = TempDir::new().unwrap();
    let store = seeded_store(dir.path());
    let first = memory::remember(
        &store, "decision", "auth uses JWT", "explicit", None, &[], None, &[], None,
        false, Some("scan:git:abc123"),
    )
    .unwrap();
    let err = memory::remember(
        &store, "decision", "different body, same source commit", "explicit", None, &[], None, &[], None,
        false, Some("scan:git:abc123"),
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("duplicate origin"), "{err}");
    assert!(err.contains(&first.id), "error must name the existing id: {err}");
    let count: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM entries", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1, "the rejected write must persist nothing");
}

#[test]
fn empty_origin_is_refused() {
    let dir = TempDir::new().unwrap();
    let store = seeded_store(dir.path());
    let err = memory::remember(
        &store, "fact", "x", "explicit", None, &[], None, &[], None, false, Some("  "),
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("origin"), "{err}");
}

#[test]
fn private_and_origin_are_stored() {
    let dir = TempDir::new().unwrap();
    let store = seeded_store(dir.path());
    let r = memory::remember(
        &store, "insight", "kept off the shared export", "explicit", None, &[], None, &[], None,
        true, Some("scan:mem:notes.md"),
    )
    .unwrap();
    let (private, origin): (i64, Option<String>) = store
        .conn
        .query_row(
            "SELECT private, origin FROM entries WHERE id = ?1",
            [&r.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(private, 1);
    assert_eq!(origin.as_deref(), Some("scan:mem:notes.md"));
}

#[test]
fn export_withholds_private_and_reports_count() {
    let dir = TempDir::new().unwrap();
    let store = seeded_store(dir.path());
    memory::remember(&store, "fact", "public knowledge", "explicit", None, &[], None, &[], None, false, None).unwrap();
    memory::remember(&store, "insight", "machine-local secret sauce", "explicit", None, &[], None, &[], None, true, None).unwrap();

    let mut out = Vec::new();
    let report = store.export_jsonl(&mut out).unwrap();
    assert_eq!(report.exported, 1);
    assert_eq!(report.private_withheld, 1);
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("public knowledge"));
    assert!(!text.contains("machine-local"), "private body leaked into export");
}

#[test]
fn origin_roundtrips_and_import_rejects_forged_origin_collision() {
    let dir = TempDir::new().unwrap();
    let store = seeded_store(dir.path());
    memory::remember(&store, "decision", "seeded from PR 12", "explicit", None, &[], None, &[], None, false, Some("scan:git:pr12")).unwrap();

    let mut out = Vec::new();
    store.export_jsonl(&mut out).unwrap();
    assert!(String::from_utf8_lossy(&out).contains("scan:git:pr12"));

    // Import into a fresh store: origin lands, so a scan re-run there dedups.
    let dir2 = TempDir::new().unwrap();
    let mut fresh = seeded_store(dir2.path());
    let rep = fresh.import_jsonl(&mut std::io::BufReader::new(out.as_slice())).unwrap();
    assert_eq!(rep.added, 1);
    let origin: Option<String> = fresh
        .conn
        .query_row("SELECT origin FROM entries LIMIT 1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(origin.as_deref(), Some("scan:git:pr12"));

    // A different id claiming the same origin is a forgery; rejected, not applied.
    let forged = r#"{"id":"01AAAAAAAAAAAAAAAAAAAAAAAA","kind":"fact","body":"impostor","created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-02T00:00:00Z","source":"explicit","confidence":0.8,"status":"active","stale_reason":null,"branch":null,"evidence_cmd":null,"evidence_digest":null,"evidence_ran_at":null,"origin":"scan:git:pr12","anchors":[],"links":[]}"#;
    let rep2 = fresh
        .import_jsonl(&mut std::io::BufReader::new(format!("{forged}\n").as_bytes()))
        .unwrap();
    assert_eq!(rep2.rejected, 1);
    assert_eq!(rep2.added, 0);
}

#[test]
fn origin_credential_shape_is_refused() {
    let dir = TempDir::new().unwrap();
    let store = seeded_store(dir.path());
    // AWS-key-shaped origin must fire the secret detector and be refused.
    // Split with concat! so external scanners do not flag this test file.
    let aws_key = concat!("AKIAIOSFOD", "NN7EXAMPLE");
    let err = memory::remember(
        &store, "fact", "some fact", "explicit", None, &[], None, &[], None, false, Some(aws_key),
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("origin"), "error must mention origin: {err}");

    // 300-byte origin must also be refused.
    let long_origin = "x".repeat(300);
    let err2 = memory::remember(
        &store, "fact", "some other fact", "explicit", None, &[], None, &[], None,
        false, Some(long_origin.as_str()),
    )
    .unwrap_err()
    .to_string();
    assert!(err2.contains("origin"), "error must mention origin: {err2}");
}

#[test]
fn import_rejects_credential_shaped_origin() {
    use std::io::BufReader;
    let dir = TempDir::new().unwrap();
    let mut store = seeded_store(dir.path());
    // A line whose origin looks like an AWS key must be counted rejected.
    // Split with concat! so external scanners do not flag this test file.
    let aws_key = concat!("AKIAIOSFOD", "NN7EXAMPLE");
    let line = format!(
        r#"{{"id":"01CREDORG000000000000000AA","kind":"fact","body":"some body","created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z","source":"explicit","confidence":0.8,"status":"active","stale_reason":null,"branch":null,"evidence_cmd":null,"evidence_digest":null,"evidence_ran_at":null,"origin":"{aws_key}","anchors":[],"links":[]}}"#
    );
    let report = store
        .import_jsonl(&mut BufReader::new(format!("{line}\n").as_bytes()))
        .unwrap();
    assert_eq!(report.rejected, 1, "credential-shaped origin must be rejected");
    assert_eq!(report.added, 0);
}

#[test]
fn lww_import_preserves_local_origin() {
    use std::io::BufReader;
    let dir = TempDir::new().unwrap();
    let mut store = seeded_store(dir.path());

    // Store an entry with an origin, then back-date it so the import line
    // can win with a past timestamp that is still newer than the stored one
    // without tripping the future-timestamp guard.
    let r = memory::remember(
        &store, "fact", "original body", "explicit", None, &[], None, &[], None,
        false, Some("scan:git:orig"),
    )
    .unwrap();
    store
        .conn
        .execute(
            "UPDATE entries SET created_at='2020-01-01T00:00:00Z', updated_at='2020-01-01T00:00:00Z' WHERE id = ?1",
            rusqlite::params![r.id],
        )
        .unwrap();

    // Craft a JSON line with the same id, NO "origin" field, and a newer
    // (but still past) updated_at.
    let no_origin_line = format!(
        r#"{{"id":"{}","kind":"fact","body":"updated body","created_at":"2020-01-01T00:00:00Z","updated_at":"2021-01-01T00:00:00Z","source":"explicit","confidence":0.8,"status":"active","stale_reason":null,"branch":null,"evidence_cmd":null,"evidence_digest":null,"evidence_ran_at":null,"anchors":[],"links":[]}}"#,
        r.id
    );
    let report = store
        .import_jsonl(&mut BufReader::new(format!("{no_origin_line}\n").as_bytes()))
        .unwrap();
    assert_eq!(report.updated, 1, "newer past timestamp must win the LWW merge");

    // Body must be updated; origin must survive (COALESCE, not overwrite).
    let (body, origin): (String, Option<String>) = store
        .conn
        .query_row(
            "SELECT body, origin FROM entries WHERE id = ?1",
            [&r.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(body, "updated body", "body must update on newer timestamp");
    assert_eq!(
        origin.as_deref(),
        Some("scan:git:orig"),
        "origin must survive LWW import that omits the origin field"
    );
}

#[test]
fn dispatch_remember_passes_private_and_origin() {
    let dir = TempDir::new().unwrap();
    let mut store = seeded_store(dir.path());
    let args = serde_json::json!({
        "kind": "insight",
        "body": "seeded via scan, stays local",
        "private": true,
        "origin": "scan:doc:README#setup"
    });
    let out = limpet::tools::dispatch(&mut store, dir.path(), "remember", &args).unwrap();
    let id = out["data"]["id"].as_str().unwrap().to_string();
    let (private, origin): (i64, Option<String>) = store
        .conn
        .query_row("SELECT private, origin FROM entries WHERE id = ?1", [&id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(private, 1);
    assert_eq!(origin.as_deref(), Some("scan:doc:README#setup"));

    let status = limpet::tools::dispatch(&mut store, dir.path(), "admin", &serde_json::json!({"op":"status"})).unwrap();
    assert_eq!(status["data"]["private"].as_i64(), Some(1));
}
