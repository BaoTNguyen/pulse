use std::fs;
use std::io::BufReader;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::mpsc::{self, Receiver};
use std::time::{SystemTime, UNIX_EPOCH};

use pulse::http::{self, Request};
use serde_json::{Value, json};

/// A fresh VASCULAR_HOME, short enough that the socket path stays under 100 bytes.
fn scratch(test: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let home = std::env::temp_dir().join(format!("pq-{test}-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&home).unwrap();
    home
}

/// Serves one request on the home's socket with a canned response.
fn serve_once(home: &Path, status: u16, body: &'static str) -> Receiver<Request> {
    let sock = home.join("state/pulse/pulse.sock");
    fs::create_dir_all(sock.parent().unwrap()).unwrap();
    let listener = UnixListener::bind(&sock).unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let req = http::read_request(&mut BufReader::new(&stream)).unwrap();
        tx.send(req).unwrap();
        http::write_response(&mut stream, status, "application/json", body.as_bytes()).unwrap();
    });
    rx
}

fn pulse(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pulse"))
        .args(args)
        .env("VASCULAR_HOME", home)
        .output()
        .unwrap()
}

#[test]
fn events_prints_lines_and_encodes_filters() {
    let home = scratch("ev");
    let rx = serve_once(
        &home,
        200,
        r#"{"events":[{"seq":1,"kind":"a"},{"seq":2,"kind":"b"}],"next":2}"#,
    );
    let out = pulse(
        &home,
        &[
            "events",
            "--after",
            "0",
            "--project",
            "p q&r",
            "--since",
            "2026-10-13T00:00:00Z",
        ],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let lines: Vec<Value> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(
        lines,
        [json!({"seq":1,"kind":"a"}), json!({"seq":2,"kind":"b"})]
    );
    let req = rx.recv().unwrap();
    assert_eq!(req.path, "/events");
    let want = [
        ("after", "0"),
        ("project", "p q&r"),
        ("since", "2026-10-13T00:00:00Z"),
    ]
    .map(|(k, v)| (k.to_string(), v.to_string()));
    assert_eq!(req.query, want);
    fs::remove_dir_all(&home).unwrap();
}

#[test]
fn events_400_exits_1() {
    let home = scratch("bad");
    let _rx = serve_once(&home, 400, "bad parameter: limit");
    let out = pulse(&home, &["events", "--limit", "x"]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("pulse events: 400"), "{err}");
    assert!(err.contains("bad parameter: limit"), "{err}");
    fs::remove_dir_all(&home).unwrap();
}

#[test]
fn health_prints_object() {
    let home = scratch("hl");
    let rx = serve_once(&home, 200, r#"{"lag_bytes":0,"events":3}"#);
    let out = pulse(&home, &["health"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let got: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(got, json!({"lag_bytes":0,"events":3}));
    assert_eq!(rx.recv().unwrap().path, "/health");
    fs::remove_dir_all(&home).unwrap();
}

#[test]
fn no_socket_not_running() {
    let home = scratch("ns");
    let out = pulse(&home, &["health"]);
    assert_eq!(out.status.code(), Some(1));
    let want = format!(
        "pulse: the collector isn't running (no socket at {})",
        home.join("state/pulse/pulse.sock").display()
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains(&want));
    fs::remove_dir_all(&home).unwrap();
}
