//! Transactional SQLite storage layer for Sync v2.
//!
//! All local mutations (dictation save, edit/restore, delete, TTS increment)
//! call the `*_tx` functions here inside the SAME SQLite transaction as the
//! business write so outbox ops and stats revisions can never diverge from
//! local data on crash or power loss.

use chrono::{Datelike, Duration, Local, NaiveDate, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use voicex_sync_protocol::{EditKind, LlmUsageState, SyncDomain};

use crate::services::history_service::{HISTORY_ERROR_NONE, HISTORY_MODE_TRANSLATE_READ};
use crate::storage::{
    AppUsageRange, AppUsageStat, DailyActivityPoint, DailyActivityResponse, HistoryRecord,
    StorageError, TtsCounters, UsageStats, FEATURE_KIND_DICTATION, UNTRACKED_APP_KEY,
};
use crate::sync_v2::hlc::{Hlc, HlcClock};
use crate::sync_v2::merge::{resolve_edits, should_accept_stats_revision, EditVersionNode};
use crate::sync_v2::payloads::{
    DailyAppStatPayload, DailyModelStatPayload, DictationTotalsPayload, EditMetadataPayload,
    HistoryCreatePayload, HistoryDeletePayload, HistoryEditPayload, HistoryOpPayload,
    StatsDailyMonthPayload, StatsTotalsPayload, TtsTotalsPayload, PAYLOAD_SCHEMA_V1,
};
use crate::sync_v2::scope::{
    can_send_history_delete, can_share_history_create, can_share_history_edit, RecordEligibility,
    SyncScopeConfig,
};

const DAILY_MODEL_BACKFILL_KEY: &str = "daily_model_usage_backfilled_v1";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverviewScope {
    #[default]
    All,
    Local,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncSpaceRecord {
    pub space_id: String,
    pub backend_kind: String,
    pub server_url: String,
    pub server_id: String,
    pub account_id: String,
    pub device_id: String,
    pub is_active: bool,
    pub scope_history: bool,
    pub scope_edits: bool,
    pub key_check_tag: Option<String>,
    pub last_hlc: String,
    pub stats_cursor: i64,
    pub history_cursor: i64,
    pub edits_cursor: i64,
    pub status: String,
    pub last_error: Option<String>,
    pub last_sync_at: Option<String>,
    pub scope_version: i64,
    pub acked_scope_version: i64,
}

impl SyncSpaceRecord {
    pub fn scope_config(&self) -> SyncScopeConfig {
        SyncScopeConfig {
            enabled: self.is_active && self.status != "disabled" && self.status != "revoked",
            sync_history: self.scope_history,
            sync_edits: self.scope_edits,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OverviewDeviceSyncInfo {
    pub device_id: String,
    pub device_name: String,
    pub platform: String,
    pub is_local: bool,
    pub last_synced_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelUsageItem {
    pub model_name: String,
    pub usage_count: i64,
    pub total_characters: i64,
    pub total_duration_ms: i64,
    pub ai_correction_count: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelUsageResponse {
    pub asr_models: Vec<ModelUsageItem>,
    pub llm_models: Vec<ModelUsageItem>,
    pub total_dictations: i64,
    pub unattributed_asr_count: i64,
    pub unattributed_llm_count: i64,
    pub no_llm_count: i64,
    pub undated_count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditConflictAlternative {
    pub edit_id: String,
    pub source_device_id: String,
    pub edit_kind: EditKind,
    pub hlc: String,
    pub edited_at: String,
    pub final_text: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteApplyOutcome {
    Applied,
    IgnoredTombstoned,
    IgnoredRetention,
    IgnoredDuplicate,
    StagedWithoutBaseRecord,
}

fn sql_err(err: rusqlite::Error) -> StorageError {
    StorageError::QueryFailed(err.to_string())
}

fn ensure_column_in(
    conn: &Connection,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<(), StorageError> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(sql_err)?;
    let exists = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(sql_err)?
        .any(|name| name.map(|n| n == column).unwrap_or(false));
    if !exists {
        conn.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {column} {definition}"),
            [],
        )
        .map_err(sql_err)?;
    }
    Ok(())
}

/// Create Sync v2 tables/columns and run one-time idempotent local backfills.
pub fn init_schema(conn: &Connection) -> Result<(), StorageError> {
    super::knowledge::init(conn)?;
    ensure_column_in(
        conn,
        "device_usage_stats",
        "ai_correction_count",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column_in(
        conn,
        "history_record",
        "sync_eligible",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column_in(conn, "history_record", "sync_space_id", "TEXT")?;
    ensure_column_in(conn, "history_record", "active_edit_id", "TEXT")?;
    ensure_column_in(
        conn,
        "history_record",
        "has_edit_conflict",
        "INTEGER NOT NULL DEFAULT 0",
    )?;

    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS sync_spaces (
            space_id TEXT PRIMARY KEY,
            backend_kind TEXT NOT NULL,
            server_url TEXT NOT NULL,
            server_id TEXT NOT NULL,
            account_id TEXT NOT NULL,
            device_id TEXT NOT NULL,
            is_active INTEGER NOT NULL DEFAULT 0,
            scope_history INTEGER NOT NULL DEFAULT 0,
            scope_edits INTEGER NOT NULL DEFAULT 0,
            key_check_tag TEXT,
            last_hlc TEXT NOT NULL DEFAULT '',
            stats_cursor INTEGER NOT NULL DEFAULT 0,
            history_cursor INTEGER NOT NULL DEFAULT 0,
            edits_cursor INTEGER NOT NULL DEFAULT 0,
            status TEXT NOT NULL DEFAULT 'disabled',
            last_error TEXT,
            last_sync_at TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS sync_devices (
            space_id TEXT NOT NULL,
            device_id TEXT NOT NULL,
            device_name TEXT NOT NULL,
            platform TEXT NOT NULL DEFAULT '',
            app_version TEXT NOT NULL DEFAULT '',
            last_synced_at TEXT,
            revoked_at TEXT,
            updated_at TEXT NOT NULL,
            PRIMARY KEY (space_id, device_id)
        );

        CREATE TABLE IF NOT EXISTS daily_model_usage_stats (
            device_id TEXT NOT NULL,
            local_date TEXT NOT NULL,
            asr_model_name TEXT NOT NULL,
            llm_state TEXT NOT NULL,
            llm_model_name TEXT NOT NULL,
            usage_count INTEGER NOT NULL DEFAULT 0,
            total_characters INTEGER NOT NULL DEFAULT 0,
            total_duration_ms INTEGER NOT NULL DEFAULT 0,
            ai_correction_count INTEGER NOT NULL DEFAULT 0,
            last_used_at TEXT NOT NULL,
            PRIMARY KEY (device_id, local_date, asr_model_name, llm_state, llm_model_name)
        );
        CREATE INDEX IF NOT EXISTS daily_model_usage_date_idx
            ON daily_model_usage_stats(device_id, local_date);

        CREATE TABLE IF NOT EXISTS sync_stats_shards (
            space_id TEXT NOT NULL,
            device_id TEXT NOT NULL,
            shard_key TEXT NOT NULL,
            local_revision INTEGER NOT NULL DEFAULT 0,
            acked_revision INTEGER NOT NULL DEFAULT 0,
            remote_revision INTEGER NOT NULL DEFAULT 0,
            last_synced_at TEXT,
            updated_at TEXT NOT NULL,
            PRIMARY KEY (space_id, device_id, shard_key)
        );

        CREATE TABLE IF NOT EXISTS sync_legacy_baseline (
            space_id TEXT PRIMARY KEY,
            recording_count INTEGER NOT NULL DEFAULT 0,
            total_characters INTEGER NOT NULL DEFAULT 0,
            total_duration_ms INTEGER NOT NULL DEFAULT 0,
            llm_invoked_count INTEGER NOT NULL DEFAULT 0,
            ai_correction_count INTEGER NOT NULL DEFAULT 0,
            tts_read_count INTEGER NOT NULL DEFAULT 0,
            tts_translate_count INTEGER NOT NULL DEFAULT 0,
            tts_characters INTEGER NOT NULL DEFAULT 0,
            tts_duration_ms INTEGER NOT NULL DEFAULT 0,
            tts_llm_count INTEGER NOT NULL DEFAULT 0,
            note TEXT NOT NULL DEFAULT '',
            created_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS history_edit_versions (
            edit_id TEXT PRIMARY KEY,
            record_id TEXT NOT NULL,
            space_id TEXT,
            source_device_id TEXT NOT NULL,
            edit_kind TEXT NOT NULL,
            parent_edit_ids_json TEXT NOT NULL DEFAULT '[]',
            hlc TEXT NOT NULL,
            edited_at TEXT NOT NULL,
            final_text TEXT,
            metadata_json TEXT,
            is_shared INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS history_edit_versions_record_idx
            ON history_edit_versions(record_id, hlc DESC);

        CREATE TABLE IF NOT EXISTS history_tombstones (
            space_id TEXT NOT NULL,
            record_id TEXT NOT NULL,
            deleted_by_device_id TEXT NOT NULL,
            hlc TEXT NOT NULL,
            deleted_at TEXT NOT NULL,
            PRIMARY KEY (space_id, record_id)
        );

        CREATE TABLE IF NOT EXISTS sync_outbox_ops (
            op_id TEXT PRIMARY KEY,
            space_id TEXT NOT NULL,
            domain TEXT NOT NULL,
            op_type TEXT NOT NULL,
            record_id TEXT NOT NULL,
            payload_json TEXT NOT NULL,
            created_at TEXT NOT NULL,
            attempt_count INTEGER NOT NULL DEFAULT 0,
            last_error TEXT
        );
        CREATE INDEX IF NOT EXISTS sync_outbox_space_domain_idx
            ON sync_outbox_ops(space_id, domain, created_at ASC);

        CREATE TABLE IF NOT EXISTS sync_pending_audio_deletes (
            path TEXT PRIMARY KEY,
            created_at TEXT NOT NULL
        );
        "#,
    )
    .map_err(sql_err)?;

    ensure_column_in(
        conn,
        "sync_spaces",
        "scope_version",
        "INTEGER NOT NULL DEFAULT 1",
    )?;
    ensure_column_in(
        conn,
        "sync_spaces",
        "acked_scope_version",
        "INTEGER NOT NULL DEFAULT 1",
    )?;

    backfill_daily_model_usage_and_corrections(conn)
}

fn local_date_from_rfc3339(ts: &str) -> Option<String> {
    chrono::DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|dt| dt.with_timezone(&Local).format("%Y-%m-%d").to_string())
}

fn month_shard_key_from_local_date(local_date: &str) -> Option<String> {
    if local_date.len() >= 7 {
        Some(format!("daily:{}", &local_date[..7]))
    } else {
        None
    }
}

fn local_device_id_in(conn: &Connection) -> Result<String, StorageError> {
    if let Some(id) = conn
        .query_row(
            "SELECT value FROM user_config WHERE key = 'sync_device_id' LIMIT 1",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(sql_err)?
    {
        if !id.trim().is_empty() {
            return Ok(id);
        }
    }
    let id = uuid::Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO user_config (key, value) VALUES ('sync_device_id', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![id],
    )
    .map_err(sql_err)?;
    Ok(id)
}

fn backfill_daily_model_usage_and_corrections(conn: &Connection) -> Result<(), StorageError> {
    let done: Option<String> = conn
        .query_row(
            "SELECT value FROM user_config WHERE key = ?1 LIMIT 1",
            params![DAILY_MODEL_BACKFILL_KEY],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql_err)?;
    if done.as_deref() == Some("1") {
        return Ok(());
    }

    let has_history: bool = conn
        .query_row("SELECT EXISTS(SELECT 1 FROM history_record LIMIT 1)", [], |r| {
            Ok(r.get::<_, i32>(0)? != 0)
        })
        .map_err(sql_err)?;
    if !has_history {
        conn.execute(
            "INSERT INTO user_config (key, value) VALUES (?1, '1')
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![DAILY_MODEL_BACKFILL_KEY],
        )
        .map_err(sql_err)?;
        return Ok(());
    }

    let transaction = conn
        .is_autocommit()
        .then(|| conn.unchecked_transaction())
        .transpose()
        .map_err(sql_err)?;
    let local_device_id = local_device_id_in(conn)?;

    let mut local_ai_corrections: i64 = 0;
    {
        let mut stmt = conn
            .prepare(
                "SELECT timestamp, text, duration_ms, llm_invoked, ai_correction_applied,
                        asr_model_name, llm_model_name
                 FROM history_record
                 WHERE error_code = ?1 AND mode != ?2
                   AND (source_device_id = ?3 OR source_device_id IS NULL OR source_device_id = '')",
            )
            .map_err(sql_err)?;
        let rows = stmt
            .query_map(
                params![HISTORY_ERROR_NONE, HISTORY_MODE_TRANSLATE_READ, local_device_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i32>(3)? != 0,
                        row.get::<_, i32>(4)? != 0,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, Option<String>>(6)?,
                    ))
                },
            )
            .map_err(sql_err)?;

        for row in rows {
            let (ts, text, duration_ms, llm_invoked, ai_corr, asr_model, llm_model) =
                row.map_err(sql_err)?;
            if ai_corr {
                local_ai_corrections += 1;
            }
            let Some(local_date) = local_date_from_rfc3339(&ts) else {
                continue;
            };
            upsert_daily_model_usage_stats_tx(
                conn,
                &local_device_id,
                &local_date,
                asr_model.as_deref(),
                llm_invoked,
                llm_model.as_deref(),
                text.chars().count() as i64,
                duration_ms,
                ai_corr,
                &ts,
            )?;
        }
    }

    conn.execute(
        "UPDATE device_usage_stats
         SET ai_correction_count = MAX(ai_correction_count, ?1)
         WHERE device_id = ?2",
        params![local_ai_corrections, local_device_id],
    )
    .map_err(sql_err)?;

    conn.execute(
        "INSERT INTO user_config (key, value) VALUES (?1, '1')
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![DAILY_MODEL_BACKFILL_KEY],
    )
    .map_err(sql_err)?;

    if let Some(tx) = transaction {
        tx.commit().map_err(sql_err)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn upsert_daily_model_usage_stats_tx(
    conn: &Connection,
    device_id: &str,
    local_date: &str,
    asr_model_name: Option<&str>,
    llm_invoked: bool,
    llm_model_name: Option<&str>,
    characters: i64,
    duration_ms: i64,
    ai_correction_applied: bool,
    last_used_at: &str,
) -> Result<(), StorageError> {
    let asr = asr_model_name.map(str::trim).unwrap_or("");
    let (llm_state, llm_model) = LlmUsageState::from_record_fields(llm_invoked, llm_model_name);
    conn.execute(
        "INSERT INTO daily_model_usage_stats (
            device_id, local_date, asr_model_name, llm_state, llm_model_name,
            usage_count, total_characters, total_duration_ms, ai_correction_count, last_used_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?7, ?8, ?9)
         ON CONFLICT(device_id, local_date, asr_model_name, llm_state, llm_model_name)
         DO UPDATE SET
            usage_count = daily_model_usage_stats.usage_count + 1,
            total_characters = daily_model_usage_stats.total_characters + excluded.total_characters,
            total_duration_ms = daily_model_usage_stats.total_duration_ms + excluded.total_duration_ms,
            ai_correction_count = daily_model_usage_stats.ai_correction_count + excluded.ai_correction_count,
            last_used_at = CASE
                WHEN excluded.last_used_at > daily_model_usage_stats.last_used_at
                THEN excluded.last_used_at
                ELSE daily_model_usage_stats.last_used_at
            END",
        params![
            device_id,
            local_date,
            asr,
            llm_state.as_str(),
            llm_model,
            characters.max(0),
            duration_ms.max(0),
            i64::from(ai_correction_applied),
            last_used_at,
        ],
    )
    .map_err(sql_err)?;
    Ok(())
}

pub fn active_space_in(conn: &Connection) -> Result<Option<SyncSpaceRecord>, StorageError> {
    conn.query_row(
        "SELECT space_id, backend_kind, server_url, server_id, account_id, device_id,
                is_active, scope_history, scope_edits, key_check_tag, last_hlc,
                stats_cursor, history_cursor, edits_cursor, status, last_error, last_sync_at,
                scope_version, acked_scope_version
         FROM sync_spaces
         WHERE is_active = 1
         LIMIT 1",
        [],
        |row| {
            Ok(SyncSpaceRecord {
                space_id: row.get(0)?,
                backend_kind: row.get(1)?,
                server_url: row.get(2)?,
                server_id: row.get(3)?,
                account_id: row.get(4)?,
                device_id: row.get(5)?,
                is_active: row.get::<_, i32>(6)? != 0,
                scope_history: row.get::<_, i32>(7)? != 0,
                scope_edits: row.get::<_, i32>(8)? != 0,
                key_check_tag: row.get(9)?,
                last_hlc: row.get(10)?,
                stats_cursor: row.get(11)?,
                history_cursor: row.get(12)?,
                edits_cursor: row.get(13)?,
                status: row.get(14)?,
                last_error: row.get(15)?,
                last_sync_at: row.get(16)?,
                scope_version: row.get(17)?,
                acked_scope_version: row.get(18)?,
            })
        },
    )
    .optional()
    .map_err(sql_err)
}

/// Register or activate a sync space without deleting pending outbox entries of
/// any previously used space (Section 3.1.3). Re-pairing a disabled/revoked space
/// with `make_active = true` restores `status = 'idle'` and clears `last_error` (S2).
#[allow(clippy::too_many_arguments)]
pub fn upsert_sync_space_tx(
    conn: &Connection,
    space_id: &str,
    backend_kind: &str,
    server_url: &str,
    server_id: &str,
    account_id: &str,
    device_id: &str,
    scope_history: bool,
    scope_edits: bool,
    key_check_tag: Option<&str>,
    make_active: bool,
) -> Result<(), StorageError> {
    let now = Utc::now().to_rfc3339();
    if make_active {
        conn.execute(
            "UPDATE sync_spaces SET is_active = 0, updated_at = ?1 WHERE space_id != ?2 AND is_active = 1",
            params![now, space_id],
        )
        .map_err(sql_err)?;
    }
    conn.execute(
        "INSERT INTO sync_spaces (
            space_id, backend_kind, server_url, server_id, account_id, device_id,
            is_active, scope_history, scope_edits, key_check_tag, status, last_error,
            scope_version, acked_scope_version, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'idle', NULL, 1, 1, ?11, ?11)
         ON CONFLICT(space_id) DO UPDATE SET
            backend_kind = excluded.backend_kind,
            server_url = excluded.server_url,
            device_id = excluded.device_id,
            is_active = excluded.is_active,
            scope_history = excluded.scope_history,
            scope_edits = excluded.scope_edits,
            key_check_tag = COALESCE(excluded.key_check_tag, sync_spaces.key_check_tag),
            status = CASE WHEN excluded.is_active = 1 THEN 'idle' ELSE sync_spaces.status END,
            last_error = CASE WHEN excluded.is_active = 1 THEN NULL ELSE sync_spaces.last_error END,
            acked_scope_version = sync_spaces.scope_version,
            updated_at = excluded.updated_at",
        params![
            space_id,
            backend_kind,
            server_url,
            server_id,
            account_id,
            device_id,
            i32::from(make_active),
            i32::from(scope_history),
            i32::from(scope_edits),
            key_check_tag,
            now,
        ],
    )
    .map_err(sql_err)?;
    Ok(())
}

/// Update a sync space's scope toggles and cancel unsent outbox ops for any
/// narrowed domain (preserving cursors so re-widening resumes cleanly).
pub fn update_sync_scope_tx(
    conn: &Connection,
    space_id: &str,
    sync_history: bool,
    sync_edits: bool,
) -> Result<(), StorageError> {
    let current = conn
        .query_row(
            "SELECT is_active, scope_history, scope_edits, status FROM sync_spaces WHERE space_id = ?1",
            params![space_id],
            |row| {
                Ok(SyncScopeConfig {
                    enabled: row.get::<_, i32>(0)? != 0
                        && !matches!(row.get::<_, String>(3)?.as_str(), "disabled" | "revoked"),
                    sync_history: row.get::<_, i32>(1)? != 0,
                    sync_edits: row.get::<_, i32>(2)? != 0,
                })
            },
        )
        .optional()
        .map_err(sql_err)?
        .ok_or_else(|| StorageError::NotFound(space_id.to_string()))?;

    let effective_edits = sync_history && sync_edits;
    let next = SyncScopeConfig {
        enabled: current.enabled,
        sync_history,
        sync_edits: effective_edits,
    };

    for domain in next.domains_to_cancel_on_narrow(&current) {
        match domain {
            SyncDomain::History => {
                // Cancel unsent creates, but keep explicit delete tombstones for already-shared IDs.
                conn.execute(
                    "DELETE FROM sync_outbox_ops
                     WHERE space_id = ?1 AND domain = 'history' AND op_type != 'history.delete'",
                    params![space_id],
                )
                .map_err(sql_err)?;
            }
            SyncDomain::Edits => {
                conn.execute(
                    "DELETE FROM sync_outbox_ops WHERE space_id = ?1 AND domain = 'edits'",
                    params![space_id],
                )
                .map_err(sql_err)?;
            }
            SyncDomain::Stats
            | SyncDomain::Dictionary
            | SyncDomain::Rules
            | SyncDomain::Prompts => {}
        }
    }

    conn.execute(
        "UPDATE sync_spaces
         SET scope_history = ?2,
             scope_edits = ?3,
             scope_version = scope_version + 1,
             updated_at = ?4
         WHERE space_id = ?1",
        params![
            space_id,
            i32::from(sync_history),
            i32::from(effective_edits),
            Utc::now().to_rfc3339()
        ],
    )
    .map_err(sql_err)?;
    Ok(())
}

/// Mark `acked_scope_version` up to `acked_version` once the server confirms
/// `PUT /v2/devices/{device_id}` (S6).
pub fn ack_sync_scope_version_tx(
    conn: &Connection,
    space_id: &str,
    acked_version: i64,
) -> Result<(), StorageError> {
    conn.execute(
        "UPDATE sync_spaces
         SET acked_scope_version = MAX(acked_scope_version, ?2), updated_at = ?3
         WHERE space_id = ?1",
        params![space_id, acked_version, Utc::now().to_rfc3339()],
    )
    .map_err(sql_err)?;
    Ok(())
}

pub fn tick_hlc_tx(
    conn: &Connection,
    space: Option<&SyncSpaceRecord>,
    device_id: &str,
) -> Result<String, StorageError> {
    let persisted = space
        .and_then(|s| s.last_hlc.parse::<Hlc>().ok());
    let mut clock = HlcClock::new(device_id, persisted.as_ref())
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
    let next = clock
        .tick(Utc::now().timestamp_millis())
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
    let wire = next.to_wire_string();
    if let Some(space) = space {
        conn.execute(
            "UPDATE sync_spaces SET last_hlc = ?2, updated_at = ?3 WHERE space_id = ?1",
            params![space.space_id, wire, Utc::now().to_rfc3339()],
        )
        .map_err(sql_err)?;
    }
    Ok(wire)
}

pub fn observe_hlc_tx(
    conn: &Connection,
    space_id: &str,
    device_id: &str,
    remote_hlc: &str,
) -> Result<(), StorageError> {
    let Ok(remote) = remote_hlc.parse::<Hlc>() else {
        return Ok(());
    };
    let current_hlc: Option<String> = conn
        .query_row(
            "SELECT last_hlc FROM sync_spaces WHERE space_id = ?1",
            params![space_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql_err)?;
    let persisted = current_hlc.as_deref().and_then(|s| s.parse::<Hlc>().ok());
    let mut clock = HlcClock::new(device_id, persisted.as_ref())
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
    let outcome = clock
        .observe(&remote, Utc::now().timestamp_millis())
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
    if outcome == crate::sync_v2::hlc::ObserveOutcome::DriftClamped {
        log::warn!(
            "Sync v2 clamped excessive remote HLC drift from device {}",
            remote.device_id
        );
    }
    conn.execute(
        "UPDATE sync_spaces SET last_hlc = ?2, updated_at = ?3 WHERE space_id = ?1",
        params![space_id, clock.last_hlc().to_wire_string(), Utc::now().to_rfc3339()],
    )
    .map_err(sql_err)?;
    Ok(())
}

/// Atomically bump `local_revision` for `(space_id, device_id, shard_key)` inside
/// the caller's SQLite transaction.
pub fn bump_local_stats_shard_tx(
    conn: &Connection,
    space_id: &str,
    device_id: &str,
    shard_key: &str,
) -> Result<i64, StorageError> {
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO sync_stats_shards (
            space_id, device_id, shard_key, local_revision, acked_revision, remote_revision, updated_at
         ) VALUES (?1, ?2, ?3, 1, 0, 0, ?4)
         ON CONFLICT(space_id, device_id, shard_key) DO UPDATE SET
            local_revision = sync_stats_shards.local_revision + 1,
            updated_at = excluded.updated_at",
        params![space_id, device_id, shard_key, now],
    )
    .map_err(sql_err)?;
    let rev: i64 = conn
        .query_row(
            "SELECT local_revision FROM sync_stats_shards
             WHERE space_id = ?1 AND device_id = ?2 AND shard_key = ?3",
            params![space_id, device_id, shard_key],
            |row| row.get(0),
        )
        .map_err(sql_err)?;
    Ok(rev)
}

/// Transactional hook executed inside `insert_local_history_record_with_stats_conn`.
pub fn on_local_history_inserted_tx(
    conn: &Connection,
    record: &HistoryRecord,
    local_date: &str,
) -> Result<(), StorageError> {
    if record.error_code != HISTORY_ERROR_NONE || record.mode == HISTORY_MODE_TRANSLATE_READ {
        return Ok(());
    }
    let device_id = record
        .source_device_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            StorageError::QueryFailed("Local history is missing its device ID".into())
        })?;
    let chars = record.text.chars().count() as i64;

    if record.ai_correction_applied {
        conn.execute(
            "UPDATE device_usage_stats
             SET ai_correction_count = ai_correction_count + 1
             WHERE device_id = ?1",
            params![device_id],
        )
        .map_err(sql_err)?;
    }

    upsert_daily_model_usage_stats_tx(
        conn,
        device_id,
        local_date,
        record.asr_model_name.as_deref(),
        record.llm_invoked,
        record.llm_model_name.as_deref(),
        chars,
        record.duration_ms,
        record.ai_correction_applied,
        &record.timestamp,
    )?;

    let Some(space) = active_space_in(conn)? else {
        return Ok(());
    };
    let scope = space.scope_config();
    if !scope.enabled {
        return Ok(());
    }

    // Stats shards always advance in the same transaction as local dictation.
    bump_local_stats_shard_tx(conn, &space.space_id, device_id, "totals")?;
    if let Some(month_key) = month_shard_key_from_local_date(local_date) {
        bump_local_stats_shard_tx(conn, &space.space_id, device_id, &month_key)?;
    }

    let eligibility = RecordEligibility {
        mode: &record.mode,
        error_code: record.error_code,
        is_final: record.is_final,
    };
    if can_share_history_create(&scope, eligibility) {
        conn.execute(
            "UPDATE history_record SET sync_eligible = 1, sync_space_id = ?2 WHERE id = ?1",
            params![record.id, space.space_id],
        )
        .map_err(sql_err)?;

        let op = HistoryOpPayload::Create(HistoryCreatePayload {
            schema: PAYLOAD_SCHEMA_V1,
            record_id: record.id.clone(),
            source_device_id: device_id.to_string(),
            timestamp: record.timestamp.clone(),
            text: record.text.clone(),
            original_text: record.original_text.clone(),
            ai_correction_applied: record.ai_correction_applied,
            llm_invoked: record.llm_invoked,
            mode: record.mode.clone(),
            duration_ms: record.duration_ms,
            asr_model_name: record.asr_model_name.clone(),
            llm_model_name: record.llm_model_name.clone(),
            target_app_platform: record.target_app_platform.clone(),
            target_app_key: record.target_app_key.clone(),
            target_app_name: record.target_app_name.clone(),
        });
        enqueue_outbox_op_tx(
            conn,
            &space.space_id,
            &format!("create:{}", record.id),
            SyncDomain::History,
            "history.create",
            &record.id,
            &op,
        )?;
    }
    Ok(())
}

/// Transactional hook executed inside `increment_device_tts_stats`.
pub fn on_local_tts_incremented_tx(conn: &Connection, device_id: &str) -> Result<(), StorageError> {
    let Some(space) = active_space_in(conn)? else {
        return Ok(());
    };
    if !space.scope_config().enabled {
        return Ok(());
    }
    bump_local_stats_shard_tx(conn, &space.space_id, device_id, "totals")?;
    Ok(())
}

fn load_edit_nodes_in(conn: &Connection, record_id: &str) -> Result<Vec<EditVersionNode>, StorageError> {
    let mut stmt = conn
        .prepare(
            "SELECT edit_id, source_device_id, edit_kind, parent_edit_ids_json, hlc, edited_at, final_text
             FROM history_edit_versions
             WHERE record_id = ?1
             ORDER BY hlc DESC, edit_id DESC",
        )
        .map_err(sql_err)?;
    let rows = stmt
        .query_map(params![record_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<String>>(6)?,
            ))
        })
        .map_err(sql_err)?;

    let mut nodes = Vec::new();
    for row in rows {
        let (edit_id, source_device_id, kind_str, parents_json, hlc, edited_at, final_text) =
            row.map_err(sql_err)?;
        let edit_kind: EditKind = kind_str
            .parse()
            .map_err(|e: String| StorageError::QueryFailed(e))?;
        let parent_edit_ids: Vec<String> =
            serde_json::from_str(&parents_json).unwrap_or_default();
        nodes.push(EditVersionNode {
            edit_id,
            source_device_id,
            edit_kind,
            parent_edit_ids,
            hlc,
            edited_at,
            final_text,
        });
    }
    Ok(nodes)
}

fn epoch_ms_from_rfc3339_or_zero(ts: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(ts)
        .map(|dt| dt.timestamp_millis().max(0))
        .unwrap_or(0)
}

/// Seed a private (`is_shared = 0`) version row in `history_edit_versions` for a
/// legacy v1 `history_record.edited_text` if the record has no version rows yet.
/// Prevents `project_resolved_edits_to_history_tx` from wiping v1 edits on pull.
pub fn ensure_legacy_edit_seeded_tx(
    conn: &Connection,
    record_id: &str,
) -> Result<bool, StorageError> {
    let has_versions: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM history_edit_versions WHERE record_id = ?1 LIMIT 1)",
            params![record_id],
            |r| Ok(r.get::<_, i32>(0)? != 0),
        )
        .map_err(sql_err)?;
    if has_versions {
        return Ok(false);
    }

    let row: Option<(String, Option<String>, Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT timestamp, edited_text, edit_source, edited_at
             FROM history_record
             WHERE id = ?1",
            params![record_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .map_err(sql_err)?;
    let Some((timestamp, Some(edited_text), edit_source, edited_at_opt)) = row else {
        return Ok(false);
    };
    if edited_text.is_empty() {
        return Ok(false);
    }

    let device_id = local_device_id_in(conn)?;
    let edit_kind = match edit_source.as_deref() {
        Some("manual") => EditKind::Manual,
        _ => EditKind::Observed,
    };
    let edited_at = edited_at_opt
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(timestamp);

    let post_edit_row: Option<(String, String)> = conn
        .query_row(
            "SELECT id, payload FROM post_edit_record WHERE source_id = ?1 LIMIT 1",
            params![record_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .ok()
        .flatten();

    let (edit_id, metadata_json) = if edit_kind == EditKind::Observed {
        match post_edit_row {
            Some((pe_id, payload)) => {
                let meta_json = serde_json::from_str::<serde_json::Value>(&payload)
                    .ok()
                    .and_then(|v| {
                        serde_json::to_string(&EditMetadataPayload {
                            complete: v.get("complete").and_then(|x| x.as_bool()).unwrap_or(false),
                            end_reason: v
                                .get("endReason")
                                .and_then(|x| x.as_str())
                                .map(String::from),
                            insertions: v
                                .get("insertions")
                                .and_then(|x| x.as_u64())
                                .map(|n| n as usize),
                            deletions: v
                                .get("deletions")
                                .and_then(|x| x.as_u64())
                                .map(|n| n as usize),
                            substitutions: v
                                .get("substitutions")
                                .and_then(|x| x.as_u64())
                                .map(|n| n as usize),
                        })
                        .ok()
                    });
                (pe_id, meta_json)
            }
            None => (format!("legacy-edit:{device_id}:{record_id}"), None),
        }
    } else {
        (format!("legacy-edit:{device_id}:{record_id}"), None)
    };

    let hlc = Hlc::new(epoch_ms_from_rfc3339_or_zero(&edited_at), 0, &device_id)
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?
        .to_wire_string();
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT OR IGNORE INTO history_edit_versions (
            edit_id, record_id, space_id, source_device_id, edit_kind,
            parent_edit_ids_json, hlc, edited_at, final_text, metadata_json,
            is_shared, created_at
         ) VALUES (?1, ?2, NULL, ?3, ?4, '[]', ?5, ?6, ?7, ?8, 0, ?9)",
        params![
            edit_id,
            record_id,
            device_id,
            edit_kind.as_str(),
            hlc,
            edited_at,
            edited_text,
            metadata_json,
            now,
        ],
    )
    .map_err(sql_err)?;
    conn.execute(
        "UPDATE history_record SET active_edit_id = COALESCE(active_edit_id, ?2) WHERE id = ?1",
        params![record_id, edit_id],
    )
    .map_err(sql_err)?;
    Ok(true)
}

/// Seed private (`is_shared = 0`) version rows for every local legacy `edited_text`
/// that does not yet have any `history_edit_versions` entry. Idempotent.
pub fn seed_all_legacy_local_edits_tx(conn: &Connection) -> Result<usize, StorageError> {
    let ids: Vec<String> = {
        let mut stmt = conn
            .prepare(
                "SELECT h.id
                 FROM history_record h
                 WHERE h.edited_text IS NOT NULL
                   AND h.edited_text != ''
                   AND NOT EXISTS (
                       SELECT 1 FROM history_edit_versions v WHERE v.record_id = h.id
                   )",
            )
            .map_err(sql_err)?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(sql_err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sql_err)?;
        rows
    };

    let mut seeded = 0usize;
    for id in ids {
        if ensure_legacy_edit_seeded_tx(conn, &id)? {
            seeded += 1;
        }
    }
    Ok(seeded)
}

/// Summary of an atomic, idempotent v1 `sync_outbox` -> v2 handoff.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct V1OutboxHandoffSummary {
    pub already_completed: bool,
    pub deletes_handed_off: usize,
    pub creates_handed_off: usize,
    pub creates_skipped_unauthorized: usize,
}

/// Atomically and idempotently convert pending v1 `sync_outbox` creates/deletes
/// into v2 tombstones & `sync_outbox_ops` before the first v2 network cycle.
/// Preserves the original v1 `sync_outbox` rows for reconciliation/audit.
pub fn handoff_v1_outbox_to_v2_tx(
    conn: &Connection,
    space_id: &str,
    device_id: &str,
    scope_history: bool,
) -> Result<V1OutboxHandoffSummary, StorageError> {
    // Older clients wrote a per-space completion marker. Its presence also
    // consumes the single legacy queue, regardless of the next pair target.
    let done: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM user_config
          WHERE key GLOB 'v1_outbox_handoff_done:*' AND value='1')",
            [],
            |r| r.get(0),
        )
        .map_err(sql_err)?;
    if done {
        return Ok(V1OutboxHandoffSummary {
            already_completed: true,
            ..Default::default()
        });
    }

    let has_v1_outbox: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'sync_outbox')",
            [],
            |r| Ok(r.get::<_, i32>(0)? != 0),
        )
        .map_err(sql_err)?;

    let mut summary = V1OutboxHandoffSummary::default();
    if has_v1_outbox {
        let pending: bool = conn
            .query_row("SELECT EXISTS(SELECT 1 FROM sync_outbox)", [], |r| r.get(0))
            .map_err(sql_err)?;
        if !pending {
            return Ok(summary);
        }
        conn.execute(
            "INSERT OR IGNORE INTO user_config(key,value)
             SELECT 'v1_outbox_source_space', 'selfhost:' || current_server_id || ':' || current_account_id
             FROM sync_state WHERE id=1 AND length(current_server_id)>0 AND length(current_account_id)>0",
            [],
        ).map_err(sql_err)?;
        let source: Option<String> = conn
            .query_row(
                "SELECT value FROM user_config WHERE key='v1_outbox_source_space'",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(sql_err)?;
        match source {
            Some(source) if source == space_id => {}
            Some(_) => return Ok(summary), // Preserve it for its authenticated source.
            None => {
                return Err(StorageError::InvalidInput(
                    "cannot identify the source account for the pending v1 queue".into(),
                ))
            }
        }
        let outbox_rows: Vec<(String, Option<String>, String, String)> = {
            let mut stmt = conn
                .prepare(
                    "SELECT event_type, record_id, payload, created_at
                     FROM sync_outbox
                     ORDER BY created_at ASC, event_id ASC",
                )
                .map_err(sql_err)?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                    ))
                })
                .map_err(sql_err)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(sql_err)?;
            rows
        };

        let mut pending_deletes: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::new();
        let mut pending_upserts: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::new();

        for (ev_type, rid_opt, payload_str, created_at) in outbox_rows {
            let rid = rid_opt
                .filter(|s| !s.trim().is_empty())
                .or_else(|| {
                    serde_json::from_str::<serde_json::Value>(&payload_str)
                        .ok()
                        .and_then(|v| {
                            v.get("recordId")
                                .or_else(|| v.get("id"))
                                .or_else(|| v.get("record").and_then(|r| r.get("id")))
                                .and_then(|x| x.as_str())
                                .map(String::from)
                        })
                });
            let Some(record_id) = rid.filter(|s| !s.trim().is_empty()) else {
                continue;
            };
            match ev_type.as_str() {
                "history.delete" => {
                    pending_upserts.remove(&record_id);
                    pending_deletes.insert(record_id, created_at);
                }
                "history.upsert" => {
                    if !pending_deletes.contains_key(&record_id) {
                        pending_upserts.insert(record_id, payload_str);
                    }
                }
                _ => {}
            }
        }

        // 1. Apply pending deletes first (tombstone + v2 delete op) so first pull cannot resurrect them.
        let space_opt = active_space_in(conn)?;
        for (record_id, deleted_at) in pending_deletes {
            let hlc = tick_hlc_tx(conn, space_opt.as_ref(), device_id)?;
            conn.execute(
                "INSERT INTO history_tombstones (space_id, record_id, deleted_by_device_id, hlc, deleted_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(space_id, record_id) DO UPDATE SET
                    hlc = CASE WHEN excluded.hlc > history_tombstones.hlc THEN excluded.hlc ELSE history_tombstones.hlc END",
                params![space_id, record_id, device_id, hlc, deleted_at],
            )
            .map_err(sql_err)?;
            conn.execute(
                "DELETE FROM history_edit_versions WHERE record_id = ?1",
                params![record_id],
            )
            .map_err(sql_err)?;
            conn.execute(
                "DELETE FROM history_record WHERE id = ?1",
                params![record_id],
            )
            .map_err(sql_err)?;
            conn.execute(
                "DELETE FROM sync_outbox_ops WHERE space_id = ?1 AND record_id = ?2 AND op_type != 'history.delete'",
                params![space_id, record_id],
            )
            .map_err(sql_err)?;
            let op = HistoryOpPayload::Delete(HistoryDeletePayload {
                schema: PAYLOAD_SCHEMA_V1,
                record_id: record_id.clone(),
                source_device_id: device_id.to_string(),
                hlc,
                deleted_at,
            });
            enqueue_outbox_op_tx(
                conn,
                space_id,
                &format!("delete:{record_id}"),
                SyncDomain::History,
                "history.delete",
                &record_id,
                &op,
            )?;
            summary.deletes_handed_off += 1;
        }

        // 2. Convert authorized pending creates when history sync is enabled.
        for (record_id, _payload_str) in pending_upserts {
            if !scope_history {
                summary.creates_skipped_unauthorized += 1;
                continue;
            }
            let row = conn
                .query_row(
                    "SELECT timestamp, text, original_text, ai_correction_applied, llm_invoked,
                            mode, duration_ms, is_final, error_code, source_device_id,
                            asr_model_name, llm_model_name,
                            target_app_platform, target_app_key, target_app_name
                     FROM history_record
                     WHERE id = ?1",
                    params![record_id],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, Option<String>>(2)?,
                            r.get::<_, i32>(3)? != 0,
                            r.get::<_, i32>(4)? != 0,
                            r.get::<_, String>(5)?,
                            r.get::<_, i64>(6)?,
                            r.get::<_, i32>(7)? != 0,
                            r.get::<_, i32>(8)?,
                            r.get::<_, Option<String>>(9)?,
                            r.get::<_, Option<String>>(10)?,
                            r.get::<_, Option<String>>(11)?,
                            r.get::<_, Option<String>>(12)?,
                            r.get::<_, Option<String>>(13)?,
                            r.get::<_, Option<String>>(14)?,
                        ))
                    },
                )
                .optional()
                .map_err(sql_err)?;
            let Some((
                timestamp,
                text,
                original_text,
                ai_correction_applied,
                llm_invoked,
                mode,
                duration_ms,
                is_final,
                error_code,
                source_device_id,
                asr_model_name,
                llm_model_name,
                target_app_platform,
                target_app_key,
                target_app_name,
            )) = row
            else {
                continue;
            };
            if error_code != HISTORY_ERROR_NONE || !is_final || mode == HISTORY_MODE_TRANSLATE_READ
            {
                summary.creates_skipped_unauthorized += 1;
                continue;
            }
            conn.execute(
                "UPDATE history_record SET sync_eligible = 1, sync_space_id = ?2 WHERE id = ?1",
                params![record_id, space_id],
            )
            .map_err(sql_err)?;
            let src_dev = source_device_id
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| device_id.to_string());
            let op = HistoryOpPayload::Create(HistoryCreatePayload {
                schema: PAYLOAD_SCHEMA_V1,
                record_id: record_id.clone(),
                source_device_id: src_dev,
                timestamp,
                text,
                original_text,
                ai_correction_applied,
                llm_invoked,
                mode,
                duration_ms: duration_ms.max(0),
                asr_model_name,
                llm_model_name,
                target_app_platform,
                target_app_key,
                target_app_name,
            });
            enqueue_outbox_op_tx(
                conn,
                space_id,
                &format!("create:{record_id}"),
                SyncDomain::History,
                "history.create",
                &record_id,
                &op,
            )?;
            summary.creates_handed_off += 1;
        }
    }

    let flag_key = format!("v1_outbox_handoff_done:{space_id}");
    conn.execute(
        "INSERT INTO user_config (key, value) VALUES (?1, '1')
         ON CONFLICT(key) DO UPDATE SET value = '1'",
        params![flag_key],
    )
    .map_err(sql_err)?;
    Ok(summary)
}

