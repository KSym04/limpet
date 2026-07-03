//! Fully qualified name formation.
//!
//! Shape: `<rel path, extension stripped, '/' -> '.'>[.<parent>...].<name>`
//! Example: `src/scan/queue.php` + class `ScanQueue` + method `push`
//! becomes `src.scan.queue.ScanQueue.push`.

/// Build an FQN from a repo-relative path, enclosing symbol names, and the
/// symbol's own name.
pub fn fqn(rel_path: &str, parents: &[&str], name: &str) -> String {
    let mut base = rel_path.replace('\\', "/");
    if let Some(dot) = base.rfind('.') {
        // Strip a real extension only (dot after the last slash).
        let last_slash = base.rfind('/').map(|i| i + 1).unwrap_or(0);
        if dot > last_slash {
            base.truncate(dot);
        }
    }
    let mut out = base.replace('/', ".");
    for p in parents {
        out.push('.');
        out.push_str(p);
    }
    out.push('.');
    out.push_str(name);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fqn_strips_extension_and_dots_path() {
        assert_eq!(fqn("src/lib.rs", &[], "hello"), "src.lib.hello");
        assert_eq!(
            fqn("src/scan/queue.php", &["ScanQueue"], "push"),
            "src.scan.queue.ScanQueue.push"
        );
    }

    #[test]
    fn fqn_keeps_dotted_dirs_intact() {
        // A dot in a directory name must not be treated as an extension.
        assert_eq!(fqn("pkg.d/file", &[], "f"), "pkg.d.file.f");
    }
}
