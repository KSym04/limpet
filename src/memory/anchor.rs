//! Structural anchors: the load-bearing mechanism of limpet.
//!
//! A memory entry attaches to code through an anchor holding a normalized
//! AST body hash of the anchored symbol. Like its namesake, the anchor
//! survives the code moving: renames and file moves are followed by
//! matching the body hash at its new location; real edits flip the memory
//! to `stale`; deletions invalidate it. Nothing goes stale silently
//! (invariant I3).

use crate::index::lang::{self, Lang};
use anyhow::{bail, Result};
use rusqlite::params;
use sha2::{Digest, Sha256};
use tree_sitter::{Node, Parser};

/// Node kinds whose token text carries identity and must feed the hash.
/// Everything else contributes only its structural kind.
fn is_identity_leaf(kind: &str) -> bool {
    matches!(
        kind,
        "identifier"
            | "name"
            | "property_identifier"
            | "field_identifier"
            | "type_identifier"
            | "shorthand_property_identifier"
            | "variable_name"
            | "string"
            | "string_literal"
            | "string_content"
            | "string_fragment"
            | "encapsed_string"
            | "integer"
            | "integer_literal"
            | "float"
            | "float_literal"
            | "number"
            | "number_literal"
            | "char_literal"
            | "raw_string_literal"
            // Go: double-quoted and backtick string content nodes. These are
            // Go-only at runtime: tree-sitter-rust defines a raw_string_literal_content
            // symbol internally but ALIASES it to "string_content" in its public
            // node-type table, so node.kind() never returns this for Rust and
            // adding it here cannot shift existing Rust body hashes.
            | "interpreted_string_literal_content"
            | "raw_string_literal_content"
            | "true"
            | "false"
            | "none"
            | "null"
            // Bash-specific: unquoted word tokens carry command and variable identity.
            | "word"
    )
}

fn is_comment(kind: &str) -> bool {
    kind.contains("comment")
}


/// The node carrying a definition's own name. Most grammars expose a
/// `name` field; C/C++ `function_definition` buries it in the declarator
/// chain (possibly qualified, `GLGaeaClient::GetSkinChar`), so without the
/// descent the name feeds the hash and C++ rename-following is dead
/// (audit 2026-07).
fn own_name_node(node: Node) -> Option<Node> {
    if let Some(n) = node.child_by_field_name("name") {
        return Some(n);
    }
    let mut cur = node.child_by_field_name("declarator")?;
    loop {
        match cur.kind() {
            "function_declarator" | "pointer_declarator" | "reference_declarator"
            | "parenthesized_declarator" => cur = cur.child_by_field_name("declarator")?,
            "identifier" | "field_identifier" | "destructor_name" | "operator_name" => {
                return Some(cur)
            }
            "qualified_identifier" => match cur.child_by_field_name("name") {
                Some(n) if n.kind() == "qualified_identifier" => cur = n,
                other => return other,
            },
            _ => return None,
        }
    }
}

/// Build the normalization buffer for the subtree rooted at `node`:
/// kind names + parens + identity-leaf text, comments skipped, the
/// symbol's own name node excluded. This buffer IS the hash input; its
/// length is the body's entropy measure (schema v5 `symbols.body_len`).
fn normalization_buffer(node: Node, src: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(1024);
    let own_name_id = own_name_node(node).map(|n| n.id());
    buf.extend_from_slice(node.kind().as_bytes());
    buf.push(b'(');
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        emit_excluding(child, src, &mut buf, own_name_id);
    }
    buf.push(b')');
    buf
}

/// Hash the normalized AST subtree rooted at `node`.
///
/// The symbol's own name node is excluded: a pure rename keeps the body
/// hash identical, which is exactly what makes rename following possible.
/// Identifiers inside the body still count.
///
/// Properties (tested in tests/anchor_golden.rs):
/// reformatting and comments never change the hash; renaming an identifier
/// inside the body or adding a statement always does; identical bodies
/// hash identically across files and names.
pub fn ast_body_hash_node(node: Node, src: &[u8]) -> String {
    let digest = Sha256::digest(normalization_buffer(node, src));
    hex32(&digest)
}

