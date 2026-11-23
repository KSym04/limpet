//! Small shared primitives: ULID generation, token estimation, repo keys,
//! and path validation. No network, no unsafe, no shell.

use anyhow::{bail, Result};
use sha2::{Digest, Sha256};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const CROCKFORD: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Canonicalize a path WITHOUT the Windows `\\?\` verbatim prefix.
///
/// `std::fs::canonicalize` returns verbatim paths on Windows (`\\?\C:\...`),
/// and verbatim paths disable the kernel's `/`->`\` translation. Since the
/// whole index stores repo-relative paths with `/`, a verbatim root makes
/// every `root.join("src/foo.rs")` unreadable. This is the single reason
/// limpet was broken for subdirectories on Windows. Callers that feed a
/// canonicalized root into the index MUST use this instead.
pub fn canonicalize_plain(path: &Path) -> std::io::Result<PathBuf> {
    let c = path.canonicalize()?;
    #[cfg(windows)]
    {
        let s = c.as_os_str().to_string_lossy();
        if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
            return Ok(PathBuf::from(format!(r"\\{rest}")));
        }
        if let Some(rest) = s.strip_prefix(r"\\?\") {
            return Ok(PathBuf::from(rest.to_string()));
        }
    }
    Ok(c)
}

/// Normalize a caller-supplied repo-relative path to the `/` separator the
/// index stores. A Windows agent naturally sends `src\util.rs`; without this
/// it would be stored, anchored, and compared with backslashes and never
/// match the walker's `/`-keyed rows.
pub fn normalize_rel(rel: &str) -> String {
    rel.replace('\\', "/")
}

/// Windows reserved device names: a component equal to one of these
/// (ignoring case and extension) resolves to a device, not a file, and would
/// let `admin export path:"NUL"` silently write to the void once the root is
/// no longer verbatim.
#[cfg(windows)]
fn is_reserved_device(component: &str) -> bool {
    let stem = component.split('.').next().unwrap_or(component);
    matches!(
        stem.to_ascii_uppercase().as_str(),
        "CON" | "PRN" | "AUX" | "NUL"
            | "COM1" | "COM2" | "COM3" | "COM4" | "COM5" | "COM6" | "COM7" | "COM8" | "COM9"
            | "LPT1" | "LPT2" | "LPT3" | "LPT4" | "LPT5" | "LPT6" | "LPT7" | "LPT8" | "LPT9"
    )
}

static LAST_ULID_STATE: AtomicU64 = AtomicU64::new(0);

/// Per-process entropy from the OS-seeded std hasher, so two processes
/// generating a ULID in the same millisecond cannot collide.
fn process_seed() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    use std::sync::OnceLock;
    static SEED: OnceLock<u64> = OnceLock::new();
    *SEED.get_or_init(|| {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(std::process::id() as u64);
        h.finish()
    })
}

/// Generate a 26-character Crockford base32 ULID.
///
/// Time-ordered: the first 10 chars encode Unix milliseconds. The next 16
/// chars carry a monotonic per-process sequence in the high bits (so IDs
/// created in the same millisecond still sort in creation order) followed
/// by per-process entropy. Cryptographic strength is not required here;
/// uniqueness and ordering are.
pub fn ulid() -> String {
    // A clock set before 1970 is absurd, but this runs on the remember hot path
    // and a panic there kills the live MCP session. Degrade to ms=0 instead: the
    // per-process sequence and entropy still keep the id unique; only ordering
    // for that one impossible case is lost.
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    let seq = LAST_ULID_STATE.fetch_add(1, Ordering::SeqCst);
    let mixed = splitmix64(ms ^ process_seed() ^ seq.rotate_left(17));
    // High 16 bits of the random section are the raw sequence: monotonic
    // within a process, which is what keeps same-millisecond IDs ordered.
    let hi16 = seq & 0xFFFF;

    let mut out = [0u8; 26];
    // 48-bit timestamp -> 10 chars (5 bits each).
    let mut t = ms & 0xFFFF_FFFF_FFFF;
    for i in (0..10).rev() {
        out[i] = CROCKFORD[(t & 0x1F) as usize];
        t >>= 5;
    }
    // 80-bit tail: 16 monotonic bits then 64 entropy bits -> 16 chars.
    let mut r: u128 = ((hi16 as u128) << 64) | (mixed as u128);
    for i in (10..26).rev() {
        out[i] = CROCKFORD[(r & 0x1F) as usize];
        r >>= 5;
    }
    // Every byte comes from CROCKFORD, which is compile-time ASCII, so each is a
    // valid single-byte codepoint. Build the string without a fallible UTF-8
    // check so id generation can never panic the server.
    out.iter().map(|&b| b as char).collect()
}

fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// Rough token count for budget packing: ceil(bytes / 4).
///
/// This is the standard "1 token ~ 4 bytes of English/code" approximation.
/// It is documented as an approximation everywhere it is surfaced.
pub fn token_estimate(s: &str) -> usize {
    s.len().div_ceil(4)
}

/// Filesystem-safe, INJECTIVE key for a repository, derived from a stable
/// identity (git origin when available, else the canonical path). A short
/// content hash of the exact identity string is appended to a readable slug,
/// so two identities that once slugged alike (`/a/b` vs `/a-b`) now differ.
pub fn repo_key(root: &Path) -> String {
    key_from_identity(&repo_identity(root))
}

/// Stable identity string for a repository: the normalized git `origin`
/// remote when the root is a git repo with one, otherwise the canonical
/// filesystem path. The `git:`/`path:` prefix keeps a path that happens to
/// look like a remote from colliding with an actual remote, and makes the
/// identity source legible in `doctor`.
pub fn repo_identity(root: &Path) -> String {
    if let Some(url) = git_remote_origin(root) {
        if let Some(norm) = normalize_git_remote(&url) {
            return format!("git:{norm}");
        }
    }
    let canon = canonicalize_plain(root).unwrap_or_else(|_| root.to_path_buf());
    format!("path:{}", canon.to_string_lossy())
}

/// Readable slug of an identity plus an 8-hex disambiguating hash, so the
/// key is both human-recognizable in the data dir and collision-free.
pub(crate) fn key_from_identity(identity: &str) -> String {
    let mut slug = String::with_capacity(48);
    for c in identity.chars() {
        if slug.len() >= 40 {
            break;
        }
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
            slug.push(c.to_ascii_lowercase());
        } else {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-').to_string();
    let hash = identity_hash8(identity);
    if slug.is_empty() {
        hash
    } else {
        format!("{slug}-{hash}")
    }
}

/// First 32 bits of SHA-256 as hex: enough to disambiguate the handful of
/// repositories one machine keys, with negligible collision odds.
fn identity_hash8(s: &str) -> String {
    let d = Sha256::digest(s.as_bytes());
    d[..4].iter().map(|b| format!("{b:02x}")).collect()
}

/// The pre-0.9 key algorithm, retained ONLY so migration can locate a store
/// created under the old lossy path-slug scheme. Never use for new stores.
pub fn legacy_repo_key(root: &Path) -> String {
    let canon = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let s = canon.to_string_lossy();
    let mut key = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
            key.push(c);
        } else {
            key.push('-');
        }
    }
    key.trim_matches('-').to_string()
}

/// The `origin` remote URL for a git repository root, or None if the root is
/// not a git repo or has no `origin`. Runs git with an argument array (never
/// a shell). When the git invocation itself fails for a reason unrelated to
/// the repository (macOS exits 69 until the Xcode licence is accepted, git
/// missing from PATH, a refused `safe.directory`), the url is read from the
/// repo's git config file instead (I-C1). Before 0.18 any such failure
/// silently degraded the identity to the path key, which forked the store:
/// the agent opened a brand-new empty memory with no error anywhere.
pub fn git_remote_origin(root: &Path) -> Option<String> {
    let via_git = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["remote", "get-url", "origin"])
        .output();
    match via_git {
        Ok(out) if out.status.success() => {
            let url = String::from_utf8(out.stdout).ok()?;
            let url = url.trim();
            if url.is_empty() {
                None
            } else {
                Some(url.to_string())
            }
        }
        // A clean "no such remote" exit and a broken git look alike from
        // here; the config file answers both honestly (no origin in the
        // file means None either way), so one fallback covers them.
        _ => git_config_origin(root),
    }
}

/// Hard cap on any git metadata file the fallback reads. A real config is a
/// few hundred bytes; the cap only exists so a hostile clone cannot make
/// identity resolution read an unbounded file.
const GIT_META_MAX_BYTES: u64 = 64 * 1024;

/// Read a small regular text file, refusing anything over `cap` bytes.
///
/// Only a regular file is ever opened (after symlinks are followed): a FIFO
/// or a device planted at `.git/config` (a symlink to `/dev/stdin`, say,
/// shipped in an archive) would otherwise block the SessionStart hook
/// forever or make `limpet serve` swallow the client's MCP stream. On unix
/// the open is also non-blocking and the type is re-checked on the open
/// handle, so a file swapped for a FIFO between the check and the open
/// still cannot block.
fn read_capped(path: &Path, cap: u64) -> Option<String> {
    use std::io::Read;
    if !std::fs::metadata(path).ok()?.is_file() {
        return None;
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NONBLOCK);
    }
    let f = opts.open(path).ok()?;
    if !f.metadata().ok()?.is_file() {
        return None;
    }
    let mut buf = Vec::new();
    f.take(cap + 1).read_to_end(&mut buf).ok()?;
    if buf.len() as u64 > cap {
        return None;
    }
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// Resolve a path written inside git metadata: absolute as-is, relative
/// against the directory that holds the pointer.
fn resolve_git_pointer(base: &Path, written: &str) -> PathBuf {
    let p = PathBuf::from(written);
    if p.is_absolute() {
        p
    } else {
        base.join(p)
    }
}

/// Locate the config file git would read for `root`, without running git:
/// walk up to the first `.git` git itself would accept, follow a `gitdir:`
/// pointer file (linked worktrees, submodules) and a `commondir` file
/// (worktrees share the main repository's config). Mirrors git discovery
/// where it matters: a `.git` directory without `HEAD` is not a repository
/// and the walk continues upward, and the walk never climbs into a
/// `GIT_CEILING_DIRECTORIES` entry. Read-only, every file size-capped.
fn find_git_config(root: &Path) -> Option<PathBuf> {
    let start = canonicalize_plain(root).unwrap_or_else(|_| root.to_path_buf());
    let ceilings: Vec<PathBuf> = std::env::var_os("GIT_CEILING_DIRECTORIES")
        .map(|v| {
            std::env::split_paths(&v)
                .filter(|p| p.is_absolute())
                .map(|p| canonicalize_plain(&p).unwrap_or(p))
                .collect()
        })
        .unwrap_or_default();
    for (i, dir) in start.ancestors().enumerate() {
        // The starting directory is always examined; a ceiling only stops
        // the walk from moving UP into it (git semantics).
        if i > 0 && ceilings.iter().any(|c| c == dir) {
            return None;
        }
        let dotgit = dir.join(".git");
        let gitdir = if dotgit.is_dir() {
            if !dotgit.join("HEAD").is_file() {
                continue;
            }
            dotgit
        } else if dotgit.is_file() {
            let text = read_capped(&dotgit, 4096)?;
            let written = text
                .lines()
                .find_map(|l| l.trim().strip_prefix("gitdir:"))?
                .trim()
                .to_string();
            if written.is_empty() {
                return None;
            }
            resolve_git_pointer(dir, &written)
        } else {
            continue;
        };
        let common = match read_capped(&gitdir.join("commondir"), 4096) {
            Some(c) if !c.trim().is_empty() => resolve_git_pointer(&gitdir, c.trim()),
            _ => gitdir,
        };
        return Some(common.join("config"));
    }
    None
}

/// The `origin` url from the repository's git config file, read directly.
/// Deliberate limits, documented in SPEC I-C1: `include`/`includeIf` are not
/// followed and `url.<base>.insteadOf` rewrites are not applied. Git output
/// stays primary whenever git runs, so these limits only matter while git
/// itself is broken, and even then the result is never worse than the path
/// key the old code fell back to.
pub(crate) fn git_config_origin(root: &Path) -> Option<String> {
    let config = find_git_config(root)?;
    let text = read_capped(&config, GIT_META_MAX_BYTES)?;
    parse_origin_url(&text)
}

/// Pull the first `url` of `[remote "origin"]` out of git config text.
/// Section and key names are case-insensitive, the `"origin"` subsection is
/// exact (git semantics, after its `\"` / `\\` escapes are decoded), the
/// deprecated `[remote.origin]` spelling is accepted, values may be quoted,
/// `#`/`;` start a comment outside quotes, a trailing backslash continues a
/// value onto the next line, and a leading UTF-8 BOM is ignored. The first
/// url wins, matching `git remote get-url`. Not modelled (SPEC I-C1):
/// `include`/`includeIf`, `insteadOf`, and a per-worktree `config.worktree`.
pub(crate) fn parse_origin_url(text: &str) -> Option<String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut in_origin = false;
    for logical in config_logical_lines(text) {
        let mut line = logical.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(rest) = line.strip_prefix('[') {
            let Some(end) = header_end(rest) else {
                in_origin = false;
                continue;
            };
            in_origin = is_remote_origin_header(&rest[..end]);
            // Git allows `[section] key = value` on one line.
            line = rest[end + 1..].trim();
            if line.is_empty() {
                continue;
            }
        }
        if !in_origin {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if !key.trim().eq_ignore_ascii_case("url") {
            continue;
        }
        let value = parse_git_config_value(value);
        if !value.is_empty() {
            return Some(value);
        }
    }
    None
}

