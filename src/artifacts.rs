//! Artifact intake: moves content-addressed blobs that emitters drop into the
//! journal spool into `<artifact_root>/<2hex>/<hash>.zst` and records a row.
//!
//! Every step is ordered so a crash loses nothing: the compressed file and its
//! row exist before the spool copy is deleted.

use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::ingest::QuarantineRow;
use crate::store::{ArtifactRow, Batch, Store, StoreError};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IntakeStats {
    pub stored: usize,
    pub duplicate: usize,
    pub quarantined: usize,
}

/// Takes every `^[0-9a-f]{64}$` file from `<journal_dir>/artifacts/` and
/// `<journal_dir>/incoming/<key>/artifacts/`; anything else is left alone.
pub fn intake(
    store: &mut impl Store,
    journal_dir: &Path,
    artifact_root: &Path,
) -> Result<IntakeStats, StoreError> {
    let mut dirs = vec!["artifacts".to_string()];
    for key in list(&journal_dir.join("incoming"), true)? {
        dirs.push(format!("incoming/{key}/artifacts"));
    }
    let mut stats = IntakeStats::default();
    for dir in dirs {
        for name in list(&journal_dir.join(&dir), false)? {
            if !is_hash(&name) {
                continue;
            }
            let path = journal_dir.join(&dir).join(&name);
            // ponytail: whole blob in memory, fine up to a few hundred MB; stream through zstd and sha2 if blobs get huge.
            let bytes = fs::read(&path)?;
            let hash = format!("{:x}", Sha256::digest(&bytes));
            if hash == name {
                let location = format!("{}/{hash}.zst", &hash[..2]);
                let target = artifact_root.join(&location);
                if target.exists() {
                    stats.duplicate += 1;
                } else {
                    let parent = target.parent().expect("target has a parent");
                    fs::create_dir_all(parent)?;
                    let tmp = parent.join(format!(".{hash}.zst.tmp"));
                    fs::write(&tmp, zstd::encode_all(&bytes[..], 0)?)?;
                    fs::rename(&tmp, &target)?;
                    stats.stored += 1;
                }
                store.add_artifact(&ArtifactRow {
                    hash,
                    size: bytes.len() as i64,
                    media_type: None,
                    location,
                })?;
            } else {
                store.commit(&Batch {
                    quarantine: vec![QuarantineRow {
                        spool_file: format!("{dir}/{name}"),
                        spool_offset: 0,
                        error: "artifact_hash_mismatch".into(),
                        raw: bytes[..bytes.len().min(4096)].to_vec(),
                    }],
                    ..Default::default()
                })?;
                stats.quarantined += 1;
            }
            fs::remove_file(&path)?;
        }
    }
    Ok(stats)
}

/// Sorted names of the directories (`want_dirs`) or plain files in `dir`;
/// empty when `dir` is missing. Non-UTF-8 names are skipped.
fn list(dir: &Path, want_dirs: bool) -> Result<Vec<String>, StoreError> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry?;
        let ft = entry.file_type()?;
        let keep = if want_dirs { ft.is_dir() } else { ft.is_file() };
        if let (true, Ok(name)) = (keep, entry.file_name().into_string()) {
            names.push(name);
        }
    }
    names.sort();
    Ok(names)
}

fn is_hash(name: &str) -> bool {
    name.len() == 64 && name.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}
