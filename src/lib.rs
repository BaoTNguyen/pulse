//! Pulse: the vascular stack's event journal.
//!
//! `contract/envelope.json` is the line format every component writes;
//! `PLAN.md` is the build order.

pub mod artifacts;
pub mod catalog;
pub mod collect;
pub mod envelope;
pub mod http;
pub mod ingest;
pub mod paths;
pub mod pg;
pub mod query;
pub mod serve;
pub mod spool;
pub mod store;
