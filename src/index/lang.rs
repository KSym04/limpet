//! Language registry: extension detection and tree-sitter grammar handles.
//!
//! Ten curated grammars ship (PHP, JS, TS, Python, Rust, C/C++, Go, Java, Ruby, C#). Every
//! language listed here has fixture
//! coverage in `tests/index_langs.rs` (invariant I7); adding a language
//! without a fixture is a review-blocking change.

use std::collections::HashMap;
use tree_sitter::Language;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lang {
    Php,
    Js,
    Ts,
    Py,
    Rust,
    Cpp,
    Go,
    Java,
    Ruby,
    CSharp,
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
            Lang::Go => "go",
            Lang::Java => "java",
            Lang::Ruby => "ruby",
            Lang::CSharp => "c_sharp",
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
        "go" => Some(Lang::Go),
        "java" => Some(Lang::Java),
        "rb" | "rake" => Some(Lang::Ruby),
        "cs" => Some(Lang::CSharp),
        _ => None,
    }
}

/// Parse a grammar name from a `.limpet.json` extension-map value. Accepts
/// short and long spellings so config authors need not know the internal
/// `as_str` form.
pub fn from_config_str(s: &str) -> Option<Lang> {
    match s.to_ascii_lowercase().as_str() {
        "php" => Some(Lang::Php),
        "js" | "javascript" => Some(Lang::Js),
        "ts" | "typescript" => Some(Lang::Ts),
        "py" | "python" => Some(Lang::Py),
        "rs" | "rust" => Some(Lang::Rust),
        "cpp" | "c" | "c++" => Some(Lang::Cpp),
        "go" | "golang" => Some(Lang::Go),
        "java" => Some(Lang::Java),
        "rb" | "ruby" => Some(Lang::Ruby),
        "cs" | "csharp" | "c#" => Some(Lang::CSharp),
        _ => None,
    }
}

/// Map a file path to a language, consulting a user-supplied extension
/// override map before the built-in table. Keys are name suffixes without a
/// leading dot (`inc`, `blade.php`); the longest matching suffix wins, so a
/// specific `blade.php` beats a generic `php`. A match requires a `.` before
/// the suffix so `inc` never matches `zinc`. When no user key matches, the
/// built-in `detect` decides.
pub fn detect_with(path: &std::path::Path, ext_map: &HashMap<String, Lang>) -> Option<Lang> {
    if !ext_map.is_empty() {
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            let mut best: Option<(usize, Lang)> = None;
            for (suffix, lang) in ext_map {
                let matches = name.len() > suffix.len()
                    && name.as_bytes()[name.len() - suffix.len() - 1] == b'.'
                    && name.ends_with(suffix.as_str());
                if matches && best.map_or(true, |(len, _)| suffix.len() > len) {
                    best = Some((suffix.len(), *lang));
                }
            }
            if let Some((_, lang)) = best {
                return Some(lang);
            }
        }
    }
    detect(path)
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
        Lang::Go => tree_sitter_go::LANGUAGE.into(),
        Lang::Java => tree_sitter_java::LANGUAGE.into(),
        Lang::Ruby => tree_sitter_ruby::LANGUAGE.into(),
        Lang::CSharp => tree_sitter_c_sharp::LANGUAGE.into(),
    }
}
