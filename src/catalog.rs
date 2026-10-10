//! The kind catalog: `contract/catalog.json` describes every known
//! (source, kind) and classifies its payload fields. It describes, it does
//! not gate: an unknown kind is still a valid event.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::OnceLock;

const CATALOG: &str = include_str!("../contract/catalog.json");

#[derive(Debug, Deserialize)]
pub struct Catalog {
    pub about: String,
    pub classes: BTreeMap<String, String>,
    pub kinds: Vec<KindInfo>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct KindInfo {
    pub source: String,
    pub kind: String,
    pub description: String,
    pub envelope_fields: Vec<String>,
    pub payload_fields: BTreeMap<String, String>,
}

pub fn catalog() -> &'static Catalog {
    static C: OnceLock<Catalog> = OnceLock::new();
    C.get_or_init(|| serde_json::from_str(CATALOG).expect("catalog.json is not valid"))
}

impl Catalog {
    /// `None` for a kind the catalog does not know; that is not an error.
    pub fn lookup(&self, source: &str, kind: &str) -> Option<&KindInfo> {
        // ponytail: linear scan over ~100 entries; binary search on the sorted list if it grows
        self.kinds
            .iter()
            .find(|k| k.source == source && k.kind == kind)
    }
}
