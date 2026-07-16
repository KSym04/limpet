//! Recall quality regression guard.
//!
//! `recall_eval.rs` proves recall runs; this proves recall ranks. It seeds a
//! small labeled corpus, fires realistic task queries, and asserts the right
//! memory lands at the top (precision@1) and near the top (recall@k). A
//! ranking change that quietly regresses now fails CI, and the printed score
//! is a number you can quote: "N% precision@1 on the eval set".
//!
//! It also guards the truth-layer contract (v0.14.0): an unverified claim,
//! however confidently typed, can never outrank a verified fact, and every
//! recalled memory carries its verification state so the model never mistakes
//! a claim for a proof.
//!
//! The corpus is deliberately small and hand-labeled. Grow it as you find
//! queries that should work and do not: each new case is a permanent guard.

use limpet::store::Store;
use limpet::tools;
use serde_json::{json, Value};
use std::path::Path;

/// (memory body, kind). Bodies are distinct conclusions an agent would store.
const CORPUS: &[(&str, &str)] = &[
    ("the scanner batch size is 50 because shared hosts kill long requests over 60 seconds", "decision"),
    ("checkout totals must be computed server side; the client price is display only and never trusted", "decision"),
    ("the nightly cron rebuilds the search index; it protects against drift after bulk imports", "fact"),
    ("we tried moving auth into middleware and reverted it: it broke the password reset flow", "episode"),
    ("public method names on the Scanner class are frozen because customer plugins hook them", "decision"),
];

/// (task query, substring that identifies the expected memory body).
const QUERIES: &[(&str, &str)] = &[
    ("why is the scanner batch size 50", "batch size is 50"),
    ("can I trust the price sent by the client at checkout", "computed server side"),
    ("what does the nightly cron job actually do", "nightly cron rebuilds"),
    ("has anyone tried moving auth into middleware", "moving auth into middleware"),
    ("is it safe to rename a public method on the Scanner class", "method names on the Scanner class are frozen"),
];

fn seed_corpus(store: &mut Store, root: &Path) {
    // Index once so sweep has a clean baseline; the memories are unanchored,
    // so this exercises pure text ranking without fixture-symbol coupling.
    tools::dispatch(store, root, "admin", &json!({ "op": "index" })).expect("index");
    for (body, kind) in CORPUS {
        tools::dispatch(
            store,
            root,
            "remember",
            &json!({ "kind": kind, "body": body, "source": "explicit" }),
        )
        .expect("remember");
    }
}

/// Full recalled items (not just bodies): callers inspect `source` and `flags`.
fn recall_items(store: &mut Store, root: &Path, task: &str) -> Vec<Value> {
    let resp = tools::dispatch(store, root, "recall", &json!({ "task": task, "budget_tokens": 2000 }))
        .expect("recall");
    resp.get("data")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn recall_bodies(store: &mut Store, root: &Path, task: &str) -> Vec<String> {
    recall_items(store, root, task)
        .iter()
        .filter_map(|it| it.get("body").and_then(Value::as_str).map(str::to_string))
        .collect()
}

#[test]
fn recall_ranks_the_right_memory_first() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("repo");
    std::fs::create_dir_all(&root).expect("mkdir repo");
    let root = root.as_path();
    let mut store = Store::open(&dir.path().join("store/key/eval.db")).expect("open store");

    seed_corpus(&mut store, root);

    let mut precision_at_1 = 0usize;
    let mut recall_at_3 = 0usize;
    for (task, needle) in QUERIES {
        let ranked = recall_bodies(&mut store, root, task);
        assert!(!ranked.is_empty(), "recall returned nothing for {task:?}");

        if ranked[0].contains(needle) {
            precision_at_1 += 1;
        }
        if ranked.iter().take(3).any(|b| b.contains(needle)) {
            recall_at_3 += 1;
        }
    }

    let total = QUERIES.len();
    eprintln!(
        "recall quality: precision@1 = {}/{}, recall@3 = {}/{}",
        precision_at_1, total, recall_at_3, total
    );

    // Thresholds are the contract. Tighten them as ranking improves; never
    // loosen them silently to make a regression pass.
    assert!(
        precision_at_1 >= 4,
        "precision@1 regressed: {precision_at_1}/{total} (expected >= 4)"
    );
    assert!(
        recall_at_3 == total,
        "recall@3 regressed: {recall_at_3}/{total} (expected {total})"
    );
}

