//! Case-balanced scoring. Occurrences remain evidence; fixed repair tasks share
//! credit, rounds average within a case, and cases average within a model.
use crate::adjudicate::{Answer, Answers, Ask, AskKind, CorrectionTask, Reference, Scored, Tier};
use crate::fidelity::Verdict;
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Weights {
    pub correction: f64,
    pub fidelity: f64,
    pub cleanup: f64,
    pub latency: f64,
}

impl Default for Weights {
    fn default() -> Self {
        Self {
            correction: 0.30,
            fidelity: 0.40,
            cleanup: 0.20,
            latency: 0.10,
        }
    }
}

impl Weights {
    pub fn validate(self) -> Result<(), String> {
        let values = [self.correction, self.fidelity, self.cleanup, self.latency];
        if values
            .iter()
            .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
        {
            return Err("scoring weights must be finite fractions between 0 and 1".into());
        }
        if (values.iter().sum::<f64>() - 1.0).abs() > 1e-9 || self.latency >= 1.0 {
            return Err("weight_correction + weight_fidelity + weight_clean + weight_latency must sum to 1, with a nonzero quality weight".into());
        }
        Ok(())
    }
    pub fn composite(self, m: Metrics) -> f64 {
        100.0
            * (self.correction * m.correction
                + self.fidelity * m.fidelity
                + self.cleanup * m.cleanup
                + self.latency * m.latency)
    }
    pub fn ability(self, m: Metrics) -> f64 {
        100.0
            * (self.correction * m.correction
                + self.fidelity * m.fidelity
                + self.cleanup * m.cleanup)
            / (1.0 - self.latency)
    }
}

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize)]
pub struct Metrics {
    pub correction: f64,
    pub fidelity: f64,
    pub tidy: f64,
    pub readability: f64,
    pub style: f64,
    /// Half tidy, one quarter readability, one quarter style: 10/5/5 at default weights.
    pub cleanup: f64,
    pub latency: f64,
}

pub fn mean(items: impl IntoIterator<Item = Metrics>) -> Metrics {
    let mut sum = Metrics::default();
    let mut count = 0;
    for m in items {
        count += 1;
        sum.correction += m.correction;
        sum.fidelity += m.fidelity;
        sum.tidy += m.tidy;
        sum.readability += m.readability;
        sum.style += m.style;
        sum.cleanup += m.cleanup;
        sum.latency += m.latency;
    }
    if count > 0 {
        let n = count as f64;
        sum.correction /= n;
        sum.fidelity /= n;
        sum.tidy /= n;
        sum.readability /= n;
        sum.style /= n;
        sum.cleanup /= n;
        sum.latency /= n;
    }
    sum
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TaskCredit {
    pub id: String,
    pub tier: Tier,
    pub heard: String,
    pub intended: String,
    pub occurrences: usize,
    pub occurrence_credits: Vec<f64>,
    pub credit: f64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct QualityGrade {
    pub credit: f64,
    pub confidence: Option<f64>,
    pub probabilities: Option<[f64; 3]>,
    pub error: Option<String>,
}

pub(crate) fn quality_grade(answer: Option<&Answer>) -> QualityGrade {
    match answer {
        Some(Answer::Levels { p, confidence })
            if p.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v))
                && (p.iter().sum::<f64>() - 1.0).abs() <= 0.021 =>
        {
            QualityGrade {
                credit: (0.5 * p[1] + p[2]) / p.iter().sum::<f64>(),
                confidence: Some(*confidence),
                probabilities: Some(*p),
                error: None,
            }
        }
        _ => QualityGrade {
            credit: 0.0,
            confidence: None,
            probabilities: None,
            error: Some("missing or malformed quality judgment; this component scores zero".into()),
        },
    }
}

