//! Offline text-to-MP3 rendering shared with the `voicex-tts` CLI.
//!
//! The interactive reading path ([`super::controller::TtsController`]) streams
//! audio straight into an output device. Rendering a document to an `.mp3`
//! file reuses the same LLM preprocessing prompt, the same [`AppSettings`]
//! schema and the same cloud synthesis backends, while collecting the MP3
//! bytes each backend already receives from its provider instead of opening an
//! audio device.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;

use crate::commands::settings::AppSettings;
use crate::llm::LLMConfig;
use crate::services::llm_service::{build_llm_config_for_key, LLM_PROVIDER_FOLLOW};

use super::aliyun::{self, AliyunConfig};
use super::azure::{self, AzureConfig};
use super::controller::{
    provider_voice_slot, voice_request, PROVIDER_ALIYUN, PROVIDER_AZURE, PROVIDER_EDGE,
    PROVIDER_MIMO, PROVIDER_VOLCENGINE,
};
use super::edge;
use super::llm_stage::{fill_translate_prompt, run_llm_stage, LlmStageError, TRANSLATE_MAX_CHARS};
use super::mimo::{self, MimoConfig};
use super::volcengine::{self, VolcengineConfig};
use super::{split_for_backend, SessionSlot, TtsRequest};

/// Sample rate requested from cloud providers when rendering to a file.
///
/// Every cloud backend supports 24 kHz MP3 (Edge and MiMo are fixed at 24 kHz;
/// Volcengine, Aliyun and Azure offer it alongside 16 and 48 kHz), so fixing
/// it keeps file rendering independent of whether the machine has an audio
/// output device attached.
pub const RENDER_SAMPLE_RATE: u32 = 24_000;

/// Callback receiving each synthesized MP3 piece in order:
/// `(piece_index, total_pieces, piece_text, mp3_bytes)`.
pub(crate) type PieceSink<'a> = dyn FnMut(usize, usize, &str, Vec<u8>) -> Result<(), String> + 'a;

/// What to do with the input text before speech synthesis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LlmMode {
    /// Follow `settings.tts_preprocess_enabled`, using `tts_llm_provider_key`
    /// when enabled.
    FromSettings,
    /// Skip LLM optimization and synthesize the input text as-is.
    Off,
    /// Run the reading-cleanup prompt (`tts_preprocess_prompt_template`).
    /// `provider_key` overrides `tts_llm_provider_key` when `Some`.
    Preprocess {
        provider_key: Option<String>,
        model: Option<String>,
    },
    /// Run the translate-and-read prompt (`tts_translate_prompt_template`).
    Translate {
        provider_key: Option<String>,
        model: Option<String>,
        source_language: Option<String>,
        target_language: Option<String>,
    },
}

/// Overrides the caller (e.g. CLI flags) may apply on top of [`AppSettings`].
#[derive(Debug, Clone, Default)]
pub struct RenderOptions {
    /// Cloud TTS provider (`edge`, `volcengine`, `aliyun`, `mimo`, `azure`).
    /// `None` uses `settings.tts_provider_type`, or `"edge"` when the stored
    /// provider is `"system"` (which speaks through the OS without producing
    /// MP3 bytes).
    pub tts_provider: Option<String>,
    /// Backend-specific voice/speaker id; `None` keeps the provider's configured voice.
    pub voice: Option<String>,
    /// Aliyun TTS model (`qwen3-tts-flash`, `qwen-audio-3.1-tts-flash`, …);
    /// `None` keeps `settings.aliyun_tts_model`.
    pub aliyun_model: Option<String>,
    /// Speaking speed multiplier in `0.5..=2.0` (`1.0` = neutral); `None`
    /// keeps the provider's stored rate.
    pub speed: Option<f32>,
    /// LLM preprocessing / translation mode.
    pub llm: LlmMode,
}

impl Default for LlmMode {
    fn default() -> Self {
        Self::FromSettings
    }
}

/// Progress notification emitted as the pipeline advances.
#[derive(Debug, Clone)]
pub enum RenderEvent<'a> {
    LlmChunk {
        index: usize,
        total: usize,
        chars: usize,
    },
    LlmDone {
        input_chars: usize,
        output_chars: usize,
    },
    TtsPiece {
        index: usize,
        total: usize,
        chars: usize,
        bytes: usize,
    },
    #[allow(dead_code)]
    PreparedText(&'a str),
}

