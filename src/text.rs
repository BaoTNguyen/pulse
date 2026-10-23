//! `--text`: one event as a line a person reads, the way `heart pulse tail`
//! always printed it. Clock time, source, kind, role, duration, then the
//! payload as `key=value`, each value cut to 60 characters.

use serde_json::Value;

pub fn render(e: &Value) -> String {
    let field = |k: &str| e.get(k).and_then(Value::as_str).unwrap_or("");
    let ts = field("ts");
    let mut parts = vec![
        ts.get(11..19).unwrap_or(ts).to_string(),
        format!(
            "{:<9}",
            e.get("source").and_then(Value::as_str).unwrap_or("?")
        ),
        e.get("kind")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_string(),
    ];
    if !field("role").is_empty() {
        parts.push(format!("role={}", field("role")));
    }
    if let Some(ms) = e.get("duration_ms").filter(|v| !v.is_null()) {
        parts.push(format!("{ms}ms"));
    }
    if let Some(payload) = e.get("payload").and_then(Value::as_object) {
        parts.extend(payload.iter().map(|(k, v)| format!("{k}={}", short(v))));
    }
    parts.join("  ")
}

fn short(v: &Value) -> String {
    let s = v.as_str().map_or_else(|| v.to_string(), str::to_string);
    if s.chars().count() <= 60 {
        return s;
    }
    s.chars().take(57).collect::<String>() + "..."
}

#[cfg(test)]
mod tests {
    use super::render;
    use serde_json::json;

    #[test]
    fn renders_like_heart_did() {
        let e = json!({"ts": "2026-10-23T17:51:11.851+00:00", "source": "heart",
                       "kind": "role.finished", "role": "builder", "duration_ms": 1200,
                       "payload": {"outcome": "pass", "note": "é".repeat(70)}});
        let line = render(&e);
        assert!(line.starts_with("17:51:11  heart      role.finished  role=builder  1200ms  "));
        assert!(line.contains("outcome=pass"));
        assert!(line.contains(&format!("note={}...  ", "é".repeat(57))));
    }

    #[test]
    fn a_bare_event_still_renders() {
        assert_eq!(render(&json!({})), "  ?          ?");
    }
}
