//! The lowest reasoning each endpoint accepts.
//!
//! VoiceX corrects and translates short dictated text, where thinking buys
//! nothing and costs seconds (no knob at all: 10 s on api.deepseek.com, 70 s
//! on DashScope). There is no portable "off" — the same value is the floor on
//! one model, ignored by the next and a 400 on a third — so every rule here
//! was read off `usage.completion_tokens_details.reasoning_tokens` against the
//! live API. The probes are written up in `docs/llm-reasoning-knobs-2026-09-20.md`,
//! and `tools/llm-bench` sends the same knobs.

use super::config::{LLMApiMode, LLMConfig, LLMProviderType};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

/// Settings spelling of [`ReasoningChoice::ServerDefault`].
const SERVER_DEFAULT: &str = "server_default";

/// What the LLM page asks for.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningChoice {
    /// The lowest reasoning known for this endpoint and model.
    #[default]
    Lowest,
    /// No reasoning field at all; the server default applies.
    ServerDefault,
    /// This `reasoning_effort`, as chosen.
    Effort(String),
}

impl ReasoningChoice {
    /// A blank setting is `Lowest`, so an endpoint nobody tuned is already quiet.
    pub fn from_setting(value: Option<&str>) -> Self {
        match value.map(str::trim).unwrap_or_default() {
            "" => Self::Lowest,
            SERVER_DEFAULT => Self::ServerDefault,
            effort => Self::Effort(effort.to_string()),
        }
    }
}

