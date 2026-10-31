-- pulse, schema 1: the `pulse` database on the local Postgres server.
--
-- Written by hand and reviewed before anything is built on it; the collector
-- applies it (it is idempotent) and only ever appends. Columns follow
-- contract/envelope.json: every envelope field has a column, the payload
-- stays jsonb, and top-level fields the envelope doesn't name go to `extra`,
-- because the contract says unknown fields are kept, never dropped.

-- One row per process context (`context.started`): which code, host, sandbox
-- and lane produced an event. Answers "what exactly ran".
CREATE TABLE IF NOT EXISTS contexts (
    id            text PRIMARY KEY,           -- context_id, 16 hex
    first_seen    timestamptz NOT NULL,       -- ts of the context.started line
    component     text,
    git_commit    text,
    host          text,
    sandbox       text,                       -- docker | host
    lane          text,                       -- api | web | ''
    experiment_id text,
    variant       text,
    payload       jsonb NOT NULL              -- the whole context, new keys included
);

CREATE TABLE IF NOT EXISTS events (
    seq            bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,  -- ingestion order, the read cursor
    id             uuid NOT NULL UNIQUE,      -- the line's id; uuid5(file:offset) for legacy lines without one
    ts             timestamptz NOT NULL,
    ingested_at    timestamptz NOT NULL DEFAULT now(),
    schema_version integer,                   -- absent on legacy lines
    source         text NOT NULL,
    kind           text NOT NULL,
    project        text,
    trace_id       text,
    span_id        text,
    parent_span_id text,
    context_id     text,                      -- no FK: a context's line may land in a later batch
    emitter_seq    bigint,
    goal_id        text,
    feature_id     text,
    task_id        text,
    episode_id     text,
    run_id         text,
    turn_id        text,
    role           text,
    duration_ms    double precision,
    origin         text CHECK (origin IN ('operator', 'agent', 'external')),
    payload        jsonb,
    extra          jsonb,                     -- top-level fields the envelope doesn't name
    spool_file     text NOT NULL,             -- where the line was read: day file name
    spool_offset   bigint NOT NULL            -- and the byte offset of its first byte
);
CREATE INDEX IF NOT EXISTS events_ts ON events (ts);
CREATE INDEX IF NOT EXISTS events_source_kind_ts ON events (source, kind, ts);
CREATE INDEX IF NOT EXISTS events_project_ts ON events (project, ts);
CREATE INDEX IF NOT EXISTS events_trace ON events (trace_id) WHERE trace_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS events_goal ON events (goal_id) WHERE goal_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS events_task ON events (task_id) WHERE task_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS events_episode ON events (episode_id) WHERE episode_id IS NOT NULL;

-- Full content (prompts, responses, tool I/O, diffs, transcripts), named by
-- the sha256 of its uncompressed bytes. Events reference it by hash and stay
-- small. Emitters drop raw `<hash>` files into a spool or inbox `artifacts/`
-- directory; the collector verifies the name, stores the bytes zstd-compressed
-- under data/pulse/artifacts/<first 2 hex>/<hash>.zst and records it here.
CREATE TABLE IF NOT EXISTS artifacts (
    hash       text PRIMARY KEY CHECK (hash ~ '^[0-9a-f]{64}$'),
    size       bigint NOT NULL,               -- uncompressed bytes
    media_type text,
    stored_at  timestamptz NOT NULL DEFAULT now(),
    location   text NOT NULL                  -- path relative to data/pulse/artifacts
);

-- How far each day file has been read. Updated in the same transaction as
-- the batch it covers, so a crash replays at most one batch, and the replay
-- hits events.id's unique constraint instead of duplicating.
CREATE TABLE IF NOT EXISTS checkpoints (
    spool_file   text PRIMARY KEY,
    byte_offset  bigint NOT NULL,             -- just past the last complete line read
    committed_at timestamptz NOT NULL DEFAULT now()
);

-- Lines that couldn't be read as envelopes. Never blocks the collector;
-- inspectable, and replayable once the cause is fixed.
CREATE TABLE IF NOT EXISTS quarantine (
    spool_file     text NOT NULL,
    byte_offset    bigint NOT NULL,
    error          text NOT NULL,             -- which rule the line broke
    raw            bytea NOT NULL,            -- the first 4 KiB of the line
    quarantined_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (spool_file, byte_offset)
);

-- One row per unit of work: a `<x>.started` and the `<x>.finished`,
-- `<x>.failed` or `<x>.landed` sharing its span_id (features end landed or
-- failed, never finished). Emitters start sending span ids in P2b.
CREATE OR REPLACE VIEW spans AS
SELECT s.span_id,
       s.trace_id,
       s.parent_span_id,
       left(s.kind, length(s.kind) - length('.started')) AS name,
       s.source,
       s.project,
       s.ts AS started,
       f.ts AS finished,
       extract(epoch FROM f.ts - s.ts) * 1000 AS duration_ms
FROM events s
LEFT JOIN events f
       ON f.span_id = s.span_id
      AND f.kind IN (left(s.kind, length(s.kind) - length('.started')) || '.finished',
                     left(s.kind, length(s.kind) - length('.started')) || '.failed',
                     left(s.kind, length(s.kind) - length('.started')) || '.landed')
WHERE s.span_id IS NOT NULL
  AND s.kind LIKE '%.started';

-- metric_samples (CPU/GPU sampling) is planned for GPU sampling but has no writer
-- yet; it is added with the sampler, not before.