/// Explicitly promote existing unshared local edits on already-shared records
/// belonging to `space` into shared `history_edit_versions` and `sync_outbox_ops`.
pub fn import_existing_edits_tx(
    conn: &Connection,
    space: &SyncSpaceRecord,
) -> Result<usize, StorageError> {
    let record_ids: Vec<String> = {
        let mut stmt = conn
            .prepare(
                "SELECT h.id
                 FROM history_record h
                 WHERE h.sync_eligible = 1
                   AND h.sync_space_id = ?1
                   AND h.edited_text IS NOT NULL
                   AND h.edited_text != ''
                   AND NOT EXISTS (
                       SELECT 1 FROM history_edit_versions v
                       WHERE v.record_id = h.id AND v.is_shared = 1
                   )",
            )
            .map_err(sql_err)?;
        let rows = stmt
            .query_map(params![space.space_id], |r| r.get::<_, String>(0))
            .map_err(sql_err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sql_err)?;
        rows
    };

    let mut imported = 0usize;
    for record_id in record_ids {
        ensure_legacy_edit_seeded_tx(conn, &record_id)?;
        let versions: Vec<(
            String,
            String,
            String,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
        )> = {
            let mut stmt = conn
                .prepare(
                    "SELECT edit_id, source_device_id, edit_kind, parent_edit_ids_json,
                            hlc, edited_at, final_text, metadata_json
                     FROM history_edit_versions
                     WHERE record_id = ?1 AND is_shared = 0
                     ORDER BY hlc ASC, edit_id ASC",
                )
                .map_err(sql_err)?;
            let rows = stmt
                .query_map(params![record_id], |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                    ))
                })
                .map_err(sql_err)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(sql_err)?;
            rows
        };

        for (
            edit_id,
            source_device_id,
            edit_kind_str,
            parents_json,
            hlc,
            edited_at,
            final_text,
            meta_json,
        ) in versions
        {
            let edit_kind: EditKind = edit_kind_str.parse().unwrap_or(EditKind::Observed);
            let parent_edit_ids: Vec<String> =
                serde_json::from_str(&parents_json).unwrap_or_default();
            let metadata: Option<EditMetadataPayload> = meta_json
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok());
            conn.execute(
                "UPDATE history_edit_versions
                 SET is_shared = 1, space_id = ?2
                 WHERE edit_id = ?1",
                params![edit_id, space.space_id],
            )
            .map_err(sql_err)?;
            let op = HistoryOpPayload::Edit(HistoryEditPayload {
                schema: PAYLOAD_SCHEMA_V1,
                edit_id: edit_id.clone(),
                record_id: record_id.clone(),
                source_device_id,
                edit_kind,
                parent_edit_ids,
                hlc,
                edited_at,
                final_text,
                metadata,
            });
            enqueue_outbox_op_tx(
                conn,
                &space.space_id,
                &format!("edit:{edit_id}"),
                SyncDomain::Edits,
                "history.edit",
                &record_id,
                &op,
            )?;
            imported += 1;
        }
        project_resolved_edits_to_history_tx(conn, &record_id)?;
    }

    Ok(imported)
}

