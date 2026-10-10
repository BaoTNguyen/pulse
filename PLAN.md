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
- **Origin, not trust.** Each event says where its content came from:
  `operator` (a person), `agent` (written by an agent of this stack) or
  `external` (web pages, search results, third-party text). The worry with
  external text is the actions it can trigger (prompt injection), so it is kept
  as evidence linking input to action, and as robustness training data. It is
  never deleted for being external. Whether a run was sandboxed, and on which
  network lane, is provenance and lives on the context row.
- **Keep everything.** Episodes can't be regenerated (models and harness move
  on), and compressed history is cheap, so every event and artifact is kept
  from the beginning. Staleness is handled at export time by filtering or
  weighting on date and provenance, never by deleting. Only regenerable or
  redundant data expires. The one exception is a captured secret: `pulse
  purge` removes named artifacts everywhere, cold packs and snapshots
  included, and records that it did.
- **marrow reads, never edits.** marrow does the offline, learning-oriented
  analysis (datasets, reward models, policy evaluation, training) from frozen
  dataset snapshots exported by pulse and named by hash. Every model records the
  snapshot it learned from. marrow writes only its own `training.*` events and
  its outputs. Live operations (health, alerts, troubleshooting) stay in pulse
  and the components that own the concepts.
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
                duration_ms, origin, payload jsonb
spans (view)    *.started / *.finished pairs: one row per unit of work, its
                parent, start, end and duration
artifacts       hash (sha256), size, media_type, stored_at, location
                -- full prompts, responses, tool I/O, diffs, transcripts; events
                -- reference them by hash and stay small
contexts        id, component, git_commit, model, profile, config hashes,
                image digest, host, sandbox (docker-sbx | off), lane (api | web),
                experiment_id, variant -- one row per process start; answers
                "what exactly ran"
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
- **origin:** lets an export include or exclude runs whose inputs held
  external text, and lets an investigation walk from an action back to the
  external text the agent had just read.
- **emitter_seq:** orders events from one process that share a timestamp.
- Labels (rewards, acceptance, review verdicts, operator approvals and
  `resolve` answers) are ordinary events pointing at the span they judge.

Every payload field in `contract/catalog.json` carries one class, which decides
where it is stored, how long it lives, whether it is redacted, and which exports
see it:

| Class | Holds | Main readers |
|---|---|---|
| identity | ids that tie events together | every query, trace joins |
| time | durations, timestamps | latency, critical path |
| measure | tokens, cost, sizes, exit codes, counts, scores | cost, performance, rollups |
| label | judgements: outcome, passed, reward, verdict, failure class, approvals | training, quality |
| decision | a choice and its options | policy tuning |
| content | text or code an agent saw or produced | artifacts, training |
| provenance | what ran: model, agent, commit, spec hash | reproducibility, experiments |
| security | paths, network, rules, seats | audit |
| diagnostic | debugging detail | logs, short retention |

Each use reads only what it needs. The latency view never touches payload or
artifacts, and training reads content and labels for the runs its filter
selects.

Not covered yet: the inner turns of codex and opencode, which only reach us
through their transcript files. That needs a parser per CLI, later.

## Retention and storage

| Tier | Holds | Format | Where |
|---|---|---|---|
| hot (90 days, or pinned) | recent events, recent artifacts | Postgres rows; one zstd file per artifact | `pulse` DB; `~/.vascular/data/pulse/artifacts/` |
| cold (older, forever) | events | monthly Parquet, zstd-19 | `~/.vascular/data/pulse/archive/events/YYYY-MM.parquet` |
| | artifacts | monthly packs: one `zstd -19 --long=31` stream plus a hash-to-offset index | `.../archive/artifacts/YYYY-MM.pack` + `.idx` |
| datasets | marrow's training snapshots | Parquet, named by hash | `~/.vascular/data/pulse/datasets/` |

Pinned means it stays hot: traces of unfinished goals, anything an active
dataset snapshot references, and anything pinned by hand. Reads from cold data
decompress on the fly.

Measured on real data (2026-10-08): 193 MB of transcripts packs to 22.9 MB
(8.4x) and unpacks at about 1.6 GB/s. `--ultra -22` gains only 1% more at 3.5x
the CPU, and xz 1.5% more at 8x slower reads. Events: the 17.1 MB journal is
1.83 MB as Parquet, the same as the best generic compression, and reading two
columns of all of it takes 2 ms. Dedup by content hash saves under 1%, so
compression in large packs is what matters.

