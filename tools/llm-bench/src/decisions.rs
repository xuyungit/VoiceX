//! The judge over OpenAI's Decisions API: `POST /v1/decisions`, `gpt-6-luna` only for now
//! (https://developers.openai.com/api/docs/guides/decisions).
//!
//! The rubric stays the TypeSafe questions the rest of the bench validates and records. This module translates a
//! question on the way out and the answer on the way back, so both judges answer the same rubric in the same shape and
//! `read_answer` holds them to the same standard. A question part with no Decisions counterpart is an error, never
//! dropped: the judge would otherwise answer a different rubric than the one the run records.

use crate::typesafe::{RawAnswer, Reply, Usage};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};

const ENDPOINT: &str = "https://api.openai.com/v1/decisions";

/// Accepts the endpoint itself, its `/v1` base, or nothing.
pub fn endpoint(base_url: &str) -> String {
    let base = base_url.trim().trim_end_matches('/');
    if base.is_empty() {
        ENDPOINT.to_string()
    } else if base.ends_with("/decisions") {
        base.to_string()
    } else {
        format!("{}/decisions", base)
    }
}

/// The questions about one state, each under its id. The state goes in as the JSON TypeSafe receives, so the field
/// names the instructions refer to (`before`, `after`, `heard`, …) reach the model verbatim.
pub fn body(model: &str, state: &Value, questions: &[(&str, &Value)]) -> Result<Value, String> {
    let input = serde_json::to_string_pretty(state).map_err(|e| e.to_string())?;
    let questions = questions.iter().map(|(name, question)| translate(name, question)).collect::<Result<Vec<_>, _>>()?;
    Ok(json!({ "model": model, "input": input, "questions": questions }))
}

/// A TypeSafe `score` becomes levels in the same order, labelled by index; a `choice` becomes one choice per option.
fn translate(name: &str, question: &Value) -> Result<Value, String> {
    let fields = question.as_object().ok_or("a question must be an object")?;
    if let Some(key) = fields.keys().find(|k| !matches!(k.as_str(), "type" | "instructions" | "criteria")) {
        return Err(format!("question field `{key}` has no Decisions counterpart"));
    }
    let instructions = instructions(&question["instructions"])?;
    let criteria = &question["criteria"];
    match question["type"].as_str() {
        Some("score") => {
            let levels = criteria
                .as_array()
                .ok_or("`score` criteria must be a list, lowest level first")?
                .iter()
                .enumerate()
                .map(|(i, c)| Ok(json!({ "label": i.to_string(), "description": criterion(c)? })))
                .collect::<Result<Vec<_>, String>>()?;
            Ok(json!({ "type": "score", "name": name, "instructions": instructions, "levels": levels }))
        }
        Some("choice") => {
            let choices = criteria
                .as_object()
                .ok_or("`choice` criteria must map each option to what it means")?
                .iter()
                .map(|(value, c)| Ok(json!({ "value": value, "description": criterion(c)? })))
                .collect::<Result<Vec<_>, String>>()?;
            Ok(json!({ "type": "choice", "name": name, "instructions": instructions, "choices": choices }))
        }
        other => Err(format!("cannot ask a `{}` question", other.unwrap_or("untyped"))),
    }
}

/// Text, or `{task, question}`: the task first, as context, then the question.
fn instructions(value: &Value) -> Result<String, String> {
    let text = match value {
        Value::String(text) => text.clone(),
        Value::Object(fields) => {
            if let Some(key) = fields.keys().find(|k| !matches!(k.as_str(), "task" | "question")) {
                return Err(format!("instructions field `{key}` has no Decisions counterpart"));
            }
            let mut parts = Vec::new();
            for key in ["task", "question"] {
                if let Some(part) = fields.get(key) {
                    parts.push(part.as_str().ok_or(format!("instructions `{key}` must be text"))?);
                }
            }
            parts.join("\n\n")
        }
        _ => return Err("instructions must be text or {task, question}".into()),
    };
    if text.trim().is_empty() {
        return Err("a question without instructions".into());
    }
    Ok(text)
}