/// Split config text into logical lines: a physical line ending in an odd
/// number of backslashes continues onto the next (the backslash and the
/// newline are both dropped, as git does inside a value).
fn config_logical_lines(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for raw in text.lines() {
        // A comment ends at its newline; only a value line continues.
        let is_comment = cur.is_empty() && matches!(raw.trim_start().chars().next(), Some('#' | ';'));
        let trailing = raw.chars().rev().take_while(|&c| c == '\\').count();
        if trailing % 2 == 1 && !is_comment {
            cur.push_str(&raw[..raw.len() - 1]);
            continue;
        }
        cur.push_str(raw);
        out.push(std::mem::take(&mut cur));
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Index of the `]` closing a section header, skipping a `]` inside the
/// quoted subsection (`[remote "a]b"]`).
fn header_end(rest: &str) -> Option<usize> {
    let mut quoted = false;
    let mut escaped = false;
    for (i, c) in rest.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            ']' if !quoted => return Some(i),
            _ => {}
        }
    }
    None
}

/// True for a `remote "origin"` (or legacy `remote.origin`) section header.
/// The quoted subsection is decoded the way git decodes it: `\x` is `x`
/// for any `x`, so `"orig\in"` names the remote `origin`.
fn is_remote_origin_header(header: &str) -> bool {
    let header = header.trim();
    match header.split_once(char::is_whitespace) {
        Some((section, sub)) => {
            let sub = sub.trim();
            let Some(inner) = sub.strip_prefix('"').and_then(|s| s.strip_suffix('"')) else {
                return false;
            };
            let mut name = String::new();
            let mut chars = inner.chars();
            while let Some(c) = chars.next() {
                if c == '\\' {
                    if let Some(n) = chars.next() {
                        name.push(n);
                    }
                } else {
                    name.push(c);
                }
            }
            section.eq_ignore_ascii_case("remote") && name == "origin"
        }
        None => header.eq_ignore_ascii_case("remote.origin"),
    }
}

