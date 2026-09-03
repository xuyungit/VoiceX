use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

// ── Config ──────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct Config {
    rounds: Option<usize>,
    prompt: Option<String>,
    dictionary: Option<String>,
    provider: Vec<Provider>,
    /// Optional second-pass evaluator. Runs after all provider tests finish.
    judge: Option<Provider>,
    #[serde(default)]
    eval: EvalConfig,
}

#[derive(Deserialize, Default, Clone)]
struct EvalConfig {
    /// Judge free-form rewrite quality. Default 0.20.
    weight_quality: Option<f64>,
    /// Dictionary / basic ASR fixes (names, simple typos). Default 0.20.
    weight_basic: Option<f64>,
    /// Distinctive "cloud → Claude" style corrections. Default 0.20.
    weight_cloud: Option<f64>,
    /// Icing: README, 下一集→下一级, etc. Default 0.10.
    weight_bonus: Option<f64>,
    /// Voice-correction latency. Default 0.30.
    weight_latency: Option<f64>,
    /// Optional; default 0 (API failures already zero latency + checkpoints).
    weight_success: Option<f64>,
    /// Legacy: if set and the split checkpoint weights are unset, split 40/40/20
    /// across basic / cloud / bonus.
    weight_checkpoints: Option<f64>,
    /// Avg latency at or below this (ms) scores 1.0. Default 1000.
    latency_full_ms: Option<f64>,
    /// Avg latency at or above this (ms) scores 0.0. Default 5000.
    latency_zero_ms: Option<f64>,
    /// "borda" (default): N..1 by place. "f1": 25/18/15/... for the top 10.
    standing_scheme: Option<String>,
    /// Per-race decay in (0, 1]. 1.0 keeps all history equally. Default 0.9.
    standing_decay: Option<f64>,
    /// Only the last N races feed form. 0 = all history. Default 10.
    standing_window: Option<usize>,
    /// Consecutive absences before a model is retired from the active table. 0 = never. Default 3.
    standing_retire_after: Option<usize>,
}

#[derive(Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct Provider {
    name: String,
    base_url: String,
    api_key: String,
    model: String,
    #[serde(rename = "type", default = "default_type")]
    provider_type: String,
    /// API mode: "completion" (default, /chat/completions) or "response" (/responses)
    #[serde(default = "default_api_mode")]
    api_mode: String,
    reasoning_effort: Option<String>,
    /// Extra fields merged into the request body (e.g. enable_thinking = false)
    #[serde(default)]
    extra: std::collections::HashMap<String, toml::Value>,
}

fn default_type() -> String {
    "custom".into()
}

fn default_api_mode() -> String {
    "completion".into()
}

#[derive(Deserialize)]
struct Cases {
    case: Vec<TestCase>,
}

#[derive(Deserialize, Clone)]
struct TestCase {
    name: String,
    input: String,
    expected: Option<String>,
    #[serde(default)]
    checkpoint: Vec<Checkpoint>,
}

#[derive(Deserialize, Clone)]
struct Checkpoint {
    description: String,
    must_contain: Option<String>,
    must_not_contain: Option<String>,
    /// Optional regular expression; must match for the checkpoint to pass.
    pattern: Option<String>,
    #[serde(default)]
    case_sensitive: bool,
    /// Scoring bucket: "basic" (default), "cloud", or "bonus" / "icing".
    #[serde(default)]
    tier: String,
}

// ── OpenAI-compatible API types ─────────────────────────────────────────────

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_completion_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<String>,
}

#[derive(Serialize)]
struct Message {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Option<Vec<Choice>>,
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct Choice {
    message: ResponseMessage,
}

#[derive(Deserialize)]
struct ResponseMessage {
    content: Option<String>,
}

#[derive(Deserialize)]
struct Usage {
    total_tokens: Option<u32>,
}

// ── OpenAI Responses API types ─────────────────────────────────────────────

#[derive(Serialize)]
struct ResponseApiRequest {
    model: String,
    input: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    instructions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<u32>,
}

#[derive(Deserialize)]
struct ResponseApiResponse {
    output: Option<Vec<ResponseApiOutputItem>>,
    usage: Option<ResponseApiUsage>,
}

#[derive(Deserialize)]
struct ResponseApiOutputItem {
    #[serde(rename = "type")]
    item_type: String,
    content: Option<Vec<ResponseApiContent>>,
}

#[derive(Deserialize)]
struct ResponseApiContent {
    #[serde(rename = "type")]
    content_type: Option<String>,
    text: Option<String>,
}

#[derive(Deserialize)]
struct ResponseApiUsage {
    total_tokens: Option<u32>,
}

// ── Gemini API types ────────────────────────────────────────────────────────

#[derive(Serialize)]
struct GeminiRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    system_instruction: Option<GeminiContent>,
    contents: Vec<GeminiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    generation_config: Option<GeminiGenerationConfig>,
}

#[derive(Serialize, Deserialize)]
struct GeminiContent {
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    parts: Vec<GeminiPart>,
}

#[derive(Serialize, Deserialize)]
struct GeminiPart {
    text: String,
}

#[derive(Serialize)]
struct GeminiGenerationConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<u32>,
}

#[derive(Deserialize)]
struct GeminiResponse {
    candidates: Option<Vec<GeminiCandidate>>,
    #[serde(rename = "usageMetadata")]
    usage_metadata: Option<GeminiUsageMetadata>,
}

#[derive(Deserialize)]
struct GeminiCandidate {
    content: Option<GeminiContent>,
}

#[derive(Deserialize)]
struct GeminiUsageMetadata {
    #[serde(rename = "totalTokenCount")]
    total_token_count: Option<u32>,
}

// ── Results ─────────────────────────────────────────────────────────────────

struct RoundResult {
    duration_ms: u128,
    output: String,
    tokens: Option<u32>,
    error: Option<String>,
}

struct ProviderStats {
    name: String,
    total_rounds: usize,
    successes: usize,
    avg_ms: u128,
    min_ms: u128,
    max_ms: u128,
    avg_tokens: Option<u32>,
    checkpoints: CheckpointTally,
}

#[derive(Clone, Copy, Default)]
struct CheckpointTally {
    basic_hits: usize,
    basic_total: usize,
    cloud_hits: usize,
    cloud_total: usize,
    bonus_hits: usize,
    bonus_total: usize,
}

impl CheckpointTally {
    fn hits(self) -> usize {
        self.basic_hits + self.cloud_hits + self.bonus_hits
    }

    fn total(self) -> usize {
        self.basic_total + self.cloud_total + self.bonus_total
    }

    fn merge(&mut self, other: Self) {
        self.basic_hits += other.basic_hits;
        self.basic_total += other.basic_total;
        self.cloud_hits += other.cloud_hits;
        self.cloud_total += other.cloud_total;
        self.bonus_hits += other.bonus_hits;
        self.bonus_total += other.bonus_total;
    }

    fn record(&mut self, tier: CheckpointTier, passed: bool) {
        let (hits, total) = match tier {
            CheckpointTier::Basic => (&mut self.basic_hits, &mut self.basic_total),
            CheckpointTier::Cloud => (&mut self.cloud_hits, &mut self.cloud_total),
            CheckpointTier::Bonus => (&mut self.bonus_hits, &mut self.bonus_total),
        };
        *total += 1;
        if passed {
            *hits += 1;
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CheckpointTier {
    Basic,
    Cloud,
    Bonus,
}

impl CheckpointTier {
    fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "cloud" => Self::Cloud,
            "bonus" | "icing" => Self::Bonus,
            _ => Self::Basic,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Basic => "basic",
            Self::Cloud => "cloud",
            Self::Bonus => "bonus",
        }
    }
}

fn hit_rate(hits: usize, total: usize) -> Option<f64> {
    if total == 0 {
        None
    } else {
        Some(hits as f64 / total as f64)
    }
}

struct CaseRecord {
    case: TestCase,
    providers: Vec<ProviderRecord>,
}

struct ProviderRecord {
    name: String,
    model: String,
    stats: ProviderStats,
    rounds: Vec<RoundResult>,
}

struct RankedProvider {
    name: String,
    composite: f64,
    quality: Option<f64>,
    checkpoint_rate: Option<f64>,
    basic_rate: Option<f64>,
    cloud_rate: Option<f64>,
    bonus_rate: Option<f64>,
    latency_score: f64,
    avg_ms: u128,
    success_rate: f64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StandingScheme {
    Borda,
    F1,
}

#[derive(Serialize, Deserialize, Clone, Default)]
struct StandingLens {
    scheme: String,
    decay: f64,
    window: usize,
    retire_after: usize,
}

#[derive(Serialize, Deserialize, Clone)]
struct StandingsFile {
    version: u32,
    scheme: String,
    #[serde(default)]
    lens: StandingLens,
    runs: Vec<StandingRun>,
    standings: Vec<StandingEntry>,
}

#[derive(Serialize, Deserialize, Clone)]
struct StandingRun {
    at: String,
    scheme: String,
    ranking: Vec<RunPlace>,
}

#[derive(Serialize, Deserialize, Clone)]
struct RunPlace {
    place: usize,
    provider: String,
    points: f64,
    composite: f64,
    quality: Option<f64>,
    checkpoint_rate: Option<f64>,
    #[serde(default)]
    basic_rate: Option<f64>,
    #[serde(default)]
    cloud_rate: Option<f64>,
    #[serde(default)]
    bonus_rate: Option<f64>,
    #[serde(default)]
    latency_score: Option<f64>,
    avg_ms: u64,
    success_rate: f64,
}

#[derive(Serialize, Deserialize, Clone)]
struct StandingEntry {
    provider: String,
    /// Recency-weighted average of races this model actually entered. Ranking key.
    #[serde(default)]
    form: f64,
    #[serde(default)]
    decayed_points: f64,
    total_points: f64,
    avg_points: f64,
    runs: usize,
    #[serde(default)]
    window_runs: usize,
    #[serde(default)]
    absences: usize,
    #[serde(default)]
    active: bool,
    best_place: usize,
    last_place: usize,
    last_points: f64,
}

#[derive(Deserialize)]
struct JudgeReport {
    providers: Vec<JudgeProviderScore>,
    ranking: Option<Vec<String>>,
    summary: Option<String>,
}

#[derive(Deserialize)]
struct JudgeProviderScore {
    name: String,
    quality_score: f64,
    strengths: Option<String>,
    weaknesses: Option<String>,
}

// ── Main ────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_usage();
        return;
    }