/// Like `emit`, but skips one node id anywhere in the subtree. Needed for
/// C++ where the name node is nested inside the declarator, not a direct
/// child of the definition.
fn emit_excluding(node: Node, src: &[u8], out: &mut Vec<u8>, skip: Option<usize>) {
    if Some(node.id()) == skip {
        return;
    }
    let kind = node.kind();
    if is_comment(kind) {
        return;
    }
    out.extend_from_slice(kind.as_bytes());
    out.push(b'(');
    if node.child_count() == 0 {
        if is_identity_leaf(kind) {
            out.extend_from_slice(&src[node.byte_range()]);
        }
    } else {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            emit_excluding(child, src, out, skip);
        }
    }
    out.push(b')');
}

fn hex32(digest: &[u8]) -> String {
    // 128 bits is ample for per-repo symbol identity.
    digest[..16].iter().map(|b| format!("{b:02x}")).collect()
}

/// Parse `src` and hash the subtree covering `byte_range` (a symbol's
/// defining node, as recorded by extraction).
pub fn ast_body_hash(lang_id: Lang, src: &str, byte_range: (usize, usize)) -> Result<String> {
    Ok(ast_body_hashes(lang_id, src, &[byte_range])?.remove(0).0)
}

/// Hash many symbol ranges from ONE parse. Indexing previously reparsed the
/// whole file once per symbol, O(symbols x full parse) on big files.
/// Returns (hash, normalization-buffer byte length) per range; the length
/// is the entropy signal behind the low-entropy follow guard.
pub fn ast_body_hashes(
    lang_id: Lang,
    src: &str,
    ranges: &[(usize, usize)],
) -> Result<Vec<(String, u32)>> {
    let mut parser = Parser::new();
    parser
        .set_language(&lang::ts_language(lang_id))
        .map_err(|e| anyhow::anyhow!("grammar load failed: {e}"))?;
    let Some(tree) = parser.parse(src, None) else {
        bail!("tree-sitter returned no tree");
    };
    let root = tree.root_node();
    Ok(ranges
        .iter()
        .map(|&(s, e)| {
            let node = root.descendant_for_byte_range(s, e).unwrap_or(root);
            let buf = normalization_buffer(node, src.as_bytes());
            let digest = Sha256::digest(&buf);
            (hex32(&digest), buf.len() as u32)
        })
        .collect())
}

/// Entropy floor for rename/move following. A unique body-hash match whose
/// normalization buffer is shorter than this is a trivial body (empty fn,
/// bare delegation): refusing to follow beats silently re-pointing the
/// anchor at a wrong twin. Calibrated in tests/entropy_calibration.rs from
/// real buffer lengths across all 11 grammars (biased low: a missed follow
/// heals as stale; a wrong follow lies forever). Measured floors: max
/// trivial = 123B (TypeScript), min real = 265B (Ruby); 124 clears the
/// TypeScript trivial fixture with a 141B margin below the Ruby real floor.
pub const MIN_FOLLOW_BODY_BYTES: u32 = 124;

/// Same floor for file-level (content-hash) follows, in file bytes;
/// files.size is already stored so this costs no schema. Calibrated from an
/// empty file (0B) and a lone-import line (33B) against the shortest real
/// fixture source (Ruby, 63B) in tests/entropy_calibration.rs; the
/// provisional 64B guess from the brief failed to clear Ruby's 63B real
/// fixture, so this was lowered per the same max(trivial)+1 rule.
pub const MIN_FOLLOW_FILE_BYTES: i64 = 34;