#[test]
fn recall_never_hides_a_matching_memory() {
    // Every seeded memory must be retrievable by a query that names its topic;
    // the budget may reorder, but it must never drop a clear match silently.
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("repo");
    std::fs::create_dir_all(&root).expect("mkdir repo");
    let root = root.as_path();
    let mut store = Store::open(&dir.path().join("store/key/eval.db")).expect("open store");
    seed_corpus(&mut store, root);

    let ranked = recall_bodies(&mut store, root, "scanner batch size shared hosts");
    assert!(
        ranked.iter().any(|b| b.contains("batch size is 50")),
        "a clearly matching memory was not returned at all"
    );
}

// --- Truth-layer contract (v0.14.0) --------------------------------------

/// The col9/col10 failure, distilled: a caller types high confidence on an
/// unverified claim and it masquerades as truth. It must not outrank a proven
/// fact of the same content. This exercises P0.a (verification is a ranking
/// term) together with P0.c (typed confidence on unverified memory is capped).
#[test]
fn unverified_claim_cannot_outrank_verified_fact() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("repo");
    std::fs::create_dir_all(&root).expect("mkdir repo");
    let root = root.as_path();
    let mut store = Store::open(&dir.path().join("store/key/eval.db")).expect("open store");
    tools::dispatch(&mut store, root, "admin", &json!({ "op": "index" })).expect("index");

    let body = "the deploy column is col9 not col10 for the nightly export job";

    // An unverified claim, typed at near-maximum confidence (the swagger).
    tools::dispatch(
        &mut store,
        root,
        "remember",
        &json!({ "kind": "fact", "body": body, "source": "explicit", "confidence": 0.99 }),
    )
    .expect("remember explicit claim");

    // The same conclusion, proven, with evidence on file.
    tools::dispatch(
        &mut store,
        root,
        "remember",
        &json!({
            "kind": "fact",
            "body": body,
            "source": "verified",
            "evidence": { "command": "psql -c 'select col from export_target'", "output": "col9" }
        }),
    )
    .expect("remember verified fact");

    let ranked = recall_items(&mut store, root, "which deploy column col9 or col10 for the export");
    assert!(!ranked.is_empty(), "recall returned nothing");
    assert_eq!(
        ranked[0].get("source").and_then(Value::as_str),
        Some("verified"),
        "an unverified 0.99-confidence claim outranked the verified fact (col9/col10 regression)"
    );
}

/// Provenance must be unambiguous in recall output so the model never treats a
/// claim as a proof (P0.b). Rather than pay a per-item marker on the common
/// (explicit) case, which breaks the token bench, the wire convention is: a
/// `verified` fact self-identifies via `source == "verified"`; an unverified
/// explicit claim carries NO `source` field. Absence is the unverified signal,
/// documented in the recall tool description. This test pins that contract.
#[test]
fn provenance_is_distinguishable_in_recall() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("repo");
    std::fs::create_dir_all(&root).expect("mkdir repo");
    let root = root.as_path();
    let mut store = Store::open(&dir.path().join("store/key/eval.db")).expect("open store");
    tools::dispatch(&mut store, root, "admin", &json!({ "op": "index" })).expect("index");

    tools::dispatch(
        &mut store,
        root,
        "remember",
        &json!({ "kind": "fact", "body": "the retry ceiling is three attempts before a job is parked", "source": "explicit" }),
    )
    .expect("remember explicit");
    tools::dispatch(
        &mut store,
        root,
        "remember",
        &json!({
            "kind": "fact",
            "body": "the health check endpoint returns 204 when the queue is fully drained",
            "source": "verified",
            "evidence": { "command": "curl -so /dev/null -w '%{http_code}' /healthz", "output": "204" }
        }),
    )
    .expect("remember verified");

    let claim = recall_items(&mut store, root, "how many retries before a job is parked");
    let claim = claim.first().expect("recall returned nothing for the claim");
    assert_eq!(
        claim.get("source").and_then(Value::as_str),
        None,
        "an unverified explicit claim leaked a `source` marker (convention: absent == unverified): {claim}"
    );

    let proof = recall_items(&mut store, root, "what does the health check endpoint return when drained");
    let proof = proof.first().expect("recall returned nothing for the proof");
    assert_eq!(
        proof.get("source").and_then(Value::as_str),
        Some("verified"),
        "a verified fact did not self-identify via source: {proof}"
    );
}

