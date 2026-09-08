//! The v1.0 stability contract: the store must be safe to depend on for
//! years, which starts with never letting an older binary quietly rewrite a
//! newer store's schema stamp downward (I-S1, I-S2) and never reading a
//! future export format as if it were entries (I-S3).

use limpet::memory;
use limpet::store::{Store, SCHEMA_VERSION};

/// One plain, unanchored entry through the real public write path.
fn seed_entry(store: &Store, body: &str) {
    memory::remember(
        store, "fact", body, "explicit", None, &[], None, &[], Some("main"), false, None, false,
    )
    .expect("seed entry");
}

fn scratch() -> tempfile::TempDir {
    tempfile::tempdir().expect("tempdir")
}

/// Simulate "a newer limpet touched this store": stamp a schema_version one
/// past what this binary knows, exactly what a future migration would leave
/// behind. The current binary must refuse the open loudly instead of running
/// its own (older) chain and stamping the version back down.
#[test]
fn opening_a_newer_schema_store_refuses_loudly() {
    let dir = scratch();
    let db = dir.path().join("store.db");
    {
        let s = Store::open(&db).expect("fresh open");
        s.kv_set("schema_version", &(SCHEMA_VERSION + 1).to_string())
            .expect("stamp forward");
    }
    let err = match Store::open(&db) {
        Ok(_) => panic!("an older binary opened a newer store without refusing"),
        Err(e) => format!("{e:#}"),
    };
    // The refusal must name both versions and the way out, so the one time a
    // user ever sees it, the message is the fix.
    let this = SCHEMA_VERSION.to_string();
    let newer = (SCHEMA_VERSION + 1).to_string();
    assert!(err.contains(&this), "refusal names this binary's schema: {err}");
    assert!(err.contains(&newer), "refusal names the store's schema: {err}");
    assert!(
        err.contains("limpet update") || err.contains("newer"),
        "refusal points at the remedy: {err}"
    );
}

/// A refused open runs NO migration and rewrites nothing: the stamp stays,
/// and a deliberately removed v8 column stays removed. The column is the
/// load-bearing half: `SCHEMA_V1` is IF-NOT-EXISTS DDL that runs before the
/// guard either way, so only a MIGRATION artifact (the self-gating v8 ALTER
/// would re-add the dropped column) can prove the guard fired before the
/// chain rather than after it.
#[test]
fn a_refused_open_runs_no_migrations() {
    let dir = scratch();
    let db = dir.path().join("store.db");
    { Store::open(&db).expect("fresh open"); }
    {
        let conn = rusqlite::Connection::open(&db).expect("raw open");
        conn.execute("ALTER TABLE entries DROP COLUMN conf_before_stale", [])
            .expect("drop the v8 column");
        conn.execute(
            "UPDATE meta_kv SET v = ?1 WHERE k = 'schema_version'",
            [(SCHEMA_VERSION + 1).to_string()],
        )
        .expect("stamp forward");
    }
    assert!(Store::open(&db).is_err(), "the open must be refused");
    let conn = rusqlite::Connection::open(&db).expect("raw open");
    let v: String = conn
        .query_row("SELECT v FROM meta_kv WHERE k='schema_version'", [], |r| r.get(0))
        .expect("stamp still present");
    assert_eq!(
        v,
        (SCHEMA_VERSION + 1).to_string(),
        "the refused open must not have rewritten the stamp"
    );
    let readded: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('entries') WHERE name='conf_before_stale'",
            [],
            |r| r.get(0),
        )
        .expect("pragma");
    assert_eq!(
        readded, 0,
        "migrate_to_v8 ran despite the refusal: the guard is not ahead of the chain"
    );
}

/// The export names its wire format on the first line (I-S3), so a future
/// limpet reading an old file, and an old limpet reading a future file,
/// both know exactly what they are holding instead of guessing from shape.
#[test]
fn export_writes_a_format_header_first() {
    let dir = scratch();
    let s = Store::open(&dir.path().join("store.db")).expect("open");
    let mut out: Vec<u8> = Vec::new();
    s.export_jsonl(&mut out).expect("export");
    let text = String::from_utf8(out).expect("utf8");
    let first = text.lines().next().expect("export has at least the header");
    let obj: serde_json::Value = serde_json::from_str(first).expect("header is JSON");
    assert_eq!(obj["limpet_export"], 1, "header names the export format: {first}");
    assert_eq!(
        obj["schema_version"],
        serde_json::json!(SCHEMA_VERSION),
        "header names the schema it was written from: {first}"
    );
}

