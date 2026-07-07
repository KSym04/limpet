//! v0.9.0 Unit 2: repo-level config (`.limpet.json`) and the extension
//! override map. The config is repo-controlled and treated as untrusted
//! (invariant I-P3): bounded size, values validated against the six known
//! grammars, pure lookup, no execution.

use limpet::config::RepoConfig;
use limpet::index::lang::{self, Lang};
use limpet::index;
use limpet::store::Store;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

fn write_config(dir: &Path, body: &str) {
    fs::write(dir.join(".limpet.json"), body).unwrap();
}

#[test]
fn missing_config_is_default() {
    let dir = TempDir::new().unwrap();
    let cfg = RepoConfig::load(dir.path()).unwrap();
    assert!(cfg.extensions.is_empty());
    assert!(cfg.auto_import, "auto_import defaults on");
}

#[test]
fn parses_extension_map_and_auto_import() {
    let dir = TempDir::new().unwrap();
    write_config(
        dir.path(),
        r#"{ "extensions": { "inc": "cpp", "module": "php" }, "auto_import": false }"#,
    );
    let cfg = RepoConfig::load(dir.path()).unwrap();
    assert_eq!(cfg.extensions.get("inc"), Some(&Lang::Cpp));
    assert_eq!(cfg.extensions.get("module"), Some(&Lang::Php));
    assert!(!cfg.auto_import);
}

#[test]
fn unknown_grammar_value_is_rejected() {
    let dir = TempDir::new().unwrap();
    write_config(dir.path(), r#"{ "extensions": { "inc": "cobol" } }"#);
    let err = RepoConfig::load(dir.path()).unwrap_err();
    assert!(
        err.to_string().to_lowercase().contains("cobol"),
        "error should name the offending grammar: {err}"
    );
}

#[test]
fn malformed_json_is_rejected() {
    let dir = TempDir::new().unwrap();
    write_config(dir.path(), "{ not json");
    assert!(RepoConfig::load(dir.path()).is_err());
}

#[test]
fn oversize_config_is_rejected() {
    let dir = TempDir::new().unwrap();
    let big = format!(
        r#"{{ "extensions": {{ "x": "php" }}, "_pad": "{}" }}"#,
        "a".repeat(70 * 1024)
    );
    write_config(dir.path(), &big);
    assert!(RepoConfig::load(dir.path()).is_err(), "config over the size cap must be rejected");
}

#[test]
fn detect_with_override_maps_unknown_extension() {
    let map: HashMap<String, Lang> = [("inc".to_string(), Lang::Cpp)].into_iter().collect();
    assert_eq!(lang::detect_with(Path::new("legacy/foo.inc"), &map), Some(Lang::Cpp));
}

#[test]
fn detect_with_longest_suffix_wins() {
    // A longer, more specific user key beats a shorter one. Contrived value
    // (Ts for blade) purely to prove longest-suffix precedence, not realism.
    let map: HashMap<String, Lang> = [
        ("php".to_string(), Lang::Php),
        ("blade.php".to_string(), Lang::Ts),
    ]
    .into_iter()
    .collect();
    assert_eq!(lang::detect_with(Path::new("views/home.blade.php"), &map), Some(Lang::Ts));
}

#[test]
fn detect_with_falls_back_to_builtin() {
    let map: HashMap<String, Lang> = HashMap::new();
    assert_eq!(lang::detect_with(Path::new("src/main.rs"), &map), Some(Lang::Rust));
    assert_eq!(lang::detect_with(Path::new("README.md"), &map), None);
}

#[test]
fn full_index_honors_extension_override() {
    // A `.inc` file is unknown to the built-in grammar table; the override
    // makes it parse as C++, so the function symbol is extracted instead of
    // the file landing as a symbol-less file-level row.
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    fs::write(root.join(".limpet.json"), r#"{ "extensions": { "inc": "cpp" } }"#).unwrap();
    fs::write(root.join("legacy.inc"), "int add(int a, int b) { return a + b; }\n").unwrap();

    let store = Store::open_in_memory().unwrap();
    index::full_index(&store, root).unwrap();

    let lang: Option<String> = store
        .conn
        .query_row("SELECT lang FROM files WHERE path = 'legacy.inc'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(lang.as_deref(), Some("cpp"), "override should tag legacy.inc as C++");

    let symbols: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM symbols WHERE file = 'legacy.inc'", [], |r| r.get(0))
        .unwrap();
    assert!(symbols >= 1, "C++ parse of legacy.inc should extract the add() function");
}

/// Write a committed `.limpet/memory.jsonl` at `root` carrying one entry,
/// produced by a real export so the format is authoritative.
fn seed_committed_memory(root: &Path) {
    fs::create_dir_all(root.join(".limpet")).unwrap();
    let src = Store::open_in_memory().unwrap();
    src.conn
        .execute(
            "INSERT INTO entries(id,kind,body,created_at,updated_at,source,confidence)
             VALUES('e1','fact','seeded from a teammate','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z','explicit',0.8)",
            [],
        )
        .unwrap();
    let mut buf: Vec<u8> = Vec::new();
    src.export_jsonl(&mut buf).unwrap();
    fs::write(root.join(".limpet/memory.jsonl"), &buf).unwrap();
}

#[test]
fn auto_imports_committed_memory_on_fresh_index() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    seed_committed_memory(root);

    let mut store = Store::open_in_memory().unwrap();
    let (_idx, import) = index::index_and_bootstrap(&mut store, root).unwrap();
    assert!(import.is_some(), "a fresh store with committed memory.jsonl should auto-import");

    let n: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM entries WHERE id = 'e1'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1, "the committed memory should be seeded into the fresh store");
}

#[test]
fn does_not_auto_import_when_store_already_indexed() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    seed_committed_memory(root);

    let mut store = Store::open_in_memory().unwrap();
    store.kv_set("indexed_at", "2020-01-01T00:00:00Z").unwrap(); // pretend already indexed
    let (_idx, import) = index::index_and_bootstrap(&mut store, root).unwrap();
    assert!(import.is_none(), "an already-indexed store must not re-import");
}

#[test]
fn auto_import_can_be_disabled_by_config() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    seed_committed_memory(root);
    fs::write(root.join(".limpet.json"), r#"{ "auto_import": false }"#).unwrap();

    let mut store = Store::open_in_memory().unwrap();
    let (_idx, import) = index::index_and_bootstrap(&mut store, root).unwrap();
    assert!(import.is_none(), "auto_import:false must skip the bootstrap");
}
