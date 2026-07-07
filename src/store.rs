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

pub const SCHEMA_VERSION: i64 = 3;

pub struct Store {
    pub conn: Connection,
    /// This process's session baseline for the savings ledger. In-memory on
    /// purpose: a shared-kv base let one server boot (or a ledger_reset in
    /// another process) corrupt every other process's session view.
    session_base: std::cell::Cell<Ledger>,
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

CREATE TABLE IF NOT EXISTS inherits (
  child_fqn   TEXT NOT NULL,
  parent_name TEXT NOT NULL,
  rel         TEXT NOT NULL CHECK(rel IN ('extends','implements','impl_trait')),
  file        TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_inherits_child  ON inherits(child_fqn);
CREATE INDEX IF NOT EXISTS idx_inherits_parent ON inherits(parent_name);
CREATE INDEX IF NOT EXISTS idx_inherits_file   ON inherits(file);
"#;

/// Schema v2 (lazy migration): `private` marks a memory that must never
/// leave this machine via export; `origin` is a caller-supplied dedup key
/// (the scan flow stamps `scan:git:<sha>` etc.) enforced unique so a
/// re-run cannot double-seed. Each column is guarded independently so a
/// crash between the two ALTER statements leaves the store recoverable on
/// the next open; fresh databases get v1 from the batch above, then
/// arrive here like any old store, so there is exactly one code path.
fn migrate_to_v2(conn: &Connection) -> Result<()> {
    for (col, ddl) in [
        ("private", "ALTER TABLE entries ADD COLUMN private INTEGER NOT NULL DEFAULT 0"),
        ("origin", "ALTER TABLE entries ADD COLUMN origin TEXT"),
    ] {
        let present: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('entries') WHERE name = ?1",
            [col],
            |r| r.get(0),
        )?;
        if present == 0 {
            conn.execute_batch(ddl)?;
        }
    }
    conn.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_entries_origin
         ON entries(origin) WHERE origin IS NOT NULL;",
    )?;
    conn.execute(
        "INSERT INTO meta_kv(k, v) VALUES('schema_version', ?1)
         ON CONFLICT(k) DO UPDATE SET v = excluded.v",
        [SCHEMA_VERSION.to_string()],
    )?;
    Ok(())
}

