//! Gemini generateContent parameter policy shared by ASR, LLM and llm-bench.
//! Verified 2026-10-07 against:
//! https://ai.google.dev/gemini-api/docs/generate-content/thinking
//! https://ai.google.dev/gemini-api/docs/whats-new-gemini-3.5
//! https://ai.google.dev/gemini-api/docs/latest-model

use serde::Serialize;

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub(crate) struct ThinkingConfig {
    #[serde(rename = "thinkingLevel", skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<String>,
    #[serde(rename = "thinkingBudget", skip_serializing_if = "Option::is_none")]
    pub thinking_budget: Option<i32>,
}

enum Support {
    Budget {
        min: i32,
        max: i32,
        off: bool,
        default_off: bool,
    },
    Levels {
        minimal: bool,
        default_minimal: bool,
        xhigh: bool,
    },
    NoThinking,
    Unknown,
}

fn normalized_model(model: &str) -> String {
    model
        .trim()
        .strip_prefix("models/")
        .unwrap_or(model.trim())
        .to_ascii_lowercase()
}

fn family(model: &str, name: &str) -> bool {
    if model == name {
        return true;
    }
    let Some(suffix) = model
        .strip_prefix(name)
        .and_then(|rest| rest.strip_prefix('-'))
    else {
        return false;
    };
    // Accept documented version forms, not a different subtype such as a
    // future Flash-Lite model that happens to start with a known Flash ID.
    suffix == "preview"
        || suffix.starts_with("preview-")
        || suffix == "exp"
        || suffix.starts_with("exp-")
        || (!suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit() || c == '-'))
}

fn support(model: &str) -> Support {
    let model = normalized_model(model);
    // Specialized endpoints have their own controls; do not infer them from
    // a shared Flash / Pro substring.
    if ["-image", "-tts", "-live", "-transcribe", "-thinking"]
        .iter()
        .any(|suffix| model.contains(suffix))
    {
        return Support::Unknown;
    }
    if family(&model, "gemini-2.5-pro") {
        Support::Budget {
            min: 128,
            max: 32768,
            off: false,
            default_off: false,
        }
    } else if family(&model, "gemini-2.5-flash-lite") {
        Support::Budget {
            min: 512,
            max: 24576,
            off: true,
            default_off: true,
        }
    } else if family(&model, "gemini-2.5-flash") {
        Support::Budget {
            min: 0,
            max: 24576,
            off: true,
            default_off: false,
        }
    } else if ["gemini-3.1-flash-lite", "gemini-3.5-flash-lite"]
        .iter()
        .any(|name| family(&model, name))
    {
        Support::Levels {
            minimal: true,
            default_minimal: true,
            xhigh: false,
        }
    } else if ["gemini-3-flash", "gemini-3.5-flash", "gemini-3.6-flash"]
        .iter()
        .any(|name| family(&model, name))
    {
        Support::Levels {
            minimal: true,
            default_minimal: false,
            xhigh: false,
        }
    } else if ["gemini-3.1-pro", "gemini-3.7-flash", "gemini-3.8-flash"]
        .iter()
        .any(|name| family(&model, name))
    {
        Support::Levels {
            minimal: false,
            default_minimal: false,
            xhigh: false,
        }
    } else if ["gemini-4-hydrogen", "gemini-4-neon"]
        .iter()
        .any(|name| family(&model, name))
    {
        Support::Levels {
            minimal: false,
            default_minimal: false,
            xhigh: true,
        }
    } else if ["gemini-4-carbon", "gemini-4-argon"]
        .iter()
        .any(|name| family(&model, name))
    {
        Support::Levels {
            minimal: false,
            default_minimal: false,
            xhigh: false,
        }
    } else if [
        "gemini-1.5-pro",
        "gemini-1.5-flash",
        "gemini-2.0-flash",
        "gemini-2.0-flash-lite",
    ]
    .iter()
    .any(|name| family(&model, name))
    {
        Support::NoThinking
    } else {
        Support::Unknown
    }
}

