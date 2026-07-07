//! Per-file structural extraction via tree-sitter.
//!
//! Produces symbols (functions, methods, classes, consts), import targets,
//! and *syntactic* call pairs. Calls are name-based with no type
//! resolution; every consumer labels them `confidence: "syntactic"`.

use crate::index::lang::{self, Lang};
use anyhow::{bail, Result};
use tree_sitter::{Node, Parser};

#[derive(Debug, Clone, PartialEq)]
pub struct Sym {
    pub name: String,
    /// "function" | "method" | "class" | "const"
    pub kind: &'static str,
    pub start_line: usize,
    pub end_line: usize,
    /// Enclosing symbol names, outermost first (e.g. ["ScanQueue"]).
    pub parents: Vec<String>,
    /// Byte range of the defining node, for body hashing.
    pub byte_range: (usize, usize),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Inherit {
    /// Enclosing symbol names of the child type, outermost first (FQN formed
    /// at persist time, mirroring `Sym`).
    pub parents: Vec<String>,
    /// The child type's own name.
    pub name: String,
    /// Bare syntactic name of the supertype (resolved read-time, I-G1).
    pub parent_name: String,
    /// "extends" | "implements" | "impl_trait"
    pub rel: &'static str,
}

#[derive(Debug, Default)]
pub struct FileFacts {
    pub symbols: Vec<Sym>,
    pub imports: Vec<String>,
    /// (enclosing symbol name or "<file>", callee name)
    pub calls: Vec<(String, String)>,
    pub inherits: Vec<Inherit>,
}

/// Parse `src` as `lang` and extract structural facts.
pub fn extract(lang_id: Lang, src: &str) -> Result<FileFacts> {
    let mut parser = Parser::new();
    parser
        .set_language(&lang::ts_language(lang_id))
        .map_err(|e| anyhow::anyhow!("grammar load failed: {e}"))?;
    // PHP grammar requires the `<?php` opening tag; add it when absent so
    // callers (tests, REPL snippets) can pass raw PHP code directly.
    let owned;
    let src = if lang_id == Lang::Php && !src.trim_start().starts_with("<?") {
        owned = format!("<?php\n{src}");
        owned.as_str()
    } else {
        src
    };
    let Some(tree) = parser.parse(src, None) else {
        bail!("tree-sitter returned no tree");
    };
    let mut facts = FileFacts::default();
    let mut parents: Vec<String> = Vec::new();
    walk(lang_id, tree.root_node(), src.as_bytes(), &mut parents, &mut facts);
    Ok(facts)
}

fn node_text(node: Node, src: &[u8]) -> String {
    String::from_utf8_lossy(&src[node.byte_range()]).into_owned()
}

fn name_of(node: Node, src: &[u8]) -> Option<String> {
    node.child_by_field_name("name").map(|n| node_text(n, src))
}

fn push_inherit(
    facts: &mut FileFacts,
    parents: &[String],
    name: &str,
    parent_name: String,
    rel: &'static str,
) {
    facts.inherits.push(Inherit {
        parents: parents.to_vec(),
        name: name.to_string(),
        parent_name,
        rel,
    });
}

/// Collect bare type names from a clause node, taking each child that is a
/// plain name/identifier and skipping punctuation, keywords, and generics.
fn base_names(clause: Node, src: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut c = clause.walk();
    for ch in clause.children(&mut c) {
        match ch.kind() {
            "name" | "identifier" | "type_identifier" | "qualified_name"
            | "scoped_type_identifier" | "namespace_name" | "dotted_name" => {
                out.push(node_text(ch, src));
            }
            _ => {}
        }
    }
    out
}

fn push_sym(
    facts: &mut FileFacts,
    node: Node,
    _src: &[u8],
    parents: &[String],
    kind: &'static str,
    name: String,
) {
    facts.symbols.push(Sym {
        name,
        kind,
        start_line: node.start_position().row + 1,
        end_line: node.end_position().row + 1,
        parents: parents.to_vec(),
        byte_range: (node.start_byte(), node.end_byte()),
    });
}

/// Extract the callee name from a call-ish node's function part.
fn callee_name(func: Node, src: &[u8]) -> Option<String> {
    match func.kind() {
        "identifier" | "name" => Some(node_text(func, src)),
        // js/ts obj.method(), php $obj->method(), python obj.attr(),
        // rust path::func() or obj.method()
        "member_expression" | "attribute" => func
            .child_by_field_name("property")
            .or_else(|| func.child_by_field_name("attribute"))
            .map(|n| node_text(n, src)),
        "field_expression" => func
            .child_by_field_name("field")
            .map(|n| node_text(n, src)),
        // rust path::func(), c++ Namespace::func()
        "scoped_identifier" | "qualified_identifier" => func
            .child_by_field_name("name")
            .map(|n| node_text(n, src)),
        _ => None,
    }
}

/// C/C++ definition name: descend the declarator chain to the naming node.
/// Out-of-line `GLGaeaClient::GetSkinChar` anchors as `GetSkinChar`; the
/// file-scoped FQN plus body hash disambiguate, and true duplicates surface
/// as `ambiguous_anchor` rather than being guessed.
fn cpp_declarator_name(node: Node, src: &[u8]) -> Option<String> {
    let mut cur = node.child_by_field_name("declarator")?;
    loop {
        match cur.kind() {
            "function_declarator" | "pointer_declarator" | "reference_declarator"
            | "parenthesized_declarator" => {
                cur = cur.child_by_field_name("declarator")?;
            }
            "identifier" | "field_identifier" | "destructor_name" | "operator_name" => {
                return Some(node_text(cur, src));
            }
            "qualified_identifier" => match cur.child_by_field_name("name") {
                Some(n) if n.kind() == "qualified_identifier" => cur = n,
                Some(n) => return Some(node_text(n, src)),
                None => return None,
            },
            _ => return None,
        }
    }
}

fn current_scope(parents: &[String]) -> String {
    parents.last().cloned().unwrap_or_else(|| "<file>".to_string())
}

fn walk(
    lang_id: Lang,
    node: Node,
    src: &[u8],
    parents: &mut Vec<String>,
    facts: &mut FileFacts,
) {
    let kind = node.kind();
    let mut pushed_parent = false;

    match lang_id {
        Lang::Php => match kind {
            "function_definition" => {
                if let Some(name) = name_of(node, src) {
                    push_sym(facts, node, src, parents, "function", name.clone());
                    parents.push(name);
                    pushed_parent = true;
                }
            }
            "method_declaration" => {
                if let Some(name) = name_of(node, src) {
                    push_sym(facts, node, src, parents, "method", name.clone());
                    parents.push(name);
                    pushed_parent = true;
                }
            }
            "class_declaration" | "interface_declaration" | "trait_declaration" => {
                if let Some(name) = name_of(node, src) {
                    push_sym(facts, node, src, parents, "class", name.clone());
                    let mut cc = node.walk();
                    for ch in node.children(&mut cc) {
                        match ch.kind() {
                            "base_clause" => {
                                for p in base_names(ch, src) {
                                    push_inherit(facts, parents, &name, p, "extends");
                                }
                            }
                            "class_interface_clause" => {
                                for p in base_names(ch, src) {
                                    push_inherit(facts, parents, &name, p, "implements");
                                }
                            }
                            _ => {}
                        }
                    }
                    parents.push(name);
                    pushed_parent = true;
                }
            }
            "namespace_use_declaration" => {
                // use Foo\Bar; -> import target Foo\Bar
                let mut c = node.walk();
                for ch in node.children(&mut c) {
                    if ch.kind() == "namespace_use_clause" {
                        facts.imports.push(node_text(ch, src));
                    }
                }
            }
            "function_call_expression" | "member_call_expression"
            | "scoped_call_expression" => {
                if let Some(func) = node
                    .child_by_field_name("function")
                    .or_else(|| node.child_by_field_name("name"))
                {
                    if let Some(callee) = callee_name(func, src) {
                        facts.calls.push((current_scope(parents), callee));
                    }
                }
            }
            _ => {}
        },
        Lang::Js | Lang::Ts => match kind {
            "function_declaration" | "generator_function_declaration" => {
                if let Some(name) = name_of(node, src) {
                    push_sym(facts, node, src, parents, "function", name.clone());
                    parents.push(name);
                    pushed_parent = true;
                }
            }
            "method_definition" => {
                if let Some(name) = name_of(node, src) {
                    push_sym(facts, node, src, parents, "method", name.clone());
                    parents.push(name);
                    pushed_parent = true;
                }
            }
            "class_declaration" | "abstract_class_declaration" => {
                if let Some(name) = name_of(node, src) {
                    push_sym(facts, node, src, parents, "class", name.clone());
                    // class_heritage is not a named field in JS/TS grammars; locate by kind.
                    let heritage = (0..node.child_count())
                        .filter_map(|i| node.child(i))
                        .find(|c| c.kind() == "class_heritage");
                    if let Some(h) = heritage {
                        let mut hc = h.walk();
                        for ch in h.children(&mut hc) {
                            match ch.kind() {
                                "extends_clause" => {
                                    for p in base_names(ch, src) {
                                        push_inherit(facts, parents, &name, p, "extends");
                                    }
                                }
                                "implements_clause" => {
                                    for p in base_names(ch, src) {
                                        push_inherit(facts, parents, &name, p, "implements");
                                    }
                                }
                                // JS: class_heritage has direct identifier children (no
                                // extends_clause wrapper), all are extends targets.
                                "identifier" | "type_identifier" => {
                                    push_inherit(facts, parents, &name, node_text(ch, src), "extends");
                                }
                                _ => {}
                            }
                        }
                    }
                    parents.push(name);
                    pushed_parent = true;
                }
            }
            "import_statement" => {
                if let Some(srcn) = node.child_by_field_name("source") {
                    facts
                        .imports
                        .push(node_text(srcn, src).trim_matches(['"', '\'']).to_string());
                }
            }
            "call_expression" => {
                if let Some(func) = node.child_by_field_name("function") {
                    if let Some(callee) = callee_name(func, src) {
                        facts.calls.push((current_scope(parents), callee));
                    }
                }
            }
            _ => {}
        },
        Lang::Py => match kind {
            "function_definition" => {
                if let Some(name) = name_of(node, src) {
                    let k = if parents.is_empty() { "function" } else { "method" };
                    push_sym(facts, node, src, parents, k, name.clone());
                    parents.push(name);
                    pushed_parent = true;
                }
            }
            "class_definition" => {
                if let Some(name) = name_of(node, src) {
                    push_sym(facts, node, src, parents, "class", name.clone());
                    if let Some(args) = node.child_by_field_name("superclasses") {
                        let mut ac = args.walk();
                        for ch in args.children(&mut ac) {
                            // Positional bases only: identifier / dotted_name.
                            // keyword_argument (metaclass=...) is skipped.
                            if matches!(ch.kind(), "identifier" | "dotted_name" | "attribute") {
                                push_inherit(facts, parents, &name, node_text(ch, src), "extends");
                            }
                        }
                    }
                    parents.push(name);
                    pushed_parent = true;
                }
            }
            "import_statement" | "import_from_statement" => {
                let mut c = node.walk();
                for ch in node.children(&mut c) {
                    if matches!(ch.kind(), "dotted_name" | "aliased_import") {
                        facts.imports.push(node_text(ch, src));
                    }
                }
            }
            "call" => {
                if let Some(func) = node.child_by_field_name("function") {
                    if let Some(callee) = callee_name(func, src) {
                        facts.calls.push((current_scope(parents), callee));
                    }
                }
            }
            _ => {}
        },
        Lang::Cpp => match kind {
            "function_definition" => {
                if let Some(name) = cpp_declarator_name(node, src) {
                    let k = if parents.is_empty() { "function" } else { "method" };
                    push_sym(facts, node, src, parents, k, name.clone());
                    parents.push(name);
                    pushed_parent = true;
                }
            }
            "class_specifier" | "struct_specifier" | "enum_specifier" => {
                // Named types only; anonymous structs/enums stay unanchored.
                if let Some(name) = name_of(node, src) {
                    push_sym(facts, node, src, parents, "class", name.clone());
                    let bc_opt = node.child_by_field_name("bases").or_else(|| {
                        (0..node.child_count())
                            .filter_map(|i| node.child(i))
                            .find(|c| c.kind() == "base_class_clause")
                    });
                    if let Some(bc) = bc_opt {
                        for p in base_names(bc, src) {
                            push_inherit(facts, parents, &name, p, "extends");
                        }
                    }
                    parents.push(name);
                    pushed_parent = true;
                }
            }
            "namespace_definition" => {
                // Scope for FQNs, not a symbol itself (like Rust impl_item).
                if let Some(name) = name_of(node, src) {
                    parents.push(name);
                    pushed_parent = true;
                }
            }
            "preproc_include" => {
                if let Some(p) = node.child_by_field_name("path") {
                    facts
                        .imports
                        .push(node_text(p, src).trim_matches(['"', '<', '>']).to_string());
                }
            }
            "call_expression" => {
                if let Some(func) = node.child_by_field_name("function") {
                    if let Some(callee) = callee_name(func, src) {
                        facts.calls.push((current_scope(parents), callee));
                    }
                }
            }
            _ => {}
        },
        Lang::Rust => match kind {
            "function_item" => {
                if let Some(name) = name_of(node, src) {
                    let k = if parents.is_empty() { "function" } else { "method" };
                    push_sym(facts, node, src, parents, k, name.clone());
                    parents.push(name);
                    pushed_parent = true;
                }
            }
            "struct_item" | "enum_item" | "trait_item" => {
                if let Some(name) = name_of(node, src) {
                    push_sym(facts, node, src, parents, "class", name.clone());
                    if node.kind() == "trait_item" {
                        if let Some(b) = node.child_by_field_name("bounds") {
                            for p in base_names(b, src) {
                                push_inherit(facts, parents, &name, p, "extends");
                            }
                        }
                    }
                    parents.push(name);
                    pushed_parent = true;
                }
            }
            "impl_item" => {
                let ty = node.child_by_field_name("type").map(|t| node_text(t, src));
                if let (Some(tf), Some(ty_name)) =
                    (node.child_by_field_name("trait"), ty.as_ref())
                {
                    // `impl Trait for Type` -> Type impl_trait Trait. Skip
                    // generic/complex trait names (contain '<' or "::").
                    let trait_name = node_text(tf, src);
                    if !trait_name.contains('<') && !trait_name.contains("::") {
                        push_inherit(facts, parents, ty_name, trait_name, "impl_trait");
                    }
                }
                if let Some(t) = node.child_by_field_name("type") {
                    parents.push(node_text(t, src));
                    pushed_parent = true;
                }
            }
            "const_item" | "static_item" => {
                if let Some(name) = name_of(node, src) {
                    push_sym(facts, node, src, parents, "const", name);
                }
            }
            "use_declaration" => {
                if let Some(arg) = node.child_by_field_name("argument") {
                    facts.imports.push(node_text(arg, src));
                }
            }
            "call_expression" => {
                if let Some(func) = node.child_by_field_name("function") {
                    if let Some(callee) = callee_name(func, src) {
                        facts.calls.push((current_scope(parents), callee));
                    }
                }
            }
            _ => {}
        },
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(lang_id, child, src, parents, facts);
    }

    if pushed_parent {
        parents.pop();
    }
}

#[cfg(test)]
mod inherit_tests {
    use super::*;
    use crate::index::lang::Lang;

