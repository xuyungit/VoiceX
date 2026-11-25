//! Sync v2 orchestration engine and Tauri background service.
//!
//! Core functions (`pair_space`, `sync_once_in`, `import_existing_edits_in`,
//! `resolve_edit_conflict_in`) operate on an abstract `SyncTransport` +
//! `CredentialStore` + SQLite `Connection` closure so multi-client end-to-end
//! tests can exercise every acceptance scenario deterministically without GUI.

use chrono::Utc;
use futures_util::StreamExt;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use thiserror::Error;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use voicex_sync_protocol::{
    selfhost_space_id, stats_object_id, DeviceUpdateRequest, EditKind, EnvelopeStatus,
    EnvelopeSubmission, PairRedeemRequest, PushEnvelopesRequest, SyncDomain, DEFAULT_PULL_LIMIT,
    MAX_PUSH_BATCH_LEN, MAX_PUSH_BODY_BYTES,
};

use crate::storage::{self, StorageError};
use crate::sync_v2::credentials::{
    current_machine_fingerprint, CredentialError, CredentialStore, DeviceIdentityStore,
    ResolvedDeviceIdentity, SpaceCredentials,
};
use crate::sync_v2::crypto::{
    decode_sync_invite, decrypt_envelope, encode_sync_invite, encrypt_envelope, CryptoError,
    EnvelopeContext, MasterKey, SyncInvite,
};
use crate::sync_v2::payloads::{HistoryOpPayload, StatsShardPayload};
use crate::sync_v2::scope::can_send_outbox_op;
use crate::sync_v2::store::{
    ack_sync_scope_version_tx, active_space_in, apply_remote_history_op_tx,
    apply_remote_stats_daily_month_tx, apply_remote_stats_totals_tx,
    build_local_daily_month_shard_in, build_local_totals_shard_in, bump_local_stats_shard_tx,
    flush_pending_audio_deletes_in, get_overview_devices_in, handoff_v1_outbox_to_v2_tx,
    import_existing_edits_tx, on_local_edit_applied_with_parents_tx,
    seed_all_legacy_local_edits_tx, upsert_sync_space_tx, OverviewDeviceSyncInfo, SyncSpaceRecord,
};
use crate::sync_v2::transport::{SelfHostTransport, SyncTransport, TransportError};

const PUSH_BATCH_SIZE: usize = MAX_PUSH_BATCH_LEN;
/// Safe request payload ceiling leaving headroom for `{"envelopes":[...]}` JSON framing.
const MAX_SAFE_PUSH_BODY_BYTES: usize = MAX_PUSH_BODY_BYTES - 8192;
const OVERSIZED_ERROR_PREFIX: &str = "oversized:";

