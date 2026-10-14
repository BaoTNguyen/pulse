use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use pulse::http::{self, Request};
use pulse::query::EventFilter;
use pulse::serve::{Backend, bind, handle, serve};
use serde_json::{Value, json};

const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// Canned data, or every call fails with "database down".
#[derive(Default)]
struct Fake {
    down: bool,
    /// Artifact at `HASH`: its bytes and media type.
    artifact: Option<(Vec<u8>, Option<String>)>,
}

impl Fake {
    fn up_or_down(&self) -> Result<(), String> {
        if self.down {
            Err("database down".into())
        } else {
            Ok(())
        }
    }
}

impl Backend for Fake {
    fn events(&mut self, _: &EventFilter) -> Result<(Vec<Value>, i64), String> {
        self.up_or_down()?;
        Ok((vec![json!({"seq": 7, "kind": "a.b"})], 7))
    }
    fn spans(&mut self, trace: &str) -> Result<Vec<Value>, String> {
        self.up_or_down()?;
        Ok(vec![json!({"name": "s", "trace": trace})])
    }
    fn artifact(&mut self, hash: &str) -> Result<Option<(Vec<u8>, Option<String>)>, String> {
        self.up_or_down()?;
        Ok(self.artifact.clone().filter(|_| hash == HASH))
    }
    fn health(&mut self) -> Result<Value, String> {
        self.up_or_down()?;
        Ok(json!({"lag_bytes": 0, "events": 1}))
    }
}

fn down() -> Fake {
    Fake {
        down: true,
        ..Fake::default()
    }
}

fn with_artifact(media_type: Option<&str>) -> Fake {
    Fake {
        down: false,
        artifact: Some((b"hello\x00bytes".to_vec(), media_type.map(String::from))),
    }
}

fn req(path: &str, query: &[(&str, &str)]) -> Request {
    Request {
        path: path.into(),
        query: query
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    }
}

fn call(b: &mut Fake, path: &str, query: &[(&str, &str)]) -> (u16, &'static str, Vec<u8>) {
    handle(b, &req(path, query))
}

fn json_of(body: &[u8]) -> Value {
    serde_json::from_slice(body).unwrap()
}

#[test]
fn events_route() {
    let (s, ct, body) = call(&mut Fake::default(), "/events", &[("kind", "a.b")]);
    assert_eq!((s, ct), (200, "application/json"));
    let v = json_of(&body);
    assert_eq!(v["events"], json!([{"seq": 7, "kind": "a.b"}]));
    assert_eq!(v["next"], 7);

    let (s, ct, body) = call(&mut Fake::default(), "/events", &[("limit", "abc")]);
    assert_eq!((s, ct), (400, "text/plain"));
    assert!(String::from_utf8(body).unwrap().contains("limit"));

    let (s, ct, body) = call(&mut down(), "/events", &[]);
    assert_eq!(
        (s, ct, body.as_slice()),
        (503, "text/plain", &b"database down"[..])
    );
}

#[test]
fn spans_route() {
    let (s, ct, body) = call(&mut Fake::default(), "/spans", &[("trace", "t")]);
    assert_eq!((s, ct), (200, "application/json"));
    assert_eq!(json_of(&body), json!([{"name": "s", "trace": "t"}]));

    let (s, ct, body) = call(&mut Fake::default(), "/spans", &[]);
    assert_eq!((s, ct), (400, "text/plain"));
    assert!(String::from_utf8(body).unwrap().contains("trace"));
}

#[test]
fn artifacts_route() {
    let path = format!("/artifacts/{HASH}");
    let (s, ct, body) = call(&mut with_artifact(Some("text/plain")), &path, &[]);
    assert_eq!(
        (s, ct, body.as_slice()),
        (200, "text/plain", &b"hello\x00bytes"[..])
    );
    for mt in [None, Some("image/x-weird")] {
        let (s, ct, _) = call(&mut with_artifact(mt), &path, &[]);
        assert_eq!((s, ct), (200, "application/octet-stream"), "{mt:?}");
    }

    let other = format!("/artifacts/{}", "f".repeat(64));
    assert_eq!(call(&mut with_artifact(None), &other, &[]).0, 404);
    assert_eq!(
        call(&mut with_artifact(None), "/artifacts/nothex", &[]).0,
        400
    );
    assert_eq!(call(&mut down(), &path, &[]).0, 503);
}

#[test]
fn health_and_unknown_routes() {
    let (s, ct, body) = call(&mut Fake::default(), "/health", &[]);
    assert_eq!((s, ct), (200, "application/json"));
    assert_eq!(json_of(&body)["events"], 1);

    let (s, ct, body) = call(&mut down(), "/health", &[]);
    assert_eq!(
        (s, ct, body.as_slice()),
        (503, "text/plain", &b"database down"[..])
    );

    assert_eq!(call(&mut Fake::default(), "/nope", &[]).0, 404);
}

/// A fresh scratch dir under the temp dir, short enough for socket paths.
fn scratch() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "psv-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn bind_replaces_stale_file_with_0600_socket() {
    let dir = scratch();
    let sock = dir.join("s");
    fs::write(&sock, "stale").unwrap();
    let _listener = bind(&sock).unwrap();
    let meta = fs::metadata(&sock).unwrap();
    assert!(meta.file_type().is_socket());
    assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn bind_creates_missing_parent() {
    let dir = scratch();
    let sock = dir.join("a/b/s");
    let _listener = bind(&sock).unwrap();
    assert!(fs::metadata(&sock).unwrap().file_type().is_socket());
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn round_trip_over_socket() {
    let dir = scratch();
    let sock = dir.join("s");
    let listener = bind(&sock).unwrap();
    thread::spawn(move || serve(listener, with_artifact(Some("text/plain"))));

    let (s, body) = http::get(&sock, "/health").unwrap();
    assert_eq!(s, 200);
    assert_eq!(json_of(&body)["lag_bytes"], 0);
    assert_eq!(http::get(&sock, "/events?limit=0").unwrap().0, 400);
    let (s, body) = http::get(&sock, &format!("/artifacts/{HASH}")).unwrap();
    assert_eq!((s, body.as_slice()), (200, &b"hello\x00bytes"[..]));

    // A non-GET gets a 405 and the loop keeps serving.
    let mut raw = UnixStream::connect(&sock).unwrap();
    raw.write_all(b"POST / HTTP/1.1\r\n\r\n").unwrap();
    let mut resp = String::new();
    raw.read_to_string(&mut resp).unwrap();
    assert!(resp.starts_with("HTTP/1.1 405 "), "{resp:?}");
    assert_eq!(http::get(&sock, "/health").unwrap().0, 200);

    // A client that connects and says nothing is timed out, not waited on.
    let _silent = UnixStream::connect(&sock).unwrap();
    let start = Instant::now();
    assert_eq!(http::get(&sock, "/health").unwrap().0, 200);
    assert!(start.elapsed() < Duration::from_secs(10));

    fs::remove_dir_all(&dir).unwrap();
}
