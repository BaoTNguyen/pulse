# Pulse: refined logging design

Status: design proposal, 2026-09-30. This note supersedes the storage and
ownership choices in `DESIGN.md` (2026-09-21); its measurements and diagnosis of
the full-journal scan remain useful. Nothing here is implemented yet.

## Decision

Pulse is the central **event-journal subsystem**, not merely an aggregator.
It owns the event contract and catalog, local append format, collection,
validation, durable storage, query API, retention, and operational health.
Components still emit from their own process and language; Pulse does not
become a synchronous write service or an owner of their domain state.

Use the existing Postgres installation as the indexed store. At the measured
~1,917 events/day, and even the estimated 100x case (~191,700/day, ~2.2/s
average, ~100/s burst), Python versus Rust and Postgres versus SQLite are not
the present bottleneck. The repeated whole-journal read every two seconds is.
Do not add SQLite, Fluent Bit, Vector, Kafka, or another database to this path
without a measured need. Revisit a different store only after load tests and
query profiles show Postgres is the limiting factor.

The collector may be implemented in Rust now because Pulse is greenfield and
the wider harness may migrate to Rust. That is a maintainability/deployment
choice, not a throughput prerequisite. Local emitters should match the host
language: a small stdlib Python writer for Python components, `tracing` plus a
thin event-format layer for Rust components, and a safe append helper for
shell hooks. Do not call a Rust logger through FFI or a subprocess for every
Python event. All writers implement the same versioned event contract.

## Data path and ownership

```text
heart / arteries / plexus / capillaries / future components
    -> local, append-only NDJSON spool (including sandbox-local inboxes)
    -> Pulse collector (one ordered Postgres writer; one consumer per spool)
    -> existing Postgres: pulse.events + ingestion checkpoints/quarantine
    -> Pulse query API
    -> existing UIs, CLIs, exports, and domain-specific projections
```

An emitter must not need Pulse availability, network access, or database
credentials to keep doing its primary work. Appends are bounded, local, and
best-effort; a failed append is observable but does not fail the task. The
collector applies backpressure downstream while the spool holds the backlog.
Never delete a source file before its committed checkpoint is safely past it
and the retention grace period has elapsed. Show ingestion lag and spool size.

Pulse understands the envelope, not the meaning of `episode.*`, rewards, or
goals. Heart owns episode orchestration; Arteries owns memory and its trace
semantics; Plexus owns goal and feature semantics. Their specialized views can
read Pulse, but Pulse must not import them or reconstruct their state machines.

## Event contract and Postgres

Required envelope: `schema_version`, stable `event_id` for new writes, event
timestamp in UTC, `source`, `kind`, and an opaque JSON payload. Optional
correlation fields include `goal_id`, `feature_id`, `episode_id`, `task_id`,
`run_id`, `turn_id`, and a parent/span ID where present. Keep additive schema
evolution and retain unknown fields/kinds. Do not put secrets or full prompts
in the payload by default; redact at emission and enforce size caps.

Create `pulse.events` with a unique event ID, an independent monotonically
increasing `BIGINT` ingestion sequence, event and ingestion timestamps,
envelope fields, and `JSONB` payload. Use the ingestion sequence for stable
`WHERE seq > cursor ORDER BY seq LIMIT n` pagination; neither timestamp nor
UUID ordering is sufficient for an ingestion cursor. Keep one ordered writer
for sequence allocation and commits: concurrent transactions can commit a
higher sequence before a lower one and make a polling reader skip the latter.
Index the actual query
patterns first (for example goal, episode, kind/source with sequence), then
use `EXPLAIN` before adding more. Partition only when retention or measured
query/write behavior warrants it.

For old lines without IDs, derive a deterministic backfill ID from source
file identity plus byte offset. A content hash alone would collapse two
legitimate identical events. Collector writes and offset checkpoint must
commit in the same transaction; on crash, replay is harmless via unique IDs.
Quarantine malformed records with file and offset, error, and bounded raw
content; advance past them only when safely retained for diagnosis. Keep the
spool during database outages and replay after recovery. Avoid long-running
transactions and batch inserts in bounded chunks.

## Migration

1. Inventory each writer and reader, current journal paths, isolated inboxes,
   Postgres dependencies, and event consumers. Record baseline append latency,
   ingestion lag, scan cost, query latency, disk growth, and failure behavior.
2. Publish the versioned event envelope and catalog with examples and
   compatibility tests. Add stable IDs and goal/feature correlation where
   missing, without breaking tolerant old readers.
3. Create Postgres schema and collector, then backfill old journals. Verify
   counts by file/source/kind/time window and inspect duplicates and rejects.
4. Dual-read or shadow-query before cutover. Move the Plexus live tail first
   because its repeated full scan is the clearest bottleneck. Preserve legacy
   reads until parity and rollback are demonstrated.
5. Move remaining readers and Arteries' existing Postgres drain deliberately.
   Keep `arteries.agent_events` working as a specialized projection until its
   consumers are migrated; avoid two competing authoritative event stores.
6. Add spool/database retention, restore testing, ingestion-lag alerts, and
   redaction/audit checks. Set retention from measured volume and recovery
   needs, not an assumed 90-day hot tier.

Acceptance tests: two identical legitimate legacy lines both survive; replay
after a crash makes no duplicate; a database outage does not stop emitters;
bad lines are inspectable and do not stop later lines; sandbox emitters need no
database credentials; pagination has no gaps under same-timestamp events;
old and new queries agree on a sampled time range; deleting a spool file never
loses an uncommitted event. Measure at least the projected 100x burst with
realistic payloads before calling the migration complete.

## Deliberately deferred

Blob offloading, sampling successful tool events, hourly rollups, partitioning,
and a Rust-only emitter ecosystem are conditional improvements. They need
specific privacy, cost, or performance evidence and a clear data-loss policy.
In particular, do not sample the event journal until required audit and
debugging use cases are specified. No extra collector tier is justified yet.