fn project_resolved_edits_to_history_tx(
    conn: &Connection,
    record_id: &str,
) -> Result<crate::sync_v2::merge::ResolvedEditState, StorageError> {
    let nodes = load_edit_nodes_in(conn, record_id)?;
    let resolved = resolve_edits(&nodes);
    let edit_source = match (&resolved.displayed_edited_text, resolved.active_edit_kind) {
        (Some(_), Some(EditKind::Observed)) => Some("observed"),
        (Some(_), Some(EditKind::Manual)) => Some("manual"),
        _ => None,
    };
    let edited_at = if resolved.displayed_edited_text.is_some() {
        resolved.displayed_edited_at.as_deref()
    } else {
        None
    };
    conn.execute(
        "UPDATE history_record
         SET edited_text = ?2,
             edit_source = ?3,
             edited_at = ?4,
             active_edit_id = ?5,
             has_edit_conflict = ?6
         WHERE id = ?1",
        params![
            record_id,
            resolved.displayed_edited_text,
            edit_source,
            edited_at,
            resolved.active_edit_id,
            i32::from(resolved.has_conflict()),
        ],
    )
    .map_err(sql_err)?;
    Ok(resolved)
}

/// Transactional hook executed inside local manual edit, restore, discard, or
/// observed edit persistence.
#[allow(clippy::too_many_arguments)]
pub fn on_local_edit_applied_tx(
    conn: &Connection,
    record_id: &str,
    edit_id: &str,
    edit_kind: EditKind,
    final_text: Option<&str>,
    edited_at: &str,
    metadata: Option<EditMetadataPayload>,
) -> Result<(), StorageError> {
    on_local_edit_applied_with_parents_tx(
        conn,
        record_id,
        edit_id,
        edit_kind,
        final_text,
        edited_at,
        metadata,
        None,
    )
}

