//! `pulse export`: a dataset snapshot for marrow (docs/adr/2026-10-07-keep-everything.md). The
//! events matching the filters, oldest first, go into one zstd-compressed
//! Parquet file named by its own sha256 under `data/pulse/datasets/`, with a
//! manifest beside it saying what was asked for. A model records the hash it
//! learned from; the same rows always make the same file.
//!
//! Events only: artifact bodies stay in the store, and a snapshot carries
//! their hashes in `payload`. ponytail: whole row groups of 50,000 rows sit in
//! memory (a few tens of MB); stream column chunks if exports outgrow that.

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parquet::basic::{Compression, ZstdLevel};
use parquet::data_type::{ByteArray, ByteArrayType, DoubleType, Int64Type};
use parquet::file::properties::WriterProperties;
use parquet::file::writer::SerializedFileWriter;
use parquet::schema::parser::parse_message_type;
use sha2::{Digest, Sha256};

use crate::query::{TEXT_COLUMNS, utc};

const ROW_GROUP: i64 = 50_000;

/// What to export. Every field narrows; none means everything.
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct Filter {
    pub since: Option<String>,
    pub until: Option<String>,
    pub kinds: Vec<String>,
    pub sources: Vec<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct Snapshot {
    pub hash: String,
    pub rows: u64,
    pub path: PathBuf,
}

fn schema() -> String {
    let text: String = TEXT_COLUMNS
        .iter()
        .map(|c| {
            let rep = if matches!(*c, "source" | "kind") {
                "REQUIRED"
            } else {
                "OPTIONAL"
            };
            format!("{rep} BYTE_ARRAY {c} (UTF8); ")
        })
        .collect();
    format!(
        "message event {{ REQUIRED INT64 seq; REQUIRED BYTE_ARRAY id (UTF8); \
         REQUIRED BYTE_ARRAY ts (UTF8); {text}OPTIONAL DOUBLE duration_ms; \
         OPTIONAL BYTE_ARRAY payload (JSON); }}"
    )
}

/// Column values for one row group, in schema order after seq/id/ts.
struct Columns {
    seq: Vec<i64>,
    id: Vec<ByteArray>,
    ts: Vec<ByteArray>,
    text: Vec<Vec<Option<ByteArray>>>,
    duration_ms: Vec<Option<f64>>,
    payload: Vec<Option<ByteArray>>,
}

fn page(c: &mut postgres::Client, f: &Filter, after: i64) -> Result<Columns, postgres::Error> {
    let cols = TEXT_COLUMNS.join(", ");
    let sql = format!(
        "SELECT seq, id::text, {ts}, {cols}, duration_ms, payload::text FROM events \
         WHERE seq > $1 AND ($2::text IS NULL OR ts >= $2::timestamptz) \
         AND ($3::text IS NULL OR ts < $3::timestamptz) \
         AND (cardinality($4::text[]) = 0 OR kind = ANY($4)) \
         AND (cardinality($5::text[]) = 0 OR source = ANY($5)) \
         ORDER BY seq LIMIT {ROW_GROUP}",
        ts = utc("ts")
    );
    let rows = c.query(&sql, &[&after, &f.since, &f.until, &f.kinds, &f.sources])?;
    let n = TEXT_COLUMNS.len();
    let bytes = |s: String| ByteArray::from(s.into_bytes());
    let mut out = Columns {
        seq: vec![],
        id: vec![],
        ts: vec![],
        text: vec![vec![]; n],
        duration_ms: vec![],
        payload: vec![],
    };
    for r in rows {
        out.seq.push(r.get(0));
        out.id.push(bytes(r.get(1)));
        out.ts.push(bytes(r.get(2)));
        for (i, col) in out.text.iter_mut().enumerate() {
            col.push(r.get::<_, Option<String>>(3 + i).map(bytes));
        }
        out.duration_ms.push(r.get(3 + n));
        out.payload
            .push(r.get::<_, Option<String>>(4 + n).map(bytes));
    }
    Ok(out)
}

/// Writes an optional column: values present, and a definition level per row.
fn optional<T: Clone>(vals: &[Option<T>]) -> (Vec<T>, Vec<i16>) {
    let present = vals.iter().flatten().cloned().collect();
    let defs = vals.iter().map(|v| i16::from(v.is_some())).collect();
    (present, defs)
}

type Error = Box<dyn std::error::Error>;

/// Writes the snapshot and its manifest; returns its hash, row count and path.
pub fn export(c: &mut postgres::Client, f: &Filter, datasets: &Path) -> Result<Snapshot, Error> {
    fs::create_dir_all(datasets)?;
    let tmp = datasets.join(format!(".export-{}.parquet", std::process::id()));
    let props = WriterProperties::builder()
        .set_compression(Compression::ZSTD(ZstdLevel::try_new(19)?))
        .set_created_by("pulse export".into())
        .build();
    let mut w = SerializedFileWriter::new(
        File::create(&tmp)?,
        Arc::new(parse_message_type(&schema())?),
        Arc::new(props),
    )?;
    let (mut after, mut rows) = (0i64, 0u64);
    loop {
        let cols = page(c, f, after)?;
        let Some(&last) = cols.seq.last() else { break };
        after = last;
        rows += cols.seq.len() as u64;
        let mut rg = w.next_row_group()?;
        let mut i = 0;
        while let Some(mut col) = rg.next_column()? {
            match i {
                0 => col
                    .typed::<Int64Type>()
                    .write_batch(&cols.seq, None, None)?,
                1 => col
                    .typed::<ByteArrayType>()
                    .write_batch(&cols.id, None, None)?,
                2 => col
                    .typed::<ByteArrayType>()
                    .write_batch(&cols.ts, None, None)?,
                n if n < 3 + TEXT_COLUMNS.len() => {
                    let vals = &cols.text[n - 3];
                    if matches!(TEXT_COLUMNS[n - 3], "source" | "kind") {
                        let all: Vec<ByteArray> = vals.iter().flatten().cloned().collect();
                        col.typed::<ByteArrayType>().write_batch(&all, None, None)?
                    } else {
                        let (v, d) = optional(vals);
                        col.typed::<ByteArrayType>()
                            .write_batch(&v, Some(&d), None)?
                    }
                }
                n if n == 3 + TEXT_COLUMNS.len() => {
                    let (v, d) = optional(&cols.duration_ms);
                    col.typed::<DoubleType>().write_batch(&v, Some(&d), None)?
                }
                _ => {
                    let (v, d) = optional(&cols.payload);
                    col.typed::<ByteArrayType>()
                        .write_batch(&v, Some(&d), None)?
                }
            };
            col.close()?;
            i += 1;
        }
        rg.close()?;
    }
    w.close()?;

    let mut hasher = Sha256::new();
    let mut file = File::open(&tmp)?;
    let mut buf = vec![0; 1 << 16];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let hash = format!("{:x}", hasher.finalize());
    let path = datasets.join(format!("{hash}.parquet"));
    fs::rename(&tmp, &path)?;
    let manifest = serde_json::json!({"hash": hash, "rows": rows, "filter": f, "format": "parquet, zstd-19", "schema": schema()});
    fs::write(
        datasets.join(format!("{hash}.json")),
        format!("{manifest:#}\n"),
    )?;
    Ok(Snapshot { hash, rows, path })
}
