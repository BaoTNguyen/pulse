use std::fs;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread::sleep;
use std::time::{Duration, Instant};

use pulse::http;

/// A fresh VASCULAR_HOME with a short name: the socket path must stay
/// under the ~108-byte sun_path limit.
fn scratch() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let home = std::env::temp_dir().join(format!("pcs-{}-{n}", std::process::id()));
    let _ = fs::remove_dir_all(&home);
    fs::create_dir_all(&home).unwrap();
    home
}

fn pulse(home: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_pulse"));
    c.args(args)
        .env("VASCULAR_HOME", home)
        .env_remove("EVENT_JOURNAL_DIR")
        .env(
            "PULSE_DATABASE_URL",
            "host=/nonexistent-pulse-socket dbname=pulse",
        );
    c
}

fn run(home: &Path, args: &[&str]) -> Output {
    pulse(home, args).output().unwrap()
}

/// Kills the collector and removes its home, even when an assertion panics.
struct Running(Child, PathBuf);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
        let _ = fs::remove_dir_all(&self.1);
    }
}

#[test]
fn once_replaces_stale_socket() {
    let home = scratch();
    let sock = home.join("state/pulse/pulse.sock");
    fs::create_dir_all(sock.parent().unwrap()).unwrap();
    fs::write(&sock, b"stale").unwrap();

    let out = run(&home, &["collect", "--once"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let meta = fs::symlink_metadata(&sock).unwrap();
    assert!(meta.file_type().is_socket());
    assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    fs::remove_dir_all(&home).unwrap();
}

#[test]
fn serves_while_running() {
    let home = scratch();
    let sock = home.join("state/pulse/pulse.sock");
    let child = pulse(&home, &["collect", "--interval", "60"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let _guard = Running(child, home.clone());

    let deadline = Instant::now() + Duration::from_secs(5);
    let health = loop {
        match http::get(&sock, "/health") {
            Ok(r) => break r,
            Err(e) if Instant::now() > deadline => panic!("socket never answered: {e}"),
            Err(_) => sleep(Duration::from_millis(50)),
        }
    };
    assert_eq!(health.0, 503, "{}", String::from_utf8_lossy(&health.1));

    assert_eq!(http::get(&sock, "/nope").unwrap().0, 404);

    let (status, body) = http::get(&sock, "/events?limit=x").unwrap();
    assert_eq!(status, 400);
    assert!(String::from_utf8_lossy(&body).contains("limit"));
}

#[test]
fn bind_error_is_fatal() {
    let home = scratch();
    let sock = home.join("state/pulse/pulse.sock");
    fs::create_dir_all(&sock).unwrap();
    fs::write(sock.join("keep"), b"x").unwrap();

    let out = run(&home, &["collect", "--once"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("pulse collect:"));
    fs::remove_dir_all(&home).unwrap();
}

#[test]
fn a_scratch_home_without_a_database_url_is_refused() {
    // PULSE-B14: it would write the real database's checkpoints
    let home = scratch();
    let out = pulse(&home, &["collect", "--once"])
        .env_remove("PULSE_DATABASE_URL")
        .output()
        .unwrap();
    let _ = fs::remove_dir_all(&home);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("PULSE_DATABASE_URL"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!home.join("state/pulse/pulse.sock").exists());
}
