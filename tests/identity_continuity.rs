//! Identity continuity (spec v0.18): the real binary, a `git` that always
//! fails (exit 69, the macOS unaccepted-Xcode-licence shape), and a remote
//! host change. Proves the store key still comes from `.git/config`, that a
//! store fork is surfaced instead of silently starting over, that
//! `limpet adopt` merges a same-repository orphan losslessly, idempotently,
//! and without writing the old store, and that a store under a different
//! remote is named but merged only on request (`--from`, `--dismiss`).
#![cfg(unix)]

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use tempfile::TempDir;

/// A scratch world: a project root with a hand-written `.git/config`, a
/// private data dir, and a PATH whose only `git` exits 69.
struct World {
    root: TempDir,
    data: TempDir,
    fake_bin: TempDir,
}

impl World {
    fn new(origin: &str) -> World {
        let w = World {
            root: TempDir::new().unwrap(),
            data: TempDir::new().unwrap(),
            fake_bin: TempDir::new().unwrap(),
        };
        std::fs::create_dir_all(w.root.path().join(".git")).unwrap();
        // A `.git` dir without HEAD is not a repository to git, nor to the
        // config fallback.
        std::fs::write(w.root.path().join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        w.set_origin(origin);
        std::fs::write(w.root.path().join("lib.rs"), "pub fn widget() -> u32 { 7 }\n").unwrap();
        let git = w.fake_bin.path().join("git");
        std::fs::write(&git, "#!/bin/sh\necho 'xcode licence not accepted' >&2\nexit 69\n").unwrap();
        std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o755)).unwrap();
        w
    }

    fn set_origin(&self, origin: &str) {
        let remote = if origin.is_empty() {
            String::new()
        } else {
            format!("[remote \"origin\"]\n\turl = {origin}\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n")
        };
        std::fs::write(
            self.root.path().join(".git/config"),
            format!("[core]\n\trepositoryformatversion = 0\n{remote}"),
        )
        .unwrap();
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_limpet"));
        c.args(args)
            .arg("--root")
            .arg(self.root.path())
            .env("LIMPET_DATA_DIR", self.data.path())
            // Only the failing git is reachable; /bin keeps `sh` for it.
            .env("PATH", format!("{}:/bin", self.fake_bin.path().display()))
            .stdin(Stdio::null());
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd(args).output().expect("run limpet")
    }

    fn store_dirs(&self) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(self.data.path())
            .unwrap()
            .flatten()
            .filter(|e| e.path().join("store.db").is_file())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    /// Index, then remember `bodies` (the last one private) over MCP.
    fn seed(&self, bodies: &[&str]) {
        let out = self.run(&["index"]);
        assert!(out.status.success(), "index failed: {}", String::from_utf8_lossy(&out.stderr));
        let mut srv = Mcp::start(self);
        for (i, b) in bodies.iter().enumerate() {
            let private = i + 1 == bodies.len();
            let r = srv.call_tool(
                10 + i as u64,
                "remember",
                json!({ "kind": "insight", "body": b, "source": "explicit", "private": private,
                        "anchors": [{ "file": "lib.rs", "symbol": "widget" }] }),
            );
            assert!(r["data"]["id"].is_string(), "remember failed: {r}");
        }
    }
}

struct Mcp {
    child: Child,
    reader: BufReader<std::process::ChildStdout>,
}

