//! Archival (P4, v0.14.0): a recoverable visibility state, deliberately NOT a
//! status value. The `archived` sidecar table hides an entry from recall, the
//! verify queue, and `map`, while the staleness engine keeps tracking reality
//! underneath. Restore deletes the flag row and the entry reappears with its
//! CURRENT, truthful status. Nothing is deleted; export still carries the
//! entry (flagged), so archival can never silently lose knowledge.

use limpet::store::Store;
use limpet::tools;
use serde_json::{json, Value};
use std::path::Path;

const FIXTURE_V1: &str = "<?php\nfunction scan_batch( $items ) {\n    return array_chunk( $items, 50 );\n}\n";
const FIXTURE_V2: &str = "<?php\nfunction scan_batch( $items ) {\n    return array_chunk( $items, 200 );\n}\n";

/// Fixture repo in `repo/`, store in a SIBLING subtree: a store inside the
/// repo root makes `store_exclude_dir` exclude the whole repo, so discovery
/// silently finds nothing (the trap `limpet demo` documents and avoids).
fn setup(base: &Path) -> (Store, std::path::PathBuf) {
    let root = base.join("repo");
    std::fs::create_dir_all(root.join("src")).expect("mkdir src");
    std::fs::write(root.join("src/scanner.php"), FIXTURE_V1).expect("write fixture");
    let mut store = Store::open(&base.join("store/key/eval.db")).expect("open store");
    let indexed = tools::dispatch(&mut store, &root, "admin", &json!({ "op": "index" })).expect("index");
    // Guard the layout itself: discovery must actually see the fixture.
    assert!(
        indexed["data"]["index"]["files"].as_u64().unwrap_or(0) >= 1,
        "the index discovered nothing; the store leaked inside the repo root: {indexed}"
    );
    (store, root)
}

fn remember(store: &mut Store, root: &Path, body: &str) -> String {
    let r = tools::dispatch(
        store,
        root,
        "remember",
        &json!({ "kind": "decision", "body": body, "anchors": [ { "file": "src/scanner.php" } ] }),
    )
    .expect("remember");
    r["data"]["id"].as_str().expect("id").to_string()
}

