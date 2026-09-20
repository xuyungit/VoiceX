mod adjudicate;
mod typesafe;

use adjudicate::{Analysis, Answers, Ask, Pin, Reference, Scored, Tier};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;
use typesafe::{Judge, JudgeRun, Questions};

// ── Config ──────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct Config {
    rounds: Option<usize>,
    prompt: Option<String>,
    dictionary: Option<String>,
    provider: Vec<Provider>,
    /// Optional: settles the verdicts code cannot (is this other word the speaker's word, did this unrequested
    /// change do harm). Must be `type = "typesafe"`. Runs after all provider tests finish, outside their timing.
    judge: Option<Provider>,
    #[serde(default)]
    eval: EvalConfig,
}

#[derive(Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
struct EvalConfig {
    /// Dictionary sites: the dictionary term stands where the speaker said it. Default 0.40.
    /// Unset, the legacy `weight_basic` + `weight_cloud` are summed instead.
    weight_dictionary: Option<f64>,
    /// Semantic sites: a wrong word outside the dictionary, fixed from context. Default 0.15.
    #[serde(alias = "weight_bonus")]
    weight_semantic: Option<f64>,
    /// Clean transcript: nothing damaged, rephrased or added outside the sites. Default 0.10.
    #[serde(alias = "weight_quality")]
    weight_clean: Option<f64>,
    /// Voice-correction latency. Default 0.35.
    weight_latency: Option<f64>,
    /// Optional; default 0 (a failed call already scores zero on latency, every site and the transcript).
    weight_success: Option<f64>,
    /// Legacy names of the two dictionary tiers; see `weight_dictionary`.
    weight_basic: Option<f64>,
    weight_cloud: Option<f64>,
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
    /// Override the judge's questions; defaults to the embedded typesafe_questions.json.
    typesafe_questions: Option<String>,
    /// Max concurrent judge calls. Default 8.
    typesafe_concurrency: Option<usize>,
}

impl EvalConfig {
    fn validate(&self) -> Result<(), String> {
        if self.weight_dictionary.is_some() && (self.weight_basic.is_some() || self.weight_cloud.is_some()) {
            return Err("[eval] set `weight_dictionary`, or the legacy `weight_basic` / `weight_cloud`, not both".into());
        }
        Ok(())
    }
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

#[derive(Debug, Deserialize)]
struct Cases {
    case: Vec<TestCase>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct TestCase {
    name: String,
    /// What the recognizer produced.
    input: String,
    /// What the speaker said. The sites to correct are the differences between the two.
    expected: String,
    /// Human verdicts: whoever wrote `written` where `heard` stood gets `credit`. Final, the judge is not asked.
    #[serde(default)]
    pin: Vec<Pin>,
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
    /// Nested the way the Responses API and the main app send it.
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<ResponseApiReasoning>,
}

#[derive(Serialize)]
struct ResponseApiReasoning {
    effort: String,
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
    #[serde(rename = "thinkingConfig", skip_serializing_if = "Option::is_none")]
    thinking_config: Option<GeminiThinkingConfig>,
}

#[derive(Serialize, Default)]
struct GeminiThinkingConfig {
    /// Gemini 3.x: MINIMAL / LOW / MEDIUM / HIGH. MINIMAL is rejected by 3.7/3.8 Flash.
    #[serde(rename = "thinkingLevel", skip_serializing_if = "Option::is_none")]
    thinking_level: Option<String>,
    /// Gemini 2.5 Flash: 0 disables thinking. Invalid on current 3.x Flash-Lite.
    #[serde(rename = "thinkingBudget", skip_serializing_if = "Option::is_none")]
    thinking_budget: Option<i32>,
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
    #[allow(dead_code)]
    #[serde(rename = "thoughtsTokenCount")]
    thoughts_token_count: Option<u32>,
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
    tallies: Tallies,
}

/// Credit earned over the items of one tier. An item that needed the judge and got no answer is `unjudged`:
/// it is reported, and left out of the rate rather than counted as a miss.
#[derive(Clone, Copy, Default, Serialize)]
struct Tally {
    credit: f64,
    total: usize,
    unjudged: usize,
}

impl Tally {
    fn add(&mut self, credit: Option<f64>) {
        self.total += 1;
        match credit {
            Some(c) => self.credit += c,
            None => self.unjudged += 1,
        }
    }

    fn merge(&mut self, other: Self) {
        self.credit += other.credit;
        self.total += other.total;
        self.unjudged += other.unjudged;
    }

    fn judged(self) -> usize {
        self.total - self.unjudged
    }

    fn rate(self) -> Option<f64> {
        (self.judged() > 0).then(|| self.credit / self.judged() as f64)
    }
}

#[derive(Clone, Copy, Default)]
struct Tallies {
    dictionary: Tally,
    semantic: Tally,
    /// One item per output: what its changes outside the sites left of a clean transcript.
    clean: Tally,
}

impl Tallies {
    fn add(&mut self, scored: &Scored) {
        for site in &scored.sites {
            self.tier(site.tier).add(site.credit);
        }
        self.clean.add(scored.clean);
    }

    /// A failed call corrected nothing: every site of the case scores zero, and so does the transcript.
    fn add_failed(&mut self, reference: &Reference) {
        for (tier, _, _) in reference.sites() {
            self.tier(tier).add(Some(0.0));
        }
        self.clean.add(Some(0.0));
    }

    fn tier(&mut self, tier: Tier) -> &mut Tally {
        match tier {
            Tier::Dictionary => &mut self.dictionary,
            Tier::Semantic => &mut self.semantic,
        }
    }

    fn merge(&mut self, other: Self) {
        self.dictionary.merge(other.dictionary);
        self.semantic.merge(other.semantic);
        self.clean.merge(other.clean);
    }

