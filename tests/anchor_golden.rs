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
        None, false,
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
    index::sweep(store, root, &Default::default()).unwrap();
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
        None, false,
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
        None, false,
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
        None, false,
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
        None, false,
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
        None, false,
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
        None, false,
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
    fs::write(root.join("hero.twig"), "{% block hero %}This is a substantial hero block with meaningful content that exceeds the low-entropy threshold{% endblock %}\n").unwrap();
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
        None, false,
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

#[test]
fn body_hashes_carry_normalization_length() {
    // Two identical bodies under different names: same hash, same len, len > 0.
    let src = "def alpha():\n    return compute(1)\n\ndef beta():\n    return compute(1)\n";
    let facts = index::extract::extract(Lang::Py, src).unwrap();
    let ranges: Vec<(usize, usize)> = facts.symbols.iter().map(|s| s.byte_range).collect();
    let out = anchor::ast_body_hashes(Lang::Py, src, &ranges).unwrap();
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].0, out[1].0, "identical bodies must hash identically");
    assert_eq!(out[0].1, out[1].1, "identical bodies must measure identically");
    assert!(out[0].1 > 0, "normalization buffer is never empty");
}

/// Seed a store with an entry anchored to a trivial `orig` symbol in a.py,
/// plus an identical-bodied `twin` symbol already living in b.py. Both
/// bodies measure well under MIN_FOLLOW_BODY_BYTES (76B, see
/// entropy_calibration.rs for the Python trivial floor).
fn seed_trivial_twin(root: &std::path::Path) -> (Store, String) {
    fs::write(root.join("a.py"), "def orig():\n    pass\n").unwrap();
    fs::write(root.join("b.py"), "def twin():\n    pass\n").unwrap();
    let store = Store::open_in_memory().unwrap();
    index::full_index(&store, root).unwrap();
    let result = memory::remember(
        &store,
        "fact",
        "orig is a placeholder awaiting implementation",
        "explicit",
        None,
        &[AnchorSpec { file: "a.py".into(), symbol: Some("orig".into()) }],
        None,
        &[],
        None,
        false,
        None, false,
    )
    .unwrap();
    (store, result.id)
}

#[test]
fn trivial_unique_twin_is_refused_as_low_entropy() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let (store, id) = seed_trivial_twin(root);

    // orig disappears from a.py; twin's identical trivial body in b.py is
    // the only remaining body-hash match. It is a real UNIQUE match, but an
    // empty `pass` body is not evidence of identity: refuse to follow it.
    fs::write(root.join("a.py"), "def unrelated():\n    return 1\n").unwrap();
    let report = mutate_and_resolve(&store, root);
    assert_eq!(report.stale, 1, "trivial unique match must not silently follow");
    assert_eq!(report.followed, 0, "the anchor must not be re-pointed at the twin");
    let (status, reason) = status_of(&store, &id);
    assert_eq!(status, "stale");
    assert_eq!(reason.as_deref(), Some("low_entropy"));
    let fqn: String = store
        .conn
        .query_row("SELECT symbol_fqn FROM anchors WHERE entry_id = ?1", [&id], |r| r.get(0))
        .unwrap();
    assert_eq!(fqn, "a.orig", "anchor must still point at the original, never re-pointed");
}

#[test]
fn low_entropy_refusal_heals_when_the_original_returns() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let (store, id) = seed_trivial_twin(root);

    fs::write(root.join("a.py"), "def unrelated():\n    return 1\n").unwrap();
    let _ = mutate_and_resolve(&store, root);
    assert_eq!(status_of(&store, &id).0, "stale", "precondition: refusal must have staled it");

    // orig comes back exactly as it was (branch switch, stash pop, etc).
    fs::write(root.join("a.py"), "def orig():\n    pass\n").unwrap();
    let _ = mutate_and_resolve(&store, root);
    assert_eq!(
        status_of(&store, &id).0,
        "active",
        "the original returning must heal a low_entropy refusal (symmetric staleness)"
    );
    let fqn: String = store
        .conn
        .query_row("SELECT symbol_fqn FROM anchors WHERE entry_id = ?1", [&id], |r| r.get(0))
        .unwrap();
    assert_eq!(fqn, "a.orig", "anchor was never touched through the whole cycle");
}

