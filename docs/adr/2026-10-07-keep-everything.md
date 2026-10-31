---
status: accepted
date: 2026-10-07
approved-by: operator
drafted-by: operator
retire-evidence: storage cost; this machine had 160 GB free on 2026-10-08, pulse needs ~20 GB/year at 100x
switch: none
---
# Keep every event and artifact; staleness is handled at export

## Context and Problem Statement

Episodes can't be regenerated: models and harness move on. Compressed history
is cheap (193 MB of transcripts packs to 22.9 MB).

## Decision Drivers

* Training and audits need the full record, including what is now stale
* A captured secret must still be removable everywhere

## Considered Options

* Keep everything; filter or weight by date and provenance at export; expire only regenerable or redundant data
* Sample the journal (for example successful tool events): not until audit and debugging use cases are specified
* Delete external or stale content: it is evidence linking input to action

## Decision Outcome

Chosen option: "keep everything". The one exception is a captured secret:
`pulse purge` removes named artifacts everywhere and records that it did.

### Consequences

* Good, because any later question can be asked of the full history.
* Bad, because disk is the constraint; cold packs and pins are deferred until it bites.

### Confirmation

`src/purge.rs`, `src/export.rs`.