impl Mcp {
    fn start(w: &World) -> Mcp {
        let mut child = w
            .cmd(&["serve"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn limpet serve");
        let stdout = child.stdout.take().unwrap();
        let mut m = Mcp { child, reader: BufReader::new(stdout) };
        m.request(json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "protocolVersion": "2024-11-05", "capabilities": {},
                        "clientInfo": { "name": "t", "version": "0" } } }));
        m.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        m
    }

    fn send(&mut self, v: &Value) {
        let stdin = self.child.stdin.as_mut().unwrap();
        writeln!(stdin, "{v}").unwrap();
        stdin.flush().unwrap();
    }

    fn request(&mut self, v: Value) -> Value {
        self.send(&v);
        let mut line = String::new();
        self.reader.read_line(&mut line).unwrap();
        serde_json::from_str(&line).expect("json response")
    }

    fn call_tool(&mut self, id: u64, name: &str, args: Value) -> Value {
        let resp = self.request(json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": { "name": name, "arguments": args } }));
        let text = resp["result"]["content"][0]["text"].as_str().unwrap_or_else(|| panic!("{resp}"));
        serde_json::from_str(text).unwrap()
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn json_out(o: &Output) -> Value {
    assert!(o.status.success(), "command failed: {}", String::from_utf8_lossy(&o.stderr));
    serde_json::from_str(&stdout(o)).unwrap_or_else(|e| panic!("not json ({e}): {}", stdout(o)))
}

fn listing(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut v: Vec<(String, Vec<u8>)> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            // -shm is SQLite's shared-memory coordination file: an ordinary
            // concurrent reader takes read marks in it. Its presence is
            // checked; its bytes are not. The db and wal must be identical.
            let bytes = if name.ends_with("-shm") { Vec::new() } else { std::fs::read(e.path()).unwrap() };
            (name, bytes)
        })
        .collect();
    v.sort();
    v
}

#[test]
fn failing_git_still_keys_the_store_by_remote() {
    let w = World::new("git@bitbucket.org:acme/site.git");
    w.seed(&["widget returns seven"]);
    let dirs = w.store_dirs();
    assert_eq!(dirs.len(), 1, "{dirs:?}");
    assert!(dirs[0].starts_with("git-"), "a failing git must not fall back to the path key: {dirs:?}");
}

