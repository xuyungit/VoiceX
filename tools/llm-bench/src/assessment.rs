//! Evidence-based, deterministic one-sentence reviews; no extra judge requests.
use crate::adjudicate::Tier;
use crate::scoring::Weights;
use crate::{CaseRecord, RankedProvider};
use serde::Serialize;

pub const VERSION: u32 = 1;

#[derive(Debug, Serialize)]
pub struct Assessment {
    pub sentence: String,
    pub evidence: Evidence,
}

#[derive(Debug, Serialize)]
pub struct Evidence {
    pub basis: &'static str,
    pub cases: usize,
    pub calls: usize,
    pub successful_calls: usize,
    pub failed_calls: usize,
    pub evaluated_fidelity_outputs: usize,
    pub severe_outputs: usize,
    pub evaluator_error_outputs: usize,
    pub average_successful_latency_ms: u128,
    pub successful_calls_over_5_seconds: usize,
    pub dictionary_task_credit: Option<f64>,
    pub contextual_task_credit: Option<f64>,
    pub cleanup_credit: Option<f64>,
    pub repair_misses: Vec<RepairMiss>,
}

#[derive(Debug, Serialize)]
pub struct RepairMiss {
    pub case: String,
    pub task_id: String,
    pub tier: Tier,
    pub heard: String,
    pub intended: String,
    pub evaluated_rounds: usize,
    pub occurrences_per_round: usize,
    pub mean_credit: f64,
    /// Repair loss before the hallucination gate, used only to prioritize the review.
    pub repair_loss_points: f64,
}

