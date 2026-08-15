//! The 0.16 refinement loop: confidence refund on heal (schema v8) and the
//! reverify/consolidate admin ops. Decay is once per reason; a reason that
//! evaporates (branch switch, revert) refunds the penalty it charged.

use limpet::index::{self};
use limpet::memory::{self, anchor, AnchorSpec};
use limpet::store::Store;
use std::fs;
use tempfile::TempDir;

const BODY: &str = "pub fn score(n: u32) -> u32 {\n    let mut t = 3;\n    for i in 1..n {\n        t = t.wrapping_add(i * 7);\n    }\n    t\n}\n";
const EDITED: &str = "pub fn score(n: u32) -> u32 {\n    n * 9\n}\n";

fn seed(root: &std::path::Path) -> (Store, String) {
    fs::write(root.join("a.rs"), BODY).unwrap();
    let store = Store::open_in_memory().unwrap();
    index::full_index(&store, root).unwrap();
    let r = memory::remember(
        &store,
        "fact",
        "score folds every index below n by sevens",
        "explicit",
        None,
        &[AnchorSpec { file: "a.rs".into(), symbol: Some("score".into()) }],
        None,
        &[],
        None,
        false,
        None,
        false,
    )
    .unwrap();
    (store, r.id)
}

fn resolve(store: &Store, root: &std::path::Path) -> anchor::ResolveReport {
    std::thread::sleep(std::time::Duration::from_millis(20));
    index::sweep(store, root, &Default::default()).unwrap();
    anchor::resolve_all(store).unwrap()
}

fn conf_state(store: &Store, id: &str) -> (String, f64, Option<f64>) {
    store
        .conn
        .query_row(
            "SELECT status, confidence, conf_before_stale FROM entries WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
}

#[test]
fn a_branch_switch_round_trip_is_confidence_neutral() {
    // The gate from ROADMAP: edit the anchored body (stale, penalized), then
    // revert it (heal). The transient disappearance must cost nothing.
    let dir = TempDir::new().unwrap();
    let (store, id) = seed(dir.path());
    let (_, c0, before) = conf_state(&store, &id);
    assert_eq!(before, None, "a fresh entry stores no pre-stale value");

    fs::write(dir.path().join("a.rs"), EDITED).unwrap();
    resolve(&store, dir.path());
    let (status, penalized, stored) = conf_state(&store, &id);
    assert_eq!(status, "stale");
    assert!(penalized < c0, "the stale transition must still penalize");
    assert_eq!(stored, Some(c0), "the pre-penalty value must be stored");

    fs::write(dir.path().join("a.rs"), BODY).unwrap();
    resolve(&store, dir.path());
    let (status, healed, stored) = conf_state(&store, &id);
    assert_eq!(status, "active");
    assert_eq!(healed, c0, "healing must refund the exact pre-stale confidence");
    assert_eq!(stored, None, "the refund clears its stored value");
}

#[test]
fn re_staling_while_stale_keeps_the_original_refund() {
    // Penalty applies once; the stored refund must survive repeated resolves
    // and a second edit while already stale, or the refund would restore a
    // penalized value.
    let dir = TempDir::new().unwrap();
    let (store, id) = seed(dir.path());
    let (_, c0, _) = conf_state(&store, &id);

    fs::write(dir.path().join("a.rs"), EDITED).unwrap();
    resolve(&store, dir.path());
    let (_, c1, stored1) = conf_state(&store, &id);
    assert_eq!(stored1, Some(c0));

    // Another edit while stale, plus extra resolve passes.
    fs::write(dir.path().join("a.rs"), "pub fn score(n: u32) -> u32 {\n    n + 1\n}\n").unwrap();
    resolve(&store, dir.path());
    resolve(&store, dir.path());
    let (status, c2, stored2) = conf_state(&store, &id);
    assert_eq!(status, "stale");
    assert_eq!(c2, c1, "the penalty never compounds");
    assert_eq!(stored2, Some(c0), "the original pre-stale value survives re-staling");

    fs::write(dir.path().join("a.rs"), BODY).unwrap();
    resolve(&store, dir.path());
    let (_, healed, _) = conf_state(&store, &id);
    assert_eq!(healed, c0);
}

#[test]
fn verified_stale_refunds_to_its_verified_confidence() {
    // The verified path penalizes via MIN(conf, 0.5); the refund must restore
    // the verified confidence, not the floor.
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("a.rs"), BODY).unwrap();
    let store = Store::open_in_memory().unwrap();
    index::full_index(&store, dir.path()).unwrap();
    let r = memory::remember(
        &store,
        "fact",
        "score's fold is proven by the harness",
        "explicit",
        None,
        &[AnchorSpec { file: "a.rs".into(), symbol: Some("score".into()) }],
        Some(&memory::Evidence {
            command: "cargo test score_fold".into(),
            output: "test result: ok. 1 passed".into(),
        }),
        &[],
        None,
        false,
        None,
        false,
    )
    .unwrap();
    let (_, c0, _) = conf_state(&store, &r.id);
    assert!(c0 > 0.5, "premise: verified confidence sits above the stale floor");

    fs::write(dir.path().join("a.rs"), EDITED).unwrap();
    resolve(&store, dir.path());
    let (_, penalized, stored) = conf_state(&store, &r.id);
    assert!(penalized <= 0.5);
    assert_eq!(stored, Some(c0));

    fs::write(dir.path().join("a.rs"), BODY).unwrap();
    resolve(&store, dir.path());
    let (status, healed, stored) = conf_state(&store, &r.id);
    assert_eq!(status, "active");
    assert_eq!(healed, c0);
    assert_eq!(stored, None);
}