/// Contradiction at write time (P1): a new memory whose value diverges from an
/// existing memory on the SAME anchor must surface as a conflict, so the writer
/// can supersede deliberately instead of silently stacking a contradiction. The
/// hard case is near-identical text with a flipped value ("batch size 200" vs
/// "batch size 50"): high token overlap, but a real conflict. Nothing is
/// auto-merged or auto-superseded; the conflict is surfaced and the writer acts.
#[test]
fn a_diverging_value_on_the_same_anchor_surfaces_as_a_conflict() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("repo");
    std::fs::create_dir_all(&root).expect("mkdir repo");
    let root = root.as_path();
    std::fs::create_dir_all(root.join("src")).expect("mkdir src");
    std::fs::write(
        root.join("src/scanner.php"),
        "<?php\nfunction scan_batch( $items ) {\n    return array_chunk( $items, 50 );\n}\n",
    )
    .expect("write fixture");

    let mut store = Store::open(&dir.path().join("store/key/eval.db")).expect("open store");
    tools::dispatch(&mut store, root, "admin", &json!({ "op": "index" })).expect("index");

    let first = tools::dispatch(
        &mut store,
        root,
        "remember",
        &json!({
            "kind": "decision",
            "body": "the scanner batch size is 50 because shared hosts kill long requests over 60 seconds",
            "anchors": [ { "file": "src/scanner.php" } ]
        }),
    )
    .expect("remember first");
    let first_id = first["data"]["id"].as_str().expect("first id").to_string();

    let second = tools::dispatch(
        &mut store,
        root,
        "remember",
        &json!({
            "kind": "decision",
            "body": "the scanner batch size is 200 because shared hosts kill long requests over 60 seconds",
            "anchors": [ { "file": "src/scanner.php" } ]
        }),
    )
    .expect("remember second");

    let conflicts = second["data"]["possible_conflicts"]
        .as_array()
        .expect("possible_conflicts field present on the remember result");
    assert!(
        conflicts.iter().any(|c| c.get("id").and_then(Value::as_str) == Some(first_id.as_str())),
        "the 200-vs-50 conflict on the same anchor did not surface the existing memory: {second}"
    );
}

/// Dedup enforcement at write (P3): re-stating a near-identical body on the
/// same anchor is refused, returning the existing id and the supersede path,
/// so a re-run or a forgetful writer cannot silently stack duplicates. The
/// write is a refusal, never a merge: nothing existing is touched.
#[test]
fn a_near_duplicate_write_on_the_same_anchor_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("repo");
    std::fs::create_dir_all(&root).expect("mkdir repo");
    let root = root.as_path();
    std::fs::create_dir_all(root.join("src")).expect("mkdir src");
    std::fs::write(
        root.join("src/scanner.php"),
        "<?php\nfunction scan_batch( $items ) {\n    return array_chunk( $items, 50 );\n}\n",
    )
    .expect("write fixture");

    let mut store = Store::open(&dir.path().join("store/key/eval.db")).expect("open store");
    tools::dispatch(&mut store, root, "admin", &json!({ "op": "index" })).expect("index");

    let body = "the scanner batch size is 50 because shared hosts kill long requests over 60 seconds";
    let first = tools::dispatch(
        &mut store,
        root,
        "remember",
        &json!({ "kind": "decision", "body": body, "anchors": [ { "file": "src/scanner.php" } ] }),
    )
    .expect("remember first");
    let first_id = first["data"]["id"].as_str().expect("first id").to_string();

    let err = tools::dispatch(
        &mut store,
        root,
        "remember",
        &json!({ "kind": "decision", "body": body, "anchors": [ { "file": "src/scanner.php" } ] }),
    )
    .expect_err("a near-identical same-anchor write must be refused");
    let msg = format!("{err:#}");
    assert!(
        msg.contains(&first_id),
        "the refusal must name the existing id so the writer can supersede it: {msg}"
    );
    assert!(
        msg.contains("supersede") && msg.contains("force"),
        "the refusal must offer both paths (supersede, or force to store anyway): {msg}"
    );
}