    fn edges(lang: Lang, src: &str) -> Vec<(String, String, String, &'static str)> {
        extract(lang, src)
            .unwrap()
            .inherits
            .into_iter()
            .map(|i| (i.parents.join("."), i.name, i.parent_name, i.rel))
            .collect()
    }


#[test]
    fn php_extends_and_implements() {
        let e = edges(Lang::Php, "class Dog extends Animal implements Pet, Runner {}");
        assert!(e.contains(&(String::new(), "Dog".into(), "Animal".into(), "extends")));
        assert!(e.contains(&(String::new(), "Dog".into(), "Pet".into(), "implements")));
        assert!(e.contains(&(String::new(), "Dog".into(), "Runner".into(), "implements")));
    }

    #[test]
    fn js_class_extends() {
        let e = edges(Lang::Js, "class Dog extends Animal {}");
        assert_eq!(e, vec![(String::new(), "Dog".into(), "Animal".into(), "extends")]);
    }

    #[test]
    fn ts_extends_and_implements() {
        let e = edges(Lang::Ts, "class Dog extends Animal implements Pet {}");
        assert!(e.contains(&(String::new(), "Dog".into(), "Animal".into(), "extends")));
        assert!(e.contains(&(String::new(), "Dog".into(), "Pet".into(), "implements")));
    }

