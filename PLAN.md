# Pulse: build plan

Written 2026-10-07. `REFINED_DESIGN.md` says what pulse is; this says what
gets built, in what order, and how we know each step is done. Where they
disagree, this file is newer.

## Decisions

- **Language.** Pulse is Rust (1.99.0, pinned in `rust-toolchain.toml`; the
  verifier image and the umbrella CI pin the same version). Emitters stay in each
  component's own language. Python components keep Python emitters until that
  component is ported. The contract between them is a line format, not a
  library.
- **Store.** A separate `pulse` database on the existing local Postgres server.
  It uses socket peer auth, so there is no password and no `secrets/pulse/`.
  Backup, `--drop-data` and uninstall are one database each. Joins against
  arteries' memory tables go through `postgres_fdw` when an analysis needs them.
- **Reads.** A small HTTP API served on a Unix socket,
  `~/.vascular/state/pulse/pulse.sock`, mode 0600. Only the owning user can
  connect. No TCP port means Docker's `host.docker.internal` forwarding can't
  reach it, so sandboxed agents can't read other projects' events. Readers never
  touch pulse's tables, so the schema can change underneath them.
- **One pulse per machine.** One collector, guarded by a lock file, so a second
  start says "already running" and exits. A stale socket left by a crash is
  detected by a failed connect, then removed. Every event carries `project`.
  `VASCULAR_HOME` moves socket, spool, lock and log together, so tests get their
  own pulse.
- **Record versus state.** Pulse holds the record of what happened. A component
  keeps only the state it makes runtime decisions from, as a projection it could
  rebuild from pulse. Memory itself (arteries' facts, capillaries' prompts and
  skills) is data, not logs, and stays where it is.
- **heart's module.** `heart/pulse.py` becomes `heart observe`. Its episode
  analysis (`insights`, `health`) stays in heart and reads through pulse. The
  generic `tail`, `render` and timelines become the pulse CLI.

## Where things are today (measured 2026-10-07)

- Journal: 43,405 lines in 86 day files (20 MB) under `~/.vascular/spool/events/`,
  plus 223 sandbox inboxes in `incoming/<run_id>/`. 92 distinct kinds. arteries
  writes about 85% of lines, heart about 10%.
