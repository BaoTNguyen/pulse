use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use pulse::envelope::is_timestamp;

/// A fresh, unique VASCULAR_HOME under the system temp dir.
fn scratch(test: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let home =
        std::env::temp_dir().join(format!("pulse-cli-{test}-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&home).unwrap();
    home
}

fn pulse(home: &PathBuf, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pulse"))
        .args(args)
        .env("VASCULAR_HOME", home)
        .env_remove("EVENT_JOURNAL_DIR")
        .env(
            "PULSE_DATABASE_URL",
            "host=/nonexistent-pulse-socket dbname=pulse",
        )
        .output()
        .unwrap()
}

#[test]
fn once_logs_store_error_and_leaves_spool() {
    let home = scratch("once");
    let spool = home.join("spool/events");
    fs::create_dir_all(spool.join("artifacts")).unwrap();
    let day = spool.join("20261013.ndjson");
    let line = b"{\"ts\":\"2026-10-13T00:00:00Z\",\"source\":\"test\",\"kind\":\"synthetic\"}\n";
    fs::write(&day, line).unwrap();
    let blob = spool.join("artifacts").join("ab".repeat(32));
    fs::write(&blob, b"synthetic blob").unwrap();

    let out = pulse(&home, &["collect", "--once"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let log = fs::read_to_string(home.join("log/pulse/collector.log")).unwrap();
    let err = log.lines().find(|l| l.contains("store error")).expect(&log);
    let ts = err.split(' ').next().unwrap();
    assert!(is_timestamp(ts) && ts.ends_with('Z'), "{ts}");

    assert_eq!(fs::read(&day).unwrap(), line);
    assert_eq!(fs::read(&blob).unwrap(), b"synthetic blob");
    assert!(!home.join("data/pulse/artifacts").exists());
    fs::remove_dir_all(&home).unwrap();
}

#[test]
fn second_instance_is_refused() {
    let home = scratch("lock");
    let state = home.join("state/pulse");
    fs::create_dir_all(&state).unwrap();
    let lock = File::create(state.join("collector.lock")).unwrap();
    lock.try_lock().unwrap();

    let out = pulse(&home, &["collect", "--once"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("pulse collect: already running"));
    drop(lock);
    fs::remove_dir_all(&home).unwrap();
}

#[test]
fn unknown_subcommand_prints_usage() {
    let home = scratch("usage");
    let out = pulse(&home, &["frobnicate"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("usage: pulse collect"));
    assert_eq!(
        pulse(&home, &["collect", "--interval", "nope"])
            .status
            .code(),
        Some(2)
    );
    fs::remove_dir_all(&home).unwrap();
}

fn plant(home: &Path, hash: &str) -> [PathBuf; 2] {
    let spool = home.join("spool/events/artifacts");
    let stored = home.join("data/pulse/artifacts").join(&hash[..2]);
    fs::create_dir_all(&spool).unwrap();
    fs::create_dir_all(&stored).unwrap();
    let paths = [spool.join(hash), stored.join(format!("{hash}.zst"))];
    for p in &paths {
        fs::write(p, b"a captured secret").unwrap();
    }
    paths
}

fn purged_event(home: &Path) -> serde_json::Value {
    let day = fs::read_dir(home.join("spool/events"))
        .unwrap()
        .flatten()
        .find(|e| e.file_name().to_string_lossy().ends_with(".ndjson"))
        .expect("an event day file");
    let line = fs::read_to_string(day.path()).unwrap();
    serde_json::from_str(line.trim()).unwrap()
}

#[test]
fn purge_removes_the_files_even_with_the_database_down_and_says_so() {
    let home = scratch("purge");
    let hash = "ab".repeat(32);
    let paths = plant(&home, &hash);

    let out = pulse(
        &home,
        &["purge", &hash, "--reason", "token in a transcript"],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("database unreachable"));
    assert!(paths.iter().all(|p| !p.exists()));

    let e = purged_event(&home);
    assert_eq!(
        (e["source"].as_str(), e["kind"].as_str()),
        (Some("pulse"), Some("artifact.purged"))
    );
    assert_eq!(e["payload"]["hash"], hash.as_str());
    assert_eq!(e["payload"]["reason"], "token in a transcript");
    assert_eq!(
        e["payload"]["removed"],
        serde_json::json!(["spool", "store"])
    );
    assert!(e["payload"]["row_deleted"].is_null());
    assert!(is_timestamp(e["ts"].as_str().unwrap()));
    fs::remove_dir_all(&home).unwrap();
}

#[test]
fn purge_needs_a_reason_and_real_hashes() {
    let home = scratch("purge-usage");
    assert_eq!(
        pulse(&home, &["purge", &"ab".repeat(32)]).status.code(),
        Some(2)
    );
    assert_eq!(
        pulse(&home, &["purge", "not-a-hash", "--reason", "x"])
            .status
            .code(),
        Some(2)
    );
    fs::remove_dir_all(&home).unwrap();
}

#[test]
#[ignore = "needs PULSE_TEST_DATABASE_URL"]
fn purge_deletes_the_row() {
    let url = std::env::var("PULSE_TEST_DATABASE_URL").unwrap();
    let mut db = postgres::Client::connect(&url, postgres::NoTls).unwrap();
    db.batch_execute(pulse::pg::SCHEMA).unwrap();
    let hash = "cd".repeat(32);
    db.execute(
        "INSERT INTO artifacts (hash, size, location) VALUES ($1, 17, $2) ON CONFLICT DO NOTHING",
        &[&hash, &format!("cd/{hash}.zst")],
    )
    .unwrap();
    let home = scratch("purge-pg");
    plant(&home, &hash);
    let out = Command::new(env!("CARGO_BIN_EXE_pulse"))
        .args(["purge", &hash, "--reason", "test"])
        .env("VASCULAR_HOME", &home)
        .env_remove("EVENT_JOURNAL_DIR")
        .env("PULSE_DATABASE_URL", &url)
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let left: i64 = db
        .query_one("SELECT count(*) FROM artifacts WHERE hash = $1", &[&hash])
        .unwrap()
        .get(0);
    assert_eq!(left, 0);
    assert_eq!(purged_event(&home)["payload"]["row_deleted"], true);
    fs::remove_dir_all(&home).unwrap();
}
