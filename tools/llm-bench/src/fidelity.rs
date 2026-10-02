//! Severe hallucination gate for every output, independent of correction quality.
use crate::adjudicate::{Answer, Ask, AskKind, Scored};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;

/// Low-confidence passes also require review: severe changes must not slip through weak assurances.
pub const CLEAR_CONFIDENCE: f64 = 0.7;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verdict {
    /// None is an intermediate state; the automatic reviewer must settle it before scoring.
    pub credit: Option<f64>,
    pub reasons: Vec<String>,
    pub identifier_changes: Option<[Vec<String>; 2]>,
    pub judge_choice: Option<String>,
    pub confidence: Option<f64>,
    pub probabilities: Option<BTreeMap<String, f64>>,
    #[serde(default)]
    pub secondary: Option<Secondary>,
    #[serde(default)]
    pub evaluation_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Secondary {
    pub model: String,
    pub severe: bool,
    pub reason: String,
    pub attempts: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewAnswer {
    pub severe: bool,
    pub reason: String,
}

pub fn parse_review(text: &str) -> Result<ReviewAnswer, String> {
    let text = text.trim();
    let text = text
        .strip_prefix("```json")
        .or_else(|| text.strip_prefix("```"))
        .and_then(|v| v.strip_suffix("```"))
        .unwrap_or(text)
        .trim();
    let answer: ReviewAnswer =
        serde_json::from_str(text).map_err(|e| format!("invalid binary review: {e}"))?;
    if answer.reason.trim().is_empty() {
        return Err("review has no reason".into());
    }
    Ok(answer)
}

pub fn finalize(
    verdict: &mut Verdict,
    model: &str,
    answer: Result<ReviewAnswer, String>,
    attempts: usize,
) {
    match answer {
        Ok(answer) => {
            verdict.credit = Some(if answer.severe { 0.0 } else { 1.0 });
            verdict.secondary = Some(Secondary {
                model: model.into(),
                severe: answer.severe,
                reason: answer.reason,
                attempts,
            });
        }
        Err(error) => {
            verdict.credit = Some(0.0);
            verdict.reasons.push(format!("evaluation failed: {error}"));
            verdict.evaluation_error = Some(error);
        }
    }
}

impl Verdict {
    pub fn needs_review(&self) -> bool {
        self.credit.is_none()
            || (self.credit == Some(0.0)
                && self.secondary.is_none()
                && self.evaluation_error.is_none())
    }
}

/// ASCII names/numbers in order. Case, spacing and hyphens are formatting;
/// decimal points are content. Repeated adjacent identifiers permit stutter removal.
fn identifiers(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut token = String::new();
    for (i, &c) in chars.iter().enumerate() {
        let decimal = c == '.'
            && i > 0
            && i + 1 < chars.len()
            && chars[i - 1].is_ascii_digit()
            && chars[i + 1].is_ascii_digit();
        if c.is_ascii_alphanumeric() || c == '_' || decimal {
            token.push(c.to_ascii_lowercase());
        } else if !token.is_empty() {
            tokens.push(std::mem::take(&mut token));
        }
    }
    if !token.is_empty() {
        tokens.push(token);
    }
    tokens.dedup();
    tokens
}

pub fn ask_with_reference(input: &str, reference: &str, output: &str) -> Ask {
    // Judge the whole utterance; identifier changes are evidence, not a verdict.
    Ask {
        kind: AskKind::Fidelity,
        state: json!({ "before": input, "reference": reference, "after": output,
        "identifier_hints": { "before": identifiers(input), "after": identifiers(output) } }),
        label: "severe hallucination: whole transcript".into(),
    }
}

pub fn check(input: &str, output: &str, error: Option<&str>, answer: Option<&Answer>) -> Verdict {
    let mut verdict = Verdict {
        credit: None,
        reasons: Vec::new(),
        judge_choice: None,
        confidence: None,
        probabilities: None,
        identifier_changes: None,
        secondary: None,
        evaluation_error: None,
    };
    if let Some(error) = error {
        verdict.reasons.push(format!("call failed: {error}"));
    }
    let before = identifiers(input);
    let after = identifiers(output);
    if before != after {
        verdict.identifier_changes = Some([before, after]);
    }
    if !verdict.reasons.is_empty() {
        verdict.credit = Some(0.0);
        return verdict;
    }
    if input == output {
        verdict.credit = Some(1.0); // no changes can introduce a hallucination
        return verdict;
    }
    if let Some(Answer::Choice {
        choice,
        p,
        confidence,
    }) = answer
    {
        verdict.judge_choice = Some(choice.clone());
        verdict.confidence = Some(*confidence);
        verdict.probabilities = Some(p.clone());
        if *confidence >= CLEAR_CONFIDENCE {
            match choice.as_str() {
                "faithful" => verdict.credit = Some(1.0),
                "changed" => {
                    verdict.credit = Some(0.0);
                    verdict
                        .reasons
                        .push(format!("JEV whole transcript: {choice}"));
                }
                _ => {}
            }
        }
    }
    verdict
}

/// A clear result earns no extra correction credit. Only a severe hallucination
/// zeros quality. Evaluator errors also zero quality, with separate evidence.
pub fn apply(scored: &mut Scored, verdict: &Verdict) {
    match verdict.credit {
        Some(0.0) => {
            scored.clean = Some(0.0);
            for site in &mut scored.sites {
                site.credit = Some(0.0);
                site.how = if verdict.evaluation_error.is_some() {
                    "evaluation_failed"
                } else {
                    "severe_hallucination"
                }
                .into();
            }
            // A conclusive zero does not depend on unanswered correction questions.
            scored.unjudged.clear();
        }
        None => scored
            .unjudged
            .push("severe hallucination check unresolved; quality is provisional".into()),
        _ => {}
    }
}

#[derive(Clone, Copy, Default, Debug, Serialize, Deserialize)]
pub struct Stats {
    pub clear: usize,
    pub severe: usize,
    pub pending: usize,
    pub call_failed: usize,
    #[serde(default)]
    pub evaluation_failed: usize,
}

impl Stats {
    pub fn add(&mut self, verdict: &Verdict, call_failed: bool) {
        if call_failed {
            self.call_failed += 1;
            return;
        }
        if verdict.evaluation_error.is_some() {
            self.evaluation_failed += 1;
            return;
        }
        match verdict.credit {
            Some(1.0) => self.clear += 1,
            Some(0.0) => self.severe += 1,
            _ => self.pending += 1,
        }
    }
    pub fn merge(&mut self, other: Self) {
        self.clear += other.clear;
        self.severe += other.severe;
        self.pending += other.pending;
        self.call_failed += other.call_failed;
        self.evaluation_failed += other.evaluation_failed;
    }
    pub fn coverage(&self) -> Option<f64> {
        let n = self.clear + self.severe + self.pending + self.evaluation_failed;
        (n > 0).then(|| (self.clear + self.severe) as f64 / n as f64)
    }
    pub fn display(&self) -> String {
        format!(
            "{}/{} H{} E{}",
            self.clear + self.severe,
            self.clear + self.severe + self.pending + self.evaluation_failed,
            self.severe,
            self.evaluation_failed
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_review_is_binary_and_keeps_errors_distinct() {
        assert!(parse_review(r#"{"severe":"uncertain","reason":"unsure"}"#).is_err());
        assert!(parse_review(r#"{"severe":false,"reason":""}"#).is_err());
        let mut verdict = check("GPT 6.1", "GPT 4", None, None);
        finalize(
            &mut verdict,
            "reviewer",
            Ok(
                parse_review(r#"{"severe":true,"reason":"version changed from 6.1 to 4"}"#)
                    .unwrap(),
            ),
            1,
        );
        assert_eq!(verdict.credit, Some(0.0));
        assert_eq!(verdict.secondary.as_ref().unwrap().attempts, 1);
        let mut stats = Stats::default();
        stats.add(&verdict, false);
        finalize(&mut verdict, "reviewer", Err("timeout".into()), 2);
        stats.add(&verdict, false);
        assert_eq!(
            (stats.severe, stats.evaluation_failed, stats.pending),
            (1, 1, 0)
        );
    }

    #[test]
    fn a_probe_supplies_the_raw_text_and_correction_reference_separately() {
        let ask = ask_with_reference("用 Abacus", "用 Abaqus", "使用 Abaqus");
        assert_eq!(ask.state["before"], "用 Abacus");
        assert_eq!(ask.state["reference"], "用 Abaqus");
        assert_eq!(ask.state["after"], "使用 Abaqus");
    }

    #[test]
    fn identifier_changes_are_hints_and_never_decide_a_semantic_verdict() {
        let input = "用 Luna、GPT 6.1、Sora";
        let output = "用 LLaMA、GPT-4o";
        let pending = check(input, output, None, None);
        assert!(pending.identifier_changes.is_some());
        assert_eq!(pending.credit, None);
        let changed = Answer::Choice {
            choice: "changed".into(),
            p: BTreeMap::new(),
            confidence: 0.9,
        };
        assert_eq!(check(input, output, None, Some(&changed)).credit, Some(0.0));
        // Names may reorder in an equivalent paraphrase; the judge owns this decision.
        let faithful = Answer::Choice {
            choice: "faithful".into(),
            p: BTreeMap::new(),
            confidence: 0.9,
        };
        assert_eq!(
            check(input, "用 Sora、GPT-6.1、Luna", None, Some(&faithful)).credit,
            Some(1.0)
        );
        assert_eq!(check(input, input, None, None).credit, Some(1.0));
        assert_eq!(
            check(input, output, Some("timeout"), None).credit,
            Some(0.0)
        );
    }

    #[test]
    fn uncertain_low_confidence_and_missing_judgments_never_pass_or_get_partial_credit() {
        let input = "Luna 比较便宜吧";
        let output = "Luna 是免费的";
        let answer = |choice: &str, confidence| Answer::Choice {
            choice: choice.into(),
            p: BTreeMap::new(),
            confidence,
        };
        assert_eq!(
            check(input, output, None, Some(&answer("changed", 0.9))).credit,
            Some(0.0)
        );
        assert_eq!(
            check(input, output, None, Some(&answer("changed", 0.1))).credit,
            None
        );
        assert!(check(input, output, None, Some(&answer("changed", 0.9))).needs_review());
        assert!(check(input, output, None, Some(&answer("faithful", 0.65))).needs_review());
        assert!(!check(input, output, None, Some(&answer("faithful", 0.9))).needs_review());
        assert_eq!(
            check(input, output, None, Some(&answer("uncertain", 0.9))).credit,
            None
        );
        assert_eq!(check(input, output, None, None).credit, None);
        let mut scored = crate::adjudicate::score(
            &crate::adjudicate::Reference::new(input, input, &[]).analyze(input, &[]),
            &crate::adjudicate::Answers::new(),
        );
        apply(
            &mut scored,
            &check(input, output, None, Some(&answer("changed", 0.9))),
        );
        assert_eq!(scored.clean, Some(0.0));
    }

    #[test]
    fn passing_or_pending_gate_never_grants_credit_for_uncorrected_errors() {
        let reference =
            crate::adjudicate::Reference::new("用 Abacus", "用 Abaqus", &["Abaqus".into()]);
        let original = crate::adjudicate::score(
            &reference.analyze("用 Abacus", &[]),
            &crate::adjudicate::Answers::new(),
        );
        assert_eq!(original.sites[0].credit, Some(0.0));
        for credit in [Some(1.0), None] {
            let mut gated = original.clone();
            let mut verdict = check("用 Abacus", "用 Abacus", None, None);
            verdict.credit = credit;
            apply(&mut gated, &verdict);
            assert_eq!(gated.sites[0].credit, original.sites[0].credit);
            assert_eq!(gated.clean, original.clean);
            assert_eq!(
                gated.unjudged.len(),
                original.unjudged.len() + usize::from(credit.is_none())
            );
        }
        let mut gated = crate::adjudicate::score(
            &reference.analyze("用 Abaqus", &[]),
            &crate::adjudicate::Answers::new(),
        );
        assert_eq!(gated.sites[0].credit, Some(1.0));
        let changed = Answer::Choice {
            choice: "changed".into(),
            p: BTreeMap::new(),
            confidence: 0.9,
        };
        apply(
            &mut gated,
            &check("用 Abacus", "用 ANSYS", None, Some(&changed)),
        );
        assert!(gated.sites.iter().all(|s| s.credit == Some(0.0)));
        assert_eq!(gated.clean, Some(0.0));
    }

    #[test]
    fn missing_local_judge_answers_are_final_errors_in_the_denominator() {
        let reference = crate::adjudicate::Reference::new("使用它", "使用它", &[]);
        let mut scored = crate::adjudicate::score(
            &reference.analyze("使用另一个软件", &[]),
            &crate::adjudicate::Answers::new(),
        );
        assert!(!scored.unjudged.is_empty());
        scored.finalize_missing();
        assert_eq!(scored.clean, Some(0.0));
        assert!(scored.unjudged.is_empty());
        assert!(!scored.evaluation_errors.is_empty());
        assert!(scored.edits.iter().all(|e| e.credit.is_some()));
    }

    #[test]
    fn coverage_keeps_pending_and_call_failure_distinct_from_hallucinations() {
        let mut stats = Stats::default();
        let mut verdict = check("原文", "原文", None, None);
        stats.add(&verdict, false);
        verdict.credit = None;
        stats.add(&verdict, false);
        verdict.credit = Some(0.0);
        stats.add(&verdict, false);
        stats.add(&verdict, true);
        assert_eq!(
            (stats.clear, stats.severe, stats.pending, stats.call_failed),
            (1, 1, 1, 1)
        );
        assert_eq!(stats.coverage(), Some(2.0 / 3.0));
        assert_eq!(Stats::default().coverage(), None);
    }
}
