//! Integration tests for the lineage block emitted by `map` for symbol targets.

#[test]
fn map_symbol_target_emits_lineage() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("a.py"),
        "class Animal:\n    pass\nclass Dog(Animal):\n    pass\n",
    )
    .unwrap();
    // Use open_in_memory so store_exclude_dir returns None and the tempdir is
    // not mistakenly excluded from the index walk (store.db directly in the
    // tempdir would cause its parent to be excluded).
    let store = limpet::store::Store::open_in_memory().unwrap();
    limpet::index::full_index(&store, dir.path()).unwrap();

    let fqn: String = store
        .conn
        .query_row("SELECT fqn FROM symbols WHERE name='Dog'", [], |r| r.get(0))
        .unwrap();
    let args = serde_json::json!({ "target": fqn });
    let resp = limpet::tools::dispatch_for_test("map", &store, &args).unwrap();
    let lineage = &resp["data"]["lineage"];
    assert!(!lineage.is_null(), "symbol target has a lineage block");
    let anc = lineage["ancestors"].as_array().unwrap();
    assert!(
        anc.iter().any(|e| e["fqn"].as_str().unwrap().ends_with("Animal")
            && e["resolved"] == "unique"),
        "Animal ancestor must be present with resolved==unique, got: {anc:?}"
    );
}

/// Index one fixture file into a fresh in-memory store and return the store.
///
/// `open_in_memory` keeps `store_exclude_dir` at None so the tempdir is walked
/// in full (a store.db inside the tempdir would exclude its own parent).
fn indexed(name: &str, body: &str) -> (tempfile::TempDir, limpet::store::Store) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(name), body).unwrap();
    let store = limpet::store::Store::open_in_memory().unwrap();
    limpet::index::full_index(&store, dir.path()).unwrap();
    (dir, store)
}

/// The `map` descendants for the symbol whose FQN is `fqn`, as bare strings.
fn descendant_fqns(store: &limpet::store::Store, fqn: &str) -> Vec<String> {
    let args = serde_json::json!({ "target": fqn });
    let resp = limpet::tools::dispatch_for_test("map", store, &args).unwrap();
    let lineage = &resp["data"]["lineage"];
    assert!(!lineage.is_null(), "symbol target {fqn} must have a lineage block");
    lineage["descendants"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["fqn"].as_str().unwrap().to_string())
        .collect()
}

fn sym_fqn(store: &limpet::store::Store, name: &str) -> String {
    store
        .conn
        .query_row("SELECT fqn FROM symbols WHERE name = ?1", [name], |r| r.get(0))
        .unwrap()
}

/// Regression (shipped 0.11): `calls.caller_fqn` was built from an EMPTY
/// parents vector, so it was always `<file>.<innermost name>` and never the
/// caller's real `symbols.fqn`. A C++ out-of-line definition is a member of
/// its qualifier, so its FQN carries that qualifier and the join missed.
#[test]
fn cpp_out_of_line_definition_has_call_descendants() {
    let (_d, s) = indexed(
        "a.cpp",
        "class A { public: void run(); };\n\
         void A::run() { Lookup::Find(id); }\n\
         class Lookup { public: static void Find(int i); };\n",
    );
    let run = sym_fqn(&s, "run");
    assert_eq!(run, "a.A.run", "out-of-line def is a member of its qualifier");
    let desc = descendant_fqns(&s, &run);
    assert!(
        desc.iter().any(|f| f.ends_with("Find")),
        "A::run must expose its Lookup::Find callee, got: {desc:?}"
    );
}

/// The pre-existing half of the same break: every method inside a class has an
/// FQN of `<file>.<Class>.<method>`, which the flat caller_fqn never matched.
#[test]
fn method_inside_class_has_call_descendants() {
    let (_d, s) = indexed(
        "a.py",
        "def target():\n    pass\n\nclass C:\n    def m(self):\n        target()\n",
    );
    let m = sym_fqn(&s, "m");
    assert_eq!(m, "a.C.m");
    let desc = descendant_fqns(&s, &m);
    assert!(
        desc.iter().any(|f| f.ends_with("target")),
        "C.m must expose its target() callee, got: {desc:?}"
    );
}

/// A Rust `fn` inside an inline `mod` gets the module as a parent scope, so it
/// broke the same way the moment inline mods started contributing scope.
#[test]
fn rust_fn_in_inline_mod_has_call_descendants() {
    let (_d, s) = indexed(
        "a.rs",
        "fn helper() {}\nmod inner {\n    pub fn caller() { helper(); }\n}\n",
    );
    let caller = sym_fqn(&s, "caller");
    assert_eq!(caller, "a.inner.caller");
    let desc = descendant_fqns(&s, &caller);
    assert!(
        desc.iter().any(|f| f.ends_with("helper")),
        "inner::caller must expose its helper() callee, got: {desc:?}"
    );
}

/// The `<file>` sentinel scope (a call made at file top level) must keep
/// producing `<file path>.<file>` so top-level call edges stay recorded.
#[test]
fn top_level_call_keeps_file_sentinel_scope() {
    let (_d, s) = indexed("a.py", "def target():\n    pass\n\ntarget()\n");
    let callers: Vec<String> = s
        .conn
        .prepare("SELECT caller_fqn FROM calls WHERE callee_name = 'target'")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(std::result::Result::unwrap)
        .collect();
    assert!(
        callers.iter().any(|c| c == "a.<file>"),
        "top-level call keeps the <file> sentinel, got: {callers:?}"
    );
}

/// The callers direction is keyed on the callee's last segment, so it must
/// keep working AND now report the caller by its real FQN.
#[test]
fn callers_direction_reports_real_caller_fqn() {
    let (_d, s) = indexed(
        "a.py",
        "def target():\n    pass\n\nclass C:\n    def m(self):\n        target()\n",
    );
    let target = sym_fqn(&s, "target");
    let args = serde_json::json!({ "target": target });
    let resp = limpet::tools::dispatch_for_test("map", &s, &args).unwrap();
    let callers: Vec<String> = resp["data"]["lineage"]["callers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["fqn"].as_str().unwrap().to_string())
        .collect();
    assert!(
        callers.iter().any(|c| c == "a.C.m"),
        "caller must be reported by its real FQN, got: {callers:?}"
    );
}

#[test]
fn map_file_target_has_no_lineage() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.py"), "class Animal:\n    pass\n").unwrap();
    let store = limpet::store::Store::open_in_memory().unwrap();
    limpet::index::full_index(&store, dir.path()).unwrap();
    let args = serde_json::json!({ "target": "a.py" });
    let resp = limpet::tools::dispatch_for_test("map", &store, &args).unwrap();
    assert!(resp["data"]["lineage"].is_null(), "file target keeps old shape");
}