- Every line has `ts`, `source` and `kind`, and every `ts` is UTC with `+00:00`.
  About 32% carry an `id` (arteries' run events). None carry a schema version.
  No line in the journal fails to parse.
- Readers all go through heart's `pulse.load_events`, a full scan: plexus
  `serve.py` (3), `observe.py` (3), `scope.py`, `cli tail`; heart `serve.py` and
  its CLI.
- Event-shaped Postgres tables that move to pulse: `arteries.agent_events`
  (a copy of the journal), `arteries.decisions`, `agent_runs`, `retrievals`;
  capillaries' `serving_log` and `batch_processing_log`. Tables kept as
  projections fed from pulse: `arteries.rewards`, `arteries.episodes`,
  `skills.skill_runs`, `optimization_runs`.

## Schema

```
events          seq bigint (ingestion order, the cursor), id uuid unique,
                ts, ingested_at, schema_version, source, kind, project,
                trace_id, span_id, parent_span_id, context_id, emitter_seq,
                goal_id, feature_id, task_id, episode_id, run_id, turn_id, role,
                duration_ms, trust, payload jsonb
spans (view)    *.started / *.finished pairs: one row per unit of work, its
                parent, start, end and duration
artifacts       hash (sha256), size, media_type, stored_at, location
                -- full prompts, responses, tool I/O, diffs, transcripts; events
                -- reference them by hash and stay small
contexts        id, component, git_commit, model, profile, config hashes,
                image digest, host -- one row per process start; answers "what
                exactly ran"
metric_samples  ts, metric, value, labels -- CPU/GPU sampling, its own retention
checkpoints     spool file, byte offset, committed_at
quarantine      spool file, byte offset, error, first 4 KiB of the raw line
```

What each addition is for:

- **artifacts:** training and reverse engineering need the full text an agent
  saw and wrote, not a preview.
- **contexts:** comparing runs needs to know which code, model and config ran.
- **trace_id / span ids:** the tree goal → feature → episode → role → tool
  call. Parents pass their ids to child processes, sandboxes included, through
  `PULSE_TRACE_ID` and `PULSE_PARENT_SPAN`.
- **trust:** untrusted (web-derived) content must never become training data or
  steer an agent unflagged. It mirrors arteries' trust rule.
- **emitter_seq:** orders events from one process that share a timestamp.
- Labels (rewards, acceptance, review verdicts, operator approvals and
  `resolve` answers) are ordinary events pointing at the span they judge.

Not covered yet: the inner turns of codex and opencode, which only reach us
through their transcript files. That needs a parser per CLI, later.

## Phases

### P0. Skeleton (by hand)
Crate `pulse` with `serde` and `serde_json` only, a committed `Cargo.lock`,
`rust-toolchain.toml`, `contract/envelope.json` (the envelope, hand-written
because three repos will vendor it), and CI running `cargo test --locked`.
Then rebuild the verifier image with these crates fetched, because the
verifier has no network. **Rule from here on: a new dependency means an image
rebuild first.**

### P1. The contract
- `contract/catalog.json`: every kind, with its source and notable payload
  fields, built from heart's SPINE.md and the kinds actually observed.
- `contract/examples/valid/*.ndjson` and `contract/examples/invalid/*.ndjson`:
  synthetic lines only, because this repo is public and real payloads carry
  prompt text.
- Rust: parse and validate a line against `envelope.json`. Keep unknown fields
  and unknown kinds ("skip and retain"). Return a typed error for each invalid
  case.
- heart: SPINE.md names `pulse/contract/` as the canon and keeps the prose.

### P2. Emitters (Python, per component)
heart, arteries and capillaries stamp `id` (uuid4), `schema_version`,
`project`, `emitter_seq`, `context_id` and the trace ids. Each writes a line
with one `os.write`, and writes full content to artifacts. `envelope.json` is
vendored into each and hash-checked in umbrella CI. A Rust port of
`vascular_paths`, plus `vectors.json` in the umbrella, checked against both
languages.

### P3. Collector
Tails the day files from a checkpoint. Lines without an `id` get
`uuid5(file:offset)`, so identical legacy lines both survive. Each batch's
inserts and its checkpoint commit together, and a replay hits `ON CONFLICT DO
NOTHING`. Bad lines go to quarantine and never stop the collector. Storage
sits behind a trait: an in-memory implementation for the verifier, which has
no Postgres, and Postgres tests marked `#[ignore]` that are run on the host.
Logs go to `~/.vascular/log/pulse/collector.log`. By hand: create the
database, install `pulse.service`, backfill, and reconcile counts per file,
source and kind. First, check whether arteries' drain already copies
`incoming/` into the day files; if it does, reading both would double-count
id-less lines.

### P4. Query API
On the socket: `GET /events?after=&limit=&project=&goal=&episode=&task=&trace=&source=&kind=&since=`,
`GET /spans?trace=`, `GET /artifacts/<hash>`, and `GET /health` (ingestion lag
in bytes, spool size, quarantine count). CLI: `pulse tail`, `pulse events`,
`pulse health`.

### P5. Cut over
- A ~30-line stdlib client replaces `heart.pulse.load_events` in plexus and
  heart. It falls back to the file scan while each reader is checked old
  against new on a sampled day.
- `heart observe`.
- arteries and capillaries move the tables listed above onto events, and
  their readers onto pulse.

### Done when
These come from REFINED_DESIGN:

- Two identical legacy lines both survive.
- Replay after a crash makes no duplicates.
- A database outage leaves the spool intact, and it replays afterwards.
- Bad lines are inspectable.
- No pagination gaps under events with the same timestamp.
- Old and new queries agree on a sampled day.
- A 100x burst is measured.

Then the umbrella's phase 3d: pulse deletes spool files behind its checkpoint
plus a grace period, and the fallbacks and direct readers go.