    let config_path = get_arg(&args, "--config").unwrap_or("config.toml".into());
    let cases_path = get_arg(&args, "--cases").unwrap_or("test_cases.toml".into());
    let rounds_override: Option<usize> = get_arg(&args, "--rounds").and_then(|s| s.parse().ok());
    let output_path = get_arg(&args, "--output");
    let skip_judge = args.iter().any(|a| a == "--skip-judge");
    let skip_standings = args.iter().any(|a| a == "--no-standings");
    let standings_path =
        get_arg(&args, "--standings").unwrap_or("standings.json".into());

    let config = load_config(&config_path);
    let cases = load_cases(&cases_path);
    let eval_cfg = config.eval.clone();

    let rounds = rounds_override.or(config.rounds).unwrap_or(3);
    let prompt = config.prompt.unwrap_or_else(|| {
        eprintln!("Warning: no root `prompt` in config; using the built-in default");
        default_prompt()
    });
    let dictionary = config.dictionary.unwrap_or_default();
    if dictionary.trim().is_empty() {
        eprintln!(
            "Warning: root `dictionary` is empty; hotword replacements such as names will not be applied"
        );
        eprintln!(
            "Hint: keep `prompt` and `dictionary` above [judge] / [[provider]] so they stay root keys"
        );
    }

    let providers: Vec<Provider> = config
        .provider
        .into_iter()
        .map(resolve_provider)
        .collect();

    let judge = config.judge.map(resolve_provider);

    println!(
        "\n\x1b[1m══════════════════════════════════════════════════════════\x1b[0m"
    );
    println!(
        "\x1b[1m  LLM Benchmark\x1b[0m   Providers: {}  Cases: {}  Rounds: {}",
        providers.len(),
        cases.case.len(),
        rounds
    );
    println!(
        "\x1b[1m══════════════════════════════════════════════════════════\x1b[0m\n"
    );

    for p in &providers {
        println!(
            "  \x1b[2m{}: {} ({})\x1b[0m",
            p.name, p.model, p.api_mode
        );
    }
    if let Some(ref j) = judge {
        if skip_judge {
            println!("  \x1b[2mJudge: {} (skipped)\x1b[0m", j.name);
        } else {
            println!("  \x1b[2mJudge: {} ({})\x1b[0m", j.name, j.model);
        }
    }
    println!();

    let http = Client::new();
    let mut bench_cases: Vec<CaseRecord> = Vec::new();
    let mut json_results: Vec<serde_json::Value> = Vec::new();

    for case in &cases.case {
        println!("\x1b[1;36m━━━ {} ━━━\x1b[0m", case.name);
        println!("\x1b[2mInput:\x1b[0m    {}", case.input);
        if let Some(ref exp) = case.expected {
            println!("\x1b[2mExpected:\x1b[0m {}", exp);
        }
        if !case.checkpoint.is_empty() {
            println!("\x1b[2mCheckpoints:\x1b[0m");
            for cp in &case.checkpoint {
                println!("  \x1b[2m• {}\x1b[0m", cp.description);
            }
        }
        println!();

        let mut case_providers: Vec<ProviderRecord> = Vec::new();

        for provider in &providers {
            let mut results: Vec<RoundResult> = Vec::new();

            for _ in 0..rounds {
                let result =
                    run_once(&http, provider, &prompt, &dictionary, &case.input).await;
                results.push(result);
            }

            // Display
            let first = &results[0];
            if let Some(ref err) = first.error {
                println!(
                    "  \x1b[1m{}\x1b[0m \x1b[31m✗ ERROR:\x1b[0m {}",
                    provider.name,
                    truncate(err, 80)
                );
            } else {
                println!(
                    "  \x1b[1m{}\x1b[0m → {}",
                    provider.name, first.output
                );
            }

            let ok: Vec<&RoundResult> = results.iter().filter(|r| r.error.is_none()).collect();
            let checkpoints = score_checkpoints(&ok, &case.checkpoint);
            let stats = if ok.is_empty() {
                println!("  \x1b[2m{} rounds: all failed\x1b[0m\n", rounds);
                ProviderStats {
                    name: provider.name.clone(),
                    total_rounds: rounds,
                    successes: 0,
                    avg_ms: 0,
                    min_ms: 0,
                    max_ms: 0,
                    avg_tokens: None,
                    checkpoints,
                }
            } else {
                let times: Vec<u128> = ok.iter().map(|r| r.duration_ms).collect();
                let avg_ms = times.iter().sum::<u128>() / times.len() as u128;
                let min_ms = *times.iter().min().unwrap();
                let max_ms = *times.iter().max().unwrap();
                let token_values: Vec<u32> = ok.iter().filter_map(|r| r.tokens).collect();
                let avg_tokens = if token_values.is_empty() {
                    None
                } else {
                    Some(token_values.iter().sum::<u32>() / token_values.len() as u32)
                };

                print!(
                    "  \x1b[2mLatency: avg={}ms min={}ms max={}ms\x1b[0m",
                    avg_ms, min_ms, max_ms
                );
                if let Some(t) = avg_tokens {
                    print!("  \x1b[2mTokens: {}\x1b[0m", t);
                }
                if checkpoints.total() > 0 {
                    print!(
                        "  \x1b[2mCheckpoints: {}\x1b[0m",
                        format_tally(checkpoints)
                    );
                }
                if ok.len() < results.len() {
                    print!(
                        "  \x1b[33m({}/{} succeeded)\x1b[0m",
                        ok.len(),
                        results.len()
                    );
                }
                println!("\n");

                ProviderStats {
                    name: provider.name.clone(),
                    total_rounds: rounds,
                    successes: ok.len(),
                    avg_ms,
                    min_ms,
                    max_ms,
                    avg_tokens,
                    checkpoints,
                }
            };

            // JSON output
            let round_details: Vec<serde_json::Value> = results
                .iter()
                .map(|r| {
                    serde_json::json!({
                        "duration_ms": r.duration_ms,
                        "output": r.output,
                        "tokens": r.tokens,
                        "error": r.error,
                    })
                })
                .collect();

            json_results.push(serde_json::json!({
                "case": case.name,
                "provider": provider.name,
                "model": provider.model,
                "avg_ms": stats.avg_ms,
                "min_ms": stats.min_ms,
                "max_ms": stats.max_ms,
                "successes": stats.successes,
                "total_rounds": stats.total_rounds,
                "avg_tokens": stats.avg_tokens,
                "checkpoint_hits": stats.checkpoints.hits(),
                "checkpoint_total": stats.checkpoints.total(),
                "basic_hits": stats.checkpoints.basic_hits,
                "basic_total": stats.checkpoints.basic_total,
                "cloud_hits": stats.checkpoints.cloud_hits,
                "cloud_total": stats.checkpoints.cloud_total,
                "bonus_hits": stats.checkpoints.bonus_hits,
                "bonus_total": stats.checkpoints.bonus_total,
                "rounds": round_details,
            }));

            case_providers.push(ProviderRecord {
                name: provider.name.clone(),
                model: provider.model.clone(),
                stats,
                rounds: results,
            });
        }

        bench_cases.push(CaseRecord {
            case: case.clone(),
            providers: case_providers,
        });
    }

    // ── Summary table ───────────────────────────────────────────────────────
    println!(
        "\x1b[1m══════════════════════════════════════════════════════════\x1b[0m"
    );
    println!("\x1b[1m  Summary (averaged across all test cases)\x1b[0m");
    println!(
        "\x1b[1m══════════════════════════════════════════════════════════\x1b[0m\n"
    );

    // Aggregate per provider
    let provider_names: Vec<String> = providers.iter().map(|p| p.name.clone()).collect();
    let name_width = provider_names.iter().map(|n| n.len()).max().unwrap_or(8).max(8);

    println!(
        "  {:<width$}  {:>8}  {:>8}  {:>8}  {:>8}  {:>10}  {:>11}  {:>11}  {:>11}",
        "Provider",
        "Avg ms",
        "Min ms",
        "Max ms",
        "Tokens",
        "Success",
        "Basic",
        "Cloud",
        "Bonus",
        width = name_width
    );
    println!(
        "  {:<width$}  {:>8}  {:>8}  {:>8}  {:>8}  {:>10}  {:>11}  {:>11}  {:>11}",
        "─".repeat(name_width),
        "────────",
        "────────",
        "────────",
        "────────",
        "──────────",
        "───────────",
        "───────────",
        "───────────",
        width = name_width
    );

    let mut aggregated: Vec<ProviderStats> = Vec::new();

