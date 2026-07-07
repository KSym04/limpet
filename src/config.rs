//! Repo-level configuration read from `.limpet.json` at the repository root.
//!
//! The file is optional and repo-controlled, so it is treated as untrusted
//! (invariant I-P3): its size is bounded, grammar values are validated
//! against the six shipped languages, and it is a pure lookup table with no
//! executable content. A missing file yields defaults; a malformed, oversize,
//! or invalid file is a hard error surfaced to the caller rather than silently
//! ignored, so a broken config never degrades indexing unnoticed.

use crate::index::lang::{self, Lang};
use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::path::Path;

/// Upper bound on `.limpet.json` size. A config is a small lookup table;
/// anything larger is a mistake or an attempt to waste memory.
const MAX_CONFIG_BYTES: u64 = 64 * 1024;

/// The config file name at the repository root.
const CONFIG_FILE: &str = ".limpet.json";

/// Parsed, validated `.limpet.json`. Consumers use the fields directly
/// without re-checking them.
#[derive(Debug, Clone)]
pub struct RepoConfig {
    /// Extension suffix (no leading dot) -> grammar override, layered over
    /// the built-in `lang::detect` table.
    pub extensions: HashMap<String, Lang>,
    /// Auto-import a committed `.limpet/memory.jsonl` on first index.
    pub auto_import: bool,
}

impl Default for RepoConfig {
    fn default() -> Self {
        RepoConfig { extensions: HashMap::new(), auto_import: true }
    }
}

/// The on-disk shape before validation. Kept separate so `RepoConfig` can
/// hold resolved `Lang` values instead of raw strings.
#[derive(serde::Deserialize)]
struct RawConfig {
    #[serde(default)]
    extensions: HashMap<String, String>,
    #[serde(default = "default_true")]
    auto_import: bool,
}

fn default_true() -> bool {
    true
}

impl RepoConfig {
    /// Load and validate `<root>/.limpet.json`. A missing file returns
    /// defaults. A file over the size cap, malformed JSON, or an unknown
    /// grammar value returns an error naming the problem.
    pub fn load(root: &Path) -> Result<RepoConfig> {
        let path = root.join(CONFIG_FILE);
        let meta = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(_) => return Ok(RepoConfig::default()),
        };
        if meta.len() > MAX_CONFIG_BYTES {
            bail!("{CONFIG_FILE} is {} bytes, over the {MAX_CONFIG_BYTES}-byte limit", meta.len());
        }
        let text =
            std::fs::read_to_string(&path).with_context(|| format!("reading {CONFIG_FILE}"))?;
        let raw: RawConfig =
            serde_json::from_str(&text).with_context(|| format!("parsing {CONFIG_FILE}"))?;

        let mut extensions = HashMap::with_capacity(raw.extensions.len());
        for (ext, grammar) in raw.extensions {
            let lang = lang::from_config_str(&grammar).ok_or_else(|| {
                anyhow::anyhow!(
                    "{CONFIG_FILE}: unknown grammar \"{grammar}\" for extension \"{ext}\" \
                     (expected one of php, js, ts, py, rust, cpp)"
                )
            })?;
            let ext = ext.trim_start_matches('.').to_string();
            if ext.is_empty() {
                bail!("{CONFIG_FILE}: empty extension key");
            }
            extensions.insert(ext, lang);
        }
        Ok(RepoConfig { extensions, auto_import: raw.auto_import })
    }
}
