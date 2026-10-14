//! Reader for the journal's day files: returns complete NDJSON lines past
//! per-file byte checkpoints.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// Day file name only, e.g. "20261013.ndjson".
    pub file: String,
    /// Byte offset of the line's first byte in that file.
    pub offset: u64,
    /// The line without its trailing '\n'.
    pub bytes: Vec<u8>,
}

fn is_day_file(name: &str) -> bool {
    name.strip_suffix(".ndjson")
        .is_some_and(|d| d.len() == 8 && d.bytes().all(|b| b.is_ascii_digit()))
}

/// Read complete lines appended since `checkpoints`, up to `max_lines` in
/// total. Returns the lines and the new checkpoint of every file touched.
pub fn read_new(
    dir: &Path,
    checkpoints: &HashMap<String, u64>,
    max_lines: usize,
) -> io::Result<(Vec<Line>, HashMap<String, u64>)> {
    let mut lines = Vec::new();
    let mut out = HashMap::new();
    if max_lines == 0 {
        return Ok((lines, out));
    }

    let mut names = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        // DirEntry::file_type does not follow symlinks: only real regular files count.
        if !entry.file_type()?.is_file() {
            continue;
        }
        if let Some(name) = entry.file_name().to_str().filter(|n| is_day_file(n)) {
            names.push(name.to_owned());
        }
    }
    names.sort();

    for name in names {
        if lines.len() >= max_lines {
            break;
        }
        let mut f = File::open(dir.join(&name))?;
        let len = f.metadata()?.len();
        let mut start = checkpoints.get(&name).copied().unwrap_or(0);
        if start > len {
            start = 0;
            out.insert(name.clone(), 0);
        }
        // ponytail: whole-tail read; switch to a bounded read if day files get large.
        f.seek(SeekFrom::Start(start))?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf)?;
        let Some(end) = buf.iter().rposition(|&b| b == b'\n') else {
            continue;
        };

        let mut pos = 0;
        for chunk in buf[..end].split(|&b| b == b'\n') {
            if lines.len() >= max_lines {
                break;
            }
            if !chunk.is_empty() {
                lines.push(Line {
                    file: name.clone(),
                    offset: start + pos as u64,
                    bytes: chunk.to_vec(),
                });
            }
            pos += chunk.len() + 1;
        }
        out.insert(name, start + pos as u64);
    }
    Ok((lines, out))
}
