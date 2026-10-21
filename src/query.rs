//! The read side of the pulse database (P4 of `PLAN.md`), behind the socket
//! API's `/events`, `/spans`, `/artifacts` and `/health`.

use crate::envelope::is_timestamp;

/// The seq order of a `GET /events` page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Order {
    #[default]
    Asc,
    Desc,
}

/// Which events a `GET /events` page asks for. `goal`, `episode`, `task` and
/// `trace` filter the events columns `goal_id`, `episode_id`, `task_id` and
/// `trace_id`; `project`, `source` and `kind` filter the columns of the same
/// name. `after` is an exclusive lower bound on seq and `before`, when set,
/// an exclusive upper bound; `since` is a lower bound on time. `order` picks
/// the page direction: `Asc` pages forward with `after` as the cursor, `Desc`
/// pages backward (newest first) with `before` as the cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventFilter {
    pub after: i64,
    pub before: Option<i64>,
    pub order: Order,
    pub limit: i64,
    pub project: Option<String>,
    pub goal: Option<String>,
    pub episode: Option<String>,
    pub task: Option<String>,
    pub trace: Option<String>,
    pub source: Option<String>,
    pub kind: Option<String>,
    pub since: Option<String>,
}

impl Default for EventFilter {
    fn default() -> Self {
        EventFilter {
            after: 0,
            before: None,
            order: Order::Asc,
            limit: 500,
            project: None,
            goal: None,
            episode: None,
            task: None,
            trace: None,
            source: None,
            kind: None,
            since: None,
        }
    }
}

impl EventFilter {
    /// Builds a filter from the decoded query pairs of `GET /events?...`.
    /// Every error starts with the offending parameter's name and a colon.
    pub fn from_query(q: &[(String, String)]) -> Result<EventFilter, String> {
        let mut f = EventFilter::default();
        let mut seen: Vec<&str> = Vec::new();
        for (name, value) in q {
            if seen.contains(&name.as_str()) {
                return Err(format!("{name}: given more than once"));
            }
            seen.push(name);
            let text = match name.as_str() {
                "after" => {
                    f.after = value
                        .parse()
                        .ok()
                        .filter(|n| *n >= 0)
                        .ok_or("after: must be a non-negative integer")?;
                    continue;
                }
                "before" => {
                    f.before = Some(
                        value
                            .parse()
                            .ok()
                            .filter(|n| *n >= 0)
                            .ok_or("before: must be a non-negative integer")?,
                    );
                    continue;
                }
                "order" => {
                    f.order = match value.as_str() {
                        "asc" => Order::Asc,
                        "desc" => Order::Desc,
                        _ => return Err("order: must be asc or desc".into()),
                    };
                    continue;
                }
                "limit" => {
                    f.limit = value
                        .parse()
                        .ok()
                        .filter(|n| (1..=5000).contains(n))
                        .ok_or("limit: must be between 1 and 5000")?;
                    continue;
                }
                "since" => {
                    if !is_timestamp(value) {
                        return Err("since: must be an RFC 3339 timestamp".into());
                    }
                    f.since = Some(value.clone());
                    continue;
                }
                "project" => &mut f.project,
                "goal" => &mut f.goal,
                "episode" => &mut f.episode,
                "task" => &mut f.task,
                "trace" => &mut f.trace,
                "source" => &mut f.source,
                "kind" => &mut f.kind,
                _ => return Err(format!("{name}: unknown parameter")),
            };
            if value.is_empty() {
                return Err(format!("{name}: must not be empty"));
            }
            *text = Some(value.clone());
        }
        Ok(f)
    }
}

/// True iff `s` is a content hash as artifacts are keyed: exactly 64
/// lowercase hex characters.
pub fn is_hash(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
}

/// SQL rendering timestamptz `col` as an RFC 3339 UTC timestamp with
/// microseconds, e.g. `2026-10-13T08:00:00.123456Z`.
fn utc(col: &str) -> String {
    format!("to_char({col} AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')")
}