/// Text, or `{what, examples, not_for}`: all of it goes into the description.
fn criterion(value: &Value) -> Result<String, String> {
    let fields = match value {
        Value::String(text) => return Ok(text.clone()),
        Value::Object(fields) => fields,
        _ => return Err("a criterion must be text or {what, examples, not_for}".into()),
    };
    let mut text = fields.get("what").and_then(Value::as_str).ok_or("criterion without `what`")?.to_string();
    for (key, value) in fields {
        match key.as_str() {
            "what" => {}
            "examples" => {
                let examples = value
                    .as_array()
                    .and_then(|list| list.iter().map(Value::as_str).collect::<Option<Vec<_>>>())
                    .ok_or("criterion `examples` must be a list of text")?;
                text.push_str("\nExamples:");
                for example in examples {
                    text.push_str("\n- ");
                    text.push_str(example);
                }
            }
            "not_for" => {
                text.push_str("\nNot for: ");
                text.push_str(value.as_str().ok_or("criterion `not_for` must be text")?);
            }
            _ => return Err(format!("criterion field `{key}` has no Decisions counterpart")),
        }
    }
    Ok(text)
}

#[derive(Deserialize)]
struct Response {
    answers: Vec<DecisionAnswer>,
    usage: Option<Usage>,
    model: Option<String>,
}

/// The bench asks no `predicate`; one in the response fails to parse like any other answer it did not ask for.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum DecisionAnswer {
    Choice { name: Option<String>, choice: String, probabilities: Vec<OptionProbability>, confidence: f64 },
    Score { name: Option<String>, probabilities: Vec<LevelProbability>, confidence: f64 },
    Refusal { name: Option<String> },
}

#[derive(Deserialize)]
struct OptionProbability {
    value: String,
    probability: f64,
}

#[derive(Deserialize)]
struct LevelProbability {
    value: i64,
    probability: f64,
}

/// Every answer by question id, as TypeSafe would have given it: options by value, levels by index. A refusal is a
/// failure of the judge on that question, like any answer it does not give.
pub fn read(text: &str) -> Result<Reply, String> {
    let response: Response = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let mut answers = HashMap::new();
    for answer in response.answers {
        let (name, raw) = match answer {
            DecisionAnswer::Refusal { name } => (name, Err("the judge refused to answer".to_string())),
            DecisionAnswer::Choice { name, choice, probabilities, confidence } => (
                name,
                distribution(probabilities.into_iter().map(|p| (p.value, p.probability)))
                    .map(|p| RawAnswer { choice: Some(choice), probabilities: Some(p), confidence: Some(confidence) }),
            ),
            DecisionAnswer::Score { name, probabilities, confidence } => (
                name,
                distribution(probabilities.into_iter().map(|p| (p.value.to_string(), p.probability)))
                    .map(|p| RawAnswer { choice: None, probabilities: Some(p), confidence: Some(confidence) }),
            ),
        };
        let name = name.ok_or("an answer without a question name")?;
        if answers.insert(name.clone(), raw).is_some() {
            return Err(format!("two answers to `{name}`"));
        }
    }
    Ok(Reply { answers, usage: response.usage, model: response.model })
}

