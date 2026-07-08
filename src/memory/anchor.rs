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
    let mut buf = Vec::with_capacity(1024);
    let own_name_id = own_name_node(node).map(|n| n.id());
    buf.extend_from_slice(node.kind().as_bytes());
    buf.push(b'(');
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        emit_excluding(child, src, &mut buf, own_name_id);
    }
    buf.push(b')');
    let digest = Sha256::digest(&buf);
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
    Ok(ast_body_hashes(lang_id, src, &[byte_range])?.remove(0))
}

/// Hash many symbol ranges from ONE parse. Indexing previously reparsed the
/// whole file once per symbol, O(symbols x full parse) on big files.
pub fn ast_body_hashes(
    lang_id: Lang,
    src: &str,
    ranges: &[(usize, usize)],
) -> Result<Vec<String>> {
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
            ast_body_hash_node(node, src.as_bytes())
        })
        .collect())
}

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
        hash: Option<String>,
    }
    // Invalidated entries ARE re-resolved: a transient disappearance (branch
    // switch, git stash, mid-rebase, sweep-budget lag) must not be a death
    // sentence. If the code comes back and the anchors resolve again, the
    // entry recovers (audit 2026-07). Only superseded is final.
    let mut stmt = store.conn.prepare(
        "SELECT a.id, a.entry_id, a.file, a.symbol_fqn, a.ast_body_hash
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
                        .prepare("SELECT path FROM files WHERE hash = ?1 LIMIT 3")?;
                    let homes: Vec<String> = fstmt
                        .query_map([h], |r| r.get(0))?
                        .collect::<rusqlite::Result<_>>()?;
                    match homes.len() {
                        0 => AnchorFate::Invalidated,
                        1 => {
                            store.conn.execute(
                                "UPDATE anchors SET file = ?1 WHERE id = ?2",
                                params![homes[0], row.anchor_id],
                            )?;
                            AnchorFate::Followed {
                                new_fqn: homes[0].clone(),
                                new_file: homes[0].clone(),
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

        // FQNs are not unique (trait impls, overloads): check for ANY row
        // matching (fqn, hash) before declaring the body edited, otherwise
        // which duplicate a LIMIT 1 returns is nondeterministic and anchors
        // flap between fresh and stale across sweeps (audit 2026-07).
        let hash_matches: bool = store.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM symbols WHERE fqn = ?1 AND body_hash = ?2)",
            params![anchor_fqn, anchor_hash],
            |r| r.get(0),
        )?;
        let fqn_exists: bool = store.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM symbols WHERE fqn = ?1)",
            [anchor_fqn],
            |r| r.get(0),
        )?;

        let fate = match (hash_matches, fqn_exists) {
            (true, _) => AnchorFate::Fresh,
            (false, true) => AnchorFate::Stale { reason: "body_edited" },
            (false, false) => {
                // FQN gone: search for the body elsewhere (rename/move).
                let mut fstmt = store.conn.prepare(
                    "SELECT fqn, file FROM symbols WHERE body_hash = ?1 LIMIT 3",
                )?;
                let matches: Vec<(String, String)> = fstmt
                    .query_map([anchor_hash], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<rusqlite::Result<_>>()?;
                match matches.len() {
                    0 => AnchorFate::Invalidated,
                    1 => AnchorFate::Followed {
                        new_fqn: matches[0].0.clone(),
                        new_file: matches[0].1.clone(),
                    },
                    _ => AnchorFate::Stale { reason: "ambiguous_anchor" },
                }
            }
        };

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
