//! One-time v1 -> v2 migration, encryption, and reconciliation engine
//! (design doc Section 7).
//!
//! Reads read-only snapshots of the v1 sync server SQLite DB and optional
//! per-device v1 client SQLite DBs, produces:
//! 1. A fresh E2EE v2 server SQLite database ready for `voicex-sync-server`.
//! 2. Upgraded v2 client SQLite database copies (preserving all local private
//!    audio paths, failed records, translate-read rows, and local edits).
//! 3. A deterministic reconciliation report + optional full decrypt/replay check.

use chrono::{Local, Utc};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use voicex_sync_protocol::{
    selfhost_space_id, stats_object_id, EditKind, LlmUsageState, SyncDomain,
};

use crate::services::history_service::{HISTORY_ERROR_NONE, HISTORY_MODE_TRANSLATE_READ};
use crate::storage::{StorageError, FEATURE_KIND_DICTATION, UNTRACKED_APP_KEY};
use crate::sync_v2::crypto::{decrypt_envelope, encrypt_envelope, EnvelopeContext, MasterKey};
use crate::sync_v2::hlc::Hlc;
use crate::sync_v2::payloads::{
    DailyAppStatPayload, DailyModelStatPayload, DictationTotalsPayload, EditMetadataPayload,
    HistoryCreatePayload, HistoryDeletePayload, HistoryEditPayload, HistoryOpPayload,
    StatsDailyMonthPayload, StatsShardPayload, StatsTotalsPayload, TtsTotalsPayload,
    PAYLOAD_SCHEMA_V1,
};
use crate::sync_v2::store::{
    apply_remote_history_op_tx, apply_remote_stats_daily_month_tx, apply_remote_stats_totals_tx,
    get_overview_usage_stats_in, init_schema, upsert_sync_space_tx, OverviewScope,
};

#[derive(Debug, Clone)]
pub struct DeviceMigrationInput {
    pub db_path: PathBuf,
    pub out_db_path: PathBuf,
    pub scope_history: bool,
    pub scope_edits: bool,
    pub import_existing_edits: bool,
}