/// A round trip through the new wire format is lossless and the header is
/// never counted as data: every entry lands, nothing is rejected.
#[test]
fn import_skips_its_own_header_as_data() {
    let dir = scratch();
    let src = Store::open(&dir.path().join("a.db")).expect("open a");
    seed_entry(&src, "the scheduler drains the queue before rebalancing");
    let mut wire: Vec<u8> = Vec::new();
    src.export_jsonl(&mut wire).expect("export");

    let mut dst = Store::open(&dir.path().join("b.db")).expect("open b");
    let report = dst
        .import_jsonl(&mut std::io::BufReader::new(wire.as_slice()))
        .expect("import");
    assert_eq!(report.added, 1, "the one real entry lands");
    assert_eq!(report.rejected, 0, "the header is not rejected data: {report:?}");
}

/// Yesterday's export has no header. It stays importable forever: the
/// header is an addition, not a gate on the past.
#[test]
fn import_accepts_a_pre_1_0_headerless_file() {
    let dir = scratch();
    let mut dst = Store::open(&dir.path().join("b.db")).expect("open");
    let line = r#"{"id":"01HEADERLESS00000000000000","kind":"fact","body":"pre-1.0 exports keep importing","created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z","source":"explicit","confidence":0.5,"status":"active","stale_reason":null,"branch":"main","evidence_cmd":null,"evidence_digest":null,"evidence_ran_at":null,"origin":null,"links":[],"anchors":[]}"#;
    let mut input = std::io::BufReader::new(line.as_bytes());
    let report = dst.import_jsonl(&mut input).expect("headerless import");
    assert_eq!(report.added, 1);
}

/// A file claiming a FUTURE export format is refused whole, loudly, naming
/// both formats: guessing at unknown wire data is how stores get corrupted.
#[test]
fn import_refuses_a_future_format_loudly() {
    let dir = scratch();
    let mut dst = Store::open(&dir.path().join("b.db")).expect("open");
    let wire = "{\"limpet_export\":2,\"schema_version\":99}\n";
    let mut input = std::io::BufReader::new(wire.as_bytes());
    let err = match dst.import_jsonl(&mut input) {
        Ok(r) => panic!("a future-format file imported: {r:?}"),
        Err(e) => format!("{e:#}"),
    };
    assert!(err.contains('2'), "refusal names the file's format: {err}");
    assert!(err.contains('1'), "refusal names the supported format: {err}");
}

/// `open` and `open_in_memory` must produce the same schema (I-S4): the
/// migration chain is one function now, and this pins that it stays one.
#[test]
fn file_and_memory_stores_share_one_schema() {
    let dir = scratch();
    let file = Store::open(&dir.path().join("store.db")).expect("file open");
    let mem = Store::open_in_memory().expect("memory open");
    let dump = |s: &Store| -> Vec<String> {
        s.conn
            .prepare("SELECT COALESCE(sql,'') FROM sqlite_master ORDER BY type, name")
            .expect("prepare")
            .query_map([], |r| r.get::<_, String>(0))
            .expect("query")
            .collect::<Result<Vec<_>, _>>()
            .expect("rows")
    };
    assert_eq!(dump(&file), dump(&mem), "the two open paths built different schemas");
}

// ---- I-S5: the ui surface is closed and read-only ----