    #[test]
    fn python_bases_skip_keyword() {
        let e = edges(Lang::Py, "class Dog(Animal, metaclass=Meta):\n    pass\n");
        assert!(e.contains(&(String::new(), "Dog".into(), "Animal".into(), "extends")));
        assert!(!e.iter().any(|(_, _, p, _)| p == "Meta"), "metaclass= is not a base");
    }

    #[test]
    fn rust_impl_trait_for_type() {
        let e = edges(Lang::Rust, "impl Animal for Dog { fn speak(&self) {} }");
        assert_eq!(e, vec![(String::new(), "Dog".into(), "Animal".into(), "impl_trait")]);
    }

    #[test]
    fn cpp_base_class_clause_multiple() {
        let e = edges(Lang::Cpp, "class Dog : public Animal, private Pet {};");
        assert!(e.contains(&(String::new(), "Dog".into(), "Animal".into(), "extends")));
        assert!(e.contains(&(String::new(), "Dog".into(), "Pet".into(), "extends")));
    }

    #[test]
    fn malformed_supertype_no_panic() {
        // Generic/templated bases are skipped, never panic.
        let _ = extract(Lang::Rust, "impl<T> Foo<T> for Bar<T> {}").unwrap();
        let _ = extract(Lang::Cpp, "template<class T> class X : public Y<T> {};").unwrap();
    }
}
