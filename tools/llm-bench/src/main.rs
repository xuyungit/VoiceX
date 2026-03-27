use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Instant;

// ── Config ──────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct Config {
    rounds: Option<usize>,
    prompt: Option<String>,
    dictionary: Option<String>,
    provider: Vec<Provider>,
}

#[derive(Deserialize, Clone)]
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

#[derive(Deserialize)]
struct TestCase {
    name: String,
    input: String,
    expected: Option<String>,
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

    let config = load_config(&config_path);
    let cases = load_cases(&cases_path);

    let rounds = rounds_override.or(config.rounds).unwrap_or(3);
    let prompt = config.prompt.unwrap_or_else(default_prompt);
    let dictionary = config.dictionary.unwrap_or_default();

    let providers: Vec<Provider> = config
        .provider
        .into_iter()
        .map(|mut p| {
            p.api_key = resolve_env(&p.api_key);
            p.base_url = resolve_env(&p.base_url);
            p
        })
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
    println!();

    let http = Client::new();
    let mut all_stats: Vec<(String, Vec<ProviderStats>)> = Vec::new();
    let mut json_results: Vec<serde_json::Value> = Vec::new();

    for case in &cases.case {
        println!("\x1b[1;36m━━━ {} ━━━\x1b[0m", case.name);
        println!("\x1b[2mInput:\x1b[0m    {}", case.input);
        if let Some(ref exp) = case.expected {
            println!("\x1b[2mExpected:\x1b[0m {}", exp);
        }
        println!();

        let mut case_stats: Vec<ProviderStats> = Vec::new();

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
                "rounds": round_details,
            }));

            case_stats.push(stats);
        }

        all_stats.push((case.name.clone(), case_stats));
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
        "  {:<width$}  {:>8}  {:>8}  {:>8}  {:>8}  {:>10}",
        "Provider",
        "Avg ms",
        "Min ms",
        "Max ms",
        "Tokens",
        "Success",
        width = name_width
    );
    println!(
        "  {:<width$}  {:>8}  {:>8}  {:>8}  {:>8}  {:>10}",
        "─".repeat(name_width),
        "────────",
        "────────",
        "────────",
        "────────",
        "──────────",
        width = name_width
    );

    for pname in &provider_names {
        let mut total_avg = 0u128;
        let mut total_min = u128::MAX;
        let mut total_max = 0u128;
        let mut total_tokens = 0u32;
        let mut token_count = 0u32;
        let mut total_success = 0usize;
        let mut total_rounds = 0usize;
        let mut case_count = 0u128;

        for (_, stats) in &all_stats {
            if let Some(s) = stats.iter().find(|s| &s.name == pname) {
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
            "  {:<width$}  {:>8}  {:>8}  {:>8}  {:>8}  {:>5}/{:<4}",
            pname,
            avg,
            min,
            total_max,
            tokens_str,
            total_success,
            total_rounds,
            width = name_width
        );
    }
    println!();

    // Write JSON output if requested
    if let Some(path) = output_path {
        let json = serde_json::to_string_pretty(&json_results).unwrap();
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

    if provider.provider_type == "gemini" {
        return run_once_gemini(http, provider, &system_prompt, input).await;
    }

    if provider.api_mode == "response" {
        return run_once_response(http, provider, &system_prompt, input).await;
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
                content: format!("原文：\n{}", input),
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
        }
        _ => {
            request.temperature = Some(0.2);
            request.max_tokens = Some(4096);
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
    input: &str,
) -> RoundResult {
    let mut request = ResponseApiRequest {
        model: provider.model.clone(),
        input: serde_json::json!(format!("原文：\n{}", input)),
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
    input: &str,
) -> RoundResult {
    let request = GeminiRequest {
        system_instruction: Some(GeminiContent {
            role: None,
            parts: vec![GeminiPart { text: system_prompt.to_string() }],
        }),
        contents: vec![GeminiContent {
            role: Some("user".into()),
            parts: vec![GeminiPart { text: format!("原文：\n{}", input) }],
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
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max])
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
    eprintln!("  -h, --help         Show this help");
}