/// Outcome of resolving one anchor against the current index.
#[derive(Debug, PartialEq)]
pub enum AnchorFate {
    Fresh,
    /// Body found under a different FQN (rename or move); anchor re-pointed.
    Followed { new_fqn: String, new_file: String },
    Stale { reason: &'static str },
    Invalidated,
}

#[derive(Debug, Default, serde::Serialize, PartialEq)]
pub struct ResolveReport {
    pub fresh: usize,
    pub followed: usize,
    pub stale: usize,
    pub invalidated: usize,
}

/// Resolve ONE symbol anchor against the index, slot first.
///
/// The slot is `(symbol_fqn, disamb)`. FQNs are not unique (trait impls,
/// overloads, nested modules), so the schema v7 discriminator is what names
/// WHICH same-FQN symbol a memory was attached to. Reading Fresh off any row
/// sharing the FQN let an identical-bodied twin answer for an edited symbol
/// and hide the edit (invariant I-F1); the ladder asks the anchor's own slot
/// first and only widens when that slot is gone.
///
/// | Step | Condition | Fate |
/// |---|---|---|
/// | 1 | EXISTS(fqn, disamb, hash) | Fresh |
/// | 2 | EXISTS(fqn, disamb) | Stale{body_edited} |
/// | 3a | slot gone, ONE row at (fqn, hash) and it names a slot | slot relabelled, Followed |
/// | 3b | slot gone, no matching row names any slot | Fresh, slot kept |
/// | 3c | slot gone, 2+ matching rows and a slot is named | Stale{ambiguous_anchor} |
/// | 4 | every carrier of my body is in my file, exactly ONE has my last segment AND extends my scope chain | scope respelled, Followed |
/// | 5 | EXISTS(fqn) | Stale{body_edited} |
/// | 6 | body hunted store-wide | Invalidated / Stale / Followed |
///
/// Step 4 (the scope respell) runs BEFORE the fqn-survives verdict: a v7
/// respell can split pre-v7 fqn twins so the OLD spelling stays alive on a
/// different symbol, and checking EXISTS(fqn) first would false-stale the
/// respelled anchor on every sweep forever (whole-branch review 2026-08-04).
///
/// No step depends on row order: 1, 2, 3 and 5 are EXISTS or COUNT
/// aggregates, the respell step acts only when its whole result set is a
/// single row, and step 6's store-wide hunt indexes its result only when
/// exactly one row came back. Which duplicate a LIMIT 1 returned first was
/// nondeterministic and anchors flapped between fresh and stale across
/// sweeps (audit 2026-07).
/// Anchors written before v7 carry no slot and keep the 0.14 fates (I-F2),
/// hardening themselves only when one row proves which slot they meant; the
/// single addition is that a followed legacy anchor takes its new home's slot
/// with it. The entropy floor and the ambiguity refusal guard every edge that
/// MOVES an anchor to a different body (I-F6); step 3 moves nothing, it
/// respells a label over a byte-identical body, so its certainty is bounded by
/// how distinctive that body is: an anchored symbol deleted while an
/// identical-bodied twin survives relabels onto the survivor (design section B,
/// accepted with the entropy floor deliberately not applied).
/// Does `new_fqn` spell the same symbol under an EXTENDED scope chain: every
/// segment of `old_fqn` present in `new_fqn`, in order? Sanctioned respells
/// (S1-S9) only insert segments, so a genuine respell always passes, while a
/// sibling-scoped twin (`shapes.alpha.reset` vs `shapes.beta.reset`) fails.
fn is_scope_extension(old_fqn: &str, new_fqn: &str) -> bool {
    let mut new_segs = new_fqn.split('.');
    'old: for seg in old_fqn.split('.') {
        for cand in new_segs.by_ref() {
            if cand == seg {
                continue 'old;
            }
        }
        return false;
    }
    true
}

