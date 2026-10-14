use pulse::store::{ArtifactRow, MemoryStore, Store};

fn row(hex: char, size: i64, location: &str) -> ArtifactRow {
    ArtifactRow {
        hash: hex.to_string().repeat(64),
        size,
        media_type: Some("text/plain".into()),
        location: location.into(),
    }
}

#[test]
fn add_stores_the_row() {
    let mut s = MemoryStore::new();
    let a = row('a', 10, "aa/aaaa.zst");
    s.add_artifact(&a).unwrap();
    assert_eq!(s.artifacts, vec![a]);
}

#[test]
fn same_hash_is_a_noop_first_wins() {
    let mut s = MemoryStore::new();
    let first = row('a', 10, "aa/first.zst");
    s.add_artifact(&first).unwrap();
    s.add_artifact(&row('a', 99, "aa/second.zst")).unwrap();
    assert_eq!(s.artifacts, vec![first]);
}

#[test]
fn different_hashes_give_two_rows() {
    let mut s = MemoryStore::new();
    let a = row('a', 1, "aa/a.zst");
    let b = row('b', 2, "bb/b.zst");
    s.add_artifact(&a).unwrap();
    s.add_artifact(&b).unwrap();
    assert_eq!(s.artifacts, vec![a, b]);
}

#[test]
fn works_during_outage_and_leaves_fail_next() {
    let mut s = MemoryStore::new();
    s.fail_next = 2;
    let a = row('c', 3, "cc/c.zst");
    s.add_artifact(&a).unwrap();
    assert_eq!(s.artifacts, vec![a]);
    assert_eq!(s.fail_next, 2);
}
