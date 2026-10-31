//! `pulse purge`: remove named artifacts everywhere and record that it did.
//! The one exception to keep-everything
//! (docs/adr/2026-10-07-keep-everything.md): a captured secret. Spool copies go
//! first, so the collector cannot take the blob in again, then the stored file.
//! The `artifacts` row only names the hash, so the caller drops it when the
//! database answers. Cold packs and snapshots join here once they exist.

use std::fs;
use std::io::ErrorKind;
use std::path::Path;

/// Deletes every copy of `hash` under the journal spool (its own `artifacts/`
/// and each sandbox inbox's) and the stored `<2hex>/<hash>.zst`. Returns what
/// it removed, as `spool`, `incoming/<key>` and `store`.
pub fn files(hash: &str, journal: &Path, artifact_root: &Path) -> std::io::Result<Vec<String>> {
    let mut places = vec![("spool".to_string(), journal.join("artifacts").join(hash))];
    match fs::read_dir(journal.join("incoming")) {
        Ok(dir) => {
            for entry in dir {
                let entry = entry?;
                let key = entry.file_name().to_string_lossy().into_owned();
                places.push((
                    format!("incoming/{key}"),
                    entry.path().join("artifacts").join(hash),
                ));
            }
        }
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    places.push((
        "store".to_string(),
        artifact_root.join(&hash[..2]).join(format!("{hash}.zst")),
    ));
    let mut removed = Vec::new();
    for (label, path) in places {
        match fs::remove_file(&path) {
            Ok(()) => removed.push(label),
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::files;
    use std::fs;

    #[test]
    fn removes_every_copy_and_names_where() {
        let root = std::env::temp_dir().join(format!("purge-{}", std::process::id()));
        let (journal, store) = (root.join("spool"), root.join("store"));
        let hash = "ab".repeat(32);
        for dir in [
            journal.join("artifacts"),
            journal.join("incoming/k1/artifacts"),
            journal.join("incoming/k2/artifacts"),
            store.join("ab"),
        ] {
            fs::create_dir_all(&dir).unwrap();
        }
        fs::write(journal.join("artifacts").join(&hash), b"secret").unwrap();
        fs::write(journal.join("incoming/k2/artifacts").join(&hash), b"secret").unwrap();
        fs::write(store.join("ab").join(format!("{hash}.zst")), b"secret").unwrap();
        let other = journal.join("artifacts").join("cd".repeat(32));
        fs::write(&other, b"keep").unwrap();

        let mut removed = files(&hash, &journal, &store).unwrap();
        removed.sort();
        assert_eq!(removed, ["incoming/k2", "spool", "store"]);
        assert!(other.exists());
        assert_eq!(
            files(&hash, &journal, &store).unwrap(),
            Vec::<String>::new()
        );
        fs::remove_dir_all(&root).unwrap();
    }
}