/// Schema v3 (lazy migration): the additive `inherits` table for the lineage
/// graph. `CREATE TABLE IF NOT EXISTS` is idempotent, so a fresh store (which
/// got the table from SCHEMA_V1 above) and an old v2 store take one code path.
/// No data is touched; the table fills on the next `index`.
fn migrate_to_v3(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS inherits (
           child_fqn   TEXT NOT NULL,
           parent_name TEXT NOT NULL,
           rel         TEXT NOT NULL CHECK(rel IN ('extends','implements','impl_trait')),
           file        TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_inherits_child  ON inherits(child_fqn);
         CREATE INDEX IF NOT EXISTS idx_inherits_parent ON inherits(parent_name);
         CREATE INDEX IF NOT EXISTS idx_inherits_file   ON inherits(file);",
    )?;
    conn.execute(
        "INSERT INTO meta_kv(k, v) VALUES('schema_version', ?1)
         ON CONFLICT(k) DO UPDATE SET v = excluded.v",
        [SCHEMA_VERSION.to_string()],
    )?;
    Ok(())
}

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
        migrate_to_v2(&conn)?;
        migrate_to_v3(&conn)?;
        Ok(Store { conn, session_base: std::cell::Cell::new(Ledger::default()) })
    }

    /// Open an in-memory store (tests).
    pub fn open_in_memory() -> Result<Store> {
        let conn = Connection::open_in_memory()?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA_V1)?;
        migrate_to_v2(&conn)?;
        migrate_to_v3(&conn)?;
        Ok(Store { conn, session_base: std::cell::Cell::new(Ledger::default()) })
    }

    /// Default database path for a repository root.
    ///
    /// Resolution order: LIMPET_DATA_DIR override, then the platform's
    /// conventional app-data location (APPDATA on Windows, ~/.local/share
    /// elsewhere), falling back to USERPROFILE when HOME is unset.
    pub fn default_db_path(root: &Path) -> PathBuf {
        Self::resolve_db_path(&Self::data_base(), root)
    }

    /// The base data directory holding every project's keyed store dir.
    /// Resolution order: LIMPET_DATA_DIR override, then the platform app-data
    /// location (APPDATA on Windows, ~/.local/share elsewhere), falling back
    /// to USERPROFILE when HOME is unset.
    pub(crate) fn data_base() -> PathBuf {
        std::env::var_os("LIMPET_DATA_DIR")
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
            })
    }

    /// Read-only store location for display surfaces (statusline, hook):
    /// prefer an existing store at the new key, fall back to one at the
    /// legacy key, and NEVER migrate, rename, create, or open anything.
    /// Callers treat a missing file as "no store yet".
    pub fn locate_db_path(root: &Path) -> PathBuf {
        Self::locate_db_path_in(&Self::data_base(), root)
    }

    /// `locate_db_path` against an explicit base (tests).
    fn locate_db_path_in(base: &Path, root: &Path) -> PathBuf {
        let new_db = base.join(crate::util::repo_key(root)).join("store.db");
        if new_db.is_file() {
            return new_db;
        }
        let legacy_db = base.join(crate::util::legacy_repo_key(root)).join("store.db");
        if legacy_db.is_file() {
            return legacy_db;
        }
        new_db
    }

    /// Resolve the store db path under `base` for `root`, performing a
    /// one-time migration from the pre-0.9 lossy path-slug key when a legacy
    /// store is found and unambiguously owned by this root.
    fn resolve_db_path(base: &Path, root: &Path) -> PathBuf {
        let new_dir = base.join(crate::util::repo_key(root));
        if !new_dir.exists() {
            Self::migrate_legacy_store(base, root, &new_dir);
        }
        new_dir.join("store.db")
    }

    /// Best-effort migration: if a store exists under the legacy key and its
    /// recorded `project_root` matches this root exactly, atomically rename
    /// its directory to the new key and stamp provenance. A legacy store with
    /// a mismatched or absent owner (a slug collision, or one never indexed)
    /// is left untouched so it is never mis-claimed or lost (invariant I-P1).
    fn migrate_legacy_store(base: &Path, root: &Path, new_dir: &Path) {
        let legacy_dir = base.join(crate::util::legacy_repo_key(root));
        if legacy_dir == *new_dir {
            return;
        }
        let legacy_db = legacy_dir.join("store.db");
        if !legacy_db.exists() || !Self::legacy_store_owns_root(&legacy_db, root) {
            return;
        }
        if std::fs::rename(&legacy_dir, new_dir).is_ok() {
            // The rename succeeded, so this store is ours: reuse the normal
            // open + kv_set path instead of hand-rolling the meta_kv upsert.
            if let Ok(s) = Store::open(&new_dir.join("store.db")) {
                let _ = s.kv_set("legacy_repo_key", &crate::util::legacy_repo_key(root));
                let _ = s.kv_set("identity", &crate::util::repo_identity(root));
            }
        }
    }

    /// True only when the legacy store's recorded `project_root` canonically
    /// equals `root`. Absent ownership returns false: an un-indexed legacy
    /// store is never auto-claimed. The open is `immutable=1`: this is a
    /// probe of a possibly-foreign store, and even a READ_ONLY open of a
    /// WAL database materializes -shm/-wal files in a directory the
    /// migration contract promises never to touch. If the foreign store is
    /// being written at this exact moment an immutable read can misread,
    /// but every failure mode of this probe degrades to "not owned", which
    /// only means no migration happens; it can never mis-claim.
    fn legacy_store_owns_root(legacy_db: &Path, root: &Path) -> bool {
        // SQLite URI: percent-encode the characters that would terminate or
        // corrupt the URI, and use `/` separators on Windows.
        let uri_path = legacy_db
            .to_string_lossy()
            .replace('%', "%25")
            .replace('?', "%3F")
            .replace('#', "%23")
            .replace('\\', "/");
        let Ok(conn) = Connection::open_with_flags(
            format!("file:{uri_path}?immutable=1"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                | rusqlite::OpenFlags::SQLITE_OPEN_URI
                | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        ) else {
            return false;
        };
        let stored: Option<String> = conn
            .query_row("SELECT v FROM meta_kv WHERE k='project_root'", [], |r| r.get(0))
            .ok();
        match stored {
            Some(p) => {
                let a = crate::util::canonicalize_plain(Path::new(&p))
                    .unwrap_or_else(|_| PathBuf::from(&p));
                let b =
                    crate::util::canonicalize_plain(root).unwrap_or_else(|_| root.to_path_buf());
                a == b
            }
            None => false,
        }
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
        // Compare-and-stamp atomically: without the IMMEDIATE transaction a
        // stale image could read the old stamp in the window before a newer
        // binary writes its own, and proceed to write (audit 2026-07).
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        let outcome = (|| -> Result<()> {
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
        })();
        match outcome {
            Ok(()) => {
                self.conn.execute_batch("COMMIT")?;
                Ok(())
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(e)
            }
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
    pub fn export_jsonl(&self, w: &mut impl Write) -> Result<ExportReport> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, body, created_at, updated_at, source, confidence,
                    status, stale_reason, branch, evidence_cmd, evidence_digest,
                    evidence_ran_at, origin
             FROM entries WHERE private = 0 ORDER BY id",
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
                    "origin": r.get::<_, Option<String>>(13)?,
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
        let private_withheld: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM entries WHERE private = 1", [], |r| r.get(0))?;
        Ok(ExportReport { exported: count, private_withheld: private_withheld as usize })
    }

    /// Import entries from JSONL produced by `export_jsonl`.
    ///
    /// Reconciles by id: newer `updated_at` wins; identical or older lines
    /// are skipped. Never deletes existing entries (invariant I4). Lines that
    /// fail a guard are counted in `rejected`, never partially applied.
    ///
    /// A `.limpet/memory.jsonl` arrives over `git pull` from a teammate and
    /// is UNTRUSTED. Import therefore enforces the same guards the live
    /// `remember` path does, because it is a second write path into the same
    /// store: secrets are rejected (they must never enter the store, even
    /// from a peer), bodies are size-capped, confidence is clamped, future
    /// `updated_at` timestamps cannot win the merge forever, imported anchor
    /// hashes are re-resolved against the LOCAL index (a forged hash cannot
    /// fake freshness against code you do not have), and each line is read
    /// with a hard size cap.
    pub fn import_jsonl(&mut self, r: &mut impl BufRead) -> Result<ImportReport> {
        let mut report = ImportReport::default();
        let now = crate::index::now_iso();
        let tx = self.conn.transaction()?;
        // Bounded line reads: one multi-GB line must not exhaust memory.
        const MAX_IMPORT_LINE: u64 = 1024 * 1024;
        let mut raw: Vec<u8> = Vec::new();
        loop {
            raw.clear();
            let n = std::io::Read::take(r.by_ref(), MAX_IMPORT_LINE + 1)
                .read_until(b'\n', &mut raw)?;
            if n == 0 {
                break;
            }
            if n as u64 > MAX_IMPORT_LINE {
                bail!("import line exceeds {MAX_IMPORT_LINE} bytes; refusing a malformed or hostile export");
            }
            let line = String::from_utf8_lossy(&raw);
            if line.trim().is_empty() {
                continue;
            }
            let obj: serde_json::Value =
                serde_json::from_str(&line).context("malformed JSONL line")?;
            let id = obj["id"].as_str().context("entry missing id")?.to_string();
            let incoming_updated = obj["updated_at"].as_str().unwrap_or_default();

            // A future timestamp would win the LWW merge against every honest
            // later update forever (denial-of-correction). Treat future or
            // unparseable stamps as lowest priority.
            if let Some(secs) = crate::memory::parse_iso_secs(incoming_updated) {
                if let Some(now_secs) = crate::memory::parse_iso_secs(&now) {
                    if secs > now_secs {
                        report.rejected += 1;
                        continue;
                    }
                }
            } else {
                report.rejected += 1;
                continue;
            }

            // Secrets must never enter the store, not even from a peer.
            let body = obj["body"].as_str().unwrap_or_default();
            let ev_cmd = obj["evidence_cmd"].as_str().unwrap_or_default();
            if crate::secrets::detect(body).is_some()
                || crate::secrets::detect(ev_cmd).is_some()
            {
                report.rejected += 1;
                continue;
            }
            if body.len() > crate::memory::MAX_BODY_BYTES {
                report.rejected += 1;
                continue;
            }

            // An origin names ONE memory. A different id claiming an existing
            // origin would let a hostile export overwrite the dedup key space.
            let origin = obj["origin"].as_str();
            if let Some(o) = origin {
                if o.len() > 256 || crate::secrets::detect(o).is_some() {
                    report.rejected += 1;
                    continue;
                }
                let clash: Option<String> = tx
                    .query_row(
                        "SELECT id FROM entries WHERE origin = ?1 AND id != ?2",
                        rusqlite::params![o, id],
                        |r| r.get(0),
                    )
                    .ok();
                if clash.is_some() {
                    report.rejected += 1;
                    continue;
                }
            }

            let existing: Option<String> = match tx.query_row(
                "SELECT updated_at FROM entries WHERE id = ?1",
                [&id],
                |row| row.get(0),
            ) {
                Ok(v) => Some(v),
                Err(rusqlite::Error::QueryReturnedNoRows) => None,
                Err(other) => return Err(other.into()),
            };

            let entry_skipped = match existing {
                Some(cur) if cur.as_str() >= incoming_updated => {
                    report.skipped += 1;
                    true
                }
                Some(_) => {
                    report.updated += 1;
                    false
                }
                None => {
                    report.added += 1;
                    false
                }
            };
            if entry_skipped {
                // The entry body is not newer, but links merge regardless:
                // add_link never bumps updated_at, so link-only changes would
                // otherwise never propagate between machines.
                if let Some(links) = obj["links"].as_array() {
                    for l in links {
                        let dst = l["dst"].as_str().unwrap_or_default();
                        let dst_exists: bool = tx.query_row(
                            "SELECT EXISTS(SELECT 1 FROM entries WHERE id = ?1)",
                            [dst],
                            |r| r.get(0),
                        )?;
                        if !dst_exists {
                            report.links_dropped += 1;
                            continue;
                        }
                        tx.execute(
                            "INSERT OR IGNORE INTO links(src, dst, rel) VALUES (?1,?2,?3)",
                            rusqlite::params![id, dst, l["rel"].as_str().unwrap_or("supports")],
                        )?;
                    }
                }
                continue;
            }

            tx.execute(
                "INSERT INTO entries(id, kind, body, created_at, updated_at, source,
                                     confidence, status, stale_reason, branch,
                                     evidence_cmd, evidence_digest, evidence_ran_at, origin)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)
                 ON CONFLICT(id) DO UPDATE SET
                   kind=excluded.kind, body=excluded.body,
                   created_at=excluded.created_at, updated_at=excluded.updated_at,
                   source=excluded.source, confidence=excluded.confidence,
                   status=excluded.status, stale_reason=excluded.stale_reason,
                   branch=excluded.branch, evidence_cmd=excluded.evidence_cmd,
                   evidence_digest=excluded.evidence_digest,
                   evidence_ran_at=excluded.evidence_ran_at,
                   origin=COALESCE(excluded.origin, origin)",
                rusqlite::params![
                    id,
                    obj["kind"].as_str().unwrap_or("insight"),
                    obj["body"].as_str().unwrap_or_default(),
                    obj["created_at"].as_str().unwrap_or_default(),
                    incoming_updated,
                    obj["source"].as_str().unwrap_or("explicit"),
                    // Clamp: an imported 1e300 would pin a hostile memory to
                    // the top of every recall (schema has no CHECK on this).
                    crate::memory::quantize_confidence(obj["confidence"].as_f64().unwrap_or(0.5)),
                    obj["status"].as_str().unwrap_or("active"),
                    obj["stale_reason"].as_str(),
                    obj["branch"].as_str(),
                    obj["evidence_cmd"].as_str(),
                    obj["evidence_digest"].as_str(),
                    obj["evidence_ran_at"].as_str(),
                    origin,
                ],
            )?;
            tx.execute("DELETE FROM anchors WHERE entry_id = ?1", [&id])?;
            if let Some(anchors) = obj["anchors"].as_array() {
                for a in anchors {
                    let file = a["file"].as_str().unwrap_or_default();
                    let symbol_fqn = a["symbol_fqn"].as_str();
                    // Re-resolve the hash against the LOCAL index rather than
                    // trusting the imported one: a forged hash must not be
                    // able to fake "fresh" against code this machine does not
                    // have. If the anchored symbol/file exists here, adopt the
                    // local hash (honestly fresh); otherwise keep the imported
                    // hash so normal follow/invalidate logic applies.
                    let local_hash: Option<String> = match symbol_fqn {
                        Some(fqn) => tx
                            .query_row(
                                "SELECT body_hash FROM symbols WHERE fqn = ?1 LIMIT 1",
                                [fqn],
                                |r| r.get(0),
                            )
                            .ok(),
                        None => tx
                            .query_row("SELECT hash FROM files WHERE path = ?1", [file], |r| {
                                r.get(0)
                            })
                            .ok(),
                    };
                    let hash = local_hash.or_else(|| a["ast_body_hash"].as_str().map(str::to_string));
                    tx.execute(
                        "INSERT INTO anchors(entry_id, file, symbol_fqn, ast_body_hash, context_hint)
                         VALUES (?1,?2,?3,?4,?5)",
                        rusqlite::params![id, file, symbol_fqn, hash, a["context_hint"].as_str()],
                    )?;
                }
            }
            if let Some(links) = obj["links"].as_array() {
                for l in links {
                    let dst = l["dst"].as_str().unwrap_or_default();
                    // INSERT OR IGNORE would swallow an FK violation as a
                    // silent drop; check and count instead (honesty applies
                    // to imports too).
                    let dst_exists: bool = tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM entries WHERE id = ?1)",
                        [dst],
                        |r| r.get(0),
                    )?;
                    if !dst_exists {
                        report.links_dropped += 1;
                        continue;
                    }
                    tx.execute(
                        "INSERT OR IGNORE INTO links(src, dst, rel) VALUES (?1,?2,?3)",
                        rusqlite::params![id, dst, l["rel"].as_str().unwrap_or("supports")],
                    )?;
                }
            }
        }
        tx.commit()?;
        Ok(report)
    }
}

