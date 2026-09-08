//! The `limpet ui` HTTP surface as a BROWSER drives it. The route tests in
//! `tests/stability.rs` and `tests/ledger_session.rs` shut down the write
//! half of the socket after the request, which hands the server an EOF and
//! hid a real defect: a browser keeps the connection open, and from the
//! 2026-07 hardening through 0.16.1 every request then stalled for the full
//! 5 s read timeout because the header drain re-wrapped the shared reader
//! per line and swallowed the remaining header bytes. This file talks to the
//! server the way Chrome does and pins the response time.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// A spawned `limpet ui` that dies with the test, so an assert failure
/// never leaks an accept loop.
struct UiChild(Child);
impl Drop for UiChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Readiness probe with the write half shut down: fast on every server
/// version, so it cannot mask the latency the test below measures.
fn probe(port: u16) -> bool {
    let addr = format!("127.0.0.1:{port}").parse().unwrap();
    let Ok(mut sock) = TcpStream::connect_timeout(&addr, Duration::from_secs(2)) else {
        return false;
    };
    let _ = sock.set_read_timeout(Some(Duration::from_secs(15)));
    if write!(sock, "GET /api/projects HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").is_err() {
        return false;
    }
    let _ = sock.shutdown(Shutdown::Write);
    let mut raw = String::new();
    sock.read_to_string(&mut raw).is_ok() && raw.starts_with("HTTP/1.1 200")
}

fn spawn_ui(tmp: &tempfile::TempDir) -> (UiChild, u16) {
    let root = tmp.path().join("repo");
    std::fs::create_dir_all(&root).expect("mkdir repo");
    let data = tmp.path().join("data");
    std::fs::create_dir_all(&data).expect("mkdir data");
    for _ in 0..3 {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .expect("bind ephemeral")
            .local_addr()
            .expect("addr")
            .port();
        let mut child = UiChild(
            Command::new(env!("CARGO_BIN_EXE_limpet"))
                .args(["ui", "--root"])
                .arg(&root)
                .args(["--port", &port.to_string()])
                .env("LIMPET_DATA_DIR", &data)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn limpet ui"),
        );
        for _ in 0..50 {
            // A child that died (port stolen in the bind-drop-rebind window,
            // or a real crash) is retried on a fresh port at once instead of
            // being probed for five seconds, and a stranger on the port is
            // never mistaken for our server.
            if let Ok(Some(status)) = child.0.try_wait() {
                eprintln!("limpet ui exited early on port {port}: {status}");
                break;
            }
            if probe(port) {
                return (child, port);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    panic!("limpet ui never came up on three fresh ports");
}

/// A request shaped like Chrome's: several headers, keep-alive, and the
/// write half left OPEN. Returns the whole response and the wall time from
/// the last request byte to EOF.
fn browser_get(port: u16, path: &str) -> (String, Duration) {
    let addr = format!("127.0.0.1:{port}").parse().unwrap();
    let mut sock = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).expect("connect");
    sock.set_read_timeout(Some(Duration::from_secs(15))).expect("read timeout");
    write!(
        sock,
        "GET {path} HTTP/1.1\r\n\
         Host: 127.0.0.1\r\n\
         User-Agent: Mozilla/5.0 (Macintosh) AppleWebKit/537.36 Chrome/128.0 Safari/537.36\r\n\
         Accept: */*\r\n\
         Accept-Language: en-US,en;q=0.9\r\n\
         Accept-Encoding: gzip, deflate, br\r\n\
         Connection: keep-alive\r\n\
         Cache-Control: no-cache\r\n\
         \r\n"
    )
    .expect("write request");
    sock.flush().expect("flush");
    let started = Instant::now();
    let mut raw = String::new();
    sock.read_to_string(&mut raw).expect("read response to EOF");
    (raw, started.elapsed())
}

#[test]
fn a_browser_shaped_request_is_answered_without_waiting_for_the_read_timeout() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (_ui, port) = spawn_ui(&tmp);

    for path in ["/api/projects", "/", "/api/ledger", "/api/graph"] {
        let (raw, took) = browser_get(port, path);
        let (head, body) = raw.split_once("\r\n\r\n").expect("response has a header block");
        assert!(head.starts_with("HTTP/1.1 200"), "{path}: {head}");
        let declared: usize = head
            .lines()
            .find_map(|l| l.strip_prefix("Content-Length: "))
            .expect("Content-Length header")
            .trim()
            .parse()
            .expect("numeric Content-Length");
        assert_eq!(body.len(), declared, "{path}: body truncated or padded");
        assert!(
            head.contains("Connection: close"),
            "{path}: the server must close so an open client socket cannot hold the thread"
        );
        // The bar is generous (the real answer is milliseconds) so a slow CI
        // runner passes, and far below the 5 s read timeout the defect hit.
        assert!(
            took < Duration::from_secs(2),
            "{path}: answered in {took:?}; a keep-alive request must not wait out the read timeout"
        );
    }
}

#[test]
fn a_slow_drip_client_is_answered_at_the_deadline_not_per_byte() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (_ui, port) = spawn_ui(&tmp);
    let addr = format!("127.0.0.1:{port}").parse().unwrap();
    let mut sock = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).expect("connect");
    sock.set_read_timeout(Some(Duration::from_secs(20))).expect("read timeout");
    sock.write_all(b"GET /api/projects HTTP/1.1\r\n").expect("write request line");
    let started = Instant::now();
    // Header bytes arrive one every 250 ms for ten seconds: each lands
    // inside a per-recv timeout, so only a wall-clock deadline ends the
    // request. The drip runs on a clone so the read below can block.
    let mut drip = sock.try_clone().expect("clone socket");
    let dripper = std::thread::spawn(move || {
        for _ in 0..40 {
            if drip.write_all(b"X").is_err() {
                break; // the server closed: expected once it answered
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    });
    let mut raw = String::new();
    let _ = sock.read_to_string(&mut raw);
    let took = started.elapsed();
    let _ = dripper.join();
    assert!(
        raw.starts_with("HTTP/1.1 200"),
        "the request line was complete, so it must be routed: {}",
        raw.lines().next().unwrap_or("<empty response>")
    );
    assert!(
        took >= Duration::from_secs(4) && took < Duration::from_secs(8),
        "answered in {took:?}; the deadline is 5 s (a per-recv timeout would have waited out the whole 10 s drip)"
    );
}

/// The server's per-line byte cap (MAX_REQ_LINE in src/ui.rs).
const LINE_CAP: usize = 8 * 1024;

/// One raw request sent in a single write (one syscall, so the server can
/// never reset the socket under a later chunk), then the response to EOF.
fn raw_request(port: u16, req: &str) -> (String, Duration) {
    let addr = format!("127.0.0.1:{port}").parse().unwrap();
    let mut sock = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).expect("connect");
    sock.set_read_timeout(Some(Duration::from_secs(15))).expect("read timeout");
    sock.write_all(req.as_bytes()).expect("write request");
    sock.flush().expect("flush");
    let started = Instant::now();
    let mut raw = String::new();
    let _ = sock.read_to_string(&mut raw);
    (raw, started.elapsed())
}

