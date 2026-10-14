use std::fs::{self, File};
use std::path::PathBuf;
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
