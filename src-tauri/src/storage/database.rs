//! Database operations

use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json;
use std::cmp::Reverse;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};

use crate::commands::settings::AppSettings;
use crate::foreground_app::RecentTargetApp;
use crate::services::history_service::{
    HISTORY_ERROR_NONE, HISTORY_MODE_TRANSLATE_PREFIX, HISTORY_MODE_TRANSLATE_READ,
};
use uuid::Uuid;

static DB: std::sync::OnceLock<Mutex<Connection>> = std::sync::OnceLock::new();
static SETTINGS_CACHE: SettingsCache = SettingsCache::new();
const RECENT_TARGET_APPS_KEY: &str = "recent_target_apps";

/// Write-through in-memory cache for [`AppSettings`].
///
/// Hot-path readers acquire only a shared read lock and avoid touching the
/// SQLite mutex, parsing JSON, or running migrations. Cache misses and writes
/// both run under the SQLite `DB` mutex so a slow read can never overwrite a
/// newer `save_settings`.
struct SettingsCache {
    slot: RwLock<Option<Arc<AppSettings>>>,
}

impl SettingsCache {
    const fn new() -> Self {
        Self {
            slot: RwLock::new(None),
        }
    }

    fn get(&self) -> Option<Arc<AppSettings>> {
        self.slot
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(Arc::clone)
    }

    fn set(&self, settings: Option<Arc<AppSettings>>) {
        *self.slot.write().unwrap_or_else(|e| e.into_inner()) = settings;
    }

    fn get_or_load_with<E>(
        &self,
        load: impl FnOnce() -> Result<AppSettings, E>,
    ) -> Result<Arc<AppSettings>, E> {
        if let Some(cached) = self.get() {
            return Ok(cached);
        }
        let loaded = Arc::new(load()?);
        self.set(Some(Arc::clone(&loaded)));
        Ok(loaded)
    }

    fn write_through<E>(
        &self,
        settings: AppSettings,
        persist: impl FnOnce() -> Result<(), E>,
    ) -> Result<(), E> {
        persist()?;
        self.set(Some(Arc::new(settings)));
        Ok(())
    }
}
const MAX_RECENT_TARGET_APPS: usize = 5;
const DAILY_USAGE_BACKFILL_KEY: &str = "daily_usage_backfilled_v1";
pub const FEATURE_KIND_DICTATION: &str = "dictation";
pub const UNTRACKED_APP_KEY: &str = "__untracked__";

/// Initialize the database
pub fn init_database(path: &Path) -> Result<(), StorageError> {
    let conn = Connection::open(path).map_err(|e| StorageError::ConnectionFailed(e.to_string()))?;
    // History searches use separate readers. WAL lets their snapshots coexist
    // with recording/settings writes instead of holding up the writer.
    let journal: String = conn
        .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
        .map_err(|e| StorageError::ConnectionFailed(e.to_string()))?;
    if journal != "wal" {
        return Err(StorageError::ConnectionFailed(format!(
            "Expected WAL journal mode, got {journal}"
        )));
    }

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
            error_message TEXT,
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

    ensure_column(
        &conn,
        "usage_stats",
        "total_recording_count",
        "INTEGER DEFAULT 0",
    )?;
    ensure_column(
        &conn,
        "device_usage_stats",
        "total_recording_count",
        "INTEGER DEFAULT 0",
    )?;

    // Reading (TTS) counters. Unlike the dictation counters below there is
    // nothing to backfill them from: plain reads leave no history row at all,
    // and translate-and-read rows are deliberately kept out of the counters.
    // They start at zero and count only reads made from here on.
    for table in ["usage_stats", "device_usage_stats"] {
        for column in TTS_COUNTER_COLUMNS {
            ensure_column(&conn, table, column, "INTEGER DEFAULT 0")?;
        }
    }

    ensure_column(&conn, "history_record", "source_device_id", "TEXT")?;
    ensure_column(&conn, "history_record", "llm_invoked", "INTEGER DEFAULT 0")?;
    ensure_column(&conn, "history_record", "error_message", "TEXT")?;
    ensure_column(&conn, "history_record", "asr_model_name", "TEXT")?;
    ensure_column(&conn, "history_record", "llm_model_name", "TEXT")?;
    ensure_column(&conn, "history_record", "edited_text", "TEXT")?;
    ensure_column(&conn, "history_record", "edit_source", "TEXT")?;
    ensure_column(&conn, "history_record", "edited_at", "TEXT")?;
    ensure_column(&conn, "history_record", "target_app_platform", "TEXT")?;
    ensure_column(&conn, "history_record", "target_app_key", "TEXT")?;
    ensure_column(&conn, "history_record", "target_app_name", "TEXT")?;
    ensure_column(&conn, "sync_state", "current_server_id", "TEXT")?;
    ensure_column(&conn, "sync_state", "current_account_id", "TEXT")?;
    ensure_column(&conn, "sync_server_state", "usage_seq", "INTEGER DEFAULT 0")?;
    ensure_column(&conn, "sync_server_state", "seeded_at", "TEXT")?;

    // History pages and retention cleanup all order or filter by timestamp;
    // without this every page is a full scan plus a sort.
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS history_record_time ON history_record(timestamp DESC);
         CREATE TABLE IF NOT EXISTS daily_usage_stats (
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
         CREATE INDEX IF NOT EXISTS daily_usage_stats_date_idx
             ON daily_usage_stats(device_id, feature_kind, local_date);",
    )
    .map_err(|e| StorageError::QueryFailed(e.to_string()))?;

    migrate_recording_counts(&conn)?;
    migrate_daily_usage_stats(&conn)?;

    // Upgrade the persisted settings blob in place (e.g. legacy single custom
    // LLM endpoint -> named endpoints array). Best-effort: never block startup.
    migrate_settings_blob(&conn);

    crate::post_edit::records::create_schema(&conn)?;
    crate::sync_v2::store::init_schema(&conn)?;
    super::history_search::init(path)?;
    if DB.set(Mutex::new(conn)).is_ok() {
        if let Err(err) = get_settings_cached() {
            log::warn!("Could not preload settings cache: {}", err);
        }
    }
    log::info!("Database initialized at {:?}", path);
    Ok(())
}

/// Recording counts are owned locally, independently of the server's usage
/// snapshot/usage_seq (which cover duration, characters and LLM calls only).
/// Keep counted IDs after text deletion so event replay cannot count them again.
fn migrate_recording_counts(conn: &Connection) -> Result<(), StorageError> {
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS counted_recordings (record_id TEXT PRIMARY KEY);")
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
    tx.execute(
        "INSERT OR IGNORE INTO counted_recordings (record_id)
         SELECT id FROM history_record WHERE error_code = ?1 AND mode != ?2",
        params![HISTORY_ERROR_NONE, HISTORY_MODE_TRANSLATE_READ],
    )
    .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
    // Preserve lifetime totals when older history has already been deleted.
    tx.execute(
        "UPDATE usage_stats SET total_recording_count =
             MAX(total_recording_count, (SELECT COUNT(*) FROM counted_recordings))
         WHERE id = 1",
        [],
    )
    .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
    tx.execute(
        "UPDATE device_usage_stats SET total_recording_count = MAX(total_recording_count, (
             SELECT COUNT(*) FROM history_record
             WHERE error_code = ?1 AND mode != ?2
               AND (source_device_id = device_usage_stats.device_id
                    OR source_device_id IS NULL OR source_device_id = '')
         ))",
        params![HISTORY_ERROR_NONE, HISTORY_MODE_TRANSLATE_READ],
    )
    .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
    tx.commit()
        .map_err(|e| StorageError::QueryFailed(e.to_string()))
}

fn count_recording_conn(conn: &Connection, record: &HistoryRecord) -> Result<(), StorageError> {
    if record.error_code != HISTORY_ERROR_NONE || record.mode == HISTORY_MODE_TRANSLATE_READ {
        return Ok(());
    }
    let counted = conn
        .execute(
            "INSERT OR IGNORE INTO counted_recordings (record_id) VALUES (?1)",
            params![record.id],
        )
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
    if counted == 1 {
        conn.execute(
            "UPDATE usage_stats SET total_recording_count = total_recording_count + 1 WHERE id = 1",
            [],
        )
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
    }
    Ok(())
}

/// One-time, idempotent upgrade of the `app_settings` JSON blob at startup.
///
/// Settings are stored as a single JSON document (no per-field columns), so
/// schema "migrations" here are JSON-shape transforms. `get_settings` already
/// applies them on read; doing it once eagerly at startup persists the cleaned
/// form (dropping legacy fields, stabilizing generated ids) instead of leaving
/// it to the next user-triggered save.
///
/// Best-effort by design: any failure is logged and startup proceeds, because
/// `get_settings` still migrates on read as a fallback.
fn migrate_settings_blob(conn: &Connection) {
    let raw_json: Option<String> = match conn
        .query_row(
            "SELECT value FROM user_config WHERE key = 'app_settings' LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()
    {
        Ok(value) => value,
        Err(e) => {
            log::warn!("Settings migration skipped (read failed): {}", e);
            return;
        }
    };

    let Some(json) = raw_json else {
        return; // No settings persisted yet; nothing to migrate.
    };

    let mut value: serde_json::Value = match serde_json::from_str(&json) {
        Ok(value) => value,
        Err(e) => {
            // Leave the blob untouched; get_settings handles the fallback.
            log::warn!("Settings migration skipped (parse failed): {}", e);
            return;
        }
    };

    let migrated_endpoints = crate::commands::settings::migrate_llm_custom_endpoints(&mut value);
    let migrated_gemini_relay =
        crate::commands::settings::migrate_non_official_gemini_endpoint(&mut value);
    let migrated_restore =
        crate::commands::settings::migrate_text_injection_override_restore_flag(&mut value);
    let migrated_asr = crate::commands::settings::migrate_openai_refine_model(&mut value);
    let migrated_hotkeys = crate::commands::settings::migrate_hotkey_digit_key_codes(&mut value);
    let migrated_reasoning =
        crate::commands::settings::migrate_volcengine_minimal_effort(&mut value);
    let migrated_volcengine_model =
        crate::commands::settings::migrate_retired_volcengine_default(&mut value);
    let migrated_model_defaults =
        crate::commands::settings::migrate_scheduled_model_defaults(&mut value);
    let migrated_asr_models = crate::asr::models::migrate_known_model_settings(&mut value);
    let migrated_tts_provider =
        crate::commands::settings::migrate_tts_provider_without_system_voice(&mut value);
    let migrated_hotkey_default = crate::commands::settings::migrate_hotkey_defaults(&mut value);
    if !migrated_endpoints
        && !migrated_gemini_relay
        && !migrated_restore
        && !migrated_asr
        && !migrated_hotkeys
        && !migrated_reasoning
        && !migrated_volcengine_model
        && !migrated_model_defaults
        && !migrated_asr_models
        && !migrated_tts_provider
        && !migrated_hotkey_default
    {
        return;
    }

    // Re-serialize through AppSettings so legacy fields are dropped and the
    // canonical shape is persisted.
    let settings: AppSettings = match serde_json::from_value(value) {
        Ok(settings) => settings,
        Err(e) => {
            log::warn!("Settings migration skipped (deserialize failed): {}", e);
            return;
        }
    };
    let payload = match serde_json::to_string(&settings) {
        Ok(payload) => payload,
        Err(e) => {
            log::warn!("Settings migration skipped (serialize failed): {}", e);
            return;
        }
    };

    match conn.execute(
        "INSERT INTO user_config (key, value) VALUES ('app_settings', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![payload],
    ) {
        Ok(_) => log::info!(
            "Migrated persisted settings (custom LLM endpoints: {}, override restore flag: {}, \
             hotkey digit key codes: {}, Volcengine reasoning: {}, Volcengine model: {}, \
             scheduled model defaults: {}, ASR models: {}, hotkey default: {})",
            migrated_endpoints,
            migrated_restore,
            migrated_hotkeys,
            migrated_reasoning,
            migrated_volcengine_model,
            migrated_model_defaults,
            migrated_asr_models,
            migrated_hotkey_default
        ),
        Err(e) => log::warn!("Settings migration write-back failed: {}", e),
    }
}

pub(crate) fn with_db<T, F>(f: F) -> Result<T, StorageError>
where
    F: FnOnce(&Connection) -> Result<T, StorageError>,
{
    let db = DB.get().ok_or(StorageError::NotInitialized)?;
    // A panic while the lock was held unwinds through any open transaction,
    // which rolls it back, so the connection is still consistent. Refusing it
    // would instead fail every later save until the app restarts.
    let conn = db.lock().unwrap_or_else(|poisoned| {
        log::warn!("Database lock was poisoned by a panic; recovering");
        db.clear_poison();
        poisoned.into_inner()
    });
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
    pub error_message: Option<String>,
    pub source_device_id: Option<String>,
    pub source_device_name: Option<String>,
    pub asr_model_name: Option<String>,
    pub llm_model_name: Option<String>,
    /// The user's final version when it differs from `text`, which stays the
    /// output VoiceX produced. Local only: never part of the sync payload.
    #[serde(default)]
    pub edited_text: Option<String>,
    /// Target application that received the injected text. Local only.
    #[serde(default)]
    pub target_app_platform: Option<String>,
    #[serde(default)]
    pub target_app_key: Option<String>,
    #[serde(default)]
    pub target_app_name: Option<String>,
    /// True when concurrent manual edits created a recoverable alternative branch.
    #[serde(default)]
    pub has_edit_conflict: bool,
}

const HISTORY_RECORD_SELECT_COLUMNS: &str = "h.id, h.timestamp, h.text, h.original_text, h.ai_correction_applied, h.llm_invoked, h.mode, h.duration_ms, h.audio_path, h.is_final, h.error_code, h.error_message, h.source_device_id, d.device_name, h.asr_model_name, h.llm_model_name, h.edited_text, h.target_app_platform, h.target_app_key, h.target_app_name, COALESCE(h.has_edit_conflict, 0)";

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
        error_message: row.get(11)?,
        source_device_id: row.get(12)?,
        source_device_name: row.get(13)?,
        asr_model_name: row.get(14)?,
        llm_model_name: row.get(15)?,
        edited_text: row.get(16)?,
        target_app_platform: row.get(17)?,
        target_app_key: row.get(18)?,
        target_app_name: row.get(19)?,
        has_edit_conflict: row.get::<_, i32>(20)? != 0,
    })
}