pub fn quality_ask(kind: AskKind, input: &str, reference: &str, output: &str) -> Ask {
    assert!(matches!(kind, AskKind::Readability | AskKind::Style));
    Ask {
        kind,
        state: json!({"before": input.trim(), "reference": reference.trim(), "after": output}),
        label: format!("{}: whole transcript", kind.name()),
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RoundScore {
    /// Before the severe-hallucination gate. A missed repair only changes its task.
    pub base: Metrics,
    /// Quality zeroed only for severe hallucination or failed whole-text evaluation.
    pub final_metrics: Metrics,
    pub tasks: Vec<TaskCredit>,
    pub readability_judgment: QualityGrade,
    pub style_judgment: QualityGrade,
    pub quality_zeroed: bool,
    pub evaluation_errors: Vec<String>,
}

fn task_score(task: &CorrectionTask, score: Option<&Scored>) -> TaskCredit {
    let occurrence_credits: Vec<f64> = task
        .site_indices
        .iter()
        .map(|&i| score.and_then(|s| s.sites[i].credit).unwrap_or(0.0))
        .collect();
    let credit = occurrence_credits.iter().sum::<f64>() / occurrence_credits.len() as f64;
    TaskCredit {
        id: task.id.clone(),
        tier: task.tier,
        heard: task.heard.clone(),
        intended: task.intended.clone(),
        occurrences: task.site_indices.len(),
        occurrence_credits,
        credit,
    }
}

pub fn score(
    reference: &Reference,
    output: &str,
    base: Option<&Scored>,
    verdict: &Verdict,
    answers: &Answers,
    latency: f64,
    call_failed: bool,
) -> RoundScore {
    if let Some(base) = base {
        assert_eq!(
            base.sites.len(),
            reference.sites().len(),
            "saved site plan differs; rejudge this run"
        );
    }
    let tasks: Vec<_> = reference
        .correction_tasks()
        .iter()
        .map(|t| task_score(t, base))
        .collect();
    let correction = if tasks.is_empty() {
        1.0
    } else {
        tasks.iter().map(|t| t.credit).sum::<f64>() / tasks.len() as f64
    };
    let cleanup: Vec<f64> = reference
        .sites()
        .iter()
        .enumerate()
        .filter(|(_, s)| s.0 == Tier::Cleanup)
        .map(|(i, _)| base.and_then(|s| s.sites[i].credit).unwrap_or(0.0))
        .collect();
    let tidy = if cleanup.is_empty() {
        1.0
    } else {
        cleanup.iter().sum::<f64>() / cleanup.len() as f64
    };
    let input = reference.input();
    let expected = reference.expected();
    let readability_judgment = quality_grade(
        answers.get(&quality_ask(AskKind::Readability, &input, expected, output).key()),
    );
    let style_judgment =
        quality_grade(answers.get(&quality_ask(AskKind::Style, &input, expected, output).key()));
    let quality_zeroed = verdict.credit != Some(1.0);
    let mut base_metrics = Metrics {
        correction,
        fidelity: verdict.credit.unwrap_or(0.0),
        tidy,
        readability: readability_judgment.credit,
        style: style_judgment.credit,
        cleanup: 0.0,
        latency,
    };
    base_metrics.cleanup = 0.5 * tidy + 0.25 * base_metrics.readability + 0.25 * base_metrics.style;
    let final_metrics = if call_failed {
        Metrics::default()
    } else if quality_zeroed {
        Metrics {
            latency,
            ..Metrics::default()
        }
    } else {
        base_metrics
    };
    let mut evaluation_errors = base
        .map(|s| s.evaluation_errors.clone())
        .unwrap_or_default();
    if !call_failed {
        for (name, result) in [
            ("readability", &readability_judgment),
            ("style", &style_judgment),
        ] {
            if let Some(e) = &result.error {
                evaluation_errors.push(format!("{name}: {e}"));
            }
        }
        if let Some(e) = &verdict.evaluation_error {
            evaluation_errors.push(e.clone());
        }
    }
    RoundScore {
        base: base_metrics,
        final_metrics,
        tasks,
        readability_judgment,
        style_judgment,
        quality_zeroed,
        evaluation_errors,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adjudicate;
    use crate::fidelity;
    fn evaluated(input: &str, expected: &str, output: &str, terms: &[&str]) -> RoundScore {
        let reference = Reference::new(
            input,
            expected,
            &terms.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        );
        let base = adjudicate::score(&reference.analyze(output, &[]), &Answers::new());
        let mut answers = Answers::new();
        for kind in [AskKind::Readability, AskKind::Style] {
            answers.insert(
                quality_ask(kind, input, expected, output).key(),
                Answer::Levels {
                    p: [0.0, 0.0, 1.0],
                    confidence: 1.0,
                },
            );
        }
        let mut verdict = fidelity::check(input, input, None, None);
        verdict.credit = Some(1.0);
        score(
            &reference,
            output,
            Some(&base),
            &verdict,
            &answers,
            1.0,
            false,
        )
    }
    #[test]
    fn repeated_missed_term_only_costs_its_share_of_correction() {
        let input = "先用 Abacus 建模，再用 Abacus 求解，之后用 Abacus 查看，最后用 Abacus 导出，也用麦达斯。";
        let expected = "先用 Abaqus 建模，再用 Abaqus 求解，之后用 Abaqus 查看，最后用 Abaqus 导出，也用Midas。";
        let missed = evaluated(
            input,
            expected,
            &input.replace("麦达斯", "Midas"),
            &["Abaqus", "Midas"],
        );
        assert_eq!(missed.tasks.len(), 2);
        assert_eq!(missed.tasks[0].occurrences, 4);
        assert_eq!(missed.final_metrics.correction, 0.5);
        assert_eq!(Weights::default().composite(missed.final_metrics), 85.0);
        let one = evaluated(
            "Abacus、麦达斯",
            "Abaqus、Midas",
            "Abacus、Midas",
            &["Abaqus", "Midas"],
        );
        assert_eq!(
            one.final_metrics.correction,
            missed.final_metrics.correction
        );
        let partial = evaluated(
            input,
            expected,
            &input
                .replacen("Abacus", "Abaqus", 2)
                .replace("麦达斯", "Midas"),
            &["Abaqus", "Midas"],
        );
        assert_eq!(partial.tasks[0].credit, 0.5);
        assert_eq!(partial.final_metrics.correction, 0.75);
    }
    #[test]
    fn six_equal_cases_bound_one_missed_task_to_two_and_a_half_points() {
        let perfect = Metrics {
            correction: 1.0,
            fidelity: 1.0,
            cleanup: 1.0,
            latency: 1.0,
            ..Metrics::default()
        };
        let average = mean([
            perfect,
            perfect,
            perfect,
            perfect,
            perfect,
            Metrics {
                correction: 0.5,
                ..perfect
            },
        ]);
        assert!((Weights::default().composite(average) - 97.5).abs() < 1e-9);
    }
    #[test]
    fn round_count_does_not_change_case_weight_and_failed_calls_stay_in_denominator() {
        let pass = Metrics {
            correction: 1.0,
            fidelity: 1.0,
            cleanup: 1.0,
            latency: 1.0,
            ..Metrics::default()
        };
        assert!(
            (Weights::default().composite(mean([mean([pass; 9]), mean([Metrics::default()])]))
                - 50.0)
                .abs()
                < 1e-9
        );
        assert!(
            (Weights::default().composite(mean([pass, Metrics::default()])) - 50.0).abs() < 1e-9
        );
    }
    #[test]
    fn missing_quality_is_zero_in_its_component_and_bad_weights_are_rejected() {
        assert_eq!(quality_grade(None).credit, 0.0);
        assert!(quality_grade(None).error.is_some());
        assert!(Weights {
            correction: 0.4,
            ..Weights::default()
        }
        .validate()
        .is_err());
        assert!(Weights {
            correction: f64::NAN,
            ..Weights::default()
        }
        .validate()
        .is_err());
        Weights::default().validate().unwrap();
    }
    #[test]
    fn severe_hallucination_only_zeros_this_round_quality() {
        let reference = Reference::new("保留 GPT 6.1", "保留 GPT 6.1", &[]);
        let mut verdict = fidelity::check("保留 GPT 6.1", "保留 GPT 4", None, None);
        fidelity::finalize(
            &mut verdict,
            "reviewer",
            Ok(fidelity::ReviewAnswer {
                severe: true,
                reason: "changed version".into(),
            }),
            1,
        );
        let result = score(
            &reference,
            "保留 GPT 4",
            None,
            &verdict,
            &Answers::new(),
            0.8,
            false,
        );
        assert_eq!(result.final_metrics.correction, 0.0);
        assert_eq!(result.final_metrics.fidelity, 0.0);
        assert_eq!(result.final_metrics.cleanup, 0.0);
        assert!((Weights::default().composite(result.final_metrics) - 8.0).abs() < 1e-9);
    }
}
