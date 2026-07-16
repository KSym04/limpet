//! `limpet seed <file>`: ingest a plain markdown notes file (MEMORY.md,
//! CLAUDE.md, a rules doc) into anchored memories.
//!
//! This is the adoption bridge. A dev with a working MEMORY.md does not want
//! to abandon it; seeding lets them keep it and get the same knowledge back
//! as staleness-aware memory. The seed is:
//!
//!   - best-effort anchored: a chunk that names a file under the repo is
//!     anchored to that file (so it goes stale when the file is edited);
//!     a chunk that names nothing resolvable is stored unanchored.
//!   - idempotent: each chunk carries a deterministic `origin`, so re-running
//!     on the same file changes nothing. A duplicate origin is counted as
//!     `unchanged`, never a hard error.
//!   - non-destructive: it only ever adds; it never edits or removes.
//!
//! Usage:
//!   limpet seed <file> [--anchor auto|file|none] [--kind <kind>] [--force] [--root <path>]
//!
//! `<file>` (when relative), the anchors, and the store all resolve under
//! `--root` (default: the current directory), so the notes file is found in
//! the same place its anchors point. An absolute `<file>` is used as-is.
//! `--kind` accepts any memory kind (fact, decision, episode, insight,
//! intent); default fact.
//!
//! Anchor modes:
//!   auto (default)  anchor to a referenced file if one resolves, else store unanchored
//!   file            anchor to a referenced file, or skip the chunk if none resolves
//!   none            store every chunk unanchored
//!
//! Re-seeding an EDITED notes file: a reworded chunk is a new origin but often a
//! near-duplicate of the memory its previous wording stored, so the write-path
//! dedup refuses it. Those are counted and reported separately with the fix:
//! re-run with `--force` to store the new wording anyway (the old wording stays;
//! supersede it from the agent side if it should die).

use anyhow::{bail, Context, Result};
use limpet::store::Store;
use limpet::tools;
use serde_json::{json, Value};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq)]
enum AnchorMode {
    Auto,
    File,
    None,
}

#[derive(Default)]
struct SeedReport {
    seeded: usize,
    anchored: usize,
    unchanged: usize,
    skipped_unanchorable: usize,
    /// Refused by write-path dedup: near-identical to an existing memory on the
    /// same anchor (typically a reworded note whose old wording is stored).
    refused_near_duplicate: usize,
    rejected: usize,
}

/// The one usage line, shared by every argument error so the three doc
/// surfaces (this string, main.rs HELP, the main.rs doc header) stay in step.
const USAGE: &str =
    "usage: limpet seed <file> [--anchor auto|file|none] [--kind <kind>] [--force] [--root <path>]";