/// The `force: true` escape hatch stores the duplicate anyway (the writer has
/// judged it is genuinely distinct), and a plain restatement is never flagged
/// as a conflict: same value, no contradiction.
#[test]
fn force_stores_a_near_duplicate_and_a_restatement_is_not_a_conflict() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("repo");
    std::fs::create_dir_all(&root).expect("mkdir repo");
    let root = root.as_path();
    std::fs::create_dir_all(root.join("src")).expect("mkdir src");
    std::fs::write(
        root.join("src/scanner.php"),
        "<?php\nfunction scan_batch( $items ) {\n    return array_chunk( $items, 50 );\n}\n",
    )
    .expect("write fixture");

    let mut store = Store::open(&dir.path().join("store/key/eval.db")).expect("open store");
    tools::dispatch(&mut store, root, "admin", &json!({ "op": "index" })).expect("index");

    let body = "the scanner batch size is 50 because shared hosts kill long requests over 60 seconds";
    tools::dispatch(
        &mut store,
        root,
        "remember",
        &json!({ "kind": "decision", "body": body, "anchors": [ { "file": "src/scanner.php" } ] }),
    )
    .expect("remember first");
    let second = tools::dispatch(
        &mut store,
        root,
        "remember",
        &json!({ "kind": "decision", "body": body, "force": true, "anchors": [ { "file": "src/scanner.php" } ] }),
    )
    .expect("remember second with force");

    let conflicts = second["data"]["possible_conflicts"].as_array();
    assert!(
        conflicts.map(|c| c.is_empty()).unwrap_or(true),
        "an identical restatement was wrongly flagged as a conflict: {second}"
    );
}

/// A correction must never be blocked by dedup: near-identical prose with a
/// DIVERGENT value is a conflict (surfaced by the conflict test above), and it
/// stores without `force`. Blocking it would freeze the col9/col10 mistake in
/// place, which is the opposite of the truth layer's job.
#[test]
fn a_correction_with_a_new_value_is_never_refused_as_a_duplicate() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("repo");
    std::fs::create_dir_all(&root).expect("mkdir repo");
    let root = root.as_path();
    std::fs::create_dir_all(root.join("src")).expect("mkdir src");
    std::fs::write(
        root.join("src/scanner.php"),
        "<?php\nfunction scan_batch( $items ) {\n    return array_chunk( $items, 50 );\n}\n",
    )
    .expect("write fixture");

    let mut store = Store::open(&dir.path().join("store/key/eval.db")).expect("open store");
    tools::dispatch(&mut store, root, "admin", &json!({ "op": "index" })).expect("index");

    tools::dispatch(
        &mut store,
        root,
        "remember",
        &json!({
            "kind": "decision",
            "body": "the scanner batch size is 50 because shared hosts kill long requests over 60 seconds",
            "anchors": [ { "file": "src/scanner.php" } ]
        }),
    )
    .expect("remember original");
    // Same prose, flipped value: a correction, not a duplicate. No force.
    tools::dispatch(
        &mut store,
        root,
        "remember",
        &json!({
            "kind": "decision",
            "body": "the scanner batch size is 200 because shared hosts kill long requests over 60 seconds",
            "anchors": [ { "file": "src/scanner.php" } ]
        }),
    )
    .expect("a correction with a divergent value must store without force");
}

