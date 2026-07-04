//! Golden corpus for the anchor mechanism (spec section 4.3).
//!
//! These tests are the product. Each seeds a memory anchored to a symbol,
//! mutates the code the way real development does (reformat, comment,
//! rename, edit, move, delete, duplicate), then asserts the exact status
//! transition. A failure here means limpet lies about memory validity.

use limpet::index::{self, lang::Lang};
use limpet::memory::{self, anchor, AnchorSpec};
use limpet::store::Store;
use std::fs;
use tempfile::TempDir;

const ORIGINAL: &str = r#"
def compute_health_score(issues):
    critical = sum(1 for i in issues if i.level == "critical")
    total = len(issues)
    return 100 - critical * 10 - (total - critical) * 2
"#;

fn seed(root: &std::path::Path) -> (Store, String) {
    fs::write(root.join("score.py"), ORIGINAL).unwrap();
    let store = Store::open_in_memory().unwrap();
    index::full_index(&store, root).unwrap();
    let result = memory::remember(
        &store,
        "fact",
        "health score subtracts 10 per critical issue, 2 per non-critical",
        "explicit",
        None,
        &[AnchorSpec { file: "score.py".into(), symbol: Some("compute_health_score".into()) }],
        None,
        &[],
        None,
        false,
        None,
    )
    .unwrap();
    (store, result.id)
}

