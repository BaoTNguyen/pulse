---
status: accepted
date: 2026-10-07
approved-by: operator
drafted-by: operator
retire-evidence: a change in requirement
switch: none
---
# Pulse holds the record; components keep projections; marrow reads snapshots

## Context and Problem Statement

Each component kept its own logs and tables of what happened, and marrow read
exports directly.

## Decision Drivers

* One record of what happened, so components can't disagree about it
* The learner must not be able to edit what judges it

## Considered Options

* Pulse holds the record; a component keeps only the state it decides from, as a projection it could rebuild; marrow learns from frozen, hash-named snapshots and writes only its own `training.*` events
* Each component keeps its own record (as before)

## Decision Outcome

Chosen option: "record in pulse". Memory itself (arteries' facts, capillaries'
prompts and skills) is data, not logs, and stays where it is.

### Consequences

* Good, because every model records the snapshot it learned from.
* Bad, because a component's projection can lag the record.

### Confirmation

`src/export.rs` (hash-named snapshots).