#[test]
fn a_git_ownership_refusal_is_never_overridden_by_the_fallback() {
    // git ran and refused the repository as owned by someone else
    // (safe.directory, CVE-2022-24765). Reading .git/config behind its back
    // would let a planted repository pick the identity; the path key stands.
    let w = World::new("git@bitbucket.org:acme/site.git");
    let git = w.fake_bin.path().join("git");
    std::fs::write(
        &git,
        "#!/bin/sh\necho \"fatal: detected dubious ownership in repository at '$PWD'\" >&2\nexit 128\n",
    )
    .unwrap();
    let out = w.run(&["index"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let dirs = w.store_dirs();
    assert_eq!(dirs.len(), 1, "{dirs:?}");
    assert!(dirs[0].starts_with("path-"), "a refused repository must not be keyed by its config: {dirs:?}");
}

#[test]
fn provider_ssh_alias_keeps_the_same_store() {
    let w = World::new("git@bitbucket.org:acme/site.git");
    w.seed(&["widget returns seven"]);
    let before = w.store_dirs();
    w.set_origin("ssh://git@ssh.bitbucket.org:443/acme/site.git");
    let hook = w.run(&["hook"]);
    assert!(hook.status.success());
    assert!(stdout(&hook).contains("1 active"), "alias host must find the same store: {}", stdout(&hook));
    assert!(!stdout(&hook).contains("other store"));
    assert_eq!(w.store_dirs(), before);
}

#[test]
fn path_key_fork_is_surfaced_and_adopt_heals_it() {
    // A repo indexed before it had a remote (or while git was broken on a
    // 0.17 binary) keyed its store by path. Adding the remote re-keys it:
    // the old store is provably this repository and adopts by default.
    let w = World::new("");
    w.seed(&["widget returns seven", "widget is pure", "widget owner detail"]);
    let old_dirs = w.store_dirs();
    assert_eq!(old_dirs.len(), 1);
    assert!(old_dirs[0].starts_with("path-"), "{old_dirs:?}");
    let old_dir = w.data.path().join(&old_dirs[0]);
    w.set_origin("git@bitbucket.org:acme/site.git");

    // Hook: no store at the new key, yet the fork is announced.
    let hook = w.run(&["hook"]);
    assert!(hook.status.success());
    let text = stdout(&hook);
    assert!(text.contains("3 more memories") && text.contains("limpet adopt --root"), "{text}");
    assert!(!text.contains("--from"), "a same-repository fork needs no --from: {text}");
    assert_eq!(w.store_dirs(), old_dirs, "the hook must never create a store");

    // Dry run previews and still creates nothing.
    let dry = json_out(&w.run(&["adopt", "--dry-run"]));
    assert_eq!(dry["dry_run"], true);
    assert_eq!(dry["would_adopt"], 3);
    assert_eq!(dry["other_stores"][0]["dir"], old_dirs[0]);
    assert_eq!(dry["other_stores"][0]["same_repo"], true);
    assert_eq!(w.store_dirs(), old_dirs, "a dry run must not create a store");

    let before = listing(&old_dir);
    let adopted = json_out(&w.run(&["adopt"]));
    assert_eq!(adopted["adopted"][0]["added"], 3, "{adopted}");
    assert_eq!(adopted["adopted"][0]["rejected"], 0);
    assert_eq!(listing(&old_dir), before, "adopt must never write the old store");
    assert_eq!(w.store_dirs().len(), 2);

    // Doctor and hook go quiet; the private memory stayed private.
    let doctor = w.run(&["doctor"]);
    assert!(!stdout(&doctor).contains("warn store fork"), "{}", stdout(&doctor));
    let hook = stdout(&w.run(&["hook"]));
    assert!(!hook.contains("other store"), "{hook}");
    let mut srv = Mcp::start(&w);
    let status = srv.call_tool(2, "admin", json!({ "op": "status" }));
    assert_eq!(status["data"]["entries"], 3, "{status}");
    assert_eq!(status["data"]["private"], 1, "{status}");
    assert_eq!(status["data"]["other_stores"].as_array().map(Vec::len), Some(1), "{status}");
    assert_eq!(status["data"]["other_stores"][0]["unadopted"], 0, "{status}");
    assert_eq!(status["data"]["other_stores_truncated"], false, "{status}");
    drop(srv);

    // Re-running adopt is a no-op.
    let again = json_out(&w.run(&["adopt"]));
    assert_eq!(again["adopted"], json!([]), "{again}");
    assert_eq!(again["would_adopt"], 0);
}

#[test]
fn adopt_judges_fork_memories_against_the_current_code() {
    // The canonical store was last indexed on the OLD body (V1); the fork
    // then lived on while the code moved to V2 and wrote its memory against
    // V2. Adopt re-bases anchors onto the current store's index, so it must
    // refresh that index first: otherwise the memory inherits h(V1) and the
    // `limpet index` adopt recommends flags a correct memory stale.
    let w = World::new("git@bitbucket.org:acme/site.git");
    let out = w.run(&["index"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let canonical = w.store_dirs();
    assert_eq!(canonical.len(), 1);

    // The fork: git keyed by path (no remote readable), code at V2.
    w.set_origin("");
    std::fs::write(w.root.path().join("lib.rs"), "pub fn widget() -> u32 { 8 }\n").unwrap();
    w.seed(&["widget returns eight", "widget fork private detail"]);
    assert_eq!(w.store_dirs().len(), 2);

    w.set_origin("git@bitbucket.org:acme/site.git");
    let adopted = json_out(&w.run(&["adopt"]));
    assert_eq!(adopted["adopted"][0]["added"], 2, "{adopted}");
    let out = w.run(&["index"]);
    assert!(out.status.success());

    let mut srv = Mcp::start(&w);
    let r = srv.call_tool(3, "recall", json!({ "task": "widget returns eight" }));
    let hit = r["data"]
        .as_array()
        .and_then(|d| d.iter().find(|e| e["body"] == "widget returns eight"))
        .unwrap_or_else(|| panic!("adopted memory must recall: {r}"));
    assert!(hit.get("status").is_none() && hit.get("flags").is_none(), "an adopted memory that matches the current code is not stale: {r}");
    assert_eq!(r["meta"]["staleness"]["stale"], 0, "{r}");
}

#[test]
fn a_different_remote_is_named_and_only_merged_on_request() {
    let w = World::new("https://git.old-host.example/acme/site.git");
    w.seed(&["widget returns seven", "widget is pure", "widget owner detail"]);
    let old_dirs = w.store_dirs();
    let old_dir = w.data.path().join(&old_dirs[0]);
    // The remote moves hosts. limpet cannot tell this from a different
    // repository that once lived at this path, so it names the store.
    w.set_origin("https://git.new-host.example/acme/site.git");

    let text = stdout(&w.run(&["hook"]));
    assert!(text.contains("different remote") && text.contains(&old_dirs[0]), "{text}");
    assert!(text.contains("--from <dir>") && text.contains("--dismiss <dir>"), "{text}");

    let dry = json_out(&w.run(&["adopt", "--dry-run"]));
    assert_eq!(dry["would_adopt"], 0, "a different remote is never adopted by default: {dry}");
    assert_eq!(dry["unadopted_other_remotes"], 3);
    assert_eq!(dry["other_stores"][0]["same_repo"], false);
    assert_eq!(dry["other_stores"][0]["identity"], "git:git.old-host.example/acme/site");

    let none = json_out(&w.run(&["adopt"]));
    assert_eq!(none["adopted"], json!([]), "{none}");
    assert_eq!(w.store_dirs(), old_dirs, "nothing to do creates nothing");

    // A name the scan did not find is refused.
    let bad = w.run(&["adopt", "--from", "no-such-store-00000000"]);
    assert!(!bad.status.success());
    let both = w.run(&["adopt", "--from", &old_dirs[0], "--dismiss", &old_dirs[0]]);
    assert!(!both.status.success(), "--from and --dismiss together are refused");

    let dry = json_out(&w.run(&["adopt", "--from", &old_dirs[0], "--dry-run"]));
    assert_eq!(dry["would_adopt"], 3, "{dry}");
    let before = listing(&old_dir);
    let adopted = json_out(&w.run(&["adopt", "--from", &old_dirs[0]]));
    assert_eq!(adopted["adopted"][0]["added"], 3, "{adopted}");
    assert_eq!(listing(&old_dir), before, "adopt must never write the old store");
    let hook = stdout(&w.run(&["hook"]));
    assert!(!hook.contains("different remote"), "{hook}");
}

#[test]
fn dismiss_silences_a_different_remote_without_merging() {
    let w = World::new("https://github.com/clienta/site.git");
    w.seed(&["client a widget fact"]);
    let old_dirs = w.store_dirs();
    // A different repository now lives at this path.
    w.set_origin("https://github.com/clientb/site.git");
    let out = json_out(&w.run(&["adopt", "--dismiss", &old_dirs[0]]));
    assert_eq!(out["dismissed"][0], old_dirs[0], "{out}");
    let hook = stdout(&w.run(&["hook"]));
    assert!(!hook.contains("different remote"), "{hook}");
    let doctor = stdout(&w.run(&["doctor"]));
    assert!(!doctor.contains("warn store fork"), "{doctor}");
    let mut srv = Mcp::start(&w);
    let status = srv.call_tool(2, "admin", json!({ "op": "status" }));
    assert_eq!(status["data"]["entries"], 0, "dismiss merged nothing: {status}");
}

#[test]
fn doctor_warns_on_an_unadopted_fork() {
    let w = World::new("");
    w.seed(&["widget returns seven"]);
    w.set_origin("git@bitbucket.org:acme/site.git");
    let out = w.run(&["index"]);
    assert!(out.status.success());
    let doctor = w.run(&["doctor"]);
    let text = stdout(&doctor);
    assert!(text.contains("warn store fork: 1 memories"), "{text}");
    // Advisory only: the exit code here also reflects environment checks
    // (registration freshness), so assert the fork is never a FAIL line.
    assert!(!text.lines().any(|l| l.starts_with("FAIL") && l.contains("fork")), "{text}");
    assert!(text.contains("limpet adopt --root \""), "the advice names the root: {text}");
}
