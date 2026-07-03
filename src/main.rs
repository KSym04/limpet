//! limpet: memory that clamps onto your code.
//!
//! CLI surface:
//!   limpet serve   [--root <path>]        MCP stdio server
//!   limpet index   [--root <path>]        full index
//!   limpet status  [--root <path>]        counts + freshness
//!   limpet export  [--root <path>]        memory -> .limpet/memory.jsonl
//!   limpet import  [--root <path>]        .limpet/memory.jsonl -> memory
//!   limpet install [--dry-run]            register with Claude Code
//!   limpet uninstall                      remove registration
//!   limpet update  [--check]              self-update to the latest release

use anyhow::{bail, Context, Result};
use limpet::{index, mcp, memory, store, tools, ui};
use std::path::PathBuf;

mod update;

fn main() {
    if let Err(e) = run() {
        eprintln!("limpet: {e:#}");
        std::process::exit(1);
    }
}

fn arg_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn root_from(args: &[String]) -> Result<PathBuf> {
    let root = arg_value(args, "--root")
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir()?);
    let root = root
        .canonicalize()
        .with_context(|| format!("root does not exist: {}", root.display()))?;
    Ok(root)
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("help");

    // Help/version flags short-circuit any subcommand. Without this,
    // `limpet index --help` falls through to the "index" arm and runs a full
    // index of the current directory (the flag is ignored by the hand-rolled
    // parser) instead of printing help.
    if cmd != "help" && args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{HELP}");
        return Ok(());
    }
    if cmd != "version" && args.iter().any(|a| a == "-V" || a == "--version") {
        println!("limpet {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    match cmd {
        "serve" => {
            let root = root_from(&args)?;
            mcp::serve(root)
        }
        "ui" => {
            let root = root_from(&args)?;
            let port: u16 = arg_value(&args, "--port")
                .map(|p| p.parse())
                .transpose()
                .context("--port must be a number")?
                .unwrap_or(9748);
            ui::serve_ui(&root, port)
        }
        "index" => {
            let root = root_from(&args)?;
            let store = store::Store::open(&store::Store::default_db_path(&root))?;
            store.version_guard()?;
            let report = index::full_index(&store, &root)?;
            let anchors = memory::anchor::resolve_all(&store)?;
            println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                "index": report, "anchors": anchors
            }))?);
            Ok(())
        }
        "status" | "export" | "import" => {
            let root = root_from(&args)?;
            let mut st = store::Store::open(&store::Store::default_db_path(&root))?;
            let op = match cmd { "status" => "status", "export" => "export", _ => "import" };
            let out = tools::dispatch(
                &mut st,
                &root,
                "admin",
                &serde_json::json!({ "op": op }),
            )?;
            println!("{}", serde_json::to_string_pretty(&out)?);
            Ok(())
        }
        "doctor" => doctor(&args),
        "install" => install(args.iter().any(|a| a == "--dry-run")),
        "uninstall" => uninstall(),
        "stats" => {
            let root = root_from(&args)?;
            let store = store::Store::open(&store::Store::default_db_path(&root))?;
            store.version_guard()?;
            println!("{}", serde_json::to_string_pretty(&tools::ledger_payload(&store))?);
            Ok(())
        }
        "update" => {
            let check_only = args.iter().any(|a| a == "--check");
            update::run(check_only)?;
            if !check_only {
                // Post-update sanity: catches a stale registration pointing
                // at a moved binary and similar drift. The running process
                // is still the OLD image; store/registration checks are
                // image-independent.
                println!("\n-- post-update doctor --");
                let _ = doctor_run(&args);
            }
            Ok(())
        }
        "help" | "--help" | "-h" => {
            println!("{}", HELP);
            Ok(())
        }
        "version" | "--version" | "-V" => {
            println!("limpet {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        other => bail!("unknown command '{other}' (try: limpet help)"),
    }
}

const HELP: &str = "limpet - memory that clamps onto your code

USAGE:
  limpet serve   [--root <path>]   run the MCP stdio server
  limpet ui      [--root <path>] [--port <n>]   visual memory at 127.0.0.1:9748
  limpet index   [--root <path>]   full index of the repository
  limpet status  [--root <path>]   index and memory counts
  limpet stats   [--root <path>]   token-savings ledger (session + lifetime)
  limpet doctor  [--root <path>]   diagnose install/registration/store issues
  limpet export  [--root <path>]   write memory to .limpet/memory.jsonl
  limpet import  [--root <path>]   read memory from .limpet/memory.jsonl
  limpet install [--dry-run]       register with Claude Code (user scope)
  limpet uninstall                 remove the registration
  limpet update  [--check]         update to the latest release binary

