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

#[derive(Debug, Default)]
pub struct FileFacts {
    pub symbols: Vec<Sym>,
    pub imports: Vec<String>,
    /// (enclosing symbol name or "<file>", callee name)
    pub calls: Vec<(String, String)>,
}

/// Parse `src` as `lang` and extract structural facts.
pub fn extract(lang_id: Lang, src: &str) -> Result<FileFacts> {
    let mut parser = Parser::new();
    parser
        .set_language(&lang::ts_language(lang_id))
        .map_err(|e| anyhow::anyhow!("grammar load failed: {e}"))?;
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
                    parents.push(name);
                    pushed_parent = true;
                }
            }
            "impl_item" => {
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
