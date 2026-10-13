use pulse::paths;
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

fn arg(args: &[Value], i: usize) -> &str {
    args[i].as_str().expect("args are strings")
}

#[test]
fn vectors() {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/contract/paths_vectors.json"
    ))
    .expect("read paths_vectors.json");
    let doc: Value = serde_json::from_str(&text).expect("parse paths_vectors.json");
    let cases = doc["cases"].as_array().expect("cases array");

    let mut ran = 0;
    for (i, case) in cases.iter().enumerate() {
        let map: HashMap<String, String> = case["env"]
            .as_object()
            .expect("env object")
            .iter()
            .map(|(k, v)| (k.clone(), v.as_str().expect("env value").to_string()))
            .collect();
        // The map is the whole environment; empty values are passed through as Some("").
        let env = |k: &str| map.get(k).cloned();
        let args = case["args"].as_array().expect("args array");

        let got = match case["call"].as_str().expect("call string") {
            "home" => Ok(paths::home(&env)),
            "path" => {
                let parts: Vec<&str> = (2..args.len()).map(|j| arg(args, j)).collect();
                paths::path(&env, arg(args, 0), arg(args, 1), &parts)
            }
            "journal_dir" => Ok(paths::journal_dir(&env)),
            "repo_dir" => Ok(paths::repo_dir(Path::new(arg(args, 0)), arg(args, 1))),
            other => panic!("case {i}: unknown call {other:?}: {case}"),
        };

        match &case["expect"] {
            Value::String(want) => {
                let got = got.unwrap_or_else(|e| panic!("case {i}: unexpected error {e}: {case}"));
                assert_eq!(got.to_string_lossy(), *want, "case {i}: {case}");
            }
            Value::Object(o) if o.get("error") == Some(&Value::Bool(true)) => {
                assert!(
                    got.is_err(),
                    "case {i}: expected error, got {got:?}: {case}"
                );
            }
            other => panic!("case {i}: bad expect {other}: {case}"),
        }
        ran += 1;
    }
    assert!(ran >= 19, "only {ran} cases ran");
}
