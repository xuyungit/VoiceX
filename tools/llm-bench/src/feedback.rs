//! Offline human feedback. Labels never enter automatic scores or standings.
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub fn from_file(source: &Path, destination: Option<&Path>) -> Result<PathBuf, String> {
    let payload: Value =
        serde_json::from_str(&std::fs::read_to_string(source).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    let path = destination
        .map(Path::to_path_buf)
        .unwrap_or_else(|| source.with_file_name("feedback.html"));
    if path == source
        || path
            .canonicalize()
            .ok()
            .is_some_and(|p| Some(p) == source.canonicalize().ok())
    {
        return Err("feedback destination must not overwrite the source results".into());
    }
    write(&path, &payload)?;
    Ok(path)
}

fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str, String> {
    v[key]
        .as_str()
        .ok_or_else(|| format!("missing string field: {key}"))
}

fn dataset(payload: &Value) -> Result<Value, String> {
    let cases = payload["cases"]
        .as_array()
        .ok_or("results need a cases array")?;
    let mut items: Vec<Value> = Vec::new();
    let mut index = HashMap::new();
    let mut call_count = 0;
    for (case_index, c) in cases.iter().enumerate() {
        let (name, input, expected) = (text(c, "case")?, text(c, "input")?, text(c, "expected")?);
        let (provider, model) = (text(c, "provider")?, text(c, "model")?);
        let rounds = c["rounds"].as_array().ok_or("case needs rounds")?;
        for (round, r) in rounds.iter().enumerate() {
            let output = text(r, "output")?;
            let failed = !r["error"].is_null();
            // Identical outputs for the same case share one human decision. All call IDs remain traceable.
            let key = json!([name, input, expected, output, failed]).to_string();
            let next = items.len();
            let item_index = *index.entry(key).or_insert(next);
            if item_index == next {
                items.push(json!({"id": format!("output-{}", next + 1), "case": name,
                    "input": input, "expected": expected, "output": output,
                    "call_failed": failed, "calls": []}));
            }
            items[item_index]["calls"]
                .as_array_mut()
                .unwrap()
                .push(json!({
                    "case_index": case_index, "round_index": round, "provider": provider,
                    "model": model, "duration_ms": r["duration_ms"],
                }));
            call_count += 1;
        }
    }
    if items.is_empty() {
        return Err("results contain no output rounds".into());
    }
    let content = json!({"recorded_at": payload["run"]["at"], "items": items}).to_string();
    // Stable content identity for local drafts/import validation; not a security signature.
    let fingerprint = content.bytes().fold(0xcbf29ce484222325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    });
    Ok(
        json!({"version": 1, "dataset_id": format!("{:016x}", fingerprint),
        "recorded_at": payload["run"]["at"], "call_count": call_count, "items": items}),
    )
}

pub fn write(path: &Path, payload: &Value) -> Result<(), String> {
    let data = dataset(payload)?;
    // Never let transcript content close the inert JSON script element.
    let encoded = data
        .to_string()
        .replace('&', "\\u0026")
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029");
    let page = include_str!("feedback.html").replacen("__BENCH_DATA__", &encoded, 1);
    std::fs::write(path, page).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Value {
        json!({"run": {"at": "2026-10-08", "api_key": "never-export-this"}, "cases": [
            {"case": "sample", "input": "嗯，好", "expected": "好", "provider": "one", "model": "m1",
             "rounds": [{"output": "好", "duration_ms": 100, "error": null}, {"output": "好", "duration_ms": 200, "error": null}]},
            {"case": "sample", "input": "嗯，好", "expected": "好", "provider": "two", "model": "m2",
             "rounds": [{"output": "好", "duration_ms": 300, "error": null}, {"output": "", "duration_ms": 10000, "error": "HTTP error with secret body"}]}
        ]})
    }
    #[test]
    fn duplicates_share_a_label_without_losing_call_identity_or_failures() {
        let d = dataset(&fixture()).unwrap();
        assert_eq!(d["call_count"], 4);
        assert_eq!(d["items"].as_array().unwrap().len(), 2);
        assert_eq!(d["items"][0]["calls"].as_array().unwrap().len(), 3);
        assert_eq!(d["items"][1]["call_failed"], true);
        assert!(!d.to_string().contains("never-export-this"));
        assert!(!d.to_string().contains("secret body"));
        let mut changed = fixture();
        changed["cases"][0]["input"] = json!("不同原文");
        assert_ne!(d["dataset_id"], dataset(&changed).unwrap()["dataset_id"]);
    }
    #[test]
    fn transcripts_are_inert_even_when_they_contain_html_and_script_end_tags() {
        let mut payload = fixture();
        payload["cases"][0]["rounds"][0]["output"] = json!("</script><script>alert('x')</script>");
        let path =
            std::env::temp_dir().join(format!("llm-bench-feedback-{}.html", std::process::id()));
        write(&path, &payload).unwrap();
        let html = std::fs::read_to_string(&path).unwrap();
        assert!(!html.contains("</script><script>alert"));
        assert!(html.contains("\\u003c/script\\u003e"));
        assert!(!html.contains("never-export-this"));
        std::fs::remove_file(path).unwrap();
    }
}