fn recall_ids(store: &mut Store, root: &Path, task: &str) -> Vec<String> {
    let resp = tools::dispatch(store, root, "recall", &json!({ "task": task, "budget_tokens": 2000 }))
        .expect("recall");
    resp["data"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|it| it.get("id").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn archive_hides_from_recall_and_restore_returns_the_current_truth() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut store, root) = setup(dir.path());
    let root = root.as_path();

    let id = remember(&mut store, root, "the scanner batch size is 50 because shared hosts kill long requests");
    assert!(
        recall_ids(&mut store, root, "why is the scanner batch size 50").contains(&id),
        "sanity: the memory recalls before archival"
    );

    let archived = tools::dispatch(&mut store, root, "admin", &json!({ "op": "archive", "id": id }))
        .expect("archive");
    assert_eq!(archived["data"]["archived"].as_str(), Some(id.as_str()));
    assert!(
        !recall_ids(&mut store, root, "why is the scanner batch size 50").contains(&id),
        "an archived memory must vanish from default recall"
    );

    // Reality moves on while the memory is archived: the anchored code changes.
    // Archival gates visibility only, so the staleness engine keeps tracking.
    std::fs::write(root.join("src/scanner.php"), FIXTURE_V2).expect("edit fixture");
    let _ = recall_ids(&mut store, root, "anything, to run the sweep");

    let restored = tools::dispatch(&mut store, root, "admin", &json!({ "op": "restore", "id": id }))
        .expect("restore");
    assert_eq!(restored["data"]["restored"].as_str(), Some(id.as_str()));

    let resp = tools::dispatch(
        &mut store,
        root,
        "recall",
        &json!({ "task": "why is the scanner batch size 50", "budget_tokens": 2000 }),
    )
    .expect("recall after restore");
    let item = resp["data"]
        .as_array()
        .and_then(|items| items.iter().find(|it| it.get("id").and_then(Value::as_str) == Some(id.as_str())))
        .cloned()
        .expect("a restored memory must recall again");
    let flags: Vec<String> = item["flags"]
        .as_array()
        .map(|a| a.iter().filter_map(|f| f.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    assert!(
        flags.iter().any(|f| f.starts_with("stale")),
        "restore must return the CURRENT truth: the code changed while archived, so the memory is stale: {item}"
    );
}

#[test]
fn archived_entries_survive_export_and_import_round_trips() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut store, root) = setup(dir.path());
    let root = root.as_path();

    let keep = remember(&mut store, root, "the retry ceiling is three attempts before a job is parked");
    let arch = remember(&mut store, root, "the nightly cron rebuilds the search index after bulk imports");
    tools::dispatch(&mut store, root, "admin", &json!({ "op": "archive", "id": arch }))
        .expect("archive");

    tools::dispatch(&mut store, root, "admin", &json!({ "op": "export" })).expect("export");
    let exported = std::fs::read_to_string(root.join(".limpet/memory.jsonl")).expect("read export");
    assert!(
        exported.contains(&arch),
        "an archived entry must still be exported: archival hides, never loses"
    );
    let arch_line = exported.lines().find(|l| l.contains(&arch)).expect("archived line");
    assert!(
        arch_line.contains("\"archived\":true"),
        "the export must carry the archived flag: {arch_line}"
    );
    let keep_line = exported.lines().find(|l| l.contains(&keep)).expect("kept line");
    assert!(
        !keep_line.contains("archived"),
        "a visible entry must not pay for an archived field: {keep_line}"
    );

    // Fresh machine: import the export; the archived entry stays archived.
    let dir2 = tempfile::tempdir().expect("tempdir2");
    let root2_buf = dir2.path().join("repo");
    let root2 = root2_buf.as_path();
    std::fs::create_dir_all(root2.join("src")).expect("mkdir src2");
    std::fs::write(root2.join("src/scanner.php"), FIXTURE_V1).expect("fixture2");
    std::fs::create_dir_all(root2.join(".limpet")).expect("mkdir .limpet");
    std::fs::write(root2.join(".limpet/memory.jsonl"), &exported).expect("copy export");
    let mut store2 = Store::open(&dir2.path().join("store/key/eval.db")).expect("open store2");
    tools::dispatch(&mut store2, root2, "admin", &json!({ "op": "index" })).expect("index2");
    tools::dispatch(&mut store2, root2, "admin", &json!({ "op": "import" })).expect("import");

    let ids = recall_ids(&mut store2, root2, "retry ceiling cron rebuild index");
    assert!(ids.contains(&keep), "the visible entry recalls on the new machine");
    assert!(
        !ids.contains(&arch),
        "the archived entry must stay archived across an export/import round trip"
    );
    let restored = tools::dispatch(&mut store2, root2, "admin", &json!({ "op": "restore", "id": arch }))
        .expect("restore on the new machine");
    assert_eq!(restored["data"]["restored"].as_str(), Some(arch.as_str()));
    assert!(
        recall_ids(&mut store2, root2, "nightly cron rebuilds the search index").contains(&arch),
        "restore must work on the imported store"
    );
}