/// Transactional hook accepting an optional explicit parent snapshot captured
/// when the user opened the editor or conflict resolver (S3), plus enforcing
/// record space ownership so edits to an old space never enter the active space (S1).
#[allow(clippy::too_many_arguments)]
pub fn on_local_edit_applied_with_parents_tx(
    conn: &Connection,
    record_id: &str,
    edit_id: &str,
    edit_kind: EditKind,
    final_text: Option<&str>,
    edited_at: &str,
    metadata: Option<EditMetadataPayload>,
    explicit_parent_edit_ids: Option<Vec<String>>,
) -> Result<(), StorageError> {
    let row: Option<(String, i32, bool, bool, Option<String>)> = conn
        .query_row(
            "SELECT mode, error_code, is_final, sync_eligible, sync_space_id
             FROM history_record WHERE id = ?1",
            params![record_id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get::<_, i32>(2)? != 0,
                    r.get::<_, i32>(3)? != 0,
                    r.get(4)?,
                ))
            },
        )
        .optional()
        .map_err(sql_err)?;
    let Some((mode, error_code, is_final, sync_eligible, record_space)) = row else {
        return Ok(());
    };

    let device_id = local_device_id_in(conn)?;
    let space = active_space_in(conn)?;
    let parent_edit_ids = if edit_kind.is_manual() {
        match explicit_parent_edit_ids {
            Some(parents) => parents,
            None => {
                let existing = load_edit_nodes_in(conn, record_id)?;
                resolve_edits(&existing).next_parent_ids
            }
        }
    } else {
        Vec::new()
    };

    let hlc = tick_hlc_tx(conn, space.as_ref(), &device_id)?;
    let now = Utc::now().to_rfc3339();
    let parents_json = serde_json::to_string(&parent_edit_ids)
        .map_err(|e| StorageError::SerializeFailed(e.to_string()))?;
    let meta_json = metadata
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|e| StorageError::SerializeFailed(e.to_string()))?;

    // S1: Only share into the active space if the record belongs to that exact space.
    let should_share = space.as_ref().is_some_and(|s| {
        record_space.as_deref() == Some(s.space_id.as_str())
            && can_share_history_edit(
                &s.scope_config(),
                RecordEligibility {
                    mode: &mode,
                    error_code,
                    is_final,
                },
                sync_eligible,
            )
    });
    let space_id_opt = if should_share {
        space.as_ref().map(|s| s.space_id.as_str())
    } else {
        None
    };

    let normalized_text = match edit_kind {
        EditKind::ManualRestore => None,
        EditKind::Manual | EditKind::Observed => final_text.map(str::to_string),
    };

    conn.execute(
        "INSERT OR REPLACE INTO history_edit_versions (
            edit_id, record_id, space_id, source_device_id, edit_kind,
            parent_edit_ids_json, hlc, edited_at, final_text, metadata_json,
            is_shared, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            edit_id,
            record_id,
            space_id_opt,
            device_id,
            edit_kind.as_str(),
            parents_json,
            hlc,
            edited_at,
            normalized_text,
            meta_json,
            i32::from(should_share),
            now,
        ],
    )
    .map_err(sql_err)?;

    project_resolved_edits_to_history_tx(conn, record_id)?;

    if should_share {
        if let Some(space) = space.as_ref() {
            let op = HistoryOpPayload::Edit(HistoryEditPayload {
                schema: PAYLOAD_SCHEMA_V1,
                edit_id: edit_id.to_string(),
                record_id: record_id.to_string(),
                source_device_id: device_id,
                edit_kind,
                parent_edit_ids,
                hlc,
                edited_at: edited_at.to_string(),
                final_text: normalized_text,
                metadata,
            });
            enqueue_outbox_op_tx(
                conn,
                &space.space_id,
                &format!("edit:{edit_id}"),
                SyncDomain::Edits,
                "history.edit",
                record_id,
                &op,
            )?;
        }
    }

    Ok(())
}

/// Transactional hook executed BEFORE `DELETE FROM history_record` in the same
/// SQLite transaction.
pub fn on_local_history_deleted_tx(
    conn: &Connection,
    record_id: &str,
    is_user_delete: bool,
) -> Result<(), StorageError> {
    let rec_meta: Option<(bool, Option<String>)> = conn
        .query_row(
            "SELECT sync_eligible, sync_space_id FROM history_record WHERE id = ?1",
            params![record_id],
            |r| Ok((r.get::<_, i32>(0)? != 0, r.get(1)?)),
        )
        .optional()
        .map_err(sql_err)?;
    let (sync_eligible, record_space) = rec_meta.unwrap_or((false, None));

    conn.execute(
        "DELETE FROM history_edit_versions WHERE record_id = ?1",
        params![record_id],
    )
    .map_err(sql_err)?;

    if !is_user_delete {
        return Ok(());
    }

    let Some(space) = active_space_in(conn)? else {
        return Ok(());
    };
    // S1: Only propagate delete tombstones into the space that actually owns this record.
    if record_space.as_deref() != Some(space.space_id.as_str()) {
        return Ok(());
    }
    if !can_send_history_delete(&space.scope_config(), sync_eligible) {
        return Ok(());
    }

    let device_id = local_device_id_in(conn)?;
    let hlc = tick_hlc_tx(conn, Some(&space), &device_id)?;
    let now = Utc::now().to_rfc3339();

    conn.execute(
        "INSERT INTO history_tombstones (space_id, record_id, deleted_by_device_id, hlc, deleted_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(space_id, record_id) DO UPDATE SET
            hlc = CASE WHEN excluded.hlc > history_tombstones.hlc THEN excluded.hlc ELSE history_tombstones.hlc END",
        params![space.space_id, record_id, device_id, hlc, now],
    )
    .map_err(sql_err)?;

    // Cancel any unsent create/edit for this record before enqueueing the delete tombstone.
    conn.execute(
        "DELETE FROM sync_outbox_ops
         WHERE space_id = ?1 AND record_id = ?2 AND op_type != 'history.delete'",
        params![space.space_id, record_id],
    )
    .map_err(sql_err)?;

    let op = HistoryOpPayload::Delete(HistoryDeletePayload {
        schema: PAYLOAD_SCHEMA_V1,
        record_id: record_id.to_string(),
        source_device_id: device_id,
        hlc,
        deleted_at: now,
    });
    enqueue_outbox_op_tx(
        conn,
        &space.space_id,
        &format!("delete:{record_id}"),
        SyncDomain::History,
        "history.delete",
        record_id,
        &op,
    )
}

fn enqueue_outbox_op_tx(
    conn: &Connection,
    space_id: &str,
    op_id: &str,
    domain: SyncDomain,
    op_type: &str,
    record_id: &str,
    payload: &HistoryOpPayload,
) -> Result<(), StorageError> {
    let payload_json = serde_json::to_string(payload)
        .map_err(|e| StorageError::SerializeFailed(e.to_string()))?;
    conn.execute(
        "INSERT OR REPLACE INTO sync_outbox_ops (
            op_id, space_id, domain, op_type, record_id, payload_json, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            op_id,
            space_id,
            domain.as_str(),
            op_type,
            record_id,
            payload_json,
            Utc::now().to_rfc3339(),
        ],
    )
    .map_err(sql_err)?;
    Ok(())
}

/// Build the local device's `totals` stats shard snapshot for publishing.
pub fn build_local_totals_shard_in(
    conn: &Connection,
    device_id: &str,
    revision: i64,
) -> Result<StatsTotalsPayload, StorageError> {
    let row = conn
        .query_row(
            "SELECT total_recording_count, total_characters, total_duration_ms,
                    llm_correction_count, ai_correction_count,
                    tts_read_count, tts_translate_count, tts_characters,
                    tts_duration_ms, tts_llm_count
             FROM device_usage_stats
             WHERE device_id = ?1",
            params![device_id],
            |r| {
                Ok((
                    DictationTotalsPayload {
                        recording_count: r.get(0)?,
                        total_characters: r.get(1)?,
                        total_duration_ms: r.get(2)?,
                        llm_invoked_count: r.get(3)?,
                        ai_correction_count: r.get(4)?,
                    },
                    TtsTotalsPayload {
                        read_count: r.get(5)?,
                        translate_count: r.get(6)?,
                        characters: r.get(7)?,
                        duration_ms: r.get(8)?,
                        llm_count: r.get(9)?,
                    },
                ))
            },
        )
        .optional()
        .map_err(sql_err)?;

    let (dictation, tts) = row.unwrap_or_default();
    Ok(StatsTotalsPayload {
        schema: PAYLOAD_SCHEMA_V1,
        device_id: device_id.to_string(),
        revision,
        updated_at: Utc::now().to_rfc3339(),
        dictation,
        tts,
    })
}

/// Build the local device's `daily:YYYY-MM` stats shard snapshot for publishing.
pub fn build_local_daily_month_shard_in(
    conn: &Connection,
    device_id: &str,
    month: &str,
    revision: i64,
) -> Result<StatsDailyMonthPayload, StorageError> {
    let like_prefix = format!("{month}-%");
    let mut app_stmt = conn
        .prepare(
            "SELECT local_date, feature_kind, platform, app_key, app_name,
                    usage_count, total_characters, total_duration_ms, llm_count, last_used_at
             FROM daily_usage_stats
             WHERE device_id = ?1 AND local_date LIKE ?2
             ORDER BY local_date ASC, feature_kind ASC, platform ASC, app_key ASC",
        )
        .map_err(sql_err)?;
    let app_rows = app_stmt
        .query_map(params![device_id, like_prefix], |r| {
            Ok(DailyAppStatPayload {
                local_date: r.get(0)?,
                feature_kind: r.get(1)?,
                platform: r.get(2)?,
                app_key: r.get(3)?,
                app_name: r.get(4)?,
                usage_count: r.get(5)?,
                total_characters: r.get(6)?,
                total_duration_ms: r.get(7)?,
                llm_count: r.get(8)?,
                last_used_at: r.get(9)?,
            })
        })
        .map_err(sql_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_err)?;

    let mut model_stmt = conn
        .prepare(
            "SELECT local_date, asr_model_name, llm_state, llm_model_name,
                    usage_count, total_characters, total_duration_ms, ai_correction_count
             FROM daily_model_usage_stats
             WHERE device_id = ?1 AND local_date LIKE ?2
             ORDER BY local_date ASC, asr_model_name ASC, llm_state ASC, llm_model_name ASC",
        )
        .map_err(sql_err)?;
    let model_rows = model_stmt
        .query_map(params![device_id, like_prefix], |r| {
            let state_str: String = r.get(2)?;
            let llm_state = state_str.parse().unwrap_or(LlmUsageState::None);
            Ok(DailyModelStatPayload {
                local_date: r.get(0)?,
                asr_model_name: r.get(1)?,
                llm_state,
                llm_model_name: r.get(3)?,
                usage_count: r.get(4)?,
                total_characters: r.get(5)?,
                total_duration_ms: r.get(6)?,
                ai_correction_count: r.get(7)?,
            })
        })
        .map_err(sql_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_err)?;

    Ok(StatsDailyMonthPayload {
        schema: PAYLOAD_SCHEMA_V1,
        device_id: device_id.to_string(),
        month: month.to_string(),
        revision,
        updated_at: Utc::now().to_rfc3339(),
        app_rows,
        model_rows,
    })
}

