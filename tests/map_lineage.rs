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