/// Decode one git config value: quotes toggle, backslash escapes (`\n`,
/// `\t`, `\b`, `\"`, `\\`), and an unquoted `#` or `;` ends the value.
fn parse_git_config_value(v: &str) -> String {
    let mut out = String::new();
    let mut quoted = false;
    let mut chars = v.trim().chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => quoted = !quoted,
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('b') => out.push('\u{8}'),
                Some(other) => out.push(other),
                None => {}
            },
            '#' | ';' if !quoted => break,
            _ => out.push(c),
        }
    }
    out.trim().to_string()
}

/// Provider SSH host aliases that serve the SAME repositories as the
/// canonical host (I-C2). Exact match only: a blanket `ssh.` strip would
/// merge hosts that are genuinely different (`ssh.dev.azure.com` is not
/// `dev.azure.com`). Without this table a provider changing its recommended
/// clone host (Bitbucket moved to `ssh.bitbucket.org` in 2026) re-keys every
/// affected repository and orphans its store.
fn canonical_git_host(host: &str) -> &str {
    GIT_HOST_ALIASES
        .iter()
        .find(|(alias, _)| *alias == host)
        .map(|(_, canonical)| *canonical)
        .unwrap_or(host)
}

/// The alias table behind [`canonical_git_host`]: `(alias, canonical)`.
const GIT_HOST_ALIASES: &[(&str, &str)] = &[
    ("ssh.github.com", "github.com"),
    ("ssh.bitbucket.org", "bitbucket.org"),
    ("altssh.bitbucket.org", "bitbucket.org"),
    ("altssh.gitlab.com", "gitlab.com"),
];

