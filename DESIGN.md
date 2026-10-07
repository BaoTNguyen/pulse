# Design

Written 2026-09-21, from measurements taken the same day. Nothing is built yet.
Historical design: see `REFINED_DESIGN.md` (2026-09-30) for the current
Postgres-backed, full event-journal ownership proposal. The measurements and
full-scan diagnosis below remain useful; SQLite and read-side-only decisions
below are superseded.

## 1. What exists today

The spine is real and specified: `heart/SPINE.md`, append-only NDJSON at
`~/.local/share/heart/events/YYYYMMDD.ndjson`, one event per line, fields
`ts / source / kind / episode_id / task_id / turn_id / payload`. Two rules
protect it — additive-only, tolerant readers — and there is deliberately no
shared library, so each repo carries a ~40-line stdlib emitter. `capillaries/
spine.py` is fifteen lines and says as much in its docstring.

Everyone already writes to it. Three recent day-files, 1,338 events:

| count | source | kind |
|---|---|---|
| 256 | arteries | `tool.result` (tool, target, exit_code, failed) |
| 215 | arteries | `turn.observed` (cli, repo, project_id, message_preview) |
| 102 | arteries | `decision.retrieval.gate` |
| 35 | arteries | `assistant.response` |
| 1 | capillaries | `prompt.retrieved` |
| — | heart | `episode.*`, `role.*`, `verify.round`, `diff.captured` |
| — | plexus | `goal.*`, `plan.*`, `feature.*`, `escalation.*` |

Everyone already reads it. `heart pulse` has `tail`, `render`,
`episode_timeline`, `goal_timeline`, `insights`, `health`. Arteries has
`journal`, `runs`, `trace`, `decisions`, `rewards`, with Postgres as the
queryable archive and the journal as the live channel that works from inside a
sandbox with no credentials. Plexus's live tail is `serve.py:578`, reading
`heart.pulse.load_events` and filtering to one goal.

So the timestamped action log is mostly built. What it lacks is an index, a
cursor, and a catalog with teeth.

## 2. Measured volume (2026-09-21)

| | today (busiest day) | 10x | 100x |
|---|---|---|---|
| events/day | 1,917 | 19,170 | 191,700 |
| bytes/day | 610 KiB | 6 MiB | 60 MiB |
| average rate | 0.02/s | 0.2/s | 2.2/s |
| burst (est.) | ~5/s | ~50/s | ~100/s |
| per year | — | 2 GiB | 21 GiB |

Median event 297 bytes, p95 787. All history to date: 13 MiB over ~3 months.

2.2 events/second is small. SQLite WAL handles thousands of appends per second
on one writer; a year at 100x fits in page cache. **No queue, no Kafka, no
Loki, no ClickHouse.** At these rates they are cost without benefit.

## 3. What breaks, in order

1. **The full-journal scan — breaks around 5x, not 100x.** `plexus/serve.py:584`
   calls `load_events()` every 2-second poll and filters in Python: every
   day-file read and JSON-parsed, per poll, per open tab. 13 MiB today; one
   week at 100x is 420 MiB per poll.
2. **Timestamp cursors.** `plexus/serve.py:1525` paginates on `ts > since`. At
   100 events/second, same-millisecond collisions drop or duplicate lines.
   Needs a monotonic id.
3. **Payload bloat.** `message_preview`, `assistant_preview`, `evidence` are why
   the median line is 297 bytes and not 80. At 100x those previews are most of
   the 21 GiB and none of them are queryable.
4. **Retention.** Unowned. Nothing prunes, nothing archives.

The append path, arteries' hooks and Postgres all survive 100x untouched.

## 4. Architecture

```text
emitters (5 repos, vendored, unchanged)
    │  append NDJSON · never blocks · never raises · no credential needed
    ▼
spool: ~/.local/share/heart/events/YYYYMMDD.ndjson
    │  collector tails from a checkpoint byte offset
    ▼
collector (single writer, daemon)
    │  validate against catalog · dedupe by id · spill big payloads out of line
    ▼
SQLite WAL ──────────► query API ──────────► plexus UI · heart pulse · marrow
    │
    └─ prune spool past checkpoint · archive cold rows to Postgres
```