Unpacking never writes an uncompressed copy to disk: `pulse export` streams
the packs into one compressed Parquet snapshot. The disk blow-ups live in
training tooling (Hugging Face's Arrow cache, tokenized copies), so marrow
streams by default and keeps any cache under `~/.vascular/cache/marrow/`,
cleared per run. Disk is tight: this machine had 160 GB free of 2 TB on
2026-10-08, so a year of tokenized data (~150 GB at 100x) would not fit. pulse
itself needs ~20 GB/year at 100x.

Expires: spool files once ingested plus 7 days; raw metric samples after 14
days (rollups kept); previews in cold storage; training caches; snapshot data no
model references (manifest kept); diagnostic logs by rotation. Settings live in
`~/.vascular/config/pulse/retention.toml`.

Backup: Proxmox Backup Server on the Proxmox host. This machine is a VM on
that host, with its own NVMe passed through, so the PBS datastore (1 TB, single
disk) sits on a different physical disk in the same chassis. It runs on the
host, never inside this VM. Client-side encryption, a role that may add backups but not delete
them (pruning happens on the PBS side), scheduled verify jobs, and a restore
test. It covers the pulse DB dump, `data/pulse/`, the `capillaries` memory
database, and `~/.vascular/config`. `secrets/` is never backed up. That makes
a second copy on a separate disk. It survives this NVMe dying, a broken VM, an
accidental delete, or a compromised agent in the VM. It does not survive the
host itself failing (a power surge, a bad PSU, theft, fire), which takes both
copies at once. So the encrypted off-site third copy is needed, not optional.
Status 2026-10-09: PBS not installed yet, no off-site provider chosen. The
backup job is built in P5 once both exist.

## Phases

### P0. Skeleton (by hand)
Crate `pulse` with `serde` and `serde_json` only, a committed `Cargo.lock`,
`rust-toolchain.toml`, `contract/envelope.json` (the envelope, hand-written
because three repos will vendor it), and CI running `cargo test --locked`.
Then rebuild the verifier image with these crates fetched, because the
verifier has no network. **Rule from here on: a new dependency means an image
rebuild first.**

### P1. The contract
- `contract/catalog.json`: every kind with its source, a description, and its
  payload fields, each tagged with a class. Built from heart's SPINE.md and the
  kinds actually observed, plus `context.started` (the row behind `contexts`).
- `contract/examples/valid/*.ndjson` and `contract/examples/invalid/*.ndjson`:
  synthetic lines only, because this repo is public and real payloads carry
  prompt text.
- Rust: parse and validate a line against `envelope.json`. Keep unknown fields
  and unknown kinds ("skip and retain"). Return a typed error for each invalid
  case.
- heart: SPINE.md names `pulse/contract/` as the canon and keeps the prose.

### P2a. Emitters (Python): the envelope everywhere
One emitter module, `emitters/python/pulse_emit.py` (stdlib only, written by
hand like `envelope.json`), vendored verbatim into heart, arteries and
capillaries next to `vascular_paths.py`. Umbrella CI compares hashes. It stamps
`id` (uuid4), `schema_version`, `emitter_seq`, `context_id`, `project` and, when
a parent set them, `trace_id`/`parent_span_id`. It appends each line with one
`write(2)` on an `O_APPEND` descriptor, and announces each context once with
`context.started` (a marker file under `~/.vascular/state/pulse/contexts/`
stops short-lived hook processes announcing on every run). About 0.2 ms per
event.
- heart: `events.emit` goes through it; plexus and marrow inherit.
- arteries: `journal_append` goes through it, keeping its `id`/`run_id`
  behaviour. `turn.observed` gets `origin` `operator` in interactive sessions
  and `agent` under `ARTERIES_TRUST=untrusted`; `assistant.response` gets
  `agent`.
- capillaries: `spine.emit` goes through it.
- pulse: a Rust port of `vascular_paths` (`src/paths.rs`), tested against
  `contract/paths_vectors.json`. The umbrella runs the same vectors against the
  Python module.

Settled while planning: arteries' `drain` folds each sandbox inbox into the day
file and deletes the inbox, so the collector reads day files only.

### P2b. Traces, content and origin per call site (after P3)
- heart and plexus set `PULSE_TRACE_ID` and `PULSE_PARENT_SPAN` (and
  `PULSE_PROJECT`) at every process boundary, sandboxes included, and give
  start/finish pairs a `span_id`.
- Full content (prompts, responses, tool I/O, diffs) goes to artifacts. This
  needs P3's artifact store first.
- `origin` at every call site where content enters: WebFetch/WebSearch, the
  web lane, third-party GitHub text are `external`.

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
- Retention: cold packs, Parquet archive, pins, `retention.toml`, `pulse export`
  (dataset snapshots), and `pulse purge`.
- The PBS backup job and its restore test (by hand).

### Done when
These come from REFINED_DESIGN:

- Two identical legacy lines both survive.
- Replay after a crash makes no duplicates.
- A database outage leaves the spool intact, and it replays afterwards.
- Bad lines are inspectable.
- No pagination gaps under events with the same timestamp.
- Old and new queries agree on a sampled day.
- A 100x burst is measured.

### P6. Performance: bottlenecks and alternatives
- Stage spans so every second of a goal is attributed: waiting (seat, lane),
  sandbox start, model time, tool execution, verifier, acceptance, review, and
  harness overhead. On 2026-10-08, 28% of attempt wall time (4.2 h of 15.1 h)
  fell outside the agent roles and could not be attributed.
- A critical-path report per goal, and as a trend.
- Profiler captures linked to spans as artifacts: Nsight Systems for GPU
  processes (llama-server, embeddings, marrow), py-spy for Python, perf for
  Rust; Nsight Compute only for kernels this stack owns. `ncu` needs root on
  this box (RmProfilingAdminOnly=1), and that stays: profiling is an operator
  action, never an agent's.
- GPU sampling into `metric_samples`.
- Experiments: `experiment_id` and `variant` on context rows, plus a benchmark
  replay set built from landed features with known-good acceptance. An
  alternative counts as better only when it matches on quality (pass rate,
  review rejects, attempts), not just on time.
- marrow takes latency and cost as reward terms when it learns routing
  policies.

Then the umbrella's phase 3d: pulse deletes spool files behind its checkpoint
plus a grace period, and the fallbacks and direct readers go.