#[allow(dead_code)] // Used by the app reasoning preview.
pub(crate) fn is_known_model(model: &str) -> bool {
    !matches!(support(model), Support::Unknown)
}

/// Supported UI reasoning-effort choices for a recognized Gemini model.
pub(crate) fn supported_efforts(model: &str) -> Vec<&'static str> {
    match support(model) {
        Support::Budget { off: true, .. } => vec!["none", "low", "medium", "high"],
        Support::Budget { off: false, .. } => vec!["low", "medium", "high"],
        Support::Levels {
            minimal: true,
            xhigh: false,
            ..
        } => vec!["minimal", "low", "medium", "high"],
        Support::Levels {
            minimal: false,
            xhigh: true,
            ..
        } => vec!["low", "medium", "high", "xhigh"],
        Support::Levels {
            minimal: true,
            xhigh: true,
            ..
        } => vec!["minimal", "low", "medium", "high", "xhigh"],
        Support::Levels {
            minimal: false,
            xhigh: false,
            ..
        } => vec!["low", "medium", "high"],
        Support::NoThinking | Support::Unknown => Vec::new(),
    }
}

/// Map a user-selected logical effort (`"none"`, `"minimal"`, `"low"`, `"medium"`,
/// `"high"`, `"xhigh"`) into the model's `thinkingConfig`. Returns `None` when
/// the model does not support explicit control or the effort is incompatible with it.
pub(crate) fn thinking_config_for_effort(model: &str, effort: &str) -> Option<ThinkingConfig> {
    let effort = effort.trim().to_ascii_lowercase();
    match support(model) {
        Support::Budget { min, max, off, .. } => {
            let budget = match effort.as_str() {
                "none" if off => 0,
                "low" => (if off { min.max(1024) } else { min }).clamp(min, max),
                "medium" => 8192_i32.clamp(min, max),
                "high" => 24576_i32.clamp(min, max),
                _ => return None,
            };
            Some(ThinkingConfig {
                thinking_budget: Some(budget),
                ..Default::default()
            })
        }
        Support::Levels { minimal, xhigh, .. } => {
            let level = match effort.as_str() {
                "minimal" if minimal => "MINIMAL",
                "low" => "LOW",
                "medium" => "MEDIUM",
                "high" => "HIGH",
                "xhigh" if xhigh => "XHIGH",
                _ => return None,
            };
            Some(ThinkingConfig {
                thinking_level: Some(level.to_string()),
                ..Default::default()
            })
        }
        Support::NoThinking | Support::Unknown => None,
    }
}

/// Filter official `models.list` entries to Gemini text models suitable for
/// dictation polishing and correction (excluding specialized modalities,
/// legacy 1.x generations, experimental snapshots, and dated/numbered builds).
pub(crate) fn is_suitable_polishing_model(id: &str) -> bool {
    let model = normalized_model(id);
    if !model.starts_with("gemini-") {
        return false;
    }
    if [
        "-image",
        "-tts",
        "-live",
        "-transcribe",
        "-thinking",
        "embedding",
        "aqa",
        "deep-research",
        "computer-use",
        "robotics",
        "nano-banana",
        "-customtools",
        "-audio",
        "-video",
        "-vision",
    ]
    .iter()
    .any(|kw| model.contains(kw))
    {
        return false;
    }
    if model == "gemini-pro"
        || model.starts_with("gemini-1.")
        || model.starts_with("gemini-ultra")
        || model.starts_with("gemini-exp")
        || model.ends_with("-exp")
        || model.contains("-exp-")
    {
        return false;
    }
    if model
        .rsplit('-')
        .next()
        .is_some_and(|seg| !seg.is_empty() && seg.bytes().all(|b| b.is_ascii_digit()))
    {
        return false;
    }
    true
}

