//! A short CLI benchmark: basic whole-text checks first, speed within each quality tier.
use crate::{runlog, Provider, RoundResult, TestCase};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::Path;
use std::time::Instant;

#[derive(Debug, Default)]
pub struct Selection {
    pub full: bool,
    pub dry_run: bool,
    pub speed_only: bool,
    all_providers: bool,
    providers: Vec<String>,
    cases: Vec<String>,
}

impl Selection {
    pub fn parse(args: &[String]) -> Result<Self, String> {
        let mut selection = Self::default();
        let mut quick = false;
        let mut i = 1;
        while i < args.len() {
            let flag = args[i].as_str();
            let takes_value = matches!(
                flag,
                "--config"
                    | "--cases"
                    | "--rounds"
                    | "--concurrency"
                    | "--output"
                    | "--log-dir"
                    | "--standings"
                    | "--standings-results"
                    | "--provider"
                    | "--case"
                    | "--replay"
                    | "--rescore-results"
                    | "--finalize-results"
                    | "--probe-fidelity"
                    | "--probe-quality"
                    | "--fidelity-rubric"
            );
            let value = if takes_value {
                i += 1;
                Some(
                    args.get(i)
                        .filter(|s| !s.starts_with("--") && !s.is_empty())
                        .ok_or_else(|| format!("{flag} requires a value"))?,
                )
            } else {
                None
            };
            match flag {
                "--quick" => quick = true,
                "--full" | "--replay" | "--rescore-results" | "--finalize-results"
                | "--probe-fidelity" | "--probe-quality" => selection.full = true,
                "--provider" => selection.providers.push(value.unwrap().clone()),
                "--case" => selection.cases.push(value.unwrap().clone()),
                "--all-providers" => selection.all_providers = true,
                "--dry-run" => selection.dry_run = true,
                "--speed-only" => selection.speed_only = true,
                "--no-log" | "--no-standings" | "--skip-judge" | "--check-cases"
                | "--probe-review-all" => {}
                _ if takes_value => {}
                _ => return Err(format!("unknown option {flag:?}; see --help")),
            }
            i += 1;
        }
        if selection.speed_only && selection.full {
            return Err(
                "--speed-only belongs to quick mode; use --skip-judge for legacy full mode".into(),
            );
        }
        if quick && selection.full {
            return Err(
                "--quick cannot be combined with --full or a full evaluation action".into(),
            );
        }
        if selection.dry_run
            && args.iter().any(|a| {
                matches!(
                    a.as_str(),
                    "--replay"
                        | "--rescore-results"
                        | "--finalize-results"
                        | "--probe-fidelity"
                        | "--probe-quality"
                )
            })
        {
            return Err("--dry-run shows live correction plans; it cannot be combined with a replay or probe action".into());
        }
        if selection.all_providers && !selection.providers.is_empty() {
            return Err("choose --provider or --all-providers, not both".into());
        }
        if args
            .iter()
            .any(|a| a == "--probe-review-all" || a == "--fidelity-rubric")
            && !args.iter().any(|a| a == "--probe-fidelity")
        {
            return Err("--probe-review-all / --fidelity-rubric require --probe-fidelity".into());
        }
        Ok(selection)
    }

    pub fn default_cases(&self) -> &'static str {
        if self.full {
            "test_cases.toml"
        } else {
            "quick_cases.toml"
        }
    }

    pub fn select_providers(&self, providers: Vec<Provider>) -> Result<Vec<Provider>, String> {
        select_named(providers, &self.providers, |p| &p.name, "provider")
    }

    pub fn select_cases(&self, cases: Vec<TestCase>) -> Result<Vec<TestCase>, String> {
        select_named(cases, &self.cases, |c| &c.name, "case")
    }
}

fn select_named<T>(
    items: Vec<T>,
    names: &[String],
    name: fn(&T) -> &str,
    kind: &str,
) -> Result<Vec<T>, String> {
    let mut seen = HashSet::new();
    if items.is_empty()
        || items
            .iter()
            .any(|item| name(item).trim().is_empty() || !seen.insert(name(item)))
    {
        return Err(format!(
            "{kind} names must be nonempty and unique, with at least one configured"
        ));
    }
    let available = items.iter().map(name).collect::<Vec<_>>().join(", ");
    let mut selected = HashSet::new();
    for requested in names {
        if !seen.contains(requested.as_str()) {
            return Err(format!(
                "unknown {kind} {requested:?}; available: {available}"
            ));
        }
        if !selected.insert(requested) {
            return Err(format!("duplicate {kind} selection {requested:?}"));
        }
    }
    Ok(items
        .into_iter()
        .filter(|item| names.is_empty() || names.iter().any(|n| n == name(item)))
        .collect())
}