fn status_of(store: &Store, id: &str) -> (String, Option<String>) {
    store
        .conn
        .query_row(
            "SELECT status, stale_reason FROM entries WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
}

fn mutate_and_resolve(store: &Store, root: &std::path::Path) -> anchor::ResolveReport {
    std::thread::sleep(std::time::Duration::from_millis(20));
    index::sweep(store, root).unwrap();
    anchor::resolve_all(store).unwrap()
}

#[test]
fn untouched_code_stays_active() {
    let dir = TempDir::new().unwrap();
    let (store, id) = seed(dir.path());
    let report = mutate_and_resolve(&store, dir.path());
    assert_eq!(report.fresh, 1);
    assert_eq!(status_of(&store, &id).0, "active");
}

#[test]
fn reformat_and_comments_stay_active() {
    let dir = TempDir::new().unwrap();
    let (store, id) = seed(dir.path());
    // Same AST identity: extra blank lines, comment added, spacing changed.
    fs::write(
        dir.path().join("score.py"),
        r#"
def compute_health_score(issues):
    # criticals hurt the most
    critical = sum(1 for i in issues if i.level == "critical")

    total = len(issues)
    return 100 - critical * 10 - (total - critical) * 2
"#,
    )
    .unwrap();
    let report = mutate_and_resolve(&store, dir.path());
    assert_eq!(report.fresh, 1, "reformatting must not stale a memory");
    assert_eq!(status_of(&store, &id).0, "active");
}

#[test]
fn rename_is_followed() {
    let dir = TempDir::new().unwrap();
    let (store, id) = seed(dir.path());
    fs::write(
        dir.path().join("score.py"),
        ORIGINAL.replace("compute_health_score", "calculate_health_score"),
    )
    .unwrap();
    let report = mutate_and_resolve(&store, dir.path());
    // Body hash includes the parameter identifiers but the defining name
    // node too; a pure rename of the function keeps the body statements
    // identical, so the anchor either stays fresh (hash covers body only)
    // or is followed by body match. Either way the memory must stay active
    // and the anchor must point at the new FQN.
    assert_eq!(status_of(&store, &id).0, "active", "rename must not kill memory");
    assert_eq!(report.invalidated, 0);
    let fqn: String = store
        .conn
        .query_row("SELECT symbol_fqn FROM anchors LIMIT 1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(fqn, "score.calculate_health_score");
}

#[test]
fn file_move_is_followed() {
    let dir = TempDir::new().unwrap();
    let (store, id) = seed(dir.path());
    fs::create_dir_all(dir.path().join("lib")).unwrap();
    fs::remove_file(dir.path().join("score.py")).unwrap();
    fs::write(dir.path().join("lib/scoring.py"), ORIGINAL).unwrap();
    let _ = mutate_and_resolve(&store, dir.path());
    assert_eq!(status_of(&store, &id).0, "active", "file move must not kill memory");
    let (fqn, file): (String, String) = store
        .conn
        .query_row("SELECT symbol_fqn, file FROM anchors LIMIT 1", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(fqn, "lib.scoring.compute_health_score");
    assert_eq!(file, "lib/scoring.py");
}

#[test]
fn real_edit_goes_stale_with_reason_and_confidence_drop() {
    let dir = TempDir::new().unwrap();
    let (store, id) = seed(dir.path());
    let conf_before: f64 = store
        .conn
        .query_row("SELECT confidence FROM entries WHERE id = ?1", [&id], |r| r.get(0))
        .unwrap();
    // The scoring weights change: the memorized fact is now suspect.
    fs::write(
        dir.path().join("score.py"),
        ORIGINAL.replace("critical * 10", "critical * 25"),
    )
    .unwrap();
    let report = mutate_and_resolve(&store, dir.path());
    assert_eq!(report.stale, 1);
    let (status, reason) = status_of(&store, &id);
    assert_eq!(status, "stale");
    assert_eq!(reason.as_deref(), Some("body_edited"));
    let conf_after: f64 = store
        .conn
        .query_row("SELECT confidence FROM entries WHERE id = ?1", [&id], |r| r.get(0))
        .unwrap();
    assert!(conf_after < conf_before, "stale memory must lose confidence");
}

#[test]
fn deletion_invalidates() {
    let dir = TempDir::new().unwrap();
    let (store, id) = seed(dir.path());
    fs::write(dir.path().join("score.py"), "def unrelated():\n    return 0\n").unwrap();
    let report = mutate_and_resolve(&store, dir.path());
    assert_eq!(report.invalidated, 1);
    let (status, reason) = status_of(&store, &id);
    assert_eq!(status, "invalidated");
    assert_eq!(reason.as_deref(), Some("anchor_deleted"));
}

#[test]
fn duplicate_bodies_go_ambiguous_not_guessed() {
    let dir = TempDir::new().unwrap();
    let (store, id) = seed(dir.path());
    // Original disappears; two identical copies appear under new names.
    let dup = ORIGINAL.replace("compute_health_score", "score_a")
        + &ORIGINAL.replace("compute_health_score", "score_b");
    fs::write(dir.path().join("score.py"), dup).unwrap();
    let report = mutate_and_resolve(&store, dir.path());
    assert_eq!(report.stale, 1, "ambiguity must be reported, never guessed");
    let (status, reason) = status_of(&store, &id);
    assert_eq!(status, "stale");
    assert_eq!(reason.as_deref(), Some("ambiguous_anchor"));
}

#[test]
fn verified_fact_gets_reverify_flag_when_stale() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("score.py"), ORIGINAL).unwrap();
    let store = Store::open_in_memory().unwrap();
    index::full_index(&store, dir.path()).unwrap();
    let result = memory::remember(
        &store,
        "fact",
        "score of 3 criticals and 2 warnings is 66",
        "explicit",
        None,
        &[AnchorSpec { file: "score.py".into(), symbol: Some("compute_health_score".into()) }],
        Some(&memory::Evidence {
            command: "python -m pytest tests/test_score.py -q".into(),
            output: "1 passed".into(),
        }),
        &[],
        None,
        false,
        None,
    )
    .unwrap();

    fs::write(
        dir.path().join("score.py"),
        ORIGINAL.replace("* 2", "* 5"),
    )
    .unwrap();
    let _ = mutate_and_resolve(&store, dir.path());

    let conf: f64 = store
        .conn
        .query_row("SELECT confidence FROM entries WHERE id = ?1", [&result.id], |r| r.get(0))
        .unwrap();
    assert!(conf <= 0.5, "stale verified fact must drop to <= 0.5, got {conf}");

    let out = memory::recall::recall(&store, "health score criticals warnings", &[], 2000).unwrap();
    let item = out.items.iter().find(|i| i.id == result.id).expect("stale item must surface");
    assert!(item.flags.iter().any(|f| f.starts_with("stale:")));
    assert!(
        item.flags.iter().any(|f| f == "reverify:python -m pytest tests/test_score.py -q"),
        "flags: {:?}",
        item.flags
    );
}

#[test]
fn file_anchor_goes_stale_on_edit_and_invalidated_on_delete() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    fs::write(root.join("interior.twig"), "{% block hero %}old{% endblock %}\n").unwrap();
    let store = Store::open_in_memory().unwrap();
    index::full_index(&store, root).unwrap();

    let result = memory::remember(
        &store,
        "insight",
        "hero block height is locked to 480px by the design system",
        "explicit",
        None,
        &[AnchorSpec { file: "interior.twig".into(), symbol: None }],
        None,
        &[],
        None,
        false,
        None,
    )
    .unwrap();
    assert_eq!(result.anchored, 1);

    // Untouched: stays active.
    let report = mutate_and_resolve(&store, root);
    assert_eq!(report.fresh, 1);
    assert_eq!(status_of(&store, &result.id).0, "active");

    // Edited: stale with file_edited.
    fs::write(root.join("interior.twig"), "{% block hero %}new{% endblock %}\n").unwrap();
    let report = mutate_and_resolve(&store, root);
    assert_eq!(report.stale, 1, "editing an anchored file must stale the memory");
    let (status, reason) = status_of(&store, &result.id);
    assert_eq!(status, "stale");
    assert_eq!(reason.as_deref(), Some("file_edited"));

    // Deleted: invalidated.
    fs::remove_file(root.join("interior.twig")).unwrap();
    let report = mutate_and_resolve(&store, root);
    assert_eq!(report.invalidated, 1);
    let (status, reason) = status_of(&store, &result.id);
    assert_eq!(status, "invalidated");
    assert_eq!(reason.as_deref(), Some("anchor_deleted"));
}

