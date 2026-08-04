//! Branch-only companion to `if3_dump.rs`: same corpus, same join key, but also
//! prints the `disamb` discriminator. The baseline has no such field, so every
//! non-NULL value printed here is a NULL -> non-NULL flip relative to it.

include!("if3_corpus.inc");

fn main() {
    let mut lines: Vec<String> = Vec::new();
    for (grammar, lang, fixture, src) in corpus() {
        let facts = extract::extract(lang, src)
            .unwrap_or_else(|e| panic!("extract failed for {grammar}/{fixture}: {e}"));
        for s in &facts.symbols {
            lines.push(format!(
                "{}\t{}\t{:04}-{:04}\t{}\tparents={}\tkind={}\tdisamb={}",
                grammar,
                fixture,
                s.start_line,
                s.end_line,
                s.name,
                s.parents.join("."),
                s.kind,
                s.disamb.as_deref().unwrap_or("<NULL>")
            ));
        }
    }
    lines.sort();
    for l in lines {
        println!("{l}");
    }
}
