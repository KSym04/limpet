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

pub const SCHEMA_VERSION: i64 = 8;

pub struct Store {
    pub conn: Connection,
    /// What THIS process actually served: an accumulator `ledger_add`
    /// increments with the same committed figures it writes to meta_kv,
    /// never a view of those shared counters. A boot snapshot cannot carry
    /// the claim, because the ledger.* rows are cross-process: subtracting
    /// one from lifetime also collects every recall a SECOND `limpet serve`
    /// (another project window, another editor) served afterwards, and a
    /// `ledger_reset` in that process drives the subtraction negative.
    /// Counting our own calls is exact by construction and cannot go
    /// below zero.
    session: std::cell::Cell<Ledger>,
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
  body_len INTEGER,
  parent_fqn TEXT,
  ordinal INTEGER NOT NULL DEFAULT 0,
  disamb TEXT
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
  evidence_ran_at TEXT,
  conf_before_stale REAL
);

CREATE TABLE IF NOT EXISTS anchors (
  id INTEGER PRIMARY KEY,
  entry_id TEXT NOT NULL REFERENCES entries(id) ON DELETE CASCADE,
  file TEXT NOT NULL,
  symbol_fqn TEXT,
  ast_body_hash TEXT,
  context_hint TEXT,
  disamb TEXT
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
  rel         TEXT NOT NULL
    CHECK(rel IN ('extends','implements','impl_trait','embeds','mixin')),
  file        TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_inherits_child  ON inherits(child_fqn);
CREATE INDEX IF NOT EXISTS idx_inherits_parent ON inherits(parent_name);
CREATE INDEX IF NOT EXISTS idx_inherits_file   ON inherits(file);

-- Archival sidecar (schema v6): a row here hides the entry from recall, the
-- verify queue, and map, without touching its status. See migrate_to_v6.
CREATE TABLE IF NOT EXISTS archived (
  entry_id    TEXT PRIMARY KEY,
  archived_at TEXT NOT NULL
);
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

/// Schema v4 (lazy migration): widen `inherits.rel` to admit `embeds` (Go
/// embedding) and `mixin` (Ruby include/prepend/extend). SQLite cannot ALTER a
/// CHECK in place, but `inherits` is fully derived and repopulated per file on
/// index, so we drop and recreate it with the wider CHECK; content refills on
/// the next `index`.
///
/// IMPORTANT: the DROP must happen ONLY when the table still carries the old
/// narrow CHECK. An unconditional DROP would wipe freshly-indexed edges every
/// time a second process (e.g. `limpet serve`) opens the store, because all
/// migrations run on every open. We gate the rebuild on whether the stored
/// CREATE SQL already contains `'embeds'`: present means already widened,
/// absent (or table missing) means the one-time rebuild is needed.
fn migrate_to_v4(conn: &Connection) -> Result<()> {
    // Only rebuild inherits when it still carries the OLD narrow CHECK.
    // The table is derived (refilled per file on index), but the rebuild must
    // NOT run on every open: an unconditional DROP would wipe the freshly
    // indexed edges each time a second process (e.g. `serve`) opens the store.
    use rusqlite::OptionalExtension;
    let existing_sql: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='inherits'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    let needs_widen = existing_sql
        .as_deref()
        .is_none_or(|s| !s.contains("'embeds'"));
    if needs_widen {
        conn.execute_batch(
            "DROP TABLE IF EXISTS inherits;
             CREATE TABLE inherits (
               child_fqn   TEXT NOT NULL,
               parent_name TEXT NOT NULL,
               rel         TEXT NOT NULL
                 CHECK(rel IN ('extends','implements','impl_trait','embeds','mixin')),
               file        TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_inherits_child  ON inherits(child_fqn);
             CREATE INDEX IF NOT EXISTS idx_inherits_parent ON inherits(parent_name);
             CREATE INDEX IF NOT EXISTS idx_inherits_file   ON inherits(file);",
        )?;
        // The rebuild emptied a table the sweep will not refill on its own
        // (unchanged files are never re-parsed). Zero every known mtime so the
        // bounded sweep sees each file as changed and repopulates inherits
        // incrementally; rows (and thus symbols and anchors) are untouched.
        conn.execute("UPDATE files SET mtime_ns = 0", [])?;
    }
    conn.execute(
        "INSERT INTO meta_kv(k, v) VALUES('schema_version', ?1)
         ON CONFLICT(k) DO UPDATE SET v = excluded.v",
        [SCHEMA_VERSION.to_string()],
    )?;
    Ok(())
}

/// Schema v5 (lazy migration): additive `symbols.body_len`: the byte length
/// of the normalization buffer behind `body_hash`, the entropy signal for the
/// low-entropy follow guard. Gate on the ACTUAL table state (pragma), never
/// the version stamp: migrations run on every open and must no-op on re-run.
/// The one-time branch also zeroes files.mtime_ns so the bounded sweep
/// re-parses every file and fills the column; without it, unchanged files
/// keep NULL forever (the v4 lesson).
fn migrate_to_v5(conn: &Connection) -> Result<()> {
    let present: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('symbols') WHERE name = 'body_len'",
        [],
        |r| r.get(0),
    )?;
    if present == 0 {
        conn.execute_batch("ALTER TABLE symbols ADD COLUMN body_len INTEGER")?;
        conn.execute("UPDATE files SET mtime_ns = 0", [])?;
    }
    conn.execute(
        "INSERT INTO meta_kv(k, v) VALUES('schema_version', ?1)
         ON CONFLICT(k) DO UPDATE SET v = excluded.v",
        [SCHEMA_VERSION.to_string()],
    )?;
    Ok(())
}

/// Schema v6 (lazy migration): the additive `archived` sidecar table.
/// Archival is deliberately NOT a status value: admitting 'archived' into the
/// entries.status CHECK would force a rebuild of the core table, and status
/// must keep tracking reality (stale/heal) while an entry is shelved. A row
/// here means "hidden from recall, the verify queue, and map"; deleting the
/// row restores the entry with its CURRENT, truthful status. Same idempotent
/// CREATE TABLE IF NOT EXISTS pattern as v3; no data is touched.
fn migrate_to_v6(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS archived (
           entry_id    TEXT PRIMARY KEY,
           archived_at TEXT NOT NULL
         );",
    )?;
    conn.execute(
        "INSERT INTO meta_kv(k, v) VALUES('schema_version', ?1)
         ON CONFLICT(k) DO UPDATE SET v = excluded.v",
        [SCHEMA_VERSION.to_string()],
    )?;
    Ok(())
}

