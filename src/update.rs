//! Self-update: fetch the latest published release binary for this platform,
//! verify its SHA-256 against the published checksum, and atomically replace
//! the running executable.
//!
//! The network is touched only here, and only when the user runs
//! `limpet update` (or `limpet update --check`). Every other command
//! stays fully offline.

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::io::Read;

const REPO: &str = "KSym04/limpet";
const USER_AGENT: &str = concat!("limpet/", env!("CARGO_PKG_VERSION"));

/// Exit code emitted by `--check` when a newer version is available, so
/// scripts and status lines can detect it without parsing stdout.
const UPDATE_AVAILABLE_EXIT: i32 = 10;

/// Map the compile-time target to its published raw-binary asset name.
fn asset_name() -> Result<&'static str> {
    Ok(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "limpet-aarch64-apple-darwin",
        ("macos", "x86_64") => "limpet-x86_64-apple-darwin",
        ("linux", "x86_64") => "limpet-x86_64-unknown-linux-gnu",
        ("windows", "x86_64") => "limpet-x86_64-pc-windows-msvc.exe",
        (os, arch) => bail!(
            "no prebuilt binary for {os}/{arch}; build from source with `cargo install --path .`"
        ),
    })
}

/// Parse a dotted version into a comparable tuple. Missing or non-numeric
/// components sort low, so a malformed remote can never appear "newer".
fn semver(v: &str) -> (u64, u64, u64) {
    let mut it = v
        .trim_start_matches('v')
        .split('.')
        .map(|p| p.parse::<u64>().unwrap_or(0));
    (
        it.next().unwrap_or(0),
        it.next().unwrap_or(0),
        it.next().unwrap_or(0),
    )
}

/// HTTPS GET with the required User-Agent. `ureq` treats any non-2xx status as
/// an error, so a missing asset or API failure never reaches the caller as a
/// success (INVARIANT I1).
fn http_get(url: &str) -> Result<ureq::Response> {
    ureq::get(url)
        .set("User-Agent", USER_AGENT)
        .call()
        .with_context(|| format!("GET {url}"))
}

/// Latest release tag from the GitHub API, with any leading 'v' stripped.
fn latest_version() -> Result<String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let body = http_get(&url)?
        .into_string()
        .context("reading releases API response")?;
    let v: serde_json::Value =
        serde_json::from_str(&body).context("parsing releases API response")?;
    let tag = v
        .get("tag_name")
        .and_then(|t| t.as_str())
        .context("releases API response has no tag_name")?;
    Ok(tag.trim_start_matches('v').to_string())
}

/// `limpet update [--check]`.
pub fn run(check_only: bool) -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    let latest = latest_version()?;

    // INVARIANT I2: never downgrade or reinstall the same version.
    if semver(&latest) <= semver(current) {
        println!("limpet is up to date ({current})");
        return Ok(());
    }

    if check_only {
        println!("update available: {current} -> {latest}");
        println!("run `limpet update` to install");
        std::process::exit(UPDATE_AVAILABLE_EXIT);
    }

    let asset = asset_name()?;
    let base = format!("https://github.com/{REPO}/releases/download/v{latest}");
    println!("updating {current} -> {latest} ...");

    // Download the binary fully into memory.
    let mut bin = Vec::new();
    http_get(&format!("{base}/{asset}"))?
        .into_reader()
        .read_to_end(&mut bin)
        .context("downloading binary")?;

    // Download and parse the published checksum (`<hex>  <name>`).
    let sha_line = http_get(&format!("{base}/{asset}.sha256"))?
        .into_string()
        .context("downloading checksum")?;
    let want = sha_line
        .split_whitespace()
        .next()
        .context("empty checksum file")?
        .to_lowercase();

    // INVARIANT I1: verify before anything touches the disk.
    let got: String = {
        let d = Sha256::digest(&bin);
        d.iter().map(|b| format!("{b:02x}")).collect()
    };
    if got != want {
        bail!("checksum mismatch (expected {want}, got {got}); aborting, nothing changed");
    }

    // Stage to a temp file, then atomically swap it in (INVARIANT I3).
    // `self_replace` handles the Windows case where a running .exe cannot be
    // overwritten directly.
    let tmp = std::env::temp_dir().join(format!("limpet-update-{}", std::process::id()));
    std::fs::write(&tmp, &bin).with_context(|| format!("writing {}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
            .context("marking new binary executable")?;
    }
    let replace = self_replace::self_replace(&tmp).context("replacing the running executable");
    let _ = std::fs::remove_file(&tmp);
    replace?;

    println!("updated {current} -> {latest}");
    println!("restart Claude Code to reload the MCP server onto the new binary.");
    println!("(servers still running the old image will refuse store writes until restarted.)");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_orders_numerically_not_lexically() {
        assert!(semver("0.10.0") > semver("0.9.0"));
        assert!(semver("1.0.0") > semver("0.99.99"));
        assert_eq!(semver("v0.3.0"), semver("0.3.0"));
        assert!(semver("0.4.0") > semver("0.3.0"));
    }

    #[test]
    fn malformed_version_never_outranks_a_real_one() {
        assert!(semver("garbage") <= semver("0.0.1"));
        assert!(semver("") <= semver("0.0.1"));
    }

    #[test]
    fn asset_name_is_known_for_this_platform() {
        // The host running the test suite must be a supported target.
        assert!(asset_name().is_ok());
    }
}
