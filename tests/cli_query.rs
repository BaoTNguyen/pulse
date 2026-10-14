use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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

#[test]
fn tail_skips_backlog_and_prints_new() {
    let home = scratch("tl");
    let sock = home.join("state/pulse/pulse.sock");
    fs::create_dir_all(sock.parent().unwrap()).unwrap();
    let listener = UnixListener::bind(&sock).unwrap();
    let seen = Arc::new(Mutex::new(Vec::<Request>::new()));
    let log = Arc::clone(&seen);
    std::thread::spawn(move || {
        let mut empty_at_2 = false;
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let req = http::read_request(&mut BufReader::new(&stream)).unwrap();
            let after = req
                .query
                .iter()
                .find(|(k, _)| k == "after")
                .map(|(_, v)| v.clone());
            let body = match after.as_deref() {
                Some("0") => r#"{"events":[{"seq":1},{"seq":2}],"next":2}"#,
                Some("2") if !empty_at_2 => {
                    empty_at_2 = true;
                    r#"{"events":[],"next":2}"#
                }
                Some("2") => r#"{"events":[{"seq":3,"kind":"new"}],"next":3}"#,
                _ => r#"{"events":[],"next":3}"#,
            };
            log.lock().unwrap().push(req);
            http::write_response(&mut stream, 200, "application/json", body.as_bytes()).unwrap();
        }
    });

    let mut child = Command::new(env!("CARGO_BIN_EXE_pulse"))
        .args(["tail", "--kind", "x"])
        .env("VASCULAR_HOME", &home)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if tx.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    let first = rx.recv_timeout(Duration::from_secs(10));
    child.kill().unwrap();
    child.wait().unwrap();
    let first = first.expect("tail printed nothing within 10 s");
    assert_eq!(
        serde_json::from_str::<Value>(&first).unwrap(),
        json!({"seq":3,"kind":"new"})
    );
    for line in rx.try_iter() {
        let seq = serde_json::from_str::<Value>(&line).unwrap()["seq"].clone();
        assert!(
            seq != json!(1) && seq != json!(2),
            "backlog printed: {line}"
        );
    }
    let seen = seen.lock().unwrap();
    assert!(!seen.is_empty());
    for req in seen.iter() {
        assert!(
            req.query.contains(&("kind".to_string(), "x".to_string())),
            "{:?}",
            req.query
        );
    }
    fs::remove_dir_all(&home).unwrap();
}