/// Text columns that go back into the envelope under their own name.
const TEXT_COLUMNS: [&str; 15] = [
    "source",
    "kind",
    "project",
    "trace_id",
    "span_id",
    "parent_span_id",
    "context_id",
    "goal_id",
    "feature_id",
    "task_id",
    "episode_id",
    "run_id",
    "turn_id",
    "role",
    "origin",
];

/// One page of events with `f.after < seq < f.before` (no upper bound when
/// `before` is None), each rebuilt as the envelope it was written as plus its
/// `seq`. Returns the page and the cursor for the next one. `Asc` pages in
/// seq order and the cursor is the last row's seq, or `f.after` if empty: pass
/// it back as `after`. `Desc` pages newest first and the cursor is the last
/// (smallest) row's seq, or `f.before` (0 if None) if empty: pass it back as
/// `before`.
pub fn events(
    c: &mut postgres::Client,
    f: &EventFilter,
) -> Result<(Vec<serde_json::Value>, i64), postgres::Error> {
    use serde_json::{Map, Value};

    let mut params: Vec<&(dyn postgres::types::ToSql + Sync)> = vec![&f.after];
    let mut sql = format!(
        "SELECT seq, id::text, \
         {}, schema_version, emitter_seq, duration_ms, payload, extra, {} \
         FROM events WHERE seq > $1",
        utc("ts"),
        TEXT_COLUMNS.join(", ")
    );
    let filters = [
        ("project", &f.project),
        ("goal_id", &f.goal),
        ("episode_id", &f.episode),
        ("task_id", &f.task),
        ("trace_id", &f.trace),
        ("source", &f.source),
        ("kind", &f.kind),
    ];
    for (col, value) in filters {
        if let Some(v) = value {
            params.push(v);
            sql += &format!(" AND {col} = ${}", params.len());
        }
    }
    if let Some(since) = &f.since {
        params.push(since);
        sql += &format!(" AND ts >= ${}::text::timestamptz", params.len());
    }
    if let Some(before) = &f.before {
        params.push(before);
        sql += &format!(" AND seq < ${}", params.len());
    }
    params.push(&f.limit);
    let dir = match f.order {
        Order::Asc => "",
        Order::Desc => " DESC",
    };
    sql += &format!(" ORDER BY seq{dir} LIMIT ${}", params.len());

    let mut next = match f.order {
        Order::Asc => f.after,
        Order::Desc => f.before.unwrap_or(0),
    };
    let mut out = Vec::new();
    for row in c.query(&sql, &params)? {
        let mut m = Map::new();
        m.insert("id".into(), Value::String(row.get(1)));
        m.insert("ts".into(), Value::String(row.get(2)));
        if let Some(v) = row.get::<_, Option<i32>>(3) {
            m.insert("schema_version".into(), v.into());
        }
        if let Some(v) = row.get::<_, Option<i64>>(4) {
            m.insert("emitter_seq".into(), v.into());
        }
        if let Some(v) = row.get::<_, Option<f64>>(5) {
            // the column is float8, but emitters write whole milliseconds as
            // integers; hand those back as written, not as 12.0
            let v: Value = if v.fract() == 0.0 && v.abs() < 9e15 {
                (v as i64).into()
            } else {
                v.into()
            };
            m.insert("duration_ms".into(), v);
        }
        if let Some(v) = row.get::<_, Option<Value>>(6) {
            m.insert("payload".into(), v);
        }
        for (i, col) in TEXT_COLUMNS.iter().enumerate() {
            if let Some(v) = row.get::<_, Option<String>>(8 + i) {
                m.insert((*col).into(), Value::String(v));
            }
        }
        if let Some(Value::Object(extra)) = row.get::<_, Option<Value>>(7) {
            for (k, v) in extra {
                m.entry(k).or_insert(v);
            }
        }
        next = row.get(0);
        m.insert("seq".into(), next.into());
        out.push(Value::Object(m));
    }
    Ok((out, next))
}

