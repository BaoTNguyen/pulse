# Decisions

Why pulse is shaped the way it is, one file per decision in trimmed MADR 4.
Format and rules: [vascular/docs/adr](https://github.com/BaoTNguyen/vascular/tree/main/docs/adr).
The "In force" table is generated from each record's front matter: run
`python3 docs/adr/index.py` after adding or superseding one. CI fails when the
table is stale.

## In force

<!-- adr-index:start -->
| Decision | Date |
|---|---|
| [Reads go through an API on a 0600 Unix socket, never a TCP port](2026-10-07-socket-reads.md) | 2026-10-07 |
| [Pulse holds the record; components keep projections; marrow reads snapshots](2026-10-07-record-vs-state.md) | 2026-10-07 |
| [Events record where content came from, not whether to trust it](2026-10-07-origin-not-trust.md) | 2026-10-07 |
| [Keep every event and artifact; staleness is handled at export](2026-10-07-keep-everything.md) | 2026-10-07 |
| [The collector is Rust; emitters stay in each component's language](2026-10-06-rust-collector.md) | 2026-10-06 |
| [Events live in a pulse database on the existing Postgres](2026-10-06-postgres-store.md) | 2026-10-06 |
| [Pulse is its own repo: writes stay local appends, catalog and reads are central](2026-10-06-own-repo-local-writes.md) | 2026-10-06 |
<!-- adr-index:end -->

## Stack decisions that bind pulse

| Decision | Date |
|---|---|
| [A feature lives in the repo whose question it answers](https://github.com/BaoTNguyen/vascular/blob/main/docs/adr/2026-07-20-ownership-by-question.md) | 2026-07-20 |
| [Seams are pinned by contract tests in each repo](https://github.com/BaoTNguyen/vascular/blob/main/docs/adr/2026-07-20-contract-tests.md) | 2026-07-20 |
| [GitHub enforces PR plus CI on main](https://github.com/BaoTNguyen/vascular/blob/main/docs/adr/2026-10-30-protect-main.md) | 2026-10-30 |
| [Plan text reaches pulse as artifacts](https://github.com/BaoTNguyen/vascular/blob/main/docs/adr/2026-10-30-planning-data-in-pulse.md) | 2026-10-30 |
