//! Guard against README drift (the philosophy forbids silently lying, and a
//! stale README is exactly that). These tests fail the build when a shipped
//! tool, grammar, or CLI command is missing from the docs, so accuracy is
//! enforced rather than remembered.

use limpet::index::lang::{from_config_str, Lang};
use limpet::tools::tool_schemas;

fn readme() -> String {
    std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/README.md"))
        .expect("README.md must exist")
}

fn skill_md() -> String {
    std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/skill.md"))
        .expect("src/skill.md must exist")
}

/// The op enum the shipped `admin` schema advertises. Reading it from the
/// schema rather than a hand-kept list is the point: a new op cannot be added
/// without the doc guards below seeing it.
fn admin_ops() -> Vec<String> {
    tool_schemas()
        .as_array()
        .expect("tool_schemas is an array")
        .iter()
        .find(|t| t["name"] == "admin")
        .expect("the admin tool ships")["inputSchema"]["properties"]["op"]["enum"]
        .as_array()
        .expect("admin.op is an enum")
        .iter()
        .map(|v| v.as_str().expect("enum values are strings").to_string())
        .collect()
}

#[test]
fn every_shipped_tool_is_documented() {
    let readme = readme();
    let schemas = tool_schemas();
    let tools = schemas.as_array().expect("tool_schemas is an array");
    assert_eq!(tools.len(), 6, "the README says 'six tools'; keep it true");
    for t in tools {
        let name = t["name"].as_str().unwrap();
        assert!(
            readme.contains(&format!("`{name}`")),
            "tool `{name}` is shipped but not mentioned in README.md"
        );
    }
}

/// The op strings the `tool_admin` dispatcher actually matches on, read from
/// the source. The schema enum is hand-written prose two hundred lines away
/// from the `match`, and nothing at compile time ties them together: an op
/// added to the dispatch without a schema entry ships callable, undocumented,
/// and invisible to every guard that trusts the enum. The arms of
/// `let data = match op {` sit at one fixed indent with string patterns, so
/// the scrape is a plain line scan; if the function is ever reshaped this
/// panics loudly rather than returning an empty set.
fn dispatched_admin_ops() -> Vec<String> {
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/tools.rs"))
        .expect("src/tools.rs must exist");
    let start = src.find("fn tool_admin").expect("tool_admin must exist in src/tools.rs");
    let body = &src[start..];
    let end = body[1..].find("\nfn ").map(|i| i + 1).unwrap_or(body.len());
    let ops: Vec<String> = body[..end]
        .lines()
        .filter_map(|line| {
            let arm = line.strip_prefix("        \"")?;
            let (op, rest) = arm.split_once('"')?;
            rest.trim_start().starts_with("=>").then(|| op.to_string())
        })
        .collect();
    assert!(
        !ops.is_empty(),
        "the tool_admin op-arm scrape found nothing; the match was reshaped, update the scrape"
    );
    ops
}

#[test]
fn the_admin_schema_enum_matches_the_dispatcher() {
    let mut schema = admin_ops();
    let mut dispatched = dispatched_admin_ops();
    schema.sort();
    dispatched.sort();
    assert_eq!(
        schema, dispatched,
        "admin.op schema enum and the tool_admin dispatch arms disagree; \
         every op must appear in both"
    );
}

/// The words of the README `admin` row, so an op assertion cannot be
/// satisfied by unrelated prose elsewhere in the file: a mutation test showed
/// six of the eleven ops surviving deletion from the row because "index",
/// "status", "export", "import", "ledger", and "reverify" all occur in other
/// sentences. Same lesson as the grammar lists below: read the enumerating
/// text itself, never the whole file.
fn readme_admin_row_words() -> Vec<String> {
    let readme = readme();
    let row = readme
        .lines()
        .find(|l| l.starts_with("| `admin` |"))
        .expect("README.md no longer carries the `admin` command row");
    row.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty())
        .map(|w| w.to_string())
        .collect()
}

#[test]
fn every_admin_op_is_documented() {
    let words = readme_admin_row_words();
    // The ops the shipped schema advertises; keep the README admin row honest.
    for op in admin_ops() {
        assert!(
            words.iter().any(|w| w == &op),
            "admin op '{op}' is handled but the README admin row never names it"
        );
    }
}

#[test]
fn the_agent_instructions_name_every_admin_op() {
    // src/skill.md is the file that drives the agent, so an op missing from it
    // is unreachable through the documented flow no matter how well the README
    // reads. 0.16 shipped reverify and consolidate and this file kept
    // describing the pre-0.16 manual workaround for a full release: that is the
    // drift this guard exists to catch.
    let skill = skill_md();
    for op in admin_ops() {
        assert!(
            skill.contains(&format!("\"{op}\"")),
            "admin op '{op}' is shipped but src/skill.md never names it"
        );
    }
}

