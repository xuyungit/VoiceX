//! Local, explicitly consented text pairs. Never added to the sync outbox.
use chrono::{Duration, SecondsFormat, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use tauri::Emitter;

use crate::storage::{self, StorageError};

const RETENTION_DAYS: i64 = 30;
const SETTINGS_KEY: &str = "post_edit_settings";

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Off,
    MetricsOnly,
    Records,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub mode: Mode,
}

#[derive(Default)]
struct Policy {
    settings: Settings,
    generation: u64,
}
static POLICY: Mutex<Policy> = Mutex::new(Policy {
    settings: Settings { mode: Mode::Off },
    generation: 0,
});
static APP: OnceLock<tauri::AppHandle> = OnceLock::new();
static SAVE_ERROR: Mutex<Option<String>> = Mutex::new(None);
// prepare() runs inside the injection resolver. Consent lookup must never wait
// for SQLite or a settings/clear operation holding POLICY.
static CAPTURE_GENERATION: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy)]
pub struct CaptureTicket(u64);

impl Policy {
    #[cfg(test)]
    fn ticket(&self) -> Option<CaptureTicket> {
        (self.settings.mode == Mode::Records).then_some(CaptureTicket(self.generation))
    }
    fn accepts(&self, ticket: CaptureTicket) -> bool {
        self.settings.mode == Mode::Records && self.generation == ticket.0
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Record {
    pub id: String,
    pub record_id: String,
    pub session_id: String,
    pub timestamp: String,
    pub last_observed_at: String,
    pub app_name: Option<String>,
    pub original_text: String,
    pub edited_text: String,
    pub complete: bool,
    pub end_reason: String,
    pub strategy: String,
    pub range_evidence: String,
    pub insertions: Option<usize>,
    pub deletions: Option<usize>,
    pub substitutions: Option<usize>,
    pub had_intermediate_changes: bool,
    #[serde(default)]
    pub stable: bool,
    #[serde(default)]
    pub composition: super::platform::CompositionState,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordPage {
    pub records: Vec<Record>,
    pub save_error: Option<String>,
}

fn sql(error: rusqlite::Error) -> StorageError {
    StorageError::QueryFailed(error.to_string())
}
fn json(error: serde_json::Error) -> StorageError {
    StorageError::QueryFailed(error.to_string())
}
fn timestamp(at: chrono::DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Millis, true)
}
pub fn create_schema(conn: &Connection) -> Result<(), StorageError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS post_edit_record (
            id TEXT PRIMARY KEY, source_id TEXT NOT NULL,
            timestamp TEXT NOT NULL, payload TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS post_edit_time ON post_edit_record(timestamp DESC, id DESC);
         CREATE UNIQUE INDEX IF NOT EXISTS post_edit_one_per_dictation ON post_edit_record(source_id);
         CREATE TABLE IF NOT EXISTS post_edit_deleted (
            kind TEXT NOT NULL, id TEXT NOT NULL, timestamp TEXT NOT NULL,
            PRIMARY KEY(kind, id)
         );",
    )
    .map_err(sql)
}

pub fn init(app: &tauri::AppHandle) -> Result<(), StorageError> {
    let settings = storage::with_db(|conn| {
        let value: Option<String> = conn
            .query_row(
                "SELECT value FROM user_config WHERE key = ?1",
                [SETTINGS_KEY],
                |r| r.get(0),
            )
            .optional()
            .map_err(sql)?;
        cleanup(conn)?;
        value
            .map(|value| serde_json::from_str(&value).map_err(json))
            .transpose()
    })?
    .unwrap_or_default();
    let _ = APP.set(app.clone());
    let mut policy = POLICY.lock().unwrap();
    policy.settings = settings;
    policy.generation = policy.generation.wrapping_add(1);
    publish_policy(&policy);
    apply_mode(policy.settings.mode);
    Ok(())
}

fn apply_mode(mode: Mode) {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    super::diagnostics::set_enabled(mode != Mode::Off);
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let _ = mode;
}

pub fn settings() -> Settings {
    POLICY.lock().unwrap().settings.clone()
}
pub fn set_mode(mode: Mode) -> Result<Settings, StorageError> {
    let mut policy = POLICY.lock().unwrap();
    let settings = Settings { mode };
    let value = serde_json::to_string(&settings).map_err(json)?;
    storage::with_db(|conn| {
        conn.execute(
            "INSERT INTO user_config(key,value) VALUES (?1,?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![SETTINGS_KEY, value],
        )
        .map_err(sql)?;
        Ok(())
    })?;
    policy.generation = policy.generation.wrapping_add(1);
    policy.settings = settings.clone();
    publish_policy(&policy);
    apply_mode(mode);
    drop(policy);
    changed();
    Ok(settings)
}
pub fn capture_ticket() -> Option<CaptureTicket> {
    let generation = CAPTURE_GENERATION.load(Ordering::Acquire);
    (generation != 0).then_some(CaptureTicket(generation))
}

fn publish_policy(policy: &Policy) {
    CAPTURE_GENERATION.store(
        if policy.settings.mode == Mode::Records {
            policy.generation
        } else {
            0
        },
        Ordering::Release,
    );
}

fn cleanup(conn: &Connection) -> Result<(), StorageError> {
    let cutoff = timestamp(Utc::now() - Duration::days(RETENTION_DAYS));
    conn.execute(
        "DELETE FROM post_edit_record WHERE timestamp < ?1",
        [&cutoff],
    )
    .map_err(sql)?;
    // A delayed write older than retention is rejected too, so expired tombstones
    // can be removed without allowing an old observer to resurrect content.
    conn.execute(
        "DELETE FROM post_edit_deleted WHERE timestamp < ?1",
        [&cutoff],
    )
    .map_err(sql)?;
    Ok(())
}

fn insert(conn: &Connection, record: &Record) -> Result<bool, StorageError> {
    if !record.stable
        || record.composition == super::platform::CompositionState::Active
        || record.original_text == record.edited_text
        || record.timestamp < timestamp(Utc::now() - Duration::days(RETENTION_DAYS))
    {
        return Ok(false);
    }
    let payload = serde_json::to_string(record).map_err(json)?;
    let count = conn.execute(
        "INSERT INTO post_edit_record(id,source_id,timestamp,payload)
         SELECT ?1,?2,?3,?4 WHERE NOT EXISTS (
            SELECT 1 FROM post_edit_deleted WHERE (kind='record' AND id=?1) OR (kind='source' AND id=?2)
         ) ON CONFLICT(source_id) DO UPDATE SET
            id=excluded.id, timestamp=excluded.timestamp, payload=excluded.payload
            WHERE excluded.timestamp > post_edit_record.timestamp",
        params![record.id, record.record_id, record.timestamp, payload],
    ).map_err(sql)?;
    Ok(count > 0)
}

pub fn save(ticket: CaptureTicket, record: &Record) -> Result<(), StorageError> {
    let policy = POLICY.lock().unwrap();
    if !policy.accepts(ticket) {
        return Ok(());
    }
    let result = storage::with_db(|conn| {
        cleanup(conn)?;
        insert(conn, record)
    });
    match &result {
        Err(error) => *SAVE_ERROR.lock().unwrap() = Some(error.to_string()),
        Ok(true) => *SAVE_ERROR.lock().unwrap() = None,
        Ok(false) => {}
    }
    drop(policy);
    if !matches!(result, Ok(false)) {
        changed();
    }
    result.map(|_| ())
}

pub fn list(limit: u32, offset: u32) -> Result<RecordPage, StorageError> {
    let records = storage::with_db(|conn| {
        cleanup(conn)?;
        let mut stmt = conn.prepare(
            "SELECT payload FROM post_edit_record ORDER BY timestamp DESC,id DESC LIMIT ?1 OFFSET ?2",
        ).map_err(sql)?;
        let rows = stmt
            .query_map(params![limit.clamp(1, 100), offset], |r| {
                r.get::<_, String>(0)
            })
            .map_err(sql)?;
        rows.map(|row| serde_json::from_str(&row.map_err(sql)?).map_err(json))
            .collect()
    })?;
    Ok(RecordPage {
        records,
        save_error: SAVE_ERROR.lock().unwrap().clone(),
    })
}

fn delete(conn: &Connection, kind: &str, id: &str) -> Result<(), StorageError> {
    conn.execute(
        "INSERT INTO post_edit_deleted(kind,id,timestamp) VALUES (?1,?2,?3)
         ON CONFLICT(kind,id) DO UPDATE SET timestamp=excluded.timestamp",
        params![kind, id, timestamp(Utc::now())],
    )
    .map_err(sql)?;
    let query = if kind == "source" {
        "DELETE FROM post_edit_record WHERE source_id=?1"
    } else {
        "DELETE FROM post_edit_record WHERE id=?1"
    };
    conn.execute(query, [id]).map_err(sql)?;
    Ok(())
}
/// Called inside the history-deletion transaction, including sync deletions.
pub fn delete_source(conn: &Connection, id: &str) -> Result<(), StorageError> {
    delete(conn, "source", id)
}
pub fn delete_record(id: &str, source: Option<&str>) -> Result<(), StorageError> {
    storage::with_db(|conn| {
        let tx = conn.unchecked_transaction().map_err(sql)?;
        delete_comparison(&tx, id, source)?;
        tx.commit().map_err(sql)
    })?;
    changed();
    Ok(())
}

fn delete_comparison(
    conn: &Connection,
    id: &str,
    source: Option<&str>,
) -> Result<(), StorageError> {
    let stored_source: Option<String> = conn
        .query_row(
            "SELECT source_id FROM post_edit_record WHERE id=?1",
            [id],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql)?;
    delete(conn, "record", id)?;
    // The UI also supplies the dictation ID: its displayed injection may have
    // been replaced by a newer final before the delete command reaches SQLite.
    if let Some(source) = source.or(stored_source.as_deref()) {
        delete_source(conn, source)?;
    }
    Ok(())
}
pub fn clear() -> Result<(), StorageError> {
    let mut policy = POLICY.lock().unwrap();
    storage::with_db(|conn| {
        conn.execute_batch(
            "BEGIN; DELETE FROM post_edit_record; DELETE FROM post_edit_deleted; COMMIT;",
        )
        .map_err(sql)
    })?;
    policy.generation = policy.generation.wrapping_add(1);
    publish_policy(&policy);
    *SAVE_ERROR.lock().unwrap() = None;
    apply_mode(policy.settings.mode);
    drop(policy);
    changed();
    Ok(())
}
pub fn changed() {
    if let Some(app) = APP.get() {
        if let Err(error) = app.emit("post-edit:updated", ()) {
            log::warn!("Could not notify post-edit history: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn record() -> Record {
        Record {
            id: "injection".into(),
            record_id: "source".into(),
            session_id: "session".into(),
            timestamp: timestamp(Utc::now()),
            last_observed_at: timestamp(Utc::now()),
            app_name: Some("Fixture".into()),
            original_text: "返利".into(),
            edited_text: "反力".into(),
            complete: false,
            end_reason: "focus_left".into(),
            strategy: "checkpoint_only".into(),
            range_evidence: "before_after_verified".into(),
            insertions: Some(0),
            deletions: Some(0),
            substitutions: Some(2),
            had_intermediate_changes: true,
            stable: true,
            composition: super::super::platform::CompositionState::Unknown,
        }
    }
    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();
        conn
    }
    #[test]
    fn opt_in_ticket_is_revoked_by_mode_changes_and_clear() {
        let mut policy = Policy::default();
        assert!(policy.ticket().is_none());
        policy.settings.mode = Mode::MetricsOnly;
        assert!(policy.ticket().is_none());
        policy.settings.mode = Mode::Records;
        let ticket = policy.ticket().unwrap();
        assert!(policy.accepts(ticket));
        policy.generation += 1;
        assert!(!policy.accepts(ticket));
        policy.settings.mode = Mode::Off;
        assert!(!policy.accepts(policy.ticket().unwrap_or(ticket)));
    }
    #[test]
    fn deletions_block_in_flight_writes_and_do_not_affect_other_sources() {
        for kind in ["record", "source"] {
            let conn = db();
            let record = record();
            assert!(insert(&conn, &record).unwrap());
            delete(
                &conn,
                kind,
                if kind == "record" {
                    &record.id
                } else {
                    &record.record_id
                },
            )
            .unwrap();
            assert!(!insert(&conn, &record).unwrap());
            let mut other = record.clone();
            other.id = "other-injection".into();
            other.record_id = "other-source".into();
            assert!(insert(&conn, &other).unwrap());
            let payload: String = conn
                .query_row("SELECT payload FROM post_edit_record", [], |r| r.get(0))
                .unwrap();
            let loaded: Record = serde_json::from_str(&payload).unwrap();
            assert_eq!(loaded.edited_text, "反力");
            assert!(!loaded.complete);
        }
    }
    #[test]
    fn unchanged_undo_and_expired_records_are_not_saved() {
        let conn = db();
        let mut record = record();
        record.edited_text = record.original_text.clone();
        assert!(!insert(&conn, &record).unwrap());
        record.edited_text = "反力".into();
        record.timestamp = timestamp(Utc::now() - Duration::days(31));
        assert!(!insert(&conn, &record).unwrap());
        record.timestamp = timestamp(Utc::now());
        assert!(insert(&conn, &record).unwrap());
        conn.execute(
            "UPDATE post_edit_record SET timestamp=?1",
            [timestamp(Utc::now() - Duration::days(31))],
        )
        .unwrap();
        cleanup(&conn).unwrap();
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM post_edit_record", [], |r| r
                .get::<_, usize>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn only_latest_attempt_for_a_dictation_survives_duplicate_and_late_writes() {
        let conn = db();
        let mut first = record();
        first.timestamp = timestamp(Utc::now() - Duration::seconds(2));
        assert!(insert(&conn, &first).unwrap());
        assert!(!insert(&conn, &first).unwrap());
        let mut latest = first.clone();
        latest.id = "latest-injection".into();
        latest.timestamp = timestamp(Utc::now());
        latest.edited_text = "最终反力".into();
        assert!(insert(&conn, &latest).unwrap());
        assert!(!insert(&conn, &first).unwrap());
        let (count, id): (usize, String) = conn
            .query_row("SELECT COUNT(*),id FROM post_edit_record", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((count, id), (1, latest.id));
    }

    #[test]
    fn deleting_a_comparison_blocks_later_attempts_for_the_same_dictation() {
        let conn = db();
        let first = record();
        assert!(insert(&conn, &first).unwrap());
        delete_comparison(&conn, &first.id, None).unwrap();
        let mut retry = first.clone();
        retry.id = "newer-final".into();
        retry.timestamp = timestamp(Utc::now() + Duration::seconds(1));
        assert!(!insert(&conn, &retry).unwrap());
    }

    #[test]
    fn unstable_or_composing_text_is_not_saved_and_legacy_stability_is_unknown() {
        let conn = db();
        let mut record = record();
        record.stable = false;
        assert!(!insert(&conn, &record).unwrap());
        record.stable = true;
        record.composition = super::super::platform::CompositionState::Active;
        assert!(!insert(&conn, &record).unwrap());
        let mut legacy = serde_json::to_value(&record).unwrap();
        legacy.as_object_mut().unwrap().remove("stable");
        legacy.as_object_mut().unwrap().remove("composition");
        let loaded: Record = serde_json::from_value(legacy).unwrap();
        assert!(!loaded.stable);
        assert_eq!(
            loaded.composition,
            super::super::platform::CompositionState::Unknown
        );
    }

    #[test]
    fn delete_from_an_old_view_also_removes_the_replaced_injection() {
        let conn = db();
        let first = record();
        assert!(insert(&conn, &first).unwrap());
        let mut newer = first.clone();
        newer.id = "newer-final".into();
        newer.timestamp = timestamp(Utc::now() + Duration::seconds(1));
        assert!(insert(&conn, &newer).unwrap());
        delete_comparison(&conn, &first.id, Some(&first.record_id)).unwrap();
        assert!(!insert(&conn, &newer).unwrap());
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM post_edit_record", [], |r| r
                .get::<_, usize>(0))
                .unwrap(),
            0
        );
    }
}
