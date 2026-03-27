use crate::services::hotword_service::{BoostingTable, HotwordService};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct SyncResult {
    pub status: String, // "synced", "downloaded", "uploaded", "created", "error"
    pub message: String,
    pub remote_updated_at: String,
    pub local_updated_at: String,
    pub diagnostics: Option<SyncDiagnostics>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SyncDiagnostics {
    pub server_updated_at: String,
    pub remote_synced_at: String,
    pub local_updated_at: String,
    pub local_word_count: usize,
    pub remote_word_count: usize,
    pub server_newer: bool,
    pub local_newer: bool,
    pub count_mismatch: bool,
    pub has_file_content: bool,
    pub file_size: usize,
    pub linked_table_id: String,
    pub table_name: String,
}

fn find_remote_table(tables: &[BoostingTable], online_id: &str) -> Option<BoostingTable> {
    if !online_id.is_empty() {
        if let Some(found) = tables.iter().find(|t| t.id == online_id) {
            return Some(found.clone());
        }
        log::info!("Table with ID '{}' not found, searching by name 'voicex_hotwords' (case-insensitive)...", online_id);
    }

    tables
        .iter()
        .find(|t| t.name.eq_ignore_ascii_case("voicex_hotwords"))
        .cloned()
}

#[tauri::command]
pub async fn sync_hotwords(_app_handle: tauri::AppHandle) -> Result<SyncResult, String> {
    let mut settings = crate::storage::get_settings().map_err(|e| e.to_string())?;
    let initial_local_updated_at = settings.local_hotword_updated_at.clone();

    if settings.volc_access_key.is_empty()
        || settings.volc_secret_key.is_empty()
        || settings.volc_app_id.is_empty()
    {
        return Err("Please configure Volcengine AK, SK, and App ID first.".to_string());
    }

    let service = HotwordService::new();
    let current_normalized = service.get_normalized_content(&settings.dictionary_text);
    let local_count = current_normalized.lines().count();
    log::info!(
        "Current local dictionary (normalized): {} unique words",
        local_count
    );

    let tables = service.fetch_remote_tables(&settings).await?;

    // Find our table: check ID first, then fallback to Name if ID is not found or empty
    log::info!(
        "Searching for remote table. Current linked ID: '{}'",
        settings.online_hotword_id
    );
    let remote_table = find_remote_table(&tables, &settings.online_hotword_id);

    if let Some(ref t) = remote_table {
        log::info!("Found match: name='{}', id='{}'", t.name, t.id);
    } else {
        log::info!("No matching table found among {} tables.", tables.len());
        for t in tables.iter().take(5) {
            // Log first few for debugging
            log::info!("  - [Candidate] name='{}', id='{}'", t.name, t.id);
        }
    }

    if remote_table.is_none() {
        // Create new table
        log::info!("No remote hotword table found, creating 'voicex_hotwords'");
        let latest_settings = crate::storage::get_settings().map_err(|e| e.to_string())?;
        settings = latest_settings.clone();
        let latest_normalized = service.get_normalized_content(&latest_settings.dictionary_text);
        let latest_count = latest_normalized.lines().count();
        let create_result = service
            .create_table(
                &latest_settings,
                "voicex_hotwords",
                &latest_settings.dictionary_text,
            )
            .await;

        let new_table = match create_result {
            Ok(t) => t,
            Err(e) if e.contains("BoostingNameDuplicated") => {
                log::warn!("Table 'voicex_hotwords' already exists remotely but wasn't found in search. Attempting to relink...");
                // Re-fetch with a potentially larger list or just search for it
                let tables = service.fetch_remote_tables(&settings).await?;
                if let Some(t) = tables.iter().find(|t| t.name == "voicex_hotwords") {
                    t.clone()
                } else {
                    return Err(format!(
                        "Table name duplicated on server, but still cannot find it locally: {}",
                        e
                    ));
                }
            }
            Err(e) => return Err(e),
        };

        settings.online_hotword_id = new_table.id;
        settings.remote_hotword_updated_at = new_table.update_time;
        // Keep local_hotword_updated_at as-is to avoid marking a download/create as a local edit.

        crate::storage::save_settings(&settings).map_err(|e| e.to_string())?;

        let diagnostics = SyncDiagnostics {
            server_updated_at: settings.remote_hotword_updated_at.clone(),
            remote_synced_at: settings.remote_hotword_updated_at.clone(),
            local_updated_at: settings.local_hotword_updated_at.clone(),
            local_word_count: latest_count,
            remote_word_count: new_table.word_count as usize,
            server_newer: false,
            local_newer: false,
            count_mismatch: latest_count != new_table.word_count as usize,
            has_file_content: false,
            file_size: 0,
            linked_table_id: settings.online_hotword_id.clone(),
            table_name: new_table.name.clone(),
        };

        return Ok(SyncResult {
            status: "created".to_string(),
            message: "Successfully link to remote hotword table.".to_string(),
            remote_updated_at: settings.remote_hotword_updated_at.clone(),
            local_updated_at: settings.local_hotword_updated_at.clone(),
            diagnostics: Some(diagnostics),
        });
    }

    let remote = remote_table.unwrap();
    let fallback_ts = chrono::DateTime::from_timestamp(0, 0)
        .unwrap()
        .with_timezone(&chrono::Utc);

    let remote_synced_ts =
        chrono::DateTime::parse_from_rfc3339(&settings.remote_hotword_updated_at)
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .unwrap_or(fallback_ts);

    let local_ts = chrono::DateTime::parse_from_rfc3339(&settings.local_hotword_updated_at)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .unwrap_or(fallback_ts);

    let server_ts = chrono::DateTime::parse_from_rfc3339(&remote.update_time)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .unwrap_or(remote_synced_ts);

    // Compare counts for diagnostics and decision logs
    let remote_count = remote.word_count as usize;
    let count_mismatch = local_count != remote_count;

    log::info!("Hotwords sync check: server={}, remote_synced={}, local={}, local_count={}, remote_count={}", 
        server_ts, remote_synced_ts, local_ts, local_count, remote_count);

    if count_mismatch {
        log::info!(
            "Hotwords count mismatch detected (local={}, remote={})",
            local_count,
            remote_count
        );
    }

    let compare_window = chrono::Duration::seconds(1);
    let remote_sync_guard = remote_synced_ts
        .checked_add_signed(compare_window)
        .unwrap_or(remote_synced_ts);
    let server_newer = server_ts > remote_sync_guard;
    let local_newer = local_ts > remote_sync_guard;

    let base_diagnostics = SyncDiagnostics {
        server_updated_at: remote.update_time.clone(),
        remote_synced_at: settings.remote_hotword_updated_at.clone(),
        local_updated_at: settings.local_hotword_updated_at.clone(),
        local_word_count: local_count,
        remote_word_count: remote_count,
        server_newer,
        local_newer,
        count_mismatch,
        has_file_content: false,
        file_size: 0,
        linked_table_id: remote.id.clone(),
        table_name: remote.name.clone(),
    };

    // 1. If both changed since last sync, pick the latest (last-write-wins)
    if server_newer && local_newer {
        if local_ts > server_ts {
            log::info!(
                "Both sides updated. Local is newer ({} > {}), uploading...",
                local_ts,
                server_ts
            );
            let latest_settings = crate::storage::get_settings().map_err(|e| e.to_string())?;
            if latest_settings.local_hotword_updated_at != settings.local_hotword_updated_at {
                settings.dictionary_text = latest_settings.dictionary_text;
                settings.local_hotword_updated_at = latest_settings.local_hotword_updated_at;
            }
            let normalized_latest = service.get_normalized_content(&settings.dictionary_text);
            let normalized_count = normalized_latest.lines().count();
            let updated = service
                .update_table(&settings, &remote.id, &settings.dictionary_text)
                .await?;
            settings.remote_hotword_updated_at = updated.update_time;
            settings.online_hotword_id = remote.id;

            settings.dictionary_text = normalized_latest;
            crate::storage::save_settings(&settings).map_err(|e| e.to_string())?;

            let mut diagnostics = base_diagnostics.clone();
            diagnostics.server_updated_at = settings.remote_hotword_updated_at.clone();
            diagnostics.remote_synced_at = settings.remote_hotword_updated_at.clone();
            diagnostics.remote_word_count = updated.word_count as usize;
            diagnostics.local_word_count = normalized_count;
            diagnostics.count_mismatch =
                diagnostics.local_word_count != diagnostics.remote_word_count;

            return Ok(SyncResult {
                status: "uploaded".to_string(),
                message: "Uploaded local hotword changes to server.".to_string(),
                remote_updated_at: settings.remote_hotword_updated_at.clone(),
                local_updated_at: settings.local_hotword_updated_at.clone(),
                diagnostics: Some(diagnostics),
            });
        }

        log::info!(
            "Both sides updated. Server is newer ({} > {}), downloading...",
            server_ts,
            local_ts
        );
        let latest_settings = crate::storage::get_settings().map_err(|e| e.to_string())?;
        if latest_settings.local_hotword_updated_at != initial_local_updated_at
            && !latest_settings.local_hotword_updated_at.is_empty()
        {
            log::warn!("Local hotwords changed during sync, skipping download to avoid overwrite.");
            let latest_normalized =
                service.get_normalized_content(&latest_settings.dictionary_text);
            let mut diagnostics = base_diagnostics.clone();
            diagnostics.local_updated_at = latest_settings.local_hotword_updated_at.clone();
            diagnostics.local_word_count = latest_normalized.lines().count();
            diagnostics.remote_synced_at = latest_settings.remote_hotword_updated_at.clone();
            return Ok(SyncResult {
                status: "skipped".to_string(),
                message: "Local hotwords updated during sync; download skipped to avoid overwrite."
                    .to_string(),
                remote_updated_at: latest_settings.remote_hotword_updated_at.clone(),
                local_updated_at: latest_settings.local_hotword_updated_at.clone(),
                diagnostics: Some(diagnostics),
            });
        }
        // ... (download logic same as before)
        let detail = service.get_table_detail(&settings, &remote.id).await?;
        let mut diagnostics = base_diagnostics.clone();
        if let Some(content) = detail.file_content {
            diagnostics.has_file_content = true;
            diagnostics.file_size = content.len();
            let normalized = service.get_normalized_content(&content);
            diagnostics.local_word_count = normalized.lines().count();
            diagnostics.count_mismatch =
                diagnostics.local_word_count != diagnostics.remote_word_count;
            settings.dictionary_text = content;
            settings.remote_hotword_updated_at = remote.update_time;
            settings.online_hotword_id = remote.id;

            crate::storage::save_settings(&settings).map_err(|e| e.to_string())?;

            diagnostics.remote_synced_at = settings.remote_hotword_updated_at.clone();
            return Ok(SyncResult {
                status: "downloaded".to_string(),
                message: "Downloaded newer hotwords from server.".to_string(),
                remote_updated_at: settings.remote_hotword_updated_at.clone(),
                local_updated_at: settings.local_hotword_updated_at.clone(),
                diagnostics: Some(diagnostics),
            });
        }

        return Ok(SyncResult {
            status: "error".to_string(),
            message: "Server did not return hotword content; local dictionary unchanged."
                .to_string(),
            remote_updated_at: settings.remote_hotword_updated_at.clone(),
            local_updated_at: settings.local_hotword_updated_at.clone(),
            diagnostics: Some(diagnostics),
        });
    }

    // 2. Check if server is newer
    if server_newer {
        log::info!(
            "Server hotwords are newer ({} > {}), downloading...",
            server_ts,
            remote_synced_ts
        );
        let latest_settings = crate::storage::get_settings().map_err(|e| e.to_string())?;
        if latest_settings.local_hotword_updated_at != initial_local_updated_at
            && !latest_settings.local_hotword_updated_at.is_empty()
        {
            log::warn!("Local hotwords changed during sync, skipping download to avoid overwrite.");
            let latest_normalized =
                service.get_normalized_content(&latest_settings.dictionary_text);
            let mut diagnostics = base_diagnostics.clone();
            diagnostics.local_updated_at = latest_settings.local_hotword_updated_at.clone();
            diagnostics.local_word_count = latest_normalized.lines().count();
            diagnostics.remote_synced_at = latest_settings.remote_hotword_updated_at.clone();
            return Ok(SyncResult {
                status: "skipped".to_string(),
                message: "Local hotwords updated during sync; download skipped to avoid overwrite."
                    .to_string(),
                remote_updated_at: latest_settings.remote_hotword_updated_at.clone(),
                local_updated_at: latest_settings.local_hotword_updated_at.clone(),
                diagnostics: Some(diagnostics),
            });
        }
        let detail = service.get_table_detail(&settings, &remote.id).await?;
        let mut diagnostics = base_diagnostics.clone();
        if let Some(content) = detail.file_content {
            diagnostics.has_file_content = true;
            diagnostics.file_size = content.len();
            let normalized = service.get_normalized_content(&content);
            diagnostics.local_word_count = normalized.lines().count();
            diagnostics.count_mismatch =
                diagnostics.local_word_count != diagnostics.remote_word_count;
            settings.dictionary_text = content;
            settings.remote_hotword_updated_at = remote.update_time;
            settings.online_hotword_id = remote.id;

            crate::storage::save_settings(&settings).map_err(|e| e.to_string())?;

            diagnostics.remote_synced_at = settings.remote_hotword_updated_at.clone();
            return Ok(SyncResult {
                status: "downloaded".to_string(),
                message: "Downloaded newer hotwords from server.".to_string(),
                remote_updated_at: settings.remote_hotword_updated_at.clone(),
                local_updated_at: settings.local_hotword_updated_at.clone(),
                diagnostics: Some(diagnostics),
            });
        }

        return Ok(SyncResult {
            status: "error".to_string(),
            message: "Server did not return hotword content; local dictionary unchanged."
                .to_string(),
            remote_updated_at: settings.remote_hotword_updated_at.clone(),
            local_updated_at: settings.local_hotword_updated_at.clone(),
            diagnostics: Some(diagnostics),
        });
    }

    // 3. Check if local is newer
    if local_newer {
        log::info!(
            "Local hotwords are newer ({} > {}), uploading...",
            local_ts,
            remote_synced_ts
        );
        let latest_settings = crate::storage::get_settings().map_err(|e| e.to_string())?;
        if latest_settings.local_hotword_updated_at != settings.local_hotword_updated_at {
            settings.dictionary_text = latest_settings.dictionary_text;
            settings.local_hotword_updated_at = latest_settings.local_hotword_updated_at;
        }
        let normalized_latest = service.get_normalized_content(&settings.dictionary_text);
        let normalized_count = normalized_latest.lines().count();
        let updated = service
            .update_table(&settings, &remote.id, &settings.dictionary_text)
            .await?;
        settings.remote_hotword_updated_at = updated.update_time;
        settings.online_hotword_id = remote.id;

        // Standardization: Use the normalized text locally too
        settings.dictionary_text = normalized_latest;

        crate::storage::save_settings(&settings).map_err(|e| e.to_string())?;

        let mut diagnostics = base_diagnostics.clone();
        diagnostics.server_updated_at = settings.remote_hotword_updated_at.clone();
        diagnostics.remote_synced_at = settings.remote_hotword_updated_at.clone();
        diagnostics.remote_word_count = updated.word_count as usize;
        diagnostics.local_word_count = normalized_count;
        diagnostics.count_mismatch = diagnostics.local_word_count != diagnostics.remote_word_count;

        return Ok(SyncResult {
            status: "uploaded".to_string(),
            message: "Uploaded local hotword changes to server.".to_string(),
            remote_updated_at: settings.remote_hotword_updated_at.clone(),
            local_updated_at: settings.local_hotword_updated_at.clone(),
            diagnostics: Some(diagnostics),
        });
    }

    if count_mismatch {
        log::warn!("Hotwords count mismatch without timestamp changes; attempting reconciliation.");
        if local_ts > server_ts {
            log::info!(
                "Count mismatch and local timestamp newer ({} > {}), uploading...",
                local_ts,
                server_ts
            );
            let latest_settings = crate::storage::get_settings().map_err(|e| e.to_string())?;
            if latest_settings.local_hotword_updated_at != settings.local_hotword_updated_at {
                settings.dictionary_text = latest_settings.dictionary_text;
                settings.local_hotword_updated_at = latest_settings.local_hotword_updated_at;
            }
            let normalized_latest = service.get_normalized_content(&settings.dictionary_text);
            let normalized_count = normalized_latest.lines().count();
            let updated = service
                .update_table(&settings, &remote.id, &settings.dictionary_text)
                .await?;
            settings.remote_hotword_updated_at = updated.update_time;
            settings.online_hotword_id = remote.id;
            settings.dictionary_text = normalized_latest;

            crate::storage::save_settings(&settings).map_err(|e| e.to_string())?;

            let mut diagnostics = base_diagnostics.clone();
            diagnostics.server_updated_at = settings.remote_hotword_updated_at.clone();
            diagnostics.remote_synced_at = settings.remote_hotword_updated_at.clone();
            diagnostics.remote_word_count = updated.word_count as usize;
            diagnostics.local_word_count = normalized_count;
            diagnostics.count_mismatch =
                diagnostics.local_word_count != diagnostics.remote_word_count;

            return Ok(SyncResult {
                status: "uploaded".to_string(),
                message: "Uploaded local hotword changes to reconcile mismatch.".to_string(),
                remote_updated_at: settings.remote_hotword_updated_at.clone(),
                local_updated_at: settings.local_hotword_updated_at.clone(),
                diagnostics: Some(diagnostics),
            });
        }

        log::info!("Count mismatch with no newer local timestamp; attempting download.");
        let latest_settings = crate::storage::get_settings().map_err(|e| e.to_string())?;
        if latest_settings.local_hotword_updated_at != initial_local_updated_at
            && !latest_settings.local_hotword_updated_at.is_empty()
        {
            log::warn!("Local hotwords changed during sync, skipping download to avoid overwrite.");
            let latest_normalized =
                service.get_normalized_content(&latest_settings.dictionary_text);
            let mut diagnostics = base_diagnostics.clone();
            diagnostics.local_updated_at = latest_settings.local_hotword_updated_at.clone();
            diagnostics.local_word_count = latest_normalized.lines().count();
            diagnostics.remote_synced_at = latest_settings.remote_hotword_updated_at.clone();
            diagnostics.count_mismatch =
                diagnostics.local_word_count != diagnostics.remote_word_count;
            return Ok(SyncResult {
                status: "skipped".to_string(),
                message: "Local hotwords updated during sync; download skipped to avoid overwrite."
                    .to_string(),
                remote_updated_at: latest_settings.remote_hotword_updated_at.clone(),
                local_updated_at: latest_settings.local_hotword_updated_at.clone(),
                diagnostics: Some(diagnostics),
            });
        }

        let detail = service.get_table_detail(&settings, &remote.id).await?;
        let mut diagnostics = base_diagnostics.clone();
        if let Some(content) = detail.file_content {
            diagnostics.has_file_content = true;
            diagnostics.file_size = content.len();
            let normalized = service.get_normalized_content(&content);
            diagnostics.local_word_count = normalized.lines().count();
            diagnostics.count_mismatch =
                diagnostics.local_word_count != diagnostics.remote_word_count;
            settings.dictionary_text = content;
            settings.remote_hotword_updated_at = remote.update_time;
            settings.online_hotword_id = remote.id;

            crate::storage::save_settings(&settings).map_err(|e| e.to_string())?;

            diagnostics.remote_synced_at = settings.remote_hotword_updated_at.clone();
            return Ok(SyncResult {
                status: "downloaded".to_string(),
                message: "Downloaded hotwords to reconcile mismatch.".to_string(),
                remote_updated_at: settings.remote_hotword_updated_at.clone(),
                local_updated_at: settings.local_hotword_updated_at.clone(),
                diagnostics: Some(diagnostics),
            });
        }

        return Ok(SyncResult {
            status: "error".to_string(),
            message: "Server did not return hotword content; local dictionary unchanged."
                .to_string(),
            remote_updated_at: settings.remote_hotword_updated_at.clone(),
            local_updated_at: settings.local_hotword_updated_at.clone(),
            diagnostics: Some(diagnostics),
        });
    }

    log::debug!("Hotwords already in sync ({} words).", local_count);

    Ok(SyncResult {
        status: "synced".to_string(),
        message: "Hotwords are already in sync.".to_string(),
        remote_updated_at: settings.remote_hotword_updated_at.clone(),
        local_updated_at: settings.local_hotword_updated_at.clone(),
        diagnostics: Some(base_diagnostics),
    })
}

#[tauri::command]
pub async fn force_download_hotwords(_app_handle: tauri::AppHandle) -> Result<SyncResult, String> {
    let mut settings = crate::storage::get_settings().map_err(|e| e.to_string())?;
    if settings.volc_access_key.is_empty()
        || settings.volc_secret_key.is_empty()
        || settings.volc_app_id.is_empty()
    {
        return Err("Please configure Volcengine AK, SK, and App ID first.".to_string());
    }

    let service = HotwordService::new();
    let current_normalized = service.get_normalized_content(&settings.dictionary_text);
    let local_count = current_normalized.lines().count();

    let tables = service.fetch_remote_tables(&settings).await?;
    let remote = find_remote_table(&tables, &settings.online_hotword_id)
        .ok_or_else(|| "No remote hotword table found to download.".to_string())?;

    let mut diagnostics = SyncDiagnostics {
        server_updated_at: remote.update_time.clone(),
        remote_synced_at: settings.remote_hotword_updated_at.clone(),
        local_updated_at: settings.local_hotword_updated_at.clone(),
        local_word_count: local_count,
        remote_word_count: remote.word_count as usize,
        server_newer: false,
        local_newer: false,
        count_mismatch: local_count != remote.word_count as usize,
        has_file_content: false,
        file_size: 0,
        linked_table_id: remote.id.clone(),
        table_name: remote.name.clone(),
    };

    let detail = service.get_table_detail(&settings, &remote.id).await?;
    if let Some(content) = detail.file_content {
        diagnostics.has_file_content = true;
        diagnostics.file_size = content.len();
        let normalized = service.get_normalized_content(&content);
        diagnostics.local_word_count = normalized.lines().count();
        diagnostics.count_mismatch = diagnostics.local_word_count != diagnostics.remote_word_count;
        settings.dictionary_text = content;
        settings.remote_hotword_updated_at = remote.update_time;
        settings.online_hotword_id = remote.id;

        crate::storage::save_settings(&settings).map_err(|e| e.to_string())?;

        diagnostics.remote_synced_at = settings.remote_hotword_updated_at.clone();
        return Ok(SyncResult {
            status: "downloaded".to_string(),
            message: "Forced download completed.".to_string(),
            remote_updated_at: settings.remote_hotword_updated_at.clone(),
            local_updated_at: settings.local_hotword_updated_at.clone(),
            diagnostics: Some(diagnostics),
        });
    }

    Ok(SyncResult {
        status: "error".to_string(),
        message: "Server did not return hotword content; local dictionary unchanged.".to_string(),
        remote_updated_at: settings.remote_hotword_updated_at.clone(),
        local_updated_at: settings.local_hotword_updated_at.clone(),
        diagnostics: Some(diagnostics),
    })
}

#[tauri::command]
pub async fn list_online_vocabularies() -> Result<Vec<BoostingTable>, String> {
    let settings = crate::storage::get_settings().map_err(|e| e.to_string())?;
    if settings.volc_access_key.is_empty()
        || settings.volc_secret_key.is_empty()
        || settings.volc_app_id.is_empty()
    {
        return Err("Please configure Volcengine AK, SK, and App ID first.".to_string());
    }
    let service = HotwordService::new();
    service.fetch_remote_tables(&settings).await
}