    fn unjudged(self) -> usize {
        self.dictionary.unjudged + self.semantic.unjudged + self.clean.unjudged
    }
}

struct CaseRecord {
    case: TestCase,
    reference: Reference,
    providers: Vec<ProviderRecord>,
}

struct ProviderRecord {
    name: String,
    model: String,
    stats: ProviderStats,
    rounds: Vec<RoundRecord>,
}

struct RoundRecord {
    result: RoundResult,
    /// What code found in the output; `None` when the call failed.
    analysis: Option<Analysis>,
    /// The analysis with the judge's answers applied.
    scored: Option<Scored>,
}

struct RankedProvider {
    name: String,
    composite: f64,
    dictionary_rate: Option<f64>,
    semantic_rate: Option<f64>,
    clean_rate: Option<f64>,
    /// Items without a verdict: the rates cover the judged items only.
    unjudged: usize,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dictionary_rate: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    semantic_rate: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    clean_rate: Option<f64>,
    /// Races scored with checkpoints and a free-form judge carry these instead; kept so their history survives a save.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    quality: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    checkpoint_rate: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    basic_rate: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cloud_rate: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
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

    let http = Client::new();
    let judge = config
        .judge
        .map(resolve_provider)
        .map(|j| build_judge(&http, j, &eval_cfg));

    // What every case requires, before the first provider call: a case that cannot be read costs nothing yet.
    let terms = adjudicate::dictionary_terms(&dictionary);
    let references: Vec<Reference> = cases
        .case
        .iter()
        .map(|c| Reference::new(&c.input, &c.expected, &terms))
        .collect();

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
    match &judge {
        Some(j) if skip_judge => println!("  \x1b[2mJudge: {} (skipped)\x1b[0m", j.name),
        Some(j) => println!("  \x1b[2mJudge: {} ({})\x1b[0m", j.name, j.model),
        None => println!("  \x1b[2mJudge: none\x1b[0m"),
    }
    println!();

    let mut bench_cases: Vec<CaseRecord> = Vec::new();

    for (case, reference) in cases.case.iter().zip(references) {
        println!("\x1b[1;36m━━━ {} ━━━\x1b[0m", case.name);
        println!("\x1b[2mInput:\x1b[0m    {}", case.input);
        println!("\x1b[2mExpected:\x1b[0m {}", case.expected);
        let sites = reference.sites();
        if sites.is_empty() {
            println!("\x1b[2mSites:\x1b[0m    none (input already reads as expected)");
        } else {
            println!("\x1b[2mSites:\x1b[0m");
            for (tier, heard, intended) in &sites {
                println!("  \x1b[2m• {:<10}  {} → {}\x1b[0m", tier.name(), heard, intended);
            }
        }
        println!();

        let mut case_providers: Vec<ProviderRecord> = Vec::new();

        for provider in &providers {
            let mut records: Vec<RoundRecord> = Vec::new();

            for _ in 0..rounds {
                let result =
                    run_once(&http, provider, &prompt, &dictionary, &case.input).await;
                let analysis = result
                    .error
                    .is_none()
                    .then(|| reference.analyze(&result.output, &case.pin));
                records.push(RoundRecord { result, analysis, scored: None });
            }

            // Display
            let first = &records[0].result;
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

            let ok: Vec<&RoundResult> = records
                .iter()
                .map(|r| &r.result)
                .filter(|r| r.error.is_none())
                .collect();
            // The final tallies wait for the judge; this is what code alone already settles.
            let mut by_code = Tallies::default();
            for analysis in records.iter().filter_map(|r| r.analysis.as_ref()) {
                by_code.add(&adjudicate::score(analysis, &Answers::new()));
            }
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
                    tallies: Tallies::default(),
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
                print!("  \x1b[2mBy code: {}\x1b[0m", format_by_code(by_code));
                if ok.len() < records.len() {
                    print!(
                        "  \x1b[33m({}/{} succeeded)\x1b[0m",
                        ok.len(),
                        records.len()
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
                    tallies: Tallies::default(),
                }
            };

            case_providers.push(ProviderRecord {
                name: provider.name.clone(),
                model: provider.model.clone(),
                stats,
                rounds: records,
            });
        }

        bench_cases.push(CaseRecord {
            case: case.clone(),
            reference,
            providers: case_providers,
        });
    }

    // ── Judge ───────────────────────────────────────────────────────────────
    // Outside every provider's timing. Each distinct question is asked once, whoever needs the answer.
    let asks: Vec<Ask> = bench_cases
        .iter()
        .flat_map(|c| &c.providers)
        .flat_map(|p| &p.rounds)
        .filter_map(|r| r.analysis.as_ref())
        .flat_map(|a| a.asks().into_iter().cloned())
        .collect();
    let needed = asks.len();
    let judge_run: Option<JudgeRun> = match &judge {
        _ if needed == 0 => {
            println!("Code settled every verdict; the judge was not needed.\n");
            None
        }
        None => {
            println!("No [judge] configured: {} verdicts stay unjudged.\n", needed);
            None
        }
        Some(_) if skip_judge => {
            println!("Judge skipped (--skip-judge): {} verdicts stay unjudged.\n", needed);
            None
        }
        Some(j) => {
            println!(
                "\x1b[1m══════════════════════════════════════════════════════════\x1b[0m"
            );
            println!("\x1b[1m  Judge\x1b[0m  ({})", j.model);
            println!(
                "\x1b[1m══════════════════════════════════════════════════════════\x1b[0m\n"
            );
            let run = j.ask_all(asks).await;
            println!(
                "  {} verdicts needed the judge · {} distinct questions · {} failed · tokens: {} in / {} out",
                needed,
                run.distinct,
                run.failures.len(),
                run.input_tokens,
                run.output_tokens
            );
            for (question, error) in &run.failures {
                println!("  \x1b[31m✗\x1b[0m {} — {}", question, error);
            }
            println!();
            Some(run)
        }
    };