fn resolve_symbol_anchor(
    store: &crate::store::Store,
    anchor_id: i64,
    anchor_fqn: &str,
    anchor_disamb: Option<&str>,
    anchor_file: &str,
    anchor_hash: &str,
) -> Result<AnchorFate> {
    match anchor_disamb {
        Some(slot) => {
            // 1. My own slot still holds my body: nothing moved, nothing changed.
            let slot_fresh: bool = store.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM symbols
                 WHERE fqn = ?1 AND disamb = ?2 AND body_hash = ?3)",
                params![anchor_fqn, slot, anchor_hash],
                |r| r.get(0),
            )?;
            if slot_fresh {
                return Ok(AnchorFate::Fresh);
            }
            // 2. My slot is still there holding a DIFFERENT body: the edit is
            //    mine. THE TWIN-MASKING FIX: a same-FQN twin still carrying
            //    the old body no longer answers for my slot.
            let slot_exists: bool = store.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM symbols WHERE fqn = ?1 AND disamb = ?2)",
                params![anchor_fqn, slot],
                |r| r.get(0),
            )?;
            if slot_exists {
                return Ok(AnchorFate::Stale { reason: "body_edited" });
            }
            // 3. Slot gone but my body is still under my FQN: the
            //    discriminator itself was respelled (a trait rename leaves the
            //    method body untouched). Aggregates, not an ordered read: with
            //    exactly one matching row they ARE that row's values.
            let (matches, new_slot, new_file): (i64, Option<String>, Option<String>) =
                store.conn.query_row(
                    "SELECT COUNT(*), MIN(disamb), MIN(file) FROM symbols
                     WHERE fqn = ?1 AND body_hash = ?2",
                    params![anchor_fqn, anchor_hash],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )?;
            match (matches, new_slot) {
                // One row, same FQN, byte-identical body, new discriminator:
                // the label moved and the body did not, so relabel the slot in
                // place. No entropy floor here, nothing moved. The identity is
                // only as certain as the body is distinctive: delete the
                // anchored symbol while an identical-bodied twin survives and
                // this relabels onto the survivor. The design takes that trade
                // (section B) rather than strand the memory.
                (1, Some(d)) => {
                    store.conn.execute(
                        "UPDATE anchors SET disamb = ?1 WHERE id = ?2",
                        params![d, anchor_id],
                    )?;
                    return Ok(AnchorFate::Followed {
                        new_fqn: anchor_fqn.to_string(),
                        // MIN over the single matched row; symbols.file is NOT
                        // NULL, so the fallback is unreachable defence.
                        new_file: new_file.unwrap_or_else(|| anchor_file.to_string()),
                    });
                }
                // The refill window, NOT a rename: v7 zeroes mtimes and symbol
                // rows carry NULL disamb until the sweep re-parses their file
                // (a 0.14 binary re-indexing a v7 store writes NULL too). Rows
                // naming no discriminator at all are not evidence that mine was
                // respelled, nor, when several of them match, evidence of
                // ambiguity: they are legacy-shaped rows and get the legacy
                // answer. My body is under my own FQN, so this reads fresh and
                // the anchor keeps the slot it recorded. Residual, accepted:
                // when the discriminator was genuinely dropped (a trait impl
                // rewritten as an inherent one) the anchor holds a label no row
                // answers to, and the day another symbol claims that label with
                // a different body, step 2 stales this memory once. Adopting
                // the NULL instead would strip real slots inside the refill
                // window, and a slot lost under twins never comes back: a
                // recoverable stale beats a silent, permanent loss of twin
                // protection.
                (n, None) if n > 0 => return Ok(AnchorFate::Fresh),
                // Several rows carry this body under this FQN and at least one
                // of them names a slot: which one is mine is unknowable, and
                // guessing is how anchors start lying.
                (n, _) if n > 1 => return Ok(AnchorFate::Stale { reason: "ambiguous_anchor" }),
                // No row under my FQN carries my body at all: the respell
                // step and the fqn-survives verdict below decide.
                _ => {}
            }
        }
        None => {
            // Legacy anchor (written before v7, or a symbol class that has no
            // discriminator at all): the 0.14 ladder byte for byte. Any row at
            // (fqn, hash) is Fresh, twin masking included, because an anchor
            // that never recorded a slot offers nothing to disambiguate with.
            let (matches, slot): (i64, Option<String>) = store.conn.query_row(
                "SELECT COUNT(*), MIN(disamb) FROM symbols WHERE fqn = ?1 AND body_hash = ?2",
                params![anchor_fqn, anchor_hash],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            if matches > 0 {
                // Opportunistic backfill, gated on a TWIN-FREE fqn. A
                // hash-unique row under a shared fqn proves nothing: if the
                // twins' bodies were ever identical and the anchored symbol
                // was just edited, the one surviving hash match is the TWIN
                // wearing the anchor's old body, and adopting it hardens the
                // anchor to the wrong symbol, permanently (whole-branch
                // review 2026-08-04). Only a sole row under the fqn can be
                // the anchor's own symbol. Everything else stays legacy:
                // 0.14 fates exactly, no guess (I-F2).
                if let (1, Some(d)) = (matches, slot) {
                    let fqn_rows: i64 = store.conn.query_row(
                        "SELECT COUNT(*) FROM symbols WHERE fqn = ?1",
                        [anchor_fqn],
                        |r| r.get(0),
                    )?;
                    if fqn_rows == 1 {
                        store.conn.execute(
                            "UPDATE anchors SET disamb = ?1 WHERE id = ?2",
                            params![d, anchor_id],
                        )?;
                    }
                }
                return Ok(AnchorFate::Fresh);
            }
        }
    }

    // 4. SCOPE RESPELL. My body is still in my file under a name whose last
    //    segment is mine and whose scope chain EXTENDS my old spelling: an
    //    ENCLOSING SCOPE was respelled, not the symbol.
    //
    //    Upgrades do this on purpose. Every scope fix in 0.15 (Rust `mod`, PHP
    //    bracketed namespace, C# block namespace, Java enum/record, PHP enum,
    //    TS namespace/module, C++ out-of-line qualifier segments) inserts a
    //    segment into FQNs that ALREADY have anchors pointed at them, and the
    //    v7 migration re-parses every file at once, so the whole repo respells
    //    in one sweep. Without this step those anchors fall through to the
    //    store-wide hunt, where a body under `MIN_FOLLOW_BODY_BYTES` becomes
    //    Stale{low_entropy} and a body with any duplicate becomes
    //    Stale{ambiguous_anchor}: measured at 16.3% of respelled symbols, all
    //    of them permanent, because a refused hunt never repairs the anchor
    //    and every later sweep repeats the verdict. False-staling a memory
    //    whose code never changed is the exact failure limpet exists to
    //    prevent.
    //
    //    Three guards make the shortcut evidence, not a guess (whole-branch
    //    review 2026-08-04):
    //    - EVERY carrier of my body in the store is in my file. A carrier in
    //      another file means the move interpretation competes with the
    //      respell interpretation, and choosing between them is the hunt's
    //      job, which refuses duplicates honestly.
    //    - The candidate's scope chain extends mine: every segment of my old
    //      FQN appears in the new one, in order. Sanctioned respells only
    //      INSERT segments, so a genuine respell always passes, while a
    //      same-named twin under a SIBLING scope (mod alpha deleted, mod beta
    //      survives) fails the extension check and is refused; 0.14's verdict
    //      for it survives via the hunt.
    //    - Exactly one candidate. Two candidate homes fall through to the
    //      hunt rather than be picked between.
    //    The entropy floor is deliberately not applied: with the guards above
    //    the follow lands on a byte-identical body in the same file under an
    //    extended spelling of the same name, and refusing short bodies would
    //    refuse precisely the ones this step is here to rescue. Residual,
    //    accepted on the same terms as step 3: deleting a symbol while an
    //    identically-bodied, same-named twin under an EXTENDED scope of it
    //    pre-exists in the same file follows that twin.
    //
    //    This runs BEFORE the fqn-survives verdict below because a v7 respell
    //    can split pre-v7 fqn twins: the old spelling legitimately survives on
    //    a DIFFERENT symbol (a C-export wrapper beside the respelled method),
    //    and stale-on-EXISTS(fqn) would shadow the rescue forever.
    //
    //    Filtering the last segment in Rust, not in SQL: an FQN segment
    //    routinely contains `_`, which SQLite LIKE treats as a
    //    single-character wildcard, so a LIKE pattern would match unrelated
    //    names.
    let last_seg = anchor_fqn.rsplit('.').next().unwrap_or(anchor_fqn);
    let mut rstmt = store.conn.prepare(
        "SELECT fqn, file, disamb FROM symbols WHERE body_hash = ?1 LIMIT 4",
    )?;
    let carriers: Vec<(String, String, Option<String>)> = rstmt
        .query_map(params![anchor_hash], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<Vec<(String, String, Option<String>)>>>()?;
    let all_in_my_file = !carriers.is_empty()
        && carriers.len() <= 3
        && carriers.iter().all(|(_, f, _)| f == anchor_file);
    if all_in_my_file {
        let respelled: Vec<&(String, String, Option<String>)> = carriers
            .iter()
            .filter(|(f, _, _)| {
                f.rsplit('.').next() == Some(last_seg) && is_scope_extension(anchor_fqn, f)
            })
            .collect();
        if let [(new_fqn, _, new_slot)] = respelled.as_slice() {
            // Take the new home's slot with it, exactly as the hunt does: an
            // anchor that lands on a shared FQN carrying no discriminator is
            // maskable by the twins living there.
            store.conn.execute(
                "UPDATE anchors SET disamb = ?1 WHERE id = ?2",
                params![new_slot, anchor_id],
            )?;
            return Ok(AnchorFate::Followed {
                new_fqn: new_fqn.clone(),
                new_file: anchor_file.to_string(),
            });
        }
    }

    // 5. The FQN is still indexed but nothing under it carries my body:
    //    conservative stale rather than a store-wide hunt, and it heals the
    //    moment the body comes back (a revert, a stash pop).
    let fqn_exists: bool = store.conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM symbols WHERE fqn = ?1)",
        [anchor_fqn],
        |r| r.get(0),
    )?;
    if fqn_exists {
        return Ok(AnchorFate::Stale { reason: "body_edited" });
    }

    // 6. FQN gone: search for the body elsewhere (rename/move).
    let mut fstmt = store.conn.prepare(
        "SELECT fqn, file, body_len, disamb FROM symbols WHERE body_hash = ?1 LIMIT 3",
    )?;
    let matches: Vec<(String, String, Option<i64>, Option<String>)> = fstmt
        .query_map([anchor_hash], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(match matches.len() {
        0 => AnchorFate::Invalidated,
        1 => match matches[0].2 {
            // A trivial body is not evidence: an empty fn or bare delegation
            // hashes identically to every twin, and a wrong follow lies
            // forever. Stale heals if the original returns; NULL (pre-v5)
            // keeps legacy grace.
            Some(len) if len < i64::from(MIN_FOLLOW_BODY_BYTES) => {
                AnchorFate::Stale { reason: "low_entropy" }
            }
            _ => {
                // The body has ONE proven home: take that home's slot with it,
                // or the anchor would land on a shared FQN carrying no
                // discriminator and be maskable by the twins living there.
                store.conn.execute(
                    "UPDATE anchors SET disamb = ?1 WHERE id = ?2",
                    params![matches[0].3, anchor_id],
                )?;
                AnchorFate::Followed {
                    new_fqn: matches[0].0.clone(),
                    new_file: matches[0].1.clone(),
                }
            }
        },
        _ => AnchorFate::Stale { reason: "ambiguous_anchor" },
    })
}