#[test]
fn null_body_len_keeps_legacy_follow() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    // A real (non-trivial) body, well clear of the follow floor (343B
    // measured for this shape in tests/entropy_calibration.rs's sibling
    // fixtures), so the only variable under test is body_len itself.
    let real = "def orig(x):\n    y = x + 1\n    if y > 2:\n        return y * 3\n    return y\n";
    fs::write(root.join("a.py"), real).unwrap();
    fs::write(root.join("b.py"), real.replace("orig", "twin")).unwrap();
    let store = Store::open_in_memory().unwrap();
    index::full_index(&store, root).unwrap();
    let result = memory::remember(
        &store,
        "fact",
        "orig triples y once it clears 2",
        "explicit",
        None,
        &[AnchorSpec { file: "a.py".into(), symbol: Some("orig".into()) }],
        None,
        &[],
        None,
        false,
        None, false,
    )
    .unwrap();

    // orig disappears from a.py; twin is the unique body-hash match. Simulate
    // a pre-v5 row that never got a body_len backfill.
    fs::write(root.join("a.py"), "def unrelated():\n    return 1\n").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    index::sweep(&store, root, &Default::default()).unwrap();
    store
        .conn
        .execute("UPDATE symbols SET body_len = NULL WHERE name = 'twin'", [])
        .unwrap();
    let report = anchor::resolve_all(&store).unwrap();

    assert_eq!(report.followed, 1, "NULL body_len must keep legacy grace and follow");
    let (status, _reason) = status_of(&store, &result.id);
    assert_eq!(status, "active");
    let (fqn, file): (String, String) = store
        .conn
        .query_row("SELECT symbol_fqn, file FROM anchors WHERE entry_id = ?1", [&result.id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(fqn, "b.twin");
    assert_eq!(file, "b.py");
}

#[test]
fn tiny_file_move_is_refused_as_low_entropy() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let tiny_content = "# stub\n";
    fs::write(root.join("tiny.md"), tiny_content).unwrap();
    let store = Store::open_in_memory().unwrap();
    index::full_index(&store, root).unwrap();
    let result = memory::remember(
        &store,
        "insight",
        "stub documentation",
        "explicit",
        None,
        &[AnchorSpec { file: "tiny.md".into(), symbol: None }],
        None,
        &[],
        None,
        false,
        None, false,
    )
    .unwrap();

    // Low-entropy file move: content hash matches but size < MIN_FOLLOW_FILE_BYTES
    // so it refuses to follow. Anchor stays at original path, entry goes stale.
    fs::create_dir_all(root.join("docs")).unwrap();
    fs::rename(root.join("tiny.md"), root.join("docs/renamed.md")).unwrap();
    let _ = mutate_and_resolve(&store, root);
    let (status, reason) = status_of(&store, &result.id);
    assert_eq!(status, "stale", "tiny file move must be refused as stale");
    assert_eq!(reason, Some("low_entropy".into()), "stale reason must be low_entropy");
    let file: String = store
        .conn
        .query_row("SELECT file FROM anchors WHERE entry_id = ?1", [&result.id], |r| r.get(0))
        .unwrap();
    assert_eq!(file, "tiny.md", "anchor must not re-point for low-entropy file");
}

#[test]
fn real_file_move_still_follows() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let real_content = "# Real Documentation\n\nThis is a substantial file with enough content to exceed the low-entropy threshold. \
        It should have multiple paragraphs and meaningful size so that when it moves to a new location, \
        the anchor system will recognize it as a high-confidence follow and update the file path in the anchors table. \
        This ensures that large, meaningful files are not mistakenly refused as low-entropy when they change locations.";
    fs::write(root.join("docs.md"), real_content).unwrap();
    let store = Store::open_in_memory().unwrap();
    index::full_index(&store, root).unwrap();
    let result = memory::remember(
        &store,
        "insight",
        "real documentation",
        "explicit",
        None,
        &[AnchorSpec { file: "docs.md".into(), symbol: None }],
        None,
        &[],
        None,
        false,
        None, false,
    )
    .unwrap();

    // Real file move: substantial content, size >= MIN_FOLLOW_FILE_BYTES,
    // so anchor follows the moved file to its new location.
    fs::create_dir_all(root.join("archives")).unwrap();
    fs::rename(root.join("docs.md"), root.join("archives/docs.md")).unwrap();
    let _ = mutate_and_resolve(&store, root);
    assert_eq!(status_of(&store, &result.id).0, "active", "real file move must be followed");
    let file: String = store
        .conn
        .query_row("SELECT file FROM anchors WHERE entry_id = ?1", [&result.id], |r| r.get(0))
        .unwrap();
    assert_eq!(file, "archives/docs.md", "anchor must re-point for high-entropy file");
}

// ---------------------------------------------------------------------------
// Slot resolution: same-FQN twins (trait impls, overloads).
//
// Rust trait impls of one type share an FQN (`router.Router.route`), so the
// schema v7 discriminator is the only thing telling one method from the
// other. These fixtures build that collision on purpose.
// ---------------------------------------------------------------------------

/// The `Ingress` impl of `Router::route`. The body clears
/// MIN_FOLLOW_BODY_BYTES comfortably, so nothing in these tests is decided by
/// the entropy floor.
const INGRESS_IMPL: &str = r#"
impl Ingress for Router {
    fn route(&self, hops: u32) -> u32 {
        let mut cost = 0;
        for hop in 0..hops {
            cost += hop * 3;
        }
        cost
    }
}
"#;

/// The `Egress` twin: same FQN, byte-identical body, so its row carries the
/// other impl's body hash. This is the masking shape the wedge exists to
/// catch.
const EGRESS_TWIN_IMPL: &str = r#"
impl Egress for Router {
    fn route(&self, hops: u32) -> u32 {
        let mut cost = 0;
        for hop in 0..hops {
            cost += hop * 3;
        }
        cost
    }
}
"#;

/// The `Egress` impl with a body of its own: still the same FQN, but a
/// distinct body hash, so `(fqn, hash)` stays unique and the rename and
/// backfill steps run without ambiguity.
const EGRESS_DISTINCT_IMPL: &str = r#"
impl Egress for Router {
    fn route(&self, hops: u32) -> u32 {
        let mut cost = 1;
        for hop in 0..hops {
            cost *= hop + 2;
        }
        cost
    }
}
"#;

fn write_router(root: &std::path::Path, impls: &[&str]) {
    let mut src = String::from("struct Router;\n");
    for block in impls {
        src.push_str(block);
    }
    fs::write(root.join("router.rs"), src).unwrap();
}

