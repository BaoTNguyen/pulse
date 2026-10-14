//! The wire layer for pulse's query API: plain HTTP/1.1, GET only, one
//! request per connection, over a Unix socket. Server-side parsing and
//! response writing, plus the matching client `get`.

use std::fmt;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

/// Cap on the request line plus headers plus their terminators.
const MAX_HEAD: u64 = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// Target before any `?`, percent-decoded (`+` stays literal).
    pub path: String,
    /// Query pairs in order, percent-decoded with `+` as a space.
    pub query: Vec<(String, String)>,
}

#[derive(Debug)]
pub enum HttpError {
    Method(String),
    BadRequest(String),
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HttpError::Method(m) => write!(f, "method not allowed: {m}"),
            HttpError::BadRequest(why) => write!(f, "bad request: {why}"),
        }
    }
}

impl std::error::Error for HttpError {}

fn bad(why: impl Into<String>) -> HttpError {
    HttpError::BadRequest(why.into())
}

/// Reads one line within the remaining header budget, without its line ending.
fn read_head_line(r: &mut impl BufRead, budget: &mut u64) -> Result<String, HttpError> {
    let mut buf = Vec::new();
    let n = r
        .take(*budget)
        .read_until(b'\n', &mut buf)
        .map_err(|e| bad(format!("read failed: {e}")))?;
    *budget -= n as u64;
    if buf.pop() != Some(b'\n') {
        return Err(bad(if *budget == 0 {
            "header section exceeds 16 KiB"
        } else {
            "connection closed before end of headers"
        }));
    }
    if buf.last() == Some(&b'\r') {
        buf.pop();
    }
    String::from_utf8(buf).map_err(|_| bad("header is not UTF-8"))
}

pub fn read_request(r: &mut impl BufRead) -> Result<Request, HttpError> {
    let mut budget = MAX_HEAD;
    let line = read_head_line(r, &mut budget)?;
    while !read_head_line(r, &mut budget)?.is_empty() {}

    let parts: Vec<&str> = line.split(' ').collect();
    let [method, target, version] = parts[..] else {
        return Err(bad("malformed request line"));
    };
    if method.is_empty() || !version.starts_with("HTTP/1.") || !target.starts_with('/') {
        return Err(bad("malformed request line"));
    }
    if method != "GET" {
        return Err(HttpError::Method(method.to_string()));
    }

    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let query = query
        .split('&')
        .filter(|piece| !piece.is_empty())
        .map(|piece| {
            let (k, v) = piece.split_once('=').unwrap_or((piece, ""));
            Ok((decode(k, true)?, decode(v, true)?))
        })
        .collect::<Result<_, HttpError>>()?;
    Ok(Request {
        path: decode(path, false)?,
        query,
    })
}

fn decode(s: &str, plus_is_space: bool) -> Result<String, HttpError> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let byte = bytes
                    .get(i + 1..i + 3)
                    .filter(|hex| hex.iter().all(u8::is_ascii_hexdigit))
                    .and_then(|hex| u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok())
                    .ok_or_else(|| bad(format!("invalid percent-encoding in {s:?}")))?;
                out.push(byte);
                i += 3;
                continue;
            }
            b'+' if plus_is_space => out.push(b' '),
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8(out).map_err(|_| bad(format!("{s:?} does not decode to UTF-8")))
}

pub fn write_response(
    w: &mut impl Write,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Unknown",
    };
    write!(
        w,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    w.write_all(body)?;
    w.flush()
}

/// Percent-encodes everything but the unreserved bytes `A-Z a-z 0-9 - . _ ~`.
pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn invalid(why: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why)
}

/// Client side: one GET over the socket; returns (status, body).
pub fn get(socket: &Path, path_and_query: &str) -> io::Result<(u16, Vec<u8>)> {
    let mut stream = UnixStream::connect(socket)?;
    write!(
        stream,
        "GET {path_and_query} HTTP/1.1\r\nHost: pulse\r\nConnection: close\r\n\r\n"
    )?;
    stream.flush()?;
    let mut r = BufReader::new(stream);

    let mut line = String::new();
    r.read_line(&mut line)?;
    let status_line = line.trim_end_matches(['\r', '\n']);
    let mut parts = status_line.splitn(3, ' ');
    let status = match (parts.next(), parts.next(), parts.next()) {
        (Some(version), Some(code), Some(reason))
            if version.starts_with("HTTP/1.")
                && code.len() == 3
                && code.bytes().all(|b| b.is_ascii_digit())
                && !reason.is_empty() =>
        {
            code.parse::<u16>().expect("three ASCII digits")
        }
        _ => return Err(invalid(format!("malformed status line {status_line:?}"))),
    };

    let mut content_length = None;
    loop {
        line.clear();
        if r.read_line(&mut line)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "connection closed before end of headers",
            ));
        }
        let header = line.trim_end_matches(['\r', '\n']);
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            let n = value
                .trim()
                .parse::<usize>()
                .map_err(|_| invalid(format!("bad Content-Length {value:?}")))?;
            content_length = Some(n);
        }
    }

    let mut body = Vec::new();
    match content_length {
        Some(n) => {
            body.resize(n, 0);
            r.read_exact(&mut body)?;
        }
        None => {
            r.read_to_end(&mut body)?;
        }
    }
    Ok((status, body))
}