    for pname in &provider_names {
        let mut total_avg = 0u128;
        let mut total_min = u128::MAX;
        let mut total_max = 0u128;
        let mut total_tokens = 0u32;
        let mut token_count = 0u32;
        let mut total_success = 0usize;
        let mut total_rounds = 0usize;
        let mut case_count = 0u128;
        let mut checkpoints = CheckpointTally::default();

        for record in &bench_cases {
            if let Some(p) = record.providers.iter().find(|s| &s.name == pname) {
                let s = &p.stats;
                if s.successes > 0 {
                    total_avg += s.avg_ms;
                    if s.min_ms < total_min {
                        total_min = s.min_ms;
                    }
                    if s.max_ms > total_max {
                        total_max = s.max_ms;
                    }
                    case_count += 1;
                }
                if let Some(t) = s.avg_tokens {
                    total_tokens += t;
                    token_count += 1;
                }
                total_success += s.successes;
                total_rounds += s.total_rounds;
                checkpoints.merge(s.checkpoints);
            }
        }

        let avg = if case_count > 0 {
            total_avg / case_count
        } else {
            0
        };
        let min = if total_min == u128::MAX {
            0
        } else {
            total_min
        };
        let tokens_str = if token_count > 0 {
            format!("{}", total_tokens / token_count)
        } else {
            "-".into()
        };

        println!(
            "  {:<width$}  {:>8}  {:>8}  {:>8}  {:>8}  {:>5}/{:<4}  {:>11}  {:>11}  {:>11}",
            pname,
            avg,
            min,
            total_max,
            tokens_str,
            total_success,
            total_rounds,
            format_ratio(checkpoints.basic_hits, checkpoints.basic_total),
            format_ratio(checkpoints.cloud_hits, checkpoints.cloud_total),
            format_ratio(checkpoints.bonus_hits, checkpoints.bonus_total),
            width = name_width
        );

        aggregated.push(ProviderStats {
            name: pname.clone(),
            total_rounds,
            successes: total_success,
            avg_ms: avg,
            min_ms: min,
            max_ms: total_max,
            avg_tokens: if token_count > 0 {
                Some(total_tokens / token_count)
            } else {
                None
            },
            checkpoints,
        });
    }
    println!();

    // ── Judge + ranking ─────────────────────────────────────────────────────
    let mut judge_report: Option<JudgeReport> = None;
    let mut judge_raw: Option<String> = None;

    if let Some(ref judge_provider) = judge {
        if skip_judge {
            println!("Judge skipped (--skip-judge).\n");
        } else {
            println!(
                "\x1b[1m══════════════════════════════════════════════════════════\x1b[0m"
            );
            println!(
                "\x1b[1m  Judge evaluation\x1b[0m  ({})",
                judge_provider.model
            );
            println!(
                "\x1b[1m══════════════════════════════════════════════════════════\x1b[0m\n"
            );

            let payload = build_judge_payload(&bench_cases, &aggregated);
            let result = run_prompt(
                &http,
                judge_provider,
                default_judge_prompt(),
                payload,
            )
            .await;

            if let Some(ref err) = result.error {
                println!("  \x1b[31mJudge failed:\x1b[0m {}\n", err);
            } else {
                judge_raw = Some(result.output.clone());
                match parse_judge_report(&result.output) {
                    Ok(report) => {
                        let q_width = name_width;
                        println!(
                            "  {:<width$}  {:>7}  {}",
                            "Provider",
                            "Quality",
                            "Notes",
                            width = q_width
                        );
                        println!(
                            "  {:<width$}  {:>7}  {}",
                            "─".repeat(q_width),
                            "───────",
                            "─────",
                            width = q_width
                        );
                        for score in &report.providers {
                            let note = score
                                .weaknesses
                                .as_ref()
                                .or(score.strengths.as_ref())
                                .map(|s| truncate(s, 60))
                                .unwrap_or_default();
                            println!(
                                "  {:<width$}  {:>7.1}  {}",
                                score.name,
                                score.quality_score,
                                note,
                                width = q_width
                            );
                        }
                        println!();
                        if let Some(ref ranking) = report.ranking {
                            println!(
                                "  \x1b[2mJudge ranking:\x1b[0m {}",
                                ranking.join(" > ")
                            );
                        }
                        if let Some(ref summary) = report.summary {
                            println!("  {}\n", summary);
                        } else {
                            println!();
                        }
                        judge_report = Some(report);
                    }
                    Err(e) => {
                        println!(
                            "  \x1b[33mCould not parse judge JSON ({}):\x1b[0m\n{}\n",
                            e,
                            truncate(&result.output, 800)
                        );
                    }
                }
            }
        }
    }

    let quality_map: HashMap<String, f64> = judge_report
        .as_ref()
        .map(|r| {
            r.providers
                .iter()
                .map(|p| (p.name.clone(), p.quality_score))
                .collect()
        })
        .unwrap_or_default();

    let ranked = rank_providers(&aggregated, &quality_map, &eval_cfg);

    println!(
        "\x1b[1m══════════════════════════════════════════════════════════\x1b[0m"
    );
    println!("\x1b[1m  Ranking (weighted dimensions)\x1b[0m");
    println!(
        "\x1b[1m══════════════════════════════════════════════════════════\x1b[0m\n"
    );
    print_ranking_legend(&eval_cfg, &aggregated, !quality_map.is_empty());

    println!(
        "  {:>3}  {:<width$}  {:>9}  {:>6}  {:>6}  {:>6}  {:>6}  {:>7}  {:>8}",
        "#",
        "Provider",
        "Composite",
        "Basic",
        "Cloud",
        "Bonus",
        "Speed",
        "Quality",
        "Avg ms",
        width = name_width
    );
    println!(
        "  {:>3}  {:<width$}  {:>9}  {:>6}  {:>6}  {:>6}  {:>6}  {:>7}  {:>8}",
        "─".repeat(3),
        "─".repeat(name_width),
        "─────────",
        "──────",
        "──────",
        "──────",
        "──────",
        "───────",
        "────────",
        width = name_width
    );

    for (i, r) in ranked.iter().enumerate() {
        let quality = r
            .quality
            .map(|q| format!("{:.1}", q))
            .unwrap_or_else(|| "-".into());
        println!(
            "  {:>3}  {:<width$}  {:>9.1}  {:>6}  {:>6}  {:>6}  {:>5.0}%  {:>7}  {:>8}",
            i + 1,
            r.name,
            r.composite,
            format_pct(r.basic_rate),
            format_pct(r.cloud_rate),
            format_pct(r.bonus_rate),
            r.latency_score * 100.0,
            quality,
            r.avg_ms,
            width = name_width
        );
    }
    println!();

    if !skip_standings && !ranked.is_empty() {
        let lens = StandingLens::from_eval(&eval_cfg);
        match update_standings(&standings_path, lens, &ranked) {
            Ok(file) => print_standings(&file, name_width),
            Err(e) => eprintln!("Failed to update {}: {}", standings_path, e),
        }
    }

    // Write JSON output if requested
    if let Some(path) = output_path {
        let ranking_json: Vec<serde_json::Value> = ranked
            .iter()
            .enumerate()
            .map(|(i, r)| {
                serde_json::json!({
                    "rank": i + 1,
                    "provider": r.name,
                    "composite": r.composite,
                    "quality": r.quality,
                    "checkpoint_rate": r.checkpoint_rate,
                    "basic_rate": r.basic_rate,
                    "cloud_rate": r.cloud_rate,
                    "bonus_rate": r.bonus_rate,
                    "latency_score": r.latency_score,
                    "avg_ms": r.avg_ms,
                    "success_rate": r.success_rate,
                })
            })
            .collect();
        let payload = serde_json::json!({
            "cases": json_results,
            "summary": aggregated.iter().map(|s| serde_json::json!({
                "provider": s.name,
                "avg_ms": s.avg_ms,
                "min_ms": s.min_ms,
                "max_ms": s.max_ms,
                "avg_tokens": s.avg_tokens,
                "successes": s.successes,
                "total_rounds": s.total_rounds,
                "checkpoint_hits": s.checkpoints.hits(),
                "checkpoint_total": s.checkpoints.total(),
                "basic_hits": s.checkpoints.basic_hits,
                "basic_total": s.checkpoints.basic_total,
                "cloud_hits": s.checkpoints.cloud_hits,
                "cloud_total": s.checkpoints.cloud_total,
                "bonus_hits": s.checkpoints.bonus_hits,
                "bonus_total": s.checkpoints.bonus_total,
            })).collect::<Vec<_>>(),
            "ranking": ranking_json,
            "judge": {
                "raw": judge_raw,
                "report": judge_report.as_ref().map(|r| serde_json::json!({
                    "summary": r.summary,
                    "ranking": r.ranking,
                    "providers": r.providers.iter().map(|p| serde_json::json!({
                        "name": p.name,
                        "quality_score": p.quality_score,
                        "strengths": p.strengths,
                        "weaknesses": p.weaknesses,
                    })).collect::<Vec<_>>(),
                })),
            },
        });
        let json = serde_json::to_string_pretty(&payload).unwrap();
        std::fs::write(&path, &json).unwrap_or_else(|e| {
            eprintln!("Failed to write {}: {}", path, e);
        });
        println!("Results written to {}", path);
    }
}

// ── HTTP call ───────────────────────────────────────────────────────────────

async fn run_once(
    http: &Client,
    provider: &Provider,
    prompt: &str,
    dictionary: &str,
    input: &str,
) -> RoundResult {
    let system_prompt = if prompt.contains("{{DICTIONARY}}") {
        prompt.replace("{{DICTIONARY}}", dictionary.trim())
    } else {
        format!("{}\n\n用户热词词典：\n{}", prompt, dictionary.trim())
    };
    let user_content = format!("原文：\n{}", input);
    run_prompt(http, provider, system_prompt, user_content).await
}