/// Index a `router.rs` built from `impls` and anchor a memory to ONE slot,
/// `route@Ingress`.
fn seed_router(root: &std::path::Path, impls: &[&str]) -> (Store, String) {
    write_router(root, impls);
    let store = Store::open_in_memory().unwrap();
    index::full_index(&store, root).unwrap();
    let result = memory::remember(
        &store,
        "fact",
        "ingress routing costs three units per hop",
        "explicit",
        None,
        &[AnchorSpec { file: "router.rs".into(), symbol: Some("route@Ingress".into()) }],
        None,
        &[],
        None,
        false,
        None, false,
    )
    .unwrap();
    (store, result.id)
}

/// (symbol_fqn, disamb, ast_body_hash) of an entry's single anchor.
fn anchor_slot(store: &Store, id: &str) -> (String, Option<String>, String) {
    store
        .conn
        .query_row(
            "SELECT symbol_fqn, disamb, ast_body_hash FROM anchors WHERE entry_id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
}

/// Does ANY symbol row carry this (fqn, body_hash) pair? True here means the
/// pre-slot ladder would have answered Fresh.
fn fqn_hash_exists(store: &Store, fqn: &str, hash: &str) -> bool {
    store
        .conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM symbols WHERE fqn = ?1 AND body_hash = ?2)",
            rusqlite::params![fqn, hash],
            |r| r.get(0),
        )
        .unwrap()
}

#[test]
fn twin_impl_edit_is_not_masked_by_its_identical_twin() {
    // Invariant I-F1, the reason slot resolution exists: an edit inside one
    // trait impl must surface even while an identical-bodied twin under the
    // same FQN still carries the anchor's old body hash.
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let (store, id) = seed_router(root, &[INGRESS_IMPL, EGRESS_TWIN_IMPL]);
    let (fqn, slot, hash) = anchor_slot(&store, &id);
    assert_eq!(fqn, "router.Router.route", "the twins share one FQN");
    assert_eq!(slot.as_deref(), Some("Ingress"), "the anchor must record its slot");
    let twins: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM symbols WHERE fqn = ?1", [&fqn], |r| r.get(0))
        .unwrap();
    assert_eq!(twins, 2, "premise: two symbol rows share this FQN");

    // A second memory on the OTHER slot of the same FQN. Slot resolution must
    // separate the two in BOTH directions: no masking, and no collateral
    // staleness for the twin nobody touched.
    let sibling = memory::remember(
        &store,
        "fact",
        "egress routing costs three units per hop",
        "explicit",
        None,
        &[AnchorSpec { file: "router.rs".into(), symbol: Some("route@Egress".into()) }],
        None,
        &[],
        None,
        false,
        None, false,
    )
    .unwrap();
    assert_eq!(mutate_and_resolve(&store, root).fresh, 2, "untouched twins stay fresh");

    // Edit ONLY the Ingress impl; Egress keeps the original body.
    write_router(
        root,
        &[&INGRESS_IMPL.replace("cost += hop * 3;", "cost += hop * 7;"), EGRESS_TWIN_IMPL],
    );
    let report = mutate_and_resolve(&store, root);
    assert!(
        fqn_hash_exists(&store, &fqn, &hash),
        "premise: the twin still carries the anchor's old body hash, so the \
         pre-0.15 (fqn, hash) check would have answered Fresh here",
    );
    assert_eq!(report.stale, 1, "the edit must surface, not hide behind the twin");
    assert_eq!(report.fresh, 1, "and it must stale ONLY the slot that was edited");
    assert_eq!(report.followed, 0, "an in-place edit is not a move");
    assert_eq!(
        status_of(&store, &sibling.id).0,
        "active",
        "the untouched twin's memory must not be dragged down with it",
    );
    let (status, reason) = status_of(&store, &id);
    assert_eq!(status, "stale");
    assert_eq!(reason.as_deref(), Some("body_edited"));
    assert_eq!(
        anchor_slot(&store, &id),
        (fqn.clone(), Some("Ingress".to_string()), hash.clone()),
        "a stale anchor must keep its slot, never be re-pointed",
    );

    // Set semantics, not row order (I-F7): re-resolving the same index must
    // give the same answer every time, so the memory cannot flap.
    for _ in 0..3 {
        let repeat = mutate_and_resolve(&store, root);
        assert_eq!(repeat.stale, 1, "resolution must be stable across sweeps");
        assert_eq!(status_of(&store, &id).1.as_deref(), Some("body_edited"));
    }

    // The edit is reverted (a stash pop, a rollback): the slot holds the
    // memorized body again and the memory heals.
    write_router(root, &[INGRESS_IMPL, EGRESS_TWIN_IMPL]);
    let report = mutate_and_resolve(&store, root);
    assert_eq!(report.fresh, 2, "the returning body must heal the anchor");
    assert_eq!(status_of(&store, &id).0, "active");
    assert_eq!(status_of(&store, &sibling.id).0, "active");
    assert_eq!(
        anchor_slot(&store, &id),
        (fqn, Some("Ingress".to_string()), hash),
        "healing must not move the anchor either",
    );
}

#[test]
fn trait_rename_rewrites_the_slot_and_keeps_the_fqn() {
    // A trait name lives outside the method body, so renaming the trait
    // changes the discriminator and nothing else. The anchor's slot is
    // relabelled in place: same FQN, same body, no stale, no penalty.
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let (store, id) = seed_router(root, &[INGRESS_IMPL, EGRESS_DISTINCT_IMPL]);
    let (fqn, _, hash) = anchor_slot(&store, &id);

    write_router(
        root,
        &[&INGRESS_IMPL.replace("impl Ingress", "impl Inbound"), EGRESS_DISTINCT_IMPL],
    );
    let report = mutate_and_resolve(&store, root);
    assert_eq!(report.followed, 1, "a renamed discriminator is followed, not staled");
    assert_eq!(report.stale, 0);
    assert_eq!(status_of(&store, &id).0, "active");
    assert_eq!(
        anchor_slot(&store, &id),
        (fqn.clone(), Some("Inbound".to_string()), hash),
        "the slot is relabelled; the FQN and the body hash are untouched",
    );
    let file: String = store
        .conn
        .query_row("SELECT file FROM anchors WHERE entry_id = ?1", [&id], |r| r.get(0))
        .unwrap();
    assert_eq!(file, "router.rs");
    let twins: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM symbols WHERE fqn = ?1", [&fqn], |r| r.get(0))
        .unwrap();
    assert_eq!(twins, 2, "the twin still shares the FQN, so the slot did the work");
}

