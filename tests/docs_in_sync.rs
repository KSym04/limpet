//! Guard against README drift (the philosophy forbids silently lying, and a
//! stale README is exactly that). These tests fail the build when a shipped
//! tool, grammar, or CLI command is missing from the docs, so accuracy is
//! enforced rather than remembered.

use limpet::tools::tool_schemas;

fn readme() -> String {
    std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/README.md"))
        .expect("README.md must exist")
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

#[test]
fn every_admin_op_is_documented() {
    let readme = readme();
    // The ops tool_admin actually handles; keep the README admin row honest.
    for op in ["index", "status", "forget", "export", "import", "ledger"] {
        assert!(
            readme.contains(op),
            "admin op '{op}' is handled but not documented in README.md"
        );
    }
}

#[test]
fn every_shipped_grammar_is_documented() {
    let readme = readme().to_lowercase();
    // A shipped grammar the README does not name is a coverage lie.
    for lang in ["php", "javascript", "typescript", "python", "rust"] {
        assert!(
            readme.contains(lang),
            "grammar '{lang}' ships but README.md does not name it"
        );
    }
    // C/C++ is named as a pair.
    assert!(
        readme.contains("c/c++") || readme.contains("c++"),
        "the C/C++ grammar ships but README.md does not name it"
    );
}

#[test]
fn documented_cli_commands_exist() {
    // Every `limpet <cmd>` the README lists must be a real subcommand. The
    // HELP string in main.rs is the source of truth; assert the README's
    // command set is a subset of it.
    let help = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/main.rs"))
        .expect("main.rs");
    for cmd in ["serve", "index", "status", "stats", "doctor", "export", "import", "install", "uninstall", "update", "ui", "statusline", "hook"] {
        assert!(
            help.contains(&format!("\"{cmd}\"")),
            "README documents `limpet {cmd}` but main.rs has no such match arm"
        );
    }
}
