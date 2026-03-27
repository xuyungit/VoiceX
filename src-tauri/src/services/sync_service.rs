use chrono::Utc;
use futures_util::StreamExt;
use hmac::{Hmac, Mac};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::commands::settings::AppSettings;
use crate::storage::{self, HistoryRecord, UsageStats};

#[derive(Clone, Default)]
pub struct SyncService {
    inner: Arc<Mutex<SyncServiceInner>>,
}

#[derive(Default)]
struct SyncServiceInner {
    app_handle: Option<AppHandle>,
    config: SyncConfig,
    cancel: Option<CancellationToken>,
    flush_tx: Option<mpsc::UnboundedSender<()>>,
}

#[derive(Debug, Clone, Default, PartialEq)]
struct SyncConfig {
    enabled: bool,
    server_url: String,
    token: String,
    shared_secret: String,
    device_name: String,
}

impl SyncConfig {
    fn from_settings(settings: &AppSettings) -> Self {
        Self {
            enabled: settings.sync_enabled,
            server_url: settings.sync_server_url.trim().to_string(),
            token: settings.sync_token.trim().to_string(),
            shared_secret: settings.sync_shared_secret.trim().to_string(),
            device_name: settings.sync_device_name.trim().to_string(),
        }
    }

    fn is_valid(&self) -> bool {
        self.enabled
            && !self.server_url.is_empty()
            && !self.token.is_empty()
            && !self.shared_secret.is_empty()
            && !self.device_name.is_empty()
    }

    fn bearer_token(&self) -> Result<String, String> {
        if self.token.is_empty() || self.shared_secret.is_empty() {
            return Err("missing sync auth".to_string());
        }
        let token_hash = sha256_hex(self.token.as_bytes());
        let sig = hmac_sha256_hex(&self.shared_secret, &token_hash)?;
        Ok(format!("vx1.{}.{}", token_hash, sig))
    }
}

impl SyncService {
    pub fn init_with_handle(&self, handle: &AppHandle) {
        let mut inner = self.inner.lock().expect("sync service lock");
        inner.app_handle = Some(handle.clone());
    }

    pub fn apply_settings(&self, settings: &AppSettings) {
        let new_config = SyncConfig::from_settings(settings);
        let mut inner = self.inner.lock().expect("sync service lock");
        let config_changed = inner.config != new_config;
        inner.config = new_config.clone();

        let device_id = storage::get_or_create_device_id().unwrap_or_default();
        if !device_id.is_empty() {
            let _ = storage::upsert_device_registry(&device_id, &new_config.device_name);
        }

        if !new_config.enabled {
            stop_sync(&mut inner);
            drop(inner);
            self.update_status("disabled", None);
            return;
        }

        if !new_config.is_valid() {
            stop_sync(&mut inner);
            drop(inner);
            self.update_status("blocked", Some("missing sync config".to_string()));
            return;
        }

        if config_changed || inner.cancel.is_none() {
            start_sync(inner, new_config);
        } else if let Some(tx) = inner.flush_tx.as_ref() {
            let _ = tx.send(());
        }
    }

    pub fn enqueue_history_upsert(&self, record: &HistoryRecord) {
        let inner = self.inner.lock().expect("sync service lock");
        if !inner.config.is_valid() {
            return;
        }

        let source_device_id = record
            .source_device_id
            .clone()
            .unwrap_or_else(|| storage::get_or_create_device_id().unwrap_or_default());
        if source_device_id.is_empty() {
            return;
        }

        let payload = SyncHistoryRecord {
            id: record.id.clone(),
            source_device_id,
            timestamp: record.timestamp.clone(),
            text: record.text.clone(),
            original_text: record.original_text.clone(),
            ai_correction_applied: record.ai_correction_applied,
            llm_invoked: record.llm_invoked,
            mode: record.mode.clone(),
            duration_ms: record.duration_ms,
            is_final: record.is_final,
            error_code: record.error_code,
            asr_model_name: record.asr_model_name.clone(),
            llm_model_name: record.llm_model_name.clone(),
        };

        let event_id = format!("history:{}", record.id);
        let payload_value = serde_json::to_value(payload).unwrap_or(Value::Null);
        let _ = storage::enqueue_outbox_event(
            &event_id,
            "history.upsert",
            Some(&record.id),
            &payload_value,
        );
        if let Some(tx) = inner.flush_tx.as_ref() {
            let _ = tx.send(());
        }
    }