#[test]
fn slot_rename_into_identical_twins_refuses_rather_than_guessing() {
    // The discriminator is respelled AND an identical-bodied twin shares the
    // FQN: two rows now answer to (fqn, body), so which one is the anchor's
    // is unknowable. The refusal is unweakened by disambiguation (I-F6).
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let (store, id) = seed_router(root, &[INGRESS_IMPL, EGRESS_TWIN_IMPL]);
    let before = anchor_slot(&store, &id);

    write_router(
        root,
        &[&INGRESS_IMPL.replace("impl Ingress", "impl Inbound"), EGRESS_TWIN_IMPL],
    );
    let report = mutate_and_resolve(&store, root);
    assert_eq!(report.stale, 1, "two identical bodies under one FQN are not evidence");
    assert_eq!(report.followed, 0, "a relabel must never be guessed");
    let (status, reason) = status_of(&store, &id);
    assert_eq!(status, "stale");
    assert_eq!(reason.as_deref(), Some("ambiguous_anchor"));
    assert_eq!(anchor_slot(&store, &id), before, "a refusal moves nothing");
}

#[test]
fn refill_window_is_not_read_as_a_rename() {
    // Schema v7 lands `disamb` NULL and the bounded sweep refills it file by
    // file. A slot-carrying anchor that meets a not-yet-refilled row must not
    // read that NULL as "my discriminator was respelled to nothing": it keeps
    // the slot it recorded and stays fresh, because an un-refilled row is not
    // evidence of anything. resolve_all is called directly here: a sweep would
    // re-parse the file and refill the column being tested.
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let (store, id) = seed_router(root, &[INGRESS_IMPL]);
    let before = anchor_slot(&store, &id);
    store.conn.execute("UPDATE symbols SET disamb = NULL", []).unwrap();

    let report = anchor::resolve_all(&store).unwrap();
    assert_eq!(report.fresh, 1, "an un-refilled row must not stale the anchor");
    assert_eq!(report.followed, 0, "and must not be mistaken for a rename");
    assert_eq!(status_of(&store, &id).0, "active");
    assert_eq!(anchor_slot(&store, &id), before, "the anchor keeps the slot it recorded");
}

#[test]
fn a_refill_window_full_of_twins_is_not_read_as_ambiguity() {
    // The same rule as the single-row refill case, with twins: when NO row
    // under the FQN names a discriminator, none of them is evidence that the
    // slot was respelled, and several of them are not evidence of ambiguity
    // either. They are legacy-shaped rows and get the legacy answer. Staling
    // here would spend the one-shot confidence penalty on a window that closes
    // by itself. Reachable when an older binary re-indexes a v7 store: its
    // INSERT writes no disamb while the anchors keep the slots they recorded.
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let (store, id) = seed_router(root, &[INGRESS_IMPL, EGRESS_TWIN_IMPL]);
    let before = anchor_slot(&store, &id);
    store.conn.execute("UPDATE symbols SET disamb = NULL", []).unwrap();

    let report = anchor::resolve_all(&store).unwrap();
    assert_eq!(report.fresh, 1, "un-refilled twins must not stale the anchor");
    assert_eq!(report.stale, 0, "and must not be read as ambiguity");
    assert_eq!(report.followed, 0);
    assert_eq!(status_of(&store, &id).0, "active");
    assert_eq!(anchor_slot(&store, &id), before, "the anchor keeps the slot it recorded");
}

#[test]
fn a_followed_body_takes_its_new_slot_with_it() {
    // Step 5 re-points an anchor at the one proven home of its body, so the
    // anchor must take that home's discriminator too. Keeping the old label
    // would leave the anchor claiming a slot no row answers to, and the moment
    // any symbol claims that label under the new FQN, step 2 reads the
    // newcomer's body as this memory's own edit.
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let (store, id) = seed_router(root, &[INGRESS_IMPL]);
    let (fqn, slot, hash) = anchor_slot(&store, &id);
    assert_eq!(fqn, "router.Router.route");
    assert_eq!(slot.as_deref(), Some("Ingress"));

    // The impl moves to another file AND its trait is renamed, so the old FQN
    // is gone entirely and the ladder falls through to the store-wide hunt.
    fs::remove_file(root.join("router.rs")).unwrap();
    let mut moved = String::from("struct Router;\n");
    moved.push_str(&INGRESS_IMPL.replace("impl Ingress", "impl Outbound"));
    fs::write(root.join("gateway.rs"), moved).unwrap();
    let report = mutate_and_resolve(&store, root);
    assert_eq!(report.followed, 1, "a unique body must be followed to its new home");
    assert_eq!(report.stale, 0);
    assert_eq!(status_of(&store, &id).0, "active");
    assert_eq!(
        anchor_slot(&store, &id),
        ("gateway.Router.route".to_string(), Some("Outbound".to_string()), hash),
        "a follow rewrites the slot, not just the FQN",
    );

    // The old label is free now, and another impl claims it with a body of its
    // own. A relabelled anchor is none of its business; an anchor still
    // holding "Ingress" would go stale against code it never described.
    let mut crowded = String::from("struct Router;\n");
    crowded.push_str(&INGRESS_IMPL.replace("impl Ingress", "impl Outbound"));
    crowded.push_str(&EGRESS_DISTINCT_IMPL.replace("impl Egress", "impl Ingress"));
    fs::write(root.join("gateway.rs"), crowded).unwrap();
    let report = mutate_and_resolve(&store, root);
    assert_eq!(report.fresh, 1, "a reused discriminator must not touch the relabelled anchor");
    assert_eq!(report.stale, 0);
    assert_eq!(status_of(&store, &id).0, "active");
}

