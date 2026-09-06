//! Shared with the settings UI: known incompatibilities must fail explicitly.
use super::{AsrConfig, AsrProviderType};
use serde::Deserialize;
use std::{collections::HashMap, sync::OnceLock};

#[derive(Deserialize)]
struct Catalog {
    defaults: HashMap<String, String>,
    models: Vec<Model>,
}
#[derive(Deserialize)]
struct Model {
    provider: String,
    id: String,
    modes: Vec<String>,
    #[serde(default)]
    delay: bool,
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
pub fn supports_delay(provider: &str, id: &str) -> bool {
    catalog().models.iter().filter(|m| m.provider == provider &&
        (m.id == id.trim() || id.trim().starts_with(&format!("{}-", m.id))))
        .max_by_key(|m| m.id.len()).is_some_and(|m| m.delay)
}

pub fn validate_model(provider: &str, id: &str, mode: &str) -> Result<(), String> {
    if id.trim().is_empty() {
        return Err(format!("{provider}: 请选择识别模型 / Select a model"));
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
