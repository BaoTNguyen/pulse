//! `pulse collect`: drain the spool into Postgres, one round per interval.
//! `pulse events` / `pulse tail` / `pulse health`: clients of the collector's query socket.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{ErrorKind, Write};
use std::path::Path;
use std::process::exit;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use pulse::paths::{self, process_env};
use pulse::pg::PgStore;
use pulse::serve::{self, PgBackend};
use pulse::{artifacts, collect, http};

const USAGE: &str = "usage: pulse collect [--once] [--interval SECS]\n\
  --once            run one round and exit\n\
  --interval SECS   seconds between rounds (default 2)\n\
\x20      pulse events [--after N] [--order asc|desc] [--before N] [--limit N] [--project P] [--goal G] [--episode E] [--task T] [--trace T] [--source S] [--kind K] [--since TS]\n\
\x20      pulse tail [--limit N] [--project P] [--goal G] [--episode E] [--task T] [--trace T] [--source S] [--kind K] [--since TS]\n\
\x20      pulse health";

const TAIL_FLAGS: [&str; 9] = [
    "limit", "project", "goal", "episode", "task", "trace", "source", "kind", "since",
];

const EVENT_FLAGS: [&str; 12] = [
    "after", "order", "before", "limit", "project", "goal", "episode", "task", "trace", "source",
    "kind", "since",
];

fn usage() -> ! {
    eprintln!("{USAGE}");
    exit(2)
}

fn fail(e: impl std::fmt::Display) -> ! {
    eprintln!("pulse collect: {e}");
    exit(1)
}

/// Howard Hinnant's civil_from_days: days since 1970-01-01 -> (y, m, d).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

fn timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    let s = secs.rem_euclid(86_400);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        s / 3600,
        s / 60 % 60,
        s % 60
    )
}

fn log(file: &mut File, msg: &str) {
    // ponytail: a failed log write is dropped; the collector keeps running.
    let _ = writeln!(file, "{} {msg}", timestamp());
}

fn open_in(path: &Path, append: bool) -> std::io::Result<File> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    OpenOptions::new()
        .create(true)
        .append(append)
        .write(!append)
        .open(path)
}

fn conninfo() -> String {
    process_env("PULSE_DATABASE_URL")
        .unwrap_or_else(|| "host=/var/run/postgresql dbname=pulse".to_string())
}

fn round(store: &mut Option<PgStore>, log_file: &mut File, journal: &Path, artifact_root: &Path) {
    if store.is_none() {
        match PgStore::connect(&conninfo()) {
            Ok(s) => *store = Some(s),
            Err(e) => return log(log_file, &format!("store error: {e}")),
        }
    }
    let s = store.as_mut().expect("connected above");
    let result = collect::pass(s, journal, 1000)
        .and_then(|p| artifacts::intake(s, journal, artifact_root).map(|a| (p, a)));
    match result {
        Err(e) => {
            log(log_file, &format!("store error: {e}"));
            *store = None;
        }
        Ok((p, a)) => {
            let counts = [
                p.events,
                p.quarantined,
                p.contexts,
                a.stored,
                a.duplicate,
                a.quarantined,
            ];
            if counts.iter().any(|&n| n != 0) {
                log(
                    log_file,
                    &format!(
                        "events={} quarantined={} contexts={} artifacts={} artifact_duplicates={} artifact_quarantined={}",
                        p.events, p.quarantined, p.contexts, a.stored, a.duplicate, a.quarantined
                    ),
                );
            }
        }
    }
}

/// GETs `target` from the collector's socket; on 200 returns the parsed body.
fn query(cmd: &str, target: &str) -> serde_json::Value {
    let fail = |e: &dyn std::fmt::Display| -> ! {
        eprintln!("pulse {cmd}: {e}");
        exit(1)
    };
    let sock =
        paths::path(&process_env, "state", "pulse", &["pulse.sock"]).unwrap_or_else(|e| fail(&e));
    let (status, body) = match http::get(&sock, target) {
        Ok(r) => r,
        Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::ConnectionRefused) => {
            eprintln!(
                "pulse: the collector isn't running (no socket at {})",
                sock.display()
            );
            exit(1)
        }
        Err(e) => fail(&e),
    };
    if status != 200 {
        fail(&format_args!("{status} {}", String::from_utf8_lossy(&body)));
    }
    serde_json::from_slice(&body).unwrap_or_else(|e| fail(&e))
}

/// `--flag value` pairs, in command-line order; anything else prints the usage.
fn filters<'a>(args: &'a [String], flags: &[&str]) -> Vec<(&'a str, &'a String)> {
    let mut pairs = Vec::new();
    let mut rest = args.iter();
    while let Some(a) = rest.next() {
        let key = a
            .strip_prefix("--")
            .filter(|k| flags.contains(k))
            .unwrap_or_else(|| usage());
        pairs.push((key, rest.next().unwrap_or_else(|| usage())));
    }
    pairs
}