#[test]
fn legacy_anchor_never_adopts_while_a_twin_shares_the_fqn() {
    // A hash-unique row under a SHARED fqn does not prove the slot: if the
    // twins' bodies were ever identical and the anchored symbol was just
    // edited, the single surviving hash match is the TWIN wearing the
    // anchor's old body, and adopting it hardens the anchor to the wrong
    // symbol (whole-branch review 2026-08-04). With a twin present the anchor
    // stays legacy: 0.14 fates exactly (I-F2), no guess.
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let (store, id) = seed_router(root, &[INGRESS_IMPL, EGRESS_DISTINCT_IMPL]);
    store.conn.execute("UPDATE anchors SET disamb = NULL", []).unwrap();

    let report = mutate_and_resolve(&store, root);
    assert_eq!(report.fresh, 1, "a legacy anchor on untouched code stays fresh");
    assert_eq!(status_of(&store, &id).0, "active");
    let (fqn, slot, _) = anchor_slot(&store, &id);
    assert_eq!(fqn, "router.Router.route");
    assert_eq!(slot, None, "a shared fqn proves nothing; the slot must stay legacy");

    // Legacy semantics still catch this edit, because the distinct twin's
    // body never carried the anchor's hash.
    write_router(
        root,
        &[&INGRESS_IMPL.replace("cost += hop * 3;", "cost += hop * 7;"), EGRESS_DISTINCT_IMPL],
    );
    let report = mutate_and_resolve(&store, root);
    assert_eq!(report.stale, 1);
    assert_eq!(status_of(&store, &id).1.as_deref(), Some("body_edited"));
}

#[test]
fn legacy_anchor_keeps_its_null_slot_when_twins_prove_nothing() {
    // Two rows carry the same (fqn, body), so nothing says which one the
    // legacy anchor meant. It keeps 0.14 semantics exactly: Fresh, slot still
    // NULL, no guess (I-F2).
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let (store, id) = seed_router(root, &[INGRESS_IMPL, EGRESS_TWIN_IMPL]);
    store.conn.execute("UPDATE anchors SET disamb = NULL", []).unwrap();

    let report = mutate_and_resolve(&store, root);
    assert_eq!(report.fresh, 1);
    assert_eq!(status_of(&store, &id).0, "active");
    let (_, slot, _) = anchor_slot(&store, &id);
    assert_eq!(slot, None, "an ambiguous match is not evidence: no slot may be adopted");
}

#[test]
fn vanished_slot_with_a_live_fqn_goes_body_edited_and_heals() {
    // The anchored impl is deleted outright while its twin keeps the FQN
    // alive. No row carries the body under that FQN, so the ladder refuses to
    // hunt the store and stales conservatively; restoring the impl heals it.
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let (store, id) = seed_router(root, &[INGRESS_IMPL, EGRESS_DISTINCT_IMPL]);
    let before = anchor_slot(&store, &id);

    write_router(root, &[EGRESS_DISTINCT_IMPL]);
    let report = mutate_and_resolve(&store, root);
    assert_eq!(report.stale, 1, "a vanished slot under a live FQN is stale, never followed");
    assert_eq!(report.followed, 0);
    assert_eq!(report.invalidated, 0);
    let (status, reason) = status_of(&store, &id);
    assert_eq!(status, "stale");
    assert_eq!(reason.as_deref(), Some("body_edited"));
    assert!(
        !fqn_hash_exists(&store, &before.0, &before.2),
        "premise: no row carries the anchored body under that FQN any more",
    );
    assert_eq!(anchor_slot(&store, &id), before, "a conservative stale moves nothing");

    write_router(root, &[INGRESS_IMPL, EGRESS_DISTINCT_IMPL]);
    let report = mutate_and_resolve(&store, root);
    assert_eq!(report.fresh, 1, "the restored impl must heal the memory");
    assert_eq!(status_of(&store, &id).0, "active");
    assert_eq!(anchor_slot(&store, &id), before);
}

// --- T2b: every shape that used to file two symbols in one slot -------------

