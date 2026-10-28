//! One collector pass: spool lines -> ingest -> store, a batch at a time.

use std::path::Path;

use crate::ingest::{Ingested, ingest};
use crate::spool;
use crate::store::{Batch, Store, StoreError};

/// Rows in the batches this pass committed (store-side duplicates included).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PassStats {
    pub events: usize,
    pub quarantined: usize,
    pub contexts: usize,
}

/// Spool retention: delete day files older than `cutoff` (`YYYYMMDD`) whose
/// every byte is committed, except files with quarantined lines, then drop
/// their checkpoints. Checkpoints go first: a crash between the two leaves a
/// file with no checkpoint, which the next pass re-reads from 0 into rows it
/// already has (ids dedupe). The other order could leave an old offset for a
/// name that comes back and grows past it, and those bytes would be skipped.
pub fn retire(
    store: &mut impl Store,
    journal_dir: &Path,
    cutoff: &str,
) -> Result<Vec<String>, StoreError> {
    let checkpoints = store.checkpoints()?;
    let keep = store.quarantined_files()?;
    let gone = spool::retirable(journal_dir, &checkpoints, &keep, cutoff)?;
    if !gone.is_empty() {
        store.forget(&gone)?;
    }
    for name in &gone {
        std::fs::remove_file(journal_dir.join(name))?;
    }
    Ok(gone)
}

/// Drain everything new in `journal_dir` into `store`. A failed commit
/// returns at once; its checkpoints never moved, so the next pass re-reads
/// the same lines. `batch_lines == 0` reads nothing and returns default stats.
pub fn pass(
    store: &mut impl Store,
    journal_dir: &Path,
    batch_lines: usize,
) -> Result<PassStats, StoreError> {
    let mut checkpoints = store.checkpoints()?;
    let mut stats = PassStats::default();
    loop {
        let (lines, new) = spool::read_new(journal_dir, &checkpoints, batch_lines)?;
        if lines.is_empty() {
            // A truncated file with no complete line yet still reset its
            // checkpoint to 0. Persist that, or a file that regrows past the
            // old offset before the next pass is read from mid-line.
            if !new.is_empty() {
                store.commit(&Batch {
                    checkpoints: new,
                    ..Batch::default()
                })?;
            }
            return Ok(stats);
        }
        let mut batch = Batch::default();
        for line in &lines {
            match ingest(&line.file, line.offset, &line.bytes) {
                Ingested::Event(row) => {
                    if let Some(c) = &row.context {
                        batch.contexts.push(c.clone());
                    }
                    batch.events.push(row);
                }
                Ingested::Quarantine(q) => batch.quarantine.push(q),
            }
        }
        batch.checkpoints = new;
        store.commit(&batch)?;
        checkpoints.extend(batch.checkpoints);
        stats.events += batch.events.len();
        stats.quarantined += batch.quarantine.len();
        stats.contexts += batch.contexts.len();
    }
}
