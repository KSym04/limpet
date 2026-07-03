//! Language registry: extension detection and tree-sitter grammar handles.
//!
//! Six curated grammars ship (PHP, JS, TS, Python, Rust, C/C++). Every
//! language listed here has fixture
//! coverage in `tests/index_langs.rs` (invariant I7); adding a language
//! without a fixture is a review-blocking change.

use tree_sitter::Language;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lang {
    Php,
    Js,
    Ts,
    Py,
    Rust,
    Cpp,
}

impl Lang {
    pub fn as_str(&self) -> &'static str {
        match self {
            Lang::Php => "php",
            Lang::Js => "javascript",
            Lang::Ts => "typescript",
            Lang::Py => "python",
            Lang::Rust => "rust",
            Lang::Cpp => "cpp",
        }
    }
}

/// Map a file path to a supported language by extension.
///
/// `.h` and `.c` go to the C++ grammar: tree-sitter-cpp parses plain C,
/// and per-file parse isolation means an occasional Objective-C header
/// degrades to parse_ok=0 without affecting anything else.
pub fn detect(path: &std::path::Path) -> Option<Lang> {
    match path.extension()?.to_str()? {
        "php" => Some(Lang::Php),
        "js" | "mjs" | "cjs" | "jsx" => Some(Lang::Js),
        "ts" | "tsx" | "mts" | "cts" => Some(Lang::Ts),
        "py" => Some(Lang::Py),
        "rs" => Some(Lang::Rust),
        "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" | "h" | "c" | "inl" => Some(Lang::Cpp),
        _ => None,
    }
}

/// The tree-sitter grammar for a language.
pub fn ts_language(lang: Lang) -> Language {
    match lang {
        Lang::Php => tree_sitter_php::LANGUAGE_PHP.into(),
        Lang::Js => tree_sitter_javascript::LANGUAGE.into(),
        Lang::Ts => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        Lang::Py => tree_sitter_python::LANGUAGE.into(),
        Lang::Rust => tree_sitter_rust::LANGUAGE.into(),
        Lang::Cpp => tree_sitter_cpp::LANGUAGE.into(),
    }
}
