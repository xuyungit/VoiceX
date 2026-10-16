//! Shared with the settings UI: known incompatibilities must fail explicitly.
use super::{AsrConfig, AsrProviderType};
use serde::Deserialize;
use std::{collections::HashMap, sync::OnceLock};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Catalog {
    defaults: HashMap<String, String>,
    models: Vec<Model>,
    retired: Vec<RetiredModel>,
    default_migrations: Vec<DefaultMigration>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RetiredModel {
    provider: String,
    id: String,
    shutdown_date: String,
    replacement: String,
    #[serde(default)]
    settings_fields: Vec<String>,
}
#[derive(Deserialize)]
struct DefaultMigration {
    field: String,
    previous: String,
    replacement: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Model {
    provider: String,
    id: String,
    modes: Vec<String>,
    #[serde(default)]
    delay: bool,
    #[serde(default)]
    max_recording_minutes: Option<u32>,
}
fn catalog() -> &'static Catalog {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    CATALOG.get_or_init(|| {
        serde_json::from_str(include_str!("../../../src/shared/asrModels.json"))
            .expect("bundled ASR model catalog must be valid")
    })
}
pub fn default_model(key: &str) -> String {
    catalog()
        .defaults
        .get(key)
        .expect("ASR default must exist in catalog")
        .clone()
}

/// Migrate only exact IDs in their designated built-in settings fields. The
/// catalog intentionally omits migrations that would require a provider switch.
pub fn migrate_known_model_settings(value: &mut serde_json::Value) -> bool {
    let Some(object) = value.as_object_mut() else {
        return false;
    };
    let mut changed = false;
    let mut replace = |field: &str, previous: &str, replacement: &str| {
        if object.get(field).and_then(serde_json::Value::as_str) == Some(previous) {
            object.insert(field.to_string(), replacement.into());
            changed = true;
        }
    };
    for model in &catalog().retired {
        for field in &model.settings_fields {
            replace(field, &model.id, &model.replacement);
        }
    }
    changed
}

pub fn migrate_default_model_settings(value: &mut serde_json::Value) -> bool {
    let Some(object) = value.as_object_mut() else {
        return false;
    };
    let mut changed = false;
    for migration in &catalog().default_migrations {
        if object
            .get(&migration.field)
            .and_then(serde_json::Value::as_str)
            == Some(&migration.previous)
        {
            object.insert(
                migration.field.clone(),
                migration.replacement.clone().into(),
            );
            changed = true;
        }
    }
    changed
}

pub fn supports_delay(provider: &str, id: &str) -> bool {
    catalog().models.iter().filter(|m| m.provider == provider &&
        (m.id == id.trim() || id.trim().starts_with(&format!("{}-", m.id))))
        .max_by_key(|m| m.id.len()).is_some_and(|m| m.delay)
}

pub fn max_recording_minutes(provider: &str, id: &str) -> Option<u32> {
    catalog()
        .models
        .iter()
        .filter(|model| {
            model.provider == provider
                && (model.id == id.trim() || id.trim().starts_with(&format!("{}-", model.id)))
        })
        .max_by_key(|model| model.id.len())
        .and_then(|model| model.max_recording_minutes)
}

pub fn validate_model(provider: &str, id: &str, mode: &str) -> Result<(), String> {
    if id.trim().is_empty() {
        return Err(format!("{provider}: 请选择识别模型 / Select a model"));
    }
    if let Some(model) = catalog().retired.iter().find(|model| {
        (model.provider == provider
            || (provider.starts_with("gemini") && model.provider.starts_with("gemini")))
            && model.id == id.trim()
    }) {
        return Err(format!(
            "{provider}: 模型 {} 已于 {} 停止服务。请选择 {} 或其他可用模型；原配置已保留。 / Model {} was shut down on {}. Select {} or another available model; your saved value is preserved.",
            model.id, model.shutdown_date, model.replacement,
            model.id, model.shutdown_date, model.replacement
        ));
    }
    if let Some(model) = catalog()
        .models
        .iter()
        .filter(|m| {
            (m.provider == provider
                || (provider.starts_with("gemini") && m.provider.starts_with("gemini")))
                && (m.id == id.trim() || id.trim().starts_with(&format!("{}-", m.id)))
        })
        .max_by_key(|m| m.id.len())
    {
        if model.provider != provider || !model.modes.iter().any(|m| m == mode) {
            return Err(format!("{provider}: 模型 {id} 不支持当前 {mode} 模式 / Model is not supported in this mode"));
        }
    }
    // Unknown IDs remain available for custom endpoints and future snapshots.
    Ok(())
}