#[derive(Debug, Clone)]
pub struct MigrationConfig {
    pub server_v1_db: PathBuf,
    pub out_server_v2_db: PathBuf,
    pub device_inputs: Vec<DeviceMigrationInput>,
    pub server_url: String,
    pub account_label: String,
    pub recovery_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceReconciliation {
    pub device_id: String,
    pub device_name: String,
    pub platform: String,
    pub migrated_surviving_history: usize,
    pub migrated_all_time_dictations: i64,
    pub migrated_characters: i64,
    pub migrated_duration_ms: i64,
    pub migrated_llm_invoked: i64,
    pub migrated_ai_corrections: i64,
    pub tts_read_count: i64,
    pub tts_translate_count: i64,
    pub tts_characters: i64,
    pub tts_duration_ms: i64,
    pub tts_llm_count: i64,
    pub local_device_recording_count: Option<i64>,
    pub local_vs_migrated_recording_diff: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountReconciliation {
    pub v1_server_total_characters: i64,
    pub v1_server_total_duration_ms: i64,
    pub v1_server_llm_correction_count: i64,
    pub migrated_total_characters: i64,
    pub migrated_total_duration_ms: i64,
    pub migrated_llm_invoked_count: i64,
    pub excluded_translate_read_characters: i64,
    pub excluded_translate_read_duration_ms: i64,
    pub excluded_translate_read_llm_count: i64,
    pub unexplained_characters_diff: i64,
    pub unexplained_duration_ms_diff: i64,
    pub unexplained_llm_diff: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationReport {
    pub server_id: String,
    pub account_id: String,
    pub space_id: String,
    pub key_check_tag: String,
    pub recovery_key: String,
    pub surviving_shared_history_count: usize,
    pub excluded_translate_read_count: usize,
    pub excluded_failed_count: usize,
    pub tombstone_count: usize,
    pub imported_edits_count: usize,
    pub local_only_edits_preserved_count: usize,
    pub stats_shards_published: usize,
    pub total_envelopes_written: usize,
    pub server_latest_seq: i64,
    pub server_used_bytes: i64,
    pub devices: Vec<DeviceReconciliation>,
    pub account_reconciliation: AccountReconciliation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplayVerificationReport {
    pub envelopes_decrypted: usize,
    pub replayed_history_count: usize,
    pub replayed_tombstone_count: usize,
    pub replayed_total_recordings: i64,
    pub replayed_total_characters: i64,
    pub replayed_total_duration_ms: i64,
    pub replayed_llm_invoked: i64,
    pub replayed_tts_reads: i64,
    pub replayed_tts_translates: i64,
    pub deleted_ids_resurrected: usize,
    pub translate_read_leaked: usize,
}

#[derive(Debug, Clone)]
struct V1DeviceMeta {
    device_id: String,
    device_name: String,
    platform: String,
    app_version: String,
    created_at: String,
    last_seen_at: String,
}

#[derive(Debug, Clone)]
struct MergedHistoryRow {
    record_id: String,
    source_device_id: String,
    timestamp: String,
    text: String,
    original_text: Option<String>,
    ai_correction_applied: bool,
    llm_invoked: bool,
    mode: String,
    duration_ms: i64,
    asr_model_name: Option<String>,
    llm_model_name: Option<String>,
    target_app_platform: Option<String>,
    target_app_key: Option<String>,
    target_app_name: Option<String>,
}

#[derive(Debug, Clone)]
struct LocalEditEntry {
    record_id: String,
    source_device_id: String,
    edit_id: String,
    edit_kind: EditKind,
    edited_text: String,
    edited_at: String,
    metadata: Option<EditMetadataPayload>,
}

fn sql_err(err: rusqlite::Error) -> StorageError {
    StorageError::QueryFailed(err.to_string())
}

fn open_ro(path: &Path) -> Result<Connection, StorageError> {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| StorageError::ConnectionFailed(format!("{}: {e}", path.display())))
}

fn local_date_from_rfc3339(ts: &str) -> Option<String> {
    chrono::DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|dt| dt.with_timezone(&Local).format("%Y-%m-%d").to_string())
}

fn epoch_ms_from_rfc3339(ts: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(ts)
        .map(|dt| dt.timestamp_millis().max(0))
        .unwrap_or(0)
}

/// Initialize a fresh v2 server SQLite database matching Section 5 of the spec.
pub fn init_v2_server_schema(conn: &Connection) -> Result<(), StorageError> {
    conn.execute_batch(
        r#"
        PRAGMA journal_mode = WAL;
        PRAGMA busy_timeout = 5000;
        PRAGMA foreign_keys = ON;

        CREATE TABLE IF NOT EXISTS server_info (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            server_id TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS accounts (
            account_id TEXT PRIMARY KEY,
            label TEXT NOT NULL DEFAULT '',
            next_seq INTEGER NOT NULL DEFAULT 0,
            key_check_tag TEXT,
            storage_quota_bytes INTEGER NOT NULL DEFAULT 536870912,
            used_bytes INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL,
            disabled_at TEXT
        );

        CREATE TABLE IF NOT EXISTS pairing_codes (
            code_hash TEXT PRIMARY KEY,
            account_id TEXT NOT NULL REFERENCES accounts(account_id),
            created_at TEXT NOT NULL,
            expires_at TEXT NOT NULL,
            redeemed_at TEXT,
            redeemed_device_id TEXT
        );

        CREATE TABLE IF NOT EXISTS devices (
            account_id TEXT NOT NULL REFERENCES accounts(account_id),
            device_id TEXT NOT NULL,
            device_name TEXT NOT NULL,
            platform TEXT NOT NULL DEFAULT '',
            app_version TEXT NOT NULL DEFAULT '',
            token_hash TEXT NOT NULL UNIQUE,
            allow_stats INTEGER NOT NULL DEFAULT 1,
            allow_history INTEGER NOT NULL DEFAULT 1,
            allow_edits INTEGER NOT NULL DEFAULT 1,
            created_at TEXT NOT NULL,
            last_seen_at TEXT NOT NULL,
            revoked_at TEXT,
            PRIMARY KEY (account_id, device_id)
        );

        CREATE TABLE IF NOT EXISTS envelopes (
            account_id TEXT NOT NULL REFERENCES accounts(account_id),
            seq INTEGER NOT NULL,
            op_id TEXT NOT NULL,
            domain TEXT NOT NULL,
            device_id TEXT NOT NULL,
            object_id TEXT NOT NULL,
            revision INTEGER NOT NULL DEFAULT 0,
            nonce TEXT NOT NULL,
            ciphertext TEXT NOT NULL,
            byte_size INTEGER NOT NULL,
            superseded INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL,
            PRIMARY KEY (account_id, seq),
            UNIQUE (account_id, op_id)
        );
        CREATE INDEX IF NOT EXISTS envelopes_pull_idx
            ON envelopes(account_id, domain, superseded, seq);

        CREATE TABLE IF NOT EXISTS stats_shards (
            account_id TEXT NOT NULL REFERENCES accounts(account_id),
            device_id TEXT NOT NULL,
            shard_key TEXT NOT NULL,
            revision INTEGER NOT NULL,
            op_id TEXT NOT NULL,
            seq INTEGER NOT NULL,
            nonce TEXT NOT NULL,
            ciphertext TEXT NOT NULL,
            byte_size INTEGER NOT NULL,
            updated_at TEXT NOT NULL,
            PRIMARY KEY (account_id, device_id, shard_key)
        );
        "#,
    )
    .map_err(sql_err)
}

/// Run the deterministic v1 -> v2 migration on read-only backups and produce
/// fresh v2 server DB + upgraded device DB copies + reconciliation report.
pub fn run_migration(cfg: &MigrationConfig) -> Result<MigrationReport, StorageError> {
    let srv_ro = open_ro(&cfg.server_v1_db)?;

    let server_id: String = srv_ro
        .query_row("SELECT server_id FROM server_info WHERE id = 1", [], |r| {
            r.get(0)
        })
        .map_err(sql_err)?;
    let (account_id, account_created_at): (String, String) = srv_ro
        .query_row(
            "SELECT account_id, created_at FROM accounts LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(sql_err)?;
    let space_id = selfhost_space_id(&server_id, &account_id);

    let master_key = match cfg.recovery_key.as_deref() {
        Some(rk) => MasterKey::from_recovery_key(rk)
            .map_err(|e| StorageError::InvalidInput(e.to_string()))?,
        None => MasterKey::generate(),
    };
    let recovery_key = master_key.to_recovery_key();
    let key_check_tag = master_key
        .derive_key_check_tag(&account_id, &space_id)
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;

    // 1. Load v1 server devices
    let mut devices_by_id: BTreeMap<String, V1DeviceMeta> = BTreeMap::new();
    {
        let mut stmt = srv_ro
            .prepare(
                "SELECT device_id, device_name, COALESCE(platform, ''), COALESCE(app_version, ''),
                        created_at, last_seen_at
                 FROM devices WHERE account_id = ?1",
            )
            .map_err(sql_err)?;
        let rows = stmt
            .query_map(params![account_id], |r| {
                Ok(V1DeviceMeta {
                    device_id: r.get(0)?,
                    device_name: r.get(1)?,
                    platform: r.get(2)?,
                    app_version: r.get(3)?,
                    created_at: r.get(4)?,
                    last_seen_at: r.get(5)?,
                })
            })
            .map_err(sql_err)?;
        for row in rows {
            let d = row.map_err(sql_err)?;
            devices_by_id.insert(d.device_id.clone(), d);
        }
    }

    // 2. Load v1 server account usage_stats & per-device TTS stats
    let (v1_srv_dur, v1_srv_chars, v1_srv_llm): (i64, i64, i64) = srv_ro
        .query_row(
            "SELECT total_duration_ms, total_characters, llm_correction_count
             FROM usage_stats WHERE account_id = ?1",
            params![account_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(sql_err)?
        .unwrap_or((0, 0, 0));

    let mut tts_by_device: BTreeMap<String, TtsTotalsPayload> = BTreeMap::new();
    {
        let mut stmt = srv_ro
            .prepare(
                "SELECT device_id, read_count, translate_count, characters, duration_ms, llm_count
                 FROM device_tts_stats WHERE account_id = ?1",
            )
            .map_err(sql_err)?;
        let rows = stmt
            .query_map(params![account_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    TtsTotalsPayload {
                        read_count: r.get(1)?,
                        translate_count: r.get(2)?,
                        characters: r.get(3)?,
                        duration_ms: r.get(4)?,
                        llm_count: r.get(5)?,
                    },
                ))
            })
            .map_err(sql_err)?;
        for row in rows {
            let (dev_id, tts) = row.map_err(sql_err)?;
            tts_by_device.insert(dev_id, tts);
        }
    }

    // 3. Parse v1 server events in seq order: deduplicate upserts + collect deletes
    let mut all_time_upserts: BTreeMap<String, MergedHistoryRow> = BTreeMap::new();
    let mut deleted_ids: BTreeMap<String, (String, String)> = BTreeMap::new();
    let mut excluded_translate_read_ids: BTreeSet<String> = BTreeSet::new();
    let mut excluded_translate_read_chars: i64 = 0;
    let mut excluded_translate_read_dur: i64 = 0;
    let mut excluded_translate_read_llm: i64 = 0;
    let mut excluded_failed_count: usize = 0;

    {
        let mut stmt = srv_ro
            .prepare(
                "SELECT type, device_id, record_id, payload, created_at
                 FROM events
                 WHERE account_id = ?1 AND type IN ('history.upsert', 'history.delete')
                 ORDER BY seq ASC",
            )
            .map_err(sql_err)?;
        let rows = stmt
            .query_map(params![account_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })
            .map_err(sql_err)?;
        for row in rows {
            let (ev_type, dev_id, rid_opt, payload_str, created_at) = row.map_err(sql_err)?;
            if ev_type == "history.delete" {
                let rid = rid_opt.or_else(|| {
                    serde_json::from_str::<Value>(&payload_str)
                        .ok()
                        .and_then(|v| v.get("recordId").and_then(Value::as_str).map(String::from))
                });
                if let Some(rid) = rid {
                    deleted_ids.insert(rid, (dev_id, created_at));
                }
                continue;
            }

            let Ok(v) = serde_json::from_str::<Value>(&payload_str) else {
                continue;
            };
            let rec = v.get("record").unwrap_or(&v);
            let record_id = rid_opt
                .or_else(|| rec.get("id").and_then(Value::as_str).map(String::from))
                .unwrap_or_default();
            if record_id.is_empty() {
                continue;
            }
            let mode = rec
                .get("mode")
                .and_then(Value::as_str)
                .unwrap_or("assistant_hands_free")
                .to_string();
            let error_code = rec.get("errorCode").and_then(Value::as_i64).unwrap_or(0) as i32;
            let is_final = rec.get("isFinal").and_then(Value::as_bool).unwrap_or(true);
            let text = rec
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let duration_ms = rec.get("durationMs").and_then(Value::as_i64).unwrap_or(0);
            let ai_corr = rec
                .get("aiCorrectionApplied")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let llm_invoked = rec
                .get("llmInvoked")
                .and_then(Value::as_bool)
                .unwrap_or(ai_corr);

            if mode == HISTORY_MODE_TRANSLATE_READ {
                if excluded_translate_read_ids.insert(record_id) {
                    excluded_translate_read_chars += text.chars().count() as i64;
                    excluded_translate_read_dur += duration_ms.max(0);
                    excluded_translate_read_llm += i64::from(llm_invoked);
                }
                continue;
            }
            if error_code != HISTORY_ERROR_NONE || !is_final {
                excluded_failed_count += 1;
                continue;
            }

            let source_device_id = rec
                .get("sourceDeviceId")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .unwrap_or(&dev_id)
                .to_string();
            let timestamp = rec
                .get("timestamp")
                .and_then(Value::as_str)
                .unwrap_or(&created_at)
                .to_string();

            all_time_upserts.insert(
                record_id.clone(),
                MergedHistoryRow {
                    record_id,
                    source_device_id,
                    timestamp,
                    text,
                    original_text: rec
                        .get("originalText")
                        .and_then(Value::as_str)
                        .map(String::from),
                    ai_correction_applied: ai_corr,
                    llm_invoked,
                    mode,
                    duration_ms: duration_ms.max(0),
                    asr_model_name: rec
                        .get("asrModelName")
                        .and_then(Value::as_str)
                        .filter(|s| !s.trim().is_empty())
                        .map(String::from),
                    llm_model_name: rec
                        .get("llmModelName")
                        .and_then(Value::as_str)
                        .filter(|s| !s.trim().is_empty())
                        .map(String::from),
                    target_app_platform: None,
                    target_app_key: None,
                    target_app_name: None,
                },
            );
        }
    }

    // 4. Merge each supplied device DB (read-only): unsynced creates, pending
    //    deletes, target apps, local edits, and local device totals comparison.
    let mut local_device_recording_counts: HashMap<String, i64> = HashMap::new();
    let mut imported_edits: Vec<LocalEditEntry> = Vec::new();
    let mut local_only_edits_preserved_count: usize = 0;
    let mut device_id_by_input_idx: Vec<String> = Vec::with_capacity(cfg.device_inputs.len());

    for input in &cfg.device_inputs {
        let dev_ro = open_ro(&input.db_path)?;
        let dev_id: String = dev_ro
            .query_row(
                "SELECT value FROM user_config WHERE key = 'sync_device_id' LIMIT 1",
                [],
                |r| r.get(0),
            )
            .map_err(sql_err)?;
        device_id_by_input_idx.push(dev_id.clone());

        // Check local device_usage_stats for reconciliation & TTS comparison
        if let Some((rec_cnt, tts)) = dev_ro
            .query_row(
                "SELECT total_recording_count, tts_read_count, tts_translate_count,
                        tts_characters, tts_duration_ms, tts_llm_count
                 FROM device_usage_stats WHERE device_id = ?1 LIMIT 1",
                params![dev_id],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        TtsTotalsPayload {
                            read_count: r.get(1)?,
                            translate_count: r.get(2)?,
                            characters: r.get(3)?,
                            duration_ms: r.get(4)?,
                            llm_count: r.get(5)?,
                        },
                    ))
                },
            )
            .optional()
            .map_err(sql_err)?
        {
            local_device_recording_counts.insert(dev_id.clone(), rec_cnt);
            let entry = tts_by_device.entry(dev_id.clone()).or_default();
            entry.read_count = entry.read_count.max(tts.read_count);
            entry.translate_count = entry.translate_count.max(tts.translate_count);
            entry.characters = entry.characters.max(tts.characters);
            entry.duration_ms = entry.duration_ms.max(tts.duration_ms);
            entry.llm_count = entry.llm_count.max(tts.llm_count);
        }

        // Check pending local deletes in v1 sync_outbox
        {
            let mut stmt = dev_ro
                .prepare(
                    "SELECT record_id, created_at FROM sync_outbox
                     WHERE event_type = 'history.delete' AND record_id IS NOT NULL",
                )
                .map_err(sql_err)?;
            let rows = stmt
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .map_err(sql_err)?;
            for row in rows {
                let (rid, ts) = row.map_err(sql_err)?;
                deleted_ids.entry(rid).or_insert((dev_id.clone(), ts));
            }
        }

        // Enrich target_app metadata + pick up any unsynced local eligible dictations + edits
        {
            let mut stmt = dev_ro
                .prepare(
                    "SELECT id, timestamp, text, original_text, ai_correction_applied, llm_invoked,
                            mode, duration_ms, is_final, error_code, source_device_id,
                            asr_model_name, llm_model_name,
                            target_app_platform, target_app_key, target_app_name,
                            edited_text, edit_source, edited_at
                     FROM history_record",
                )
                .map_err(sql_err)?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, i32>(4)? != 0,
                        r.get::<_, i32>(5)? != 0,
                        r.get::<_, String>(6)?,
                        r.get::<_, i64>(7)?,
                        r.get::<_, i32>(8)? != 0,
                        r.get::<_, i32>(9)?,
                        r.get::<_, Option<String>>(10)?,
                        r.get::<_, Option<String>>(11)?,
                        r.get::<_, Option<String>>(12)?,
                        r.get::<_, Option<String>>(13)?,
                        r.get::<_, Option<String>>(14)?,
                        r.get::<_, Option<String>>(15)?,
                        r.get::<_, Option<String>>(16)?,
                        r.get::<_, Option<String>>(17)?,
                        r.get::<_, Option<String>>(18)?,
                    ))
                })
                .map_err(sql_err)?;

            for row in rows {
                let (
                    id,
                    timestamp,
                    text,
                    original_text,
                    ai_corr,
                    llm_invoked,
                    mode,
                    duration_ms,
                    is_final,
                    error_code,
                    src_dev,
                    asr_model,
                    llm_model,
                    app_plat,
                    app_key,
                    app_name,
                    edited_text,
                    edit_source,
                    edited_at,
                ) = row.map_err(sql_err)?;

                if error_code != HISTORY_ERROR_NONE || !is_final || mode == HISTORY_MODE_TRANSLATE_READ {
                    continue;
                }
                let effective_dev = src_dev
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| dev_id.clone());

                if let Some(existing) = all_time_upserts.get_mut(&id) {
                    if existing.target_app_key.is_none() && app_key.as_deref().is_some_and(|k| !k.is_empty()) {
                        existing.target_app_platform = app_plat;
                        existing.target_app_key = app_key;
                        existing.target_app_name = app_name;
                    }
                    if existing.asr_model_name.is_none() {
                        existing.asr_model_name = asr_model;
                    }
                    if existing.llm_model_name.is_none() {
                        existing.llm_model_name = llm_model;
                    }
                } else if !deleted_ids.contains_key(&id) {
                    all_time_upserts.insert(
                        id.clone(),
                        MergedHistoryRow {
                            record_id: id.clone(),
                            source_device_id: effective_dev.clone(),
                            timestamp: timestamp.clone(),
                            text,
                            original_text,
                            ai_correction_applied: ai_corr,
                            llm_invoked,
                            mode,
                            duration_ms: duration_ms.max(0),
                            asr_model_name: asr_model,
                            llm_model_name: llm_model,
                            target_app_platform: app_plat,
                            target_app_key: app_key,
                            target_app_name: app_name,
                        },
                    );
                }

                if let Some(edited) = edited_text.filter(|t| !t.is_empty()) {
                    if deleted_ids.contains_key(&id) {
                        continue;
                    }
                    if input.scope_history && input.scope_edits && input.import_existing_edits {
                        let edit_kind = match edit_source.as_deref() {
                            Some("manual") => EditKind::Manual,
                            _ => EditKind::Observed,
                        };
                        let post_edit_meta: Option<(String, Option<EditMetadataPayload>)> = dev_ro
                            .query_row(
                                "SELECT id, payload FROM post_edit_record WHERE source_id = ?1 LIMIT 1",
                                params![id],
                                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
                            )
                            .optional()
                            .map_err(sql_err)?
                            .map(|(pe_id, payload)| {
                                let meta = serde_json::from_str::<Value>(&payload).ok().map(|v| {
                                    EditMetadataPayload {
                                        complete: v
                                            .get("complete")
                                            .and_then(Value::as_bool)
                                            .unwrap_or(false),
                                        end_reason: v
                                            .get("endReason")
                                            .and_then(Value::as_str)
                                            .map(String::from),
                                        insertions: v
                                            .get("insertions")
                                            .and_then(Value::as_u64)
                                            .map(|n| n as usize),
                                        deletions: v
                                            .get("deletions")
                                            .and_then(Value::as_u64)
                                            .map(|n| n as usize),
                                        substitutions: v
                                            .get("substitutions")
                                            .and_then(Value::as_u64)
                                            .map(|n| n as usize),
                                    }
                                });
                                (pe_id, meta)
                            });

                        let (edit_id, metadata) = match post_edit_meta {
                            Some((pe_id, meta)) => (pe_id, meta),
                            None => (format!("migrated-edit:{dev_id}:{id}"), None),
                        };
                        imported_edits.push(LocalEditEntry {
                            record_id: id,
                            source_device_id: dev_id.clone(),
                            edit_id,
                            edit_kind,
                            edited_text: edited,
                            edited_at: edited_at.unwrap_or(timestamp),
                            metadata,
                        });
                    } else {
                        local_only_edits_preserved_count += 1;
                    }
                }
            }
        }
    }

    // 5. Partition surviving shared records vs all-time lifetime stats per device
    let mut surviving_records: Vec<&MergedHistoryRow> = all_time_upserts
        .values()
        .filter(|r| !deleted_ids.contains_key(&r.record_id))
        .collect();
    surviving_records.sort_by(|a, b| {
        a.timestamp
            .cmp(&b.timestamp)
            .then_with(|| a.record_id.cmp(&b.record_id))
    });

    // Aggregate per-device lifetime totals + per-month daily app & model rows from ALL-TIME valid dictations
    let mut totals_by_dev: BTreeMap<String, DictationTotalsPayload> = BTreeMap::new();
    let mut surviving_count_by_dev: HashMap<String, usize> = HashMap::new();
    for rec in &surviving_records {
        *surviving_count_by_dev
            .entry(rec.source_device_id.clone())
            .or_insert(0) += 1;
    }

    // (device_id, month) -> ((local_date, platform, app_key) -> DailyAppStatPayload)
    let mut daily_apps: BTreeMap<(String, String), BTreeMap<(String, String, String), DailyAppStatPayload>> =
        BTreeMap::new();
    // (device_id, month) -> ((local_date, asr, llm_state, llm_model) -> DailyModelStatPayload)
    let mut daily_models: BTreeMap<
        (String, String),
        BTreeMap<(String, String, LlmUsageState, String), DailyModelStatPayload>,
    > = BTreeMap::new();

    for rec in all_time_upserts.values() {
        let chars = rec.text.chars().count() as i64;
        let tot = totals_by_dev
            .entry(rec.source_device_id.clone())
            .or_default();
        tot.recording_count += 1;
        tot.total_characters += chars;
        tot.total_duration_ms += rec.duration_ms;
        tot.llm_invoked_count += i64::from(rec.llm_invoked);
        tot.ai_correction_count += i64::from(rec.ai_correction_applied);

        if let Some(local_date) = local_date_from_rfc3339(&rec.timestamp) {
            let month = local_date[..7].to_string();
            let app_key = rec
                .target_app_key
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(UNTRACKED_APP_KEY)
                .to_string();
            let platform = if app_key == UNTRACKED_APP_KEY {
                String::new()
            } else {
                rec.target_app_platform
                    .as_deref()
                    .map(str::trim)
                    .unwrap_or("")
                    .to_string()
            };
            let app_name = if app_key == UNTRACKED_APP_KEY {
                String::new()
            } else {
                rec.target_app_name
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .unwrap_or(&app_key)
                    .to_string()
            };

            let app_entry = daily_apps
                .entry((rec.source_device_id.clone(), month.clone()))
                .or_default()
                .entry((local_date.clone(), platform.clone(), app_key.clone()))
                .or_insert_with(|| DailyAppStatPayload {
                    local_date: local_date.clone(),
                    feature_kind: FEATURE_KIND_DICTATION.to_string(),
                    platform,
                    app_key,
                    app_name: app_name.clone(),
                    usage_count: 0,
                    total_characters: 0,
                    total_duration_ms: 0,
                    llm_count: 0,
                    last_used_at: rec.timestamp.clone(),
                });
            app_entry.usage_count += 1;
            app_entry.total_characters += chars;
            app_entry.total_duration_ms += rec.duration_ms;
            app_entry.llm_count += i64::from(rec.llm_invoked);
            if rec.timestamp >= app_entry.last_used_at {
                app_entry.last_used_at = rec.timestamp.clone();
                if !app_name.is_empty() {
                    app_entry.app_name = app_name;
                }
            }

            let asr = rec
                .asr_model_name
                .as_deref()
                .map(str::trim)
                .unwrap_or("")
                .to_string();
            let (llm_state, llm_model) =
                LlmUsageState::from_record_fields(rec.llm_invoked, rec.llm_model_name.as_deref());
            let model_entry = daily_models
                .entry((rec.source_device_id.clone(), month))
                .or_default()
                .entry((local_date.clone(), asr.clone(), llm_state, llm_model.clone()))
                .or_insert_with(|| DailyModelStatPayload {
                    local_date,
                    asr_model_name: asr,
                    llm_state,
                    llm_model_name: llm_model,
                    usage_count: 0,
                    total_characters: 0,
                    total_duration_ms: 0,
                    ai_correction_count: 0,
                });
            model_entry.usage_count += 1;
            model_entry.total_characters += chars;
            model_entry.total_duration_ms += rec.duration_ms;
            model_entry.ai_correction_count += i64::from(rec.ai_correction_applied);
        }
    }

    // 6. Write fresh v2 server SQLite database
    if cfg.out_server_v2_db.exists() {
        std::fs::remove_file(&cfg.out_server_v2_db)
            .map_err(|e| StorageError::FileOpFailed(e.to_string()))?;
    }
    if let Some(parent) = cfg.out_server_v2_db.parent() {
        std::fs::create_dir_all(parent).map_err(|e| StorageError::FileOpFailed(e.to_string()))?;
    }
    let srv_v2 = Connection::open(&cfg.out_server_v2_db)
        .map_err(|e| StorageError::ConnectionFailed(e.to_string()))?;
    init_v2_server_schema(&srv_v2)?;

    let now = Utc::now().to_rfc3339();
    let tx = srv_v2.unchecked_transaction().map_err(sql_err)?;
    tx.execute(
        "INSERT INTO server_info (id, server_id) VALUES (1, ?1)",
        params![server_id],
    )
    .map_err(sql_err)?;
    tx.execute(
        "INSERT INTO accounts (account_id, label, next_seq, key_check_tag, used_bytes, created_at)
         VALUES (?1, ?2, 0, ?3, 0, ?4)",
        params![account_id, cfg.account_label, key_check_tag, account_created_at],
    )
    .map_err(sql_err)?;

    // Ensure every device appearing in totals or TTS exists in `devices`
    let mut all_device_ids: BTreeSet<String> = devices_by_id.keys().cloned().collect();
    all_device_ids.extend(totals_by_dev.keys().cloned());
    all_device_ids.extend(tts_by_device.keys().cloned());

    for dev_id in &all_device_ids {
        let meta = devices_by_id.get(dev_id);
        let name = meta
            .map(|m| m.device_name.as_str())
            .unwrap_or(dev_id.as_str());
        let platform = meta.map(|m| m.platform.as_str()).unwrap_or("");
        let app_ver = meta.map(|m| m.app_version.as_str()).unwrap_or("");
        let created = meta.map(|m| m.created_at.as_str()).unwrap_or(&now);
        let last_seen = meta.map(|m| m.last_seen_at.as_str()).unwrap_or(&now);
        // Migrated devices get a placeholder revoked token hash until re-paired via admin CLI.
        let placeholder_token_hash = format!("migrated-unpaired:{dev_id}");
        tx.execute(
            "INSERT INTO devices (
                account_id, device_id, device_name, platform, app_version,
                token_hash, allow_stats, allow_history, allow_edits,
                created_at, last_seen_at, revoked_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, 1, 1, ?7, ?8, NULL)",
            params![
                account_id,
                dev_id,
                name,
                platform,
                app_ver,
                placeholder_token_hash,
                created,
                last_seen
            ],
        )
        .map_err(sql_err)?;
    }

    let mut seq: i64 = 0;
    let mut used_bytes: i64 = 0;
    let mut stats_shards_published: usize = 0;

    // Helper closure to encrypt & insert one envelope into v2 server DB.
    let mut write_envelope = |domain: SyncDomain,
                              device_id: &str,
                              object_id: &str,
                              revision: i64,
                              op_id: &str,
                              plaintext: &[u8],
                              created_at: &str,
                              shard_key_opt: Option<&str>|
     -> Result<i64, StorageError> {
        let sealed = encrypt_envelope(
            &master_key,
            EnvelopeContext {
                account_id: &account_id,
                space_id: &space_id,
                domain,
                object_id,
                revision,
                op_id,
            },
            plaintext,
        )
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;

        seq += 1;
        let byte_size = (sealed.nonce_b64.len() + sealed.ciphertext_b64.len()) as i64;
        used_bytes += byte_size;

        tx.execute(
            "INSERT INTO envelopes (
                account_id, seq, op_id, domain, device_id, object_id, revision,
                nonce, ciphertext, byte_size, superseded, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 0, ?11)",
            params![
                account_id,
                seq,
                op_id,
                domain.as_str(),
                device_id,
                object_id,
                revision,
                sealed.nonce_b64,
                sealed.ciphertext_b64,
                byte_size,
                created_at
            ],
        )
        .map_err(sql_err)?;

        if let Some(shard_key) = shard_key_opt {
            tx.execute(
                "INSERT INTO stats_shards (
                    account_id, device_id, shard_key, revision, op_id, seq,
                    nonce, ciphertext, byte_size, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    account_id,
                    device_id,
                    shard_key,
                    revision,
                    op_id,
                    seq,
                    sealed.nonce_b64,
                    sealed.ciphertext_b64,
                    byte_size,
                    created_at
                ],
            )
            .map_err(sql_err)?;
        }
        Ok(seq)
    };

    // 6a. Publish per-device stats totals & daily:YYYY-MM shards (revision = 1)
    let mut totals_payloads: Vec<StatsTotalsPayload> = Vec::new();
    let mut daily_payloads: Vec<StatsDailyMonthPayload> = Vec::new();

    let mut stats_end_seq: i64 = 0;
    for dev_id in &all_device_ids {
        let dictation = totals_by_dev.get(dev_id).cloned().unwrap_or_default();
        let tts = tts_by_device.get(dev_id).cloned().unwrap_or_default();
        let totals_payload = StatsTotalsPayload {
            schema: PAYLOAD_SCHEMA_V1,
            device_id: dev_id.clone(),
            revision: 1,
            updated_at: now.clone(),
            dictation,
            tts,
        };
        let bytes = serde_json::to_vec(&StatsShardPayload::Totals(totals_payload.clone()))
            .map_err(|e| StorageError::SerializeFailed(e.to_string()))?;
        let obj_id = stats_object_id(dev_id, "totals");
        let op_id = format!("stats:{dev_id}:totals:1");
        stats_end_seq = write_envelope(
            SyncDomain::Stats,
            dev_id,
            &obj_id,
            1,
            &op_id,
            &bytes,
            &now,
            Some("totals"),
        )?;
        stats_shards_published += 1;
        totals_payloads.push(totals_payload);

        let mut months: BTreeSet<String> = BTreeSet::new();
        for (d, m) in daily_apps.keys() {
            if d == dev_id {
                months.insert(m.clone());
            }
        }
        for (d, m) in daily_models.keys() {
            if d == dev_id {
                months.insert(m.clone());
            }
        }
        for month in months {
            let shard_key = format!("daily:{month}");
            let app_rows = daily_apps
                .get(&(dev_id.clone(), month.clone()))
                .map(|m| m.values().cloned().collect())
                .unwrap_or_default();
            let model_rows = daily_models
                .get(&(dev_id.clone(), month.clone()))
                .map(|m| m.values().cloned().collect())
                .unwrap_or_default();
            let daily_payload = StatsDailyMonthPayload {
                schema: PAYLOAD_SCHEMA_V1,
                device_id: dev_id.clone(),
                month: month.clone(),
                revision: 1,
                updated_at: now.clone(),
                app_rows,
                model_rows,
            };
            let bytes =
                serde_json::to_vec(&StatsShardPayload::DailyMonth(daily_payload.clone()))
                    .map_err(|e| StorageError::SerializeFailed(e.to_string()))?;
            let obj_id = stats_object_id(dev_id, &shard_key);
            let op_id = format!("stats:{dev_id}:{shard_key}:1");
            stats_end_seq = write_envelope(
                SyncDomain::Stats,
                dev_id,
                &obj_id,
                1,
                &op_id,
                &bytes,
                &now,
                Some(&shard_key),
            )?;
            stats_shards_published += 1;
            daily_payloads.push(daily_payload);
        }
    }

    // 6b. Publish surviving shared history.create envelopes
    let mut history_end_seq = stats_end_seq;
    for rec in &surviving_records {
        let op = HistoryOpPayload::Create(HistoryCreatePayload {
            schema: PAYLOAD_SCHEMA_V1,
            record_id: rec.record_id.clone(),
            source_device_id: rec.source_device_id.clone(),
            timestamp: rec.timestamp.clone(),
            text: rec.text.clone(),
            original_text: rec.original_text.clone(),
            ai_correction_applied: rec.ai_correction_applied,
            llm_invoked: rec.llm_invoked,
            mode: rec.mode.clone(),
            duration_ms: rec.duration_ms,
            asr_model_name: rec.asr_model_name.clone(),
            llm_model_name: rec.llm_model_name.clone(),
            target_app_platform: rec.target_app_platform.clone(),
            target_app_key: rec.target_app_key.clone(),
            target_app_name: rec.target_app_name.clone(),
        });
        let bytes =
            serde_json::to_vec(&op).map_err(|e| StorageError::SerializeFailed(e.to_string()))?;
        let op_id = format!("create:{}", rec.record_id);
        history_end_seq = write_envelope(
            SyncDomain::History,
            &rec.source_device_id,
            &rec.record_id,
            0,
            &op_id,
            &bytes,
            &rec.timestamp,
            None,
        )?;
    }

    // 6c. Publish history.delete tombstones (zero text!)
    let mut del_counter: u32 = 0;
    for (rid, (dev_id, deleted_at)) in &deleted_ids {
        let hlc = Hlc::new(epoch_ms_from_rfc3339(deleted_at), del_counter, dev_id)
            .map_err(|e| StorageError::QueryFailed(e.to_string()))?
            .to_wire_string();
        del_counter += 1;
        let op = HistoryOpPayload::Delete(HistoryDeletePayload {
            schema: PAYLOAD_SCHEMA_V1,
            record_id: rid.clone(),
            source_device_id: dev_id.clone(),
            hlc,
            deleted_at: deleted_at.clone(),
        });
        let bytes =
            serde_json::to_vec(&op).map_err(|e| StorageError::SerializeFailed(e.to_string()))?;
        let op_id = format!("delete:{rid}");
        history_end_seq = write_envelope(
            SyncDomain::History,
            dev_id,
            rid,
            0,
            &op_id,
            &bytes,
            deleted_at,
            None,
        )?;
    }

    // 6d. Publish authorized imported edits (only when explicitly enabled for a device)
    let mut edits_end_seq = history_end_seq;
    let mut edit_counter: u32 = 0;
    for edit in &imported_edits {
        let hlc = Hlc::new(
            epoch_ms_from_rfc3339(&edit.edited_at),
            edit_counter,
            &edit.source_device_id,
        )
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?
        .to_wire_string();
        edit_counter += 1;
        let op = HistoryOpPayload::Edit(HistoryEditPayload {
            schema: PAYLOAD_SCHEMA_V1,
            edit_id: edit.edit_id.clone(),
            record_id: edit.record_id.clone(),
            source_device_id: edit.source_device_id.clone(),
            edit_kind: edit.edit_kind,
            parent_edit_ids: Vec::new(),
            hlc,
            edited_at: edit.edited_at.clone(),
            final_text: Some(edit.edited_text.clone()),
            metadata: edit.metadata.clone(),
        });
        let bytes =
            serde_json::to_vec(&op).map_err(|e| StorageError::SerializeFailed(e.to_string()))?;
        let op_id = format!("edit:{}", edit.edit_id);
        edits_end_seq = write_envelope(
            SyncDomain::Edits,
            &edit.source_device_id,
            &edit.record_id,
            0,
            &op_id,
            &bytes,
            &edit.edited_at,
            None,
        )?;
    }
    drop(write_envelope);

    tx.execute(
        "UPDATE accounts SET next_seq = ?2, used_bytes = ?3 WHERE account_id = ?1",
        params![account_id, seq, used_bytes],
    )
    .map_err(sql_err)?;
    tx.commit().map_err(sql_err)?;

    // 7. Upgrade each device's output DB copy via Online Backup + in-place v2 schema & state
    for (idx, input) in cfg.device_inputs.iter().enumerate() {
        let dev_id = &device_id_by_input_idx[idx];
        if input.out_db_path.exists() {
            std::fs::remove_file(&input.out_db_path)
                .map_err(|e| StorageError::FileOpFailed(e.to_string()))?;
        }
        if let Some(parent) = input.out_db_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| StorageError::FileOpFailed(e.to_string()))?;
        }
        {
            let src = open_ro(&input.db_path)?;
            let mut dst = Connection::open(&input.out_db_path)
                .map_err(|e| StorageError::ConnectionFailed(e.to_string()))?;
            let backup = rusqlite::backup::Backup::new(&src, &mut dst).map_err(sql_err)?;
            backup.run_to_completion(200, std::time::Duration::from_millis(5), None).map_err(sql_err)?;
        }

        let dev_v2 = Connection::open(&input.out_db_path)
            .map_err(|e| StorageError::ConnectionFailed(e.to_string()))?;
        init_schema(&dev_v2)?;

        let dtx = dev_v2.unchecked_transaction().map_err(sql_err)?;
        upsert_sync_space_tx(
            &dtx,
            &space_id,
            "selfhost",
            &cfg.server_url,
            &server_id,
            &account_id,
            dev_id,
            input.scope_history,
            input.scope_edits,
            Some(&key_check_tag),
            true,
        )?;

        // Seed sync_devices metadata
        for d_id in &all_device_ids {
            let meta = devices_by_id.get(d_id);
            dtx.execute(
                "INSERT INTO sync_devices (space_id, device_id, device_name, platform, app_version, last_synced_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(space_id, device_id) DO UPDATE SET
                    device_name = excluded.device_name,
                    platform = excluded.platform,
                    app_version = excluded.app_version,
                    last_synced_at = excluded.last_synced_at,
                    updated_at = excluded.updated_at",
                params![
                    space_id,
                    d_id,
                    meta.map(|m| m.device_name.as_str()).unwrap_or(d_id.as_str()),
                    meta.map(|m| m.platform.as_str()).unwrap_or(""),
                    meta.map(|m| m.app_version.as_str()).unwrap_or(""),
                    meta.map(|m| m.last_seen_at.as_str()),
                    now,
                ],
            )
            .map_err(sql_err)?;
        }

        // Apply all per-device stats shards so Overview All/Local matches server immediately
        for tp in &totals_payloads {
            apply_remote_stats_totals_tx(&dtx, &space_id, tp)?;
            if tp.device_id == *dev_id {
                dtx.execute(
                    "UPDATE sync_stats_shards SET local_revision = 1, acked_revision = 1
                     WHERE space_id = ?1 AND device_id = ?2 AND shard_key = 'totals'",
                    params![space_id, dev_id],
                )
                .map_err(sql_err)?;
            }
        }
        for dp in &daily_payloads {
            apply_remote_stats_daily_month_tx(&dtx, &space_id, dp)?;
            if dp.device_id == *dev_id {
                let sk = format!("daily:{}", dp.month);
                dtx.execute(
                    "UPDATE sync_stats_shards SET local_revision = 1, acked_revision = 1
                     WHERE space_id = ?1 AND device_id = ?2 AND shard_key = ?3",
                    params![space_id, dev_id, sk],
                )
                .map_err(sql_err)?;
            }
        }

        // Persist tombstones & mark already-shared local history rows
        for (rid, (del_dev, del_at)) in &deleted_ids {
            let hlc = Hlc::new(epoch_ms_from_rfc3339(del_at), 0, del_dev)
                .map_err(|e| StorageError::QueryFailed(e.to_string()))?
                .to_wire_string();
            dtx.execute(
                "INSERT OR IGNORE INTO history_tombstones (space_id, record_id, deleted_by_device_id, hlc, deleted_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![space_id, rid, del_dev, hlc, del_at],
            )
            .map_err(sql_err)?;
            dtx.execute("DELETE FROM history_record WHERE id = ?1", params![rid])
                .map_err(sql_err)?;
        }
        for rec in &surviving_records {
            dtx.execute(
                "UPDATE history_record SET sync_eligible = 1, sync_space_id = ?2 WHERE id = ?1",
                params![rec.record_id, space_id],
            )
            .map_err(sql_err)?;
        }

        let hist_cursor = if input.scope_history { history_end_seq } else { 0 };
        let edit_cursor = if input.scope_history && input.scope_edits {
            edits_end_seq
        } else {
            0
        };
        dtx.execute(
            "UPDATE sync_spaces
             SET stats_cursor = ?2, history_cursor = ?3, edits_cursor = ?4, last_sync_at = ?5, updated_at = ?5
             WHERE space_id = ?1",
            params![space_id, stats_end_seq, hist_cursor, edit_cursor, now],
        )
        .map_err(sql_err)?;
        // Disable legacy v1 sync flag in app_settings on the upgraded DB copy so
        // the client runs exclusively on Sync v2 after cutover.
        if let Some(raw_settings) = dtx
            .query_row(
                "SELECT value FROM user_config WHERE key = 'app_settings' LIMIT 1",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(sql_err)?
        {
            if let Ok(mut val) = serde_json::from_str::<Value>(&raw_settings) {
                if let Some(obj) = val.as_object_mut() {
                    obj.insert("syncEnabled".to_string(), Value::Bool(false));
                    if let Ok(updated) = serde_json::to_string(&val) {
                        dtx.execute(
                            "UPDATE user_config SET value = ?1 WHERE key = 'app_settings'",
                            params![updated],
                        )
                        .map_err(sql_err)?;
                    }
                }
            }
        }
        dtx.commit().map_err(sql_err)?;
    }

    // 8. Build reconciliation report
    let mut device_reports = Vec::new();
    let mut mig_tot_chars = 0i64;
    let mut mig_tot_dur = 0i64;
    let mut mig_tot_llm = 0i64;

    for dev_id in &all_device_ids {
        let meta = devices_by_id.get(dev_id);
        let d = totals_by_dev.get(dev_id).cloned().unwrap_or_default();
        let t = tts_by_device.get(dev_id).cloned().unwrap_or_default();
        mig_tot_chars += d.total_characters;
        mig_tot_dur += d.total_duration_ms;
        mig_tot_llm += d.llm_invoked_count;

        let local_cnt = local_device_recording_counts.get(dev_id).copied();
        let diff = local_cnt.map(|lc| lc - d.recording_count);
        device_reports.push(DeviceReconciliation {
            device_id: dev_id.clone(),
            device_name: meta
                .map(|m| m.device_name.clone())
                .unwrap_or_else(|| dev_id.clone()),
            platform: meta.map(|m| m.platform.clone()).unwrap_or_default(),
            migrated_surviving_history: surviving_count_by_dev.get(dev_id).copied().unwrap_or(0),
            migrated_all_time_dictations: d.recording_count,
            migrated_characters: d.total_characters,
            migrated_duration_ms: d.total_duration_ms,
            migrated_llm_invoked: d.llm_invoked_count,
            migrated_ai_corrections: d.ai_correction_count,
            tts_read_count: t.read_count,
            tts_translate_count: t.translate_count,
            tts_characters: t.characters,
            tts_duration_ms: t.duration_ms,
            tts_llm_count: t.llm_count,
            local_device_recording_count: local_cnt,
            local_vs_migrated_recording_diff: diff,
        });
    }

    let account_reconciliation = AccountReconciliation {
        v1_server_total_characters: v1_srv_chars,
        v1_server_total_duration_ms: v1_srv_dur,
        v1_server_llm_correction_count: v1_srv_llm,
        migrated_total_characters: mig_tot_chars,
        migrated_total_duration_ms: mig_tot_dur,
        migrated_llm_invoked_count: mig_tot_llm,
        excluded_translate_read_characters: excluded_translate_read_chars,
        excluded_translate_read_duration_ms: excluded_translate_read_dur,
        excluded_translate_read_llm_count: excluded_translate_read_llm,
        unexplained_characters_diff: v1_srv_chars - mig_tot_chars - excluded_translate_read_chars,
        unexplained_duration_ms_diff: v1_srv_dur - mig_tot_dur - excluded_translate_read_dur,
        unexplained_llm_diff: v1_srv_llm - mig_tot_llm - excluded_translate_read_llm,
    };

    Ok(MigrationReport {
        server_id,
        account_id,
        space_id,
        key_check_tag,
        recovery_key,
        surviving_shared_history_count: surviving_records.len(),
        excluded_translate_read_count: excluded_translate_read_ids.len(),
        excluded_failed_count,
        tombstone_count: deleted_ids.len(),
        imported_edits_count: imported_edits.len(),
        local_only_edits_preserved_count,
        stats_shards_published,
        total_envelopes_written: seq as usize,
        server_latest_seq: seq,
        server_used_bytes: used_bytes,
        devices: device_reports,
        account_reconciliation,
    })
}

