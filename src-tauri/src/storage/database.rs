//! Database operations

use chrono::{Duration, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json;
use std::path::Path;
use std::sync::Mutex;

use crate::commands::settings::AppSettings;
use uuid::Uuid;

static DB: std::sync::OnceLock<Mutex<Connection>> = std::sync::OnceLock::new();

/// Initialize the database
pub fn init_database(path: &Path) -> Result<(), StorageError> {
    let conn = Connection::open(path).map_err(|e| StorageError::ConnectionFailed(e.to_string()))?;

    // Create tables
    conn.execute_batch(r#"
        CREATE TABLE IF NOT EXISTS history_record (
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
            source_device_id TEXT,
            asr_model_name TEXT,
            llm_model_name TEXT
        );

        CREATE TABLE IF NOT EXISTS usage_stats (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            total_duration_ms INTEGER DEFAULT 0,
            total_characters INTEGER DEFAULT 0,
            llm_correction_count INTEGER DEFAULT 0,
            last_updated TEXT
        );

        CREATE TABLE IF NOT EXISTS device_usage_stats (
            device_id TEXT PRIMARY KEY,
            total_duration_ms INTEGER DEFAULT 0,
            total_characters INTEGER DEFAULT 0,
            llm_correction_count INTEGER DEFAULT 0,
            last_updated TEXT
        );

        CREATE TABLE IF NOT EXISTS user_config (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS device_registry (
            device_id TEXT PRIMARY KEY,
            device_name TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS sync_state (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            last_seq INTEGER NOT NULL DEFAULT 0,
            last_sync_at TEXT,
            last_error TEXT,
            status TEXT
        );

        CREATE TABLE IF NOT EXISTS sync_server_state (
            server_id TEXT NOT NULL,
            account_id TEXT NOT NULL,
            last_seq INTEGER NOT NULL DEFAULT 0,
            usage_seq INTEGER NOT NULL DEFAULT 0,
            seeded_at TEXT,
            last_sync_at TEXT,
            PRIMARY KEY (server_id, account_id)
        );

        CREATE TABLE IF NOT EXISTS sync_outbox (
            event_id TEXT PRIMARY KEY,
            event_type TEXT NOT NULL,
            record_id TEXT,
            payload TEXT NOT NULL,
            created_at TEXT NOT NULL
        );

        INSERT OR IGNORE INTO usage_stats (id, total_duration_ms, total_characters, llm_correction_count)
        VALUES (1, 0, 0, 0);

        INSERT OR IGNORE INTO sync_state (id, last_seq, status)
        VALUES (1, 0, 'disabled');
    "#).map_err(|e| StorageError::QueryFailed(e.to_string()))?;

    ensure_column(&conn, "usage_stats", "total_recording_count", "INTEGER DEFAULT 0")?;
    ensure_column(&conn, "device_usage_stats", "total_recording_count", "INTEGER DEFAULT 0")?;

    // Backfill total_recording_count from actual history_record rows.
    // The cached counter may be too low if the column was added after
    // recordings already existed and a few increment calls have since
    // bumped it above 0.  Correct it whenever the actual count is larger.
    {
        let cached: i64 = conn
            .query_row("SELECT total_recording_count FROM usage_stats WHERE id = 1", [], |r| r.get(0))
            .unwrap_or(0);
        let actual: i64 = conn
            .query_row("SELECT COUNT(*) FROM history_record", [], |r| r.get(0))
            .unwrap_or(0);
        if actual > cached {
            conn.execute(
                "UPDATE usage_stats SET total_recording_count = ?1 WHERE id = 1",
                params![actual],
            ).map_err(|e| StorageError::QueryFailed(e.to_string()))?;
        }
    }
    // Per-device usage_stats – recompute total_recording_count from
    // history_record for every cached device row where the stored count
    // is smaller than the actual number of records (i.e. the counter
    // missed older recordings that existed before this column was added).
    {
        let mut stmt = conn.prepare(
            "SELECT device_id, total_recording_count FROM device_usage_stats"
        ).map_err(|e| StorageError::QueryFailed(e.to_string()))?;
        let rows: Vec<(String, i64)> = stmt
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))
            .map_err(|e| StorageError::QueryFailed(e.to_string()))?
            .filter_map(|r| r.ok())
            .collect();
        drop(stmt);
        for (did, cached_count) in rows {
            let actual_count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM history_record
                 WHERE source_device_id = ?1 OR source_device_id IS NULL OR source_device_id = ''",
                params![did],
                |r| r.get(0),
            ).unwrap_or(0);
            if actual_count > cached_count {
                conn.execute(
                    "UPDATE device_usage_stats SET total_recording_count = ?1 WHERE device_id = ?2",
                    params![actual_count, did],
                ).map_err(|e| StorageError::QueryFailed(e.to_string()))?;
            }
        }
    }

    ensure_column(&conn, "history_record", "source_device_id", "TEXT")?;
    ensure_column(&conn, "history_record", "llm_invoked", "INTEGER DEFAULT 0")?;
    ensure_column(&conn, "history_record", "asr_model_name", "TEXT")?;
    ensure_column(&conn, "history_record", "llm_model_name", "TEXT")?;
    ensure_column(&conn, "sync_state", "current_server_id", "TEXT")?;
    ensure_column(&conn, "sync_state", "current_account_id", "TEXT")?;
    ensure_column(&conn, "sync_server_state", "usage_seq", "INTEGER DEFAULT 0")?;
    ensure_column(&conn, "sync_server_state", "seeded_at", "TEXT")?;

    let _ = DB.set(Mutex::new(conn));
    log::info!("Database initialized at {:?}", path);
    Ok(())
}

fn with_db<T, F>(f: F) -> Result<T, StorageError>
where
    F: FnOnce(&Connection) -> Result<T, StorageError>,
{
    let db = DB.get().ok_or(StorageError::NotInitialized)?;
    let conn = db.lock().map_err(|_| StorageError::LockFailed)?;
    f(&conn)
}

/// History record
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryRecord {
    pub id: String,
    pub timestamp: String,
    pub text: String,
    pub original_text: Option<String>,
    pub ai_correction_applied: bool,
    pub llm_invoked: bool,
    pub mode: String,
    pub duration_ms: i64,
    pub audio_path: Option<String>,
    pub is_final: bool,
    pub error_code: i32,
    pub source_device_id: Option<String>,
    pub source_device_name: Option<String>,
    pub asr_model_name: Option<String>,
    pub llm_model_name: Option<String>,
}

fn map_history_record_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<HistoryRecord> {
    Ok(HistoryRecord {
        id: row.get(0)?,
        timestamp: row.get(1)?,
        text: row.get(2)?,
        original_text: row.get(3)?,
        ai_correction_applied: row.get::<_, i32>(4)? != 0,
        llm_invoked: row.get::<_, i32>(5)? != 0,
        mode: row.get(6)?,
        duration_ms: row.get(7)?,
        audio_path: row.get(8)?,
        is_final: row.get::<_, i32>(9)? != 0,
        error_code: row.get(10)?,
        source_device_id: row.get(11)?,
        source_device_name: row.get(12)?,
        asr_model_name: row.get(13)?,
        llm_model_name: row.get(14)?,
    })
}

/// Usage statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageStats {
    pub total_duration_ms: i64,
    pub total_characters: i64,
    pub llm_correction_count: i64,
    #[serde(default)]
    pub total_recording_count: i64,
}