pub fn print_plan(providers: &[Provider], cases: &[TestCase], rounds: usize, full: bool) {
    println!(
        "{}: {} models × {} cases × {} rounds = {} correction calls",
        if full {
            "Full evaluation"
        } else {
            "Quick comparison"
        },
        providers.len(),
        cases.len(),
        rounds,
        providers.len() * cases.len() * rounds
    );
    println!(
        "{}",
        if full {
            "Judges and automatic scoring follow correction calls."
        } else {
            "CLI ranking: basic passes first, successful calls next, then median request time. Rolling standings follow the current run ranking."
        }
    );
    for p in providers {
        println!("  Model: {} ({})", p.name, p.model);
    }
    for c in cases {
        println!("  Case: {} · {} chars", c.name, c.input.chars().count());
        println!("    Input: {}", c.input);
        if !c.focus.is_empty() {
            println!("    Look for: {}", c.focus);
        }
    }
}

pub struct Run<'a> {
    pub args: &'a [String],
    pub config_path: &'a str,
    pub cases_path: &'a str,
    pub providers: Vec<Provider>,
    pub cases: &'a [TestCase],
    pub prompt: &'a str,
    pub dictionary: &'a str,
    pub prompt_source: &'a str,
    pub app_db: Option<String>,
    pub rounds: usize,
    pub concurrency: usize,
    pub judge: Option<Provider>,
    pub judge_concurrency: usize,
    pub speed_only: bool,
    pub log_dir: &'a str,
    pub output_path: Option<&'a str>,
    pub skip_log: bool,
    pub dry_run: bool,
    pub standings_path: &'a str,
    pub skip_standings: bool,
    pub standing_lens: crate::StandingLens,
}