/// Resolve every anchor of every non-invalidated entry against the index,
/// applying the spec 4.3 decision table, and update entry statuses.
///
/// Aggregation is per-anchor, not worst-anchor-wins: an entry is
/// invalidated only when EVERY anchor is gone. Losing some anchors while
/// others still resolve marks it `stale:anchor_lost`, because a memory
/// that is still 80% attached to live code is degraded, not dead
/// (invariant I-C). A `verified` entry that goes stale has its confidence
/// dropped to 0.5 so recall ranks it honestly until re-verified.
pub fn resolve_all(store: &crate::store::Store) -> Result<ResolveReport> {
    let mut report = ResolveReport::default();

    struct Row {
        anchor_id: i64,
        entry_id: String,
        file: String,
        symbol_fqn: Option<String>,
        disamb: Option<String>,
        hash: Option<String>,
    }
    // Invalidated entries ARE re-resolved: a transient disappearance (branch
    // switch, git stash, mid-rebase, sweep-budget lag) must not be a death
    // sentence. If the code comes back and the anchors resolve again, the
    // entry recovers (audit 2026-07). Only superseded is final.
    let mut stmt = store.conn.prepare(
        "SELECT a.id, a.entry_id, a.file, a.symbol_fqn, a.ast_body_hash, a.disamb
         FROM anchors a JOIN entries e ON e.id = a.entry_id
         WHERE e.status != 'superseded'",
    )?;
    let rows: Vec<Row> = stmt
        .query_map([], |r| {
            Ok(Row {
                anchor_id: r.get(0)?,
                entry_id: r.get(1)?,
                file: r.get(2)?,
                symbol_fqn: r.get(3)?,
                hash: r.get(4)?,
                disamb: r.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;

    use std::collections::HashMap;
    let mut tallies: HashMap<String, EntryTally> = HashMap::new();

    for row in rows {
        // File-level anchor (no symbol): compare the stored content hash
        // against the file's current hash so edits surface as stale.
        let Some(ref anchor_fqn) = row.symbol_fqn else {
            let current: Option<String> = store
                .conn
                .query_row("SELECT hash FROM files WHERE path = ?1", [&row.file], |r| {
                    r.get(0)
                })
                .ok();
            let fate = match (current, &row.hash) {
                (None, Some(h)) => {
                    // File row gone, but the content may have MOVED: follow
                    // by hash exactly like symbol anchors follow bodies.
                    let mut fstmt = store
                        .conn
                        .prepare("SELECT path, size FROM files WHERE hash = ?1 LIMIT 3")?;
                    let homes: Vec<(String, i64)> = fstmt
                        .query_map([h], |r| Ok((r.get(0)?, r.get(1)?)))?
                        .collect::<rusqlite::Result<_>>()?;
                    match homes.len() {
                        0 => AnchorFate::Invalidated,
                        // A near-empty file's content hash matches every
                        // stub like it; refusing beats guessing (heals if
                        // the original path returns).
                        1 if homes[0].1 < MIN_FOLLOW_FILE_BYTES => {
                            AnchorFate::Stale { reason: "low_entropy" }
                        }
                        1 => {
                            store.conn.execute(
                                "UPDATE anchors SET file = ?1 WHERE id = ?2",
                                params![homes[0].0, row.anchor_id],
                            )?;
                            AnchorFate::Followed {
                                new_fqn: homes[0].0.clone(),
                                new_file: homes[0].0.clone(),
                            }
                        }
                        _ => AnchorFate::Stale { reason: "ambiguous_anchor" },
                    }
                }
                (None, None) => AnchorFate::Invalidated,
                (Some(cur), None) => {
                    // Legacy anchor written before file hashes were stored:
                    // adopt the current content as its baseline.
                    store.conn.execute(
                        "UPDATE anchors SET ast_body_hash = ?1 WHERE id = ?2",
                        params![cur, row.anchor_id],
                    )?;
                    AnchorFate::Fresh
                }
                (Some(ref cur), Some(h)) if cur == h => AnchorFate::Fresh,
                (Some(_), Some(_)) => AnchorFate::Stale { reason: "file_edited" },
            };
            tally(&mut report, &fate);
            record(&mut tallies, &row.entry_id, &fate);
            continue;
        };
        let Some(ref anchor_hash) = row.hash else {
            // Symbol anchor without a hash cannot be verified; call it stale.
            let fate = AnchorFate::Stale { reason: "missing_hash" };
            tally(&mut report, &fate);
            record(&mut tallies, &row.entry_id, &fate);
            continue;
        };

        // Symbol anchor: walk the slot-first ladder. FQNs are not unique
        // (trait impls, overloads), so no step may read a row whose position
        // SQLite chooses: the aggregate steps use EXISTS or COUNT, and both
        // the respell step and the store-wide hunt act on their result only
        // when exactly one row came back. Which duplicate a LIMIT 1 returned
        // was nondeterministic and anchors flapped between fresh and stale
        // across sweeps (audit 2026-07). The slot is what tells the twins
        // apart.
        let fate = resolve_symbol_anchor(
            store,
            row.anchor_id,
            anchor_fqn,
            row.disamb.as_deref(),
            &row.file,
            anchor_hash,
        )?;

        if let AnchorFate::Followed { ref new_fqn, ref new_file } = fate {
            store.conn.execute(
                "UPDATE anchors SET symbol_fqn = ?1, file = ?2 WHERE id = ?3",
                params![new_fqn, new_file, row.anchor_id],
            )?;
        }
        tally(&mut report, &fate);
        record(&mut tallies, &row.entry_id, &fate);
    }

    for (entry_id, t) in &tallies {
        if t.invalidated == t.total {
            // Every anchor is gone: the memory has nothing left to describe.
            store.conn.execute(
                "UPDATE entries SET status = 'invalidated',
                    stale_reason = 'anchor_deleted'
                 WHERE id = ?1 AND status != 'superseded'",
                [entry_id],
            )?;
        } else if t.invalidated > 0 || t.stale > 0 {
            let reason = if t.invalidated > 0 {
                "anchor_lost"
            } else {
                t.stale_reason.unwrap_or("stale")
            };
            // Confidence penalty applies ONCE, on the active->stale
            // transition. resolve_all runs on every tool call; re-applying
            // *0.6 each time collapsed stale memories to the floor within a
            // handful of calls (audit 2026-07). The CASE reads the pre-
            // update status, so an already-stale entry keeps its confidence.
            // ROUND(..., 6): quantize the penalized confidence so f64 chains
            // stay clean in the store and export roundtrips bit-exactly.
            store.conn.execute(
                "UPDATE entries SET
                    confidence = ROUND(CASE
                        WHEN status = 'active' AND source = 'verified'
                            THEN MIN(confidence, 0.5)
                        WHEN status = 'active'
                            THEN confidence * 0.6
                        ELSE confidence END, 6),
                    status = 'stale', stale_reason = ?2
                 WHERE id = ?1 AND status != 'superseded'",
                params![entry_id, reason],
            )?;
        } else {
            store.conn.execute(
                "UPDATE entries SET status = 'active', stale_reason = NULL
                 WHERE id = ?1 AND status != 'superseded'",
                [entry_id],
            )?;
        }
    }

    Ok(report)
}

/// Per-entry anchor outcome counts for status aggregation.
#[derive(Default)]
struct EntryTally {
    total: usize,
    invalidated: usize,
    stale: usize,
    stale_reason: Option<&'static str>,
}

fn tally(report: &mut ResolveReport, fate: &AnchorFate) {
    match fate {
        AnchorFate::Fresh => report.fresh += 1,
        AnchorFate::Followed { .. } => report.followed += 1,
        AnchorFate::Stale { .. } => report.stale += 1,
        AnchorFate::Invalidated => report.invalidated += 1,
    }
}

fn record(
    tallies: &mut std::collections::HashMap<String, EntryTally>,
    entry_id: &str,
    fate: &AnchorFate,
) {
    let t = tallies.entry(entry_id.to_string()).or_default();
    t.total += 1;
    match fate {
        AnchorFate::Fresh | AnchorFate::Followed { .. } => {}
        AnchorFate::Stale { reason } => {
            t.stale += 1;
            t.stale_reason.get_or_insert(reason);
        }
        AnchorFate::Invalidated => t.invalidated += 1,
    }
}