    pub fn enqueue_history_delete(&self, record_id: &str) {
        let inner = self.inner.lock().expect("sync service lock");
        if !inner.config.is_valid() {
            return;
        }

        let _ = storage::delete_outbox_upsert_for_record(record_id);
        let event_id = Uuid::new_v4().to_string();
        let payload = json!({ "recordId": record_id });
        let _ =
            storage::enqueue_outbox_event(&event_id, "history.delete", Some(record_id), &payload);
        if let Some(tx) = inner.flush_tx.as_ref() {
            let _ = tx.send(());
        }
    }

    pub fn request_sync_now(&self) {
        let inner = self.inner.lock().expect("sync service lock");
        if let Some(tx) = inner.flush_tx.as_ref() {
            let _ = tx.send(());
        }
    }

    pub fn emit_sync_state(&self) {
        if let Some(app) = self
            .inner
            .lock()
            .expect("sync service lock")
            .app_handle
            .clone()
        {
            emit_sync_state(&app, None);
        }
    }

    fn update_status(&self, status: &str, error: Option<String>) {
        let app = {
            self.inner
                .lock()
                .expect("sync service lock")
                .app_handle
                .clone()
        };
        if let Some(app) = app {
            update_status(&app, status, error);
        }
    }
}

fn stop_sync(inner: &mut SyncServiceInner) {
    if let Some(cancel) = inner.cancel.take() {
        cancel.cancel();
    }
    inner.flush_tx = None;
}

fn start_sync(mut inner: std::sync::MutexGuard<'_, SyncServiceInner>, config: SyncConfig) {
    stop_sync(&mut inner);
    let app = match inner.app_handle.clone() {
        Some(handle) => handle,
        None => {
            log::warn!("Sync service missing app handle; cannot start");
            return;
        }
    };

    let (tx, rx) = mpsc::unbounded_channel();
    let cancel = CancellationToken::new();
    inner.flush_tx = Some(tx);
    inner.cancel = Some(cancel.clone());
    drop(inner);

    tauri::async_runtime::spawn(sync_worker(config, app, cancel, rx));
}

async fn sync_worker(
    config: SyncConfig,
    app: AppHandle,
    cancel: CancellationToken,
    mut flush_rx: mpsc::UnboundedReceiver<()>,
) {
    let client = reqwest::Client::new();
    let device_id = match storage::get_or_create_device_id() {
        Ok(id) => id,
        Err(err) => {
            log::warn!("Failed to load device id: {}", err);
            update_status(&app, "error", Some("device id unavailable".to_string()));
            return;
        }
    };

    if let Err(err) = storage::backfill_source_device_id(&device_id) {
        log::warn!("Failed to backfill device id: {}", err);
    }
    if let Err(err) = storage::upsert_device_registry(&device_id, &config.device_name) {
        log::warn!("Failed to update device registry: {}", err);
    }

    let mut backoff = Duration::from_secs(2);
    loop {
        if cancel.is_cancelled() {
            break;
        }

        update_status(&app, "connecting", None);

        let result =
            run_sync_session(&client, &config, &device_id, &app, &cancel, &mut flush_rx).await;

        if let Err(err) = result {
            log::warn!("Sync session ended: {}", err);
            update_status(&app, "reconnecting", Some(err));
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(Duration::from_secs(30));
        } else {
            backoff = Duration::from_secs(2);
        }
    }

    update_status(&app, "disabled", None);
}

