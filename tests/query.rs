//! Query side of the pulse database. The Postgres tests in this file are
//! `#[ignore]`d and run with
//! `PULSE_TEST_DATABASE_URL='host=/var/run/postgresql dbname=pulse_test'
//! cargo test --test query -- --ignored`. Each test gets its own schema.

use postgres::{Client, NoTls};
use pulse::query::{EventFilter, artifact_location, events, health, is_hash, spans, spool_bytes};
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use uuid::Uuid;

fn pairs(q: &[(&str, &str)]) -> Vec<(String, String)> {
    q.iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[test]
fn empty_query_is_default() {
    let f = EventFilter::from_query(&[]).unwrap();
    assert_eq!(f, EventFilter::default());
    assert_eq!((f.after, f.limit), (0, 500));
}

#[test]
fn full_query_sets_every_field() {
    let f = EventFilter::from_query(&pairs(&[
        ("after", "10"),
        ("limit", "5000"),
        ("project", "p"),
        ("goal", "g"),
        ("episode", "e"),
        ("task", "t"),
        ("trace", "tr"),
        ("source", "s"),
        ("kind", "k"),
        ("since", "2026-10-13T08:00:00Z"),
    ]))
    .unwrap();
    let some = |s: &str| Some(s.to_string());
    assert_eq!(
        f,
        EventFilter {
            after: 10,
            limit: 5000,
            project: some("p"),
            goal: some("g"),
            episode: some("e"),
            task: some("t"),
            trace: some("tr"),
            source: some("s"),
            kind: some("k"),
            since: some("2026-10-13T08:00:00Z"),
        }
    );
    assert_eq!(
        EventFilter::from_query(&pairs(&[("limit", "1")]))
            .unwrap()
            .limit,
        1
    );
}

#[test]
fn bad_parameters_name_themselves() {
    let cases: &[(&[(&str, &str)], &str)] = &[
        (&[("after", "-1")], "after:"),
        (&[("after", "x")], "after:"),
        (&[("limit", "0")], "limit:"),
        (&[("limit", "5001")], "limit:"),
        (&[("limit", "abc")], "limit:"),
        (&[("since", "yesterday")], "since:"),
        (&[("since", "2026-10-13")], "since:"),
        (&[("project", "")], "project:"),
        (&[("limit", "10"), ("limit", "20")], "limit:"),
        (&[("foo", "1")], "foo:"),
    ];
    for (q, prefix) in cases {
        let err = EventFilter::from_query(&pairs(q)).unwrap_err();
        assert!(err.starts_with(prefix), "{q:?} gave {err:?}");
    }
    let err = |q: &[(&str, &str)]| EventFilter::from_query(&pairs(q)).unwrap_err();
    assert_eq!(
        err(&[("limit", "abc")]),
        "limit: must be between 1 and 5000"
    );
    assert_eq!(err(&[("foo", "1")]), "foo: unknown parameter");
    assert_eq!(
        err(&[("limit", "1"), ("limit", "2")]),
        "limit: given more than once"
    );
    assert_eq!(
        err(&[("after", "-1")]),
        "after: must be a non-negative integer"
    );
    assert_eq!(err(&[("project", "")]), "project: must not be empty");
}

#[test]
fn hashes_are_64_lowercase_hex() {
    let h = "0123456789abcdef".repeat(4);
    assert!(is_hash(&h));
    assert!(!is_hash(&h[..63]));
    assert!(!is_hash(&format!("{h}0")));
    assert!(!is_hash(&h.to_uppercase()));
    assert!(!is_hash(&format!("{}g", &h[..63])));
    assert!(!is_hash(""));
}

fn conninfo() -> String {
    std::env::var("PULSE_TEST_DATABASE_URL")
        .expect("PULSE_TEST_DATABASE_URL must be set to a Postgres conninfo to run this test")
}

/// Drops the test's schema on its own connection, even when the test panics.
struct SchemaGuard(String);

impl Drop for SchemaGuard {
    fn drop(&mut self) {
        if let Ok(mut c) = Client::connect(&conninfo(), NoTls) {
            let _ = c.batch_execute(&format!("DROP SCHEMA IF EXISTS {} CASCADE", self.0));
        }
    }
}

fn db(test: &str) -> (Client, SchemaGuard) {
    let schema = format!("pulse_test_q_{test}_{}", std::process::id());
    let mut c = Client::connect(&conninfo(), NoTls).expect("connect to Postgres");
    c.batch_execute(&format!(
        "CREATE SCHEMA {schema}; SET search_path TO {schema}"
    ))
    .expect("create test schema");
    let guard = SchemaGuard(schema);
    c.batch_execute(pulse::pg::SCHEMA).expect("apply schema");
    (c, guard)
}

/// Inserts event `n` (id `Uuid::from_u128(n)`) at `ts`, source `demo`, kind
/// `demo.tick`, with the text columns in `cols` overriding or adding to that.
fn insert(c: &mut Client, n: u128, ts: &str, cols: &[(&str, &str)]) {
    let mut names = vec!["source", "kind"];
    let mut values = vec!["demo", "demo.tick"];
    for (k, v) in cols {
        match names.iter().position(|n| n == k) {
            Some(i) => values[i] = v,
            None => {
                names.push(k);
                values.push(v);
            }
        }
    }
    let id = Uuid::from_u128(n).to_string();
    let offset = n as i64;
    let mut params: Vec<&(dyn postgres::types::ToSql + Sync)> = vec![&id, &ts, &offset];
    for v in &values {
        params.push(v);
    }
    let placeholders: Vec<String> = (4..=params.len()).map(|i| format!("${i}")).collect();
    c.execute(
        &format!(
            "INSERT INTO events (id, ts, spool_file, spool_offset, {}) \
             VALUES ($1::text::uuid, $2::text::timestamptz, 'f.ndjson', $3, {})",
            names.join(", "),
            placeholders.join(", ")
        ),
        &params,
    )
    .expect("insert event");
}

fn ids(page: &[serde_json::Value]) -> Vec<String> {
    page.iter()
        .map(|e| e["id"].as_str().unwrap().to_string())
        .collect()
}

fn uuid(n: u128) -> String {
    Uuid::from_u128(n).to_string()
}

#[test]
#[ignore = "needs Postgres: set PULSE_TEST_DATABASE_URL"]
fn pages_follow_next_without_gaps_or_repeats() {
    let (mut c, _g) = db("paging");
    for n in 1..=7 {
        insert(&mut c, n, "2026-10-13T08:00:00Z", &[]);
    }
    let mut f = EventFilter {
        limit: 3,
        ..EventFilter::default()
    };
    let mut sizes = Vec::new();
    let mut all = Vec::new();
    loop {
        let (page, next) = events(&mut c, &f).unwrap();
        sizes.push(page.len());
        if page.is_empty() {
            assert_eq!(next, f.after);
            break;
        }
        assert_eq!(next, page.last().unwrap()["seq"].as_i64().unwrap());
        all.extend(page);
        f.after = next;
    }
    assert_eq!(sizes, [3, 3, 1, 0]);
    assert_eq!(ids(&all), (1..=7).map(uuid).collect::<Vec<_>>());
    let seqs: Vec<i64> = all.iter().map(|e| e["seq"].as_i64().unwrap()).collect();
    assert!(seqs.windows(2).all(|w| w[0] < w[1]), "{seqs:?}");
}

#[test]
#[ignore = "needs Postgres: set PULSE_TEST_DATABASE_URL"]
fn each_filter_matches_its_column_exactly() {
    let (mut c, _g) = db("filters");
    type Set = fn(&mut EventFilter, String);
    let cases: [(&str, Set); 7] = [
        ("project", |f, v| f.project = Some(v)),
        ("goal_id", |f, v| f.goal = Some(v)),
        ("episode_id", |f, v| f.episode = Some(v)),
        ("task_id", |f, v| f.task = Some(v)),
        ("trace_id", |f, v| f.trace = Some(v)),
        ("source", |f, v| f.source = Some(v)),
        ("kind", |f, v| f.kind = Some(v)),
    ];
    // Each case runs before the next inserts, and earlier rows hold NULL (or
    // the base source and kind) in the columns filtered later.
    for (i, (col, set)) in cases.iter().enumerate() {
        let base = 100 * (i as u128 + 1);
        insert(&mut c, base + 1, "2026-10-13T08:00:00Z", &[(col, "want")]);
        insert(&mut c, base + 2, "2026-10-13T08:00:00Z", &[(col, "other")]);
        insert(&mut c, base + 3, "2026-10-13T08:00:00Z", &[(col, "want")]);
        let mut f = EventFilter::default();
        set(&mut f, "want".into());
        let (page, _) = events(&mut c, &f).unwrap();
        assert_eq!(ids(&page), [uuid(base + 1), uuid(base + 3)], "{col}");
    }
}

#[test]
#[ignore = "needs Postgres: set PULSE_TEST_DATABASE_URL"]
fn since_is_an_inclusive_lower_bound() {
    let (mut c, _g) = db("since");
    insert(&mut c, 1, "2026-10-13T07:00:00Z", &[]);
    insert(&mut c, 2, "2026-10-13T08:00:00Z", &[]);
    insert(&mut c, 3, "2026-10-13T09:00:00Z", &[]);
    let f = EventFilter {
        since: Some("2026-10-13T08:00:00Z".into()),
        ..EventFilter::default()
    };
    let (page, _) = events(&mut c, &f).unwrap();
    assert_eq!(ids(&page), [uuid(2), uuid(3)]);
}

#[test]
#[ignore = "needs Postgres: set PULSE_TEST_DATABASE_URL"]
fn rows_come_back_as_envelopes() {
    let (mut c, _g) = db("envelope");
    let id = uuid(42);
    let payload = json!({"n": 1, "msg": "hello", "nested": [true, null]});
    let extra = json!({"custom": 1, "tag": "x", "source": "not-this"});
    c.execute(
        "INSERT INTO events (id, ts, schema_version, source, kind, emitter_seq, \
         duration_ms, payload, extra, spool_file, spool_offset) \
         VALUES ($1::text::uuid, '2026-10-13T08:00:00.123456Z', 1, 'demo', 'demo.tick', \
         7, 12.5, $2, $3, 'f.ndjson', 99)",
        &[&id, &payload, &extra],
    )
    .unwrap();
    let (page, _) = events(&mut c, &EventFilter::default()).unwrap();
    assert_eq!(page.len(), 1);
    let e = page[0].as_object().unwrap();
    assert_eq!(e["id"], json!(id));
    let ts = e["ts"].as_str().unwrap();
    assert!(pulse::envelope::is_timestamp(ts), "{ts}");
    assert_eq!(ts, "2026-10-13T08:00:00.123456Z");
    assert_eq!(e["custom"], json!(1));
    assert_eq!(e["tag"], json!("x"));
    assert_eq!(
        e["source"],
        json!("demo"),
        "extra must not overwrite a column"
    );
    assert_eq!(e["kind"], json!("demo.tick"));
    assert_eq!(e["schema_version"], json!(1));
    assert_eq!(e["emitter_seq"], json!(7));
    assert_eq!(e["duration_ms"], json!(12.5));
    assert_eq!(e["payload"], payload);
    assert!(e["seq"].is_i64());
    for absent in [
        "project",
        "trace_id",
        "origin",
        "spool_file",
        "spool_offset",
        "ingested_at",
        "extra",
    ] {
        assert!(!e.contains_key(absent), "{absent} present in {e:?}");
    }
    assert!(e.values().all(|v| !v.is_null()), "{e:?}");
}

/// A fresh scratch dir under the temp dir, unique to `test` and this process.
fn scratch(test: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("pulse_query_{test}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Writes the two day files the spool and health tests count: 100 and 40 bytes.
fn day_files(d: &std::path::Path) {
    std::fs::write(d.join("20261012.ndjson"), [b'x'; 100]).unwrap();
    std::fs::write(d.join("20261013.ndjson"), [b'x'; 40]).unwrap();
}

#[test]
fn spool_bytes_counts_day_files_past_checkpoints() {
    let d = scratch("spool_bytes");
    day_files(&d);
    std::fs::write(d.join("notes.txt"), "ignored").unwrap();
    std::fs::write(d.join("2026101.ndjson"), "ignored").unwrap();
    let cp = |off| HashMap::from([("20261012.ndjson".to_string(), off)]);
    assert_eq!(spool_bytes(&d, &cp(60)).unwrap(), (80, 140));
    assert_eq!(spool_bytes(&d, &cp(500)).unwrap(), (40, 140));
    assert_eq!(spool_bytes(&d, &HashMap::new()).unwrap(), (140, 140));
    std::fs::remove_dir_all(&d).unwrap();
    assert_eq!(spool_bytes(&d, &cp(60)).unwrap(), (0, 0));
}

#[test]
#[ignore = "needs Postgres: set PULSE_TEST_DATABASE_URL"]
fn spans_of_a_trace_in_start_order() {
    let (mut c, _g) = db("spans");
    c.batch_execute(
        "INSERT INTO events (id, ts, source, kind, trace_id, span_id, parent_span_id, \
         spool_file, spool_offset) VALUES \
         ('00000000-0000-0000-0000-000000000001', '2026-10-13T08:00:00Z', 'demo', 'job.started', 't1', 's1', NULL, 'f.ndjson', 1), \
         ('00000000-0000-0000-0000-000000000002', '2026-10-13T08:00:01.5Z', 'demo', 'job.finished', 't1', 's1', NULL, 'f.ndjson', 2), \
         ('00000000-0000-0000-0000-000000000003', '2026-10-13T08:00:01Z', 'demo', 'step.started', 't1', 's2', 's1', 'f.ndjson', 3), \
         ('00000000-0000-0000-0000-000000000004', '2026-10-13T08:00:00Z', 'demo', 'job.started', 't2', 's9', NULL, 'f.ndjson', 4)",
    )
    .unwrap();
    let rows = spans(&mut c, "t1").unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");
    for r in &rows {
        let keys: Vec<&String> = r.as_object().unwrap().keys().collect();
        let mut want = [
            "name",
            "span_id",
            "parent_span_id",
            "source",
            "project",
            "started",
            "finished",
            "duration_ms",
        ];
        want.sort();
        assert_eq!(keys, want);
    }
    assert_eq!(rows[0]["name"], json!("job"));
    assert_eq!(rows[0]["span_id"], json!("s1"));
    assert!(rows[0]["finished"].is_string());
    assert!(pulse::envelope::is_timestamp(
        rows[0]["started"].as_str().unwrap()
    ));
    let ms = rows[0]["duration_ms"].as_f64().unwrap();
    assert!((ms - 1500.0).abs() < 0.001, "{ms}");
    assert_eq!(rows[1]["name"], json!("step"));
    assert_eq!(rows[1]["parent_span_id"], json!("s1"));
    assert!(rows[1]["finished"].is_null());
    assert!(rows[1]["duration_ms"].is_null());
    assert!(spans(&mut c, "nope").unwrap().is_empty());
}

#[test]
#[ignore = "needs Postgres: set PULSE_TEST_DATABASE_URL"]
fn artifact_location_by_hash() {
    let (mut c, _g) = db("artifact");
    let hash = "ab".repeat(32);
    let location = format!("ab/{hash}.zst");
    c.execute(
        "INSERT INTO artifacts (hash, size, media_type, location) \
         VALUES ($1, 5, 'text/plain', $2)",
        &[&hash, &location],
    )
    .unwrap();
    assert_eq!(
        artifact_location(&mut c, &hash).unwrap(),
        Some((location, Some("text/plain".to_string())))
    );
    assert_eq!(artifact_location(&mut c, &"cd".repeat(32)).unwrap(), None);
}

#[test]
#[ignore = "needs Postgres: set PULSE_TEST_DATABASE_URL"]
fn health_reports_lag_counts_and_last_ingest() {
    let (mut c, _g) = db("health");
    let d = scratch("health");
    let empty = health(&mut c, &d).unwrap();
    assert_eq!(
        empty,
        json!({"lag_bytes": 0, "spool_bytes": 0, "quarantine": 0, "events": 0,
               "last_ingested_at": null})
    );
    day_files(&d);
    c.batch_execute(
        "INSERT INTO checkpoints (spool_file, byte_offset) VALUES ('20261012.ndjson', 60); \
         INSERT INTO quarantine (spool_file, byte_offset, error, raw) \
         VALUES ('20261012.ndjson', 0, 'synthetic', '\\x7b'::bytea)",
    )
    .unwrap();
    insert(&mut c, 1, "2026-10-13T08:00:00Z", &[]);
    insert(&mut c, 2, "2026-10-13T08:00:01Z", &[]);
    let h = health(&mut c, &d).unwrap();
    std::fs::remove_dir_all(&d).unwrap();
    assert_eq!(h["lag_bytes"], json!(80));
    assert_eq!(h["spool_bytes"], json!(140));
    assert_eq!(h["quarantine"], json!(1));
    assert_eq!(h["events"], json!(2));
    let last = h["last_ingested_at"].as_str().unwrap();
    assert!(pulse::envelope::is_timestamp(last), "{last}");
}
