//! Small shared primitives: ULID generation, token estimation, repo keys,
//! and path validation. No network, no unsafe, no shell.

use anyhow::{bail, Result};
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
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before 1970")
        .as_millis() as u64;

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
    String::from_utf8(out.to_vec()).expect("crockford alphabet is ascii")
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

/// Stable filesystem-safe key for a repository root path.
pub fn repo_key(root: &Path) -> String {
    let canon = root
        .canonicalize()
        .unwrap_or_else(|_| root.to_path_buf());
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
}