/// Get history records
pub fn get_history(limit: u32, offset: u32) -> Result<Vec<HistoryRecord>, StorageError> {
    with_db(|conn| {
        let mut stmt = conn.prepare(
            "SELECT h.id, h.timestamp, h.text, h.original_text, h.ai_correction_applied, h.llm_invoked, h.mode, h.duration_ms, h.audio_path, h.is_final, h.error_code, h.source_device_id, d.device_name, h.asr_model_name, h.llm_model_name
             FROM history_record h
             LEFT JOIN device_registry d ON d.device_id = h.source_device_id
             ORDER BY h.timestamp DESC 
             LIMIT ?1 OFFSET ?2"
        ).map_err(|e| StorageError::QueryFailed(e.to_string()))?;

        let records = stmt
            .query_map(params![limit, offset], map_history_record_row)
            .map_err(|e| StorageError::QueryFailed(e.to_string()))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(records)
    })
}

pub fn get_history_since(
    limit: u32,
    offset: u32,
    since: &str,
) -> Result<Vec<HistoryRecord>, StorageError> {
    let trimmed = since.trim();
    if trimmed.is_empty() {
        return get_history(limit, offset);
    }
    with_db(|conn| {
        let mut stmt = conn.prepare(
            "SELECT h.id, h.timestamp, h.text, h.original_text, h.ai_correction_applied, h.llm_invoked, h.mode, h.duration_ms, h.audio_path, h.is_final, h.error_code, h.source_device_id, d.device_name, h.asr_model_name, h.llm_model_name
             FROM history_record h
             LEFT JOIN device_registry d ON d.device_id = h.source_device_id
             WHERE h.timestamp > ?3
             ORDER BY h.timestamp DESC
             LIMIT ?1 OFFSET ?2"
        ).map_err(|e| StorageError::QueryFailed(e.to_string()))?;

        let records = stmt
            .query_map(params![limit, offset, trimmed], map_history_record_row)
            .map_err(|e| StorageError::QueryFailed(e.to_string()))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(records)
    })
}

