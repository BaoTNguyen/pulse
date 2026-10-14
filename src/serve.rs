//! The server side of pulse's query API (P4): routes `/events`, `/spans`,
//! `/artifacts/<hash>` and `/health` to a `Backend`, over a Unix socket.

use std::fs::{self, Permissions};
use std::io::{self, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use crate::http::{self, HttpError};
use crate::query::{self, EventFilter};

/// What the routes read. Every error is a message for a 503.
#[allow(clippy::type_complexity)] // the artifact signature is the planned API
pub trait Backend {
    fn events(&mut self, f: &EventFilter) -> Result<(Vec<Value>, i64), String>;
    fn spans(&mut self, trace: &str) -> Result<Vec<Value>, String>;
    fn artifact(&mut self, hash: &str) -> Result<Option<(Vec<u8>, Option<String>)>, String>;
    fn health(&mut self) -> Result<Value, String>;
}

/// The Postgres backend. Connects on first use and again after any error.
pub struct PgBackend {
    conninfo: String,
    journal_dir: PathBuf,
    artifact_root: PathBuf,
    client: Option<postgres::Client>,
}

impl PgBackend {
    pub fn new(conninfo: String, journal_dir: PathBuf, artifact_root: PathBuf) -> PgBackend {
        PgBackend {
            conninfo,
            journal_dir,
            artifact_root,
            client: None,
        }
    }

    /// Runs `f` on the connection, connecting first if needed; drops the
    /// connection on error so the next call reconnects.
    fn with<T, E: ToString>(
        &mut self,
        f: impl FnOnce(&mut postgres::Client, &Path) -> Result<T, E>,
    ) -> Result<T, String> {
        if self.client.is_none() {
            self.client = Some(
                postgres::Client::connect(&self.conninfo, postgres::NoTls)
                    .map_err(|e| e.to_string())?,
            );
        }
        let r = f(
            self.client.as_mut().expect("connected above"),
            &self.journal_dir,
        );
        r.map_err(|e| {
            self.client = None;
            e.to_string()
        })
    }
}

impl Backend for PgBackend {
    fn events(&mut self, f: &EventFilter) -> Result<(Vec<Value>, i64), String> {
        self.with(|c, _| query::events(c, f))
    }

    fn spans(&mut self, trace: &str) -> Result<Vec<Value>, String> {
        self.with(|c, _| query::spans(c, trace))
    }

    fn artifact(&mut self, hash: &str) -> Result<Option<(Vec<u8>, Option<String>)>, String> {
        let Some((location, media_type)) = self.with(|c, _| query::artifact_location(c, hash))?
        else {
            return Ok(None);
        };
        let path = self.artifact_root.join(&location);
        let raw = fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let bytes = zstd::decode_all(&raw[..]).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Some((bytes, media_type)))
    }

    fn health(&mut self) -> Result<Value, String> {
        // ponytail: query::health folds spool read errors into the same
        // String, so those drop the connection too; reconnecting is cheap.
        self.with(query::health)
    }
}

const JSON: &str = "application/json";
const TEXT: &str = "text/plain";

fn json_body(v: &Value) -> Vec<u8> {
    serde_json::to_vec(v).expect("a Value always serializes")
}

/// ponytail: known media types only, the rest is octet-stream; returning an
/// owned content type is the upgrade if the list stops covering what's stored.
fn content_type(media_type: Option<&str>) -> &'static str {
    match media_type {
        Some("application/json") => "application/json",
        Some("application/x-ndjson") => "application/x-ndjson",
        Some("text/plain") => "text/plain",
        Some("text/x-diff") => "text/x-diff",
        Some("text/markdown") => "text/markdown",
        _ => "application/octet-stream",
    }
}

/// Answers one request: (status, content type, body).
pub fn handle(backend: &mut impl Backend, req: &http::Request) -> (u16, &'static str, Vec<u8>) {
    let r = match req.path.as_str() {
        "/events" => match EventFilter::from_query(&req.query) {
            Err(why) => return (400, TEXT, why.into_bytes()),
            Ok(f) => backend.events(&f).map(|(events, next)| {
                (
                    200,
                    JSON,
                    json_body(&json!({"events": events, "next": next})),
                )
            }),
        },
        "/spans" => {
            let Some((_, trace)) = req.query.iter().find(|(k, _)| k == "trace") else {
                return (400, TEXT, b"trace: required".to_vec());
            };
            backend
                .spans(trace)
                .map(|s| (200, JSON, json_body(&Value::from(s))))
        }
        "/health" => backend.health().map(|h| (200, JSON, json_body(&h))),
        path => match path.strip_prefix("/artifacts/") {
            Some(hash) if !query::is_hash(hash) => {
                return (
                    400,
                    TEXT,
                    format!("not an artifact hash: {hash:?}").into_bytes(),
                );
            }
            Some(hash) => backend.artifact(hash).map(|a| match a {
                None => (404, TEXT, b"no such artifact".to_vec()),
                Some((bytes, media_type)) => (200, content_type(media_type.as_deref()), bytes),
            }),
            None => return (404, TEXT, b"not found".to_vec()),
        },
    };
    // A database that is down is not the client's fault.
    r.unwrap_or_else(|why| (503, TEXT, why.into_bytes()))
}

/// Binds the API socket at `socket`, mode 0600. The caller holds the
/// collector lock, so any file already there is stale and is replaced.
pub fn bind(socket: &Path) -> io::Result<UnixListener> {
    if let Some(dir) = socket.parent() {
        fs::create_dir_all(dir)?;
    }
    match fs::remove_file(socket) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let listener = UnixListener::bind(socket)?;
    fs::set_permissions(socket, Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// Serves connections forever, one request per connection.
pub fn serve(listener: UnixListener, mut backend: impl Backend) {
    // ponytail: one request at a time is the ceiling; a thread per
    // connection is the upgrade if readers ever queue.
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let timeout = Some(Duration::from_secs(5));
        if stream.set_read_timeout(timeout).is_err() || stream.set_write_timeout(timeout).is_err() {
            continue;
        }
        let Ok(read_half) = stream.try_clone() else {
            continue;
        };
        let (status, ctype, body) = match http::read_request(&mut BufReader::new(read_half)) {
            Ok(req) => handle(&mut backend, &req),
            Err(e @ HttpError::Method(_)) => (405, TEXT, e.to_string().into_bytes()),
            Err(e @ HttpError::BadRequest(_)) => (400, TEXT, e.to_string().into_bytes()),
        };
        let _ = http::write_response(&mut stream, status, ctype, &body);
    }
}
