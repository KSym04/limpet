//! Read-time name resolution and bounded lineage traversal over the indexed
//! symbol / call / inherit graph. All read-only; never writes or migrates.

use crate::store::Store;
use anyhow::Result;
use std::collections::HashSet;

/// A resolved symbol reference.
#[derive(Debug, Clone)]
pub struct SymRef {
    pub fqn: String,
    pub file: String,
}

/// The outcome of resolving a bare name against the current symbol table.
pub enum Resolution {
    Unresolved,
    Unique(SymRef),
    Ambiguous(Vec<SymRef>),
}

impl Resolution {
    pub fn label(&self) -> &'static str {
        match self {
            Resolution::Unresolved => "unresolved",
            Resolution::Unique(_) => "unique",
            Resolution::Ambiguous(_) => "ambiguous",
        }
    }
}

/// Resolve a bare name against the CURRENT symbols table (I-G1). Never guesses:
/// 0 rows -> Unresolved, 1 -> Unique, >1 -> Ambiguous (all candidates kept).
pub fn resolve_name(store: &Store, bare: &str) -> Resolution {
    let mut refs = Vec::new();
    if let Ok(mut stmt) = store
        .conn
        .prepare("SELECT fqn, file FROM symbols WHERE name = ?1 ORDER BY fqn LIMIT 8")
    {
        if let Ok(rows) = stmt.query_map([bare], |r| {
            Ok(SymRef { fqn: r.get(0)?, file: r.get(1)? })
        }) {
            for row in rows.flatten() {
                refs.push(row);
            }
        }
    }
    match refs.len() {
        0 => Resolution::Unresolved,
        1 => Resolution::Unique(refs.pop().unwrap()),
        _ => Resolution::Ambiguous(refs),
    }
}

#[derive(Debug, Clone)]
pub struct Edge {
    pub fqn: String,
    pub rel: String,
    pub resolved: String,
    pub depth: u32,
}

#[derive(Debug, Default)]
pub struct Lineage {
    pub target: String,
    pub ancestors: Vec<Edge>,
    pub descendants: Vec<Edge>,
    pub callers: Vec<Edge>,
    pub truncated: bool,
    pub unresolved_count: u32,
}

pub struct LineageOpts {
    pub depth: u32,
    pub node_cap: usize,
}

impl Default for LineageOpts {
    fn default() -> Self {
        LineageOpts { depth: 2, node_cap: 40 }
    }
}

/// Turn a bare parent name into an Edge, labeling resolution honestly (I-G3).
/// Bumps `unresolved_count` when the name has no home in this repo.
fn edge_for(store: &Store, bare: &str, rel: &str, depth: u32, unresolved: &mut u32) -> Edge {
    let res = resolve_name(store, bare);
    let fqn = match &res {
        Resolution::Unique(s) => s.fqn.clone(),
        Resolution::Ambiguous(v) => v.first().map(|s| s.fqn.clone()).unwrap_or_else(|| bare.into()),
        Resolution::Unresolved => {
            *unresolved += 1;
            bare.to_string()
        }
    };
    Edge { fqn, rel: rel.to_string(), resolved: res.label().to_string(), depth }
}

fn last_segment(fqn: &str) -> &str {
    fqn.rsplit(['.', ':']).next().unwrap_or(fqn)
}

