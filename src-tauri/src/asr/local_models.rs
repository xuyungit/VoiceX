use serde::Serialize;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalModel {
    path: String,
    label: String,
    files_ready: bool,
    message: String,
    configured: bool,
}

fn inspect(path: &Path) -> LocalModel {
    let result = (|| -> Result<(), String> {
        let bytes =
            std::fs::read(path.join("config.json")).map_err(|e| format!("config.json: {e}"))?;
        let config: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        if config["model_type"] != "qwen3_asr" {
            return Err("Not a Qwen3-ASR model / 不是 Qwen3-ASR 模型".into());
        }
        for file in ["vocab.json", "merges.txt"] {
            if !path.join(file).is_file() {
                return Err(format!(
                    "Missing {file} / 缺少模型文件；请使用 CLI 兼容格式"
                ));
            }
        }
        let single = path.join("model.safetensors");
        if single.is_file() {
            if std::fs::metadata(single).map_err(|e| e.to_string())?.len() == 0 {
                return Err("Empty model weights / 模型权重为空".into());
            }
        } else {
            let bytes = std::fs::read(path.join("model.safetensors.index.json"))
                .map_err(|_| "Missing model weights / 缺少模型权重".to_string())?;
            let index: serde_json::Value =
                serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            let shards = index["weight_map"]
                .as_object()
                .ok_or("Invalid weight map")?;
            if shards.is_empty() {
                return Err("Empty weight map".into());
            }
            for name in shards.values() {
                let name = name.as_str().ok_or("Invalid shard name")?;
                let relative = Path::new(name);
                if relative
                    .components()
                    .any(|c| !matches!(c, std::path::Component::Normal(_)))
                {
                    return Err("Invalid shard path".into());
                }
                if std::fs::metadata(path.join(relative))
                    .map_err(|e| e.to_string())?
                    .len()
                    == 0
                {
                    return Err(format!("Empty shard: {name}"));
                }
            }
        }
        Ok(())
    })();
    LocalModel {
        path: path.to_string_lossy().into_owned(),
        label: path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        configured: false,
        files_ready: result.is_ok(),
        message: result.err().unwrap_or_default(),
    }
}

pub fn discover(configured: &str) -> Result<Vec<LocalModel>, String> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    let configured = if let (Some(home), Some(rest)) = (&home, configured.strip_prefix("~/")) {
        home.join(rest)
    } else {
        PathBuf::from(configured)
    };
    let mut roots = BTreeSet::new();
    let mut candidates = BTreeSet::new();
    if !configured.as_os_str().is_empty() {
        candidates.insert(configured.clone());
        if let Some(parent) = configured.parent() {
            roots.insert(parent.to_path_buf());
        }
    }
    if let Some(home) = home {
        roots.insert(home.join("models"));
        roots.insert(home.join(".cache/qwen-asr"));
        let hub = home.join(".cache/huggingface/hub");
        for model in ["Qwen3-ASR-0.6B", "Qwen3-ASR-1.7B"] {
            roots.insert(hub.join(format!("models--Qwen--{model}/snapshots")));
        }
    }
    for root in roots {
        if !root.exists() {
            continue;
        }
        let entries = std::fs::read_dir(&root).map_err(|e| format!("{}: {e}", root.display()))?;
        for entry in entries {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.is_dir() && path.join("config.json").is_file() {
                let bytes = std::fs::read(path.join("config.json")).map_err(|e| e.to_string())?;
                if let Ok(config) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                    if config["model_type"] == "qwen3_asr" {
                        candidates.insert(path);
                    }
                }
            }
        }
    }
    Ok(candidates
        .iter()
        .map(|path| {
            let mut model = inspect(path);
            model.configured = path == &configured;
            model
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_incomplete_and_wrong_model_directories() {
        let path = std::env::temp_dir().join(format!("voicex-model-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("config.json"), r#"{"model_type":"qwen3_asr"}"#).unwrap();
        assert!(!inspect(&path).files_ready);
        for file in ["vocab.json", "merges.txt", "model.safetensors"] {
            std::fs::write(path.join(file), "test").unwrap();
        }
        assert!(inspect(&path).files_ready);
        std::fs::write(path.join("config.json"), r#"{"model_type":"whisper"}"#).unwrap();
        assert!(!inspect(&path).files_ready);
        std::fs::remove_dir_all(path).unwrap();
    }
}
