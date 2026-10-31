---
status: accepted
date: 2026-10-07
approved-by: operator
drafted-by: operator
retire-evidence: a change in requirement
switch: none
---
# Events record where content came from, not whether to trust it

## Context and Problem Statement

External text (web pages, search results) can trigger actions through prompt
injection. The record could drop it, or say where it came from.

## Decision Drivers

* Keep the evidence linking input to action
* External text is robustness training data

## Considered Options

* Each event carries `origin`: `operator`, `agent` or `external`; sandbox and network lane live on the context row
* Delete or exclude external content: loses the evidence of what an injection did

## Decision Outcome

Chosen option: "origin, not trust". Nothing is deleted for being external.

### Consequences

* Good, because injected behavior can be traced back to its input.
* Bad, because readers must filter on `origin` themselves.

### Confirmation

`src/envelope.rs` (`origin`), `contract/catalog.json`.