/// Column names for the reading counters, in the order every query below
/// reads and writes them.
const TTS_COUNTER_COLUMNS: [&str; 5] = [
    "tts_read_count",
    "tts_translate_count",
    "tts_characters",
    "tts_duration_ms",
    "tts_llm_count",
];

/// Reading (TTS) counters, held as running totals rather than per-read rows.
///
/// Field names match the sync server's `ttsUsage` payload, so this is also the
/// wire type the sync service uploads and reads back.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TtsCounters {
    pub read_count: i64,
    pub translate_count: i64,
    pub characters: i64,
    pub duration_ms: i64,
    pub llm_count: i64,
}

impl TtsCounters {
    /// Read the five counters starting at `offset` in a row that selected
    /// them in `TTS_COUNTER_COLUMNS` order.
    fn from_row(row: &rusqlite::Row, offset: usize) -> rusqlite::Result<Self> {
        Ok(Self {
            read_count: row.get(offset)?,
            translate_count: row.get(offset + 1)?,
            characters: row.get(offset + 2)?,
            duration_ms: row.get(offset + 3)?,
            llm_count: row.get(offset + 4)?,
        })
    }

    fn clamped(self) -> Self {
        Self {
            read_count: self.read_count.max(0),
            translate_count: self.translate_count.max(0),
            characters: self.characters.max(0),
            duration_ms: self.duration_ms.max(0),
            llm_count: self.llm_count.max(0),
        }
    }
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
    /// Absent from the server's `usage` object, which only covers dictation;
    /// the account's reading totals arrive separately as `ttsUsage`.
    #[serde(default)]
    pub tts: TtsCounters,
}

/// Presets for the history browser; failures remain independently discoverable.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryRecordType {
    #[default]
    All,
    Dictation,
    Translation,
    TranslateRead,
    Failed,
}

pub fn search_history(
    limit: u32,
    offset: u32,
    query: &str,
    record_type: HistoryRecordType,
) -> Result<Vec<HistoryRecord>, StorageError> {
    with_db(|conn| search_history_in(conn, limit, offset, query, record_type, None))
}

/// The `WHERE` clause shared by a search's pages and its count, with the
/// values it binds; its placeholders are numbered from `first_param`.
fn history_filter(
    query: &str,
    record_type: HistoryRecordType,
    target_app_key: Option<&str>,
    first_param: usize,
) -> (String, Vec<rusqlite::types::Value>) {
    let mut values: Vec<rusqlite::types::Value> = Vec::new();
    let mut bind = |value: rusqlite::types::Value| {
        values.push(value);
        format!("?{}", first_param + values.len() - 1)
    };
    let ok = format!("h.error_code = {HISTORY_ERROR_NONE}");
    let mut sql = match record_type {
        HistoryRecordType::All => "1 = 1".to_string(),
        HistoryRecordType::Dictation => {
            let translated = bind(format!("{HISTORY_MODE_TRANSLATE_PREFIX}*").into());
            let read = bind(HISTORY_MODE_TRANSLATE_READ.to_string().into());
            format!("{ok} AND h.mode NOT GLOB {translated} AND h.mode != {read}")
        }
        HistoryRecordType::Translation => {
            let translated = bind(format!("{HISTORY_MODE_TRANSLATE_PREFIX}*").into());
            format!("{ok} AND h.mode GLOB {translated}")
        }
        HistoryRecordType::TranslateRead => {
            let read = bind(HISTORY_MODE_TRANSLATE_READ.to_string().into());
            format!("{ok} AND h.mode = {read}")
        }
        HistoryRecordType::Failed => format!("h.error_code != {HISTORY_ERROR_NONE}"),
    };
    if let Some(app_key) = target_app_key.map(str::trim).filter(|s| !s.is_empty()) {
        let p = bind(app_key.to_ascii_lowercase().into());
        sql.push_str(&format!(" AND h.target_app_key = {p}"));
    }
    // Spaces mean AND across keywords; a keyword may occur in any text version.
    // LIKE folds ASCII case, preserves Chinese substrings, and treats user input literally.
    for word in query.split_whitespace() {
        let escaped = word
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let p = bind(format!("%{escaped}%").into());
        sql.push_str(&format!(
            " AND (h.edited_text LIKE {p} ESCAPE '\\' OR h.text LIKE {p} ESCAPE '\\' OR h.original_text LIKE {p} ESCAPE '\\')"
        ));
    }
    (sql, values)
}

pub(super) fn search_history_in(
    conn: &Connection,
    limit: u32,
    offset: u32,
    query: &str,
    record_type: HistoryRecordType,
    target_app_key: Option<&str>,
) -> Result<Vec<HistoryRecord>, StorageError> {
    let (filter, filter_values) = history_filter(query, record_type, target_app_key, 3);
    // The ID makes pagination deterministic even for equal timestamps.
    let sql = format!(
        "SELECT {HISTORY_RECORD_SELECT_COLUMNS}
         FROM history_record h
         LEFT JOIN device_registry d ON d.device_id = h.source_device_id
         WHERE {filter}
         ORDER BY h.timestamp DESC, h.id DESC LIMIT ?1 OFFSET ?2"
    );
    let mut values: Vec<rusqlite::types::Value> = vec![limit.into(), offset.into()];
    values.extend(filter_values);
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(values), map_history_record_row)
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| StorageError::QueryFailed(e.to_string()))
}

pub(super) fn count_history_in(
    conn: &Connection,
    query: &str,
    record_type: HistoryRecordType,
    target_app_key: Option<&str>,
) -> Result<u32, StorageError> {
    let (filter, values) = history_filter(query, record_type, target_app_key, 1);
    conn.query_row(
        &format!("SELECT COUNT(*) FROM history_record h WHERE {filter}"),
        rusqlite::params_from_iter(values),
        |row| row.get(0),
    )
    .map_err(|e| StorageError::QueryFailed(e.to_string()))
}

/// Get history records, newest first, unfiltered.
pub fn get_history(limit: u32, offset: u32) -> Result<Vec<HistoryRecord>, StorageError> {
    search_history(limit, offset, "", HistoryRecordType::All)
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
        let sql = format!(
            "SELECT {HISTORY_RECORD_SELECT_COLUMNS}
             FROM history_record h
             LEFT JOIN device_registry d ON d.device_id = h.source_device_id
             WHERE h.timestamp > ?3
             ORDER BY h.timestamp DESC
             LIMIT ?1 OFFSET ?2"
        );
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| StorageError::QueryFailed(e.to_string()))?;

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
            let sql = format!(
                "SELECT {HISTORY_RECORD_SELECT_COLUMNS}
                 FROM history_record h
                 LEFT JOIN device_registry d ON d.device_id = h.source_device_id
                 WHERE h.timestamp > ?3 AND (h.source_device_id = ?4 OR h.source_device_id IS NULL OR h.source_device_id = '')
                 ORDER BY h.timestamp DESC
                 LIMIT ?1 OFFSET ?2"
            );
            let mut stmt = conn
                .prepare(&sql)
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
            let sql = format!(
                "SELECT {HISTORY_RECORD_SELECT_COLUMNS}
                 FROM history_record h
                 LEFT JOIN device_registry d ON d.device_id = h.source_device_id
                 WHERE (h.source_device_id = ?3 OR h.source_device_id IS NULL OR h.source_device_id = '')
                 ORDER BY h.timestamp DESC
                 LIMIT ?1 OFFSET ?2"
            );
            let mut stmt = conn
                .prepare(&sql)
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
    with_db(|conn| insert_history_record_with_stats_conn(conn, record, update_stats, false))
}

/// Commit a local history record, device totals and daily app usage together.
pub fn insert_local_history_record_with_stats(
    record: &HistoryRecord,
    update_stats: bool,
) -> Result<bool, StorageError> {
    with_db(|conn| insert_history_record_with_stats_conn(conn, record, update_stats, true))
}

fn insert_history_record_with_stats_conn(
    conn: &Connection,
    record: &HistoryRecord,
    update_stats: bool,
    update_local_stats: bool,
) -> Result<bool, StorageError> {
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
    let inserted = tx.execute(
            "INSERT OR IGNORE INTO history_record (
                id, timestamp, text, original_text, ai_correction_applied, llm_invoked, mode, duration_ms, audio_path, is_final, error_code, error_message, source_device_id, asr_model_name, llm_model_name, target_app_platform, target_app_key, target_app_name
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
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
                record.error_message,
                record.source_device_id,
                record.asr_model_name,
                record.llm_model_name,
                record.target_app_platform,
                record.target_app_key,
                record.target_app_name,
            ],
        )
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;

    if inserted == 1 {
        count_recording_conn(&tx, record)?;
        if update_stats {
            increment_usage_stats_conn(
                &tx,
                record.duration_ms,
                record.text.chars().count() as i64,
                record.llm_invoked,
            )?;
        }
        if update_local_stats {
            increment_local_usage_stats_for_record_conn(&tx, record)?;
        }
    }
    tx.commit()
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
    Ok(inserted == 1)
}

/// The `mode` of one history record, or `None` when there is no such row.
pub fn history_record_mode(id: &str) -> Result<Option<String>, StorageError> {
    with_db(|conn| history_record_mode_in(conn, id))
}