pub async fn run(run: Run<'_>) -> Result<(), String> {
    print_plan(&run.providers, run.cases, run.rounds, false);
    if run.speed_only {
        println!("Speed-only ranking: successful calls first, then median time; quality unreviewed. Judge calls: 0.");
    } else {
        println!("Basic checks: at most {} requests (identical outputs share a verdict); reviewer calls: 0.", run.providers.len() * run.cases.len() * run.rounds);
    }
    if run.dry_run {
        return Ok(());
    }
    if run.judge_concurrency == 0 && !run.speed_only {
        return Err("judge concurrency must be positive".into());
    }
    let judge = if run.speed_only {
        None
    } else {
        let provider = run
            .judge
            .ok_or("basic ranking requires [judge]; configure it or explicitly use --speed-only")?;
        crate::quick_judge::validate(&provider)?;
        Some(crate::resolve_provider(provider))
    };
    // Resolve selected providers only; the legacy secondary reviewer is never loaded.
    let providers: Vec<_> = run
        .providers
        .into_iter()
        .map(crate::resolve_provider)
        .collect();
    let run_dir = if run.skip_log {
        None
    } else {
        Some(
            runlog::open(
                Path::new(run.log_dir),
                &runlog::header(run.args, run.config_path, run.cases_path),
            )
            .map_err(|e| format!("cannot create run directory: {e}"))?,
        )
    };
    let started = Instant::now();
    let outputs = crate::run_corrections(
        &reqwest::Client::new(),
        &providers,
        run.cases,
        run.prompt,
        run.dictionary,
        run.rounds,
        run.concurrency,
        None,
    )
    .await;
    let correction_wall_ms = started.elapsed().as_millis();
    let mut cases = records(&providers, run.cases, outputs);
    let judge_started = Instant::now();
    let judgment = match judge {
        Some(judge) => {
            crate::quick_judge::evaluate(
                &mut cases,
                judge,
                run.prompt,
                run.dictionary,
                run.judge_concurrency,
            )
            .await?
        }
        None => json!({"calls": 0}),
    };
    let judge_wall_ms = judge_started.elapsed().as_millis();
    let wall_ms = started.elapsed().as_millis();
    let summary = summaries(&providers, &cases);
    let ranking = rank(summary.clone(), run.speed_only);
    for case in run.cases {
        println!("\n[{}]", case.name);
        println!("  原文：{}", case.input);
        println!("  检查：{}", case.focus);
        for c in cases.iter().filter(|c| c["case"] == case.name) {
            println!("  {}", c["provider"].as_str().unwrap());
            for (i, r) in c["rounds"].as_array().unwrap().iter().enumerate() {
                let verdict = r["basic"]["verdict"].as_str().map(verdict_label).unwrap_or(
                    if r["error"].is_null() {
                        "未评估"
                    } else {
                        "调用失败"
                    },
                );
                println!(
                    "    Round {} · {} ms · {}{}",
                    i + 1,
                    r["duration_ms"],
                    verdict,
                    if r["basic"]["low_confidence"] == true {
                        "（低置信，建议核对）"
                    } else {
                        ""
                    }
                );
                println!("    {}", r["output"].as_str().unwrap());
                if let Some(error) = r["basic"]["error"].as_str() {
                    println!("    Judge error: {error}");
                }
                if let Some(error) = r["error"].as_str() {
                    println!("    Error: {error}");
                }
            }
        }
    }
    let at = chrono::Local::now().to_rfc3339();
    println!("本轮时间：{}", at);
    print_ranking(&ranking, run.speed_only);
    let mut payload = json!({
        "format": "quick-ranking-v2",
        "run": { "mode": "quick", "at": at, "command": run.args,
            "config": run.config_path, "cases": run.cases_path, "rounds": run.rounds,
            "prompt": run.prompt, "dictionary": run.dictionary, "prompt_source": run.prompt_source,
            "app_db": run.app_db, "git": runlog::git_state(), "quality_evaluation": if run.speed_only { "not_performed" } else { "basic_whole_text_v1" },
            "ranking_policy": if run.speed_only { "successes_desc_median_ms_asc" } else { "passes_desc_successes_desc_median_ms_asc_pending_unranked" },
            "providers": providers.iter().map(|p| json!({"name": p.name, "model": p.model,
                "type": p.provider_type, "api_mode": p.api_mode, "reasoning_effort": p.reasoning_effort})).collect::<Vec<_>>(),
            "execution": {"mode": "live", "correction_wall_ms": correction_wall_ms, "judge_wall_ms": judge_wall_ms, "wall_ms": wall_ms,
                "correction_concurrency": run.concurrency, "judge_concurrency": run.judge_concurrency, "judge_calls": judgment["calls"], "reviewer_calls": 0}},
        "cases": cases, "summary": summary, "ranking": ranking, "judge": judgment
    });
    let paths: Vec<std::path::PathBuf> = run_dir
        .iter()
        .map(|dir| dir.join(runlog::RESULTS_FILE))
        .chain(run.output_path.map(Into::into))
        .collect();
    let save = |payload: &Value| -> Result<(), String> {
        let json = serde_json::to_string_pretty(payload).map_err(|e| e.to_string())?;
        for path in &paths {
            std::fs::write(path, &json).map_err(|e| format!("{}: {e}", path.display()))?;
        }
        Ok(())
    };
    // Preserve measured outputs even if history import/writing fails.
    save(&payload)?;
    if !run.skip_standings {
        let report = crate::quick_standings::update(
            &payload,
            Path::new(run.log_dir),
            Path::new(run.standings_path),
            &run.standing_lens,
        )?;
        crate::quick_standings::print(&report, &ranking);
        payload["standings"] = report;
        save(&payload)?;
    }
    for path in &paths {
        println!("Results: {}", path.display());
    }
    println!(
        "Finished in {:.1}s · correction {:.1}s · basic checks {:.1}s",
        wall_ms as f64 / 1000.0,
        correction_wall_ms as f64 / 1000.0,
        judge_wall_ms as f64 / 1000.0
    );
    Ok(())
}

fn records(
    providers: &[Provider],
    cases: &[TestCase],
    outputs: Vec<Vec<Vec<RoundResult>>>,
) -> Vec<Value> {
    let mut rows = Vec::new();
    for (p, output) in providers.iter().zip(outputs) {
        for (c, rounds) in cases.iter().zip(output) {
            let rounds: Vec<_> = rounds
                .into_iter()
                .map(|r| {
                    let error = r.error.or_else(|| {
                        r.output
                            .trim()
                            .is_empty()
                            .then(|| "empty model output".into())
                    });
                    json!({"output": r.output, "duration_ms": r.duration_ms, "tokens": r.tokens,
                    "status": if error.is_some() { "failed" } else { "succeeded" }, "error": error})
                })
                .collect();
            rows.push(
                json!({"case": c.name, "focus": c.focus, "input": c.input, "expected": c.expected,
                "provider": p.name, "model": p.model, "rounds": rounds}),
            );
        }
    }
    rows
}