/// Seed a memory on each of two same-named symbols, prove both start fresh,
/// then apply an edit that touches only the FIRST and assert the edit surfaces
/// while the untouched sibling stays active.
///
/// Every case below is a pair that the pre-0.15 extractor filed under ONE
/// `(fqn, disamb)` slot with byte-identical bodies, so resolution read the
/// sibling's row, answered Fresh for the edited symbol, and hid the edit
/// (invariant I-F1). The pair is separated either by a new FQN segment (a
/// namespace, enum or record scope) or by a new discriminator (a Ruby
/// receiver, a parameter list, a generic arity, a C++ cv qualifier).
fn assert_no_twin_masking(
    case: &str,
    file: &str,
    original: &str,
    edited: &str,
    spec_a: &str,
    spec_b: &str,
) {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    fs::write(root.join(file), original).unwrap();
    let store = Store::open_in_memory().unwrap();
    index::full_index(&store, root).unwrap();

    let mut ids = Vec::new();
    for (n, spec) in [spec_a, spec_b].iter().enumerate() {
        let r = memory::remember(
            &store,
            "fact",
            &format!("{case} note {n}: what this symbol does"),
            "explicit",
            None,
            &[AnchorSpec { file: file.into(), symbol: Some((*spec).into()) }],
            None,
            &[],
            None,
            false,
            None,
            false,
        )
        .unwrap_or_else(|e| panic!("{case}: anchoring {spec} failed: {e}"));
        assert_eq!(r.anchored, 1, "{case}: {spec} must anchor to exactly one slot");
        ids.push(r.id);
    }
    assert_eq!(
        mutate_and_resolve(&store, root).fresh,
        2,
        "{case}: premise, both memories start fresh"
    );

    fs::write(root.join(file), edited).unwrap();
    mutate_and_resolve(&store, root);
    assert_eq!(
        status_of(&store, &ids[0]).0,
        "stale",
        "{case}: the edited symbol must go stale, not be masked by its twin"
    );
    assert_eq!(
        status_of(&store, &ids[1]).0,
        "active",
        "{case}: the untouched twin must not go stale in sympathy"
    );
}

#[test]
fn ruby_singleton_twin_cannot_mask_its_instance_method() {
    let original = "class C\n  def m\n    helper(1)\n  end\n  def self.m\n    helper(1)\n  end\nend\n";
    let edited = "class C\n  def m\n    other(2)\n  end\n  def self.m\n    helper(1)\n  end\nend\n";
    assert_no_twin_masking("ruby self.", "c.rb", original, edited, "m@#", "m@self.");
}

#[test]
fn ruby_class_shovel_self_twin_cannot_mask_its_instance_method() {
    let original =
        "class C\n  def m\n    helper(1)\n  end\n  class << self\n    def m\n      helper(1)\n    end\n  end\nend\n";
    let edited =
        "class C\n  def m\n    other(2)\n  end\n  class << self\n    def m\n      helper(1)\n    end\n  end\nend\n";
    assert_no_twin_masking("ruby class << self", "d.rb", original, edited, "m@#", "m@<<self.");
}

#[test]
fn csharp_block_namespace_twin_cannot_mask_its_sibling() {
    let original = "namespace A { class Options { void Validate() { Check(1); } } }\nnamespace B { class Options { void Validate() { Check(1); } } }\n";
    let edited = "namespace A { class Options { void Validate() { Other(2); } } }\nnamespace B { class Options { void Validate() { Check(1); } } }\n";
    assert_no_twin_masking(
        "csharp namespace",
        "o.cs",
        original,
        edited,
        "o.A.Options.Validate",
        "o.B.Options.Validate",
    );
}

#[test]
fn java_nested_enum_twin_cannot_mask_its_sibling() {
    let original = "class Outer {\n  enum A { X; String label() { return fmt(1); } }\n  enum B { Y; String label() { return fmt(1); } }\n}\n";
    let edited = "class Outer {\n  enum A { X; String label() { return other(2); } }\n  enum B { Y; String label() { return fmt(1); } }\n}\n";
    assert_no_twin_masking(
        "java enum",
        "Outer.java",
        original,
        edited,
        "Outer.Outer.A.label",
        "Outer.Outer.B.label",
    );
}

#[test]
fn python_property_setter_pair_cannot_mask_each_other() {
    let original =
        "class C:\n    @property\n    def x(self):\n        return helper(1)\n    @x.setter\n    def x(self, v):\n        return helper(1)\n";
    let edited =
        "class C:\n    @property\n    def x(self):\n        return other(2)\n    @x.setter\n    def x(self, v):\n        return helper(1)\n";
    assert_no_twin_masking("py property", "p.py", original, edited, "x@(self)", "x@(self,v)");
}

#[test]
fn cpp_reference_returning_cv_overloads_cannot_mask_each_other() {
    // The exact pair the C++ overload discriminator was written for. Neither
    // half extracted at all before the declarator fallback, so no memory could
    // be anchored to either one and the discriminator was never exercised on
    // the shape it names.
    let original = "class Buffer {\n  std::string& name() { return name_; }\n  const std::string& name() const { return name_; }\n};\n";
    let edited = "class Buffer {\n  std::string& name() { return renamed_; }\n  const std::string& name() const { return name_; }\n};\n";
    assert_no_twin_masking("cpp ref return", "buf.cpp", original, edited, "name@()", "name@() const");
}

// --- T2b: a respelled SCOPE must not false-stale the anchors under it -------

