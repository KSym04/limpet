//! Calibration + regression pin for the low-entropy follow guard thresholds.
//! The normalization buffer includes node-kind names, so the floor differs
//! per grammar; these fixtures pin every grammar's trivial and real bodies
//! to the correct side of MIN_FOLLOW_BODY_BYTES. A grammar upgrade that
//! shifts buffer composition fails here loudly instead of silently moving
//! the guard.

use limpet::index::extract;
use limpet::index::lang::Lang;
use limpet::memory::anchor;

/// (grammar, trivial-body source, real-body source)
fn fixtures() -> Vec<(Lang, &'static str, &'static str)> {
    vec![
        (Lang::Py, "def t():\n    pass\n",
         "def r(x):\n    y = x + 1\n    if y > 2:\n        return y * 3\n    return y\n"),
        (Lang::Js, "function t() {}\n",
         "function r(x) {\n  const y = x + 1;\n  if (y > 2) { return y * 3; }\n  return y;\n}\n"),
        (Lang::Ts, "function t(): void {}\n",
         "function r(x: number): number {\n  const y = x + 1;\n  if (y > 2) { return y * 3; }\n  return y;\n}\n"),
        (Lang::Php, "<?php\nfunction t() {}\n",
         "<?php\nfunction r($x) {\n  $y = $x + 1;\n  if ($y > 2) { return $y * 3; }\n  return $y;\n}\n"),
        (Lang::Rust, "fn t() {}\n",
         "fn r(x: i64) -> i64 {\n    let y = x + 1;\n    if y > 2 { return y * 3; }\n    y\n}\n"),
        (Lang::Cpp, "void t() {}\n",
         "int r(int x) {\n    int y = x + 1;\n    if (y > 2) { return y * 3; }\n    return y;\n}\n"),
        (Lang::Go, "func t() {}\n",
         "func r(x int) int {\n\ty := x + 1\n\tif y > 2 {\n\t\treturn y * 3\n\t}\n\treturn y\n}\n"),
        (Lang::Java, "class C { void t() {} }\n",
         "class C { int r(int x) {\n  int y = x + 1;\n  if (y > 2) { return y * 3; }\n  return y;\n} }\n"),
        (Lang::Ruby, "def t\nend\n",
         "def r(x)\n  y = x + 1\n  if y > 2\n    return y * 3\n  end\n  y\nend\n"),
        (Lang::CSharp, "class C { void T() {} }\n",
         "class C { int R(int x) {\n  var y = x + 1;\n  if (y > 2) { return y * 3; }\n  return y;\n} }\n"),
        (Lang::Bash, "t() { :; }\n",
         "r() {\n  local y=$(($1 + 1))\n  if [ \"$y\" -gt 2 ]; then echo $((y * 3)); return; fi\n  echo \"$y\"\n}\n"),
    ]
}

/// Measure the FUNCTION symbol's buffer length in `src` (the deepest-nested
/// symbol extract returns; for the class-wrapped Java/C# fixtures that is
/// the method, which is what an anchor would bind to).
fn measure(lang: Lang, src: &str) -> u32 {
    let facts = extract::extract(lang, src).unwrap();
    let sym = facts.symbols.last().expect("fixture must yield a symbol");
    anchor::ast_body_hashes(lang, src, &[sym.byte_range]).unwrap()[0].1
}

#[test]
fn print_calibration_table() {
    for (lang, trivial, real) in fixtures() {
        println!(
            "{:?}: trivial={} real={}",
            lang,
            measure(lang, trivial),
            measure(lang, real)
        );
    }
}

#[test]
fn thresholds_separate_trivial_from_real_on_every_grammar() {
    for (lang, trivial, real) in fixtures() {
        let t = measure(lang, trivial);
        let r = measure(lang, real);
        assert!(
            t < anchor::MIN_FOLLOW_BODY_BYTES,
            "{lang:?} trivial body ({t}B) must fall under the follow floor"
        );
        assert!(
            r >= anchor::MIN_FOLLOW_BODY_BYTES,
            "{lang:?} real body ({r}B) must clear the follow floor"
        );
    }
}

/// File-level companion to the body-byte pin above: MIN_FOLLOW_FILE_BYTES
/// must exclude an empty file and a lone-import line while clearing every
/// grammar's real fixture source (the shortest is Ruby at 63B).
#[test]
fn file_threshold_separates_trivial_from_real_content() {
    let empty = "";
    let lone_import = "import { helper } from \"./util\";\n";
    assert!(
        (empty.len() as i64) < anchor::MIN_FOLLOW_FILE_BYTES,
        "empty file ({}B) must fall under the file follow floor",
        empty.len()
    );
    assert!(
        (lone_import.len() as i64) < anchor::MIN_FOLLOW_FILE_BYTES,
        "lone-import file ({}B) must fall under the file follow floor",
        lone_import.len()
    );
    for (lang, _trivial, real) in fixtures() {
        assert!(
            (real.len() as i64) >= anchor::MIN_FOLLOW_FILE_BYTES,
            "{lang:?} real fixture source ({}B) must clear the file follow floor",
            real.len()
        );
    }
}