pub fn get_history_for_device_since(
    limit: u32,
    offset: u32,
    device_id: &str,
    since: Option<&str>,
) -> Result<Vec<HistoryRecord>, StorageError> {
    let device_id = device_id.trim();
    let since_value = since.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    });
    if device_id.is_empty() {
        return match since_value.as_deref() {
            Some(value) => get_history_since(limit, offset, value),
            None => get_history(limit, offset),
        };
    }
    with_db(|conn| {
        let records = if let Some(value) = since_value.as_deref() {
            let mut stmt = conn
                .prepare(
                    "SELECT h.id, h.timestamp, h.text, h.original_text, h.ai_correction_applied, h.llm_invoked, h.mode, h.duration_ms, h.audio_path, h.is_final, h.error_code, h.source_device_id, d.device_name, h.asr_model_name, h.llm_model_name
                     FROM history_record h
                     LEFT JOIN device_registry d ON d.device_id = h.source_device_id
                     WHERE h.timestamp > ?3 AND (h.source_device_id = ?4 OR h.source_device_id IS NULL OR h.source_device_id = '')
                     ORDER BY h.timestamp DESC
                     LIMIT ?1 OFFSET ?2",
                )
                .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
            let rows = stmt
                .query_map(
                    params![limit, offset, value, device_id],
                    map_history_record_row,
                )
                .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
            let records: Vec<HistoryRecord> = rows.filter_map(|r| r.ok()).collect();
            records
        } else {
            let mut stmt = conn
                .prepare(
                    "SELECT h.id, h.timestamp, h.text, h.original_text, h.ai_correction_applied, h.llm_invoked, h.mode, h.duration_ms, h.audio_path, h.is_final, h.error_code, h.source_device_id, d.device_name, h.asr_model_name, h.llm_model_name
                     FROM history_record h
                     LEFT JOIN device_registry d ON d.device_id = h.source_device_id
                     WHERE (h.source_device_id = ?3 OR h.source_device_id IS NULL OR h.source_device_id = '')
                     ORDER BY h.timestamp DESC
                     LIMIT ?1 OFFSET ?2",
                )
                .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
            let rows = stmt
                .query_map(params![limit, offset, device_id], map_history_record_row)
                .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
            let records: Vec<HistoryRecord> = rows.filter_map(|r| r.ok()).collect();
            records
        };

        Ok(records)
    })
}

/// Insert a history record and increment aggregate usage stats.
pub fn insert_history_record(record: &HistoryRecord) -> Result<bool, StorageError> {
    insert_history_record_with_stats(record, true)
}