    // ── Score ───────────────────────────────────────────────────────────────
    let no_answers = Answers::new();
    let answers = judge_run.as_ref().map_or(&no_answers, |run| &run.answers);
    let mut review: Vec<String> = Vec::new();
    let mut unjudged: Vec<String> = Vec::new();
    for record in &mut bench_cases {
        for p in &mut record.providers {
            let mut tallies = Tallies::default();
            for r in &mut p.rounds {
                let Some(analysis) = &r.analysis else {
                    tallies.add_failed(&record.reference);
                    continue;
                };
                let scored = adjudicate::score(analysis, answers);
                tallies.add(&scored);
                review.extend(scored.review.iter().map(|v| format!("[{}] {}", record.case.name, v)));
                unjudged.extend(scored.unjudged.iter().map(|v| format!("[{}] {}", record.case.name, v)));
                r.scored = Some(scored);
            }
            p.stats.tallies = tallies;
        }
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
        "Dictionary",
        "Semantic",
        "Clean",
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
        let mut tallies = Tallies::default();

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
                tallies.merge(s.tallies);
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
            format_tally(tallies.dictionary),
            format_tally(tallies.semantic),
            format_tally(tallies.clean),
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
            tallies,
        });
    }
    println!();

    // ── Ranking ─────────────────────────────────────────────────────────────
    let ranked = rank_providers(&aggregated, &eval_cfg);

    println!(
        "\x1b[1m══════════════════════════════════════════════════════════\x1b[0m"
    );
    println!("\x1b[1m  Ranking (weighted dimensions)\x1b[0m");
    println!(
        "\x1b[1m══════════════════════════════════════════════════════════\x1b[0m\n"
    );
    print_ranking_legend(&eval_cfg, &aggregated);

    println!(
        "  {:>3}  {:<width$}  {:>10}  {:>10}  {:>8}  {:>6}  {:>6}  {:>8}",
        "#",
        "Provider",
        "Composite",
        "Dictionary",
        "Semantic",
        "Clean",
        "Speed",
        "Avg ms",
        width = name_width
    );
    println!(
        "  {:>3}  {:<width$}  {:>10}  {:>10}  {:>8}  {:>6}  {:>6}  {:>8}",
        "─".repeat(3),
        "─".repeat(name_width),
        "──────────",
        "──────────",
        "────────",
        "──────",
        "──────",
        "────────",
        width = name_width
    );

    for (i, r) in ranked.iter().enumerate() {
        println!(
            "  {:>3}  {:<width$}  {:>10}  {:>10}  {:>8}  {:>6}  {:>5.0}%  {:>8}",
            i + 1,
            r.name,
            format!("{:.1}{}", r.composite, if r.unjudged > 0 { "*" } else { " " }),
            format_pct(r.dictionary_rate),
            format_pct(r.semantic_rate),
            format_pct(r.clean_rate),
            r.latency_score * 100.0,
            r.avg_ms,
            width = name_width
        );
    }
    println!();

    let review = counted(&review);
    let unjudged = counted(&unjudged);
    if !unjudged.is_empty() {
        println!(
            "  \x1b[33m* Verdicts without an answer are left out of the rates, never counted as misses; a tier with nothing judged is left out of that model's composite.\x1b[0m"
        );
        println!("  \x1b[33mUnjudged ({}):\x1b[0m", unjudged.len());
        for (item, outputs) in &unjudged {
            println!("    {}  \x1b[2m×{}\x1b[0m", item, outputs);
        }
        println!();
    }
    if !review.is_empty() {
        println!(
            "  \x1b[33mReview ({}):\x1b[0m the judge was unsure here. A [[case.pin]] with heard / written / credit settles one for good.",
            review.len()
        );
        for (verdict, outputs) in &review {
            println!("    {}  \x1b[2m×{}\x1b[0m", verdict, outputs);
        }
        println!();
    }

    if !skip_standings && !ranked.is_empty() {
        let lens = StandingLens::from_eval(&eval_cfg);
        match update_standings(&standings_path, lens, &ranked) {
            Ok(file) => print_standings(&file, name_width),
            Err(e) => eprintln!("Failed to update {}: {}", standings_path, e),
        }
    }

    // Write JSON output if requested
    if let Some(path) = output_path {
        let cases_json: Vec<serde_json::Value> = bench_cases
            .iter()
            .flat_map(|record| record.providers.iter().map(move |p| (record, p)))
            .map(|(record, p)| {
                let rounds: Vec<serde_json::Value> = p
                    .rounds
                    .iter()
                    .map(|r| {
                        serde_json::json!({
                            "duration_ms": r.result.duration_ms,
                            "output": r.result.output,
                            "tokens": r.result.tokens,
                            "error": r.result.error,
                            "score": &r.scored,
                        })
                    })
                    .collect();
                serde_json::json!({
                    "case": record.case.name,
                    "provider": p.name,
                    "model": p.model,
                    "avg_ms": p.stats.avg_ms,
                    "min_ms": p.stats.min_ms,
                    "max_ms": p.stats.max_ms,
                    "successes": p.stats.successes,
                    "total_rounds": p.stats.total_rounds,
                    "avg_tokens": p.stats.avg_tokens,
                    "dictionary": p.stats.tallies.dictionary,
                    "semantic": p.stats.tallies.semantic,
                    "clean": p.stats.tallies.clean,
                    "rounds": rounds,
                })
            })
            .collect();
        let ranking_json: Vec<serde_json::Value> = ranked
            .iter()
            .enumerate()
            .map(|(i, r)| {
                serde_json::json!({
                    "rank": i + 1,
                    "provider": r.name,
                    "composite": r.composite,
                    "dictionary_rate": r.dictionary_rate,
                    "semantic_rate": r.semantic_rate,
                    "clean_rate": r.clean_rate,
                    "unjudged": r.unjudged,
                    "latency_score": r.latency_score,
                    "avg_ms": r.avg_ms,
                    "success_rate": r.success_rate,
                })
            })
            .collect();
        let listed = |lines: &[(&str, usize)]| -> Vec<serde_json::Value> {
            lines
                .iter()
                .map(|(what, outputs)| serde_json::json!({ "what": what, "outputs": outputs }))
                .collect()
        };
        let payload = serde_json::json!({
            "cases": cases_json,
            "summary": aggregated.iter().map(|s| serde_json::json!({
                "provider": s.name,
                "avg_ms": s.avg_ms,
                "min_ms": s.min_ms,
                "max_ms": s.max_ms,
                "avg_tokens": s.avg_tokens,
                "successes": s.successes,
                "total_rounds": s.total_rounds,
                "dictionary": s.tallies.dictionary,
                "semantic": s.tallies.semantic,
                "clean": s.tallies.clean,
            })).collect::<Vec<_>>(),
            "ranking": ranking_json,
            "judge": {
                "model": judge.as_ref().map(|j| j.model.as_str()),
                "ran": judge_run.is_some(),
                "verdicts_needed": needed,
                "distinct_questions": judge_run.as_ref().map(|run| run.distinct),
                "input_tokens": judge_run.as_ref().map(|run| run.input_tokens),
                "output_tokens": judge_run.as_ref().map(|run| run.output_tokens),
                "failures": judge_run.iter().flat_map(|run| &run.failures).map(|(question, error)| serde_json::json!({
                    "question": question,
                    "error": error,
                })).collect::<Vec<_>>(),
                "review": listed(&review),
                "unjudged": listed(&unjudged),
            },
        });
        let json = serde_json::to_string_pretty(&payload).unwrap();
        std::fs::write(&path, &json).unwrap_or_else(|e| {
            eprintln!("Failed to write {}: {}", path, e);
        });
        println!("Results written to {}", path);
    }
}