#[derive(Debug, Error)]
pub enum SyncEngineError {
    #[error("storage error: {0}")]
    Storage(#[from] StorageError),
    #[error("credential error: {0}")]
    Credential(#[from] CredentialError),
    #[error("crypto error: {0}")]
    Crypto(#[from] CryptoError),
    #[error("transport error: {0}")]
    Transport(#[from] TransportError),
    #[error("{0}")]
    InvalidState(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncV2StatusSnapshot {
    pub configured: bool,
    pub space_id: Option<String>,
    pub server_url: Option<String>,
    pub account_id: Option<String>,
    pub device_id: String,
    pub device_name: String,
    pub status: String,
    pub last_error: Option<String>,
    pub last_sync_at: Option<String>,
    pub scope_history: bool,
    pub scope_edits: bool,
    pub scope_sync_pending: bool,
    pub pending_ops_count: i64,
    pub pending_stats_count: i64,
    pub has_credentials: bool,
    pub devices: Vec<OverviewDeviceSyncInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairSuccessResult {
    pub space_id: String,
    pub account_id: String,
    pub server_id: String,
    pub device_id: String,
    pub recovery_key: String,
    pub generated_new_recovery_key: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInviteResult {
    pub invite_code: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncCycleSummary {
    pub stats_pushed: usize,
    pub ops_pushed: usize,
    pub stats_pulled: usize,
    pub history_pulled: usize,
    pub edits_pulled: usize,
}

fn current_platform_name() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "macos"
    }
    #[cfg(target_os = "windows")]
    {
        "windows"
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        "linux"
    }
}

fn app_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn sql_err(e: rusqlite::Error) -> StorageError {
    StorageError::QueryFailed(e.to_string())
}

/// Pair this installation with a v2 server using a one-time pairing code and
/// either an existing recovery key (to join an existing space) or a freshly
/// generated one (for the first device in a new space).
#[allow(clippy::too_many_arguments)]
pub async fn pair_space<F, T>(
    with_conn: F,
    creds: &CredentialStore,
    transport: &T,
    server_url: &str,
    pairing_code: &str,
    device_id: &str,
    device_name: &str,
    recovery_key_input: Option<&str>,
    scope_history: bool,
    scope_edits: bool,
) -> Result<PairSuccessResult, SyncEngineError>
where
    F: Fn(&dyn Fn(&Connection) -> Result<(), StorageError>) -> Result<(), StorageError>,
    T: SyncTransport,
{
    let endpoint = server_url.trim().trim_end_matches('/');
    transport.check_server_url(endpoint)?;
    let proof = creds.pairing_proof(endpoint, device_id)?;
    let legacy_space = Mutex::new(None::<String>);
    with_conn(&|conn| {
        *legacy_space.lock().unwrap() = conn
            .query_row(
                "SELECT space_id FROM sync_spaces WHERE server_url=?1 AND device_id=?2
             ORDER BY is_active DESC, updated_at DESC LIMIT 1",
                params![endpoint, device_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(sql_err)?;
        Ok(())
    })?;
    let previous_device_token = legacy_space
        .into_inner()
        .unwrap()
        .map(|space| creds.get(&space))
        .transpose()?
        .flatten()
        .map(|c| c.device_token);
    let redeem = transport
        .pair_redeem(&PairRedeemRequest {
            pairing_code: pairing_code.trim().to_string(),
            device_id: device_id.trim().to_string(),
            device_name: device_name.trim().to_string(),
            platform: current_platform_name().to_string(),
            app_version: app_version().to_string(),
            installation_proof: Some(proof),
            previous_device_token,
        })
        .await?;

    let space_id = selfhost_space_id(&redeem.server_id, &redeem.account_id);
    let stored_creds = creds.get(&space_id)?;
    let trimmed_rk = recovery_key_input
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| stored_creds.as_ref().map(|c| c.recovery_key.as_str()));

    let (master_key, generated_new) = match trimmed_rk {
        Some(rk) => (MasterKey::from_recovery_key(rk)?, false),
        None => {
            if redeem.key_check_tag.is_some() {
                return Err(SyncEngineError::InvalidState(
                    "This sync account already has encrypted data; enter the existing recovery key to join."
                        .into(),
                ));
            }
            (MasterKey::generate(), true)
        }
    };

    let key_check_tag = master_key.derive_key_check_tag(&redeem.account_id, &space_id)?;
    if let Some(expected) = redeem.key_check_tag.as_deref() {
        if expected != key_check_tag {
            return Err(SyncEngineError::InvalidState(
                "Recovery key does not match this sync account's existing encryption key.".into(),
            ));
        }
    }

    let recovery_key = master_key.to_recovery_key();
    creds.save(
        &space_id,
        &SpaceCredentials {
            device_token: redeem.device_token.clone(),
            recovery_key: recovery_key.clone(),
        },
    )?;

    let effective_edits = scope_history && scope_edits;
    transport
        .update_device(
            &redeem.device_token,
            &redeem.device_id,
            &DeviceUpdateRequest {
                device_name: device_name.trim().to_string(),
                platform: current_platform_name().to_string(),
                app_version: app_version().to_string(),
                allow_stats: true,
                allow_history: scope_history,
                allow_edits: effective_edits,
                key_check_tag: Some(key_check_tag.clone()),
            },
        )
        .await?;

    let server_url_clean = server_url.trim().trim_end_matches('/').to_string();
    let device_name_clean = device_name.trim().to_string();
    let dev_id_owned = redeem.device_id.clone();
    let server_id_owned = redeem.server_id.clone();
    let account_id_owned = redeem.account_id.clone();
    let space_id_owned = space_id.clone();
    let tag_owned = key_check_tag.clone();

    with_conn(&|conn| {
        let tx = conn.unchecked_transaction().map_err(sql_err)?;
        upsert_sync_space_tx(
            &tx,
            &space_id_owned,
            "selfhost",
            &server_url_clean,
            &server_id_owned,
            &account_id_owned,
            &dev_id_owned,
            scope_history,
            effective_edits,
            Some(&tag_owned),
            true,
        )?;
        let now = Utc::now().to_rfc3339();
        tx.execute(
            "INSERT INTO sync_devices (space_id, device_id, device_name, platform, app_version, last_synced_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6)
             ON CONFLICT(space_id, device_id) DO UPDATE SET
                device_name = excluded.device_name,
                platform = excluded.platform,
                app_version = excluded.app_version,
                revoked_at = NULL,
                updated_at = excluded.updated_at",
            params![
                space_id_owned,
                dev_id_owned,
                device_name_clean,
                current_platform_name(),
                app_version(),
                now
            ],
        )
        .map_err(sql_err)?;

        // Ensure local totals shard & any existing local months are marked dirty to publish on first sync.
        let has_totals_shard: bool = tx
            .query_row(
                "SELECT 1 FROM sync_stats_shards WHERE space_id = ?1 AND device_id = ?2 AND shard_key = 'totals'",
                params![space_id_owned, dev_id_owned],
                |_| Ok(true),
            )
            .optional()
            .map_err(sql_err)?
            .unwrap_or(false);
        if !has_totals_shard {
            bump_local_stats_shard_tx(&tx, &space_id_owned, &dev_id_owned, "totals")?;
        }

        let months: Vec<String> = {
            let mut stmt = tx
                .prepare(
                    "SELECT DISTINCT substr(local_date, 1, 7) FROM daily_usage_stats
                     WHERE device_id = ?1 AND length(local_date) >= 7
                     UNION
                     SELECT DISTINCT substr(local_date, 1, 7) FROM daily_model_usage_stats
                     WHERE device_id = ?1 AND length(local_date) >= 7",
                )
                .map_err(sql_err)?;
            let rows = stmt
                .query_map(params![dev_id_owned], |r| r.get::<_, String>(0))
                .map_err(sql_err)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(sql_err)?;
            rows
        };
        for month in months {
            let shard_key = format!("daily:{month}");
            let exists: bool = tx
                .query_row(
                    "SELECT 1 FROM sync_stats_shards WHERE space_id = ?1 AND device_id = ?2 AND shard_key = ?3",
                    params![space_id_owned, dev_id_owned, shard_key],
                    |_| Ok(true),
                )
                .optional()
                .map_err(sql_err)?
                .unwrap_or(false);
            if !exists {
                bump_local_stats_shard_tx(&tx, &space_id_owned, &dev_id_owned, &shard_key)?;
            }
        }

        // Preserve any legacy v1 local edited_text as private versions before the first pull (M5),
        // and atomically hand off any pending v1 sync_outbox creates/deletes (M3).
        seed_all_legacy_local_edits_tx(&tx)?;
        handoff_v1_outbox_to_v2_tx(&tx, &space_id_owned, &dev_id_owned, scope_history)?;

        tx.commit().map_err(sql_err)
    })?;

    Ok(PairSuccessResult {
        space_id,
        account_id: redeem.account_id,
        server_id: redeem.server_id,
        device_id: redeem.device_id,
        recovery_key,
        generated_new_recovery_key: generated_new,
    })
}

/// Request a one-time pairing code from the server using this device's active
/// credentials and pack it locally with `server_url` + E2EE `recovery_key` into
/// a single pasteable `VXSYNC2-...` invite code for another device.
pub async fn create_device_invite<F, T>(
    with_conn: F,
    creds: &CredentialStore,
    transport: &T,
    ttl_minutes: Option<i64>,
) -> Result<DeviceInviteResult, SyncEngineError>
where
    F: Fn(&dyn Fn(&Connection) -> Result<(), StorageError>) -> Result<(), StorageError>,
    T: SyncTransport,
{
    let space_slot: Mutex<Option<SyncSpaceRecord>> = Mutex::new(None);
    with_conn(&|conn| {
        *space_slot.lock().unwrap() = active_space_in(conn)?;
        Ok(())
    })?;
    let Some(space) = space_slot.into_inner().unwrap() else {
        return Err(SyncEngineError::InvalidState(
            "No active Sync v2 space configured".into(),
        ));
    };
    if space.status == "disabled" || space.status == "revoked" {
        return Err(SyncEngineError::InvalidState(
            "Sync space is not active; reconnect this device first.".into(),
        ));
    }
    transport.check_server_url(&space.server_url)?;
    let Some(space_creds) = creds.get(&space.space_id)? else {
        return Err(SyncEngineError::InvalidState(
            "Missing local sync credentials; please pair this device again.".into(),
        ));
    };

    let issued = transport
        .create_pairing_code(&space_creds.device_token, ttl_minutes)
        .await?;
    let invite_code = encode_sync_invite(&SyncInvite {
        server_url: space.server_url,
        pairing_code: issued.pairing_code,
        recovery_key: space_creds.recovery_key,
    })?;
    Ok(DeviceInviteResult {
        invite_code,
        expires_at: issued.expires_at,
    })
}

/// Join an existing sync space by decoding a one-time `VXSYNC2-...` invite code
/// generated on another already-synced device.
#[allow(clippy::too_many_arguments)]
pub async fn join_space_with_invite<F, T>(
    with_conn: F,
    creds: &CredentialStore,
    transport: &T,
    invite_code: &str,
    device_id: &str,
    device_name: &str,
    scope_history: bool,
    scope_edits: bool,
) -> Result<PairSuccessResult, SyncEngineError>
where
    F: Fn(&dyn Fn(&Connection) -> Result<(), StorageError>) -> Result<(), StorageError>,
    T: SyncTransport,
{
    let invite = decode_sync_invite(invite_code)?;
    pair_space(
        with_conn,
        creds,
        transport,
        &invite.server_url,
        &invite.pairing_code,
        device_id,
        device_name,
        Some(&invite.recovery_key),
        scope_history,
        scope_edits,
    )
    .await
}

/// Run one complete push + pull synchronization cycle for the active sync space.
pub async fn sync_once<F, T>(
    with_conn: F,
    creds: &CredentialStore,
    transport: &T,
    text_retention_days: u32,
) -> Result<SyncCycleSummary, SyncEngineError>
where
    F: Fn(&dyn Fn(&Connection) -> Result<(), StorageError>) -> Result<(), StorageError>,
    T: SyncTransport,
{
    let space_slot: Mutex<Option<SyncSpaceRecord>> = Mutex::new(None);
    with_conn(&|conn| {
        *space_slot.lock().unwrap() = active_space_in(conn)?;
        Ok(())
    })?;
    let Some(space) = space_slot.into_inner().unwrap() else {
        return Ok(SyncCycleSummary::default());
    };
    if space.status == "disabled" || space.status == "revoked" {
        return Ok(SyncCycleSummary::default());
    }

    transport.check_server_url(&space.server_url)?;
    let Some(space_creds) = creds.get(&space.space_id)? else {
        let msg = "Missing local sync credentials; please pair this device again.".to_string();
        mark_space_status(&with_conn, &space.space_id, "error", Some(&msg), false)?;
        return Err(SyncEngineError::InvalidState(msg));
    };
    let master_key = MasterKey::from_recovery_key(&space_creds.recovery_key)?;
    let expected_tag = master_key.derive_key_check_tag(&space.account_id, &space.space_id)?;

    mark_space_status(&with_conn, &space.space_id, "syncing", None, false)?;

    let cycle_res = run_sync_cycle_inner(
        &with_conn,
        &space,
        &space_creds.device_token,
        &master_key,
        &expected_tag,
        transport,
        text_retention_days,
    )
    .await;

    match cycle_res {
        Ok(summary) => {
            mark_space_status(&with_conn, &space.space_id, "live", None, true)?;
            Ok(summary)
        }
        Err(err) => {
            let new_status = match &err {
                SyncEngineError::Transport(TransportError::DeviceRevoked(_)) => "revoked",
                _ => "error",
            };
            let msg = err.to_string();
            let _ = mark_space_status(&with_conn, &space.space_id, new_status, Some(&msg), false);
            Err(err)
        }
    }
}

fn mark_space_status<F>(
    with_conn: &F,
    space_id: &str,
    status: &str,
    last_error: Option<&str>,
    touch_last_sync: bool,
) -> Result<(), StorageError>
where
    F: Fn(&dyn Fn(&Connection) -> Result<(), StorageError>) -> Result<(), StorageError>,
{
    let now = Utc::now().to_rfc3339();
    with_conn(&|conn| {
        if touch_last_sync {
            conn.execute(
                "UPDATE sync_spaces SET status = ?2, last_error = ?3, last_sync_at = ?4, updated_at = ?4 WHERE space_id = ?1",
                params![space_id, status, last_error, now],
            )
            .map_err(sql_err)?;
        } else {
            conn.execute(
                "UPDATE sync_spaces SET status = ?2, last_error = ?3, updated_at = ?4 WHERE space_id = ?1",
                params![space_id, status, last_error, now],
            )
            .map_err(sql_err)?;
        }
        Ok(())
    })
}

fn estimate_submission_json_bytes(sub: &EnvelopeSubmission) -> usize {
    serde_json::to_vec(sub).map(|v| v.len() + 2).unwrap_or(
        sub.op_id.len() + sub.object_id.len() + sub.nonce.len() + sub.ciphertext.len() + 160,
    )
}

fn current_scope_version_in<F>(with_conn: &F, space_id: &str) -> Result<Option<i64>, SyncEngineError>
where
    F: Fn(&dyn Fn(&Connection) -> Result<(), StorageError>) -> Result<(), StorageError>,
{
    let slot: Mutex<Option<i64>> = Mutex::new(None);
    with_conn(&|conn| {
        let ver: Option<i64> = conn
            .query_row(
                "SELECT scope_version FROM sync_spaces WHERE space_id = ?1 AND is_active = 1",
                params![space_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(sql_err)?;
        *slot.lock().unwrap() = ver;
        Ok(())
    })?;
    Ok(slot.into_inner().unwrap())
}

async fn run_sync_cycle_inner<F, T>(
    with_conn: &F,
    space: &SyncSpaceRecord,
    device_token: &str,
    master_key: &MasterKey,
    expected_tag: &str,
    transport: &T,
    text_retention_days: u32,
) -> Result<SyncCycleSummary, SyncEngineError>
where
    F: Fn(&dyn Fn(&Connection) -> Result<(), StorageError>) -> Result<(), StorageError>,
    T: SyncTransport,
{
    let mut summary = SyncCycleSummary::default();
    let mut non_fatal_oversized_errors: Vec<String> = Vec::new();
    let cycle_scope_version = space.scope_version;

    // 1. Refresh account & devices metadata + verify key_check_tag
    let account = transport.get_account(device_token).await?;
    if let Some(remote_tag) = account.key_check_tag.as_deref() {
        if remote_tag != expected_tag {
            return Err(SyncEngineError::InvalidState(
                "Recovery key mismatch with server sync space.".into(),
            ));
        }
    }
    let devices_clone = account.devices.clone();
    let space_id = space.space_id.clone();
    with_conn(&|conn| {
        let tx = conn.unchecked_transaction().map_err(sql_err)?;
        let now = Utc::now().to_rfc3339();
        for d in &devices_clone {
            // N1: Always advance `last_synced_at` monotonically from server's authoritative `last_seen_at`.
            tx.execute(
                "INSERT INTO sync_devices (space_id, device_id, device_name, platform, app_version, last_synced_at, revoked_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(space_id, device_id) DO UPDATE SET
                    device_name = excluded.device_name,
                    platform = excluded.platform,
                    app_version = excluded.app_version,
                    last_synced_at = CASE
                        WHEN sync_devices.last_synced_at IS NULL OR excluded.last_synced_at > sync_devices.last_synced_at
                        THEN excluded.last_synced_at
                        ELSE sync_devices.last_synced_at
                    END,
                    revoked_at = excluded.revoked_at,
                    updated_at = excluded.updated_at",
                params![
                    space_id,
                    d.device_id,
                    d.device_name,
                    d.platform,
                    d.app_version,
                    d.last_seen_at,
                    d.revoked_at,
                    now
                ],
            )
            .map_err(sql_err)?;
            tx.execute(
                "INSERT INTO device_registry (device_id, device_name, updated_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(device_id) DO UPDATE SET device_name = excluded.device_name, updated_at = excluded.updated_at",
                params![d.device_id, d.device_name, now],
            )
            .map_err(sql_err)?;
        }
        tx.commit().map_err(sql_err)
    })?;

    // 1.5 (S6): If local scope was updated while offline (`scope_version > acked_scope_version`),
    // push the latest device scope to the server BEFORE pushing any domain envelopes.
    if space.scope_version > space.acked_scope_version {
        let dev_name_slot: Mutex<String> = Mutex::new(space.device_id.clone());
        with_conn(&|conn| {
            let name: Option<String> = conn
                .query_row(
                    "SELECT device_name FROM sync_devices WHERE space_id = ?1 AND device_id = ?2 LIMIT 1",
                    params![space.space_id, space.device_id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(sql_err)?;
            if let Some(n) = name.filter(|s| !s.trim().is_empty()) {
                *dev_name_slot.lock().unwrap() = n;
            }
            Ok(())
        })?;
        let device_name = dev_name_slot.into_inner().unwrap();
        let effective_edits = space.scope_history && space.scope_edits;
        transport
            .update_device(
                device_token,
                &space.device_id,
                &DeviceUpdateRequest {
                    device_name,
                    platform: current_platform_name().to_string(),
                    app_version: app_version().to_string(),
                    allow_stats: true,
                    allow_history: space.scope_history,
                    allow_edits: effective_edits,
                    key_check_tag: space.key_check_tag.clone(),
                },
            )
            .await?;
        let sid = space.space_id.clone();
        let ver = space.scope_version;
        with_conn(&|conn| {
            let tx = conn.unchecked_transaction().map_err(sql_err)?;
            ack_sync_scope_version_tx(&tx, &sid, ver)?;
            tx.commit().map_err(sql_err)
        })?;
    }

    let baseline_key = format!(
        "own_stats_reconciled:{}:{}",
        space.space_id, space.device_id
    );
    let reconciled = Mutex::new(false);
    with_conn(&|conn| {
        *reconciled.lock().unwrap() = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM user_config WHERE key=?1)",
                [&baseline_key],
                |r| r.get(0),
            )
            .map_err(sql_err)?;
        Ok(())
    })?;
    if !reconciled.into_inner().unwrap() {
        let mut cursor = 0;
        let mut own = Vec::new();
        loop {
            let page = transport
                .pull_envelopes(device_token, SyncDomain::Stats, cursor, DEFAULT_PULL_LIMIT)
                .await?;
            for item in page.items.iter().filter(|i| i.device_id == space.device_id) {
                let plain = decrypt_envelope(
                    master_key,
                    EnvelopeContext {
                        account_id: &space.account_id,
                        space_id: &space.space_id,
                        domain: SyncDomain::Stats,
                        object_id: &item.object_id,
                        revision: item.revision,
                        op_id: &item.op_id,
                    },
                    &item.nonce,
                    &item.ciphertext,
                )?;
                let payload: StatsShardPayload = serde_json::from_slice(&plain)
                    .map_err(|e| StorageError::SerializeFailed(e.to_string()))?;
                let (device, key, rev) = match &payload {
                    StatsShardPayload::Totals(t) => {
                        (&t.device_id, "totals".to_string(), t.revision)
                    }
                    StatsShardPayload::DailyMonth(d) => {
                        (&d.device_id, format!("daily:{}", d.month), d.revision)
                    }
                };
                if device != &space.device_id
                    || stats_object_id(device, &key) != item.object_id
                    || rev != item.revision
                    || rev < 1
                {
                    return Err(SyncEngineError::InvalidState(
                        "stats envelope and payload identity disagree".into(),
                    ));
                }
                own.push(payload);
            }
            if !page.has_more {
                break;
            }
            if page.next_cursor <= cursor {
                return Err(SyncEngineError::InvalidState(
                    "stats cursor did not advance".into(),
                ));
            }
            cursor = page.next_cursor;
        }
        with_conn(&|conn| {
            let tx = conn.unchecked_transaction().map_err(sql_err)?;
            for shard in &own {
                super::stats_baseline::reconcile(&tx, &space.space_id, &space.device_id, shard)?;
            }
            tx.execute(
                "INSERT OR REPLACE INTO user_config(key,value) VALUES (?1,'1')",
                [&baseline_key],
            )
            .map_err(sql_err)?;
            tx.commit().map_err(sql_err)
        })?;
    }

    // 2. Push dirty local stats shards (`local_revision > acked_revision`) in size- and count-bounded batches (S9)
    let dirty_shards: Mutex<Vec<(String, i64, StatsShardPayload)>> = Mutex::new(Vec::new());
    with_conn(&|conn| {
        let mut stmt = conn
            .prepare(
                "SELECT shard_key, local_revision
                 FROM sync_stats_shards
                 WHERE space_id = ?1 AND device_id = ?2 AND local_revision > acked_revision
                 ORDER BY shard_key ASC",
            )
            .map_err(sql_err)?;
        let rows = stmt
            .query_map(params![space.space_id, space.device_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })
            .map_err(sql_err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sql_err)?;

        let mut list = dirty_shards.lock().unwrap();
        for (shard_key, rev) in rows {
            if shard_key == "totals" {
                let payload = build_local_totals_shard_in(conn, &space.device_id, rev)?;
                list.push((shard_key, rev, StatsShardPayload::Totals(payload)));
            } else if let Some(month) = shard_key.strip_prefix("daily:") {
                let payload = build_local_daily_month_shard_in(conn, &space.device_id, month, rev)?;
                list.push((shard_key, rev, StatsShardPayload::DailyMonth(payload)));
            }
        }
        Ok(())
    })?;

    let dirty_shards = dirty_shards.into_inner().unwrap();
    if !dirty_shards.is_empty() {
        let mut prepared_stats: Vec<(EnvelopeSubmission, String, i64, usize)> =
            Vec::with_capacity(dirty_shards.len());
        for (shard_key, rev, payload) in &dirty_shards {
            let plaintext = serde_json::to_vec(payload)
                .map_err(|e| SyncEngineError::Storage(StorageError::SerializeFailed(e.to_string())))?;
            let object_id = stats_object_id(&space.device_id, shard_key);
            let op_id = format!("stats:{}:{}:{}", space.device_id, shard_key, rev);
            let sealed = encrypt_envelope(
                master_key,
                EnvelopeContext {
                    account_id: &space.account_id,
                    space_id: &space.space_id,
                    domain: SyncDomain::Stats,
                    object_id: &object_id,
                    revision: *rev,
                    op_id: &op_id,
                },
                &plaintext,
            )?;
            let sub = EnvelopeSubmission {
                op_id: op_id.clone(),
                domain: SyncDomain::Stats,
                object_id,
                revision: *rev,
                nonce: sealed.nonce_b64,
                ciphertext: sealed.ciphertext_b64,
            };
            let est = estimate_submission_json_bytes(&sub);
            if est > MAX_SAFE_PUSH_BODY_BYTES {
                non_fatal_oversized_errors.push(format!(
                    "Stats shard '{}' (rev {}) exceeds 2 MiB server limit ({} bytes)",
                    shard_key, rev, est
                ));
                continue;
            }
            prepared_stats.push((sub, shard_key.clone(), *rev, est));
        }

        let mut idx = 0;
        while idx < prepared_stats.len() {
            let mut batch_subs = Vec::new();
            let mut batch_meta = Vec::new();
            let mut batch_bytes = 32usize;
            while idx < prepared_stats.len() && batch_subs.len() < MAX_PUSH_BATCH_LEN {
                let (_, _, _, est) = &prepared_stats[idx];
                if !batch_subs.is_empty() && batch_bytes + *est > MAX_SAFE_PUSH_BODY_BYTES {
                    break;
                }
                let (sub, sk, rev, sz) = &prepared_stats[idx];
                batch_subs.push(sub.clone());
                batch_meta.push((sub.op_id.clone(), sk.clone(), *rev));
                batch_bytes += *sz;
                idx += 1;
            }

            let resp = transport
                .push_envelopes(
                    device_token,
                    &PushEnvelopesRequest {
                        envelopes: batch_subs,
                    },
                )
                .await?;

            with_conn(&|conn| {
                let tx = conn.unchecked_transaction().map_err(sql_err)?;
                let now = Utc::now().to_rfc3339();
                for res in &resp.results {
                    if matches!(
                        res.status,
                        EnvelopeStatus::Duplicate | EnvelopeStatus::StaleIgnored
                    ) {
                        tx.execute("DELETE FROM user_config WHERE key=?1", [&baseline_key])
                            .map_err(sql_err)?;
                    }
                    if res.status == EnvelopeStatus::Accepted {
                        if let Some((_, shard_key, rev)) =
                            batch_meta.iter().find(|(oid, _, _)| oid == &res.op_id)
                        {
                            tx.execute(
                                "UPDATE sync_stats_shards
                                 SET acked_revision = MAX(acked_revision, ?4), last_synced_at = ?5, updated_at = ?5
                                 WHERE space_id = ?1 AND device_id = ?2 AND shard_key = ?3",
                                params![space.space_id, space.device_id, shard_key, rev, now],
                            )
                            .map_err(sql_err)?;
                        }
                    }
                }
                tx.commit().map_err(sql_err)
            })?;
            summary.stats_pushed += resp.results.len();
        }
    }

    // 3. Push pending outbox history/edit ops in byte- and count-bounded batches,
    //    verifying `scope_version` before mutating queue or sending (S5, S9).
    let scope = space.scope_config();
    loop {
        if current_scope_version_in(with_conn, &space.space_id)? != Some(cycle_scope_version) {
            break;
        }
        let batch_slot: Mutex<Vec<(String, String, String, String, String, bool)>> =
            Mutex::new(Vec::new());
        with_conn(&|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT o.op_id, o.domain, o.op_type, o.record_id, o.payload_json,
                            COALESCE(h.sync_eligible, 1)
                     FROM sync_outbox_ops o
                     LEFT JOIN history_record h ON h.id = o.record_id
                     WHERE o.space_id = ?1
                       AND (o.last_error IS NULL OR o.last_error NOT LIKE 'oversized:%')
                     ORDER BY o.created_at ASC, o.op_id ASC
                     LIMIT ?2",
                )
                .map_err(sql_err)?;
            let rows = stmt
                .query_map(params![space.space_id, PUSH_BATCH_SIZE as i64], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, i32>(5)? != 0,
                    ))
                })
                .map_err(sql_err)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(sql_err)?;
            *batch_slot.lock().unwrap() = rows;
            Ok(())
        })?;
        let batch = batch_slot.into_inner().unwrap();
        if batch.is_empty() {
            break;
        }

        let mut disallowed_ids = Vec::new();
        let mut oversized_ops: Vec<(String, String)> = Vec::new();
        let mut prepared_ops: Vec<(EnvelopeSubmission, usize)> = Vec::new();
        for (op_id, domain_str, op_type, record_id, payload_json, sync_eligible) in &batch {
            let record_was_shared = *sync_eligible || op_type == "history.delete";
            if !can_send_outbox_op(&scope, op_type, record_was_shared) {
                disallowed_ids.push(op_id.clone());
                continue;
            }
            let domain: SyncDomain = domain_str
                .parse()
                .map_err(|e: String| SyncEngineError::InvalidState(e))?;
            let sealed = encrypt_envelope(
                master_key,
                EnvelopeContext {
                    account_id: &space.account_id,
                    space_id: &space.space_id,
                    domain,
                    object_id: record_id,
                    revision: 0,
                    op_id,
                },
                payload_json.as_bytes(),
            )?;
            let sub = EnvelopeSubmission {
                op_id: op_id.clone(),
                domain,
                object_id: record_id.clone(),
                revision: 0,
                nonce: sealed.nonce_b64,
                ciphertext: sealed.ciphertext_b64,
            };
            let est = estimate_submission_json_bytes(&sub);
            if est > MAX_SAFE_PUSH_BODY_BYTES {
                let msg = format!(
                    "{OVERSIZED_ERROR_PREFIX} op '{}' (record '{}') exceeds 2 MiB server limit ({} bytes)",
                    op_id, record_id, est
                );
                non_fatal_oversized_errors.push(msg.clone());
                oversized_ops.push((op_id.clone(), msg));
                continue;
            }
            prepared_ops.push((sub, est));
        }

        if !oversized_ops.is_empty() {
            with_conn(&|conn| {
                let tx = conn.unchecked_transaction().map_err(sql_err)?;
                for (op_id, err_text) in &oversized_ops {
                    tx.execute(
                        "UPDATE sync_outbox_ops SET attempt_count = attempt_count + 1, last_error = ?2 WHERE op_id = ?1",
                        params![op_id, err_text],
                    )
                    .map_err(sql_err)?;
                }
                tx.commit().map_err(sql_err)
            })?;
        }

        if !disallowed_ids.is_empty() {
            // S5: Re-verify scope_version inside the write transaction before dropping ops.
            let scope_changed = std::sync::atomic::AtomicBool::new(false);
            with_conn(&|conn| {
                let tx = conn.unchecked_transaction().map_err(sql_err)?;
                let current_ver: Option<i64> = tx
                    .query_row(
                        "SELECT scope_version FROM sync_spaces WHERE space_id = ?1 AND is_active = 1",
                        params![space.space_id],
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(sql_err)?;
                if current_ver != Some(cycle_scope_version) {
                    scope_changed.store(true, std::sync::atomic::Ordering::Relaxed);
                    return Ok(());
                }
                for op_id in &disallowed_ids {
                    tx.execute("DELETE FROM sync_outbox_ops WHERE op_id = ?1", params![op_id])
                        .map_err(sql_err)?;
                }
                tx.commit().map_err(sql_err)
            })?;
            if scope_changed.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
        }

        if prepared_ops.is_empty() {
            if batch.len() < PUSH_BATCH_SIZE {
                break;
            }
            continue;
        }

        // Send prepared_ops in byte-bounded sub-batches <= MAX_SAFE_PUSH_BODY_BYTES (S9)
        let mut p_idx = 0;
        let mut aborted_for_scope_change = false;
        while p_idx < prepared_ops.len() {
            if current_scope_version_in(with_conn, &space.space_id)? != Some(cycle_scope_version) {
                aborted_for_scope_change = true;
                break;
            }
            let mut sub_batch = Vec::new();
            let mut sub_bytes = 32usize;
            while p_idx < prepared_ops.len() && sub_batch.len() < MAX_PUSH_BATCH_LEN {
                let (sub, est) = &prepared_ops[p_idx];
                if !sub_batch.is_empty() && sub_bytes + *est > MAX_SAFE_PUSH_BODY_BYTES {
                    break;
                }
                sub_batch.push(sub.clone());
                sub_bytes += *est;
                p_idx += 1;
            }

            let sent_ids: Vec<String> = sub_batch.iter().map(|s| s.op_id.clone()).collect();
            let push_res = transport
                .push_envelopes(
                    device_token,
                    &PushEnvelopesRequest {
                        envelopes: sub_batch.clone(),
                    },
                )
                .await;

            match push_res {
                Ok(resp) => {
                    with_conn(&|conn| {
                        let tx = conn.unchecked_transaction().map_err(sql_err)?;
                        for r in &resp.results {
                            if matches!(
                                r.status,
                                EnvelopeStatus::Accepted | EnvelopeStatus::Duplicate
                            ) {
                                tx.execute(
                                    "DELETE FROM sync_outbox_ops WHERE op_id = ?1",
                                    params![r.op_id],
                                )
                                .map_err(sql_err)?;
                            }
                        }
                        tx.commit().map_err(sql_err)
                    })?;
                    summary.ops_pushed += resp.results.len();
                }
                Err(TransportError::PayloadTooLarge(msg)) if sub_batch.len() == 1 => {
                    // Isolate single oversized op so remaining independent ops and pull can continue (S9)
                    let err_text = format!("{OVERSIZED_ERROR_PREFIX} {msg}");
                    non_fatal_oversized_errors.push(err_text.clone());
                    let op_id = sent_ids[0].clone();
                    let _ = with_conn(&|conn| {
                        let tx = conn.unchecked_transaction().map_err(sql_err)?;
                        tx.execute(
                            "UPDATE sync_outbox_ops SET attempt_count = attempt_count + 1, last_error = ?2 WHERE op_id = ?1",
                            params![op_id, err_text],
                        )
                        .map_err(sql_err)?;
                        tx.commit().map_err(sql_err)
                    });
                }
                Err(TransportError::PayloadTooLarge(_)) if sub_batch.len() > 1 => {
                    // Server rejected multi-item batch on size -> fall back to solo sends for this slice
                    for solo in sub_batch {
                        let solo_id = solo.op_id.clone();
                        match transport
                            .push_envelopes(
                                device_token,
                                &PushEnvelopesRequest {
                                    envelopes: vec![solo],
                                },
                            )
                            .await
                        {
                            Ok(resp) => {
                                with_conn(&|conn| {
                                    let tx = conn.unchecked_transaction().map_err(sql_err)?;
                                    for r in &resp.results {
                                        if matches!(
                                            r.status,
                                            EnvelopeStatus::Accepted | EnvelopeStatus::Duplicate
                                        ) {
                                            tx.execute(
                                                "DELETE FROM sync_outbox_ops WHERE op_id = ?1",
                                                params![r.op_id],
                                            )
                                            .map_err(sql_err)?;
                                        }
                                    }
                                    tx.commit().map_err(sql_err)
                                })?;
                                summary.ops_pushed += resp.results.len();
                            }
                            Err(TransportError::PayloadTooLarge(msg)) => {
                                let err_text = format!("{OVERSIZED_ERROR_PREFIX} {msg}");
                                non_fatal_oversized_errors.push(err_text.clone());
                                let _ = with_conn(&|conn| {
                                    let tx = conn.unchecked_transaction().map_err(sql_err)?;
                                    tx.execute(
                                        "UPDATE sync_outbox_ops SET attempt_count = attempt_count + 1, last_error = ?2 WHERE op_id = ?1",
                                        params![solo_id, err_text],
                                    )
                                    .map_err(sql_err)?;
                                    tx.commit().map_err(sql_err)
                                });
                            }
                            Err(err) => {
                                let err_text = err.to_string();
                                let _ = with_conn(&|conn| {
                                    let tx = conn.unchecked_transaction().map_err(sql_err)?;
                                    tx.execute(
                                        "UPDATE sync_outbox_ops SET attempt_count = attempt_count + 1, last_error = ?2 WHERE op_id = ?1",
                                        params![solo_id, err_text],
                                    )
                                    .map_err(sql_err)?;
                                    tx.commit().map_err(sql_err)
                                });
                                return Err(SyncEngineError::Transport(err));
                            }
                        }
                    }
                }
                Err(err) => {
                    let err_text = err.to_string();
                    let _ = with_conn(&|conn| {
                        let tx = conn.unchecked_transaction().map_err(sql_err)?;
                        for op_id in &sent_ids {
                            tx.execute(
                                "UPDATE sync_outbox_ops SET attempt_count = attempt_count + 1, last_error = ?2 WHERE op_id = ?1",
                                params![op_id, err_text],
                            )
                            .map_err(sql_err)?;
                        }
                        tx.commit().map_err(sql_err)
                    });
                    return Err(SyncEngineError::Transport(err));
                }
            }
        }

        if aborted_for_scope_change || batch.len() < PUSH_BATCH_SIZE {
            break;
        }
    }

    // 4. Pull every authorized domain page-by-page, committing each page + cursor atomically
    for domain in scope.pull_domains() {
        let mut cursor = match domain {
            SyncDomain::Stats => space.stats_cursor,
            SyncDomain::History => space.history_cursor,
            SyncDomain::Edits => space.edits_cursor,
        };
        let cursor_col = match domain {
            SyncDomain::Stats => "stats_cursor",
            SyncDomain::History => "history_cursor",
            SyncDomain::Edits => "edits_cursor",
        };

        loop {
            let page = transport
                .pull_envelopes(device_token, domain, cursor, DEFAULT_PULL_LIMIT)
                .await?;

            // Decrypt the entire page before opening the SQLite write transaction.
            let mut decrypted_stats = Vec::new();
            let mut decrypted_ops = Vec::new();
            for item in &page.items {
                let plaintext = decrypt_envelope(
                    master_key,
                    EnvelopeContext {
                        account_id: &space.account_id,
                        space_id: &space.space_id,
                        domain: item.domain,
                        object_id: &item.object_id,
                        revision: item.revision,
                        op_id: &item.op_id,
                    },
                    &item.nonce,
                    &item.ciphertext,
                )?;
                match item.domain {
                    SyncDomain::Stats => {
                        let shard: StatsShardPayload =
                            serde_json::from_slice(&plaintext).map_err(|e| {
                                SyncEngineError::Storage(StorageError::SerializeFailed(
                                    e.to_string(),
                                ))
                            })?;
                        // Never let an older/echoed remote copy overwrite our own live local totals.
                        let shard_owner = match &shard {
                            StatsShardPayload::Totals(t) => &t.device_id,
                            StatsShardPayload::DailyMonth(d) => &d.device_id,
                        };
                        if shard_owner != &space.device_id {
                            decrypted_stats.push(shard);
                        }
                    }
                    SyncDomain::History | SyncDomain::Edits => {
                        let op: HistoryOpPayload =
                            serde_json::from_slice(&plaintext).map_err(|e| {
                                SyncEngineError::Storage(StorageError::SerializeFailed(
                                    e.to_string(),
                                ))
                            })?;
                        decrypted_ops.push(op);
                    }
                }
            }

            let next_cursor = page.next_cursor;
            with_conn(&|conn| {
                let tx = conn.unchecked_transaction().map_err(sql_err)?;
                for shard in &decrypted_stats {
                    match shard {
                        StatsShardPayload::Totals(t) => {
                            apply_remote_stats_totals_tx(&tx, &space.space_id, t)?;
                        }
                        StatsShardPayload::DailyMonth(d) => {
                            apply_remote_stats_daily_month_tx(&tx, &space.space_id, d)?;
                        }
                    }
                }
                for op in &decrypted_ops {
                    apply_remote_history_op_tx(
                        &tx,
                        &space.space_id,
                        &space.device_id,
                        op,
                        text_retention_days,
                    )?;
                }
                tx.execute(
                    &format!(
                        "UPDATE sync_spaces SET {cursor_col} = MAX({cursor_col}, ?2), updated_at = ?3 WHERE space_id = ?1"
                    ),
                    params![space.space_id, next_cursor, Utc::now().to_rfc3339()],
                )
                .map_err(sql_err)?;
                tx.commit().map_err(sql_err)
            })?;

            // S8: Clean up local audio files for any remote deletions applied in this page.
            let _ = with_conn(&|conn| {
                let _ = flush_pending_audio_deletes_in(conn)?;
                Ok(())
            });

            match domain {
                SyncDomain::Stats => summary.stats_pulled += page.items.len(),
                SyncDomain::History => summary.history_pulled += page.items.len(),
                SyncDomain::Edits => summary.edits_pulled += page.items.len(),
            }

            cursor = next_cursor;
            if !page.has_more {
                break;
            }
        }
    }

    // Retry any previously deferred local audio deletions even when no new pages arrived.
    let _ = with_conn(&|conn| {
        let _ = flush_pending_audio_deletes_in(conn)?;
        Ok(())
    });

    if !non_fatal_oversized_errors.is_empty() {
        return Err(SyncEngineError::InvalidState(
            non_fatal_oversized_errors.join("; "),
        ));
    }

    Ok(summary)
}

/// Explicitly import surviving local edits on already-shared dictations into
/// the v2 `edits` outbox (Section 2.1: separate opt-in from enabling edits sync).
pub fn import_existing_edits_in(conn: &Connection) -> Result<usize, StorageError> {
    let Some(space) = active_space_in(conn)? else {
        return Err(StorageError::InvalidInput(
            "No active Sync v2 space configured".into(),
        ));
    };
    if !space.scope_config().effective_edits() {
        return Err(StorageError::InvalidInput(
            "Enable history and edits sync before importing existing edits".into(),
        ));
    }

    let tx = conn.unchecked_transaction().map_err(sql_err)?;
    let imported = import_existing_edits_tx(&tx, &space)?;
    tx.commit().map_err(sql_err)?;
    Ok(imported)
}

/// Resolve a concurrent manual edit conflict by writing a new manual version
/// (or `ManualRestore` when `chosen_text` is `None` or equals original output)
/// whose `parent_edit_ids` covers every active head (or the explicit visible heads
/// captured by the UI when the dialog opened, S3).
pub fn resolve_edit_conflict_in(
    conn: &Connection,
    record_id: &str,
    chosen_text: Option<&str>,
) -> Result<(), StorageError> {
    resolve_edit_conflict_with_parents_in(conn, record_id, chosen_text, None)
}

pub fn resolve_edit_conflict_with_parents_in(
    conn: &Connection,
    record_id: &str,
    chosen_text: Option<&str>,
    expected_parent_edit_ids: Option<Vec<String>>,
) -> Result<(), StorageError> {
    let output: String = conn
        .query_row(
            "SELECT text FROM history_record WHERE id = ?1",
            params![record_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sql_err)?
        .ok_or_else(|| StorageError::NotFound(record_id.to_string()))?;

    let tx = conn.unchecked_transaction().map_err(sql_err)?;
    let edit_id = uuid::Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();
    match chosen_text.filter(|t| *t != output) {
        Some(text) => {
            on_local_edit_applied_with_parents_tx(
                &tx,
                record_id,
                &edit_id,
                EditKind::Manual,
                Some(text),
                &now,
                None,
                expected_parent_edit_ids,
            )?;
        }
        None => {
            on_local_edit_applied_with_parents_tx(
                &tx,
                record_id,
                &edit_id,
                EditKind::ManualRestore,
                None,
                &now,
                None,
                expected_parent_edit_ids,
            )?;
        }
    }
    tx.commit().map_err(sql_err)
}

pub fn get_status_snapshot_in(
    conn: &Connection,
    creds: &CredentialStore,
    device_id: &str,
    default_device_name: &str,
) -> Result<SyncV2StatusSnapshot, StorageError> {
    let space = active_space_in(conn)?;
    let devices = get_overview_devices_in(conn, device_id)?;
    let Some(space) = space else {
        return Ok(SyncV2StatusSnapshot {
            configured: false,
            space_id: None,
            server_url: None,
            account_id: None,
            device_id: device_id.to_string(),
            device_name: default_device_name.to_string(),
            status: "disabled".to_string(),
            last_error: None,
            last_sync_at: None,
            scope_history: false,
            scope_edits: false,
            scope_sync_pending: false,
            pending_ops_count: 0,
            pending_stats_count: 0,
            has_credentials: false,
            devices,
        });
    };

    let pending_ops_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sync_outbox_ops WHERE space_id = ?1",
            params![space.space_id],
            |r| r.get(0),
        )
        .map_err(sql_err)?;
    let pending_stats_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sync_stats_shards
             WHERE space_id = ?1 AND device_id = ?2 AND local_revision > acked_revision",
            params![space.space_id, space.device_id],
            |r| r.get(0),
        )
        .map_err(sql_err)?;

    let device_name = devices
        .iter()
        .find(|d| d.is_local)
        .map(|d| d.device_name.clone())
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| default_device_name.to_string());

    let has_credentials = creds
        .get(&space.space_id)
        .map(|opt| opt.is_some())
        .unwrap_or(false);
    let scope_sync_pending = space.scope_version > space.acked_scope_version;

    Ok(SyncV2StatusSnapshot {
        configured: true,
        space_id: Some(space.space_id),
        server_url: Some(space.server_url),
        account_id: Some(space.account_id),
        device_id: space.device_id,
        device_name,
        status: space.status,
        last_error: space.last_error,
        last_sync_at: space.last_sync_at,
        scope_history: space.scope_history,
        scope_edits: space.scope_edits,
        scope_sync_pending,
        pending_ops_count,
        pending_stats_count,
        has_credentials,
        devices,
    })
}

/// Tauri-managed background Sync v2 service.
#[derive(Clone)]
pub struct SyncV2Service {
    inner: Arc<Mutex<SyncV2ServiceInner>>,
    /// Serializes background sync cycles, manual `sync_now`, and scope updates (S5/N2).
    sync_lock: Arc<tokio::sync::Mutex<()>>,
}

impl Default for SyncV2Service {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(SyncV2ServiceInner::default())),
            sync_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }
}

#[derive(Default)]
struct SyncV2ServiceInner {
    app_handle: Option<AppHandle>,
    data_dir: Option<PathBuf>,
    cancel: Option<CancellationToken>,
    flush_tx: Option<mpsc::UnboundedSender<()>>,
}

/// Reconcile this installation's external device identity (`device-identity.json`)
/// on startup (S4).
///
/// - On an existing machine's first boot after upgrading to S4, binds the current
///   `sync_device_id` to this machine's fingerprint on disk.
/// - On a cloned data directory (different machine fingerprint) or a pasted DB from
///   another device, rotates `user_config.sync_device_id` to a fresh local UUID,
///   disables any cloned `sync_spaces` rows belonging to the old device ID, and
///   removes their copied credentials so the clone can never silently hijack the
///   original device's token or overwrite its stats shards.
pub fn reconcile_startup_device_identity_in(
    conn: &Connection,
    data_dir: &std::path::Path,
    machine_fingerprint: &str,
) -> Result<ResolvedDeviceIdentity, SyncEngineError> {
    let db_device_id: Option<String> = conn
        .query_row(
            "SELECT value FROM user_config WHERE key = 'sync_device_id' LIMIT 1",
            [],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?
        .filter(|s| !s.trim().is_empty())
        .or_else(|| active_space_in(conn).ok().flatten().map(|s| s.device_id));

    let id_store = DeviceIdentityStore::in_data_dir(data_dir);
    // A pre-S4 original installation has credentials beside its v2 DB. A
    // DB-only copy has neither external identity nor credentials for its spaces.
    let mut db_only_clone = false;
    if !id_store.exists() {
        let mut stmt = conn
            .prepare("SELECT space_id FROM sync_spaces WHERE device_id=?1")
            .map_err(sql_err)?;
        let spaces = stmt
            .query_map(params![db_device_id], |r| r.get::<_, String>(0))
            .map_err(sql_err)?;
        let creds = CredentialStore::in_data_dir(data_dir);
        for space in spaces {
            db_only_clone = true;
            if creds.get(&space.map_err(sql_err)?)?.is_some() {
                db_only_clone = false;
                break;
            }
        }
    }
    let mut resolved = id_store.resolve_for_machine(
        if db_only_clone {
            None
        } else {
            db_device_id.as_deref()
        },
        machine_fingerprint,
    )?;
    resolved.rotated_for_new_install |= db_only_clone;

    let tx = conn.unchecked_transaction().map_err(sql_err)?;
    if resolved.rotated_for_new_install
        || db_device_id.as_deref() != Some(resolved.device_id.as_str())
    {
        tx.execute(
            "INSERT INTO user_config (key, value) VALUES ('sync_device_id', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![resolved.device_id],
        )
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
    }

    if resolved.rotated_for_new_install {
        let mut stmt = tx
            .prepare("SELECT space_id,server_url,device_id FROM sync_spaces WHERE device_id != ?1")
            .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
        let mismatched_spaces: Vec<(String, String, String)> = stmt
            .query_map(params![resolved.device_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .map_err(|e| StorageError::QueryFailed(e.to_string()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| StorageError::QueryFailed(e.to_string()))?;

        if !mismatched_spaces.is_empty() {
            let now = Utc::now().to_rfc3339();
            tx.execute(
                "UPDATE sync_spaces
                 SET is_active = 0,
                     status = 'disabled',
                     last_error = 'Cloned installation detected; please pair this device again.',
                     updated_at = ?2
                 WHERE device_id != ?1",
                params![resolved.device_id, now],
            )
            .map_err(|e| StorageError::QueryFailed(e.to_string()))?;

            let creds = CredentialStore::in_data_dir(data_dir);
            for (space_id, url, device) in mismatched_spaces {
                creds.remove(&space_id, &url, &device)?;
            }
        }
    }

    tx.commit().map_err(sql_err)?;
    Ok(resolved)
}

impl SyncV2Service {
    pub fn init(&self, app_handle: &AppHandle, app_data_dir: PathBuf) {
        let fp = current_machine_fingerprint();
        match storage::with_db(|conn| {
            reconcile_startup_device_identity_in(conn, &app_data_dir, &fp)
                .map_err(|e| StorageError::QueryFailed(e.to_string()))
        }) {
            Ok(resolved) if resolved.rotated_for_new_install => {
                log::warn!(
                    "[sync-v2] S4 detected cloned database/data directory on startup; rotated device_id to {} and disabled cloned sync spaces",
                    resolved.device_id
                );
            }
            Ok(_) => {}
            Err(err) => {
                log::warn!("[sync-v2] failed to reconcile startup device identity: {err}");
                return;
            }
        }
        {
            let mut inner = self.inner.lock().expect("sync_v2 lock");
            inner.app_handle = Some(app_handle.clone());
            inner.data_dir = Some(app_data_dir.clone());
        }
    }

    pub fn data_dir(&self) -> Result<PathBuf, String> {
        let inner = self.inner.lock().expect("sync_v2 lock");
        inner
            .data_dir
            .clone()
            .ok_or_else(|| "SyncV2Service not initialized".to_string())
    }

    pub fn credential_store(&self) -> Result<CredentialStore, String> {
        Ok(CredentialStore::in_data_dir(&self.data_dir()?))
    }

    pub fn sync_lock(&self) -> Arc<tokio::sync::Mutex<()>> {
        Arc::clone(&self.sync_lock)
    }

    pub fn start_if_active(&self) {
        let active = storage::with_db(active_space_in).ok().flatten();
        if active.is_some_and(|s| s.status != "disabled" && s.status != "revoked") {
            self.restart_worker();
        } else {
            self.stop();
        }
    }

    pub fn restart_worker(&self) {
        let mut inner = self.inner.lock().expect("sync_v2 lock");
        if let Some(cancel) = inner.cancel.take() {
            cancel.cancel();
        }
        let Some(app) = inner.app_handle.clone() else {
            return;
        };
        let Some(data_dir) = inner.data_dir.clone() else {
            return;
        };
        let cancel = CancellationToken::new();
        let (tx, rx) = mpsc::unbounded_channel();
        inner.cancel = Some(cancel.clone());
        inner.flush_tx = Some(tx);
        drop(inner);

        tauri::async_runtime::spawn(run_background_loop(
            app,
            data_dir,
            cancel,
            rx,
            Arc::clone(&self.sync_lock),
        ));
    }

    pub fn stop(&self) {
        let mut inner = self.inner.lock().expect("sync_v2 lock");
        if let Some(cancel) = inner.cancel.take() {
            cancel.cancel();
        }
        inner.flush_tx = None;
    }

    pub fn notify_local_change(&self) {
        let inner = self.inner.lock().expect("sync_v2 lock");
        if let Some(tx) = inner.flush_tx.as_ref() {
            let _ = tx.send(());
        }
    }

    pub fn emit_status(&self) {
        let (app, data_dir) = {
            let inner = self.inner.lock().expect("sync_v2 lock");
            (inner.app_handle.clone(), inner.data_dir.clone())
        };
        if let (Some(app), Some(data_dir)) = (app, data_dir) {
            emit_v2_status(&app, &data_dir);
        }
    }
}

fn emit_v2_status(app: &AppHandle, data_dir: &PathBuf) {
    let creds = CredentialStore::in_data_dir(data_dir);
    let device_id = storage::get_or_create_device_id().unwrap_or_default();
    let default_name = storage::get_settings_cached()
        .map(|s| s.sync_device_name.clone())
        .unwrap_or_default();
    if let Ok(snapshot) = storage::with_db(|conn| {
        get_status_snapshot_in(conn, &creds, &device_id, &default_name)
    }) {
        let _ = app.emit("sync_v2:state", snapshot);
    }
}

async fn run_background_loop(
    app: AppHandle,
    data_dir: PathBuf,
    cancel: CancellationToken,
    mut flush_rx: mpsc::UnboundedReceiver<()>,
    sync_lock: Arc<tokio::sync::Mutex<()>>,
) {
    let creds = CredentialStore::in_data_dir(&data_dir);

    // Spawn SSE watcher that pokes `sse_notify_tx` whenever the server broadcasts a change.
    let (sse_tx, mut sse_rx) = mpsc::unbounded_channel::<()>();
    let sse_cancel = cancel.clone();
    let sse_creds = creds.clone();
    tauri::async_runtime::spawn(async move {
        let mut backoff_secs = 2u64;
        while !sse_cancel.is_cancelled() {
            let space = storage::with_db(active_space_in).ok().flatten();
            let Some(space) = space else {
                break;
            };
            if space.status == "disabled" || space.status == "revoked" {
                break;
            }
            let token = match sse_creds.get(&space.space_id) {
                Ok(Some(c)) => c.device_token,
                _ => {
                    tokio::select! {
                        _ = sse_cancel.cancelled() => break,
                        _ = tokio::time::sleep(Duration::from_secs(10)) => continue,
                    }
                }
            };
            let transport = match SelfHostTransport::new(&space.server_url) {
                Ok(t) => t,
                Err(_) => break,
            };

            match transport.subscribe_sse(&token).await {
                Ok(resp) => {
                    backoff_secs = 2;
                    let _ = sse_tx.send(()); // Catch up on reconnect
                    let mut stream = resp.bytes_stream();
                    loop {
                        tokio::select! {
                            _ = sse_cancel.cancelled() => return,
                            item = stream.next() => match item {
                                Some(Ok(bytes)) => {
                                    if bytes.windows(6).any(|w| w == b"notify") {
                                        let _ = sse_tx.send(());
                                    }
                                }
                                Some(Err(_)) | None => break,
                            }
                        }
                    }
                }
                Err(TransportError::DeviceRevoked(_)) => break,
                Err(_) => {}
            }

            tokio::select! {
                _ = sse_cancel.cancelled() => break,
                _ = tokio::time::sleep(Duration::from_secs(backoff_secs)) => {
                    backoff_secs = (backoff_secs * 2).min(60);
                }
            }
        }
    });

    // Trigger immediate initial cycle + react to local changes / SSE notify / periodic poll.
    let mut periodic = tokio::time::interval(Duration::from_secs(60));
    periodic.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut sse_alive = true;

    loop {
        let guard = sync_lock.lock().await;
        if cancel.is_cancelled() {
            break;
        }
        let space = storage::with_db(active_space_in).ok().flatten();
        let Some(space) = space else {
            cancel.cancel();
            break;
        };
        if space.status == "disabled" || space.status == "revoked" {
            cancel.cancel();
            break;
        }

        if let Ok(transport) = SelfHostTransport::new(&space.server_url) {
            let retention_days = storage::get_settings_cached()
                .map(|s| s.text_retention_days)
                .unwrap_or(0);
            let res = sync_once(
                |f| storage::with_db(f),
                &creds,
                &transport,
                retention_days,
            )
            .await;
            emit_v2_status(&app, &data_dir);
            match res {
                Ok(summary) => {
                    if summary.stats_pulled > 0
                        || summary.history_pulled > 0
                        || summary.edits_pulled > 0
                    {
                        let _ = app.emit(
                            "history:updated",
                            serde_json::json!({ "source": "sync_v2" }),
                        );
                    }
                }
                Err(SyncEngineError::Transport(TransportError::DeviceRevoked(_))) => {
                    cancel.cancel();
                    break;
                }
                Err(_) => {}
            }
        }

        drop(guard);

        // S7: Explicitly handle closed channels so an exited SSE task never busy-loops.
        tokio::select! {
            _ = cancel.cancelled() => break,
            msg = flush_rx.recv() => {
                if msg.is_none() {
                    break;
                }
                while flush_rx.try_recv().is_ok() {}
            }
            msg = sse_rx.recv(), if sse_alive => {
                if msg.is_none() {
                    sse_alive = false;
                } else {
                    while sse_rx.try_recv().is_ok() {}
                }
            }
            _ = periodic.tick() => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::HistoryRecord;
    use crate::sync_v2::store::{
        init_schema, list_edit_conflict_alternatives_in, on_local_edit_applied_tx,
        on_local_history_deleted_tx, on_local_history_inserted_tx, update_sync_scope_tx,
    };
    use crate::sync_v2::transport::BoxFuture;
    use std::collections::{HashMap, HashSet};
    use voicex_sync_protocol::{
        AccountInfoResponse, DeviceInfo, EnvelopeItem, EnvelopeResult, PairRedeemResponse,
        PullEnvelopesResponse, PushEnvelopesResponse,
    };

    /// Deterministic in-memory v2 server transport implementing exact server
    /// semantics (single-use pairing, revocation, idempotent op_id, monotonic
    /// stats_shards, superseded skipping, and per-domain cursor pagination).
    #[derive(Default)]
    struct MockV2Server {
        state: Mutex<MockServerState>,
    }

    #[derive(Default)]
    struct MockServerState {
        server_id: String,
        account_id: String,
        key_check_tag: Option<String>,
        next_seq: i64,
        pairing_codes: HashSet<String>,
        tokens: HashMap<String, (String, bool)>, // token -> (device_id, revoked)
        devices: HashMap<String, DeviceInfo>,
        envelopes: Vec<(EnvelopeItem, bool)>, // (item, superseded)
        op_id_to_seq: HashMap<String, i64>,
        stats_revs: HashMap<(String, String), (i64, i64)>, // (dev, shard) -> (rev, seq)
    }

    impl MockV2Server {
        fn new() -> Self {
            let s = Self::default();
            {
                let mut g = s.state.lock().unwrap();
                g.server_id = "srv-test".into();
                g.account_id = "acc-test".into();
            }
            s
        }

        fn issue_code(&self, code: &str) {
            self.state
                .lock()
                .unwrap()
                .pairing_codes
                .insert(code.to_string());
        }

        fn revoke(&self, device_id: &str) {
            let mut g = self.state.lock().unwrap();
            for (_, (did, revoked)) in g.tokens.iter_mut() {
                if did == device_id {
                    *revoked = true;
                }
            }
            if let Some(d) = g.devices.get_mut(device_id) {
                d.revoked_at = Some(Utc::now().to_rfc3339());
            }
        }

        fn auth_device(&self, token: &str) -> Result<String, TransportError> {
            let g = self.state.lock().unwrap();
            match g.tokens.get(token) {
                Some((did, false)) => Ok(did.clone()),
                Some((_, true)) => Err(TransportError::DeviceRevoked("revoked".into())),
                None => Err(TransportError::Unauthorized("bad token".into())),
            }
        }
    }

    impl SyncTransport for MockV2Server {
        fn server_url(&self) -> &str {
            "http://mock"
        }

        fn pair_redeem<'a>(
            &'a self,
            req: &'a PairRedeemRequest,
        ) -> BoxFuture<'a, Result<PairRedeemResponse, TransportError>> {
            Box::pin(async move {
                let mut g = self.state.lock().unwrap();
                if !g.pairing_codes.remove(&req.pairing_code) {
                    return Err(TransportError::PairingInvalidOrExpired("used".into()));
                }
                let token = format!("tok-{}", req.device_id);
                g.tokens.insert(token.clone(), (req.device_id.clone(), false));
                g.devices.insert(
                    req.device_id.clone(),
                    DeviceInfo {
                        device_id: req.device_id.clone(),
                        device_name: req.device_name.clone(),
                        platform: req.platform.clone(),
                        app_version: req.app_version.clone(),
                        allow_stats: true,
                        allow_history: true,
                        allow_edits: true,
                        last_seen_at: Utc::now().to_rfc3339(),
                        revoked_at: None,
                    },
                );
                Ok(PairRedeemResponse {
                    account_id: g.account_id.clone(),
                    server_id: g.server_id.clone(),
                    device_id: req.device_id.clone(),
                    device_token: token,
                    key_check_tag: g.key_check_tag.clone(),
                })
            })
        }

        fn create_pairing_code<'a>(
            &'a self,
            device_token: &'a str,
            ttl_minutes: Option<i64>,
        ) -> BoxFuture<'a, Result<voicex_sync_protocol::CreatePairingCodeResponse, TransportError>>
        {
            Box::pin(async move {
                let _ = self.auth_device(device_token)?;
                let mut g = self.state.lock().unwrap();
                let code = format!("VXP2-MOCK-{}", uuid::Uuid::new_v4().simple());
                g.pairing_codes.insert(code.clone());
                let expires_at =
                    (Utc::now() + chrono::Duration::minutes(ttl_minutes.unwrap_or(15))).to_rfc3339();
                Ok(voicex_sync_protocol::CreatePairingCodeResponse {
                    pairing_code: code,
                    expires_at,
                })
            })
        }

        fn get_account<'a>(
            &'a self,
            device_token: &'a str,
        ) -> BoxFuture<'a, Result<AccountInfoResponse, TransportError>> {
            Box::pin(async move {
                let _ = self.auth_device(device_token)?;
                let g = self.state.lock().unwrap();
                Ok(AccountInfoResponse {
                    account_id: g.account_id.clone(),
                    server_id: g.server_id.clone(),
                    latest_seq: g.next_seq,
                    key_check_tag: g.key_check_tag.clone(),
                    devices: g.devices.values().cloned().collect(),
                })
            })
        }

        fn update_device<'a>(
            &'a self,
            device_token: &'a str,
            device_id: &'a str,
            req: &'a DeviceUpdateRequest,
        ) -> BoxFuture<'a, Result<DeviceInfo, TransportError>> {
            Box::pin(async move {
                let authed = self.auth_device(device_token)?;
                if authed != device_id {
                    return Err(TransportError::ScopeForbidden("wrong device".into()));
                }
                let mut g = self.state.lock().unwrap();
                if let Some(tag) = &req.key_check_tag {
                    if g.key_check_tag.is_none() {
                        g.key_check_tag = Some(tag.clone());
                    }
                }
                let info = DeviceInfo {
                    device_id: device_id.to_string(),
                    device_name: req.device_name.clone(),
                    platform: req.platform.clone(),
                    app_version: req.app_version.clone(),
                    allow_stats: req.allow_stats,
                    allow_history: req.allow_history,
                    allow_edits: req.allow_edits,
                    last_seen_at: Utc::now().to_rfc3339(),
                    revoked_at: None,
                };
                g.devices.insert(device_id.to_string(), info.clone());
                Ok(info)
            })
        }

        fn push_envelopes<'a>(
            &'a self,
            device_token: &'a str,
            req: &'a PushEnvelopesRequest,
        ) -> BoxFuture<'a, Result<PushEnvelopesResponse, TransportError>> {
            Box::pin(async move {
                let device_id = self.auth_device(device_token)?;
                let mut g = self.state.lock().unwrap();
                let mut results = Vec::new();
                for sub in &req.envelopes {
                    if let Some(&seq) = g.op_id_to_seq.get(&sub.op_id) {
                        results.push(EnvelopeResult {
                            op_id: sub.op_id.clone(),
                            status: EnvelopeStatus::Duplicate,
                            seq,
                        });
                        continue;
                    }
                    if sub.domain == SyncDomain::Stats {
                        let (_, shard_key) =
                            voicex_sync_protocol::parse_stats_object_id(&sub.object_id).unwrap();
                        let key = (device_id.clone(), shard_key.to_string());
                        if let Some(&(cur_rev, cur_seq)) = g.stats_revs.get(&key) {
                            if sub.revision <= cur_rev {
                                results.push(EnvelopeResult {
                                    op_id: sub.op_id.clone(),
                                    status: EnvelopeStatus::StaleIgnored,
                                    seq: cur_seq,
                                });
                                continue;
                            }
                            for (it, sup) in g.envelopes.iter_mut() {
                                if it.seq == cur_seq {
                                    *sup = true;
                                }
                            }
                        }
                        g.next_seq += 1;
                        let seq = g.next_seq;
                        g.stats_revs.insert(key, (sub.revision, seq));
                        g.op_id_to_seq.insert(sub.op_id.clone(), seq);
                        g.envelopes.push((
                            EnvelopeItem {
                                seq,
                                op_id: sub.op_id.clone(),
                                domain: sub.domain,
                                device_id: device_id.clone(),
                                object_id: sub.object_id.clone(),
                                revision: sub.revision,
                                nonce: sub.nonce.clone(),
                                ciphertext: sub.ciphertext.clone(),
                                created_at: Utc::now().to_rfc3339(),
                            },
                            false,
                        ));
                        results.push(EnvelopeResult {
                            op_id: sub.op_id.clone(),
                            status: EnvelopeStatus::Accepted,
                            seq,
                        });
                        continue;
                    }

                    g.next_seq += 1;
                    let seq = g.next_seq;
                    g.op_id_to_seq.insert(sub.op_id.clone(), seq);
                    g.envelopes.push((
                        EnvelopeItem {
                            seq,
                            op_id: sub.op_id.clone(),
                            domain: sub.domain,
                            device_id: device_id.clone(),
                            object_id: sub.object_id.clone(),
                            revision: 0,
                            nonce: sub.nonce.clone(),
                            ciphertext: sub.ciphertext.clone(),
                            created_at: Utc::now().to_rfc3339(),
                        },
                        false,
                    ));
                    results.push(EnvelopeResult {
                        op_id: sub.op_id.clone(),
                        status: EnvelopeStatus::Accepted,
                        seq,
                    });
                }
                Ok(PushEnvelopesResponse {
                    results,
                    latest_seq: g.next_seq,
                })
            })
        }