/// Default `.mp3` path for `input`: appends `.mp3` to the full file name
/// (`notes.md` -> `notes.md.mp3`), matching the CLI specification.
pub fn default_output_path(input: &Path) -> PathBuf {
    let mut os = input.as_os_str().to_os_string();
    os.push(".mp3");
    PathBuf::from(os)
}

/// Default location of the desktop app's SQLite database (`VOICEX_DB` if set,
/// otherwise `<app_data_dir>/com.voicex.app/voicex.db` on macOS, Windows and
/// Linux).
pub fn default_db_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("VOICEX_DB") {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            return Some(PathBuf::from(trimmed));
        }
    }
    let base = if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Library/Application Support"))
    } else if cfg!(target_os = "windows") {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share"))
            })
    }?;
    Some(base.join("com.voicex.app").join("voicex.db"))
}

/// Resolve the cloud provider to use for MP3 rendering.
///
/// The macOS `"system"` provider speaks directly to the OS audio output and
/// cannot produce an MP3 stream; when no cloud provider was chosen explicitly
/// and the app settings are still on `"system"`, `"edge"` is used because it
/// requires no API key. Passing `--tts system` explicitly is rejected so the
/// caller sees why instead of getting a different voice than asked for.
pub fn resolve_tts_provider(
    settings: &AppSettings,
    explicit: Option<&str>,
) -> Result<String, String> {
    if let Some(raw) = explicit.map(str::trim).filter(|s| !s.is_empty()) {
        let provider = raw.to_ascii_lowercase();
        return match provider.as_str() {
            PROVIDER_EDGE | PROVIDER_VOLCENGINE | PROVIDER_ALIYUN | PROVIDER_MIMO
            | PROVIDER_AZURE => Ok(provider),
            "system" => Err(
                "the 'system' voice speaks directly through the OS and cannot export MP3; \
                 choose one of: edge, volcengine, aliyun, mimo, azure"
                    .to_string(),
            ),
            other => Err(format!(
                "unknown TTS provider '{other}' (expected: edge, volcengine, aliyun, mimo, azure)"
            )),
        };
    }

    let configured = settings.tts_provider_type.trim();
    match configured {
        PROVIDER_EDGE | PROVIDER_VOLCENGINE | PROVIDER_ALIYUN | PROVIDER_MIMO | PROVIDER_AZURE => {
            Ok(configured.to_string())
        }
        _ => Ok(PROVIDER_EDGE.to_string()),
    }
}

/// Apply `options` to a working copy of `settings` and build the [`TtsRequest`]
/// that every cloud backend consumes.
pub fn build_render_request(
    settings: &AppSettings,
    options: &RenderOptions,
    text: String,
) -> Result<(AppSettings, TtsRequest), String> {
    let provider = resolve_tts_provider(settings, options.tts_provider.as_deref())?;
    let mut effective = settings.clone();
    effective.tts_provider_type = provider;

    if let Some(model) = options
        .aliyun_model
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty())
    {
        effective.aliyun_tts_model = model.to_string();
    }

    if let Some(voice) = options
        .voice
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        *provider_voice_slot(&mut effective) = voice.to_string();
    }

    let mut request = voice_request(&effective, text);
    if let Some(speed) = options.speed {
        if !speed.is_finite() || !(0.5..=2.0).contains(&speed) {
            return Err(format!(
                "invalid speed {speed}: expected a multiplier between 0.5 and 2.0"
            ));
        }
        // `voice_request` stores normalized rate where 0.5 == 1.0x.
        request.rate = Some((speed * 0.5).clamp(0.25, 1.0));
    }

    Ok((effective, request))
}

