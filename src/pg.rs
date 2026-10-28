//! The Postgres `Store`: rows land in the tables of `sql/schema.sql`, which
//! `PgStore::new` applies (idempotently) on every connect.

use std::collections::{HashMap, HashSet};

use postgres::{Client, NoTls};

use crate::store::{ArtifactRow, Batch, Store, StoreError};

pub const SCHEMA: &str = include_str!("../sql/schema.sql");

// `$n::text::timestamptz`, not `$n::timestamptz`: otherwise the client infers
// the parameter as timestamptz and won't serialize a String into it.
const INSERT_EVENT: &str = "INSERT INTO events (id, ts, schema_version, source, kind, project, \
    trace_id, span_id, parent_span_id, context_id, emitter_seq, goal_id, feature_id, task_id, \
    episode_id, run_id, turn_id, role, duration_ms, origin, payload, extra, spool_file, \
    spool_offset) VALUES ($1, $2::text::timestamptz, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, \
    $13, $14, $15, $16, $17, $18, $19, $20, $21, $22, $23, $24) ON CONFLICT (id) DO NOTHING";
const INSERT_CONTEXT: &str = "INSERT INTO contexts (id, first_seen, component, git_commit, host, \
    sandbox, lane, experiment_id, variant, payload) VALUES ($1, $2::text::timestamptz, $3, $4, \
    $5, $6, $7, $8, $9, $10) ON CONFLICT (id) DO NOTHING";
const INSERT_QUARANTINE: &str = "INSERT INTO quarantine (spool_file, byte_offset, error, raw) \
    VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING";
const UPSERT_CHECKPOINT: &str = "INSERT INTO checkpoints (spool_file, byte_offset) \
    VALUES ($1, $2) ON CONFLICT (spool_file) \
    DO UPDATE SET byte_offset = EXCLUDED.byte_offset, committed_at = now()";
const INSERT_ARTIFACT: &str = "INSERT INTO artifacts (hash, size, media_type, location) \
    VALUES ($1, $2, $3, $4) ON CONFLICT (hash) DO NOTHING";

fn pg_err(e: postgres::Error) -> StoreError {
    StoreError(e.to_string())
}

/// The SQLSTATE of an error that is about this row's values (class 22, data
/// exception; class 23, integrity violation), not the connection or schema.
fn refused(e: &postgres::Error) -> Option<String> {
    let state = e.code()?.code();
    (state.starts_with("22") || state.starts_with("23")).then(|| state.to_string())
}

/// The refused row as JSON, for `quarantine.raw` (first 4 KiB, like the
/// parse-time rows). The original line stays in the spool at the same
/// file and offset until retention removes it.
fn reconstructed(e: &crate::ingest::EventRow) -> Vec<u8> {
    let mut raw = serde_json::json!({
        "id": e.id.to_string(), "ts": e.ts, "source": e.source, "kind": e.kind,
        "project": e.project, "context_id": e.context_id, "episode_id": e.episode_id,
        "task_id": e.task_id, "payload": e.payload, "extra": e.extra,
    })
    .to_string()
    .into_bytes();
    raw.truncate(4096);
    raw
}

pub struct PgStore {
    pub client: Client,
}

impl PgStore {
    pub fn connect(conninfo: &str) -> Result<PgStore, StoreError> {
        PgStore::new(Client::connect(conninfo, NoTls).map_err(pg_err)?)
    }

    pub fn new(mut client: Client) -> Result<PgStore, StoreError> {
        client.batch_execute(SCHEMA).map_err(pg_err)?;
        Ok(PgStore { client })
    }
}

impl Store for PgStore {
    fn checkpoints(&mut self) -> Result<HashMap<String, u64>, StoreError> {
        let rows = self
            .client
            .query("SELECT spool_file, byte_offset FROM checkpoints", &[])
            .map_err(pg_err)?;
        rows.iter()
            .map(|r| {
                let file: String = r.get(0);
                let off: i64 = r.get(1);
                let off = u64::try_from(off)
                    .map_err(|_| StoreError(format!("negative checkpoint {off} for {file}")))?;
                Ok((file, off))
            })
            .collect()
    }