/// An identity string with its git host mapped through the alias table, so
/// an identity recorded before the table existed (`git:ssh.github.com/a/b`)
/// compares equal to today's (`git:github.com/a/b`). `path:` identities and
/// unknown hosts come back unchanged.
pub fn canonical_identity(identity: &str) -> String {
    match identity.strip_prefix("git:").and_then(|r| r.split_once('/')) {
        Some((host, path)) => format!("git:{}/{path}", canonical_git_host(host)),
        None => identity.to_string(),
    }
}

/// Store keys that provably belong to the SAME repository as `identity` at
/// `root_canon` (I-C3): the path key this root gets whenever git and the
/// config fallback both fail (the F1 fork), and the key every provider SSH
/// alias of the current remote produced before the alias table existed (the
/// F2 fork, which a checkout at a different path also left behind).
pub fn equivalent_store_keys(root_canon: &Path, identity: &str) -> Vec<String> {
    let mut keys = vec![key_from_identity(&format!("path:{}", root_canon.to_string_lossy()))];
    if let Some((host, path)) = identity.strip_prefix("git:").and_then(|r| r.split_once('/')) {
        for (alias, canonical) in GIT_HOST_ALIASES {
            if *canonical == host {
                keys.push(key_from_identity(&format!("git:{alias}/{path}")));
            }
        }
    }
    keys
}

