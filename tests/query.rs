//! Query side of the pulse database. The Postgres tests in this file are
//! `#[ignore]`d and run with
//! `PULSE_TEST_DATABASE_URL='host=/var/run/postgresql dbname=pulse_test'
//! cargo test --test query -- --ignored` (none yet: later features add them).

use pulse::query::{EventFilter, is_hash};

fn pairs(q: &[(&str, &str)]) -> Vec<(String, String)> {
    q.iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[test]
fn empty_query_is_default() {
    let f = EventFilter::from_query(&[]).unwrap();
    assert_eq!(f, EventFilter::default());
    assert_eq!((f.after, f.limit), (0, 500));
}

#[test]
fn full_query_sets_every_field() {
    let f = EventFilter::from_query(&pairs(&[
        ("after", "10"),
        ("limit", "5000"),
        ("project", "p"),
        ("goal", "g"),
        ("episode", "e"),
        ("task", "t"),
        ("trace", "tr"),
        ("source", "s"),
        ("kind", "k"),
        ("since", "2026-10-13T08:00:00Z"),
    ]))
    .unwrap();
    let some = |s: &str| Some(s.to_string());
    assert_eq!(
        f,
        EventFilter {
            after: 10,
            limit: 5000,
            project: some("p"),
            goal: some("g"),
            episode: some("e"),
            task: some("t"),
            trace: some("tr"),
            source: some("s"),
            kind: some("k"),
            since: some("2026-10-13T08:00:00Z"),
        }
    );
    assert_eq!(
        EventFilter::from_query(&pairs(&[("limit", "1")]))
            .unwrap()
            .limit,
        1
    );
}

#[test]
fn bad_parameters_name_themselves() {
    let cases: &[(&[(&str, &str)], &str)] = &[
        (&[("after", "-1")], "after:"),
        (&[("after", "x")], "after:"),
        (&[("limit", "0")], "limit:"),
        (&[("limit", "5001")], "limit:"),
        (&[("limit", "abc")], "limit:"),
        (&[("since", "yesterday")], "since:"),
        (&[("since", "2026-10-13")], "since:"),
        (&[("project", "")], "project:"),
        (&[("limit", "10"), ("limit", "20")], "limit:"),
        (&[("foo", "1")], "foo:"),
    ];
    for (q, prefix) in cases {
        let err = EventFilter::from_query(&pairs(q)).unwrap_err();
        assert!(err.starts_with(prefix), "{q:?} gave {err:?}");
    }
    let err = |q: &[(&str, &str)]| EventFilter::from_query(&pairs(q)).unwrap_err();
    assert_eq!(
        err(&[("limit", "abc")]),
        "limit: must be between 1 and 5000"
    );
    assert_eq!(err(&[("foo", "1")]), "foo: unknown parameter");
    assert_eq!(
        err(&[("limit", "1"), ("limit", "2")]),
        "limit: given more than once"
    );
    assert_eq!(
        err(&[("after", "-1")]),
        "after: must be a non-negative integer"
    );
    assert_eq!(err(&[("project", "")]), "project: must not be empty");
}

#[test]
fn hashes_are_64_lowercase_hex() {
    let h = "0123456789abcdef".repeat(4);
    assert!(is_hash(&h));
    assert!(!is_hash(&h[..63]));
    assert!(!is_hash(&format!("{h}0")));
    assert!(!is_hash(&h.to_uppercase()));
    assert!(!is_hash(&format!("{}g", &h[..63])));
    assert!(!is_hash(""));
}
