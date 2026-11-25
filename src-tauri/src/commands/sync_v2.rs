//! Tauri commands for VoiceX Sync v2 (pairing, recovery key backup, scope
//! toggles, manual sync, existing-edits import, and edit conflict resolution).

use crate::storage;
use crate::sync_v2::credentials::current_machine_fingerprint;
use crate::sync_v2::crypto::decode_sync_invite;
use crate::sync_v2::engine::{
    create_device_invite, get_status_snapshot_in, import_existing_edits_in, pair_space,
    resolve_edit_conflict_with_parents_in, sync_once, DeviceInviteResult, PairSuccessResult,
    SyncV2Service, SyncV2StatusSnapshot,
};
use crate::sync_v2::store::{
    ack_sync_scope_version_tx, active_space_in, list_edit_conflict_alternatives_in,
    update_sync_scope_tx, EditConflictAlternative,
};
use crate::sync_v2::transport::{SelfHostTransport, SyncTransport};
use tauri::{AppHandle, Emitter, State};
use voicex_sync_protocol::DeviceUpdateRequest;

fn resolve_pairing_device_identity(data_dir: &std::path::Path) -> Result<String, String> {
    let fingerprint = current_machine_fingerprint();
    storage::with_db(|conn| {
        crate::sync_v2::engine::reconcile_startup_device_identity_in(conn, data_dir, &fingerprint)
            .map(|r| r.device_id)
            .map_err(|e| storage::StorageError::QueryFailed(e.to_string()))
    })
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn sync_v2_get_status(
    sync_v2: State<'_, SyncV2Service>,
) -> Result<SyncV2StatusSnapshot, String> {
    let creds = sync_v2.credential_store()?;
    tauri::async_runtime::spawn_blocking(move || {
        let device_id = storage::get_or_create_device_id().map_err(|e| e.to_string())?;
        let default_name = storage::get_settings_cached()
            .map(|s| s.sync_device_name.clone())
            .unwrap_or_default();
        storage::with_db(|conn| get_status_snapshot_in(conn, &creds, &device_id, &default_name))
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn sync_v2_pair(
    server_url: String,
    pairing_code: String,
    device_name: String,
    recovery_key: Option<String>,
    scope_history: bool,
    scope_edits: bool,
    sync_v1: State<'_, crate::services::sync_service::SyncService>,
    sync_v2: State<'_, SyncV2Service>,
) -> Result<PairSuccessResult, String> {
    let sync_lock = sync_v2.sync_lock();
    let _guard = sync_lock.lock().await;
    let data_dir = sync_v2.data_dir()?;
    let creds = sync_v2.credential_store()?;
    let transport = SelfHostTransport::new(&server_url).map_err(|e| e.to_string())?;
    let device_id =
        tauri::async_runtime::spawn_blocking(move || resolve_pairing_device_identity(&data_dir))
            .await
            .map_err(|e| e.to_string())??;

    let _legacy_pause = sync_v1.pause_for_pairing().await;
    let result = pair_space(
        |f| storage::with_db(f),
        &creds,
        &transport,
        &server_url,
        &pairing_code,
        &device_id,
        &device_name,
        recovery_key.as_deref(),
        scope_history,
        scope_edits,
    )
    .await
    .map_err(|e| e.to_string())?;

    let mut settings = storage::get_settings().map_err(|e| e.to_string())?;
    if settings.sync_enabled {
        settings.sync_enabled = false;
        storage::save_settings(&settings).map_err(|e| e.to_string())?;
    }
    sync_v2.restart_worker();
    sync_v2.emit_status();
    Ok(result)
}

#[tauri::command]
pub async fn sync_v2_create_device_invite(
    sync_v2: State<'_, SyncV2Service>,
) -> Result<DeviceInviteResult, String> {
    let sync_lock = sync_v2.sync_lock();
    let _guard = sync_lock.lock().await;
    let creds = sync_v2.credential_store()?;
    let space = tauri::async_runtime::spawn_blocking(|| {
        storage::with_db(active_space_in).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())??
    .ok_or_else(|| "No active Sync v2 space configured".to_string())?;

    let transport = SelfHostTransport::new(&space.server_url).map_err(|e| e.to_string())?;
    create_device_invite(|f| storage::with_db(f), &creds, &transport, Some(15))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn sync_v2_join_with_invite(
    invite_code: String,
    device_name: String,
    scope_history: bool,
    scope_edits: bool,
    sync_v1: State<'_, crate::services::sync_service::SyncService>,
    sync_v2: State<'_, SyncV2Service>,
) -> Result<PairSuccessResult, String> {
    let parsed = decode_sync_invite(&invite_code).map_err(|e| e.to_string())?;
    sync_v2_pair(
        parsed.server_url,
        parsed.pairing_code,
        device_name,
        Some(parsed.recovery_key),
        scope_history,
        scope_edits,
        sync_v1,
        sync_v2,
    )
    .await
}

#[tauri::command]
pub async fn sync_v2_get_recovery_key(
    sync_v2: State<'_, SyncV2Service>,
) -> Result<Option<String>, String> {
    let creds = sync_v2.credential_store()?;
    tauri::async_runtime::spawn_blocking(move || {
        let space = storage::with_db(active_space_in).map_err(|e| e.to_string())?;
        let Some(space) = space else {
            return Ok(None);
        };
        creds
            .get(&space.space_id)
            .map(|opt| opt.map(|c| c.recovery_key))
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn sync_v2_update_scope(
    sync_history: bool,
    sync_edits: bool,
    device_name: Option<String>,
    sync_v2: State<'_, SyncV2Service>,
) -> Result<SyncV2StatusSnapshot, String> {
    let sync_lock = sync_v2.sync_lock();
    let _guard = sync_lock.lock().await;
    let creds = sync_v2.credential_store()?;
    let space = tauri::async_runtime::spawn_blocking(|| {
        storage::with_db(active_space_in).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())??
    .ok_or_else(|| "No active Sync v2 space configured".to_string())?;

    let effective_edits = sync_history && sync_edits;
    let space_id = space.space_id.clone();
    let new_scope_version = tauri::async_runtime::spawn_blocking(move || {
        storage::with_db(|conn| {
            let tx = conn
                .unchecked_transaction()
                .map_err(|e| storage::StorageError::QueryFailed(e.to_string()))?;
            update_sync_scope_tx(&tx, &space_id, sync_history, effective_edits)?;
            let ver: i64 = tx
                .query_row(
                    "SELECT scope_version FROM sync_spaces WHERE space_id = ?1",
                    rusqlite::params![space_id],
                    |r| r.get(0),
                )
                .map_err(|e| storage::StorageError::QueryFailed(e.to_string()))?;
            tx.commit()
                .map_err(|e| storage::StorageError::QueryFailed(e.to_string()))?;
            Ok(ver)
        })
        .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())??;

    // S5/S6: Serialize with active sync cycles; ack `scope_version` ONLY if server accepts.
    // If offline or failed, `scope_version > acked_scope_version` stays pending and is pushed
    // automatically on reconnect before any envelopes are sent.
    if let Ok(Some(space_creds)) = creds.get(&space.space_id) {
        if let Ok(transport) = SelfHostTransport::new(&space.server_url) {
            let name = device_name
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| {
                    storage::get_settings_cached()
                        .map(|s| s.sync_device_name.clone())
                        .unwrap_or_else(|_| space.device_id.clone())
                });
            let platform = if cfg!(target_os = "macos") {
                "macos"
            } else if cfg!(target_os = "windows") {
                "windows"
            } else {
                "linux"
            };
            if transport
                .update_device(
                    &space_creds.device_token,
                    &space.device_id,
                    &DeviceUpdateRequest {
                        device_name: name,
                        platform: platform.to_string(),
                        app_version: env!("CARGO_PKG_VERSION").to_string(),
                        allow_stats: true,
                        allow_history: sync_history,
                        allow_edits: effective_edits,
                        key_check_tag: space.key_check_tag.clone(),
                    },
                )
                .await
                .is_ok()
            {
                let ack_space_id = space.space_id.clone();
                let _ = tauri::async_runtime::spawn_blocking(move || {
                    storage::with_db(|conn| {
                        let tx = conn
                            .unchecked_transaction()
                            .map_err(|e| storage::StorageError::QueryFailed(e.to_string()))?;
                        ack_sync_scope_version_tx(&tx, &ack_space_id, new_scope_version)?;
                        tx.commit()
                            .map_err(|e| storage::StorageError::QueryFailed(e.to_string()))
                    })
                })
                .await;
            }
        }
    }
    drop(_guard);

    sync_v2.notify_local_change();
    sync_v2.emit_status();
    sync_v2_get_status(sync_v2).await
}

#[tauri::command]
pub async fn sync_v2_import_existing_edits(
    sync_v2: State<'_, SyncV2Service>,
) -> Result<usize, String> {
    let count = tauri::async_runtime::spawn_blocking(|| {
        storage::with_db(import_existing_edits_in).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())??;

    sync_v2.notify_local_change();
    sync_v2.emit_status();
    Ok(count)
}

#[tauri::command]
pub async fn sync_v2_sync_now(
    app: AppHandle,
    sync_v2: State<'_, SyncV2Service>,
) -> Result<SyncV2StatusSnapshot, String> {
    let sync_lock = sync_v2.sync_lock();
    let _guard = sync_lock.lock().await;
    let creds = sync_v2.credential_store()?;
    let space = tauri::async_runtime::spawn_blocking(|| {
        storage::with_db(active_space_in).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())??
    .ok_or_else(|| "No active Sync v2 space configured".to_string())?;

    let transport = SelfHostTransport::new(&space.server_url).map_err(|e| e.to_string())?;
    let retention_days = storage::get_settings_cached()
        .map(|s| s.text_retention_days)
        .unwrap_or(0);
    let summary = sync_once(|f| storage::with_db(f), &creds, &transport, retention_days)
        .await
        .map_err(|e| e.to_string())?;
    drop(_guard);

    if summary.stats_pulled > 0 || summary.history_pulled > 0 || summary.edits_pulled > 0 {
        let _ = app.emit(
            "history:updated",
            serde_json::json!({ "source": "sync_v2" }),
        );
    }
    sync_v2.emit_status();
    sync_v2_get_status(sync_v2).await
}

#[tauri::command]
pub async fn sync_v2_disconnect(
    clear_credentials: Option<bool>,
    sync_v2: State<'_, SyncV2Service>,
) -> Result<SyncV2StatusSnapshot, String> {
    let sync_lock = sync_v2.sync_lock();
    let _guard = sync_lock.lock().await;
    let creds = sync_v2.credential_store()?;
    let clear = clear_credentials.unwrap_or(false);
    tauri::async_runtime::spawn_blocking(move || {
        let space = storage::with_db(active_space_in).map_err(|e| e.to_string())?;
        if let Some(space) = space {
            storage::with_db(|conn| {
                conn.execute(
                    "UPDATE sync_spaces SET is_active = 0, status = 'disabled', updated_at = ?2 WHERE space_id = ?1",
                    rusqlite::params![space.space_id, chrono::Utc::now().to_rfc3339()],
                )
                .map_err(|e| storage::StorageError::QueryFailed(e.to_string()))?;
                Ok(())
            })
            .map_err(|e| e.to_string())?;
            if clear {
                creds.remove(&space.space_id, &space.server_url, &space.device_id).map_err(|e| e.to_string())?;
            }
        }
        Ok::<_, String>(())
    })
    .await
    .map_err(|e| e.to_string())??;

    sync_v2.stop();
    sync_v2.emit_status();
    sync_v2_get_status(sync_v2).await
}

#[tauri::command]
pub async fn sync_v2_list_edit_conflicts(
    record_id: String,
) -> Result<Vec<EditConflictAlternative>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        storage::with_db(|conn| list_edit_conflict_alternatives_in(conn, &record_id))
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn sync_v2_resolve_edit_conflict(
    record_id: String,
    chosen_text: Option<String>,
    expected_parent_edit_ids: Option<Vec<String>>,
    app: AppHandle,
    sync_v2: State<'_, SyncV2Service>,
) -> Result<(), String> {
    let rid = record_id.clone();
    tauri::async_runtime::spawn_blocking(move || {
        storage::with_db(|conn| {
            resolve_edit_conflict_with_parents_in(
                conn,
                &rid,
                chosen_text.as_deref(),
                expected_parent_edit_ids,
            )
        })
        .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())??;

    sync_v2.notify_local_change();
    let _ = app.emit(
        "history:updated",
        serde_json::json!({ "id": record_id, "source": "conflict_resolved" }),
    );
    Ok(())
}