/// Apply a remote `totals` stats shard inside the receiver's transaction.
/// Ignores stale/duplicate revisions (`incoming <= current`).
pub fn apply_remote_stats_totals_tx(
    conn: &Connection,
    space_id: &str,
    payload: &StatsTotalsPayload,
) -> Result<bool, StorageError> {
    let current_rev: i64 = conn
        .query_row(
            "SELECT remote_revision FROM sync_stats_shards
             WHERE space_id = ?1 AND device_id = ?2 AND shard_key = 'totals'",
            params![space_id, payload.device_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sql_err)?
        .unwrap_or(0);

    if !should_accept_stats_revision(current_rev, payload.revision) {
        return Ok(false);
    }

    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO device_usage_stats (
            device_id, total_recording_count, total_characters, total_duration_ms,
            llm_correction_count, ai_correction_count,
            tts_read_count, tts_translate_count, tts_characters, tts_duration_ms, tts_llm_count,
            last_updated
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT(device_id) DO UPDATE SET
            total_recording_count = excluded.total_recording_count,
            total_characters = excluded.total_characters,
            total_duration_ms = excluded.total_duration_ms,
            llm_correction_count = excluded.llm_correction_count,
            ai_correction_count = excluded.ai_correction_count,
            tts_read_count = excluded.tts_read_count,
            tts_translate_count = excluded.tts_translate_count,
            tts_characters = excluded.tts_characters,
            tts_duration_ms = excluded.tts_duration_ms,
            tts_llm_count = excluded.tts_llm_count,
            last_updated = excluded.last_updated",
        params![
            payload.device_id,
            payload.dictation.recording_count.max(0),
            payload.dictation.total_characters.max(0),
            payload.dictation.total_duration_ms.max(0),
            payload.dictation.llm_invoked_count.max(0),
            payload.dictation.ai_correction_count.max(0),
            payload.tts.read_count.max(0),
            payload.tts.translate_count.max(0),
            payload.tts.characters.max(0),
            payload.tts.duration_ms.max(0),
            payload.tts.llm_count.max(0),
            payload.updated_at,
        ],
    )
    .map_err(sql_err)?;

    conn.execute(
        "INSERT INTO sync_stats_shards (
            space_id, device_id, shard_key, local_revision, acked_revision, remote_revision, last_synced_at, updated_at
         ) VALUES (?1, ?2, 'totals', 0, 0, ?3, ?4, ?4)
         ON CONFLICT(space_id, device_id, shard_key) DO UPDATE SET
            remote_revision = excluded.remote_revision,
            last_synced_at = excluded.last_synced_at,
            updated_at = excluded.updated_at",
        params![space_id, payload.device_id, payload.revision, now],
    )
    .map_err(sql_err)?;

    // N1: Use remote shard `updated_at` (never receiver's local clock) and advance monotonically.
    conn.execute(
        "UPDATE sync_devices
         SET last_synced_at = CASE
                WHEN last_synced_at IS NULL OR ?3 > last_synced_at THEN ?3
                ELSE last_synced_at
             END,
             updated_at = ?4
         WHERE space_id = ?1 AND device_id = ?2",
        params![space_id, payload.device_id, payload.updated_at, now],
    )
    .map_err(sql_err)?;
    Ok(true)
}

/// Apply a remote `daily:YYYY-MM` stats shard atomically inside the receiver's transaction.
pub fn apply_remote_stats_daily_month_tx(
    conn: &Connection,
    space_id: &str,
    payload: &StatsDailyMonthPayload,
) -> Result<bool, StorageError> {
    let shard_key = format!("daily:{}", payload.month);
    let current_rev: i64 = conn
        .query_row(
            "SELECT remote_revision FROM sync_stats_shards
             WHERE space_id = ?1 AND device_id = ?2 AND shard_key = ?3",
            params![space_id, payload.device_id, shard_key],
            |r| r.get(0),
        )
        .optional()
        .map_err(sql_err)?
        .unwrap_or(0);

    if !should_accept_stats_revision(current_rev, payload.revision) {
        return Ok(false);
    }

    let like_prefix = format!("{}-%", payload.month);
    conn.execute(
        "DELETE FROM daily_usage_stats WHERE device_id = ?1 AND local_date LIKE ?2",
        params![payload.device_id, like_prefix],
    )
    .map_err(sql_err)?;
    conn.execute(
        "DELETE FROM daily_model_usage_stats WHERE device_id = ?1 AND local_date LIKE ?2",
        params![payload.device_id, like_prefix],
    )
    .map_err(sql_err)?;

    for row in &payload.app_rows {
        conn.execute(
            "INSERT INTO daily_usage_stats (
                device_id, local_date, feature_kind, platform, app_key, app_name,
                usage_count, total_characters, total_duration_ms, llm_count, last_used_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                payload.device_id,
                row.local_date,
                row.feature_kind,
                row.platform,
                row.app_key,
                row.app_name,
                row.usage_count.max(0),
                row.total_characters.max(0),
                row.total_duration_ms.max(0),
                row.llm_count.max(0),
                row.last_used_at,
            ],
        )
        .map_err(sql_err)?;
    }

    for row in &payload.model_rows {
        conn.execute(
            "INSERT INTO daily_model_usage_stats (
                device_id, local_date, asr_model_name, llm_state, llm_model_name,
                usage_count, total_characters, total_duration_ms, ai_correction_count, last_used_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                payload.device_id,
                row.local_date,
                row.asr_model_name,
                row.llm_state.as_str(),
                row.llm_model_name,
                row.usage_count.max(0),
                row.total_characters.max(0),
                row.total_duration_ms.max(0),
                row.ai_correction_count.max(0),
                payload.updated_at,
            ],
        )
        .map_err(sql_err)?;
    }

    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO sync_stats_shards (
            space_id, device_id, shard_key, local_revision, acked_revision, remote_revision, last_synced_at, updated_at
         ) VALUES (?1, ?2, ?3, 0, 0, ?4, ?5, ?5)
         ON CONFLICT(space_id, device_id, shard_key) DO UPDATE SET
            remote_revision = excluded.remote_revision,
            last_synced_at = excluded.last_synced_at,
            updated_at = excluded.updated_at",
        params![space_id, payload.device_id, shard_key, payload.revision, now],
    )
    .map_err(sql_err)?;

    Ok(true)
}

/// Apply a decrypted remote history operation (`create`, `edit`, or `delete`)
/// inside the receiver's page transaction. Never increments usage stats.
pub fn apply_remote_history_op_tx(
    conn: &Connection,
    space_id: &str,
    local_device_id: &str,
    op: &HistoryOpPayload,
    text_retention_days: u32,
) -> Result<RemoteApplyOutcome, StorageError> {
    match op {
        HistoryOpPayload::Delete(del) => {
            observe_hlc_tx(conn, space_id, local_device_id, &del.hlc)?;
            // S8: Record local audio_path in the same transaction before deleting the row
            // so post-commit cleanup removes the audio file on disk (and retries on failure).
            if let Some(audio_path) = conn
                .query_row(
                    "SELECT audio_path FROM history_record WHERE id = ?1",
                    params![del.record_id],
                    |r| r.get::<_, Option<String>>(0),
                )
                .optional()
                .map_err(sql_err)?
                .flatten()
                .filter(|p| !p.trim().is_empty())
            {
                conn.execute(
                    "INSERT OR IGNORE INTO sync_pending_audio_deletes (path, created_at)
                     VALUES (?1, ?2)",
                    params![audio_path, Utc::now().to_rfc3339()],
                )
                .map_err(sql_err)?;
            }
            conn.execute(
                "INSERT INTO history_tombstones (space_id, record_id, deleted_by_device_id, hlc, deleted_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(space_id, record_id) DO UPDATE SET
                    hlc = CASE WHEN excluded.hlc > history_tombstones.hlc THEN excluded.hlc ELSE history_tombstones.hlc END",
                params![space_id, del.record_id, del.source_device_id, del.hlc, del.deleted_at],
            )
            .map_err(sql_err)?;
            conn.execute(
                "DELETE FROM history_edit_versions WHERE record_id = ?1",
                params![del.record_id],
            )
            .map_err(sql_err)?;
            crate::post_edit::records::delete_source(conn, &del.record_id)?;
            conn.execute(
                "DELETE FROM history_record WHERE id = ?1",
                params![del.record_id],
            )
            .map_err(sql_err)?;
            conn.execute(
                "DELETE FROM sync_outbox_ops WHERE space_id = ?1 AND record_id = ?2 AND op_type != 'history.delete'",
                params![space_id, del.record_id],
            )
            .map_err(sql_err)?;
            Ok(RemoteApplyOutcome::Applied)
        }
        HistoryOpPayload::Create(create) => {
            let tombstoned: bool = conn
                .query_row(
                    "SELECT 1 FROM history_tombstones WHERE space_id = ?1 AND record_id = ?2 LIMIT 1",
                    params![space_id, create.record_id],
                    |_| Ok(true),
                )
                .optional()
                .map_err(sql_err)?
                .unwrap_or(false);
            if tombstoned {
                return Ok(RemoteApplyOutcome::IgnoredTombstoned);
            }

            if text_retention_days > 0 {
                let cutoff = Utc::now() - Duration::days(i64::from(text_retention_days));
                if let Ok(ts) = chrono::DateTime::parse_from_rfc3339(&create.timestamp) {
                    if ts.with_timezone(&Utc) < cutoff {
                        return Ok(RemoteApplyOutcome::IgnoredRetention);
                    }
                }
            }

            let inserted = conn
                .execute(
                    "INSERT OR IGNORE INTO history_record (
                        id, timestamp, text, original_text, ai_correction_applied, llm_invoked,
                        mode, duration_ms, audio_path, is_final, error_code, error_message,
                        source_device_id, asr_model_name, llm_model_name,
                        target_app_platform, target_app_key, target_app_name,
                        sync_eligible, sync_space_id
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, 1, 0, NULL, ?9, ?10, ?11, ?12, ?13, ?14, 1, ?15)",
                    params![
                        create.record_id,
                        create.timestamp,
                        create.text,
                        create.original_text,
                        create.ai_correction_applied,
                        create.llm_invoked,
                        create.mode,
                        create.duration_ms,
                        create.source_device_id,
                        create.asr_model_name,
                        create.llm_model_name,
                        create.target_app_platform,
                        create.target_app_key,
                        create.target_app_name,
                        space_id,
                    ],
                )
                .map_err(sql_err)?;

            if inserted == 0 {
                // Preserve any legacy v1 edited_text before re-projecting versions.
                ensure_legacy_edit_seeded_tx(conn, &create.record_id)?;
                // Ensure existing local row is marked shared with this space and merge any staged edits.
                conn.execute(
                    "UPDATE history_record SET sync_eligible = 1, sync_space_id = ?2 WHERE id = ?1",
                    params![create.record_id, space_id],
                )
                .map_err(sql_err)?;
                project_resolved_edits_to_history_tx(conn, &create.record_id)?;
                return Ok(RemoteApplyOutcome::IgnoredDuplicate);
            }

            // Re-project in case edits arrived before the base record.
            project_resolved_edits_to_history_tx(conn, &create.record_id)?;
            Ok(RemoteApplyOutcome::Applied)
        }
        HistoryOpPayload::Edit(edit) => {
            observe_hlc_tx(conn, space_id, local_device_id, &edit.hlc)?;
            let tombstoned: bool = conn
                .query_row(
                    "SELECT 1 FROM history_tombstones WHERE space_id = ?1 AND record_id = ?2 LIMIT 1",
                    params![space_id, edit.record_id],
                    |_| Ok(true),
                )
                .optional()
                .map_err(sql_err)?
                .unwrap_or(false);
            if tombstoned {
                return Ok(RemoteApplyOutcome::IgnoredTombstoned);
            }

            // If this record has an unseeded legacy v1 edit from a different edit/text,
            // seed it as private before applying the remote edit so it isn't silently lost.
            // If the incoming edit is this device's own identical migrated edit, let the
            // authoritative server edit_id take over directly without creating a duplicate row.
            let local_edited_text: Option<String> = conn
                .query_row(
                    "SELECT edited_text FROM history_record WHERE id = ?1",
                    params![edit.record_id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(sql_err)?
                .flatten();
            let is_same_device_identical_migrated = edit.source_device_id == local_device_id
                && edit.final_text.is_some()
                && edit.final_text == local_edited_text;
            if !is_same_device_identical_migrated {
                ensure_legacy_edit_seeded_tx(conn, &edit.record_id)?;
            }

            let parents_json = serde_json::to_string(&edit.parent_edit_ids)
                .map_err(|e| StorageError::SerializeFailed(e.to_string()))?;
            let meta_json = edit
                .metadata
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(|e| StorageError::SerializeFailed(e.to_string()))?;
            let final_text = match edit.edit_kind {
                EditKind::ManualRestore => None,
                EditKind::Manual | EditKind::Observed => edit.final_text.as_deref(),
            };

            conn.execute(
                "INSERT OR IGNORE INTO history_edit_versions (
                    edit_id, record_id, space_id, source_device_id, edit_kind,
                    parent_edit_ids_json, hlc, edited_at, final_text, metadata_json,
                    is_shared, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 1, ?11)",
                params![
                    edit.edit_id,
                    edit.record_id,
                    space_id,
                    edit.source_device_id,
                    edit.edit_kind.as_str(),
                    parents_json,
                    edit.hlc,
                    edit.edited_at,
                    final_text,
                    meta_json,
                    Utc::now().to_rfc3339(),
                ],
            )
            .map_err(sql_err)?;

            let base_exists: bool = conn
                .query_row(
                    "SELECT 1 FROM history_record WHERE id = ?1 LIMIT 1",
                    params![edit.record_id],
                    |_| Ok(true),
                )
                .optional()
                .map_err(sql_err)?
                .unwrap_or(false);
            if !base_exists {
                return Ok(RemoteApplyOutcome::StagedWithoutBaseRecord);
            }

            project_resolved_edits_to_history_tx(conn, &edit.record_id)?;
            Ok(RemoteApplyOutcome::Applied)
        }
    }
}

/// Remove local audio files queued by remote delete tombstones (S8).
/// Files that cannot be removed right now remain queued for retry on later cycles.
pub fn flush_pending_audio_deletes_in(conn: &Connection) -> Result<usize, StorageError> {
    let paths: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT path FROM sync_pending_audio_deletes ORDER BY created_at ASC")
            .map_err(sql_err)?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(sql_err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sql_err)?;
        rows
    };

    let mut deleted = 0usize;
    for path in paths {
        match std::fs::remove_file(&path) {
            Ok(()) => {
                conn.execute(
                    "DELETE FROM sync_pending_audio_deletes WHERE path = ?1",
                    params![path],
                )
                .map_err(sql_err)?;
                deleted += 1;
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                conn.execute(
                    "DELETE FROM sync_pending_audio_deletes WHERE path = ?1",
                    params![path],
                )
                .map_err(sql_err)?;
            }
            Err(err) => {
                log::warn!(
                    "Deferred audio cleanup failed for {} (will retry): {}",
                    path,
                    err
                );
            }
        }
    }
    Ok(deleted)
}

/// Return recoverable concurrent manual edit alternatives for a history record.
pub fn list_edit_conflict_alternatives_in(
    conn: &Connection,
    record_id: &str,
) -> Result<Vec<EditConflictAlternative>, StorageError> {
    let nodes = load_edit_nodes_in(conn, record_id)?;
    let resolved = resolve_edits(&nodes);
    Ok(resolved
        .conflict_alternatives
        .into_iter()
        .map(|n| EditConflictAlternative {
            edit_id: n.edit_id,
            source_device_id: n.source_device_id,
            edit_kind: n.edit_kind,
            hlc: n.hlc,
            edited_at: n.edited_at,
            final_text: n.final_text,
        })
        .collect())
}

/// Returns true when the DB has switched to per-device v2 aggregation (either
/// active v2 space, legacy baseline row, or multiple devices in `device_usage_stats`).
fn uses_v2_device_aggregation(conn: &Connection) -> Result<bool, StorageError> {
    let has_v2: bool = conn
        .query_row(
            "SELECT (
                EXISTS(SELECT 1 FROM sync_spaces WHERE is_active = 1)
                OR EXISTS(SELECT 1 FROM sync_legacy_baseline)
                OR (SELECT COUNT(*) FROM device_usage_stats) > 1
            )",
            [],
            |r| Ok(r.get::<_, i32>(0)? != 0),
        )
        .map_err(sql_err)?;
    Ok(has_v2)
}

