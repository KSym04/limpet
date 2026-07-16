//! `limpet demo`: the anchor lifecycle, end to end, on a throwaway repo.
//!
//! This is the reproducible proof. It builds a tiny fixture in a temp
//! directory, drives the REAL tool handlers through `tools::dispatch`, and
//! prints the honesty envelope at every step so the output verifies itself:
//! a reader watches a memory go `stale` on an edit and heal on a revert,
//! with the reason attached, without trusting a GIF.
//!
//! It touches no user store (everything lives under the temp dir, deleted on
//! exit), reaches no network, and exits non-zero if any step's envelope does
//! not match the expected state. That last property lets it double as a CI
//! smoke test of the whole read path.

use anyhow::{bail, Context, Result};
use limpet::store::Store;
use limpet::tools;
use limpet::util::canonicalize_plain;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// The decision we store, then watch survive, go stale, and heal. Its exact
/// text is the needle we look for in each recall response.
const DECISION: &str = "batch size is 50 because shared hosts kill long requests";

/// The anchored symbol, in its original form. Enough entropy to anchor
/// unambiguously (a comment plus a distinct literal), so the write is never
/// refused as a low-entropy body.
const FIXTURE_ORIGINAL: &str = "<?php\n\
function scan_batch( $items ) {\n\
    // batch size 50: shared hosts kill long requests\n\
    return array_chunk( $items, 50 );\n\
}\n";

/// The same symbol after a real edit: the literal changes and the comment is
/// gone, so the normalized AST body hash differs and the anchor goes stale.
const FIXTURE_EDITED: &str = "<?php\n\
function scan_batch( $items ) {\n\
    return array_chunk( $items, 200 );\n\
}\n";

