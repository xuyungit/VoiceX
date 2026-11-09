//! Exercise the production manager without opening the user's database.
//! Usage: local_models_check ROOT ENGINE status|download|redownload|pause-check|delete|recognize MODEL SOURCE
use std::{path::PathBuf, time::Duration};
use voicex_lib::asr::{
    managed_models::{ManagedModels, Phase, Source},
    AsrConfig, QwenLocalAsrClient,
};

#[tokio::main]
async fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args().collect();
    let root = PathBuf::from(args.get(1).ok_or("ROOT required")?);
    let engine = PathBuf::from(args.get(2).ok_or("ENGINE required")?);
    let action = args.get(3).map(String::as_str).unwrap_or("status");
    let id = args.get(4).map(String::as_str).unwrap_or("qwen3-asr-0.6b");
    let source = match args.get(5).map(String::as_str).unwrap_or("modelscope") {
        "modelscope" => Source::Modelscope,
        "huggingface" => Source::Huggingface,
        _ => return Err("Unknown source".into()),
    };
    let manager = ManagedModels::open(root, engine)?;
    manager.install_global()?;
    match action {
        "status" => {}
        "delete" => manager.remove(id, false)?,
        "recognize" => {
            let audio = tempfile::Builder::new()
                .suffix(".ogg")
                .tempfile()
                .map_err(|e| e.to_string())?;
            std::fs::write(audio.path(), include_bytes!("../provider-probe.ogg"))
                .map_err(|e| e.to_string())?;
            let mut config = AsrConfig::default();
            config.qwen_local_model = id.to_string();
            let result = QwenLocalAsrClient::new(config)
                .transcribe_file(audio.path())
                .await
                .map_err(|e| e.to_string())?;
            println!("recognized={result}");
            if result.trim().is_empty() {
                return Err("Empty recognition".into());
            }
        }
        "download" | "redownload" | "pause-check" => {
            let initial_bytes = manager
                .status()?
                .models
                .iter()
                .find(|m| m.id == id)
                .ok_or("Unknown model")?
                .downloaded_bytes;
            manager.start(id, source, action == "redownload").await?;
            let mut last = 0;
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                let status = manager.status()?;
                let item = status
                    .models
                    .iter()
                    .find(|m| m.id == id)
                    .ok_or("Unknown model")?;
                if item.downloaded_bytes / 50_000_000 != last || item.phase != Phase::Downloading {
                    println!(
                        "{} {:?} {}/{} error={:?}",
                        id, item.phase, item.downloaded_bytes, item.download_bytes, item.error
                    );
                    last = item.downloaded_bytes / 50_000_000;
                }
                if item.phase == Phase::Error {
                    return Err(item.error.clone().unwrap_or_default());
                }
                if item.phase == Phase::Ready {
                    break;
                }
                if action == "pause-check" && item.downloaded_bytes > initial_bytes + 10_000_000 {
                    manager.pause(id).await?;
                    let paused = manager.status()?;
                    assert_eq!(
                        paused.models.iter().find(|m| m.id == id).unwrap().phase,
                        Phase::Paused
                    );
                    println!("paused successfully");
                    break;
                }
            }
        }
        _ => return Err("Unknown action".into()),
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&manager.status()?).unwrap()
    );
    Ok(())
}