async fn run_sync_session(
    client: &reqwest::Client,
    config: &SyncConfig,
    device_id: &str,
    app: &AppHandle,
    cancel: &CancellationToken,
    flush_rx: &mut mpsc::UnboundedReceiver<()>,
) -> Result<(), String> {
    register_device(client, config, device_id).await?;
    let account = refresh_account(client, config, app).await?;
    let (server_state, _is_new) =
        storage::ensure_sync_server_state(&account.server_id, &account.account_id)
            .map_err(|e| e.to_string())?;
    storage::set_current_sync_target(
        &account.server_id,
        &account.account_id,
        server_state.last_seq,
    )
    .map_err(|e| e.to_string())?;
    if let Err(err) = seed_outbox_from_history(device_id, &server_state) {
        log::warn!("Failed to seed outbox: {}", err);
    }
    let catch_up_since = server_state.last_seq.saturating_sub(1000);
    catch_up_events(
        client,
        config,
        catch_up_since,
        &account.server_id,
        &account.account_id,
        app,
        cancel,
    )
    .await?;
    update_status(app, "syncing", None);
    flush_outbox_until_empty(
        client,
        config,
        device_id,
        &account.server_id,
        &account.account_id,
        app,
        200,
    )
    .await?;

    update_status(app, "live", None);
    run_sse_stream(
        client,
        config,
        device_id,
        &account.server_id,
        &account.account_id,
        app,
        cancel,
        flush_rx,
    )
    .await?;
    Ok(())
}

async fn register_device(
    client: &reqwest::Client,
    config: &SyncConfig,
    device_id: &str,
) -> Result<(), String> {
    let url = format!("{}/v1/device", config.server_url.trim_end_matches('/'));
    let auth = config.bearer_token()?;
    let payload = json!({
        "deviceId": device_id,
        "deviceName": config.device_name,
        "platform": std::env::consts::OS,
        "appVersion": env!("CARGO_PKG_VERSION"),
    });

    let resp = client
        .put(&url)
        .bearer_auth(auth)
        .json(&payload)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if resp.status().is_success() {
        Ok(())
    } else {
        Err(format!("device register failed ({})", resp.status()))
    }
}

async fn refresh_account(
    client: &reqwest::Client,
    config: &SyncConfig,
    app: &AppHandle,
) -> Result<AccountResponse, String> {
    let url = format!("{}/v1/account", config.server_url.trim_end_matches('/'));
    let auth = config.bearer_token()?;
    let resp = client
        .get(&url)
        .bearer_auth(auth)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if resp.status() != StatusCode::OK {
        return Err(format!("account fetch failed ({})", resp.status()));
    }

    let payload: AccountResponse = resp.json().await.map_err(|e| e.to_string())?;
    let Some(last_seq) = payload.last_seq else {
        log::warn!("Account response missing last_seq; skipping usage stats refresh");
        return Ok(payload);
    };

    if let Err(err) = storage::set_usage_stats(&payload.usage) {
        log::warn!("Failed to update usage stats: {}", err);
    } else if let Ok((mut server_state, _)) =
        storage::ensure_sync_server_state(&payload.server_id, &payload.account_id)
    {
        server_state.usage_seq = last_seq.max(0);
        if let Err(err) = storage::set_sync_server_state(&server_state) {
            log::warn!("Failed to update usage seq after account refresh: {}", err);
        }
        emit_history_updated(app, "sync");
    }
    Ok(payload)
}

async fn catch_up_events(
    client: &reqwest::Client,
    config: &SyncConfig,
    mut last_seq: i64,
    server_id: &str,
    account_id: &str,
    app: &AppHandle,
    cancel: &CancellationToken,
) -> Result<(), String> {
    let auth = config.bearer_token()?;
    loop {
        if cancel.is_cancelled() {
            break;
        }

        let url = format!(
            "{}/v1/events?since={}&limit=200",
            config.server_url.trim_end_matches('/'),
            last_seq
        );

        let resp = client
            .get(&url)
            .bearer_auth(&auth)
            .send()
            .await
            .map_err(|e| e.to_string())?;

        if !resp.status().is_success() {
            return Err(format!("catch-up failed ({})", resp.status()));
        }

        let payload: GetEventsResponse = resp.json().await.map_err(|e| e.to_string())?;
        if payload.events.is_empty() {
            break;
        }

        for event in payload.events {
            apply_server_event(event, server_id, account_id, app)?;
            last_seq = last_seq.max(payload.last_seq);
        }
    }

    Ok(())
}