/// The verified boost is earned by LIVE evidence: once the anchored code
/// changes, the proof is rotten and the boost must vanish, so a FRESH
/// unverified memory of equal relevance outranks a STALE verified one. The
/// stale item still appears, still flagged (I3); only its rank privilege dies
/// with its evidence. Mirrors invariant I-A1: no boost ever ranks rot above
/// fresh knowledge.
#[test]
fn a_stale_verified_fact_does_not_outrank_a_fresh_explicit_memory() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("repo");
    std::fs::create_dir_all(&root).expect("mkdir repo");
    let root = root.as_path();
    std::fs::create_dir_all(root.join("src")).expect("mkdir src");
    std::fs::write(
        root.join("src/scanner.php"),
        "<?php\nfunction scan_batch( $items ) {\n    return array_chunk( $items, 50 );\n}\n",
    )
    .expect("write fixture");

    let mut store = Store::open(&dir.path().join("store/key/eval.db")).expect("open store");
    tools::dispatch(&mut store, root, "admin", &json!({ "op": "index" })).expect("index");

    let body = "the export job writes to the reports directory once per night";
    tools::dispatch(
        &mut store,
        root,
        "remember",
        &json!({
            "kind": "fact",
            "body": body,
            "source": "verified",
            "evidence": { "command": "ls reports/", "output": "nightly.csv" },
            "anchors": [ { "file": "src/scanner.php" } ]
        }),
    )
    .expect("remember verified anchored");
    tools::dispatch(
        &mut store,
        root,
        "remember",
        &json!({ "kind": "fact", "body": body, "source": "explicit" }),
    )
    .expect("remember fresh explicit");

    // Rot the verified memory's evidence: edit the anchored file.
    std::fs::write(
        root.join("src/scanner.php"),
        "<?php\nfunction scan_batch( $items ) {\n    return array_chunk( $items, 200 );\n}\n",
    )
    .expect("edit fixture");

    let ranked = recall_items(&mut store, root, "where does the export job write reports at night");
    assert!(ranked.len() >= 2, "recall must return both memories: {ranked:?}");
    let first_is_fresh_explicit = ranked[0].get("source").and_then(Value::as_str).is_none()
        && ranked[0].get("status").and_then(Value::as_str).is_none();
    assert!(
        first_is_fresh_explicit,
        "a stale verified fact outranked a fresh explicit memory of equal relevance: {:?}",
        ranked[0]
    );
}

/// A SUPERSEDED memory is dead to recall, so it must be dead to the write-path
/// checks too: re-asserting a body whose only near-identical twin is a
/// superseded entry must store freely (correcting the correction), and any
/// conflict hint must name the LIVE entry, never the dead one.
#[test]
fn a_superseded_memory_neither_refuses_nor_conflicts_a_new_write() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("repo");
    std::fs::create_dir_all(&root).expect("mkdir repo");
    let root = root.as_path();
    std::fs::create_dir_all(root.join("src")).expect("mkdir src");
    std::fs::write(
        root.join("src/scanner.php"),
        "<?php\nfunction scan_batch( $items ) {\n    return array_chunk( $items, 50 );\n}\n",
    )
    .expect("write fixture");

    let mut store = Store::open(&dir.path().join("store/key/eval.db")).expect("open store");
    tools::dispatch(&mut store, root, "admin", &json!({ "op": "index" })).expect("index");

    let body_50 = "the scanner batch size is 50 because shared hosts kill long requests over 60 seconds";
    let body_200 = "the scanner batch size is 200 because shared hosts kill long requests over 60 seconds";

    let a = tools::dispatch(
        &mut store,
        root,
        "remember",
        &json!({ "kind": "decision", "body": body_50, "anchors": [ { "file": "src/scanner.php" } ] }),
    )
    .expect("remember A");
    let a_id = a["data"]["id"].as_str().expect("A id").to_string();

    // B supersedes A: A is now dead to recall.
    let b = tools::dispatch(
        &mut store,
        root,
        "remember",
        &json!({
            "kind": "decision",
            "body": body_200,
            "anchors": [ { "file": "src/scanner.php" } ],
            "links": [ { "target": a_id, "rel": "supersedes" } ]
        }),
    )
    .expect("remember B superseding A");
    let b_id = b["data"]["id"].as_str().expect("B id").to_string();

    // Correct the correction: re-assert 50. Its only near-identical twin is
    // the DEAD entry A; the live divergence is with B. Must store, no force.
    let third = tools::dispatch(
        &mut store,
        root,
        "remember",
        &json!({ "kind": "decision", "body": body_50, "anchors": [ { "file": "src/scanner.php" } ] }),
    )
    .expect("re-asserting a superseded body must store without force");

    // And the conflict surfaced must be the LIVE entry B, never dead A.
    let conflicts = third["data"]["possible_conflicts"].as_array().cloned().unwrap_or_default();
    assert!(
        conflicts.iter().any(|c| c.get("id").and_then(Value::as_str) == Some(b_id.as_str())),
        "the live conflicting entry must be surfaced: {third}"
    );
    assert!(
        !conflicts.iter().any(|c| c.get("id").and_then(Value::as_str) == Some(a_id.as_str())),
        "a superseded (dead) entry must never be surfaced as a conflict: {third}"
    );
}