#[test]
fn a_scope_respell_follows_instead_of_false_staling() {
    // Wrapping a function in a `mod` respells its FQN while its body stays
    // byte-identical. Every 0.15 scope fix does exactly this to FQNs that
    // ALREADY have anchors on them, and the v7 migration respells a whole repo
    // in one sweep. The hash-only store-wide hunt cannot rescue those anchors:
    // it refuses a body under the entropy floor and refuses any body with a
    // duplicate, and a refusal never repairs the anchor, so the memory reads
    // stale forever with no code change behind it.
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    // Deliberately SHORT: 114B of normalization buffer, under the 124B follow
    // floor, so the store-wide hunt would refuse this exact body.
    let body = "pub fn small_fn() -> u32 { 7 }\n";
    fs::write(root.join("a.rs"), body).unwrap();
    let store = Store::open_in_memory().unwrap();
    index::full_index(&store, root).unwrap();
    let r = memory::remember(
        &store,
        "fact",
        "small_fn is the seven sentinel",
        "explicit",
        None,
        &[AnchorSpec { file: "a.rs".into(), symbol: Some("small_fn".into()) }],
        None,
        &[],
        None,
        false,
        None,
        false,
    )
    .unwrap();
    assert_eq!(anchor_slot(&store, &r.id).0, "a.small_fn");
    let len: i64 = store
        .conn
        .query_row("SELECT body_len FROM symbols WHERE name = 'small_fn'", [], |r| r.get(0))
        .unwrap();
    assert!(len < 124, "premise: this body is below the follow floor, got {len}B");

    // Same function, same bytes, new enclosing scope.
    fs::write(root.join("a.rs"), format!("mod inner {{\n{body}}}\n")).unwrap();
    let report = mutate_and_resolve(&store, root);
    assert_eq!(report.followed, 1, "a scope respell is a follow, not a stale: {report:?}");
    assert_eq!(report.stale, 0, "{report:?}");
    assert_eq!(status_of(&store, &r.id).0, "active");
    assert_eq!(
        anchor_slot(&store, &r.id).0,
        "a.inner.small_fn",
        "the anchor must be repaired, or every later sweep repeats the verdict"
    );
}

#[test]
fn a_scope_respell_picks_the_right_twin_by_name() {
    // Two byte-identical bodies move under one new scope together. The
    // store-wide hunt sees two hash matches and refuses as ambiguous, but the
    // last FQN segment says which one is which, so the respell step resolves
    // it exactly.
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let twins = "pub fn twin_a(n: u32) -> u32 {\n    let mut t = 0;\n    for i in 0..n {\n        t += i * 2;\n    }\n    t\n}\npub fn twin_b(n: u32) -> u32 {\n    let mut t = 0;\n    for i in 0..n {\n        t += i * 2;\n    }\n    t\n}\n";
    fs::write(root.join("a.rs"), twins).unwrap();
    let store = Store::open_in_memory().unwrap();
    index::full_index(&store, root).unwrap();
    let dupes: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM symbols WHERE body_hash =
             (SELECT body_hash FROM symbols WHERE name = 'twin_a')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(dupes, 2, "premise: the body hash is not unique");
    let r = memory::remember(
        &store,
        "fact",
        "twin_a doubles every index below n",
        "explicit",
        None,
        &[AnchorSpec { file: "a.rs".into(), symbol: Some("twin_a".into()) }],
        None,
        &[],
        None,
        false,
        None,
        false,
    )
    .unwrap();

    fs::write(root.join("a.rs"), format!("mod inner {{\n{twins}}}\n")).unwrap();
    let report = mutate_and_resolve(&store, root);
    assert_eq!(report.followed, 1, "{report:?}");
    assert_eq!(status_of(&store, &r.id).0, "active");
    assert_eq!(
        anchor_slot(&store, &r.id).0,
        "a.inner.twin_a",
        "the respell must land on the anchor's own name, never its twin"
    );
}

#[test]
fn a_respell_with_two_same_named_candidates_still_refuses() {
    // The respell step is not a licence to guess: two rows in the file share
    // the anchored body AND the anchored last segment, so which one the memory
    // meant is unknowable and the ladder falls through to the honest refusal.
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let one = "pub fn dup(n: u32) -> u32 {\n    let mut t = 0;\n    for i in 0..n {\n        t += i * 2;\n    }\n    t\n}\n";
    fs::write(root.join("a.rs"), one).unwrap();
    let store = Store::open_in_memory().unwrap();
    index::full_index(&store, root).unwrap();
    let r = memory::remember(
        &store,
        "fact",
        "dup doubles every index below n",
        "explicit",
        None,
        &[AnchorSpec { file: "a.rs".into(), symbol: Some("dup".into()) }],
        None,
        &[],
        None,
        false,
        None,
        false,
    )
    .unwrap();

    fs::write(root.join("a.rs"), format!("mod p {{\n{one}}}\nmod q {{\n{one}}}\n")).unwrap();
    let report = mutate_and_resolve(&store, root);
    assert_eq!(report.followed, 0, "two candidate homes must not be guessed between: {report:?}");
    assert_eq!(report.stale, 1, "{report:?}");
    assert_eq!(status_of(&store, &r.id).1.as_deref(), Some("ambiguous_anchor"));
    assert_eq!(anchor_slot(&store, &r.id).0, "a.dup", "a refusal moves nothing");
}

// --- whole-branch review 2026-08-04: the respell step must not guess --------