/// The judge settles what code cannot: is this other word the speaker's word, did this unrequested change do harm.
/// It answers fixed questions with a probability per answer, which a chat model does not give.
fn build_judge(http: &Client, provider: Provider, eval: &EvalConfig) -> Judge {
    if provider.provider_type != "typesafe" {
        eprintln!(
            "[judge] `{}` has type \"{}\"; the judge must be `type = \"typesafe\"` (see config.example.toml)",
            provider.name, provider.provider_type
        );
        std::process::exit(1);
    }
    let questions = Questions::load(eval.typesafe_questions.as_deref()).unwrap_or_else(|e| {
        eprintln!("Judge questions: {}", e);
        std::process::exit(1);
    });
    Judge {
        name: provider.name,
        http: http.clone(),
        url: typesafe::endpoint(&provider.base_url),
        api_key: provider.api_key,
        model: provider.model,
        questions,
        concurrency: eval.typesafe_concurrency.unwrap_or(8),
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

    let body = build_chat_request_body(provider, system_prompt, user_content);

    let url = format!(
        "{}/chat/completions",
        provider.base_url.trim_end_matches('/')
    );

    // The clock stops after the body is read, not after `send()`: that resolves
    // on the response headers, which api.deepseek.com flushes ~100 ms in and
    // then holds the connection open until the completion is ready.
    let start = Instant::now();
    let resp = http
        .post(&url)
        .header("Content-Type", "application/json")
        .bearer_auth(&provider.api_key)
        .json(&body)
        .send()
        .await;

    let response = match resp {
        Err(e) => {
            return RoundResult {
                duration_ms: start.elapsed().as_millis(),
                output: String::new(),
                tokens: None,
                error: Some(format!("HTTP error: {}", e)),
            }
        }
        Ok(r) => r,
    };

    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    let duration_ms = start.elapsed().as_millis();

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
    let body = build_responses_request_body(provider, system_prompt, user_content);

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

    let response = match resp {
        Err(e) => {
            return RoundResult {
                duration_ms: start.elapsed().as_millis(),
                output: String::new(),
                tokens: None,
                error: Some(format!("HTTP error: {}", e)),
            }
        }
        Ok(r) => r,
    };

    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    let duration_ms = start.elapsed().as_millis();

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
            thinking_config: gemini_thinking_config(provider),
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

    let response = match resp {
        Err(e) => {
            return RoundResult {
                duration_ms: start.elapsed().as_millis(),
                output: String::new(),
                tokens: None,
                error: Some(format!("HTTP error: {}", e)),
            }
        }
        Ok(r) => r,
    };

    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    let duration_ms = start.elapsed().as_millis();

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

fn configured_reasoning_effort(provider: &Provider) -> Option<String> {
    provider
        .reasoning_effort
        .as_deref()
        .map(str::trim)
        .filter(|effort| !effort.is_empty())
        .map(|effort| effort.to_string())
}

fn responses_reasoning(provider: &Provider) -> Option<ResponseApiReasoning> {
    configured_reasoning_effort(provider).map(|effort| ResponseApiReasoning { effort })
}

/// Chat-completions body, including vendor extras. An entry that names no
/// knob gets the main app's default: Volcengine `thinking: disabled`, Qwen
/// `enable_thinking=false` (see `merge_provider_extra`).
fn build_chat_request_body(
    provider: &Provider,
    system_prompt: String,
    user_content: String,
) -> serde_json::Value {
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
            request.reasoning_effort = configured_reasoning_effort(provider);
        }
        "openai" => {
            request.max_completion_tokens = Some(4096);
            request.reasoning_effort = configured_reasoning_effort(provider);
        }
        _ => {
            request.temperature = Some(0.2);
            request.max_tokens = Some(4096);
            request.reasoning_effort = configured_reasoning_effort(provider);
        }
    }

    let mut body = serde_json::to_value(&request).unwrap();
    merge_provider_extra(&mut body, provider);
    body
}

/// Responses API body. Effort is nested as `reasoning.effort`, matching the
/// main app. The previous path dropped `reasoning_effort` entirely.
fn build_responses_request_body(
    provider: &Provider,
    system_prompt: &str,
    user_content: &str,
) -> serde_json::Value {
    let mut request = ResponseApiRequest {
        model: provider.model.clone(),
        input: serde_json::json!(user_content),
        instructions: Some(system_prompt.to_string()),
        temperature: None,
        max_output_tokens: None,
        reasoning: None,
    };

    match provider.provider_type.as_str() {
        "volcengine" => {
            request.temperature = Some(0.2);
            request.reasoning = responses_reasoning(provider);
        }
        "openai" => {
            request.reasoning = responses_reasoning(provider);
        }
        _ => {
            request.temperature = Some(0.2);
            request.max_output_tokens = Some(4096);
            request.reasoning = responses_reasoning(provider);
        }
    }

    let mut body = serde_json::to_value(&request).unwrap();
    merge_provider_extra(&mut body, provider);
    body
}