/// Overview usage stats supporting `All` (local latest + other synced devices +
/// legacy baseline) and `Local` (this device only).
pub fn get_overview_usage_stats_in(
    conn: &Connection,
    local_device_id: &str,
    scope: OverviewScope,
) -> Result<UsageStats, StorageError> {
    if scope == OverviewScope::Local {
        return read_device_usage_stats_in(conn, local_device_id);
    }

    if !uses_v2_device_aggregation(conn)? {
        // Pre-migration v1 DB: preserve existing account-wide row in `usage_stats`.
        return conn
            .query_row(
                "SELECT total_duration_ms, total_characters, llm_correction_count, total_recording_count,
                        tts_read_count, tts_translate_count, tts_characters, tts_duration_ms, tts_llm_count
                 FROM usage_stats WHERE id = 1",
                [],
                |row| {
                    Ok(UsageStats {
                        total_duration_ms: row.get(0)?,
                        total_characters: row.get(1)?,
                        llm_correction_count: row.get(2)?,
                        total_recording_count: row.get(3)?,
                        tts: TtsCounters {
                            read_count: row.get(4)?,
                            translate_count: row.get(5)?,
                            characters: row.get(6)?,
                            duration_ms: row.get(7)?,
                            llm_count: row.get(8)?,
                        },
                    })
                },
            )
            .map_err(sql_err);
    }

    let mut stats = conn
        .query_row(
            "SELECT COALESCE(SUM(total_duration_ms), 0),
                    COALESCE(SUM(total_characters), 0),
                    COALESCE(SUM(llm_correction_count), 0),
                    COALESCE(SUM(total_recording_count), 0),
                    COALESCE(SUM(tts_read_count), 0),
                    COALESCE(SUM(tts_translate_count), 0),
                    COALESCE(SUM(tts_characters), 0),
                    COALESCE(SUM(tts_duration_ms), 0),
                    COALESCE(SUM(tts_llm_count), 0)
             FROM device_usage_stats",
            [],
            |row| {
                Ok(UsageStats {
                    total_duration_ms: row.get(0)?,
                    total_characters: row.get(1)?,
                    llm_correction_count: row.get(2)?,
                    total_recording_count: row.get(3)?,
                    tts: TtsCounters {
                        read_count: row.get(4)?,
                        translate_count: row.get(5)?,
                        characters: row.get(6)?,
                        duration_ms: row.get(7)?,
                        llm_count: row.get(8)?,
                    },
                })
            },
        )
        .map_err(sql_err)?;

    let baseline: Option<UsageStats> = conn
        .query_row(
            "SELECT COALESCE(SUM(total_duration_ms), 0),
                    COALESCE(SUM(total_characters), 0),
                    COALESCE(SUM(llm_invoked_count), 0),
                    COALESCE(SUM(recording_count), 0),
                    COALESCE(SUM(tts_read_count), 0),
                    COALESCE(SUM(tts_translate_count), 0),
                    COALESCE(SUM(tts_characters), 0),
                    COALESCE(SUM(tts_duration_ms), 0),
                    COALESCE(SUM(tts_llm_count), 0)
             FROM sync_legacy_baseline",
            [],
            |row| {
                Ok(UsageStats {
                    total_duration_ms: row.get(0)?,
                    total_characters: row.get(1)?,
                    llm_correction_count: row.get(2)?,
                    total_recording_count: row.get(3)?,
                    tts: TtsCounters {
                        read_count: row.get(4)?,
                        translate_count: row.get(5)?,
                        characters: row.get(6)?,
                        duration_ms: row.get(7)?,
                        llm_count: row.get(8)?,
                    },
                })
            },
        )
        .optional()
        .map_err(sql_err)?;

    if let Some(b) = baseline {
        stats.total_duration_ms += b.total_duration_ms;
        stats.total_characters += b.total_characters;
        stats.llm_correction_count += b.llm_correction_count;
        stats.total_recording_count += b.total_recording_count;
        stats.tts.read_count += b.tts.read_count;
        stats.tts.translate_count += b.tts.translate_count;
        stats.tts.characters += b.tts.characters;
        stats.tts.duration_ms += b.tts.duration_ms;
        stats.tts.llm_count += b.tts.llm_count;
    }

    Ok(stats)
}

fn read_device_usage_stats_in(
    conn: &Connection,
    device_id: &str,
) -> Result<UsageStats, StorageError> {
    let row = conn
        .query_row(
            "SELECT total_duration_ms, total_characters, llm_correction_count, total_recording_count,
                    tts_read_count, tts_translate_count, tts_characters, tts_duration_ms, tts_llm_count
             FROM device_usage_stats WHERE device_id = ?1 LIMIT 1",
            params![device_id],
            |row| {
                Ok(UsageStats {
                    total_duration_ms: row.get(0)?,
                    total_characters: row.get(1)?,
                    llm_correction_count: row.get(2)?,
                    total_recording_count: row.get(3)?,
                    tts: TtsCounters {
                        read_count: row.get(4)?,
                        translate_count: row.get(5)?,
                        characters: row.get(6)?,
                        duration_ms: row.get(7)?,
                        llm_count: row.get(8)?,
                    },
                })
            },
        )
        .optional()
        .map_err(sql_err)?;

    Ok(row.unwrap_or(UsageStats {
        total_duration_ms: 0,
        total_characters: 0,
        llm_correction_count: 0,
        total_recording_count: 0,
        tts: TtsCounters::default(),
    }))
}

fn range_start_date(range: AppUsageRange, today: NaiveDate) -> Option<String> {
    let date = match range {
        AppUsageRange::Today => today,
        AppUsageRange::Week => {
            today - Duration::days(i64::from(today.weekday().num_days_from_monday()))
        }
        AppUsageRange::Month => NaiveDate::from_ymd_opt(today.year(), today.month(), 1)?,
        AppUsageRange::All => return None,
    };
    Some(date.format("%Y-%m-%d").to_string())
}

/// Per-app dictation stats across all devices or local device, grouped strictly
/// by `(platform, app_key)` so macOS bundle IDs and Windows exes stay separate.
pub fn get_app_usage_stats_scoped_in(
    conn: &Connection,
    local_device_id: &str,
    scope: OverviewScope,
    range: AppUsageRange,
    today: NaiveDate,
) -> Result<Vec<AppUsageStat>, StorageError> {
    let since_date = range_start_date(range, today);
    let mut sql = String::from(
        "SELECT platform, app_key, app_name, usage_count, total_characters,
                total_duration_ms, llm_count, last_used_at
         FROM daily_usage_stats
         WHERE feature_kind = ?1 AND app_key != ?2",
    );
    let mut bind_values: Vec<rusqlite::types::Value> = vec![
        FEATURE_KIND_DICTATION.to_string().into(),
        UNTRACKED_APP_KEY.to_string().into(),
    ];
    if scope == OverviewScope::Local {
        sql.push_str(&format!(" AND device_id = ?{}", bind_values.len() + 1));
        bind_values.push(local_device_id.to_string().into());
    }
    if let Some(since) = since_date {
        sql.push_str(&format!(" AND local_date >= ?{}", bind_values.len() + 1));
        bind_values.push(since.into());
    }

    let mut stmt = conn.prepare(&sql).map_err(sql_err)?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(bind_values), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, String>(7)?,
            ))
        })
        .map_err(sql_err)?;

    let mut grouped: HashMap<(String, String), AppUsageStat> = HashMap::new();
    for row in rows {
        let (platform, app_key, app_name, count, chars, duration_ms, llm_count, last_used_at) =
            row.map_err(sql_err)?;
        let entry = grouped
            .entry((platform.clone(), app_key.clone()))
            .or_insert_with(|| AppUsageStat {
                platform,
                app_key,
                app_name: app_name.clone(),
                usage_count: 0,
                total_characters: 0,
                total_duration_ms: 0,
                llm_count: 0,
                last_used_at: last_used_at.clone(),
            });
        entry.usage_count += count;
        entry.total_characters += chars;
        entry.total_duration_ms += duration_ms;
        entry.llm_count += llm_count;
        if last_used_at >= entry.last_used_at {
            entry.last_used_at = last_used_at;
            if !app_name.is_empty() {
                entry.app_name = app_name;
            }
        }
    }

    let mut items: Vec<AppUsageStat> = grouped.into_values().collect();
    items.sort_by(|a, b| {
        b.usage_count
            .cmp(&a.usage_count)
            .then_with(|| b.total_characters.cmp(&a.total_characters))
            .then_with(|| a.app_name.cmp(&b.app_name))
    });
    Ok(items)
}

/// 52-week daily activity heatmap across all devices or local device only.
pub fn get_daily_activity_scoped_in(
    conn: &Connection,
    local_device_id: &str,
    scope: OverviewScope,
    today: NaiveDate,
) -> Result<DailyActivityResponse, StorageError> {
    let end_week_monday = today - Duration::days(i64::from(today.weekday().num_days_from_monday()));
    let start_date = end_week_monday - Duration::weeks(51);
    let start_str = start_date.format("%Y-%m-%d").to_string();
    let end_str = today.format("%Y-%m-%d").to_string();

    let (prior_count, prior_characters, prior_duration_ms): (i64, i64, i64) = match scope {
        OverviewScope::Local => conn
            .query_row(
                "SELECT COALESCE(SUM(usage_count), 0),
                        COALESCE(SUM(total_characters), 0),
                        COALESCE(SUM(total_duration_ms), 0)
                 FROM daily_usage_stats
                 WHERE device_id = ?1 AND feature_kind = ?2 AND local_date < ?3",
                params![local_device_id, FEATURE_KIND_DICTATION, start_str],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .map_err(sql_err)?,
        OverviewScope::All => conn
            .query_row(
                "SELECT COALESCE(SUM(usage_count), 0),
                        COALESCE(SUM(total_characters), 0),
                        COALESCE(SUM(total_duration_ms), 0)
                 FROM daily_usage_stats
                 WHERE feature_kind = ?1 AND local_date < ?2",
                params![FEATURE_KIND_DICTATION, start_str],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .map_err(sql_err)?,
    };

    let days = match scope {
        OverviewScope::Local => {
            let mut stmt = conn
                .prepare(
                    "SELECT local_date, SUM(usage_count), SUM(total_characters),
                            SUM(total_duration_ms), SUM(llm_count)
                     FROM daily_usage_stats
                     WHERE device_id = ?1 AND feature_kind = ?2
                       AND local_date >= ?3 AND local_date <= ?4
                     GROUP BY local_date ORDER BY local_date ASC",
                )
                .map_err(sql_err)?;
            let rows = stmt
                .query_map(
                    params![local_device_id, FEATURE_KIND_DICTATION, start_str, end_str],
                    map_daily_point,
                )
                .map_err(sql_err)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(sql_err)?;
            rows
        }
        OverviewScope::All => {
            let mut stmt = conn
                .prepare(
                    "SELECT local_date, SUM(usage_count), SUM(total_characters),
                            SUM(total_duration_ms), SUM(llm_count)
                     FROM daily_usage_stats
                     WHERE feature_kind = ?1
                       AND local_date >= ?2 AND local_date <= ?3
                     GROUP BY local_date ORDER BY local_date ASC",
                )
                .map_err(sql_err)?;
            let rows = stmt
                .query_map(
                    params![FEATURE_KIND_DICTATION, start_str, end_str],
                    map_daily_point,
                )
                .map_err(sql_err)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(sql_err)?;
            rows
        }
    };

    let window_count: i64 = days.iter().map(|d| d.usage_count).sum();
    let window_characters: i64 = days.iter().map(|d| d.total_characters).sum();
    let window_duration_ms: i64 = days.iter().map(|d| d.total_duration_ms).sum();

    let totals = get_overview_usage_stats_in(conn, local_device_id, scope)?;
    let undated_count = (totals.total_recording_count - prior_count - window_count).max(0);
    let undated_characters = (totals.total_characters - prior_characters - window_characters).max(0);
    let undated_duration_ms = (totals.total_duration_ms - prior_duration_ms - window_duration_ms).max(0);

    Ok(DailyActivityResponse {
        start_date: start_str,
        end_date: end_str,
        prior_count,
        prior_characters,
        prior_duration_ms,
        undated_count,
        undated_characters,
        undated_duration_ms,
        days,
    })
}

fn map_daily_point(row: &rusqlite::Row<'_>) -> rusqlite::Result<DailyActivityPoint> {
    Ok(DailyActivityPoint {
        local_date: row.get(0)?,
        usage_count: row.get(1)?,
        total_characters: row.get(2)?,
        total_duration_ms: row.get(3)?,
        llm_count: row.get(4)?,
    })
}

/// Overview model usage insights (ASR + LLM models, distinguishing no-LLM vs
/// legacy unknown-model LLM calls and undated lifetime remainder).
pub fn get_model_usage_stats_scoped_in(
    conn: &Connection,
    local_device_id: &str,
    scope: OverviewScope,
    range: AppUsageRange,
    today: NaiveDate,
) -> Result<ModelUsageResponse, StorageError> {
    let since_date = range_start_date(range, today);
    let mut sql = String::from(
        "SELECT asr_model_name, llm_state, llm_model_name,
                usage_count, total_characters, total_duration_ms, ai_correction_count
         FROM daily_model_usage_stats
         WHERE 1 = 1",
    );
    let mut bind_values: Vec<rusqlite::types::Value> = Vec::new();
    if scope == OverviewScope::Local {
        sql.push_str(&format!(" AND device_id = ?{}", bind_values.len() + 1));
        bind_values.push(local_device_id.to_string().into());
    }
    if let Some(ref since) = since_date {
        sql.push_str(&format!(" AND local_date >= ?{}", bind_values.len() + 1));
        bind_values.push(since.clone().into());
    }

    let mut stmt = conn.prepare(&sql).map_err(sql_err)?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(bind_values), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
            ))
        })
        .map_err(sql_err)?;

    let mut asr_map: HashMap<String, ModelUsageItem> = HashMap::new();
    let mut llm_map: HashMap<String, ModelUsageItem> = HashMap::new();
    let mut total_dictations = 0i64;
    let mut unattributed_asr_count = 0i64;
    let mut unattributed_llm_count = 0i64;
    let mut no_llm_count = 0i64;

    for row in rows {
        let (asr, llm_state, llm_model, count, chars, duration_ms, ai_corr) =
            row.map_err(sql_err)?;
        total_dictations += count;

        if asr.trim().is_empty() {
            unattributed_asr_count += count;
        } else {
            let entry = asr_map.entry(asr.clone()).or_insert_with(|| ModelUsageItem {
                model_name: asr,
                usage_count: 0,
                total_characters: 0,
                total_duration_ms: 0,
                ai_correction_count: 0,
            });
            entry.usage_count += count;
            entry.total_characters += chars;
            entry.total_duration_ms += duration_ms;
            entry.ai_correction_count += ai_corr;
        }

        match llm_state.as_str() {
            "known" if !llm_model.trim().is_empty() => {
                let entry = llm_map
                    .entry(llm_model.clone())
                    .or_insert_with(|| ModelUsageItem {
                        model_name: llm_model,
                        usage_count: 0,
                        total_characters: 0,
                        total_duration_ms: 0,
                        ai_correction_count: 0,
                    });
                entry.usage_count += count;
                entry.total_characters += chars;
                entry.total_duration_ms += duration_ms;
                entry.ai_correction_count += ai_corr;
            }
            "unknown" => unattributed_llm_count += count,
            _ => no_llm_count += count,
        }
    }

    let mut asr_models: Vec<ModelUsageItem> = asr_map.into_values().collect();
    let mut llm_models: Vec<ModelUsageItem> = llm_map.into_values().collect();
    let sort_models = |items: &mut Vec<ModelUsageItem>| {
        items.sort_by(|a, b| {
            b.usage_count
                .cmp(&a.usage_count)
                .then_with(|| b.total_characters.cmp(&a.total_characters))
                .then_with(|| a.model_name.cmp(&b.model_name))
        });
    };
    sort_models(&mut asr_models);
    sort_models(&mut llm_models);

    let undated_count = if since_date.is_none() {
        let totals = get_overview_usage_stats_in(conn, local_device_id, scope)?;
        (totals.total_recording_count - total_dictations).max(0)
    } else {
        0
    };

    Ok(ModelUsageResponse {
        asr_models,
        llm_models,
        total_dictations,
        unattributed_asr_count,
        unattributed_llm_count,
        no_llm_count,
        undated_count,
    })
}

