use pulse::envelope::{EnvelopeError, parse_line};
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

const CODES: [&str; 9] = [
    "not_json",
    "not_object",
    "missing_field",
    "wrong_type",
    "bad_timestamp",
    "bad_uuid",
    "bad_name",
    "bad_kind",
    "bad_origin",
];

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn ndjson_files(dir: &str) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(root().join(dir))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "ndjson"))
        .collect();
    files.sort();
    files
}

fn lines(path: &Path) -> Vec<(usize, String)> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(i, l)| (i + 1, l.to_owned()))
        .collect()
}

fn err(line: &str) -> EnvelopeError {
    parse_line(line).unwrap_err()
}

fn code(line: &str) -> &'static str {
    parse_line(line).unwrap_err().code()
}

const BASE: &str = r#""ts":"2026-01-01T00:00:00Z","source":"demo","kind":"example.thing.done""#;

fn with(extra: &str) -> String {
    format!("{{{BASE}{extra}}}")
}

#[test]
fn valid_examples_parse_and_are_observed() {
    let observed: Value =
        serde_json::from_str(&fs::read_to_string(root().join("contract/observed.json")).unwrap())
            .unwrap();
    let pairs: HashSet<(&str, &str)> = observed["kinds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| (k["source"].as_str().unwrap(), k["kind"].as_str().unwrap()))
        .collect();
    let mut count = 0;
    for file in ndjson_files("contract/examples/valid") {
        for (n, line) in lines(&file) {
            let ev = parse_line(&line).unwrap_or_else(|e| panic!("{}:{n}: {e}", file.display()));
            assert!(
                pairs.contains(&(ev.source.as_str(), ev.kind.as_str()))
                    || ev.kind.starts_with("example."),
                "{}:{n}: ({}, {}) not in observed.json",
                file.display(),
                ev.source,
                ev.kind
            );
            count += 1;
        }
    }
    assert!(count >= 20, "only {count} valid lines");
}

#[test]
fn invalid_examples_fail_with_their_code() {
    let mut seen = HashSet::new();
    for file in ndjson_files("contract/examples/invalid") {
        let stem = file.file_stem().unwrap().to_str().unwrap().to_owned();
        assert!(
            CODES.contains(&stem.as_str()),
            "unexpected file {}",
            file.display()
        );
        let ls = lines(&file);
        assert!(ls.len() >= 2, "{} needs at least 2 lines", file.display());
        for (n, line) in ls {
            match parse_line(&line) {
                Ok(_) => panic!("{}:{n}: parsed but should fail", file.display()),
                Err(e) => assert_eq!(e.code(), stem, "{}:{n}: {e}", file.display()),
            }
        }
        seen.insert(stem);
    }
    for c in CODES {
        assert!(seen.contains(c), "no invalid/{c}.ndjson");
    }
}

#[test]
fn unknown_field_retained_and_unknown_kind_accepted() {
    let ev = parse_line(&with(r#","flavor":"mint""#)).unwrap();
    assert_eq!(ev.fields["flavor"], "mint");
    assert_eq!(ev.fields["kind"], "example.thing.done");
    assert_eq!(ev.kind, "example.thing.done");
    assert_eq!(ev.source, "demo");
    assert_eq!(ev.ts, "2026-01-01T00:00:00Z");
}

#[test]
fn null_optional_is_absent() {
    parse_line(&with(r#","payload":null,"emitter_seq":null"#)).unwrap();
}

#[test]
fn timestamps() {
    for ts in [
        "2026-01-01T00:00:00Z",
        "2026-01-01T00:00:00+00:00",
        "2026-01-01T00:00:00.5Z",
        "2026-01-01T00:00:00.123456789-23:59",
    ] {
        let line = format!(r#"{{"ts":"{ts}","source":"demo","kind":"example.a.b"}}"#);
        parse_line(&line).unwrap_or_else(|e| panic!("{ts}: {e}"));
    }
    let bad = r#"{"ts":"2026-13-01T00:00:00Z","source":"demo","kind":"example.a.b"}"#;
    assert_eq!(code(bad), "bad_timestamp");
}

#[test]
fn missing_ts_and_negative_seq() {
    assert_eq!(
        err(r#"{"source":"demo","kind":"example.a.b"}"#),
        (EnvelopeError::MissingField("ts".into()))
    );
    assert_eq!(code(&with(r#","emitter_seq":-1"#)), "wrong_type");
}

#[test]
fn fields_checked_in_declared_order() {
    // envelope.json order, not alphabetical: source before kind, id before duration_ms.
    assert_eq!(
        err(r#"{"ts":"2026-01-01T00:00:00Z","source":"Bad","kind":"Bad"}"#),
        (EnvelopeError::BadName("source".into()))
    );
    assert_eq!(
        err(&with(r#","duration_ms":-1,"id":"nope""#)),
        (EnvelopeError::BadUuid("id".into()))
    );
}

#[test]
fn display_names_the_field() {
    let msg = parse_line(&with(r#","emitter_seq":-1"#))
        .unwrap_err()
        .to_string();
    assert!(msg.starts_with("wrong_type"), "{msg}");
    assert!(msg.contains("\"emitter_seq\""), "{msg}");
}
