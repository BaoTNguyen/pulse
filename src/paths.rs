//! Where the stack keeps state outside a checkout: a port of the vendored
//! Python `vascular_paths` module, answering `contract/paths_vectors.json`
//! the same way. Pure path arithmetic: nothing here touches the disk.
//! Every function takes its environment as a lookup closure (real callers
//! pass `&process_env`), and a set-but-empty value counts as unset.

use std::fmt;
use std::path::{Path, PathBuf};

pub const KINDS: [&str; 8] = [
    "config", "secrets", "state", "spool", "log", "cache", "data", "backups",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathsError {
    pub kind: String,
}

impl fmt::Display for PathsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown path kind {:?} (expected one of {})",
            self.kind,
            KINDS.join(", ")
        )
    }
}

impl std::error::Error for PathsError {}

pub fn process_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn lookup(env: &impl Fn(&str) -> Option<String>, name: &str) -> Option<String> {
    env(name).filter(|v| !v.is_empty())
}

pub fn home(env: &impl Fn(&str) -> Option<String>) -> PathBuf {
    lookup(env, "VASCULAR_HOME").map_or_else(
        // An unset HOME yields the relative ".vascular", like Python's "" / ".vascular".
        || PathBuf::from(lookup(env, "HOME").unwrap_or_default()).join(".vascular"),
        PathBuf::from,
    )
}

pub fn path(
    env: &impl Fn(&str) -> Option<String>,
    kind: &str,
    component: &str,
    parts: &[&str],
) -> Result<PathBuf, PathsError> {
    if !KINDS.contains(&kind) {
        return Err(PathsError {
            kind: kind.to_string(),
        });
    }
    let mut p = home(env);
    p.push(kind);
    p.push(component);
    p.extend(parts);
    Ok(p)
}

pub fn journal_dir(env: &impl Fn(&str) -> Option<String>) -> PathBuf {
    lookup(env, "EVENT_JOURNAL_DIR").map_or_else(
        || path(env, "spool", "events", &[]).expect("spool is a valid kind"),
        PathBuf::from,
    )
}

pub fn repo_dir(root: &Path, component: &str) -> PathBuf {
    root.join(".vascular").join(component)
}