async fn flush_outbox_until_empty(
    client: &reqwest::Client,
    config: &SyncConfig,
    device_id: &str,
    server_id: &str,
    account_id: &str,
    app: &AppHandle,
    limit: u32,
) -> Result<(), String> {
    loop {
        let sent = flush_outbox_batch(client, config, device_id, server_id, account_id, app, limit)
            .await?;
        if sent == 0 {
            break;
        }
    }
    Ok(())
}

async fn flush_outbox_batch(
    client: &reqwest::Client,
    config: &SyncConfig,
    device_id: &str,
    _server_id: &str,
    _account_id: &str,
    _app: &AppHandle,
    limit: u32,
) -> Result<usize, String> {
    let auth = config.bearer_token()?;
    let events = storage::get_outbox_events(limit).map_err(|e| e.to_string())?;
    if events.is_empty() {
        return Ok(0);
    }

    let mut payload_events: Vec<Value> = Vec::with_capacity(events.len());
    for event in &events {
        let mut entry = json!({
            "eventId": event.event_id,
            "type": event.event_type,
        });
        if event.event_type == "history.upsert" {
            entry["record"] = event.payload.clone();
        } else if event.event_type == "history.delete" {
            if let Some(record_id) = &event.record_id {
                entry["recordId"] = Value::String(record_id.clone());
            }
        }
        payload_events.push(entry);
    }

    let body = json!({
        "deviceId": device_id,
        "events": payload_events
    });

    let url = format!("{}/v1/events", config.server_url.trim_end_matches('/'));
    let resp = client
        .post(&url)
        .bearer_auth(auth)
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        return Err(format!("outbox flush failed ({})", resp.status()));
    }

    let _: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    let ids: Vec<String> = events.iter().map(|e| e.event_id.clone()).collect();
    if let Err(err) = storage::delete_outbox_events(&ids) {
        log::warn!("Failed to prune outbox: {}", err);
    }

    Ok(events.len())
}

async fn run_sse_stream(
    client: &reqwest::Client,
    config: &SyncConfig,
    device_id: &str,
    server_id: &str,
    account_id: &str,
    app: &AppHandle,
    cancel: &CancellationToken,
    flush_rx: &mut mpsc::UnboundedReceiver<()>,
) -> Result<(), String> {
    let last_seq = storage::ensure_sync_server_state(server_id, account_id)
        .map(|(state, _)| state.last_seq)
        .unwrap_or(0);
    let auth = config.bearer_token()?;
    let url = format!(
        "{}/v1/subscribe?since={}",
        config.server_url.trim_end_matches('/'),
        last_seq
    );

    let response = client
        .get(&url)
        .bearer_auth(auth)
        .header("Accept", "text/event-stream")
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !response.status().is_success() {
        return Err(format!("subscribe failed ({})", response.status()));
    }

    let mut parser = SseParser::default();
    let mut stream = response.bytes_stream();
    let mut last_rx_at = tokio::time::Instant::now();
    let mut idle_check = tokio::time::interval(Duration::from_secs(10));

    loop {
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            Some(_) = flush_rx.recv() => {
                let _ = flush_outbox_until_empty(
                    client,
                    config,
                    device_id,
                    server_id,
                    account_id,
                    app,
                    200,
                )
                .await;
            }
            _ = idle_check.tick() => {
                if last_rx_at.elapsed() > Duration::from_secs(45) {
                    return Err("sse idle timeout".to_string());
                }
            }
            chunk = stream.next() => {
                match chunk {
                    Some(Ok(bytes)) => {
                        last_rx_at = tokio::time::Instant::now();
                        let events = parser.push_bytes(&bytes);
                        for data in events {
                            if let Ok(event) = serde_json::from_str::<ServerEvent>(&data) {
                                apply_server_event(event, server_id, account_id, app)?;
                            }
                        }
                    }
                    Some(Err(err)) => return Err(err.to_string()),
                    None => return Err("sse closed".to_string()),
                }
            }
        }
    }
}