        fn pull_envelopes<'a>(
            &'a self,
            device_token: &'a str,
            domain: SyncDomain,
            since: i64,
            limit: usize,
        ) -> BoxFuture<'a, Result<PullEnvelopesResponse, TransportError>> {
            Box::pin(async move {
                let _ = self.auth_device(device_token)?;
                let g = self.state.lock().unwrap();
                let matching: Vec<EnvelopeItem> = g
                    .envelopes
                    .iter()
                    .filter(|(it, sup)| !sup && it.domain == domain && it.seq > since)
                    .map(|(it, _)| it.clone())
                    .collect();
                let has_more = matching.len() > limit;
                let items: Vec<EnvelopeItem> = matching.into_iter().take(limit).collect();
                let next_cursor = if has_more {
                    items.last().map(|i| i.seq).unwrap_or(since)
                } else {
                    items
                        .last()
                        .map(|i| i.seq)
                        .unwrap_or(since)
                        .max(g.next_seq)
                };
                Ok(PullEnvelopesResponse {
                    items,
                    next_cursor,
                    has_more,
                })
            })
        }
    }

    struct TestClient {
        device_id: String,
        conn: Mutex<Connection>,
        creds: CredentialStore,
        _dir: tempfile::TempDir,
    }

    impl TestClient {
        fn new(device_id: &str) -> Self {
            let dir = tempfile::TempDir::new().unwrap();
            let conn = Connection::open_in_memory().unwrap();
            conn.execute_batch(
                r#"
                CREATE TABLE history_record (
                    id TEXT PRIMARY KEY, timestamp TEXT NOT NULL, text TEXT NOT NULL,
                    original_text TEXT, ai_correction_applied INTEGER DEFAULT 0, llm_invoked INTEGER DEFAULT 0,
                    mode TEXT NOT NULL, duration_ms INTEGER NOT NULL, audio_path TEXT,
                    is_final INTEGER DEFAULT 1, error_code INTEGER DEFAULT 0, error_message TEXT,
                    source_device_id TEXT, asr_model_name TEXT, llm_model_name TEXT,
                    edited_text TEXT, edit_source TEXT, edited_at TEXT,
                    target_app_platform TEXT, target_app_key TEXT, target_app_name TEXT
                );
                CREATE TABLE usage_stats (
                    id INTEGER PRIMARY KEY CHECK (id = 1), total_duration_ms INTEGER DEFAULT 0,
                    total_characters INTEGER DEFAULT 0, llm_correction_count INTEGER DEFAULT 0,
                    last_updated TEXT, total_recording_count INTEGER DEFAULT 0,
                    tts_read_count INTEGER DEFAULT 0, tts_translate_count INTEGER DEFAULT 0,
                    tts_characters INTEGER DEFAULT 0, tts_duration_ms INTEGER DEFAULT 0, tts_llm_count INTEGER DEFAULT 0
                );
                INSERT INTO usage_stats (id) VALUES (1);
                CREATE TABLE device_usage_stats (
                    device_id TEXT PRIMARY KEY, total_duration_ms INTEGER DEFAULT 0,
                    total_characters INTEGER DEFAULT 0, llm_correction_count INTEGER DEFAULT 0,
                    last_updated TEXT, total_recording_count INTEGER DEFAULT 0,
                    tts_read_count INTEGER DEFAULT 0, tts_translate_count INTEGER DEFAULT 0,
                    tts_characters INTEGER DEFAULT 0, tts_duration_ms INTEGER DEFAULT 0, tts_llm_count INTEGER DEFAULT 0
                );
                CREATE TABLE daily_usage_stats (
                    device_id TEXT NOT NULL, local_date TEXT NOT NULL, feature_kind TEXT NOT NULL,
                    platform TEXT NOT NULL, app_key TEXT NOT NULL, app_name TEXT NOT NULL,
                    usage_count INTEGER NOT NULL DEFAULT 0, total_characters INTEGER NOT NULL DEFAULT 0,
                    total_duration_ms INTEGER NOT NULL DEFAULT 0, llm_count INTEGER NOT NULL DEFAULT 0,
                    last_used_at TEXT NOT NULL,
                    PRIMARY KEY (device_id, local_date, feature_kind, platform, app_key)
                );
                CREATE TABLE user_config (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                CREATE TABLE device_registry (device_id TEXT PRIMARY KEY, device_name TEXT NOT NULL, updated_at TEXT NOT NULL);
                "#,
            )
            .unwrap();
            crate::post_edit::records::create_schema(&conn).unwrap();
            conn.execute(
                "INSERT INTO user_config (key, value) VALUES ('sync_device_id', ?1)",
                params![device_id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO device_usage_stats (device_id) VALUES (?1)",
                params![device_id],
            )
            .unwrap();
            init_schema(&conn).unwrap();
            let creds = CredentialStore::in_data_dir(dir.path());
            Self {
                device_id: device_id.to_string(),
                conn: Mutex::new(conn),
                creds,
                _dir: dir,
            }
        }

        fn with_db(&self, f: &dyn Fn(&Connection) -> Result<(), StorageError>) -> Result<(), StorageError> {
            let g = self.conn.lock().unwrap();
            f(&g)
        }

        fn dictate(&self, id: &str, text: &str, timestamp: &str, local_date: &str) {
            let rec = HistoryRecord {
                id: id.to_string(),
                timestamp: timestamp.to_string(),
                text: text.to_string(),
                original_text: None,
                ai_correction_applied: true,
                llm_invoked: true,
                mode: "assistant_hands_free".to_string(),
                duration_ms: 1500,
                audio_path: None,
                is_final: true,
                error_code: 0,
                error_message: None,
                source_device_id: Some(self.device_id.clone()),
                source_device_name: None,
                asr_model_name: Some("Qwen / flash".to_string()),
                llm_model_name: Some("DeepSeek / flash".to_string()),
                edited_text: None,
                target_app_platform: Some("macos".to_string()),
                target_app_key: Some("com.apple.Notes".to_string()),
                target_app_name: Some("Notes".to_string()),
                has_edit_conflict: false,
                active_edit_id: None,
            };
            let g = self.conn.lock().unwrap();
            let tx = g.unchecked_transaction().unwrap();
            tx.execute(
                "INSERT INTO history_record (id, timestamp, text, ai_correction_applied, llm_invoked, mode, duration_ms, source_device_id, asr_model_name, llm_model_name, target_app_platform, target_app_key, target_app_name)
                 VALUES (?1, ?2, ?3, 1, 1, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    rec.id, rec.timestamp, rec.text, rec.mode, rec.duration_ms, rec.source_device_id,
                    rec.asr_model_name, rec.llm_model_name, rec.target_app_platform, rec.target_app_key, rec.target_app_name
                ],
            )
            .unwrap();
            tx.execute(
                "UPDATE device_usage_stats SET total_recording_count = total_recording_count + 1, total_characters = total_characters + ?2, total_duration_ms = total_duration_ms + ?3, llm_correction_count = llm_correction_count + 1 WHERE device_id = ?1",
                params![self.device_id, text.chars().count() as i64, rec.duration_ms],
            )
            .unwrap();
            on_local_history_inserted_tx(&tx, &rec, local_date).unwrap();
            tx.commit().unwrap();
        }
    }

    #[test]
    fn own_totals_with_an_existing_revision_keep_both_baselines_and_publish_once() {
        let client = TestClient::new("source");
        let conn = client.conn.lock().unwrap();
        conn.execute(
            "UPDATE device_usage_stats SET total_recording_count=12 WHERE device_id='source'",
            [],
        )
        .unwrap();
        let mut server = build_local_totals_shard_in(&conn, "source", 1).unwrap();
        server.dictation.recording_count = 10;
        server.tts.read_count = 3;
        super::super::stats_baseline::reconcile(
            &conn,
            "space",
            "source",
            &StatsShardPayload::Totals(server),
        )
        .unwrap();
        let merged = build_local_totals_shard_in(&conn, "source", 2).unwrap();
        assert_eq!(
            (merged.dictation.recording_count, merged.tts.read_count),
            (12, 3)
        );
        let revisions = || {
            conn.query_row(
                "SELECT local_revision,acked_revision FROM sync_stats_shards",
                [],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
            )
            .unwrap()
        };
        assert_eq!(revisions(), (2, 1));
        super::super::stats_baseline::reconcile(
            &conn,
            "space",
            "source",
            &StatsShardPayload::Totals(merged),
        )
        .unwrap();
        assert_eq!(revisions(), (2, 2));
    }

    #[tokio::test]
    async fn e2e_two_devices_stats_only_vs_history_edits_conflicts_delete_and_revocation() {
        let srv = MockV2Server::new();
        let mac = TestClient::new("mac-1");
        let win = TestClient::new("win-1");
        let stats_dev = TestClient::new("win-stats-only");

        // 1. Pair Mac (generates new recovery key) + Win (joins with same recovery key) + StatsOnly
        srv.issue_code("CODE-MAC");
        let mac_pair = pair_space(
            |f| mac.with_db(f),
            &mac.creds,
            &srv,
            "http://mock",
            "CODE-MAC",
            "mac-1",
            "MBP",
            None,
            true,
            true,
        )
        .await
        .unwrap();
        assert!(mac_pair.generated_new_recovery_key);

        // Wrong recovery key is rejected before pairing succeeds
        srv.issue_code("CODE-BAD");
        let bad_rk = MasterKey::generate().to_recovery_key();
        assert!(pair_space(
            |f| win.with_db(f),
            &win.creds,
            &srv,
            "http://mock",
            "CODE-BAD",
            "win-1",
            "Yoga",
            Some(&bad_rk),
            true,
            true,
        )
        .await
        .is_err());

        srv.issue_code("CODE-WIN");
        pair_space(
            |f| win.with_db(f),
            &win.creds,
            &srv,
            "http://mock",
            "CODE-WIN",
            "win-1",
            "Yoga",
            Some(&mac_pair.recovery_key),
            true,
            true,
        )
        .await
        .unwrap();

        srv.issue_code("CODE-STATS");
        pair_space(
            |f| stats_dev.with_db(f),
            &stats_dev.creds,
            &srv,
            "http://mock",
            "CODE-STATS",
            "win-stats-only",
            "WorkPC",
            Some(&mac_pair.recovery_key),
            false,
            false,
        )
        .await
        .unwrap();

        // 2. Mac creates a dictation; stats_dev creates a private dictation
        let now_ts = Utc::now().to_rfc3339();
        let today = Utc::now().format("%Y-%m-%d").to_string();
        mac.dictate("r-shared", "hello world", &now_ts, &today);
        stats_dev.dictate("r-private", "secret work note", &now_ts, &today);

        sync_once(|f| mac.with_db(f), &mac.creds, &srv, 0).await.unwrap();
        sync_once(|f| stats_dev.with_db(f), &stats_dev.creds, &srv, 0).await.unwrap();
        sync_once(|f| win.with_db(f), &win.creds, &srv, 0).await.unwrap();
        sync_once(|f| mac.with_db(f), &mac.creds, &srv, 0).await.unwrap();

        // Verify stats converged across all 3 devices (2 total dictations), while
        // stats_dev never pulled r-shared and never pushed r-private!
        {
            let sg = stats_dev.conn.lock().unwrap();
            let hist_cnt: i64 = sg
                .query_row("SELECT COUNT(*) FROM history_record", [], |r| r.get(0))
                .unwrap();
            assert_eq!(hist_cnt, 1); // Only its own r-private
            let all = crate::sync_v2::store::get_overview_usage_stats_in(
                &sg,
                "win-stats-only",
                crate::sync_v2::store::OverviewScope::All,
            )
            .unwrap();
            assert_eq!(all.total_recording_count, 2);
        }
        {
            let wg = win.conn.lock().unwrap();
            let private_leaked: i64 = wg
                .query_row(
                    "SELECT COUNT(*) FROM history_record WHERE id = 'r-private'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(private_leaked, 0);
        }

        // 3. Concurrent offline manual edits on r-shared from Mac & Win -> both converge with recoverable alternative
        {
            let mg = mac.conn.lock().unwrap();
            on_local_edit_applied_tx(
                &mg,
                "r-shared",
                "ed-mac",
                EditKind::Manual,
                Some("mac version"),
                &now_ts,
                None,
            )
            .unwrap();
        }
        {
            let wg = win.conn.lock().unwrap();
            on_local_edit_applied_tx(
                &wg,
                "r-shared",
                "ed-win",
                EditKind::Manual,
                Some("win version"),
                &now_ts,
                None,
            )
            .unwrap();
        }
        sync_once(|f| mac.with_db(f), &mac.creds, &srv, 0).await.unwrap();
        sync_once(|f| win.with_db(f), &win.creds, &srv, 0).await.unwrap();
        sync_once(|f| mac.with_db(f), &mac.creds, &srv, 0).await.unwrap();

        let (mac_disp, win_disp) = {
            let mg = mac.conn.lock().unwrap();
            let wg = win.conn.lock().unwrap();
            let m_text: Option<String> = mg
                .query_row("SELECT edited_text FROM history_record WHERE id = 'r-shared'", [], |r| r.get(0))
                .unwrap();
            let w_text: Option<String> = wg
                .query_row("SELECT edited_text FROM history_record WHERE id = 'r-shared'", [], |r| r.get(0))
                .unwrap();
            assert_eq!(list_edit_conflict_alternatives_in(&mg, "r-shared").unwrap().len(), 1);
            assert_eq!(list_edit_conflict_alternatives_in(&wg, "r-shared").unwrap().len(), 1);
            (m_text, w_text)
        };
        assert_eq!(mac_disp, win_disp);

        // Resolve conflict on Mac -> Win converges with 0 conflicts
        {
            let mg = mac.conn.lock().unwrap();
            resolve_edit_conflict_in(&mg, "r-shared", Some("mac version")).unwrap();
        }
        sync_once(|f| mac.with_db(f), &mac.creds, &srv, 0).await.unwrap();
        sync_once(|f| win.with_db(f), &win.creds, &srv, 0).await.unwrap();
        {
            let wg = win.conn.lock().unwrap();
            assert!(list_edit_conflict_alternatives_in(&wg, "r-shared").unwrap().is_empty());
            let w_text: Option<String> = wg
                .query_row("SELECT edited_text FROM history_record WHERE id = 'r-shared'", [], |r| r.get(0))
                .unwrap();
            assert_eq!(w_text.as_deref(), Some("mac version"));
        }

        // 4. User deletes r-shared on Mac -> removed on Win while dictation totals stay 2
        {
            let mg = mac.conn.lock().unwrap();
            let tx = mg.unchecked_transaction().unwrap();
            on_local_history_deleted_tx(&tx, "r-shared", true).unwrap();
            tx.execute("DELETE FROM history_record WHERE id = 'r-shared'", []).unwrap();
            tx.commit().unwrap();
        }
        sync_once(|f| mac.with_db(f), &mac.creds, &srv, 0).await.unwrap();
        sync_once(|f| win.with_db(f), &win.creds, &srv, 0).await.unwrap();
        {
            let wg = win.conn.lock().unwrap();
            let cnt: i64 = wg
                .query_row("SELECT COUNT(*) FROM history_record WHERE id = 'r-shared'", [], |r| r.get(0))
                .unwrap();
            assert_eq!(cnt, 0);
            let all = crate::sync_v2::store::get_overview_usage_stats_in(
                &wg,
                "win-1",
                crate::sync_v2::store::OverviewScope::All,
            )
            .unwrap();
            assert_eq!(all.total_recording_count, 2);
        }

        // 5. Revoke Win -> Win gets DeviceRevoked & status='revoked', while Mac keeps syncing cleanly!
        srv.revoke("win-1");
        let win_err = sync_once(|f| win.with_db(f), &win.creds, &srv, 0).await;
        assert!(matches!(
            win_err,
            Err(SyncEngineError::Transport(TransportError::DeviceRevoked(_)))
        ));
        assert!(sync_once(|f| mac.with_db(f), &mac.creds, &srv, 0).await.is_ok());

        // 6. Verify widening stats_dev from stats-only to history pulls existing history/tombstones cleanly
        {
            let sg = stats_dev.conn.lock().unwrap();
            update_sync_scope_tx(&sg, &mac_pair.space_id, true, false).unwrap();
        }
        let widened = sync_once(|f| stats_dev.with_db(f), &stats_dev.creds, &srv, 0)
            .await
            .unwrap();
        assert!(widened.history_pulled >= 1);
    }

    #[tokio::test]
    async fn s6_s9_offline_scope_sync_large_stats_batching_and_oversized_isolation() {
        let srv = MockV2Server::new();
        srv.issue_code("VXP2-S6-A");
        srv.issue_code("VXP2-S6-B");
        let dev_a = TestClient::new("dev-a");
        let dev_b = TestClient::new("dev-b");

        let pair_a = pair_space(
            |f| dev_a.with_db(f),
            &dev_a.creds,
            &srv,
            "http://mock",
            "VXP2-S6-A",
            "dev-a",
            "MacA",
            None,
            false,
            false,
        )
        .await
        .unwrap();
        let _pair_b = pair_space(
            |f| dev_b.with_db(f),
            &dev_b.creds,
            &srv,
            "http://mock",
            "VXP2-S6-B",
            "dev-b",
            "WinB",
            Some(&pair_a.recovery_key),
            true,
            true,
        )
        .await
        .unwrap();

        // S6: Dev A widens scope offline (`scope_version` > `acked_scope_version`).
        // Before sync_once, status snapshot reports `scope_sync_pending == true`.
        {
            let g = dev_a.conn.lock().unwrap();
            update_sync_scope_tx(&g, &pair_a.space_id, true, true).unwrap();
            let snap = get_status_snapshot_in(&g, &dev_a.creds, "dev-a", "MacA").unwrap();
            assert!(snap.scope_sync_pending);
        }
        // S9a: Seed 105 dirty monthly stats shards (> MAX_PUSH_BATCH_LEN=100) on dev A
        {
            let g = dev_a.conn.lock().unwrap();
            for i in 0..105 {
                let year = 2015 + (i / 12);
                let month = (i % 12) + 1;
                let sk = format!("daily:{year:04}-{month:02}");
                bump_local_stats_shard_tx(&g, &pair_a.space_id, "dev-a", &sk).unwrap();
            }
        }
        // First sync_once pushes updated scope (S6) + batches all 106 stats shards cleanly!
        let sum = sync_once(|f| dev_a.with_db(f), &dev_a.creds, &srv, 0)
            .await
            .unwrap();
        assert_eq!(sum.stats_pushed, 106); // 105 monthly + 1 totals
        {
            let g = dev_a.conn.lock().unwrap();
            let snap = get_status_snapshot_in(&g, &dev_a.creds, "dev-a", "MacA").unwrap();
            assert!(!snap.scope_sync_pending);
            assert_eq!(snap.pending_stats_count, 0);
        }

        // S9b: Dev A has 1 oversized (>2 MiB) history record + 1 normal history record,
        // while Dev B already pushed a record. Dev A's oversized record must be isolated
        // (flagged `oversized:...`) while its normal record pushes AND Dev B's record still pulls!
        dev_b.dictate("from-b", "hello from b", "2026-11-24T12:00:00Z", "2026-11-24");
        sync_once(|f| dev_b.with_db(f), &dev_b.creds, &srv, 0)
            .await
            .unwrap();

        let huge_text = "x".repeat(MAX_PUSH_BODY_BYTES);
        dev_a.dictate("a-huge", &huge_text, "2026-11-24T12:01:00Z", "2026-11-24");
        dev_a.dictate("a-small", "normal short text", "2026-11-24T12:02:00Z", "2026-11-24");

        let res = sync_once(|f| dev_a.with_db(f), &dev_a.creds, &srv, 0).await;
        // Surfaces visible error for the oversized record...
        assert!(res.is_err());
        // ...while STILL pushing `a-small` and pulling `from-b`!
        sync_once(|f| dev_b.with_db(f), &dev_b.creds, &srv, 0)
            .await
            .unwrap();
        {
            let ga = dev_a.conn.lock().unwrap();
            let pulled_from_b: i64 = ga
                .query_row("SELECT COUNT(*) FROM history_record WHERE id = 'from-b'", [], |r| r.get(0))
                .unwrap();
            assert_eq!(pulled_from_b, 1);
            let oversized_err: String = ga
                .query_row(
                    "SELECT last_error FROM sync_outbox_ops WHERE record_id = 'a-huge'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(oversized_err.starts_with(OVERSIZED_ERROR_PREFIX));
        }
        {
            let gb = dev_b.conn.lock().unwrap();
            let got_small: i64 = gb
                .query_row("SELECT COUNT(*) FROM history_record WHERE id = 'a-small'", [], |r| r.get(0))
                .unwrap();
            assert_eq!(got_small, 1);
        }
    }

    #[test]
    fn s4_startup_reconciliation_binds_existing_and_neutralizes_full_dir_clone() {
        let dir = tempfile::TempDir::new().unwrap();
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE user_config (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE history_record (id TEXT PRIMARY KEY, timestamp TEXT NOT NULL, text TEXT NOT NULL, mode TEXT NOT NULL, duration_ms INTEGER NOT NULL);
             CREATE TABLE device_usage_stats (device_id TEXT PRIMARY KEY);",
        )
        .unwrap();
        crate::sync_v2::store::init_schema(&conn).unwrap();
        conn.execute(
            "INSERT INTO user_config (key, value) VALUES ('sync_device_id', 'dev-orig')",
            [],
        )
        .unwrap();
        upsert_sync_space_tx(
            &conn,
            "selfhost:srv1:acc1",
            "selfhost",
            "http://127.0.0.1:8787",
            "srv1",
            "acc1",
            "dev-orig",
            true,
            true,
            Some("tag"),
            true,
        )
        .unwrap();
        let creds = CredentialStore::in_data_dir(dir.path());
        creds
            .save(
                "selfhost:srv1:acc1",
                &SpaceCredentials {
                    device_token: "vxd2_orig_token".into(),
                    recovery_key: "VXK2-TEST".into(),
                },
            )
            .unwrap();

        // First boot on original machine A: binds `device-identity.json` without disabling sync
        let r1 = reconcile_startup_device_identity_in(&conn, dir.path(), "fp-machine-a").unwrap();
        assert_eq!(r1.device_id, "dev-orig");
        assert!(!r1.rotated_for_new_install);
        assert!(!r1.existed_on_disk);
        assert!(active_space_in(&conn).unwrap().is_some());
        assert!(creds.get("selfhost:srv1:acc1").unwrap().is_some());

        // The normal startup path must also distinguish a DB-only copy.
        let db_only = tempfile::tempdir().unwrap();
        let mut copied = Connection::open_in_memory().unwrap();
        rusqlite::backup::Backup::new(&conn, &mut copied)
            .unwrap()
            .run_to_completion(128, Duration::ZERO, None)
            .unwrap();
        let clone_id =
            reconcile_startup_device_identity_in(&copied, db_only.path(), "fp-machine-c").unwrap();
        assert_ne!(clone_id.device_id, r1.device_id);
        assert!(active_space_in(&copied).unwrap().is_none());
        let again =
            reconcile_startup_device_identity_in(&copied, db_only.path(), "fp-machine-c").unwrap();
        assert_eq!(again.device_id, clone_id.device_id);

        // Second boot on same machine A: `existed_on_disk == true`, stays active
        let r2 = reconcile_startup_device_identity_in(&conn, dir.path(), "fp-machine-a").unwrap();
        assert_eq!(r2.device_id, "dev-orig");
        assert!(!r2.rotated_for_new_install);
        assert!(r2.existed_on_disk);

        // Simulated full directory copy to machine B (different fingerprint):
        // must mint fresh device_id, update user_config, disable cloned space, and purge cloned creds!
        let r3 = reconcile_startup_device_identity_in(&conn, dir.path(), "fp-machine-b").unwrap();
        assert_ne!(r3.device_id, "dev-orig");
        assert!(r3.rotated_for_new_install);
        assert!(active_space_in(&conn).unwrap().is_none());
        let status: String = conn
            .query_row(
                "SELECT status FROM sync_spaces WHERE space_id = 'selfhost:srv1:acc1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "disabled");
        let persisted_id: String = conn
            .query_row(
                "SELECT value FROM user_config WHERE key = 'sync_device_id'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(persisted_id, r3.device_id);
        assert_eq!(creds.get("selfhost:srv1:acc1").unwrap(), None);
    }
}
