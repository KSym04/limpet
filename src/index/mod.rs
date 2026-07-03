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

use crate::memory::anchor;
use crate::store::Store;
use anyhow::Result;
use rusqlite::params;
use sha2::{Digest, Sha256};
use std::path::Path;
use std::time::Instant;

/// Max changed files reindexed inline during one sweep.
const SWEEP_REINDEX_BUDGET: usize = 32;

/// Files larger than this are skipped by the walker. Generated bundles
/// (webpacked JS, concatenated vendor blobs) routinely reach several MB and
/// make tree-sitter pathologically slow while yielding no useful anchors.
/// Hand-written source essentially never exceeds this.
const MAX_FILE_BYTES: u64 = 512 * 1024;

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

/// Index a single file: parse, extract, replace its rows.
/// Parse failures are isolated: the file is recorded with `parse_ok = 0`
/// and indexing continues (spec section 10).
fn index_file(store: &Store, root: &Path, rel: &str) -> Result<(usize, bool)> {
    let abs = root.join(rel);
    let Some(lang_id) = lang::detect(&abs) else {
        return Ok((0, true));
    };
    let Some((mtime_ns, size)) = file_meta(&abs) else {
        return Ok((0, false));
    };
    let src = match std::fs::read_to_string(&abs) {
        Ok(s) => s,
        Err(_) => return Ok((0, false)),
    };
    let content_hash: String = {
        let d = Sha256::digest(src.as_bytes());
        d[..16].iter().map(|b| format!("{b:02x}")).collect()
    };

    store.conn.execute("DELETE FROM symbols WHERE file = ?1", [rel])?;
    store.conn.execute("DELETE FROM imports WHERE file = ?1", [rel])?;
    store.conn.execute("DELETE FROM calls WHERE file = ?1", [rel])?;

    let (facts, parse_ok) = match extract::extract(lang_id, &src) {
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
        let body_hash = anchor::ast_body_hash(lang_id, &src, sym.byte_range)
            .unwrap_or_else(|_| String::from("unhashed"));
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
    Ok((count, parse_ok))
}

/// Walk the repository and collect indexable files, honoring .gitignore, an
/// optional `.limpetignore` (gitignore syntax; works even outside a git repo),
/// a built-in directory skip list, a max file size, and a minified-asset skip.
fn discover(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let walker = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_global(false)
        .max_filesize(Some(MAX_FILE_BYTES))
        .add_custom_ignore_filename(".limpetignore")
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            !matches!(
                name.as_ref(),
                "node_modules" | "vendor" | "target" | "dist" | "build" | ".git"
            )
        })
        .build();
    for entry in walker.flatten() {
        let p = entry.path();
        if p.is_file() && lang::detect(p).is_some() {
            let name = entry.file_name().to_string_lossy();
            if is_minified(&name) {
                continue;
            }
            if let Ok(rel) = p.strip_prefix(root) {
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
    for rel in discover(root) {
        match index_file(store, root, &rel) {
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

/// Bounded incremental sweep: detect changed/new/removed files, reindex up
/// to the budget inline, report the rest dirty.
pub fn sweep(store: &Store, root: &Path) -> Result<SweepReport> {
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
                report.removed.push(rel.clone());
            }
        }
    }

    use std::collections::HashSet;
    let known_set: HashSet<&String> = known.iter().map(|(p, _, _)| p).collect();
    for rel in discover(root) {
        if !known_set.contains(&rel) {
            changed.push(rel);
        }
    }

    for rel in changed.iter().take(SWEEP_REINDEX_BUDGET) {
        match index_file(store, root, rel) {
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

        let found = discover(root);
        assert_eq!(found, vec!["keep.php".to_string()], "unexpected: {found:?}");
    }
}