/// List known devices and their most recent sync time for Overview display.
pub fn get_overview_devices_in(
    conn: &Connection,
    local_device_id: &str,
) -> Result<Vec<OverviewDeviceSyncInfo>, StorageError> {
    let active_space = active_space_in(conn)?;
    if let Some(space) = active_space {
        let mut stmt = conn
            .prepare(
                "SELECT d.device_id, d.device_name, d.platform,
                        COALESCE(d.last_synced_at, MAX(s.last_synced_at)) AS last_synced_at
                 FROM sync_devices d
                 LEFT JOIN sync_stats_shards s
                   ON s.space_id = d.space_id AND s.device_id = d.device_id
                 WHERE d.space_id = ?1 AND d.revoked_at IS NULL
                 GROUP BY d.device_id
                 ORDER BY CASE WHEN d.device_id = ?2 THEN 0 ELSE 1 END,
                          last_synced_at DESC, d.device_name ASC",
            )
            .map_err(sql_err)?;
        let rows = stmt
            .query_map(params![space.space_id, local_device_id], |r| {
                let device_id: String = r.get(0)?;
                Ok(OverviewDeviceSyncInfo {
                    is_local: device_id == local_device_id,
                    device_id,
                    device_name: r.get(1)?,
                    platform: r.get(2)?,
                    last_synced_at: r.get(3)?,
                })
            })
            .map_err(sql_err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sql_err)?;
        return Ok(rows);
    }

    // Fallback before v2 pairing: read known devices from `device_registry`.
    let mut stmt = conn
        .prepare(
            "SELECT device_id, device_name, updated_at
             FROM device_registry
             ORDER BY CASE WHEN device_id = ?1 THEN 0 ELSE 1 END, updated_at DESC",
        )
        .map_err(sql_err)?;
    let rows = stmt
        .query_map(params![local_device_id], |r| {
            let device_id: String = r.get(0)?;
            Ok(OverviewDeviceSyncInfo {
                is_local: device_id == local_device_id,
                device_id,
                device_name: r.get(1)?,
                platform: String::new(),
                last_synced_at: r.get(2)?,
            })
        })
        .map_err(sql_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_err)?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE history_record (
                id TEXT PRIMARY KEY,
                timestamp TEXT NOT NULL,
                text TEXT NOT NULL,
                original_text TEXT,
                ai_correction_applied INTEGER DEFAULT 0,
                llm_invoked INTEGER DEFAULT 0,
                mode TEXT NOT NULL,
                duration_ms INTEGER NOT NULL,
                audio_path TEXT,
                is_final INTEGER DEFAULT 1,
                error_code INTEGER DEFAULT 0,
                error_message TEXT,
                source_device_id TEXT,
                asr_model_name TEXT,
                llm_model_name TEXT,
                edited_text TEXT,
                edit_source TEXT,
                edited_at TEXT,
                target_app_platform TEXT,
                target_app_key TEXT,
                target_app_name TEXT
            );
            CREATE TABLE usage_stats (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                total_duration_ms INTEGER DEFAULT 0,
                total_characters INTEGER DEFAULT 0,
                llm_correction_count INTEGER DEFAULT 0,
                last_updated TEXT,
                total_recording_count INTEGER DEFAULT 0,
                tts_read_count INTEGER DEFAULT 0,
                tts_translate_count INTEGER DEFAULT 0,
                tts_characters INTEGER DEFAULT 0,
                tts_duration_ms INTEGER DEFAULT 0,
                tts_llm_count INTEGER DEFAULT 0
            );
            INSERT INTO usage_stats (id) VALUES (1);
            CREATE TABLE device_usage_stats (
                device_id TEXT PRIMARY KEY,
                total_duration_ms INTEGER DEFAULT 0,
                total_characters INTEGER DEFAULT 0,
                llm_correction_count INTEGER DEFAULT 0,
                last_updated TEXT,
                total_recording_count INTEGER DEFAULT 0,
                tts_read_count INTEGER DEFAULT 0,
                tts_translate_count INTEGER DEFAULT 0,
                tts_characters INTEGER DEFAULT 0,
                tts_duration_ms INTEGER DEFAULT 0,
                tts_llm_count INTEGER DEFAULT 0
            );
            CREATE TABLE daily_usage_stats (
                device_id TEXT NOT NULL,
                local_date TEXT NOT NULL,
                feature_kind TEXT NOT NULL,
                platform TEXT NOT NULL,
                app_key TEXT NOT NULL,
                app_name TEXT NOT NULL,
                usage_count INTEGER NOT NULL DEFAULT 0,
                total_characters INTEGER NOT NULL DEFAULT 0,
                total_duration_ms INTEGER NOT NULL DEFAULT 0,
                llm_count INTEGER NOT NULL DEFAULT 0,
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
            "INSERT INTO user_config (key, value) VALUES ('sync_device_id', 'mac-dev')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO device_usage_stats (device_id, total_recording_count, total_characters, total_duration_ms, llm_correction_count)
             VALUES ('mac-dev', 1, 5, 1000, 1)",
            [],
        )
        .unwrap();
        init_schema(&conn).unwrap();
        conn
    }

    fn sample_record(id: &str) -> HistoryRecord {
        HistoryRecord {
            id: id.to_string(),
            timestamp: "2026-11-24T12:00:00Z".to_string(),
            text: "hello".to_string(),
            original_text: Some("helo".to_string()),
            ai_correction_applied: true,
            llm_invoked: true,
            mode: "assistant_hands_free".to_string(),
            duration_ms: 1000,
            audio_path: None,
            is_final: true,
            error_code: 0,
            error_message: None,
            source_device_id: Some("mac-dev".to_string()),
            source_device_name: None,
            asr_model_name: Some("Qwen / flash".to_string()),
            llm_model_name: Some("DeepSeek / flash".to_string()),
            edited_text: None,
            target_app_platform: Some("macos".to_string()),
            target_app_key: Some("com.apple.Notes".to_string()),
            target_app_name: Some("Notes".to_string()),
            has_edit_conflict: false,
            active_edit_id: None,
        }
    }

    #[test]
    fn transactional_local_insert_updates_model_stats_shards_and_outbox() {
        let conn = open_test_db();
        upsert_sync_space_tx(
            &conn,
            "selfhost:srv:acc",
            "selfhost",
            "http://127.0.0.1:8787",
            "srv",
            "acc",
            "mac-dev",
            true,
            true,
            None,
            true,
        )
        .unwrap();

        let rec = sample_record("rec-1");
        conn.execute(
            "INSERT INTO history_record (id, timestamp, text, mode, duration_ms, source_device_id, asr_model_name, llm_model_name, llm_invoked, ai_correction_applied)
             VALUES (?1, ?2, ?3, ?4, ?5, 'mac-dev', ?6, ?7, 1, 1)",
            params![rec.id, rec.timestamp, rec.text, rec.mode, rec.duration_ms, rec.asr_model_name, rec.llm_model_name],
        )
        .unwrap();
        on_local_history_inserted_tx(&conn, &rec, "2026-11-24").unwrap();

        let rev_totals: i64 = conn
            .query_row(
                "SELECT local_revision FROM sync_stats_shards WHERE shard_key = 'totals'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let rev_month: i64 = conn
            .query_row(
                "SELECT local_revision FROM sync_stats_shards WHERE shard_key = 'daily:2026-11'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let outbox_cnt: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sync_outbox_ops WHERE op_id = 'create:rec-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rev_totals, 1);
        assert_eq!(rev_month, 1);
        assert_eq!(outbox_cnt, 1);

        let models = get_model_usage_stats_scoped_in(
            &conn,
            "mac-dev",
            OverviewScope::Local,
            AppUsageRange::All,
            NaiveDate::from_ymd_opt(2026, 11, 24).unwrap(),
        )
        .unwrap();
        assert_eq!(models.asr_models.len(), 1);
        assert_eq!(models.asr_models[0].model_name, "Qwen / flash");
        assert_eq!(models.llm_models.len(), 1);
        assert_eq!(models.llm_models[0].model_name, "DeepSeek / flash");
    }

    #[test]
    fn stats_only_device_never_enqueues_history_create_or_edits() {
        let conn = open_test_db();
        upsert_sync_space_tx(
            &conn,
            "selfhost:srv:acc",
            "selfhost",
            "http://127.0.0.1:8787",
            "srv",
            "acc",
            "mac-dev",
            false,
            false,
            None,
            true,
        )
        .unwrap();

        let rec = sample_record("rec-private");
        conn.execute(
            "INSERT INTO history_record (id, timestamp, text, mode, duration_ms, source_device_id)
             VALUES (?1, ?2, ?3, ?4, ?5, 'mac-dev')",
            params![rec.id, rec.timestamp, rec.text, rec.mode, rec.duration_ms],
        )
        .unwrap();
        on_local_history_inserted_tx(&conn, &rec, "2026-11-24").unwrap();
        on_local_edit_applied_tx(
            &conn,
            "rec-private",
            "ed-1",
            EditKind::Manual,
            Some("edited"),
            "2026-11-24T12:01:00Z",
            None,
        )
        .unwrap();

        let outbox_cnt: i64 = conn
            .query_row("SELECT COUNT(*) FROM sync_outbox_ops", [], |r| r.get(0))
            .unwrap();
        assert_eq!(outbox_cnt, 0);
    }

    #[test]
    fn remote_stats_revision_101_then_100_keeps_101_and_aggregates_all_vs_local() {
        let conn = open_test_db();
        upsert_sync_space_tx(
            &conn,
            "selfhost:srv:acc",
            "selfhost",
            "http://127.0.0.1:8787",
            "srv",
            "acc",
            "mac-dev",
            false,
            false,
            None,
            true,
        )
        .unwrap();

        let rev101 = StatsTotalsPayload {
            schema: PAYLOAD_SCHEMA_V1,
            device_id: "win-dev".to_string(),
            revision: 101,
            updated_at: "2026-11-24T12:05:00Z".to_string(),
            dictation: DictationTotalsPayload {
                recording_count: 101,
                total_characters: 5050,
                total_duration_ms: 90000,
                llm_invoked_count: 90,
                ai_correction_count: 80,
            },
            tts: TtsTotalsPayload {
                read_count: 3,
                translate_count: 1,
                characters: 400,
                duration_ms: 12000,
                llm_count: 2,
            },
        };
        let rev100 = StatsTotalsPayload {
            revision: 100,
            dictation: DictationTotalsPayload {
                recording_count: 100,
                total_characters: 5000,
                ..rev101.dictation.clone()
            },
            ..rev101.clone()
        };

        assert!(apply_remote_stats_totals_tx(&conn, "selfhost:srv:acc", &rev101).unwrap());
        assert!(!apply_remote_stats_totals_tx(&conn, "selfhost:srv:acc", &rev100).unwrap());

        let all = get_overview_usage_stats_in(&conn, "mac-dev", OverviewScope::All).unwrap();
        let local = get_overview_usage_stats_in(&conn, "mac-dev", OverviewScope::Local).unwrap();
        assert_eq!(local.total_recording_count, 1);
        assert_eq!(all.total_recording_count, 102); // 1 (mac-dev) + 101 (win-dev)
        assert_eq!(all.total_characters, 5055);
        assert_eq!(all.tts.read_count, 3);
    }

    #[test]
    fn remote_edit_before_create_and_delete_before_create_behave_deterministically() {
        let conn = open_test_db();
        upsert_sync_space_tx(
            &conn,
            "selfhost:srv:acc",
            "selfhost",
            "http://127.0.0.1:8787",
            "srv",
            "acc",
            "mac-dev",
            true,
            true,
            None,
            true,
        )
        .unwrap();

        // 1. Edit arrives before base record
        let edit_op = HistoryOpPayload::Edit(HistoryEditPayload {
            schema: PAYLOAD_SCHEMA_V1,
            edit_id: "e1".into(),
            record_id: "r-order".into(),
            source_device_id: "win-dev".into(),
            edit_kind: EditKind::Manual,
            parent_edit_ids: vec![],
            hlc: Hlc::new(2000, 0, "win-dev").unwrap().to_wire_string(),
            edited_at: "2026-11-24T12:02:00Z".into(),
            final_text: Some("edited first".into()),
            metadata: None,
        });
        assert_eq!(
            apply_remote_history_op_tx(&conn, "selfhost:srv:acc", "mac-dev", &edit_op, 0).unwrap(),
            RemoteApplyOutcome::StagedWithoutBaseRecord
        );

        let create_op = HistoryOpPayload::Create(HistoryCreatePayload {
            schema: PAYLOAD_SCHEMA_V1,
            record_id: "r-order".into(),
            source_device_id: "win-dev".into(),
            timestamp: "2026-11-24T12:00:00Z".into(),
            text: "original".into(),
            original_text: None,
            ai_correction_applied: false,
            llm_invoked: false,
            mode: "assistant_hands_free".into(),
            duration_ms: 1500,
            asr_model_name: None,
            llm_model_name: None,
            target_app_platform: None,
            target_app_key: None,
            target_app_name: None,
        });
        assert_eq!(
            apply_remote_history_op_tx(&conn, "selfhost:srv:acc", "mac-dev", &create_op, 0).unwrap(),
            RemoteApplyOutcome::Applied
        );

        let edited: Option<String> = conn
            .query_row(
                "SELECT edited_text FROM history_record WHERE id = 'r-order'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(edited.as_deref(), Some("edited first"));

        // 2. Delete arrives before create -> create is ignored forever
        let del_op = HistoryOpPayload::Delete(HistoryDeletePayload {
            schema: PAYLOAD_SCHEMA_V1,
            record_id: "r-dead".into(),
            source_device_id: "win-dev".into(),
            hlc: Hlc::new(3000, 0, "win-dev").unwrap().to_wire_string(),
            deleted_at: "2026-11-24T12:05:00Z".into(),
        });
        assert_eq!(
            apply_remote_history_op_tx(&conn, "selfhost:srv:acc", "mac-dev", &del_op, 0).unwrap(),
            RemoteApplyOutcome::Applied
        );
        let dead_create = HistoryOpPayload::Create(HistoryCreatePayload {
            record_id: "r-dead".into(),
            ..match create_op {
                HistoryOpPayload::Create(c) => c,
                _ => unreachable!(),
            }
        });
        assert_eq!(
            apply_remote_history_op_tx(&conn, "selfhost:srv:acc", "mac-dev", &dead_create, 0).unwrap(),
            RemoteApplyOutcome::IgnoredTombstoned
        );
    }

    #[test]
    fn narrowing_scope_cancels_pending_creates_and_edits_but_keeps_deletes() {
        let conn = open_test_db();
        upsert_sync_space_tx(
            &conn,
            "selfhost:srv:acc",
            "selfhost",
            "http://127.0.0.1:8787",
            "srv",
            "acc",
            "mac-dev",
            true,
            true,
            None,
            true,
        )
        .unwrap();

        let rec1 = sample_record("r1");
        let rec2 = sample_record("r2");
        for r in [&rec1, &rec2] {
            conn.execute(
                "INSERT INTO history_record (id, timestamp, text, mode, duration_ms, source_device_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'mac-dev')",
                params![r.id, r.timestamp, r.text, r.mode, r.duration_ms],
            )
            .unwrap();
            on_local_history_inserted_tx(&conn, r, "2026-11-24").unwrap();
        }
        on_local_edit_applied_tx(
            &conn,
            "r1",
            "ed-r1",
            EditKind::Manual,
            Some("r1 edited"),
            "2026-11-24T12:01:00Z",
            None,
        )
        .unwrap();
        on_local_history_deleted_tx(&conn, "r2", true).unwrap();

        // Narrow to stats-only
        update_sync_scope_tx(&conn, "selfhost:srv:acc", false, false).unwrap();

        let ops: Vec<String> = conn
            .prepare("SELECT op_type FROM sync_outbox_ops ORDER BY op_id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(ops, vec!["history.delete".to_string()]);
    }

    #[test]
    fn m3_v1_outbox_handoff_is_idempotent_and_prevents_delete_resurrection() {
        let conn = open_test_db();
        conn.execute_batch(
            "CREATE TABLE sync_state(id INTEGER PRIMARY KEY, current_server_id TEXT, current_account_id TEXT);
             INSERT INTO sync_state VALUES(1, 'srv', 'acc');
             CREATE TABLE IF NOT EXISTS sync_outbox (
                event_id TEXT PRIMARY KEY,
                event_type TEXT NOT NULL,
                record_id TEXT,
                payload TEXT NOT NULL,
                created_at TEXT NOT NULL
            );",
        )
        .unwrap();
        upsert_sync_space_tx(
            &conn,
            "selfhost:srv:acc",
            "selfhost",
            "http://127.0.0.1:8787",
            "srv",
            "acc",
            "win-dev",
            true,
            false,
            None,
            true,
        )
        .unwrap();

        // Record r-keep: pending create; r-del: pending delete; r-both: upsert then delete (delete wins)
        conn.execute(
            "INSERT INTO history_record (id, timestamp, text, mode, duration_ms, source_device_id)
             VALUES ('r-keep', '2026-11-24T12:00:00Z', 'offline text', 'assistant_hands_free', 1500, 'win-dev')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sync_outbox (event_id, event_type, record_id, payload, created_at) VALUES
             ('ev-1', 'history.upsert', 'r-keep', '{}', '2026-11-24T12:00:01Z'),
             ('ev-2', 'history.delete', 'r-del', '{}', '2026-11-24T12:00:02Z'),
             ('ev-3', 'history.upsert', 'r-both', '{}', '2026-11-24T12:00:03Z'),
             ('ev-4', 'history.delete', 'r-both', '{}', '2026-11-24T12:00:04Z')",
            [],
        )
        .unwrap();

        assert_eq!(
            handoff_v1_outbox_to_v2_tx(&conn, "other", "win-dev", true)
                .unwrap()
                .deletes_handed_off,
            0
        );
        let first = handoff_v1_outbox_to_v2_tx(&conn, "selfhost:srv:acc", "win-dev", true).unwrap();
        assert!(!first.already_completed);
        assert_eq!(first.deletes_handed_off, 2);
        assert_eq!(first.creates_handed_off, 1);

        // Second execution is a no-op (idempotent)
        let second =
            handoff_v1_outbox_to_v2_tx(&conn, "selfhost:srv:acc", "win-dev", true).unwrap();
        assert!(second.already_completed);
        assert_eq!(second.deletes_handed_off, 0);
        assert_eq!(second.creates_handed_off, 0);

        assert!(
            handoff_v1_outbox_to_v2_tx(&conn, "other", "win-dev", true)
                .unwrap()
                .already_completed
        );
        // Original v1 sync_outbox is preserved intact for reconciliation
        let v1_cnt: i64 = conn
            .query_row("SELECT COUNT(*) FROM sync_outbox", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v1_cnt, 4);

        // First v2 pull receiving r-del cannot resurrect it because tombstone is already persisted
        let remote_create_deleted = HistoryOpPayload::Create(HistoryCreatePayload {
            schema: PAYLOAD_SCHEMA_V1,
            record_id: "r-del".into(),
            source_device_id: "mac-dev".into(),
            timestamp: "2026-11-24T11:00:00Z".into(),
            text: "should not return".into(),
            original_text: None,
            ai_correction_applied: false,
            llm_invoked: false,
            mode: "assistant_hands_free".into(),
            duration_ms: 1000,
            asr_model_name: None,
            llm_model_name: None,
            target_app_platform: None,
            target_app_key: None,
            target_app_name: None,
        });
        assert_eq!(
            apply_remote_history_op_tx(
                &conn,
                "selfhost:srv:acc",
                "win-dev",
                &remote_create_deleted,
                0
            )
            .unwrap(),
            RemoteApplyOutcome::IgnoredTombstoned
        );
    }

    #[test]
    fn m5_duplicate_create_and_explicit_import_preserve_v1_local_edits() {
        let conn = open_test_db();
        upsert_sync_space_tx(
            &conn,
            "selfhost:srv:acc",
            "selfhost",
            "http://127.0.0.1:8787",
            "srv",
            "acc",
            "win-dev",
            true,
            false,
            None,
            true,
        )
        .unwrap();

        // Pre-existing v1 record with edited_text and ZERO history_edit_versions rows
        conn.execute(
            "INSERT INTO history_record (id, timestamp, text, mode, duration_ms, source_device_id, edited_text, edit_source, edited_at)
             VALUES ('r-v1-edit', '2026-11-24T10:00:00Z', 'raw text', 'assistant_hands_free', 1200, 'win-dev', 'manual v1 fix', 'manual', '2026-11-24T10:05:00Z')",
            [],
        )
        .unwrap();

        let dup_create = HistoryOpPayload::Create(HistoryCreatePayload {
            schema: PAYLOAD_SCHEMA_V1,
            record_id: "r-v1-edit".into(),
            source_device_id: "win-dev".into(),
            timestamp: "2026-11-24T10:00:00Z".into(),
            text: "raw text".into(),
            original_text: None,
            ai_correction_applied: false,
            llm_invoked: false,
            mode: "assistant_hands_free".into(),
            duration_ms: 1200,
            asr_model_name: None,
            llm_model_name: None,
            target_app_platform: None,
            target_app_key: None,
            target_app_name: None,
        });

        // Duplicate create with edits OFF must preserve edited_text and seed 1 private version
        for _ in 0..2 {
            assert_eq!(
                apply_remote_history_op_tx(&conn, "selfhost:srv:acc", "win-dev", &dup_create, 0)
                    .unwrap(),
                RemoteApplyOutcome::IgnoredDuplicate
            );
            let edited: Option<String> = conn
                .query_row(
                    "SELECT edited_text FROM history_record WHERE id = 'r-v1-edit'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(edited.as_deref(), Some("manual v1 fix"));
        }

        let (ver_cnt, shared_cnt): (i64, i64) = conn
            .query_row(
                "SELECT COUNT(*), SUM(is_shared) FROM history_edit_versions WHERE record_id = 'r-v1-edit'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((ver_cnt, shared_cnt), (1, 0));

        // Explicit import promotes that exact private row to shared and enqueues 1 history.edit
        update_sync_scope_tx(&conn, "selfhost:srv:acc", true, true).unwrap();
        let space = active_space_in(&conn).unwrap().unwrap();
        assert_eq!(import_existing_edits_tx(&conn, &space).unwrap(), 1);
        assert_eq!(import_existing_edits_tx(&conn, &space).unwrap(), 0); // idempotent

        let (ver_cnt_after, shared_cnt_after): (i64, i64) = conn
            .query_row(
                "SELECT COUNT(*), SUM(is_shared) FROM history_edit_versions WHERE record_id = 'r-v1-edit'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((ver_cnt_after, shared_cnt_after), (1, 1));
    }

    #[test]
    fn s1_s2_s3_s8_n1_cross_space_repair_editor_parents_audio_cleanup_and_device_times() {
        let conn = open_test_db();
        // Create space A with record r-space-a
        upsert_sync_space_tx(
            &conn,
            "space-a",
            "selfhost",
            "http://srv-a",
            "srv-a",
            "acc-a",
            "mac-dev",
            true,
            true,
            None,
            true,
        )
        .unwrap();
        let rec_a = sample_record("r-space-a");
        conn.execute(
            "INSERT INTO history_record (id, timestamp, text, mode, duration_ms, source_device_id)
             VALUES (?1, ?2, ?3, ?4, ?5, 'mac-dev')",
            params![rec_a.id, rec_a.timestamp, rec_a.text, rec_a.mode, rec_a.duration_ms],
        )
        .unwrap();
        on_local_history_inserted_tx(&conn, &rec_a, "2026-11-24").unwrap();

        // S2: Disconnect space A -> status='disabled', last_error='err'; re-pairing must reset both!
        conn.execute(
            "UPDATE sync_spaces SET status = 'revoked', last_error = 'revoked' WHERE space_id = 'space-a'",
            [],
        )
        .unwrap();
        upsert_sync_space_tx(
            &conn,
            "space-a",
            "selfhost",
            "http://srv-a",
            "srv-a",
            "acc-a",
            "mac-dev",
            true,
            true,
            None,
            true,
        )
        .unwrap();
        let repaired = active_space_in(&conn).unwrap().unwrap();
        assert_eq!(repaired.status, "idle");
        assert_eq!(repaired.last_error, None);

        // S1: Switch active space to space B; editing/deleting/importing r-space-a must NOT enter space B outbox!
        upsert_sync_space_tx(
            &conn,
            "space-b",
            "selfhost",
            "http://srv-b",
            "srv-b",
            "acc-b",
            "mac-dev",
            true,
            true,
            None,
            true,
        )
        .unwrap();
        on_local_edit_applied_tx(
            &conn,
            "r-space-a",
            "ed-old-space",
            EditKind::Manual,
            Some("edited in space b"),
            "2026-11-24T13:00:00Z",
            None,
        )
        .unwrap();
        let space_b = active_space_in(&conn).unwrap().unwrap();
        assert_eq!(import_existing_edits_tx(&conn, &space_b).unwrap(), 0);
        on_local_history_deleted_tx(&conn, "r-space-a", true).unwrap();
        let b_ops: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sync_outbox_ops WHERE space_id = 'space-b'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(b_ops, 0);

        // S3: Explicit editor parent snapshot detects remote edit arriving while user is typing!
        let rec_b = sample_record("r-space-b");
        conn.execute(
            "INSERT INTO history_record (id, timestamp, text, mode, duration_ms, source_device_id)
             VALUES (?1, ?2, ?3, ?4, ?5, 'mac-dev')",
            params![rec_b.id, rec_b.timestamp, rec_b.text, rec_b.mode, rec_b.duration_ms],
        )
        .unwrap();
        on_local_history_inserted_tx(&conn, &rec_b, "2026-11-24").unwrap();
        // User opens editor on unedited record (parent snapshot = [])
        // Remote edit from Windows arrives before user hits Save:
        let remote_ed = HistoryOpPayload::Edit(HistoryEditPayload {
            schema: PAYLOAD_SCHEMA_V1,
            edit_id: "ed-win-mid-draft".into(),
            record_id: "r-space-b".into(),
            source_device_id: "win-dev".into(),
            edit_kind: EditKind::Manual,
            parent_edit_ids: vec![],
            hlc: "2026-11-24T13:05:00.000Z#0000#win-dev".into(),
            edited_at: "2026-11-24T13:05:00Z".into(),
            final_text: Some("windows mid-draft text".into()),
            metadata: None,
        });
        apply_remote_history_op_tx(&conn, "space-b", "mac-dev", &remote_ed, 0).unwrap();
        // Now Mac saves with its captured empty parent snapshot -> MUST detect conflict (both recoverable)!
        on_local_edit_applied_with_parents_tx(
            &conn,
            "r-space-b",
            "ed-mac-save",
            EditKind::Manual,
            Some("mac draft text"),
            "2026-11-24T13:06:00Z",
            None,
            Some(vec![]),
        )
        .unwrap();
        let alts = list_edit_conflict_alternatives_in(&conn, "r-space-b").unwrap();
        assert_eq!(alts.len(), 1);

        // S8: Remote delete tombstone queues & deletes real local audio file on disk
        let tmp_dir = tempfile::TempDir::new().unwrap();
        let audio_file = tmp_dir.path().join("rec-b.wav");
        std::fs::write(&audio_file, b"RIFFfake").unwrap();
        conn.execute(
            "UPDATE history_record SET audio_path = ?1 WHERE id = 'r-space-b'",
            params![audio_file.to_str().unwrap()],
        )
        .unwrap();
        let remote_del = HistoryOpPayload::Delete(HistoryDeletePayload {
            schema: PAYLOAD_SCHEMA_V1,
            record_id: "r-space-b".into(),
            source_device_id: "win-dev".into(),
            hlc: "2026-11-24T13:10:00.000Z#0000#win-dev".into(),
            deleted_at: "2026-11-24T13:10:00Z".into(),
        });
        apply_remote_history_op_tx(&conn, "space-b", "mac-dev", &remote_del, 0).unwrap();
        assert!(audio_file.exists()); // Not deleted before post-commit flush
        assert_eq!(flush_pending_audio_deletes_in(&conn).unwrap(), 1);
        assert!(!audio_file.exists());

        // N1: apply_remote_stats_totals_tx uses payload.updated_at (not receiver clock)
        conn.execute(
            "INSERT INTO sync_devices (space_id, device_id, device_name, platform, app_version, last_synced_at, updated_at)
             VALUES ('space-b', 'win-dev', 'Win', 'windows', '0.20.0', '2026-11-20T00:00:00Z', '2026-11-20T00:00:00Z')",
            [],
        )
        .unwrap();
        apply_remote_stats_totals_tx(
            &conn,
            "space-b",
            &StatsTotalsPayload {
                schema: PAYLOAD_SCHEMA_V1,
                device_id: "win-dev".into(),
                revision: 1,
                updated_at: "2026-11-22T05:00:00Z".into(),
                dictation: DictationTotalsPayload::default(),
                tts: TtsTotalsPayload::default(),
            },
        )
        .unwrap();
        let synced_at: String = conn
            .query_row(
                "SELECT last_synced_at FROM sync_devices WHERE space_id = 'space-b' AND device_id = 'win-dev'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(synced_at, "2026-11-22T05:00:00Z");
    }
}