fn average(values: impl IntoIterator<Item = f64>) -> Option<f64> {
    let values: Vec<_> = values.into_iter().collect();
    (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
}

// Limit quoted evidence to one console line. Full source phrases stay in JSON.
fn quote(text: &str) -> String {
    let text: String = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches(|c: char| "。，；！？,.!?;".contains(c))
        .to_owned();
    let head: String = text.chars().take(24).collect();
    format!(
        "「{}{}」",
        head,
        if text.chars().count() > 24 { "…" } else { "" }
    )
}

/// A reference that moves a short trailing fragment into its sentence is a
/// post-dictated qualifier task. This recognizes the structure, not specific words.
fn trailing_fragment(heard: &str, intended: &str) -> Option<String> {
    if !heard.contains(['。', '？', '?', '！', '!']) {
        return None;
    }
    let content = |text: &str| {
        text.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect::<Vec<_>>()
    };
    let before = content(heard);
    let after = content(intended);
    let literal = heard
        .rsplit(['。', '？', '?', '！', '!'])
        .find(|part| !content(part).is_empty())?;
    for split in 1..before.len() {
        let fragment = &before[split..];
        if fragment.len() <= 8
            && fragment.len() < split
            && fragment
                .iter()
                .chain(&before[..split])
                .copied()
                .eq(after.iter().copied())
        {
            if content(literal) == fragment {
                return Some(literal.trim().to_owned());
            }
        }
    }
    None
}

fn repair_phrase(m: &RepairMiss) -> String {
    if m.tier == Tier::Dictionary {
        let action = if m.mean_credit <= 1e-9 {
            "字典替换漏改"
        } else {
            "字典替换不够稳定"
        };
        format!("{}{}", quote(&m.intended), action)
    } else if m.intended.trim().is_empty() {
        format!("{}等冗余内容仍有残留", quote(&m.heard))
    } else if let Some(fragment) = trailing_fragment(&m.heard, &m.intended) {
        format!("后补词{}处理不到位", quote(&fragment))
    } else {
        format!("{}→{}的修复不到位", quote(&m.heard), quote(&m.intended))
    }
}

fn render(e: &Evidence) -> String {
    if e.calls == 0 {
        return "本轮没有测试调用，无法评价纠错能力、清理效果或幻觉情况。".into();
    }
    if e.successful_calls == 0 {
        return format!(
            "本轮 {} 次调用全部失败，无法评价纠错能力、清理效果或幻觉情况。",
            e.calls
        );
    }
    let mut strengths = Vec::new();
    if e.dictionary_task_credit.is_some_and(|v| v >= 0.95) {
        strengths.push(if e.evaluator_error_outputs == 0 {
            "字典替换准确"
        } else {
            "已判分字典项替换准确"
        });
    } else if e.dictionary_task_credit.is_some_and(|v| v >= 0.8) {
        strengths.push("字典替换总体较好");
    }
    if e.contextual_task_credit.is_some_and(|v| v >= 0.85) {
        strengths.push("上下文纠错较好");
    }
    if e.cleanup_credit.is_some_and(|v| v >= 0.9) {
        strengths.push("文字清理较好");
    }
    let mut parts = Vec::new();
    if !strengths.is_empty() {
        parts.push(strengths.into_iter().take(2).collect::<Vec<_>>().join("、"));
    }
    let pace = match e.average_successful_latency_ms {
        0..=1000 => "响应快",
        1001..=2000 => "响应较快",
        2001..=3000 => "响应速度中等",
        _ => "响应偏慢",
    };
    parts.push(format!(
        "{}（成功调用平均 {:.2} 秒）",
        pace,
        e.average_successful_latency_ms as f64 / 1000.0
    ));
    if e.severe_outputs > 0 {
        parts.push(format!(
            "本轮 {} 条已判定输出中有 {} 条严重幻觉或原意变化",
            e.evaluated_fidelity_outputs, e.severe_outputs
        ));
    } else if e.evaluated_fidelity_outputs > 0 {
        parts.push(format!(
            "本轮 {} 条已判定输出未检出严重幻觉",
            e.evaluated_fidelity_outputs
        ));
    } else {
        parts.push("本轮没有完成有效的幻觉判定".into());
    }
    // Surface a dictionary weakness when present, then the most costly other repair.
    let dictionary = e.repair_misses.iter().find(|m| m.tier == Tier::Dictionary);
    let contextual = e.repair_misses.iter().find(|m| m.tier != Tier::Dictionary);
    let weaknesses: Vec<_> = [dictionary, contextual]
        .into_iter()
        .flatten()
        .map(repair_phrase)
        .collect();
    if !weaknesses.is_empty() {
        parts.push(format!("但{}", weaknesses.join("、")));
    } else if e.cleanup_credit.is_some_and(|v| v < 0.85) {
        parts.push("但文字清理效果仍有不足".into());
    }
    if e.successful_calls_over_5_seconds > 0 {
        parts.push(format!(
            "{} 次成功调用耗时超过 5 秒",
            e.successful_calls_over_5_seconds
        ));
    }
    if e.failed_calls > 0 {
        parts.push(format!("另有 {}/{} 次调用失败", e.failed_calls, e.calls));
    }
    if e.evaluator_error_outputs > 0 {
        parts.push(format!(
            "有 {} 条输出的部分评测失败",
            e.evaluator_error_outputs
        ));
    }
    format!("{}。", parts.join("，"))
}

pub fn for_model(model: &RankedProvider, cases: &[CaseRecord], weights: Weights) -> Assessment {
    let mut dictionary_cases = Vec::new();
    let mut contextual_cases = Vec::new();
    let mut cleanup_cases = Vec::new();
    let mut repair_misses = Vec::new();
    for c in cases {
        let Some(provider) = c.providers.iter().find(|p| p.name == model.name) else {
            continue;
        };
        let tasks = c.reference.correction_tasks();
        let mut dictionary = Vec::new();
        let mut contextual = Vec::new();
        for (index, task) in tasks.iter().enumerate() {
            // A missing judgment is an evaluator failure, not evidence of a model miss.
            let credits: Vec<_> = provider
                .rounds
                .iter()
                .filter(|r| r.result.error.is_none())
                .filter(|r| {
                    r.base_score.as_ref().is_some_and(|base| {
                        task.site_indices
                            .iter()
                            .all(|&i| base.sites[i].credit.is_some())
                    })
                })
                .map(|r| {
                    r.balanced
                        .as_ref()
                        .expect("assessment requires completed scoring")
                        .tasks[index]
                        .credit
                })
                .collect();
            let Some(mean_credit) = average(credits.iter().copied()) else {
                continue;
            };
            if task.tier == Tier::Dictionary {
                dictionary.push(mean_credit);
            } else {
                contextual.push(mean_credit);
            }
            // Ignore tiny probability-level deductions in prose.
            if mean_credit < 0.9 {
                repair_misses.push(RepairMiss {
                    case: c.case.name.clone(),
                    task_id: task.id.clone(),
                    tier: task.tier,
                    heard: task.heard.clone(),
                    intended: task.intended.clone(),
                    evaluated_rounds: credits.len(),
                    occurrences_per_round: task.site_indices.len(),
                    mean_credit,
                    repair_loss_points: 100.0
                        * weights.correction
                        * (1.0 - mean_credit)
                        * credits.len() as f64
                        / provider.rounds.len() as f64
                        / tasks.len() as f64
                        / cases.len() as f64,
                });
            }
        }
        if let Some(v) = average(dictionary) {
            dictionary_cases.push(v);
        }
        if let Some(v) = average(contextual) {
            contextual_cases.push(v);
        }
        let cleanup = provider
            .rounds
            .iter()
            .filter(|r| r.result.error.is_none())
            .filter_map(|r| {
                let score = r.balanced.as_ref()?;
                score
                    .evaluation_errors
                    .is_empty()
                    .then_some(score.base.cleanup)
            });
        if let Some(v) = average(cleanup) {
            cleanup_cases.push(v);
        }
    }
    repair_misses.sort_by(|a, b| {
        b.repair_loss_points
            .total_cmp(&a.repair_loss_points)
            .then_with(|| a.case.cmp(&b.case))
            .then_with(|| a.task_id.cmp(&b.task_id))
    });
    let model_rounds: Vec<_> = cases
        .iter()
        .flat_map(|c| &c.providers)
        .filter(|p| p.name == model.name)
        .flat_map(|p| &p.rounds)
        .collect();
    let calls = model_rounds.len();
    let successful_timings: Vec<_> = model_rounds
        .iter()
        .filter(|r| r.result.error.is_none())
        .map(|r| r.result.duration_ms)
        .collect();
    let successful_calls = successful_timings.len();
    let average_successful_latency_ms = if successful_calls > 0 {
        successful_timings.iter().sum::<u128>() / successful_calls as u128
    } else {
        0
    };
    let evidence = Evidence {
        basis: "current_run_fixed_repair_tasks_and_cleanup_before_fidelity_gate",
        cases: cases.len(),
        calls,
        successful_calls,
        failed_calls: calls - successful_calls,
        evaluated_fidelity_outputs: model.hallucination.clear + model.hallucination.severe,
        severe_outputs: model.hallucination.severe,
        evaluator_error_outputs: model.evaluation_failures,
        average_successful_latency_ms,
        successful_calls_over_5_seconds: successful_timings.iter().filter(|&&ms| ms > 5000).count(),
        dictionary_task_credit: average(dictionary_cases),
        contextual_task_credit: average(contextual_cases),
        cleanup_credit: average(cleanup_cases),
        repair_misses,
    };
    Assessment {
        sentence: render(&evidence),
        evidence,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn evidence() -> Evidence {
        Evidence {
            basis: "test",
            cases: 6,
            calls: 18,
            successful_calls: 18,
            failed_calls: 0,
            evaluated_fidelity_outputs: 18,
            severe_outputs: 0,
            evaluator_error_outputs: 0,
            average_successful_latency_ms: 823,
            successful_calls_over_5_seconds: 0,
            dictionary_task_credit: Some(1.0),
            contextual_task_credit: Some(0.6),
            cleanup_credit: Some(0.95),
            repair_misses: vec![],
        }
    }
    #[test]
    fn failures_and_partial_coverage_do_not_turn_into_zero_hallucination_claims() {
        let mut e = evidence();
        e.evaluated_fidelity_outputs = 0;
        e.evaluator_error_outputs = 18;
        let sentence = render(&e);
        assert!(!sentence.contains("未检出"));
        assert!(sentence.contains("没有完成有效"));
        e.successful_calls = 0;
        e.failed_calls = 18;
        let sentence = render(&e);
        assert!(sentence.contains("全部失败") && !sentence.contains("平均"));
        e.calls = 0;
        assert!(render(&e).contains("没有测试调用"));
    }
    #[test]
    fn severe_changes_are_not_softened_and_repairs_use_the_actual_task_name() {
        let mut e = evidence();
        e.severe_outputs = 1;
        e.repair_misses.push(RepairMiss {
            case: "custom case".into(),
            task_id: "custom".into(),
            tier: Tier::Dictionary,
            heard: "misheard".into(),
            intended: "NewTool".into(),
            evaluated_rounds: 3,
            occurrences_per_round: 1,
            mean_credit: 0.0,
            repair_loss_points: 2.5,
        });
        let sentence = render(&e);
        assert!(sentence.contains("1 条严重幻觉或原意变化"));
        assert!(sentence.contains("「NewTool」字典替换漏改"));
        assert!(!sentence.contains("未检出"));
    }

    #[test]
    fn post_dictated_fragments_are_described_by_structure() {
        assert_eq!(
            trailing_fragment("现在开始吗？明天。", "明天现在开始吗？"),
            Some("明天".into())
        );
        assert_eq!(trailing_fragment("有没有更新？", "已经有更新。"), None);
        assert_eq!(
            trailing_fragment("明天开始测试吗？GPT 6.1。", "GPT 6.1 明天开始测试吗？"),
            Some("GPT 6.1".into())
        );
    }
}
