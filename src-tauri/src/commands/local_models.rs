use crate::asr::managed_models::{ManagedModels, Source, Status};
use tauri::State;

#[tauri::command(async)]
pub fn get_local_model_status(models: State<'_, ManagedModels>) -> Result<Status, String> {
    models.status()
}

#[tauri::command]
pub async fn download_local_model(
    models: State<'_, ManagedModels>,
    id: String,
    source: Source,
    restart: bool,
) -> Result<(), String> {
    models.start(&id, source, restart).await
}

#[tauri::command]
pub async fn pause_local_model_download(
    models: State<'_, ManagedModels>,
    id: String,
) -> Result<(), String> {
    models.pause(&id).await
}

#[tauri::command]
pub async fn delete_local_model(
    models: State<'_, ManagedModels>,
    id: String,
    partial_only: bool,
) -> Result<(), String> {
    models.pause(&id).await?;
    let models = models.inner().clone();
    tauri::async_runtime::spawn_blocking(move || models.remove(&id, partial_only))
        .await
        .map_err(|e| e.to_string())?
}
