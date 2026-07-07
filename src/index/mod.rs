//! Index orchestration: full walk plus bounded incremental sweeps.
//!
//! Freshness model (spec I6): every tool call runs `sweep` first. A sweep
//! stats known files and discovers new ones, reindexes up to
//! `SWEEP_REINDEX_BUDGET` changed files inline, and reports the remainder
//! as dirty in the honesty envelope. Queries are never blocked by long
//! indexing work.

pub mod extract;
pub mod fqn;
pub mod lang;

use crate::config::RepoConfig;
use crate::memory::anchor;
use crate::store::{ImportReport, Store};
use anyhow::Result;
use rusqlite::params;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
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
    let hashes = anchor::ast_body_hashes(lang_id, src, &ranges)
        .unwrap_or_else(|_| vec![String::from("unhashed"); ranges.len()]);

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
        let body_hash = hashes
            .get(ordinal)
            .cloned()
            .unwrap_or_else(|| String::from("unhashed"));
        store.conn.execute(
            "INSERT INTO symbols(fqn, name, kind, file, start_line, end_line,
                                 body_hash, parent_fqn, ordinal)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                sym_fqn, sym.name, sym.kind, rel,
                sym.start_line as i64, sym.end_line as i64,
                body_hash, parent_fqn, ordinal as i64
            ],
        )?;
        count += 1;
    }
    for imp in &facts.imports {
        store
            .conn
            .execute("INSERT INTO imports(file, target) VALUES (?1,?2)", params![rel, imp])?;
    }
    for (caller, callee) in &facts.calls {
        let parent_refs: Vec<&str> = vec![];
        let caller_fqn = if caller == "<file>" {
            fqn::fqn(rel, &parent_refs, "<file>")
        } else {
            fqn::fqn(rel, &parent_refs, caller)
        };
        store.conn.execute(
            "INSERT INTO calls(caller_fqn, callee_name, file) VALUES (?1,?2,?3)",
            params![caller_fqn, callee, rel],
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
            !matches!(
                name.as_ref(),
                "node_modules" | "vendor" | "target" | "dist" | "build" | ".git" | ".limpet"
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
                store.conn.execute("DELETE FROM files WHERE path = ?1", [rel])?;
                store.conn.execute("DELETE FROM imports WHERE file = ?1", [rel])?;
                store.conn.execute("DELETE FROM calls WHERE file = ?1", [rel])?;
                store.conn.execute("DELETE FROM inherits WHERE file = ?1", [rel])?;
                report.removed.push(rel.clone());
            }
        }
    }

    use std::collections::HashSet;
    let known_set: HashSet<&String> = known.iter().map(|(p, _, _)| p).collect();
    let exclude = store_exclude_dir(store);
    for rel in discover(root, exclude.as_deref()) {
        if !known_set.contains(&rel) {
            changed.push(rel);
        }
    }

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
}