/// The classifier's own motivating case: col9 vs col10. The divergent value is
/// EMBEDDED in a token, not a standalone number, so digit extraction must see
/// inside tokens or the flagship failure ships unflagged.
#[test]
fn a_digit_embedded_divergence_surfaces_as_a_conflict() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("repo");
    std::fs::create_dir_all(&root).expect("mkdir repo");
    let root = root.as_path();
    std::fs::create_dir_all(root.join("src")).expect("mkdir src");
    std::fs::write(root.join("src/export.php"), "<?php\nfunction export_run() {\n    return 'col';\n}\n")
        .expect("write fixture");

    let mut store = Store::open(&dir.path().join("store/key/eval.db")).expect("open store");
    tools::dispatch(&mut store, root, "admin", &json!({ "op": "index" })).expect("index");

    let first = tools::dispatch(
        &mut store,
        root,
        "remember",
        &json!({
            "kind": "fact",
            "body": "the nightly export job writes the customer id into col10 of the target sheet",
            "anchors": [ { "file": "src/export.php" } ]
        }),
    )
    .expect("remember col10");
    let first_id = first["data"]["id"].as_str().expect("id").to_string();

    let second = tools::dispatch(
        &mut store,
        root,
        "remember",
        &json!({
            "kind": "fact",
            "body": "the nightly export job writes the customer id into col9 of the target sheet",
            "anchors": [ { "file": "src/export.php" } ]
        }),
    )
    .expect("the col9 correction must store without force");
    let conflicts = second["data"]["possible_conflicts"].as_array().cloned().unwrap_or_default();
    assert!(
        conflicts.iter().any(|c| c.get("id").and_then(Value::as_str) == Some(first_id.as_str())),
        "col9 vs col10 is the motivating conflict and must be surfaced: {second}"
    );
}

/// Non-ASCII bodies must not be judged by their ASCII scraps: two genuinely
/// different CJK notes that happen to share a couple of English tokens are
/// NOT near-duplicates.
#[test]
fn different_cjk_bodies_sharing_english_tokens_are_not_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("repo");
    std::fs::create_dir_all(&root).expect("mkdir repo");
    let root = root.as_path();
    std::fs::create_dir_all(root.join("src")).expect("mkdir src");
    std::fs::write(root.join("src/config.php"), "<?php\nfunction config_load() {\n    return 1;\n}\n")
        .expect("write fixture");

    let mut store = Store::open(&dir.path().join("store/key/eval.db")).expect("open store");
    tools::dispatch(&mut store, root, "admin", &json!({ "op": "index" })).expect("index");

    tools::dispatch(
        &mut store,
        root,
        "remember",
        &json!({
            "kind": "fact",
            "body": "設定ファイルはキャッシュに保存される config cache",
            "anchors": [ { "file": "src/config.php" } ]
        }),
    )
    .expect("remember first CJK note");
    tools::dispatch(
        &mut store,
        root,
        "remember",
        &json!({
            "kind": "fact",
            "body": "認証トークンは毎回サーバーで再検証する config cache",
            "anchors": [ { "file": "src/config.php" } ]
        }),
    )
    .expect("a different CJK note must not be refused as a near-duplicate of the first");
}

/// Word order can invert meaning with identical vocabulary ("prefer sqlite
/// over postgres" vs "prefer postgres over sqlite"). A reversed claim is a
/// correction, never a duplicate; refusing it would freeze the wrong order in.
#[test]
fn a_reversed_claim_with_identical_vocabulary_is_not_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("repo");
    std::fs::create_dir_all(&root).expect("mkdir repo");
    let root = root.as_path();
    std::fs::create_dir_all(root.join("src")).expect("mkdir src");
    std::fs::write(root.join("src/storage.php"), "<?php\nfunction storage_pick() {\n    return 's';\n}\n")
        .expect("write fixture");

    let mut store = Store::open(&dir.path().join("store/key/eval.db")).expect("open store");
    tools::dispatch(&mut store, root, "admin", &json!({ "op": "index" })).expect("index");

    tools::dispatch(
        &mut store,
        root,
        "remember",
        &json!({
            "kind": "decision",
            "body": "prefer sqlite over postgres for the local storage layer",
            "anchors": [ { "file": "src/storage.php" } ]
        }),
    )
    .expect("remember original ordering");
    tools::dispatch(
        &mut store,
        root,
        "remember",
        &json!({
            "kind": "decision",
            "body": "prefer postgres over sqlite for the local storage layer",
            "anchors": [ { "file": "src/storage.php" } ]
        }),
    )
    .expect("the reversed claim must store without force");
}