async fn run_prompt(
    http: &Client,
    provider: &Provider,
    system_prompt: String,
    user_content: String,
) -> RoundResult {
    if provider.provider_type == "gemini" {
        return run_once_gemini(http, provider, &system_prompt, &user_content).await;
    }

    if provider.api_mode == "response" {
        return run_once_response(http, provider, &system_prompt, &user_content).await;
    }

    let mut request = ChatRequest {
        model: provider.model.clone(),
        messages: vec![
            Message {
                role: "system".into(),
                content: system_prompt,
            },
            Message {
                role: "user".into(),
                content: user_content,
            },
        ],
        temperature: None,
        max_tokens: None,
        max_completion_tokens: None,
        reasoning_effort: None,
    };

    match provider.provider_type.as_str() {
        "volcengine" => {
            request.temperature = Some(0.2);
            request.reasoning_effort = provider
                .reasoning_effort
                .clone()
                .or_else(|| Some("minimal".into()));
        }
        "openai" => {
            request.max_completion_tokens = Some(4096);
            request.reasoning_effort = provider.reasoning_effort.clone();
        }
        _ => {
            request.temperature = Some(0.2);
            request.max_tokens = Some(4096);
            request.reasoning_effort = provider.reasoning_effort.clone();
        }
    }

    // Serialize request then merge extra fields
    let mut body = serde_json::to_value(&request).unwrap();
    if !provider.extra.is_empty() {
        if let serde_json::Value::Object(ref mut map) = body {
            for (k, v) in &provider.extra {
                let json_val = toml_to_json(v);
                map.insert(k.clone(), json_val);
            }
        }
    }

    let url = format!(
        "{}/chat/completions",
        provider.base_url.trim_end_matches('/')
    );

    let start = Instant::now();
    let resp = http
        .post(&url)
        .header("Content-Type", "application/json")
        .bearer_auth(&provider.api_key)
        .json(&body)
        .send()
        .await;
    let duration_ms = start.elapsed().as_millis();

    let response = match resp {
        Err(e) => {
            return RoundResult {
                duration_ms,
                output: String::new(),
                tokens: None,
                error: Some(format!("HTTP error: {}", e)),
            }
        }
        Ok(r) => r,
    };

    let status = response.status();
    let body = response.text().await.unwrap_or_default();

    if !status.is_success() {
        return RoundResult {
            duration_ms,
            output: String::new(),
            tokens: None,
            error: Some(format!("HTTP {}: {}", status, truncate(&body, 200))),
        };
    }

    match serde_json::from_str::<ChatResponse>(&body) {
        Err(e) => RoundResult {
            duration_ms,
            output: String::new(),
            tokens: None,
            error: Some(format!("Parse error: {} body={}", e, truncate(&body, 200))),
        },
        Ok(parsed) => {
            let content = parsed
                .choices
                .and_then(|c| c.into_iter().next())
                .and_then(|c| c.message.content)
                .unwrap_or_default()
                .trim()
                .to_string();
            let tokens = parsed.usage.and_then(|u| u.total_tokens);
            RoundResult {
                duration_ms,
                output: content,
                tokens,
                error: None,
            }
        }
    }
}

// ── Responses API call ──────────────────────────────────────────────────────

async fn run_once_response(
    http: &Client,
    provider: &Provider,
    system_prompt: &str,
    user_content: &str,
) -> RoundResult {
    let mut request = ResponseApiRequest {
        model: provider.model.clone(),
        input: serde_json::json!(user_content),
        instructions: Some(system_prompt.to_string()),
        temperature: None,
        max_output_tokens: None,
    };

    match provider.provider_type.as_str() {
        "volcengine" => {
            request.temperature = Some(0.2);
        }
        "openai" => {}
        _ => {
            request.temperature = Some(0.2);
            request.max_output_tokens = Some(4096);
        }
    }

    let mut body = serde_json::to_value(&request).unwrap();
    if !provider.extra.is_empty() {
        if let serde_json::Value::Object(ref mut map) = body {
            for (k, v) in &provider.extra {
                let json_val = toml_to_json(v);
                map.insert(k.clone(), json_val);
            }
        }
    }

    let url = format!(
        "{}/responses",
        provider.base_url.trim_end_matches('/')
    );

    let start = Instant::now();
    let resp = http
        .post(&url)
        .header("Content-Type", "application/json")
        .bearer_auth(&provider.api_key)
        .json(&body)
        .send()
        .await;
    let duration_ms = start.elapsed().as_millis();

    let response = match resp {
        Err(e) => {
            return RoundResult {
                duration_ms,
                output: String::new(),
                tokens: None,
                error: Some(format!("HTTP error: {}", e)),
            }
        }
        Ok(r) => r,
    };

    let status = response.status();
    let body = response.text().await.unwrap_or_default();

    if !status.is_success() {
        return RoundResult {
            duration_ms,
            output: String::new(),
            tokens: None,
            error: Some(format!("HTTP {}: {}", status, truncate(&body, 200))),
        };
    }

    match serde_json::from_str::<ResponseApiResponse>(&body) {
        Err(e) => RoundResult {
            duration_ms,
            output: String::new(),
            tokens: None,
            error: Some(format!("Parse error: {} body={}", e, truncate(&body, 200))),
        },
        Ok(parsed) => {
            let content = parsed
                .output
                .unwrap_or_default()
                .into_iter()
                .filter(|item| item.item_type == "message")
                .filter_map(|item| item.content)
                .flatten()
                .filter(|c| c.content_type.as_deref() == Some("output_text"))
                .filter_map(|c| c.text)
                .collect::<Vec<_>>()
                .join("")
                .trim()
                .to_string();
            let tokens = parsed.usage.and_then(|u| u.total_tokens);
            RoundResult {
                duration_ms,
                output: content,
                tokens,
                error: None,
            }
        }
    }
}

// ── Gemini API call ─────────────────────────────────────────────────────────

async fn run_once_gemini(
    http: &Client,
    provider: &Provider,
    system_prompt: &str,
    user_content: &str,
) -> RoundResult {
    let request = GeminiRequest {
        system_instruction: Some(GeminiContent {
            role: None,
            parts: vec![GeminiPart { text: system_prompt.to_string() }],
        }),
        contents: vec![GeminiContent {
            role: Some("user".into()),
            parts: vec![GeminiPart { text: user_content.to_string() }],
        }],
        generation_config: Some(GeminiGenerationConfig {
            temperature: Some(0.2),
            max_output_tokens: Some(4096),
        }),
    };

    let mut body = serde_json::to_value(&request).unwrap();
    if !provider.extra.is_empty() {
        if let serde_json::Value::Object(ref mut map) = body {
            for (k, v) in &provider.extra {
                map.insert(k.clone(), toml_to_json(v));
            }
        }
    }

    let url = format!(
        "{}/models/{}:generateContent",
        provider.base_url.trim_end_matches('/'),
        provider.model
    );

    let start = Instant::now();
    let resp = http
        .post(&url)
        .header("Content-Type", "application/json")
        .header("x-goog-api-key", &provider.api_key)
        .json(&body)
        .send()
        .await;
    let duration_ms = start.elapsed().as_millis();

    let response = match resp {
        Err(e) => {
            return RoundResult {
                duration_ms,
                output: String::new(),
                tokens: None,
                error: Some(format!("HTTP error: {}", e)),
            }
        }
        Ok(r) => r,
    };

    let status = response.status();
    let body = response.text().await.unwrap_or_default();

    if !status.is_success() {
        return RoundResult {
            duration_ms,
            output: String::new(),
            tokens: None,
            error: Some(format!("HTTP {}: {}", status, truncate(&body, 200))),
        };
    }

    match serde_json::from_str::<GeminiResponse>(&body) {
        Err(e) => RoundResult {
            duration_ms,
            output: String::new(),
            tokens: None,
            error: Some(format!("Parse error: {} body={}", e, truncate(&body, 200))),
        },
        Ok(parsed) => {
            let content = parsed
                .candidates
                .unwrap_or_default()
                .into_iter()
                .next()
                .and_then(|c| c.content)
                .map(|c| {
                    c.parts
                        .into_iter()
                        .map(|p| p.text)
                        .collect::<Vec<_>>()
                        .join("")
                })
                .unwrap_or_default()
                .trim()
                .to_string();
            let tokens = parsed.usage_metadata.and_then(|u| u.total_token_count);
            RoundResult {
                duration_ms,
                output: content,
                tokens,
                error: None,
            }
        }
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

fn toml_to_json(v: &toml::Value) -> serde_json::Value {
    match v {
        toml::Value::String(s) => serde_json::Value::String(s.clone()),
        toml::Value::Integer(i) => serde_json::json!(i),
        toml::Value::Float(f) => serde_json::json!(f),
        toml::Value::Boolean(b) => serde_json::Value::Bool(*b),
        toml::Value::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(toml_to_json).collect())
        }
        toml::Value::Table(t) => {
            let map: serde_json::Map<String, serde_json::Value> =
                t.iter().map(|(k, v)| (k.clone(), toml_to_json(v))).collect();
            serde_json::Value::Object(map)
        }
        toml::Value::Datetime(d) => serde_json::Value::String(d.to_string()),
    }
}

fn resolve_provider(mut p: Provider) -> Provider {
    p.api_key = resolve_env(&p.api_key);
    p.base_url = resolve_env(&p.base_url);
    p
}

fn resolve_env(s: &str) -> String {
    if let Some(var) = s.strip_prefix("${").and_then(|s| s.strip_suffix('}')) {
        match std::env::var(var) {
            Ok(val) => val,
            Err(_) => {
                eprintln!("Warning: env var {} not set", var);
                String::new()
            }
        }
    } else {
        s.to_string()
    }
}

fn truncate(s: &str, max: usize) -> String {
    let mut chars = s.chars();
    let shortened: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        format!("{}...", shortened)
    } else {
        shortened
    }
}

fn get_arg(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn load_config(path: &str) -> Config {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| {
        eprintln!("Failed to read {}: {}", path, e);
        eprintln!("Hint: copy config.example.toml to config.toml and fill in your API keys");
        std::process::exit(1);
    });
    toml::from_str(&text).unwrap_or_else(|e| {
        eprintln!("Failed to parse {}: {}", path, e);
        std::process::exit(1);
    })
}

fn load_cases(path: &str) -> Cases {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| {
        eprintln!("Failed to read {}: {}", path, e);
        eprintln!("Hint: copy test_cases.example.toml to test_cases.toml");
        std::process::exit(1);
    });
    toml::from_str(&text).unwrap_or_else(|e| {
        eprintln!("Failed to parse {}: {}", path, e);
        std::process::exit(1);
    })
}