/// Retain VoiceX's sampling only for recognized pre-3 models. Gemini 3.x
/// should use server defaults; future models and aliases are not guessed.
pub(crate) fn temperature(model: &str) -> Option<f64> {
    match support(model) {
        Support::Budget { .. } | Support::NoThinking => Some(0.2),
        _ => None,
    }
}

pub(crate) fn lowest_thinking_config(model: &str) -> Option<ThinkingConfig> {
    match support(model) {
        // Flash-Lite defaults to its minimum already. Omit the control.
        Support::Budget {
            default_off: true, ..
        }
        | Support::Levels {
            default_minimal: true,
            ..
        }
        | Support::NoThinking
        | Support::Unknown => None,
        Support::Budget { min, off, .. } => Some(ThinkingConfig {
            thinking_budget: Some(if off { 0 } else { min }),
            ..Default::default()
        }),
        Support::Levels { minimal, .. } => Some(ThinkingConfig {
            thinking_level: Some(if minimal { "MINIMAL" } else { "LOW" }.into()),
            ..Default::default()
        }),
    }
}

/// Explicit benchmark choices must be compatible, never silently replaced.
#[allow(dead_code)] // Used by llm-bench; the app offers only automatic controls.
pub(crate) fn validate_thinking_config(model: &str, config: &ThinkingConfig) -> Result<(), String> {
    if config.thinking_level.is_some() && config.thinking_budget.is_some() {
        return Err("Use only one of thinking_level and thinking_budget".into());
    }
    if config.thinking_level.is_none() && config.thinking_budget.is_none() {
        return Ok(());
    }
    let valid = match support(model) {
        Support::Budget { min, max, off, .. } => config.thinking_budget.is_some_and(|budget| {
            budget == -1 || (off && budget == 0) || (min..=max).contains(&budget)
        }),
        Support::Levels { minimal, xhigh, .. } => config.thinking_level.as_deref().is_some_and(|level| {
            matches!(level, "LOW" | "MEDIUM" | "HIGH")
                || (minimal && level == "MINIMAL")
                || (xhigh && level == "XHIGH")
        }),
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(format!(
            "Unsupported thinking configuration for {model}: {config:?}"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn old_and_new_models_use_only_their_supported_control() {
        for (model, expected) in [
            ("gemini-2.5-pro", json!({"thinkingBudget":128})),
            (
                "gemini-2.5-pro-preview-03-25",
                json!({"thinkingBudget":128}),
            ),
            ("gemini-2.5-flash", json!({"thinkingBudget":0})),
            (
                "gemini-2.5-flash-preview-09-2025",
                json!({"thinkingBudget":0}),
            ),
            ("gemini-3-flash-preview", json!({"thinkingLevel":"MINIMAL"})),
            ("gemini-3.5-flash", json!({"thinkingLevel":"MINIMAL"})),
            ("gemini-3.6-flash", json!({"thinkingLevel":"MINIMAL"})),
            ("gemini-3.7-flash", json!({"thinkingLevel":"LOW"})),
            ("gemini-3.8-flash", json!({"thinkingLevel":"LOW"})),
            ("gemini-3.1-pro-preview", json!({"thinkingLevel":"LOW"})),
            (" models/gemini-3.8-flash ", json!({"thinkingLevel":"LOW"})),
        ] {
            let config = lowest_thinking_config(model).unwrap();
            assert!(validate_thinking_config(model, &config).is_ok(), "{model}");
            assert_eq!(serde_json::to_value(config).unwrap(), expected, "{model}");
        }
    }

    #[test]
    fn default_minimal_legacy_and_unknown_models_omit_thinking() {
        for model in [
            "gemini-2.5-flash-lite",
            "gemini-3.1-flash-lite-preview",
            "gemini-3.5-flash-lite",
            "gemini-2.0-flash",
            "gemini-1.5-pro",
            "gemini-4-flash",
            "gemini-3.8-flash-lite",
            "gemini-3.5-flash-custom",
            "gemini-flash-latest",
            "my-2.5-pro",
            "",
            "gemini-3.5-transcribe",
            "gemini-3.1-flash-lite-image",
            "gemini-3.1-flash-live-preview",
        ] {
            assert!(lowest_thinking_config(model).is_none(), "{model}");
        }
        assert!(is_known_model("gemini-3.5-flash-lite"));
        assert!(!is_known_model("gemini-4-flash"));
    }

    #[test]
    fn sampling_is_omitted_for_gemini_three_and_unknown_models() {
        for model in [
            "gemini-3.5-flash-lite",
            "gemini-3.8-flash",
            "gemini-3-flash-preview",
            "gemini-4-flash",
            "gemini-flash-latest",
        ] {
            assert_eq!(temperature(model), None, "{model}");
        }
        assert_eq!(temperature("gemini-2.5-pro"), Some(0.2));
        assert_eq!(temperature("gemini-2.0-flash"), Some(0.2));
    }

    #[test]
    fn explicit_budgets_obey_model_ranges() {
        for (model, valid, invalid) in [
            (
                "gemini-2.5-pro",
                vec![-1, 128, 32768],
                vec![-2, 0, 127, 32769],
            ),
            ("gemini-2.5-flash", vec![-1, 0, 1, 24576], vec![-2, 24577]),
            (
                "gemini-2.5-flash-lite",
                vec![-1, 0, 512, 24576],
                vec![-2, 1, 511, 24577],
            ),
        ] {
            for budget in valid {
                assert!(
                    validate_thinking_config(
                        model,
                        &ThinkingConfig {
                            thinking_budget: Some(budget),
                            ..Default::default()
                        }
                    )
                    .is_ok(),
                    "{model} {budget}"
                );
            }
            for budget in invalid {
                assert!(
                    validate_thinking_config(
                        model,
                        &ThinkingConfig {
                            thinking_budget: Some(budget),
                            ..Default::default()
                        }
                    )
                    .is_err(),
                    "{model} {budget}"
                );
            }
        }
    }

    #[test]
    fn explicit_choices_reject_wrong_control_conflicts_and_unsupported_levels() {
        let level = |value: &str| ThinkingConfig {
            thinking_level: Some(value.into()),
            ..Default::default()
        };
        for model in ["gemini-3.7-flash", "gemini-3.8-flash", "gemini-3.1-pro"] {
            assert!(validate_thinking_config(model, &level("MINIMAL")).is_err());
            for value in ["LOW", "MEDIUM", "HIGH"] {
                assert!(validate_thinking_config(model, &level(value)).is_ok());
            }
        }
        for model in [
            "gemini-3-flash-preview",
            "gemini-3.5-flash",
            "gemini-3.5-flash-lite",
        ] {
            assert!(validate_thinking_config(model, &level("MINIMAL")).is_ok());
        }
        for model in ["gemini-2.5-pro", "gemini-2.0-flash", "gemini-4-flash"] {
            assert!(validate_thinking_config(model, &level("LOW")).is_err());
        }
        assert!(validate_thinking_config("gemini-3.8-flash", &level("NONE")).is_err());
        assert!(validate_thinking_config(
            "gemini-3.8-flash",
            &ThinkingConfig {
                thinking_budget: Some(0),
                ..Default::default()
            }
        )
        .is_err());
        assert!(validate_thinking_config(
            "gemini-2.5-flash",
            &ThinkingConfig {
                thinking_level: Some("LOW".into()),
                thinking_budget: Some(0)
            }
        )
        .is_err());
        assert!(validate_thinking_config("gemini-4-flash", &ThinkingConfig::default()).is_ok());
    }

    #[test]
    fn supported_efforts_and_explicit_configs_match_model_capabilities() {
        assert_eq!(
            supported_efforts("gemini-2.5-pro"),
            vec!["low", "medium", "high"]
        );
        assert_eq!(
            supported_efforts("gemini-2.5-flash"),
            vec!["none", "low", "medium", "high"]
        );
        assert_eq!(
            supported_efforts("gemini-3.5-flash-lite"),
            vec!["minimal", "low", "medium", "high"]
        );
        assert_eq!(
            supported_efforts("gemini-3.8-flash"),
            vec!["low", "medium", "high"]
        );
        assert_eq!(
            supported_efforts("gemini-4-hydrogen"),
            vec!["low", "medium", "high", "xhigh"]
        );
        assert_eq!(
            supported_efforts("gemini-4-neon"),
            vec!["low", "medium", "high", "xhigh"]
        );
        assert_eq!(
            supported_efforts("gemini-4-carbon"),
            vec!["low", "medium", "high"]
        );
        assert_eq!(
            supported_efforts("gemini-4-argon"),
            vec!["low", "medium", "high"]
        );
        assert!(supported_efforts("gemini-2.0-flash").is_empty());
        assert!(supported_efforts("gemini-4-flash").is_empty());

        for model in [
            "gemini-4-hydrogen",
            "gemini-4-neon",
            "gemini-4-carbon",
            "gemini-4-argon",
        ] {
            assert_eq!(
                lowest_thinking_config(model),
                Some(ThinkingConfig {
                    thinking_level: Some("LOW".into()),
                    ..Default::default()
                }),
                "{model}"
            );
            assert_eq!(temperature(model), None, "{model}");
        }
        assert_eq!(
            thinking_config_for_effort("gemini-4-hydrogen", "xhigh"),
            Some(ThinkingConfig {
                thinking_level: Some("XHIGH".into()),
                ..Default::default()
            })
        );
        assert_eq!(thinking_config_for_effort("gemini-4-carbon", "xhigh"), None);

        assert_eq!(
            thinking_config_for_effort("gemini-2.5-pro", "low"),
            Some(ThinkingConfig {
                thinking_budget: Some(128),
                ..Default::default()
            })
        );
        assert_eq!(thinking_config_for_effort("gemini-2.5-pro", "none"), None);
        assert_eq!(
            thinking_config_for_effort("gemini-2.5-flash", "none"),
            Some(ThinkingConfig {
                thinking_budget: Some(0),
                ..Default::default()
            })
        );
        assert_eq!(
            thinking_config_for_effort("gemini-3.8-flash", "minimal"),
            None
        );
        assert_eq!(
            thinking_config_for_effort("gemini-3.8-flash", "high"),
            Some(ThinkingConfig {
                thinking_level: Some("HIGH".into()),
                ..Default::default()
            })
        );
    }

    #[test]
    fn polishing_model_filter_keeps_gemini_text_aliases_and_excludes_specialties() {
        for id in [
            "models/gemini-2.5-flash",
            "gemini-2.5-flash-lite",
            "gemini-2.5-pro",
            "gemini-3-flash-preview",
            "gemini-3.1-pro-preview",
            "gemini-3.5-flash-lite",
            "gemini-flash-latest",
            "gemini-pro-latest",
            "gemini-4-hydrogen",
            "gemini-4-neon",
            "gemini-4-carbon",
            "gemini-4-argon",
            "gemini-4-flash",
        ] {
            assert!(is_suitable_polishing_model(id), "expected suitable: {id}");
        }
        for id in [
            "gemma-3-27b-it",
            "imagen-3.0-generate-002",
            "veo-2.0-generate-001",
            "gemini-2.5-flash-image",
            "gemini-2.5-flash-preview-tts",
            "gemini-2.5-flash-live",
            "gemini-3.5-transcribe",
            "gemini-2.5-flash-thinking",
            "gemini-embedding-001",
            "gemini-1.5-pro",
            "gemini-pro-vision",
            "gemini-2.5-flash-001",
            "gemini-2.5-flash-preview-09-2025",
            "gemini-exp-1206",
        ] {
            assert!(!is_suitable_polishing_model(id), "expected excluded: {id}");
        }
    }
}