**One writer.** This is the reason for a collector rather than five repos
writing SQLite directly: WAL permits one writer at a time, so concurrent agents
would contend inside the hot path of a turn. The spool absorbs concurrency, the
collector serialises it.

**Emitters never change.** No dependency, no network, no credential, no failure
mode. Worth more at 100x than today.

### Schema

```sql
CREATE TABLE events (
  id         TEXT PRIMARY KEY,      -- UUIDv7: sortable, dedupes replays
  ts         TEXT NOT NULL,
  source     TEXT NOT NULL,
  kind       TEXT NOT NULL,
  episode_id TEXT, task_id TEXT, run_id TEXT, turn_id TEXT,
  goal_id    TEXT, feature_id TEXT,   -- lifted out of payload, indexed
  payload    TEXT NOT NULL            -- JSON, opaque to this layer
);
CREATE INDEX ix_goal    ON events(goal_id, id);
CREATE INDEX ix_episode ON events(episode_id, id);
CREATE INDEX ix_kind    ON events(kind, id);
```

The live tail becomes `WHERE id > ? ORDER BY id LIMIT 500` — O(log n) per poll
regardless of history. That single change is most of the 100x.

### Blobs out of line

Anything over ~2 KiB (previews, diffs, `evidence`) goes to a content-addressed
file; the event carries hash and length. Sentry does this with attachments. It
keeps the events table small enough to stay in page cache indefinitely.

### Volume control

Per-kind caps declared in the catalog. `tool.result` is 256 of the last 1,338
events and will dominate at scale: sample the successful ones, keep every
failure. A kind that can flood has to say so.

### Retention tiers

SQLite holds a hot window (90 days ≈ 5 GiB at 100x). Colder rows drain to
Postgres. Spool files delete once the checkpoint passes them, which also makes
collector lag measurable for free.

### Failure modes, designed in

