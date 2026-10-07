# pulse

Centralized logging for the agent stack: one indexed store for every event the
stack emits, and one API the rest of the stack reads it through.

Status: design only. Nothing here runs yet. `REFINED_DESIGN.md` is the current
architecture and migration plan; `DESIGN.md` preserves the original measurements
but its SQLite/read-side-only choices are superseded.

## Intent

Today every repo appends events to a shared NDJSON journal and every reader
scans the whole thing. That works at 2,000 events a day. The working software
factory will run 10-100x that, and the scan is already the slowest thing in the
dashboard. Pulse owns the event-journal contract and turns local append spools
into something queryable without making writes depend on a service.

The goal is not more logging. It is the same logging, answerable:

- "what happened in this run, in order, with times" — in one query, not a scan
- "is the factory worse than yesterday" — without reading yesterday
- "why did this feature escalate" — from the record, not from a screen recording

## Where it sits

```text
capillaries  prompt/skill retrieval          ─┐
arteries     memory + trace substrate         │  emit events (vendored, ~40 lines each)
heart        orchestration + environment      │
plexus       goal decomposition + acceptance ─┘
                     │
                  pulse  ← collector, store, query API, catalog
                     │
             plexus UI · heart pulse CLI · marrow exports
```

Pulse owns the event contract and central collection, storage, and reads.
Emitters stay local and native to their component's language, appending to
NDJSON without a database credential or live Pulse service. A logging outage
must never stop a factory floor of agents.

## What it owns

- the catalog of event kinds (machine-readable, currently prose in `heart/SPINE.md`)
- the collector: tails the NDJSON spool, validates, writes the store
- the store: the existing Postgres database, indexed by actual query patterns
- the query API every reader uses instead of touching files
- retention: what gets pruned, archived, rolled up, and when

## What it must never own

Domain knowledge. Pulse knows `ts`, `source`, `kind`, correlation ids, and
payload-as-opaque-JSON. It does not know what an episode is, how a reward is
computed, what `scope_denied` means, or how goals decompose into features.

The moment it knows those things it becomes a second heart, and the two will be
reconciled forever. Interpretation stays in the repo that owns the concept:
`pulse insights` and `pulse health` in heart keep their episode semantics and
read pulse instead of scanning files.

It also depends on nothing in the stack. A substrate that imports heart is not
a substrate.

## Why a separate repo

The extraction test is whether more than one repo needs the same non-trivial
code. Writers fail that test — appending a line is fifteen lines and five
copies cost nothing. Readers pass it: plexus, marrow and heart's CLI all need
indexed queries, correlation and retention, and today plexus imports a workflow
orchestrator to read a log file.

The indexed store and collector cannot stay vendored. Five independent
implementations of the event contract and retention policy would drift.
