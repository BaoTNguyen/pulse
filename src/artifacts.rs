//! Artifact intake: moves content-addressed blobs that emitters drop into the
//! journal spool into `<artifact_root>/<2hex>/<hash>.zst` and records a row.
//!
//! Every step is ordered so a crash loses nothing: the compressed file and its
//! row exist before the spool copy is deleted.

use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::ingest::QuarantineRow;
use crate::query::is_hash;
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
            let location = format!("{}/{name}.zst", &name[..2]);
            let target = artifact_root.join(&location);
            let parent = target.parent().expect("target has a parent");
            fs::create_dir_all(parent)?;
            let tmp = parent.join(format!(".{name}.zst.tmp"));
            // ponytail: memory is one 64 KiB chunk per blob (+4 KiB raw prefix); no size cap.
            let mut file = fs::File::open(&path)?;
            let mut encoder = zstd::stream::Encoder::new(fs::File::create(&tmp)?, 0)?;
            let mut hasher = Sha256::new();
            let mut buf = [0u8; 65536];
            let mut size: u64 = 0;
            let mut raw: Vec<u8> = Vec::new();
            loop {
                let n = match file.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                    Err(e) => return Err(e.into()),
                };
                let chunk = &buf[..n];
                hasher.update(chunk);
                encoder.write_all(chunk)?;
                size += n as u64;
                let take = (4096 - raw.len()).min(n);
                raw.extend_from_slice(&chunk[..take]);
            }
            let hash = format!("{:x}", hasher.finalize());
            if hash == name {
                encoder.finish()?;
                if target.exists() {
                    fs::remove_file(&tmp)?;
                    stats.duplicate += 1;
                } else {
                    fs::rename(&tmp, &target)?;
                    stats.stored += 1;
                }
                store.add_artifact(&ArtifactRow {
                    hash,
                    size: size as i64,
                    media_type: None,
                    location,
                })?;
            } else {
                drop(encoder);
                fs::remove_file(&tmp)?;
                store.commit(&Batch {
                    quarantine: vec![QuarantineRow {
                        spool_file: format!("{dir}/{name}"),
                        spool_offset: 0,
                        error: "artifact_hash_mismatch".into(),
                        raw,
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
