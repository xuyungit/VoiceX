//! One bounded semantic decision per distinct complete output; no edit questions or secondary reviewer.
use crate::{
    scheduler,
    typesafe::{Backend, RawAnswer},
    Provider,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::Duration;

const LOW_CONFIDENCE_HINT: f64 = 0.7;

pub fn question() -> Value {
    serde_json::from_str(include_str!("../quick_question.json")).expect("invalid quick question")
}

pub fn validate(provider: &Provider) -> Result<Backend, String> {
    let backend = Backend::from_type(&provider.provider_type)
        .ok_or("quick ranking needs [judge] type = 'typesafe' or 'openai'; use --speed-only to skip quality judgments")?;
    backend.body(&provider.model, &Value::Null, &[("basic", &question())])?;
    Ok(backend)
}

fn verdict(raw: RawAnswer) -> Result<Value, String> {
    let choice = raw.choice.ok_or("missing choice")?;
    let confidence = raw.confidence.ok_or("missing confidence")?;
    let probabilities = raw.probabilities.ok_or("missing probabilities")?;
    let options = ["pass", "needs_revision", "uncertain"];
    if !options.contains(&choice.as_str())
        || !confidence.is_finite()
        || !(0.0..=1.0).contains(&confidence)
        || probabilities.len() != options.len()
        || options.iter().any(|o| !probabilities.contains_key(*o))
        || probabilities
            .values()
            .any(|p| !p.is_finite() || !(0.0..=1.0).contains(p))
        || (probabilities.values().sum::<f64>() - 1.0).abs() > 0.021
    {
        return Err("invalid basic judgment choice, confidence or probability distribution".into());
    }
    // Confidence is a review hint, not a calibrated quality gate. Preserve the actual decision.
    Ok(
        json!({"verdict": choice, "low_confidence": confidence < LOW_CONFIDENCE_HINT,
        "choice": choice, "confidence": confidence, "probabilities": probabilities}),
    )
}

pub async fn evaluate(
    cases: &mut [Value],
    provider: Provider,
    prompt: &str,
    dictionary: &str,
    concurrency: usize,
) -> Result<Value, String> {
    let backend = validate(&provider)?;
    let rubric = question();
    let mut unique = HashMap::new();
    let mut jobs = Vec::new();
    let mut slots = Vec::new();
    for (ci, case) in cases.iter().enumerate() {
        for (ri, round) in case["rounds"].as_array().unwrap().iter().enumerate() {
            if !round["error"].is_null() {
                continue;
            }
            let state = json!({"before": case["input"], "after": round["output"],
                "reference": case["expected"], "focus": case["focus"],
                "correction_prompt": prompt, "dictionary": dictionary});
            let key = state.to_string();
            let index = *unique.entry(key).or_insert_with(|| {
                let index = jobs.len();
                jobs.push((index.to_string(), state));
                index
            });
            slots.push((ci, ri, index));
        }
    }
    let calls = jobs.len();
    println!("Basic checks: {calls} distinct outputs · concurrency {concurrency} · one request each, no retry/reviewer");
    let url = backend.endpoint(&provider.base_url);
    let model = provider.model.clone();
    let key = provider.api_key;
    let http = reqwest::Client::new();
    let answers = scheduler::run(jobs, concurrency, move |state| {
        let (http, url, key, model) = (http.clone(), url.clone(), key.clone(), model.clone());
        async move {
            let call = async {
                let body = backend.body(&model, &state, &[("basic", &question())])?;
                let response = http
                    .post(url)
                    .bearer_auth(key)
                    .json(&body)
                    .timeout(Duration::from_secs(15))
                    .send()
                    .await
                    .map_err(|e| e.to_string())?;
                let status = response.status();
                let text = response.text().await.map_err(|e| e.to_string())?;
                if !status.is_success() {
                    return Err(format!(
                        "judge HTTP {status}: {}",
                        text.chars().take(200).collect::<String>()
                    ));
                }
                let mut reply = backend.read(&text)?;
                let raw = reply
                    .answers
                    .remove("basic")
                    .ok_or("missing basic answer")??;
                let mut answer = verdict(raw)?;
                answer["reported_model"] = json!(reply.model);
                Ok::<Value, String>(answer)
            }
            .await;
            call.unwrap_or_else(|e| json!({"verdict": "error", "error": e}))
        }
    })
    .await;
    for (ci, ri, index) in slots {
        cases[ci]["rounds"][ri]["basic"] = answers[index].clone();
    }
    Ok(
        json!({"name": provider.name, "model": provider.model, "backend": backend.name(),
        "rubric": rubric, "calls": calls, "low_confidence_hint_below": LOW_CONFIDENCE_HINT, "timeout_seconds": 15,
        "errors": answers.iter().filter(|a| a["verdict"] == "error").count()}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn choices_survive_confidence_hints_and_incomplete_answers_fail() {
        let raw = |choice: &str, confidence: f64| {
            serde_json::from_value(json!({"choice": choice,
            "confidence": confidence, "probabilities": {"pass": 0.8, "needs_revision": 0.1, "uncertain": 0.1}})).unwrap()
        };
        assert_eq!(verdict(raw("pass", 0.9)).unwrap()["verdict"], "pass");
        assert_eq!(verdict(raw("pass", 0.69)).unwrap()["verdict"], "pass");
        assert_eq!(verdict(raw("pass", 0.69)).unwrap()["low_confidence"], true);
        assert_eq!(
            verdict(raw("uncertain", 0.9)).unwrap()["verdict"],
            "uncertain"
        );
        assert!(verdict(raw("invented", 0.9)).is_err());
        let missing = serde_json::from_value(json!({"choice":"pass", "confidence":0.9})).unwrap();
        assert!(verdict(missing).is_err());
    }
}