#[test]
fn legacy_file_anchor_without_hash_is_backfilled_not_killed() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    fs::write(root.join("page.twig"), "{% block a %}{% endblock %}\n").unwrap();
    let store = Store::open_in_memory().unwrap();
    index::full_index(&store, root).unwrap();

    let result = memory::remember(
        &store,
        "fact",
        "page template renders the a block",
        "explicit",
        None,
        &[AnchorSpec { file: "page.twig".into(), symbol: None }],
        None,
        &[],
        None,
        false,
        None,
    )
    .unwrap();
    // Simulate a v0.4.0 store where file anchors carried no hash.
    store
        .conn
        .execute("UPDATE anchors SET ast_body_hash = NULL WHERE entry_id = ?1", [&result.id])
        .unwrap();

    let report = mutate_and_resolve(&store, root);
    assert_eq!(report.fresh, 1, "legacy anchor must be adopted, not stale/killed");
    assert_eq!(status_of(&store, &result.id).0, "active");
    let hash: Option<String> = store
        .conn
        .query_row("SELECT ast_body_hash FROM anchors WHERE entry_id = ?1", [&result.id], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(hash.is_some(), "backfill must store the current content hash");
}

#[test]
fn one_dead_anchor_degrades_multi_anchor_memory_instead_of_killing_it() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    fs::write(root.join("score.py"), ORIGINAL).unwrap();
    fs::write(root.join("interior.twig"), "{% block hero %}{% endblock %}\n").unwrap();
    let store = Store::open_in_memory().unwrap();
    index::full_index(&store, root).unwrap();

    let result = memory::remember(
        &store,
        "insight",
        "health score is rendered by the hero block",
        "explicit",
        None,
        &[
            AnchorSpec { file: "score.py".into(), symbol: Some("compute_health_score".into()) },
            AnchorSpec { file: "interior.twig".into(), symbol: None },
        ],
        None,
        &[],
        None,
        false,
        None,
    )
    .unwrap();
    assert_eq!(result.anchored, 2);

    // One anchor dies; the other still resolves.
    fs::remove_file(root.join("interior.twig")).unwrap();
    let report = mutate_and_resolve(&store, root);
    assert_eq!(report.invalidated, 1);
    assert_eq!(report.fresh, 1);
    let (status, reason) = status_of(&store, &result.id);
    assert_eq!(status, "stale", "a memory with a live anchor must not be invalidated");
    assert_eq!(reason.as_deref(), Some("anchor_lost"));

    // Both anchors dead: now it is genuinely invalidated.
    fs::write(root.join("score.py"), "def unrelated():\n    return 0\n").unwrap();
    let _ = mutate_and_resolve(&store, root);
    let (status, reason) = status_of(&store, &result.id);
    assert_eq!(status, "invalidated");
    assert_eq!(reason.as_deref(), Some("anchor_deleted"));
}

