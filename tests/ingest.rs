use pulse::ingest::*;
use serde_json::json;
use uuid::Uuid;

fn event(file: &str, offset: u64, line: &str) -> EventRow {
    match ingest(file, offset, line.as_bytes()) {
        Ingested::Event(e) => e,
        q => panic!("{line}: expected an event, got {q:?}"),
    }
}

fn quarantined(file: &str, offset: u64, bytes: &[u8]) -> QuarantineRow {
    match ingest(file, offset, bytes) {
        Ingested::Quarantine(q) => q,
        e => panic!("expected quarantine, got {e:?}"),
    }
}

fn legacy(file: &str, offset: u64) -> Uuid {
    Uuid::new_v5(
        &PULSE_LEGACY_NAMESPACE,
        format!("{file}:{offset}").as_bytes(),
    )
}

const EDGE: &str = include_str!("../contract/examples/valid/edge.ndjson");

fn edge(n: usize) -> &'static str {
    EDGE.lines().nth(n).unwrap()
}

#[test]
fn full_line_maps_every_field() {
    let line = r#"{"ts":"2026-06-15T08:30:00Z","source":"heart","kind":"episode.started","id":"0F8E2C4A-1B3D-4E5F-8A9B-0C1D2E3F4A5B","schema_version":1,"project":"pulse","trace_id":"t1","span_id":"s1","parent_span_id":"s0","context_id":"c1","emitter_seq":7,"goal_id":"g1","feature_id":"f1","task_id":"k1","episode_id":"e1","run_id":"r1","turn_id":"u1","role":"developer","duration_ms":12.5,"origin":"agent","payload":{"a":1}}"#;
    let expected = EventRow {
        id: Uuid::parse_str("0f8e2c4a-1b3d-4e5f-8a9b-0c1d2e3f4a5b").unwrap(),
        ts: "2026-06-15T08:30:00Z".into(),
        schema_version: Some(1),
        source: "heart".into(),
        kind: "episode.started".into(),
        project: Some("pulse".into()),
        trace_id: Some("t1".into()),
        span_id: Some("s1".into()),
        parent_span_id: Some("s0".into()),
        context_id: Some("c1".into()),
        emitter_seq: Some(7),
        goal_id: Some("g1".into()),
        feature_id: Some("f1".into()),
        task_id: Some("k1".into()),
        episode_id: Some("e1".into()),
        run_id: Some("r1".into()),
        turn_id: Some("u1".into()),
        role: Some("developer".into()),
        duration_ms: Some(12.5),
        origin: Some("agent".into()),
        payload: Some(json!({"a": 1})),
        extra: None,
        spool_file: "2026-06-15.ndjson".into(),
        spool_offset: 42,
        context: None,
    };
    assert_eq!(event("2026-06-15.ndjson", 42, line), expected);
    // A trailing newline is fine too.
    assert_eq!(
        event("2026-06-15.ndjson", 42, &format!("{line}\n")),
        expected
    );
}

#[test]
fn legacy_id_is_uuid5_of_file_offset() {
    let line = edge(0);
    let a = event("day.ndjson", 0, line).id;
    let b = event("day.ndjson", 100, line).id;
    assert_ne!(a, b);
    assert_eq!(a, Uuid::new_v5(&PULSE_LEGACY_NAMESPACE, b"day.ndjson:0"));
    assert_eq!(b, Uuid::new_v5(&PULSE_LEGACY_NAMESPACE, b"day.ndjson:100"));
    assert_eq!(event("day.ndjson", 100, line).id, b);
    assert_eq!(a.get_version_num(), 5);
}

#[test]
fn unknown_fields_land_in_extra() {
    let e = event("f", 0, edge(1));
    assert_eq!(
        e.extra,
        Some(json!({"flavor": "mint", "extra": {"nested": [1, 2, 3]}}))
    );
    assert_eq!(
        event("f", 0, edge(4)).extra,
        Some(json!({"unknown_top": "kept"}))
    );
    for k in ["ts", "source", "kind"] {
        assert!(e.extra.as_ref().unwrap().get(k).is_none());
    }
}

#[test]
fn null_optional_fields_are_absent() {
    let e = event("f", 9, edge(3));
    assert_eq!((e.emitter_seq, e.origin, e.extra), (None, None, None));
    assert_eq!(e.id, legacy("f", 9));
    let e = event("f", 0, edge(2));
    assert_eq!((e.payload, e.extra), (None, None));
}