/// Lifetime savings counters (spec v0.7.0). Purely observational (I-L5):
/// stored as decimal strings in meta_kv, missing keys read as zero, and a
/// bug here can misreport a number but never touch memory content.
#[derive(Debug, Default, Clone, Copy, PartialEq, serde::Serialize)]
pub struct Ledger {
    pub recalls: i64,
    pub distinct_queries: i64,
    pub served: i64,
    pub baseline: i64,
    pub reads_avoided: i64,
}

impl Ledger {
    pub fn saved(&self) -> i64 {
        // Never floored (I-L2): a pack that cost more than the files it
        // replaced reports a real negative.
        self.baseline - self.served
    }

    pub fn diff(&self, base: &Ledger) -> Ledger {
        Ledger {
            recalls: self.recalls - base.recalls,
            distinct_queries: self.distinct_queries - base.distinct_queries,
            served: self.served - base.served,
            baseline: self.baseline - base.baseline,
            reads_avoided: self.reads_avoided - base.reads_avoided,
        }
    }
}

const LEDGER_KEYS: [&str; 5] = [
    "ledger.recalls",
    "ledger.distinct_queries",
    "ledger.served",
    "ledger.baseline",
    "ledger.reads_avoided",
];

impl Store {
    fn kv_i64(&self, k: &str) -> i64 {
        self.kv_get(k)
            .ok()
            .flatten()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    }