#[test]
fn conf_before_stale_travels_the_jsonl_wire() {
    // A stale entry exported mid-penalty must carry its refund to the peer,
    // or a heal on the other machine restores nothing.
    let dir = TempDir::new().unwrap();
    let (store, id) = seed(dir.path());
    let (_, c0, _) = conf_state(&store, &id);
    fs::write(dir.path().join("a.rs"), EDITED).unwrap();
    resolve(&store, dir.path());

    let mut out = Vec::new();
    store.export_jsonl(&mut out).unwrap();
    let text = String::from_utf8(out.clone()).unwrap();
    assert!(
        text.contains("\"conf_before_stale\""),
        "export must carry the stored refund: {text}"
    );

    // The peer has not pulled the edit: its tree still holds the ORIGINAL
    // body. Import re-bases the anchor onto that local body (the anti-forgery
    // adopt), the entry arrives stale with its stored refund, and the first
    // resolve heals it: the refund must fire on the peer.
    let dir2 = TempDir::new().unwrap();
    fs::write(dir2.path().join("a.rs"), BODY).unwrap();
    let mut fresh = Store::open_in_memory().unwrap();
    index::full_index(&fresh, dir2.path()).unwrap();
    fresh.import_jsonl(&mut std::io::BufReader::new(out.as_slice())).unwrap();
    resolve(&fresh, dir2.path());
    let (status, healed, stored) = conf_state(&fresh, &id);
    assert_eq!(status, "active");
    assert_eq!(healed, c0, "the refund must survive the wire and fire on the peer");
    assert_eq!(stored, None);
}

fn seed_verified(root: &std::path::Path) -> (Store, String, f64) {
    fs::write(root.join("a.rs"), BODY).unwrap();
    let store = Store::open_in_memory().unwrap();
    index::full_index(&store, root).unwrap();
    let r = memory::remember(
        &store,
        "fact",
        "score's fold is proven by the harness",
        "explicit",
        None,
        &[AnchorSpec { file: "a.rs".into(), symbol: Some("score".into()) }],
        Some(&memory::Evidence {
            command: "cargo test score_fold".into(),
            output: "test result: ok. 1 passed".into(),
        }),
        &[],
        None,
        false,
        None,
        false,
    )
    .unwrap();
    let (_, c0, _) = conf_state(&store, &r.id);
    (store, r.id, c0)
}