fn summaries(providers: &[Provider], cases: &[Value]) -> Vec<Value> {
    providers.iter().map(|p| {
        let rounds: Vec<_> = cases.iter().filter(|c| c["provider"] == p.name)
            .flat_map(|c| c["rounds"].as_array().unwrap()).collect();
        let mut times: Vec<_> = rounds.iter().filter(|r| r["error"].is_null())
            .map(|r| r["duration_ms"].as_u64().unwrap()).collect();
        times.sort_unstable();
        let median = if times.is_empty() { None } else {
            Some((times[(times.len() - 1) / 2] as f64 + times[times.len() / 2] as f64) / 2.0)
        };
        let count = |verdict: &str| rounds.iter().filter(|r| r["basic"]["verdict"] == verdict).count();
        json!({"provider": p.name, "passes": count("pass"), "needs_revision": count("needs_revision"),
            "uncertain": count("uncertain"), "judge_errors": count("error"),
            "low_confidence": rounds.iter().filter(|r| r["basic"]["low_confidence"] == true).count(),
            "pending": count("uncertain") + count("error"), "total_rounds": rounds.len(), "successes": times.len(),
            "failures": rounds.len() - times.len(), "median_ms": median, "min_ms": times.first(), "max_ms": times.last()})
    }).collect()
}

fn verdict_label(verdict: &str) -> &str {
    match verdict {
        "pass" => "通过",
        "needs_revision" => "需修改",
        "uncertain" => "待确认",
        "error" => "评估失败",
        _ => "未评估",
    }
}

fn rank(mut summary: Vec<Value>, speed_only: bool) -> Vec<Value> {
    let order = |a: &Value, b: &Value| {
        let pending = |s: &Value| !speed_only && s["pending"].as_u64().unwrap() > 0;
        pending(a)
            .cmp(&pending(b))
            .then_with(|| {
                if speed_only {
                    std::cmp::Ordering::Equal
                } else {
                    b["passes"].as_u64().cmp(&a["passes"].as_u64())
                }
            })
            .then_with(|| b["successes"].as_u64().cmp(&a["successes"].as_u64()))
            .then_with(|| {
                a["median_ms"]
                    .as_f64()
                    .unwrap_or(f64::INFINITY)
                    .total_cmp(&b["median_ms"].as_f64().unwrap_or(f64::INFINITY))
            })
    };
    summary.sort_by(order);
    let mut place = 1;
    for i in 0..summary.len() {
        if i > 0 && !order(&summary[i - 1], &summary[i]).is_eq() {
            place = i + 1;
        }
        summary[i]["place"] = if !speed_only && summary[i]["pending"].as_u64().unwrap() > 0 {
            Value::Null
        } else {
            json!(place)
        };
    }
    summary
}