fn encoded(pairs: &[(&str, &String)]) -> Vec<String> {
    pairs
        .iter()
        .map(|(k, v)| format!("{k}={}", http::encode(v)))
        .collect()
}

/// Pulls one page; returns its events and the `next` cursor.
fn page(cursor: u64, filters: &str) -> (Vec<serde_json::Value>, u64) {
    let body = query("tail", &format!("/events?after={cursor}{filters}"));
    match (
        body.get("events").and_then(|v| v.as_array()),
        body.get("next").and_then(|v| v.as_u64()),
    ) {
        (Some(list), Some(next)) => (list.clone(), next),
        _ => {
            eprintln!("pulse tail: response has no events array or next cursor");
            exit(1)
        }
    }
}

fn tail(args: &[String]) {
    let pairs = filters(args, &TAIL_FLAGS);
    // A value that looks like a flag means the real value is missing.
    if pairs.iter().any(|(_, v)| v.starts_with("--")) {
        usage();
    }
    let filters: String = encoded(&pairs).iter().map(|p| format!("&{p}")).collect();
    let mut cursor = 0;
    loop {
        let (list, next) = page(cursor, &filters);
        cursor = next;
        if list.is_empty() {
            break;
        }
    }
    let mut out = std::io::stdout().lock();
    loop {
        std::thread::sleep(Duration::from_secs(1));
        let (list, next) = page(cursor, &filters);
        for event in &list {
            writeln!(out, "{event}").unwrap_or_else(|e| fail_tail(e));
        }
        out.flush().unwrap_or_else(|e| fail_tail(e));
        cursor = next;
    }
}

fn fail_tail(e: impl std::fmt::Display) -> ! {
    eprintln!("pulse tail: {e}");
    exit(1)
}

fn events(args: &[String]) {
    let pairs = encoded(&filters(args, &EVENT_FLAGS));
    let mut target = "/events".to_string();
    if !pairs.is_empty() {
        target = format!("{target}?{}", pairs.join("&"));
    }
    let body = query("events", &target);
    let Some(list) = body.get("events").and_then(|v| v.as_array()) else {
        eprintln!("pulse events: response has no events array");
        exit(1)
    };
    for event in list {
        println!(
            "{}",
            serde_json::to_string(event).expect("Value serializes")
        );
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("collect") => {}
        Some("events") => return events(&args[1..]),
        Some("tail") => return tail(&args[1..]),
        Some("health") if args.len() == 1 => {
            return println!("{}", query("health", "/health"));
        }
        _ => usage(),
    }
    let (mut once, mut interval) = (false, 2u64);
    let mut rest = args[1..].iter();
    while let Some(a) = rest.next() {
        match a.as_str() {
            "--once" => once = true,
            "--interval" => {
                interval = rest
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or_else(|| usage())
            }
            _ => usage(),
        }
    }

    // A scratch VASCULAR_HOME or EVENT_JOURNAL_DIR moves the spool, lock and
    // socket but not the database, and checkpoints are keyed by day-file name
    // alone: a second collector would write the real database's checkpoints
    // and move the real collector past bytes it never read (PULSE-B14).
    if process_env("PULSE_DATABASE_URL").is_none()
        && (process_env("VASCULAR_HOME").is_some() || process_env("EVENT_JOURNAL_DIR").is_some())
    {
        fail(
            "VASCULAR_HOME or EVENT_JOURNAL_DIR is set, so set PULSE_DATABASE_URL too: \
             a collector on another journal must not write the default database",
        );
    }
    let lock_path = paths::path(&process_env, "state", "pulse", &["collector.lock"])
        .unwrap_or_else(|e| fail(e));
    let lock = open_in(&lock_path, false).unwrap_or_else(|e| fail(e));
    match lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => fail("already running"),
        Err(TryLockError::Error(e)) => fail(e),
    }
    let log_path =
        paths::path(&process_env, "log", "pulse", &["collector.log"]).unwrap_or_else(|e| fail(e));
    let mut log_file = open_in(&log_path, true).unwrap_or_else(|e| fail(e));
    let journal = paths::journal_dir(&process_env);
    let artifact_root =
        paths::path(&process_env, "data", "pulse", &["artifacts"]).unwrap_or_else(|e| fail(e));

    // The lock holder owns the API socket; bind before the first round.
    let sock_path =
        paths::path(&process_env, "state", "pulse", &["pulse.sock"]).unwrap_or_else(|e| fail(e));
    let listener = serve::bind(&sock_path).unwrap_or_else(|e| fail(e));
    let backend = PgBackend::new(conninfo(), journal.clone(), artifact_root.clone());
    std::thread::spawn(move || serve::serve(listener, backend));

    let mut store = None;
    loop {
        round(&mut store, &mut log_file, &journal, &artifact_root);
        if once {
            break;
        }
        std::thread::sleep(Duration::from_secs(interval));
    }
    drop(lock);
}