/// Insert a history record with optional stats update.
pub fn insert_history_record_with_stats(
    record: &HistoryRecord,
    update_stats: bool,
) -> Result<bool, StorageError> {
    with_db(|conn| {
        let inserted = conn.execute(
            "INSERT OR IGNORE INTO history_record (
                id, timestamp, text, original_text, ai_correction_applied, llm_invoked, mode, duration_ms, audio_path, is_final, error_code, source_device_id, asr_model_name, llm_model_name
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                record.id,
                record.timestamp,
                record.text,
                record.original_text,
                record.ai_correction_applied,
                record.llm_invoked,
                record.mode,
                record.duration_ms,
                record.audio_path,
                record.is_final,
                record.error_code,
                record.source_device_id,
                record.asr_model_name,
                record.llm_model_name,
            ],
        )
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;

        if inserted == 1 {
            if update_stats {
                increment_usage_stats_conn(
                    conn,
                    record.duration_ms,
                    record.text.chars().count() as i64,
                    record.llm_invoked,
                )?;
            }
            Ok(true)
        } else {
            Ok(false)
        }
    })
}

/// Delete a history record
pub fn delete_history_record(id: &str) -> Result<(), StorageError> {
    delete_history_record_internal(id, true)
}

/// Delete a history record, allowing missing records without warning.
pub fn delete_history_record_allow_missing(id: &str) -> Result<(), StorageError> {
    delete_history_record_internal(id, false)
}