impl AsrConfig {
    pub fn validate_model_selection(&self) -> Result<(), String> {
        use AsrProviderType::*;
        match self.provider_type {
            OpenAI => {
                validate_model("openai", &self.openai_asr_model, &self.openai_asr_mode)?;
                if self.post_recording_batch_refine_enabled() {
                    validate_model("openai", &self.openai_asr_refine_model, "batch")?;
                }
                Ok(())
            }
            Qwen => {
                let realtime = !self.is_batch();
                if realtime {
                    validate_model("qwen", &self.qwen_model, "realtime")?;
                }
                if !realtime || self.post_recording_batch_refine_enabled() {
                    validate_model("qwen", &self.qwen_batch_model, "batch")?;
                }
                Ok(())
            }
            Gemini => validate_model("gemini", &self.gemini_model, "batch"),
            GeminiLive => validate_model("gemini-live", &self.gemini_live_model, "realtime"),
            FunAsr => validate_model("funasr", &self.funasr_model, "realtime"),
            Soniox => validate_model("soniox", &self.soniox_model, "realtime"),
            Cohere => validate_model("cohere", &self.cohere_model, "batch"),
            StepAudio => validate_model("stepaudio", &self.stepaudio_model, "batch"),
            Mimo => validate_model("mimo", &self.mimo_model, "batch"),
            ElevenLabs => {
                if !self.is_batch() {
                    validate_model("elevenlabs", &self.elevenlabs_realtime_model, "realtime")?;
                }
                if self.is_batch() || self.post_recording_batch_refine_enabled() {
                    validate_model("elevenlabs", &self.elevenlabs_batch_model, "batch")?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    pub fn openai_refinement_config(&self) -> Self {
        let mut config = self.clone();
        config.openai_asr_model = self.openai_asr_refine_model.clone();
        config
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn known_model_settings_migrate_exact_ids_to_supported_same_provider_models_once() {
        let modes = [
            ("qwenAsrModel", "realtime"),
            ("qwenAsrBatchModel", "batch"),
            ("funasrModel", "realtime"),
        ];
        for model in &catalog().retired {
            for field in &model.settings_fields {
                let mode = modes.iter().find(|(key, _)| *key == field).unwrap().1;
                assert!(validate_model(&model.provider, &model.id, mode).is_err());
                assert!(validate_model(&model.provider, &model.replacement, mode).is_ok());
                let mut raw = serde_json::json!({
                    "asrProviderType": model.provider,
                    "qwenAsrRecognitionMode": "realtime_refine",
                    "qwenAsrWsUrl": "wss://custom.example/realtime",
                    "llmCustomEndpoints": [{"model": model.id}]
                });
                raw[field] = model.id.clone().into();
                let mut expected = raw.clone();
                expected[field] = model.replacement.clone().into();
                assert!(migrate_known_model_settings(&mut raw));
                assert_eq!(raw, expected);
                assert!(!migrate_known_model_settings(&mut raw));
            }
        }
    }

    #[test]
    fn gemini_live_default_migration_preserves_custom_models_and_other_fields() {
        let mut raw = serde_json::json!({"geminiLiveModel":"gemini-3.1-flash-live-preview", "geminiModel":"gemini-3.1-flash-live-preview"});
        assert!(migrate_default_model_settings(&mut raw));
        assert_eq!(raw["geminiLiveModel"], "gemini-3.5-transcribe-live");
        assert_eq!(raw["geminiModel"], "gemini-3.1-flash-live-preview");
        assert!(!migrate_default_model_settings(&mut raw));
        for id in [
            "my-deployment",
            "gemini-3.1-flash-live-preview-custom",
            "",
            "  ",
        ] {
            let mut raw = serde_json::json!({"geminiLiveModel":id,"qwenAsrModel":id,"qwenAsrBatchModel":id,"funasrModel":id});
            let original = raw.clone();
            assert!(!migrate_known_model_settings(&mut raw));
            assert!(!migrate_default_model_settings(&mut raw));
            assert_eq!(raw, original);
        }
    }

    #[test]
    fn asynchronous_filetrans_snapshot_is_blocked_without_a_cross_protocol_migration() {
        let id = "qwen3-asr-flash-filetrans-2025-11-17";
        let mut raw = serde_json::json!({"qwenAsrBatchModel":id});
        let original = raw.clone();
        assert!(!migrate_known_model_settings(&mut raw));
        assert_eq!(raw, original);
        let error = validate_model("qwen", id, "batch").unwrap_err();
        assert!(error.contains("2026-10-10"));
        assert!(error.contains("your saved value is preserved"));
        assert!(validate_model("custom", id, "batch").is_ok());
    }

    #[test]
    fn retired_model_rejects_saved_config_without_replacing_it() {
        let mut config = AsrConfig::default();
        config.provider_type = AsrProviderType::Gemini;
        config.gemini_model = "gemini-3.1-flash-lite-preview".into();
        let error = config.validate_model_selection().unwrap_err();
        assert!(error.contains("2026-05-25"));
        assert!(error.contains("gemini-3.1-flash-lite"));
        assert!(error.contains("your saved value is preserved"));
        assert_eq!(config.gemini_model, "gemini-3.1-flash-lite-preview");
        assert!(validate_model("gemini", " gemini-3.1-flash-lite-preview ", "batch").is_err());
        assert!(validate_model("gemini-live", &config.gemini_model, "realtime").is_err());
    }

    #[tokio::test]
    async fn retired_model_fails_before_audio_access_or_client_creation() {
        let mut config = AsrConfig::default();
        config.provider_type = AsrProviderType::Gemini;
        config.gemini_model = "gemini-3.1-flash-lite-preview".into();
        let path = std::path::PathBuf::from("unused-retired-model-test.ogg");
        let error = crate::asr::transcribe_audio_path_detailed(
            &path,
            &mut config,
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert!(error.contains("was shut down on 2026-05-25"));
        assert_eq!(config.gemini_model, "gemini-3.1-flash-lite-preview");
    }

    #[test]
    fn scheduled_retirements_and_routing_aliases_stay_available() {
        assert!(validate_model("gemini-live", "gemini-3.1-flash-live-preview", "realtime").is_ok());
        for id in ["whisper-1", "gpt-4o-transcribe", "gpt-4o-mini-transcribe"] {
            assert!(validate_model("openai", id, "batch").is_ok());
        }
        assert!(validate_model("soniox", "stt-rt-v4", "realtime").is_ok());
        assert!(validate_model("gemini", "gemini-3.1-flash-lite", "batch").is_ok());
    }

    #[test]
    fn retirement_does_not_claim_unknown_ids_or_other_providers() {
        assert!(validate_model("custom", "gemini-3.1-flash-lite-preview", "batch").is_ok());
        assert!(validate_model("gemini", "gemini-3.1-flash-lite-preview-my-deployment", "batch").is_ok());
    }

    #[test]
    fn retired_ids_are_not_presets_or_defaults() {
        for retired in &catalog().retired {
            assert!(!catalog().models.iter().any(|model|
                model.provider == retired.provider && model.id == retired.id));
            assert!(!catalog().defaults.values().any(|id| id == &retired.id));
        }
    }
    #[test]
    fn modes_reject_known_mismatches_but_keep_custom_ids() {
        assert!(validate_model("openai", "gpt-live-transcribe", "batch").is_err());
        assert!(validate_model("openai", "gpt-transcribe", "realtime").is_ok());
        assert!(validate_model("qwen", "qwen-audio-3.0-asr-flash-filetrans", "batch").is_err());
        assert!(validate_model("soniox", "stt-async-v5", "realtime").is_err());
        assert!(validate_model("openai", "my-custom-deployment", "batch").is_ok());
        assert!(validate_model("gemini", "gemini-3.5-transcribe-live", "batch").is_err());
        assert!(validate_model("qwen", "qwen3-asr-flash-filetrans-2025-11-17", "batch").is_err());
    }
    #[test]
    fn refinement_uses_its_own_model_without_mutating_live_config() {
        let mut config = AsrConfig::default();
        config.openai_asr_model = "gpt-live-transcribe".into();
        config.openai_asr_refine_model = "gpt-4o-transcribe".into();
        assert_eq!(
            config.openai_refinement_config().openai_asr_model,
            "gpt-4o-transcribe"
        );
        assert_eq!(config.openai_asr_model, "gpt-live-transcribe");
    }
    #[test]
    fn catalog_ids_are_unique_per_provider() {
        let mut seen = std::collections::HashSet::new();
        for m in &catalog().models {
            assert!(seen.insert((&m.provider, &m.id)));
        }
    }
}
