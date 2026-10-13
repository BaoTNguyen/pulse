//! Where ingested rows land: the `Store` contract and an in-memory store.
//!
//! The Postgres store comes later; `MemoryStore` pins the semantics it must
//! match and lets the collector be tested without a database.

use std::collections::HashMap;
use std::fmt;

use crate::ingest::{ContextRow, EventRow, QuarantineRow};

/// One commit's worth of rows, plus how far each day file has been read.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Batch {
    pub events: Vec<EventRow>,
    pub contexts: Vec<ContextRow>,
    pub quarantine: Vec<QuarantineRow>,
    /// Day-file name -> byte offset just past the last complete line read.
    pub checkpoints: HashMap<String, u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreError(pub String);

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for StoreError {}

impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        StoreError(e.to_string())
    }
}

/// `commit` is all-or-nothing. Rows whose key already exists (event id,
/// context id, quarantine `(spool_file, spool_offset)`) are silent no-ops,
/// within a batch too: first one wins. Batch checkpoints overwrite per file.
pub trait Store {
    fn checkpoints(&mut self) -> Result<HashMap<String, u64>, StoreError>;
    fn commit(&mut self, batch: &Batch) -> Result<(), StoreError>;
}

#[derive(Debug, Default)]
pub struct MemoryStore {
    pub events: Vec<EventRow>,
    pub contexts: Vec<ContextRow>,
    pub quarantine: Vec<QuarantineRow>,
    pub checkpoints: HashMap<String, u64>,
    /// Outage switch: while > 0, `commit` decrements it and fails.
    pub fail_next: usize,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Store for MemoryStore {
    fn checkpoints(&mut self) -> Result<HashMap<String, u64>, StoreError> {
        Ok(self.checkpoints.clone())
    }

    // ponytail: linear scans for dedup, O(n·m); key HashSets if tests ever get big.
    fn commit(&mut self, batch: &Batch) -> Result<(), StoreError> {
        if self.fail_next > 0 {
            self.fail_next -= 1;
            return Err(StoreError("simulated outage".into()));
        }
        for e in &batch.events {
            if !self.events.iter().any(|x| x.id == e.id) {
                self.events.push(e.clone());
            }
        }
        for c in &batch.contexts {
            if !self.contexts.iter().any(|x| x.id == c.id) {
                self.contexts.push(c.clone());
            }
        }
        for q in &batch.quarantine {
            let dup = self
                .quarantine
                .iter()
                .any(|x| x.spool_file == q.spool_file && x.spool_offset == q.spool_offset);
            if !dup {
                self.quarantine.push(q.clone());
            }
        }
        self.checkpoints
            .extend(batch.checkpoints.iter().map(|(k, v)| (k.clone(), *v)));
        Ok(())
    }
}