/// Canonicalize a git remote URL to `host/path` (lowercased, no scheme, no
/// credentials, no port, no trailing `.git`, known provider SSH aliases
/// mapped to their canonical host), so ssh, https, and scp-style spellings
/// of the same repository collapse to one identity.
pub fn normalize_git_remote(url: &str) -> Option<String> {
    let s = url.trim();
    if s.is_empty() {
        return None;
    }
    // Strip scheme:// , or interpret an scp-style `user@host:path`.
    let after_scheme = match s.split_once("://") {
        Some((_scheme, rest)) => rest.to_string(),
        None => {
            let (userhost, path) = s.split_once(':')?;
            if userhost.contains('/') {
                return None;
            }
            format!("{userhost}/{path}")
        }
    };
    // Drop credentials before an '@'.
    let after_creds = match after_scheme.split_once('@') {
        Some((_creds, rest)) => rest,
        None => after_scheme.as_str(),
    };
    let (host, path) = after_creds.trim_start_matches('/').split_once('/')?;
    let host = host.split(':').next().unwrap_or(host).to_ascii_lowercase();
    let host = canonical_git_host(&host);
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let path = path.trim_matches('/');
    if host.is_empty() || path.is_empty() {
        return None;
    }
    Some(format!("{host}/{}", path.to_ascii_lowercase()))
}

