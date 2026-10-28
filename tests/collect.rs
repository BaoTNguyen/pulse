use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use pulse::collect::{PassStats, pass, retire};
use pulse::store::MemoryStore;

const DAY1: &str = "20261013.ndjson";
const DAY2: &str = "20261014.ndjson";

fn scratch(test: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "pulse-collect-{test}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn line(n: u32) -> String {
    format!(
        r#"{{"ts":"2026-06-15T08:30:00Z","source":"heart","kind":"x.y","id":"00000000-0000-4000-8000-{n:012}"}}"#
    )
}

fn write_lines(dir: &Path, file: &str, ns: impl IntoIterator<Item = u32>) {
    let text: String = ns.into_iter().map(|n| line(n) + "\n").collect();
    append(dir, file, &text);
}

fn append(dir: &Path, file: &str, text: &str) {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(file))
        .unwrap()
        .write_all(text.as_bytes())
        .unwrap();
}

fn len(dir: &Path, file: &str) -> u64 {
    fs::metadata(dir.join(file)).unwrap().len()
}

fn distinct_ids(store: &MemoryStore) -> usize {
    store
        .events
        .iter()
        .map(|e| e.id)
        .collect::<HashSet<_>>()
        .len()
}

#[test]
fn every_line_lands_once() {
    let dir = scratch("once");
    write_lines(&dir, DAY1, 0..3);
    write_lines(&dir, DAY2, 3..7);
    let mut store = MemoryStore::new();

    let stats = pass(&mut store, &dir, 2).unwrap();
    assert_eq!(
        stats,
        PassStats {
            events: 7,
            quarantined: 0,
            contexts: 0
        }
    );
    assert_eq!(store.events.len(), 7);
    assert_eq!(distinct_ids(&store), 7);
    assert_eq!(store.checkpoints[DAY1], len(&dir, DAY1));
    assert_eq!(store.checkpoints[DAY2], len(&dir, DAY2));
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn second_pass_adds_nothing() {
    let dir = scratch("twice");
    write_lines(&dir, DAY1, 0..5);
    let mut store = MemoryStore::new();
    pass(&mut store, &dir, 2).unwrap();
    let (events, checkpoints) = (store.events.clone(), store.checkpoints.clone());

    assert_eq!(pass(&mut store, &dir, 2).unwrap(), PassStats::default());
    assert_eq!(store.events, events);
    assert_eq!(store.checkpoints, checkpoints);
    assert!(store.quarantine.is_empty() && store.contexts.is_empty());
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn identical_legacy_lines_both_survive() {
    let dir = scratch("legacy");
    let legacy = r#"{"ts":"2026-06-15T08:30:00Z","source":"heart","kind":"x.y"}"#;
    append(&dir, DAY1, &format!("{legacy}\n{legacy}\n"));
    let mut store = MemoryStore::new();

    assert_eq!(pass(&mut store, &dir, 2).unwrap().events, 2);
    assert_eq!(store.events.len(), 2);
    assert_ne!(store.events[0].id, store.events[1].id);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn failed_first_commit_loses_nothing() {
    let dir = scratch("fail-first");
    write_lines(&dir, DAY1, 0..4);
    let mut store = MemoryStore::new();
    store.fail_next = 1;

    assert!(pass(&mut store, &dir, 2).is_err());
    assert!(store.events.is_empty() && store.quarantine.is_empty());
    assert!(store.checkpoints.is_empty());

    assert_eq!(pass(&mut store, &dir, 2).unwrap().events, 4);
    assert_eq!(store.events.len(), 4);
    assert_eq!(distinct_ids(&store), 4);
    assert_eq!(store.checkpoints[DAY1], len(&dir, DAY1));
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn failed_later_commit_loses_nothing() {
    let dir = scratch("fail-later");
    write_lines(&dir, DAY1, 0..2);
    let mut store = MemoryStore::new();
    pass(&mut store, &dir, 2).unwrap();
    let after_first = store.checkpoints[DAY1];

    write_lines(&dir, DAY1, 2..4);
    store.fail_next = 1;
    assert!(pass(&mut store, &dir, 2).is_err());
    assert_eq!(store.events.len(), 2);
    assert_eq!(store.checkpoints[DAY1], after_first);

    assert_eq!(pass(&mut store, &dir, 2).unwrap().events, 2);
    assert_eq!(store.events.len(), 4);
    assert_eq!(distinct_ids(&store), 4);
    assert_eq!(store.checkpoints[DAY1], len(&dir, DAY1));
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn partial_last_line_waits() {
    let dir = scratch("partial");
    write_lines(&dir, DAY1, 0..1);
    let tail = line(1);
    let (head, rest) = tail.split_at(20);
    append(&dir, DAY1, head);
    let mut store = MemoryStore::new();

    assert_eq!(pass(&mut store, &dir, 2).unwrap().events, 1);
    assert_eq!(store.events.len(), 1);
    assert!(store.quarantine.is_empty());

    append(&dir, DAY1, &format!("{rest}\n"));
    assert_eq!(pass(&mut store, &dir, 2).unwrap().events, 1);
    assert_eq!(store.events.len(), 2);
    assert_eq!(distinct_ids(&store), 2);
    assert!(store.quarantine.is_empty());
    assert_eq!(store.checkpoints[DAY1], len(&dir, DAY1));
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn bad_lines_are_quarantined_and_do_not_stop_the_pass() {
    let dir = scratch("bad");
    let first = line(0) + "\n";
    let no_kind = r#"{"ts":"2026-06-15T08:30:00Z","source":"heart"}"#;
    append(&dir, DAY1, &format!("{first}not json\n{no_kind}\n"));
    write_lines(&dir, DAY1, 1..3);
    let mut store = MemoryStore::new();

    let stats = pass(&mut store, &dir, 2).unwrap();
    assert_eq!(
        stats,
        PassStats {
            events: 3,
            quarantined: 2,
            contexts: 0
        }
    );
    assert_eq!(store.events.len(), 3);
    let at: Vec<_> = store
        .quarantine
        .iter()
        .map(|q| (q.spool_file.as_str(), q.spool_offset))
        .collect();
    let second = first.len() as i64;
    assert_eq!(at, [(DAY1, second), (DAY1, second + 9)]);

    assert_eq!(pass(&mut store, &dir, 2).unwrap(), PassStats::default());
    assert_eq!(store.quarantine.len(), 2);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn context_started_fills_contexts_once() {
    let dir = scratch("context");
    let started = r#"{"ts":"2026-06-15T08:30:00Z","source":"heart","kind":"context.started","context_id":"ctx-1","payload":{"host":"box"}}"#;
    append(&dir, DAY1, &format!("{started}\n{started}\n"));
    let mut store = MemoryStore::new();

    assert_eq!(pass(&mut store, &dir, 2).unwrap().contexts, 2);
    assert_eq!(store.events.len(), 2);
    assert_eq!(store.contexts.len(), 1);
    assert_eq!(store.contexts[0].id, "ctx-1");

    pass(&mut store, &dir, 2).unwrap();
    assert_eq!(store.contexts.len(), 1);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn zero_batch_lines_returns() {
    let dir = scratch("zero");
    write_lines(&dir, DAY1, 0..2);
    let mut store = MemoryStore::new();
    assert_eq!(pass(&mut store, &dir, 0).unwrap(), PassStats::default());
    assert!(store.events.is_empty());
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_truncation_with_no_complete_line_still_resets_the_checkpoint() {
    let dir = scratch("trunc-partial");
    write_lines(&dir, DAY1, 1..=3);
    let mut store = MemoryStore::new();
    pass(&mut store, &dir, 100).unwrap();
    let old = len(&dir, DAY1);

    // truncated, then only half a line written before the next pass
    fs::write(dir.join(DAY1), &line(4)[..10]).unwrap();
    pass(&mut store, &dir, 100).unwrap();
    assert_eq!(store.checkpoints[DAY1], 0);

    // the file regrows past the old offset: every new line still arrives whole
    fs::write(dir.join(DAY1), "").unwrap();
    write_lines(&dir, DAY1, 4..=8);
    assert!(len(&dir, DAY1) > old);
    pass(&mut store, &dir, 100).unwrap();
    assert_eq!(distinct_ids(&store), 8);
    assert!(store.quarantine.is_empty());
    fs::remove_dir_all(&dir).unwrap();
}

// Retention (B12): a day file goes only when it is older than the cutoff, read
// to its last byte, and free of quarantined lines; its checkpoint goes with it.
#[test]
fn retire_deletes_only_old_fully_read_clean_files() {
    let dir = scratch("retire");
    let (old, torn, bad, unread) = (
        "20260901.ndjson",
        "20260902.ndjson",
        "20260903.ndjson",
        "20260904.ndjson",
    );
    write_lines(&dir, old, 0..3);
    write_lines(&dir, torn, 3..5);
    append(&dir, bad, "not json\n");
    write_lines(&dir, DAY1, 5..7); // newer than the cutoff
    let mut store = MemoryStore::new();
    pass(&mut store, &dir, 100).unwrap();
    append(&dir, torn, "{\"half\":"); // a torn last line: not all committed
    write_lines(&dir, unread, 7..8); // never passed: no checkpoint

    let gone = retire(&mut store, &dir, "20261001").unwrap();

    assert_eq!(gone, vec![old.to_string()]);
    assert!(!dir.join(old).exists());
    assert!(!store.checkpoints.contains_key(old));
    for kept in [torn, bad, unread, DAY1] {
        assert!(dir.join(kept).exists(), "{kept} was deleted");
    }
    assert_eq!(distinct_ids(&store), 7); // retiring removes files, never rows
    assert_eq!(
        retire(&mut store, &dir, "20261001").unwrap(),
        Vec::<String>::new()
    );
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_retired_name_that_comes_back_is_read_from_zero() {
    let dir = scratch("retire-reborn");
    let old = "20260901.ndjson";
    write_lines(&dir, old, 0..3);
    let mut store = MemoryStore::new();
    pass(&mut store, &dir, 100).unwrap();
    retire(&mut store, &dir, "20261001").unwrap();

    write_lines(&dir, old, 10..15); // longer than the old checkpoint
    pass(&mut store, &dir, 100).unwrap();

    assert_eq!(distinct_ids(&store), 8);
    fs::remove_dir_all(&dir).unwrap();
}