/// Merge `[provider.extra]` into the request, then fill in the main app's
/// lowest-reasoning default where the entry names no knob: `type = "qwen"`
/// turns `enable_thinking` off, and `type = "volcengine"` without a
/// `reasoning_effort` sends `thinking: disabled`, the one spelling every Ark
/// model honors (`minimal` and `none` are ignored by some).
fn merge_provider_extra(body: &mut serde_json::Value, provider: &Provider) {
    let serde_json::Value::Object(map) = body else {
        return;
    };
    for (k, v) in &provider.extra {
        map.insert(k.clone(), toml_to_json(v));
    }
    match provider.provider_type.as_str() {
        "qwen" => {
            map.entry("enable_thinking".to_string())
                .or_insert(serde_json::Value::Bool(false));
        }
        "volcengine" if configured_reasoning_effort(provider).is_none() => {
            map.entry("thinking".to_string())
                .or_insert(serde_json::json!({ "type": "disabled" }));
        }
        _ => {}
    }
}

/// Lowest thinking Gemini accepts for this model. 3.7/3.8 Flash reject MINIMAL;
/// 2.5 Flash can set thinkingBudget=0; Flash-Lite already thinks off by default.
fn gemini_thinking_config(provider: &Provider) -> Option<GeminiThinkingConfig> {
    let extra_level = extra_string(&provider.extra, "thinking_level")
        .or_else(|| extra_string(&provider.extra, "thinkingLevel"));
    let extra_budget = extra_i32(&provider.extra, "thinking_budget")
        .or_else(|| extra_i32(&provider.extra, "thinkingBudget"));
    if extra_level.is_some() || extra_budget.is_some() {
        return Some(GeminiThinkingConfig {
            thinking_level: extra_level,
            thinking_budget: extra_budget,
        });
    }
    if let Some(level) = configured_reasoning_effort(provider) {
        return Some(GeminiThinkingConfig {
            thinking_level: Some(normalize_gemini_thinking_level(&level)),
            thinking_budget: None,
        });
    }
    default_gemini_thinking(&provider.model)
}

fn default_gemini_thinking(model: &str) -> Option<GeminiThinkingConfig> {
    let model = model.to_ascii_lowercase();
    if model.contains("2.5") {
        if model.contains("pro") {
            return Some(GeminiThinkingConfig {
                thinking_level: Some("LOW".into()),
                thinking_budget: None,
            });
        }
        return Some(GeminiThinkingConfig {
            thinking_level: None,
            thinking_budget: Some(0),
        });
    }
    // 3.5 Flash-Lite already defaults to no thinking; sending LOW can be slower.
    if model.contains("lite") {
        return None;
    }
    Some(GeminiThinkingConfig {
        thinking_level: Some("LOW".into()),
        thinking_budget: None,
    })
}

fn normalize_gemini_thinking_level(level: &str) -> String {
    match level.trim().to_ascii_lowercase().as_str() {
        "minimal" | "min" => "MINIMAL".into(),
        "low" => "LOW".into(),
        "medium" | "mid" => "MEDIUM".into(),
        "high" => "HIGH".into(),
        other => other.to_ascii_uppercase(),
    }
}

fn extra_string(
    extra: &std::collections::HashMap<String, toml::Value>,
    key: &str,
) -> Option<String> {
    extra.get(key).and_then(|v| v.as_str()).map(|s| s.to_string())
}

fn extra_i32(extra: &std::collections::HashMap<String, toml::Value>, key: &str) -> Option<i32> {
    extra.get(key).and_then(|v| v.as_integer()).and_then(|n| i32::try_from(n).ok())
}

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
    // file:<path> keeps the secret out of the config file itself.
    if let Some(path) = s.strip_prefix("file:") {
        return match std::fs::read_to_string(path) {
            Ok(v) => v.trim().to_string(),
            Err(e) => {
                eprintln!("Warning: could not read {}: {}", path, e);
                String::new()
            }
        };
    }
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
    let config: Config = toml::from_str(&text).unwrap_or_else(|e| {
        eprintln!("Failed to parse {}: {}", path, e);
        std::process::exit(1);
    });
    if let Err(e) = config.eval.validate() {
        eprintln!("{}: {}", path, e);
        std::process::exit(1);
    }
    config
}

fn load_cases(path: &str) -> Cases {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| {
        eprintln!("Failed to read {}: {}", path, e);
        eprintln!("Hint: copy test_cases.example.toml to test_cases.toml");
        std::process::exit(1);
    });
    parse_cases(&text).unwrap_or_else(|e| {
        eprintln!("Failed to parse {}: {}", path, e);
        eprintln!(
            "Hint: a case is `name`, `input` (what was heard) and `expected` (what was said); the sites to correct are derived from their difference. See test_cases.example.toml"
        );
        std::process::exit(1);
    })
}

fn parse_cases(text: &str) -> Result<Cases, String> {
    let cases: Cases = toml::from_str(text).map_err(|e| e.to_string())?;
    for case in &cases.case {
        if let Some(pin) = case.pin.iter().find(|p| !(0.0..=1.0).contains(&p.credit)) {
            return Err(format!(
                "case `{}`: pin {:?} → {:?} has credit {}, which must be between 0 and 1",
                case.name, pin.heard, pin.written, pin.credit
            ));
        }
    }
    Ok(cases)
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
    eprintln!("  --skip-judge       Do not call the judge; verdicts that need it are reported as unjudged");
    eprintln!("  -h, --help         Show this help");
}

// ── Evaluation ──────────────────────────────────────────────────────────────

