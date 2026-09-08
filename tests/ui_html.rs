//! Guards on the embedded visual-memory page (`src/ui.html`). The page is a
//! display surface served by `limpet ui`; these pin the properties the
//! stability contract and the brain-view spec promise about the FILE, which
//! the route tests in `tests/stability.rs` cannot see:
//!
//! - one embedded file with no external references (I-B4): nothing is
//!   fetched from anywhere but the local server, so the UI works offline and
//!   leaks nothing;
//! - deterministic layout (I-B3): no `Math.random`, so two loads of the same
//!   data draw the same picture;
//! - the script parses (I-B4): a syntax error in the page would ship silently
//!   because no Rust test executes JavaScript. `node --check` runs when node
//!   is on PATH (every CI runner carries it); when it is not, the test says so
//!   on stderr and passes, so a machine without node can still run the suite.

use std::process::Command;

fn ui_html() -> String {
    std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/ui.html"))
        .expect("src/ui.html must exist")
}

/// The body of the page's single inline script block.
fn script_body(html: &str) -> String {
    let start = html.find("<script>").expect("ui.html has an inline <script> block") + "<script>".len();
    let end = html[start..].find("</script>").expect("the <script> block closes") + start;
    html[start..end].to_string()
}

#[test]
fn ui_page_is_one_file_with_no_external_references() {
    let html = ui_html();
    assert_eq!(
        html.matches("<script").count(),
        1,
        "ui.html must carry exactly one inline <script> block, no external scripts"
    );
    assert!(!html.contains("<script src"), "no external script may be loaded");
    assert!(!html.contains("<link rel=\"stylesheet\""), "no external stylesheet may be loaded");
    assert!(!html.contains("@import"), "no CSS import may reach out");
    assert!(!html.contains("url(http"), "no CSS asset may reach out");
    // The only absolute URL in the file is the SVG namespace inside the
    // inline favicon data URI; every fetch() targets a relative /api path.
    // The header's decorative `limpet://` has an empty scheme and is not a
    // URL; only a `<scheme>://` with letters in front of it counts.
    let urls: Vec<&str> = html
        .match_indices("://")
        .filter_map(|(i, _)| {
            let head = html[..i].rfind(|c: char| !c.is_ascii_alphabetic()).map_or(0, |p| p + 1);
            if head == i {
                return None;
            }
            let tail = html[i..].find(|c: char| c == '\'' || c == '"' || c == ')' || c.is_whitespace()).map_or(html.len(), |p| i + p);
            Some(&html[head..tail])
        })
        .collect();
    assert_eq!(
        urls,
        vec!["http://www.w3.org/2000/svg"],
        "unexpected absolute URL in ui.html: {urls:?}"
    );
    // Scheme-relative references (`//host/path`) carry no `://`; a `//`
    // right after a quote, a paren, or `=` is one. JS comments sit after
    // whitespace and never match.
    for pat in ["\"//", "'//", "(//", "=//"] {
        assert!(!html.contains(pat), "scheme-relative reference in ui.html: {pat}");
    }
    for (line_no, line) in html.lines().enumerate() {
        if let Some(pos) = line.find("fetch(") {
            let arg = &line[pos + "fetch(".len()..];
            assert!(
                arg.starts_with("\"/api/"),
                "line {}: fetch must target a relative /api path, got: {}",
                line_no + 1,
                line.trim()
            );
        }
    }
}

#[test]
fn ui_layout_is_deterministic_no_math_random() {
    let html = ui_html();
    assert!(
        !html.contains("Math.random"),
        "ui.html must not use Math.random: every per-node quantity derives from an id hash so the picture is stable across loads"
    );
}

#[test]
fn ui_script_parses_under_node_when_available() {
    let html = ui_html();
    let body = script_body(&html);
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("ui-script.js");
    std::fs::write(&path, &body).expect("write script body");
    let out = match Command::new("node").arg("--check").arg(&path).output() {
        Ok(out) => out,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // The skip is for developer machines only. CI runners carry
            // node, so there a missing node is a broken gate, not a skip.
            assert!(
                std::env::var_os("CI").is_none(),
                "node is required on CI for the ui.html syntax check"
            );
            eprintln!("ui_script_parses_under_node_when_available: node not on PATH, syntax check skipped on this machine (CI runs it)");
            return;
        }
        Err(e) => panic!("could not run node --check: {e}"),
    };
    assert!(
        out.status.success(),
        "ui.html's script does not parse:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
