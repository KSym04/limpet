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
    /// Overload/impl discriminator: trait name (Rust `impl Trait for Type`),
    /// receiver type (Go), canonicalized parameter list plus trailing
    /// cv/ref/exception qualifiers (C++), or canonicalized parameter list
    /// prefixed by the explicit interface qualifier when there is one
    /// (Java/C#). Inherited from the innermost enclosing symbol/impl when the
    /// symbol has none of its own. Never enters the body hash.
    pub disamb: Option<String>,
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

/// One syntactic call edge, carrying the FULL scope path of the caller.
///
/// The innermost name alone is not enough: `caller_fqn` has to equal the
/// calling symbol's own `symbols.fqn` or the lineage join finds nothing, and
/// that FQN is built from every enclosing scope (class, namespace, C++
/// qualifier, Rust `mod`, enclosing function), not just the last one.
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    /// Enclosing scope names of the CALLING symbol, outermost first (FQN
    /// formed at persist time, mirroring `Sym` and `Inherit`). Empty for a
    /// call made at file top level.
    pub parents: Vec<String>,
    /// The calling symbol's own name, or the `<file>` sentinel for a call made
    /// outside every symbol. A scope that emits no symbol row of its own (Rust
    /// `mod`, C++/PHP namespace, Rust `impl`) can never appear here: a call in
    /// a module-level initializer belongs to no symbol and says so.
    pub name: String,
    /// Bare syntactic name of the callee (resolved read-time, I-G1).
    pub callee: String,
}