/// Bounded, read-only lineage around `target_fqn` (I-G2). Depth caps the
/// transitive inheritance walk; calls stay depth-1 (direct callers/callees) to
/// keep the graph from exploding. `node_cap` bounds each bucket; hitting it
/// sets `truncated`.
pub fn lineage(store: &Store, target_fqn: &str, opts: LineageOpts) -> Result<Lineage> {
    let mut out = Lineage { target: target_fqn.to_string(), ..Default::default() };
    let mut truncated = false;

    // ancestors: transitive inheritance up, cycle-safe on child_fqn.
    let mut visited: HashSet<String> = HashSet::new();
    let mut frontier = vec![target_fqn.to_string()];
    for depth in 1..=opts.depth {
        let mut next = Vec::new();
        for child in frontier.drain(..) {
            if !visited.insert(child.clone()) {
                continue;
            }
            let mut stmt = store
                .conn
                .prepare("SELECT parent_name, rel FROM inherits WHERE child_fqn = ?1")?;
            let rows: Vec<(String, String)> = stmt
                .query_map([&child], |r| Ok((r.get(0)?, r.get(1)?)))?
                .filter_map(std::result::Result::ok)
                .collect();
            for (parent_name, rel) in rows {
                if out.ancestors.len() >= opts.node_cap {
                    truncated = true;
                    break;
                }
                let e = edge_for(store, &parent_name, &rel, depth, &mut out.unresolved_count);
                if e.resolved == "unique" {
                    next.push(e.fqn.clone());
                }
                out.ancestors.push(e);
            }
        }
        frontier = next;
        if frontier.is_empty() {
            break;
        }
    }

    // descendants: implementors/subclasses (inherits parent_name resolves to
    // target) + direct callees. Depth-1 to stay lean.
    let target_leaf = last_segment(target_fqn);
    {
        let mut stmt = store
            .conn
            .prepare("SELECT child_fqn, rel FROM inherits WHERE parent_name = ?1")?;
        let rows: Vec<(String, String)> = stmt
            .query_map([target_leaf], |r| Ok((r.get(0)?, r.get(1)?)))?
            .filter_map(std::result::Result::ok)
            .collect();
        for (child_fqn, rel) in rows {
            if out.descendants.len() >= opts.node_cap {
                truncated = true;
                break;
            }
            out.descendants.push(Edge {
                fqn: child_fqn,
                rel,
                resolved: "unique".into(),
                depth: 1,
            });
        }
    }
    {
        let mut stmt = store
            .conn
            .prepare("SELECT callee_name FROM calls WHERE caller_fqn = ?1")?;
        let rows: Vec<String> = stmt
            .query_map([target_fqn], |r| r.get(0))?
            .filter_map(std::result::Result::ok)
            .collect();
        for callee in rows {
            if out.descendants.len() >= opts.node_cap {
                truncated = true;
                break;
            }
            out.descendants
                .push(edge_for(store, &callee, "calls", 1, &mut out.unresolved_count));
        }
    }

    // callers: who calls this symbol (call-up), depth-1.
    {
        let mut stmt = store
            .conn
            .prepare("SELECT DISTINCT caller_fqn FROM calls WHERE callee_name = ?1")?;
        let rows: Vec<String> = stmt
            .query_map([target_leaf], |r| r.get(0))?
            .filter_map(std::result::Result::ok)
            .collect();
        for caller in rows {
            if out.callers.len() >= opts.node_cap {
                truncated = true;
                break;
            }
            out.callers.push(Edge {
                fqn: caller,
                rel: "calls".into(),
                resolved: "unique".into(),
                depth: 1,
            });
        }
    }

    out.truncated = truncated;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_with(files: &[(&str, &str)]) -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        for (name, body) in files {
            let p = dir.path().join(name);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
        let store = Store::open_in_memory().unwrap();
        crate::index::full_index(&store, dir.path()).unwrap();
        (dir, store)
    }

    #[test]
    fn resolves_unique_zero_and_ambiguous() {
        let (_d, s) = store_with(&[
            ("a.py", "class Base:\n    pass\n"),
            ("b.py", "class Base:\n    pass\n"),
            ("c.py", "class Solo:\n    pass\n"),
        ]);
        assert_eq!(resolve_name(&s, "Solo").label(), "unique");
        assert_eq!(resolve_name(&s, "Base").label(), "ambiguous");
        assert_eq!(resolve_name(&s, "Nope").label(), "unresolved");
    }

    #[test]
    fn ancestors_follow_inheritance() {
        let (_d, s) = store_with(&[(
            "a.py",
            "class Animal:\n    pass\nclass Dog(Animal):\n    pass\n",
        )]);
        let fqn: String = s
            .conn
            .query_row("SELECT fqn FROM symbols WHERE name='Dog'", [], |r| r.get(0))
            .unwrap();
        let lin = lineage(&s, &fqn, LineageOpts::default()).unwrap();
        assert!(lin.ancestors.iter().any(|e| e.fqn.ends_with("Animal") && e.rel == "extends"));
    }

    #[test]
    fn callers_are_found() {
        let (_d, s) = store_with(&[(
            "a.py",
            "def target():\n    pass\ndef caller():\n    target()\n",
        )]);
        let fqn: String = s
            .conn
            .query_row("SELECT fqn FROM symbols WHERE name='target'", [], |r| r.get(0))
            .unwrap();
        let lin = lineage(&s, &fqn, LineageOpts::default()).unwrap();
        assert!(lin.callers.iter().any(|e| e.fqn.ends_with("caller")));
    }

    #[test]
    fn traversal_is_cycle_safe_and_capped() {
        // A -> B -> A inheritance cycle must terminate.
        let (_d, s) = store_with(&[(
            "a.py",
            "class A(B):\n    pass\nclass B(A):\n    pass\n",
        )]);
        let fqn: String = s
            .conn
            .query_row("SELECT fqn FROM symbols WHERE name='A'", [], |r| r.get(0))
            .unwrap();
        let lin = lineage(&s, &fqn, LineageOpts { depth: 5, node_cap: 40 }).unwrap();
        assert!(lin.ancestors.len() < 40, "cycle terminates, does not explode");
    }

    #[test]
    fn node_cap_sets_truncated() {
        let mut body = String::from("class Base:\n    pass\n");
        for i in 0..50 {
            body.push_str(&format!("class D{i}(Base):\n    pass\n"));
        }
        let (_d, s) = store_with(&[("a.py", &body)]);
        let fqn: String = s
            .conn
            .query_row("SELECT fqn FROM symbols WHERE name='Base'", [], |r| r.get(0))
            .unwrap();
        let lin = lineage(&s, &fqn, LineageOpts { depth: 2, node_cap: 10 }).unwrap();
        assert!(lin.truncated, "hitting node_cap discloses truncation");
        assert!(lin.descendants.len() <= 10);
    }
}