/// Entry point wired into `main.rs`. Parses flags strictly (this command
/// WRITES memory, so a typo must fail loudly, never silently change what gets
/// stored), reads the file, and drives the real `remember` handler through
/// `tools::dispatch`, one chunk at a time.
pub fn run(args: &[String]) -> Result<()> {
    // args[0] is "seed". One strict pass: value flags consume the next token
    // (so a flag value is never mistaken for the file path), unknown flags
    // are refused, and exactly one positional argument is accepted.
    let mut file: Option<String> = None;
    let mut anchor_raw: Option<String> = None;
    let mut kind_raw: Option<String> = None;
    let mut root_raw: Option<String> = None;
    let mut force = false;
    let mut it = args.iter().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--anchor" | "--kind" | "--root" => {
                let v = it
                    .next()
                    .with_context(|| format!("{a} needs a value. {USAGE}"))?
                    .clone();
                match a.as_str() {
                    "--anchor" => anchor_raw = Some(v),
                    "--kind" => kind_raw = Some(v),
                    _ => root_raw = Some(v),
                }
            }
            "--force" => force = true,
            other if other.starts_with("--") => {
                // The '=' spelling would otherwise be silently ignored and
                // every chunk stored with default settings.
                if let Some((flag, _)) = other.split_once('=') {
                    bail!("{flag} does not take '=': pass the value as the next argument. {USAGE}");
                }
                bail!("unknown flag '{other}'. {USAGE}");
            }
            positional => {
                if let Some(first) = &file {
                    bail!("unexpected extra argument '{positional}' (notes file is already '{first}'). {USAGE}");
                }
                file = Some(positional.to_string());
            }
        }
    }
    let file = file.with_context(|| USAGE.to_string())?;

    let anchor_mode = match anchor_raw.as_deref() {
        None | Some("auto") => AnchorMode::Auto,
        Some("file") => AnchorMode::File,
        Some("none") => AnchorMode::None,
        Some(other) => bail!("unknown --anchor '{other}' (expected auto|file|none)"),
    };
    let kind = kind_raw.unwrap_or_else(|| "fact".to_string());
    // Validate up front: an invalid kind would otherwise reject EVERY chunk in
    // the per-chunk error swallow below and exit 0 with only "N rejected" as
    // the clue, which reads like bad notes rather than a bad flag.
    if !limpet::memory::KINDS.contains(&kind.as_str()) {
        bail!(
            "unknown --kind '{kind}' (expected one of {:?})",
            limpet::memory::KINDS
        );
    }

    let root = root_from(root_raw)?;
    // The notes file, anchors, and store all resolve under one base: --root
    // (default: the current directory). So `cd repo && limpet seed NOTES.md`
    // and `limpet seed NOTES.md --root repo` both read repo/NOTES.md, with no
    // surprising split between where the file is found and where anchors point.
    // An absolute path is honored as-is.
    let file_path = resolve_notes_path(&root, &file);
    let text = std::fs::read_to_string(&file_path)
        .with_context(|| format!("reading notes file {}", file_path.display()))?;
    let file_label = file_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "notes".to_string());

    let mut store = Store::open(&Store::default_db_path(&root)).context("opening store")?;
    store.version_guard()?;

    // Index once up front so anchor resolution has a current symbol/file table
    // to match against; the per-chunk `remember` calls then only re-index the
    // one file they anchor to.
    let _ = tools::dispatch(&mut store, &root, "admin", &json!({ "op": "index" }))
        .context("initial index before seeding")?;

    let chunks = chunk_markdown(&text);
    if chunks.is_empty() {
        println!("nothing to seed: {} produced no memory-worthy chunks", file_label);
        return Ok(());
    }

    let mut report = SeedReport::default();
    for chunk in &chunks {
        // Deterministic dedup key: same file + same chunk text -> same origin,
        // so a second run is a no-op. DefaultHasher keeps this dependency-free;
        // a 64-bit key is ample for a notes file's worth of chunks.
        let origin = format!("seed:{}:{:016x}", file_label, hash64(chunk));

        let anchors = resolve_chunk_anchor(&root, chunk, anchor_mode);
        if anchor_mode == AnchorMode::File && anchors.is_empty() {
            report.skipped_unanchorable += 1;
            continue;
        }

        // Seeded notes are `mined`, not `explicit`: they are imported prose, not
        // a re-runnable claim, so the truth-layer ranking (v0.14.0) correctly
        // treats them as lower-trust than verified facts until re-confirmed.
        // `mined` also caps confidence at 0.5.
        let mut memory = json!({
            "kind": kind,
            "body": chunk,
            "source": "mined",
            "origin": origin,
            "force": force,
        });
        if !anchors.is_empty() {
            memory["anchors"] = Value::Array(anchors.clone());
        }

        match tools::dispatch(&mut store, &root, "remember", &memory) {
            Ok(_) => {
                report.seeded += 1;
                if !anchors.is_empty() {
                    report.anchored += 1;
                }
            }
            Err(e) => {
                let msg = format!("{e:#}");
                // A duplicate origin is the idempotency signal, not a failure:
                // this exact chunk is already stored.
                if msg.contains("duplicate origin") {
                    report.unchanged += 1;
                } else if msg.contains("near-duplicate") {
                    // Write-path dedup refused it: usually a REWORDED note whose
                    // earlier wording is already stored on the same anchor. Not
                    // a silent bucket: counted apart and the fix is printed in
                    // the report line (--force stores the new wording).
                    report.refused_near_duplicate += 1;
                } else {
                    // Secrets, empty bodies, oversize bodies, unknown kind: the
                    // remember handler rejected it on purpose. Count and move
                    // on rather than aborting the whole seed.
                    report.rejected += 1;
                }
            }
        }
    }

    println!(
        "seeded {} ({} anchored), {} unchanged, {} skipped (no anchor), {} rejected",
        report.seeded, report.anchored, report.unchanged, report.skipped_unanchorable, report.rejected
    );
    if report.refused_near_duplicate > 0 {
        println!(
            "{} refused as near-duplicates of memories already stored on the same anchor \
             (a reworded note keeps its old wording until you act): re-run with --force \
             to store the new wording as well",
            report.refused_near_duplicate
        );
    }
    Ok(())
}