pub(crate) fn print_ranking(ranking: &[Value], speed_only: bool) {
    println!("\n排名（本次小样本）");
    println!(
        "{}",
        if speed_only {
            "规则：调用成功数降序 → 成功请求耗时中位数升序；基本能力未评估。"
        } else {
            "规则：基本能力通过数降序 → 调用成功数降序 → 成功请求耗时中位数升序。待确认/评估失败的模型列为待定。"
        }
    );
    println!("  名次  基本能力  调用成功  中位(ms)  低置信  模型");
    for s in ranking {
        let place = s["place"]
            .as_u64()
            .map(|n| n.to_string())
            .unwrap_or_else(|| "—".into());
        let ms = s["median_ms"]
            .as_f64()
            .map(|v| format!("{v:.0}"))
            .unwrap_or_else(|| "—".into());
        let basic = if speed_only {
            "—".into()
        } else {
            format!("{}/{}", s["passes"], s["total_rounds"])
        };
        let success = format!("{}/{}", s["successes"], s["total_rounds"]);
        println!(
            "  {:>4}  {:>8}  {:>8}  {:>8}  {:>6}  {}{}",
            place,
            basic,
            success,
            ms,
            s["low_confidence"].to_string(),
            s["provider"].as_str().unwrap(),
            if s["place"].is_null() {
                "（待定）"
            } else {
                ""
            }
        );
        if s["needs_revision"].as_u64().unwrap()
            + s["pending"].as_u64().unwrap()
            + s["failures"].as_u64().unwrap()
            > 0
        {
            println!(
                "        需修改 {} · 待确认 {} · 评估失败 {} · 调用失败 {}",
                s["needs_revision"], s["uncertain"], s["judge_errors"], s["failures"]
            );
        }
    }
    if !speed_only {
        println!("低置信仅提示核对，不改变裁判的通过/需修改判定；原始输出和逐项判定见上方。");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Cases;
    fn args(s: &[&str]) -> Vec<String> {
        std::iter::once("bench")
            .chain(s.iter().copied())
            .map(str::to_owned)
            .collect()
    }
    fn provider(name: &str) -> Provider {
        serde_json::from_value(
            json!({"name":name,"base_url":"http://localhost/v1","api_key":"secret","model":"test"}),
        )
        .unwrap()
    }

    #[test]
    fn default_selects_all_models_and_full_evaluation_is_explicit() {
        let selection = Selection::parse(&args(&[])).unwrap();
        assert!(!selection.full);
        assert_eq!(selection.default_cases(), "quick_cases.toml");
        let providers = selection
            .select_providers(vec![provider("one"), provider("two")])
            .unwrap();
        assert_eq!(providers.len(), 2);
        assert_eq!(providers[0].name, "one");
        assert_eq!(providers[1].name, "two");
        for flags in [
            vec!["--full"],
            vec!["--replay", "saved.json"],
            vec!["--probe-quality", "probes.json"],
        ] {
            assert!(Selection::parse(&args(&flags)).unwrap().full);
        }
        for flags in [
            vec!["--quick", "--full"],
            vec!["--provider"],
            vec!["--rounds", "--full"],
            vec!["--provder", "one"],
            vec!["--provider", "one", "--all-providers"],
        ] {
            assert!(Selection::parse(&args(&flags)).is_err());
        }
    }

    #[test]
    fn selection_does_not_silently_expand_or_ignore_mistyped_names() {
        let selection = Selection::parse(&args(&["--provider", "two", "--case", "small"])).unwrap();
        assert_eq!(
            selection
                .select_providers(vec![provider("one"), provider("two")])
                .unwrap()[0]
                .name,
            "two"
        );
        assert!(selection.select_providers(vec![provider("one")]).is_err());
        assert!(selection
            .select_providers(vec![provider("two"), provider("two")])
            .is_err());
        let cases: Cases = toml::from_str("[[case]]\nname='small'\ninput='嗯，保留 GPT 6.1'\nexpected='保留 GPT 6.1'\n[[case]]\nname='long'\ninput='很长'\nexpected='很长'").unwrap();
        assert_eq!(selection.select_cases(cases.case).unwrap().len(), 1);
    }

    #[test]
    fn failed_and_empty_calls_remain_visible_without_zero_quality_scores() {
        let providers = vec![provider("one"), provider("two")];
        let cases: Cases =
            toml::from_str("[[case]]\nname='sample'\ninput='嗯，好'\nexpected='好'").unwrap();
        let round = |output: &str, error: Option<&str>, duration_ms| RoundResult {
            output: output.into(),
            error: error.map(str::to_owned),
            duration_ms,
            tokens: None,
        };
        let rows = records(
            &providers,
            &cases.case,
            vec![
                vec![vec![
                    round("嗯，好", None, 100),
                    round("", Some("timeout"), 10000),
                ]],
                vec![vec![round(" ", None, 200)]],
            ],
        );
        let summary = summaries(&providers, &rows);
        assert_eq!(summary[0]["total_rounds"], 2);
        assert_eq!(summary[0]["median_ms"], 100.0);
        assert_eq!(summary[1]["successes"], 0);
        assert!(summary[1]["median_ms"].is_null());
        assert_eq!(rows[0]["rounds"][0]["status"], "succeeded");
        assert!(rows[0]["rounds"][0].get("score").is_none());
        assert_eq!(rows[1]["rounds"][0]["error"], "empty model output");
        assert!(!serde_json::to_string(&rows).unwrap().contains("secret"));
    }

    #[test]
    fn quality_precedes_speed_and_unresolved_judgments_have_no_place() {
        let row = |name: &str, passes, successes, pending, ms| {
            json!({"provider": name,
            "passes": passes, "successes": successes, "pending": pending, "median_ms": ms})
        };
        let ranking = rank(
            vec![
                row("fast-but-wrong", 2, 3, 0, Some(10.0)),
                row("correct", 3, 3, 0, Some(500.0)),
                row("correct-faster", 3, 3, 0, Some(100.0)),
                row("unresolved", 2, 3, 1, Some(1.0)),
                row("failed", 0, 0, 0, None),
                row("same-time", 3, 3, 0, Some(100.0)),
            ],
            false,
        );
        let names: Vec<_> = ranking
            .iter()
            .map(|r| r["provider"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "correct-faster",
                "same-time",
                "correct",
                "fast-but-wrong",
                "failed",
                "unresolved"
            ]
        );
        assert_eq!(ranking[0]["place"], 1);
        assert_eq!(ranking[1]["place"], 1);
        assert_eq!(ranking[2]["place"], 3);
        assert!(ranking[5]["place"].is_null());
        let speed = rank(ranking, true);
        assert_eq!(speed[0]["provider"], "unresolved");
        assert_eq!(speed.last().unwrap()["provider"], "failed");
    }
}