fn apply_server_event(
    event: ServerEvent,
    server_id: &str,
    account_id: &str,
    app: &AppHandle,
) -> Result<(), String> {
    match event.event_type.as_str() {
        "history.upsert" => {
            let record: SyncHistoryRecord =
                serde_json::from_value(event.payload).map_err(|e| e.to_string())?;
            let local_record = HistoryRecord {
                id: record.id,
                timestamp: record.timestamp,
                text: record.text,
                original_text: record.original_text,
                ai_correction_applied: record.ai_correction_applied,
                llm_invoked: record.llm_invoked,
                mode: record.mode,
                duration_ms: record.duration_ms,
                audio_path: None,
                is_final: record.is_final,
                error_code: record.error_code,
                source_device_id: Some(record.source_device_id),
                source_device_name: None,
                asr_model_name: record.asr_model_name,
                llm_model_name: record.llm_model_name,
            };

            if let Ok(inserted) = storage::insert_history_record_with_stats(&local_record, false) {
                if inserted {
                    emit_history_updated(app, &local_record.id);
                }
            }

            if let Ok((mut server_state, _)) =
                storage::ensure_sync_server_state(server_id, account_id)
            {
                if event.seq > server_state.usage_seq {
                    let chars = local_record.text.chars().count() as i64;
                    if let Err(err) = storage::increment_usage_stats(
                        local_record.duration_ms,
                        chars,
                        local_record.llm_invoked,
                    ) {
                        log::warn!("Failed to update usage stats from sync event: {}", err);
                    } else {
                        server_state.usage_seq = event.seq;
                        if let Err(err) = storage::set_sync_server_state(&server_state) {
                            log::warn!("Failed to update usage seq: {}", err);
                        } else {
                            emit_history_updated(app, "stats");
                        }
                    }
                }
            }
        }
        "history.delete" => {
            if let Some(record_id) = event.payload.get("recordId").and_then(|v| v.as_str()) {
                storage::delete_history_record_allow_missing(record_id)
                    .map_err(|err| format!("Sync delete failed id={} err={}", record_id, err))?;
                let _ = storage::delete_outbox_upsert_for_record(record_id);
                emit_history_updated(app, record_id);
            }
        }
        "device.updated" => {
            if let (Some(id), Some(name)) = (
                event.payload.get("deviceId").and_then(|v| v.as_str()),
                event.payload.get("deviceName").and_then(|v| v.as_str()),
            ) {
                let _ = storage::upsert_device_registry(id, name);
                emit_history_updated(app, id);
            }
        }
        _ => {}
    }

    update_last_seq(app, server_id, account_id, event.seq);
    update_status(app, "live", None);
    Ok(())
}

fn emit_history_updated(app: &AppHandle, id: &str) {
    let _ = app.emit("history:updated", json!({ "id": id }));
}

fn update_last_seq(app: &AppHandle, server_id: &str, account_id: &str, seq: i64) {
    if let Ok((mut server_state, _)) = storage::ensure_sync_server_state(server_id, account_id) {
        if seq > server_state.last_seq {
            server_state.last_seq = seq;
            server_state.last_sync_at = Some(Utc::now().to_rfc3339());
            if let Err(err) = storage::set_sync_server_state(&server_state) {
                log::warn!("Failed to update server sync state: {}", err);
            }
            if let Err(err) =
                storage::set_current_sync_target(server_id, account_id, server_state.last_seq)
            {
                log::warn!("Failed to update sync cursor: {}", err);
            }
            emit_sync_state(app, None);
        }
    }
}

fn update_status(app: &AppHandle, status: &str, error: Option<String>) {
    if let Ok(mut state) = storage::get_sync_state() {
        state.status = Some(status.to_string());
        if let Some(err) = error {
            state.last_error = Some(err);
        } else if status == "live" || status == "connecting" {
            state.last_error = None;
        }
        state.last_sync_at = Some(Utc::now().to_rfc3339());
        if let Err(err) = storage::set_sync_state(&state) {
            log::warn!("Failed to update sync state: {}", err);
        } else {
            emit_sync_state(app, None);
        }
    }
}

fn emit_sync_state(app: &AppHandle, device_id_override: Option<&str>) {
    let device_id = device_id_override
        .map(|s| s.to_string())
        .or_else(|| storage::get_or_create_device_id().ok())
        .unwrap_or_default();
    if let Ok(state) = storage::get_sync_state() {
        let payload = json!({
            "state": state,
            "deviceId": device_id,
        });
        let _ = app.emit("sync:status", payload);
    }
}