#[test]
fn transient_deletion_heals_after_restore() {
    // Branch switches, git stash, and mid-rebase states make files vanish
    // briefly. Invalidation must not be a death sentence: when the code
    // comes back, the memory recovers (audit 2026-07).
    let dir = TempDir::new().unwrap();
    let (store, id) = seed(dir.path());

    fs::remove_file(dir.path().join("score.py")).unwrap();
    let _ = mutate_and_resolve(&store, dir.path());
    assert_eq!(status_of(&store, &id).0, "invalidated");

    fs::write(dir.path().join("score.py"), ORIGINAL).unwrap();
    let _ = mutate_and_resolve(&store, dir.path());
    assert_eq!(
        status_of(&store, &id).0,
        "active",
        "restored code must resurrect the memory"
    );
}

#[test]
fn superseded_never_resurrects() {
    let dir = TempDir::new().unwrap();
    let (store, id) = seed(dir.path());
    let newer = memory::remember(
        &store,
        "fact",
        "score subtracts twenty five per critical now",
        "explicit",
        None,
        &[AnchorSpec { file: "score.py".into(), symbol: Some("compute_health_score".into()) }],
        None,
        &[memory::LinkSpec { target: id.clone(), rel: "supersedes".into() }],
        None,
        false,
        None,
    )
    .unwrap();
    let _ = mutate_and_resolve(&store, dir.path());
    assert_eq!(status_of(&store, &id).0, "superseded", "supersession is final");
    assert_eq!(status_of(&store, &newer.id).0, "active");
}

#[test]
fn stale_confidence_penalty_applies_once() {
    let dir = TempDir::new().unwrap();
    let (store, id) = seed(dir.path());
    let conf_before: f64 = store
        .conn
        .query_row("SELECT confidence FROM entries WHERE id = ?1", [&id], |r| r.get(0))
        .unwrap();

    fs::write(dir.path().join("score.py"), ORIGINAL.replace("* 2", "* 9")).unwrap();
    let _ = mutate_and_resolve(&store, dir.path());
    let conf_first: f64 = store
        .conn
        .query_row("SELECT confidence FROM entries WHERE id = ?1", [&id], |r| r.get(0))
        .unwrap();
    assert!(conf_first < conf_before, "transition must drop confidence");

    // Further resolves with no code change must NOT keep compounding: the
    // penalty applies on the active->stale transition only (audit 2026-07).
    let _ = mutate_and_resolve(&store, dir.path());
    let _ = mutate_and_resolve(&store, dir.path());
    let conf_after: f64 = store
        .conn
        .query_row("SELECT confidence FROM entries WHERE id = ?1", [&id], |r| r.get(0))
        .unwrap();
    assert!(
        (conf_after - conf_first).abs() < 1e-9,
        "stale penalty compounded: {conf_first} -> {conf_after}"
    );
}

