//! SQLite store: schema, migrations, and JSONL export/import.
//!
//! One database per project, at `$LIMPET_DATA_DIR/<repo_key>/store.db`
//! (default data dir: `~/.local/share/limpet`). WAL mode, NORMAL sync.
//! Every statement in this codebase is parameterized; string-built SQL
//! with user input is forbidden.

use anyhow::{bail, Context, Result};
use rusqlite::Connection;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

pub const SCHEMA_VERSION: i64 = 1;

pub struct Store {
    pub conn: Connection,
}

const SCHEMA_V1: &str = r#"
CREATE TABLE IF NOT EXISTS meta_kv (k TEXT PRIMARY KEY, v TEXT NOT NULL);

CREATE TABLE IF NOT EXISTS files (
  path TEXT PRIMARY KEY,
  lang TEXT,
  mtime_ns INTEGER NOT NULL,
  size INTEGER NOT NULL,
  hash TEXT NOT NULL,
  parse_ok INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE IF NOT EXISTS symbols (
  id INTEGER PRIMARY KEY,
  fqn TEXT NOT NULL,
  name TEXT NOT NULL,
  kind TEXT NOT NULL,
  file TEXT NOT NULL REFERENCES files(path) ON DELETE CASCADE,
  start_line INTEGER,
  end_line INTEGER,
  body_hash TEXT NOT NULL,
  parent_fqn TEXT,
  ordinal INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_symbols_fqn ON symbols(fqn);
CREATE INDEX IF NOT EXISTS idx_symbols_body ON symbols(body_hash);
CREATE INDEX IF NOT EXISTS idx_symbols_file ON symbols(file);

CREATE TABLE IF NOT EXISTS imports (
  file TEXT NOT NULL,
  target TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_imports_file ON imports(file);
CREATE INDEX IF NOT EXISTS idx_imports_target ON imports(target);

CREATE TABLE IF NOT EXISTS calls (
  caller_fqn TEXT NOT NULL,
  callee_name TEXT NOT NULL,
  file TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_calls_file ON calls(file);
CREATE INDEX IF NOT EXISTS idx_calls_callee ON calls(callee_name);

CREATE TABLE IF NOT EXISTS entries (
  id TEXT PRIMARY KEY,
  kind TEXT NOT NULL CHECK(kind IN ('fact','decision','episode','insight','intent')),
  body TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  source TEXT NOT NULL CHECK(source IN ('explicit','mined','verified')),
  confidence REAL NOT NULL,
  status TEXT NOT NULL DEFAULT 'active'
    CHECK(status IN ('active','stale','invalidated','superseded')),
  stale_reason TEXT,
  branch TEXT,
  evidence_cmd TEXT,
  evidence_digest TEXT,
  evidence_ran_at TEXT
);

CREATE TABLE IF NOT EXISTS anchors (
  id INTEGER PRIMARY KEY,
  entry_id TEXT NOT NULL REFERENCES entries(id) ON DELETE CASCADE,
  file TEXT NOT NULL,
  symbol_fqn TEXT,
  ast_body_hash TEXT,
  context_hint TEXT
);
CREATE INDEX IF NOT EXISTS idx_anchors_entry ON anchors(entry_id);
CREATE INDEX IF NOT EXISTS idx_anchors_file ON anchors(file);

CREATE TABLE IF NOT EXISTS links (
  src TEXT NOT NULL REFERENCES entries(id) ON DELETE CASCADE,
  dst TEXT NOT NULL REFERENCES entries(id) ON DELETE CASCADE,
  rel TEXT NOT NULL CHECK(rel IN ('supports','contradicts','supersedes')),
  PRIMARY KEY (src, dst, rel)
);

CREATE VIRTUAL TABLE IF NOT EXISTS entries_fts USING fts5(
  body,
  content=entries,
  content_rowid=rowid,
  tokenize='porter unicode61'
);

CREATE TRIGGER IF NOT EXISTS entries_ai AFTER INSERT ON entries BEGIN
  INSERT INTO entries_fts(rowid, body) VALUES (new.rowid, new.body);
END;
CREATE TRIGGER IF NOT EXISTS entries_ad AFTER DELETE ON entries BEGIN
  INSERT INTO entries_fts(entries_fts, rowid, body) VALUES('delete', old.rowid, old.body);
END;
CREATE TRIGGER IF NOT EXISTS entries_au AFTER UPDATE OF body ON entries BEGIN
  INSERT INTO entries_fts(entries_fts, rowid, body) VALUES('delete', old.rowid, old.body);
  INSERT INTO entries_fts(rowid, body) VALUES (new.rowid, new.body);
END;
"#;

impl Store {
    /// Open (creating if needed) the store at an explicit database path.
    pub fn open(db_path: &Path) -> Result<Store> {
        if let Some(dir) = db_path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("creating store dir {}", dir.display()))?;
        }
        let conn = Connection::open(db_path)
            .with_context(|| format!("opening store {}", db_path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA_V1)?;
        conn.execute(
            "INSERT INTO meta_kv(k, v) VALUES('schema_version', ?1)
             ON CONFLICT(k) DO NOTHING",
            [SCHEMA_VERSION.to_string()],
        )?;
        Ok(Store { conn })
    }

    /// Open an in-memory store (tests).
    pub fn open_in_memory() -> Result<Store> {
        let conn = Connection::open_in_memory()?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA_V1)?;
        Ok(Store { conn })
    }

    /// Default database path for a repository root.
    ///
    /// Resolution order: LIMPET_DATA_DIR override, then the platform's
    /// conventional app-data location (APPDATA on Windows, ~/.local/share
    /// elsewhere), falling back to USERPROFILE when HOME is unset.
    pub fn default_db_path(root: &Path) -> PathBuf {
        let base = std::env::var_os("LIMPET_DATA_DIR")
            .map(PathBuf::from)
            .or_else(|| {
                if cfg!(windows) {
                    std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("limpet"))
                } else {
                    None
                }
            })
            .unwrap_or_else(|| {
                let home = std::env::var_os("HOME")
                    .or_else(|| std::env::var_os("USERPROFILE"))
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("."));
                home.join(".local").join("share").join("limpet")
            });
        base.join(crate::util::repo_key(root)).join("store.db")
    }

    /// Refuse to serve a store that a NEWER limpet has already touched.
    ///
    /// `limpet update` replaces the binary on disk, but a running server
    /// keeps its old code image; old-code writes racing new-code writes on
    /// one store have produced a spurious invalidation (issue #9). Every
    /// tool call and every CLI write path calls this first: it stamps the
    /// store with the running version, and a stale image gets a loud,
    /// self-describing error instead of silently corrupting statuses.
    pub fn version_guard(&self) -> Result<()> {
        let running = env!("CARGO_PKG_VERSION");
        match self.kv_get("code_version")? {
            Some(stamped) if ver_tuple(&stamped) > ver_tuple(running) => bail!(
                "this limpet process is running {running} but the store was \
                 upgraded by limpet {stamped}. Restart the MCP client (or kill \
                 lingering `limpet serve` processes) so a current binary serves \
                 this store."
            ),
            Some(stamped) if stamped == running => Ok(()),
            _ => self.kv_set("code_version", running),
        }
    }

    pub fn kv_get(&self, k: &str) -> Result<Option<String>> {
        let v = self
            .conn
            .query_row("SELECT v FROM meta_kv WHERE k = ?1", [k], |r| r.get(0))
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;
        Ok(v)
    }

    pub fn kv_set(&self, k: &str, v: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta_kv(k, v) VALUES(?1, ?2)
             ON CONFLICT(k) DO UPDATE SET v = excluded.v",
            [k, v],
        )?;
        Ok(())
    }

    /// Export all memory entries (with anchors and links) as JSONL.
    ///
    /// One entry per line, ULID-sorted, stable field order. Text format so
    /// team sharing via git produces reviewable, mergeable diffs.
    pub fn export_jsonl(&self, w: &mut impl Write) -> Result<usize> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, body, created_at, updated_at, source, confidence,
                    status, stale_reason, branch, evidence_cmd, evidence_digest,
                    evidence_ran_at
             FROM entries ORDER BY id",
        )?;
        let ids: Vec<serde_json::Value> = stmt
            .query_map([], |r| {
                Ok(serde_json::json!({
                    "id": r.get::<_, String>(0)?,
                    "kind": r.get::<_, String>(1)?,
                    "body": r.get::<_, String>(2)?,
                    "created_at": r.get::<_, String>(3)?,
                    "updated_at": r.get::<_, String>(4)?,
                    "source": r.get::<_, String>(5)?,
                    "confidence": r.get::<_, f64>(6)?,
                    "status": r.get::<_, String>(7)?,
                    "stale_reason": r.get::<_, Option<String>>(8)?,
                    "branch": r.get::<_, Option<String>>(9)?,
                    "evidence_cmd": r.get::<_, Option<String>>(10)?,
                    "evidence_digest": r.get::<_, Option<String>>(11)?,
                    "evidence_ran_at": r.get::<_, Option<String>>(12)?,
                }))
            })?
            .collect::<rusqlite::Result<_>>()?;

        let mut count = 0usize;
        for mut obj in ids {
            let id = obj["id"].as_str().unwrap_or_default().to_string();
            let mut astmt = self.conn.prepare(
                "SELECT file, symbol_fqn, ast_body_hash, context_hint
                 FROM anchors WHERE entry_id = ?1 ORDER BY id",
            )?;
            let anchors: Vec<serde_json::Value> = astmt
                .query_map([&id], |r| {
                    Ok(serde_json::json!({
                        "file": r.get::<_, String>(0)?,
                        "symbol_fqn": r.get::<_, Option<String>>(1)?,
                        "ast_body_hash": r.get::<_, Option<String>>(2)?,
                        "context_hint": r.get::<_, Option<String>>(3)?,
                    }))
                })?
                .collect::<rusqlite::Result<_>>()?;
            let mut lstmt = self.conn.prepare(
                "SELECT dst, rel FROM links WHERE src = ?1 ORDER BY dst, rel",
            )?;
            let links: Vec<serde_json::Value> = lstmt
                .query_map([&id], |r| {
                    Ok(serde_json::json!({
                        "dst": r.get::<_, String>(0)?,
                        "rel": r.get::<_, String>(1)?,
                    }))
                })?
                .collect::<rusqlite::Result<_>>()?;
            obj["anchors"] = serde_json::Value::Array(anchors);
            obj["links"] = serde_json::Value::Array(links);
            writeln!(w, "{}", serde_json::to_string(&obj)?)?;
            count += 1;
        }
        Ok(count)
    }

    /// Import entries from JSONL produced by `export_jsonl`.
    ///
    /// Reconciles by id: newer `updated_at` wins; identical or older lines
    /// are skipped. Never deletes existing entries (invariant I4).
    pub fn import_jsonl(&mut self, r: &mut impl BufRead) -> Result<ImportReport> {
        let mut report = ImportReport::default();
        let tx = self.conn.transaction()?;
        for line in r.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let obj: serde_json::Value =
                serde_json::from_str(&line).context("malformed JSONL line")?;
            let id = obj["id"].as_str().context("entry missing id")?.to_string();
            let incoming_updated = obj["updated_at"].as_str().unwrap_or_default();

            let existing: Option<String> = match tx.query_row(
                "SELECT updated_at FROM entries WHERE id = ?1",
                [&id],
                |row| row.get(0),
            ) {
                Ok(v) => Some(v),
                Err(rusqlite::Error::QueryReturnedNoRows) => None,
                Err(other) => return Err(other.into()),
            };

            match existing {
                Some(cur) if cur.as_str() >= incoming_updated => {
                    report.skipped += 1;
                    continue;
                }
                Some(_) => report.updated += 1,
                None => report.added += 1,
            }

            tx.execute(
                "INSERT INTO entries(id, kind, body, created_at, updated_at, source,
                                     confidence, status, stale_reason, branch,
                                     evidence_cmd, evidence_digest, evidence_ran_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)
                 ON CONFLICT(id) DO UPDATE SET
                   kind=excluded.kind, body=excluded.body,
                   created_at=excluded.created_at, updated_at=excluded.updated_at,
                   source=excluded.source, confidence=excluded.confidence,
                   status=excluded.status, stale_reason=excluded.stale_reason,
                   branch=excluded.branch, evidence_cmd=excluded.evidence_cmd,
                   evidence_digest=excluded.evidence_digest,
                   evidence_ran_at=excluded.evidence_ran_at",
                rusqlite::params![
                    id,
                    obj["kind"].as_str().unwrap_or("insight"),
                    obj["body"].as_str().unwrap_or_default(),
                    obj["created_at"].as_str().unwrap_or_default(),
                    incoming_updated,
                    obj["source"].as_str().unwrap_or("explicit"),
                    obj["confidence"].as_f64().unwrap_or(0.5),
                    obj["status"].as_str().unwrap_or("active"),
                    obj["stale_reason"].as_str(),
                    obj["branch"].as_str(),
                    obj["evidence_cmd"].as_str(),
                    obj["evidence_digest"].as_str(),
                    obj["evidence_ran_at"].as_str(),
                ],
            )?;
            tx.execute("DELETE FROM anchors WHERE entry_id = ?1", [&id])?;
            if let Some(anchors) = obj["anchors"].as_array() {
                for a in anchors {
                    tx.execute(
                        "INSERT INTO anchors(entry_id, file, symbol_fqn, ast_body_hash, context_hint)
                         VALUES (?1,?2,?3,?4,?5)",
                        rusqlite::params![
                            id,
                            a["file"].as_str().unwrap_or_default(),
                            a["symbol_fqn"].as_str(),
                            a["ast_body_hash"].as_str(),
                            a["context_hint"].as_str(),
                        ],
                    )?;
                }
            }
            if let Some(links) = obj["links"].as_array() {
                for l in links {
                    tx.execute(
                        "INSERT OR IGNORE INTO links(src, dst, rel) VALUES (?1,?2,?3)",
                        rusqlite::params![
                            id,
                            l["dst"].as_str().unwrap_or_default(),
                            l["rel"].as_str().unwrap_or("supports"),
                        ],
                    )?;
                }
            }
        }
        tx.commit()?;
        Ok(report)
    }
}