fn delete_history_record_internal(id: &str, warn_on_missing: bool) -> Result<(), StorageError> {
    let audio_path = with_db(|conn| {
        conn.query_row(
            "SELECT audio_path FROM history_record WHERE id = ?1",
            params![id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .map_err(|e| StorageError::QueryFailed(e.to_string()))
    })?
    .flatten();

    if let Some(path) = audio_path.as_deref() {
        remove_audio_file_if_needed(path)?;
    }

    with_db(|conn| {
        let affected = conn
            .execute("DELETE FROM history_record WHERE id = ?1", params![id])
            .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
        if affected == 0 && warn_on_missing {
            log::warn!("History delete: record not found id={}", id);
        }
        Ok(())
    })?;

    Ok(())
}

fn remove_audio_file_if_needed(path: &str) -> Result<(), StorageError> {
    if path.trim().is_empty() {
        return Ok(());
    }

    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(StorageError::FileOpFailed(format!(
            "Failed to delete audio file {}: {}",
            path, err
        ))),
    }
}

/// Get usage statistics
pub fn get_usage_stats() -> Result<UsageStats, StorageError> {
    with_db(|conn| {
        let mut stmt = conn.prepare(
            "SELECT total_duration_ms, total_characters, llm_correction_count, total_recording_count FROM usage_stats WHERE id = 1"
        ).map_err(|e| StorageError::QueryFailed(e.to_string()))?;

        let stats = stmt
            .query_row([], |row| {
                Ok(UsageStats {
                    total_duration_ms: row.get(0)?,
                    total_characters: row.get(1)?,
                    llm_correction_count: row.get(2)?,
                    total_recording_count: row.get(3)?,
                })
            })
            .map_err(|e| StorageError::QueryFailed(e.to_string()))?;

        Ok(stats)
    })
}

/// Get usage stats for the current device based on local history records.
pub fn get_local_usage_stats(device_id: &str) -> Result<UsageStats, StorageError> {
    let trimmed = device_id.trim();
    if trimmed.is_empty() {
        return Ok(UsageStats {
            total_duration_ms: 0,
            total_characters: 0,
            llm_correction_count: 0,
            total_recording_count: 0,
        });
    }

    with_db(|conn| {
        let existing = conn
            .query_row(
                "SELECT total_duration_ms, total_characters, llm_correction_count, total_recording_count
                 FROM device_usage_stats WHERE device_id = ?1",
                params![trimmed],
                |row| {
                    Ok(UsageStats {
                        total_duration_ms: row.get(0)?,
                        total_characters: row.get(1)?,
                        llm_correction_count: row.get(2)?,
                        total_recording_count: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(|e| StorageError::QueryFailed(e.to_string()))?;

        if let Some(stats) = existing {
            return Ok(stats);
        }

        let stats = conn
            .query_row(
                "SELECT
                    COALESCE(SUM(duration_ms), 0),
                    COALESCE(SUM(LENGTH(text)), 0),
                    COALESCE(SUM(llm_invoked), 0),
                    COUNT(*)
                 FROM history_record
                 WHERE source_device_id = ?1 OR source_device_id IS NULL OR source_device_id = ''",
                params![trimmed],
                |row| {
                    Ok(UsageStats {
                        total_duration_ms: row.get(0)?,
                        total_characters: row.get(1)?,
                        llm_correction_count: row.get(2)?,
                        total_recording_count: row.get(3)?,
                    })
                },
            )
            .map_err(|e| StorageError::QueryFailed(e.to_string()))?;

        conn.execute(
            "INSERT INTO device_usage_stats
             (device_id, total_duration_ms, total_characters, llm_correction_count, total_recording_count, last_updated)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                trimmed,
                stats.total_duration_ms.max(0),
                stats.total_characters.max(0),
                stats.llm_correction_count.max(0),
                stats.total_recording_count.max(0),
                Utc::now().to_rfc3339()
            ],
        )
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;

        Ok(stats)
    })
}

pub fn set_usage_stats(stats: &UsageStats) -> Result<(), StorageError> {
    with_db(|conn| {
        // total_recording_count is intentionally excluded from the server-driven
        // overwrite.  The sync server does not yet track this field (it arrives
        // as 0 via #[serde(default)]), so writing it here would corrupt the
        // locally-maintained count.  The field is kept accurate by:
        //   1. one-time backfill from history_record at DB init,
        //   2. increment_usage_stats() on each new recording / sync event.
        // Once the server starts supplying a real count, add it back here.
        conn.execute(
            "UPDATE usage_stats
             SET total_duration_ms = ?1,
                 total_characters = ?2,
                 llm_correction_count = ?3,
                 last_updated = ?4
             WHERE id = 1",
            params![
                stats.total_duration_ms.max(0),
                stats.total_characters.max(0),
                stats.llm_correction_count.max(0),
                Utc::now().to_rfc3339(),
            ],
        )
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
        Ok(())
    })
}

/// Apply retention policies: delete expired text records and prune audio files.
pub fn cleanup_history_retention(
    text_retention_days: u32,
    audio_retention_days: u32,
) -> Result<(), StorageError> {
    let now = Utc::now();
    if text_retention_days > 0 {
        let cutoff = (now - Duration::days(text_retention_days as i64)).to_rfc3339();
        let expired_ids: Vec<String> = with_db(|conn| {
            let mut stmt = conn
                .prepare("SELECT id FROM history_record WHERE timestamp < ?1")
                .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
            let rows = stmt
                .query_map(params![cutoff], |row| row.get::<_, String>(0))
                .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
            Ok(rows.filter_map(|r| r.ok()).collect())
        })?;

        for id in expired_ids {
            if let Err(err) = delete_history_record_allow_missing(&id) {
                log::warn!("Failed to purge expired history record id={}: {}", id, err);
            }
        }
    }

    if audio_retention_days > 0 {
        let cutoff = (now - Duration::days(audio_retention_days as i64)).to_rfc3339();
        let candidates: Vec<(String, String)> = with_db(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT id, audio_path
                     FROM history_record
                     WHERE timestamp < ?1
                       AND audio_path IS NOT NULL
                       AND TRIM(audio_path) != ''",
                )
                .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
            let rows = stmt
                .query_map(params![cutoff], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
            Ok(rows.filter_map(|r| r.ok()).collect())
        })?;

        for (id, path) in candidates {
            if let Err(err) = remove_audio_file_if_needed(&path) {
                log::warn!(
                    "Failed to prune expired audio file id={} path={} err={}",
                    id,
                    path,
                    err
                );
                continue;
            }

            if let Err(err) = with_db(|conn| {
                conn.execute(
                    "UPDATE history_record SET audio_path = NULL WHERE id = ?1",
                    params![&id],
                )
                .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
                Ok(())
            }) {
                log::warn!(
                    "Failed to clear audio path after pruning file id={} path={} err={}",
                    id,
                    path,
                    err
                );
            }
        }
    }

    Ok(())
}

fn increment_usage_stats_conn(
    conn: &Connection,
    duration_ms: i64,
    characters: i64,
    llm_invoked: bool,
) -> Result<(), StorageError> {
    let dur = duration_ms.max(0);
    let chars = characters.max(0);
    let llm = if llm_invoked { 1 } else { 0 };

    conn.execute(
        "UPDATE usage_stats
         SET total_duration_ms = total_duration_ms + ?1,
             total_characters = total_characters + ?2,
             llm_correction_count = llm_correction_count + ?3,
             total_recording_count = total_recording_count + 1,
             last_updated = ?4
         WHERE id = 1",
        params![dur, chars, llm, Utc::now().to_rfc3339()],
    )
    .map_err(|e| StorageError::QueryFailed(e.to_string()))?;

    Ok(())
}

/// Increment usage stats by provided deltas.
pub fn increment_usage_stats(
    duration_ms: i64,
    characters: i64,
    llm_invoked: bool,
) -> Result<(), StorageError> {
    with_db(|conn| increment_usage_stats_conn(conn, duration_ms, characters, llm_invoked))
}

/// Increment per-device usage stats.
pub fn increment_device_usage_stats(
    device_id: &str,
    duration_ms: i64,
    characters: i64,
    llm_invoked: bool,
) -> Result<(), StorageError> {
    let trimmed = device_id.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    let dur = duration_ms.max(0);
    let chars = characters.max(0);
    let llm = if llm_invoked { 1 } else { 0 };
    let now = Utc::now().to_rfc3339();

    with_db(|conn| {
        conn.execute(
            "INSERT INTO device_usage_stats
             (device_id, total_duration_ms, total_characters, llm_correction_count, total_recording_count, last_updated)
             VALUES (?1, ?2, ?3, ?4, 1, ?5)
             ON CONFLICT(device_id) DO UPDATE SET
                 total_duration_ms = total_duration_ms + excluded.total_duration_ms,
                 total_characters = total_characters + excluded.total_characters,
                 llm_correction_count = llm_correction_count + excluded.llm_correction_count,
                 total_recording_count = total_recording_count + 1,
                 last_updated = excluded.last_updated",
            params![trimmed, dur, chars, llm, now],
        )
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
        Ok(())
    })
}

/// Get settings
pub fn get_settings() -> Result<AppSettings, StorageError> {
    with_db(|conn| {
        let value: Option<String> = conn
            .query_row(
                "SELECT value FROM user_config WHERE key = 'app_settings' LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| StorageError::QueryFailed(e.to_string()))?;

        if let Some(json) = value {
            match serde_json::from_str::<AppSettings>(&json) {
                Ok(settings) => Ok(settings),
                Err(e) => {
                    log::warn!("Failed to parse settings, falling back to defaults: {}", e);
                    Ok(AppSettings::default())
                }
            }
        } else {
            Ok(AppSettings::default())
        }
    })
}

/// Save settings
pub fn save_settings(settings: &AppSettings) -> Result<(), StorageError> {
    let payload = serde_json::to_string(settings)
        .map_err(|e| StorageError::SerializeFailed(e.to_string()))?;

    with_db(|conn| {
        conn.execute(
            "INSERT INTO user_config (key, value) VALUES ('app_settings', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![payload],
        )
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
        Ok(())
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncState {
    pub last_seq: i64,
    pub last_sync_at: Option<String>,
    pub last_error: Option<String>,
    pub status: Option<String>,
    pub current_server_id: Option<String>,
    pub current_account_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutboxEvent {
    pub event_id: String,
    pub event_type: String,
    pub record_id: Option<String>,
    pub payload: serde_json::Value,
    pub created_at: String,
}

pub fn get_or_create_device_id() -> Result<String, StorageError> {
    if let Some(value) = get_user_config_value("sync_device_id")? {
        if !value.trim().is_empty() {
            return Ok(value);
        }
    }
    let new_id = Uuid::new_v4().to_string();
    set_user_config_value("sync_device_id", &new_id)?;
    Ok(new_id)
}

pub fn backfill_source_device_id(device_id: &str) -> Result<(), StorageError> {
    if device_id.trim().is_empty() {
        return Ok(());
    }
    with_db(|conn| {
        conn.execute(
            "UPDATE history_record SET source_device_id = ?1 WHERE source_device_id IS NULL OR source_device_id = ''",
            params![device_id],
        )
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
        Ok(())
    })
}

pub fn upsert_device_registry(device_id: &str, device_name: &str) -> Result<(), StorageError> {
    if device_id.trim().is_empty() || device_name.trim().is_empty() {
        return Ok(());
    }
    with_db(|conn| {
        conn.execute(
            "INSERT INTO device_registry (device_id, device_name, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(device_id) DO UPDATE SET device_name = excluded.device_name, updated_at = excluded.updated_at",
            params![device_id, device_name, Utc::now().to_rfc3339()],
        )
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
        Ok(())
    })
}

pub fn get_sync_state() -> Result<SyncState, StorageError> {
    with_db(|conn| {
        let mut stmt = conn
            .prepare("SELECT last_seq, last_sync_at, last_error, status, current_server_id, current_account_id FROM sync_state WHERE id = 1")
            .map_err(|e| StorageError::QueryFailed(e.to_string()))?;

        let state = stmt
            .query_row([], |row| {
                Ok(SyncState {
                    last_seq: row.get(0)?,
                    last_sync_at: row.get(1)?,
                    last_error: row.get(2)?,
                    status: row.get(3)?,
                    current_server_id: row.get(4)?,
                    current_account_id: row.get(5)?,
                })
            })
            .map_err(|e| StorageError::QueryFailed(e.to_string()))?;

        Ok(state)
    })
}

pub fn set_sync_state(state: &SyncState) -> Result<(), StorageError> {
    with_db(|conn| {
        conn.execute(
            "UPDATE sync_state SET last_seq = ?1, last_sync_at = ?2, last_error = ?3, status = ?4, current_server_id = ?5, current_account_id = ?6 WHERE id = 1",
            params![
                state.last_seq,
                state.last_sync_at,
                state.last_error,
                state.status,
                state.current_server_id,
                state.current_account_id,
            ],
        )
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
        Ok(())
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncServerState {
    pub server_id: String,
    pub account_id: String,
    pub last_seq: i64,
    pub usage_seq: i64,
    pub seeded_at: Option<String>,
    pub last_sync_at: Option<String>,
}

pub fn ensure_sync_server_state(
    server_id: &str,
    account_id: &str,
) -> Result<(SyncServerState, bool), StorageError> {
    if server_id.trim().is_empty() || account_id.trim().is_empty() {
        return Err(StorageError::QueryFailed(
            "server_id/account_id required".to_string(),
        ));
    }

    let existing = with_db(|conn| {
        conn.query_row(
            "SELECT server_id, account_id, last_seq, usage_seq, seeded_at, last_sync_at FROM sync_server_state WHERE server_id = ?1 AND account_id = ?2",
            params![server_id, account_id],
            |row| {
                Ok(SyncServerState {
                    server_id: row.get(0)?,
                    account_id: row.get(1)?,
                    last_seq: row.get(2)?,
                    usage_seq: row.get(3)?,
                    seeded_at: row.get(4)?,
                    last_sync_at: row.get(5)?,
                })
            },
        )
        .optional()
        .map_err(|e| StorageError::QueryFailed(e.to_string()))
    })?;

    if let Some(state) = existing {
        return Ok((state, false));
    }

    with_db(|conn| {
        conn.execute(
            "INSERT INTO sync_server_state (server_id, account_id, last_seq, usage_seq, seeded_at, last_sync_at) VALUES (?1, ?2, 0, 0, NULL, NULL)",
            params![server_id, account_id],
        )
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
        Ok(())
    })?;

    Ok((
        SyncServerState {
            server_id: server_id.to_string(),
            account_id: account_id.to_string(),
            last_seq: 0,
            usage_seq: 0,
            seeded_at: None,
            last_sync_at: None,
        },
        true,
    ))
}

pub fn set_sync_server_state(state: &SyncServerState) -> Result<(), StorageError> {
    with_db(|conn| {
        conn.execute(
            "INSERT INTO sync_server_state (server_id, account_id, last_seq, usage_seq, seeded_at, last_sync_at)\n             VALUES (?1, ?2, ?3, ?4, ?5, ?6)\n             ON CONFLICT(server_id, account_id) DO UPDATE SET last_seq = excluded.last_seq, usage_seq = excluded.usage_seq, seeded_at = excluded.seeded_at, last_sync_at = excluded.last_sync_at",
            params![
                state.server_id,
                state.account_id,
                state.last_seq,
                state.usage_seq,
                state.seeded_at,
                state.last_sync_at
            ],
        )
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
        Ok(())
    })
}

pub fn set_current_sync_target(
    server_id: &str,
    account_id: &str,
    last_seq: i64,
) -> Result<(), StorageError> {
    let mut state = get_sync_state()?;
    state.current_server_id = Some(server_id.to_string());
    state.current_account_id = Some(account_id.to_string());
    state.last_seq = last_seq;
    set_sync_state(&state)
}

pub fn enqueue_outbox_event(
    event_id: &str,
    event_type: &str,
    record_id: Option<&str>,
    payload: &serde_json::Value,
) -> Result<(), StorageError> {
    let payload_str =
        serde_json::to_string(payload).map_err(|e| StorageError::SerializeFailed(e.to_string()))?;
    let created_at = Utc::now().to_rfc3339();

    with_db(|conn| {
        conn.execute(
            "INSERT OR IGNORE INTO sync_outbox (event_id, event_type, record_id, payload, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![event_id, event_type, record_id, payload_str, created_at],
        )
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
        Ok(())
    })
}

pub fn get_outbox_events(limit: u32) -> Result<Vec<OutboxEvent>, StorageError> {
    with_db(|conn| {
        let mut stmt = conn
            .prepare(
                "SELECT event_id, event_type, record_id, payload, created_at
                 FROM sync_outbox
                 ORDER BY created_at ASC
                 LIMIT ?1",
            )
            .map_err(|e| StorageError::QueryFailed(e.to_string()))?;

        let rows = stmt
            .query_map(params![limit], |row| {
                let payload_str: String = row.get(3)?;
                let payload: serde_json::Value =
                    serde_json::from_str(&payload_str).unwrap_or(serde_json::Value::Null);
                Ok(OutboxEvent {
                    event_id: row.get(0)?,
                    event_type: row.get(1)?,
                    record_id: row.get(2)?,
                    payload,
                    created_at: row.get(4)?,
                })
            })
            .map_err(|e| StorageError::QueryFailed(e.to_string()))?;

        Ok(rows.filter_map(|r| r.ok()).collect())
    })
}

pub fn delete_outbox_events(event_ids: &[String]) -> Result<(), StorageError> {
    if event_ids.is_empty() {
        return Ok(());
    }
    with_db(|conn| {
        let placeholders = std::iter::repeat("?")
            .take(event_ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "DELETE FROM sync_outbox WHERE event_id IN ({})",
            placeholders
        );

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
        let params: Vec<&str> = event_ids.iter().map(|s| s.as_str()).collect();
        stmt.execute(rusqlite::params_from_iter(params))
            .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
        Ok(())
    })
}

pub fn delete_outbox_upsert_for_record(record_id: &str) -> Result<(), StorageError> {
    let trimmed = record_id.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    with_db(|conn| {
        conn.execute(
            "DELETE FROM sync_outbox WHERE event_type = 'history.upsert' AND record_id = ?1",
            params![trimmed],
        )
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
        Ok(())
    })
}

fn get_user_config_value(key: &str) -> Result<Option<String>, StorageError> {
    with_db(|conn| {
        conn.query_row(
            "SELECT value FROM user_config WHERE key = ?1 LIMIT 1",
            params![key],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| StorageError::QueryFailed(e.to_string()))
    })
}

fn set_user_config_value(key: &str, value: &str) -> Result<(), StorageError> {
    with_db(|conn| {
        conn.execute(
            "INSERT INTO user_config (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
        Ok(())
    })
}

fn ensure_column(
    conn: &Connection,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<(), StorageError> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({})", table))
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;

    for row in rows {
        if let Ok(name) = row {
            if name == column {
                return Ok(());
            }
        }
    }

    conn.execute(
        &format!("ALTER TABLE {} ADD COLUMN {} {}", table, column, definition),
        [],
    )
    .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("Database not initialized")]
    NotInitialized,

    #[error("Failed to acquire database lock")]
    LockFailed,

    #[error("Connection failed: {0}")]
    ConnectionFailed(String),

    #[error("Query failed: {0}")]
    QueryFailed(String),

    #[error("Failed to serialize data: {0}")]
    SerializeFailed(String),

    #[error("Failed to parse stored data: {0}")]
    DeserializeFailed(String),

    #[error("File operation failed: {0}")]
    FileOpFailed(String),
}