fn history_record_mode_in(conn: &Connection, id: &str) -> Result<Option<String>, StorageError> {
    conn.query_row(
        "SELECT mode FROM history_record WHERE id = ?1",
        params![id],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .map_err(|e| StorageError::QueryFailed(e.to_string()))
}

/// Delete a history record
pub fn delete_history_record(id: &str) -> Result<(), StorageError> {
    delete_history_record_internal(id, true)
}

/// Delete a history record, allowing missing records without warning.
pub fn delete_history_record_allow_missing(id: &str) -> Result<(), StorageError> {
    delete_history_record_internal(id, false)
}

/// The text of this device's most recent successful dictation, as last
/// edited. Translate-and-read rows are reads, not dictations.
pub fn latest_dictation_text(device_id: &str) -> Result<Option<String>, StorageError> {
    with_db(|conn| {
        conn.query_row(
            "SELECT COALESCE(NULLIF(edited_text, ''), text)
             FROM history_record
             WHERE error_code = ?1 AND mode != ?2 AND TRIM(text) != ''
               AND (source_device_id = ?3 OR source_device_id IS NULL OR source_device_id = '')
             ORDER BY timestamp DESC
             LIMIT 1",
            params![HISTORY_ERROR_NONE, HISTORY_MODE_TRANSLATE_READ, device_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|e| StorageError::QueryFailed(e.to_string()))
    })
}

/// The recording kept for a history record, if it has one.
pub fn history_audio_path(id: &str) -> Result<Option<String>, StorageError> {
    let path = with_db(|conn| {
        conn.query_row(
            "SELECT audio_path FROM history_record WHERE id = ?1",
            params![id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .map_err(|e| StorageError::QueryFailed(e.to_string()))
    })?
    .flatten();
    Ok(path.filter(|path| !path.trim().is_empty()))
}

fn delete_history_record_internal(id: &str, warn_on_missing: bool) -> Result<(), StorageError> {
    let audio_path = history_audio_path(id)?;

    if let Some(path) = audio_path.as_deref() {
        remove_audio_file_if_needed(path)?;
    }

    with_db(|conn| {
        let tx = conn.unchecked_transaction()
            .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
        crate::post_edit::records::delete_source(&tx, id)?;
        crate::sync_v2::store::on_local_history_deleted_tx(&tx, id, warn_on_missing)?;
        let affected = tx
            .execute("DELETE FROM history_record WHERE id = ?1", params![id])
            .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
        if affected == 0 && warn_on_missing {
            log::warn!("History delete: record not found id={}", id);
        }
        tx.commit().map_err(|e| StorageError::QueryFailed(e.to_string()))
    })?;

    crate::post_edit::records::changed();
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
            "SELECT total_duration_ms, total_characters, llm_correction_count, total_recording_count,
                    tts_read_count, tts_translate_count, tts_characters, tts_duration_ms, tts_llm_count
             FROM usage_stats WHERE id = 1"
        ).map_err(|e| StorageError::QueryFailed(e.to_string()))?;

        let stats = stmt
            .query_row([], |row| {
                Ok(UsageStats {
                    total_duration_ms: row.get(0)?,
                    total_characters: row.get(1)?,
                    llm_correction_count: row.get(2)?,
                    total_recording_count: row.get(3)?,
                    tts: TtsCounters::from_row(row, 4)?,
                })
            })
            .map_err(|e| StorageError::QueryFailed(e.to_string()))?;

        Ok(stats)
    })
}

fn seed_device_usage_stats_if_missing_conn(
    conn: &Connection,
    device_id: &str,
    exclude_record_id: Option<&str>,
) -> Result<Option<UsageStats>, StorageError> {
    let exists: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM device_usage_stats WHERE device_id = ?1 LIMIT 1",
            params![device_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
    if exists.is_some() {
        return Ok(None);
    }

    let stats = conn
        .query_row(
            "SELECT
                COALESCE(SUM(duration_ms), 0),
                COALESCE(SUM(LENGTH(text)), 0),
                COALESCE(SUM(llm_invoked), 0),
                COUNT(*)
             FROM history_record
             WHERE error_code = ?2
               AND mode != ?3
               AND (?4 IS NULL OR id != ?4)
               AND (source_device_id = ?1 OR source_device_id IS NULL OR source_device_id = '')",
            params![
                device_id,
                HISTORY_ERROR_NONE,
                HISTORY_MODE_TRANSLATE_READ,
                exclude_record_id
            ],
            |row| {
                Ok(UsageStats {
                    total_duration_ms: row.get(0)?,
                    total_characters: row.get(1)?,
                    llm_correction_count: row.get(2)?,
                    total_recording_count: row.get(3)?,
                    // No history rows describe reading, so a device row
                    // built this way starts its reading counters at zero.
                    tts: TtsCounters::default(),
                })
            },
        )
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;

    conn.execute(
        "INSERT INTO device_usage_stats
         (device_id, total_duration_ms, total_characters, llm_correction_count, total_recording_count, last_updated)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            device_id,
            stats.total_duration_ms.max(0),
            stats.total_characters.max(0),
            stats.llm_correction_count.max(0),
            stats.total_recording_count.max(0),
            Utc::now().to_rfc3339()
        ],
    )
    .map_err(|e| StorageError::QueryFailed(e.to_string()))?;

    Ok(Some(stats))
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
            tts: TtsCounters::default(),
        });
    }

    with_db(|conn| get_local_usage_stats_conn(conn, trimmed))
}

fn get_local_usage_stats_conn(conn: &Connection, device_id: &str) -> Result<UsageStats, StorageError> {
    let existing = conn
        .query_row(
            "SELECT total_duration_ms, total_characters, llm_correction_count, total_recording_count,
                    tts_read_count, tts_translate_count, tts_characters, tts_duration_ms, tts_llm_count
             FROM device_usage_stats WHERE device_id = ?1",
            params![device_id],
            |row| {
                Ok(UsageStats {
                    total_duration_ms: row.get(0)?,
                    total_characters: row.get(1)?,
                    llm_correction_count: row.get(2)?,
                    total_recording_count: row.get(3)?,
                    tts: TtsCounters::from_row(row, 4)?,
                })
            },
        )
        .optional()
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;

    if let Some(stats) = existing {
        return Ok(stats);
    }

    Ok(seed_device_usage_stats_if_missing_conn(conn, device_id, None)?.unwrap_or(UsageStats {
        total_duration_ms: 0,
        total_characters: 0,
        llm_correction_count: 0,
        total_recording_count: 0,
        tts: TtsCounters::default(),
    }))
}