/// Validate a repo-relative path supplied by a tool caller.
///
/// Rejects absolute paths and any `..` traversal, then joins onto `root`.
/// This is the single choke point for all file arguments arriving over MCP.
pub fn validate_rel_path(root: &Path, rel: &str) -> Result<PathBuf> {
    // Accept either separator from callers; the index speaks `/`.
    let rel_norm = normalize_rel(rel);
    let p = Path::new(&rel_norm);
    if p.as_os_str().is_empty() {
        bail!("empty path is not allowed");
    }
    if p.is_absolute() {
        bail!("absolute paths are not allowed: {rel}");
    }
    for comp in p.components() {
        match comp {
            Component::ParentDir => bail!("path traversal is not allowed: {rel}"),
            Component::Prefix(_) | Component::RootDir => {
                bail!("absolute paths are not allowed: {rel}")
            }
            Component::Normal(name) => {
                #[cfg(windows)]
                if is_reserved_device(&name.to_string_lossy()) {
                    bail!("reserved device name is not allowed: {rel}");
                }
                let _ = name;
            }
            _ => {}
        }
    }
    let joined = root.join(p);
    // Symlink check (audit 2026-07): canonicalize the deepest EXISTING
    // ancestor (the file itself may legitimately not exist yet, e.g. a
    // fresh export target) and require it to stay under the canonical
    // root, so a repo-internal `evil -> /etc` link cannot escape.
    if let Ok(canon_root) = root.canonicalize() {
        let mut probe: &Path = &joined;
        let canon_probe = loop {
            match probe.canonicalize() {
                Ok(c) => break Some(c),
                Err(_) => match probe.parent() {
                    Some(parent) => probe = parent,
                    None => break None,
                },
            }
        };
        if let Some(c) = canon_probe {
            if !c.starts_with(&canon_root) {
                bail!("path escapes the repository via a symlink: {rel}");
            }
        }
    }
    Ok(joined)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ulid_is_26_chars_and_sorts() {
        let a = ulid();
        let b = ulid();
        assert_eq!(a.len(), 26);
        assert_eq!(b.len(), 26);
        assert!(a < b, "ulids must be monotonic within a process: {a} !< {b}");
        assert!(a.bytes().all(|c| CROCKFORD.contains(&c)));
    }

    #[test]
    fn token_estimate_is_bytes_over_four_ceil() {
        assert_eq!(token_estimate(""), 0);
        assert_eq!(token_estimate("abcd"), 1);
        assert_eq!(token_estimate("abcde"), 2);
    }

    #[test]
    fn normalize_rel_converts_backslashes() {
        assert_eq!(normalize_rel("src\\util.rs"), "src/util.rs");
        assert_eq!(normalize_rel("a\\b\\c"), "a/b/c");
        assert_eq!(normalize_rel("already/unix"), "already/unix");
    }

    #[test]
    fn validate_accepts_backslash_input_and_normalizes() {
        // A Windows-style rel must validate and join correctly.
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        let out = validate_rel_path(dir.path(), "src\\lib.rs").unwrap();
        assert!(out.ends_with("lib.rs"));
    }

    #[test]
    fn rel_path_rejects_empty_and_symlink_escape() {
        assert!(validate_rel_path(Path::new("/tmp/repo"), "").is_err());

        #[cfg(unix)]
        {
            let dir = tempfile::TempDir::new().unwrap();
            let root = dir.path();
            std::fs::create_dir(root.join("ok")).unwrap();
            std::os::unix::fs::symlink("/etc", root.join("evil")).unwrap();
            assert!(
                validate_rel_path(root, "evil/hosts").is_err(),
                "symlink out of the repo must be rejected"
            );
            assert!(validate_rel_path(root, "ok/new-file.txt").is_ok());
        }
    }

    #[test]
    fn rel_path_validation_blocks_escape() {
        let root = Path::new("/tmp/repo");
        assert!(validate_rel_path(root, "../etc/passwd").is_err());
        assert!(validate_rel_path(root, "/etc/passwd").is_err());
        assert!(validate_rel_path(root, "a/../../b").is_err());
        assert_eq!(
            validate_rel_path(root, "src/lib.rs").unwrap(),
            PathBuf::from("/tmp/repo/src/lib.rs")
        );
    }

    #[test]
    fn repo_key_is_fs_safe() {
        let k = repo_key(Path::new("/Users/x/Dev/My App"));
        assert!(!k.contains('/'));
        assert!(!k.contains(' '));
    }

    #[test]
    fn repo_key_is_injective_across_slug_collisions() {
        // The pre-0.9 slug mapped /a/b and /a-b onto the same key, silently
        // sharing one store between two repos. The new key must not collide.
        let k1 = repo_key(Path::new("/a/b"));
        let k2 = repo_key(Path::new("/a-b"));
        assert_ne!(k1, k2, "distinct roots must get distinct keys");
    }

    #[test]
    fn legacy_repo_key_reproduces_old_algorithm() {
        // Migration depends on reproducing the pre-0.9 key byte-for-byte to
        // find an existing store's directory.
        assert_eq!(legacy_repo_key(Path::new("/a/b")), "a-b");
        assert_eq!(legacy_repo_key(Path::new("/tmp/x y")), "tmp-x-y");
    }

    #[test]
    fn normalize_git_remote_collapses_url_forms() {
        let want = Some("github.com/ksym04/limpet".to_string());
        assert_eq!(normalize_git_remote("git@github.com:KSym04/limpet.git"), want);
        assert_eq!(normalize_git_remote("https://github.com/KSym04/limpet.git"), want);
        assert_eq!(normalize_git_remote("https://github.com/KSym04/limpet"), want);
        assert_eq!(normalize_git_remote("ssh://git@github.com/KSym04/limpet.git"), want);
        assert_eq!(normalize_git_remote("https://user:pass@github.com/KSym04/limpet.git"), want);
        assert_eq!(normalize_git_remote("git@github.com:KSym04/limpet.git/"), want);
        assert_eq!(normalize_git_remote("not a url"), None);
        assert_eq!(normalize_git_remote(""), None);
    }

    #[test]
    fn provider_ssh_aliases_collapse_to_the_canonical_host() {
        // The field fork (I-C2): Bitbucket's recommended host change re-keyed
        // every repo. All spellings of one repository are one identity.
        let bb = Some("bitbucket.org/acme/site".to_string());
        assert_eq!(normalize_git_remote("git@bitbucket.org:acme/site.git"), bb);
        assert_eq!(normalize_git_remote("git@ssh.bitbucket.org:acme/site.git"), bb);
        assert_eq!(normalize_git_remote("ssh://git@altssh.bitbucket.org:443/acme/site.git"), bb);
        let gh = Some("github.com/o/r".to_string());
        assert_eq!(normalize_git_remote("ssh://git@ssh.github.com:443/o/r.git"), gh);
        assert_eq!(normalize_git_remote("ssh://git@SSH.GitHub.com:443/o/r.git"), gh);
        assert_eq!(
            normalize_git_remote("ssh://git@altssh.gitlab.com:443/g/p.git"),
            Some("gitlab.com/g/p".to_string())
        );
        // Exact match only: a genuinely different host is never merged.
        assert_eq!(
            normalize_git_remote("git@ssh.dev.azure.com:v3/org/proj/repo"),
            Some("ssh.dev.azure.com/v3/org/proj/repo".to_string())
        );
        assert_eq!(
            normalize_git_remote("git@ssh.example.com:a/b.git"),
            Some("ssh.example.com/a/b".to_string())
        );
    }

    #[test]
    fn origin_url_parses_from_git_config_text() {
        let cfg = "[core]\n\trepositoryformatversion = 0\n\
                   [remote \"upstream\"]\n\turl = git@github.com:up/stream.git\n\
                   [remote \"origin\"]\n\turl = git@github.com:me/repo.git\n\
                   \turl = git@github.com:second/ignored.git\n\
                   \tfetch = +refs/heads/*:refs/remotes/origin/*\n";
        assert_eq!(parse_origin_url(cfg).as_deref(), Some("git@github.com:me/repo.git"));
        // Case-insensitive section and key, quoted value, inline comment.
        let cfg = "; top comment\n# another\n[REMOTE \"origin\"]\n  URL = \"https://h.example/a b.git\" # note\n";
        assert_eq!(parse_origin_url(cfg).as_deref(), Some("https://h.example/a b.git"));
        // The subsection is exact: "Origin" is a different remote.
        assert_eq!(parse_origin_url("[remote \"Origin\"]\nurl = x:y/z\n"), None);
        // Deprecated dotted spelling, and a key on the header line.
        assert_eq!(parse_origin_url("[remote.origin]\nurl = a:b/c\n").as_deref(), Some("a:b/c"));
        assert_eq!(parse_origin_url("[remote \"origin\"] url = a:b/d\n").as_deref(), Some("a:b/d"));
        // A later section closes origin; an unquoted ';' ends the value.
        assert_eq!(
            parse_origin_url("[remote \"origin\"]\n[core]\nurl = nope:x/y\n"),
            None
        );
        assert_eq!(parse_origin_url("[remote \"origin\"]\nurl = a:b/e;junk\n").as_deref(), Some("a:b/e"));
        assert_eq!(parse_origin_url(""), None);
        assert_eq!(parse_origin_url("[remote \"origin\"]\nurl =\n"), None);
    }

    #[test]
    fn config_fallback_finds_origin_without_running_git() {
        // Plain `.git` directory, queried from a nested subdirectory.
        let t = tempfile::tempdir().unwrap();
        let repo = t.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::create_dir_all(repo.join("src/deep")).unwrap();
        std::fs::write(
            repo.join(".git/config"),
            "[remote \"origin\"]\n\turl = git@ssh.bitbucket.org:acme/site.git\n",
        )
        .unwrap();
        assert_eq!(
            git_config_origin(&repo.join("src/deep")).as_deref(),
            Some("git@ssh.bitbucket.org:acme/site.git")
        );

        // Linked worktree: `.git` is a file pointing at a per-worktree dir
        // whose `commondir` leads back to the main repo's config.
        let wt = t.path().join("wt");
        std::fs::create_dir_all(repo.join(".git/worktrees/wt")).unwrap();
        std::fs::write(repo.join(".git/worktrees/wt/commondir"), "../..\n").unwrap();
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".git"), "gitdir: ../repo/.git/worktrees/wt\n").unwrap();
        assert_eq!(
            git_config_origin(&wt).as_deref(),
            Some("git@ssh.bitbucket.org:acme/site.git")
        );

        // No `.git` anywhere up the tree under the temp root: None (the walk
        // continues above the temp dir, so only assert on an isolated repo
        // without a remote).
        let bare = t.path().join("noremote");
        std::fs::create_dir_all(bare.join(".git")).unwrap();
        std::fs::write(bare.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(bare.join(".git/config"), "[core]\n\tbare = false\n").unwrap();
        assert_eq!(git_config_origin(&bare), None);

        // An oversized config is refused, not read.
        let big = t.path().join("big");
        std::fs::create_dir_all(big.join(".git")).unwrap();
        std::fs::write(big.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        let mut body = String::from("[remote \"origin\"]\nurl = a:b/c\n");
        body.push_str(&"#".repeat(GIT_META_MAX_BYTES as usize));
        std::fs::write(big.join(".git/config"), body).unwrap();
        assert_eq!(git_config_origin(&big), None);
    }

    #[test]
    fn origin_parse_matches_git_on_escapes_bom_and_continuations() {
        // A leading BOM is not part of the first header.
        assert_eq!(parse_origin_url("\u{feff}[remote \"origin\"]\nurl = a:b/c\n").as_deref(), Some("a:b/c"));
        // Backslash-newline continues the value; the pieces join.
        assert_eq!(
            parse_origin_url("[remote \"origin\"]\nurl = git@h.example:ac\\\nme/site.git\n").as_deref(),
            Some("git@h.example:acme/site.git")
        );
        // An escaped backslash at line end is NOT a continuation.
        assert_eq!(parse_origin_url("[remote \"origin\"]\nurl = a:b\\\\\nfetch = x\n").as_deref(), Some("a:b\\"));
        // A comment line ending in a backslash does not swallow the header.
        assert_eq!(parse_origin_url("# note \\\n[remote \"origin\"]\nurl = a:b/c\n").as_deref(), Some("a:b/c"));
        // Subsection escapes decode the way git decodes them.
        assert_eq!(parse_origin_url("[remote \"orig\\in\"]\nurl = a:b/d\n").as_deref(), Some("a:b/d"));
        assert_eq!(parse_origin_url("[remote \"orig]in\"]\nurl = a:b/e\n"), None);
        // \b is a backspace, not a literal b.
        assert_eq!(parse_git_config_value("a\\bc"), "a\u{8}c");
    }

    #[test]
    fn config_fallback_follows_git_discovery() {
        let t = tempfile::tempdir().unwrap();
        let outer = t.path().join("outer");
        std::fs::create_dir_all(outer.join(".git")).unwrap();
        std::fs::write(outer.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(outer.join(".git/config"), "[remote \"origin\"]\nurl = a:outer/repo\n").unwrap();
        // A `.git` dir without HEAD is not a repository: git walks past it
        // to the enclosing one, so the fallback must too.
        let inner = outer.join("inner");
        std::fs::create_dir_all(inner.join(".git")).unwrap();
        std::fs::write(inner.join(".git/config"), "[remote \"origin\"]\nurl = a:fake/repo\n").unwrap();
        assert_eq!(find_git_config(&inner), Some(canonicalize_plain(&outer).unwrap().join(".git/config")));
    }

    #[cfg(unix)]
    #[test]
    fn config_fallback_never_blocks_on_a_fifo_or_device() {
        let t = tempfile::tempdir().unwrap();
        let fifo_repo = t.path().join("fifo");
        std::fs::create_dir_all(fifo_repo.join(".git")).unwrap();
        std::fs::write(fifo_repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        let st = std::process::Command::new("mkfifo").arg(fifo_repo.join(".git/config")).status().unwrap();
        assert!(st.success());
        let dev_repo = t.path().join("dev");
        std::fs::create_dir_all(dev_repo.join(".git")).unwrap();
        std::fs::write(dev_repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::os::unix::fs::symlink("/dev/stdin", dev_repo.join(".git/config")).unwrap();
        // A blocking open would hang this thread forever; the channel turns
        // that into a bounded failure.
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send((git_config_origin(&fifo_repo), git_config_origin(&dev_repo)));
        });
        let got = rx.recv_timeout(std::time::Duration::from_secs(5)).expect("fallback blocked on a FIFO");
        assert_eq!(got, (None, None));
    }

    #[test]
    fn alias_keys_and_identities_recognize_the_same_repository() {
        assert_eq!(canonical_identity("git:ssh.github.com/a/b"), "git:github.com/a/b");
        assert_eq!(canonical_identity("git:github.com/a/b"), "git:github.com/a/b");
        assert_eq!(canonical_identity("path:/x/y"), "path:/x/y");
        let root = Path::new("/work/site");
        let keys = equivalent_store_keys(root, "git:bitbucket.org/acme/site");
        assert!(keys.contains(&key_from_identity("path:/work/site")));
        assert!(keys.contains(&key_from_identity("git:ssh.bitbucket.org/acme/site")));
        assert!(keys.contains(&key_from_identity("git:altssh.bitbucket.org/acme/site")));
        assert!(!keys.contains(&key_from_identity("git:bitbucket.org/acme/site")));
        // A host with no alias yields only the path key.
        assert_eq!(equivalent_store_keys(root, "git:example.com/acme/site").len(), 1);
    }
}
