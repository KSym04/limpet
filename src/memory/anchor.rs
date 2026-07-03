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
            | "true"
            | "false"
            | "none"
            | "null"
    )
}

fn is_comment(kind: &str) -> bool {
    kind.contains("comment")
}

fn emit(node: Node, src: &[u8], out: &mut Vec<u8>) {
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
        // Non-identity leaves (operators, keywords) already contributed
        // their kind above; their text adds nothing structural.
    } else {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            emit(child, src, out);
        }
    }
    out.push(b')');
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
    let own_name_id = node.child_by_field_name("name").map(|n| n.id());
    buf.extend_from_slice(node.kind().as_bytes());
    buf.push(b'(');
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if Some(child.id()) == own_name_id {
            continue;
        }
        emit(child, src, &mut buf);
    }
    buf.push(b')');
    let digest = Sha256::digest(&buf);
    hex32(&digest)
}

fn hex32(digest: &[u8]) -> String {
    // 128 bits is ample for per-repo symbol identity.
    digest[..16].iter().map(|b| format!("{b:02x}")).collect()
}

/// Parse `src` and hash the subtree covering `byte_range` (a symbol's
/// defining node, as recorded by extraction).
pub fn ast_body_hash(lang_id: Lang, src: &str, byte_range: (usize, usize)) -> Result<String> {
    let mut parser = Parser::new();
    parser
        .set_language(&lang::ts_language(lang_id))
        .map_err(|e| anyhow::anyhow!("grammar load failed: {e}"))?;
    let Some(tree) = parser.parse(src, None) else {
        bail!("tree-sitter returned no tree");
    };
    let root = tree.root_node();
    let node = root
        .descendant_for_byte_range(byte_range.0, byte_range.1)
        .unwrap_or(root);
    Ok(ast_body_hash_node(node, src.as_bytes()))
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

/// Resolve every anchor of every non-invalidated entry against the symbols
/// table, applying the spec 4.3 decision table, and update entry statuses.
///
/// Entry status becomes the worst of its anchors
/// (active < stale < invalidated). A `verified` entry that goes stale has
/// its confidence dropped to 0.5 so recall ranks it honestly until
/// re-verified.
pub fn resolve_all(store: &crate::store::Store) -> Result<ResolveReport> {
    let mut report = ResolveReport::default();

    struct Row {
        anchor_id: i64,
        entry_id: String,
        file: String,
        symbol_fqn: Option<String>,
        hash: Option<String>,
    }
    let mut stmt = store.conn.prepare(
        "SELECT a.id, a.entry_id, a.file, a.symbol_fqn, a.ast_body_hash
         FROM anchors a JOIN entries e ON e.id = a.entry_id
         WHERE e.status != 'invalidated' AND e.status != 'superseded'",
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
    let mut worst: HashMap<String, u8> = HashMap::new(); // 0 active, 1 stale, 2 invalidated
    let mut reasons: HashMap<String, &'static str> = HashMap::new();

    for row in rows {
        // File-level anchor (no symbol): fresh while the file exists.
        let Some(ref anchor_fqn) = row.symbol_fqn else {
            let file_exists: bool = store
                .conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM files WHERE path = ?1)",
                    [&row.file],
                    |r| r.get(0),
                )
                .unwrap_or(false);
            let fate = if file_exists { AnchorFate::Fresh } else { AnchorFate::Invalidated };
            tally(&mut report, &fate);
            record_worst(&mut worst, &mut reasons, &row.entry_id, &fate);
            continue;
        };
        let Some(ref anchor_hash) = row.hash else {
            // Symbol anchor without a hash cannot be verified; call it stale.
            let fate = AnchorFate::Stale { reason: "missing_hash" };
            tally(&mut report, &fate);
            record_worst(&mut worst, &mut reasons, &row.entry_id, &fate);
            continue;
        };

        let hash_at_fqn: Option<String> = store
            .conn
            .query_row(
                "SELECT body_hash FROM symbols WHERE fqn = ?1 LIMIT 1",
                [anchor_fqn],
                |r| r.get(0),
            )
            .ok();

        let fate = match hash_at_fqn {
            Some(ref h) if h == anchor_hash => AnchorFate::Fresh,
            Some(_) => AnchorFate::Stale { reason: "body_edited" },
            None => {
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
        record_worst(&mut worst, &mut reasons, &row.entry_id, &fate);
    }

    for (entry_id, sev) in &worst {
        match sev {
            0 => {
                store.conn.execute(
                    "UPDATE entries SET status = 'active', stale_reason = NULL
                     WHERE id = ?1 AND status IN ('active','stale')",
                    [entry_id],
                )?;
            }
            1 => {
                let reason = reasons.get(entry_id).copied().unwrap_or("stale");
                store.conn.execute(
                    "UPDATE entries SET status = 'stale', stale_reason = ?2,
                        confidence = CASE WHEN source = 'verified'
                                          THEN MIN(confidence, 0.5)
                                          ELSE confidence * 0.6 END
                     WHERE id = ?1 AND status IN ('active','stale')",
                    params![entry_id, reason],
                )?;
            }
            _ => {
                store.conn.execute(
                    "UPDATE entries SET status = 'invalidated',
                        stale_reason = 'anchor_deleted'
                     WHERE id = ?1 AND status != 'superseded'",
                    [entry_id],
                )?;
            }
        }
    }

    Ok(report)
}

fn tally(report: &mut ResolveReport, fate: &AnchorFate) {
    match fate {
        AnchorFate::Fresh => report.fresh += 1,
        AnchorFate::Followed { .. } => report.followed += 1,
        AnchorFate::Stale { .. } => report.stale += 1,
        AnchorFate::Invalidated => report.invalidated += 1,
    }
}

fn record_worst(
    worst: &mut std::collections::HashMap<String, u8>,
    reasons: &mut std::collections::HashMap<String, &'static str>,
    entry_id: &str,
    fate: &AnchorFate,
) {
    let sev = match fate {
        AnchorFate::Fresh | AnchorFate::Followed { .. } => 0u8,
        AnchorFate::Stale { .. } => 1,
        AnchorFate::Invalidated => 2,
    };
    if let AnchorFate::Stale { reason } = fate {
        reasons.insert(entry_id.to_string(), reason);
    }
    let cur = worst.entry(entry_id.to_string()).or_insert(0);
    if sev > *cur {
        *cur = sev;
    }
}
