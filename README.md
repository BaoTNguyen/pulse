# About

pulse is part of [vascular](https://github.com/BaoTNguyen/vascular), the software factory
monorepo. This repo acts as the event log every component writes to and every reader
queries.

One indexed store for every event the stack emits, and one API the rest of the
stack reads it through.

Status: running. `pulse collect` ingests the journal spool into Postgres and
stores artifacts; `pulse events`, `tail`, `export`, `purge` and `health` read
and manage it. Why it is built this way: [`docs/adr/`](docs/adr/).

## Intent

Every repo appends events to a local NDJSON spool. Before pulse, every reader
scanned the whole journal: fine at 2,000 events a day, not at the 10-100x a
working software factory runs, and the scan was the slowest thing in the
dashboard. Pulse owns the event-journal contract and turns local append spools
into something queryable without making writes depend on a service.

The goal is not more logging. It is the same logging, answerable:

- "what happened in this run, in order, with times" — in one query, not a scan
- "is the factory worse than yesterday" — without reading yesterday
- "why did this feature escalate" — from the record, not from a screen recording

## Where it sits

Every component appends its
events to a local spool with a small vendored emitter; pulse collects the
spool, stores it, and is what every reader queries.

Emitters stay local and native to their component's language, appending to
NDJSON without a database credential or live Pulse service. A logging outage
must never stop a factory floor of agents.

## What it owns

- the catalog of event kinds ([`contract/catalog.json`](contract/catalog.json))
- the collector: tails the NDJSON spool, validates, writes the store
- the store: the existing Postgres database, indexed by actual query patterns
- the query API every reader uses instead of touching files
- retention: what gets pruned, archived, rolled up, and when

## What it must never own

Domain knowledge. Pulse knows `ts`, `source`, `kind`, correlation ids, and
payload-as-opaque-JSON. It does not know what an episode is, how a reward is
computed, what `scope_denied` means, or how goals decompose into features.

The moment it knows those things it becomes a second copy of the component
that does, and the two will be reconciled forever. Interpretation stays in the
repo that owns the concept, and that repo reads pulse instead of scanning files.

It also depends on nothing in the stack. A substrate that imports one of its
clients is not a substrate.

## Why a separate repo

The extraction test is whether more than one repo needs the same non-trivial
code. Writers fail that test — appending a line is fifteen lines and five
copies cost nothing. Readers pass it: several components need indexed queries,
correlation and retention, and one of them was importing a workflow
orchestrator to read a log file.

The indexed store and collector cannot stay vendored. Five independent
implementations of the event contract and retention policy would drift.