/// Split a markdown notes file into individual memory-worthy chunks.
///
/// Rules, in order of preference:
///   - fenced code blocks (``` or ~~~) are skipped whole: fence markers must
///     never land inside a stored body, `#` lines inside a fence are code, not
///     headings, and list-shaped code lines are not facts. A command worth
///     remembering is described by the prose around it;
///   - a bullet line (`- `, `* `, `+ `, or `1. `) is one chunk on its own,
///     because bullets are usually independent facts;
///   - otherwise a blank-line-delimited paragraph is one chunk;
///   - pure heading lines (`#`, `##`, ...) are dropped: they are structure,
///     not knowledge.
/// Markdown markers are stripped from the stored body so recall reads clean.
fn chunk_markdown(text: &str) -> Vec<String> {
    let mut chunks: Vec<String> = Vec::new();
    let mut paragraph: Vec<String> = Vec::new();
    let mut fence: Option<&str> = None;

    let flush = |paragraph: &mut Vec<String>, chunks: &mut Vec<String>| {
        if !paragraph.is_empty() {
            let joined = paragraph.join(" ").trim().to_string();
            if !joined.is_empty() {
                chunks.push(joined);
            }
            paragraph.clear();
        }
    };

    for raw in text.lines() {
        let line = raw.trim();
        // Fence state machine first: everything inside a fence is code and is
        // skipped, including lines that look like headings or bullets.
        if let Some(open) = fence {
            if line.starts_with(open) {
                fence = None;
            }
            continue;
        }
        if line.starts_with("```") || line.starts_with("~~~") {
            flush(&mut paragraph, &mut chunks);
            fence = Some(if line.starts_with("```") { "```" } else { "~~~" });
            continue;
        }
        if line.is_empty() {
            flush(&mut paragraph, &mut chunks);
            continue;
        }
        if is_heading(line) {
            flush(&mut paragraph, &mut chunks);
            continue;
        }
        if let Some(bullet) = strip_bullet(line) {
            // A bullet ends the running paragraph and stands alone.
            flush(&mut paragraph, &mut chunks);
            let bullet = bullet.trim();
            if !bullet.is_empty() {
                chunks.push(bullet.to_string());
            }
            continue;
        }
        paragraph.push(line.to_string());
    }
    flush(&mut paragraph, &mut chunks);
    chunks
}

fn is_heading(line: &str) -> bool {
    line.starts_with('#')
}

/// Strip a leading bullet or ordered-list marker, returning the content if the
/// line was a list item.
fn strip_bullet(line: &str) -> Option<&str> {
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = line.strip_prefix(marker) {
            return Some(rest);
        }
    }
    // Ordered list: "1. ", "2. ", ... Match digits then ". ".
    let bytes = line.as_bytes();
    let digits = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
    if digits > 0 && line[digits..].starts_with(". ") {
        return Some(&line[digits + 2..]);
    }
    None
}

/// Best-effort file anchor: scan the chunk for a token that names a file
/// existing under the repo root, and anchor to the first one found. Symbol
/// resolution from prose is unreliable, so `auto`/`file` anchor at file level,
/// which is exactly what template-heavy notes ("this layout is locked to
/// 480px") need. `none` never anchors.
fn resolve_chunk_anchor(root: &Path, chunk: &str, mode: AnchorMode) -> Vec<Value> {
    if mode == AnchorMode::None {
        return Vec::new();
    }
    for token in candidate_paths(chunk) {
        let rel = limpet::util::normalize_rel(&token);
        // Guard against traversal and absolute paths; validate_rel_path keeps
        // the anchor inside the repo.
        if limpet::util::validate_rel_path(root, &rel).is_ok() && root.join(&rel).is_file() {
            return vec![json!({ "file": rel })];
        }
    }
    Vec::new()
}

