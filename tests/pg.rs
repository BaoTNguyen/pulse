//! Postgres store. Everything but the schema check needs a live server:
//! `PULSE_TEST_DATABASE_URL='host=/var/run/postgresql dbname=pulse_test'
//! cargo test --test pg -- --ignored`. Each test gets its own schema.

use std::collections::{HashMap, HashSet};

use postgres::{Client, NoTls};
use pulse::ingest::{ContextRow, EventRow, QuarantineRow};
use pulse::pg::{PgStore, SCHEMA};
use pulse::store::{ArtifactRow, Batch, Store};
use serde_json::json;
use uuid::Uuid;

#[test]
fn schema_is_embedded() {
    assert!(!SCHEMA.is_empty());
    assert!(SCHEMA.contains("CREATE TABLE IF NOT EXISTS events"));
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

fn store(test: &str) -> (PgStore, SchemaGuard) {
    let schema = format!("pulse_test_{test}_{}", std::process::id());
    let mut client = Client::connect(&conninfo(), NoTls).expect("connect to Postgres");
    client
        .batch_execute(&format!(
            "CREATE SCHEMA {schema}; SET search_path TO {schema}"
        ))
        .expect("create test schema");
    let guard = SchemaGuard(schema);
    (PgStore::new(client).expect("apply schema"), guard)
}

fn count(s: &mut PgStore, table: &str) -> i64 {
    s.client
        .query_one(&format!("SELECT count(*) FROM {table}"), &[])
        .unwrap()
        .get(0)
}

fn event(n: u128, ts: &str) -> EventRow {
    EventRow {
        id: Uuid::from_u128(n),
        ts: ts.into(),
        schema_version: Some(1),
        source: "demo".into(),
        kind: "demo.tick".into(),
        project: Some("proj-x".into()),
        trace_id: Some("t".repeat(32)),
        span_id: Some("s".repeat(16)),
        parent_span_id: Some("p".repeat(16)),
        context_id: Some("0123456789abcdef".into()),
        emitter_seq: Some(7),
        goal_id: Some("goal-1".into()),
        feature_id: Some("feat-1".into()),
        task_id: Some("task-1".into()),
        episode_id: Some("ep-1".into()),
        run_id: Some("run-1".into()),
        turn_id: Some("turn-1".into()),
        role: Some("worker".into()),
        duration_ms: Some(12.5),
        origin: Some("agent".into()),
        payload: Some(json!({"n": 1, "msg": "hello"})),
        extra: Some(json!({"unknown_field": [1, 2]})),
        spool_file: "2026-01-01.ndjson".into(),
        spool_offset: 42,
        context: None,
    }
}

#[test]
#[ignore = "needs Postgres: set PULSE_TEST_DATABASE_URL"]
fn retention_reads_quarantined_files_and_forgets_checkpoints() {
    let (mut s, _g) = store("retention");
    s.commit(&full_batch()).unwrap();
    s.commit(&Batch {
        checkpoints: HashMap::from([("b.ndjson".to_string(), 7)]),
        ..Batch::default()
    })
    .unwrap();
    assert_eq!(
        s.quarantined_files().unwrap(),
        HashSet::from(["2026-01-01.ndjson".to_string()])
    );

    s.forget(&["2026-01-01.ndjson".to_string()]).unwrap();
    assert_eq!(
        s.checkpoints().unwrap(),
        HashMap::from([("b.ndjson".to_string(), 7)])
    );
}

fn full_batch() -> Batch {
    Batch {
        events: vec![event(1, "2026-01-01T00:00:00.123Z")],
        contexts: vec![ContextRow {
            id: "0123456789abcdef".into(),
            first_seen: "2026-01-01T00:00:00Z".into(),
            component: Some("demo".into()),
            git_commit: Some("deadbeef".into()),
            host: Some("host-a".into()),
            sandbox: Some("docker".into()),
            lane: Some("api".into()),
            experiment_id: Some("exp-1".into()),
            variant: Some("B".into()),
            payload: json!({"component": "demo", "new_key": true}),
        }],
        quarantine: vec![QuarantineRow {
            spool_file: "2026-01-01.ndjson".into(),
            spool_offset: 100,
            error: "not json".into(),
            raw: vec![0xff, 0xfe, b'{', 0x80],
        }],
        checkpoints: HashMap::from([("2026-01-01.ndjson".to_string(), 200)]),
    }
}

#[test]
#[ignore = "needs Postgres: set PULSE_TEST_DATABASE_URL"]
fn batch_lands_with_every_column() {
    let (mut s, _g) = store("every_column");
    let b = full_batch();
    s.commit(&b).unwrap();
    let e = &b.events[0];

    let r = s
        .client
        .query_one(
            "SELECT id, ts = $1::text::timestamptz, schema_version, source, kind, project, \
             trace_id, span_id, parent_span_id, context_id, emitter_seq, goal_id, feature_id, \
             task_id, episode_id, run_id, turn_id, role, duration_ms, origin, payload, extra, \
             spool_file, spool_offset FROM events",
            &[&e.ts],
        )
        .unwrap();
    assert_eq!(r.get::<_, Uuid>(0), e.id);
    assert!(r.get::<_, bool>(1), "ts round-trips");
    assert_eq!(r.get::<_, Option<i32>>(2), e.schema_version);
    assert_eq!(r.get::<_, String>(3), e.source);
    assert_eq!(r.get::<_, String>(4), e.kind);
    let text = [
        (5, &e.project),
        (6, &e.trace_id),
        (7, &e.span_id),
        (8, &e.parent_span_id),
        (9, &e.context_id),
        (11, &e.goal_id),
        (12, &e.feature_id),
        (13, &e.task_id),
        (14, &e.episode_id),
        (15, &e.run_id),
        (16, &e.turn_id),
        (17, &e.role),
        (19, &e.origin),
    ];
    for (i, want) in text {
        assert_eq!(&r.get::<_, Option<String>>(i), want, "events column {i}");
    }
    assert_eq!(r.get::<_, Option<i64>>(10), e.emitter_seq);
    assert_eq!(r.get::<_, Option<f64>>(18), e.duration_ms);
    assert_eq!(r.get::<_, Option<serde_json::Value>>(20), e.payload);
    assert_eq!(r.get::<_, Option<serde_json::Value>>(21), e.extra);
    assert_eq!(r.get::<_, String>(22), e.spool_file);
    assert_eq!(r.get::<_, i64>(23), e.spool_offset);

    let c = &b.contexts[0];
    let r = s
        .client
        .query_one(
            "SELECT id, first_seen = $1::text::timestamptz, component, git_commit, host, \
             sandbox, lane, experiment_id, variant, payload FROM contexts",
            &[&c.first_seen],
        )
        .unwrap();
    assert_eq!(r.get::<_, String>(0), c.id);
    assert!(r.get::<_, bool>(1), "first_seen round-trips");
    let text = [
        (2, &c.component),
        (3, &c.git_commit),
        (4, &c.host),
        (5, &c.sandbox),
        (6, &c.lane),
        (7, &c.experiment_id),
        (8, &c.variant),
    ];
    for (i, want) in text {
        assert_eq!(&r.get::<_, Option<String>>(i), want, "contexts column {i}");
    }
    assert_eq!(r.get::<_, serde_json::Value>(9), c.payload);

    let q = &b.quarantine[0];
    let r = s
        .client
        .query_one(
            "SELECT spool_file, byte_offset, error, raw FROM quarantine",
            &[],
        )
        .unwrap();
    assert_eq!(r.get::<_, String>(0), q.spool_file);
    assert_eq!(r.get::<_, i64>(1), q.spool_offset);
    assert_eq!(r.get::<_, String>(2), q.error);
    assert_eq!(r.get::<_, Vec<u8>>(3), q.raw);

    assert_eq!(s.checkpoints().unwrap(), b.checkpoints);
}

#[test]
#[ignore = "needs Postgres: set PULSE_TEST_DATABASE_URL"]
fn replayed_batch_adds_nothing() {
    let (mut s, _g) = store("replay");
    let b = full_batch();
    s.commit(&b).unwrap();
    s.commit(&b).unwrap();
    assert_eq!(count(&mut s, "events"), 1);
    assert_eq!(count(&mut s, "contexts"), 1);
    assert_eq!(count(&mut s, "quarantine"), 1);
}

#[test]
#[ignore = "needs Postgres: set PULSE_TEST_DATABASE_URL"]
fn checkpoints_round_trip_and_overwrite() {
    let (mut s, _g) = store("checkpoints");
    let first = HashMap::from([("a.ndjson".to_string(), 10), ("b.ndjson".to_string(), 20)]);
    s.commit(&Batch {
        checkpoints: first.clone(),
        ..Batch::default()
    })
    .unwrap();
    assert_eq!(s.checkpoints().unwrap(), first);

    let second = HashMap::from([("a.ndjson".to_string(), 30)]);
    s.commit(&Batch {
        checkpoints: second,
        ..Batch::default()
    })
    .unwrap();
    let want = HashMap::from([("a.ndjson".to_string(), 30), ("b.ndjson".to_string(), 20)]);
    assert_eq!(s.checkpoints().unwrap(), want);
}

#[test]
#[ignore = "needs Postgres: set PULSE_TEST_DATABASE_URL"]
fn a_row_postgres_refuses_is_quarantined_and_the_batch_lands() {
    // PULSE-B08: one refused row used to fail every round on the same lines
    let (mut s, _g) = store("refused");
    let mut nul = event(3, "2026-01-01T00:00:00Z");
    nul.payload = Some(json!({"stdout": "a\u{0}b"}));
    nul.spool_offset = 300;
    let mut feb31 = event(2, "2026-02-31T00:00:00Z");
    feb31.spool_offset = 200;
    let b = Batch {
        events: vec![event(1, "2026-01-01T00:00:00Z"), feb31, nul],
        checkpoints: HashMap::from([("a.ndjson".to_string(), 10)]),
        ..Batch::default()
    };
    s.commit(&b).unwrap();
    assert_eq!(count(&mut s, "events"), 1);
    assert_eq!(s.checkpoints().unwrap()["a.ndjson"], 10);
    let rows = s
        .client
        .query(
            "SELECT byte_offset, error, raw FROM quarantine ORDER BY byte_offset",
            &[],
        )
        .unwrap();
    let got: Vec<(i64, String)> = rows.iter().map(|r| (r.get(0), r.get(1))).collect();
    assert_eq!(got.len(), 2, "{got:?}");
    assert_eq!(got[0].0, 200);
    assert!(got[0].1.starts_with("store_rejected:22"), "{got:?}");
    assert!(got[1].1.starts_with("store_rejected:22"), "{got:?}");
    let raw: Vec<u8> = rows[0].get(2);
    assert!(String::from_utf8_lossy(&raw).contains("2026-02-31"));
}

#[test]
#[ignore = "needs Postgres: set PULSE_TEST_DATABASE_URL"]
fn add_artifact_first_wins_and_check_refuses_bad_hash() {
    let (mut s, _g) = store("artifacts");
    let hash = "ab".repeat(32);
    let a = ArtifactRow {
        hash: hash.clone(),
        size: 5,
        media_type: Some("text/plain".into()),
        location: format!("ab/{hash}.zst"),
    };
    s.add_artifact(&a).unwrap();
    s.add_artifact(&ArtifactRow {
        location: "elsewhere.zst".into(),
        ..a.clone()
    })
    .unwrap();

    let rows = s
        .client
        .query(
            "SELECT hash, size, media_type, location FROM artifacts",
            &[],
        )
        .unwrap();
    assert_eq!(rows.len(), 1);
    let r = &rows[0];
    let got = ArtifactRow {
        hash: r.get(0),
        size: r.get(1),
        media_type: r.get(2),
        location: r.get(3),
    };
    assert_eq!(got, a);

    let bad = ArtifactRow {
        hash: "AB".repeat(32),
        ..a
    };
    assert!(s.add_artifact(&bad).is_err());
}