#[test]
fn cpp_rename_is_followed() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let original = "int compute_score(int a) {\n    return a * 10 + 7;\n}\n";
    fs::write(root.join("engine.cpp"), original).unwrap();
    let store = Store::open_in_memory().unwrap();
    index::full_index(&store, root).unwrap();
    let result = memory::remember(
        &store,
        "fact",
        "score multiplies by ten and adds seven",
        "explicit",
        None,
        &[AnchorSpec { file: "engine.cpp".into(), symbol: Some("compute_score".into()) }],
        None,
        &[],
        None,
        false,
        None,
    )
    .unwrap();

    // A pure rename: the declarator name is excluded from the hash, so the
    // anchor must FOLLOW, not stale or die (audit 2026-07: C++ names live
    // in the declarator chain, not a `name` field).
    fs::write(root.join("engine.cpp"), original.replace("compute_score", "calc_score")).unwrap();
    let _ = mutate_and_resolve(&store, root);
    assert_eq!(status_of(&store, &result.id).0, "active", "C++ rename must not kill memory");
    let fqn: String = store
        .conn
        .query_row(
            "SELECT symbol_fqn FROM anchors WHERE entry_id = ?1",
            [&result.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(fqn, "engine.calc_score");
}

#[test]
fn file_anchor_follows_a_move() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    fs::write(root.join("hero.twig"), "{% block hero %}x{% endblock %}\n").unwrap();
    let store = Store::open_in_memory().unwrap();
    index::full_index(&store, root).unwrap();
    let result = memory::remember(
        &store,
        "insight",
        "hero height is design locked",
        "explicit",
        None,
        &[AnchorSpec { file: "hero.twig".into(), symbol: None }],
        None,
        &[],
        None,
        false,
        None,
    )
    .unwrap();

    // git mv: same bytes, new path. File anchors get the same follow
    // courtesy symbol anchors always had (audit 2026-07).
    fs::create_dir_all(root.join("views")).unwrap();
    fs::rename(root.join("hero.twig"), root.join("views/hero.twig")).unwrap();
    let _ = mutate_and_resolve(&store, root);
    assert_eq!(status_of(&store, &result.id).0, "active", "moved file must be followed");
    let file: String = store
        .conn
        .query_row("SELECT file FROM anchors WHERE entry_id = ?1", [&result.id], |r| r.get(0))
        .unwrap();
    assert_eq!(file, "views/hero.twig");
}

#[test]
fn hash_properties_hold_per_language() {
    // Same-body equality and edit sensitivity for each shipped grammar.
    let cases: Vec<(Lang, &str, &str, &str)> = vec![
        (
            Lang::Py,
            "def f(a):\n    return a + 1\n",
            "def f(a):\n\n    return a + 1  # comment\n",
            "def f(a):\n    return a + 2\n",
        ),
        (
            Lang::Js,
            "function f(a) { return a + 1; }",
            "function f(a) {\n  // c\n  return a + 1;\n}",
            "function f(a) { return a + 2; }",
        ),
        (
            Lang::Ts,
            "function f(a: number) { return a + 1; }",
            "function f(a: number) {\n  return a + 1; // c\n}",
            "function f(a: number) { return a + 2; }",
        ),
        (
            Lang::Php,
            "<?php\nfunction f($a) { return $a + 1; }",
            "<?php\nfunction f($a) {\n  // c\n  return $a + 1;\n}",
            "<?php\nfunction f($a) { return $a + 2; }",
        ),
        (
            Lang::Rust,
            "fn f(a: u32) -> u32 { a + 1 }",
            "fn f(a: u32) -> u32 {\n    // c\n    a + 1\n}",
            "fn f(a: u32) -> u32 { a + 2 }",
        ),
        (
            Lang::Cpp,
            "int f(int a) { return a + 1; }",
            "int f(int a) {\n    // c\n    return a + 1;\n}",
            "int f(int a) { return a + 2; }",
        ),
    ];
    for (lang, original, cosmetic, real_edit) in cases {
        let h = |src: &str| {
            let facts = limpet::index::extract::extract(lang, src).unwrap();
            let sym = facts.symbols.first().unwrap_or_else(|| panic!("no symbol for {lang:?}"));
            anchor::ast_body_hash(lang, src, sym.byte_range).unwrap()
        };
        assert_eq!(h(original), h(cosmetic), "{lang:?}: cosmetic change altered hash");
        assert_ne!(h(original), h(real_edit), "{lang:?}: real edit did not alter hash");
    }
}