fn format_credit(credit: f64) -> String {
    if (credit - credit.round()).abs() < 1e-9 {
        format!("{:.0}", credit)
    } else {
        format!("{:.2}", credit)
    }
}

/// "5.50/6": credit over judged items; "4/4*" when further items are unjudged.
fn format_tally(t: Tally) -> String {
    if t.total == 0 {
        return "-".into();
    }
    format!(
        "{}/{}{}",
        format_credit(t.credit),
        t.judged(),
        if t.unjudged > 0 { "*" } else { "" }
    )
}

/// What code alone settled for one model on one case, before the judge is asked.
fn format_by_code(t: Tallies) -> String {
    [("dictionary", t.dictionary), ("semantic", t.semantic), ("clean", t.clean)]
        .iter()
        .filter(|(_, tally)| tally.total > 0)
        .map(|(name, tally)| {
            let pending = if tally.unjudged > 0 {
                format!(" (+{} to judge)", tally.unjudged)
            } else {
                String::new()
            };
            format!("{} {}/{}{}", name, format_credit(tally.credit), tally.judged(), pending)
        })
        .collect::<Vec<_>>()
        .join("  ")
}

fn format_pct(rate: Option<f64>) -> String {
    rate.map(|c| format!("{:.0}%", c * 100.0))
        .unwrap_or_else(|| "-".into())
}

/// The same verdict usually serves many outputs: each distinct line once, with how many outputs it serves.
fn counted(lines: &[String]) -> Vec<(&str, usize)> {
    let mut out: Vec<(&str, usize)> = Vec::new();
    for line in lines {
        match out.iter_mut().find(|(seen, _)| *seen == line.as_str()) {
            Some((_, n)) => *n += 1,
            None => out.push((line, 1)),
        }
    }
    out
}

const DEFAULT_W_DICTIONARY: f64 = 0.40;
const DEFAULT_W_SEMANTIC: f64 = 0.15;
const DEFAULT_W_CLEAN: f64 = 0.10;
const DEFAULT_W_LATENCY: f64 = 0.35;
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

/// Weights as configured; they need not sum to 1.
#[derive(Clone, Copy)]
struct ScoreWeights {
    dictionary: f64,
    semantic: f64,
    clean: f64,
    latency: f64,
    success: f64,
}

fn score_weights(eval: &EvalConfig) -> ScoreWeights {
    let dictionary = eval.weight_dictionary.unwrap_or_else(|| match (eval.weight_basic, eval.weight_cloud) {
        (None, None) => DEFAULT_W_DICTIONARY,
        (basic, cloud) => basic.unwrap_or(0.0) + cloud.unwrap_or(0.0),
    });
    ScoreWeights {
        dictionary,
        semantic: eval.weight_semantic.unwrap_or(DEFAULT_W_SEMANTIC),
        clean: eval.weight_clean.unwrap_or(DEFAULT_W_CLEAN),
        latency: eval.weight_latency.unwrap_or(DEFAULT_W_LATENCY),
        success: eval.weight_success.unwrap_or(0.0),
    }
}

impl ScoreWeights {
    /// Out of 100: the weighted mean of the dimensions that have a rate. A tier without one (the cases have no
    /// such site, or none of this model's verdicts came back) is left out instead of scoring zero.
    fn composite(self, tiers: [Option<f64>; 3], latency: f64, success: f64) -> f64 {
        let parts = [
            (self.dictionary, tiers[0]),
            (self.semantic, tiers[1]),
            (self.clean, tiers[2]),
            (self.latency, Some(latency)),
            (self.success, Some(success)),
        ];
        let (mut sum, mut weight) = (0.0, 0.0);
        for (w, rate) in parts {
            if let Some(rate) = rate {
                sum += w * rate;
                weight += w;
            }
        }
        if weight > 0.0 {
            100.0 * sum / weight
        } else {
            0.0
        }
    }
}

fn print_ranking_legend(eval: &EvalConfig, stats: &[ProviderStats]) {
    let w = score_weights(eval);
    let exists = |tier: fn(&Tallies) -> Tally| stats.iter().any(|s| tier(&s.tallies).total > 0);
    let parts = [
        ("dictionary", w.dictionary, exists(|t| t.dictionary)),
        ("semantic", w.semantic, exists(|t| t.semantic)),
        ("clean", w.clean, exists(|t| t.clean)),
        ("speed", w.latency, true),
        ("success", w.success, w.success > 0.0),
    ];
    let sum: f64 = parts.iter().filter(|p| p.2).map(|p| p.1).sum();
    let shares: Vec<String> = parts
        .iter()
        .filter(|p| p.2 && sum > 0.0)
        .map(|(name, weight, _)| format!("{} {:.0}%", name, 100.0 * weight / sum))
        .collect();
    let full = eval.latency_full_ms.unwrap_or(DEFAULT_LATENCY_FULL_MS);
    let zero = eval.latency_zero_ms.unwrap_or(DEFAULT_LATENCY_ZERO_MS);
    println!("  \x1b[2mWeights: {}\x1b[0m", shares.join(" · "));
    println!(
        "  \x1b[2mDictionary: the dictionary term stands where it was said · Semantic: other wrong words fixed from context · Clean: nothing else damaged, rephrased or added\x1b[0m"
    );
    println!(
        "  \x1b[2mSpeed: 1.0 at ≤{:.0}ms, 0 at ≥{:.0}ms (not min-max across the field)\x1b[0m\n",
        full, zero
    );
}