fn distribution(pairs: impl Iterator<Item = (String, f64)>) -> Result<BTreeMap<String, f64>, String> {
    let mut p = BTreeMap::new();
    for (value, probability) in pairs {
        if p.insert(value.clone(), probability).is_some() {
            return Err(format!("two probabilities for `{value}`"));
        }
    }
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_accepts_bare_v1_or_full_endpoint() {
        assert_eq!(endpoint(""), ENDPOINT);
        assert_eq!(endpoint("https://api.openai.com/v1/"), ENDPOINT);
        assert_eq!(endpoint("https://api.openai.com/v1/decisions"), ENDPOINT);
    }

    #[test]
    fn every_part_of_a_question_reaches_the_request_or_fails_it() {
        let score = json!({
            "type": "score",
            "instructions": { "task": "T", "question": "Q?" },
            "criteria": [{ "what": "low", "examples": ["a", "b"] }, "mid", { "what": "high", "not_for": "c" }]
        });
        let style = json!({ "type": "score", "instructions": "S?", "criteria": ["a", "b"] });
        let body = body("gpt-6-luna", &json!({ "after": "x" }), &[("q", &score), ("style", &style)]).unwrap();
        assert_eq!(body["input"], "{\n  \"after\": \"x\"\n}");
        // the questions about one state go in one request
        assert_eq!(body["questions"][1]["name"], "style");
        let q = &body["questions"][0];
        assert_eq!(q["instructions"], "T\n\nQ?");
        assert_eq!(q["levels"][0], json!({ "label": "0", "description": "low\nExamples:\n- a\n- b" }));
        assert_eq!(q["levels"][2], json!({ "label": "2", "description": "high\nNot for: c" }));
        let choice = json!({ "type": "choice", "instructions": "Q?", "criteria": { "yes": "Y", "no": { "what": "N" } } });
        let q = translate("q", &choice).unwrap();
        assert_eq!(q["choices"], json!([{ "value": "no", "description": "N" }, { "value": "yes", "description": "Y" }]));

        let mut unknown = score.clone();
        unknown["criteria"][1] = json!({ "what": "mid", "weight": 2 });
        assert!(translate("q", &unknown).unwrap_err().contains("`weight`"));
        unknown = score.clone();
        unknown["instructions"]["hint"] = json!("h");
        assert!(translate("q", &unknown).unwrap_err().contains("`hint`"));
    }

    #[test]
    fn answers_read_like_typesafe_answers_and_a_refusal_is_a_failure() {
        let text = r#"{"model": "gpt-6-luna", "answers": [
            {"type": "choice", "name": "q", "choice": "a", "confidence": 0.9,
             "probabilities": [{"value": "a", "probability": 0.8}, {"value": "b", "probability": 0.2}]},
            {"type": "refusal", "name": "r"}],
            "usage": {"input_tokens": 12, "output_tokens": 0, "total_tokens": 12}}"#;
        let mut reply = read(text).unwrap();
        let choice = reply.answers.remove("q").unwrap().unwrap();
        assert_eq!(choice.choice.as_deref(), Some("a"));
        assert_eq!(choice.probabilities.unwrap(), BTreeMap::from([("a".into(), 0.8), ("b".into(), 0.2)]));
        // a refusal fails only the question it refuses
        assert!(reply.answers.remove("r").unwrap().unwrap_err().contains("refused"));
        assert_eq!(reply.usage.unwrap().input_tokens, 12);
        assert_eq!(reply.model.as_deref(), Some("gpt-6-luna"));

        let score = r#"{"answers": [{"type": "score", "name": "q", "score": 1.1, "confidence": 0.5, "probabilities": [
            {"value": 0, "label": "0", "probability": 0.1}, {"value": 1, "label": "1", "probability": 0.7},
            {"value": 2, "label": "2", "probability": 0.2}]}]}"#;
        let reply = read(score).unwrap();
        assert_eq!(reply.answers["q"].as_ref().unwrap().probabilities.as_ref().unwrap()["1"], 0.7);
        assert_eq!(reply.model, None);

        assert!(read(r#"{"answers": [{"type": "predicate", "name": "q", "probability": 0.5}]}"#).is_err());
        assert!(read(r#"{"answers": [{"type": "refusal"}]}"#).unwrap_err().contains("name"));
        assert!(read(r#"{"answers": [{"type": "refusal", "name": "q"}, {"type": "refusal", "name": "q"}]}"#)
            .unwrap_err().contains("two answers"));
    }
}
