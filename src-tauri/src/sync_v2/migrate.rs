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
    active_space_in, apply_remote_history_op_tx, apply_remote_stats_daily_month_tx,
    apply_remote_stats_totals_tx, bump_local_stats_shard_tx, get_overview_usage_stats_in,
    init_schema, seed_all_legacy_local_edits_tx, upsert_sync_space_tx, OverviewScope,
    RemoteApplyOutcome,
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
    /// Totals reconstructed strictly from all-time valid dictations (excluding translate_read & failed).
    pub server_rebuilt_dictations: i64,
    pub server_rebuilt_characters: i64,
    pub server_rebuilt_duration_ms: i64,
    pub server_rebuilt_llm_invoked: i64,
    pub server_rebuilt_ai_corrections: i64,
    /// Raw lifetime counters from the device's v1 `device_usage_stats` (when device DB was supplied).
    pub local_device_recording_count: Option<i64>,
    pub local_device_characters: Option<i64>,
    pub local_device_duration_ms: Option<i64>,
    pub local_device_llm_invoked: Option<i64>,
    /// Legacy `translate_read` rows attributed to this device and excluded from dictation counters.
    pub excluded_translate_read_count: i64,
    pub excluded_translate_read_characters: i64,
    pub excluded_translate_read_duration_ms: i64,
    pub excluded_translate_read_llm_count: i64,
    /// Adopted lifetime counters written identically to server stats shards and device DBs.
    pub migrated_all_time_dictations: i64,
    pub migrated_characters: i64,
    pub migrated_duration_ms: i64,
    pub migrated_llm_invoked: i64,
    pub migrated_ai_corrections: i64,
    /// Device-level baseline adjustment (`adopted - server_rebuilt`); never fabricated into daily tables.
    pub baseline_adjustment_dictations: i64,
    pub baseline_adjustment_characters: i64,
    pub baseline_adjustment_duration_ms: i64,
    pub baseline_adjustment_llm_invoked: i64,
    pub baseline_decision: String,
    pub tts_read_count: i64,
    pub tts_translate_count: i64,
    pub tts_characters: i64,
    pub tts_duration_ms: i64,
    pub tts_llm_count: i64,
    pub local_vs_migrated_recording_diff: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountReconciliation {
    pub v1_server_total_characters: i64,
    pub v1_server_total_duration_ms: i64,
    pub v1_server_llm_correction_count: i64,
    pub server_rebuilt_total_characters: i64,
    pub server_rebuilt_total_duration_ms: i64,
    pub server_rebuilt_llm_invoked_count: i64,
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

/// Public, secret-free migration and reconciliation report safe to write to JSON/logs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationReport {
    pub server_id: String,
    pub account_id: String,
    pub space_id: String,
    pub key_check_tag: String,
    pub surviving_shared_history_count: usize,
    pub excluded_translate_read_count: usize,
    pub excluded_failed_count: usize,
    pub excluded_private_local_only_count: usize,
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

/// Full migration result separating the public reconciliation report from the
/// secret recovery key (intentionally NOT `Serialize` so it cannot leak into logs).
#[derive(Clone)]
pub struct MigrationOutcome {
    pub report: MigrationReport,
    pub recovery_key: String,
}

impl std::fmt::Debug for MigrationOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MigrationOutcome")
            .field("report", &self.report)
            .field("recovery_key", &"[REDACTED]")
            .finish()
    }
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
pub fn run_migration(cfg: &MigrationConfig) -> Result<MigrationOutcome, StorageError> {
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
    // `stats_records` holds all valid dictations for stats/daily breakdown (including local-only);
    // `shared_history_ids` holds ONLY records authorized to be published to the v2 server `history` domain (M2).
    let mut stats_records: BTreeMap<String, MergedHistoryRow> = BTreeMap::new();
    let mut shared_history_ids: BTreeSet<String> = BTreeSet::new();
    let mut deleted_ids: BTreeMap<String, (String, String)> = BTreeMap::new();
    let mut excluded_translate_read_ids: BTreeSet<String> = BTreeSet::new();
    let mut excluded_translate_read_by_dev: BTreeMap<String, DictationTotalsPayload> =
        BTreeMap::new();
    let mut excluded_translate_read_chars: i64 = 0;
    let mut excluded_translate_read_dur: i64 = 0;
    let mut excluded_translate_read_llm: i64 = 0;
    let mut excluded_failed_count: usize = 0;
    let mut excluded_private_local_only_count: usize = 0;

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

            let source_device_id = rec
                .get("sourceDeviceId")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .unwrap_or(&dev_id)
                .to_string();

            if mode == HISTORY_MODE_TRANSLATE_READ {
                if excluded_translate_read_ids.insert(record_id) {
                    let chars = text.chars().count() as i64;
                    let dur = duration_ms.max(0);
                    let llm = i64::from(llm_invoked);
                    excluded_translate_read_chars += chars;
                    excluded_translate_read_dur += dur;
                    excluded_translate_read_llm += llm;
                    let tr_dev = excluded_translate_read_by_dev
                        .entry(source_device_id)
                        .or_default();
                    tr_dev.recording_count += 1;
                    tr_dev.total_characters += chars;
                    tr_dev.total_duration_ms += dur;
                    tr_dev.llm_invoked_count += llm;
                }
                continue;
            }
            if error_code != HISTORY_ERROR_NONE || !is_final {
                excluded_failed_count += 1;
                continue;
            }

            let timestamp = rec
                .get("timestamp")
                .and_then(Value::as_str)
                .unwrap_or(&created_at)
                .to_string();

            shared_history_ids.insert(record_id.clone());
            stats_records.insert(
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

    // 4. Merge each supplied device DB (read-only): authorized pending creates,
    //    pending deletes, target apps, local edits, and local device totals comparison.
    let mut local_device_raw_totals: HashMap<String, DictationTotalsPayload> = HashMap::new();
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

        // Check local device_usage_stats for full lifetime reconciliation & TTS comparison (M1)
        if let Some((local_dict, tts)) = dev_ro
            .query_row(
                "SELECT total_recording_count, total_characters, total_duration_ms,
                        llm_correction_count,
                        tts_read_count, tts_translate_count,
                        tts_characters, tts_duration_ms, tts_llm_count
                 FROM device_usage_stats WHERE device_id = ?1 LIMIT 1",
                params![dev_id],
                |r| {
                    Ok((
                        DictationTotalsPayload {
                            recording_count: r.get(0)?,
                            total_characters: r.get(1)?,
                            total_duration_ms: r.get(2)?,
                            llm_invoked_count: r.get(3)?,
                            ai_correction_count: 0,
                        },
                        TtsTotalsPayload {
                            read_count: r.get(4)?,
                            translate_count: r.get(5)?,
                            characters: r.get(6)?,
                            duration_ms: r.get(7)?,
                            llm_count: r.get(8)?,
                        },
                    ))
                },
            )
            .optional()
            .map_err(sql_err)?
        {
            local_device_raw_totals.insert(dev_id.clone(), local_dict);
            let entry = tts_by_device.entry(dev_id.clone()).or_default();
            entry.read_count = entry.read_count.max(tts.read_count);
            entry.translate_count = entry.translate_count.max(tts.translate_count);
            entry.characters = entry.characters.max(tts.characters);
            entry.duration_ms = entry.duration_ms.max(tts.duration_ms);
            entry.llm_count = entry.llm_count.max(tts.llm_count);
        }

        // Inspect v1 sync_outbox: pending deletes + explicit v1 pending upsert evidence (M2)
        let mut device_outbox_upsert_ids: BTreeSet<String> = BTreeSet::new();
        {
            let has_outbox: bool = dev_ro
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'sync_outbox')",
                    [],
                    |r| Ok(r.get::<_, i32>(0)? != 0),
                )
                .map_err(sql_err)?;
            if has_outbox {
                let mut stmt = dev_ro
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
                    .map_err(sql_err)?;
                for row in rows {
                    let (ev_type, rid_opt, payload_str, ts) = row.map_err(sql_err)?;
                    let rid = rid_opt
                        .filter(|s| !s.trim().is_empty())
                        .or_else(|| {
                            serde_json::from_str::<Value>(&payload_str)
                                .ok()
                                .and_then(|v| {
                                    v.get("recordId")
                                        .or_else(|| v.get("id"))
                                        .or_else(|| v.get("record").and_then(|r| r.get("id")))
                                        .and_then(Value::as_str)
                                        .map(String::from)
                                })
                        });
                    let Some(rid) = rid.filter(|s| !s.trim().is_empty()) else {
                        continue;
                    };
                    match ev_type.as_str() {
                        "history.delete" => {
                            device_outbox_upsert_ids.remove(&rid);
                            deleted_ids.entry(rid).or_insert((dev_id.clone(), ts));
                        }
                        "history.upsert" => {
                            if !deleted_ids.contains_key(&rid) {
                                device_outbox_upsert_ids.insert(rid);
                            }
                        }
                        _ => {}
                    }
                }
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

                if let Some(existing) = stats_records.get_mut(&id) {
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
                    // Always include eligible local dictations in stats reconstruction,
                    // but ONLY authorize publishing text to `history` if this device is
                    // history-scoped AND has explicit v1 outbox upload evidence (M2).
                    let can_publish_local =
                        input.scope_history && device_outbox_upsert_ids.contains(&id);
                    if can_publish_local {
                        shared_history_ids.insert(id.clone());
                    } else {
                        excluded_private_local_only_count += 1;
                    }
                    stats_records.insert(
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
                    if shared_history_ids.contains(&id)
                        && input.scope_history
                        && input.scope_edits
                        && input.import_existing_edits
                    {
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
    let mut surviving_records: Vec<&MergedHistoryRow> = stats_records
        .values()
        .filter(|r| shared_history_ids.contains(&r.record_id) && !deleted_ids.contains_key(&r.record_id))
        .collect();
    surviving_records.sort_by(|a, b| {
        a.timestamp
            .cmp(&b.timestamp)
            .then_with(|| a.record_id.cmp(&b.record_id))
    });

    // Aggregate per-device server-rebuilt lifetime totals + per-month daily app & model rows
    let mut server_rebuilt_by_dev: BTreeMap<String, DictationTotalsPayload> = BTreeMap::new();
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

    for rec in stats_records.values() {
        let chars = rec.text.chars().count() as i64;
        let tot = server_rebuilt_by_dev
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
    all_device_ids.extend(server_rebuilt_by_dev.keys().cloned());
    all_device_ids.extend(local_device_raw_totals.keys().cloned());
    all_device_ids.extend(tts_by_device.keys().cloned());

    // M1: Compute adopted lifetime totals per device (preserving same-metric local lifetime
    // baseline after deducting excluded `translate_read` rows, without fabricating daily rows).
    let mut adopted_totals_by_dev: BTreeMap<String, DictationTotalsPayload> = BTreeMap::new();
    let mut baseline_decision_by_dev: BTreeMap<String, String> = BTreeMap::new();
    for dev_id in &all_device_ids {
        let rebuilt = server_rebuilt_by_dev.get(dev_id).cloned().unwrap_or_default();
        let tr = excluded_translate_read_by_dev
            .get(dev_id)
            .cloned()
            .unwrap_or_default();
        if let Some(local_raw) = local_device_raw_totals.get(dev_id) {
            let local_adj_rec = (local_raw.recording_count - tr.recording_count).max(0);
            let local_adj_chars = (local_raw.total_characters - tr.total_characters).max(0);
            let local_adj_dur = (local_raw.total_duration_ms - tr.total_duration_ms).max(0);
            let local_adj_llm = (local_raw.llm_invoked_count - tr.llm_invoked_count).max(0);

            let adopted = DictationTotalsPayload {
                recording_count: rebuilt.recording_count.max(local_adj_rec),
                total_characters: rebuilt.total_characters.max(local_adj_chars),
                total_duration_ms: rebuilt.total_duration_ms.max(local_adj_dur),
                llm_invoked_count: rebuilt.llm_invoked_count.max(local_adj_llm),
                ai_correction_count: rebuilt.ai_correction_count,
            };
            let used_local = adopted.recording_count > rebuilt.recording_count
                || adopted.total_characters > rebuilt.total_characters
                || adopted.total_duration_ms > rebuilt.total_duration_ms
                || adopted.llm_invoked_count > rebuilt.llm_invoked_count;
            baseline_decision_by_dev.insert(
                dev_id.clone(),
                if used_local {
                    "adopted_local_baseline_minus_translate_read".to_string()
                } else {
                    "matched_server_rebuilt".to_string()
                },
            );
            adopted_totals_by_dev.insert(dev_id.clone(), adopted);
        } else {
            baseline_decision_by_dev
                .insert(dev_id.clone(), "server_rebuilt_only_no_local_db".to_string());
            adopted_totals_by_dev.insert(dev_id.clone(), rebuilt);
        }
    }

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
        let dictation = adopted_totals_by_dev.get(dev_id).cloned().unwrap_or_default();
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
        let _ = write_envelope(
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
        let _ = write_envelope(
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
        let _ = write_envelope(
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

        // Read this device's configured text retention policy (default 0 = keep all).
        let dev_retention_days: u32 = dtx
            .query_row(
                "SELECT value FROM user_config WHERE key = 'app_settings' LIMIT 1",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(sql_err)?
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .and_then(|v| v.get("textRetentionDays").and_then(Value::as_u64))
            .map(|n| n as u32)
            .unwrap_or(0);

        // M4: Replay authorized server envelopes using the exact same pull apply logic
        // so device DB records, edit versions, tombstones, retention, and cursors match live pull.
        let mut dev_history_cursor: i64 = 0;
        let mut dev_edits_cursor: i64 = 0;

        // 7a. Replay stats domain (always authorized)
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
        let dev_stats_cursor = stats_end_seq;

        // Helper to decrypt & replay all active envelopes of a single domain from `srv_v2`.
        let replay_domain = |domain: SyncDomain, dtx: &Connection| -> Result<i64, StorageError> {
            let mut stmt = srv_v2
                .prepare(
                    "SELECT seq, op_id, object_id, revision, nonce, ciphertext
                     FROM envelopes
                     WHERE account_id = ?1 AND domain = ?2 AND superseded = 0
                     ORDER BY seq ASC",
                )
                .map_err(sql_err)?;
            let rows = stmt
                .query_map(params![account_id, domain.as_str()], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, String>(5)?,
                    ))
                })
                .map_err(sql_err)?;
            let mut last_seq = 0i64;
            for row in rows {
                let (env_seq, op_id, object_id, revision, nonce, ciphertext) = row.map_err(sql_err)?;
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
                let op: HistoryOpPayload = serde_json::from_slice(&plaintext)
                    .map_err(|e| StorageError::SerializeFailed(e.to_string()))?;
                apply_remote_history_op_tx(dtx, &space_id, dev_id, &op, dev_retention_days)?;
                last_seq = env_seq;
            }
            Ok(last_seq)
        };

        // 7b. Replay `edits` BEFORE `history` when authorized so imported edits land with their
        // authoritative server `edit_id` (`is_shared = 1`) before `Create` checks legacy edits (M4+M5).
        if input.scope_history && input.scope_edits {
            dev_edits_cursor = replay_domain(SyncDomain::Edits, &dtx)?;
        }
        if input.scope_history {
            dev_history_cursor = replay_domain(SyncDomain::History, &dtx)?;
        }

        // 7c. Seed any remaining unimported local legacy edits as private (`is_shared = 0`) versions (M5).
        seed_all_legacy_local_edits_tx(&dtx)?;

        // 7d. Mark v1 outbox handoff done since offline migration already processed this device's v1 outbox.
        dtx.execute(
            "INSERT INTO user_config (key, value) VALUES (?1, '1')
             ON CONFLICT(key) DO UPDATE SET value = '1'",
            params![format!("v1_outbox_handoff_done:{space_id}")],
        )
        .map_err(sql_err)?;

        dtx.execute(
            "UPDATE sync_spaces
             SET stats_cursor = ?2, history_cursor = ?3, edits_cursor = ?4, last_sync_at = ?5, updated_at = ?5
             WHERE space_id = ?1",
            params![space_id, dev_stats_cursor, dev_history_cursor, dev_edits_cursor, now],
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

    // 8. Build reconciliation report (secret-free: recovery_key returned separately in MigrationOutcome)
    let mut device_reports = Vec::new();
    let mut rebuilt_tot_chars = 0i64;
    let mut rebuilt_tot_dur = 0i64;
    let mut rebuilt_tot_llm = 0i64;
    let mut mig_tot_chars = 0i64;
    let mut mig_tot_dur = 0i64;
    let mut mig_tot_llm = 0i64;

    for dev_id in &all_device_ids {
        let meta = devices_by_id.get(dev_id);
        let rebuilt = server_rebuilt_by_dev.get(dev_id).cloned().unwrap_or_default();
        let adopted = adopted_totals_by_dev.get(dev_id).cloned().unwrap_or_default();
        let tr = excluded_translate_read_by_dev
            .get(dev_id)
            .cloned()
            .unwrap_or_default();
        let t = tts_by_device.get(dev_id).cloned().unwrap_or_default();

        rebuilt_tot_chars += rebuilt.total_characters;
        rebuilt_tot_dur += rebuilt.total_duration_ms;
        rebuilt_tot_llm += rebuilt.llm_invoked_count;
        mig_tot_chars += adopted.total_characters;
        mig_tot_dur += adopted.total_duration_ms;
        mig_tot_llm += adopted.llm_invoked_count;

        let local_raw = local_device_raw_totals.get(dev_id);
        let local_cnt = local_raw.map(|r| r.recording_count);
        let diff = local_cnt.map(|lc| lc - adopted.recording_count);
        device_reports.push(DeviceReconciliation {
            device_id: dev_id.clone(),
            device_name: meta
                .map(|m| m.device_name.clone())
                .unwrap_or_else(|| dev_id.clone()),
            platform: meta.map(|m| m.platform.clone()).unwrap_or_default(),
            migrated_surviving_history: surviving_count_by_dev.get(dev_id).copied().unwrap_or(0),
            server_rebuilt_dictations: rebuilt.recording_count,
            server_rebuilt_characters: rebuilt.total_characters,
            server_rebuilt_duration_ms: rebuilt.total_duration_ms,
            server_rebuilt_llm_invoked: rebuilt.llm_invoked_count,
            server_rebuilt_ai_corrections: rebuilt.ai_correction_count,
            local_device_recording_count: local_cnt,
            local_device_characters: local_raw.map(|r| r.total_characters),
            local_device_duration_ms: local_raw.map(|r| r.total_duration_ms),
            local_device_llm_invoked: local_raw.map(|r| r.llm_invoked_count),
            excluded_translate_read_count: tr.recording_count,
            excluded_translate_read_characters: tr.total_characters,
            excluded_translate_read_duration_ms: tr.total_duration_ms,
            excluded_translate_read_llm_count: tr.llm_invoked_count,
            migrated_all_time_dictations: adopted.recording_count,
            migrated_characters: adopted.total_characters,
            migrated_duration_ms: adopted.total_duration_ms,
            migrated_llm_invoked: adopted.llm_invoked_count,
            migrated_ai_corrections: adopted.ai_correction_count,
            baseline_adjustment_dictations: adopted.recording_count - rebuilt.recording_count,
            baseline_adjustment_characters: adopted.total_characters - rebuilt.total_characters,
            baseline_adjustment_duration_ms: adopted.total_duration_ms - rebuilt.total_duration_ms,
            baseline_adjustment_llm_invoked: adopted.llm_invoked_count - rebuilt.llm_invoked_count,
            baseline_decision: baseline_decision_by_dev
                .get(dev_id)
                .cloned()
                .unwrap_or_default(),
            tts_read_count: t.read_count,
            tts_translate_count: t.translate_count,
            tts_characters: t.characters,
            tts_duration_ms: t.duration_ms,
            tts_llm_count: t.llm_count,
            local_vs_migrated_recording_diff: diff,
        });
    }

    let account_reconciliation = AccountReconciliation {
        v1_server_total_characters: v1_srv_chars,
        v1_server_total_duration_ms: v1_srv_dur,
        v1_server_llm_correction_count: v1_srv_llm,
        server_rebuilt_total_characters: rebuilt_tot_chars,
        server_rebuilt_total_duration_ms: rebuilt_tot_dur,
        server_rebuilt_llm_invoked_count: rebuilt_tot_llm,
        migrated_total_characters: mig_tot_chars,
        migrated_total_duration_ms: mig_tot_dur,
        migrated_llm_invoked_count: mig_tot_llm,
        excluded_translate_read_characters: excluded_translate_read_chars,
        excluded_translate_read_duration_ms: excluded_translate_read_dur,
        excluded_translate_read_llm_count: excluded_translate_read_llm,
        unexplained_characters_diff: v1_srv_chars - rebuilt_tot_chars - excluded_translate_read_chars,
        unexplained_duration_ms_diff: v1_srv_dur - rebuilt_tot_dur - excluded_translate_read_dur,
        unexplained_llm_diff: v1_srv_llm - rebuilt_tot_llm - excluded_translate_read_llm,
    };

    Ok(MigrationOutcome {
        report: MigrationReport {
            server_id,
            account_id,
            space_id,
            key_check_tag,
            surviving_shared_history_count: surviving_records.len(),
            excluded_translate_read_count: excluded_translate_read_ids.len(),
            excluded_failed_count,
            excluded_private_local_only_count,
            tombstone_count: deleted_ids.len(),
            imported_edits_count: imported_edits.len(),
            local_only_edits_preserved_count,
            stats_shards_published,
            total_envelopes_written: seq as usize,
            server_latest_seq: seq,
            server_used_bytes: used_bytes,
            devices: device_reports,
            account_reconciliation,
        },
        recovery_key,
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

/// Configuration for idempotent forward-repair of an already-cutover live v2 device DB
/// (M1 net lifetime baseline delta + M4/M5 migrated edit versions + N3 180-day history backfill).
#[derive(Debug, Clone)]
pub struct ForwardRepairConfig {
    pub server_v2_db: PathBuf,
    pub in_device_db: PathBuf,
    pub out_device_db: PathBuf,
    pub recovery_key_file: PathBuf,
    pub v1_device_db: Option<PathBuf>,
    pub v1_server_db: Option<PathBuf>,
    pub initial_v2_device_db: Option<PathBuf>,
    pub replay_history_within_retention: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceRepairSnapshot {
    pub local_recording_count: i64,
    pub local_characters: i64,
    pub local_duration_ms: i64,
    pub local_llm_invoked: i64,
    pub history_record_count: i64,
    pub oldest_history_timestamp: Option<String>,
    pub newest_history_timestamp: Option<String>,
    pub edited_text_count: i64,
    pub edit_versions_count: i64,
    pub edit_conflict_count: i64,
    pub tombstone_count: i64,
    pub totals_local_revision: i64,
    pub totals_acked_revision: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForwardRepairReport {
    pub space_id: String,
    pub device_id: String,
    pub text_retention_days: u32,
    pub baseline_repair_already_done: bool,
    pub baseline_delta_recordings: i64,
    pub baseline_delta_characters: i64,
    pub baseline_delta_duration_ms: i64,
    pub baseline_delta_llm_invoked: i64,
    pub edits_envelopes_replayed: usize,
    pub history_envelopes_replayed: usize,
    pub history_records_backfilled: usize,
    pub legacy_private_edits_seeded: usize,
    pub deleted_ids_resurrected: i64,
    pub translate_read_leaked: i64,
    pub before: DeviceRepairSnapshot,
    pub after: DeviceRepairSnapshot,
}

fn snapshot_device_db(conn: &Connection, space_id: &str, device_id: &str) -> Result<DeviceRepairSnapshot, StorageError> {
    let (local_recording_count, local_characters, local_duration_ms, local_llm_invoked): (i64, i64, i64, i64) = conn
        .query_row(
            "SELECT total_recording_count, total_characters, total_duration_ms, llm_correction_count
             FROM device_usage_stats WHERE device_id = ?1",
            params![device_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .map_err(sql_err)?
        .unwrap_or((0, 0, 0, 0));
    let history_record_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM history_record", [], |r| r.get(0))
        .map_err(sql_err)?;
    let (oldest_history_timestamp, newest_history_timestamp): (Option<String>, Option<String>) = conn
        .query_row(
            "SELECT MIN(timestamp), MAX(timestamp) FROM history_record",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(sql_err)?;
    let edited_text_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM history_record WHERE edited_text IS NOT NULL AND edited_text != ''",
            [],
            |r| r.get(0),
        )
        .map_err(sql_err)?;
    let edit_versions_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM history_edit_versions", [], |r| r.get(0))
        .map_err(sql_err)?;
    let edit_conflict_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM history_record WHERE COALESCE(has_edit_conflict, 0) != 0",
            [],
            |r| r.get(0),
        )
        .map_err(sql_err)?;
    let tombstone_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM history_tombstones", [], |r| r.get(0))
        .map_err(sql_err)?;
    let (totals_local_revision, totals_acked_revision): (i64, i64) = conn
        .query_row(
            "SELECT local_revision, acked_revision FROM sync_stats_shards
             WHERE space_id = ?1 AND device_id = ?2 AND shard_key = 'totals'",
            params![space_id, device_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(sql_err)?
        .unwrap_or((0, 0));

    Ok(DeviceRepairSnapshot {
        local_recording_count,
        local_characters,
        local_duration_ms,
        local_llm_invoked,
        history_record_count,
        oldest_history_timestamp,
        newest_history_timestamp,
        edited_text_count,
        edit_versions_count,
        edit_conflict_count,
        tombstone_count,
        totals_local_revision,
        totals_acked_revision,
    })
}

/// Idempotently forward-repair an already-cutover v2 device DB copy without losing
/// post-cutover dictations, edits, or outbox ops.
pub fn run_forward_repair(cfg: &ForwardRepairConfig) -> Result<ForwardRepairReport, StorageError> {
    let recovery_key = std::fs::read_to_string(&cfg.recovery_key_file)
        .map_err(|e| StorageError::FileOpFailed(format!("{}: {e}", cfg.recovery_key_file.display())))?;
    let master_key = MasterKey::from_recovery_key(recovery_key.trim())
        .map_err(|e| StorageError::InvalidInput(e.to_string()))?;

    let srv = open_ro(&cfg.server_v2_db)?;
    let server_id: String = srv
        .query_row("SELECT server_id FROM server_info WHERE id = 1", [], |r| r.get(0))
        .map_err(sql_err)?;

    // Copy input DB via SQLite online backup if out != in
    if cfg.out_device_db != cfg.in_device_db {
        if cfg.out_device_db.exists() {
            std::fs::remove_file(&cfg.out_device_db)
                .map_err(|e| StorageError::FileOpFailed(e.to_string()))?;
        }
        if let Some(parent) = cfg.out_device_db.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| StorageError::FileOpFailed(e.to_string()))?;
            }
        }
        let src = open_ro(&cfg.in_device_db)?;
        let mut dst = Connection::open(&cfg.out_device_db)
            .map_err(|e| StorageError::ConnectionFailed(e.to_string()))?;
        let backup = rusqlite::backup::Backup::new(&src, &mut dst).map_err(sql_err)?;
        backup
            .run_to_completion(200, std::time::Duration::from_millis(5), None)
            .map_err(sql_err)?;
    }

    let dev = Connection::open(&cfg.out_device_db)
        .map_err(|e| StorageError::ConnectionFailed(e.to_string()))?;
    init_schema(&dev)?;

    let space = active_space_in(&dev)?
        .ok_or_else(|| StorageError::InvalidInput("device DB has no active v2 sync space".into()))?;
    let expected_space_id = selfhost_space_id(&server_id, &space.account_id);
    if space.space_id != expected_space_id {
        return Err(StorageError::InvalidInput(format!(
            "space_id mismatch: device has {}, server has {}",
            space.space_id, expected_space_id
        )));
    }
    let expected_tag = master_key
        .derive_key_check_tag(&space.account_id, &space.space_id)
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
    let stored_tag: Option<String> = srv
        .query_row(
            "SELECT key_check_tag FROM accounts WHERE account_id = ?1",
            params![space.account_id],
            |r| r.get(0),
        )
        .map_err(sql_err)?;
    if stored_tag.as_deref() != Some(expected_tag.as_str()) {
        return Err(StorageError::InvalidInput(
            "recovery key does not match server key_check_tag".into(),
        ));
    }

    let before = snapshot_device_db(&dev, &space.space_id, &space.device_id)?;

    let text_retention_days: u32 = dev
        .query_row(
            "SELECT value FROM user_config WHERE key = 'app_settings' LIMIT 1",
            [],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .map_err(sql_err)?
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|v| v.get("textRetentionDays").and_then(Value::as_u64))
        .map(|n| n as u32)
        .unwrap_or(0);

    // Compute exact net same-metric baseline delta if v1 backups + initial v2 DB were supplied
    let mut delta_rec = 0i64;
    let mut delta_chars = 0i64;
    let mut delta_dur = 0i64;
    let mut delta_llm = 0i64;

    if let (Some(v1_dev_p), Some(v1_srv_p), Some(init_v2_p)) = (
        cfg.v1_device_db.as_deref(),
        cfg.v1_server_db.as_deref(),
        cfg.initial_v2_device_db.as_deref(),
    ) {
        let v1_dev = open_ro(v1_dev_p)?;
        let v1_srv = open_ro(v1_srv_p)?;
        let init_v2 = open_ro(init_v2_p)?;

        let (raw_rec, raw_chars, raw_dur, raw_llm): (i64, i64, i64, i64) = v1_dev
            .query_row(
                "SELECT total_recording_count, total_characters, total_duration_ms, llm_correction_count
                 FROM device_usage_stats WHERE device_id = ?1",
                params![space.device_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .map_err(sql_err)?;
        let (init_rec, init_chars, init_dur, init_llm): (i64, i64, i64, i64) = init_v2
            .query_row(
                "SELECT total_recording_count, total_characters, total_duration_ms, llm_correction_count
                 FROM device_usage_stats WHERE device_id = ?1",
                params![space.device_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .map_err(sql_err)?;

        // Deduct legacy `translate_read` rows attributed to this device from v1_local_raw
        let mut tr_ids = BTreeSet::new();
        let (mut tr_rec, mut tr_chars, mut tr_dur, mut tr_llm) = (0i64, 0i64, 0i64, 0i64);
        {
            let mut stmt = v1_srv
                .prepare(
                    "SELECT device_id, record_id, payload FROM events
                     WHERE account_id = ?1 AND type = 'history.upsert'",
                )
                .map_err(sql_err)?;
            let rows = stmt
                .query_map(params![space.account_id], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                })
                .map_err(sql_err)?;
            for row in rows {
                let (ev_dev, rid_opt, payload_str) = row.map_err(sql_err)?;
                let Ok(v) = serde_json::from_str::<Value>(&payload_str) else {
                    continue;
                };
                let rec = v.get("record").unwrap_or(&v);
                if rec.get("mode").and_then(Value::as_str) != Some(HISTORY_MODE_TRANSLATE_READ) {
                    continue;
                }
                let src_dev = rec
                    .get("sourceDeviceId")
                    .and_then(Value::as_str)
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or(&ev_dev);
                if src_dev != space.device_id {
                    continue;
                }
                let rid = rid_opt
                    .or_else(|| rec.get("id").and_then(Value::as_str).map(String::from))
                    .unwrap_or_default();
                if rid.is_empty() || !tr_ids.insert(rid) {
                    continue;
                }
                let text = rec.get("text").and_then(Value::as_str).unwrap_or("");
                let dur = rec.get("durationMs").and_then(Value::as_i64).unwrap_or(0).max(0);
                let ai_corr = rec.get("aiCorrectionApplied").and_then(Value::as_bool).unwrap_or(false);
                let llm = rec.get("llmInvoked").and_then(Value::as_bool).unwrap_or(ai_corr);
                tr_rec += 1;
                tr_chars += text.chars().count() as i64;
                tr_dur += dur;
                tr_llm += i64::from(llm);
            }
        }

        delta_rec = ((raw_rec - tr_rec).max(0) - init_rec).max(0);
        delta_chars = ((raw_chars - tr_chars).max(0) - init_chars).max(0);
        delta_dur = ((raw_dur - tr_dur).max(0) - init_dur).max(0);
        delta_llm = ((raw_llm - tr_llm).max(0) - init_llm).max(0);
    }

    let tx = dev.unchecked_transaction().map_err(sql_err)?;
    let now = Utc::now().to_rfc3339();

    // 1. Apply one-time idempotent baseline delta + bump `totals` shard so next sync publishes to server
    let baseline_flag_key = format!("v2_forward_repair_baseline_v1:{}", space.space_id);
    let baseline_already_done = tx
        .query_row(
            "SELECT value FROM user_config WHERE key = ?1 LIMIT 1",
            params![baseline_flag_key],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .map_err(sql_err)?
        .as_deref()
        == Some("1");

    if !baseline_already_done && (delta_rec > 0 || delta_chars > 0 || delta_dur > 0 || delta_llm > 0)
    {
        tx.execute(
            "UPDATE device_usage_stats
             SET total_recording_count = total_recording_count + ?2,
                 total_characters = total_characters + ?3,
                 total_duration_ms = total_duration_ms + ?4,
                 llm_correction_count = llm_correction_count + ?5,
                 last_updated = ?6
             WHERE device_id = ?1",
            params![
                space.device_id,
                delta_rec,
                delta_chars,
                delta_dur,
                delta_llm,
                now
            ],
        )
        .map_err(sql_err)?;
        bump_local_stats_shard_tx(&tx, &space.space_id, &space.device_id, "totals")?;
        tx.execute(
            "INSERT INTO user_config (key, value) VALUES (?1, '1')
             ON CONFLICT(key) DO UPDATE SET value = '1'",
            params![baseline_flag_key],
        )
        .map_err(sql_err)?;
    }

    // 2. Replay all active `edits` envelopes FIRST so the 69 migrated edits land with their
    //    exact authoritative server edit_ids (`is_shared = 1`) and merge with post-cutover edits.
    let mut edits_envelopes_replayed = 0usize;
    let mut max_edits_seq = space.edits_cursor;
    if space.scope_history && space.scope_edits {
        let mut stmt = srv
            .prepare(
                "SELECT seq, op_id, object_id, revision, nonce, ciphertext
                 FROM envelopes
                 WHERE account_id = ?1 AND domain = 'edits' AND superseded = 0
                 ORDER BY seq ASC",
            )
            .map_err(sql_err)?;
        let rows = stmt
            .query_map(params![space.account_id], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                ))
            })
            .map_err(sql_err)?;
        for row in rows {
            let (env_seq, op_id, object_id, revision, nonce, ciphertext) = row.map_err(sql_err)?;
            let plaintext = decrypt_envelope(
                &master_key,
                EnvelopeContext {
                    account_id: &space.account_id,
                    space_id: &space.space_id,
                    domain: SyncDomain::Edits,
                    object_id: &object_id,
                    revision,
                    op_id: &op_id,
                },
                &nonce,
                &ciphertext,
            )
            .map_err(|e| StorageError::QueryFailed(format!("decrypt failed for {op_id}: {e}")))?;
            let op: HistoryOpPayload = serde_json::from_slice(&plaintext)
                .map_err(|e| StorageError::SerializeFailed(e.to_string()))?;
            apply_remote_history_op_tx(&tx, &space.space_id, &space.device_id, &op, text_retention_days)?;
            edits_envelopes_replayed += 1;
            max_edits_seq = max_edits_seq.max(env_seq);
        }
    }

    // 3. Optionally replay all active `history` envelopes within `textRetentionDays` (N3)
    let mut history_envelopes_replayed = 0usize;
    let mut history_records_backfilled = 0usize;
    let mut max_history_seq = space.history_cursor;
    if cfg.replay_history_within_retention && space.scope_history {
        let mut stmt = srv
            .prepare(
                "SELECT seq, op_id, object_id, revision, nonce, ciphertext
                 FROM envelopes
                 WHERE account_id = ?1 AND domain = 'history' AND superseded = 0
                 ORDER BY seq ASC",
            )
            .map_err(sql_err)?;
        let rows = stmt
            .query_map(params![space.account_id], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                ))
            })
            .map_err(sql_err)?;
        for row in rows {
            let (env_seq, op_id, object_id, revision, nonce, ciphertext) = row.map_err(sql_err)?;
            let plaintext = decrypt_envelope(
                &master_key,
                EnvelopeContext {
                    account_id: &space.account_id,
                    space_id: &space.space_id,
                    domain: SyncDomain::History,
                    object_id: &object_id,
                    revision,
                    op_id: &op_id,
                },
                &nonce,
                &ciphertext,
            )
            .map_err(|e| StorageError::QueryFailed(format!("decrypt failed for {op_id}: {e}")))?;
            let op: HistoryOpPayload = serde_json::from_slice(&plaintext)
                .map_err(|e| StorageError::SerializeFailed(e.to_string()))?;
            let outcome = apply_remote_history_op_tx(
                &tx,
                &space.space_id,
                &space.device_id,
                &op,
                text_retention_days,
            )?;
            history_envelopes_replayed += 1;
            if matches!(op, HistoryOpPayload::Create(_)) && outcome == RemoteApplyOutcome::Applied {
                history_records_backfilled += 1;
            }
            max_history_seq = max_history_seq.max(env_seq);
        }
    }

    // 4. Seed any remaining unimported local legacy edits as private (`is_shared = 0`)
    let legacy_private_edits_seeded = seed_all_legacy_local_edits_tx(&tx)?;

    // 5. Align `sync_devices.last_synced_at` with server `devices.last_seen_at` (N1)
    {
        let mut stmt = srv
            .prepare("SELECT device_id, last_seen_at FROM devices WHERE account_id = ?1")
            .map_err(sql_err)?;
        let rows = stmt
            .query_map(params![space.account_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(sql_err)?;
        for row in rows {
            let (d_id, last_seen) = row.map_err(sql_err)?;
            tx.execute(
                "UPDATE sync_devices SET last_synced_at = ?3, updated_at = ?4
                 WHERE space_id = ?1 AND device_id = ?2",
                params![space.space_id, d_id, last_seen, now],
            )
            .map_err(sql_err)?;
        }
    }

    tx.execute(
        "UPDATE sync_spaces
         SET history_cursor = MAX(history_cursor, ?2),
             edits_cursor = MAX(edits_cursor, ?3),
             updated_at = ?4
         WHERE space_id = ?1",
        params![space.space_id, max_history_seq, max_edits_seq, now],
    )
    .map_err(sql_err)?;
    tx.commit().map_err(sql_err)?;

    let deleted_ids_resurrected: i64 = dev
        .query_row(
            "SELECT COUNT(*) FROM history_record h
             JOIN history_tombstones t ON t.record_id = h.id AND t.space_id = ?1",
            params![space.space_id],
            |r| r.get(0),
        )
        .map_err(sql_err)?;
    let translate_read_leaked: i64 = dev
        .query_row(
            "SELECT COUNT(*) FROM history_record WHERE mode = ?1 AND sync_eligible = 1",
            params![HISTORY_MODE_TRANSLATE_READ],
            |r| r.get(0),
        )
        .map_err(sql_err)?;
    let after = snapshot_device_db(&dev, &space.space_id, &space.device_id)?;

    Ok(ForwardRepairReport {
        space_id: space.space_id,
        device_id: space.device_id,
        text_retention_days,
        baseline_repair_already_done: baseline_already_done,
        baseline_delta_recordings: if baseline_already_done { 0 } else { delta_rec },
        baseline_delta_characters: if baseline_already_done { 0 } else { delta_chars },
        baseline_delta_duration_ms: if baseline_already_done { 0 } else { delta_dur },
        baseline_delta_llm_invoked: if baseline_already_done { 0 } else { delta_llm },
        edits_envelopes_replayed,
        history_envelopes_replayed,
        history_records_backfilled,
        legacy_private_edits_seeded,
        deleted_ids_resurrected,
        translate_read_leaked,
        before,
        after,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_v1_server_fixture(path: &Path) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE server_info (id INTEGER PRIMARY KEY, server_id TEXT NOT NULL);
            CREATE TABLE accounts (account_id TEXT PRIMARY KEY, created_at TEXT NOT NULL);
            CREATE TABLE devices (
                account_id TEXT NOT NULL, device_id TEXT NOT NULL, device_name TEXT NOT NULL,
                platform TEXT, app_version TEXT, created_at TEXT NOT NULL, last_seen_at TEXT NOT NULL
            );
            CREATE TABLE usage_stats (
                account_id TEXT PRIMARY KEY, total_duration_ms INTEGER NOT NULL,
                total_characters INTEGER NOT NULL, llm_correction_count INTEGER NOT NULL,
                updated_at TEXT
            );
            CREATE TABLE device_tts_stats (
                account_id TEXT NOT NULL, device_id TEXT NOT NULL,
                read_count INTEGER NOT NULL, translate_count INTEGER NOT NULL,
                characters INTEGER NOT NULL, duration_ms INTEGER NOT NULL, llm_count INTEGER NOT NULL
            );
            CREATE TABLE events (
                seq INTEGER PRIMARY KEY AUTOINCREMENT,
                account_id TEXT NOT NULL, event_id TEXT NOT NULL, device_id TEXT NOT NULL,
                type TEXT NOT NULL, record_id TEXT, payload TEXT NOT NULL, created_at TEXT NOT NULL
            );
            INSERT INTO server_info VALUES (1, 'srv-1');
            INSERT INTO accounts VALUES ('acc-1', '2026-01-01T00:00:00Z');
            INSERT INTO devices VALUES ('acc-1', 'dev-a', 'MacA', 'macos', '0.19.4', '2026-01-01T00:00:00Z', '2026-11-25T00:00:00Z');
            INSERT INTO devices VALUES ('acc-1', 'dev-b', 'WinB', 'windows', '0.19.4', '2026-01-01T00:00:00Z', '2026-11-25T00:00:00Z');
            INSERT INTO usage_stats VALUES ('acc-1', 3000, 30, 3, '2026-11-25T00:00:00Z');
            "#,
        )
        .unwrap();

        let evs = [
            (
                "ev-1",
                "dev-a",
                "history.upsert",
                "r-shared-1",
                r#"{"id":"r-shared-1","sourceDeviceId":"dev-a","timestamp":"2026-11-24T01:00:00Z","text":"1234567890","durationMs":1000,"aiCorrectionApplied":true,"llmInvoked":true,"mode":"assistant_hands_free","isFinal":true,"errorCode":0}"#,
            ),
            (
                "ev-2",
                "dev-a",
                "history.upsert",
                "r-server-only",
                r#"{"id":"r-server-only","sourceDeviceId":"dev-a","timestamp":"2026-11-24T02:00:00Z","text":"abcdefghij","durationMs":2000,"aiCorrectionApplied":false,"llmInvoked":true,"mode":"assistant_hands_free","isFinal":true,"errorCode":0}"#,
            ),
            (
                "ev-3",
                "dev-a",
                "history.upsert",
                "r-tr-1",
                r#"{"id":"r-tr-1","sourceDeviceId":"dev-a","timestamp":"2026-11-24T02:30:00Z","text":"1234567890","durationMs":0,"aiCorrectionApplied":true,"llmInvoked":true,"mode":"translate_read","isFinal":true,"errorCode":0}"#,
            ),
            (
                "ev-4",
                "dev-a",
                "history.upsert",
                "r-deleted",
                r#"{"id":"r-deleted","sourceDeviceId":"dev-a","timestamp":"2026-11-24T03:00:00Z","text":"gone","durationMs":500,"aiCorrectionApplied":false,"llmInvoked":false,"mode":"assistant_hands_free","isFinal":true,"errorCode":0}"#,
            ),
            (
                "ev-5",
                "dev-a",
                "history.delete",
                "r-deleted",
                r#"{"recordId":"r-deleted"}"#,
            ),
        ];
        for (eid, dev, typ, rid, payload) in evs {
            conn.execute(
                "INSERT INTO events (account_id, event_id, device_id, type, record_id, payload, created_at)
                 VALUES ('acc-1', ?1, ?2, ?3, ?4, ?5, '2026-11-24T03:00:00Z')",
                params![eid, dev, typ, rid, payload],
            )
            .unwrap();
        }
    }

    fn create_v1_device_fixture(path: &Path, dev_id: &str) -> Connection {
        let conn = Connection::open(path).unwrap();
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
            CREATE TABLE sync_outbox (
                event_id TEXT PRIMARY KEY, event_type TEXT NOT NULL, record_id TEXT,
                payload TEXT NOT NULL, created_at TEXT NOT NULL
            );
            "#,
        )
        .unwrap();
        crate::post_edit::records::create_schema(&conn).unwrap();
        conn.execute(
            "INSERT INTO user_config (key, value) VALUES ('sync_device_id', ?1)",
            params![dev_id],
        )
        .unwrap();
        conn
    }

    #[test]
    fn migration_m1_m2_m4_m5_c2_preserves_baselines_blocks_private_replays_device_and_redacts_report()
    {
        let dir = TempDir::new().unwrap();
        let srv_v1 = dir.path().join("srv-v1.db");
        let srv_v2 = dir.path().join("srv-v2.db");
        let dev_a_in = dir.path().join("dev-a-v1.db");
        let dev_a_out = dir.path().join("dev-a-v2.db");
        let dev_b_in = dir.path().join("dev-b-v1.db");
        let dev_b_out = dir.path().join("dev-b-v2.db");

        create_v1_server_fixture(&srv_v1);

        // Device A: history+edits+import_edits; has +5 baseline dictations (+1 translate_read in v1 local),
        // 1 unseen private record (must NOT leak to server), 1 pending outbox record (MUST publish),
        // and 1 shared record with a manual edit. Notice r-server-only is NOT in dev-a-v1!
        {
            let ca = create_v1_device_fixture(&dev_a_in, "dev-a");
            // Local raw = 10 dictations (3 server valid + 1 outbox + 1 private + 4 early pruned + 1 translate_read)
            // Local chars = 60 (10+10+4+6+5 + 15 pruned + 10 translate_read)
            ca.execute(
                "INSERT INTO device_usage_stats (device_id, total_recording_count, total_characters, total_duration_ms, llm_correction_count)
                 VALUES ('dev-a', 10, 60, 9000, 4)",
                [],
            )
            .unwrap();
            ca.execute(
                "INSERT INTO history_record (id, timestamp, text, mode, duration_ms, source_device_id, edited_text, edit_source, edited_at)
                 VALUES ('r-shared-1', '2026-11-24T01:00:00Z', '1234567890', 'assistant_hands_free', 1000, 'dev-a', 'edited-shared', 'manual', '2026-11-24T01:05:00Z')",
                [],
            )
            .unwrap();
            ca.execute(
                "INSERT INTO history_record (id, timestamp, text, mode, duration_ms, source_device_id, edited_text, edit_source, edited_at)
                 VALUES ('r-local-private', '2026-11-24T04:00:00Z', 'secret', 'assistant_hands_free', 500, 'dev-a', 'private-edit', 'manual', '2026-11-24T04:01:00Z')",
                [],
            )
            .unwrap();
            ca.execute(
                "INSERT INTO history_record (id, timestamp, text, mode, duration_ms, source_device_id)
                 VALUES ('r-outbox-pending', '2026-11-24T05:00:00Z', 'queued', 'assistant_hands_free', 600, 'dev-a')",
                [],
            )
            .unwrap();
            ca.execute(
                "INSERT INTO sync_outbox (event_id, event_type, record_id, payload, created_at)
                 VALUES ('ev-local-1', 'history.upsert', 'r-outbox-pending', '{}', '2026-11-24T05:00:00Z')",
                [],
            )
            .unwrap();
        }

        // Device B: stats_only; has a local record with an edit (must NOT leak create or edit to server,
        // must keep history_cursor = 0, edits_cursor = 0, and must preserve local edit!)
        {
            let cb = create_v1_device_fixture(&dev_b_in, "dev-b");
            cb.execute(
                "INSERT INTO device_usage_stats (device_id, total_recording_count, total_characters, total_duration_ms, llm_correction_count)
                 VALUES ('dev-b', 1, 7, 700, 0)",
                [],
            )
            .unwrap();
            cb.execute(
                "INSERT INTO history_record (id, timestamp, text, mode, duration_ms, source_device_id, edited_text, edit_source, edited_at)
                 VALUES ('r-b-private', '2026-11-24T06:00:00Z', 'winpriv', 'assistant_hands_free', 700, 'dev-b', 'win-edit', 'manual', '2026-11-24T06:01:00Z')",
                [],
            )
            .unwrap();
        }

        let outcome = run_migration(&MigrationConfig {
            server_v1_db: srv_v1,
            out_server_v2_db: srv_v2.clone(),
            device_inputs: vec![
                DeviceMigrationInput {
                    db_path: dev_a_in,
                    out_db_path: dev_a_out.clone(),
                    scope_history: true,
                    scope_edits: true,
                    import_existing_edits: true,
                },
                DeviceMigrationInput {
                    db_path: dev_b_in,
                    out_db_path: dev_b_out.clone(),
                    scope_history: false,
                    scope_edits: false,
                    import_existing_edits: false,
                },
            ],
            server_url: "http://127.0.0.1:8787".into(),
            account_label: "test".into(),
            recovery_key: None,
        })
        .unwrap();

        // C2: Serialized report must NOT contain recoveryKey or VXK2-
        let report_json = serde_json::to_string(&outcome.report).unwrap();
        assert!(!report_json.contains("recoveryKey"));
        assert!(!report_json.contains("VXK2-"));

        // M2: Server must have ONLY r-shared-1, r-server-only, r-outbox-pending (3 surviving shared),
        // NEVER r-local-private or r-b-private!
        assert_eq!(outcome.report.surviving_shared_history_count, 3);
        assert_eq!(outcome.report.excluded_private_local_only_count, 2);
        assert_eq!(outcome.report.imported_edits_count, 1); // only r-shared-1, NOT r-local-private or r-b-private
        let srv_conn = open_ro(&srv_v2).unwrap();
        let leaked_private: i64 = srv_conn
            .query_row(
                "SELECT COUNT(*) FROM envelopes WHERE object_id IN ('r-local-private', 'r-b-private')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(leaked_private, 0);

        // M1: Device A raw=10 minus 1 translate_read = 9 adopted dictations (60 - 10 = 50 chars)
        let dev_a_rep = outcome
            .report
            .devices
            .iter()
            .find(|d| d.device_id == "dev-a")
            .unwrap();
        assert_eq!(dev_a_rep.excluded_translate_read_count, 1);
        assert_eq!(dev_a_rep.migrated_all_time_dictations, 9);
        assert_eq!(dev_a_rep.migrated_characters, 50);
        assert_eq!(
            dev_a_rep.baseline_decision,
            "adopted_local_baseline_minus_translate_read"
        );

        // Verify replay matches adopted totals (dev-a: 9 + dev-b: 1 = 10)
        let replay = verify_server_v2_replay(&srv_v2, &outcome.recovery_key).unwrap();
        assert_eq!(replay.replayed_history_count, 3);
        assert_eq!(replay.replayed_total_recordings, 10);
        assert_eq!(replay.deleted_ids_resurrected, 0);
        assert_eq!(replay.translate_read_leaked, 0);

        // M4 & M5 on Device A output DB:
        let ca_out = open_ro(&dev_a_out).unwrap();
        // Must have replayed r-server-only, plus kept r-shared-1, r-outbox-pending, and r-local-private!
        let has_srv_only: bool = ca_out
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM history_record WHERE id = 'r-server-only' AND sync_eligible = 1)",
                [],
                |r| Ok(r.get::<_, i32>(0)? != 0),
            )
            .unwrap();
        assert!(has_srv_only);
        // r-shared-1 edit version must exist with is_shared = 1, and r-local-private edit with is_shared = 0
        let shared_ver_cnt: i64 = ca_out
            .query_row(
                "SELECT COUNT(*) FROM history_edit_versions WHERE record_id = 'r-shared-1' AND is_shared = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(shared_ver_cnt, 1);
        let priv_ver: (String, i32) = ca_out
            .query_row(
                "SELECT final_text, is_shared FROM history_edit_versions WHERE record_id = 'r-local-private'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(priv_ver, ("private-edit".to_string(), 0));
        // Device A Overview Local & All match M1 adopted totals
        let a_local = get_overview_usage_stats_in(&ca_out, "dev-a", OverviewScope::Local).unwrap();
        let a_all = get_overview_usage_stats_in(&ca_out, "dev-a", OverviewScope::All).unwrap();
        assert_eq!(a_local.total_recording_count, 9);
        assert_eq!(a_all.total_recording_count, 10);

        // M4 & M5 on Device B (stats_only) output DB:
        let cb_out = open_ro(&dev_b_out).unwrap();
        let (b_hist_cur, b_edit_cur): (i64, i64) = cb_out
            .query_row(
                "SELECT history_cursor, edits_cursor FROM sync_spaces LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((b_hist_cur, b_edit_cur), (0, 0));
        let b_edit: (String, i32) = cb_out
            .query_row(
                "SELECT final_text, is_shared FROM history_edit_versions WHERE record_id = 'r-b-private'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(b_edit, ("win-edit".to_string(), 0));
    }

    #[test]
    fn forward_repair_is_idempotent_and_restores_baseline_edits_and_retention_history() {
        let dir = TempDir::new().unwrap();
        let srv_v1 = dir.path().join("srv-v1.db");
        let srv_v2 = dir.path().join("srv-v2.db");
        let dev_a_v1 = dir.path().join("dev-a-v1.db");
        let dev_a_init_v2 = dir.path().join("dev-a-init-v2.db");
        let dev_a_live = dir.path().join("dev-a-live.db");
        let dev_a_run1 = dir.path().join("dev-a-run1.db");
        let dev_a_run2 = dir.path().join("dev-a-run2.db");
        let key_file = dir.path().join("recovery-key.secret");

        create_v1_server_fixture(&srv_v1);
        let recent_ts = (Utc::now() - chrono::Duration::days(10)).to_rfc3339();
        {
            // Refresh timestamps of r-shared-1 and r-server-only so they fall inside 180d retention,
            // and add an ancient >180d record that MUST NOT be backfilled.
            let s1 = Connection::open(&srv_v1).unwrap();
            let recent_shared = format!(
                r#"{{"id":"r-shared-1","sourceDeviceId":"dev-a","timestamp":"{recent_ts}","text":"1234567890","durationMs":1000,"aiCorrectionApplied":true,"llmInvoked":true,"mode":"assistant_hands_free","isFinal":true,"errorCode":0}}"#
            );
            let recent_srv_only = format!(
                r#"{{"id":"r-server-only","sourceDeviceId":"dev-a","timestamp":"{recent_ts}","text":"abcdefghij","durationMs":2000,"aiCorrectionApplied":false,"llmInvoked":true,"mode":"assistant_hands_free","isFinal":true,"errorCode":0}}"#
            );
            s1.execute(
                "UPDATE events SET payload = ?1 WHERE event_id = 'ev-1'",
                params![recent_shared],
            )
            .unwrap();
            s1.execute(
                "UPDATE events SET payload = ?1 WHERE event_id = 'ev-2'",
                params![recent_srv_only],
            )
            .unwrap();
            s1.execute(
                "INSERT INTO events (account_id, event_id, device_id, type, record_id, payload, created_at)
                 VALUES ('acc-1', 'ev-ancient', 'dev-a', 'history.upsert', 'r-ancient',
                 '{\"id\":\"r-ancient\",\"sourceDeviceId\":\"dev-a\",\"timestamp\":\"2020-01-01T00:00:00Z\",\"text\":\"old\",\"durationMs\":300,\"aiCorrectionApplied\":false,\"llmInvoked\":false,\"mode\":\"assistant_hands_free\",\"isFinal\":true,\"errorCode\":0}',
                 '2020-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
        }
        {
            let ca = create_v1_device_fixture(&dev_a_v1, "dev-a");
            // Local v1 raw = 10 recs, 60 chars, 9000 ms, 4 llm (includes 1 translate_read: 1 rec, 10 chars, 0 ms, 1 llm)
            // => same-metric net v1 target = 9 recs, 50 chars, 9000 ms, 3 llm.
            ca.execute(
                "INSERT INTO device_usage_stats (device_id, total_recording_count, total_characters, total_duration_ms, llm_correction_count)
                 VALUES ('dev-a', 10, 60, 9000, 4)",
                [],
            )
            .unwrap();
            ca.execute(
                "INSERT INTO user_config (key, value) VALUES ('app_settings', '{\"textRetentionDays\":180}')",
                [],
            )
            .unwrap();
            ca.execute(
                "INSERT INTO history_record (id, timestamp, text, mode, duration_ms, source_device_id, edited_text, edit_source, edited_at)
                 VALUES ('r-shared-1', ?1, '1234567890', 'assistant_hands_free', 1000, 'dev-a', 'edited-shared', 'manual', ?1)",
                params![recent_ts],
            )
            .unwrap();
            ca.execute(
                "INSERT INTO history_record (id, timestamp, text, mode, duration_ms, source_device_id, edited_text, edit_source, edited_at)
                 VALUES ('r-local-private', ?1, 'secret', 'assistant_hands_free', 500, 'dev-a', 'private-edit', 'manual', ?1)",
                params![recent_ts],
            )
            .unwrap();
        }

        let outcome = run_migration(&MigrationConfig {
            server_v1_db: srv_v1.clone(),
            out_server_v2_db: srv_v2.clone(),
            device_inputs: vec![DeviceMigrationInput {
                db_path: dev_a_v1.clone(),
                out_db_path: dev_a_init_v2.clone(),
                scope_history: true,
                scope_edits: true,
                import_existing_edits: true,
            }],
            server_url: "http://127.0.0.1:8787".into(),
            account_label: "test".into(),
            recovery_key: None,
        })
        .unwrap();
        std::fs::write(&key_file, &outcome.recovery_key).unwrap();

        // Simulate pre-fix cutover state on initial_v2 (dropped baseline by 3 recs / 20 chars / 4000 ms / 1 llm)
        // and simulate live device having +2 post-cutover dictations, missing edit versions, missing r-server-only,
        // and stale sync_devices.last_synced_at.
        {
            let init_conn = Connection::open(&dev_a_init_v2).unwrap();
            init_conn
                .execute(
                    "UPDATE device_usage_stats
                     SET total_recording_count = 6, total_characters = 30, total_duration_ms = 5000, llm_correction_count = 2
                     WHERE device_id = 'dev-a'",
                    [],
                )
                .unwrap();
        }
        std::fs::copy(&dev_a_init_v2, &dev_a_live).unwrap();
        {
            let live = Connection::open(&dev_a_live).unwrap();
            live.execute(
                "UPDATE device_usage_stats
                 SET total_recording_count = 8, total_characters = 42, total_duration_ms = 6200, llm_correction_count = 3
                 WHERE device_id = 'dev-a'",
                [],
            )
            .unwrap();
            live.execute(
                "UPDATE sync_stats_shards SET local_revision = 5, acked_revision = 5
                 WHERE device_id = 'dev-a' AND shard_key = 'totals'",
                [],
            )
            .unwrap();
            live.execute("DELETE FROM history_edit_versions", []).unwrap();
            live.execute(
                "UPDATE history_record SET active_edit_id = NULL WHERE id IN ('r-shared-1', 'r-local-private')",
                [],
            )
            .unwrap();
            live.execute("DELETE FROM history_record WHERE id = 'r-server-only'", [])
                .unwrap();
            live.execute(
                "UPDATE sync_devices SET last_synced_at = '2099-01-01T00:00:00Z' WHERE device_id = 'dev-b'",
                [],
            )
            .unwrap();
        }

        // Run 1: must apply exact net baseline delta (+3 recs, +20 chars, +4000 ms, +1 llm),
        // preserve post-cutover dictations (8+3=11, 42+20=62, 6200+4000=10200, 3+1=4),
        // bump totals revision (5 -> 6), restore r-shared-1 edit version (is_shared=1),
        // seed r-local-private edit (is_shared=0), backfill r-server-only, and skip r-ancient.
        let r1 = run_forward_repair(&ForwardRepairConfig {
            server_v2_db: srv_v2.clone(),
            in_device_db: dev_a_live.clone(),
            out_device_db: dev_a_run1.clone(),
            recovery_key_file: key_file.clone(),
            v1_device_db: Some(dev_a_v1.clone()),
            v1_server_db: Some(srv_v1.clone()),
            initial_v2_device_db: Some(dev_a_init_v2.clone()),
            replay_history_within_retention: true,
        })
        .unwrap();

        assert!(!r1.baseline_repair_already_done);
        assert_eq!(r1.baseline_delta_recordings, 3);
        assert_eq!(r1.baseline_delta_characters, 20);
        assert_eq!(r1.baseline_delta_duration_ms, 4000);
        assert_eq!(r1.baseline_delta_llm_invoked, 1);
        assert_eq!(r1.after.local_recording_count, 11);
        assert_eq!(r1.after.local_characters, 62);
        assert_eq!(r1.after.local_duration_ms, 10200);
        assert_eq!(r1.after.local_llm_invoked, 4);
        assert_eq!(r1.after.totals_local_revision, 6);
        assert_eq!(r1.after.totals_acked_revision, 5);
        assert_eq!(r1.history_records_backfilled, 1);
        assert_eq!(r1.legacy_private_edits_seeded, 1);
        assert_eq!(r1.after.edit_versions_count, 2);
        assert_eq!(r1.after.edit_conflict_count, 0);
        assert_eq!(r1.deleted_ids_resurrected, 0);
        assert_eq!(r1.translate_read_leaked, 0);

        let c1 = open_ro(&dev_a_run1).unwrap();
        let has_srv_only: bool = c1
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM history_record WHERE id = 'r-server-only')",
                [],
                |r| Ok(r.get::<_, i32>(0)? != 0),
            )
            .unwrap();
        let has_ancient: bool = c1
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM history_record WHERE id = 'r-ancient')",
                [],
                |r| Ok(r.get::<_, i32>(0)? != 0),
            )
            .unwrap();
        assert!(has_srv_only);
        assert!(!has_ancient);

        // Run 2 (idempotency): zero baseline delta, zero backfill, identical before == after.
        let r2 = run_forward_repair(&ForwardRepairConfig {
            server_v2_db: srv_v2,
            in_device_db: dev_a_run1,
            out_device_db: dev_a_run2,
            recovery_key_file: key_file,
            v1_device_db: Some(dev_a_v1),
            v1_server_db: Some(srv_v1),
            initial_v2_device_db: Some(dev_a_init_v2),
            replay_history_within_retention: true,
        })
        .unwrap();

        assert!(r2.baseline_repair_already_done);
        assert_eq!(r2.baseline_delta_recordings, 0);
        assert_eq!(r2.baseline_delta_characters, 0);
        assert_eq!(r2.baseline_delta_duration_ms, 0);
        assert_eq!(r2.baseline_delta_llm_invoked, 0);
        assert_eq!(r2.history_records_backfilled, 0);
        assert_eq!(r2.legacy_private_edits_seeded, 0);
        assert_eq!(r2.before, r2.after);
        assert_eq!(r2.after, r1.after);
    }
}