#[test]
fn context_started_fills_context_row() {
    let payload = json!({"component": "heart", "git_commit": "abc123", "host": "box", "sandbox": "docker",
        "lane": "api", "experiment_id": "x1", "variant": "b", "model": "m"});
    let line = json!({"ts": "2026-06-15T08:30:00Z", "source": "heart", "kind": "context.started",
        "context_id": "ctx-1", "payload": payload})
    .to_string();
    let e = event("f", 0, &line);
    assert_eq!(
        e.context,
        Some(ContextRow {
            id: "ctx-1".into(),
            first_seen: "2026-06-15T08:30:00Z".into(),
            component: Some("heart".into()),
            git_commit: Some("abc123".into()),
            host: Some("box".into()),
            sandbox: Some("docker".into()),
            lane: Some("api".into()),
            experiment_id: Some("x1".into()),
            variant: Some("b".into()),
            payload,
        })
    );
    let bare = r#"{"ts":"2026-06-15T08:30:00Z","source":"heart","kind":"context.started"}"#;
    assert_eq!(event("f", 0, bare).context, None);
    let no_payload = r#"{"ts":"2026-06-15T08:30:00Z","source":"heart","kind":"context.started","context_id":"c"}"#;
    assert_eq!(
        event("f", 0, no_payload).context.unwrap().payload,
        json!({})
    );
}

#[test]
fn bad_lines_are_quarantined() {
    let first = |s: &'static str| s.lines().next().unwrap().as_bytes();
    let cases: [(&[u8], &str); 6] = [
        (
            first(include_str!("../contract/examples/invalid/not_json.ndjson")),
            "not_json",
        ),
        (
            first(include_str!(
                "../contract/examples/invalid/not_object.ndjson"
            )),
            "not_object",
        ),
        (
            include_str!("../contract/examples/invalid/missing_field.ndjson")
                .lines()
                .nth(1)
                .unwrap()
                .as_bytes(),
            "missing_field",
        ),
        (
            first(include_str!("../contract/examples/invalid/bad_kind.ndjson")),
            "bad_kind",
        ),
        (
            first(include_str!(
                "../contract/examples/invalid/bad_origin.ndjson"
            )),
            "bad_origin",
        ),
        (b"{\"ts\":\"\xff\"}", "not_utf8"),
    ];
    for (i, (bytes, code)) in cases.into_iter().enumerate() {
        let q = quarantined("bad.ndjson", i as u64 * 10, bytes);
        assert_eq!(
            q,
            QuarantineRow {
                spool_file: "bad.ndjson".into(),
                spool_offset: i as i64 * 10,
                error: code.into(),
                raw: bytes.to_vec(),
            }
        );
    }
}

#[test]
fn raw_is_truncated_to_4096() {
    let mut not_utf8 = vec![b'x'; 10000];
    not_utf8[0] = 0xff;
    for (bytes, code) in [(vec![b'x'; 10000], "not_json"), (not_utf8, "not_utf8")] {
        let q = quarantined("f", 0, &bytes);
        assert_eq!(q.error, code);
        assert_eq!(q.raw, bytes[..4096]);
    }
}

#[test]
fn contract_valid_examples_all_ingest() {
    let valid = [
        include_str!("../contract/examples/valid/basic.ndjson"),
        EDGE,
        include_str!("../contract/examples/valid/optional.ndjson"),
    ];
    for line in valid
        .iter()
        .flat_map(|s| s.lines())
        .filter(|l| !l.trim().is_empty())
    {
        event("f", 0, line);
    }
    macro_rules! invalid {
        ($($stem:literal),*) => {
            [$(($stem, include_str!(concat!("../contract/examples/invalid/", $stem, ".ndjson")))),*]
        };
    }
    let invalid = invalid!(
        "bad_kind",
        "bad_name",
        "bad_origin",
        "bad_timestamp",
        "bad_uuid",
        "missing_field",
        "not_json",
        "not_object",
        "wrong_type"
    );
    for (stem, text) in invalid {
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            assert_eq!(quarantined("f", 0, line.as_bytes()).error, stem, "{line}");
        }
    }
}