Indexing and memory stay fully offline. `limpet update` is the only command
that reaches the network, and only when you run it.";

/// Claude Code user-scope MCP registration: ~/.claude.json `mcpServers`.
/// Claude Code uses the same dotfile location on every platform, with
/// USERPROFILE standing in for HOME on Windows.
fn claude_config_path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .context("neither HOME nor USERPROFILE is set")?;
    Ok(PathBuf::from(home).join(".claude.json"))
}

const SKILL_MD: &str = include_str!("skill.md");

/// The /limpet skill for Claude Code: ~/.claude/skills/limpet/SKILL.md.
fn skill_path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .context("neither HOME nor USERPROFILE is set")?;
    Ok(PathBuf::from(home).join(".claude/skills/limpet/SKILL.md"))
}

/// Diagnose a broken or half-installed setup: the "it works on my other
/// laptop but not here" command. Read-only except for opening the store.
fn doctor(args: &[String]) -> Result<()> {
    if doctor_run(args)? {
        Ok(())
    } else {
        std::process::exit(1);
    }
}

/// The checks themselves; returns overall health so install/update can run
/// them automatically without turning a warning into a hard exit.
fn doctor_run(args: &[String]) -> Result<bool> {
    let mut ok = true;
    let mut check = |name: &str, pass: bool, detail: String| {
        println!("{} {name}: {detail}", if pass { "ok  " } else { "FAIL" });
        if !pass {
            ok = false;
        }
    };

    // 1. The running binary.
    let exe = std::env::current_exe().unwrap_or_default();
    check(
        "binary",
        true,
        format!("limpet {} at {}", env!("CARGO_PKG_VERSION"), exe.display()),
    );

    // 2. Claude Code MCP registration.
    match claude_config_path() {
        Ok(cfg_path) => match std::fs::read_to_string(&cfg_path) {
            Ok(s) => {
                let cfg: serde_json::Value =
                    serde_json::from_str(&s).unwrap_or(serde_json::Value::Null);
                match cfg.pointer("/mcpServers/limpet/command").and_then(|v| v.as_str()) {
                    Some(cmd) => {
                        let exists = std::path::Path::new(cmd).exists();
                        check(
                            "mcp registration",
                            exists,
                            if exists {
                                format!("{} -> {cmd}", cfg_path.display())
                            } else {
                                format!(
                                    "registered command does not exist: {cmd} \
                                     (moved binary? run `limpet install` again)"
                                )
                            },
                        );
                        if exists && std::path::Path::new(cmd) != exe.as_path() {
                            check(
                                "registration freshness",
                                false,
                                format!(
                                    "registered {cmd} is not this binary ({}); \
                                     an old copy may serve stale code. Run `limpet install`.",
                                    exe.display()
                                ),
                            );
                        }
                    }
                    None => check(
                        "mcp registration",
                        false,
                        format!(
                            "no mcpServers.limpet in {} — run `limpet install`, then \
                             restart Claude Code",
                            cfg_path.display()
                        ),
                    ),
                }
            }
            Err(_) => check(
                "mcp registration",
                false,
                format!("{} not found — run `limpet install`", cfg_path.display()),
            ),
        },
        Err(e) => check("mcp registration", false, format!("{e}")),
    }

    // 3. The /limpet skill file.
    match skill_path() {
        Ok(p) => check(
            "skill",
            p.exists(),
            if p.exists() {
                format!("{}", p.display())
            } else {
                format!("{} missing — run `limpet install`", p.display())
            },
        ),
        Err(e) => check("skill", false, format!("{e}")),
    }

    // 4. This repo's store.
    let root = root_from(args)?;
    let db = store::Store::default_db_path(&root);
    check("data dir", true, format!("{}", db.display()));
    match store::Store::open(&db) {
        Ok(store) => {
            match store.version_guard() {
                Ok(()) => {
                    let stamp = store
                        .kv_get("code_version")
                        .ok()
                        .flatten()
                        .unwrap_or_else(|| "unset".into());
                    check("store version", true, format!("stamped {stamp}"));
                }
                Err(e) => check("store version", false, format!("{e}")),
            }
            let files: i64 = store
                .conn
                .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
                .unwrap_or(0);
            let entries: i64 = store
                .conn
                .query_row("SELECT COUNT(*) FROM entries", [], |r| r.get(0))
                .unwrap_or(0);
            check(
                "index",
                files > 0,
                if files > 0 {
                    format!("{files} files, {entries} memories for {}", root.display())
                } else {
                    format!(
                        "empty index for {} — run `limpet index` here or `/limpet` \
                         in a session",
                        root.display()
                    )
                },
            );
        }
        Err(e) => check("store", false, format!("cannot open: {e}")),
    }

    if ok {
        println!("\nall checks passed. If tools still fail in Claude Code, restart it: \
                  running servers keep the old binary image until restart.");
    } else {
        println!("\nfix the FAIL lines above, restart Claude Code, then re-run `limpet doctor`.");
    }
    Ok(ok)
}