/// The seam a fresh-machine round trip cannot see: two machines ALREADY
/// sharing an entry (equal updated_at after import). Archive and restore are
/// deliberate state changes, so they must participate in the LWW merge; if
/// they never bump updated_at, the peer's import skips the line and the
/// visibility change silently never propagates, in either direction.
#[test]
fn archival_propagates_between_already_synced_stores() {
    // Machine A.
    let dir_a = tempfile::tempdir().expect("tempdir a");
    let (mut store_a, root_a) = setup(dir_a.path());
    let root_a = root_a.as_path();
    let id = remember(&mut store_a, root_a, "the scanner batch size is 50 because shared hosts kill long requests");

    // Machine B, synced via export/import: both now hold the entry with the
    // SAME updated_at.
    tools::dispatch(&mut store_a, root_a, "admin", &json!({ "op": "export" })).expect("export a");
    let exported = std::fs::read_to_string(root_a.join(".limpet/memory.jsonl")).expect("read export");
    let dir_b = tempfile::tempdir().expect("tempdir b");
    let (mut store_b, root_b) = setup(dir_b.path());
    let root_b = root_b.as_path();
    std::fs::create_dir_all(root_b.join(".limpet")).expect("mkdir .limpet b");
    std::fs::write(root_b.join(".limpet/memory.jsonl"), &exported).expect("seed b");
    tools::dispatch(&mut store_b, root_b, "admin", &json!({ "op": "import" })).expect("import b");
    assert!(
        recall_ids(&mut store_b, root_b, "why is the scanner batch size 50").contains(&id),
        "sanity: B holds the synced entry"
    );

    // A archives, exports; B imports: the archive must reach B.
    tools::dispatch(&mut store_a, root_a, "admin", &json!({ "op": "archive", "id": id })).expect("archive a");
    tools::dispatch(&mut store_a, root_a, "admin", &json!({ "op": "export" })).expect("export a2");
    let exported = std::fs::read_to_string(root_a.join(".limpet/memory.jsonl")).expect("read export 2");
    std::fs::write(root_b.join(".limpet/memory.jsonl"), &exported).expect("sync b 2");
    tools::dispatch(&mut store_b, root_b, "admin", &json!({ "op": "import" })).expect("import b 2");
    assert!(
        !recall_ids(&mut store_b, root_b, "why is the scanner batch size 50").contains(&id),
        "an archive on machine A must propagate to already-synced machine B"
    );

    // A restores, exports; B imports: the restore must reach B too.
    tools::dispatch(&mut store_a, root_a, "admin", &json!({ "op": "restore", "id": id })).expect("restore a");
    tools::dispatch(&mut store_a, root_a, "admin", &json!({ "op": "export" })).expect("export a3");
    let exported = std::fs::read_to_string(root_a.join(".limpet/memory.jsonl")).expect("read export 3");
    std::fs::write(root_b.join(".limpet/memory.jsonl"), &exported).expect("sync b 3");
    tools::dispatch(&mut store_b, root_b, "admin", &json!({ "op": "import" })).expect("import b 3");
    assert!(
        recall_ids(&mut store_b, root_b, "why is the scanner batch size 50").contains(&id),
        "a restore on machine A must propagate to already-synced machine B"
    );
}

/// The admin tool description promises archive hides an entry from recall,
/// the verify queue, AND map; `affected` shows memories on changed code and
/// must honor the same contract. Pin both surfaces.
#[test]
fn map_and_affected_exclude_archived_entries() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut store, root) = setup(dir.path());
    let root = root.as_path();
    let id = remember(&mut store, root, "the scanner batch size is 50 because shared hosts kill long requests");

    let in_map = |store: &mut Store| -> bool {
        let m = tools::dispatch(store, root, "map", &json!({ "target": "src/scanner.php" }))
            .expect("map");
        m["data"]["memories"]
            .as_array()
            .map(|a| a.iter().any(|e| e["id"].as_str() == Some(id.as_str())))
            .unwrap_or(false)
    };

    assert!(in_map(&mut store), "sanity: the memory shows in map before archival");
    tools::dispatch(&mut store, root, "admin", &json!({ "op": "archive", "id": id })).expect("archive");
    assert!(!in_map(&mut store), "an archived memory must not appear in map");

    // `affected` reads uncommitted git changes: build a real git fixture.
    let git = |args: &[&str]| {
        let ok = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .expect("git runs");
        assert!(ok.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&ok.stderr));
    };
    git(&["init", "-q", "."]);
    git(&["add", "."]);
    git(&["-c", "commit.gpgsign=false", "commit", "-qm", "fixture"]);
    std::fs::write(root.join("src/scanner.php"), FIXTURE_V2).expect("edit fixture");

    let aff = tools::dispatch(&mut store, root, "affected", &json!({})).expect("affected");
    let hit = aff["data"]["memories_on_changed_code"]
        .as_array()
        .map(|a| a.iter().any(|e| e["id"].as_str() == Some(id.as_str())))
        .unwrap_or(false);
    assert!(!hit, "an archived memory must not appear in affected: {aff}");

    tools::dispatch(&mut store, root, "admin", &json!({ "op": "restore", "id": id })).expect("restore");
    assert!(in_map(&mut store), "a restored memory must reappear in map");
}