#[test]
fn reverify_returns_a_stale_verified_fact_to_active() {
    // The full loop the release is named for: verified fact goes stale on an
    // edit, verify_queue offers it, fresh evidence returns it to trusted
    // with the penalty refunded and the anchor re-bound to the CURRENT body.
    let dir = TempDir::new().unwrap();
    let (store, id, c0) = seed_verified(dir.path());

    fs::write(dir.path().join("a.rs"), EDITED).unwrap();
    resolve(&store, dir.path());
    let (status, penalized, _) = conf_state(&store, &id);
    assert_eq!(status, "stale");
    assert!(penalized <= 0.5, "verified stale floors at 0.5");
    let queued: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM entries WHERE source='verified' AND status='stale'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(queued, 1, "premise: the fact sits in the verify queue");
    let (old_digest, old_updated): (String, String) = store
        .conn
        .query_row(
            "SELECT evidence_digest, updated_at FROM entries WHERE id = ?1",
            [&id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();

    let res = memory::reverify(
        &store,
        &id,
        "cargo test score_fold",
        "test result: ok. 1 passed; 0 failed (edited fold verified)",
    )
    .unwrap();
    assert_eq!(res.anchors_rebound, 1);
    assert_eq!(res.confidence, c0, "the stale penalty refunds on reverify");

    let (status, conf, stored) = conf_state(&store, &id);
    assert_eq!(status, "active");
    assert_eq!(conf, c0);
    assert_eq!(stored, None);
    let (digest, updated, ran_at): (String, String, String) = store
        .conn
        .query_row(
            "SELECT evidence_digest, updated_at, evidence_ran_at FROM entries WHERE id = ?1",
            [&id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_ne!(digest, old_digest, "fresh output mints a fresh digest");
    assert!(updated > old_updated, "reverify must win LWW on peers");
    assert!(!ran_at.is_empty());
    // The anchor now carries the EDITED body's hash: reverified means proven
    // true of the code as it is now.
    let (anchor_hash, current_hash): (String, String) = store
        .conn
        .query_row(
            "SELECT a.ast_body_hash, s.body_hash FROM anchors a
             JOIN symbols s ON s.fqn = a.symbol_fqn AND s.file = a.file
             WHERE a.entry_id = ?1",
            [&id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(anchor_hash, current_hash);
    // And the next resolve keeps it active: the loop actually closed.
    resolve(&store, dir.path());
    assert_eq!(conf_state(&store, &id).0, "active");
}

#[test]
fn reverify_refuses_when_an_anchored_symbol_is_gone() {
    // Partial anchor loss: the entry is stale (not invalidated) because a
    // second anchor survives, but the vanished symbol must still refuse the
    // whole reverify; a fact cannot be half-attached to current code.
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("a.rs"), BODY).unwrap();
    fs::write(dir.path().join("b.rs"), "pub const LIMIT: u32 = 9;\n").unwrap();
    let store = Store::open_in_memory().unwrap();
    index::full_index(&store, dir.path()).unwrap();
    let r = memory::remember(
        &store,
        "fact",
        "score respects LIMIT when folding",
        "explicit",
        None,
        &[
            AnchorSpec { file: "a.rs".into(), symbol: Some("score".into()) },
            AnchorSpec { file: "b.rs".into(), symbol: None },
        ],
        Some(&memory::Evidence {
            command: "cargo test score_limit".into(),
            output: "test result: ok. 1 passed".into(),
        }),
        &[],
        None,
        false,
        None,
        false,
    )
    .unwrap();
    fs::write(dir.path().join("a.rs"), "// nothing left\n").unwrap();
    resolve(&store, dir.path());
    let (status, _, _) = conf_state(&store, &r.id);
    assert_eq!(status, "stale", "premise: partial loss stales, never invalidates");
    let err = memory::reverify(&store, &r.id, "cargo test score_limit", "ok")
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("no longer resolves"),
        "a vanished anchor must refuse, never guess: {err}"
    );
    assert_ne!(conf_state(&store, &r.id).0, "active", "the refusal changed nothing");
}

#[test]
fn reverify_refuses_a_fully_invalidated_entry() {
    let dir = TempDir::new().unwrap();
    let (store, id, _) = seed_verified(dir.path());
    fs::write(dir.path().join("a.rs"), "// nothing left\n").unwrap();
    resolve(&store, dir.path());
    let err = memory::reverify(&store, &id, "cargo test score_fold", "ok")
        .unwrap_err()
        .to_string();
    assert!(err.contains("invalidated"), "{err}");
}

#[test]
fn reverify_refuses_secrets_junk_and_wrong_states() {
    let dir = TempDir::new().unwrap();
    let (store, id, _) = seed_verified(dir.path());

    let err = memory::reverify(&store, &id, "aws s3 ls --key AKIAIOSFODNN7EXAMPLE", "ok")
        .unwrap_err()
        .to_string();
    assert!(err.contains("scrub"), "secret-bearing command refused: {err}");
    let err = memory::reverify(&store, &id, "   ", "ok").unwrap_err().to_string();
    assert!(err.contains("command is empty"), "{err}");
    let err = memory::reverify(&store, &id, "cargo test", "  ").unwrap_err().to_string();
    assert!(err.contains("output is empty"), "{err}");
    let err = memory::reverify(&store, "01NOPE0000000000000000000X", "cargo test", "ok")
        .unwrap_err()
        .to_string();
    assert!(err.contains("no memory"), "{err}");

    // Archived: refuse until restored.
    store
        .conn
        .execute(
            "INSERT INTO archived(entry_id, archived_at) VALUES (?1, '2026-08-15T00:00:00Z')",
            [&id],
        )
        .unwrap();
    let err = memory::reverify(&store, &id, "cargo test", "ok").unwrap_err().to_string();
    assert!(err.contains("archived"), "{err}");
    store.conn.execute("DELETE FROM archived WHERE entry_id = ?1", [&id]).unwrap();

    // Superseded: refuse, point at the successor.
    store
        .conn
        .execute("UPDATE entries SET status='superseded' WHERE id = ?1", [&id])
        .unwrap();
    let err = memory::reverify(&store, &id, "cargo test", "ok").unwrap_err().to_string();
    assert!(err.contains("superseded"), "{err}");
}

#[test]
fn imported_refund_is_clamped_and_quantized() {
    // A hostile 1e300 refund must not park a future confidence above range.
    let dir = TempDir::new().unwrap();
    let (mut store, _) = seed(dir.path());
    let line = r#"{"id":"01REFUND000000000000000000","kind":"fact","body":"hostile refund line","created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z","source":"explicit","confidence":0.4,"status":"stale","stale_reason":"body_edited","conf_before_stale":1e300,"anchors":[],"links":[]}"#;
    store.import_jsonl(&mut std::io::BufReader::new(line.as_bytes())).unwrap();
    let stored: Option<f64> = store
        .conn
        .query_row(
            "SELECT conf_before_stale FROM entries WHERE id = '01REFUND000000000000000000'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stored, Some(1.0), "refund clamps to [0,1] on import");
}
