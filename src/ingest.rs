//! P3: one spool line to one database row. `parse_line` is the only
//! validator; this module only maps what it accepted onto the `events` and
//! `contexts` columns, or what it rejected onto `quarantine`.

use crate::envelope::parse_line;
use serde_json::{Map, Value};
use uuid::Uuid;

/// Namespace of the uuid5 ids given to legacy lines without an `id`.
/// It must never change: those ids are already stored, and re-reading a
/// line has to produce the same one.
pub const PULSE_LEGACY_NAMESPACE: Uuid = Uuid::from_u128(0xf86a0c1e_5dbc_419a_b0f3_7a65ec751e7e);

/// Every top-level field with its own column; the rest go to `extra`.
const ENVELOPE_FIELDS: &[&str] = &[
    "ts",
    "source",
    "kind",
    "id",
    "schema_version",
    "project",
    "trace_id",
    "span_id",
    "parent_span_id",
    "context_id",
    "emitter_seq",
    "goal_id",
    "feature_id",
    "task_id",
    "episode_id",
    "run_id",
    "turn_id",
    "role",
    "duration_ms",
    "payload",
    "origin",
];

/// Bytes of a bad line kept in `quarantine.raw`.
const RAW_MAX: usize = 4096;

/// An `events` row, minus `seq` and `ingested_at`, plus the `contexts` row a
/// `context.started` line also yields.
#[derive(Debug, Clone, PartialEq)]
pub struct EventRow {
    pub id: Uuid,
    pub ts: String,
    pub schema_version: Option<i32>,
    pub source: String,
    pub kind: String,
    pub project: Option<String>,
    pub trace_id: Option<String>,
    pub span_id: Option<String>,
    pub parent_span_id: Option<String>,
    pub context_id: Option<String>,
    pub emitter_seq: Option<i64>,
    pub goal_id: Option<String>,
    pub feature_id: Option<String>,
    pub task_id: Option<String>,
    pub episode_id: Option<String>,
    pub run_id: Option<String>,
    pub turn_id: Option<String>,
    pub role: Option<String>,
    pub duration_ms: Option<f64>,
    pub origin: Option<String>,
    pub payload: Option<Value>,
    pub extra: Option<Value>,
    pub spool_file: String,
    pub spool_offset: i64,
    pub context: Option<ContextRow>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ContextRow {
    pub id: String,
    pub first_seen: String,
    pub component: Option<String>,
    pub git_commit: Option<String>,
    pub host: Option<String>,
    pub sandbox: Option<String>,
    pub lane: Option<String>,
    pub experiment_id: Option<String>,
    pub variant: Option<String>,
    pub payload: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QuarantineRow {
    pub spool_file: String,
    pub spool_offset: i64,
    pub error: String,
    pub raw: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq)]
// ponytail: unboxed by design, one line in flight at a time; box EventRow if rows get batched by value.
#[allow(clippy::large_enum_variant)]
pub enum Ingested {
    Event(EventRow),
    Quarantine(QuarantineRow),
}

fn str_of(m: &Map<String, Value>, k: &str) -> Option<String> {
    m.get(k).and_then(Value::as_str).map(str::to_owned)
}

fn int_of(m: &Map<String, Value>, k: &str) -> Option<i64> {
    let v = m.get(k)?;
    v.as_i64()
        .or_else(|| v.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i64))
}

/// Map one line, read at byte `offset` of day file `file`, to its row.
pub fn ingest(file: &str, offset: u64, bytes: &[u8]) -> Ingested {
    let quarantine = |error: &str| {
        Ingested::Quarantine(QuarantineRow {
            spool_file: file.to_string(),
            spool_offset: offset as i64,
            error: error.to_string(),
            raw: bytes[..bytes.len().min(RAW_MAX)].to_vec(),
        })
    };
    let Ok(line) = std::str::from_utf8(bytes) else {
        return quarantine("not_utf8");
    };
    let event = match parse_line(line) {
        Ok(event) => event,
        Err(e) => return quarantine(e.code()),
    };
    let f = &event.fields;
    let s = |k| str_of(f, k);
    let id = s("id")
        .and_then(|id| Uuid::parse_str(&id).ok())
        .unwrap_or_else(|| {
            Uuid::new_v5(
                &PULSE_LEGACY_NAMESPACE,
                format!("{file}:{offset}").as_bytes(),
            )
        });
    let payload = f.get("payload").filter(|v| !v.is_null()).cloned();
    let extra: Map<String, Value> = f
        .iter()
        .filter(|(k, _)| !ENVELOPE_FIELDS.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let context_id = s("context_id");
    let context = context_id
        .clone()
        .filter(|_| event.kind == "context.started")
        .map(|id| {
            let p = payload
                .clone()
                .filter(Value::is_object)
                .unwrap_or_else(|| serde_json::json!({}));
            let ps = |k: &str| p.get(k).and_then(Value::as_str).map(str::to_owned);
            ContextRow {
                id,
                first_seen: event.ts.clone(),
                component: ps("component"),
                git_commit: ps("git_commit"),
                host: ps("host"),
                sandbox: ps("sandbox"),
                lane: ps("lane"),
                experiment_id: ps("experiment_id"),
                variant: ps("variant"),
                payload: p,
            }
        });
    Ingested::Event(EventRow {
        id,
        schema_version: int_of(f, "schema_version").and_then(|n| i32::try_from(n).ok()),
        project: s("project"),
        trace_id: s("trace_id"),
        span_id: s("span_id"),
        parent_span_id: s("parent_span_id"),
        context_id,
        emitter_seq: int_of(f, "emitter_seq"),
        goal_id: s("goal_id"),
        feature_id: s("feature_id"),
        task_id: s("task_id"),
        episode_id: s("episode_id"),
        run_id: s("run_id"),
        turn_id: s("turn_id"),
        role: s("role"),
        duration_ms: f.get("duration_ms").and_then(Value::as_f64),
        origin: s("origin"),
        payload,
        extra: (!extra.is_empty()).then_some(Value::Object(extra)),
        spool_file: file.to_string(),
        spool_offset: offset as i64,
        context,
        ts: event.ts,
        source: event.source,
        kind: event.kind,
    })
}
