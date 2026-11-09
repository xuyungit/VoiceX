//! Explicit, read-only provider probe. Does not save settings or initialize the app DB.
//! cargo run --example asr_model_probe -- <settings-db> <audio-file> <provider> <model> [mode] [--dictionary=on|off]
use std::{path::PathBuf, time::Instant};
use voicex_lib::{
    asr::{transcribe_audio_path, AsrConfig},
    commands::settings::AppSettings,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 5 {
        return Err("Expected database, audio, provider, model, optional mode".into());
    }
    let db = rusqlite::Connection::open_with_flags(
        &args[1],
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let raw: String = db.query_row(
        "SELECT value FROM user_config WHERE key='app_settings'",
        [],
        |row| row.get(0),
    )?;
    let mut value = serde_json::from_str(&raw)?;
    voicex_lib::commands::settings::migrate_openai_refine_model(&mut value);
    let mut settings: AppSettings = serde_json::from_value(value)?;
    settings.asr_provider_type = args[3].clone();
    settings.enable_diagnostics = false;
    settings.enable_asr_context = false;
    let mode = args.get(5).filter(|arg| !arg.starts_with("--"))
        .map(String::as_str).unwrap_or("batch");
    match args[3].as_str() {
        "gemini" => {
            settings.gemini_recognition_mode = mode.into();
            if mode == "realtime" {
                settings.gemini_live_model = args[4].clone();
            } else {
                settings.gemini_model = args[4].clone();
            }
            settings.gemini_post_recording_refine = "off".into();
        }
        "gemini-live" => {
            settings.asr_provider_type = "gemini".into();
            settings.gemini_recognition_mode = "realtime".into();
            settings.gemini_live_model = args[4].clone();
            settings.gemini_post_recording_refine = "off".into();
        }
        "qwen" => {
            settings.qwen_asr_model = args[4].clone();
            settings.qwen_asr_batch_model = args[4].clone();
            settings.qwen_asr_recognition_mode = mode.into();
            settings.qwen_asr_post_recording_refine = false;
        }
        "openai" => {
            settings.openai_asr_model = args[4].clone();
            settings.openai_asr_mode = mode.into();
            settings.openai_asr_post_recording_refine = "off".into();
            if let Some(refine) = args.get(6).filter(|arg| !arg.starts_with("--")) {
                settings.openai_asr_post_recording_refine = "batch_refine".into();
                settings.openai_asr_refine_model = refine.clone();
            }
        }
        "soniox" => settings.soniox_model = args[4].clone(),
        "stepaudio" => settings.stepaudio_model = args[4].clone(),
        "qwen-local" => return Err("Use the local_models_check example for the bundled engine".into()),
        _ => return Err("Unsupported probe provider".into()),
    }
    // Only the probe vocabulary is sent, never the user's dictionary/history.
    settings.dictionary_text = "VoiceX\nASR".into();
    settings.online_hotword_id.clear();
    settings.qwen_asr_vocabulary_id.clear();
    if let Some(value) = args.iter().find_map(|arg| arg.strip_prefix("--dictionary=")) {
        let enabled = match value {
            "on" => true,
            "off" => false,
            _ => return Err("Expected --dictionary=on or --dictionary=off".into()),
        };
        settings.asr_dictionary_enabled.insert(settings.asr_provider_type.clone(), enabled);
    }
    let mut config = AsrConfig::from(&settings);
    let dictionary_enabled = !config.hotwords.is_empty();
    let started = Instant::now();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(150),
        transcribe_audio_path(
            &PathBuf::from(&args[2]),
            &mut config,
            tokio_util::sync::CancellationToken::new(),
        ),
    )
    .await?;
    match result {
        Ok(text) => println!(
            "{}",
            serde_json::json!({"provider": args[3], "model": args[4],
            "dictionary_enabled": dictionary_enabled,
            "elapsed_ms": started.elapsed().as_millis(), "text": text})
        ),
        Err(error) => {
            // Provider errors can include endpoint URLs; redact configured credentials.
            let mut safe = error;
            for secret in [
                &settings.gemini_api_key,
                &settings.qwen_asr_api_key,
                &settings.openai_asr_api_key,
                &settings.soniox_api_key,
                &settings.stepaudio_api_key,
            ] {
                if !secret.is_empty() {
                    safe = safe.replace(secret, "[redacted]");
                }
            }
            eprintln!("{safe}");
            std::process::exit(1);
        }
    }
    Ok(())
}
