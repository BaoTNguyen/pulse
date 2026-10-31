---
status: accepted
date: 2026-10-06
approved-by: operator
drafted-by: operator
retire-evidence: a change in requirement; replay is not valid evidence
switch: none
---
# Pulse is its own repo: writes stay local appends, catalog and reads are central

## Context and Problem Statement

Every repo appended to a shared journal, and every reader parsed it again.
The stack needed one record and one way to read it, in some repo.

## Decision Drivers

* Logging is on every hot path and must never fail or block the work
* The self-improving part must not be able to edit the record used to judge it
* A substrate shouldn't move as fast as a product

## Considered Options

* A separate repo: emitters append locally, a collector centralizes catalog, storage and reads
* Inside marrow: the self-improving component, not installed everywhere, heavy training deps
* Inside plexus: one of five writers, work outside a goal would fall off the record, fastest-changing repo
* A central write service: every emitter depends on it being up, and must spool to disk anyway

## Decision Outcome

Chosen option: "a separate repo", centralizing the catalog and the reads, not
the writes: one place to add a kind, one query surface, one retention policy.

### Consequences

* Good, because emission stays a local append that can't fail.
* Bad, because the spool and the store are two places until ingest catches up.

### Confirmation

`contract/catalog.json`, `src/collect.rs`; emitters in `emitters/python/pulse_emit.py`.