    // Dropping `tx` on any `?` rolls the whole batch back. Each row runs under
    // its own savepoint: a value that parses but Postgres won't store (a NUL
    // in text or jsonb, Feb 31) is quarantined as `store_rejected:<sqlstate>`
    // and the batch, checkpoint included, still lands. Before, one such row
    // failed every round on the same lines and stopped ingestion (PULSE-B08).
    fn commit(&mut self, batch: &Batch) -> Result<(), StoreError> {
        let mut tx = self.client.transaction().map_err(pg_err)?;
        let ev = tx.prepare(INSERT_EVENT).map_err(pg_err)?;
        let cx = tx.prepare(INSERT_CONTEXT).map_err(pg_err)?;
        let qu = tx.prepare(INSERT_QUARANTINE).map_err(pg_err)?;
        let cp = tx.prepare(UPSERT_CHECKPOINT).map_err(pg_err)?;
        let mut rejected = Vec::new();
        for e in &batch.events {
            let mut sp = tx.transaction().map_err(pg_err)?;
            let inserted = sp.execute(
                &ev,
                &[
                    &e.id,
                    &e.ts,
                    &e.schema_version,
                    &e.source,
                    &e.kind,
                    &e.project,
                    &e.trace_id,
                    &e.span_id,
                    &e.parent_span_id,
                    &e.context_id,
                    &e.emitter_seq,
                    &e.goal_id,
                    &e.feature_id,
                    &e.task_id,
                    &e.episode_id,
                    &e.run_id,
                    &e.turn_id,
                    &e.role,
                    &e.duration_ms,
                    &e.origin,
                    &e.payload,
                    &e.extra,
                    &e.spool_file,
                    &e.spool_offset,
                ],
            );
            match inserted {
                Ok(_) => sp.commit().map_err(pg_err)?,
                Err(err) => match refused(&err) {
                    // dropping `sp` rolls back to the savepoint
                    Some(state) => rejected.push((e, state)),
                    None => return Err(pg_err(err)),
                },
            }
        }
        for c in &batch.contexts {
            let mut sp = tx.transaction().map_err(pg_err)?;
            let inserted = sp.execute(
                &cx,
                &[
                    &c.id,
                    &c.first_seen,
                    &c.component,
                    &c.git_commit,
                    &c.host,
                    &c.sandbox,
                    &c.lane,
                    &c.experiment_id,
                    &c.variant,
                    &c.payload,
                ],
            );
            match inserted {
                Ok(_) => sp.commit().map_err(pg_err)?,
                // a context row has no spool line of its own: the event that
                // announced it is quarantined above if it was refused too
                Err(err) if refused(&err).is_some() => {}
                Err(err) => return Err(pg_err(err)),
            }
        }
        for q in &batch.quarantine {
            tx.execute(&qu, &[&q.spool_file, &q.spool_offset, &q.error, &q.raw])
                .map_err(pg_err)?;
        }
        for (e, state) in rejected {
            let error = format!("store_rejected:{state}");
            tx.execute(
                &qu,
                &[&e.spool_file, &e.spool_offset, &error, &reconstructed(e)],
            )
            .map_err(pg_err)?;
        }
        for (file, &off) in &batch.checkpoints {
            let off = i64::try_from(off)
                .map_err(|_| StoreError(format!("checkpoint {off} for {file} overflows i64")))?;
            tx.execute(&cp, &[file, &off]).map_err(pg_err)?;
        }
        tx.commit().map_err(pg_err)
    }

    fn add_artifact(&mut self, a: &ArtifactRow) -> Result<(), StoreError> {
        self.client
            .execute(
                INSERT_ARTIFACT,
                &[&a.hash, &a.size, &a.media_type, &a.location],
            )
            .map(|_| ())
            .map_err(pg_err)
    }

    fn quarantined_files(&mut self) -> Result<HashSet<String>, StoreError> {
        let rows = self
            .client
            .query("SELECT DISTINCT spool_file FROM quarantine", &[])
            .map_err(pg_err)?;
        Ok(rows.iter().map(|r| r.get(0)).collect())
    }

    fn forget(&mut self, files: &[String]) -> Result<(), StoreError> {
        self.client
            .execute(
                "DELETE FROM checkpoints WHERE spool_file = ANY($1)",
                &[&files],
            )
            .map(|_| ())
            .map_err(pg_err)
    }
}
