use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use pulse::spool::{Line, read_new};

fn scratch(test: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir =
        std::env::temp_dir().join(format!("pulse-spool-{test}-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn merge(into: &mut HashMap<String, u64>, from: HashMap<String, u64>) {
    into.extend(from);
}

fn texts(lines: &[Line]) -> Vec<String> {
    lines
        .iter()
        .map(|l| String::from_utf8(l.bytes.clone()).unwrap())
        .collect()
}

#[test]
fn partial_last_line_waits_for_newline() {
    let dir = scratch("partial");
    let path = dir.join("20261013.ndjson");
    fs::write(&path, "a\nb\npart").unwrap();

    let mut cp = HashMap::new();
    let (lines, new) = read_new(&dir, &cp, 100).unwrap();
    assert_eq!(texts(&lines), ["a", "b"]);
    assert_eq!(new["20261013.ndjson"], 4);
    merge(&mut cp, new);

    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"ial\n")
        .unwrap();
    let (lines, new) = read_new(&dir, &cp, 100).unwrap();
    assert_eq!(
        lines,
        [Line {
            file: "20261013.ndjson".into(),
            offset: 4,
            bytes: b"partial".to_vec(),
        }]
    );
    assert_eq!(new["20261013.ndjson"], 12);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn resume_from_checkpoints() {
    let dir = scratch("resume");
    let all: Vec<String> = (1..=5).map(|n| format!("{{\"n\":{n}}}")).collect();
    fs::write(dir.join("20261013.ndjson"), all.join("\n") + "\n").unwrap();

    let mut cp = HashMap::new();
    let mut got = Vec::new();
    loop {
        let (lines, new) = read_new(&dir, &cp, 2).unwrap();
        if lines.is_empty() {
            assert!(new.is_empty());
            break;
        }
        assert!(lines.len() <= 2);
        got.extend(texts(&lines));
        merge(&mut cp, new);
    }
    assert_eq!(got, all);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn max_lines_spans_files() {
    let dir = scratch("across");
    fs::write(dir.join("20261012.ndjson"), "{\"n\":1}\n{\"n\":2}\n").unwrap();
    fs::write(
        dir.join("20261013.ndjson"),
        "{\"n\":3}\n{\"n\":4}\n{\"n\":5}\n",
    )
    .unwrap();

    let mut cp = HashMap::new();
    let (lines, new) = read_new(&dir, &cp, 3).unwrap();
    assert_eq!(texts(&lines), [r#"{"n":1}"#, r#"{"n":2}"#, r#"{"n":3}"#]);
    assert_eq!(lines[2].file, "20261013.ndjson");
    assert_eq!(new.len(), 2);
    assert_eq!(new["20261012.ndjson"], 16);
    assert_eq!(new["20261013.ndjson"], 8);
    merge(&mut cp, new);

    let (lines, new) = read_new(&dir, &cp, 3).unwrap();
    assert_eq!(texts(&lines), [r#"{"n":4}"#, r#"{"n":5}"#]);
    assert_eq!(lines[0].offset, 8);
    assert_eq!(new, HashMap::from([("20261013.ndjson".to_string(), 24)]));
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn ignores_non_day_entries() {
    let dir = scratch("ignored");
    let line = "{\"n\":9}\n";
    for name in ["notes.txt", "2026101.ndjson", "20261013.ndjson.tmp"] {
        fs::write(dir.join(name), line).unwrap();
    }
    fs::create_dir(dir.join("incoming")).unwrap();
    fs::write(dir.join("incoming").join("20261013.ndjson"), line).unwrap();
    fs::create_dir(dir.join("20261014.ndjson")).unwrap();
    fs::write(dir.join("20261013.ndjson"), "{\"n\":1}\n").unwrap();

    let (lines, new) = read_new(&dir, &HashMap::new(), 100).unwrap();
    assert_eq!(texts(&lines), [r#"{"n":1}"#]);
    assert_eq!(new.keys().collect::<Vec<_>>(), ["20261013.ndjson"]);
    fs::remove_dir_all(&dir).unwrap();
}

#[cfg(unix)]
#[test]
fn ignores_symlinked_day_files() {
    let dir = scratch("symlink");
    let target = dir.join("target.txt");
    fs::write(&target, "{\"n\":1}\n").unwrap();
    std::os::unix::fs::symlink(&target, dir.join("20261013.ndjson")).unwrap();

    let (lines, new) = read_new(&dir, &HashMap::new(), 100).unwrap();
    assert!(lines.is_empty());
    assert!(new.is_empty());
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn exact_offsets_with_multibyte_utf8() {
    let dir = scratch("utf8");
    let content = "{\"m\":\"héllo\"}\n\n{\"m\":\"日本語\"}\n{\"m\":\"🦀\"}\n"
        .as_bytes()
        .to_vec();
    fs::write(dir.join("20261013.ndjson"), &content).unwrap();

    let (lines, new) = read_new(&dir, &HashMap::new(), 100).unwrap();
    assert_eq!(lines.len(), 3);
    for l in &lines {
        let o = l.offset as usize;
        assert!(o == 0 || content[o - 1] == b'\n');
        assert_eq!(content[o..o + l.bytes.len()], l.bytes[..]);
        assert_eq!(content[o + l.bytes.len()], b'\n');
        assert!(!l.bytes.is_empty());
    }
    assert_eq!(new["20261013.ndjson"], content.len() as u64);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn truncated_file_restarts_at_zero() {
    let dir = scratch("truncate");
    let path = dir.join("20261013.ndjson");
    fs::write(&path, "{\"n\":1}\n{\"n\":2}\n{\"n\":3}\n").unwrap();

    let (lines, cp) = read_new(&dir, &HashMap::new(), 100).unwrap();
    assert_eq!(lines.len(), 3);
    assert_eq!(cp["20261013.ndjson"], 24);

    fs::write(&path, "{\"x\":7}\n").unwrap();
    let (lines, new) = read_new(&dir, &cp, 100).unwrap();
    assert_eq!(
        lines,
        [Line {
            file: "20261013.ndjson".into(),
            offset: 0,
            bytes: br#"{"x":7}"#.to_vec(),
        }]
    );
    assert_eq!(new["20261013.ndjson"], 8);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn missing_dir_is_an_error() {
    let dir = std::env::temp_dir().join(format!("pulse-spool-missing-{}", std::process::id()));
    assert!(read_new(&dir, &HashMap::new(), 1).is_err());
}

#[test]
fn zero_max_lines_returns_nothing() {
    let dir = scratch("zero");
    fs::write(dir.join("20261013.ndjson"), "{\"n\":1}\n").unwrap();
    let (lines, new) = read_new(&dir, &HashMap::new(), 0).unwrap();
    assert!(lines.is_empty() && new.is_empty());
    fs::remove_dir_all(&dir).unwrap();
}
