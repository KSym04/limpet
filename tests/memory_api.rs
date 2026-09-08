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
        None, false,
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
        None, false,
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
        None, false,
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
        None, false,
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
        None, false,
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
        None, false,
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
        &store, "fact", "claims to be proven", "verified", None, &[], None, &[], None, false, None, false,
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
        None, false,
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
    assert!(memory::remember(&store, "vibe", "x", "explicit", None, &[], None, &[], None, false, None, false).is_err());
    assert!(memory::remember(&store, "fact", "", "explicit", None, &[], None, &[], None, false, None, false).is_err());
    assert!(memory::remember(&store, "fact", "x", "psychic", None, &[], None, &[], None, false, None, false).is_err());
}

#[test]
fn mined_confidence_is_capped() {
    let dir = TempDir::new().unwrap();
    let store = seeded_store(dir.path());
    let r = memory::remember(&store, "episode", "tried X, failed", "mined", Some(0.9), &[], None, &[], None, false, None, false)
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
    let old = memory::remember(&store, "fact", "timeout is 30 seconds", "explicit", None, &[], None, &[], None, false, None, false)
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
        None, false,
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
    let r = memory::remember(&store, "fact", "x", "explicit", None, &[], None, &[], None, false, None, false).unwrap();
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

/// The evidence COMMAND persists raw and exports verbatim into a git-committed
/// JSONL, so it gets the same secret guard as the body. The output is only
/// ever stored as a digest; the command is the one evidence field that can
/// leak. An empty command is refused too: it would be unverifiable by
/// construction while still earning the verified ranking boost.
#[test]
fn evidence_command_is_guarded_like_the_body() {
    let dir = TempDir::new().unwrap();
    let store = seeded_store(dir.path());

    let secret_cmd = memory::Evidence {
        command: "curl -H 'Authorization: Bearer ghp_1234567890abcdefghijklmnopqrstuvwxyz' https://api.example.com".into(),
        output: "200".into(),
    };
    let err = memory::remember(
        &store, "fact", "the api answers 200 when healthy", "explicit", None, &[],
        Some(&secret_cmd), &[], None, false, None, false,
    )
    .expect_err("a credential in the evidence command must be refused");
    assert!(format!("{err:#}").contains("evidence command"), "unexpected: {err:#}");

    let empty_cmd = memory::Evidence { command: "   ".into(), output: "200".into() };
    let err = memory::remember(
        &store, "fact", "the api answers 200 when healthy", "explicit", None, &[],
        Some(&empty_cmd), &[], None, false, None, false,
    )
    .expect_err("an empty evidence command is unverifiable and must be refused");
    assert!(format!("{err:#}").contains("evidence command"), "unexpected: {err:#}");
}

/// Import is the second write path; the truth layer must hold there too. A
/// hostile line claiming `verified` with no evidence would otherwise mint the
/// ranking boost with nothing to re-verify; a typed confidence above the
/// explicit cap would rank as if earned; an unknown source string would abort
/// the WHOLE import at the schema CHECK instead of rejecting one line.
#[test]
fn import_enforces_the_truth_layer_on_the_second_write_path() {
    use std::io::BufReader;
    let dir = TempDir::new().unwrap();
    let mut store = seeded_store(dir.path());

    let lines = concat!(
        // Forged provenance: verified without evidence. Must be rejected.
        r#"{"id":"01FORGED0000000000000000AA","kind":"fact","body":"trust me it is proven","created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z","source":"verified","confidence":1.0,"status":"active","anchors":[],"links":[]}"#, "\n",
        // Typed swagger on an unverified claim: stored, but capped at the
        // explicit ceiling so it can never rank at verified levels.
        r#"{"id":"01SWAGGER000000000000000BB","kind":"fact","body":"swaggering unverified claim","created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z","source":"explicit","confidence":1.0,"status":"active","anchors":[],"links":[]}"#, "\n",
        // Unknown source: reject the LINE, not the whole import.
        r#"{"id":"01BADSRC0000000000000000CC","kind":"fact","body":"claims a source that does not exist","created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z","source":"psychic","confidence":0.5,"status":"active","anchors":[],"links":[]}"#, "\n",
        // Legit verified WITH evidence: stored, confidence at most 0.95.
        r#"{"id":"01LEGITV0000000000000000DD","kind":"fact","body":"honestly proven elsewhere","created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z","source":"verified","confidence":1.0,"status":"active","evidence_cmd":"echo ok","evidence_digest":"abc","evidence_ran_at":"2026-01-01T00:00:00Z","anchors":[],"links":[]}"#, "\n",
        // Mined stays at its documented 0.5 ceiling.
        r#"{"id":"01MINED00000000000000000EE","kind":"fact","body":"mined from history somewhere","created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z","source":"mined","confidence":0.9,"status":"active","anchors":[],"links":[]}"#, "\n",
    );
    let report = store
        .import_jsonl(&mut BufReader::new(lines.as_bytes()))
        .unwrap();
    assert_eq!(report.rejected, 2, "forged-verified and unknown-source lines rejected: {report:?}");
    assert_eq!(report.added, 3, "swagger (capped), legit verified, and mined applied: {report:?}");

    let forged: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM entries WHERE id = '01FORGED0000000000000000AA'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(forged, 0, "verified-without-evidence must never enter the store");

    let swagger: f64 = store
        .conn
        .query_row("SELECT confidence FROM entries WHERE id = '01SWAGGER000000000000000BB'", [], |r| r.get(0))
        .unwrap();
    assert!(swagger <= 0.85, "imported explicit confidence must respect the cap, got {swagger}");

    let legit: f64 = store
        .conn
        .query_row("SELECT confidence FROM entries WHERE id = '01LEGITV0000000000000000DD'", [], |r| r.get(0))
        .unwrap();
    assert!(legit <= 0.95, "imported verified confidence caps at the earned ceiling, got {legit}");

    let mined: f64 = store
        .conn
        .query_row("SELECT confidence FROM entries WHERE id = '01MINED00000000000000000EE'", [], |r| r.get(0))
        .unwrap();
    assert!(mined <= 0.5, "imported mined confidence caps at 0.5, got {mined}");
}

/// One malformed line must never abort the whole import: an unknown kind,
/// unknown status, empty body, or unknown link rel would otherwise hit a
/// schema CHECK constraint and roll back EVERY line, including during the
/// silent first-run bootstrap import. Each is a per-line rejection.
#[test]
fn import_rejects_malformed_lines_without_aborting_the_batch() {
    use std::io::BufReader;
    let dir = TempDir::new().unwrap();
    let mut store = seeded_store(dir.path());

    let lines = concat!(
        r#"{"id":"01BADKIND000000000000000AA","kind":"note","body":"claims a kind that does not exist","created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z","source":"explicit","confidence":0.5,"status":"active","anchors":[],"links":[]}"#, "\n",
        r#"{"id":"01BADSTAT000000000000000BB","kind":"fact","body":"claims the archived pseudo status","created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z","source":"explicit","confidence":0.5,"status":"archived","anchors":[],"links":[]}"#, "\n",
        r#"{"id":"01EMPTYB0000000000000000CC","kind":"fact","body":"","created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z","source":"explicit","confidence":0.5,"status":"active","anchors":[],"links":[]}"#, "\n",
        r#"{"id":"01GOODLN0000000000000000DD","kind":"fact","body":"a perfectly ordinary imported fact","created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z","source":"explicit","confidence":0.5,"status":"active","anchors":[],"links":[{"dst":"01GOODLN0000000000000000DD","rel":"replaces"}]}"#, "\n",
    );
    let report = store
        .import_jsonl(&mut BufReader::new(lines.as_bytes()))
        .expect("a malformed line must not abort the import");
    assert_eq!(report.rejected, 3, "bad kind, bad status, empty body: {report:?}");
    assert_eq!(report.added, 1, "the good line still applies: {report:?}");
    assert_eq!(report.links_dropped, 1, "the unknown link rel is dropped, not fatal: {report:?}");

    let good: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM entries WHERE id = '01GOODLN0000000000000000DD'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(good, 1, "the good line must survive its bad neighbors");
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
    let err = memory::remember(&store, "fact", &huge, "explicit", None, &[], None, &[], None, false, None, false)
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
        &[AnchorSpec { file: "s.py".into(), symbol: Some("f".into()) }], None, &[], None, false, None, false,
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
        None, false,
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
        None, false,
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
        false, Some("scan:git:abc123"), false,
    )
    .unwrap();
    let err = memory::remember(
        &store, "decision", "different body, same source commit", "explicit", None, &[], None, &[], None,
        false, Some("scan:git:abc123"), false,
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
        &store, "fact", "x", "explicit", None, &[], None, &[], None, false, Some("  "), false,
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
        true, Some("scan:mem:notes.md"), false,
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
    memory::remember(&store, "fact", "public knowledge", "explicit", None, &[], None, &[], None, false, None, false).unwrap();
    memory::remember(&store, "insight", "machine-local secret sauce", "explicit", None, &[], None, &[], None, true, None, false).unwrap();

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
    memory::remember(&store, "decision", "seeded from PR 12", "explicit", None, &[], None, &[], None, false, Some("scan:git:pr12"), false).unwrap();

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
        &store, "fact", "some fact", "explicit", None, &[], None, &[], None, false, Some(aws_key), false,
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("origin"), "error must mention origin: {err}");

    // 300-byte origin must also be refused.
    let long_origin = "x".repeat(300);
    let err2 = memory::remember(
        &store, "fact", "some other fact", "explicit", None, &[], None, &[], None,
        false, Some(long_origin.as_str()), false,
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
        false, Some("scan:git:orig"), false,
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

/// Twin trait impls plus the inherent method next to them: one FQN
/// (`t.T.go`), three slots (`@A`, `@B`, and one with NO discriminator), and
/// deliberately DIFFERENT bodies so which slot a spec picked is visible in
/// the stored anchor hash rather than having to be taken on trust. This is
/// the shape the extractor itself pins (inherent impl plus trait impls), so
/// the undiscriminated slot has to stay reachable.
fn twin_impl_store(root: &std::path::Path) -> Store {
    fs::write(
        root.join("t.rs"),
        "struct T;\n\
         impl A for T { fn go(&self) -> u32 { 1 } }\n\
         impl B for T { fn go(&self) -> u32 { 222 } }\n\
         impl T { fn go(&self) -> u32 { 33333 } }\n",
    )
    .unwrap();
    let store = Store::open_in_memory().unwrap();
    index::full_index(&store, root).unwrap();
    store
}

/// The body hash of one twin slot, read straight from the index.
fn slot_hash(store: &Store, disamb: &str) -> String {
    store
        .conn
        .query_row(
            "SELECT body_hash FROM symbols WHERE name = 'go' AND disamb = ?1",
            [disamb],
            |r| r.get(0),
        )
        .unwrap()
}

#[test]
fn disamb_suffix_anchors_to_the_named_slot() {
    let dir = TempDir::new().unwrap();
    let store = twin_impl_store(dir.path());
    let r = memory::remember(
        &store,
        "insight",
        "the A impl returns the identity value",
        "explicit",
        None,
        &[AnchorSpec { file: "t.rs".into(), symbol: Some("go@A".into()) }],
        None,
        &[],
        None,
        false,
        None, false,
    )
    .unwrap();
    assert_eq!(r.anchored, 1);

    let (fqn, disamb, hash): (String, Option<String>, String) = store
        .conn
        .query_row(
            "SELECT symbol_fqn, disamb, ast_body_hash FROM anchors WHERE entry_id = ?1",
            [&r.id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(fqn, "t.T.go");
    assert_eq!(disamb.as_deref(), Some("A"), "the anchor records the slot it resolved");
    assert_eq!(hash, slot_hash(&store, "A"), "the A slot's own body hash");
    assert_ne!(hash, slot_hash(&store, "B"), "never the twin's hash");
}

#[test]
fn bare_name_over_twin_slots_lists_the_slot_forms() {
    let dir = TempDir::new().unwrap();
    let store = twin_impl_store(dir.path());
    let err = memory::remember(
        &store,
        "insight",
        "go does a thing",
        "explicit",
        None,
        &[AnchorSpec { file: "t.rs".into(), symbol: Some("go".into()) }],
        None,
        &[],
        None,
        false,
        None, false,
    )
    .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("ambiguous"), "{msg}");
    assert!(
        msg.contains("t.T.go@A") && msg.contains("t.T.go@B"),
        "the caller must be told exactly what to type: {msg}"
    );
    assert!(
        msg.contains("t.T.go@impl"),
        "the inherent slot needs a typeable spelling of its own, not the bare \
         FQN that just bounced: {msg}"
    );
    let entries: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM entries", [], |r| r.get(0))
        .unwrap();
    assert_eq!(entries, 0, "an ambiguous slot never guesses and never stores");
}

#[test]
fn a_truncated_slot_list_says_so() {
    let dir = TempDir::new().unwrap();
    let mut src = String::from("struct T;\n");
    for i in 0..6 {
        src.push_str(&format!("impl Tr{i} for T {{ fn go(&self) -> u32 {{ {i} }} }}\n"));
    }
    fs::write(dir.path().join("many.rs"), src).unwrap();
    let store = Store::open_in_memory().unwrap();
    index::full_index(&store, dir.path()).unwrap();
    let err = memory::remember(
        &store,
        "insight",
        "go does a thing",
        "explicit",
        None,
        &[AnchorSpec { file: "many.rs".into(), symbol: Some("go".into()) }],
        None,
        &[],
        None,
        false,
        None, false,
    )
    .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("and more"),
        "a capped menu must admit the choices it dropped: {msg}"
    );
}

#[test]
fn trailing_at_anchors_the_undiscriminated_slot() {
    // A type and a callable can share an FQN in most languages (Java keeps
    // class and method names in separate namespaces, so a factory method may
    // be spelled exactly like the nested class it builds). The type row is the
    // symbol class that genuinely carries NO discriminator, so no
    // `@<disamb>` spelling and no bare FQN can name it: the empty suffix is
    // the only spec that reaches it.
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("Config.java"),
        "class Config {\n\
        \x20 class Builder {\n\
        \x20   void run() { go(); }\n\
        \x20 }\n\
        \x20 Builder Builder() {\n\
        \x20   return make();\n\
        \x20 }\n\
         }\n",
    )
    .unwrap();
    let store = Store::open_in_memory().unwrap();
    index::full_index(&store, dir.path()).unwrap();
    let slots: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM symbols WHERE fqn = 'Config.Config.Builder'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(slots, 2, "premise: the nested class and the factory share one FQN");

    let r = memory::remember(
        &store,
        "fact",
        "the Builder type collects options before validating",
        "explicit",
        None,
        &[AnchorSpec { file: "Config.java".into(), symbol: Some("Builder@".into()) }],
        None,
        &[],
        None,
        false,
        None, false,
    )
    .unwrap();
    assert_eq!(r.anchored, 1);

    let (fqn, disamb, hash): (String, Option<String>, String) = store
        .conn
        .query_row(
            "SELECT symbol_fqn, disamb, ast_body_hash FROM anchors WHERE entry_id = ?1",
            [&r.id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(fqn, "Config.Config.Builder");
    assert_eq!(disamb, None, "the empty suffix means 'no discriminator', not 'a disamb spelled \"\"'");
    let class_hash: String = store
        .conn
        .query_row(
            "SELECT body_hash FROM symbols WHERE fqn = 'Config.Config.Builder' AND kind = 'class'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(hash, class_hash, "the type's own body hash, not the factory's");
}

#[test]
fn a_discriminator_carrying_an_at_still_anchors() {
    let dir = TempDir::new().unwrap();
    // A Java discriminator is the parameter list verbatim, annotations
    // included, so the spec carries two `@` signs and a single last-`@`
    // split shreds it.
    fs::write(
        dir.path().join("C.java"),
        "class C {\n  void f(int a) { log(a); }\n  \
         void f(@NonNull String a) { log(a.length()); }\n}\n",
    )
    .unwrap();
    let store = Store::open_in_memory().unwrap();
    index::full_index(&store, dir.path()).unwrap();
    let (disamb, hash): (String, String) = store
        .conn
        .query_row(
            "SELECT disamb, body_hash FROM symbols WHERE name = 'f' AND disamb LIKE '%@%'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("fixture must produce an overload whose discriminator holds an `@`");

    let r = memory::remember(
        &store,
        "fact",
        "the annotated overload is the one callers reach",
        "explicit",
        None,
        &[AnchorSpec { file: "C.java".into(), symbol: Some(format!("f@{disamb}")) }],
        None,
        &[],
        None,
        false,
        None, false,
    )
    .unwrap();
    assert_eq!(r.anchored, 1);
    let (stored_disamb, stored_hash): (Option<String>, String) = store
        .conn
        .query_row(
            "SELECT disamb, ast_body_hash FROM anchors WHERE entry_id = ?1",
            [&r.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(stored_disamb.as_deref(), Some(disamb.as_str()));
    assert_eq!(stored_hash, hash, "the annotated overload's own body, not its twin's");
}

#[test]
fn unknown_disamb_fails_not_found_with_the_slot_list() {
    let dir = TempDir::new().unwrap();
    let store = twin_impl_store(dir.path());
    let err = memory::remember(
        &store,
        "insight",
        "go does a thing",
        "explicit",
        None,
        &[AnchorSpec { file: "t.rs".into(), symbol: Some("go@Nope".into()) }],
        None,
        &[],
        None,
        false,
        None, false,
    )
    .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("not found"), "{msg}");
    assert!(
        msg.contains("t.T.go@A") && msg.contains("t.T.go@B"),
        "the near-miss list must show the slots that DO exist: {msg}"
    );
}

#[test]
fn symbol_spelled_with_an_at_sign_still_anchors() {
    let dir = TempDir::new().unwrap();
    // bash allows `@` inside a function name, so the last-`@` split would
    // shred this spec. The verbatim retry keeps it anchorable.
    fs::write(
        dir.path().join("deploy.sh"),
        "function deploy@prod {\n  echo shipping to prod\n}\n",
    )
    .unwrap();
    let store = Store::open_in_memory().unwrap();
    index::full_index(&store, dir.path()).unwrap();
    let indexed: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM symbols WHERE name = 'deploy@prod'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(indexed, 1, "fixture must really produce an @-spelled symbol");

    let r = memory::remember(
        &store,
        "episode",
        "prod deploys need the VPN up first",
        "explicit",
        None,
        &[AnchorSpec { file: "deploy.sh".into(), symbol: Some("deploy@prod".into()) }],
        None,
        &[],
        None,
        false,
        None, false,
    )
    .unwrap();
    let (fqn, disamb): (String, Option<String>) = store
        .conn
        .query_row(
            "SELECT symbol_fqn, disamb FROM anchors WHERE entry_id = ?1",
            [&r.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(fqn, "deploy.deploy@prod");
    assert_eq!(disamb, None, "the `@` was part of the name, not a slot request");
}

#[test]
fn single_slot_bare_name_anchors_and_records_its_disamb() {
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("one.rs"),
        "struct T;\nimpl A for T { fn go(&self) -> u32 { 1 } }\n",
    )
    .unwrap();
    let store = Store::open_in_memory().unwrap();
    index::full_index(&store, dir.path()).unwrap();
    let r = memory::remember(
        &store,
        "fact",
        "go is the only impl of this trait method",
        "explicit",
        None,
        &[AnchorSpec { file: "one.rs".into(), symbol: Some("go".into()) }],
        None,
        &[],
        None,
        false,
        None, false,
    )
    .unwrap();
    assert_eq!(r.anchored, 1, "one slot resolves exactly as before");
    let disamb: Option<String> = store
        .conn
        .query_row("SELECT disamb FROM anchors WHERE entry_id = ?1", [&r.id], |row| row.get(0))
        .unwrap();
    assert_eq!(
        disamb.as_deref(),
        Some("A"),
        "an undisambiguated spec still records the slot it landed on"
    );
}

#[test]
fn export_import_round_trip_preserves_anchor_disamb() {
    let dir = TempDir::new().unwrap();
    let store = twin_impl_store(dir.path());
    let r = memory::remember(
        &store,
        "decision",
        "the B impl carries the wide value on purpose",
        "explicit",
        None,
        &[AnchorSpec { file: "t.rs".into(), symbol: Some("go@B".into()) }],
        None,
        &[],
        None,
        false,
        None, false,
    )
    .unwrap();

    let mut out = Vec::new();
    store.export_jsonl(&mut out).unwrap();
    let text = String::from_utf8(out.clone()).unwrap();
    assert!(text.contains(r#""disamb":"B""#), "export must carry the slot: {text}");

    let dir2 = TempDir::new().unwrap();
    let mut fresh = twin_impl_store(dir2.path());
    let rep = fresh
        .import_jsonl(&mut std::io::BufReader::new(out.as_slice()))
        .unwrap();
    assert_eq!(rep.added, 1);
    let (disamb, hash): (Option<String>, Option<String>) = fresh
        .conn
        .query_row(
            "SELECT disamb, ast_body_hash FROM anchors WHERE entry_id = ?1",
            [&r.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(disamb.as_deref(), Some("B"), "the slot survives the round trip");
    assert_eq!(
        hash.as_deref(),
        Some(slot_hash(&fresh, "B").as_str()),
        "the local re-resolve must read the B slot, not an arbitrary twin"
    );
}

#[test]
fn at_spec_accepts_the_source_spelling_of_a_parameter_list() {
    // Stored discriminators are whitespace-canonicalized; the user types the
    // list the way the source writes it, comma-space included, and the spec
    // parser canonicalizes before matching instead of demanding the stored
    // spelling.
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("M.java"),
        "class M {\n\
        \x20 void m(int a) { one(); }\n\
        \x20 void m(int a, String b) { two(); }\n\
         }\n",
    )
    .unwrap();
    let store = Store::open_in_memory().unwrap();
    index::full_index(&store, dir.path()).unwrap();
    let r = memory::remember(
        &store,
        "fact",
        "the two-arg overload validates before writing",
        "explicit",
        None,
        &[AnchorSpec { file: "M.java".into(), symbol: Some("m@(int a, String b)".into()) }],
        None,
        &[],
        None,
        false,
        None, false,
    )
    .unwrap();
    assert_eq!(r.anchored, 1);
    let disamb: Option<String> = store
        .conn
        .query_row("SELECT disamb FROM anchors WHERE entry_id = ?1", [&r.id], |row| row.get(0))
        .unwrap();
    assert_eq!(disamb.as_deref(), Some("(int a,String b)"), "canonical slot recorded");
}

#[test]
fn at_named_symbol_colliding_with_a_slot_spec_is_refused_loudly() {
    // bash allows `@` in a function name. When `deploy@` names a real symbol
    // AND parses as a trailing-@ slot spec that also matches (`deploy` with
    // no discriminator), silently picking either is a guess.
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("run.sh"),
        "function deploy@ {\n  push_prod\n}\nfunction deploy {\n  push_stage\n}\n",
    )
    .unwrap();
    let store = Store::open_in_memory().unwrap();
    index::full_index(&store, dir.path()).unwrap();
    let both: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM symbols WHERE fqn IN ('run.deploy', 'run.deploy@')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(both, 2, "premise: both spellings exist as symbols");
    let err = memory::remember(
        &store,
        "fact",
        "deploy pushes to production",
        "explicit",
        None,
        &[AnchorSpec { file: "run.sh".into(), symbol: Some("deploy@".into()) }],
        None,
        &[],
        None,
        false,
        None, false,
    )
    .unwrap_err();
    assert!(err.to_string().contains("ambiguous"), "must refuse loudly: {err}");
}

#[test]
fn deliberate_null_slot_survives_the_round_trip() {
    // The trailing-@ spec pins the row where disamb IS NULL (the nested
    // class), and export omits the field for it. On import that NULL must
    // stay a precise slot request, never a wildcard: the widened
    // "(?2 IS NULL OR disamb = ?2)" re-resolve adopted the factory METHOD's
    // hash (lowest ordinal) as the class anchor's fresh baseline.
    let src = "class Config {\n\
        \x20 class Builder {\n\
        \x20   void run() { go(); }\n\
        \x20 }\n\
        \x20 Builder Builder() {\n\
        \x20   return make();\n\
        \x20 }\n\
         }\n";
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("Config.java"), src).unwrap();
    let store = Store::open_in_memory().unwrap();
    index::full_index(&store, dir.path()).unwrap();
    let r = memory::remember(
        &store,
        "fact",
        "the Builder type collects options before validating",
        "explicit",
        None,
        &[AnchorSpec { file: "Config.java".into(), symbol: Some("Builder@".into()) }],
        None,
        &[],
        None,
        false,
        None, false,
    )
    .unwrap();

    let mut out = Vec::new();
    store.export_jsonl(&mut out).unwrap();

    let dir2 = TempDir::new().unwrap();
    fs::write(dir2.path().join("Config.java"), src).unwrap();
    let mut fresh = Store::open_in_memory().unwrap();
    index::full_index(&fresh, dir2.path()).unwrap();
    fresh.import_jsonl(&mut std::io::BufReader::new(out.as_slice())).unwrap();

    let class_hash: String = fresh
        .conn
        .query_row(
            "SELECT body_hash FROM symbols
             WHERE fqn = 'Config.Config.Builder' AND disamb IS NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let (disamb, hash): (Option<String>, Option<String>) = fresh
        .conn
        .query_row(
            "SELECT disamb, ast_body_hash FROM anchors WHERE entry_id = ?1",
            [&r.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(disamb, None, "the undiscriminated slot stays undiscriminated");
    assert_eq!(
        hash.as_deref(),
        Some(class_hash.as_str()),
        "the NULL slot must re-resolve to the class row, never the factory twin"
    );
}

#[test]
fn lww_reimport_through_an_old_peer_keeps_the_local_slot() {
    // The 0.14-peer wash cycle: this machine anchors a slot, a peer running a
    // binary that predates disamb imports the export, makes an entry-level
    // change (archive, supersede), and re-exports WITHOUT the field. When the
    // newer line comes back, replacing the local anchor with disamb NULL
    // would strip twin protection permanently (with a twin present the
    // backfill correctly refuses to re-adopt). The local slot must survive.
    let dir = TempDir::new().unwrap();
    let mut store = twin_impl_store(dir.path());
    let r = memory::remember(
        &store,
        "decision",
        "the B impl carries the wide value on purpose",
        "explicit",
        None,
        &[AnchorSpec { file: "t.rs".into(), symbol: Some("go@B".into()) }],
        None,
        &[],
        None,
        false,
        None, false,
    )
    .unwrap();

    let mut out = Vec::new();
    store.export_jsonl(&mut out).unwrap();
    let line = String::from_utf8(out).unwrap();
    // Skip the wire-format header: the peer line under edit is the ENTRY.
    let entry_line = line
        .lines()
        .find(|l| l.contains("\"id\""))
        .expect("export carries the entry");
    let mut obj: serde_json::Value = serde_json::from_str(entry_line).unwrap();
    // What an old peer emits back: no disamb on the anchor, a strictly newer
    // entry stamp from its own edit (within the import skew allowance).
    obj["anchors"][0].as_object_mut().unwrap().remove("disamb");
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    obj["updated_at"] = serde_json::Value::String(index::iso_from_secs(now_secs + 30));
    let peer_line = serde_json::to_string(&obj).unwrap();

    let rep = store
        .import_jsonl(&mut std::io::BufReader::new(peer_line.as_bytes()))
        .unwrap();
    assert_eq!(rep.updated, 1, "the newer line must win LWW: {rep:?}");
    let (disamb, hash): (Option<String>, Option<String>) = store
        .conn
        .query_row(
            "SELECT disamb, ast_body_hash FROM anchors WHERE entry_id = ?1",
            [&r.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(disamb.as_deref(), Some("B"), "the local slot must survive the wash cycle");
    assert_eq!(
        hash.as_deref(),
        Some(slot_hash(&store, "B").as_str()),
        "and the hash must stay the B slot's body"
    );
}

/// Two files, one FQN: the extension is stripped when an FQN is minted, so
/// `util.js` and `util.ts` both mint `util.parse`. The import re-resolve must
/// read the hash of the file the anchor actually names.
fn fqn_collision_store(root: &std::path::Path) -> Store {
    fs::write(
        root.join("util.js"),
        "function parse(s) {\n  return JSON.parse(s);\n}\n",
    )
    .unwrap();
    fs::write(
        root.join("util.ts"),
        "function parse(s: string) {\n  return s.split(\",\").map(Number);\n}\n",
    )
    .unwrap();
    let store = Store::open_in_memory().unwrap();
    index::full_index(&store, root).unwrap();
    store
}

#[test]
fn import_reresolves_the_hash_from_the_anchors_own_file() {
    let dir = TempDir::new().unwrap();
    let store = fqn_collision_store(dir.path());
    let r = memory::remember(
        &store,
        "fact",
        "parse splits on commas in the typed build",
        "explicit",
        None,
        &[AnchorSpec { file: "util.ts".into(), symbol: Some("parse".into()) }],
        None,
        &[],
        None,
        false,
        None, false,
    )
    .unwrap();
    assert_eq!(r.anchored, 1);

    let mut out = Vec::new();
    store.export_jsonl(&mut out).unwrap();

    let dir2 = TempDir::new().unwrap();
    let mut fresh = fqn_collision_store(dir2.path());
    let rep = fresh
        .import_jsonl(&mut std::io::BufReader::new(out.as_slice()))
        .unwrap();
    assert_eq!(rep.added, 1);
    let ts_hash: String = fresh
        .conn
        .query_row("SELECT body_hash FROM symbols WHERE file = 'util.ts'", [], |r| r.get(0))
        .unwrap();
    let hash: Option<String> = fresh
        .conn
        .query_row("SELECT ast_body_hash FROM anchors WHERE entry_id = ?1", [&r.id], |r| r.get(0))
        .unwrap();
    assert_eq!(
        hash.as_deref(),
        Some(ts_hash.as_str()),
        "the anchor names util.ts, so util.js's body can never supply its hash"
    );
}

#[test]
fn export_omits_disamb_for_undisambiguated_anchors() {
    let dir = TempDir::new().unwrap();
    // A Rust free function is a symbol class that genuinely has NO
    // discriminator: the language forbids overloading, so there is nothing for
    // one to tell apart. (A Python `def` is not such a class: its parameter
    // list is what splits `@property` from `@x.setter`.)
    fs::write(
        dir.path().join("cache.rs"),
        "pub fn cache_get(key: &str) -> Option<u32> {\n    store_lookup(key)\n}\n",
    )
    .unwrap();
    let store = Store::open_in_memory().unwrap();
    index::full_index(&store, dir.path()).unwrap();
    memory::remember(
        &store,
        "fact",
        "cache_get returns None on miss",
        "explicit",
        None,
        &[AnchorSpec { file: "cache.rs".into(), symbol: Some("cache_get".into()) }],
        None,
        &[],
        None,
        false,
        None, false,
    )
    .unwrap();
    let mut out = Vec::new();
    store.export_jsonl(&mut out).unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(
        !text.contains("disamb"),
        "an anchor with no discriminator must not pay for the field: {text}"
    );
}

#[test]
fn import_rejects_bogus_anchor_disamb_without_aborting_the_batch() {
    use std::io::BufReader;
    let dir = TempDir::new().unwrap();
    let mut store = seeded_store(dir.path());

    let lines = concat!(
        r#"{"id":"01BADDISAMB00000000000AAA","kind":"fact","body":"claims an object as its slot","created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z","source":"explicit","confidence":0.5,"status":"active","anchors":[{"file":"cache.py","symbol_fqn":"cache.cache_get","ast_body_hash":"deadbeefdeadbeefdeadbeefdeadbeef","context_hint":null,"disamb":{"nested":"object"}}],"links":[]}"#, "\n",
        r#"{"id":"01GOODDISAMB0000000000BBB","kind":"fact","body":"an ordinary fact next door","created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z","source":"explicit","confidence":0.5,"status":"active","anchors":[],"links":[]}"#, "\n",
    );
    let report = store
        .import_jsonl(&mut BufReader::new(lines.as_bytes()))
        .expect("a bogus disamb must not abort the batch");
    assert_eq!(report.rejected, 1, "the malformed slot is refused: {report:?}");
    assert_eq!(report.added, 1, "its neighbor still applies: {report:?}");
    let bad: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM entries WHERE id = '01BADDISAMB00000000000AAA'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(bad, 0, "a rejected line stores nothing");
}

// ---------------------------------------------------------------- P5 matched

/// P5: an item names which task terms hit its body, capped at three, in task
/// order, and the field is absent entirely when nothing hits (omit-when-empty
/// is what keeps the wire under the bench gate).
#[test]
fn recall_items_carry_capped_matched_terms_or_nothing() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let mut store = seeded_store(root);

    memory::remember(
        &store,
        "insight",
        "sweep prioritization reindexes anchored files first inside the budget",
        "explicit",
        None,
        &[AnchorSpec { file: "cache.py".into(), symbol: Some("cache_get".into()) }],
        None,
        &[],
        Some("main"),
        false,
        None,
        false,
    )
    .unwrap();

    let resp = limpet::tools::dispatch(
        &mut store,
        root,
        "recall",
        &serde_json::json!({
            "task": "how does sweep prioritization order anchored files in the reindex budget",
            "budget_tokens": 2000
        }),
    )
    .unwrap();
    let items = resp["data"].as_array().unwrap();
    let hit = items
        .iter()
        .find(|i| i["body"].as_str().unwrap().starts_with("sweep prioritization"))
        .expect("stored memory recalled");
    assert_eq!(
        hit["matched"].as_str().unwrap(),
        "sweep prioritization anchored",
        "task-order intersection, capped at three"
    );

    // No term overlap: reached via the working set instead, and the wire
    // carries no matched key at all.
    let resp = limpet::tools::dispatch(
        &mut store,
        root,
        "recall",
        &serde_json::json!({
            "task": "zebra quantum flamingo",
            "working_set": ["cache.py"],
            "budget_tokens": 2000
        }),
    )
    .unwrap();
    let items = resp["data"].as_array().unwrap();
    assert!(!items.is_empty(), "working-set proximity still surfaces the item");
    for i in items {
        assert!(
            i.get("matched").is_none(),
            "no task term hits, so no matched field: {i}"
        );
    }
}