/// Pull out tokens that look like file paths: backtick-quoted spans and bare
/// words containing a `/` or ending in a common code/asset extension.
fn candidate_paths(chunk: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();

    // Backtick spans first: `src/scan/queue.php` is the strongest signal.
    let mut rest = chunk;
    while let Some(start) = rest.find('`') {
        rest = &rest[start + 1..];
        if let Some(end) = rest.find('`') {
            let span = rest[..end].trim();
            if looks_like_path(span) {
                out.push(span.to_string());
            }
            rest = &rest[end + 1..];
        } else {
            break;
        }
    }

    // Then bare tokens.
    for word in chunk.split_whitespace() {
        let cleaned = word.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '/' && c != '.' && c != '_' && c != '-');
        if looks_like_path(cleaned) {
            out.push(cleaned.to_string());
        }
    }
    out
}

fn looks_like_path(token: &str) -> bool {
    if token.is_empty() || token.len() > 256 {
        return false;
    }
    if token.contains('/') {
        return true;
    }
    // Includes the notes, template, and config extensions this feature is
    // aimed at: a bare `MEMORY.md` or `theme.scss` at the repo root should
    // anchor without needing a `/` in the token.
    const EXTS: [&str; 25] = [
        ".php", ".js", ".ts", ".tsx", ".jsx", ".py", ".rs", ".go", ".rb", ".java", ".cs", ".css",
        ".html", ".json", ".md", ".yml", ".yaml", ".toml", ".twig", ".scss", ".sass", ".vue",
        ".svelte", ".sh", ".sql",
    ];
    EXTS.iter().any(|e| token.ends_with(e))
}

fn hash64(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// The notes file resolves under `root` when relative, so it is found in the
/// same base its anchors point to; an absolute path is used verbatim.
fn resolve_notes_path(root: &Path, file: &str) -> PathBuf {
    let given = PathBuf::from(file);
    if given.is_absolute() {
        given
    } else {
        root.join(given)
    }
}

fn root_from(root_raw: Option<String>) -> Result<PathBuf> {
    let root = root_raw
        .map(PathBuf::from)
        .map_or_else(std::env::current_dir, Ok)?;
    let root = limpet::util::canonicalize_plain(&root)
        .with_context(|| format!("root does not exist: {}", root.display()))?;
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fenced_code_blocks_are_skipped_whole() {
        let notes = "Real prose fact here about the system.\n\n\
                     ```bash\n\
                     # this is a comment, not a heading\n\
                     - not a bullet, a yaml list item\n\
                     echo run me\n\
                     ```\n\n\
                     - a real bullet after the fence\n";
        let chunks = chunk_markdown(notes);
        assert_eq!(
            chunks,
            vec![
                "Real prose fact here about the system.".to_string(),
                "a real bullet after the fence".to_string(),
            ],
            "fence content leaked into chunks: {chunks:?}"
        );
        assert!(
            chunks.iter().all(|c| !c.contains("```")),
            "a fence marker landed inside a stored body"
        );
    }

    #[test]
    fn tilde_fences_and_unclosed_fences_are_handled() {
        let notes = "before\n~~~\ncode line\n~~~\nafter\n";
        assert_eq!(chunk_markdown(notes), vec!["before".to_string(), "after".to_string()]);
        // An unclosed fence swallows to EOF rather than leaking code as prose.
        let unclosed = "before\n```\ncode forever\n";
        assert_eq!(chunk_markdown(unclosed), vec!["before".to_string()]);
    }

    #[test]
    fn bare_note_and_template_filenames_look_like_paths() {
        for t in ["MEMORY.md", "config.yml", "theme.scss", "layout.twig", "app.vue"] {
            assert!(looks_like_path(t), "{t} should be anchorable");
        }
        assert!(!looks_like_path("just-a-word"));
    }

    #[test]
    fn notes_path_resolves_under_root_but_honors_absolute() {
        let root = Path::new("/repo");
        // A relative notes file is found under --root, not the CWD, so the
        // file and its anchors share one base (no split-brain trap).
        assert_eq!(resolve_notes_path(root, "MEMORY.md"), PathBuf::from("/repo/MEMORY.md"));
        assert_eq!(resolve_notes_path(root, "docs/NOTES.md"), PathBuf::from("/repo/docs/NOTES.md"));
        // An absolute path is used verbatim.
        assert_eq!(resolve_notes_path(root, "/etc/notes.md"), PathBuf::from("/etc/notes.md"));
    }
}