#[test]
fn a_deleted_scope_twin_is_not_respell_followed() {
    // Deleting a scope whose same-named, byte-identical sibling survives in
    // the file is indistinguishable from a respell by (file, hash, last
    // segment) alone. The sibling's scope chain does NOT extend the anchor's
    // old spelling, and that structural mismatch is what refuses the follow:
    // 0.14 said Stale{low_entropy} here, and 0.15 must not say less.
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let twins = "mod alpha {\n    pub fn reset() {}\n}\nmod beta {\n    pub fn reset() {}\n}\n";
    fs::write(root.join("shapes.rs"), twins).unwrap();
    let store = Store::open_in_memory().unwrap();
    index::full_index(&store, root).unwrap();
    let r = memory::remember(
        &store,
        "fact",
        "alpha reset returns the shape to the origin pose",
        "explicit",
        None,
        &[AnchorSpec { file: "shapes.rs".into(), symbol: Some("shapes.alpha.reset".into()) }],
        None,
        &[],
        None,
        false,
        None,
        false,
    )
    .unwrap();
    assert_eq!(anchor_slot(&store, &r.id).0, "shapes.alpha.reset");

    // The whole alpha scope is deleted; beta's identical reset survives.
    fs::write(root.join("shapes.rs"), "mod beta {\n    pub fn reset() {}\n}\n").unwrap();
    let report = mutate_and_resolve(&store, root);
    assert_eq!(
        report.followed, 0,
        "a deleted symbol must not be respell-followed onto its surviving twin: {report:?}"
    );
    assert_eq!(report.stale, 1, "{report:?}");
    let (status, reason) = status_of(&store, &r.id);
    assert_eq!(status, "stale");
    assert_eq!(reason.as_deref(), Some("low_entropy"), "the 0.14 verdict must survive");
    assert_eq!(
        anchor_slot(&store, &r.id).0,
        "shapes.alpha.reset",
        "the anchor must keep naming what the memory described"
    );
}

#[test]
fn a_leftover_in_file_twin_does_not_hijack_a_cross_file_move() {
    // The anchored body moves to another file while an identical copy stays
    // behind in the anchor's file. Two interpretations compete (move vs
    // respell), so the respell step must stand down: any carrier of the body
    // OUTSIDE the anchor's file forfeits the in-file shortcut, and the
    // store-wide hunt refuses the duplicate as ambiguous, exactly as 0.14 did.
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let body = "pub fn parse(n: u32) -> u32 {\n    let mut t = 0;\n    for i in 0..n {\n        t += i * 2;\n    }\n    t\n}\n";
    fs::write(root.join("a.rs"), body).unwrap();
    let store = Store::open_in_memory().unwrap();
    index::full_index(&store, root).unwrap();
    let r = memory::remember(
        &store,
        "fact",
        "parse doubles every index below n",
        "explicit",
        None,
        &[AnchorSpec { file: "a.rs".into(), symbol: Some("parse".into()) }],
        None,
        &[],
        None,
        false,
        None,
        false,
    )
    .unwrap();

    // A byte-identical copy lands inside a mod in the same file: still fresh,
    // the anchored top-level symbol is untouched.
    fs::write(root.join("a.rs"), format!("{body}mod legacy {{\n{body}}}\n")).unwrap();
    let report = mutate_and_resolve(&store, root);
    assert_eq!(report.fresh, 1, "{report:?}");

    // The top-level symbol moves to b.rs; the mod copy stays behind.
    fs::write(root.join("a.rs"), format!("mod legacy {{\n{body}}}\n")).unwrap();
    fs::write(root.join("b.rs"), body).unwrap();
    let report = mutate_and_resolve(&store, root);
    assert_eq!(
        report.followed, 0,
        "an out-of-file carrier must veto the in-file respell shortcut: {report:?}"
    );
    assert_eq!(report.stale, 1, "{report:?}");
    let (status, reason) = status_of(&store, &r.id);
    assert_eq!(status, "stale");
    assert_eq!(reason.as_deref(), Some("ambiguous_anchor"));
}

#[test]
fn a_twin_split_respell_is_rescued_from_behind_a_surviving_fqn() {
    // A respell can leave the anchor's OLD spelling alive on a different
    // symbol (pre-v7 twins splitting under the sanctioned scope fixes). The
    // fqn-still-exists stale verdict must not shadow the rescue: the body
    // sitting in the anchor's own file under an extended scope spelling is
    // the stronger evidence.
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let body = "pub fn helper(n: u32) -> u32 {\n    let mut t = 1;\n    for i in 1..n {\n        t = t.wrapping_mul(i);\n    }\n    t\n}\n";
    fs::write(root.join("a.rs"), body).unwrap();
    let store = Store::open_in_memory().unwrap();
    index::full_index(&store, root).unwrap();
    let r = memory::remember(
        &store,
        "fact",
        "helper accumulates a wrapping product below n",
        "explicit",
        None,
        &[AnchorSpec { file: "a.rs".into(), symbol: Some("helper".into()) }],
        None,
        &[],
        None,
        false,
        None,
        false,
    )
    .unwrap();

    // The anchored body ends up inside a mod while a NEW body takes over the
    // old top-level spelling: the old fqn survives on a stranger.
    let stranger = "pub fn helper(n: u32) -> u32 {\n    n + 41\n}\n";
    fs::write(root.join("a.rs"), format!("{stranger}mod inner {{\n{body}}}\n")).unwrap();
    let report = mutate_and_resolve(&store, root);
    assert_eq!(
        report.followed, 1,
        "the surviving stranger fqn must not shadow the respell rescue: {report:?}"
    );
    assert_eq!(report.stale, 0, "{report:?}");
    assert_eq!(status_of(&store, &r.id).0, "active");
    assert_eq!(anchor_slot(&store, &r.id).0, "a.inner.helper");
}

#[test]
fn legacy_backfill_adopts_only_when_the_fqn_is_twin_free() {
    // Positive control for the twin-free gate: a single row under the fqn can
    // only be the anchor's own symbol, so adoption is safe there.
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let (store, id) = seed_router(root, &[INGRESS_IMPL]);
    store.conn.execute("UPDATE anchors SET disamb = NULL", []).unwrap();

    let report = mutate_and_resolve(&store, root);
    assert_eq!(report.fresh, 1);
    assert_eq!(
        anchor_slot(&store, &id).1.as_deref(),
        Some("Ingress"),
        "a twin-free fqn's single row proves the slot"
    );
}