    pub fn ledger_read(&self) -> Ledger {
        Ledger {
            recalls: self.kv_i64(LEDGER_KEYS[0]),
            distinct_queries: self.kv_i64(LEDGER_KEYS[1]),
            served: self.kv_i64(LEDGER_KEYS[2]),
            baseline: self.kv_i64(LEDGER_KEYS[3]),
            reads_avoided: self.kv_i64(LEDGER_KEYS[4]),
        }
    }

    /// Accumulate one recall's figures. `query_hash` marks the query as
    /// seen (lifetime): first sighting bumps distinct_queries. The whole
    /// read-modify-write runs in one IMMEDIATE transaction so concurrent
    /// serve processes can neither lose updates nor tear the counters.
    pub fn ledger_add(
        &self,
        served: i64,
        baseline: i64,
        reads_avoided: i64,
        query_hash: &str,
    ) -> Result<()> {
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        let outcome = (|| -> Result<()> {
            let cur = self.ledger_read();
            let seen_key = format!("ledger.q.{query_hash}");
            let newly_seen = self
                .conn
                .execute(
                    "INSERT OR IGNORE INTO meta_kv(k, v) VALUES(?1, '1')",
                    [&seen_key],
                )?
                > 0;
            self.kv_set(LEDGER_KEYS[0], &(cur.recalls + 1).to_string())?;
            if newly_seen {
                self.kv_set(LEDGER_KEYS[1], &(cur.distinct_queries + 1).to_string())?;
            }
            self.kv_set(LEDGER_KEYS[2], &(cur.served + served).to_string())?;
            self.kv_set(LEDGER_KEYS[3], &(cur.baseline + baseline).to_string())?;
            self.kv_set(LEDGER_KEYS[4], &(cur.reads_avoided + reads_avoided).to_string())?;
            if self.kv_get("ledger.since")?.is_none() {
                self.kv_set("ledger.since", &crate::index::now_iso())?;
            }
            Ok(())
        })();
        match outcome {
            Ok(()) => {
                self.conn.execute_batch("COMMIT")?;
                Ok(())
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    pub fn ledger_since(&self) -> Option<String> {
        self.kv_get("ledger.since").ok().flatten()
    }

    pub fn ledger_reset(&self) -> Result<()> {
        self.conn
            .execute("DELETE FROM meta_kv WHERE k LIKE 'ledger.%'", [])?;
        Ok(())
    }

    /// Snapshot the current lifetime figures as this process's session base.
    /// Session view = lifetime - base. In-memory: shared-kv storage let one
    /// server boot clobber every other process's session view.
    pub fn ledger_session_start(&self) -> Result<()> {
        self.session_base.set(self.ledger_read());
        Ok(())
    }

    pub fn ledger_session_base(&self) -> Ledger {
        self.session_base.get()
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
pub struct ExportReport {
    pub exported: usize,
    /// Private memories deliberately withheld from the shared file. Reported
    /// so "1 exported" next to "12 in the store" is explainable, not spooky.
    pub private_withheld: usize,
}

#[derive(Debug, Default, PartialEq, serde::Serialize)]
pub struct ImportReport {
    pub added: usize,
    pub updated: usize,
    pub skipped: usize,
    /// Links whose target entry does not exist locally: reported, not
    /// silently swallowed by INSERT OR IGNORE.
    pub links_dropped: usize,
    /// Untrusted lines refused: a secret in the body/evidence, an
    /// over-cap body, or a future-dated timestamp. Reported, never applied.
    pub rejected: usize,
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
    fn ledger_accumulates_diffs_and_resets() {
        let s = Store::open_in_memory().unwrap();
        assert_eq!(s.ledger_read(), Ledger::default());

        s.ledger_add(100, 700, 2, "q1").unwrap();
        s.ledger_add(50, 350, 1, "q1").unwrap(); // repeat query
        s.ledger_add(80, 80, 0, "q2").unwrap(); // zero-saving recall
        let l = s.ledger_read();
        assert_eq!(l.recalls, 3);
        assert_eq!(l.distinct_queries, 2, "repeat query counts once");
        assert_eq!(l.served, 230);
        assert_eq!(l.baseline, 1130);
        assert_eq!(l.saved(), 900);
        assert_eq!(l.reads_avoided, 3);
        assert!(s.ledger_since().is_some());

        // Session view = lifetime minus the boot stamp.
        s.ledger_session_start().unwrap();
        s.ledger_add(10, 500, 1, "q3").unwrap();
        let session = s.ledger_read().diff(&s.ledger_session_base());
        assert_eq!(session.recalls, 1);
        assert_eq!(session.saved(), 490);

        // Negative savings survive (I-L2).
        s.ledger_reset().unwrap();
        assert_eq!(s.ledger_read(), Ledger::default());
        assert!(s.ledger_since().is_none(), "reset restamps since lazily");
        s.ledger_add(1000, 300, 0, "q4").unwrap();
        assert_eq!(s.ledger_read().saved(), -700);
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

    #[test]
    fn schema_v2_adds_private_and_origin() {
        let s = Store::open_in_memory().unwrap();
        let cols: Vec<String> = s
            .conn
            .prepare("SELECT name FROM pragma_table_info('entries')")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(cols.iter().any(|c| c == "private"), "missing private: {cols:?}");
        assert!(cols.iter().any(|c| c == "origin"), "missing origin: {cols:?}");

        // Partial unique index: two NULL origins fine, two equal origins refused.
        s.conn
            .execute(
                "INSERT INTO entries(id, kind, body, created_at, updated_at, source, confidence, origin)
                 VALUES ('a','fact','x','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z','explicit',0.8,'scan:git:abc')",
                [],
            )
            .unwrap();
        let dup = s.conn.execute(
            "INSERT INTO entries(id, kind, body, created_at, updated_at, source, confidence, origin)
             VALUES ('b','fact','y','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z','explicit',0.8,'scan:git:abc')",
            [],
        );
        assert!(dup.is_err(), "duplicate origin must violate idx_entries_origin");
    }

    #[test]
    fn half_migrated_store_survives_open() {
        // Simulate a crash that added `private` but not `origin` (the old
        // single-ALTER-batch failure window). Store::open must recover and
        // produce a store with both columns present.
        let dir = tempfile::TempDir::new().unwrap();
        let db = dir.path().join("store.db");
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute_batch(SCHEMA_V1).unwrap();
            // Only the first ALTER — simulates crash before `origin` was added.
            conn.execute_batch(
                "ALTER TABLE entries ADD COLUMN private INTEGER NOT NULL DEFAULT 0",
            )
            .unwrap();
        }
        let s = Store::open(&db).unwrap();
        let cols: Vec<String> = s
            .conn
            .prepare("SELECT name FROM pragma_table_info('entries')")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(cols.iter().any(|c| c == "private"), "private must be present: {cols:?}");
        assert!(cols.iter().any(|c| c == "origin"), "origin must be added on recovery: {cols:?}");
    }

    #[test]
    fn v1_store_migrates_in_place() {
        // Simulate a database created by a 0.7.x binary: raw v1 schema only.
        let dir = tempfile::TempDir::new().unwrap();
        let db = dir.path().join("store.db");
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute_batch(SCHEMA_V1).unwrap();
            conn.execute(
                "INSERT INTO entries(id, kind, body, created_at, updated_at, source, confidence)
                 VALUES ('old1','fact','pre-migration entry','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z','explicit',0.8)",
                [],
            )
            .unwrap();
        }
        let s = Store::open(&db).unwrap();
        let (private, origin): (i64, Option<String>) = s
            .conn
            .query_row("SELECT private, origin FROM entries WHERE id='old1'", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(private, 0, "pre-existing rows default to not-private");
        assert_eq!(origin, None);
        assert_eq!(s.kv_get("schema_version").unwrap().as_deref(), Some("3"));
    }

    #[test]
    fn migrates_legacy_store_when_project_root_matches() {
        let base = tempfile::TempDir::new().unwrap();
        let root = tempfile::TempDir::new().unwrap();
        // A legacy-key store that was indexed for `root`.
        let legacy_dir = base.path().join(crate::util::legacy_repo_key(root.path()));
        std::fs::create_dir_all(&legacy_dir).unwrap();
        {
            let s = Store::open(&legacy_dir.join("store.db")).unwrap();
            s.kv_set("project_root", &root.path().to_string_lossy()).unwrap();
            s.kv_set("marker", "legacy-data").unwrap();
        }
        let db = Store::resolve_db_path(base.path(), root.path());
        let new_dir = base.path().join(crate::util::repo_key(root.path()));
        assert_eq!(db, new_dir.join("store.db"));
        assert!(db.exists(), "migrated store must exist at the new key");
        assert!(!legacy_dir.exists(), "legacy dir must be renamed away");
        let s = Store::open(&db).unwrap();
        assert_eq!(s.kv_get("marker").unwrap().as_deref(), Some("legacy-data"));
        assert_eq!(
            s.kv_get("legacy_repo_key").unwrap().as_deref(),
            Some(crate::util::legacy_repo_key(root.path()).as_str()),
            "migration should stamp provenance",
        );
    }

    #[test]
    fn ownership_probe_leaves_foreign_store_untouched() {
        // The probe asks who owns a possibly-foreign store; it must be
        // strictly read-only. A read-write SQLite open can materialize
        // -wal/-shm files or roll back a journal in a store dir we have
        // promised never to touch.
        let base = tempfile::TempDir::new().unwrap();
        let root = tempfile::TempDir::new().unwrap();
        let other = tempfile::TempDir::new().unwrap();
        let legacy_dir = base.path().join(crate::util::legacy_repo_key(root.path()));
        std::fs::create_dir_all(&legacy_dir).unwrap();
        {
            let s = Store::open(&legacy_dir.join("store.db")).unwrap();
            s.kv_set("project_root", &other.path().to_string_lossy()).unwrap();
        }
        let before: Vec<String> = std::fs::read_dir(&legacy_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        let _ = Store::resolve_db_path(base.path(), root.path());
        let after: Vec<String> = std::fs::read_dir(&legacy_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        let mut b = before.clone();
        let mut a = after.clone();
        b.sort();
        a.sort();
        assert_eq!(a, b, "probing ownership must not add or remove files in a foreign store dir");
    }

    #[test]
    fn locate_db_path_is_read_only_and_finds_legacy_store() {
        // Display surfaces (statusline, hook) resolve the store WITHOUT
        // migrating: a legacy-keyed store is found in place and no rename
        // happens.
        let base = tempfile::TempDir::new().unwrap();
        let root = tempfile::TempDir::new().unwrap();
        let legacy_dir = base.path().join(crate::util::legacy_repo_key(root.path()));
        std::fs::create_dir_all(&legacy_dir).unwrap();
        {
            let s = Store::open(&legacy_dir.join("store.db")).unwrap();
            s.kv_set("project_root", &root.path().to_string_lossy()).unwrap();
        }
        let db = Store::locate_db_path_in(base.path(), root.path());
        assert_eq!(db, legacy_dir.join("store.db"), "locate must find the legacy store in place");
        assert!(legacy_dir.exists(), "locate must never rename or migrate");
    }

    #[test]
    fn v2_store_migrates_to_v3_inherits() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("store.db");
        // Build a v1+v2 store WITHOUT the inherits table, like an old on-disk store.
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(SCHEMA_V1).unwrap();
            migrate_to_v2(&conn).unwrap();
        }
        // Opening through Store must add inherits and stamp schema_version=3.
        let store = Store::open(&db).unwrap();
        let cols: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('inherits')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(cols, 4, "inherits has child_fqn, parent_name, rel, file");
        let ver: String = store
            .conn
            .query_row("SELECT v FROM meta_kv WHERE k='schema_version'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(ver, "3");
        // Insert + read back an edge (CHECK constraint honored).
        store
            .conn
            .execute(
                "INSERT INTO inherits(child_fqn,parent_name,rel,file) VALUES('a.B','Base','extends','a.rs')",
                [],
            )
            .unwrap();
        let n: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM inherits WHERE parent_name='Base'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn does_not_claim_legacy_store_owned_by_another_root() {
        let base = tempfile::TempDir::new().unwrap();
        let root = tempfile::TempDir::new().unwrap();
        let other = tempfile::TempDir::new().unwrap();
        // A collided legacy key: the store belongs to a DIFFERENT root.
        let legacy_dir = base.path().join(crate::util::legacy_repo_key(root.path()));
        std::fs::create_dir_all(&legacy_dir).unwrap();
        {
            let s = Store::open(&legacy_dir.join("store.db")).unwrap();
            s.kv_set("project_root", &other.path().to_string_lossy()).unwrap();
            s.kv_set("marker", "not-yours").unwrap();
        }
        let _db = Store::resolve_db_path(base.path(), root.path());
        assert!(legacy_dir.exists(), "an unclaimed legacy store must be preserved");
        let s = Store::open(&_db).unwrap();
        assert_eq!(
            s.kv_get("marker").unwrap(),
            None,
            "must not inherit another repo's memory on a key collision",
        );
    }
}