fn default_prompt() -> String {
    r#"你是一个语音转写文本纠正助手。

你的任务：
- 修正语音识别文本中的识别错误、同音字错误、错别字和标点问题
- 保持原意，不增删信息
- 当识别结果中出现与用户词典中词汇发音相似的词时，替换为词典中的标准形式

用户热词词典：
{{DICTIONARY}}

输出：
纠正后的文本或原文（如果不需要任何修改），不要输出任何其他说明性的内容"#
        .into()
}

fn print_usage() {
    eprintln!("Usage: llm-bench [OPTIONS]");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --config <path>    Provider config file (default: config.toml)");
    eprintln!("  --cases <path>     Test cases file (default: test_cases.toml)");
    eprintln!("  --rounds <n>       Override number of rounds per test");
    eprintln!("  --output <path>    Write detailed results to JSON file");
    eprintln!("  --standings <path> Rolling championship file (default: standings.json)");
    eprintln!("  --no-standings     Do not update or print long-term standings");
    eprintln!("  --skip-judge       Skip the optional judge LLM evaluation");
    eprintln!("  -h, --help         Show this help");
}

// ── Evaluation ──────────────────────────────────────────────────────────────

fn score_checkpoints(ok_rounds: &[&RoundResult], checkpoints: &[Checkpoint]) -> CheckpointTally {
    let mut tally = CheckpointTally::default();
    for r in ok_rounds {
        for cp in checkpoints {
            tally.record(CheckpointTier::parse(&cp.tier), checkpoint_passed(&r.output, cp));
        }
    }
    tally
}

fn format_tally(t: CheckpointTally) -> String {
    let mut parts = Vec::new();
    if t.basic_total > 0 {
        parts.push(format!("basic {}/{}", t.basic_hits, t.basic_total));
    }
    if t.cloud_total > 0 {
        parts.push(format!("cloud {}/{}", t.cloud_hits, t.cloud_total));
    }
    if t.bonus_total > 0 {
        parts.push(format!("bonus {}/{}", t.bonus_hits, t.bonus_total));
    }
    if parts.is_empty() {
        "-".into()
    } else {
        parts.join("  ")
    }
}

fn format_ratio(hits: usize, total: usize) -> String {
    if total == 0 {
        "-".into()
    } else {
        format!("{}/{}", hits, total)
    }
}

fn format_pct(rate: Option<f64>) -> String {
    rate.map(|c| format!("{:.0}%", c * 100.0))
        .unwrap_or_else(|| "-".into())
}

fn checkpoint_passed(output: &str, cp: &Checkpoint) -> bool {
    let mut has_condition = false;
    if let Some(ref needle) = cp.must_contain {
        has_condition = true;
        if !contains_with_case(output, needle, cp.case_sensitive) {
            return false;
        }
    }
    if let Some(ref needle) = cp.must_not_contain {
        has_condition = true;
        if contains_with_case(output, needle, cp.case_sensitive) {
            return false;
        }
    }
    if let Some(ref pattern) = cp.pattern {
        has_condition = true;
        match regex::Regex::new(pattern) {
            Ok(re) => {
                if !re.is_match(output) {
                    return false;
                }
            }
            Err(e) => {
                eprintln!("Warning: invalid checkpoint regex '{}': {}", pattern, e);
                return false;
            }
        }
    }
    has_condition
}

fn contains_with_case(haystack: &str, needle: &str, case_sensitive: bool) -> bool {
    if case_sensitive {
        haystack.contains(needle)
    } else {
        haystack.to_lowercase().contains(&needle.to_lowercase())
    }
}

fn checkpoint_details(output: &str, checkpoints: &[Checkpoint]) -> String {
    checkpoints
        .iter()
        .map(|cp| {
            let mark = if checkpoint_passed(output, cp) {
                "✓"
            } else {
                "✗"
            };
            format!(
                "{} [{}] {}",
                mark,
                CheckpointTier::parse(&cp.tier).as_str(),
                cp.description
            )
        })
        .collect::<Vec<_>>()
        .join("；")
}

fn default_judge_prompt() -> String {
    r#"你是语音转写纠正质量的评审员。各模型的任务是：把 ASR 转写文本纠正为正确、自然的中文，保留原意，不增删信息。

硬性检查点（人名词典、cloud→Claude、README、下一集→下一级等）由程序按维度单独计分，不要因为某条检查点未命中就把总分打到很低。你的 quality_score 只评价「自由发挥」的整体纠正质量：

1. 是否保持原意，有没有胡乱增删、解释或跑题。
2. 通顺程度、标点、中英混排是否自然。
3. 专有名词和文件名是否写得像样（即使程序会另计 cloud/README，你也可以点出好坏，但不要主导打分）。
4. 同音/近音错误是否改对。

不要把延迟或成功率写进 quality_score——那些由程序另计。quality_score 使用 0-10 分（可保留一位小数）。name 必须与输入中的模型显示名完全一致。

只输出 JSON，不要 markdown 代码围栏，不要其它说明。格式：
{
  "providers": [
    {
      "name": "模型显示名",
      "quality_score": 8.5,
      "strengths": "一句话优点",
      "weaknesses": "一句话缺点"
    }
  ],
  "ranking": ["从优到劣的 name"],
  "summary": "2-4 句总体结论，点出谁最适合作为纠正模型及原因"
}"#
        .into()
}

fn build_judge_payload(cases: &[CaseRecord], aggregated: &[ProviderStats]) -> String {
    let mut out = String::new();
    out.push_str("# 汇总指标\n\n");
    for s in aggregated {
        let suc = if s.total_rounds > 0 {
            format!(
                "{}/{} ({:.0}%)",
                s.successes,
                s.total_rounds,
                100.0 * s.successes as f64 / s.total_rounds as f64
            )
        } else {
            "0/0".into()
        };
        let cp = if s.checkpoints.total() > 0 {
            format_tally(s.checkpoints)
        } else {
            "无".into()
        };
        out.push_str(&format!(
            "- {}: 平均 {}ms, 最小 {}ms, 最大 {}ms, 成功 {}, 检查点 {}\n",
            s.name, s.avg_ms, s.min_ms, s.max_ms, suc, cp
        ));
    }
    out.push('\n');

    for (i, c) in cases.iter().enumerate() {
        out.push_str(&format!("# Case {}: {}\n\n", i + 1, c.case.name));
        out.push_str(&format!("原文：{}\n", c.case.input));
        if let Some(ref exp) = c.case.expected {
            out.push_str(&format!("参考答案（不一定是唯一正确写法）：{}\n", exp));
        }
        if !c.case.checkpoint.is_empty() {
            out.push_str("硬性检查点（程序按 basic / cloud / bonus 单独计分，请勿重复重罚）：\n");
            for cp in &c.case.checkpoint {
                out.push_str(&format!(
                    "- [{}] {}\n",
                    CheckpointTier::parse(&cp.tier).as_str(),
                    cp.description
                ));
                if let Some(ref s) = cp.must_contain {
                    out.push_str(&format!("  必须包含：{}\n", s));
                }
                if let Some(ref s) = cp.must_not_contain {
                    out.push_str(&format!("  不得包含：{}\n", s));
                }
                if let Some(ref s) = cp.pattern {
                    out.push_str(&format!("  正则：{}\n", s));
                }
            }
        }
        out.push('\n');
        for p in &c.providers {
            out.push_str(&format!("## {} ({})\n", p.name, p.model));
            out.push_str(&format!(
                "延迟 avg={}ms min={}ms max={}ms；成功 {}/{}\n",
                p.stats.avg_ms,
                p.stats.min_ms,
                p.stats.max_ms,
                p.stats.successes,
                p.stats.total_rounds
            ));
            if p.stats.checkpoints.total() > 0 {
                out.push_str(&format!(
                    "检查点命中 {}\n",
                    format_tally(p.stats.checkpoints)
                ));
            }
            for (ri, r) in p.rounds.iter().enumerate() {
                if let Some(ref err) = r.error {
                    out.push_str(&format!(
                        "- round {}: ERROR {}\n",
                        ri + 1,
                        truncate(err, 120)
                    ));
                } else {
                    out.push_str(&format!("- round {}: {}\n", ri + 1, r.output));
                    if !c.case.checkpoint.is_empty() {
                        out.push_str(&format!(
                            "  检查点：{}\n",
                            checkpoint_details(&r.output, &c.case.checkpoint)
                        ));
                    }
                }
            }
            out.push('\n');
        }
    }
    out
}

fn extract_json_object(s: &str) -> Option<String> {
    let trimmed = s.trim();
    let stripped = if let Some(rest) = trimmed.strip_prefix("```json") {
        rest.trim().strip_suffix("```").unwrap_or(rest).trim()
    } else if let Some(rest) = trimmed.strip_prefix("```") {
        rest.trim().strip_suffix("```").unwrap_or(rest).trim()
    } else {
        trimmed
    };
    if let Some(start) = stripped.find('{') {
        if let Some(end) = stripped.rfind('}') {
            if end > start {
                return Some(stripped[start..=end].to_string());
            }
        }
    }
    None
}

fn parse_judge_report(raw: &str) -> Result<JudgeReport, String> {
    let json = extract_json_object(raw).ok_or_else(|| "no JSON object found".to_string())?;
    serde_json::from_str(&json).map_err(|e| e.to_string())
}

fn lookup_quality(map: &HashMap<String, f64>, name: &str) -> Option<f64> {
    if let Some(v) = map.get(name) {
        return Some(*v);
    }
    map.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| *v)
}

const DEFAULT_W_BASIC: f64 = 0.20;
const DEFAULT_W_CLOUD: f64 = 0.20;
const DEFAULT_W_BONUS: f64 = 0.10;
const DEFAULT_W_QUALITY: f64 = 0.20;
const DEFAULT_W_LATENCY: f64 = 0.30;
const DEFAULT_LATENCY_FULL_MS: f64 = 1000.0;
const DEFAULT_LATENCY_ZERO_MS: f64 = 5000.0;