#[derive(Debug, Default)]
pub struct FileFacts {
    pub symbols: Vec<Sym>,
    pub imports: Vec<String>,
    pub calls: Vec<Call>,
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
    // Strip a leading BOM before deciding whether the PHP open tag is present,
    // so a real <?php file that starts with a BOM is NOT prepended (which would
    // shift every symbol's byte range and corrupt its body hash).
    let has_tag = src.trim_start_matches('\u{feff}').trim_start().starts_with("<?");
    let owned;
    let src = if lang_id == Lang::Php && !has_tag {
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
    let root_ctx = Ctx { inherited: None, tdepth: 0, sym_len: 0 };
    walk(lang_id, tree.root_node(), src.as_bytes(), &mut parents, &mut facts, root_ctx);
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
            | "scoped_type_identifier" | "namespace_name" | "dotted_name"
            | "constant" => {
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
    disamb: Option<String>,
) {
    facts.symbols.push(Sym {
        name,
        kind,
        start_line: node.start_position().row + 1,
        end_line: node.end_position().row + 1,
        parents: parents.to_vec(),
        byte_range: (node.start_byte(), node.end_byte()),
        disamb,
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
        // C# this.Foo() / obj.Foo()
        "member_access_expression" => func
            .child_by_field_name("name")
            .map(|n| node_text(n, src)),
        _ => None,
    }
}

/// Anything that is neither an identifier character nor whitespace. Spacing
/// beside one of these is formatting, never meaning, in every language whose
/// parameter lists feed the discriminator.
fn is_sig_punct(ch: char) -> bool {
    !ch.is_alphanumeric() && ch != '_' && !ch.is_whitespace()
}

/// Canonical whitespace for a discriminator fragment: runs collapse to one
/// space, leading/trailing space is trimmed, and space touching a punctuation
/// character is dropped outright.
///
/// This holds the discriminator still across the formatting-only edits a
/// formatter actually performs: a rewrapped parameter list, `(int a,double b)`
/// vs `(int a, double b)`, `int *p` vs `int* p`, `A<B<int> >` vs `A<B<int>>`.
/// It does NOT hold across edits that change tokens: renaming a parameter,
/// reordering parameters, or adding a default argument each move the symbol to
/// a new slot. Comments are removed upstream by `sig_text`, not here.
fn collapse_ws(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_ws = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            in_ws = true;
            continue;
        }
        if in_ws && !out.is_empty() && !is_sig_punct(ch) && !out.ends_with(is_sig_punct) {
            out.push(' ');
        }
        in_ws = false;
        out.push(ch);
    }
    out
}

/// Every comment node kind across the vendored grammars: C/C++/C#/Go/Python/
/// PHP/Rust spell it `comment`, tree-sitter-java splits it into `line_comment`
/// and `block_comment`.
fn is_comment_kind(kind: &str) -> bool {
    matches!(kind, "comment" | "line_comment" | "block_comment")
}

/// Raw text of `node` with every comment descendant blanked to spaces. A
/// comment written inside a parameter list must not reach the discriminator:
/// the body hash already treats comments as noise, so letting them into the
/// slot would re-anchor a symbol on a comment-only edit.
fn text_sans_comments(node: Node, src: &[u8]) -> String {
    let base = node.start_byte();
    let mut bytes = src[base..node.end_byte()].to_vec();
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if is_comment_kind(n.kind()) {
            // Whole-node byte range, so both ends sit on char boundaries; the
            // clamps only guard against a malformed tree, never normal input.
            let lo = n.start_byte().saturating_sub(base).min(bytes.len());
            let hi = n.end_byte().saturating_sub(base).min(bytes.len());
            for b in &mut bytes[lo..hi] {
                *b = b' ';
            }
            continue;
        }
        let mut c = n.walk();
        for ch in n.children(&mut c) {
            stack.push(ch);
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// One canonical discriminator fragment: comments stripped, whitespace
/// canonicalized.
fn sig_text(node: Node, src: &[u8]) -> String {
    collapse_ws(&text_sans_comments(node, src))
}

/// Canonical parameter list of a definition node, for the grammars that spell
/// it as a plain `parameters` field (Python, JS/TS, Java, C#).
///
/// Overloading is not the only reason a language needs this. Python's
/// `@property` / `@x.setter` pair and JS/TS's `get x()` / `set x(v)` pair are
/// two DIFFERENT functions sharing one name inside one class; the parameter
/// list is what tells them apart, because a setter always takes the value it
/// sets. Without it both land in one slot and either half answers Fresh for an
/// edit to the other.
fn params_signature(node: Node, src: &[u8]) -> Option<String> {
    node.child_by_field_name("parameters").map(|p| sig_text(p, src))
}

/// C++ overload signature: the parameter list plus the qualifiers that follow
/// it. The grammar makes `const`/`volatile` (`type_qualifier`), `&`/`&&`
/// (`ref_qualifier`), and `noexcept`/`throw(...)` SIBLINGS of the `parameters`
/// field inside function_declarator, proved by to_sexp:
///
/// ```text
/// (function_declarator declarator: (field_identifier)
///                      parameters: (parameter_list (parameter_declaration ...))
///                      (type_qualifier))
/// ```
///
/// Reading `parameters` alone therefore files every cv/ref-qualified overload
/// pair into one slot. `&` and `&&` share the single kind `ref_qualifier`, so
/// the text is what separates them, not the kind.
///
/// Deliberately excluded: `virtual_specifier` (`override`/`final` is not part
/// of the function type and gets added long after a method is written, so it
/// would churn anchors for free), `trailing_return_type` (a return type never
/// distinguishes two overloads), attributes, and `requires_clause` (a
/// constraint CAN distinguish overloads, but it is an unbounded expression;
/// constrained twins stay in one slot).
fn cpp_signature(fd: Node, src: &[u8]) -> Option<String> {
    let params = fd.child_by_field_name("parameters")?;
    let mut out = sig_text(params, src);
    let mut c = fd.walk();
    for ch in fd.children(&mut c) {
        if matches!(
            ch.kind(),
            "type_qualifier" | "ref_qualifier" | "noexcept" | "throw_specifier"
        ) {
            out.push(' ');
            out.push_str(&sig_text(ch, src));
        }
    }
    Some(out)
}

/// C/C++ definition identity from the declarator chain: qualifier segments
/// (out-of-line `A::run` scopes under `A`), the naming leaf, and the overload
/// signature.
///
/// Each qualifier segment is the RAW source text of that scope, which keeps a
/// template qualifier like `A<T>` intact and keeps the segment stable under
/// reformatting. Raw text is not a resolved scope, so two spellings of the
/// same entity do NOT converge on one FQN:
///   - `template <class T> void A<T>::run() {}` yields `file.A<T>.run`, while
///     the in-class definition of the same member yields `file.A.run`;
///   - `namespace outer { void outer::f() {} }` yields `file.outer.outer.f`,
///     because the lexical namespace scope and the redundant qualifier are
///     both counted.
///
/// Both shapes are rare and self-consistent (the same source always produces
/// the same FQN), so they cost recall on a hand-written FQN, never correctness
/// of the slot.
///
/// Descending the chain by the `declarator` FIELD alone silently dropped every
/// reference-returning and parenthesized definition. tree-sitter-cpp 0.23.4
/// `node-types.json` declares `reference_declarator fields = []` and
/// `parenthesized_declarator fields = []`: their inner declarator is an
/// UNNAMED child, proved by to_sexp:
///
/// ```text
/// (function_definition type: (primitive_type)
///   declarator: (reference_declarator
///     (function_declarator declarator: (field_identifier) parameters: (...))))
/// ```
///
/// so `child_by_field_name("declarator")` returned None and `char& at(int)`,
/// `const T& name() const`, `Buffer& operator=(...)` and `int (f)(int)` all
/// extracted NO symbol at all: unanchorable, uninspectable by `map`, and
/// invisible to lineage. The first-named-child fallback is what recovers them.
/// It cannot perturb any shape that already worked: `pointer_declarator` and
/// `function_declarator` DO declare a `declarator` field, so the fallback fires
/// only where the walk previously bailed to None.
fn cpp_definition_parts(node: Node, src: &[u8]) -> Option<(Vec<String>, String, Option<String>)> {
    let mut cur = node.child_by_field_name("declarator")?;
    let mut quals: Vec<String> = Vec::new();
    let mut params: Option<String> = None;
    loop {
        match cur.kind() {
            "function_declarator" | "pointer_declarator" | "reference_declarator"
            | "parenthesized_declarator" => {
                // The innermost function_declarator adjacent to the naming
                // leaf holds the function's own parameter list.
                if cur.kind() == "function_declarator" {
                    params = cpp_signature(cur, src);
                }
                cur = match cur.child_by_field_name("declarator") {
                    Some(d) => d,
                    // Fieldless wrapper (`&`/`&&`/parens): the single named
                    // child IS the declarator, the sigil is an anonymous
                    // token. A comment can sit between them and is also a
                    // named child, so never land the walk on one.
                    None => (0..cur.named_child_count())
                        .filter_map(|i| cur.named_child(i))
                        .find(|c| c.kind() != "comment")?,
                };
            }
            "identifier" | "field_identifier" | "destructor_name" | "operator_name" => {
                return Some((quals, node_text(cur, src), params));
            }
            // An explicit specialization names itself with a template id:
            // `template <> void f<int>(int x) {}` parses as
            // `(function_declarator declarator: (template_function
            //   name: (identifier) arguments: (template_argument_list ...)))`.
            // Matching only the plain leaves dropped every specialization.
            // The arguments prefix the discriminator (the C# explicit-interface
            // precedent) so `f<int>` and `f<double>` never share a slot while
            // both keep the FQN a reader would write, `f`.
            "template_function" => {
                let n = cur.child_by_field_name("name")?;
                let args = cur
                    .child_by_field_name("arguments")
                    .map(|a| sig_text(a, src))
                    .unwrap_or_default();
                let d = params.map(|p| format!("{args}{p}"));
                return Some((quals, node_text(n, src), d));
            }
            "qualified_identifier" => {
                // A global-scope qualifier (`::run`) has no scope field and
                // contributes no segment.
                if let Some(s) = cur.child_by_field_name("scope") {
                    quals.push(node_text(s, src));
                }
                match cur.child_by_field_name("name") {
                    Some(n) if matches!(n.kind(), "qualified_identifier" | "template_function") => {
                        cur = n
                    }
                    Some(n) => return Some((quals, node_text(n, src), params)),
                    None => return None,
                }
            }
            _ => return None,
        }
    }
}

/// Template parameter list of the `template_declaration` wrapping `node`, if
/// any. Two overloads can differ ONLY in their template parameters:
///
/// ```text
/// template <typename T> void make() {}
/// template <typename T, typename U> void make() {}
/// ```
///
/// Both spell the FQN `make` and both take `()`, so the parameter list alone
/// files them in one slot and an identical body in either one masks an edit to
/// the other. The list is a field of the ENCLOSING node, not of the definition:
/// `(template_declaration parameters: (template_parameter_list ...)
/// (function_definition ...))`, so it is read through `parent()`.
fn cpp_template_params(node: Node, src: &[u8]) -> Option<String> {
    let p = node.parent()?;
    if p.kind() != "template_declaration" {
        return None;
    }
    p.child_by_field_name("parameters").map(|t| sig_text(t, src))
}

/// C# explicit interface qualifier, or "" when the member is an ordinary one.
///
/// `class C : IA, IB { void IA.M() {} void IB.M() {} }` gives both members the
/// FQN `C.M` and the parameter list `()`, so the parameter list alone files
/// them in one slot. The qualifier is a separate named child with no field,
/// proved by to_sexp:
///
/// ```text
/// (method_declaration returns: (predefined_type)
///                     (explicit_interface_specifier (identifier))
///                     name: (identifier) parameters: (parameter_list) body: (block))
/// ```
///
/// Following the Rust trait-name precedent, the reference is kept exactly as
/// the source spells it (qualification and generic arguments included). The
/// node's text already carries the trailing `.`, so the discriminator reads
/// `IA.(int a)`; an ordinary overload can never collide with that, since a
/// parameter list always starts with `(`.
fn csharp_explicit_iface(node: Node, src: &[u8]) -> String {
    (0..node.child_count())
        .filter_map(|i| node.child(i))
        .find(|c| c.kind() == "explicit_interface_specifier")
        .map(|c| sig_text(c, src))
        .unwrap_or_default()
}

/// Go receiver type raw text (e.g. `*A`) for method disambiguation: two
/// types can define same-named methods in one package.
fn go_receiver_type(node: Node, src: &[u8]) -> Option<String> {
    let recv = node.child_by_field_name("receiver")?;
    let mut c = recv.walk();
    let decl = recv
        .children(&mut c)
        .find(|ch| ch.kind() == "parameter_declaration")?;
    decl.child_by_field_name("type").map(|t| node_text(t, src))
}

/// Sentinel scope for a call made outside every symbol (file top level).
pub const FILE_SCOPE: &str = "<file>";

/// Discriminators for the half of a twin pair that has nothing of its own to
/// say, in the three languages where the OTHER half does.
///
/// A NULL `symbols.disamb` is the right answer only when NO symbol sharing that
/// FQN can carry one, because `anchors.disamb` overloads NULL: it means both
/// "this anchor predates the discriminator" (whose fates are frozen at 0.14 by
/// invariant I-F2) and "this anchor's slot is the undiscriminated one". An
/// anchor on the undiscriminated half therefore falls into the legacy ladder,
/// where ANY row at `(fqn, hash)` reads Fresh, and the discriminated twin still
/// carrying the old body answers for it. Verified end to end: a Rust inherent
/// method twinned by a trait impl of the same name reported `fresh: 1` and
/// stayed `active` after the inherent body was rewritten.
///
/// Rather than teach the ladder to tell the two NULL meanings apart (which
/// needs a slot representation change and a wire-format change with it), each
/// of the three mixed spaces gets a total discriminator, so no method-shaped
/// symbol in them is ever NULL and the overload can never bite. Every marker is
/// a token the alternative can never spell: `impl` and `func` are reserved
/// words in their languages, so neither can be a trait path or a receiver type,
/// and `#` opens a comment in Ruby, so it can never be a receiver expression.
/// They are user-visible in a `remember` spec (`go@impl`, `Foo@func`, `m@#`).
const RUST_INHERENT_IMPL: &str = "impl";
const GO_PACKAGE_FUNC: &str = "func";
const RUBY_INSTANCE_METHOD: &str = "#";

/// Record a call edge anchored to the innermost enclosing SYMBOL, keeping the
/// whole path above it so the persist step can rebuild the caller's real FQN.
///
/// `parents` is not usable whole: it also grows for scopes that emit no symbol
/// row (Rust `mod` and `impl`, C++/PHP namespace, Ruby `class << self`). Taking
/// its last element unconditionally attributed a call in a module-level
/// initializer to a `caller_fqn` no `symbols` row answers to (`a.inner`,
/// `a.A`), which the lineage join silently drops and `map` prints as a caller
/// that cannot be looked up. `sym_len` is the length of the `parents` prefix
/// ending at the innermost frame that DID emit a symbol, so
/// `parents[..sym_len]` is exactly what `push_sym` was handed plus that
/// symbol's own name on top, and splitting the last element off reproduces the
/// caller's `(parents, name)` pair byte for byte. `sym_len == 0` means no
/// enclosing symbol at all: the `<file>` sentinel.
fn push_call(facts: &mut FileFacts, parents: &[String], sym_len: usize, callee: String) {
    let scope = &parents[..sym_len.min(parents.len())];
    let (up, own) = match scope.split_last() {
        Some((last, up)) => (up.to_vec(), last.clone()),
        None => (Vec::new(), FILE_SCOPE.to_string()),
    };
    facts.calls.push(Call { parents: up, name: own, callee });
}

/// Number of enclosing TYPE scopes: how many struct/enum/trait/impl/class/
/// interface scopes the walk is currently inside.
///
/// This is deliberately NOT the depth of `parents`. `parents` also grows for
/// module and namespace scopes (Rust `mod`, C++/PHP `namespace`) and for
/// enclosing functions, none of which make an inner `fn` a member of a type.
/// Deciding kind from `parents.is_empty()` mislabels every function in an
/// inline `mod` (`mod tests` above all) as a method, and every function in a
/// C++ `namespace` too.
type TypeDepth = usize;

/// True for the node kinds that make their contents members of a type, so
/// entering one turns an inner function into a method.
///
/// Ruby's `module` counts: it emits a class row of its own and its `def`s are
/// instance methods of the mixin, unlike Rust `mod` and PHP/C++ `namespace`,
/// which emit no row and hold free functions.
fn is_type_scope(lang_id: Lang, kind: &str) -> bool {
    match lang_id {
        Lang::Php => matches!(
            kind,
            "class_declaration" | "interface_declaration" | "trait_declaration"
                | "enum_declaration"
        ),
        Lang::Js | Lang::Ts => matches!(kind, "class_declaration" | "abstract_class_declaration"),
        Lang::Py => kind == "class_definition",
        Lang::Cpp => matches!(kind, "class_specifier" | "struct_specifier" | "enum_specifier"),
        Lang::Rust => matches!(kind, "struct_item" | "enum_item" | "trait_item" | "impl_item"),
        Lang::Go => kind == "type_spec",
        Lang::Ruby => matches!(kind, "class" | "module"),
        Lang::Java => matches!(
            kind,
            "class_declaration" | "interface_declaration" | "enum_declaration"
                | "record_declaration"
        ),
        Lang::CSharp => matches!(
            kind,
            "class_declaration" | "interface_declaration" | "struct_declaration"
        ),
        Lang::Bash => false,
    }
}

/// "function" outside every type scope, "method" inside one.
fn fn_kind(tdepth: TypeDepth) -> &'static str {
    if tdepth == 0 {
        "function"
    } else {
        "method"
    }
}

/// What the enclosing scopes have established for the node being walked.
///
/// These three travel together and are all derived the same way (each arm may
/// hand its children a new value, otherwise the incoming one passes through),
/// so they ride in one struct rather than as three parallel parameters.
#[derive(Clone, Copy)]
struct Ctx<'a> {
    /// Discriminator of the innermost enclosing symbol or impl, inherited by a
    /// symbol that has none of its own.
    inherited: Option<&'a str>,
    /// Number of enclosing TYPE scopes (see `TypeDepth`).
    tdepth: TypeDepth,
    /// Length of the `parents` prefix ending at the innermost frame that
    /// emitted a symbol row (see `push_call`).
    sym_len: usize,
}

fn walk(
    lang_id: Lang,
    node: Node,
    src: &[u8],
    parents: &mut Vec<String>,
    facts: &mut FileFacts,
    ctx: Ctx,
) {
    let Ctx { inherited, tdepth, sym_len } = ctx;
    let kind = node.kind();
    // Symbol rows emitted before this node's arm ran: the difference after it
    // is what proves whether the frame this node pushed IS a symbol (see the
    // `child_sym_len` computation below).
    let syms_before = facts.symbols.len();
    // C++ qualified definitions push one scope level per qualifier segment.
    let mut pushed: usize = 0;
    // Type scopes this node opens for its children. A type-like node counts
    // only when its arm actually pushed it (an anonymous struct pushes
    // nothing, so it opens nothing).
    let mut tpushed: TypeDepth = 0;
    // Disamb context for children: a symbol's own discriminator wins; absent
    // one, the innermost enclosing symbol's/impl's disamb flows through
    // (single level, no concatenation). A nested helper collides across twin
    // impls or overloads exactly like its parent, so it must carry the same
    // discriminator.
    let mut own_disamb: Option<String> = None;

    match lang_id {
        Lang::Php => match kind {
            "function_definition" => {
                if let Some(name) = name_of(node, src) {
                    let d = inherited.map(str::to_string);
                    push_sym(facts, node, src, parents, "function", name.clone(), d);
                    parents.push(name);
                    pushed += 1;
                }
            }
            "method_declaration" => {
                if let Some(name) = name_of(node, src) {
                    let d = inherited.map(str::to_string);
                    push_sym(facts, node, src, parents, "method", name.clone(), d);
                    parents.push(name);
                    pushed += 1;
                }
            }
            // `enum_declaration` (PHP 8.1) is a type like any other: its
            // methods are members of it. Leaving it out gave two enums in one
            // file the SAME method FQN (`file.label` twice, no discriminator),
            // so an edit to one was masked by the other's identical body, and
            // `map SomeEnum` found nothing because the enum emitted no row.
            // Vendored tree-sitter-php 0.23.11: `enum_declaration
            // fields=['attributes','body','name']`.
            "class_declaration" | "interface_declaration" | "trait_declaration"
            | "enum_declaration" => {
                if let Some(name) = name_of(node, src) {
                    let d = inherited.map(str::to_string);
                    push_sym(facts, node, src, parents, "class", name.clone(), d);
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
                    pushed += 1;
                }
            }
            "namespace_definition" => {
                // Bracketed `namespace A { ... }` only: the sole legal
                // multi-namespace-per-file form, so the only collision class.
                // Unbracketed `namespace A;` stays invisible; a
                // single-namespace file cannot collide with itself.
                if node.child_by_field_name("body").is_some() {
                    if let Some(name) = name_of(node, src) {
                        parents.push(name);
                        pushed += 1;
                    }
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
                        push_call(facts, parents, sym_len, callee);
                    }
                }
            }
            _ => {}
        },
        Lang::Js | Lang::Ts => match kind {
            "function_declaration" | "generator_function_declaration" => {
                if let Some(name) = name_of(node, src) {
                    let d = params_signature(node, src).or_else(|| inherited.map(str::to_string));
                    push_sym(facts, node, src, parents, "function", name.clone(), d.clone());
                    own_disamb = d;
                    parents.push(name);
                    pushed += 1;
                }
            }
            // `get x()` and `set x(v)` are both `method_definition` named `x`,
            // so without a discriminator an accessor pair shares one slot and
            // either half masks an edit to the other. The parameter list is
            // what separates them (a setter always takes the value), the same
            // rule Java/C# already use.
            "method_definition" => {
                if let Some(name) = name_of(node, src) {
                    let d = params_signature(node, src).or_else(|| inherited.map(str::to_string));
                    push_sym(facts, node, src, parents, "method", name.clone(), d.clone());
                    own_disamb = d;
                    parents.push(name);
                    pushed += 1;
                }
            }
            // TypeScript `namespace A { ... }` (internal_module) and the legacy
            // `module A { ... }` (a distinct `module` node with the same shape),
            // the direct analogue of PHP's bracketed namespace: one file legally
            // holds several, and without the segment two same-named classes in
            // two namespaces produce one FQN per method. Body-bearing only, and
            // never string-named: an ambient `declare module "x" { ... }` also
            // carries a statement_block body, so the name kind is what keeps a
            // quoted module specifier out of the FQN. Neither kind exists in the
            // JS grammar, where this arm simply never matches.
            "internal_module" | "module" => {
                let string_named = node
                    .child_by_field_name("name")
                    .is_some_and(|n| n.kind() == "string");
                if !string_named && node.child_by_field_name("body").is_some() {
                    if let Some(name) = name_of(node, src) {
                        parents.push(name);
                        pushed += 1;
                    }
                }
            }
            "class_declaration" | "abstract_class_declaration" => {
                if let Some(name) = name_of(node, src) {
                    let d = inherited.map(str::to_string);
                    push_sym(facts, node, src, parents, "class", name.clone(), d);
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
                    pushed += 1;
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
                        push_call(facts, parents, sym_len, callee);
                    }
                }
            }
            _ => {}
        },
        Lang::Py => match kind {
            // A decorated `def` is a plain `function_definition` under a
            // `decorated_definition` wrapper, so `@property def x` and
            // `@x.setter def x` were two rows on one slot with no
            // discriminator, and so were the members of an `@overload` set.
            // The parameter list splits every one of those: an overload set
            // exists BECAUSE the signatures differ, and a setter always takes
            // the value it sets.
            "function_definition" => {
                if let Some(name) = name_of(node, src) {
                    let k = fn_kind(tdepth);
                    let d = params_signature(node, src)
                        .or_else(|| inherited.map(str::to_string));
                    push_sym(facts, node, src, parents, k, name.clone(), d.clone());
                    own_disamb = d;
                    parents.push(name);
                    pushed += 1;
                }
            }
            "class_definition" => {
                if let Some(name) = name_of(node, src) {
                    let d = inherited.map(str::to_string);
                    push_sym(facts, node, src, parents, "class", name.clone(), d);
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
                    pushed += 1;
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
                        push_call(facts, parents, sym_len, callee);
                    }
                }
            }
            _ => {}
        },
        Lang::Cpp => match kind {
            "function_definition" => {
                if let Some((quals, name, params)) = cpp_definition_parts(node, src) {
                    // An out-of-line `void A::run() {}` IS a member of `A`, so
                    // every qualifier segment counts as a type scope. The
                    // ambiguous shape is `namespace N { void N::f() {} }`: a
                    // qualifier cannot be told from a namespace syntactically,
                    // and the member reading is far commoner.
                    let k = fn_kind(tdepth + quals.len());
                    tpushed += quals.len();
                    for q in quals {
                        parents.push(q);
                        pushed += 1;
                    }
                    // The enclosing template's parameter list prefixes the
                    // discriminator, so two overloads differing only in their
                    // template parameters do not share a slot.
                    let d = match (cpp_template_params(node, src), params) {
                        (Some(t), Some(p)) => Some(format!("{t}{p}")),
                        (Some(t), None) => Some(t),
                        (None, p) => p,
                    };
                    let d = d.or_else(|| inherited.map(str::to_string));
                    push_sym(facts, node, src, parents, k, name.clone(), d.clone());
                    own_disamb = d;
                    parents.push(name);
                    pushed += 1;
                }
            }
            "class_specifier" | "struct_specifier" | "enum_specifier" => {
                // Named types only; anonymous structs/enums stay unanchored.
                if let Some(name) = name_of(node, src) {
                    let d = inherited.map(str::to_string);
                    push_sym(facts, node, src, parents, "class", name.clone(), d);
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
                    pushed += 1;
                }
            }
            "namespace_definition" => {
                // Scope for FQNs, not a symbol itself (like Rust impl_item).
                if let Some(name) = name_of(node, src) {
                    parents.push(name);
                    pushed += 1;
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
                        push_call(facts, parents, sym_len, callee);
                    }
                }
            }
            _ => {}
        },
        Lang::Rust => match kind {
            "function_item" => {
                if let Some(name) = name_of(node, src) {
                    let k = fn_kind(tdepth);
                    let d = inherited.map(str::to_string);
                    push_sym(facts, node, src, parents, k, name.clone(), d);
                    parents.push(name);
                    pushed += 1;
                }
            }
            "struct_item" | "enum_item" | "trait_item" => {
                if let Some(name) = name_of(node, src) {
                    let d = inherited.map(str::to_string);
                    push_sym(facts, node, src, parents, "class", name.clone(), d);
                    if node.kind() == "trait_item" {
                        if let Some(b) = node.child_by_field_name("bounds") {
                            for p in base_names(b, src) {
                                push_inherit(facts, parents, &name, p, "extends");
                            }
                        }
                    }
                    parents.push(name);
                    pushed += 1;
                }
            }
            "impl_item" => {
                let ty = node.child_by_field_name("type").map(|t| node_text(t, src));
                if let (Some(tf), Some(ty_name)) =
                    (node.child_by_field_name("trait"), ty.as_ref())
                {
                    // `impl Trait for Type` -> Type impl_trait Trait. Skip
                    // only generic trait names (angle brackets). A qualified
                    // path like `fmt::Display` is captured on its LAST segment,
                    // matching how the read-time resolver matches bare symbol names.
                    let trait_name = node_text(tf, src);
                    if !trait_name.contains('<') {
                        let bare = trait_name
                            .rsplit("::")
                            .next()
                            .unwrap_or(&trait_name)
                            .to_string();
                        push_inherit(facts, parents, ty_name, bare, "impl_trait");
                    }
                }
                // Everything under `impl Trait for Type` carries the trait
                // name: twin impls of one type produce colliding FQNs. An
                // INHERENT impl carries the `impl` marker rather than nothing,
                // because a NULL slot is the one the anchor ladder cannot tell
                // from a legacy anchor: `impl T { fn go }` twinned by
                // `impl A for T { fn go }` left the inherent half maskable.
                // Consts and statics inherit this too, and needed it for the
                // same reason.
                own_disamb = Some(match node.child_by_field_name("trait") {
                    Some(tf) => node_text(tf, src),
                    None => RUST_INHERENT_IMPL.to_string(),
                });
                if let Some(t) = node.child_by_field_name("type") {
                    parents.push(node_text(t, src));
                    pushed += 1;
                }
            }
            "mod_item" => {
                // Scope for FQNs, not a symbol itself (C++
                // namespace_definition precedent). Declaration-only `mod x;`
                // has no body and contributes nothing.
                if node.child_by_field_name("body").is_some() {
                    if let Some(name) = name_of(node, src) {
                        parents.push(name);
                        pushed += 1;
                    }
                }
            }
            "const_item" | "static_item" => {
                if let Some(name) = name_of(node, src) {
                    let d = inherited.map(str::to_string);
                    push_sym(facts, node, src, parents, "const", name, d);
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
                        push_call(facts, parents, sym_len, callee);
                    }
                }
            }
            _ => {}
        },
        Lang::Go => match kind {
            // A package function and a method of the same name share one FQN
            // (a method's receiver lives in the discriminator, not the path),
            // so `func Foo()` alongside `func (a *A) Foo()` put an
            // undiscriminated row next to a discriminated one. The marker keeps
            // the package function's slot nameable instead of NULL.
            "function_declaration" => {
                if let Some(name) = name_of(node, src) {
                    let d = inherited
                        .map(str::to_string)
                        .or_else(|| Some(GO_PACKAGE_FUNC.to_string()));
                    push_sym(facts, node, src, parents, "function", name.clone(), d.clone());
                    own_disamb = d;
                    parents.push(name);
                    pushed += 1;
                }
            }
            "method_declaration" => {
                if let Some(name) = name_of(node, src) {
                    let d = go_receiver_type(node, src)
                        .or_else(|| inherited.map(str::to_string));
                    push_sym(facts, node, src, parents, "method", name.clone(), d.clone());
                    own_disamb = d;
                    parents.push(name);
                    pushed += 1;
                }
            }
            "type_spec" => {
                if let Some(name) = name_of(node, src) {
                    let d = inherited.map(str::to_string);
                    push_sym(facts, node, src, parents, "class", name.clone(), d);
                    // Embedded fields (no field name) in the struct/interface body
                    // are Go's composition; record as `embeds`.
                    // tree-sitter-go 0.25 wraps field declarations in a
                    // `field_declaration_list` node inside `struct_type`; descend
                    // one extra level when that wrapper is present.
                    if let Some(body) = node.child_by_field_name("type") {
                        let field_list = (0..body.child_count())
                            .filter_map(|i| body.child(i))
                            .find(|c| c.kind() == "field_declaration_list")
                            .unwrap_or(body);
                        let mut bc = field_list.walk();
                        for f in field_list.children(&mut bc) {
                            // struct: field_declaration with a type and no `name`
                            // field; interface: embedded type_identifier directly.
                            if f.kind() == "field_declaration"
                                && f.child_by_field_name("name").is_none()
                            {
                                if let Some(t) = f
                                    .child_by_field_name("type")
                                    .or_else(|| f.named_child(0))
                                {
                                    push_inherit(
                                        facts,
                                        parents,
                                        &name,
                                        node_text(t, src),
                                        "embeds",
                                    );
                                }
                            } else if matches!(f.kind(), "type_identifier" | "qualified_type") {
                                push_inherit(
                                    facts,
                                    parents,
                                    &name,
                                    node_text(f, src),
                                    "embeds",
                                );
                            }
                        }
                    }
                    parents.push(name);
                    pushed += 1;
                }
            }
            "import_spec" => {
                if let Some(p) = node
                    .child_by_field_name("path")
                    .or_else(|| node.named_child(0))
                {
                    facts
                        .imports
                        .push(node_text(p, src).trim_matches('"').to_string());
                }
            }
            "call_expression" => {
                if let Some(func) = node.child_by_field_name("function") {
                    if let Some(callee) = callee_name(func, src) {
                        push_call(facts, parents, sym_len, callee);
                    }
                }
            }
            _ => {}
        },
        Lang::Ruby => match kind {
            // `def m` and `def self.m` are two DIFFERENT methods of one class,
            // and Ruby's FQN scheme has no room for the `C#m` / `C.m` split it
            // writes them with, so both spelled `C.m` with no discriminator and
            // masked each other. `singleton_method` carries the receiver in its
            // `object` field, proved by to_sexp:
            //
            //   (singleton_method object: (self) name: (identifier) body: (..))
            //
            // The receiver plus a `.` is the discriminator (the C# explicit-
            // interface precedent: a suffix that no parameter list can spell).
            // An instance method keeps None, so the two never collide.
            "method" | "singleton_method" => {
                if let Some(name) = name_of(node, src) {
                    let k = fn_kind(tdepth);
                    let d = node
                        .child_by_field_name("object")
                        .map(|o| format!("{}.", sig_text(o, src)))
                        .or_else(|| inherited.map(str::to_string))
                        .or_else(|| Some(RUBY_INSTANCE_METHOD.to_string()));
                    push_sym(facts, node, src, parents, k, name.clone(), d.clone());
                    own_disamb = d;
                    parents.push(name);
                    pushed += 1;
                }
            }
            // `class << self` reopens the singleton class, so every `def`
            // inside it is a class method twinning the instance method of the
            // same name. It is NOT a scope that emits a symbol and NOT an FQN
            // segment (`map C.m` must still find both halves); it is context
            // that flows down as the discriminator, exactly like `impl Trait`
            // in Rust. Grammar: `(singleton_class value: (self) body: (..))`,
            // and `value` is `obj` for the `class << obj` form.
            "singleton_class" => {
                let recv = node
                    .child_by_field_name("value")
                    .map(|v| sig_text(v, src))
                    .unwrap_or_else(|| "self".to_string());
                own_disamb = Some(format!("<<{recv}."));
            }
            "class" | "module" => {
                if let Some(name) = name_of(node, src) {
                    let d = inherited.map(str::to_string);
                    push_sym(facts, node, src, parents, "class", name.clone(), d);
                    // class Dog < Animal -> superclass field
                    if let Some(sc) = node.child_by_field_name("superclass") {
                        for p in base_names(sc, src) {
                            push_inherit(facts, parents, &name, p, "extends");
                        }
                    }
                    parents.push(name);
                    pushed += 1;
                }
            }
            "call" => {
                // include/prepend/extend Mod -> mixin, anchored to the enclosing class;
                // require '...' -> import; everything else -> call edge.
                let mname = node
                    .child_by_field_name("method")
                    .map(|m| node_text(m, src))
                    .unwrap_or_default();
                match mname.as_str() {
                    "include" | "prepend" | "extend" => {
                        if let Some(cls) = parents.last().cloned() {
                            if let Some(args) = node.child_by_field_name("arguments") {
                                for p in base_names(args, src) {
                                    let up = &parents[..parents.len() - 1];
                                    push_inherit(facts, up, &cls, p, "mixin");
                                }
                            }
                        }
                    }
                    "require" | "require_relative" => {
                        if let Some(args) = node.child_by_field_name("arguments") {
                            facts.imports.push(
                                node_text(args, src)
                                    .trim_matches(['(', ')', '"', '\'', ' '])
                                    .to_string(),
                            );
                        }
                    }
                    other if !other.is_empty() => {
                        push_call(facts, parents, sym_len, other.to_string());
                    }
                    _ => {}
                }
            }
            _ => {}
        },
        Lang::Java => match kind {
            "method_declaration" => {
                if let Some(name) = name_of(node, src) {
                    let d = params_signature(node, src)
                        .or_else(|| inherited.map(str::to_string));
                    push_sym(facts, node, src, parents, "method", name.clone(), d.clone());
                    own_disamb = d;
                    parents.push(name);
                    pushed += 1;
                }
            }
            // `enum` and `record` are types with bodies exactly like `class`:
            // their methods are members of them. Leaving them out scoped every
            // such method to the ENCLOSING class instead, so two nested enums
            // with a same-named method produced one FQN and one `()` slot, and
            // an identical body in either masked an edit to the other. Vendored
            // tree-sitter-java 0.23.5: `enum_declaration
            // fields=['body','interfaces','name']`, `record_declaration
            // fields=['body','interfaces','name','parameters','type_parameters']`,
            // so both feed the existing name/interfaces handling unchanged.
            "class_declaration" | "interface_declaration" | "enum_declaration"
            | "record_declaration" => {
                if let Some(name) = name_of(node, src) {
                    let d = inherited.map(str::to_string);
                    push_sym(facts, node, src, parents, "class", name.clone(), d);
                    if let Some(sc) = node.child_by_field_name("superclass") {
                        for p in base_names(sc, src) {
                            push_inherit(facts, parents, &name, p, "extends");
                        }
                    }
                    if let Some(ifc) = node.child_by_field_name("interfaces") {
                        // super_interfaces -> type_list -> type_identifier
                        let clause = (0..ifc.child_count())
                            .filter_map(|i| ifc.child(i))
                            .find(|c| c.kind() == "type_list")
                            .unwrap_or(ifc);
                        for p in base_names(clause, src) {
                            push_inherit(facts, parents, &name, p, "implements");
                        }
                    }
                    // interface X extends Y, Z -> extends
                    // extends_interfaces -> type_list -> type_identifier (parallel
                    // structure to super_interfaces above, must descend type_list).
                    if node.kind() == "interface_declaration" {
                        let mut c = node.walk();
                        for ch in node.children(&mut c) {
                            if ch.kind() == "extends_interfaces" {
                                let clause = (0..ch.child_count())
                                    .filter_map(|i| ch.child(i))
                                    .find(|c| c.kind() == "type_list")
                                    .unwrap_or(ch);
                                for p in base_names(clause, src) {
                                    push_inherit(facts, parents, &name, p, "extends");
                                }
                            }
                        }
                    }
                    parents.push(name);
                    pushed += 1;
                }
            }
            "import_declaration" => {
                facts.imports.push(
                    node_text(node, src)
                        .trim_start_matches("import")
                        .trim()
                        .trim_end_matches(';')
                        .trim()
                        .to_string(),
                );
            }
            "method_invocation" => {
                if let Some(name) = node.child_by_field_name("name") {
                    push_call(facts, parents, sym_len, node_text(name, src));
                }
            }
            _ => {}
        },
        Lang::Bash => match kind {
            "function_definition" => {
                let name_opt = name_of(node, src).or_else(|| {
                    // bash function name is often a `word` child, not a `name` field
                    let mut c = node.walk();
                    let x = node.children(&mut c).find(|ch| ch.kind() == "word").map(|w| node_text(w, src)); x
                });
                if let Some(name) = name_opt {
                    let d = inherited.map(str::to_string);
                    push_sym(facts, node, src, parents, "function", name.clone(), d);
                    parents.push(name);
                    pushed += 1;
                }
            }
            "command" => {
                // first word is the command name; `source`/`.` -> import, else call.
                let cmd = node
                    .child_by_field_name("name")
                    .or_else(|| node.named_child(0))
                    .map(|n| node_text(n, src))
                    .unwrap_or_default();
                if cmd == "source" || cmd == "." {
                    let arg_text: Option<String> = {
                        let mut c = node.walk();
                        let x = node.children(&mut c).nth(1).map(|arg| node_text(arg, src)); x
                    };
                    if let Some(t) = arg_text {
                        facts.imports.push(t.trim_matches(['"', '\'']).to_string());
                    }
                } else if !cmd.is_empty() {
                    push_call(facts, parents, sym_len, cmd);
                }
            }
            _ => {}
        },
        Lang::CSharp => match kind {
            // Generic ARITY is part of a C# signature: `void M<T>()` and
            // `void M<T, U>()` are legal together and both take `()`, so the
            // parameter list alone filed them in one slot. `type_parameters` is
            // a declared field of `method_declaration` in the vendored
            // tree-sitter-c-sharp 0.23.5, proved by to_sexp:
            //
            //   (method_declaration returns: (predefined_type) name: (identifier)
            //    type_parameters: (type_parameter_list ...) parameters: (..))
            //
            // It sits between the explicit-interface prefix and the parameter
            // list, so the discriminator reads `IA.<T>(int a)` in source order.
            "method_declaration" => {
                if let Some(name) = name_of(node, src) {
                    let d = params_signature(node, src).map(|p| {
                        let tp = node
                            .child_by_field_name("type_parameters")
                            .map(|t| sig_text(t, src))
                            .unwrap_or_default();
                        format!("{}{}{}", csharp_explicit_iface(node, src), tp, p)
                    });
                    let d = d.or_else(|| inherited.map(str::to_string));
                    push_sym(facts, node, src, parents, "method", name.clone(), d.clone());
                    own_disamb = d;
                    parents.push(name);
                    pushed += 1;
                }
            }
            // Block `namespace A { ... }`, the C# analogue of PHP's bracketed
            // namespace and the only C# form a file can legally repeat. Two
            // `namespace X { class Options { void Validate() } }` blocks in one
            // file produced ONE FQN and ONE `()` slot, so an identical body in
            // either masked an edit to the other. `file_scoped_namespace_
            // declaration` (`namespace A;`) is a DIFFERENT kind in the vendored
            // grammar and is deliberately left alone: a file can hold only one,
            // so it cannot collide with itself, and every modern file-scoped C#
            // file keeps the FQNs it already had.
            "namespace_declaration" => {
                if node.child_by_field_name("body").is_some() {
                    if let Some(name) = name_of(node, src) {
                        parents.push(name);
                        pushed += 1;
                    }
                }
            }
            "class_declaration" | "interface_declaration" | "struct_declaration" => {
                if let Some(name) = name_of(node, src) {
                    let d = inherited.map(str::to_string);
                    push_sym(facts, node, src, parents, "class", name.clone(), d);
                    let bl = node.child_by_field_name("bases").or_else(|| {
                        (0..node.child_count())
                            .filter_map(|i| node.child(i))
                            .find(|ch| ch.kind() == "base_list")
                    });
                    if let Some(bl) = bl {
                        for p in base_names(bl, src) {
                            push_inherit(facts, parents, &name, p, "extends");
                        }
                    }
                    parents.push(name);
                    pushed += 1;
                }
            }
            "using_directive" => {
                facts.imports.push(
                    node_text(node, src)
                        .trim_start_matches("using")
                        .trim()
                        .trim_end_matches(';')
                        .trim()
                        .to_string(),
                );
            }
            "invocation_expression" => {
                if let Some(func) = node.child_by_field_name("function") {
                    if let Some(callee) = callee_name(func, src) {
                        push_call(facts, parents, sym_len, callee);
                    }
                }
            }
            _ => {}
        },
    }

    // A type-like node opens a type scope for its children only when its arm
    // actually pushed it: an anonymous struct pushes nothing, so it opens
    // nothing. (C++ qualifier segments already added themselves above.)
    if pushed > 0 && is_type_scope(lang_id, kind) {
        tpushed += 1;
    }

    // Innermost SYMBOL frame for the children (see `push_call`). A node hands
    // its children a new one only when it both pushed a frame AND emitted a
    // symbol row, in which case that row's name is the frame it just pushed:
    // every arm that emits a symbol pushes that symbol's own name LAST (C++
    // pushes its qualifier segments first, then the name). Arms that push
    // without emitting (Rust `mod`/`impl`, C++/PHP namespace, TS
    // `internal_module`, Ruby `class << self`) and arms that emit without
    // pushing (Rust `const`/`static`) both fall through and pass `sym_len`
    // down untouched, which is what keeps a module-level call honest.
    let child_sym_len = if pushed > 0 && facts.symbols.len() > syms_before {
        parents.len()
    } else {
        sym_len
    };

    let child_ctx = Ctx {
        inherited: own_disamb.as_deref().or(inherited),
        tdepth: tdepth + tpushed,
        sym_len: child_sym_len,
    };
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(lang_id, child, src, parents, facts, child_ctx);
    }

    for _ in 0..pushed {
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

    /// (parents joined with '.', name, disamb) per symbol, extraction order.
    fn syms(lang: Lang, src: &str) -> Vec<(String, String, Option<String>)> {
        extract(lang, src)
            .unwrap()
            .symbols
            .into_iter()
            .map(|s| (s.parents.join("."), s.name, s.disamb))
            .collect()
    }

    fn find<'a>(
        rows: &'a [(String, String, Option<String>)],
        parents: &str,
        name: &str,
    ) -> Vec<&'a (String, String, Option<String>)> {
        rows.iter().filter(|(p, n, _)| p == parents && n == name).collect()
    }

    /// (parents joined with '.', name, kind) per symbol, extraction order.
    fn kinds(lang: Lang, src: &str) -> Vec<(String, String, &'static str)> {
        extract(lang, src)
            .unwrap()
            .symbols
            .into_iter()
            .map(|s| (s.parents.join("."), s.name, s.kind))
            .collect()
    }

    /// Every discriminator on a `(parents, name)` slot, order-independent.
    fn disambs(
        rows: &[(String, String, Option<String>)],
        parents: &str,
        name: &str,
    ) -> Vec<Option<String>> {
        find(rows, parents, name).iter().map(|(_, _, d)| d.clone()).collect()
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

    #[test]
    fn rust_impl_qualified_trait_path() {
        let e = edges(Lang::Rust, "impl fmt::Display for Dog { }");
        assert_eq!(e, vec![(String::new(), "Dog".into(), "Display".into(), "impl_trait")]);
    }

    #[test]
    fn rust_trait_supertrait_captured() {
        let e = edges(Lang::Rust, "trait Working: Animal { }");
        assert!(e.contains(&(String::new(), "Working".into(), "Animal".into(), "extends")));
    }

    #[test]
    fn disamb_rust_twin_trait_impls_same_fqn_distinct_disamb() {
        let src = "struct T;\n\
                   impl A for T { fn go(&self) {} }\n\
                   impl B for T { fn go(&self) {} }\n\
                   impl T { fn go(&self) {} }\n";
        let rows = syms(Lang::Rust, src);
        let gos = find(&rows, "T", "go");
        assert_eq!(gos.len(), 3, "three same-FQN methods: {rows:?}");
        let disambs: Vec<_> = gos.iter().map(|(_, _, d)| d.clone()).collect();
        assert!(disambs.contains(&Some("A".to_string())), "{rows:?}");
        assert!(disambs.contains(&Some("B".to_string())), "{rows:?}");
        // NOT None: an anchor recording a NULL slot is indistinguishable from a
        // legacy anchor, and legacy anchors take the 0.14 ladder where any row
        // at (fqn, hash) reads Fresh. That left the inherent method maskable by
        // either trait twin. `impl` is a reserved word, so it can never be
        // confused with a trait path.
        assert!(disambs.contains(&Some("impl".to_string())), "{rows:?}");
    }

    #[test]
    fn disamb_is_total_wherever_a_twin_can_carry_one() {
        // The three mixed spaces: a discriminated symbol sharing an FQN with an
        // undiscriminated one. Each is verified end to end in anchor_golden.
        let rust = syms(Lang::Rust, "struct T;\nimpl T { fn go(&self) {} const X: u32 = 1; }\nimpl A for T { fn go(&self) {} const X: u32 = 1; }\n");
        for (_, _, d) in find(&rust, "T", "go").iter().chain(find(&rust, "T", "X").iter()) {
            assert!(d.is_some(), "no Rust impl member may carry a NULL slot: {rust:?}");
        }
        let go = syms(Lang::Go, "package p\nfunc Foo() int { return 1 }\nfunc (a *A) Foo() int { return 1 }\n");
        let foos = find(&go, "", "Foo");
        assert_eq!(foos.len(), 2, "a package func and a method share one FQN: {go:?}");
        for (_, _, d) in &foos {
            assert!(d.is_some(), "no Go func may carry a NULL slot: {go:?}");
        }
        let ruby = syms(Lang::Ruby, "class C\n  def m\n  end\n  def self.m\n  end\nend\n");
        for (_, _, d) in find(&ruby, "C", "m") {
            assert!(d.is_some(), "no Ruby method may carry a NULL slot: {ruby:?}");
        }
        // A free function in a language whose twins are ALL undiscriminated
        // keeps None: there is nothing for a discriminator to say.
        assert_eq!(syms(Lang::Php, "<?php\nfunction f() {}\n")[0].2, None);
    }

    #[test]
    fn disamb_rust_nested_helper_inherits_trait_disamb() {
        let src = "impl Display for T { fn fmt(&self) { fn helper() {} } }";
        let rows = syms(Lang::Rust, src);
        assert_eq!(find(&rows, "T", "fmt")[0].2, Some("Display".to_string()), "{rows:?}");
        assert_eq!(
            find(&rows, "T.fmt", "helper")[0].2,
            Some("Display".to_string()),
            "nested helper collides across twin impls exactly like its parent: {rows:?}"
        );
    }

    #[test]
    fn disamb_rust_mod_scopes_fqn_without_symbol_row() {
        let src = "mod a { fn f() {} }\nmod b { fn f() {} }\nfn f() {}\nmod c;\n";
        let rows = syms(Lang::Rust, src);
        assert_eq!(find(&rows, "a", "f").len(), 1, "{rows:?}");
        assert_eq!(find(&rows, "b", "f").len(), 1, "{rows:?}");
        assert_eq!(find(&rows, "", "f").len(), 1, "{rows:?}");
        assert!(
            !rows.iter().any(|(_, n, _)| n == "a" || n == "b" || n == "c"),
            "mod is scope only, never a symbol row: {rows:?}"
        );
    }

    #[test]
    fn disamb_go_receiver_type() {
        let src = "package m\n\
                   func (a A) String() string { return \"\" }\n\
                   func (b *B) String() string { return \"\" }\n";
        let rows = syms(Lang::Go, src);
        let strings = find(&rows, "", "String");
        assert_eq!(strings.len(), 2, "{rows:?}");
        let disambs: Vec<_> = strings.iter().map(|(_, _, d)| d.clone()).collect();
        assert!(disambs.contains(&Some("A".to_string())), "{rows:?}");
        assert!(disambs.contains(&Some("*B".to_string())), "{rows:?}");
    }

    #[test]
    fn disamb_cpp_overloads_same_fqn_distinct_disamb() {
        let src = "void f(int) {}\nvoid f(double) {}\n";
        let rows = syms(Lang::Cpp, src);
        let fs = find(&rows, "", "f");
        assert_eq!(fs.len(), 2, "{rows:?}");
        let disambs: Vec<_> = fs.iter().map(|(_, _, d)| d.clone()).collect();
        assert!(disambs.contains(&Some("(int)".to_string())), "{rows:?}");
        assert!(disambs.contains(&Some("(double)".to_string())), "{rows:?}");
    }

    #[test]
    fn disamb_cpp_collapses_whitespace_runs() {
        // A rewrapped parameter list lands on the same slot as the one-line
        // spelling: the newline run collapses and the space beside the comma
        // is dropped, so both read `(int a,double b)`.
        let src = "void g(int a,\n       double b) {}\n";
        let rows = syms(Lang::Cpp, src);
        assert_eq!(
            find(&rows, "", "g")[0].2,
            Some("(int a,double b)".to_string()),
            "{rows:?}"
        );
    }

    #[test]
    fn cpp_qualified_out_of_line_gets_class_parent() {
        let src = "void A::run() {}\n";
        let rows = syms(Lang::Cpp, src);
        assert_eq!(find(&rows, "A", "run").len(), 1, "qualifier becomes parent: {rows:?}");
    }

    #[test]
    fn cpp_namespace_and_multi_qualifier_combine() {
        let src = "namespace outer { void ns::A::run() {} }\n";
        let rows = syms(Lang::Cpp, src);
        assert_eq!(find(&rows, "outer.ns.A", "run").len(), 1, "{rows:?}");
    }

    #[test]
    fn cpp_template_qualifier_stays_raw() {
        let src = "template <typename T> void A<T>::run() {}\n";
        let rows = syms(Lang::Cpp, src);
        assert_eq!(find(&rows, "A<T>", "run").len(), 1, "{rows:?}");
    }

    #[test]
    fn cpp_destructor_and_operator_parse_without_panic() {
        let src = "A::~A() {}\nint A::operator+(int o) { return o; }\n";
        let rows = syms(Lang::Cpp, src);
        assert_eq!(find(&rows, "A", "~A").len(), 1, "{rows:?}");
        assert_eq!(find(&rows, "A", "operator+").len(), 1, "{rows:?}");
    }

    #[test]
    fn disamb_java_overload_pair() {
        let src = "class C {\n    void m(int a) {}\n    void m(String a) {}\n}\n";
        let rows = syms(Lang::Java, src);
        let ms = find(&rows, "C", "m");
        assert_eq!(ms.len(), 2, "{rows:?}");
        let disambs: Vec<_> = ms.iter().map(|(_, _, d)| d.clone()).collect();
        assert!(disambs.contains(&Some("(int a)".to_string())), "{rows:?}");
        assert!(disambs.contains(&Some("(String a)".to_string())), "{rows:?}");
    }

    #[test]
    fn disamb_csharp_overload_pair() {
        let src = "class C {\n    void M(int a) {}\n    void M(string a) {}\n}\n";
        let rows = syms(Lang::CSharp, src);
        let ms = find(&rows, "C", "M");
        assert_eq!(ms.len(), 2, "{rows:?}");
        let disambs: Vec<_> = ms.iter().map(|(_, _, d)| d.clone()).collect();
        assert!(disambs.contains(&Some("(int a)".to_string())), "{rows:?}");
        assert!(disambs.contains(&Some("(string a)".to_string())), "{rows:?}");
    }

    #[test]
    fn php_bracketed_namespaces_scope_fqns() {
        let src = "<?php\nnamespace A { class C {} }\nnamespace B { class C {} }\n";
        let rows = syms(Lang::Php, src);
        assert_eq!(find(&rows, "A", "C").len(), 1, "{rows:?}");
        assert_eq!(find(&rows, "B", "C").len(), 1, "{rows:?}");
        assert!(
            !rows.iter().any(|(_, n, _)| n == "A" || n == "B"),
            "namespace is scope only, never a symbol row: {rows:?}"
        );
    }

    #[test]
    fn php_unbracketed_namespace_unchanged() {
        // Pinned to today's output: the unbracketed form stays invisible; a
        // single-namespace file cannot collide with itself.
        let src = "<?php\nnamespace App;\nfunction f() {}\nclass K {}\n";
        let rows = syms(Lang::Php, src);
        assert_eq!(find(&rows, "", "f").len(), 1, "{rows:?}");
        assert_eq!(find(&rows, "", "K").len(), 1, "{rows:?}");
        assert!(rows.iter().all(|(_, _, d)| d.is_none()), "{rows:?}");
    }

    #[test]
    fn php_mixed_namespace_forms_do_not_panic() {
        // Mixing bracketed and unbracketed in one file is illegal PHP; the
        // extractor must still return facts without panicking.
        let src = "<?php\nnamespace A;\nnamespace B { function f() {} }\n";
        let rows = syms(Lang::Php, src);
        assert_eq!(find(&rows, "B", "f").len(), 1, "{rows:?}");
    }

    // --- B1: C++ cv-, ref-, and exception-qualified overloads ---------------
    // The qualifiers are siblings of `parameters` inside function_declarator,
    // so a parameter-list-only discriminator drops these pairs in one slot.

    #[test]
    fn disamb_cpp_cv_qualified_overloads_differ() {
        // A reference return (`T& at(...)`) is dropped by the declarator walk
        // for an unrelated reason (reference_declarator carries no `declarator`
        // field), so the pair is spelled with a value return to isolate B1.
        let src = "class C {\n\
                   \x20 int at(int i) { return v[i]; }\n\
                   \x20 int at(int i) const { return v[i]; }\n\
                   };\n";
        let rows = syms(Lang::Cpp, src);
        let d = disambs(&rows, "C", "at");
        assert_eq!(d.len(), 2, "{rows:?}");
        assert!(d.contains(&Some("(int i)".to_string())), "{rows:?}");
        assert!(d.contains(&Some("(int i) const".to_string())), "{rows:?}");
    }

    #[test]
    fn disamb_cpp_ref_qualified_overloads_differ() {
        // `&` and `&&` are one node kind (ref_qualifier); only the text splits
        // them, so the discriminator must carry the text, not the kind.
        let src = "class C {\n\
                   \x20 T get() & { return v; }\n\
                   \x20 T get() && { return v; }\n\
                   };\n\
                   class D {\n\
                   \x20 T get() { return v; }\n\
                   };\n";
        let rows = syms(Lang::Cpp, src);
        let c = disambs(&rows, "C", "get");
        assert_eq!(c.len(), 2, "{rows:?}");
        assert!(c.contains(&Some("() &".to_string())), "{rows:?}");
        assert!(c.contains(&Some("() &&".to_string())), "{rows:?}");
        assert_eq!(disambs(&rows, "D", "get"), vec![Some("()".to_string())], "{rows:?}");
    }

    #[test]
    fn disamb_cpp_noexcept_reaches_the_discriminator() {
        // Overloading on noexcept alone is illegal C++, so the two forms live
        // in separate classes; the point is that the grammar puts `noexcept`
        // in the same sibling position and the discriminator picks it up.
        let src = "class C { void n(int i) noexcept {} };\n\
                   class D { void n(int i) {} };\n";
        let rows = syms(Lang::Cpp, src);
        assert_eq!(
            disambs(&rows, "C", "n"),
            vec![Some("(int i) noexcept".to_string())],
            "{rows:?}"
        );
        assert_eq!(disambs(&rows, "D", "n"), vec![Some("(int i)".to_string())], "{rows:?}");
    }

    #[test]
    fn disamb_cpp_cv_and_ref_qualifier_combine_in_source_order() {
        let src = "class C { T get() const & { return v; } };\n";
        let rows = syms(Lang::Cpp, src);
        assert_eq!(
            disambs(&rows, "C", "get"),
            vec![Some("() const &".to_string())],
            "{rows:?}"
        );
    }

    #[test]
    fn disamb_cpp_override_is_not_part_of_the_signature() {
        // `override` is a virtual_specifier: it sits in the same sibling slot
        // but is not part of the function type, and it gets added long after a
        // method is written. Letting it in would churn the anchor for free.
        let a = syms(Lang::Cpp, "class C : B { void run() {} };\n");
        let b = syms(Lang::Cpp, "class C : B { void run() override {} };\n");
        assert_eq!(disambs(&a, "C", "run"), disambs(&b, "C", "run"), "{a:?} vs {b:?}");
    }

    // --- B2: C# explicit interface implementations ---------------------------

    #[test]
    fn disamb_csharp_explicit_interface_impls_differ() {
        let src = "class C : IA, IB {\n\
                   \x20   void IA.M() {}\n\
                   \x20   void IB.M() {}\n\
                   \x20   void M() {}\n\
                   }\n";
        let rows = syms(Lang::CSharp, src);
        let d = disambs(&rows, "C", "M");
        assert_eq!(d.len(), 3, "{rows:?}");
        assert!(d.contains(&Some("IA.()".to_string())), "{rows:?}");
        assert!(d.contains(&Some("IB.()".to_string())), "{rows:?}");
        assert!(d.contains(&Some("()".to_string())), "implicit impl keeps the bare list: {rows:?}");
    }

    #[test]
    fn disamb_csharp_explicit_interface_keeps_the_written_path() {
        // Rust trait-name precedent: the reference is spelled as the source
        // writes it, qualification and generics included.
        let src = "class C : N.IA<int> {\n    void N.IA<int>.M(int a) {}\n}\n";
        let rows = syms(Lang::CSharp, src);
        assert_eq!(
            disambs(&rows, "C", "M"),
            vec![Some("N.IA<int>.(int a)".to_string())],
            "{rows:?}"
        );
    }

    // --- B5: kind comes from type-scope depth, not lexical nesting -----------

    #[test]
    fn kind_rust_fn_in_mod_is_function_not_method() {
        let src = "mod tests {\n    fn helper() {}\n}\n\
                   struct S;\n\
                   impl S { fn m(&self) { fn inner() {} } }\n";
        let rows = kinds(Lang::Rust, src);
        assert!(
            rows.contains(&("tests".into(), "helper".into(), "function")),
            "a mod is a namespace, not a type: {rows:?}"
        );
        assert!(rows.contains(&("S".into(), "m".into(), "method")), "{rows:?}");
        assert!(
            rows.contains(&("S.m".into(), "inner".into(), "method")),
            "still inside the impl's type scope: {rows:?}"
        );
    }

    #[test]
    fn kind_cpp_namespace_fn_is_function_out_of_line_is_method() {
        let src = "namespace N { void f() {} }\n\
                   void A::run() {}\n\
                   class C { void m() {} };\n";
        let rows = kinds(Lang::Cpp, src);
        assert!(rows.contains(&("N".into(), "f".into(), "function")), "{rows:?}");
        assert!(
            rows.contains(&("A".into(), "run".into(), "method")),
            "a qualifier segment is a type scope: {rows:?}"
        );
        assert!(rows.contains(&("C".into(), "m".into(), "method")), "{rows:?}");
    }

    #[test]
    fn kind_python_nested_fn_is_function() {
        let src = "def outer():\n\
                   \x20   def inner():\n\
                   \x20       pass\n\
                   \n\
                   class K:\n\
                   \x20   def m(self):\n\
                   \x20       def deep():\n\
                   \x20           pass\n";
        let rows = kinds(Lang::Py, src);
        assert!(rows.contains(&("outer".into(), "inner".into(), "function")), "{rows:?}");
        assert!(rows.contains(&("K".into(), "m".into(), "method")), "{rows:?}");
        assert!(
            rows.contains(&("K.m".into(), "deep".into(), "method")),
            "still inside K's type scope: {rows:?}"
        );
    }

    #[test]
    fn kind_ruby_module_still_holds_methods() {
        // Ruby's `module` emits a class row and its `def`s are instance methods
        // of the mixin, unlike Rust `mod` / PHP `namespace` which emit no row
        // and hold free functions. It stays a type scope.
        let src = "module M\n  def f\n  end\nend\n\ndef top\nend\n";
        let rows = kinds(Lang::Ruby, src);
        assert!(rows.contains(&("M".into(), "f".into(), "method")), "{rows:?}");
        assert!(rows.contains(&(String::new(), "top".into(), "function")), "{rows:?}");
    }

    // --- B6: the discriminator survives formatting-only edits ----------------

    #[test]
    fn disamb_normalizes_spacing_around_punctuation() {
        let a = syms(Lang::Cpp, "void g(int a,double b) {}\n");
        let b = syms(Lang::Cpp, "void g( int a , double b ) {}\n");
        assert_eq!(disambs(&a, "", "g"), disambs(&b, "", "g"), "{a:?} vs {b:?}");
        assert_eq!(disambs(&a, "", "g"), vec![Some("(int a,double b)".to_string())], "{a:?}");
        // `int *p` / `int* p` / `int * p` are one type spelled three ways.
        let p1 = syms(Lang::Cpp, "void h(int *p) {}\n");
        let p2 = syms(Lang::Cpp, "void h(int* p) {}\n");
        let p3 = syms(Lang::Cpp, "void h(int * p) {}\n");
        assert_eq!(disambs(&p1, "", "h"), disambs(&p2, "", "h"), "{p1:?} vs {p2:?}");
        assert_eq!(disambs(&p1, "", "h"), disambs(&p3, "", "h"), "{p1:?} vs {p3:?}");
    }

    #[test]
    fn disamb_ignores_comments_inside_the_parameter_list() {
        let a = syms(Lang::Cpp, "void g(int a, double b) {}\n");
        let b = syms(Lang::Cpp, "void g(int a, /* why */ double b) {}\n");
        assert_eq!(
            disambs(&a, "", "g"),
            disambs(&b, "", "g"),
            "the body hash treats comments as noise; the slot must agree: {a:?} vs {b:?}"
        );
        // tree-sitter-java names comments block_comment / line_comment, not
        // `comment`; the skip list must cover both spellings.
        let ja = syms(Lang::Java, "class C { void m(int a) {} }\n");
        let jb = syms(Lang::Java, "class C { void m(int a /* keep me out */) {} }\n");
        assert_eq!(disambs(&ja, "C", "m"), disambs(&jb, "C", "m"), "{ja:?} vs {jb:?}");
        let ca = syms(Lang::CSharp, "class C { void M(int a) {} }\n");
        let cb = syms(Lang::CSharp, "class C { void M(int a /* keep me out */) {} }\n");
        assert_eq!(disambs(&ca, "C", "M"), disambs(&cb, "C", "M"), "{ca:?} vs {cb:?}");
    }

    #[test]
    fn disamb_still_splits_on_token_changes() {
        // Normalization must not merge signatures that really differ.
        let rows = syms(Lang::Cpp, "void k(int a) {}\nvoid k(int b) {}\nvoid k(long a) {}\n");
        let d = disambs(&rows, "", "k");
        assert_eq!(d.len(), 3, "{rows:?}");
        assert!(d.contains(&Some("(int a)".to_string())), "{rows:?}");
        assert!(d.contains(&Some("(int b)".to_string())), "{rows:?}");
        assert!(d.contains(&Some("(long a)".to_string())), "{rows:?}");
    }

    // --- T2b: every remaining shape that filed two symbols in one slot -------

    #[test]
    fn disamb_ruby_singleton_and_instance_methods_split() {
        // `def m` and `def self.m` are different methods of one class, and so
        // are `def m` and a `def m` reopened under `class << self`. All three
        // spell the FQN `C.m`, so only the discriminator can keep them apart.
        let src = "class C\n\
                   \x20 def m\n\
                   \x20   helper\n\
                   \x20 end\n\
                   \x20 def self.m\n\
                   \x20   helper\n\
                   \x20 end\n\
                   \x20 class << self\n\
                   \x20   def n\n\
                   \x20     helper\n\
                   \x20   end\n\
                   \x20 end\n\
                   \x20 def n\n\
                   \x20   helper\n\
                   \x20 end\n\
                   end\n";
        let rows = syms(Lang::Ruby, src);
        let m = disambs(&rows, "C", "m");
        assert_eq!(m.len(), 2, "{rows:?}");
        assert!(m.contains(&Some("#".to_string())), "the instance method: {rows:?}");
        assert!(m.contains(&Some("self.".to_string())), "{rows:?}");
        let n = disambs(&rows, "C", "n");
        assert_eq!(n.len(), 2, "{rows:?}");
        assert!(n.contains(&Some("#".to_string())), "{rows:?}");
        assert!(n.contains(&Some("<<self.".to_string())), "{rows:?}");
        assert!(
            !rows.iter().any(|(p, _, _)| p.contains("self")),
            "`class << self` is context, never an FQN segment: {rows:?}"
        );
    }

    #[test]
    fn disamb_python_property_and_setter_split() {
        let src = "class C:\n\
                   \x20   @property\n\
                   \x20   def x(self):\n\
                   \x20       return helper()\n\
                   \x20   @x.setter\n\
                   \x20   def x(self, v):\n\
                   \x20       return helper()\n";
        let rows = syms(Lang::Py, src);
        let d = disambs(&rows, "C", "x");
        assert_eq!(d.len(), 2, "{rows:?}");
        assert!(d.contains(&Some("(self)".to_string())), "{rows:?}");
        assert!(d.contains(&Some("(self,v)".to_string())), "{rows:?}");
    }

    #[test]
    fn disamb_js_accessor_pair_splits() {
        let src = "class C {\n\
                   \x20 get x() { return this.v; }\n\
                   \x20 set x(v) { this.v = v; }\n\
                   }\n";
        for lang in [Lang::Js, Lang::Ts] {
            let rows = syms(lang, src);
            let d = disambs(&rows, "C", "x");
            assert_eq!(d.len(), 2, "{lang:?}: {rows:?}");
            assert!(d.contains(&Some("()".to_string())), "{lang:?}: {rows:?}");
            assert!(d.contains(&Some("(v)".to_string())), "{lang:?}: {rows:?}");
        }
    }

    #[test]
    fn ts_namespaces_scope_fqns_without_symbol_rows() {
        let src = "namespace A { export class Opt { run() { go(); } } }\n\
                   namespace B { export class Opt { run() { go(); } } }\n";
        let rows = syms(Lang::Ts, src);
        assert_eq!(find(&rows, "A.Opt", "run").len(), 1, "{rows:?}");
        assert_eq!(find(&rows, "B.Opt", "run").len(), 1, "{rows:?}");
        assert!(
            !rows.iter().any(|(_, n, _)| n == "A" || n == "B"),
            "a TS namespace scopes but is not a symbol (PHP/C++ precedent): {rows:?}"
        );
    }

    #[test]
    fn csharp_block_namespaces_scope_fqns() {
        let src = "namespace A { class Options { void Validate() {} } }\n\
                   namespace B { class Options { void Validate() {} } }\n";
        let rows = syms(Lang::CSharp, src);
        assert_eq!(find(&rows, "A.Options", "Validate").len(), 1, "{rows:?}");
        assert_eq!(find(&rows, "B.Options", "Validate").len(), 1, "{rows:?}");
        assert!(
            !rows.iter().any(|(_, n, _)| n == "A" || n == "B"),
            "a namespace scopes but is not a symbol: {rows:?}"
        );
    }

    #[test]
    fn csharp_file_scoped_namespace_is_left_alone() {
        // A file can hold only one, so it cannot collide with itself and
        // scoping it would respell every modern C# file for nothing.
        let rows = syms(Lang::CSharp, "namespace A.B;\nclass C { void M() {} }\n");
        assert_eq!(find(&rows, "C", "M").len(), 1, "{rows:?}");
    }

    #[test]
    fn disamb_csharp_generic_arity_splits() {
        // Arity is part of a C# signature: both overloads take `()`.
        let src = "class C {\n\
                   \x20 void M<T>() { Log(); }\n\
                   \x20 void M<T, U>() { Log(); }\n\
                   \x20 void M() { Log(); }\n\
                   }\n";
        let rows = syms(Lang::CSharp, src);
        let d = disambs(&rows, "C", "M");
        assert_eq!(d.len(), 3, "{rows:?}");
        assert!(d.contains(&Some("<T>()".to_string())), "{rows:?}");
        assert!(d.contains(&Some("<T,U>()".to_string())), "{rows:?}");
        assert!(d.contains(&Some("()".to_string())), "{rows:?}");
    }

    #[test]
    fn java_enum_and_record_scope_their_methods() {
        let src = "class Outer {\n\
                   \x20 enum A { X; String label() { return fmt(); } }\n\
                   \x20 enum B { Y; String label() { return fmt(); } }\n\
                   \x20 record R(int x) { int doubled() { return x * 2; } }\n\
                   }\n";
        let rows = syms(Lang::Java, src);
        assert_eq!(find(&rows, "Outer.A", "label").len(), 1, "{rows:?}");
        assert_eq!(find(&rows, "Outer.B", "label").len(), 1, "{rows:?}");
        assert_eq!(find(&rows, "Outer.R", "doubled").len(), 1, "{rows:?}");
        assert_eq!(find(&rows, "Outer", "A").len(), 1, "the enum itself is a row: {rows:?}");
        assert_eq!(find(&rows, "Outer", "R").len(), 1, "the record itself is a row: {rows:?}");
    }

    #[test]
    fn php_enums_scope_their_methods() {
        let src = "<?php\n\
                   enum Status { case A; public function label(): string { return fmt(); } }\n\
                   enum Kind { case B; public function label(): string { return fmt(); } }\n";
        let rows = syms(Lang::Php, src);
        assert_eq!(find(&rows, "Status", "label").len(), 1, "{rows:?}");
        assert_eq!(find(&rows, "Kind", "label").len(), 1, "{rows:?}");
        assert_eq!(find(&rows, "", "Status").len(), 1, "the enum itself is a row: {rows:?}");
    }

    #[test]
    fn cpp_reference_returning_definitions_extract() {
        // `reference_declarator` and `parenthesized_declarator` declare NO
        // fields, so a field-only descent dropped every one of these.
        let src = "class Buffer {\n\
                   \x20 char& operator[](int i) { return d_[i]; }\n\
                   \x20 const char& operator[](int i) const { return d_[i]; }\n\
                   \x20 std::string& name() { return name_; }\n\
                   \x20 const std::string& name() const { return name_; }\n\
                   \x20 Buffer& operator=(const Buffer& o) { return *this; }\n\
                   \x20 void clear() { n_ = 0; }\n\
                   };\n";
        let rows = syms(Lang::Cpp, src);
        // The exact pair the C++ overload work was written for, which could not
        // be tested before because neither half extracted at all.
        let n = disambs(&rows, "Buffer", "name");
        assert_eq!(n.len(), 2, "{rows:?}");
        assert!(n.contains(&Some("()".to_string())), "{rows:?}");
        assert!(n.contains(&Some("() const".to_string())), "{rows:?}");
        assert_eq!(disambs(&rows, "Buffer", "operator[]").len(), 2, "{rows:?}");
        assert_eq!(find(&rows, "Buffer", "operator=").len(), 1, "{rows:?}");
        assert_eq!(find(&rows, "Buffer", "clear").len(), 1, "unchanged: {rows:?}");
        // Out-of-line and parenthesized forms too.
        let out = syms(Lang::Cpp, "std::string& Buffer::name() { return name_; }\n");
        assert_eq!(find(&out, "Buffer", "name").len(), 1, "{out:?}");
        let paren = syms(Lang::Cpp, "int (f)(int a) { return a; }\n");
        assert_eq!(find(&paren, "", "f").len(), 1, "{paren:?}");
        // A pointer return already worked and must keep its exact spelling.
        let ptr = syms(Lang::Cpp, "class C { int* p(int i) { return q_; } };\n");
        assert_eq!(disambs(&ptr, "C", "p"), vec![Some("(int i)".to_string())], "{ptr:?}");
    }

    #[test]
    fn disamb_cpp_template_specializations_and_parameters_split() {
        // Explicit specializations name themselves with a `template_function`
        // leaf; without it only the primary template extracted.
        let spec = "template <typename T> void f(T x) {}\n\
                    template <> void f<int>(int x) {}\n\
                    template <> void f<double>(double x) {}\n";
        let rows = syms(Lang::Cpp, spec);
        let d = disambs(&rows, "", "f");
        assert_eq!(d.len(), 3, "every specialization is its own symbol: {rows:?}");
        assert_eq!(
            d.iter().collect::<std::collections::HashSet<_>>().len(),
            3,
            "and its own slot: {rows:?}"
        );
        // Two overloads differing ONLY in their template parameter list.
        let tp = "template <typename T> void make() {}\n\
                  template <typename T, typename U> void make() {}\n";
        let rows = syms(Lang::Cpp, tp);
        let d = disambs(&rows, "", "make");
        assert_eq!(d.len(), 2, "{rows:?}");
        assert!(d.contains(&Some("<typename T>()".to_string())), "{rows:?}");
        assert!(d.contains(&Some("<typename T,typename U>()".to_string())), "{rows:?}");
    }

    // --- T2b: a call outside every symbol keeps the <file> sentinel ----------

    /// (caller FQN path joined with '.', callee) per call edge.
    fn callers(lang: Lang, src: &str) -> Vec<(String, String)> {
        extract(lang, src)
            .unwrap()
            .calls
            .into_iter()
            .map(|c| {
                let mut p = c.parents;
                p.push(c.name);
                (p.join("."), c.callee)
            })
            .collect()
    }

    #[test]
    fn call_in_a_scope_that_is_not_a_symbol_keeps_the_file_sentinel() {
        // A scope-only frame (Rust `mod`, PHP bracketed namespace, C++
        // namespace) is not a symbol, so attributing a call to it produced a
        // `caller_fqn` that no `symbols` row answers to: the lineage join drops
        // it and `map` prints a caller nobody can look up.
        let rust = callers(Lang::Rust, "mod inner {\n    static X: u32 = compute();\n    fn f() { helper(); }\n}\n");
        assert!(
            rust.contains(&(FILE_SCOPE.to_string(), "compute".to_string())),
            "a module-level initializer belongs to no symbol: {rust:?}"
        );
        assert!(rust.contains(&("inner.f".to_string(), "helper".to_string())), "{rust:?}");

        let php = callers(Lang::Php, "<?php\nnamespace A {\n  $x = helper();\n  class Q { function f() { compute(); } }\n}\n");
        assert!(php.contains(&(FILE_SCOPE.to_string(), "helper".to_string())), "{php:?}");
        assert!(php.contains(&("A.Q.f".to_string(), "compute".to_string())), "{php:?}");

        let cpp = callers(Lang::Cpp, "namespace N { int x = f(); void g() { h(); } }\n");
        assert!(cpp.contains(&(FILE_SCOPE.to_string(), "f".to_string())), "{cpp:?}");
        assert!(cpp.contains(&("N.g".to_string(), "h".to_string())), "{cpp:?}");

        // The ordinary cases are untouched.
        let py = callers(Lang::Py, "toplevel()\ndef f():\n    inner()\n");
        assert_eq!(
            py,
            vec![
                (FILE_SCOPE.to_string(), "toplevel".to_string()),
                ("f".to_string(), "inner".to_string()),
            ]
        );
    }

    #[test]
    fn php_bom_prefixed_file_keeps_aligned_offsets() {
        let src = "\u{feff}<?php\nclass Dog extends Animal {}\n";
        let facts = extract(Lang::Php, src).unwrap();
        // Inheritance edge still captured.
        assert!(
            facts.inherits.iter().any(|i| i.name == "Dog"
                && i.parent_name == "Animal"
                && i.rel == "extends"),
            "expected Dog extends Animal edge"
        );
        // Byte range for the Dog class, sliced from the ORIGINAL src, must contain "Dog".
        let dog = facts.symbols.iter().find(|s| s.name == "Dog").expect("Dog symbol");
        let (a, b) = dog.byte_range;
        let slice = &src.as_bytes()[a..b];
        assert!(
            std::str::from_utf8(slice).unwrap().contains("class Dog"),
            "byte range must align with original src, not the prepended buffer; got: {:?}",
            std::str::from_utf8(slice)
        );
    }
}