/// Dotted version to a comparable tuple; missing or non-numeric components
/// sort low, so a malformed stamp can never outrank a real version.
fn ver_tuple(v: &str) -> (u64, u64, u64) {
    let mut it = v
        .trim_start_matches('v')
        .split('.')
        .map(|p| p.parse::<u64>().unwrap_or(0));
    (
        it.next().unwrap_or(0),
        it.next().unwrap_or(0),
        it.next().unwrap_or(0),
    )
}

#[derive(Debug, Default, PartialEq, serde::Serialize)]
pub struct ImportReport {
    pub added: usize,
    pub updated: usize,
    pub skipped: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_creates_all_tables() {
        let s = Store::open_in_memory().unwrap();
        let mut stmt = s
            .conn
            .prepare("SELECT name FROM sqlite_master WHERE type IN ('table','trigger')")
            .unwrap();
        let names: Vec<String> = stmt
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        for t in [
            "meta_kv", "files", "symbols", "imports", "calls", "entries",
            "anchors", "links", "entries_ai", "entries_ad", "entries_au",
        ] {
            assert!(names.iter().any(|n| n == t), "missing {t}: {names:?}");
        }
    }

    #[test]
    fn version_guard_stamps_and_refuses_newer_stores() {
        let s = Store::open_in_memory().unwrap();

        // First contact stamps the running version.
        s.version_guard().unwrap();
        assert_eq!(
            s.kv_get("code_version").unwrap().as_deref(),
            Some(env!("CARGO_PKG_VERSION"))
        );

        // Same version: fine. Older stamp: upgraded in place.
        s.version_guard().unwrap();
        s.kv_set("code_version", "0.1.0").unwrap();
        s.version_guard().unwrap();
        assert_eq!(
            s.kv_get("code_version").unwrap().as_deref(),
            Some(env!("CARGO_PKG_VERSION"))
        );

        // Store touched by a newer limpet: loud refusal naming both versions.
        s.kv_set("code_version", "99.0.0").unwrap();
        let err = s.version_guard().unwrap_err().to_string();
        assert!(err.contains("99.0.0"), "{err}");
        assert!(err.contains(env!("CARGO_PKG_VERSION")), "{err}");
        assert!(err.to_lowercase().contains("restart"), "{err}");

        // Malformed stamp never outranks a real version.
        s.kv_set("code_version", "not-a-version").unwrap();
        s.version_guard().unwrap();
    }

    #[test]
    fn kv_roundtrip() {
        let s = Store::open_in_memory().unwrap();
        s.kv_set("indexed_at", "2026-07-03T00:00:00Z").unwrap();
        assert_eq!(
            s.kv_get("indexed_at").unwrap().as_deref(),
            Some("2026-07-03T00:00:00Z")
        );
        assert_eq!(s.kv_get("nope").unwrap(), None);
    }
}
