---
status: accepted
date: 2026-10-06
approved-by: operator
drafted-by: operator
retire-evidence: load tests and query profiles showing Postgres is the limit
switch: none
---
# Events live in a pulse database on the existing Postgres

## Context and Problem Statement

The journal was read whole every two seconds. At ~1,917 events a day, and
even a 100x estimate (~2.2/s average, ~100/s burst), the store type is not the
bottleneck; the repeated whole-file read is.

## Decision Drivers

* No new service to run; backup, drop and uninstall stay one database each

## Considered Options

* A separate `pulse` database on the existing Postgres server, socket peer auth (no password)
* SQLite (the first design's choice): superseded once Postgres was already running here
* Fluent Bit, Vector, Kafka or another database: not without a measured need

## Decision Outcome

Chosen option: "a `pulse` database on the existing Postgres", revisited only
after load tests show Postgres is the limit. Joins with arteries' tables go
through `postgres_fdw` when an analysis needs them.

### Consequences

* Good, because nothing new runs, and there is no secret to manage.
* Bad, because pulse depends on the machine's Postgres being up; the spool covers outages.

### Confirmation

`sql/schema.sql`, `src/pg.rs`.