- collector down → spool grows, nothing else notices; `doctor` watches checkpoint lag and spool bytes
- collector behind → UI is stale, not wrong; show the lag
- replay after a crash → checkpoint is a byte offset, overlap is safe because id is the primary key and insert is `ON CONFLICT DO NOTHING`
- bad event → quarantine table, never a crash, never a dropped line (Sentry's "skip *and retain*")

## 5. Build order

1. **UUIDv7 ids in the emitters.** Five one-line changes, additive. Everything
   downstream depends on it — do it first or migrate twice.
2. **Collector + SQLite + query API**, reading the existing spool. Backfill from
   the existing 13 MiB, which is small enough to be a non-event.
3. **Point plexus at the query API.** Delete the scan. This is where the
   difference is felt.
4. **Catalog with per-kind caps**, enforced in the collector, not the emitter.
5. **Retention and `doctor` rules.**

Not building: a write-path service, a queue, an agent protocol, a dashboard
that isn't plexus.

## 6. Decisions already made, and why

**Not marrow.** It reads `episodes.jsonl` exports and never talks to Postgres or
the agent CLIs — its README says that separation is the point, because training
pulls torch, TRL, PEFT, bitsandbytes and vLLM. Logging is a hot path in every
workflow; marrow is the one repo not installed everywhere. Worse, marrow is the
self-improving component, and exo's core safety property is that the
self-improving part cannot edit the record used to judge it. Training needs
joined, labeled episodes, which heart already exports — raw events would make
marrow reimplement heart's stitching.

**Not plexus.** It is one of five writers and the least universal: work done in
a plain agent session outside a plexus goal would fall off the record entirely.
It also inverts layering, since plexus already imports heart. And it is the
fastest-changing repo in the stack — tabs, task model and transcript view all
moved in one week. Substrates should not move that fast.

**Not a central write service.** Every emitter would then depend on that service
being up, and when it is not you must spool to local disk — which is the
journal, so you keep both anyway. Sentry reaches the same conclusion: envelopes
are the on-disk format for "offline storage for deferred sending after
connection issues", and centralization happens at Relay, *after* the spool.

**Centralize the catalog and the reads, not the writes.** This is the half of
"centralized logging" that pays: one place to add a kind, one schema review, one
query surface, one retention policy — with emission still a local append that
cannot fail or block.

## 7. Prior art, and what to take

**exo / exoharness** (github.com/exoharness/exo). Splits trusted substrate
(secrets, sandbox, artifacts, append-only event log) from editable behavior. The
log is the one thing the agent cannot modify. State *is* the log — "the entire
state of the agent is defined as the version of the event log" — which makes
deterministic rewind and forking possible. Storage is behind an API
(`getEvents`, `watchEvents`), reference implementation SQLite in WAL mode, spec
not bound to it. UUIDv7 ids for ordering and pagination. Writes flow through
turn handles; the raw append is explicitly an escape hatch. Custom event types
are namespaced, so extension needs no gatekeeper.

Take: substrate ownership, API-over-store, UUIDv7, namespaced kinds.

**Sentry** (develop.sentry.dev/sdk/data-model/envelopes). Envelope is
newline-delimited JSON chosen for "fast parsing and human readability" — the
same format arrived at independently. Readers must "gracefully skip *and
retain*" unknown item types, which is stronger than skipping. Local files are
the offline spool. Relay centralizes normalization, validation, rate limiting
and PII scrubbing between many dumb SDKs and storage. Scale is handled with
limits (200 MiB envelope, 1 MiB event item), sampling and quotas — not by moving
ownership. Breadcrumbs (cheap trail) ride attached to the structured event
rather than as a parallel system.

Take: skip-and-retain, spool-then-collect, per-kind limits, and the rule that
the raw trail hangs off the structured record instead of sitting beside it.

## 8. Open questions

- **Agent coverage.** `tool.result` and `turn.observed` exist because arteries
  has hooks in Claude Code. What do codex, gemini and opencode report? That
  ceiling bounds any actions view, and `plexus.toml` lets any of them be the
  agent.
- **Goal lineage on hook events.** `arteries/journal.py:59-69` stamps
  `episode_id` and `task_id` from env but ignores `PLEXUS_GOAL_ID` /
  `PLEXUS_FEATURE_ID`, which `heart/events.py` does stamp. Inside an episode
  lineage survives by the `<goal>-<feature>-a<n>` task_id convention; outside
  one it is lost. ~4 lines in arteries to mirror heart.
- **Conversations are uncorrelated.** A `plexus discuss` session is not an
  episode, so no episode/task env is set and `plexus/serve.py:1125` `_run_env`
  does not set `PLEXUS_GOAL_ID` either. Those events land tagged only with
  `payload.repo` — recorded, but invisible to any goal-filtered view.
- **Where does the catalog live first** — a file in this repo from day one, or
  in heart until the collector exists?
- **Does arteries' Postgres drain move here?** It is the largest single piece of
  work in the idea and can be deferred past step 5.
- **Span shape.** `role.started`/`role.finished` with `duration_ms` are spans
  without a `parent_id`. Adding one turns the existing correlation model into
  real traces at no storage cost.

## 9. Levels above this, and when they are due

Logs are the floor and exist. Correlation is nearly free given the ids already
emitted. Metrics and alerting come later, and at this volume both derive from
the same store — no second pipeline.

| Level | Build when | Cheap version |
|---|---|---|
| structured log | always, first | done |
| correlation / traces | a unit of work crosses 3+ components | add `parent_id` to the existing start/finish pairs |
| indexed store + query API | a read scans more than it returns | this repo, steps 1-3 |
| derived metrics | "worse than yesterday" needs answering without a scan | hourly rollups written by the collector: count, p95, failures, cost, per kind per goal |
| alerting | something runs unattended *and* there is an action worth taking | four symptom rules, delivered as a morning digest, not a page |

Two signals no web service has, and the ones that will matter daily at 10-100x:
**cost** (dollars per landed task, per goal, per tier) and **quality**
(acceptance pass rate, attempts per feature, escalation rate, rework after
landing). A factory can be green on every SRE signal while being uneconomic or
producing garbage. Latency and saturation are what to alert on; cost and quality
are what to read every morning.
