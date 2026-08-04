//! Index orchestration: full walk plus bounded incremental sweeps.
//!
//! Freshness model (spec I6): every tool call runs `sweep` first. A sweep
//! stats known files and discovers new ones, reindexes up to
//! `SWEEP_REINDEX_BUDGET` changed files inline, and reports the remainder
//! as dirty in the honesty envelope. Queries are never blocked by long
//! indexing work.

pub mod extract;
pub mod fqn;
pub mod graph;
pub mod lang;

use crate::config::RepoConfig;
use crate::memory::anchor;
use crate::store::{ImportReport, Store};
use anyhow::Result;
use rusqlite::params;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Instant;

/// Max changed files reindexed inline during one sweep.
const SWEEP_REINDEX_BUDGET: usize = 32;

/// Files larger than this are never parsed for symbols, only file-level
/// indexed. Generated bundles (webpacked JS, concatenated vendor blobs)
/// routinely reach several MB and make tree-sitter pathologically slow,
/// but legacy hand-written source (old C++ engine translation units) does
/// legitimately exceed it, so the bound degrades to a file-level row
/// instead of dropping the file (invariant I-N1).
const MAX_PARSE_BYTES: u64 = 512 * 1024;

/// Files larger than this are skipped by the walker entirely. Above this
/// size nothing is plausibly a knowledge anchor (media, archives, database
/// dumps), and hashing it on every full index would be pure waste.
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;

/// Returns true for generated/minified assets that should never be indexed
/// (`app.min.js`, `bundle.min.mjs`, ...). These are noise as memory anchors
/// and slow to parse.
fn is_minified(name: &str) -> bool {
    matches!(
        name.rsplit_once('.').map(|(stem, _)| stem),
        Some(stem) if stem.ends_with(".min")
    )
}

#[derive(Debug, Default, serde::Serialize)]
pub struct IndexReport {
    pub files: usize,
    pub symbols: usize,
    pub failed: Vec<String>,
    pub took_ms: u128,
}

#[derive(Debug, Default, serde::Serialize)]
pub struct SweepReport {
    pub reindexed: Vec<String>,
    pub dirty: Vec<String>,
    pub removed: Vec<String>,
    pub failed: Vec<String>,
    /// The sweep itself errored: freshness is UNKNOWN, not clean.
    pub sweep_failed: bool,
}

fn file_meta(path: &Path) -> Option<(i64, i64)> {
    let md = std::fs::metadata(path).ok()?;
    let mtime = md
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos() as i64;
    Some((mtime, md.len() as i64))
}

/// First 128 bits of SHA-256 as hex; the content identity used everywhere.
fn short_hash(bytes: &[u8]) -> String {
    let d = Sha256::digest(bytes);
    d[..16].iter().map(|b| format!("{b:02x}")).collect()
}

