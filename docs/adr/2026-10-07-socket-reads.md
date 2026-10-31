---
status: accepted
date: 2026-10-07
approved-by: operator
drafted-by: operator
retire-evidence: a change in threat or requirement; replay is not valid evidence (no adversarial tasks)
switch: none
---
# Reads go through an API on a 0600 Unix socket, never a TCP port

## Context and Problem Statement

Events from every project share one store. A sandboxed agent that can read it
can read other projects' work.

## Decision Drivers

* Sandboxed agents must not reach the store
* Readers must not depend on table layout

## Considered Options

* A small HTTP API on `~/.vascular/state/pulse/pulse.sock`, mode 0600; readers never touch tables
* A TCP port: Docker's `host.docker.internal` forwarding would let sandboxed agents reach it
* Readers querying tables directly: every schema change breaks them

## Decision Outcome

Chosen option: "socket API", because only the owning user can connect and the
schema can change underneath readers.

### Consequences

* Good, because sandbox isolation holds for the record too.
* Bad, because remote readers need a forwarded socket, not a port.

### Confirmation

`src/query.rs`, `src/serve.rs`.