fn rank_providers(stats: &[ProviderStats], eval: &EvalConfig) -> Vec<RankedProvider> {
    let w = score_weights(eval);
    let full_ms = eval.latency_full_ms.unwrap_or(DEFAULT_LATENCY_FULL_MS);
    let zero_ms = eval.latency_zero_ms.unwrap_or(DEFAULT_LATENCY_ZERO_MS);

    let mut ranked: Vec<RankedProvider> = stats
        .iter()
        .map(|s| {
            let dictionary_rate = s.tallies.dictionary.rate();
            let semantic_rate = s.tallies.semantic.rate();
            let clean_rate = s.tallies.clean.rate();
            let success_rate = if s.total_rounds > 0 {
                s.successes as f64 / s.total_rounds as f64
            } else {
                0.0
            };
            let lat_score = latency_score(s.avg_ms, s.successes, full_ms, zero_ms);
            RankedProvider {
                name: s.name.clone(),
                composite: w.composite([dictionary_rate, semantic_rate, clean_rate], lat_score, success_rate),
                dictionary_rate,
                semantic_rate,
                clean_rate,
                unjudged: s.tallies.unjudged(),
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
                dictionary_rate: r.dictionary_rate,
                semantic_rate: r.semantic_rate,
                clean_rate: r.clean_rate,
                quality: None,
                checkpoint_rate: None,
                basic_rate: None,
                cloud_rate: None,
                bonus_rate: None,
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

    fn tally(credit: f64, total: usize) -> Tally {
        Tally { credit, total, unjudged: 0 }
    }

    /// Three successful rounds out of three; each tier as (credit, items).
    fn stats(name: &str, avg_ms: u128, dictionary: (f64, usize), semantic: (f64, usize)) -> ProviderStats {
        ProviderStats {
            name: name.into(),
            total_rounds: 3,
            successes: 3,
            avg_ms,
            min_ms: avg_ms,
            max_ms: avg_ms,
            avg_tokens: None,
            tallies: Tallies {
                dictionary: tally(dictionary.0, dictionary.1),
                semantic: tally(semantic.0, semantic.1),
                clean: tally(3.0, 3),
            },
        }
    }

    #[test]
    fn gemini_thinking_defaults_to_lowest_supported() {
        assert_eq!(
            default_gemini_thinking("gemini-3.7-flash")
                .unwrap()
                .thinking_level
                .as_deref(),
            Some("LOW")
        );
        assert_eq!(
            default_gemini_thinking("gemini-3.8-flash")
                .unwrap()
                .thinking_level
                .as_deref(),
            Some("LOW")
        );
        assert!(default_gemini_thinking("gemini-3.5-flash-lite").is_none());
        assert_eq!(
            default_gemini_thinking("gemini-2.5-flash")
                .unwrap()
                .thinking_budget,
            Some(0)
        );
        assert_eq!(normalize_gemini_thinking_level("low"), "LOW");
        assert_eq!(normalize_gemini_thinking_level("minimal"), "MINIMAL");
    }

    fn bench_provider(
        provider_type: &str,
        reasoning: Option<&str>,
        extra: &[(&str, toml::Value)],
    ) -> Provider {
        Provider {
            name: "t".into(),
            base_url: "https://example.test/v1".into(),
            api_key: "k".into(),
            model: "m".into(),
            provider_type: provider_type.into(),
            api_mode: "completion".into(),
            reasoning_effort: reasoning.map(|s| s.into()),
            extra: extra
                .iter()
                .map(|(k, v)| ((*k).to_string(), v.clone()))
                .collect(),
        }
    }

    #[test]
    fn chat_body_uses_lowest_thinking_per_provider() {
        let volc = build_chat_request_body(&bench_provider("volcengine", None, &[]), "s".into(), "u".into());
        assert_eq!(volc["thinking"]["type"], "disabled");
        assert!(volc.get("reasoning_effort").is_none());
        assert!(volc.get("enable_thinking").is_none());

        let volc_low = build_chat_request_body(
            &bench_provider("volcengine", Some("low"), &[]),
            "s".into(),
            "u".into(),
        );
        assert_eq!(volc_low["reasoning_effort"], "low");
        assert!(volc_low.get("thinking").is_none());

        let openai_quiet =
            build_chat_request_body(&bench_provider("openai", None, &[]), "s".into(), "u".into());
        assert!(openai_quiet.get("reasoning_effort").is_none());

        let openai_min = build_chat_request_body(
            &bench_provider("openai", Some("minimal"), &[]),
            "s".into(),
            "u".into(),
        );
        assert_eq!(openai_min["reasoning_effort"], "minimal");

        let qwen = build_chat_request_body(&bench_provider("qwen", None, &[]), "s".into(), "u".into());
        assert_eq!(qwen["enable_thinking"], false);

        let qwen_override = build_chat_request_body(
            &bench_provider("qwen", None, &[("enable_thinking", toml::Value::Boolean(true))]),
            "s".into(),
            "u".into(),
        );
        assert_eq!(qwen_override["enable_thinking"], true);

        let custom_none = build_chat_request_body(
            &bench_provider("custom", Some("none"), &[]),
            "s".into(),
            "u".into(),
        );
        assert_eq!(custom_none["reasoning_effort"], "none");
    }

    #[test]
    fn responses_body_nests_reasoning_effort() {
        let quiet =
            build_responses_request_body(&bench_provider("openai", None, &[]), "s", "u");
        assert!(quiet.get("reasoning").is_none());

        let min = build_responses_request_body(
            &bench_provider("openai", Some("minimal"), &[]),
            "s",
            "u",
        );
        assert_eq!(min["reasoning"]["effort"], "minimal");

        let volc = build_responses_request_body(&bench_provider("volcengine", None, &[]), "s", "u");
        assert_eq!(volc["thinking"]["type"], "disabled");
        assert!(volc.get("reasoning").is_none());

        let qwen = build_responses_request_body(&bench_provider("qwen", None, &[]), "s", "u");
        assert_eq!(qwen["enable_thinking"], false);
    }

    #[test]
    fn rank_uses_sites_and_latency() {
        let ranked = rank_providers(
            &[
                stats("fast-wrong", 200, (0.0, 3), (0.0, 0)),
                stats("slow-correct", 2000, (3.0, 3), (0.0, 0)),
            ],
            &EvalConfig::default(),
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

        let ranked = rank_providers(
            &[
                stats("stall", 19000, (6.0, 6), (6.0, 6)),
                stats("mid", 2000, (6.0, 6), (6.0, 6)),
                stats("fast", 800, (6.0, 6), (6.0, 6)),
            ],
            &EvalConfig::default(),
        );
        assert_eq!(ranked[0].name, "fast");
        assert_eq!(ranked[1].name, "mid");
        assert_eq!(ranked[2].name, "stall");
        assert!((ranked[0].latency_score - 1.0).abs() < 1e-9);
        // A 19s outlier must not stretch the mid model up toward 1.0 the way min-max did.
        assert!((ranked[1].latency_score - 0.75).abs() < 1e-9);
        assert!(ranked[2].latency_score.abs() < 1e-9);
    }

    #[test]
    fn a_missed_dictionary_term_costs_more_than_a_missed_semantic_fix_and_speed_more_than_the_latter() {
        let ranked = rank_providers(
            &[
                stats("miss-dictionary", 800, (0.0, 6), (6.0, 6)),
                stats("slow", 4000, (6.0, 6), (6.0, 6)),
                stats("miss-semantic", 800, (6.0, 6), (0.0, 6)),
            ],
            &EvalConfig::default(),
        );
        let order: Vec<&str> = ranked.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(order, ["miss-semantic", "slow", "miss-dictionary"]);
    }

    #[test]
    fn legacy_weight_names_still_configure_the_tiers() {
        let legacy = "weight_basic = 0.25\nweight_cloud = 0.15\nweight_bonus = 0.15\nweight_quality = 0.1\nweight_latency = 0.35";
        let eval: EvalConfig = toml::from_str(legacy).unwrap();
        eval.validate().unwrap();
        let w = score_weights(&eval);
        let configured = [w.dictionary, w.semantic, w.clean, w.latency, w.success];
        for (got, want) in configured.iter().zip([0.40, 0.15, 0.10, 0.35, 0.0]) {
            assert!((got - want).abs() < 1e-9);
        }
        let d = score_weights(&EvalConfig::default());
        assert_eq!([d.dictionary, d.semantic, d.clean, d.latency, d.success], [0.40, 0.15, 0.10, 0.35, 0.0]);

        let both: EvalConfig = toml::from_str("weight_dictionary = 0.4\nweight_basic = 0.25").unwrap();
        assert!(both.validate().is_err());
        assert!(toml::from_str::<EvalConfig>("weight_checkpoints = 0.5").is_err(), "an unknown weight must not be dropped silently");
    }

    #[test]
    fn an_unjudged_item_is_left_out_not_counted_as_a_miss() {
        let mut semantic = Tally::default();
        semantic.add(Some(1.0));
        semantic.add(Some(0.5));
        semantic.add(None);
        assert_eq!((semantic.rate(), semantic.unjudged), (Some(0.75), 1));
        assert_eq!(format_tally(semantic), "1.50/2*");

        // nothing judged in a tier: that model's composite is taken over its other dimensions
        let mut open = stats("open", 800, (6.0, 6), (0.0, 0));
        open.tallies.semantic = Tally { credit: 0.0, total: 6, unjudged: 6 };
        let ranked = rank_providers(&[open, stats("no-semantic-sites", 800, (6.0, 6), (0.0, 0))], &EvalConfig::default());
        assert_eq!(ranked[0].semantic_rate, None);
        assert!((ranked[0].composite - ranked[1].composite).abs() < 1e-9);
        let marked: Vec<usize> = ranked.iter().map(|r| r.unjudged).collect();
        assert!(marked.contains(&6) && marked.contains(&0));
    }

    #[test]
    fn a_failed_call_scores_zero_on_every_site_and_on_the_transcript() {
        let reference = Reference::new("用 cloud 写下一集的导航", "用 Claude 写下一级的导航", &["Claude".to_string()]);
        let mut tallies = Tallies::default();
        tallies.add_failed(&reference);
        tallies.add(&adjudicate::score(&reference.analyze("用 Claude 写下一级的导航", &[]), &Answers::new()));
        for tier in [tallies.dictionary, tallies.semantic, tallies.clean] {
            assert_eq!((tier.rate(), tier.total, tier.unjudged), (Some(0.5), 2, 0));
        }
    }

    #[test]
    fn a_case_is_input_and_expected_with_optional_pins() {
        let case = "[[case]]\nname = \"n\"\ninput = \"下一集\"\nexpected = \"下一级\"\n";
        assert!(parse_cases(case).unwrap().case[0].pin.is_empty());
        let pinned = format!("{case}[[case.pin]]\nheard = \"下一集\"\nwritten = \"下一层\"\ncredit = 1.0\n");
        assert_eq!(parse_cases(&pinned).unwrap().case[0].pin[0].written, "下一层");
        assert!(parse_cases(&pinned.replace("1.0", "1.5")).unwrap_err().contains("between 0 and 1"));
        let old = format!("{case}[[case.checkpoint]]\ndescription = \"d\"\nmust_contain = \"下一级\"\n");
        assert!(parse_cases(&old).unwrap_err().contains("checkpoint"));
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
            dictionary_rate: Some(1.0),
            semantic_rate: None,
            clean_rate: Some(1.0),
            unjudged: 0,
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

    #[test]
    fn races_scored_before_the_tiers_still_load_and_keep_their_fields() {
        let old = r#"{"place":1,"provider":"a","points":3.0,"composite":71.5,"quality":8.2,"checkpoint_rate":0.9,"basic_rate":1.0,"cloud_rate":0.5,"bonus_rate":null,"latency_score":0.8,"avg_ms":900,"success_rate":1.0}"#;
        let place: RunPlace = serde_json::from_str(old).unwrap();
        let saved = serde_json::to_value(&place).unwrap();
        assert_eq!((saved["quality"].as_f64(), saved["cloud_rate"].as_f64()), (Some(8.2), Some(0.5)));
        assert!(saved.get("dictionary_rate").is_none());

        let new = serde_json::to_value(&award_points(&[rp("a", 90.0)], StandingScheme::Borda)[0]).unwrap();
        assert_eq!(new["dictionary_rate"].as_f64(), Some(1.0));
        assert!(new.get("quality").is_none() && new.get("semantic_rate").is_none());
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
