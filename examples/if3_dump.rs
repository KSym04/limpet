//! I-F3 differential harness: dump (grammar, fixture, span, parents, name,
//! kind) for every symbol in the shared fixture corpus, as stable sorted text.
//!
//! This file and `if3_corpus.inc` are BYTE-IDENTICAL in the pre-branch baseline
//! worktree and in the branch worktree, so `diff` of the two outputs is a
//! runtime proof of which FQN spellings and kinds moved. It touches only
//! `Sym.parents`, `Sym.name`, `Sym.kind` and the line span, all of which exist
//! on both sides.
//!
//! The span leads the record so a symbol keeps its identity across the two
//! trees even when its FQN moved: rows join on (grammar, fixture, span, name),
//! none of which any 0.15 change touches.

include!("if3_corpus.inc");

fn main() {
    let mut lines: Vec<String> = Vec::new();
    for (grammar, lang, fixture, src) in corpus() {
        let facts = extract::extract(lang, src)
            .unwrap_or_else(|e| panic!("extract failed for {grammar}/{fixture}: {e}"));
        for s in &facts.symbols {
            lines.push(format!(
                "{}\t{}\t{:04}-{:04}\t{}\tparents={}\tkind={}",
                grammar,
                fixture,
                s.start_line,
                s.end_line,
                s.name,
                s.parents.join("."),
                s.kind
            ));
        }
    }
    lines.sort();
    for l in lines {
        println!("{l}");
    }
}