#[test]
fn the_verify_queue_excludes_archived_entries() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut store, root) = setup(dir.path());
    let root = root.as_path();

    let r = tools::dispatch(
        &mut store,
        root,
        "remember",
        &json!({
            "kind": "fact",
            "body": "the scanner batches fifty items per run on shared hosts",
            "source": "verified",
            "evidence": { "command": "php -r 'echo 50;'", "output": "50" },
            "anchors": [ { "file": "src/scanner.php" } ]
        }),
    )
    .expect("remember verified");
    let id = r["data"]["id"].as_str().expect("id").to_string();

    // Rot the evidence, then confirm it enters the queue.
    std::fs::write(root.join("src/scanner.php"), FIXTURE_V2).expect("edit fixture");
    let _ = recall_ids(&mut store, root, "anything, to run the sweep");
    let queue = tools::dispatch(&mut store, root, "verify_queue", &json!({})).expect("queue");
    assert!(
        queue["data"].as_array().map(|q| q.iter().any(|e| e["id"].as_str() == Some(id.as_str()))).unwrap_or(false),
        "sanity: the stale verified fact enters the queue: {queue}"
    );

    tools::dispatch(&mut store, root, "admin", &json!({ "op": "archive", "id": id })).expect("archive");
    let queue = tools::dispatch(&mut store, root, "verify_queue", &json!({})).expect("queue");
    assert!(
        !queue["data"].as_array().map(|q| q.iter().any(|e| e["id"].as_str() == Some(id.as_str()))).unwrap_or(false),
        "an archived entry must not demand re-verification: {queue}"
    );
}

#[test]
fn archive_and_restore_refuse_nonsense() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut store, root) = setup(dir.path());
    let root = root.as_path();
    let id = remember(&mut store, root, "the scanner batch size is 50 because shared hosts kill long requests");

    // Unknown id: loud error naming the id.
    let err = tools::dispatch(&mut store, root, "admin", &json!({ "op": "archive", "id": "01NOPE" }))
        .expect_err("archiving an unknown id must fail");
    assert!(format!("{err:#}").contains("01NOPE"));

    // Restore before archive: loud error.
    let err = tools::dispatch(&mut store, root, "admin", &json!({ "op": "restore", "id": id }))
        .expect_err("restoring a non-archived entry must fail");
    assert!(format!("{err:#}").contains(&id));

    // Double archive: loud error, not a silent no-op.
    tools::dispatch(&mut store, root, "admin", &json!({ "op": "archive", "id": id })).expect("archive");
    let err = tools::dispatch(&mut store, root, "admin", &json!({ "op": "archive", "id": id }))
        .expect_err("double-archiving must fail loudly");
    assert!(format!("{err:#}").contains("already"));
}

#[test]
fn forget_cleans_the_archived_flag_and_status_counts_archived() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut store, root) = setup(dir.path());
    let root = root.as_path();
    let id = remember(&mut store, root, "the scanner batch size is 50 because shared hosts kill long requests");

    tools::dispatch(&mut store, root, "admin", &json!({ "op": "archive", "id": id })).expect("archive");
    let status = tools::dispatch(&mut store, root, "admin", &json!({ "op": "status" })).expect("status");
    assert_eq!(
        status["data"]["archived"].as_i64(),
        Some(1),
        "status must count archived entries: {status}"
    );

    tools::dispatch(&mut store, root, "admin", &json!({ "op": "forget", "id": id })).expect("forget");
    let orphans: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM archived WHERE entry_id = ?1", [&id], |r| r.get(0))
        .expect("archived table queryable");
    assert_eq!(orphans, 0, "forget must clean the archived sidecar row");
}