/// The spans of trace `trace` in start order. `finished` and `duration_ms`
/// are null while a span is unfinished.
pub fn spans(
    c: &mut postgres::Client,
    trace: &str,
) -> Result<Vec<serde_json::Value>, postgres::Error> {
    let sql = format!(
        "SELECT name, span_id, parent_span_id, source, project, {}, {}, duration_ms::float8 \
         FROM spans WHERE trace_id = $1 ORDER BY started",
        utc("started"),
        utc("finished")
    );
    Ok(c.query(&sql, &[&trace])?
        .iter()
        .map(|r| {
            serde_json::json!({
                "name": r.get::<_, Option<String>>(0),
                "span_id": r.get::<_, Option<String>>(1),
                "parent_span_id": r.get::<_, Option<String>>(2),
                "source": r.get::<_, Option<String>>(3),
                "project": r.get::<_, Option<String>>(4),
                "started": r.get::<_, Option<String>>(5),
                "finished": r.get::<_, Option<String>>(6),
                "duration_ms": r.get::<_, Option<f64>>(7),
            })
        })
        .collect())
}

/// Where artifact `hash` is stored and its media type, or None if it isn't.
/// Callers check `is_hash` first.
pub fn artifact_location(
    c: &mut postgres::Client,
    hash: &str,
) -> Result<Option<(String, Option<String>)>, postgres::Error> {
    Ok(c.query_opt(
        "SELECT location, media_type FROM artifacts WHERE hash = $1",
        &[&hash],
    )?
    .map(|r| (r.get(0), r.get(1))))
}

/// `(lag_bytes, spool_bytes)` over the day files in `journal_dir`: their
/// bytes not yet read past `checkpoints`, and their total size. A missing
/// directory is empty.
pub fn spool_bytes(
    journal_dir: &std::path::Path,
    checkpoints: &std::collections::HashMap<String, u64>,
) -> std::io::Result<(u64, u64)> {
    let entries = match std::fs::read_dir(journal_dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((0, 0)),
        r => r?,
    };
    let (mut lag, mut total) = (0, 0);
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str().filter(|n| crate::spool::is_day_file(n)) else {
            continue;
        };
        if !entry.file_type()?.is_file() {
            continue;
        }
        let size = entry.metadata()?.len();
        total += size;
        lag += size.saturating_sub(checkpoints.get(name).copied().unwrap_or(0));
    }
    Ok((lag, total))
}

/// The `/health` body: spool lag and size, quarantine and event counts, and
/// when the last event was ingested (null if none was).
pub fn health(
    c: &mut postgres::Client,
    journal_dir: &std::path::Path,
) -> Result<serde_json::Value, String> {
    let e = |e: postgres::Error| e.to_string();
    let mut checkpoints = std::collections::HashMap::new();
    for r in c
        .query("SELECT spool_file, byte_offset FROM checkpoints", &[])
        .map_err(e)?
    {
        let (file, offset): (String, i64) = (r.get(0), r.get(1));
        let offset = u64::try_from(offset)
            .map_err(|_| format!("checkpoint {file}: negative byte_offset {offset}"))?;
        checkpoints.insert(file, offset);
    }
    let (lag, spool) = spool_bytes(journal_dir, &checkpoints).map_err(|e| e.to_string())?;
    let r = c
        .query_one(
            &format!(
                "SELECT (SELECT count(*) FROM quarantine), count(*), {} FROM events",
                utc("max(ingested_at)")
            ),
            &[],
        )
        .map_err(e)?;
    Ok(serde_json::json!({
        "lag_bytes": lag,
        "spool_bytes": spool,
        "quarantine": r.get::<_, i64>(0),
        "events": r.get::<_, i64>(1),
        "last_ingested_at": r.get::<_, Option<String>>(2),
    }))
}