/// A spawned `limpet ui` that dies with the test: a bare Child leaks a
/// forever-running accept loop whenever an assert fires before the manual
/// kill, so the kill lives in Drop.
struct UiChild(std::process::Child);
impl Drop for UiChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Spawn the real `limpet ui` arm against a seeded scratch store and return
/// (child, port, data_dir, root). The child is killed on drop. The ephemeral
/// port is bind-drop-rebind (the child rebinds it), so a parallel process
/// can steal it in the window; up to three attempts on fresh ports keep that
/// from washing the test out.
fn spawn_ui(tmp: &tempfile::TempDir) -> (UiChild, u16, std::path::PathBuf, std::path::PathBuf) {
    let root = tmp.path().join("repo");
    std::fs::create_dir_all(&root).expect("mkdir repo");
    let data = tmp.path().join("data");
    std::fs::create_dir_all(&data).expect("mkdir data");
    {
        // The keyed layout Store uses under LIMPET_DATA_DIR, built without
        // touching this process's env: tests run in parallel threads and a
        // set_var window would race between two spawns.
        let db = data.join(limpet::util::repo_key(&root)).join("store.db");
        let s = Store::open(&db).expect("seed store");
        seed_entry(&s, "the ui surface must stay read-only");
    }
    for _ in 0..3 {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .expect("bind ephemeral")
            .local_addr()
            .expect("addr")
            .port();
        let child = UiChild(
            std::process::Command::new(env!("CARGO_BIN_EXE_limpet"))
                .args(["ui", "--root"])
                .arg(&root)
                .args(["--port", &port.to_string()])
                .env("LIMPET_DATA_DIR", &data)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("spawn limpet ui"),
        );
        if ui_ready(port) {
            return (child, port, data, root);
        }
    }
    panic!("limpet ui never came up on three fresh ports");
}

/// One raw GET, full response (status line included).
fn ui_get_raw(port: u16, path: &str) -> Option<String> {
    use std::io::{Read, Write};
    let addr = format!("127.0.0.1:{port}").parse().ok()?;
    let mut sock =
        std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(2)).ok()?;
    sock.set_read_timeout(Some(std::time::Duration::from_secs(15))).ok()?;
    write!(sock, "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").ok()?;
    sock.flush().ok()?;
    let _ = sock.shutdown(std::net::Shutdown::Write);
    let mut raw = String::new();
    sock.read_to_string(&mut raw).ok()?;
    Some(raw)
}