/// Decrypt every envelope in a generated v2 server DB using `recovery_key` and
/// replay them into a brand-new empty client DB to verify full convergence.
pub fn verify_server_v2_replay(
    server_v2_db: &Path,
    recovery_key: &str,
) -> Result<ReplayVerificationReport, StorageError> {
    let srv = open_ro(server_v2_db)?;
    let server_id: String = srv
        .query_row("SELECT server_id FROM server_info WHERE id = 1", [], |r| {
            r.get(0)
        })
        .map_err(sql_err)?;
    let (account_id, stored_key_check): (String, Option<String>) = srv
        .query_row(
            "SELECT account_id, key_check_tag FROM accounts LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(sql_err)?;
    let space_id = selfhost_space_id(&server_id, &account_id);
    let master_key = MasterKey::from_recovery_key(recovery_key)
        .map_err(|e| StorageError::InvalidInput(e.to_string()))?;
    let expected_tag = master_key
        .derive_key_check_tag(&account_id, &space_id)
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
    if stored_key_check.as_deref() != Some(expected_tag.as_str()) {
        return Err(StorageError::InvalidInput(
            "key_check_tag mismatch on v2 server DB".into(),
        ));
    }

    // Build a clean in-memory client DB simulating a brand-new machine joining v2.
    let client = Connection::open_in_memory().map_err(sql_err)?;
    client
        .execute_batch(
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
        .map_err(sql_err)?;
    crate::post_edit::records::create_schema(&client)?;
    init_schema(&client)?;
    upsert_sync_space_tx(
        &client,
        &space_id,
        "selfhost",
        "http://127.0.0.1:8787",
        &server_id,
        &account_id,
        "fresh-verify-device",
        true,
        true,
        Some(&expected_tag),
        true,
    )?;

    let mut stmt = srv
        .prepare(
            "SELECT op_id, domain, object_id, revision, nonce, ciphertext
             FROM envelopes
             WHERE account_id = ?1 AND superseded = 0
             ORDER BY seq ASC",
        )
        .map_err(sql_err)?;
    let rows = stmt
        .query_map(params![account_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
            ))
        })
        .map_err(sql_err)?;

    let tx = client.unchecked_transaction().map_err(sql_err)?;
    let mut envelopes_decrypted = 0usize;
    for row in rows {
        let (op_id, domain_str, object_id, revision, nonce, ciphertext) = row.map_err(sql_err)?;
        let domain: SyncDomain = domain_str
            .parse()
            .map_err(|e: String| StorageError::QueryFailed(e))?;
        let plaintext = decrypt_envelope(
            &master_key,
            EnvelopeContext {
                account_id: &account_id,
                space_id: &space_id,
                domain,
                object_id: &object_id,
                revision,
                op_id: &op_id,
            },
            &nonce,
            &ciphertext,
        )
        .map_err(|e| StorageError::QueryFailed(format!("decrypt failed for {op_id}: {e}")))?;
        envelopes_decrypted += 1;

        match domain {
            SyncDomain::Stats => {
                let shard: StatsShardPayload = serde_json::from_slice(&plaintext)
                    .map_err(|e| StorageError::SerializeFailed(e.to_string()))?;
                match shard {
                    StatsShardPayload::Totals(t) => {
                        apply_remote_stats_totals_tx(&tx, &space_id, &t)?;
                    }
                    StatsShardPayload::DailyMonth(d) => {
                        apply_remote_stats_daily_month_tx(&tx, &space_id, &d)?;
                    }
                }
            }
            SyncDomain::History | SyncDomain::Edits => {
                let op: HistoryOpPayload = serde_json::from_slice(&plaintext)
                    .map_err(|e| StorageError::SerializeFailed(e.to_string()))?;
                apply_remote_history_op_tx(&tx, &space_id, "fresh-verify-device", &op, 0)?;
            }
        }
    }
    tx.commit().map_err(sql_err)?;

    let replayed_history_count: usize = client
        .query_row("SELECT COUNT(*) FROM history_record", [], |r| r.get(0))
        .map_err(sql_err)?;
    let replayed_tombstone_count: usize = client
        .query_row("SELECT COUNT(*) FROM history_tombstones", [], |r| r.get(0))
        .map_err(sql_err)?;
    let deleted_ids_resurrected: usize = client
        .query_row(
            "SELECT COUNT(*) FROM history_record h
             JOIN history_tombstones t ON t.record_id = h.id",
            [],
            |r| r.get(0),
        )
        .map_err(sql_err)?;
    let translate_read_leaked: usize = client
        .query_row(
            "SELECT COUNT(*) FROM history_record WHERE mode = ?1",
            params![HISTORY_MODE_TRANSLATE_READ],
            |r| r.get(0),
        )
        .map_err(sql_err)?;
    let totals = get_overview_usage_stats_in(&client, "fresh-verify-device", OverviewScope::All)?;

    Ok(ReplayVerificationReport {
        envelopes_decrypted,
        replayed_history_count,
        replayed_tombstone_count,
        replayed_total_recordings: totals.total_recording_count,
        replayed_total_characters: totals.total_characters,
        replayed_total_duration_ms: totals.total_duration_ms,
        replayed_llm_invoked: totals.llm_correction_count,
        replayed_tts_reads: totals.tts.read_count,
        replayed_tts_translates: totals.tts.translate_count,
        deleted_ids_resurrected,
        translate_read_leaked,
    })
}
