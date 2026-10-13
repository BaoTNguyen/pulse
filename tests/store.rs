use std::collections::HashMap;

use pulse::ingest::{EventRow, Ingested, QuarantineRow, ingest};
use pulse::store::{Batch, MemoryStore, Store, StoreError};

const FILE: &str = "20261013.ndjson";
const EVENT: &str = r#"{"ts":"2026-06-15T08:30:00Z","source":"heart","kind":"x.y","id":"0f8e2c4a-1b3d-4e5f-8a9b-0c1d2e3f4a5b"}"#;
const CONTEXT: &str = r#"{"ts":"2026-06-15T08:30:00Z","source":"heart","kind":"context.started","context_id":"ctx-1","payload":{}}"#;

fn event(offset: u64, line: &str) -> EventRow {
    match ingest(FILE, offset, line.as_bytes()) {
        Ingested::Event(row) => row,
        other => panic!("expected event, got {other:?}"),
    }
}

fn quarantine(offset: u64, line: &str) -> QuarantineRow {
    match ingest(FILE, offset, line.as_bytes()) {
        Ingested::Quarantine(row) => row,
        other => panic!("expected quarantine, got {other:?}"),
    }
}

fn batch() -> Batch {
    let ev = event(0, EVENT);
    let ctx = event(200, CONTEXT);
    let context = ctx
        .context
        .clone()
        .expect("context.started yields a context");
    Batch {
        events: vec![ev, ctx],
        contexts: vec![context],
        quarantine: vec![quarantine(400, "not json")],
        checkpoints: HashMap::from([(FILE.to_string(), 409)]),
    }
}

#[test]
fn commit_stores_rows_and_checkpoints() {
    let mut s = MemoryStore::new();
    let b = batch();
    s.commit(&b).unwrap();
    assert_eq!(s.events, b.events);
    assert_eq!(s.contexts, b.contexts);
    assert_eq!(s.quarantine, b.quarantine);
    assert_eq!(s.checkpoints().unwrap(), b.checkpoints);
}

#[test]
fn recommit_is_idempotent_but_moves_checkpoints() {
    let mut s = MemoryStore::new();
    let mut b = batch();
    s.commit(&b).unwrap();
    b.checkpoints.insert(FILE.to_string(), 500);
    b.checkpoints.insert("20261014.ndjson".to_string(), 7);
    s.commit(&b).unwrap();
    assert_eq!(s.events.len(), 2);
    assert_eq!(s.contexts.len(), 1);
    assert_eq!(s.quarantine.len(), 1);
    assert_eq!(s.checkpoints().unwrap(), b.checkpoints);

    // Files not named keep their checkpoint.
    s.commit(&Batch {
        checkpoints: HashMap::from([(FILE.to_string(), 600)]),
        ..Batch::default()
    })
    .unwrap();
    assert_eq!(s.checkpoints["20261014.ndjson"], 7);
    assert_eq!(s.checkpoints[FILE], 600);
}

#[test]
fn duplicates_within_a_batch_first_wins() {
    let mut s = MemoryStore::new();
    let first = event(0, EVENT);
    let mut second = event(50, EVENT);
    second.kind = "other.kind".into();
    s.commit(&Batch {
        events: vec![first.clone(), second],
        ..Batch::default()
    })
    .unwrap();
    assert_eq!(s.events, vec![first]);
}

#[test]
fn quarantine_same_key_different_error_is_noop() {
    let mut s = MemoryStore::new();
    let q = quarantine(400, "not json");
    let mut other = q.clone();
    other.error = "something else".into();
    s.commit(&Batch {
        quarantine: vec![q.clone()],
        ..Batch::default()
    })
    .unwrap();
    s.commit(&Batch {
        quarantine: vec![other],
        ..Batch::default()
    })
    .unwrap();
    assert_eq!(s.quarantine, vec![q]);
}

#[test]
fn outage_stores_nothing_then_recovers() {
    let mut s = MemoryStore::new();
    s.fail_next = 1;
    let b = batch();
    let err = s.commit(&b).unwrap_err();
    assert_eq!(err, StoreError("simulated outage".into()));
    assert_eq!(s.fail_next, 0);
    assert!(s.events.is_empty() && s.contexts.is_empty() && s.quarantine.is_empty());
    assert!(s.checkpoints().unwrap().is_empty());

    s.commit(&b).unwrap();
    assert_eq!(s.events.len(), 2);
    assert_eq!(s.checkpoints[FILE], 409);
}

#[test]
fn store_error_displays_and_boxes() {
    let e = StoreError("boom".into());
    assert_eq!(e.to_string(), "boom");
    let boxed: Box<dyn std::error::Error> = Box::new(e);
    assert_eq!(boxed.to_string(), "boom");

    let io = std::io::Error::other("disk gone");
    assert_eq!(StoreError::from(io).0, "disk gone");
}
