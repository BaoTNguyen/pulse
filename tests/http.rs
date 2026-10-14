use std::io::{BufReader, Cursor};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use pulse::http::*;

fn parse(raw: &[u8]) -> Result<Request, HttpError> {
    read_request(&mut Cursor::new(raw))
}

fn is_bad(r: Result<Request, HttpError>) -> bool {
    matches!(r, Err(HttpError::BadRequest(_)))
}

fn pairs(q: &[(&str, &str)]) -> Vec<(String, String)> {
    q.iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[test]
fn parses_path_and_query() {
    let req =
        parse(b"GET /events?kind=a.b&project=my%20proj&x=1+2 HTTP/1.1\r\nHost: pulse\r\n\r\n")
            .unwrap();
    assert_eq!(req.path, "/events");
    assert_eq!(
        req.query,
        pairs(&[("kind", "a.b"), ("project", "my proj"), ("x", "1 2")])
    );
}

#[test]
fn parses_bare_lf() {
    let req = parse(b"GET /health HTTP/1.1\nHost: x\n\n").unwrap();
    assert_eq!(req.path, "/health");
    assert!(req.query.is_empty());
}

#[test]
fn query_edge_cases() {
    let req = parse(b"GET /a+b%2Fc?&k&k=1&=v&e=x=y HTTP/1.1\r\n\r\n").unwrap();
    assert_eq!(req.path, "/a+b/c");
    assert_eq!(
        req.query,
        pairs(&[("k", ""), ("k", "1"), ("", "v"), ("e", "x=y")])
    );
}

#[test]
fn refuses_other_methods() {
    match parse(b"POST /events HTTP/1.1\r\n\r\n") {
        Err(HttpError::Method(m)) => assert_eq!(m, "POST"),
        other => panic!("expected Method, got {other:?}"),
    }
}

#[test]
fn refuses_oversized_head() {
    let mut raw = b"GET / HTTP/1.1\r\n".to_vec();
    raw.extend(vec![b'a'; 17000]);
    raw.extend(b"\r\n\r\n");
    assert!(is_bad(parse(&raw)));
}

#[test]
fn refuses_malformed_requests() {
    for raw in [
        &b"GET /events?kind=%zz HTTP/1.1\r\n\r\n"[..],
        b"GET /events?kind=%4 HTTP/1.1\r\n\r\n",
        b"GET /events?kind=% HTTP/1.1\r\n\r\n",
        b"GET /%FF HTTP/1.1\r\n\r\n",
        b"GET /health HTTP/1.1\r\nHost: x\r\n",
        b"GET /health HTTP/1.1\r\n",
        b"GET /health\r\n\r\n",
        b"GET  /health HTTP/1.1\r\n\r\n",
        b"GET health HTTP/1.1\r\n\r\n",
        b"GET /health HTTP/2\r\n\r\n",
        b"GET /health HTTP/1.1\r\nX: \xff\r\n\r\n",
        b"",
    ] {
        assert!(is_bad(parse(raw)), "{:?}", String::from_utf8_lossy(raw));
    }
}

#[test]
fn writes_exact_response() {
    let mut out = Vec::new();
    write_response(&mut out, 200, "application/json", b"{}").unwrap();
    assert_eq!(
        out,
        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}"
    );
    let mut out = Vec::new();
    write_response(&mut out, 404, "text/plain", b"").unwrap();
    assert!(out.starts_with(b"HTTP/1.1 404 Not Found\r\n"));
}

#[test]
fn encode_round_trips() {
    let s = "my proj/a&b=c+é";
    let enc = encode(s);
    assert_eq!(enc, "my%20proj%2Fa%26b%3Dc%2B%C3%A9");
    let req = parse(format!("GET /e?v={enc} HTTP/1.1\r\n\r\n").as_bytes()).unwrap();
    assert_eq!(req.query, pairs(&[("v", s)]));
}

fn sock_path() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let p = std::env::temp_dir().join(format!("pulse-http-{}-{n}.sock", std::process::id()));
    assert!(p.as_os_str().len() < 100);
    let _ = std::fs::remove_file(&p);
    p
}

#[test]
fn client_server_round_trip() {
    let sock = sock_path();
    let listener = UnixListener::bind(&sock).unwrap();
    let server = thread::spawn(move || {
        for _ in 0..2 {
            let (stream, _) = listener.accept().unwrap();
            let mut r = BufReader::new(stream);
            let req = read_request(&mut r).unwrap();
            let mut w = r.into_inner();
            if req.path == "/bin" {
                let mut body = vec![0u8, 127, 128, 200, 255];
                let x = req.query.iter().find(|(k, _)| k == "x").unwrap();
                body.extend(x.1.as_bytes());
                write_response(&mut w, 200, "application/octet-stream", &body).unwrap();
            } else {
                write_response(&mut w, 404, "text/plain", b"no such path").unwrap();
            }
        }
    });

    let (status, body) = get(&sock, &format!("/bin?x={}", encode("a b"))).unwrap();
    assert_eq!(status, 200);
    assert_eq!(body, b"\x00\x7f\x80\xc8\xffa b");

    let (status, body) = get(&sock, "/nope").unwrap();
    assert_eq!(status, 404);
    assert_eq!(body, b"no such path");

    server.join().unwrap();
    std::fs::remove_file(&sock).unwrap();
}

/// Serves one canned raw response and returns what `get` makes of it.
fn get_raw(raw: &'static [u8]) -> std::io::Result<(u16, Vec<u8>)> {
    use std::io::Write;
    let sock = sock_path();
    let listener = UnixListener::bind(&sock).unwrap();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut r = BufReader::new(stream);
        let _ = read_request(&mut r);
        let _ = r.get_mut().write_all(raw);
    });
    let got = get(&sock, "/");
    server.join().unwrap();
    std::fs::remove_file(&sock).unwrap();
    got
}

#[test]
fn client_header_handling() {
    let got = get_raw(b"HTTP/1.1 200 OK\r\nX: y\r\n\r\nto eof").unwrap();
    assert_eq!(got, (200, b"to eof".to_vec()));
    let got = get_raw(b"HTTP/1.0 503 Service Unavailable\ncontent-LENGTH:  3 \n\nabcdef").unwrap();
    assert_eq!(got, (503, b"abc".to_vec()));
}

#[test]
fn client_refuses_malformed_responses() {
    for raw in [
        &b"HTTP/1.1 200\r\n\r\n"[..],
        b"HTTP/1.1 200 \r\n\r\n",
        b"HTTP/1.1 2x0 OK\r\n\r\n",
        b"HTTP/2 200 OK\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nContent-Length: two\r\n\r\n",
    ] {
        let err = get_raw(raw).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData, "{raw:?}");
    }
}