pub fn set_usage_stats(stats: &UsageStats) -> Result<(), StorageError> {
    with_db(|conn| {
        // total_recording_count is intentionally excluded from the server-driven
        // overwrite.  The sync server does not yet track this field (it arrives
        // as 0 via #[serde(default)]), so writing it here would corrupt the
        // locally-maintained count. Count unique recording IDs at insertion,
        // independently of whether a server snapshot already covers the event.
        // Once the server starts supplying a real count, add it back here.
        //
        // The reading counters are likewise absent from the server's `usage`
        // object -- they arrive as `ttsUsage` and are written by
        // set_account_tts_stats().
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
        let candidates: Vec<(String, String, i32)> = with_db(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT id, audio_path, error_code
                     FROM history_record
                     WHERE timestamp < ?1
                       AND audio_path IS NOT NULL
                       AND TRIM(audio_path) != ''",
                )
                .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
            let rows = stmt
                .query_map(params![cutoff], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i32>(2)?,
                    ))
                })
                .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
            Ok(rows.filter_map(|r| r.ok()).collect())
        })?;

        for (id, path, error_code) in candidates {
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
                continue;
            }

            if error_code != 0 {
                if let Err(err) = delete_history_record_allow_missing(&id) {
                    log::warn!(
                        "Failed to purge expired failed history record id={} after audio prune: {}",
                        id,
                        err
                    );
                }
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

fn increment_device_usage_stats_conn(
    conn: &Connection,
    device_id: &str,
    exclude_record_id: Option<&str>,
    duration_ms: i64,
    characters: i64,
    llm_invoked: bool,
) -> Result<(), StorageError> {
    seed_device_usage_stats_if_missing_conn(conn, device_id, exclude_record_id)?;
    let dur = duration_ms.max(0);
    let chars = characters.max(0);
    let llm = if llm_invoked { 1 } else { 0 };
    let now = Utc::now().to_rfc3339();

    conn.execute(
        "UPDATE device_usage_stats
         SET total_duration_ms = total_duration_ms + ?1,
             total_characters = total_characters + ?2,
             llm_correction_count = llm_correction_count + ?3,
             total_recording_count = total_recording_count + 1,
             last_updated = ?4
         WHERE device_id = ?5",
        params![dur, chars, llm, now, device_id],
    )
    .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
    Ok(())
}

/// Overwrite the account-wide reading counters with the server's totals.
///
/// The server sums every device's last report, so this is an absolute value,
/// not a delta. Only meaningful while sync is on; with sync off the same row
/// is maintained by increment_account_tts_stats().
pub fn set_account_tts_stats(counters: &TtsCounters) -> Result<(), StorageError> {
    let c = counters.clamped();
    with_db(|conn| {
        conn.execute(
            "UPDATE usage_stats
             SET tts_read_count = ?1,
                 tts_translate_count = ?2,
                 tts_characters = ?3,
                 tts_duration_ms = ?4,
                 tts_llm_count = ?5,
                 last_updated = ?6
             WHERE id = 1",
            params![
                c.read_count,
                c.translate_count,
                c.characters,
                c.duration_ms,
                c.llm_count,
                Utc::now().to_rfc3339(),
            ],
        )
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
        Ok(())
    })
}

/// Add one read to the account-wide counters.
///
/// Mirrors increment_usage_stats: call this only when sync is off, otherwise
/// the server owns this row and a local bump would be overwritten anyway.
pub fn increment_account_tts_stats(delta: &TtsCounters) -> Result<(), StorageError> {
    let d = delta.clamped();
    with_db(|conn| {
        conn.execute(
            "UPDATE usage_stats
             SET tts_read_count = tts_read_count + ?1,
                 tts_translate_count = tts_translate_count + ?2,
                 tts_characters = tts_characters + ?3,
                 tts_duration_ms = tts_duration_ms + ?4,
                 tts_llm_count = tts_llm_count + ?5,
                 last_updated = ?6
             WHERE id = 1",
            params![
                d.read_count,
                d.translate_count,
                d.characters,
                d.duration_ms,
                d.llm_count,
                Utc::now().to_rfc3339(),
            ],
        )
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
        Ok(())
    })
}

/// Add one read to this device's counters, and return the device's new totals.
///
/// The device row is created through get_local_usage_stats() rather than by
/// this INSERT alone: that is the only path that backfills the dictation
/// counters from history_record, and a row conjured here would make it skip
/// the backfill and leave those counters stuck at zero.
pub fn increment_device_tts_stats(
    device_id: &str,
    delta: &TtsCounters,
) -> Result<TtsCounters, StorageError> {
    let trimmed = device_id.trim();
    if trimmed.is_empty() {
        return Ok(TtsCounters::default());
    }
    get_local_usage_stats(trimmed)?;

    let d = delta.clamped();
    let now = Utc::now().to_rfc3339();

    with_db(|conn| {
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
        tx.execute(
            "UPDATE device_usage_stats
             SET tts_read_count = tts_read_count + ?1,
                 tts_translate_count = tts_translate_count + ?2,
                 tts_characters = tts_characters + ?3,
                 tts_duration_ms = tts_duration_ms + ?4,
                 tts_llm_count = tts_llm_count + ?5,
                 last_updated = ?6
             WHERE device_id = ?7",
            params![
                d.read_count,
                d.translate_count,
                d.characters,
                d.duration_ms,
                d.llm_count,
                now,
                trimmed,
            ],
        )
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;

        crate::sync_v2::store::on_local_tts_incremented_tx(&tx, trimmed)?;

        let counters = tx
            .query_row(
                "SELECT tts_read_count, tts_translate_count, tts_characters,
                        tts_duration_ms, tts_llm_count
                 FROM device_usage_stats WHERE device_id = ?1",
                params![trimmed],
                |row| TtsCounters::from_row(row, 0),
            )
            .map_err(|e| StorageError::QueryFailed(e.to_string()))?;

        tx.commit()
            .map_err(|e| StorageError::QueryFailed(e.to_string()))?;

        Ok(counters)
    })
}

/// This device's reading totals, as the sync service uploads them.
pub fn get_device_tts_stats(device_id: &str) -> Result<TtsCounters, StorageError> {
    let trimmed = device_id.trim();
    if trimmed.is_empty() {
        return Ok(TtsCounters::default());
    }
    with_db(|conn| {
        let counters = conn
            .query_row(
                "SELECT tts_read_count, tts_translate_count, tts_characters,
                        tts_duration_ms, tts_llm_count
                 FROM device_usage_stats WHERE device_id = ?1",
                params![trimmed],
                |row| TtsCounters::from_row(row, 0),
            )
            .optional()
            .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
        Ok(counters.unwrap_or_default())
    })
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppUsageRange {
    Today,
    Week,
    Month,
    #[default]
    All,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AppUsageStat {
    pub platform: String,
    pub app_key: String,
    pub app_name: String,
    pub usage_count: i64,
    pub total_characters: i64,
    pub total_duration_ms: i64,
    pub llm_count: i64,
    pub last_used_at: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DailyActivityPoint {
    pub local_date: String,
    pub usage_count: i64,
    pub total_characters: i64,
    pub total_duration_ms: i64,
    pub llm_count: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DailyActivityResponse {
    pub start_date: String,
    pub end_date: String,
    pub prior_count: i64,
    pub prior_characters: i64,
    pub prior_duration_ms: i64,
    /// Lifetime usage absent from dated rows; never assign it to a chart date.
    pub undated_count: i64,
    pub undated_characters: i64,
    pub undated_duration_ms: i64,
    pub days: Vec<DailyActivityPoint>,
}

fn local_date_from_rfc3339(timestamp: &str) -> Option<String> {
    DateTime::parse_from_rfc3339(timestamp.trim())
        .ok()
        .map(|dt| dt.with_timezone(&Local).format("%Y-%m-%d").to_string())
}

fn get_or_create_device_id_conn(conn: &Connection) -> Result<String, StorageError> {
    let existing: Option<String> = conn
        .query_row(
            "SELECT value FROM user_config WHERE key = 'sync_device_id' LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
    if let Some(value) = existing.filter(|v| !v.trim().is_empty()) {
        return Ok(value);
    }
    let new_id = Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO user_config (key, value) VALUES ('sync_device_id', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![new_id],
    )
    .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
    Ok(new_id)
}

#[allow(clippy::too_many_arguments)]
fn upsert_daily_usage_stats_conn(
    conn: &Connection,
    device_id: &str,
    local_date: &str,
    feature_kind: &str,
    platform: &str,
    app_key: &str,
    app_name: &str,
    usage_count: i64,
    characters: i64,
    duration_ms: i64,
    llm_count: i64,
    last_used_at: &str,
) -> Result<(), StorageError> {
    conn.execute(
        "INSERT INTO daily_usage_stats (
             device_id, local_date, feature_kind, platform, app_key, app_name,
             usage_count, total_characters, total_duration_ms, llm_count, last_used_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT(device_id, local_date, feature_kind, platform, app_key) DO UPDATE SET
             app_name = CASE
                 WHEN excluded.app_name != '' AND excluded.last_used_at >= daily_usage_stats.last_used_at
                     THEN excluded.app_name
                 WHEN daily_usage_stats.app_name = '' AND excluded.app_name != ''
                     THEN excluded.app_name
                 ELSE daily_usage_stats.app_name
             END,
             usage_count = daily_usage_stats.usage_count + excluded.usage_count,
             total_characters = daily_usage_stats.total_characters + excluded.total_characters,
             total_duration_ms = daily_usage_stats.total_duration_ms + excluded.total_duration_ms,
             llm_count = daily_usage_stats.llm_count + excluded.llm_count,
             last_used_at = CASE
                 WHEN excluded.last_used_at > daily_usage_stats.last_used_at
                     THEN excluded.last_used_at
                 ELSE daily_usage_stats.last_used_at
             END",
        params![
            device_id,
            local_date,
            feature_kind,
            platform,
            app_key,
            app_name,
            usage_count.max(0),
            characters.max(0),
            duration_ms.max(0),
            llm_count.max(0),
            last_used_at,
        ],
    )
    .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
    Ok(())
}

/// One-time backfill of `daily_usage_stats` from surviving local history rows so
/// daily activity charts reflect past usage immediately. Runs once per database.
fn migrate_daily_usage_stats(conn: &Connection) -> Result<(), StorageError> {
    let done: Option<String> = conn
        .query_row(
            "SELECT value FROM user_config WHERE key = ?1 LIMIT 1",
            params![DAILY_USAGE_BACKFILL_KEY],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
    if done.as_deref() == Some("1") {
        return Ok(());
    }

    let local_device_id = get_or_create_device_id_conn(conn)?;
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;

    {
        let mut stmt = tx
            .prepare(
                "SELECT timestamp, text, duration_ms, llm_invoked,
                        target_app_platform, target_app_key, target_app_name
                 FROM history_record
                 WHERE error_code = ?1 AND mode != ?2
                   AND (source_device_id = ?3 OR source_device_id IS NULL OR source_device_id = '')",
            )
            .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
        let rows = stmt
            .query_map(
                params![HISTORY_ERROR_NONE, HISTORY_MODE_TRANSLATE_READ, local_device_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i32>(3)? != 0,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, Option<String>>(6)?,
                    ))
                },
            )
            .map_err(|e| StorageError::QueryFailed(e.to_string()))?;

        for row in rows {
            let (
                timestamp,
                text,
                duration_ms,
                llm_invoked,
                target_platform,
                target_key,
                target_name,
            ) = row.map_err(|e| StorageError::QueryFailed(e.to_string()))?;

            let Some(local_date) = local_date_from_rfc3339(&timestamp) else {
                continue;
            };
            let app_key = target_key
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(UNTRACKED_APP_KEY);
            let platform = if app_key == UNTRACKED_APP_KEY {
                ""
            } else {
                target_platform.as_deref().map(str::trim).unwrap_or("")
            };
            let app_name = if app_key == UNTRACKED_APP_KEY {
                ""
            } else {
                target_name
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .unwrap_or(app_key)
            };

            upsert_daily_usage_stats_conn(
                &tx,
                &local_device_id,
                &local_date,
                FEATURE_KIND_DICTATION,
                platform,
                app_key,
                app_name,
                1,
                text.chars().count() as i64,
                duration_ms,
                i64::from(llm_invoked),
                &timestamp,
            )?;
        }
    }

    tx.execute(
        "INSERT INTO user_config (key, value) VALUES (?1, '1')
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![DAILY_USAGE_BACKFILL_KEY],
    )
    .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
    tx.commit()
        .map_err(|e| StorageError::QueryFailed(e.to_string()))
}

fn increment_local_usage_stats_for_record_conn(
    conn: &Connection,
    record: &HistoryRecord,
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
    let local_date = local_date_from_rfc3339(&record.timestamp).ok_or_else(|| {
        StorageError::QueryFailed("Local history has an invalid timestamp".into())
    })?;
    let app_key = record
        .target_app_key
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(UNTRACKED_APP_KEY);
    let platform = if app_key == UNTRACKED_APP_KEY {
        ""
    } else {
        record
            .target_app_platform
            .as_deref()
            .map(str::trim)
            .unwrap_or("")
    };
    let app_name = if app_key == UNTRACKED_APP_KEY {
        ""
    } else {
        record
            .target_app_name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(app_key)
    };
    let chars = record.text.chars().count() as i64;

    increment_device_usage_stats_conn(
        conn,
        device_id,
        Some(&record.id),
        record.duration_ms,
        chars,
        record.llm_invoked,
    )?;
    upsert_daily_usage_stats_conn(
        conn,
        device_id,
        &local_date,
        FEATURE_KIND_DICTATION,
        platform,
        app_key,
        app_name,
        1,
        chars,
        record.duration_ms,
        i64::from(record.llm_invoked),
        &record.timestamp,
    )?;
    crate::sync_v2::store::on_local_history_inserted_tx(conn, record, &local_date)
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

pub fn get_app_usage_stats(
    device_id: &str,
    range: AppUsageRange,
) -> Result<Vec<AppUsageStat>, StorageError> {
    let trimmed = device_id.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let today = Local::now().date_naive();
    with_db(|conn| get_app_usage_stats_in(conn, trimmed, range, today))
}

fn get_app_usage_stats_in(
    conn: &Connection,
    device_id: &str,
    range: AppUsageRange,
    today: NaiveDate,
) -> Result<Vec<AppUsageStat>, StorageError> {
    let since_date = range_start_date(range, today);
    let mut stmt = if since_date.is_some() {
        conn.prepare(
            "SELECT platform, app_key, app_name, usage_count, total_characters,
                    total_duration_ms, llm_count, last_used_at
             FROM daily_usage_stats
             WHERE device_id = ?1
               AND feature_kind = ?2
               AND app_key != ?3
               AND local_date >= ?4",
        )
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?
    } else {
        conn.prepare(
            "SELECT platform, app_key, app_name, usage_count, total_characters,
                    total_duration_ms, llm_count, last_used_at
             FROM daily_usage_stats
             WHERE device_id = ?1
               AND feature_kind = ?2
               AND app_key != ?3",
        )
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?
    };

    let map_row = |row: &rusqlite::Row<'_>| {
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
    };

    let rows = if let Some(ref since) = since_date {
        stmt.query_map(
            params![device_id, FEATURE_KIND_DICTATION, UNTRACKED_APP_KEY, since],
            map_row,
        )
    } else {
        stmt.query_map(
            params![device_id, FEATURE_KIND_DICTATION, UNTRACKED_APP_KEY],
            map_row,
        )
    }
    .map_err(|e| StorageError::QueryFailed(e.to_string()))?;

    let mut grouped: HashMap<(String, String), AppUsageStat> = HashMap::new();
    for row in rows {
        let (platform, app_key, app_name, count, chars, duration_ms, llm_count, last_used_at) =
            row.map_err(|e| StorageError::QueryFailed(e.to_string()))?;
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

pub fn get_daily_activity(device_id: &str) -> Result<DailyActivityResponse, StorageError> {
    let trimmed = device_id.trim();
    let today = Local::now().date_naive();
    if trimmed.is_empty() {
        let end_week_monday =
            today - Duration::days(i64::from(today.weekday().num_days_from_monday()));
        let start_date = end_week_monday - Duration::weeks(51);
        return Ok(DailyActivityResponse {
            start_date: start_date.format("%Y-%m-%d").to_string(),
            end_date: today.format("%Y-%m-%d").to_string(),
            prior_count: 0,
            prior_characters: 0,
            prior_duration_ms: 0,
            undated_count: 0,
            undated_characters: 0,
            undated_duration_ms: 0,
            days: Vec::new(),
        });
    }
    with_db(|conn| get_daily_activity_in(conn, trimmed, today))
}

fn get_daily_activity_in(
    conn: &Connection,
    device_id: &str,
    today: NaiveDate,
) -> Result<DailyActivityResponse, StorageError> {
    let end_week_monday = today - Duration::days(i64::from(today.weekday().num_days_from_monday()));
    let start_date = end_week_monday - Duration::weeks(51);
    let start_str = start_date.format("%Y-%m-%d").to_string();
    let end_str = today.format("%Y-%m-%d").to_string();

    let (prior_count, prior_characters, prior_duration_ms): (i64, i64, i64) = conn
        .query_row(
            "SELECT COALESCE(SUM(usage_count), 0),
                    COALESCE(SUM(total_characters), 0),
                    COALESCE(SUM(total_duration_ms), 0)
             FROM daily_usage_stats
             WHERE device_id = ?1 AND feature_kind = ?2 AND local_date < ?3",
            params![device_id, FEATURE_KIND_DICTATION, start_str],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;

    let mut stmt = conn
        .prepare(
            "SELECT local_date,
                    SUM(usage_count),
                    SUM(total_characters),
                    SUM(total_duration_ms),
                    SUM(llm_count)
             FROM daily_usage_stats
             WHERE device_id = ?1
               AND feature_kind = ?2
               AND local_date >= ?3
               AND local_date <= ?4
             GROUP BY local_date
             ORDER BY local_date ASC",
        )
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;

    let rows = stmt
        .query_map(
            params![device_id, FEATURE_KIND_DICTATION, start_str, end_str],
            |row| {
                Ok(DailyActivityPoint {
                    local_date: row.get(0)?,
                    usage_count: row.get(1)?,
                    total_characters: row.get(2)?,
                    total_duration_ms: row.get(3)?,
                    llm_count: row.get(4)?,
                })
            },
        )
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;

    let days = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;

    let window_count: i64 = days.iter().map(|d| d.usage_count).sum();
    let window_characters: i64 = days.iter().map(|d| d.total_characters).sum();
    let window_duration_ms: i64 = days.iter().map(|d| d.total_duration_ms).sum();

    // Retention may have removed history from inside or outside this window
    // before backfill. Keep that undated remainder out of the dated baseline.
    let device_totals: Option<(i64, i64, i64)> = conn
        .query_row(
            "SELECT COALESCE(total_recording_count, 0),
                    COALESCE(total_characters, 0),
                    COALESCE(total_duration_ms, 0)
             FROM device_usage_stats
             WHERE device_id = ?1
             LIMIT 1",
            params![device_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;

    let (undated_count, undated_characters, undated_duration_ms) =
        if let Some((dev_count, dev_chars, dev_duration_ms)) = device_totals {
            (
                (dev_count - prior_count - window_count).max(0),
                (dev_chars - prior_characters - window_characters).max(0),
                (dev_duration_ms - prior_duration_ms - window_duration_ms).max(0),
            )
        } else {
            (0, 0, 0)
        };

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

/// Get settings as a shared read-only snapshot from the write-through cache.
///
/// Prefer this on read-only hot paths to avoid cloning [`AppSettings`].
pub fn get_settings_cached() -> Result<Arc<AppSettings>, StorageError> {
    if let Some(cached) = SETTINGS_CACHE.get() {
        return Ok(cached);
    }
    with_db(|conn| SETTINGS_CACHE.get_or_load_with(|| read_settings_from_connection(conn)))
}

/// Get settings
pub fn get_settings() -> Result<AppSettings, StorageError> {
    get_settings_cached().map(|settings| (*settings).clone())
}

/// Read [`AppSettings`] from an existing database file without opening it for
/// writing or initializing global storage. Used by CLI tools that share the
/// desktop app's stored credentials and preferences.
pub fn load_settings_from_path(path: &Path) -> Result<AppSettings, StorageError> {
    let conn =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| StorageError::ConnectionFailed(e.to_string()))?;
    read_settings_from_connection(&conn)
}

fn decode_settings(mut raw: serde_json::Value) -> serde_json::Result<AppSettings> {
    crate::commands::settings::migrate_llm_custom_endpoints(&mut raw);
    crate::commands::settings::migrate_non_official_gemini_endpoint(&mut raw);
    crate::commands::settings::migrate_openai_refine_model(&mut raw);
    crate::commands::settings::migrate_text_injection_override_restore_flag(&mut raw);
    crate::commands::settings::migrate_hotkey_digit_key_codes(&mut raw);
    crate::commands::settings::migrate_volcengine_minimal_effort(&mut raw);
    crate::commands::settings::migrate_retired_volcengine_default(&mut raw);
    crate::commands::settings::migrate_scheduled_model_defaults(&mut raw);
    crate::asr::models::migrate_known_model_settings(&mut raw);
    crate::commands::settings::migrate_tts_provider_without_system_voice(&mut raw);
    crate::commands::settings::migrate_hotkey_defaults(&mut raw);
    let mut settings = serde_json::from_value::<AppSettings>(raw)?;
    crate::commands::settings::normalize_text_injection_overrides(&mut settings);
    Ok(settings)
}

fn read_settings_from_connection(conn: &Connection) -> Result<AppSettings, StorageError> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM user_config WHERE key = 'app_settings' LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;

    let Some(json) = value else {
        return Ok(AppSettings::default());
    };
    match serde_json::from_str(&json).and_then(decode_settings) {
        Ok(settings) => Ok(settings),
        Err(e) => {
            log::warn!("Failed to parse settings, falling back to defaults: {}", e);
            Ok(AppSettings::default())
        }
    }
}

/// Save settings
pub fn save_settings(settings: &AppSettings) -> Result<(), StorageError> {
    let canonical = serde_json::to_value(settings)
        .and_then(decode_settings)
        .map_err(|e| StorageError::SerializeFailed(e.to_string()))?;
    let payload = serde_json::to_string(&canonical)
        .map_err(|e| StorageError::SerializeFailed(e.to_string()))?;

    with_db(|conn| {
        SETTINGS_CACHE.write_through(canonical, || {
            conn.execute(
                "INSERT INTO user_config (key, value) VALUES ('app_settings', ?1)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![payload],
            )
            .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
            Ok(())
        })
    })
}

pub fn get_recent_target_apps() -> Result<Vec<RecentTargetApp>, StorageError> {
    with_db(get_recent_target_apps_in)
}

fn get_recent_target_apps_in(conn: &Connection) -> Result<Vec<RecentTargetApp>, StorageError> {
    let Some(json) = get_user_config_value_in(conn, RECENT_TARGET_APPS_KEY)? else {
        return Ok(Vec::new());
    };

    match serde_json::from_str::<Vec<RecentTargetApp>>(&json) {
        Ok(apps) => Ok(apps),
        Err(err) => {
            log::warn!(
                "Failed to parse recent target apps, returning empty list: {}",
                err
            );
            Ok(Vec::new())
        }
    }
}

pub fn remember_recent_target_app(app: &RecentTargetApp) -> Result<(), StorageError> {
    with_db(|conn| remember_recent_target_app_in(conn, app))
}

fn remember_recent_target_app_in(
    conn: &Connection,
    app: &RecentTargetApp,
) -> Result<(), StorageError> {
    let mut apps = get_recent_target_apps_in(conn)?;
    let same_app = |existing: &RecentTargetApp| {
        existing.platform.eq_ignore_ascii_case(&app.platform)
            && existing.match_kind == app.match_kind
            && existing.match_value == app.match_value
    };
    let seen_at = |entry: &RecentTargetApp| DateTime::parse_from_rfc3339(&entry.last_seen_at).ok();
    let captured_at = seen_at(app);
    // Blocking workers may finish in a different order from the hotkey presses.
    if apps
        .iter()
        .any(|existing| same_app(existing) && seen_at(existing) > captured_at)
    {
        return Ok(());
    }
    apps.retain(|existing| !same_app(existing));
    apps.insert(0, app.clone());
    apps.sort_by_cached_key(|entry| Reverse(seen_at(entry)));
    apps.truncate(MAX_RECENT_TARGET_APPS);

    let payload = serde_json::to_string(&apps)
        .map_err(|err| StorageError::SerializeFailed(err.to_string()))?;
    set_user_config_value_in(conn, RECENT_TARGET_APPS_KEY, &payload)
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

/// Local device recovery obligations. These are intentionally outside
/// app_settings, which is exported/synced to other machines.
pub(crate) fn get_output_mute_recovery() -> Result<Option<String>, StorageError> {
    get_user_config_value("output_mute_recovery")
}

pub(crate) fn save_output_mute_recovery(json: &str) -> Result<(), StorageError> {
    set_user_config_value("output_mute_recovery", json)
}

/// Machine-local cache of fetched LLM model lists (`user_config`, outside `app_settings`).
pub fn get_llm_model_cache() -> Result<Option<String>, StorageError> {
    get_user_config_value("llm_model_cache")
}

pub fn save_llm_model_cache(json: &str) -> Result<(), StorageError> {
    set_user_config_value("llm_model_cache", json)
}

fn get_user_config_value(key: &str) -> Result<Option<String>, StorageError> {
    with_db(|conn| get_user_config_value_in(conn, key))
}

fn get_user_config_value_in(conn: &Connection, key: &str) -> Result<Option<String>, StorageError> {
    conn.query_row(
        "SELECT value FROM user_config WHERE key = ?1 LIMIT 1",
        params![key],
        |row| row.get(0),
    )
    .optional()
    .map_err(|e| StorageError::QueryFailed(e.to_string()))
}

fn set_user_config_value(key: &str, value: &str) -> Result<(), StorageError> {
    with_db(|conn| set_user_config_value_in(conn, key, value))
}

fn set_user_config_value_in(conn: &Connection, key: &str, value: &str) -> Result<(), StorageError> {
    conn.execute(
        "INSERT INTO user_config (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )
    .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
    Ok(())
}

pub(crate) fn ensure_column(
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

    #[error("Connection failed: {0}")]
    ConnectionFailed(String),

    #[error("Query failed: {0}")]
    QueryFailed(String),

    #[error("Failed to serialize data: {0}")]
    SerializeFailed(String),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Invalid input: {0}")]
    InvalidInput(String),

    #[error("Failed to parse stored data: {0}")]
    DeserializeFailed(String),

    #[error("File operation failed: {0}")]
    FileOpFailed(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn search_conn() -> Connection {
        let conn = recording_conn();
        conn.execute_batch(
            "ALTER TABLE history_record ADD COLUMN edited_text TEXT;
             CREATE TABLE device_registry (device_id TEXT PRIMARY KEY, device_name TEXT);
             INSERT INTO history_record (id, timestamp, text, original_text, edited_text) VALUES
             ('a', '2026-11-01', '今天完成标定', '支座返利', '支座反力已确认'),
             ('b', '2026-11-02', 'OPENAI 50% a_b C:\\audio', NULL, NULL),
             ('c', '2026-11-03', 'openai 500 axb C:audio', NULL, NULL);
             INSERT INTO history_record (id, timestamp, text, original_text, mode, error_code) VALUES
             ('d', '2026-11-04', 'Bearing calibration', '支座标定', 'translate_en_hands_free', 0),
             ('e', '2026-11-04', 'Calibration complete', '标定完成', 'translate_read', 0),
             ('f', '2026-11-05', '', NULL, 'assistant_raw', 1);",
        )
        .unwrap();
        conn
    }

    fn search_ids(conn: &Connection, query: &str, kind: HistoryRecordType) -> Vec<String> {
        search_history_in(conn, 20, 0, query, kind, None)
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect()
    }

    #[test]
    fn history_search_matches_all_keywords_across_text_versions_and_literal_symbols() {
        let conn = search_conn();
        for query in ["反力", "返利", "反力 标定", "  标定\t反力\u{3000}返利  "] {
            assert_eq!(search_ids(&conn, query, HistoryRecordType::All), ["a"]);
        }
        for query in ["50%", "a_b", "C:\\audio"] {
            assert_eq!(search_ids(&conn, query, HistoryRecordType::All), ["b"]);
        }
        assert_eq!(
            search_ids(&conn, "openai", HistoryRecordType::All),
            ["c", "b"]
        );
        assert!(search_ids(&conn, "反力 openai", HistoryRecordType::All).is_empty());
        assert!(search_ids(&conn, "' OR 1=1 --", HistoryRecordType::All).is_empty());
        // Error messages and provider/device labels do not participate in text search.
        conn.execute("UPDATE history_record SET error_message = 'network', asr_model_name = 'network' WHERE id = 'f'", []).unwrap();
        assert!(search_ids(&conn, "network", HistoryRecordType::All).is_empty());
    }

    #[test]
    fn history_count_matches_the_search_it_summarizes() {
        let conn = search_conn();
        for (query, kind) in [
            ("", HistoryRecordType::All),
            ("标定", HistoryRecordType::All),
            ("openai", HistoryRecordType::Dictation),
            ("标定", HistoryRecordType::Translation),
            ("", HistoryRecordType::Failed),
            ("50%", HistoryRecordType::All),
        ] {
            assert_eq!(
                count_history_in(&conn, query, kind, None).unwrap() as usize,
                search_ids(&conn, query, kind).len(),
                "query={query:?}"
            );
        }
        assert_eq!(
            HISTORY_MODE_TRANSLATE_PREFIX,
            crate::state::ProcessingIntent::TranslateEn.as_str()
        );
    }

    #[test]
    fn history_search_filters_types_and_paginates_with_stable_ties() {
        let conn = search_conn();
        assert_eq!(
            search_ids(&conn, "", HistoryRecordType::Dictation),
            ["c", "b", "a"]
        );
        assert_eq!(
            search_ids(&conn, "标定", HistoryRecordType::Translation),
            ["d"]
        );
        assert_eq!(
            search_ids(&conn, "标定", HistoryRecordType::TranslateRead),
            ["e"]
        );
        assert_eq!(search_ids(&conn, "", HistoryRecordType::Failed), ["f"]);
        let ids = (0..3)
            .flat_map(|page| {
                search_history_in(&conn, 2, page * 2, "", HistoryRecordType::All, None).unwrap()
            })
            .map(|record| record.id)
            .collect::<Vec<_>>();
        assert_eq!(ids, ["f", "e", "d", "c", "b", "a"]);
        // A keyword on the oldest row is found without first fetching the preceding pages.
        assert_eq!(
            search_history_in(&conn, 1, 0, "反力", HistoryRecordType::All, None).unwrap()[0].id,
            "a"
        );
        // Target app filter narrows both search pages and counts (case-insensitively).
        conn.execute(
            "UPDATE history_record SET target_app_platform = 'macos', target_app_key = 'com.todesktop.cursor', target_app_name = 'Cursor' WHERE id IN ('a', 'c')",
            [],
        )
        .unwrap();
        let app_ids = search_history_in(
            &conn,
            20,
            0,
            "",
            HistoryRecordType::All,
            Some(" COM.TODESKTOP.CURSOR "),
        )
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect::<Vec<_>>();
        assert_eq!(app_ids, ["c", "a"]);
        assert_eq!(
            count_history_in(
                &conn,
                "",
                HistoryRecordType::All,
                Some(" COM.TODESKTOP.CURSOR ")
            )
            .unwrap(),
            2
        );
    }

    fn recording_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE history_record (
                 id TEXT PRIMARY KEY, timestamp TEXT DEFAULT '', text TEXT DEFAULT '',
                 original_text TEXT, ai_correction_applied INTEGER DEFAULT 0,
                 llm_invoked INTEGER DEFAULT 0, mode TEXT DEFAULT 'assistant_raw',
                 duration_ms INTEGER DEFAULT 0, audio_path TEXT, is_final INTEGER DEFAULT 1,
                 error_code INTEGER DEFAULT 0, error_message TEXT, source_device_id TEXT,
                 asr_model_name TEXT, llm_model_name TEXT,
                 target_app_platform TEXT, target_app_key TEXT, target_app_name TEXT,
                 has_edit_conflict INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE usage_stats (
                 id INTEGER PRIMARY KEY, total_recording_count INTEGER DEFAULT 0,
                 total_duration_ms INTEGER DEFAULT 0, total_characters INTEGER DEFAULT 0,
                 llm_correction_count INTEGER DEFAULT 0, last_updated TEXT
             );
             INSERT INTO usage_stats (id) VALUES (1);
             CREATE TABLE device_usage_stats (
                 device_id TEXT PRIMARY KEY, total_recording_count INTEGER DEFAULT 0
             );",
        )
        .unwrap();
        migrate_recording_counts(&conn).unwrap();
        conn
    }

    fn recording(id: &str) -> HistoryRecord {
        HistoryRecord {
            id: id.into(),
            timestamp: Utc::now().to_rfc3339(),
            text: "hello".into(),
            original_text: None,
            ai_correction_applied: false,
            llm_invoked: true,
            mode: "assistant_raw".into(),
            duration_ms: 1200,
            audio_path: None,
            is_final: true,
            error_code: HISTORY_ERROR_NONE,
            error_message: None,
            source_device_id: Some("device-a".into()),
            source_device_name: None,
            asr_model_name: None,
            llm_model_name: None,
            edited_text: None,
            target_app_platform: None,
            target_app_key: None,
            target_app_name: None,
            has_edit_conflict: false,
        }
    }

    fn recording_totals(conn: &Connection) -> (i64, i64, i64, i64) {
        conn.query_row(
            "SELECT total_recording_count, total_duration_ms, total_characters,
                    llm_correction_count FROM usage_stats WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap()
    }

    #[test]
    fn recording_count_repairs_large_synced_history_and_preserves_deleted_totals() {
        let conn = recording_conn();
        conn.execute_batch(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 17000)
             INSERT INTO history_record (id, source_device_id) SELECT CAST(i AS TEXT), 'device-a' FROM n;
             INSERT INTO history_record (id, mode) VALUES ('reading', 'translate_read');
             INSERT INTO history_record (id, error_code) VALUES ('failure', 1);
             UPDATE usage_stats SET total_recording_count = 3,
                 total_duration_ms = 12345, total_characters = 67890, llm_correction_count = 123;
             INSERT INTO device_usage_stats VALUES ('device-a', 3);",
        )
        .unwrap();

        migrate_recording_counts(&conn).unwrap();
        assert_eq!(recording_totals(&conn), (17000, 12345, 67890, 123));
        let device_count: i64 = conn
            .query_row(
                "SELECT total_recording_count FROM device_usage_stats",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(device_count, 17000);
        migrate_recording_counts(&conn).unwrap();
        assert_eq!(recording_totals(&conn).0, 17000);

        conn.execute_batch(
            "DELETE FROM history_record;
             UPDATE usage_stats SET total_recording_count = 18000;",
        )
        .unwrap();
        migrate_recording_counts(&conn).unwrap();
        assert_eq!(recording_totals(&conn).0, 18000);
    }

    #[test]
    fn recording_count_is_independent_of_usage_cursor_and_survives_event_replay() {
        let conn = recording_conn();
        let record = recording("synced");
        // A server snapshot already covers this row: no aggregate delta is needed.
        assert!(insert_history_record_with_stats_conn(&conn, &record, false, false).unwrap());
        assert_eq!(recording_totals(&conn), (1, 0, 0, 0));
        assert!(!insert_history_record_with_stats_conn(&conn, &record, false, false).unwrap());
        assert_eq!(recording_totals(&conn).0, 1);

        // A live event beyond usage_seq updates aggregates but must not count twice.
        increment_usage_stats_conn(&conn, 1200, 5, true).unwrap();
        assert_eq!(recording_totals(&conn), (1, 1200, 5, 1));
        conn.execute("DELETE FROM history_record WHERE id = 'synced'", [])
            .unwrap();
        assert!(insert_history_record_with_stats_conn(&conn, &record, false, false).unwrap());
        migrate_recording_counts(&conn).unwrap();
        assert_eq!(recording_totals(&conn), (1, 1200, 5, 1));
    }

    #[test]
    fn recording_count_includes_local_dictation_and_excludes_reading_and_failures() {
        let conn = recording_conn();
        assert!(insert_history_record_with_stats_conn(&conn, &recording("local"), true, false).unwrap());
        assert_eq!(recording_totals(&conn), (1, 1200, 5, 1));
        let mut reading = recording("reading");
        reading.mode = HISTORY_MODE_TRANSLATE_READ.into();
        insert_history_record_with_stats_conn(&conn, &reading, false, false).unwrap();
        let mut failure = recording("failure");
        failure.error_code = crate::services::history_service::HISTORY_ERROR_ASR_FAILED;
        insert_history_record_with_stats_conn(&conn, &failure, false, false).unwrap();
        migrate_recording_counts(&conn).unwrap();
        assert_eq!(recording_totals(&conn), (1, 1200, 5, 1));
    }

    #[test]
    fn recording_count_failure_rolls_back_history_and_can_be_retried() {
        let conn = recording_conn();
        conn.execute_batch(
            "CREATE TRIGGER reject_count BEFORE UPDATE OF total_recording_count ON usage_stats
             BEGIN SELECT RAISE(ABORT, 'test count failure'); END;",
        )
        .unwrap();
        let record = recording("retry");
        assert!(insert_history_record_with_stats_conn(&conn, &record, true, false).is_err());
        let history_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM history_record", [], |r| r.get(0))
            .unwrap();
        assert_eq!(history_count, 0);
        assert_eq!(recording_totals(&conn), (0, 0, 0, 0));
        conn.execute_batch("DROP TRIGGER reject_count;").unwrap();
        assert!(insert_history_record_with_stats_conn(&conn, &record, true, false).unwrap());
        assert_eq!(recording_totals(&conn), (1, 1200, 5, 1));
    }

    fn setup_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE user_config (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
            [],
        )
        .unwrap();
        conn
    }

    #[test]
    fn recent_target_apps_use_capture_order_when_writes_arrive_out_of_order() {
        let conn = setup_conn();
        let app = |id: &str, name: &str, captured_at: &str| RecentTargetApp {
            platform: "macos".into(),
            app_name: name.into(),
            match_kind: "bundle_id".into(),
            match_value: id.into(),
            display_name: None,
            process_name: None,
            bundle_id: Some(id.into()),
            app_user_model_id: None,
            executable_path: None,
            last_seen_at: captured_at.into(),
        };
        let latest = app("com.example.a", "Current name", "2026-11-22T10:00:03+00:00");
        let other = app("com.example.b", "Other app", "2026-11-22T10:00:02+00:00");
        let stale = app("com.example.a", "Old name", "2026-11-22T10:00:01+00:00");
        for entry in [&latest, &other, &stale] {
            remember_recent_target_app_in(&conn, entry).unwrap();
        }
        assert_eq!(get_recent_target_apps_in(&conn).unwrap(), [latest, other]);
    }

    fn read_settings_blob(conn: &Connection) -> serde_json::Value {
        let json: String = conn
            .query_row(
                "SELECT value FROM user_config WHERE key = 'app_settings'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        serde_json::from_str(&json).unwrap()
    }

    #[test]
    fn history_record_mode_reads_one_row_and_reports_missing_ids() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE history_record (id TEXT PRIMARY KEY, mode TEXT NOT NULL)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO history_record (id, mode) VALUES ('a', 'translate_read'), ('b', 'assistant_raw')",
            [],
        )
        .unwrap();

        assert_eq!(
            history_record_mode_in(&conn, "a").unwrap().as_deref(),
            Some("translate_read")
        );
        assert_eq!(
            history_record_mode_in(&conn, "b").unwrap().as_deref(),
            Some("assistant_raw")
        );
        assert_eq!(history_record_mode_in(&conn, "missing").unwrap(), None);
    }

    #[test]
    fn migrate_settings_blob_upgrades_model_settings_without_changing_custom_endpoints() {
        let conn = setup_conn();
        let mut raw = serde_json::to_value(AppSettings::default()).unwrap();
        raw["modelDefaultsVersion"] = 0.into();
        raw["llmOpenaiModel"] = "gpt-4o-mini".into();
        raw["geminiLiveModel"] = "gemini-3.1-flash-live-preview".into();
        raw["qwenAsrModel"] = "qwen3-asr-flash-realtime-2025-10-27".into();
        raw["qwenAsrBatchModel"] = "qwen3-asr-flash-2026-02-10".into();
        raw["funasrModel"] = "fun-asr-realtime-2025-11-07".into();
        let mut expected = raw.clone();
        expected["modelDefaultsVersion"] = 1.into();
        expected["llmOpenaiModel"] = crate::llm::default_openai_model().into();
        expected["geminiLiveModel"] = "gemini-3.8-live".into();
        expected["qwenAsrModel"] = crate::asr::models::default_model("qwen").into();
        expected["qwenAsrBatchModel"] = crate::asr::models::default_model("qwenBatch").into();
        expected["funasrModel"] = crate::asr::models::default_model("funasr").into();
        conn.execute(
            "INSERT INTO user_config (key,value) VALUES ('app_settings',?1)",
            params![raw.to_string()],
        )
        .unwrap();
        migrate_settings_blob(&conn);
        assert_eq!(read_settings_blob(&conn), expected);
        migrate_settings_blob(&conn);
        assert_eq!(read_settings_blob(&conn), expected);
    }

    #[test]
    fn migrate_gemini_live_provider_preserves_recognition_configuration() {
        let conn = setup_conn();
        let raw = serde_json::json!({
            "modelDefaultsVersion": 1,
            "asrProviderType": "gemini-live",
            "geminiApiKey": "example-key",
            "geminiLiveModel": "gemini-3.5-transcribe-live",
            "geminiModel": "gemini-3.5-transcribe",
            "geminiLanguage": "zh-en"
        });
        conn.execute(
            "INSERT INTO user_config (key,value) VALUES ('app_settings',?1)",
            params![raw.to_string()],
        )
        .unwrap();
        migrate_settings_blob(&conn);
        let migrated = read_settings_blob(&conn);
        assert_eq!(migrated["asrProviderType"], "gemini");
        assert_eq!(migrated["geminiRecognitionMode"], "realtime");
        for field in [
            "geminiApiKey",
            "geminiLiveModel",
            "geminiModel",
            "geminiLanguage",
        ] {
            assert_eq!(migrated[field], raw[field]);
        }
        migrate_settings_blob(&conn);
        assert_eq!(read_settings_blob(&conn), migrated);
        let settings: AppSettings = serde_json::from_value(migrated).unwrap();
        let config = crate::asr::AsrConfig::from(&settings);
        assert_eq!(
            config.provider_type,
            crate::asr::AsrProviderType::GeminiLive
        );
        assert!(!config.post_recording_batch_refine_enabled());
    }

    #[test]
    fn migrate_settings_blob_updates_only_the_retired_volcengine_default_and_is_idempotent() {
        let conn = setup_conn();
        let mut settings = AppSettings::default();
        settings.llm_volcengine_model = "doubao-seed-2-0-mini-260215".into();
        settings
            .llm_custom_endpoints
            .push(crate::commands::settings::CustomLlmEndpoint {
                id: "mine".into(),
                name: "Custom".into(),
                base_url: "https://custom.example/v1".into(),
                api_key: "fake-key".into(),
                model: settings.llm_volcengine_model.clone(),
                api_mode: "chat_completions".into(),
                reasoning_effort: String::new(),
                extra_body: String::new(),
            });
        settings.llm_active_custom_endpoint_id = "mine".into();
        let mut expected = serde_json::to_value(&settings).unwrap();
        conn.execute(
            "INSERT INTO user_config (key, value) VALUES ('app_settings', ?1)",
            params![serde_json::to_string(&settings).unwrap()],
        )
        .unwrap();
        migrate_settings_blob(&conn);
        expected["llmVolcengineModel"] = serde_json::json!(crate::llm::default_volcengine_model());
        assert_eq!(read_settings_blob(&conn), expected);
        migrate_settings_blob(&conn);
        assert_eq!(read_settings_blob(&conn), expected);
    }

    #[test]
    fn migrate_settings_blob_persists_named_endpoint_and_drops_legacy_fields() {
        let conn = setup_conn();
        let legacy = r#"{
            "llmProviderType": "custom",
            "llmCustomBaseUrl": "https://api.deepseek.com",
            "llmCustomApiKey": "sk-legacy",
            "llmCustomModel": "deepseek-v4-flash",
            "llmCustomApiMode": "chat_completions"
        }"#;
        conn.execute(
            "INSERT INTO user_config (key, value) VALUES ('app_settings', ?1)",
            params![legacy],
        )
        .unwrap();

        migrate_settings_blob(&conn);

        let value = read_settings_blob(&conn);
        // New shape persisted.
        let endpoints = value["llmCustomEndpoints"].as_array().unwrap();
        assert_eq!(endpoints.len(), 1);
        assert_eq!(endpoints[0]["baseUrl"], "https://api.deepseek.com");
        assert_eq!(endpoints[0]["model"], "deepseek-v4-flash");
        let id = endpoints[0]["id"].as_str().unwrap().to_string();
        assert_eq!(value["llmActiveCustomEndpointId"], id);
        // Legacy fields are gone after re-serializing through AppSettings.
        assert!(value.get("llmCustomBaseUrl").is_none());
        assert!(value.get("llmCustomApiMode").is_none());
    }

    #[test]
    fn migrate_settings_blob_is_idempotent() {
        let conn = setup_conn();
        let legacy = r#"{
            "llmProviderType": "custom",
            "llmCustomBaseUrl": "https://api.deepseek.com",
            "llmCustomApiKey": "sk-legacy",
            "llmCustomModel": "deepseek-v4-flash",
            "llmCustomApiMode": "chat_completions"
        }"#;
        conn.execute(
            "INSERT INTO user_config (key, value) VALUES ('app_settings', ?1)",
            params![legacy],
        )
        .unwrap();

        migrate_settings_blob(&conn);
        let id_after_first = read_settings_blob(&conn)["llmActiveCustomEndpointId"]
            .as_str()
            .unwrap()
            .to_string();

        // Second run must not regenerate the id or duplicate the endpoint.
        migrate_settings_blob(&conn);
        let value = read_settings_blob(&conn);
        assert_eq!(value["llmCustomEndpoints"].as_array().unwrap().len(), 1);
        assert_eq!(value["llmActiveCustomEndpointId"], id_after_first);
    }

    #[test]
    fn migrate_settings_blob_noop_when_no_settings_row() {
        let conn = setup_conn();
        // Must not panic or insert anything when there's no settings row.
        migrate_settings_blob(&conn);
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM user_config", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    fn daily_stats_conn() -> Connection {
        let conn = recording_conn();
        conn.execute_batch(
            "ALTER TABLE device_usage_stats ADD COLUMN total_duration_ms INTEGER DEFAULT 0;
             ALTER TABLE device_usage_stats ADD COLUMN total_characters INTEGER DEFAULT 0;
             ALTER TABLE device_usage_stats ADD COLUMN llm_correction_count INTEGER DEFAULT 0;
             ALTER TABLE device_usage_stats ADD COLUMN last_updated TEXT;
             CREATE TABLE IF NOT EXISTS user_config (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS daily_usage_stats (
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
             );",
        )
        .unwrap();
        crate::sync_v2::store::init_schema(&conn).unwrap();
        conn
    }

    #[test]
    fn local_stats_failure_rolls_back_history_and_all_counters_and_allows_retry() {
        for table in ["device_usage_stats", "daily_usage_stats"] {
            for update_account in [false, true] {
                let conn = daily_stats_conn();
                // The one-time backfill has already run, as it would at startup.
                migrate_daily_usage_stats(&conn).unwrap();
                conn.execute_batch(&format!(
                    "CREATE TRIGGER reject_stats BEFORE INSERT ON {table}
                     BEGIN SELECT RAISE(ABORT, 'test stats failure'); END;"
                ))
                .unwrap();
                let mut record = recording("retry-local");
                record.target_app_platform = Some("macos".into());
                record.target_app_key = Some("test.app".into());
                record.target_app_name = Some("Test".into());
                let persist =
                    || insert_history_record_with_stats_conn(&conn, &record, update_account, true);
                assert!(persist().is_err());
                let row_counts: (i64, i64, i64, i64) = conn
                    .query_row(
                        "SELECT (SELECT COUNT(*) FROM history_record),
                            (SELECT COUNT(*) FROM counted_recordings),
                            (SELECT COUNT(*) FROM device_usage_stats),
                            (SELECT COUNT(*) FROM daily_usage_stats)",
                        [],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                    )
                    .unwrap();
                assert_eq!(row_counts, (0, 0, 0, 0));
                assert_eq!(recording_totals(&conn), (0, 0, 0, 0));

                conn.execute_batch("DROP TRIGGER reject_stats;").unwrap();
                assert!(persist().unwrap());
                assert!(!persist().unwrap());
                migrate_daily_usage_stats(&conn).unwrap();
                let today = Local::now().date_naive();
                let apps =
                    get_app_usage_stats_in(&conn, "device-a", AppUsageRange::All, today).unwrap();
                assert_eq!(apps.len(), 1);
                assert_eq!(
                    (
                        apps[0].usage_count,
                        apps[0].total_characters,
                        apps[0].total_duration_ms,
                        apps[0].llm_count
                    ),
                    (1, 5, 1200, 1)
                );
                let device_totals: (i64, i64, i64, i64) = conn.query_row(
                    "SELECT total_recording_count, total_characters, total_duration_ms, llm_correction_count
                     FROM device_usage_stats WHERE device_id = 'device-a'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                ).unwrap();
                assert_eq!(device_totals, (1, 5, 1200, 1));
                assert_eq!(
                    recording_totals(&conn),
                    if update_account {
                        (1, 1200, 5, 1)
                    } else {
                        (1, 0, 0, 0)
                    }
                );
            }
        }
    }

    #[test]
    fn local_stats_exclude_reads_failures_and_synced_history() {
        let conn = daily_stats_conn();
        let record = recording("local");
        insert_history_record_with_stats_conn(&conn, &record, true, true).unwrap();
        let mut reading = recording("reading");
        reading.mode = HISTORY_MODE_TRANSLATE_READ.into();
        insert_history_record_with_stats_conn(&conn, &reading, false, true).unwrap();
        let mut failed = recording("failed");
        failed.error_code = crate::services::history_service::HISTORY_ERROR_ASR_FAILED;
        insert_history_record_with_stats_conn(&conn, &failed, false, true).unwrap();
        let mut synced = recording("synced");
        synced.source_device_id = Some("other-device".into());
        insert_history_record_with_stats_conn(&conn, &synced, false, false).unwrap();

        let activity = get_daily_activity_in(&conn, "device-a", Local::now().date_naive()).unwrap();
        assert_eq!(activity.days.len(), 1);
        assert_eq!(activity.days[0].usage_count, 1);
        assert_eq!(activity.days[0].total_characters, 5);
        assert_eq!(activity.undated_count, 0);
        let device_rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM device_usage_stats", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(device_rows, 1);
        let remote = get_daily_activity_in(&conn, "other-device", Local::now().date_naive()).unwrap();
        assert!(remote.days.is_empty());
    }

    #[test]
    fn daily_app_stats_aggregate_across_ranges_and_exclude_untracked() {
        let conn = daily_stats_conn();
        let today = NaiveDate::from_ymd_opt(2026, 11, 18).unwrap(); // Wednesday

        // Today (2026-11-18)
        upsert_daily_usage_stats_conn(
            &conn,
            "dev-1",
            "2026-11-18",
            FEATURE_KIND_DICTATION,
            "macos",
            "com.todesktop.cursor",
            "Cursor",
            3,
            300,
            15000,
            2,
            "2026-11-18T10:00:00Z",
        )
        .unwrap();
        // Earlier this week (Monday 2026-11-16)
        upsert_daily_usage_stats_conn(
            &conn,
            "dev-1",
            "2026-11-16",
            FEATURE_KIND_DICTATION,
            "macos",
            "com.tencent.xinWeChat",
            "微信",
            4,
            200,
            10000,
            4,
            "2026-11-16T08:00:00Z",
        )
        .unwrap();
        // Earlier this month (2026-11-02)
        upsert_daily_usage_stats_conn(
            &conn,
            "dev-1",
            "2026-11-02",
            FEATURE_KIND_DICTATION,
            "macos",
            "com.todesktop.cursor",
            "Cursor App",
            2,
            100,
            5000,
            1,
            "2026-11-02T08:00:00Z",
        )
        .unwrap();
        // Last month (2026-10-15) + untracked + another device
        upsert_daily_usage_stats_conn(
            &conn,
            "dev-1",
            "2026-10-15",
            FEATURE_KIND_DICTATION,
            "",
            UNTRACKED_APP_KEY,
            "",
            5,
            500,
            20000,
            3,
            "2026-10-15T08:00:00Z",
        )
        .unwrap();
        upsert_daily_usage_stats_conn(
            &conn,
            "dev-2",
            "2026-11-18",
            FEATURE_KIND_DICTATION,
            "windows",
            "code.exe",
            "Code",
            99,
            9999,
            99999,
            99,
            "2026-11-18T10:00:00Z",
        )
        .unwrap();

        let today_stats = get_app_usage_stats_in(&conn, "dev-1", AppUsageRange::Today, today).unwrap();
        assert_eq!(today_stats.len(), 1);
        assert_eq!(today_stats[0].app_key, "com.todesktop.cursor");
        assert_eq!(today_stats[0].usage_count, 3);

        let week_stats = get_app_usage_stats_in(&conn, "dev-1", AppUsageRange::Week, today).unwrap();
        assert_eq!(week_stats.len(), 2);
        assert_eq!(week_stats[0].app_key, "com.tencent.xinWeChat");
        assert_eq!(week_stats[1].app_key, "com.todesktop.cursor");

        let all_stats = get_app_usage_stats_in(&conn, "dev-1", AppUsageRange::All, today).unwrap();
        assert_eq!(all_stats.len(), 2);
        assert_eq!(all_stats[0].app_key, "com.todesktop.cursor");
        assert_eq!(all_stats[0].usage_count, 5);
        assert_eq!(all_stats[0].total_characters, 400);
        // Latest timestamped display name wins.
        assert_eq!(all_stats[0].app_name, "Cursor");
    }

    #[test]
    fn daily_activity_includes_untracked_and_prior_baseline() {
        let conn = daily_stats_conn();
        let today = NaiveDate::from_ymd_opt(2026, 11, 18).unwrap();

        upsert_daily_usage_stats_conn(
            &conn,
            "dev-1",
            "2024-01-10",
            FEATURE_KIND_DICTATION,
            "",
            UNTRACKED_APP_KEY,
            "",
            10,
            1000,
            40000,
            8,
            "2024-01-10T10:00:00Z",
        )
        .unwrap();
        upsert_daily_usage_stats_conn(
            &conn,
            "dev-1",
            "2026-11-18",
            FEATURE_KIND_DICTATION,
            "macos",
            "com.tencent.xinWeChat",
            "微信",
            2,
            250,
            9000,
            2,
            "2026-11-18T10:00:00Z",
        )
        .unwrap();
        upsert_daily_usage_stats_conn(
            &conn,
            "dev-1",
            "2026-11-18",
            FEATURE_KIND_DICTATION,
            "",
            UNTRACKED_APP_KEY,
            "",
            1,
            50,
            1000,
            0,
            "2026-11-18T11:00:00Z",
        )
        .unwrap();

        let activity = get_daily_activity_in(&conn, "dev-1", today).unwrap();
        assert_eq!(activity.end_date, "2026-11-18");
        assert_eq!(activity.prior_count, 10);
        assert_eq!(activity.prior_characters, 1000);
        assert_eq!(activity.days.len(), 1);
        assert_eq!(activity.days[0].local_date, "2026-11-18");
        assert_eq!(activity.days[0].usage_count, 3);
        assert_eq!(activity.days[0].total_characters, 300);
        assert_eq!(activity.undated_count, 0);

        // When retention pruned older history rows before backfill, `device_usage_stats`
        // preserves the true lifetime total (e.g. 50 uses / 5000 chars vs 3 uses / 300 chars in window).
        conn.execute(
            "INSERT INTO device_usage_stats (device_id, total_recording_count, total_characters, total_duration_ms)
             VALUES ('dev-1', 50, 5000, 120000)",
            [],
        )
        .unwrap();
        let reconciled = get_daily_activity_in(&conn, "dev-1", today).unwrap();
        assert_eq!(reconciled.prior_count, 10);
        assert_eq!(reconciled.prior_characters, 1000);
        assert_eq!(reconciled.prior_duration_ms, 40000);
        assert_eq!(reconciled.undated_count, 37);
        assert_eq!(reconciled.undated_characters, 3700);
        assert_eq!(reconciled.undated_duration_ms, 70000);
    }

    #[test]
    fn daily_activity_does_not_backdate_retention_gaps() {
        let conn = daily_stats_conn();
        conn.execute_batch(
            "INSERT INTO device_usage_stats (device_id, total_recording_count, total_characters, total_duration_ms)
             VALUES ('dev-local', 100, 10000, 100000);"
        ).unwrap();
        upsert_daily_usage_stats_conn(
            &conn,
            "dev-local",
            "2026-11-16",
            FEATURE_KIND_DICTATION,
            "",
            UNTRACKED_APP_KEY,
            "",
            10,
            1000,
            10000,
            0,
            "2026-11-16T00:00:00Z",
        )
        .unwrap();
        let today = NaiveDate::from_ymd_opt(2026, 11, 16).unwrap();
        let activity = get_daily_activity_in(&conn, "dev-local", today).unwrap();
        assert_eq!(
            (
                activity.prior_count,
                activity.prior_characters,
                activity.prior_duration_ms
            ),
            (0, 0, 0)
        );
        assert_eq!(
            (
                activity.undated_count,
                activity.undated_characters,
                activity.undated_duration_ms
            ),
            (90, 9000, 90000)
        );

        conn.execute("UPDATE device_usage_stats SET total_recording_count = 0, total_characters = 0, total_duration_ms = 0", []).unwrap();
        let activity = get_daily_activity_in(&conn, "dev-local", today).unwrap();
        assert_eq!(
            (
                activity.undated_count,
                activity.undated_characters,
                activity.undated_duration_ms
            ),
            (0, 0, 0)
        );
        assert_eq!(activity.days[0].total_characters, 1000);
    }

    #[test]
    fn migrate_daily_usage_stats_backfills_once_and_skips_failures_and_reads() {
        let conn = daily_stats_conn();
        conn.execute_batch(
            "INSERT INTO user_config (key, value) VALUES ('sync_device_id', 'dev-local');
             INSERT INTO history_record (id, timestamp, text, duration_ms, llm_invoked, mode, error_code, source_device_id) VALUES
             ('ok1', '2026-11-15T10:00:00Z', 'hello', 1000, 1, 'assistant_raw', 0, 'dev-local'),
             ('ok_legacy', '2026-11-15T10:02:00Z', 'legacy', 800, 0, 'assistant_raw', 0, NULL),
             ('remote1', '2026-11-15T10:03:00Z', 'remote', 900, 1, 'assistant_raw', 0, 'dev-other'),
             ('read1', '2026-11-15T10:05:00Z', 'skip read', 0, 1, 'translate_read', 0, 'dev-local'),
             ('fail1', '2026-11-15T10:10:00Z', '', 500, 0, 'assistant_raw', 1, 'dev-local');",
        )
        .unwrap();

        migrate_daily_usage_stats(&conn).unwrap();
        migrate_daily_usage_stats(&conn).unwrap();

        let count: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(usage_count), 0) FROM daily_usage_stats WHERE device_id = 'dev-local'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);
        let remote_count: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(usage_count), 0) FROM daily_usage_stats WHERE device_id = 'dev-other'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(remote_count, 0);
    }

    #[test]
    fn device_stats_seed_excludes_reads_and_failures_and_preserves_prior_history_on_new_write() {
        let conn = daily_stats_conn();
        conn.execute_batch(
            "INSERT INTO history_record (id, timestamp, text, duration_ms, llm_invoked, mode, error_code, source_device_id) VALUES
             ('old-ok', '2026-11-15T10:00:00Z', 'abc', 1000, 1, 'assistant_raw', 0, 'device-a'),
             ('old-read', '2026-11-15T10:05:00Z', 'ignored read', 0, 1, 'translate_read', 0, 'device-a'),
             ('old-fail', '2026-11-15T10:10:00Z', 'ignored fail', 500, 0, 'assistant_raw', 1, 'device-a'),
             ('old-remote', '2026-11-15T10:15:00Z', 'ignored remote', 700, 1, 'assistant_raw', 0, 'device-b');",
        )
        .unwrap();

        let mut new_rec = recording("new-ok"); // text="hello" (5 chars), duration_ms=1200, llm_invoked=true
        new_rec.source_device_id = Some("device-a".into());
        assert!(insert_history_record_with_stats_conn(&conn, &new_rec, true, true).unwrap());

        let totals: (i64, i64, i64, i64) = conn
            .query_row(
                "SELECT total_recording_count, total_characters, total_duration_ms, llm_correction_count
                 FROM device_usage_stats WHERE device_id = 'device-a'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        // old-ok (1, 3, 1000, 1) + new-ok (1, 5, 1200, 1) = (2, 8, 2200, 2)
        assert_eq!(totals, (2, 8, 2200, 2));
    }

    #[test]
    fn settings_cache_avoids_redundant_reads_and_updates_on_write_through() {
        let cache = SettingsCache::new();
        let mut loads = 0usize;

        let first = cache
            .get_or_load_with(|| -> Result<AppSettings, &'static str> {
                loads += 1;
                let mut settings = AppSettings::default();
                settings.short_sentence_threshold = 9;
                Ok(settings)
            })
            .unwrap();
        let second = cache
            .get_or_load_with(|| -> Result<AppSettings, &'static str> {
                loads += 1;
                Ok(AppSettings::default())
            })
            .unwrap();
        assert_eq!(loads, 1);
        assert_eq!(first.short_sentence_threshold, 9);
        assert!(Arc::ptr_eq(&first, &second));

        let mut updated = AppSettings::default();
        updated.short_sentence_threshold = 17;
        updated
            .text_injection_overrides
            .push(crate::foreground_app::TextInjectionAppOverride {
                platform: " MACOS ".into(),
                match_kind: " bundle_id ".into(),
                match_value: " Com.Example.App ".into(),
                app_name: " Example ".into(),
                mode: " typing ".into(),
                skip_clipboard_restore: false,
            });
        updated.llm_volcengine_model = "doubao-seed-2-0-mini-260215".into();
        let canonical = decode_settings(serde_json::to_value(updated).unwrap()).unwrap();
        cache
            .write_through(canonical, || -> Result<(), &'static str> { Ok(()) })
            .unwrap();

        let after_write = cache.get().expect("cache populated after write-through");
        assert_eq!(after_write.short_sentence_threshold, 17);
        assert_eq!(after_write.text_injection_overrides.len(), 1);
        assert_eq!(after_write.text_injection_overrides[0].platform, "macos");
        assert_eq!(
            after_write.text_injection_overrides[0].match_value,
            "com.example.app"
        );
        assert_eq!(after_write.text_injection_overrides[0].mode, "typing");
        assert_eq!(after_write.llm_volcengine_model, crate::llm::default_volcengine_model());
    }

    #[test]
    fn settings_cache_preserves_previous_value_when_persist_fails() {
        let cache = SettingsCache::new();
        let mut initial = AppSettings::default();
        initial.short_sentence_threshold = 12;
        cache
            .write_through(initial, || -> Result<(), &'static str> { Ok(()) })
            .unwrap();

        let mut failed_update = AppSettings::default();
        failed_update.short_sentence_threshold = 99;
        let err = cache
            .write_through(failed_update, || Err("disk full"))
            .unwrap_err();
        assert_eq!(err, "disk full");

        let current = cache.get().expect("previous cached value retained");
        assert_eq!(current.short_sentence_threshold, 12);
    }
}
