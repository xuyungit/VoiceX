//! The judge: TypeSafe System One. Three fixed questions, each distinct state asked once.
//!
//! An answer is used only when it is complete: every level or option with its probability, and a confidence. Anything
//! else is a failure of the judge, reported as one, and leaves the items that waited for it unjudged.

use crate::adjudicate::{Answer, Answers, Ask, AskKind, EDIT_CREDIT, GATE_KINDS, SITE_CREDIT};
use reqwest::Client;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Duration;
use tokio::task::JoinSet;

const BUILT_IN_QUESTIONS: &str = include_str!("../typesafe_questions.json");
const ENDPOINT: &str = "https://api.typesafe.ai/v1/systemone";
/// 429 (rate limit), 529 (overloaded) and transport errors are retried after 1, 2 and 4 seconds.
const ATTEMPTS: u32 = 4;

/// Accepts the endpoint itself, its `/v1` base, or nothing.
pub fn endpoint(base_url: &str) -> String {
    let base = base_url.trim().trim_end_matches('/');
    if base.is_empty() {
        ENDPOINT.to_string()
    } else if base.ends_with("/systemone") {
        base.to_string()
    } else {
        format!("{}/systemone", base)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Questions {
    site: Value,
    edit: Value,
    gate: Value,
}

impl Questions {
    /// The built-in questions, or the file named by `eval.typesafe_questions`.
    pub fn load(path: Option<&str>) -> Result<Self, String> {
        match path {
            None => Self::parse(BUILT_IN_QUESTIONS).map_err(|e| format!("built-in typesafe_questions.json: {}", e)),
            Some(path) => {
                let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {}", path, e))?;
                Self::parse(&text).map_err(|e| format!("{}: {}", path, e))
            }
        }
    }

    /// The scoring code knows each question by its answers, so a questions file has to offer exactly those.
    fn parse(text: &str) -> Result<Self, String> {
        let questions: Questions = serde_json::from_str(text).map_err(|e| e.to_string())?;
        let kind = |q: &Value| q["type"].as_str().map(str::to_string);
        let levels = questions.site["criteria"].as_array().map(Vec::len);
        if kind(&questions.site).as_deref() != Some("score") || levels != Some(SITE_CREDIT.len()) {
            return Err(format!("`site` must be a `score` question with {} criteria", SITE_CREDIT.len()));
        }
        let edit_kinds: Vec<&str> = EDIT_CREDIT.iter().map(|(k, _)| *k).collect();
        for (name, question, options) in [("edit", &questions.edit, &edit_kinds[..]), ("gate", &questions.gate, &GATE_KINDS[..])] {
            let offered: Option<HashSet<&str>> = question["criteria"].as_object().map(|c| c.keys().map(String::as_str).collect());
            if kind(question).as_deref() != Some("choice") || offered != Some(options.iter().copied().collect()) {
                return Err(format!("`{}` must be a `choice` question between exactly: {}", name, options.join(", ")));
            }
        }
        Ok(questions)
    }

    fn of(&self, kind: AskKind) -> &Value {
        match kind {
            AskKind::Site => &self.site,
            AskKind::Edit => &self.edit,
            AskKind::Gate => &self.gate,
        }
    }
}

#[derive(Deserialize)]
struct Response {
    answers: HashMap<String, RawAnswer>,
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct Usage {
    input_tokens: u64,
    output_tokens: u64,
}

#[derive(Deserialize)]
struct RawAnswer {
    choice: Option<String>,
    probabilities: Option<BTreeMap<String, f64>>,
    confidence: Option<f64>,
}

fn read_answer(kind: AskKind, raw: RawAnswer) -> Result<Answer, String> {
    let confidence = raw.confidence.ok_or("answer without `confidence`")?;
    let p = raw.probabilities.ok_or("answer without `probabilities`")?;
    let of = |option: &str| p.get(option).copied().ok_or(format!("answer without a probability for `{}`", option));
    if kind == AskKind::Site {
        return Ok(Answer::Levels { p: [of("0")?, of("1")?, of("2")?], confidence });
    }
    let options: Vec<&str> = match kind {
        AskKind::Edit => EDIT_CREDIT.iter().map(|(k, _)| *k).collect(),
        _ => GATE_KINDS.to_vec(),
    };
    let choice = raw.choice.ok_or("answer without `choice`")?;
    if !options.contains(&choice.as_str()) {
        return Err(format!("answer chose `{}`, which the question does not offer", choice));
    }
    for option in &options {
        of(option)?;
    }
    Ok(Answer::Choice { choice, p, confidence })
}

fn short(text: &str) -> String {
    let head: String = text.chars().take(200).collect();
    if head.len() < text.len() {
        format!("{}…", head)
    } else {
        head
    }
}

async fn ask_one(http: Client, url: String, api_key: String, body: Value, kind: AskKind) -> Result<(Answer, u64, u64), String> {
    let mut attempt = 0;
    let text = loop {
        attempt += 1;
        let failure = match http.post(&url).bearer_auth(&api_key).json(&body).send().await {
            Err(e) => format!("request failed: {}", e),
            Ok(response) => {
                let status = response.status();
                let text = response.text().await.map_err(|e| format!("unreadable response: {}", e))?;
                if status.is_success() {
                    break text;
                }
                let failure = format!("HTTP {}: {}", status, short(&text));
                if !matches!(status.as_u16(), 429 | 529) {
                    return Err(failure);
                }
                failure
            }
        };
        if attempt == ATTEMPTS {
            return Err(format!("{} (after {} attempts)", failure, ATTEMPTS));
        }
        tokio::time::sleep(Duration::from_secs(1 << (attempt - 1))).await;
    };
    let mut response: Response = serde_json::from_str(&text).map_err(|e| format!("{}: {}", e, short(&text)))?;
    let raw = response.answers.remove("q").ok_or_else(|| format!("no answer in response: {}", short(&text)))?;
    let answer = read_answer(kind, raw).map_err(|e| format!("{}: {}", e, short(&text)))?;
    let usage = response.usage.unwrap_or(Usage { input_tokens: 0, output_tokens: 0 });
    Ok((answer, usage.input_tokens, usage.output_tokens))
}

pub struct Judge {
    pub name: String,
    pub http: Client,
    pub url: String,
    pub api_key: String,
    pub model: String,
    pub questions: Questions,
    pub concurrency: usize,
}

#[derive(Default)]
pub struct JudgeRun {
    pub answers: Answers,
    /// Questions that got no usable answer: what was asked, and what went wrong.
    pub failures: Vec<(String, String)>,
    pub distinct: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

impl Judge {
    /// Asks every distinct question once, `concurrency` at a time.
    pub async fn ask_all(&self, asks: Vec<Ask>) -> JudgeRun {
        let mut seen = HashSet::new();
        let mut pending = asks.into_iter().filter(|ask| seen.insert(ask.key())).collect::<Vec<_>>().into_iter();
        let mut run = JudgeRun { distinct: pending.len(), ..Default::default() };
        let mut running = JoinSet::new();
        loop {
            while running.len() < self.concurrency.max(1) {
                let Some(ask) = pending.next() else { break };
                let body = json!({ "state": ask.state, "model": self.model, "questions": { "q": self.questions.of(ask.kind) } });
                let call = ask_one(self.http.clone(), self.url.clone(), self.api_key.clone(), body, ask.kind);
                running.spawn(async move { (ask, call.await) });
            }
            let Some(joined) = running.join_next().await else { break };
            let (ask, result) = joined.expect("a judge call panicked");
            match result {
                Ok((answer, input_tokens, output_tokens)) => {
                    run.answers.insert(ask.key(), answer);
                    run.input_tokens += input_tokens;
                    run.output_tokens += output_tokens;
                }
                Err(error) => run.failures.push((ask.label, error)),
            }
        }
        run
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_accepts_bare_v1_or_full_endpoint() {
        assert_eq!(endpoint(""), ENDPOINT);
        assert_eq!(endpoint("https://api.typesafe.ai/v1"), ENDPOINT);
        assert_eq!(endpoint("https://api.typesafe.ai/v1/"), ENDPOINT);
        assert_eq!(endpoint("https://api.typesafe.ai/v1/systemone"), ENDPOINT);
    }

    #[test]
    fn built_in_questions_offer_exactly_the_answers_the_scoring_knows() {
        Questions::parse(BUILT_IN_QUESTIONS).unwrap();
        let without_a_kind = BUILT_IN_QUESTIONS.replace("\"rephrases\"", "\"rewords\"");
        assert!(Questions::parse(&without_a_kind).unwrap_err().contains("`edit` must be a `choice` question"));
    }

    fn raw(json: &str) -> RawAnswer {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn an_incomplete_answer_is_a_failure_not_a_default() {
        let levels = raw(r#"{"score": 1.4, "probabilities": {"0": 0.1, "1": 0.4, "2": 0.5}, "confidence": 0.2}"#);
        assert!(matches!(read_answer(AskKind::Site, levels), Ok(Answer::Levels { p, .. }) if p == [0.1, 0.4, 0.5]));
        let no_level = raw(r#"{"probabilities": {"0": 0.1, "1": 0.9}, "confidence": 0.2}"#);
        assert!(read_answer(AskKind::Site, no_level).unwrap_err().contains("probability for `2`"));
        let no_confidence = raw(r#"{"probabilities": {"0": 0.1, "1": 0.4, "2": 0.5}}"#);
        assert!(read_answer(AskKind::Site, no_confidence).unwrap_err().contains("confidence"));
        let unknown = raw(r#"{"choice": "fine", "probabilities": {"fine": 1.0}, "confidence": 0.9}"#);
        assert!(read_answer(AskKind::Gate, unknown).unwrap_err().contains("does not offer"));
    }
}