/// Schema v8 (lazy migration): additive `entries.conf_before_stale`, the
/// pre-penalty confidence stored on the active->stale transition and
/// refunded on heal or reverify (decay once per reason, refunded when the
/// reason evaporates). Pragma self-gated like v5/v7; no derived data, so no
/// refill and no mtime zeroing.
fn migrate_to_v8(conn: &Connection) -> Result<()> {
    let present: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('entries') WHERE name = 'conf_before_stale'",
        [],
        |r| r.get(0),
    )?;
    if present == 0 {
        add_column_tolerating_race(conn, "ALTER TABLE entries ADD COLUMN conf_before_stale REAL")?;
    }
    conn.execute(
        "INSERT INTO meta_kv(k, v) VALUES('schema_version', ?1)
         ON CONFLICT(k) DO UPDATE SET v = excluded.v",
        [SCHEMA_VERSION.to_string()],
    )?;
    Ok(())
}

/// Attempt an ADD COLUMN, tolerating exactly the multi-process race: two
/// processes can both pass the pragma gate, SQLite serializes the ALTERs, and
/// the loser fails with "duplicate column name". That failure means the column
/// exists (already migrated) and maps to Ok(false); any other error
/// propagates. Returns whether THIS call actually added the column, so the
/// caller can run one-time side effects exactly once across processes.
fn add_column_tolerating_race(conn: &Connection, ddl: &str) -> Result<bool> {
    match conn.execute_batch(ddl) {
        Ok(()) => Ok(true),
        Err(e) if e.to_string().contains("duplicate column name") => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// Schema v7 (lazy migration): additive `symbols.disamb` and `anchors.disamb`
/// discriminator columns (trait name, receiver type, parameter list; NULL when
/// no discriminator applies). No UNIQUE, no index: true duplicates stay legal
/// rows and uniqueness is delivered at resolution. Each ALTER gates on its OWN
/// table's ACTUAL state (pragma_table_info, the v5 precedent), never the
/// version stamp: migrations run on every open and must no-op on re-run.
/// This migration only adds the columns; the mtime-zeroing that refills them
/// lives in `refill_v7_content_once` below, gated on its own marker, so ONE
/// mechanism covers both a fresh v6 -> v7 migration and a store that already
/// reached v7 under an earlier build (the stamp cannot tell those apart). The
/// symbols ALTER still runs inside a transaction so a racing loser rolls back
/// cleanly; the anchors ALTER is a single atomic statement and needs none.
fn migrate_to_v7(conn: &Connection) -> Result<()> {
    let present: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('symbols') WHERE name = 'disamb'",
        [],
        |r| r.get(0),
    )?;
    if present == 0 {
        let tx = conn.unchecked_transaction()?;
        if add_column_tolerating_race(&tx, "ALTER TABLE symbols ADD COLUMN disamb TEXT")? {
            tx.commit()?;
        } else {
            tx.rollback()?;
        }
    }
    let present: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('anchors') WHERE name = 'disamb'",
        [],
        |r| r.get(0),
    )?;
    if present == 0 {
        add_column_tolerating_race(conn, "ALTER TABLE anchors ADD COLUMN disamb TEXT")?;
    }
    conn.execute(
        "INSERT INTO meta_kv(k, v) VALUES('schema_version', ?1)
         ON CONFLICT(k) DO UPDATE SET v = excluded.v",
        [SCHEMA_VERSION.to_string()],
    )?;
    Ok(())
}

/// The one-time v7 content refill: zero every `files.mtime_ns` so the bounded
/// sweep re-parses the whole repo and fills the new columns.
///
/// This is the SOLE refill mechanism for v7 (`migrate_to_v7` only adds
/// columns). It must cover two stores the version stamp cannot tell apart:
/// one migrating v6 -> v7 right now, and one that reached v7 under an earlier
/// mid-development build and therefore keeps rows the shipping extractor
/// would spell differently: FQNs missing the scope segments 0.15 added
/// (C# block namespace, Java enum/record, PHP enum, TS namespace), `disamb`
/// values from before the discriminator covered accessors and generic arity,
/// and `calls.caller_fqn` values from before it carried the caller's full
/// scope. Without the refill those rows heal only file by file as files
/// happen to change, so a store can serve wrong lineage and unresolvable
/// FQNs indefinitely.
///
/// The gate is its own `meta_kv` marker, not the version stamp: every
/// migration stamps the same `SCHEMA_VERSION`, so the stamp cannot
/// distinguish "reached v7 last week" from "reached v7 just now". The
/// marker's VALUE is a generation counter, not a flag: a store refilled at
/// an earlier generation still carries rows an even-later extractor spells
/// differently, and a constant marker would freeze it there forever (0.15
/// whole-branch review). The guarded upsert is the claim, so the refill runs
/// once per generation even across racing processes, and the marker plus the
/// UPDATE share one transaction: a crash between them would skip the refill
/// forever. No DDL, so `SCHEMA_VERSION` is unchanged; the shape is still v7.
///
/// Bump `CONTENT_REFILL_GENERATION` in any commit that changes FQN spelling,
/// `kind` labeling, a disamb recipe, or call extraction once the marker may
/// already be claimed on real stores. Generation history: 1 = the original
/// v7 refill (d500552); 2 = S5 identifier-named TS modules, the C++ R1
/// wrapper recovery, and the review round's disamb recipe additions (static
/// members, composed nested discriminators, enum-constant bodies).
const CONTENT_REFILL_GENERATION: i64 = 2;

