use std::fs;
use std::path::{Path, PathBuf};

use pulse::artifacts::{IntakeStats, intake};
use pulse::store::MemoryStore;
use sha2::{Digest, Sha256};

/// Fresh `(journal, root)` pair under the system temp dir.
fn scratch(test: &str) -> (PathBuf, PathBuf) {
    let base = std::env::temp_dir().join(format!("pulse-artifacts-{test}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let (journal, root) = (base.join("journal"), base.join("root"));
    fs::create_dir_all(&journal).unwrap();
    (journal, root)
}

fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Drops `bytes` into `dir` under its own hash and returns the hash.
fn drop_blob(dir: &Path, bytes: &[u8]) -> String {
    fs::create_dir_all(dir).unwrap();
    let h = sha(bytes);
    fs::write(dir.join(&h), bytes).unwrap();
    h
}

fn stored(root: &Path, h: &str) -> Vec<u8> {
    zstd::decode_all(&fs::read(root.join(&h[..2]).join(format!("{h}.zst"))).unwrap()[..]).unwrap()
}

fn file_count(dir: &Path) -> usize {
    match fs::read_dir(dir) {
        Err(_) => 0,
        Ok(rd) => rd
            .map(|e| e.unwrap().path())
            .map(|p| if p.is_dir() { file_count(&p) } else { 1 })
            .sum(),
    }
}

#[test]
fn stores_blobs_from_artifacts_and_incoming() {
    let (journal, root) = scratch("both");
    let (a, b) = (
        b"synthetic blob one".to_vec(),
        b"synthetic blob two".to_vec(),
    );
    let ha = drop_blob(&journal.join("artifacts"), &a);
    let hb = drop_blob(&journal.join("incoming/run1/artifacts"), &b);
    let mut store = MemoryStore::new();

    let stats = intake(&mut store, &journal, &root).unwrap();

    assert_eq!(
        stats,
        IntakeStats {
            stored: 2,
            duplicate: 0,
            quarantined: 0
        }
    );
    assert_eq!(stored(&root, &ha), a);
    assert_eq!(stored(&root, &hb), b);
    assert_eq!(file_count(&root), 2);
    assert_eq!(store.artifacts.len(), 2);
    for (h, bytes) in [(&ha, &a), (&hb, &b)] {
        let row = store.artifacts.iter().find(|r| &r.hash == h).unwrap();
        assert_eq!(row.size, bytes.len() as i64);
        assert_eq!(row.location, format!("{}/{h}.zst", &h[..2]));
        assert_eq!(row.media_type, None);
    }
    assert!(!journal.join("artifacts").join(&ha).exists());
    assert!(!journal.join("incoming/run1/artifacts").join(&hb).exists());
    assert!(store.quarantine.is_empty());
}

#[test]
fn same_blob_twice_is_a_duplicate() {
    let (journal, root) = scratch("dup");
    let bytes = b"synthetic repeated blob".to_vec();
    let mut store = MemoryStore::new();
    drop_blob(&journal.join("artifacts"), &bytes);
    intake(&mut store, &journal, &root).unwrap();
    let h = drop_blob(&journal.join("incoming/inbox/artifacts"), &bytes);

    let stats = intake(&mut store, &journal, &root).unwrap();

    assert_eq!(
        stats,
        IntakeStats {
            stored: 0,
            duplicate: 1,
            quarantined: 0
        }
    );
    assert_eq!(file_count(&root), 1);
    assert_eq!(stored(&root, &h), bytes);
    assert_eq!(store.artifacts.len(), 1);
    assert!(!journal.join("incoming/inbox/artifacts").join(&h).exists());
}

#[test]
fn hash_mismatch_is_quarantined() {
    for (test, len) in [("small", 100), ("big", 5000)] {
        let (journal, root) = scratch(&format!("mismatch-{test}"));
        let bytes: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
        let name = "0".repeat(64);
        assert_ne!(sha(&bytes), name);
        fs::create_dir_all(journal.join("artifacts")).unwrap();
        fs::write(journal.join("artifacts").join(&name), &bytes).unwrap();
        let mut store = MemoryStore::new();

        let stats = intake(&mut store, &journal, &root).unwrap();

        assert_eq!(
            stats,
            IntakeStats {
                stored: 0,
                duplicate: 0,
                quarantined: 1
            }
        );
        assert_eq!(store.quarantine.len(), 1);
        let q = &store.quarantine[0];
        assert_eq!(q.spool_file, format!("artifacts/{name}"));
        assert_eq!(q.spool_offset, 0);
        assert_eq!(q.error, "artifact_hash_mismatch");
        assert_eq!(q.raw, bytes[..len.min(4096)]);
        assert!(!journal.join("artifacts").join(&name).exists());
        assert_eq!(file_count(&root), 0);
        assert!(store.artifacts.is_empty());
    }
}

#[test]
fn temporary_and_odd_names_are_left_alone() {
    let (journal, root) = scratch("tmp");
    let dir = journal.join("artifacts");
    let bytes = b"synthetic in-flight blob";
    let h = sha(bytes);
    let names = [
        format!("{h}.tmp"),
        ".partial".into(),
        h.to_uppercase(),
        h[..63].into(),
    ];
    fs::create_dir_all(dir.join(&h)).unwrap(); // a subdirectory with a hash name
    for n in &names {
        fs::write(dir.join(n), bytes).unwrap();
    }
    let mut store = MemoryStore::new();

    let stats = intake(&mut store, &journal, &root).unwrap();

    assert_eq!(stats, IntakeStats::default());
    for n in &names {
        assert_eq!(fs::read(dir.join(n)).unwrap(), bytes);
    }
    assert!(dir.join(&h).is_dir());
    assert!(store.artifacts.is_empty() && store.quarantine.is_empty());
    assert!(!root.exists());
}

#[test]
fn missing_directories_are_not_an_error() {
    let (journal, root) = scratch("empty");
    let mut store = MemoryStore::new();
    assert_eq!(
        intake(&mut store, &journal, &root).unwrap(),
        IntakeStats::default()
    );
}

/// Deterministic non-repeating bytes from a u64 LCG.
fn lcg_bytes(len: usize) -> Vec<u8> {
    let mut x: u64 = 1;
    (0..len)
        .map(|_| {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (x >> 33) as u8
        })
        .collect()
}

#[test]
fn large_blob_is_streamed() {
    let (journal, root) = scratch("large");
    let bytes = lcg_bytes(20 * 1024 * 1024);
    let h = drop_blob(&journal.join("artifacts"), &bytes);
    let mut store = MemoryStore::new();

    let stats = intake(&mut store, &journal, &root).unwrap();

    assert_eq!(stats.stored, 1);
    let row = store.artifacts.iter().find(|r| r.hash == h).unwrap();
    assert_eq!(row.size, 20 * 1024 * 1024);
    assert_eq!(sha(&stored(&root, &h)), h);
    assert!(!journal.join("artifacts").join(&h).exists());
}

/// Every file name under `dir`, recursively.
fn file_names(dir: &Path) -> Vec<String> {
    match fs::read_dir(dir) {
        Err(_) => Vec::new(),
        Ok(rd) => rd
            .map(|e| e.unwrap().path())
            .flat_map(|p| {
                if p.is_dir() {
                    file_names(&p)
                } else {
                    vec![p.file_name().unwrap().to_string_lossy().into_owned()]
                }
            })
            .collect(),
    }
}

#[test]
fn mismatch_raw_is_first_4k() {
    let (journal, root) = scratch("mismatch-raw");
    let bytes = lcg_bytes(100 * 1024);
    let name = "ab".repeat(32);
    assert_ne!(sha(&bytes), name);
    fs::create_dir_all(journal.join("artifacts")).unwrap();
    fs::write(journal.join("artifacts").join(&name), &bytes).unwrap();
    let mut store = MemoryStore::new();

    let stats = intake(&mut store, &journal, &root).unwrap();

    assert_eq!(stats.quarantined, 1);
    let q = &store.quarantine[0];
    assert_eq!(q.error, "artifact_hash_mismatch");
    assert_eq!(q.raw, bytes[..4096]);
    assert!(!journal.join("artifacts").join(&name).exists());
    assert!(
        file_names(&root)
            .iter()
            .all(|n| !n.ends_with(".tmp") && !n.ends_with(".zst"))
    );
}
