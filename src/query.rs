//! The read side of the pulse database (P4 of `PLAN.md`), behind the socket
//! API's `/events`, `/spans`, `/artifacts` and `/health`.

use crate::envelope::is_timestamp;

/// Which events a `GET /events` page asks for. `goal`, `episode`, `task` and
/// `trace` filter the events columns `goal_id`, `episode_id`, `task_id` and
/// `trace_id`; `project`, `source` and `kind` filter the columns of the same
/// name. `after` is the seq cursor and `since` a lower bound on time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventFilter {
    pub after: i64,
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

/// One page of events, `seq > f.after` in seq order, each rebuilt as the
/// envelope it was written as plus its `seq`. Returns the page and the
/// cursor for the next one: the last row's seq, or `f.after` if empty.
pub fn events(
    c: &mut postgres::Client,
    f: &EventFilter,
) -> Result<(Vec<serde_json::Value>, i64), postgres::Error> {
    use serde_json::{Map, Value};

    let mut params: Vec<&(dyn postgres::types::ToSql + Sync)> = vec![&f.after];
    let mut sql = format!(
        "SELECT seq, id::text, \
         to_char(ts AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"'), \
         schema_version, emitter_seq, duration_ms, payload, extra, {} \
         FROM events WHERE seq > $1",
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
    params.push(&f.limit);
    sql += &format!(" ORDER BY seq LIMIT ${}", params.len());

    let mut next = f.after;
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
            m.insert("duration_ms".into(), v.into());
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