/// The comma-separated names inside the README list that opens at `marker`.
/// The guard reads the coverage lists themselves instead of the whole file
/// because a bare `contains` proved nothing: "go" occurs 26 times in unrelated
/// prose ("goes stale", "google", "algorithm"), "java" is a substring of
/// "javascript" so its assertion could never fail, and "bash" rides in on every
/// bash code fence and on the install one-liner. All five wave-2 names could be
/// deleted from both lists with the suite still green.
fn readme_list(readme: &str, marker: &str) -> Vec<String> {
    let start = readme
        .find(marker)
        .unwrap_or_else(|| panic!("README.md no longer opens a grammar list with '{marker}'"))
        + marker.len();
    let rest = &readme[start..];
    let end = rest
        .find(')')
        .unwrap_or_else(|| panic!("the grammar list opened by '{marker}' is never closed"));
    rest[..end]
        .split(',')
        .map(|name| name.trim().trim_matches('`').to_string())
        .collect()
}

/// How the README must spell a grammar: the prose name in the coverage
/// sentence, then the `.limpet.json` config name. The match is exhaustive on
/// purpose, so a new `Lang` variant cannot compile until someone states how
/// the docs name it.
fn readme_spellings(lang: Lang) -> (&'static str, &'static str) {
    match lang {
        Lang::Php => ("PHP", "php"),
        Lang::Js => ("JavaScript", "js"),
        Lang::Ts => ("TypeScript", "ts"),
        Lang::Py => ("Python", "py"),
        Lang::Rust => ("Rust", "rust"),
        Lang::Cpp => ("C/C++", "cpp"),
        Lang::Go => ("Go", "go"),
        Lang::Java => ("Java", "java"),
        Lang::Ruby => ("Ruby", "rb"),
        Lang::CSharp => ("C#", "cs"),
        Lang::Bash => ("Bash", "bash"),
    }
}

#[test]
fn every_shipped_grammar_is_documented() {
    // A shipped grammar the README does not name is a coverage lie, and so is
    // a README that names one that does not ship. Both README lists are
    // enumerated and matched whole-item, so no name can be satisfied by a
    // substring of unrelated text. Wave 2 (go, java, ruby, c#, bash) went
    // effectively unguarded until the 2026-08-17 audit, and c++ was never in
    // the list at all despite `Lang` shipping eleven variants.
    let readme = readme();
    let prose = readme_list(&readme, "shipped grammar (");
    let config = readme_list(&readme, "shipped grammars (");
    for lang in Lang::ALL {
        let (prose_name, config_name) = readme_spellings(lang);
        assert!(
            prose.iter().any(|n| n.eq_ignore_ascii_case(prose_name)),
            "grammar '{prose_name}' ships but the README coverage list omits it: {prose:?}"
        );
        assert!(
            config.iter().any(|n| n.eq_ignore_ascii_case(config_name)),
            "grammar '{config_name}' ships but the README .limpet.json list omits it: {config:?}"
        );
        // The config list is an offer, not prose: every name it advertises
        // must parse back to this grammar or the docs promise a value the
        // config parser rejects.
        assert_eq!(
            from_config_str(config_name),
            Some(lang),
            "README offers `{config_name}` but from_config_str does not map it to {lang:?}"
        );
    }
    assert_eq!(
        prose.len(),
        Lang::ALL.len(),
        "the README coverage list names {} grammars but {} ship: {prose:?}",
        prose.len(),
        Lang::ALL.len()
    );
    assert_eq!(
        config.len(),
        Lang::ALL.len(),
        "the README `.limpet.json` grammar list names {} but {} ship: {config:?}",
        config.len(),
        Lang::ALL.len()
    );
}

#[test]
fn documented_cli_commands_exist() {
    // Every `limpet <cmd>` the README lists must be a real subcommand. The
    // HELP string in main.rs is the source of truth; assert the README's
    // command set is a subset of it.
    let help = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/main.rs"))
        .expect("main.rs");
    for cmd in ["serve", "index", "status", "stats", "doctor", "export", "import", "install", "uninstall", "update", "ui", "statusline", "hook", "demo", "seed"] {
        assert!(
            help.contains(&format!("\"{cmd}\"")),
            "README documents `limpet {cmd}` but main.rs has no such match arm"
        );
    }
}

#[test]
fn the_015_recall_surfaces_are_documented() {
    // The 0.15 wire additions must stay documented, and their wording must
    // stay honest: the matched field's ABSENCE claim was corrected once
    // (whole-branch review 2026-08-04: FTS stems, matched does not, so
    // absence cannot promise "not vocabulary") and must not drift back.
    let readme = readme();
    assert!(
        readme.contains("`@<disamb>` suffix"),
        "the @disamb anchor syntax ships but README.md does not document it"
    );
    assert!(
        readme.contains("\"matched\": \"sweep anchored\""),
        "the matched field ships but README.md does not document it"
    );
    assert!(
        !readme.contains("not vocabulary"),
        "the matched-absence overclaim must not return to README.md"
    );

    let schemas = tool_schemas();
    let recall_desc = schemas
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "recall")
        .expect("recall tool ships")["description"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        recall_desc.contains("`matched`"),
        "the recall tool description must name the matched field"
    );
    assert!(
        !recall_desc.contains("not vocabulary"),
        "the matched-absence overclaim must not return to the tool description"
    );
}