/// How one endpoint spells "think as little as you can".
#[derive(Debug, Clone, PartialEq)]
pub enum LowestKnob {
    /// `reasoning_effort` on chat completions, `reasoning.effort` on Responses.
    Effort(&'static str),
    /// Vendor fields. Gemini's belong in `generation_config`, the rest at the
    /// top level of the body.
    Fields(Map<String, Value>),
    /// The model does not reason and rejects the field.
    NotNeeded,
}

/// Where the reasoning fields of a request came from.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningSource {
    /// `Lowest`, resolved for this endpoint and model.
    Lowest,
    /// `Lowest`, and the model does not reason: nothing to send.
    NotNeeded,
    /// `Lowest` asked of an endpoint no rule covers: nothing is sent, so the
    /// server default applies until the user picks a value or fills in the
    /// extra request fields.
    Unknown,
    /// A value the user chose, or their choice to send nothing.
    Chosen,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReasoningPlan {
    pub fields: Map<String, Value>,
    pub source: ReasoningSource,
}

/// The reasoning fields `config` puts on the wire.
pub fn plan(config: &LLMConfig) -> ReasoningPlan {
    // The Qwen and Gemini pages offer no choice.
    let choice = match config.provider_type {
        LLMProviderType::Qwen | LLMProviderType::Gemini => &ReasoningChoice::Lowest,
        _ => &config.reasoning,
    };
    match choice {
        ReasoningChoice::ServerDefault => ReasoningPlan {
            fields: Map::new(),
            source: ReasoningSource::Chosen,
        },
        ReasoningChoice::Effort(effort) => ReasoningPlan {
            fields: effort_fields(effort, &config.api_mode),
            source: ReasoningSource::Chosen,
        },
        ReasoningChoice::Lowest => {
            match lowest_knob(&config.provider_type, &config.base_url, &config.model_name) {
                Some(LowestKnob::Effort(effort)) => ReasoningPlan {
                    fields: effort_fields(effort, &config.api_mode),
                    source: ReasoningSource::Lowest,
                },
                Some(LowestKnob::Fields(fields)) => ReasoningPlan {
                    fields,
                    source: ReasoningSource::Lowest,
                },
                Some(LowestKnob::NotNeeded) => ReasoningPlan {
                    fields: Map::new(),
                    source: ReasoningSource::NotNeeded,
                },
                None => ReasoningPlan {
                    fields: Map::new(),
                    source: ReasoningSource::Unknown,
                },
            }
        }
    }
}

fn effort_fields(effort: &str, api_mode: &LLMApiMode) -> Map<String, Value> {
    let mut fields = Map::new();
    match api_mode {
        LLMApiMode::ChatCompletions => {
            fields.insert("reasoning_effort".to_string(), effort.into());
        }
        LLMApiMode::Responses => {
            fields.insert("reasoning".to_string(), json!({ "effort": effort }));
        }
    }
    fields
}

/// `None` when no rule covers the endpoint.
pub fn lowest_knob(
    provider_type: &LLMProviderType,
    base_url: &str,
    model: &str,
) -> Option<LowestKnob> {
    let model = model.trim().to_ascii_lowercase();
    match provider_type {
        LLMProviderType::Volcengine => Some(thinking_disabled()),
        LLMProviderType::Openai => openai_knob(&model),
        LLMProviderType::Qwen => Some(enable_thinking_false()),
        LLMProviderType::Gemini => Some(gemini_knob(&model)),
        LLMProviderType::Custom => custom_knob(&host_of(base_url)?, &model),
    }
}

fn host_of(base_url: &str) -> Option<String> {
    reqwest::Url::parse(base_url.trim())
        .ok()?
        .host_str()
        .map(str::to_ascii_lowercase)
}

fn custom_knob(host: &str, model: &str) -> Option<LowestKnob> {
    // gpt-oss's chat template knows low/medium/high wherever it is hosted;
    // Cerebras answers `none` with a 400.
    if model.contains("gpt-oss") {
        return Some(LowestKnob::Effort("low"));
    }
    let under = |domain: &str| {
        host == domain
            || host
                .strip_suffix(domain)
                .is_some_and(|rest| rest.ends_with('.'))
    };
    if under("volces.com") || under("deepseek.com") || under("xiaomimimo.com") {
        Some(thinking_disabled())
    } else if under("aliyuncs.com") {
        Some(enable_thinking_false())
    } else if under("cerebras.ai") {
        Some(LowestKnob::Effort("none"))
    } else if under("bigmodel.cn") {
        // GLM 5.3 Flash answers `thinking: disabled` with "该模型始终思考";
        // `low` is what brings it to 0 reasoning tokens.
        Some(LowestKnob::Effort("low"))
    } else if under("openai.com") {
        openai_knob(model)
    } else {
        None
    }
}

/// Ark, DeepSeek and MiMo share this spelling. On Ark it is the only one that
/// holds for every model: `reasoning_effort: minimal` quiets Doubao Seed 2.x
/// but not deepseek-v4-1-flash, and doubao-seed-1-6-flash ignores `none` too.
fn thinking_disabled() -> LowestKnob {
    let mut fields = Map::new();
    fields.insert("thinking".to_string(), json!({ "type": "disabled" }));
    LowestKnob::Fields(fields)
}

fn enable_thinking_false() -> LowestKnob {
    let mut fields = Map::new();
    fields.insert("enable_thinking".to_string(), Value::Bool(false));
    LowestKnob::Fields(fields)
}

/// OpenAI's floor moved with the model line: the o-series stops at `low`,
/// gpt-5 at `minimal`, gpt-5.1 and later take `none` (and reject `minimal`),
/// and models before gpt-5 reject the field outright.
fn openai_knob(model: &str) -> Option<LowestKnob> {
    if let Some(version) = model.strip_prefix("gpt-") {
        let numeric: String = version
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        let mut parts = numeric.split('.');
        let major: u32 = parts.next()?.parse().ok()?;
        let minor: u32 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
        return Some(if (major, minor) >= (5, 1) {
            LowestKnob::Effort("none")
        } else if major == 5 {
            LowestKnob::Effort("minimal")
        } else {
            LowestKnob::NotNeeded
        });
    }
    let mut chars = model.chars();
    if chars.next() == Some('o') && chars.next().is_some_and(|c| c.is_ascii_digit()) {
        return Some(LowestKnob::Effort("low"));
    }
    None
}

/// 3.7/3.8 Flash reject MINIMAL; 2.5 Flash can set thinkingBudget=0;
/// Flash-Lite already has thinking off by default.
fn gemini_knob(model: &str) -> LowestKnob {
    let thinking_config = if model.contains("2.5") {
        if model.contains("pro") {
            json!({ "thinkingLevel": "LOW" })
        } else {
            json!({ "thinkingBudget": 0 })
        }
    } else if model.contains("lite") {
        return LowestKnob::NotNeeded;
    } else {
        json!({ "thinkingLevel": "LOW" })
    };
    let mut fields = Map::new();
    fields.insert("thinkingConfig".to_string(), thinking_config);
    LowestKnob::Fields(fields)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn custom(base_url: &str, model: &str) -> Option<LowestKnob> {
        lowest_knob(&LLMProviderType::Custom, base_url, model)
    }

    fn fields(value: Value) -> LowestKnob {
        LowestKnob::Fields(value.as_object().cloned().unwrap())
    }

    #[test]
    fn a_blank_setting_asks_for_the_lowest() {
        assert_eq!(ReasoningChoice::from_setting(None), ReasoningChoice::Lowest);
        assert_eq!(ReasoningChoice::from_setting(Some("  ")), ReasoningChoice::Lowest);
        assert_eq!(
            ReasoningChoice::from_setting(Some("server_default")),
            ReasoningChoice::ServerDefault
        );
        assert_eq!(
            ReasoningChoice::from_setting(Some(" low ")),
            ReasoningChoice::Effort("low".to_string())
        );
    }

    #[test]
    fn custom_endpoints_are_recognized_by_host() {
        let thinking_off = fields(json!({ "thinking": { "type": "disabled" } }));
        for base_url in [
            "https://ark.cn-beijing.volces.com/api/v3",
            "https://api.deepseek.com",
            "https://api.xiaomimimo.com/v1",
        ] {
            assert_eq!(custom(base_url, "m"), Some(thinking_off.clone()), "{base_url}");
        }
        assert_eq!(
            custom("https://dashscope.aliyuncs.com/compatible-mode/v1", "qwen3.8-flash"),
            Some(fields(json!({ "enable_thinking": false })))
        );
        assert_eq!(
            custom("https://open.bigmodel.cn/api/paas/v4", "glm-5.3-flash"),
            Some(LowestKnob::Effort("low"))
        );
        assert_eq!(custom("http://localhost:11434/v1", "qwen3"), None);
        assert_eq!(custom("https://notdeepseek.com/v1", "m"), None);
        assert_eq!(custom("not a url", "m"), None);
    }

    #[test]
    fn gpt_oss_stops_at_low_even_where_the_host_takes_none() {
        assert_eq!(
            custom("https://api.cerebras.ai/v1", "qwen-3.8-27b"),
            Some(LowestKnob::Effort("none"))
        );
        assert_eq!(
            custom("https://api.cerebras.ai/v1", "gpt-oss-120b"),
            Some(LowestKnob::Effort("low"))
        );
    }

    #[test]
    fn openai_floor_follows_the_model_line() {
        let openai = |model| lowest_knob(&LLMProviderType::Openai, "https://api.openai.com/v1", model);
        assert_eq!(openai("gpt-5.6-luna"), Some(LowestKnob::Effort("none")));
        assert_eq!(openai("gpt-5.4-mini"), Some(LowestKnob::Effort("none")));
        assert_eq!(openai("gpt-5-mini"), Some(LowestKnob::Effort("minimal")));
        assert_eq!(openai("o4-mini"), Some(LowestKnob::Effort("low")));
        assert_eq!(openai("gpt-4.1-mini"), Some(LowestKnob::NotNeeded));
        assert_eq!(openai("gpt-4o"), Some(LowestKnob::NotNeeded));
        assert_eq!(openai("some-proxy-model"), None);
    }

    #[test]
    fn gemini_uses_the_lowest_level_each_model_accepts() {
        let gemini = |model| lowest_knob(&LLMProviderType::Gemini, "", model);
        let low = fields(json!({ "thinkingConfig": { "thinkingLevel": "LOW" } }));
        assert_eq!(gemini("gemini-3.7-flash"), Some(low.clone()));
        assert_eq!(gemini("gemini-3.8-flash"), Some(low.clone()));
        assert_eq!(gemini("gemini-2.5-pro"), Some(low));
        assert_eq!(
            gemini("gemini-2.5-flash"),
            Some(fields(json!({ "thinkingConfig": { "thinkingBudget": 0 } })))
        );
        assert_eq!(gemini("gemini-3.5-flash-lite"), Some(LowestKnob::NotNeeded));
    }

    fn config(provider_type: LLMProviderType, base_url: &str, reasoning: ReasoningChoice) -> LLMConfig {
        LLMConfig {
            provider_type,
            base_url: base_url.to_string(),
            api_key: "k".to_string(),
            model_name: "m".to_string(),
            api_mode: LLMApiMode::ChatCompletions,
            reasoning,
            extra_body: None,
        }
    }

    #[test]
    fn an_unknown_endpoint_sends_nothing_and_says_so() {
        let unknown = plan(&config(
            LLMProviderType::Custom,
            "http://localhost:1/v1",
            ReasoningChoice::Lowest,
        ));
        assert!(unknown.fields.is_empty());
        assert_eq!(unknown.source, ReasoningSource::Unknown);

        let silent = plan(&config(
            LLMProviderType::Custom,
            "https://api.deepseek.com",
            ReasoningChoice::ServerDefault,
        ));
        assert!(silent.fields.is_empty());
        assert_eq!(silent.source, ReasoningSource::Chosen);
    }

    #[test]
    fn an_effort_is_nested_on_the_responses_api() {
        let mut responses = config(
            LLMProviderType::Custom,
            "https://api.openai.com/v1",
            ReasoningChoice::Lowest,
        );
        responses.model_name = "gpt-5.6-luna".to_string();
        responses.api_mode = LLMApiMode::Responses;
        assert_eq!(
            Value::Object(plan(&responses).fields),
            json!({ "reasoning": { "effort": "none" } })
        );

        responses.reasoning = ReasoningChoice::Effort("high".to_string());
        let chosen = plan(&responses);
        assert_eq!(Value::Object(chosen.fields), json!({ "reasoning": { "effort": "high" } }));
        assert_eq!(chosen.source, ReasoningSource::Chosen);
    }

    #[test]
    fn qwen_and_gemini_stay_at_the_lowest_whatever_the_config_says() {
        let qwen = plan(&config(
            LLMProviderType::Qwen,
            "https://dashscope.aliyuncs.com/compatible-mode/v1",
            ReasoningChoice::Effort("high".to_string()),
        ));
        assert_eq!(Value::Object(qwen.fields), json!({ "enable_thinking": false }));
        assert_eq!(qwen.source, ReasoningSource::Lowest);
    }
}
