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
        "install" => install(args.iter().any(|a| a == "--dry-run")),
        "uninstall" => uninstall(),
        "update" => update::run(args.iter().any(|a| a == "--check")),
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
