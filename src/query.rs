//! The read side of the pulse database (P4 of `PLAN.md`), behind the socket
//! API's `/events`, `/spans`, `/artifacts` and `/health`.

use crate::envelope::is_timestamp;

/// Which events a `GET /events` page asks for. `goal`, `episode`, `task` and
/// `trace` filter the events columns `goal_id`, `episode_id`, `task_id` and
/// `trace_id`; `project`, `source` and `kind` filter the columns of the same
/// name. `after` is the seq cursor and `since` a lower bound on time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventFilter {
    pub after: i64,
    pub limit: i64,
    pub project: Option<String>,
    pub goal: Option<String>,
    pub episode: Option<String>,
    pub task: Option<String>,
    pub trace: Option<String>,
    pub source: Option<String>,
    pub kind: Option<String>,
    pub since: Option<String>,
}

impl Default for EventFilter {
    fn default() -> Self {
        EventFilter {
            after: 0,
            limit: 500,
            project: None,
            goal: None,
            episode: None,
            task: None,
            trace: None,
            source: None,
            kind: None,
            since: None,
        }
    }
}

impl EventFilter {
    /// Builds a filter from the decoded query pairs of `GET /events?...`.
    /// Every error starts with the offending parameter's name and a colon.
    pub fn from_query(q: &[(String, String)]) -> Result<EventFilter, String> {
        let mut f = EventFilter::default();
        let mut seen: Vec<&str> = Vec::new();
        for (name, value) in q {
            if seen.contains(&name.as_str()) {
                return Err(format!("{name}: given more than once"));
            }
            seen.push(name);
            let text = match name.as_str() {
                "after" => {
                    f.after = value
                        .parse()
                        .ok()
                        .filter(|n| *n >= 0)
                        .ok_or("after: must be a non-negative integer")?;
                    continue;
                }
                "limit" => {
                    f.limit = value
                        .parse()
                        .ok()
                        .filter(|n| (1..=5000).contains(n))
                        .ok_or("limit: must be between 1 and 5000")?;
                    continue;
                }
                "since" => {
                    if !is_timestamp(value) {
                        return Err("since: must be an RFC 3339 timestamp".into());
                    }
                    f.since = Some(value.clone());
                    continue;
                }
                "project" => &mut f.project,
                "goal" => &mut f.goal,
                "episode" => &mut f.episode,
                "task" => &mut f.task,
                "trace" => &mut f.trace,
                "source" => &mut f.source,
                "kind" => &mut f.kind,
                _ => return Err(format!("{name}: unknown parameter")),
            };
            if value.is_empty() {
                return Err(format!("{name}: must not be empty"));
            }
            *text = Some(value.clone());
        }
        Ok(f)
    }
}

/// True iff `s` is a content hash as artifacts are keyed: exactly 64
/// lowercase hex characters.
pub fn is_hash(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
}