/// Index a single file: parse, extract, replace its rows.
/// Parse failures are isolated: the file is recorded with `parse_ok = 0`
/// and indexing continues (spec section 10).
///
/// Files without a supported grammar are still indexed at file level: a
/// `files` row with a raw-byte content hash and no symbols. That is what
/// lets memories anchor to templates, styles, and configs (.twig, .scss,
/// .md, ...) and go stale when those files change.
pub(crate) fn index_file(
    store: &Store,
    root: &Path,
    rel: &str,
    ext: &HashMap<String, lang::Lang>,
) -> Result<(usize, bool)> {
    let abs = root.join(rel);
    let Some((mtime_ns, size)) = file_meta(&abs) else {
        return Ok((0, false));
    };
    let bytes = match std::fs::read(&abs) {
        Ok(b) => b,
        Err(_) => return Ok((0, false)),
    };
    // A grammar match only upgrades a file from file-level to symbol-level;
    // it must never downgrade it (invariant I-N1). Two degradations land on
    // the file-level row instead of dropping the file: source that is not
    // valid UTF-8 (CP49 legacy engine code, UTF-16 headers), and source over
    // the parse cap (giant hand-written translation units), because the cap
    // protects tree-sitter from generated bundles, not hashing.
    enum Plan {
        FileLevel(Vec<u8>),
        Parsed(lang::Lang, String),
    }
    let plan = match lang::detect_with(&abs, ext) {
        Some(_) if size > MAX_PARSE_BYTES as i64 => Plan::FileLevel(bytes),
        Some(lang_id) => match String::from_utf8(bytes) {
            Ok(s) => Plan::Parsed(lang_id, s),
            Err(e) => Plan::FileLevel(e.into_bytes()),
        },
        None => Plan::FileLevel(bytes),
    };

    // All row mutations for one file happen in one transaction: a crash (or
    // a concurrent resolve) must never observe a half-indexed file, which
    // previously looked fresh (matching mtime) yet had zero symbols and
    // could spuriously invalidate anchors (audit 2026-07).
    store.conn.execute_batch("BEGIN IMMEDIATE")?;
    let outcome = match &plan {
        Plan::FileLevel(raw) => index_file_level(store, rel, mtime_ns, size, raw),
        Plan::Parsed(lang_id, src) => index_file_parsed(store, rel, mtime_ns, size, *lang_id, src),
    };
    match outcome {
        Ok(r) => {
            store.conn.execute_batch("COMMIT")?;
            Ok(r)
        }
        Err(e) => {
            let _ = store.conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// Symbol-level indexing of decodable source. Runs inside index_file's
/// transaction. The file is parsed ONCE; all symbol body hashes come from
/// that single tree instead of a full reparse per symbol.
fn index_file_parsed(
    store: &Store,
    rel: &str,
    mtime_ns: i64,
    size: i64,
    lang_id: lang::Lang,
    src: &str,
) -> Result<(usize, bool)> {
    let content_hash = short_hash(src.as_bytes());

    store.conn.execute("DELETE FROM symbols WHERE file = ?1", [rel])?;
    store.conn.execute("DELETE FROM imports WHERE file = ?1", [rel])?;
    store.conn.execute("DELETE FROM calls WHERE file = ?1", [rel])?;
    store.conn.execute("DELETE FROM inherits WHERE file = ?1", [rel])?;

    let (facts, parse_ok) = match extract::extract(lang_id, src) {
        Ok(f) => (f, true),
        Err(_) => (extract::FileFacts::default(), false),
    };

    store.conn.execute(
        "INSERT INTO files(path, lang, mtime_ns, size, hash, parse_ok)
         VALUES (?1,?2,?3,?4,?5,?6)
         ON CONFLICT(path) DO UPDATE SET lang=excluded.lang,
           mtime_ns=excluded.mtime_ns, size=excluded.size,
           hash=excluded.hash, parse_ok=excluded.parse_ok",
        params![rel, lang_id.as_str(), mtime_ns, size, content_hash, parse_ok],
    )?;

    let ranges: Vec<(usize, usize)> = facts.symbols.iter().map(|s| s.byte_range).collect();
    let hashes: Vec<(String, Option<u32>)> = match anchor::ast_body_hashes(lang_id, src, &ranges) {
        Ok(v) => v.into_iter().map(|(h, l)| (h, Some(l))).collect(),
        Err(_) => vec![(String::from("unhashed"), None); ranges.len()],
    };

    let mut count = 0usize;
    for (ordinal, sym) in facts.symbols.iter().enumerate() {
        let parent_refs: Vec<&str> = sym.parents.iter().map(String::as_str).collect();
        let sym_fqn = fqn::fqn(rel, &parent_refs, &sym.name);
        let parent_fqn = if sym.parents.is_empty() {
            None
        } else {
            let up = &parent_refs[..parent_refs.len() - 1];
            Some(fqn::fqn(rel, up, parent_refs[parent_refs.len() - 1]))
        };
        let (body_hash, body_len) = hashes
            .get(ordinal)
            .cloned()
            .unwrap_or_else(|| (String::from("unhashed"), None));
        // `disamb` is the overload/impl discriminator (schema v7). It is NULL
        // for every symbol class that cannot collide, and it never enters the
        // body hash, so persisting it changes no existing column.
        store.conn.execute(
            "INSERT INTO symbols(fqn, name, kind, file, start_line, end_line,
                                 body_hash, parent_fqn, ordinal, body_len, disamb)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![
                sym_fqn, sym.name, sym.kind, rel,
                sym.start_line as i64, sym.end_line as i64,
                body_hash, parent_fqn, ordinal as i64, body_len, sym.disamb
            ],
        )?;
        count += 1;
    }
    for imp in &facts.imports {
        store
            .conn
            .execute("INSERT INTO imports(file, target) VALUES (?1,?2)", params![rel, imp])?;
    }
    // `caller_fqn` MUST equal the calling symbol's own `symbols.fqn`: that is
    // the column `graph::lineage` joins on for the descendants (callee)
    // direction. Building it from the full parents path is what makes a method
    // inside a class, a C++ out-of-line definition, and a Rust inline-mod fn
    // match; a flat name only ever matched top-level symbols. The `<file>`
    // sentinel needs no special case: it carries no parents by construction.
    for call in &facts.calls {
        let parent_refs: Vec<&str> = call.parents.iter().map(String::as_str).collect();
        let caller_fqn = fqn::fqn(rel, &parent_refs, &call.name);
        store.conn.execute(
            "INSERT INTO calls(caller_fqn, callee_name, file) VALUES (?1,?2,?3)",
            params![caller_fqn, call.callee, rel],
        )?;
    }
    for inh in &facts.inherits {
        let parent_refs: Vec<&str> = inh.parents.iter().map(String::as_str).collect();
        let child_fqn = fqn::fqn(rel, &parent_refs, &inh.name);
        store.conn.execute(
            "INSERT INTO inherits(child_fqn, parent_name, rel, file) VALUES (?1,?2,?3,?4)",
            params![child_fqn, inh.parent_name, inh.rel, rel],
        )?;
    }
    Ok((count, parse_ok))
}

/// File-level row: content hash over raw bytes, no symbols. Used for files
/// without a grammar and for grammar-matched files that are not valid UTF-8.
fn index_file_level(
    store: &Store,
    rel: &str,
    mtime_ns: i64,
    size: i64,
    bytes: &[u8],
) -> Result<(usize, bool)> {
    let content_hash = short_hash(bytes);
    store.conn.execute("DELETE FROM symbols WHERE file = ?1", [rel])?;
    store.conn.execute("DELETE FROM imports WHERE file = ?1", [rel])?;
    store.conn.execute("DELETE FROM calls WHERE file = ?1", [rel])?;
    store.conn.execute("DELETE FROM inherits WHERE file = ?1", [rel])?;
    store.conn.execute(
        "INSERT INTO files(path, lang, mtime_ns, size, hash, parse_ok)
         VALUES (?1,NULL,?2,?3,?4,1)
         ON CONFLICT(path) DO UPDATE SET lang=NULL,
           mtime_ns=excluded.mtime_ns, size=excluded.size,
           hash=excluded.hash, parse_ok=1",
        params![rel, mtime_ns, size, content_hash],
    )?;
    Ok((0, true))
}

/// The limpet data directory holding this (and every other) project's
/// store, resolved from the live connection. It must never be indexed:
/// when `LIMPET_DATA_DIR` points inside the repository, the SQLite WAL
/// mutates on every write, so indexing it would dirty the index on each
/// sweep forever.
fn store_exclude_dir(store: &Store) -> Option<std::path::PathBuf> {
    let db = store.conn.path().filter(|p| !p.is_empty())?;
    let db = Path::new(db);
    // <data_dir>/<repo_key>/store.db -> exclude <data_dir> entirely.
    let key_dir = db.parent()?;
    key_dir.parent().unwrap_or(key_dir).canonicalize().ok()
}

/// Walk the repository and collect indexable files, honoring .gitignore, an
/// optional `.limpetignore` (gitignore syntax; works even outside a git repo),
/// a built-in directory skip list, a max file size, and a minified-asset skip.
/// `exclude` (the store's own data dir) is never descended into.
///
/// Every file that survives those bounds is indexed, whether or not a
/// grammar exists for it: symbol-bearing files get full extraction, the
/// rest get file-level rows so anchors can attach to them.
fn discover(root: &Path, exclude: Option<&Path>) -> Vec<String> {
    let mut out = Vec::new();
    // hidden(false): dotfiles and dot-dirs are walked so tracked files like
    // .github/workflows/*.yml and .gitignore are anchorable; .git itself is
    // excluded below and junk dirs stay opt-out via .limpetignore.
    let walker = ignore::WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .git_global(false)
        .max_filesize(Some(MAX_FILE_BYTES))
        .add_custom_ignore_filename(".limpetignore")
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            // Hard-skip trees that are near-universally generated or vendored and
            // never hold hand-authored source. Repos that forget to gitignore
            // these (the boost/RanThirdParty class) would otherwise burn the
            // sweep budget and ship thousands of paths in the honesty envelope.
            // Project-specific vendored dirs still opt out via `.limpetignore`.
            !matches!(
                name.as_ref(),
                "node_modules"
                    | "vendor"
                    | "target"
                    | "dist"
                    | "build"
                    | ".git"
                    | ".limpet"
                    | "__pycache__"
                    | ".venv"
                    | "venv"
                    | ".mypy_cache"
                    | ".pytest_cache"
                    | ".ruff_cache"
                    | ".tox"
                    | "Pods"
                    | ".gradle"
                    | ".next"
                    | ".nuxt"
                    | ".svelte-kit"
                    | "bower_components"
                    | "coverage"
                    | ".terraform"
            )
        })
        .build();
    let croot = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    for entry in walker.flatten() {
        let p = entry.path();
        if p.is_file() {
            let name = entry.file_name().to_string_lossy();
            if is_minified(&name) {
                continue;
            }
            if let Ok(rel) = p.strip_prefix(root) {
                if exclude.is_some_and(|ex| croot.join(rel).starts_with(ex)) {
                    continue;
                }
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    out.sort();
    out
}

/// Full (re)index of the repository.
pub fn full_index(store: &Store, root: &Path) -> Result<IndexReport> {
    let start = Instant::now();
    let mut report = IndexReport::default();
    // An explicit index surfaces a broken `.limpet.json` as a hard error, so
    // the user learns their config is wrong instead of silently falling back
    // to the built-in grammar table.
    let ext = RepoConfig::load(root)?.extensions;
    let exclude = store_exclude_dir(store);
    for rel in discover(root, exclude.as_deref()) {
        match index_file(store, root, &rel, &ext) {
            Ok((syms, parse_ok)) => {
                report.files += 1;
                report.symbols += syms;
                if !parse_ok {
                    report.failed.push(rel);
                }
            }
            Err(_) => report.failed.push(rel),
        }
    }
    store.kv_set("indexed_at", &now_iso())?;
    store.kv_set("project_root", &root.to_string_lossy())?;
    report.took_ms = start.elapsed().as_millis();
    Ok(report)
}

/// Full index plus first-run bootstrap: on a brand-new store (never indexed)
/// with auto-import enabled and a committed `.limpet/memory.jsonl`, seed the
/// store from that file so a teammate who clones the repo gets the shared
/// memory immediately. Index runs FIRST so import re-resolves anchor hashes
/// against the freshly built index. Returns the index report and, when a
/// bootstrap import ran, its report.
pub fn index_and_bootstrap(
    store: &mut Store,
    root: &Path,
) -> Result<(IndexReport, Option<ImportReport>)> {
    let was_fresh = store.kv_get("indexed_at")?.is_none();
    let report = full_index(store, root)?;
    let import = if was_fresh { maybe_auto_import(store, root)? } else { None };
    Ok((report, import))
}

/// Import a committed `.limpet/memory.jsonl` when auto-import is enabled and
/// the file exists. Every existing import guard applies (secrets, size caps,
/// LWW, anchor re-resolution); this only decides whether to invoke them.
fn maybe_auto_import(store: &mut Store, root: &Path) -> Result<Option<ImportReport>> {
    if !RepoConfig::load(root).unwrap_or_default().auto_import {
        return Ok(None);
    }
    let path = root.join(".limpet").join("memory.jsonl");
    if !path.exists() {
        return Ok(None);
    }
    let f = std::fs::File::open(&path)?;
    let mut reader = std::io::BufReader::new(f);
    Ok(Some(store.import_jsonl(&mut reader)?))
}

/// Drop every row belonging to a file that no longer exists on disk.
///
/// One transaction per removed file, matching index_file's per-file idiom: a
/// crash between the DELETEs would otherwise leave the `files` row gone while
/// imports/calls/inherits rows survive, and nothing would ever collect them
/// (the sweep only revisits paths the `files` table still knows about).
/// `symbols` needs no DELETE of its own: it cascades off `files(path)`, which
/// holds because both store openers set `PRAGMA foreign_keys = ON` (SQLite
/// defaults the pragma OFF and scopes it per connection).
fn purge_removed_file(store: &Store, rel: &str) -> Result<()> {
    store.conn.execute_batch("BEGIN IMMEDIATE")?;
    let outcome = (|| -> Result<()> {
        store.conn.execute("DELETE FROM files WHERE path = ?1", [rel])?;
        store.conn.execute("DELETE FROM imports WHERE file = ?1", [rel])?;
        store.conn.execute("DELETE FROM calls WHERE file = ?1", [rel])?;
        store.conn.execute("DELETE FROM inherits WHERE file = ?1", [rel])?;
        Ok(())
    })();
    match outcome {
        Ok(()) => {
            // A failed COMMIT does not always end the transaction (SQLITE_BUSY
            // explicitly leaves it open), so roll back rather than hand back a
            // connection stuck mid-write for the rest of the process.
            if let Err(e) = store.conn.execute_batch("COMMIT") {
                let _ = store.conn.execute_batch("ROLLBACK");
                return Err(e.into());
            }
            Ok(())
        }
        Err(e) => {
            let _ = store.conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// Bounded incremental sweep: detect changed/new/removed files, reindex up
/// to the budget inline, report the rest dirty.
/// `ext` is the extension override map from `.limpet.json`, loaded ONCE per
/// tool dispatch and threaded in, so every index touch within one call
/// indexes under the same rules (and the hot path never re-reads the file).
pub fn sweep(
    store: &Store,
    root: &Path,
    ext: &HashMap<String, lang::Lang>,
) -> Result<SweepReport> {
    let mut report = SweepReport::default();

    let mut stmt = store
        .conn
        .prepare("SELECT path, mtime_ns, size FROM files")?;
    let known: Vec<(String, i64, i64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    let mut changed: Vec<String> = Vec::new();
    for (rel, mtime_ns, size) in &known {
        match file_meta(&root.join(rel)) {
            Some((m, s)) if m == *mtime_ns && s == *size => {}
            Some(_) => changed.push(rel.clone()),
            None => {
                // File gone: purge its rows now (cheap) so anchors resolve
                // against reality.
                purge_removed_file(store, rel)?;
                report.removed.push(rel.clone());
            }
        }
    }

    let known_set: HashSet<&String> = known.iter().map(|(p, _, _)| p).collect();
    let exclude = store_exclude_dir(store);
    for rel in discover(root, exclude.as_deref()) {
        if !known_set.contains(&rel) {
            changed.push(rel);
        }
    }

    // Anchored files first: staleness must land where memories live. Stale
    // and invalidated entries are included: they re-resolve and heal, so
    // their files matter just as much; only superseded is final. Stable
    // sort preserves discovery order within each group.
    let mut astmt = store.conn.prepare(
        "SELECT DISTINCT a.file FROM anchors a
         JOIN entries e ON e.id = a.entry_id
         WHERE e.status != 'superseded'",
    )?;
    let anchored: HashSet<String> = astmt.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    drop(astmt);
    changed.sort_by_key(|rel| !anchored.contains(rel));

    for rel in changed.iter().take(SWEEP_REINDEX_BUDGET) {
        match index_file(store, root, rel, ext) {
            Ok((_, true)) => report.reindexed.push(rel.clone()),
            Ok((_, false)) => report.failed.push(rel.clone()),
            Err(_) => report.failed.push(rel.clone()),
        }
    }
    for rel in changed.iter().skip(SWEEP_REINDEX_BUDGET) {
        report.dirty.push(rel.clone());
    }

    if !report.reindexed.is_empty() || !report.removed.is_empty() {
        store.kv_set("indexed_at", &now_iso())?;
    }
    Ok(report)
}

pub fn now_iso() -> String {
    // RFC3339 UTC without subsecond noise, no external time crate needed.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    iso_from_secs(secs)
}

/// Unix seconds to the same RFC3339 UTC shape `now_iso` emits. Public so LWW
/// participants (archive/restore) can mint a stamp strictly newer than an
/// existing one when both land within the same wall-clock second.
pub fn iso_from_secs(secs: u64) -> String {
    let days = secs / 86_400;
    let (y, m, d) = civil_from_days(days as i64);
    let rem = secs % 86_400;
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Days since 1970-01-01 to (year, month, day). Howard Hinnant's algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn is_minified_matches_only_dot_min_assets() {
        assert!(is_minified("app.min.js"));
        assert!(is_minified("bundle.min.mjs"));
        assert!(is_minified("jquery.min.css"));
        assert!(!is_minified("app.js"));
        assert!(!is_minified("minify.js")); // stem is "minify", not "*.min"
        assert!(!is_minified("min.js")); // stem is "min", no ".min" suffix
        assert!(!is_minified("README.md"));
    }

    #[test]
    fn discover_skips_oversize_minified_and_limpetignored() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();

        // Kept: ordinary source under the size cap.
        fs::write(root.join("keep.php"), "<?php function a() {}\n").unwrap();
        // Skipped: minified asset.
        fs::write(root.join("app.min.js"), "var a=1;\n").unwrap();
        // Skipped: over the size cap.
        fs::write(root.join("bundle.js"), "x".repeat((MAX_FILE_BYTES + 1) as usize)).unwrap();
        // Skipped via .limpetignore (works without a git repo).
        fs::write(root.join(".limpetignore"), "ignored/\n").unwrap();
        fs::create_dir(root.join("ignored")).unwrap();
        fs::write(root.join("ignored/secret.php"), "<?php function b() {}\n").unwrap();

        let found = discover(root, None);
        // .limpetignore itself is a legitimate anchor target (dotfiles are
        // walked since hidden(false)); everything else bounded out.
        assert_eq!(
            found,
            vec![".limpetignore".to_string(), "keep.php".to_string()],
            "unexpected: {found:?}"
        );
    }

    #[test]
    fn discover_skips_common_generated_dirs() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        fs::write(root.join("keep.php"), "<?php function a() {}\n").unwrap();
        // Universally generated/vendored trees that a repo often forgets to
        // gitignore (the boost/RanThirdParty class of feedback). None hold
        // hand-authored source; walking them wastes the sweep budget and, worse,
        // ships their paths in the honesty envelope's dirty list.
        for junk in [
            "__pycache__", ".venv", "venv", "Pods", ".next", ".nuxt",
            "bower_components", ".gradle", "coverage", ".pytest_cache",
        ] {
            fs::create_dir_all(root.join(junk)).unwrap();
            fs::write(root.join(junk).join("f.js"), "var a=1;\n").unwrap();
        }
        let found = discover(root, None);
        assert_eq!(
            found,
            vec!["keep.php".to_string()],
            "a generated/vendored dir leaked into discovery: {found:?}"
        );
    }

    #[test]
    fn discover_walks_hidden_paths_but_never_dot_git_or_dot_limpet() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join(".github/workflows")).unwrap();
        fs::write(root.join(".github/workflows/ci.yml"), "on: push\n").unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::create_dir_all(root.join(".limpet")).unwrap();
        fs::write(root.join(".limpet/memory.jsonl"), "{}\n").unwrap();

        let found = discover(root, None);
        assert_eq!(
            found,
            vec![".github/workflows/ci.yml".to_string()],
            "unexpected: {found:?}"
        );
    }

    #[test]
    fn discover_includes_files_without_a_grammar() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        fs::write(root.join("page.twig"), "{% block content %}{% endblock %}\n").unwrap();
        fs::write(root.join("style.scss"), ".a { color: red; }\n").unwrap();
        fs::write(root.join("notes.md"), "# notes\n").unwrap();
        fs::write(root.join("logic.php"), "<?php function a() {}\n").unwrap();

        let found = discover(root, None);
        assert_eq!(
            found,
            vec![
                "logic.php".to_string(),
                "notes.md".to_string(),
                "page.twig".to_string(),
                "style.scss".to_string(),
            ],
            "every bounded file must be discoverable: {found:?}"
        );
    }

    #[test]
    fn oversize_source_gets_file_level_row_not_dropped() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        // Over the parse cap, under the walk cap: a giant legacy translation
        // unit. Must be file-level anchorable, never parsed, never dropped.
        let big = format!(
            "int big_fn(int a) {{ return a + 1; }}\n// {}\n",
            "x".repeat(MAX_PARSE_BYTES as usize)
        );
        fs::write(root.join("GLGaeaClient.cpp"), &big).unwrap();
        let store = Store::open_in_memory().unwrap();
        let report = full_index(&store, root).unwrap();
        assert_eq!(report.files, 1, "over-parse-cap file must stay indexed");
        assert_eq!(report.symbols, 0, "over-parse-cap file must not be parsed");
        let lang: Option<String> = store
            .conn
            .query_row("SELECT lang FROM files WHERE path = 'GLGaeaClient.cpp'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(lang, None);
    }

    #[test]
    fn store_inside_repo_is_never_indexed() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        fs::write(root.join("keep.php"), "<?php function a() {}\n").unwrap();

        // Store lives inside the repo, as with LIMPET_DATA_DIR=<repo>/.data.
        let db_path = root.join(".data/repo-key/store.db");
        let store = Store::open(&db_path).unwrap();
        let report = full_index(&store, root).unwrap();
        assert_eq!(report.files, 1, "store artifacts must not be indexed");

        let rows: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM files WHERE path LIKE '.data/%'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(rows, 0, "no files row may point into the data dir");

        // Sweep must not rediscover it either.
        let sweep_report = sweep(&store, root, &Default::default()).unwrap();
        assert!(
            sweep_report.reindexed.iter().all(|p| !p.starts_with(".data/")),
            "sweep leaked store artifacts: {:?}",
            sweep_report.reindexed
        );
    }

    #[test]
    fn file_level_index_hashes_content_and_tracks_changes() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        fs::write(root.join("page.twig"), "{% block a %}{% endblock %}\n").unwrap();
        let store = Store::open_in_memory().unwrap();
        let report = full_index(&store, root).unwrap();
        assert_eq!(report.files, 1);
        assert_eq!(report.symbols, 0);

        let (lang, hash1): (Option<String>, String) = store
            .conn
            .query_row("SELECT lang, hash FROM files WHERE path = 'page.twig'", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(lang, None, "grammar-less files carry no lang");

        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(root.join("page.twig"), "{% block b %}{% endblock %}\n").unwrap();
        sweep(&store, root, &Default::default()).unwrap();
        let hash2: String = store
            .conn
            .query_row("SELECT hash FROM files WHERE path = 'page.twig'", [], |r| r.get(0))
            .unwrap();
        assert_ne!(hash1, hash2, "content change must change the file hash");
    }

    #[test]
    fn file_level_index_handles_binary_content() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        fs::write(root.join("logo.png"), [0x89u8, 0x50, 0x4e, 0x47, 0x00, 0x01]).unwrap();
        let store = Store::open_in_memory().unwrap();
        let report = full_index(&store, root).unwrap();
        assert_eq!(report.files, 1);
        assert!(report.failed.is_empty(), "binary files must index cleanly: {:?}", report.failed);
    }

    #[test]
    fn inherits_persisted_and_repopulated_on_reindex() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("a.rs"), "struct Dog;\nimpl Animal for Dog {}\n").unwrap();
        let store = Store::open_in_memory().unwrap();
        full_index(&store, root).unwrap();

        let (child, parent, rel): (String, String, String) = store
            .conn
            .query_row(
                "SELECT child_fqn, parent_name, rel FROM inherits WHERE file='a.rs'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(parent, "Animal");
        assert_eq!(rel, "impl_trait");
        assert!(child.ends_with("Dog"), "child_fqn resolves to the Dog type: {child}");

        // Reindex must delete+reinsert, not duplicate.
        full_index(&store, root).unwrap();
        let n: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM inherits WHERE file='a.rs'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "no duplicate edges after reindex");
    }

    #[test]
    fn parsed_symbols_carry_body_len() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("m.py"), "def f(x):\n    y = x + 1\n    return y * 2\n").unwrap();
        let store = Store::open_in_memory().unwrap();
        full_index(&store, root).unwrap();
        let (len, hash): (Option<i64>, String) = store.conn.query_row(
            "SELECT body_len, body_hash FROM symbols WHERE name='f'",
            [], |r| Ok((r.get(0)?, r.get(1)?)),
        ).unwrap();
        assert_ne!(hash, "unhashed");
        let len = len.expect("parsed symbol must carry body_len");
        assert!(len > 0);
    }

    #[test]
    fn symbols_persist_disamb_for_rust_twin_trait_impls() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // Twin impls of one type: identical FQN, identical body, distinct
        // trait. Without a persisted discriminator the two rows are
        // indistinguishable and an edit to one is masked by the other.
        std::fs::write(
            root.join("t.rs"),
            "struct T;\n\
             impl A for T { fn go(&self) {} }\n\
             impl B for T { fn go(&self) {} }\n\
             fn plain() {}\n",
        )
        .unwrap();
        let store = Store::open_in_memory().unwrap();
        full_index(&store, root).unwrap();

        let mut stmt = store
            .conn
            .prepare("SELECT fqn, disamb FROM symbols WHERE name = 'go' ORDER BY disamb")
            .unwrap();
        let rows: Vec<(String, Option<String>)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(rows.len(), 2, "both twin methods persist: {rows:?}");
        assert_eq!(rows[0].0, rows[1].0, "twins share one FQN: {rows:?}");
        assert_eq!(rows[0].1, Some("A".to_string()), "{rows:?}");
        assert_eq!(rows[1].1, Some("B".to_string()), "{rows:?}");

        let plain: Option<String> = store
            .conn
            .query_row("SELECT disamb FROM symbols WHERE name = 'plain'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(plain, None, "a free function carries no discriminator");
    }

    #[test]
    fn symbols_persist_disamb_for_cpp_overloads() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("o.cpp"), "void f(int) {}\nvoid f(double) {}\n").unwrap();
        let store = Store::open_in_memory().unwrap();
        full_index(&store, root).unwrap();

        let mut stmt = store
            .conn
            .prepare("SELECT fqn, disamb FROM symbols WHERE name = 'f' ORDER BY disamb")
            .unwrap();
        let rows: Vec<(String, Option<String>)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(rows.len(), 2, "both overloads persist: {rows:?}");
        assert_eq!(rows[0].0, rows[1].0, "overloads share one FQN: {rows:?}");
        assert_eq!(rows[0].1, Some("(double)".to_string()), "{rows:?}");
        assert_eq!(rows[1].1, Some("(int)".to_string()), "{rows:?}");
    }

    #[test]
    fn removed_file_purge_clears_every_table_and_spares_the_survivor() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // Both fixtures populate all four purged tables: a symbol (struct +
        // method + fn), an import, a call, and an inherit edge.
        std::fs::write(
            root.join("gone.rs"),
            "use std::fmt;\n\
             struct Dog;\n\
             impl Animal for Dog { fn speak(&self) { helper(); } }\n\
             fn helper() {}\n",
        )
        .unwrap();
        std::fs::write(
            root.join("keep.rs"),
            "use std::io;\n\
             struct Cat;\n\
             impl Pet for Cat { fn meow(&self) { noop(); } }\n\
             fn noop() {}\n",
        )
        .unwrap();
        let store = Store::open_in_memory().unwrap();
        full_index(&store, root).unwrap();

        let counts = |rel: &str| -> (i64, i64, i64, i64, i64) {
            let q = |sql: &str| -> i64 { store.conn.query_row(sql, [rel], |r| r.get(0)).unwrap() };
            (
                q("SELECT COUNT(*) FROM files WHERE path = ?1"),
                q("SELECT COUNT(*) FROM symbols WHERE file = ?1"),
                q("SELECT COUNT(*) FROM imports WHERE file = ?1"),
                q("SELECT COUNT(*) FROM calls WHERE file = ?1"),
                q("SELECT COUNT(*) FROM inherits WHERE file = ?1"),
            )
        };
        let before = counts("gone.rs");
        assert_eq!(before.0, 1, "fixture must be indexed: {before:?}");
        assert!(
            before.1 > 0 && before.2 > 0 && before.3 > 0 && before.4 > 0,
            "fixture must populate every purged table: {before:?}"
        );
        let survivor = counts("keep.rs");
        assert!(
            survivor.0 == 1
                && survivor.1 > 0
                && survivor.2 > 0
                && survivor.3 > 0
                && survivor.4 > 0,
            "the survivor must populate every purged table too, or the \
             untouched-rows assertion below proves nothing: {survivor:?}"
        );

        std::fs::remove_file(root.join("gone.rs")).unwrap();
        let report = sweep(&store, root, &Default::default()).unwrap();
        assert_eq!(report.removed, vec!["gone.rs".to_string()]);
        assert_eq!(
            counts("gone.rs"),
            (0, 0, 0, 0, 0),
            "the purge must leave no orphaned rows behind the deleted files row"
        );
        assert_eq!(counts("keep.rs"), survivor, "the survivor's rows are untouched");
    }

    #[test]
    fn removed_file_purge_rolls_back_every_delete_when_one_fails() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("gone.rs"),
            "use std::fmt;\n\
             struct Dog;\n\
             impl Animal for Dog { fn speak(&self) { helper(); } }\n\
             fn helper() {}\n",
        )
        .unwrap();
        let store = Store::open_in_memory().unwrap();
        full_index(&store, root).unwrap();

        let counts = || -> (i64, i64, i64, i64, i64) {
            let q = |sql: &str| -> i64 {
                store.conn.query_row(sql, ["gone.rs"], |r| r.get(0)).unwrap()
            };
            (
                q("SELECT COUNT(*) FROM files WHERE path = ?1"),
                q("SELECT COUNT(*) FROM symbols WHERE file = ?1"),
                q("SELECT COUNT(*) FROM imports WHERE file = ?1"),
                q("SELECT COUNT(*) FROM calls WHERE file = ?1"),
                q("SELECT COUNT(*) FROM inherits WHERE file = ?1"),
            )
        };
        let before = counts();
        assert!(
            before.0 == 1 && before.1 > 0 && before.2 > 0 && before.3 > 0 && before.4 > 0,
            "fixture must populate every purged table: {before:?}"
        );

        // Fail the DELETE that runs right after the files row (and, by
        // cascade, its symbols) is already gone. RAISE(ABORT) undoes only the
        // aborted statement and leaves the transaction open, so the files row
        // comes back only if the purge rolls back. Without the transaction
        // that first DELETE would already have committed, orphaning the
        // imports/calls/inherits rows forever: the sweep only ever revisits
        // paths the files table still knows about.
        store
            .conn
            .execute_batch(
                "CREATE TRIGGER halt_import_delete BEFORE DELETE ON imports
                 BEGIN SELECT RAISE(ABORT, 'injected purge failure'); END",
            )
            .unwrap();
        let err = purge_removed_file(&store, "gone.rs").unwrap_err();
        store
            .conn
            .execute_batch("DROP TRIGGER halt_import_delete")
            .unwrap();
        assert!(
            err.to_string().contains("injected purge failure"),
            "the injected failure must surface, not be swallowed: {err}"
        );
        assert_eq!(
            counts(),
            before,
            "a purge that fails part way must leave the file's rows exactly as they were"
        );
    }

    #[test]
    fn sweep_reindexes_anchored_files_before_the_budget_cuts() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // 64 unanchored files created (and named) ahead of one anchored file,
        // so a blind take(32) can never reach it whatever the walk order does
        // within the first group.
        for i in 0..64 {
            std::fs::write(root.join(format!("a{i:03}.py")), format!("def f{i}():\n    return {i}\n"))
                .unwrap();
        }
        std::fs::write(root.join("zz_anchored.py"), "def g():\n    return 41\n").unwrap();
        let store = Store::open_in_memory().unwrap();
        full_index(&store, root).unwrap();

        // Anchor a live entry to zz_anchored.py (sweep reads the tables directly).
        store
            .conn
            .execute(
                "INSERT INTO entries(id, kind, body, created_at, updated_at, source, confidence, status)
                 VALUES ('e1','fact','b','2026-07-11T00:00:00Z','2026-07-11T00:00:00Z','explicit',0.9,'active')",
                [],
            )
            .unwrap();
        store
            .conn
            .execute("INSERT INTO anchors(entry_id, file) VALUES ('e1','zz_anchored.py')", [])
            .unwrap();

        // Touch everything: every file is now changed.
        store.conn.execute("UPDATE files SET mtime_ns = 0", []).unwrap();

        let report = sweep(&store, root, &Default::default()).unwrap();
        assert_eq!(report.reindexed.len(), SWEEP_REINDEX_BUDGET);
        assert!(
            report.reindexed.contains(&"zz_anchored.py".to_string()),
            "anchored file must be inside the budget, got dirty tail of {} files",
            report.dirty.len()
        );
    }
}