/// A temp directory that deletes itself on drop, so a failed assertion (which
/// returns early) still cleans up. `tempfile` is a dev-dependency only, so we
/// roll our own rather than pull a runtime crate for one command.
struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    fn new() -> Result<ScratchDir> {
        // pid + a monotonic-ish nanosecond stamp, plus a create_dir that FAILS
        // on an existing dir (never create_dir_all): a leftover dir from a
        // crashed run must not be silently reused, or its old store would leak
        // stale state into this run's assertions. On collision, bump the
        // suffix and retry; a fresh dir always wins within a few attempts.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        for attempt in 0..64u32 {
            let path = std::env::temp_dir().join(format!(
                "limpet-demo-{}-{}-{}",
                std::process::id(),
                nanos,
                attempt
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => return Ok(ScratchDir { path }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => {
                    return Err(e).with_context(|| {
                        format!("creating demo scratch dir {}", path.display())
                    });
                }
            }
        }
        bail!("could not create a fresh demo scratch dir after 64 attempts")
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        // Best effort: a leftover temp dir is a minor annoyance, never a
        // reason to mask the real result of the demo.
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Run the full lifecycle. Returns an error (non-zero exit) on any unexpected
/// state, which is what makes this usable as a CI smoke test.
pub fn run() -> Result<()> {
    let scratch = ScratchDir::new()?;

    // Fixture must exist before we canonicalize, and canonicalize before we
    // hand a root to the index: the sweep does `root.join("src/...")`, and on
    // macOS an uncanonicalized /var vs /private/var mismatch breaks that join.
    // The fixture repo is a SUBDIRECTORY of the scratch dir, not the scratch
    // dir itself: the store lives in a sibling subtree, and the index derives
    // its exclusion dir from the db path's grandparent, so a store INSIDE the
    // repo root would make the walker exclude the whole repo and index nothing.
    let repo_dir = scratch.path.join("repo");
    let src_dir = repo_dir.join("src");
    std::fs::create_dir_all(&src_dir).context("creating fixture src dir")?;
    let fixture = src_dir.join("scanner.php");
    write_fixture(&fixture, FIXTURE_ORIGINAL)?;

    let root = canonicalize_plain(&repo_dir)
        .with_context(|| format!("canonicalizing demo root {}", repo_dir.display()))?;

    // Store lives inside the scratch dir (never the user's real data
    // location, so the demo cannot collide with a real project's memory) but
    // OUTSIDE the fixture repo, mirroring the real <data_dir>/<key>/store.db
    // layout so the exclusion logic sees the same shape it sees in production.
    let db_path = scratch.path.join("store/demo/demo-store.db");
    let mut store = Store::open(&db_path).context("opening demo store")?;

    banner("limpet demo: the anchor lifecycle on a throwaway repo");

    // 1. Index the fixture so the symbol table knows `scan_batch`.
    step_header("1. index the fixture");
    let indexed = tools::dispatch(&mut store, &root, "admin", &json!({ "op": "index" }))
        .context("indexing fixture")?;
    print_meta(&indexed);
    // The index must actually SEE the fixture: prove `scan_batch` is in the
    // symbol table via `map`, so a silent walk-nothing regression (wrong
    // exclusion dir, broken discovery) fails the demo here instead of being
    // masked by the write path's defensive per-file reindex.
    let mapped = tools::dispatch(&mut store, &root, "map", &json!({ "target": "src/scanner.php" }))
        .context("mapping the fixture file")?;
    let has_symbol = mapped["data"]["symbols"]
        .as_array()
        .map(|s| {
            s.iter()
                .any(|sym| sym.get("name").and_then(Value::as_str) == Some("scan_batch"))
        })
        .unwrap_or(false);
    if !has_symbol {
        bail!(
            "demo failed: the index did not pick up scan_batch from the fixture\n{}",
            pretty(&mapped)
        );
    }
    println!("  index verified: scan_batch is in the symbol table.");

    // 2. Remember a decision, anchored to the function.
    step_header("2. remember a decision, anchored to scan_batch");
    let stored = tools::dispatch(
        &mut store,
        &root,
        "remember",
        &json!({
            "kind": "decision",
            "body": DECISION,
            "anchors": [ { "file": "src/scanner.php", "symbol": "scan_batch" } ]
        }),
    )
    .context("storing the anchored decision")?;
    print_meta(&stored);

    // 3. Recall it: active, no stale flag.
    step_header("3. recall -> active");
    let recalled = recall(&mut store, &root)?;
    let item = require_item(&recalled, DECISION)?;
    if is_stale(item) {
        bail!("demo failed: the memory is stale before any edit\n{}", pretty(item));
    }
    println!("{}", pretty(item));
    println!("  status: active, no stale flag. Expected.");

    // 4. Edit the anchored code, recall again: stale, with the reason.
    step_header("4. edit scan_batch, recall -> stale (reason attached)");
    write_fixture(&fixture, FIXTURE_EDITED)?;
    let recalled = recall(&mut store, &root)?;
    let item = require_item(&recalled, DECISION)?;
    if !is_stale(item) {
        bail!(
            "demo failed: the memory did NOT go stale after the anchored code changed\n{}",
            pretty(item)
        );
    }
    println!("{}", pretty(item));
    println!("  status: stale, reason attached. This is the whole product.");

    // 5. Revert the edit, recall again: healed back to active.
    step_header("5. revert the edit, recall -> healed to active");
    write_fixture(&fixture, FIXTURE_ORIGINAL)?;
    let recalled = recall(&mut store, &root)?;
    let item = require_item(&recalled, DECISION)?;
    if is_stale(item) {
        bail!(
            "demo failed: the memory did NOT heal after the code reverted\n{}",
            pretty(item)
        );
    }
    println!("{}", pretty(item));
    println!("  status: active again. Staleness is symmetric: no re-verification ritual.");

    println!();
    println!("Lifecycle verified: active -> stale(body_edited) -> active.");
    println!("No network, no user store touched, scratch dir removed on exit.");
    Ok(())
}

/// Write fixture bytes. The next sweep re-hashes the file because its size
/// changes between the two fixture variants (and its mtime moves forward);
/// either signal alone is enough for the change detector.
fn write_fixture(path: &Path, contents: &str) -> Result<()> {
    std::fs::write(path, contents)
        .with_context(|| format!("writing fixture {}", path.display()))?;
    Ok(())
}

fn recall(store: &mut Store, root: &Path) -> Result<Value> {
    tools::dispatch(
        store,
        root,
        "recall",
        &json!({ "task": "why is the batch size 50" }),
    )
    .context("recall")
}

/// The recall envelope is `{ "data": [ item, ... ], "meta": ... }`. Find the
/// item whose body is our decision. `body` is present on every recalled item.
fn require_item<'a>(resp: &'a Value, needle: &str) -> Result<&'a Value> {
    let items = resp
        .get("data")
        .and_then(Value::as_array)
        .context("recall response has no data array")?;
    items
        .iter()
        .find(|it| it.get("body").and_then(Value::as_str) == Some(needle))
        .with_context(|| format!("recall did not return the seeded memory: {needle:?}"))
}

/// An item is stale when its status is not active or it carries a `stale:`
/// flag. The wire shape omits `status` when active and omits `flags` when
/// empty (tokens are precious), so absence of both means active.
fn is_stale(item: &Value) -> bool {
    let status_stale = item
        .get("status")
        .and_then(Value::as_str)
        .map(|s| s != "active")
        .unwrap_or(false);
    let flag_stale = item
        .get("flags")
        .and_then(Value::as_array)
        .map(|flags| {
            flags
                .iter()
                .filter_map(Value::as_str)
                .any(|f| f.starts_with("stale"))
        })
        .unwrap_or(false);
    status_stale || flag_stale
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
}

/// Print only the meta block for steps where the data is noise (index,
/// remember): the interesting honesty lives in meta.
fn print_meta(resp: &Value) {
    if let Some(meta) = resp.get("meta") {
        println!("{}", pretty(meta));
    } else {
        println!("{}", pretty(resp));
    }
}

fn banner(title: &str) {
    println!("{title}");
    println!("{}", "=".repeat(title.len()));
    println!();
}

fn step_header(title: &str) {
    println!();
    println!("{title}");
    println!("{}", "-".repeat(title.len()));
}
