use pulse::catalog::catalog;
use pulse::envelope::{is_kind, is_name, parse_line};
use serde_json::Value;
use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::path::Path;

const CLASSES: [&str; 9] = [
    "identity",
    "time",
    "measure",
    "label",
    "decision",
    "content",
    "provenance",
    "security",
    "diagnostic",
];

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn names_and_kinds_are_well_formed() {
    for k in &catalog().kinds {
        assert!(is_kind(&k.kind), "bad kind {:?}", k.kind);
        assert!(is_name(&k.source), "bad source {:?}", k.source);
    }
}

#[test]
fn entries_are_unique_and_sorted() {
    let keys: Vec<_> = catalog()
        .kinds
        .iter()
        .map(|k| (k.source.as_str(), k.kind.as_str()))
        .collect();
    assert_eq!(keys.iter().collect::<HashSet<_>>().len(), keys.len());
    assert!(keys.is_sorted(), "entries not sorted by (source, kind)");
}

#[test]
fn every_observed_pair_is_catalogued() {
    let observed: Value =
        serde_json::from_str(&fs::read_to_string(root().join("contract/observed.json")).unwrap())
            .unwrap();
    for o in observed["kinds"].as_array().unwrap() {
        let (s, k) = (o["source"].as_str().unwrap(), o["kind"].as_str().unwrap());
        let info = catalog()
            .lookup(s, k)
            .unwrap_or_else(|| panic!("{s} {k} missing"));
        assert_eq!(
            info.envelope_fields,
            strings(&o["envelope_fields"]),
            "{s} {k}"
        );
        let observed_payload: BTreeSet<String> =
            strings(&o["payload_fields"]).into_iter().collect();
        let catalogued: BTreeSet<String> = info.payload_fields.keys().cloned().collect();
        assert_eq!(catalogued, observed_payload, "{s} {k}");
    }
}

#[test]
fn classes_are_the_nine_and_every_field_has_one() {
    let names: BTreeSet<&str> = catalog().classes.keys().map(String::as_str).collect();
    assert_eq!(names, CLASSES.into_iter().collect());
    for k in &catalog().kinds {
        for (field, class) in &k.payload_fields {
            assert!(
                CLASSES.contains(&class.as_str()),
                "{} {} {field}: unknown class {class:?}",
                k.source,
                k.kind
            );
        }
    }
}

#[test]
fn context_started_for_every_component() {
    for s in ["heart", "arteries", "capillaries", "plexus", "marrow"] {
        assert!(catalog().lookup(s, "context.started").is_some(), "{s}");
    }
}

#[test]
fn marrow_training_kinds() {
    for k in ["training.started", "training.progress", "training.finished"] {
        let info = catalog().lookup("marrow", k).unwrap();
        let keys: Vec<&str> = info.payload_fields.keys().map(String::as_str).collect();
        assert_eq!(keys, ["loss", "stage", "step"], "{k}");
    }
}

#[test]
fn valid_examples_are_catalogued_or_examples() {
    let mut files: Vec<_> = fs::read_dir(root().join("contract/examples/valid"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "ndjson"))
        .collect();
    files.sort();
    assert!(!files.is_empty());
    for path in files {
        for line in fs::read_to_string(&path).unwrap().lines() {
            if line.trim().is_empty() {
                continue;
            }
            let ev = parse_line(line).unwrap_or_else(|e| panic!("{path:?}: {e:?}: {line}"));
            assert!(
                catalog().lookup(&ev.source, &ev.kind).is_some() || ev.kind.starts_with("example."),
                "{path:?}: {} {} not catalogued",
                ev.source,
                ev.kind
            );
        }
    }
}

#[test]
fn descriptions_are_non_empty() {
    for k in &catalog().kinds {
        assert!(!k.description.trim().is_empty(), "{} {}", k.source, k.kind);
    }
}

#[test]
fn unknown_pair_is_none() {
    assert!(catalog().lookup("demo", "example.nothing.here").is_none());
}
