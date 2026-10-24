//! `pulse export` against Postgres. Ignored by default:
//! `PULSE_TEST_DATABASE_URL='host=/var/run/postgresql dbname=pulse_test'
//! cargo test --locked --test export -- --ignored`

use std::fs::File;

use parquet::file::reader::{FileReader, SerializedFileReader};
use parquet::record::Field;
use postgres::{Client, NoTls};
use pulse::export::{Filter, export};

fn conninfo() -> String {
    std::env::var("PULSE_TEST_DATABASE_URL").expect("PULSE_TEST_DATABASE_URL")
}

struct SchemaGuard(String);

impl Drop for SchemaGuard {
    fn drop(&mut self) {
        if let Ok(mut c) = Client::connect(&conninfo(), NoTls) {
            let _ = c.batch_execute(&format!("DROP SCHEMA IF EXISTS {} CASCADE", self.0));
        }
    }
}

#[test]
#[ignore = "needs PULSE_TEST_DATABASE_URL"]
fn exports_the_filtered_events_into_one_content_addressed_parquet() {
    let schema = format!("pulse_test_export_{}", std::process::id());
    let mut c = Client::connect(&conninfo(), NoTls).unwrap();
    c.batch_execute(&format!(
        "CREATE SCHEMA {schema}; SET search_path TO {schema}"
    ))
    .unwrap();
    let _guard = SchemaGuard(schema);
    c.batch_execute(pulse::pg::SCHEMA).unwrap();
    for (i, (source, kind, ts)) in [
        ("heart", "role.finished", "2026-10-01T00:00:00Z"),
        ("heart", "episode.finished", "2026-10-02T00:00:00Z"),
        ("arteries", "turn.observed", "2026-10-03T00:00:00Z"),
        ("heart", "role.finished", "2026-09-01T00:00:00Z"),
    ]
    .iter()
    .enumerate()
    {
        c.execute(
            "INSERT INTO events (id, ts, source, kind, role, duration_ms, payload, spool_file, spool_offset) \
             VALUES (gen_random_uuid(), $1::text::timestamptz, $2, $3, $4, $5, $6, 'x', $7)",
            &[&ts, source, kind, &(i == 0).then_some("implement"), &(i == 0).then_some(12.5f64),
              &serde_json::json!({"i": i}), &(i as i64)],
        )
        .unwrap();
    }
    let dir = std::env::temp_dir().join(format!("pulse-export-{}", std::process::id()));
    let f = Filter {
        since: Some("2026-09-15T00:00:00Z".into()),
        sources: vec!["heart".into()],
        ..Filter::default()
    };
    let snap = export(&mut c, &f, &dir).unwrap();
    assert_eq!(snap.rows, 2);
    assert_eq!(snap.path, dir.join(format!("{}.parquet", snap.hash)));
    assert!(dir.join(format!("{}.json", snap.hash)).exists());

    let reader = SerializedFileReader::new(File::open(&snap.path).unwrap()).unwrap();
    let rows: Vec<_> = reader
        .get_row_iter(None)
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let get = |r: &parquet::record::Row, name: &str| {
        r.get_column_iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| v.clone())
            .unwrap()
    };
    assert_eq!(get(&rows[0], "kind"), Field::Str("role.finished".into()));
    assert_eq!(get(&rows[0], "role"), Field::Str("implement".into()));
    assert_eq!(get(&rows[0], "duration_ms"), Field::Double(12.5));
    assert_eq!(get(&rows[1], "role"), Field::Null);
    assert_eq!(
        get(&rows[1], "ts"),
        Field::Str("2026-10-02T00:00:00.000000Z".into())
    );

    // the same rows make the same file: a model can cite the hash
    assert_eq!(export(&mut c, &f, &dir).unwrap().hash, snap.hash);
    std::fs::remove_dir_all(&dir).unwrap();
}