#[test]
fn a_request_line_cut_by_the_cap_is_refused_with_414() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (_ui, port) = spawn_ui(&tmp);
    // Exactly the cap with no newline: the cut fires with every byte
    // consumed, so the server answers and closes cleanly on every OS
    // instead of resetting under unread bytes. Through 0.16.1 this line
    // was truncated and routed as `/api/projects`, answered 200.
    let mut req = String::from("GET /api/projects?project=");
    req.push_str(&"x".repeat(LINE_CAP - req.len()));
    assert_eq!(req.len(), LINE_CAP);
    let (raw, took) = raw_request(port, &req);
    assert!(
        raw.starts_with("HTTP/1.1 414"),
        "an over-cap request line must be refused with 414, got: {}",
        raw.lines().next().unwrap_or("<empty response>")
    );
    assert!(raw.contains("Connection: close"), "the refusal must close the socket");
    assert!(took < Duration::from_secs(8), "the refusal itself must not hang");
}

#[test]
fn a_request_line_that_ends_inside_the_cap_is_routed() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (_ui, port) = spawn_ui(&tmp);
    // Line bytes including the terminating "\n" total exactly the cap, so
    // the cap must NOT fire: this pins the >= boundary in the server.
    let mut req = String::from("GET /api/projects?project=");
    req.push_str(&"x".repeat(LINE_CAP - 1 - req.len()));
    req.push('\n');
    assert_eq!(req.len(), LINE_CAP);
    req.push_str("\r\n");
    let (raw, _) = raw_request(port, &req);
    assert!(
        raw.starts_with("HTTP/1.1 200"),
        "a line that ends inside the cap must reach its route, got: {}",
        raw.lines().next().unwrap_or("<empty response>")
    );
}