fn ui_ready(port: u16) -> bool {
    for _ in 0..50 {
        if ui_get_raw(port, "/api/projects").is_some() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    false
}

/// The logical content a request could hope to change: EVERY real table,
/// every column, every row (the 2026-08-25 review proved a narrower dump
/// blind to writes on archived, anchors, links, and inherits). Virtual
/// tables are skipped (FTS cannot be SELECT *'d); their real shadow tables
/// are included. Rows are sorted in Rust, so no per-table ORDER BY is
/// needed.
fn content_dump(db: &std::path::Path) -> Vec<String> {
    let conn = rusqlite::Connection::open_with_flags(
        db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .expect("read-only open");
    let tables: Vec<String> = conn
        .prepare(
            "SELECT name FROM sqlite_master
             WHERE type='table' AND name NOT LIKE 'sqlite_%'
               AND sql NOT LIKE 'CREATE VIRTUAL%'
             ORDER BY name",
        )
        .expect("prepare master")
        .query_map([], |r| r.get::<_, String>(0))
        .expect("query master")
        .collect::<Result<Vec<_>, _>>()
        .expect("table names");
    let mut rows: Vec<String> = Vec::new();
    for t in tables {
        let mut stmt = conn
            .prepare(&format!("SELECT * FROM \"{t}\""))
            .expect("prepare table");
        let ncols = stmt.column_count();
        let got = stmt
            .query_map([], |r| {
                let mut line = t.clone();
                for i in 0..ncols {
                    let v = r.get_ref(i)?;
                    line.push('|');
                    line.push_str(&match v {
                        rusqlite::types::ValueRef::Null => "NULL".to_string(),
                        rusqlite::types::ValueRef::Integer(n) => n.to_string(),
                        rusqlite::types::ValueRef::Real(f) => f.to_string(),
                        rusqlite::types::ValueRef::Text(s) => String::from_utf8_lossy(s).into_owned(),
                        rusqlite::types::ValueRef::Blob(b) => format!("blob:{}", b.len()),
                    });
                }
                Ok(line)
            })
            .expect("query table")
            .collect::<Result<Vec<_>, _>>()
            .expect("rows");
        rows.extend(got);
    }
    rows.sort();
    rows
}

/// An unknown `?project=` key, including a traversal-shaped one, is refused
/// with an error instead of ever becoming a path (I-S5). The store db path
/// is built from the enumerated data-dir entry, never from the request; this
/// pins that refusal at the HTTP surface.
#[test]
fn ui_refuses_unknown_and_traversal_project_keys() {
    let tmp = scratch();
    // A real store OUTSIDE the data dir, reachable only if resolve_project
    // ever builds a path from the request string: data/../outside/store.db
    // exists and opens, so a path-joining implementation would serve it with
    // a 200 where the enumerating one refuses the key. This is the mutation
    // the 2026-08-25 review proved the previous test blind to.
    {
        let decoy = Store::open(&tmp.path().join("outside").join("store.db"))
            .expect("decoy store");
        seed_entry(&decoy, "a store outside the data dir must be unreachable");
    }
    let (_child, port, data, _root) = spawn_ui(&tmp);

    for key in [
        "zzz-no-such-project",
        "../../../../etc",
        "..%2F..%2Fetc",
        "../outside",
    ] {
        let raw = ui_get_raw(port, &format!("/api/graph?project={key}"))
            .unwrap_or_else(|| panic!("no response for key {key}"));
        assert!(
            raw.contains("500") && raw.contains("unknown project key"),
            "key '{key}' was not refused as unknown: {raw}"
        );
    }
    // The traversal attempts must not have created anything under data.
    let created: Vec<_> = std::fs::read_dir(&data)
        .expect("read data dir")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains("etc") || n.contains("outside"))
        .collect();
    assert!(created.is_empty(), "traversal input reached the filesystem: {created:?}");
}

/// Unknown routes 404; the surface is exactly the enumerated handlers.
#[test]
fn ui_unknown_routes_get_404() {
    let tmp = scratch();
    let (_child, port, _data, _root) = spawn_ui(&tmp);
    for path in ["/nope", "/api/nope", "/../store.db", "/api/graph/../../x"] {
        let raw = ui_get_raw(port, path).unwrap_or_else(|| panic!("no response for {path}"));
        let status = raw.lines().next().unwrap_or_default().to_string();
        assert!(
            status.contains("404"),
            "route {path} did not 404: {status}"
        );
    }
}

/// A full request sweep, hostile inputs included, leaves the store's logical
/// content identical: the ui is read-only in effect, not just in intent.
#[test]
fn ui_request_sweep_leaves_store_content_identical() {
    let tmp = scratch();
    let (child, port, data, _root) = spawn_ui(&tmp);

    let db = {
        let mut found = None;
        for e in std::fs::read_dir(&data).expect("data dir").flatten() {
            let candidate = e.path().join("store.db");
            if candidate.is_file() {
                found = Some(candidate);
            }
        }
        found.expect("seeded store exists")
    };
    let before = content_dump(&db);
    assert!(!before.is_empty(), "fixture store has content");

    for path in [
        "/", "/api/projects", "/api/graph", "/api/ledger",
        "/api/graph?project=all", "/api/graph?project=../../x", "/nope",
    ] {
        let _ = ui_get_raw(port, path);
    }
    drop(child);

    let after = content_dump(&db);
    assert_eq!(before, after, "a ui request changed store content");
}

/// The open-time guard alone leaves a seam: a handle that passed it while
/// the store was old keeps writing after a newer binary migrates the store
/// forward through a surface that never stamps code_version (`limpet ui`).
/// version_guard, which every tool call runs, must therefore refuse on the
/// schema stamp too (I-S1's live-handle half, 2026-08-25 review).
#[test]
fn version_guard_refuses_a_newer_schema_on_a_live_handle() {
    let dir = scratch();
    let db = dir.path().join("store.db");
    let live = Store::open(&db).expect("open live handle");
    live.version_guard().expect("guard passes at current schema");
    {
        let raw = rusqlite::Connection::open(&db).expect("raw open");
        raw.execute(
            "UPDATE meta_kv SET v = ?1 WHERE k = 'schema_version'",
            [(SCHEMA_VERSION + 1).to_string()],
        )
        .expect("simulate a newer binary's migration");
    }
    let err = match live.version_guard() {
        Ok(()) => panic!("a live handle kept writing into a newer-schema store"),
        Err(e) => format!("{e:#}"),
    };
    assert!(
        err.contains(&(SCHEMA_VERSION + 1).to_string()) && err.contains("schema"),
        "the refusal names the newer schema: {err}"
    );
}

/// A failed bootstrap import is retried on the next index instead of burning
/// the one shot: `full_index` stamps `indexed_at` before the import runs, so
/// without the retry marker one bad `.limpet/memory.jsonl` would mean the
/// shared memory is silently never delivered (2026-08-25 review).
#[test]
fn a_failed_bootstrap_import_retries_on_the_next_index() {
    let dir = scratch();
    let root = dir.path().join("repo");
    std::fs::create_dir_all(root.join(".limpet")).expect("mkdir");
    std::fs::write(
        root.join(".limpet").join("memory.jsonl"),
        "{\"limpet_export\":99,\"schema_version\":99}\n",
    )
    .expect("write future-format file");
    let mut store = Store::open(&dir.path().join("store.db")).expect("open");

    let err = limpet::index::index_and_bootstrap(&mut store, &root);
    assert!(err.is_err(), "the future-format bootstrap import must fail loudly");

    // The fix arrives (a current binary, or a repaired file); the next index
    // must deliver the memory even though indexed_at is already stamped.
    std::fs::write(
        root.join(".limpet").join("memory.jsonl"),
        format!(
            "{}\n{}\n",
            "{\"limpet_export\":1,\"schema_version\":8}",
            r#"{"id":"01RETRYAFTERFAILURE0000000","kind":"fact","body":"the retry marker survives until an import succeeds","created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z","source":"explicit","confidence":0.5,"status":"active","stale_reason":null,"branch":"main","evidence_cmd":null,"evidence_digest":null,"evidence_ran_at":null,"origin":null,"links":[],"anchors":[]}"#
        ),
    )
    .expect("write repaired file");
    let (_, import) = limpet::index::index_and_bootstrap(&mut store, &root)
        .expect("second index succeeds");
    let report = import.expect("the retry actually imported");
    assert_eq!(report.added, 1, "the shared memory finally landed: {report:?}");

    // And the marker is consumed: a third index does not import again.
    let (_, third) = limpet::index::index_and_bootstrap(&mut store, &root)
        .expect("third index succeeds");
    assert!(third.is_none(), "the retry marker must clear after success");
}

/// A line carrying BOTH the header marker and entry data is no shape any
/// exporter writes; it is treated as data (the marker is an ignored unknown
/// field), so it lands in the counts instead of vanishing.
#[test]
fn a_line_with_header_marker_and_id_is_counted_as_data() {
    let dir = scratch();
    let mut dst = Store::open(&dir.path().join("b.db")).expect("open");
    let line = r#"{"limpet_export":1,"id":"01MARKERANDDATA00000000000","kind":"fact","body":"a marker riding on an entry is still an entry","created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z","source":"explicit","confidence":0.5,"status":"active","stale_reason":null,"branch":"main","evidence_cmd":null,"evidence_digest":null,"evidence_ran_at":null,"origin":null,"links":[],"anchors":[]}"#;
    let mut input = std::io::BufReader::new(line.as_bytes());
    let report = dst.import_jsonl(&mut input).expect("import");
    assert_eq!(report.added, 1, "the entry landed despite the stray marker: {report:?}");
}

/// A header whose marker is not an integer is refused honestly, without the
/// error inventing a number the file never claimed.
#[test]
fn a_non_integer_format_marker_is_refused_without_fabrication() {
    let dir = scratch();
    let mut dst = Store::open(&dir.path().join("b.db")).expect("open");
    let mut input = std::io::BufReader::new("{\"limpet_export\":\"1\"}\n".as_bytes());
    let err = match dst.import_jsonl(&mut input) {
        Ok(r) => panic!("a non-integer marker imported: {r:?}"),
        Err(e) => format!("{e:#}"),
    };
    assert!(err.contains("unrecognized"), "honest refusal: {err}");
    assert!(
        !err.contains("9223372036854775807"),
        "the error must not fabricate a format number: {err}"
    );
}

/// A store at exactly this binary's schema opens exactly as before: the
/// guard only fires on FUTURE stamps.
#[test]
fn a_current_schema_store_opens_normally() {
    let dir = scratch();
    let db = dir.path().join("store.db");
    { Store::open(&db).expect("fresh open"); }
    let s = Store::open(&db).expect("reopen at current schema");
    assert_eq!(
        s.kv_get("schema_version").expect("kv").as_deref(),
        Some(SCHEMA_VERSION.to_string().as_str())
    );
}