fn latency_score(avg_ms: u128, successes: usize, full_ms: f64, zero_ms: f64) -> f64 {
    if successes == 0 {
        return 0.0;
    }
    let full = full_ms.max(0.0);
    let zero = zero_ms.max(full + 1.0);
    let x = avg_ms as f64;
    if x <= full {
        1.0
    } else if x >= zero {
        0.0
    } else {
        1.0 - (x - full) / (zero - full)
    }
}

struct ScoreWeights {
    basic: f64,
    cloud: f64,
    bonus: f64,
    quality: f64,
    latency: f64,
    success: f64,
}

fn score_weights(
    eval: &EvalConfig,
    has_basic: bool,
    has_cloud: bool,
    has_bonus: bool,
    use_quality: bool,
) -> ScoreWeights {
    let (wb0, wcloud0, wbonus0) = if eval.weight_basic.is_none()
        && eval.weight_cloud.is_none()
        && eval.weight_bonus.is_none()
    {
        if let Some(lump) = eval.weight_checkpoints {
            (lump * 0.4, lump * 0.4, lump * 0.2)
        } else {
            (DEFAULT_W_BASIC, DEFAULT_W_CLOUD, DEFAULT_W_BONUS)
        }
    } else {
        (
            eval.weight_basic.unwrap_or(DEFAULT_W_BASIC),
            eval.weight_cloud.unwrap_or(DEFAULT_W_CLOUD),
            eval.weight_bonus.unwrap_or(DEFAULT_W_BONUS),
        )
    };

    let mut w = ScoreWeights {
        basic: if has_basic { wb0 } else { 0.0 },
        cloud: if has_cloud { wcloud0 } else { 0.0 },
        bonus: if has_bonus { wbonus0 } else { 0.0 },
        quality: if use_quality {
            eval.weight_quality.unwrap_or(DEFAULT_W_QUALITY)
        } else {
            0.0
        },
        latency: eval.weight_latency.unwrap_or(DEFAULT_W_LATENCY),
        success: eval.weight_success.unwrap_or(0.0),
    };
    let sum = w.basic + w.cloud + w.bonus + w.quality + w.latency + w.success;
    if sum > 0.0 {
        w.basic /= sum;
        w.cloud /= sum;
        w.bonus /= sum;
        w.quality /= sum;
        w.latency /= sum;
        w.success /= sum;
    }
    w
}

fn print_ranking_legend(eval: &EvalConfig, stats: &[ProviderStats], use_quality: bool) {
    let has_basic = stats.iter().any(|s| s.checkpoints.basic_total > 0);
    let has_cloud = stats.iter().any(|s| s.checkpoints.cloud_total > 0);
    let has_bonus = stats.iter().any(|s| s.checkpoints.bonus_total > 0);
    let w = score_weights(eval, has_basic, has_cloud, has_bonus, use_quality);
    let full = eval.latency_full_ms.unwrap_or(DEFAULT_LATENCY_FULL_MS);
    let zero = eval.latency_zero_ms.unwrap_or(DEFAULT_LATENCY_ZERO_MS);
    println!(
        "  \x1b[2mWeights: basic {:.0}% · cloud {:.0}% · bonus {:.0}% · speed {:.0}% · quality {:.0}%{}\x1b[0m",
        w.basic * 100.0,
        w.cloud * 100.0,
        w.bonus * 100.0,
        w.latency * 100.0,
        w.quality * 100.0,
        if w.success > 0.0 {
            format!(" · success {:.0}%", w.success * 100.0)
        } else {
            String::new()
        }
    );
    println!(
        "  \x1b[2mSpeed: 1.0 at ≤{:.0}ms, 0 at ≥{:.0}ms (not min-max across the field)\x1b[0m\n",
        full, zero
    );
}