/// The negation dimension of the classifiers, pinned: "there is NO timeout"
/// against "there is a timeout" has IDENTICAL significant tokens (no/a/is/on
/// all fall under the 3-char floor), so only the has_negation equality check
/// keeps the correction storable. Delete that clause and this test refuses
/// the write; it also pins that the flipped claim surfaces as a conflict.
#[test]
fn a_negated_correction_is_stored_and_surfaced_as_a_conflict() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("repo");
    std::fs::create_dir_all(&root).expect("mkdir repo");
    let root = root.as_path();
    let mut store = Store::open(&dir.path().join("store/key/eval.db")).expect("open store");
    std::fs::create_dir_all(root.join("src")).expect("mkdir src");
    std::fs::write(root.join("src/export.php"), "<?php\nfunction export_run() {\n    return 'x';\n}\n")
        .expect("write fixture");
    tools::dispatch(&mut store, root, "admin", &json!({ "op": "index" })).expect("index");

    let first = tools::dispatch(
        &mut store,
        root,
        "remember",
        &json!({
            "kind": "fact",
            "body": "there is a timeout on the nightly export job after thirty seconds",
            "anchors": [ { "file": "src/export.php" } ]
        }),
    )
    .expect("remember original");
    let first_id = first["data"]["id"].as_str().expect("id").to_string();

    let second = tools::dispatch(
        &mut store,
        root,
        "remember",
        &json!({
            "kind": "fact",
            "body": "there is no timeout on the nightly export job after thirty seconds",
            "anchors": [ { "file": "src/export.php" } ]
        }),
    )
    .expect("a negated correction must store without force");
    let conflicts = second["data"]["possible_conflicts"].as_array().cloned().unwrap_or_default();
    assert!(
        conflicts.iter().any(|c| c.get("id").and_then(Value::as_str) == Some(first_id.as_str())),
        "a negation flip is a value divergence and must surface as a conflict: {second}"
    );
}

/// The explicit confidence cap, pinned DIRECTLY: whatever a caller types, the
/// served confidence of an unverified memory never exceeds 0.85. The ranking
/// tests alone cannot isolate this (the verified boost also decides them);
/// this assertion fails if only the cap regresses.
#[test]
fn typed_confidence_on_an_unverified_memory_is_capped_on_the_wire() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("repo");
    std::fs::create_dir_all(&root).expect("mkdir repo");
    let root = root.as_path();
    let mut store = Store::open(&dir.path().join("store/key/eval.db")).expect("open store");
    tools::dispatch(&mut store, root, "admin", &json!({ "op": "index" })).expect("index");

    tools::dispatch(
        &mut store,
        root,
        "remember",
        &json!({ "kind": "fact", "body": "the widget cache refreshes every five minutes on the dot", "source": "explicit", "confidence": 0.99 }),
    )
    .expect("remember");

    let ranked = recall_items(&mut store, root, "how often does the widget cache refresh");
    let item = ranked.first().expect("recall returned the memory");
    let conf = item["conf"].as_f64().expect("conf on the wire");
    assert!(
        conf <= 0.85,
        "typed swagger leaked past the explicit confidence cap: served conf {conf}"
    );
}

/// The provenance convention is only safe if it is documented where the model
/// reads it: the recall tool description. Pin that the description explains
/// what a missing `source` means, so the contract cannot silently rot.
#[test]
fn recall_tool_documents_the_provenance_convention() {
    let schema = tools::tool_schemas();
    let recall = schema
        .as_array()
        .and_then(|tools| tools.iter().find(|t| t.get("name").and_then(Value::as_str) == Some("recall")))
        .expect("recall tool present in schema");
    let desc = recall
        .get("description")
        .and_then(Value::as_str)
        .expect("recall tool has a description");
    assert!(
        desc.contains("verified") && desc.contains("unverified"),
        "recall description must document the verified/unverified provenance convention: {desc}"
    );
}