fn refill_v7_content_once(conn: &Connection) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    let claimed = tx.execute(
        "INSERT INTO meta_kv(k, v) VALUES('v7_content_refill', CAST(?1 AS TEXT))
         ON CONFLICT(k) DO UPDATE SET v = excluded.v
         WHERE CAST(meta_kv.v AS INTEGER) < CAST(excluded.v AS INTEGER)",
        [CONTENT_REFILL_GENERATION],
    )?;
    if claimed == 1 {
        tx.execute("UPDATE files SET mtime_ns = 0", [])?;
        tx.commit()?;
    } else {
        tx.rollback()?;
    }
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
        migrate_to_v4(&conn)?;
        migrate_to_v5(&conn)?;
        migrate_to_v6(&conn)?;
        migrate_to_v7(&conn)?;
        migrate_to_v8(&conn)?;
        refill_v7_content_once(&conn)?;
        Ok(Store { conn, session: std::cell::Cell::new(Ledger::default()) })
    }

    /// Open an in-memory store (tests).
    pub fn open_in_memory() -> Result<Store> {
        let conn = Connection::open_in_memory()?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA_V1)?;
        migrate_to_v2(&conn)?;
        migrate_to_v3(&conn)?;
        migrate_to_v4(&conn)?;
        migrate_to_v5(&conn)?;
        migrate_to_v6(&conn)?;
        migrate_to_v7(&conn)?;
        migrate_to_v8(&conn)?;
        refill_v7_content_once(&conn)?;
        Ok(Store { conn, session: std::cell::Cell::new(Ledger::default()) })
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
                    evidence_ran_at, origin,
                    EXISTS(SELECT 1 FROM archived a WHERE a.entry_id = entries.id),
                    conf_before_stale
             FROM entries WHERE private = 0 ORDER BY id",
        )?;
        let ids: Vec<serde_json::Value> = stmt
            .query_map([], |r| {
                let mut obj = serde_json::json!({
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
                });
                // Archival travels with the export (hidden is not lost), but
                // only archived entries pay for the field on the wire.
                if r.get::<_, bool>(14)? {
                    obj["archived"] = serde_json::json!(true);
                }
                // The stored refund travels too, or a heal on the peer
                // restores nothing; only penalized entries pay for it.
                if let Some(cb) = r.get::<_, Option<f64>>(15)? {
                    obj["conf_before_stale"] = serde_json::json!(cb);
                }
                Ok(obj)
            })?
            .collect::<rusqlite::Result<_>>()?;

        let mut count = 0usize;
        for mut obj in ids {
            let id = obj["id"].as_str().unwrap_or_default().to_string();
            let mut astmt = self.conn.prepare(
                "SELECT file, symbol_fqn, ast_body_hash, context_hint, disamb
                 FROM anchors WHERE entry_id = ?1 ORDER BY id",
            )?;
            let anchors: Vec<serde_json::Value> = astmt
                .query_map([&id], |r| {
                    let mut a = serde_json::json!({
                        "file": r.get::<_, String>(0)?,
                        "symbol_fqn": r.get::<_, Option<String>>(1)?,
                        "ast_body_hash": r.get::<_, Option<String>>(2)?,
                        "context_hint": r.get::<_, Option<String>>(3)?,
                    });
                    // The slot travels with the anchor, but only a
                    // disambiguated anchor pays for the field on the wire
                    // (same omit-when-null rule as `archived`), so exports
                    // from before this column read back byte-identically.
                    if let Some(d) = r.get::<_, Option<String>>(4)? {
                        a["disamb"] = serde_json::Value::String(d);
                    }
                    Ok(a)
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

            // A far-future timestamp would win the LWW merge against every
            // honest later update forever (denial-of-correction). Treat such
            // stamps, and unparseable ones, as lowest priority. A bounded
            // skew allowance is deliberate: peers' wall clocks genuinely
            // differ, and archive/restore mint a strictly-newer stamp that
            // can sit a second ahead of this machine's clock. An hour of
            // skew cannot poison the merge durably; it ages out within the
            // hour.
            const MAX_FUTURE_SKEW_SECS: i64 = 3600;
            if let Some(secs) = crate::memory::parse_iso_secs(incoming_updated) {
                if let Some(now_secs) = crate::memory::parse_iso_secs(&now) {
                    if secs > now_secs + MAX_FUTURE_SKEW_SECS {
                        report.rejected += 1;
                        continue;
                    }
                }
            } else {
                report.rejected += 1;
                continue;
            }

            // Secrets must never enter the store, not even from a peer. The
            // size gate runs first: it is O(1) and the scan is O(len), so an
            // oversized line never buys a scan it was going to fail anyway.
            let body = obj["body"].as_str().unwrap_or_default();
            let ev_cmd = obj["evidence_cmd"].as_str().unwrap_or_default();
            if body.len() > crate::memory::MAX_BODY_BYTES {
                report.rejected += 1;
                continue;
            }
            if crate::secrets::detect(body).is_some()
                || crate::secrets::detect(ev_cmd).is_some()
            {
                report.rejected += 1;
                continue;
            }

            // Provenance is enforced on this write path too (truth layer,
            // 0.14). An unknown source would abort the WHOLE import at the
            // schema CHECK; reject just the line. `verified` is earned, not
            // claimed: a line asserting it with no evidence command would
            // mint the verified ranking boost with nothing to re-verify, so
            // it is rejected the same way `remember` refuses it.
            let source = obj["source"].as_str().unwrap_or("explicit");
            if !crate::memory::SOURCES.contains(&source) {
                report.rejected += 1;
                continue;
            }
            if source == "verified" && ev_cmd.is_empty() {
                report.rejected += 1;
                continue;
            }
            // Same per-line rule for kind, status, and an empty body: any of
            // these would otherwise reach a schema CHECK (or store a blank
            // memory remember refuses) and abort the whole batch, including
            // the silent first-run bootstrap import.
            let kind = obj["kind"].as_str().unwrap_or("insight");
            if !crate::memory::KINDS.contains(&kind) {
                report.rejected += 1;
                continue;
            }
            let status = obj["status"].as_str().unwrap_or("active");
            if !["active", "stale", "invalidated", "superseded"].contains(&status) {
                report.rejected += 1;
                continue;
            }
            if body.trim().is_empty() {
                report.rejected += 1;
                continue;
            }
            // An anchor's `disamb` names the exact symbol slot (trait impl,
            // overload) the memory belongs to. A line whose disamb is present
            // but not a string is malformed: coercing it to NULL would
            // silently widen the anchor back to "any twin with this FQN",
            // exactly the nondeterminism slot resolution exists to kill.
            // Reject the line, never the batch.
            if let Some(anchors) = obj["anchors"].as_array() {
                if anchors
                    .iter()
                    .any(|a| !a["disamb"].is_null() && !a["disamb"].is_string())
                {
                    report.rejected += 1;
                    continue;
                }
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
                        let rel = l["rel"].as_str().unwrap_or("supports");
                        if !["supports", "contradicts", "supersedes"].contains(&rel) {
                            // An unknown rel would abort the batch at the
                            // schema CHECK; drop and count it instead.
                            report.links_dropped += 1;
                            continue;
                        }
                        tx.execute(
                            "INSERT OR IGNORE INTO links(src, dst, rel) VALUES (?1,?2,?3)",
                            rusqlite::params![id, dst, rel],
                        )?;
                    }
                }
                continue;
            }

            // conf_before_stale COALESCEs like the anchor-slot preservation:
            // a peer running a binary that predates the column re-exports the
            // line WITHOUT it, and letting that NULL win would destroy a
            // pending refund so the next heal serves the penalized
            // confidence forever (0.16 whole-branch review). The cost is the
            // mirror edge: a refund legitimately consumed on the peer is
            // resurrected here until the next stale re-stores it, bounded by
            // the per-source cap below.
            tx.execute(
                "INSERT INTO entries(id, kind, body, created_at, updated_at, source,
                                     confidence, status, stale_reason, branch,
                                     evidence_cmd, evidence_digest, evidence_ran_at, origin,
                                     conf_before_stale)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)
                 ON CONFLICT(id) DO UPDATE SET
                   kind=excluded.kind, body=excluded.body,
                   created_at=excluded.created_at, updated_at=excluded.updated_at,
                   source=excluded.source, confidence=excluded.confidence,
                   status=excluded.status, stale_reason=excluded.stale_reason,
                   branch=excluded.branch, evidence_cmd=excluded.evidence_cmd,
                   evidence_digest=excluded.evidence_digest,
                   evidence_ran_at=excluded.evidence_ran_at,
                   origin=COALESCE(excluded.origin, origin),
                   conf_before_stale=COALESCE(excluded.conf_before_stale, conf_before_stale)",
                rusqlite::params![
                    id,
                    kind,
                    body,
                    obj["created_at"].as_str().unwrap_or_default(),
                    incoming_updated,
                    source,
                    // Clamp AND cap per source: an imported 1e300 would pin a
                    // hostile memory to the top of every recall (schema has no
                    // CHECK on this), and an explicit claim typed at 1.0 must
                    // respect the same ceiling the live remember path enforces.
                    crate::memory::quantize_confidence(
                        obj["confidence"]
                            .as_f64()
                            .unwrap_or(0.5)
                            .min(crate::memory::import_confidence_cap(source)),
                    ),
                    status,
                    obj["stale_reason"].as_str(),
                    obj["branch"].as_str(),
                    obj["evidence_cmd"].as_str(),
                    obj["evidence_digest"].as_str(),
                    obj["evidence_ran_at"].as_str(),
                    origin,
                    // Same discipline as confidence two params above, cap
                    // included: the refund is a FUTURE confidence (heal and
                    // reverify pay it out verbatim), so a refund above the
                    // source's ceiling would launder a hostile line past the
                    // trust policy the moment its anchors resolve (0.16
                    // whole-branch review).
                    obj["conf_before_stale"].as_f64().map(|c| {
                        crate::memory::quantize_confidence(
                            c.min(crate::memory::import_confidence_cap(source)),
                        )
                    }),
                ],
            )?;
            // Archival travels with the line: a winning (added/updated) line
            // dictates visibility on this machine too. Absent or false means
            // visible, so a restore propagates the same way an archive does.
            if obj["archived"].as_bool() == Some(true) {
                tx.execute(
                    "INSERT INTO archived(entry_id, archived_at) VALUES (?1, ?2)
                     ON CONFLICT(entry_id) DO NOTHING",
                    rusqlite::params![id, now],
                )?;
            } else {
                tx.execute("DELETE FROM archived WHERE entry_id = ?1", [&id])?;
            }
            // A winning line replaces the entry's anchors wholesale, but a
            // peer running an older binary exports anchors WITHOUT the disamb
            // field it does not know. Letting that wash cycle (0.15 exports,
            // 0.14 peer archives and re-exports, 0.15 re-imports) rewrite a
            // slotted anchor to NULL would strip twin protection permanently:
            // with a same-FQN twin present the backfill correctly refuses to
            // re-adopt, so the anchor falls to the legacy ladder forever.
            // Preserve the local slot when the incoming anchor names the same
            // (file, symbol_fqn) and carries no disamb of its own. Best
            // effort by design: an entry anchored to two twins of one FQN
            // keeps the first slot seen (a shape remember cannot produce).
            let mut preserved_slots: std::collections::HashMap<(String, Option<String>), String> =
                std::collections::HashMap::new();
            {
                let mut ps = tx.prepare(
                    "SELECT file, symbol_fqn, disamb FROM anchors
                     WHERE entry_id = ?1 AND disamb IS NOT NULL",
                )?;
                let rows = ps.query_map([&id], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, String>(2)?))
                })?;
                for row in rows {
                    let (f, s, d) = row?;
                    preserved_slots.entry((f, s)).or_insert(d);
                }
            }
            tx.execute("DELETE FROM anchors WHERE entry_id = ?1", [&id])?;
            if let Some(anchors) = obj["anchors"].as_array() {
                for a in anchors {
                    let file = a["file"].as_str().unwrap_or_default();
                    let symbol_fqn = a["symbol_fqn"].as_str();
                    let disamb: Option<String> = a["disamb"].as_str().map(str::to_string).or_else(|| {
                        preserved_slots
                            .get(&(file.to_string(), symbol_fqn.map(str::to_string)))
                            .cloned()
                    });
                    let disamb = disamb.as_deref();
                    // Re-resolve the hash against the LOCAL index rather than
                    // trusting the imported one: a forged hash must not be
                    // able to fake "fresh" against code this machine does not
                    // have. If the anchored symbol/file exists here, adopt the
                    // local hash (honestly fresh); otherwise keep the imported
                    // hash so normal follow/invalidate logic applies. A
                    // disambiguated anchor re-resolves against ITS slot, so a
                    // twin's body can never supply the hash. `disamb IS ?2`,
                    // never a NULL-is-wildcard OR: a NULL on the wire is the
                    // deliberate undiscriminated slot (the trailing-@ spec
                    // pins the row where disamb IS NULL), and widening it to
                    // "any twin, lowest ordinal" adopted the wrong slot's
                    // body as the anchor's fresh baseline. A NULL that
                    // matches no row falls through to the imported hash and
                    // the ladder judges it honestly. The file is pinned too:
                    // an FQN is path-derived with the extension stripped, so
                    // `util.js` and `util.ts` mint the same one and an
                    // fqn-only lookup would adopt whichever sorts first. The
                    // ordering keeps the rest deterministic.
                    let local_hash: Option<String> = match symbol_fqn {
                        Some(fqn) => {
                            let exact: Option<String> = tx
                                .query_row(
                                    "SELECT body_hash FROM symbols
                                     WHERE fqn = ?1 AND disamb IS ?2 AND file = ?3
                                     ORDER BY ordinal, body_hash LIMIT 1",
                                    rusqlite::params![fqn, disamb, file],
                                    |r| r.get(0),
                                )
                                .ok();
                            match (exact, disamb) {
                                (Some(h), _) => Some(h),
                                // A slot-less anchor (a legacy export, or a
                                // peer predating disamb) whose fqn has no NULL
                                // row here: adopt the local hash only when the
                                // fqn+file is twin-free, so the single row can
                                // only be the anchored symbol. With twins
                                // present, guessing a slot is how anchors
                                // start lying; the imported hash stays and the
                                // ladder judges it.
                                (None, None) => {
                                    let mut ts = tx.prepare(
                                        "SELECT body_hash FROM symbols
                                         WHERE fqn = ?1 AND file = ?2 LIMIT 2",
                                    )?;
                                    let hashes: Vec<String> = ts
                                        .query_map(rusqlite::params![fqn, file], |r| r.get(0))?
                                        .collect::<rusqlite::Result<_>>()?;
                                    match hashes.as_slice() {
                                        [only] => Some(only.clone()),
                                        _ => None,
                                    }
                                }
                                (None, Some(_)) => None,
                            }
                        }
                        None => tx
                            .query_row("SELECT hash FROM files WHERE path = ?1", [file], |r| {
                                r.get(0)
                            })
                            .ok(),
                    };
                    let hash = local_hash.or_else(|| a["ast_body_hash"].as_str().map(str::to_string));
                    tx.execute(
                        "INSERT INTO anchors(entry_id, file, symbol_fqn, ast_body_hash, context_hint, disamb)
                         VALUES (?1,?2,?3,?4,?5,?6)",
                        rusqlite::params![
                            id, file, symbol_fqn, hash, a["context_hint"].as_str(), disamb
                        ],
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
                    let rel = l["rel"].as_str().unwrap_or("supports");
                    if !["supports", "contradicts", "supersedes"].contains(&rel) {
                        // An unknown rel would abort the batch at the schema
                        // CHECK; drop and count it instead.
                        report.links_dropped += 1;
                        continue;
                    }
                    tx.execute(
                        "INSERT OR IGNORE INTO links(src, dst, rel) VALUES (?1,?2,?3)",
                        rusqlite::params![id, dst, rel],
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
        // replaced reports a real negative. That licence is about TOKENS
        // only. Counts (recalls, distinct queries, reads avoided) are
        // tallies of things that happened and have no negative value,
        // which is why no subtraction of one Ledger from another exists
        // here any more: `session` is counted directly by ledger_add.
        self.baseline - self.served
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
    ///
    /// This is the ONLY writer of the ledger, so it is also where the
    /// per-process session tally is kept: on commit, the very figures that
    /// went into meta_kv are added to `session` too. Session is therefore
    /// counted, not inferred, and no other process can move it.
    pub fn ledger_add(
        &self,
        served: i64,
        baseline: i64,
        reads_avoided: i64,
        query_hash: &str,
    ) -> Result<()> {
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        let outcome = (|| -> Result<bool> {
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
            Ok(newly_seen)
        })();
        match outcome {
            Ok(newly_seen) => {
                self.conn.execute_batch("COMMIT")?;
                // Only committed work counts as served: a rolled-back
                // recall never reaches this line.
                let mut mine = self.session.get();
                mine.recalls += 1;
                // Distinct here means "queries this process was first to
                // ask", the same first-sighting rule the lifetime counter
                // uses. Not published in any session block today; kept
                // consistent so it cannot become a lie if it ever is.
                if newly_seen {
                    mine.distinct_queries += 1;
                }
                mine.served += served;
                mine.baseline += baseline;
                mine.reads_avoided += reads_avoided;
                self.session.set(mine);
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

    /// Begin this process's session tally at zero. A fresh `Store` already
    /// starts there, so a server boot calling this is a no-op by design;
    /// the live use is `ledger_reset`, which wipes the shared counters and
    /// must not leave the wiping process reporting a session larger than
    /// the lifetime it just cleared. Other processes keep their own tallies
    /// untouched, which is the point: their counts stay true and, unlike
    /// the old lifetime-minus-snapshot subtraction, can never go negative.
    pub fn ledger_session_start(&self) -> Result<()> {
        self.session.set(Ledger::default());
        Ok(())
    }

    /// The recalls THIS process served, counted as it served them.
    pub fn ledger_session(&self) -> Ledger {
        self.session.get()
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
    fn ledger_accumulates_lifetime_and_session_and_resets() {
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

        // Session counts this handle's OWN calls: the three above are its
        // own work, so they are in the tally, and nothing else can be.
        let session = s.ledger_session();
        assert_eq!(session.recalls, 3);
        assert_eq!(session.distinct_queries, 2);
        assert_eq!(session.served, 230);
        assert_eq!(session.saved(), 900);
        assert_eq!(session.reads_avoided, 3);

        // A restart begins the tally at zero and only the recall served
        // after it lands in the new session.
        s.ledger_session_start().unwrap();
        assert_eq!(s.ledger_session(), Ledger::default());
        s.ledger_add(10, 500, 1, "q3").unwrap();
        assert_eq!(s.ledger_session().recalls, 1);
        assert_eq!(s.ledger_session().saved(), 490);
        assert_eq!(s.ledger_read().recalls, 4, "lifetime keeps counting across a restart");

        // Negative savings survive (I-L2). Negative COUNTS cannot happen:
        // a wipe of the shared rows leaves the tally of what was really
        // served alone, it does not subtract it into nonsense.
        s.ledger_reset().unwrap();
        assert_eq!(s.ledger_read(), Ledger::default());
        assert!(s.ledger_since().is_none(), "reset restamps since lazily");
        assert_eq!(s.ledger_session().recalls, 1, "a wipe cannot un-serve a served recall");
        s.ledger_add(1000, 300, 0, "q4").unwrap();
        assert_eq!(s.ledger_read().saved(), -700);
        assert_eq!(s.ledger_session().recalls, 2, "the tally keeps counting past a wipe");
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
            // Only the first ALTER; simulates crash before `origin` was added.
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
        assert_eq!(s.kv_get("schema_version").unwrap().as_deref(), Some("8"));
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
        assert_eq!(ver, "8");
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
    fn v3_store_migrates_to_v4_wider_rels() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("store.db");
        // Build a v3 store (v1 batch WITH the old narrow inherits CHECK + v2 + v3).
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(
                "CREATE TABLE meta_kv (k TEXT PRIMARY KEY, v TEXT NOT NULL);
                 CREATE TABLE inherits (
                   child_fqn TEXT NOT NULL, parent_name TEXT NOT NULL,
                   rel TEXT NOT NULL CHECK(rel IN ('extends','implements','impl_trait')),
                   file TEXT NOT NULL);",
            )
            .unwrap();
            conn.execute("INSERT INTO meta_kv(k,v) VALUES('schema_version','3')", []).unwrap();
        }
        // Opening through Store must migrate to v4 and accept the new rels.
        let store = Store::open(&db).unwrap();
        let ver: String = store
            .conn
            .query_row("SELECT v FROM meta_kv WHERE k='schema_version'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(ver, "8");
        for rel in ["embeds", "mixin", "extends"] {
            store
                .conn
                .execute(
                    "INSERT INTO inherits(child_fqn,parent_name,rel,file) VALUES('a.B','P',?1,'a.go')",
                    [rel],
                )
                .unwrap_or_else(|e| panic!("rel {rel} rejected: {e}"));
        }
        // A bogus rel is still rejected.
        assert!(store
            .conn
            .execute(
                "INSERT INTO inherits(child_fqn,parent_name,rel,file) VALUES('a.B','P','bogus','a.go')",
                [],
            )
            .is_err());
    }

    #[test]
    fn inherits_survive_store_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("store.db");
        {
            let store = Store::open(&db).unwrap();
            store
                .conn
                .execute(
                    "INSERT INTO inherits(child_fqn,parent_name,rel,file) VALUES('a.Dog','Animal','embeds','a.go')",
                    [],
                )
                .unwrap();
        } // drop/close
        // Reopen: migrations run again; the inherits row MUST still be there.
        let store = Store::open(&db).unwrap();
        let n: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM inherits WHERE child_fqn='a.Dog'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            n,
            1,
            "inherits must survive a store reopen (migrate_to_v4 must not wipe it every open)"
        );
    }

    #[test]
    fn fresh_store_has_body_len_column() {
        let s = Store::open_in_memory().unwrap();
        let present: i64 = s.conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('symbols') WHERE name = 'body_len'",
            [], |r| r.get(0),
        ).unwrap();
        assert_eq!(present, 1, "fresh DDL must carry body_len");
        assert_eq!(s.kv_get("schema_version").unwrap().as_deref(), Some("8"));
    }

    #[test]
    fn v5_migration_adds_column_and_marks_files_for_reindex() {
        // Build a v4-shaped store by hand: symbols WITHOUT body_len, one file
        // with a live mtime. Mirror the raw-conn setup of the v3/v4 migration
        // tests at src/store.rs:1243+.
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("memory.db");
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute_batch(
                "CREATE TABLE meta_kv (k TEXT PRIMARY KEY, v TEXT NOT NULL);
                 CREATE TABLE files (path TEXT PRIMARY KEY, lang TEXT, mtime_ns INTEGER NOT NULL,
                   size INTEGER NOT NULL, hash TEXT NOT NULL, parse_ok INTEGER NOT NULL DEFAULT 1);
                 CREATE TABLE symbols (
                   id INTEGER PRIMARY KEY, fqn TEXT NOT NULL, name TEXT NOT NULL,
                   kind TEXT NOT NULL, file TEXT NOT NULL,
                   start_line INTEGER, end_line INTEGER,
                   body_hash TEXT NOT NULL, parent_fqn TEXT,
                   ordinal INTEGER NOT NULL DEFAULT 0);
                 CREATE TABLE inherits (
                   child_fqn TEXT NOT NULL, parent_name TEXT NOT NULL,
                   rel TEXT NOT NULL CHECK(rel IN ('extends','implements','impl_trait','embeds','mixin')),
                   file TEXT NOT NULL);",
            ).unwrap();
            conn.execute("INSERT INTO meta_kv(k,v) VALUES('schema_version','4')", []).unwrap();
            conn.execute(
                "INSERT INTO files(path,lang,mtime_ns,size,hash) VALUES('a.py','python',12345,10,'h')",
                [],
            ).unwrap();
            conn.execute(
                "INSERT INTO symbols(fqn,name,kind,file,body_hash) VALUES('a.f','f','function','a.py','bh')",
                [],
            ).unwrap();
        }
        let s = Store::open(&db).unwrap();
        let present: i64 = s.conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('symbols') WHERE name = 'body_len'",
            [], |r| r.get(0),
        ).unwrap();
        assert_eq!(present, 1, "migration must add body_len");
        let mtime: i64 = s.conn
            .query_row("SELECT mtime_ns FROM files WHERE path='a.py'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mtime, 0, "one-time widen must mark files dirty so the sweep refills body_len");
        let kept: i64 = s.conn
            .query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))
            .unwrap();
        assert_eq!(kept, 1, "migration must not touch existing symbol rows");
    }

    #[test]
    fn body_len_survives_store_reopen() {
        // The refill marker must fire ONCE: a second open must not re-zero
        // mtimes or disturb data (the v4 lesson: single-open tests hid a wipe).
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("memory.db");
        {
            let s = Store::open(&db).unwrap();
            s.conn.execute(
                "INSERT INTO files(path,lang,mtime_ns,size,hash) VALUES('a.py','python',999,10,'h')",
                [],
            ).unwrap();
            s.conn.execute(
                "INSERT INTO symbols(fqn,name,kind,file,body_hash,body_len)
                 VALUES('a.f','f','function','a.py','bh',77)",
                [],
            ).unwrap();
        }
        let s = Store::open(&db).unwrap();
        let (mtime, len): (i64, i64) = s.conn.query_row(
            "SELECT f.mtime_ns, s.body_len FROM files f JOIN symbols s ON s.file=f.path",
            [], |r| Ok((r.get(0)?, r.get(1)?)),
        ).unwrap();
        assert_eq!(mtime, 999, "re-open must not re-fire the refill marker");
        assert_eq!(len, 77, "re-open must not disturb body_len");
    }

    #[test]
    fn v4_widen_marks_files_for_reindex() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("store.db");
        // v3-shaped store: narrow CHECK + a populated files row.
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(
                "CREATE TABLE meta_kv (k TEXT PRIMARY KEY, v TEXT NOT NULL);
                 CREATE TABLE files (path TEXT PRIMARY KEY, lang TEXT, mtime_ns INTEGER NOT NULL,
                   size INTEGER NOT NULL, hash TEXT NOT NULL, parse_ok INTEGER NOT NULL DEFAULT 1);
                 CREATE TABLE inherits (
                   child_fqn TEXT NOT NULL, parent_name TEXT NOT NULL,
                   rel TEXT NOT NULL CHECK(rel IN ('extends','implements','impl_trait')),
                   file TEXT NOT NULL);",
            )
            .unwrap();
            conn.execute("INSERT INTO meta_kv(k,v) VALUES('schema_version','3')", []).unwrap();
            conn.execute(
                "INSERT INTO files(path,lang,mtime_ns,size,hash) VALUES('a.go','go',12345,10,'h')",
                [],
            )
            .unwrap();
        }
        let store = Store::open(&db).unwrap();
        // The widen fired: files must be marked dirty so the sweep refills inherits.
        let mtime: i64 = store
            .conn
            .query_row("SELECT mtime_ns FROM files WHERE path='a.go'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mtime, 0, "widen must zero mtimes so the sweep repopulates inherits");
        // Reopen: already wide, mtimes must NOT be touched again.
        store.conn.execute("UPDATE files SET mtime_ns = 999", []).unwrap();
        drop(store);
        let store = Store::open(&db).unwrap();
        let mtime: i64 = store
            .conn
            .query_row("SELECT mtime_ns FROM files WHERE path='a.go'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mtime, 999, "already-wide open must not re-zero mtimes");
    }

    /// Build a v6-shaped store by hand at `db`: symbols and anchors WITHOUT
    /// disamb, everything else already current (wide inherits CHECK, body_len
    /// present) so ONLY the v7 branch can be the one zeroing mtimes. One file
    /// row with a live mtime and one symbol row.
    fn build_v6_shaped_store(db: &std::path::Path) {
        let conn = rusqlite::Connection::open(db).unwrap();
        conn.execute_batch(
            "CREATE TABLE meta_kv (k TEXT PRIMARY KEY, v TEXT NOT NULL);
             CREATE TABLE files (path TEXT PRIMARY KEY, lang TEXT, mtime_ns INTEGER NOT NULL,
               size INTEGER NOT NULL, hash TEXT NOT NULL, parse_ok INTEGER NOT NULL DEFAULT 1);
             CREATE TABLE symbols (
               id INTEGER PRIMARY KEY, fqn TEXT NOT NULL, name TEXT NOT NULL,
               kind TEXT NOT NULL, file TEXT NOT NULL,
               start_line INTEGER, end_line INTEGER,
               body_hash TEXT NOT NULL, body_len INTEGER, parent_fqn TEXT,
               ordinal INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE anchors (
               id INTEGER PRIMARY KEY, entry_id TEXT NOT NULL, file TEXT NOT NULL,
               symbol_fqn TEXT, ast_body_hash TEXT, context_hint TEXT);
             CREATE TABLE inherits (
               child_fqn TEXT NOT NULL, parent_name TEXT NOT NULL,
               rel TEXT NOT NULL CHECK(rel IN ('extends','implements','impl_trait','embeds','mixin')),
               file TEXT NOT NULL);",
        )
        .unwrap();
        conn.execute("INSERT INTO meta_kv(k,v) VALUES('schema_version','6')", []).unwrap();
        conn.execute(
            "INSERT INTO files(path,lang,mtime_ns,size,hash) VALUES('a.rs','rust',12345,10,'h')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO symbols(fqn,name,kind,file,body_hash,body_len)
             VALUES('a.f','f','function','a.rs','bh',99)",
            [],
        )
        .unwrap();
    }

    #[test]
    fn v7_migration_adds_disamb_and_marks_files_for_reindex() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("store.db");
        build_v6_shaped_store(&db);
        let s = Store::open(&db).unwrap();
        for table in ["symbols", "anchors"] {
            let present: i64 = s.conn.query_row(
                "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = 'disamb'",
                [table], |r| r.get(0),
            ).unwrap();
            assert_eq!(present, 1, "migration must add {table}.disamb");
        }
        let mtime: i64 = s.conn
            .query_row("SELECT mtime_ns FROM files WHERE path='a.rs'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mtime, 0, "one-time widen must mark files dirty so the sweep refills disamb");
        let kept: i64 = s.conn
            .query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))
            .unwrap();
        assert_eq!(kept, 1, "migration must not touch existing symbol rows");
        assert_eq!(s.kv_get("schema_version").unwrap().as_deref(), Some("8"));
    }

    #[test]
    fn v7_refill_marker_fires_once() {
        // The v7 mtime zeroing must fire ONCE: migrations run on EVERY open
        // from multiple processes, and a second open must not re-zero (the v4
        // lesson: single-open tests hid a wipe).
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("store.db");
        build_v6_shaped_store(&db);
        {
            let s = Store::open(&db).unwrap();
            let mtime: i64 = s.conn
                .query_row("SELECT mtime_ns FROM files WHERE path='a.rs'", [], |r| r.get(0))
                .unwrap();
            assert_eq!(mtime, 0, "first open must fire the refill marker");
            s.conn.execute("UPDATE files SET mtime_ns = 999", []).unwrap();
        }
        let s = Store::open(&db).unwrap();
        let mtime: i64 = s.conn
            .query_row("SELECT mtime_ns FROM files WHERE path='a.rs'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mtime, 999, "re-open must not re-fire the disamb refill marker");
    }

    #[test]
    fn a_store_already_at_v7_still_gets_one_content_refill() {
        // The gap `migrate_to_v7` cannot see: a store that reached v7 under an
        // EARLIER v7 build already has the columns, so the ALTER-gated refill
        // never fires again, and it keeps FQNs, discriminators and caller_fqns
        // the shipping extractor spells differently. Those rows heal only as
        // files happen to change, so the store serves wrong answers
        // indefinitely.
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("store.db");
        build_v6_shaped_store(&db);
        {
            // Exactly what a mid-development v7 build left behind: both columns
            // present, live mtimes, and no content-refill marker.
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute_batch(
                "ALTER TABLE symbols ADD COLUMN disamb TEXT;
                 ALTER TABLE anchors ADD COLUMN disamb TEXT;",
            )
            .unwrap();
            conn.execute("UPDATE meta_kv SET v='7' WHERE k='schema_version'", []).unwrap();
        }
        {
            let s = Store::open(&db).unwrap();
            let mtime: i64 = s
                .conn
                .query_row("SELECT mtime_ns FROM files WHERE path='a.rs'", [], |r| r.get(0))
                .unwrap();
            assert_eq!(mtime, 0, "an already-v7 store must still be marked for one reindex");
            s.conn.execute("UPDATE files SET mtime_ns = 777", []).unwrap();
        }
        let s = Store::open(&db).unwrap();
        let mtime: i64 = s
            .conn
            .query_row("SELECT mtime_ns FROM files WHERE path='a.rs'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mtime, 777, "the content refill claims its marker once and never re-fires");
    }

    #[test]
    fn a_store_refilled_at_an_earlier_generation_refills_once_more() {
        // A store that claimed the marker under an earlier extractor
        // generation keeps rows the shipping extractor spells differently;
        // the generation bump must re-fire the refill exactly once, and a
        // marker already at the current generation must stay a no-op.
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("store.db");
        build_v6_shaped_store(&db);
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute_batch(
                "ALTER TABLE symbols ADD COLUMN disamb TEXT;
                 ALTER TABLE anchors ADD COLUMN disamb TEXT;",
            )
            .unwrap();
            conn.execute("UPDATE meta_kv SET v='7' WHERE k='schema_version'", []).unwrap();
            conn.execute(
                "INSERT INTO meta_kv(k, v) VALUES('v7_content_refill', '1')",
                [],
            )
            .unwrap();
        }
        {
            let s = Store::open(&db).unwrap();
            let mtime: i64 = s
                .conn
                .query_row("SELECT mtime_ns FROM files WHERE path='a.rs'", [], |r| r.get(0))
                .unwrap();
            assert_eq!(mtime, 0, "a generation-1 store must refill once under generation 2");
            let gen: String = s
                .conn
                .query_row("SELECT v FROM meta_kv WHERE k='v7_content_refill'", [], |r| r.get(0))
                .unwrap();
            assert_eq!(gen, "2", "the claim must record the new generation");
            s.conn.execute("UPDATE files SET mtime_ns = 555", []).unwrap();
        }
        let s = Store::open(&db).unwrap();
        let mtime: i64 = s
            .conn
            .query_row("SELECT mtime_ns FROM files WHERE path='a.rs'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mtime, 555, "a marker already at the current generation never re-fires");
    }

    #[test]
    fn fresh_store_has_disamb_columns() {
        // Pin the CONST itself first: pragma inspection alone is tautological
        // here, because migrate_to_v7 would add the columns in the same open
        // if SCHEMA_V1 dropped them. Exactly two declarations, symbols and
        // anchors.
        assert_eq!(
            SCHEMA_V1.matches("disamb TEXT").count(),
            2,
            "SCHEMA_V1 must declare disamb on symbols and anchors, not lean on migrate_to_v7",
        );
        // Fresh DDL carries disamb directly, so the v7 pragma gate skips both
        // ALTERs and no migration work runs on a new store.
        let s = Store::open_in_memory().unwrap();
        for table in ["symbols", "anchors"] {
            let present: i64 = s.conn.query_row(
                "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = 'disamb'",
                [table], |r| r.get(0),
            ).unwrap();
            assert_eq!(present, 1, "fresh DDL must carry {table}.disamb");
        }
    }

    #[test]
    fn duplicate_column_alter_is_tolerated_and_other_errors_propagate() {
        // The migration-race loser: pragma gate passed stale, the column now
        // exists, the ALTER fails with SQLite's "duplicate column name". That
        // exact failure must map to Ok(false) (already migrated, no one-time
        // side effects); anything else must propagate.
        let s = Store::open_in_memory().unwrap();
        let added = add_column_tolerating_race(
            &s.conn,
            "ALTER TABLE symbols ADD COLUMN disamb TEXT",
        )
        .unwrap();
        assert!(!added, "loser must see already-migrated, not an error");
        assert!(
            add_column_tolerating_race(&s.conn, "ALTER TABLE no_such_table ADD COLUMN x TEXT")
                .is_err(),
            "non-race ALTER failures must propagate",
        );
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