fn rank_providers(
    stats: &[ProviderStats],
    quality: &HashMap<String, f64>,
    eval: &EvalConfig,
) -> Vec<RankedProvider> {
    let use_quality = !quality.is_empty();
    let has_basic = stats.iter().any(|s| s.checkpoints.basic_total > 0);
    let has_cloud = stats.iter().any(|s| s.checkpoints.cloud_total > 0);
    let has_bonus = stats.iter().any(|s| s.checkpoints.bonus_total > 0);
    let w = score_weights(eval, has_basic, has_cloud, has_bonus, use_quality);
    let full_ms = eval.latency_full_ms.unwrap_or(DEFAULT_LATENCY_FULL_MS);
    let zero_ms = eval.latency_zero_ms.unwrap_or(DEFAULT_LATENCY_ZERO_MS);

    let mut ranked: Vec<RankedProvider> = stats
        .iter()
        .map(|s| {
            let quality_score = lookup_quality(quality, &s.name);
            let quality_norm = quality_score.unwrap_or(0.0) / 10.0;
            let basic_rate = hit_rate(s.checkpoints.basic_hits, s.checkpoints.basic_total);
            let cloud_rate = hit_rate(s.checkpoints.cloud_hits, s.checkpoints.cloud_total);
            let bonus_rate = hit_rate(s.checkpoints.bonus_hits, s.checkpoints.bonus_total);
            let checkpoint_rate = hit_rate(s.checkpoints.hits(), s.checkpoints.total());
            let success_rate = if s.total_rounds > 0 {
                s.successes as f64 / s.total_rounds as f64
            } else {
                0.0
            };
            let lat_score = latency_score(s.avg_ms, s.successes, full_ms, zero_ms);
            let composite = 100.0
                * (w.basic * basic_rate.unwrap_or(0.0)
                    + w.cloud * cloud_rate.unwrap_or(0.0)
                    + w.bonus * bonus_rate.unwrap_or(0.0)
                    + w.quality * quality_norm
                    + w.latency * lat_score
                    + w.success * success_rate);
            RankedProvider {
                name: s.name.clone(),
                composite,
                quality: quality_score,
                checkpoint_rate,
                basic_rate,
                cloud_rate,
                bonus_rate,
                latency_score: lat_score,
                avg_ms: s.avg_ms,
                success_rate,
            }
        })
        .collect();

    ranked.sort_by(|a, b| {
        b.composite
            .partial_cmp(&a.composite)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    ranked
}

// ── Rolling standings ───────────────────────────────────────────────────────

impl StandingScheme {
    fn parse(s: Option<&str>) -> Self {
        match s.map(|v| v.to_ascii_lowercase()).as_deref() {
            Some("f1") | Some("formula1") => Self::F1,
            _ => Self::Borda,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Borda => "borda",
            Self::F1 => "f1",
        }
    }
}

impl StandingLens {
    fn from_eval(eval: &EvalConfig) -> Self {
        Self {
            scheme: StandingScheme::parse(eval.standing_scheme.as_deref())
                .as_str()
                .to_string(),
            decay: eval.standing_decay.unwrap_or(0.9).clamp(0.0, 1.0),
            window: eval.standing_window.unwrap_or(10),
            retire_after: eval.standing_retire_after.unwrap_or(3),
        }
    }
}

const F1_POINTS: [u32; 10] = [25, 18, 15, 12, 10, 8, 6, 4, 2, 1];

fn points_for_place(place: usize, field_size: usize, scheme: StandingScheme) -> f64 {
    if place == 0 {
        return 0.0;
    }
    match scheme {
        StandingScheme::Borda => {
            if place > field_size {
                0.0
            } else {
                (field_size + 1 - place) as f64
            }
        }
        StandingScheme::F1 => F1_POINTS.get(place - 1).copied().unwrap_or(0) as f64,
    }
}

fn composites_equal(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-6
}

fn award_points(ranked: &[RankedProvider], scheme: StandingScheme) -> Vec<RunPlace> {
    let n = ranked.len();
    let mut out = Vec::with_capacity(n);
    let mut i = 0;
    while i < n {
        let place = i + 1;
        let mut j = i + 1;
        while j < n && composites_equal(ranked[i].composite, ranked[j].composite) {
            j += 1;
        }
        let tied = j - i;
        let mut pts = 0.0;
        for p in place..place + tied {
            pts += points_for_place(p, n, scheme);
        }
        pts /= tied as f64;
        for r in &ranked[i..j] {
            out.push(RunPlace {
                place,
                provider: r.name.clone(),
                points: pts,
                composite: r.composite,
                quality: r.quality,
                checkpoint_rate: r.checkpoint_rate,
                basic_rate: r.basic_rate,
                cloud_rate: r.cloud_rate,
                bonus_rate: r.bonus_rate,
                latency_score: Some(r.latency_score),
                avg_ms: r.avg_ms as u64,
                success_rate: r.success_rate,
            });
        }
        i = j;
    }
    out
}

fn windowed_runs(runs: &[StandingRun], window: usize) -> &[StandingRun] {
    if window == 0 || runs.len() <= window {
        runs
    } else {
        &runs[runs.len() - window..]
    }
}

fn consecutive_absences(provider: &str, runs: &[StandingRun]) -> usize {
    let mut n = 0usize;
    for run in runs.iter().rev() {
        if run.ranking.iter().any(|p| p.provider == provider) {
            break;
        }
        n += 1;
    }
    n
}

fn recompute_standings(runs: &[StandingRun], lens: &StandingLens) -> Vec<StandingEntry> {
    struct Acc {
        total_points: f64,
        runs: usize,
        best_place: usize,
        last_place: usize,
        last_points: f64,
        decayed_points: f64,
        weight_sum: f64,
        window_runs: usize,
    }

    let decay = if lens.decay <= 0.0 { 0.0 } else { lens.decay.min(1.0) };
    let windowed = windowed_runs(runs, lens.window);
    let mut map: HashMap<String, Acc> = HashMap::new();

    for run in runs {
        for p in &run.ranking {
            let acc = map.entry(p.provider.clone()).or_insert(Acc {
                total_points: 0.0,
                runs: 0,
                best_place: usize::MAX,
                last_place: 0,
                last_points: 0.0,
                decayed_points: 0.0,
                weight_sum: 0.0,
                window_runs: 0,
            });
            acc.total_points += p.points;
            acc.runs += 1;
            acc.best_place = acc.best_place.min(p.place);
            acc.last_place = p.place;
            acc.last_points = p.points;
        }
    }

    for (age_from_end, run) in windowed.iter().rev().enumerate() {
        let weight = decay.powi(age_from_end as i32);
        for p in &run.ranking {
            if let Some(acc) = map.get_mut(&p.provider) {
                acc.decayed_points += p.points * weight;
                acc.weight_sum += weight;
                acc.window_runs += 1;
            }
        }
    }

    let mut standings: Vec<StandingEntry> = map
        .into_iter()
        .map(|(provider, acc)| {
            let absences = consecutive_absences(&provider, runs);
            let active = lens.retire_after == 0 || absences < lens.retire_after;
            StandingEntry {
                form: if acc.weight_sum > 0.0 {
                    acc.decayed_points / acc.weight_sum
                } else {
                    0.0
                },
                decayed_points: acc.decayed_points,
                avg_points: if acc.runs > 0 {
                    acc.total_points / acc.runs as f64
                } else {
                    0.0
                },
                total_points: acc.total_points,
                runs: acc.runs,
                window_runs: acc.window_runs,
                absences,
                active,
                best_place: if acc.best_place == usize::MAX {
                    0
                } else {
                    acc.best_place
                },
                last_place: acc.last_place,
                last_points: acc.last_points,
                provider,
            }
        })
        .collect();
    standings.sort_by(|a, b| {
        b.active
            .cmp(&a.active)
            .then_with(|| {
                b.form
                    .partial_cmp(&a.form)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| {
                b.decayed_points
                    .partial_cmp(&a.decayed_points)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| a.last_place.cmp(&b.last_place))
            .then_with(|| a.provider.cmp(&b.provider))
    });
    standings
}

fn load_standings(path: &str) -> Result<StandingsFile, String> {
    if !Path::new(path).exists() {
        return Ok(StandingsFile {
            version: 1,
            scheme: String::new(),
            lens: StandingLens::default(),
            runs: Vec::new(),
            standings: Vec::new(),
        });
    }
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

fn save_standings(path: &str, file: &StandingsFile) -> Result<(), String> {
    let json = serde_json::to_string_pretty(file).map_err(|e| e.to_string())?;
    let tmp = format!("{}.tmp", path);
    std::fs::write(&tmp, &json).map_err(|e| e.to_string())?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.to_string());
    }
    Ok(())
}

fn update_standings(
    path: &str,
    lens: StandingLens,
    ranked: &[RankedProvider],
) -> Result<StandingsFile, String> {
    let scheme = StandingScheme::parse(Some(lens.scheme.as_str()));
    let mut file = load_standings(path)?;
    file.runs.push(StandingRun {
        at: chrono::Utc::now().to_rfc3339(),
        scheme: lens.scheme.clone(),
        ranking: award_points(ranked, scheme),
    });
    file.version = 1;
    file.scheme = lens.scheme.clone();
    file.lens = lens;
    file.standings = recompute_standings(&file.runs, &file.lens);
    save_standings(path, &file)?;
    Ok(file)
}

fn print_standings(file: &StandingsFile, name_width: usize) {
    let name_width = file
        .standings
        .iter()
        .map(|s| s.provider.len())
        .max()
        .unwrap_or(name_width)
        .max(name_width)
        .max(8);
    let active: Vec<&StandingEntry> = file.standings.iter().filter(|s| s.active).collect();
    let retired: Vec<&StandingEntry> = file.standings.iter().filter(|s| !s.active).collect();
    let window_label = if file.lens.window == 0 {
        "all".to_string()
    } else {
        file.lens.window.to_string()
    };
    let retire_label = if file.lens.retire_after == 0 {
        "off".to_string()
    } else {
        file.lens.retire_after.to_string()
    };
    println!(
        "\x1b[1m══════════════════════════════════════════════════════════\x1b[0m"
    );
    println!(
        "\x1b[1m  Season standings\x1b[0m  (form · decay={} · window={} · retire after {} · {} {})",
        file.lens.decay,
        window_label,
        retire_label,
        file.runs.len(),
        if file.runs.len() == 1 { "race" } else { "races" }
    );
    println!(
        "\x1b[1m══════════════════════════════════════════════════════════\x1b[0m\n"
    );
    println!(
        "  {:>3}  {:<width$}  {:>6}  {:>7}  {:>5}  {:>4}  {:>4}  {:>6}",
        "#",
        "Provider",
        "Form",
        "Decayed",
        "Races",
        "Best",
        "Last",
        "+Pts",
        width = name_width
    );
    println!(
        "  {:>3}  {:<width$}  {:>6}  {:>7}  {:>5}  {:>4}  {:>4}  {:>6}",
        "─".repeat(3),
        "─".repeat(name_width),
        "──────",
        "───────",
        "─────",
        "────",
        "────",
        "──────",
        width = name_width
    );
    for (i, s) in active.iter().enumerate() {
        println!(
            "  {:>3}  {:<width$}  {:>6.2}  {:>7.1}  {:>5}  {:>4}  {:>4}  {:>+6.1}",
            i + 1,
            s.provider,
            s.form,
            s.decayed_points,
            s.window_runs,
            s.best_place,
            s.last_place,
            s.last_points,
            width = name_width
        );
    }
    println!();
    if !retired.is_empty() {
        println!(
            "  \x1b[2mRetired (absent {}+ consecutive races)\x1b[0m",
            file.lens.retire_after
        );
        for s in retired {
            println!(
                "  \x1b[2m    {:<width$}  form={:.2}  races={}  best={}  missed={}\x1b[0m",
                s.provider,
                s.form,
                s.runs,
                s.best_place,
                s.absences,
                width = name_width
            );
        }
        println!();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cp(desc: &str, contain: Option<&str>, not_contain: Option<&str>) -> Checkpoint {
        cp_tier(desc, contain, not_contain, "")
    }

    fn cp_tier(
        desc: &str,
        contain: Option<&str>,
        not_contain: Option<&str>,
        tier: &str,
    ) -> Checkpoint {
        Checkpoint {
            description: desc.into(),
            must_contain: contain.map(|s| s.into()),
            must_not_contain: not_contain.map(|s| s.into()),
            pattern: None,
            case_sensitive: false,
            tier: tier.into(),
        }
    }

    fn stats(name: &str, avg_ms: u128, successes: usize, hits: usize, total: usize) -> ProviderStats {
        stats_tiers(name, avg_ms, successes, (hits, total), (0, 0), (0, 0))
    }

    fn stats_tiers(
        name: &str,
        avg_ms: u128,
        successes: usize,
        basic: (usize, usize),
        cloud: (usize, usize),
        bonus: (usize, usize),
    ) -> ProviderStats {
        ProviderStats {
            name: name.into(),
            total_rounds: 3,
            successes,
            avg_ms,
            min_ms: avg_ms,
            max_ms: avg_ms,
            avg_tokens: None,
            checkpoints: CheckpointTally {
                basic_hits: basic.0,
                basic_total: basic.1,
                cloud_hits: cloud.0,
                cloud_total: cloud.1,
                bonus_hits: bonus.0,
                bonus_total: bonus.1,
            },
        }
    }

    #[test]
    fn next_level_checkpoint() {
        let c = cp("下一集→下一级", Some("下一级"), Some("下一集"));
        assert!(checkpoint_passed("指向下一级的导航", &c));
        assert!(!checkpoint_passed("指向下一集的导航", &c));
        assert!(!checkpoint_passed("指向下一级，同时下一集还在", &c));
    }

    #[test]
    fn claude_checkpoint_accepts_claude_not_filename() {
        let c = cp(
            "cloud→Claude",
            Some("Claude"),
            Some("cloud md"),
        );
        assert!(checkpoint_passed("然后我们的 CLAUDE.md 里边就去指向 README", &c));
        assert!(checkpoint_passed("Claude.md 指向 README", &c));
        assert!(checkpoint_passed("然后我们的 Claude md 里边就去指向 README", &c));
        assert!(!checkpoint_passed("cloud md 里边就去指向 read me", &c));
        assert!(!checkpoint_passed("Cloud md 里边就去指向 read me", &c));
    }

    #[test]
    fn readme_checkpoint() {
        let c = cp("README", Some("README"), None);
        assert!(checkpoint_passed("指向 README", &c));
        assert!(checkpoint_passed("指向 Readme", &c));
        assert!(!checkpoint_passed("指向 read me", &c));
    }

    #[test]
    fn parse_judge_json_from_fence() {
        let raw = "```json\n{\"providers\":[{\"name\":\"A\",\"quality_score\":8}],\"ranking\":[\"A\"],\"summary\":\"ok\"}\n```";
        let report = parse_judge_report(raw).unwrap();
        assert_eq!(report.providers[0].name, "A");
        assert_eq!(report.providers[0].quality_score, 8.0);
    }

    #[test]
    fn rank_uses_checkpoints_and_latency() {
        let eval = EvalConfig {
            weight_quality: Some(0.0),
            weight_checkpoints: Some(0.7),
            weight_success: Some(0.0),
            ..Default::default()
        };
        let ranked = rank_providers(
            &[
                stats("slow-correct", 2000, 3, 3, 3),
                stats("fast-wrong", 200, 3, 0, 3),
            ],
            &HashMap::new(),
            &eval,
        );
        assert_eq!(ranked[0].name, "slow-correct");
    }

    #[test]
    fn latency_sla_is_absolute_not_minmax() {
        assert_eq!(latency_score(800, 3, 1000.0, 5000.0), 1.0);
        assert!((latency_score(2000, 3, 1000.0, 5000.0) - 0.75).abs() < 1e-9);
        assert_eq!(latency_score(5000, 3, 1000.0, 5000.0), 0.0);
        assert_eq!(latency_score(19000, 3, 1000.0, 5000.0), 0.0);
        assert_eq!(latency_score(900, 0, 1000.0, 5000.0), 0.0);

        let eval = EvalConfig {
            weight_quality: Some(0.0),
            weight_success: Some(0.0),
            ..Default::default()
        };
        let ranked = rank_providers(
            &[
                stats_tiers("fast", 800, 3, (3, 3), (3, 3), (6, 6)),
                stats_tiers("mid", 2000, 3, (3, 3), (3, 3), (6, 6)),
                stats_tiers("stall", 19000, 3, (3, 3), (3, 3), (6, 6)),
            ],
            &HashMap::new(),
            &eval,
        );
        assert_eq!(ranked[0].name, "fast");
        assert_eq!(ranked[1].name, "mid");
        assert_eq!(ranked[2].name, "stall");
        assert!((ranked[0].latency_score - 1.0).abs() < 1e-9);
        assert!((ranked[1].latency_score - 0.75).abs() < 1e-9);
        assert!(ranked[2].latency_score.abs() < 1e-9);
        // A 19s outlier must not stretch the mid model up toward 1.0 the way min-max did.
        assert!(ranked[1].latency_score < 0.80);
    }

    #[test]
    fn cloud_outweighs_bonus_icing() {
        let eval = EvalConfig {
            weight_quality: Some(0.0),
            weight_success: Some(0.0),
            ..Default::default()
        };
        let ranked = rank_providers(
            &[
                stats_tiers("miss-cloud", 800, 3, (3, 3), (0, 3), (6, 6)),
                stats_tiers("miss-bonus", 800, 3, (3, 3), (3, 3), (0, 6)),
            ],
            &HashMap::new(),
            &eval,
        );
        assert_eq!(ranked[0].name, "miss-bonus");
        assert!(ranked[0].composite > ranked[1].composite);
    }

    #[test]
    fn speed_beats_icing_when_basics_match() {
        let eval = EvalConfig {
            weight_quality: Some(0.0),
            weight_success: Some(0.0),
            ..Default::default()
        };
        let ranked = rank_providers(
            &[
                stats_tiers("fast-no-icing", 700, 3, (3, 3), (3, 3), (0, 6)),
                stats_tiers("slow-icing", 4000, 3, (3, 3), (3, 3), (6, 6)),
            ],
            &HashMap::new(),
            &eval,
        );
        assert_eq!(ranked[0].name, "fast-no-icing");
    }

    #[test]
    fn checkpoints_score_by_tier() {
        let round = RoundResult {
            duration_ms: 10,
            output: "徐隽博 用 Claude 写 README，指向下一级".into(),
            tokens: None,
            error: None,
        };
        let tally = score_checkpoints(
            &[&round],
            &[
                cp_tier("name", Some("徐隽博"), Some("徐俊博"), "basic"),
                cp_tier("cloud", Some("Claude"), Some("cloud md"), "cloud"),
                cp_tier("readme", Some("README"), None, "bonus"),
                cp_tier("next", Some("下一级"), Some("下一集"), "bonus"),
            ],
        );
        assert_eq!((tally.basic_hits, tally.basic_total), (1, 1));
        assert_eq!((tally.cloud_hits, tally.cloud_total), (1, 1));
        assert_eq!((tally.bonus_hits, tally.bonus_total), (2, 2));
    }

    #[test]
    fn judge_table_must_not_swallow_prompt_or_dictionary() {
        let misplaced = r#"
[judge]
name = "Quality-Judge"
base_url = "https://example.com"
api_key = "k"
model = "m"
prompt = "should stay at root"
dictionary = "徐隽博"
[[provider]]
name = "p"
base_url = "https://example.com"
api_key = "k"
model = "m"
"#;
        assert!(
            toml::from_str::<Config>(misplaced).is_err(),
            "prompt/dictionary under [judge] must fail instead of being dropped"
        );

        let ok = r#"
prompt = "keep original meaning"
dictionary = "徐隽博"
[judge]
name = "Quality-Judge"
base_url = "https://example.com"
api_key = "k"
model = "m"
[[provider]]
name = "p"
base_url = "https://example.com"
api_key = "k"
model = "m"
"#;
        let cfg: Config = toml::from_str(ok).unwrap();
        assert_eq!(cfg.prompt.as_deref(), Some("keep original meaning"));
        assert_eq!(cfg.dictionary.as_deref(), Some("徐隽博"));
        assert_eq!(cfg.judge.unwrap().name, "Quality-Judge");
    }

    fn rp(name: &str, composite: f64) -> RankedProvider {
        RankedProvider {
            name: name.into(),
            composite,
            quality: None,
            checkpoint_rate: None,
            basic_rate: None,
            cloud_rate: None,
            bonus_rate: None,
            latency_score: 1.0,
            avg_ms: 100,
            success_rate: 1.0,
        }
    }

    #[test]
    fn borda_awards_n_down_to_one() {
        let places = award_points(
            &[rp("a", 90.0), rp("b", 80.0), rp("c", 70.0)],
            StandingScheme::Borda,
        );
        assert_eq!(places[0].points, 3.0);
        assert_eq!(places[1].points, 2.0);
        assert_eq!(places[2].points, 1.0);
        assert_eq!(places[0].place, 1);
    }

    #[test]
    fn f1_only_scores_top_ten() {
        let field: Vec<RankedProvider> = (0..12)
            .map(|i| rp(&format!("m{i}"), 100.0 - i as f64))
            .collect();
        let places = award_points(&field, StandingScheme::F1);
        assert_eq!(places[0].points, 25.0);
        assert_eq!(places[9].points, 1.0);
        assert_eq!(places[10].points, 0.0);
        assert_eq!(places[11].points, 0.0);
    }

    #[test]
    fn tied_places_share_points() {
        let places = award_points(
            &[rp("a", 80.0), rp("b", 80.0), rp("c", 10.0)],
            StandingScheme::Borda,
        );
        assert_eq!(places[0].place, 1);
        assert_eq!(places[1].place, 1);
        assert_eq!(places[2].place, 3);
        assert!((places[0].points - 2.5).abs() < 1e-9);
        assert!((places[1].points - 2.5).abs() < 1e-9);
        assert_eq!(places[2].points, 1.0);
    }

    #[test]
    fn standings_accumulate_across_runs() {
        let run1 = StandingRun {
            at: "1".into(),
            scheme: "borda".into(),
            ranking: award_points(&[rp("a", 90.0), rp("b", 10.0)], StandingScheme::Borda),
        };
        let run2 = StandingRun {
            at: "2".into(),
            scheme: "borda".into(),
            ranking: award_points(&[rp("b", 90.0), rp("a", 10.0)], StandingScheme::Borda),
        };
        let table = recompute_standings(&[run1, run2], &all_history_lens());
        assert_eq!(table[0].total_points, 3.0);
        assert_eq!(table[1].total_points, 3.0);
        assert_eq!(table[0].runs, 2);
        assert_eq!(table[0].avg_points, 1.5);
        assert!(table.iter().all(|s| s.active));
    }

    fn all_history_lens() -> StandingLens {
        StandingLens {
            scheme: "borda".into(),
            decay: 1.0,
            window: 0,
            retire_after: 0,
        }
    }

    fn race(at: &str, order: &[&str]) -> StandingRun {
        let ranked: Vec<RankedProvider> = order
            .iter()
            .enumerate()
            .map(|(i, name)| rp(name, 100.0 - i as f64))
            .collect();
        StandingRun {
            at: at.into(),
            scheme: "borda".into(),
            ranking: award_points(&ranked, StandingScheme::Borda),
        }
    }

    #[test]
    fn form_lets_new_model_overtake_historical_leader() {
        let mut runs = Vec::new();
        for i in 0..6 {
            runs.push(race(&format!("old-{i}"), &["veteran", "filler"]));
        }
        runs.push(race("new-1", &["rookie", "veteran"]));
        runs.push(race("new-2", &["rookie", "veteran"]));
        let table = recompute_standings(
            &runs,
            &StandingLens {
                scheme: "borda".into(),
                decay: 0.8,
                window: 10,
                retire_after: 0,
            },
        );
        assert_eq!(table[0].provider, "rookie");
        assert!(table[0].form > table.iter().find(|s| s.provider == "veteran").unwrap().form);
        assert!(
            table.iter().find(|s| s.provider == "veteran").unwrap().total_points
                > table[0].total_points,
            "veteran still has a higher raw total; form is what ranks"
        );
    }

    #[test]
    fn missing_recent_races_retires_a_model() {
        let runs = vec![
            race("1", &["old", "keep"]),
            race("2", &["keep"]),
            race("3", &["keep"]),
            race("4", &["keep"]),
        ];
        let table = recompute_standings(
            &runs,
            &StandingLens {
                scheme: "borda".into(),
                decay: 1.0,
                window: 0,
                retire_after: 3,
            },
        );
        let old = table.iter().find(|s| s.provider == "old").unwrap();
        let keep = table.iter().find(|s| s.provider == "keep").unwrap();
        assert!(!old.active);
        assert_eq!(old.absences, 3);
        assert!(keep.active);
        assert_eq!(table[0].provider, "keep");
    }

    #[test]
    fn window_drops_old_races_from_form() {
        let runs = vec![
            race("old-win", &["aged", "now"]),
            race("recent-1", &["now", "aged"]),
            race("recent-2", &["now", "aged"]),
        ];
        let table = recompute_standings(
            &runs,
            &StandingLens {
                scheme: "borda".into(),
                decay: 1.0,
                window: 2,
                retire_after: 0,
            },
        );
        let now = table.iter().find(|s| s.provider == "now").unwrap();
        let aged = table.iter().find(|s| s.provider == "aged").unwrap();
        assert_eq!(now.window_runs, 2);
        assert_eq!(aged.window_runs, 2);
        assert!(now.form > aged.form);
        assert_eq!(aged.runs, 3);
    }
}
