---
status: accepted
date: 2026-10-06
approved-by: operator
drafted-by: operator
retire-evidence: maintenance cost of Rust here exceeding its benefit
switch: none
---
# The collector is Rust; emitters stay in each component's language

## Context and Problem Statement

Pulse is greenfield, and the wider harness may move to Rust. Emitters live
inside Python components today.

## Decision Drivers

* Throughput is not the reason: the measured load is small
* Emitters must not pull a foreign runtime into each component

## Considered Options

* A Rust collector (1.99.0, pinned); emitters in the host language (stdlib Python today); the contract is a line format, not a library
* A Rust-only emitter ecosystem: conditional, needs evidence first
* A Python collector: equal at this load ("Python versus Rust… not the present bottleneck"); Rust because the harness may move to it

## Decision Outcome

Chosen option: "Rust collector, host-language emitters", as a maintainability
and deployment choice, not a throughput prerequisite.

### Consequences

* Good, because a component port to Rust changes only its emitter.
* Bad, because the line format must be kept identical across languages by tests.

### Confirmation

`rust-toolchain.toml`, `src/envelope.rs`, `emitters/python/test_pulse_emit.py`.