/// Resolve the [`LLMConfig`] and system prompt for `mode`, or `None` when the
/// LLM stage is disabled.
pub fn resolve_llm_stage(
    settings: &AppSettings,
    mode: &LlmMode,
) -> Result<Option<(LLMConfig, String)>, String> {
    match mode {
        LlmMode::Off => Ok(None),
        LlmMode::FromSettings => {
            if !settings.tts_preprocess_enabled {
                return Ok(None);
            }
            let config = build_llm_config(settings, None, None)?;
            Ok(Some((
                config,
                settings.tts_preprocess_prompt_template.clone(),
            )))
        }
        LlmMode::Preprocess {
            provider_key,
            model,
        } => {
            let config = build_llm_config(settings, provider_key.as_deref(), model.as_deref())?;
            Ok(Some((
                config,
                settings.tts_preprocess_prompt_template.clone(),
            )))
        }
        LlmMode::Translate {
            provider_key,
            model,
            source_language,
            target_language,
        } => {
            let config = build_llm_config(settings, provider_key.as_deref(), model.as_deref())?;
            let source = source_language
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or(&settings.tts_translate_source_language);
            let target = target_language
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or(&settings.tts_translate_target_language);
            let prompt =
                fill_translate_prompt(&settings.tts_translate_prompt_template, source, target);
            Ok(Some((config, prompt)))
        }
    }
}

fn build_llm_config(
    settings: &AppSettings,
    provider_override: Option<&str>,
    model_override: Option<&str>,
) -> Result<LLMConfig, String> {
    let key = provider_override
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&settings.tts_llm_provider_key);
    validate_llm_provider_key(settings, key)?;
    let mut config = build_llm_config_for_key(settings, key);
    if let Some(model) = model_override.map(str::trim).filter(|m| !m.is_empty()) {
        config.model_name = model.to_string();
    }
    if !config.is_valid() {
        let missing = config.missing_request_fields().join(", ");
        return Err(format!(
            "LLM provider '{}' is not configured (missing: {missing})",
            effective_llm_name(settings, key)
        ));
    }
    Ok(config)
}

fn effective_llm_name<'a>(settings: &'a AppSettings, key: &'a str) -> &'a str {
    let key = key.trim();
    if key.is_empty() || key == LLM_PROVIDER_FOLLOW {
        settings.llm_provider_type.as_str()
    } else {
        key
    }
}

fn validate_llm_provider_key(settings: &AppSettings, key: &str) -> Result<(), String> {
    let key = key.trim();
    if key.is_empty() || key == LLM_PROVIDER_FOLLOW {
        return Ok(());
    }
    if let Some(id) = key.strip_prefix("custom:") {
        // The desktop resolver falls back to the first endpoint. An explicit
        // CLI id must match exactly before any document can be sent there.
        if id.is_empty()
            || !settings
                .llm_custom_endpoints
                .iter()
                .any(|endpoint| endpoint.id == id)
        {
            return Err(format!(
                "custom LLM endpoint '{id}' was not found in settings"
            ));
        }
        return Ok(());
    }
    match key {
        "volcengine" | "openai" | "qwen" | "deepseek" | "gemini" | "anthropic" | "custom" => Ok(()),
        other => Err(format!(
            "unknown LLM provider '{other}' (expected: follow, volcengine, openai, qwen, deepseek, gemini, anthropic, or custom:<id>)"
        )),
    }
}

/// Run the LLM cleanup/translation stage over `text`.
///
/// Documents longer than [`TRANSLATE_MAX_CHARS`] are split on sentence or
/// paragraph boundaries and processed chunk by chunk so a full-length Markdown
/// article does not exceed a single completion's timeout or output budget.
/// Unlike interactive selection reading, failures here are returned as errors
/// rather than silently falling back to raw Markdown — when rendering a file,
/// speaking raw table pipes or URLs without telling the user would hide the
/// failure.
pub fn optimize_text_for_speech(
    config: &LLMConfig,
    system_prompt: &str,
    text: &str,
    mut on_event: impl FnMut(RenderEvent<'_>),
) -> Result<String, LlmStageError> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(String::new());
    }
    let pieces = split_for_backend(trimmed, TRANSLATE_MAX_CHARS);
    let total = pieces.len();
    let slot = SessionSlot::default();
    let token = slot.claim();
    let mut outputs = Vec::with_capacity(total);

    for (index, piece) in pieces.iter().enumerate() {
        let piece_trimmed = piece.trim();
        if piece_trimmed.is_empty() {
            continue;
        }
        on_event(RenderEvent::LlmChunk {
            index,
            total,
            chars: piece_trimmed.chars().count(),
        });
        let reply = run_llm_stage(config.clone(), system_prompt, piece_trimmed, &token)?;
        outputs.push(reply);
    }

    let joined = outputs.join("\n\n");
    on_event(RenderEvent::LlmDone {
        input_chars: trimmed.chars().count(),
        output_chars: joined.chars().count(),
    });
    Ok(joined)
}