fn install(dry_run: bool) -> Result<()> {
    let exe = std::env::current_exe()?
        .canonicalize()
        .context("resolving own binary path")?;
    let cfg_path = claude_config_path()?;
    let mut cfg: serde_json::Value = match std::fs::read_to_string(&cfg_path) {
        Ok(s) => serde_json::from_str(&s).context("parsing existing ~/.claude.json")?,
        Err(_) => serde_json::json!({}),
    };
    if !cfg.is_object() {
        bail!("~/.claude.json is not a JSON object; refusing to touch it");
    }
    let entry = serde_json::json!({
        "command": exe.to_string_lossy(),
        "args": ["serve"]
    });
    let servers = cfg
        .as_object_mut()
        .expect("checked above")
        .entry("mcpServers")
        .or_insert_with(|| serde_json::json!({}));
    if !servers.is_object() {
        bail!("mcpServers in ~/.claude.json is not an object; refusing to touch it");
    }
    let before = servers.get("limpet").cloned();
    servers
        .as_object_mut()
        .expect("checked above")
        .insert("limpet".to_string(), entry.clone());

    let skill = skill_path()?;
    if dry_run {
        println!(
            "would write to {}:\n  limpet: {} -> {}\nwould write skill to {}",
            cfg_path.display(),
            before.map(|v| v.to_string()).unwrap_or_else(|| "(absent)".into()),
            entry,
            skill.display()
        );
        return Ok(());
    }
    std::fs::write(&cfg_path, serde_json::to_string_pretty(&cfg)?)
        .with_context(|| format!("writing {}", cfg_path.display()))?;
    if let Some(dir) = skill.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&skill, SKILL_MD).with_context(|| format!("writing {}", skill.display()))?;
    println!(
        "registered limpet with Claude Code ({})\ninstalled /limpet skill ({})\nRestart Claude Code, then type: /limpet",
        cfg_path.display(),
        skill.display()
    );
    // Verify the setup that was just written, so a half-broken install is
    // caught here instead of as a mystery on first use.
    println!("\n-- post-install doctor --");
    let _ = doctor_run(&[]);
    Ok(())
}

fn uninstall() -> Result<()> {
    let cfg_path = claude_config_path()?;
    let Ok(s) = std::fs::read_to_string(&cfg_path) else {
        println!("nothing to do: {} not found", cfg_path.display());
        return Ok(());
    };
    let mut cfg: serde_json::Value = serde_json::from_str(&s)?;
    let removed = cfg
        .get_mut("mcpServers")
        .and_then(|v| v.as_object_mut())
        .map(|m| m.remove("limpet").is_some())
        .unwrap_or(false);
    if removed {
        std::fs::write(&cfg_path, serde_json::to_string_pretty(&cfg)?)?;
        println!("removed limpet from {}", cfg_path.display());
    } else {
        println!("limpet was not registered in {}", cfg_path.display());
    }
    if let Ok(skill) = skill_path() {
        if skill.is_file() {
            let _ = std::fs::remove_file(&skill);
            if let Some(dir) = skill.parent() {
                let _ = std::fs::remove_dir(dir); // only removes if empty
            }
            println!("removed /limpet skill ({})", skill.display());
        }
    }
    println!("memory stores under ~/.local/share/limpet are untouched.");
    Ok(())
}