fn seed_outbox_from_history(
    device_id: &str,
    server_state: &storage::SyncServerState,
) -> Result<(), String> {
    let since = server_state.seeded_at.clone();
    let mut offset = 0u32;
    let limit = 200u32;
    let mut max_seeded_at: Option<String> = None;

    loop {
        let records =
            storage::get_history_for_device_since(limit, offset, device_id, since.as_deref())
                .map_err(|e| e.to_string())?;
        if records.is_empty() {
            break;
        }

        for record in records {
            let source_device_id = record
                .source_device_id
                .clone()
                .unwrap_or_else(|| device_id.to_string());
            let record_ts = record.timestamp.clone();
            let payload = SyncHistoryRecord {
                id: record.id.clone(),
                source_device_id,
                timestamp: record.timestamp,
                text: record.text,
                original_text: record.original_text,
                ai_correction_applied: record.ai_correction_applied,
                llm_invoked: record.llm_invoked,
                mode: record.mode,
                duration_ms: record.duration_ms,
                is_final: record.is_final,
                error_code: record.error_code,
                asr_model_name: record.asr_model_name,
                llm_model_name: record.llm_model_name,
            };
            let event_id = format!("history:{}", record.id);
            let payload_value = serde_json::to_value(payload).unwrap_or(Value::Null);
            let _ = storage::enqueue_outbox_event(
                &event_id,
                "history.upsert",
                Some(&record.id),
                &payload_value,
            );

            match max_seeded_at.as_ref() {
                Some(current) => {
                    if record_ts > *current {
                        max_seeded_at = Some(record_ts.clone());
                    }
                }
                None => {
                    max_seeded_at = Some(record_ts.clone());
                }
            }
        }

        offset += limit;
    }

    if max_seeded_at.is_none() && since.is_none() {
        max_seeded_at = Some(Utc::now().to_rfc3339());
    }

    if let Some(value) = max_seeded_at {
        if let Ok((mut state, _)) =
            storage::ensure_sync_server_state(&server_state.server_id, &server_state.account_id)
        {
            state.seeded_at = Some(value);
            if let Err(err) = storage::set_sync_server_state(&state) {
                log::warn!("Failed to update seeded_at: {}", err);
            }
        }
    }

    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SyncHistoryRecord {
    id: String,
    source_device_id: String,
    timestamp: String,
    text: String,
    original_text: Option<String>,
    ai_correction_applied: bool,
    #[serde(default)]
    llm_invoked: bool,
    mode: String,
    duration_ms: i64,
    is_final: bool,
    error_code: i32,
    #[serde(default)]
    asr_model_name: Option<String>,
    #[serde(default)]
    llm_model_name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetEventsResponse {
    events: Vec<ServerEvent>,
    last_seq: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServerEvent {
    seq: i64,
    #[serde(rename = "type")]
    event_type: String,
    payload: Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AccountResponse {
    server_id: String,
    account_id: String,
    usage: UsageStats,
    last_seq: Option<i64>,
}

#[derive(Default)]
struct SseParser {
    buffer: String,
    data_lines: Vec<String>,
}

impl SseParser {
    fn push_bytes(&mut self, bytes: &[u8]) -> Vec<String> {
        let mut events = Vec::new();
        self.buffer.push_str(&String::from_utf8_lossy(bytes));

        while let Some(pos) = self.buffer.find('\n') {
            let mut line = self.buffer[..pos].to_string();
            self.buffer = self.buffer[pos + 1..].to_string();
            if line.ends_with('\r') {
                line.pop();
            }

            if line.is_empty() {
                if !self.data_lines.is_empty() {
                    events.push(self.data_lines.join("\n"));
                    self.data_lines.clear();
                }
                continue;
            }

            if let Some(data) = line.strip_prefix("data:") {
                self.data_lines.push(data.trim_start().to_string());
            }
        }

        events
    }
}

type HmacSha256 = Hmac<Sha256>;

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

fn hmac_sha256_hex(secret: &str, payload: &str) -> Result<String, String> {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .map_err(|_| "invalid shared secret".to_string())?;
    mac.update(payload.as_bytes());
    let result = mac.finalize().into_bytes();
    Ok(hex::encode(result))
}