/// Synthesize `request` with the chosen cloud provider and stream each piece's
/// MP3 bytes into `writer`.
pub fn synthesize_to_writer<W: Write>(
    settings: &AppSettings,
    request: &TtsRequest,
    writer: &mut W,
    mut on_event: impl FnMut(RenderEvent<'_>),
) -> Result<u64, String> {
    if request.text.trim().is_empty() {
        return Err("input text is empty after preparation".to_string());
    }
    let slot = SessionSlot::default();
    let token = slot.claim();
    let mut total_bytes = 0u64;

    let mut sink =
        |index: usize, total: usize, piece: &str, bytes: Vec<u8>| -> Result<(), String> {
            on_event(RenderEvent::TtsPiece {
                index,
                total,
                chars: piece.chars().count(),
                bytes: bytes.len(),
            });
            writer
                .write_all(&bytes)
                .map_err(|err| format!("failed to write output MP3: {err}"))?;
            total_bytes += bytes.len() as u64;
            Ok(())
        };

    let provider = settings.tts_provider_type.as_str();
    let result = tauri::async_runtime::block_on(async {
        match provider {
            PROVIDER_EDGE => edge::render(request, &token, &mut sink).await,
            PROVIDER_VOLCENGINE => {
                volcengine::render(
                    VolcengineConfig::from_settings(settings),
                    request,
                    &token,
                    &mut sink,
                )
                .await
            }
            PROVIDER_ALIYUN => {
                aliyun::render(
                    AliyunConfig::from_settings(settings),
                    request,
                    &token,
                    &mut sink,
                )
                .await
            }
            PROVIDER_MIMO => {
                mimo::render(
                    MimoConfig::from_settings(settings),
                    request,
                    &token,
                    &mut sink,
                )
                .await
            }
            PROVIDER_AZURE => {
                azure::render(
                    AzureConfig::from_settings(settings),
                    request,
                    &token,
                    &mut sink,
                )
                .await
            }
            other => Err(format!(
                "unsupported TTS provider '{other}' for MP3 rendering"
            )),
        }
    });
    token.finish();
    result?;
    writer
        .flush()
        .map_err(|err| format!("failed to flush output MP3: {err}"))?;
    Ok(total_bytes)
}

pub(crate) fn require_api_key(api_key: &str) -> Result<(), String> {
    if api_key.trim().is_empty() {
        return Err("no API key configured for the selected TTS provider".to_string());
    }
    Ok(())
}

pub(crate) fn collect_piece(
    outcome: Result<bool, String>,
    rx: Receiver<Vec<u8>>,
) -> Result<Vec<u8>, String> {
    match outcome {
        Ok(true) => {
            let mut bytes = Vec::new();
            for chunk in rx {
                bytes.extend_from_slice(&chunk);
            }
            if bytes.is_empty() {
                return Err("the stream ended before any audio arrived".to_string());
            }
            Ok(bytes)
        }
        Ok(false) => Err("synthesis was cancelled".to_string()),
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::settings::CustomLlmEndpoint;

    fn custom_endpoint(id: &str) -> CustomLlmEndpoint {
        CustomLlmEndpoint {
            id: id.to_string(),
            name: id.to_string(),
            base_url: format!("https://{id}.example/v1"),
            api_key: "fixture-only".to_string(),
            model: format!("{id}-model"),
            api_mode: "chat_completions".to_string(),
            reasoning_effort: String::new(),
            extra_body: String::new(),
        }
    }

    #[test]
    fn explicit_custom_endpoint_must_exist_instead_of_using_the_first() {
        let mut settings = AppSettings::default();
        settings.llm_custom_endpoints = vec![custom_endpoint("first"), custom_endpoint("second")];

        for id in ["missing", ""] {
            let error = resolve_llm_stage(
                &settings,
                &LlmMode::Preprocess {
                    provider_key: Some(format!("custom:{id}")),
                    model: None,
                },
            )
            .unwrap_err();
            assert!(error.contains("was not found"), "{error}");
        }

        let (config, _) = resolve_llm_stage(
            &settings,
            &LlmMode::Preprocess {
                provider_key: Some("custom:second".to_string()),
                model: None,
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(config.base_url, "https://second.example/v1");
        assert_eq!(config.model_name, "second-model");
    }

    #[test]
    fn missing_stored_custom_endpoint_is_also_rejected() {
        let mut settings = AppSettings::default();
        settings.tts_preprocess_enabled = true;
        settings.tts_llm_provider_key = "custom:missing".to_string();
        settings.llm_custom_endpoints = vec![custom_endpoint("first")];
        let error = resolve_llm_stage(&settings, &LlmMode::FromSettings).unwrap_err();
        assert!(error.contains("'missing' was not found"), "{error}");
    }

    #[test]
    fn default_output_appends_mp3_to_the_input_filename() {
        assert_eq!(
            default_output_path(Path::new("article.md")),
            PathBuf::from("article.md.mp3")
        );
        assert_eq!(
            default_output_path(Path::new("/tmp/notes.txt")),
            PathBuf::from("/tmp/notes.txt.mp3")
        );
    }

    #[test]
    fn system_provider_defaults_to_edge_unless_explicitly_requested() {
        let mut settings = AppSettings::default();
        settings.tts_provider_type = "system".to_string();
        assert_eq!(resolve_tts_provider(&settings, None).unwrap(), "edge");
        assert!(resolve_tts_provider(&settings, Some("system")).is_err());
        assert!(resolve_tts_provider(&settings, Some("bogus")).is_err());
    }

    #[test]
    fn configured_cloud_provider_is_preserved_when_flag_is_omitted() {
        let mut settings = AppSettings::default();
        settings.tts_provider_type = "aliyun".to_string();
        assert_eq!(resolve_tts_provider(&settings, None).unwrap(), "aliyun");
        assert_eq!(
            resolve_tts_provider(&settings, Some("volcengine")).unwrap(),
            "volcengine"
        );
    }

    #[test]
    fn voice_and_speed_overrides_land_on_the_provider_request() {
        let mut settings = AppSettings::default();
        settings.tts_provider_type = "aliyun".to_string();
        settings.aliyun_tts_model = aliyun::MODEL_QWEN3.to_string();
        let options = RenderOptions {
            tts_provider: Some("aliyun".to_string()),
            aliyun_model: Some(aliyun::MODEL_QWEN_AUDIO_31.to_string()),
            voice: Some(" custom_voice ".to_string()),
            speed: Some(1.5),
            llm: LlmMode::Off,
        };
        let (effective, request) =
            build_render_request(&settings, &options, "你好".to_string()).unwrap();
        assert_eq!(effective.aliyun_tts_model, aliyun::MODEL_QWEN_AUDIO_31);
        assert_eq!(request.voice.as_deref(), Some("custom_voice"));
        assert_eq!(request.rate, Some(0.75));
    }

    #[test]
    fn out_of_range_speed_is_rejected() {
        let settings = AppSettings::default();
        for bad in [0.2, 2.5, f32::NAN] {
            let options = RenderOptions {
                speed: Some(bad),
                ..Default::default()
            };
            assert!(build_render_request(&settings, &options, "hi".to_string()).is_err());
        }
    }

    #[test]
    fn llm_mode_respects_off_settings_and_overrides() {
        let mut settings = AppSettings::default();
        settings.tts_preprocess_enabled = false;
        settings.llm_provider_type = "openai".to_string();
        settings.llm_openai_api_key = "sk-test".to_string();
        settings.llm_openai_model = "gpt-default".to_string();

        assert!(resolve_llm_stage(&settings, &LlmMode::Off)
            .unwrap()
            .is_none());
        assert!(resolve_llm_stage(&settings, &LlmMode::FromSettings)
            .unwrap()
            .is_none());

        let (config, _) = resolve_llm_stage(
            &settings,
            &LlmMode::Preprocess {
                provider_key: Some("openai".to_string()),
                model: Some("gpt-override".to_string()),
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(config.model_name, "gpt-override");
        assert_eq!(config.api_key, "sk-test");
    }

    #[test]
    fn unconfigured_llm_provider_returns_an_error_instead_of_falling_back() {
        let settings = AppSettings::default();
        let err = resolve_llm_stage(
            &settings,
            &LlmMode::Preprocess {
                provider_key: Some("gemini".to_string()),
                model: None,
            },
        )
        .unwrap_err();
        assert!(err.contains("gemini"), "got: {err}");
    }
}
