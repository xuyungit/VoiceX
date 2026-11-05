//! Local, explicitly consented edit entries. Never added to the sync outbox.
//!
//! An entry stores how a dictation's edit was obtained. Its text is the
//! dictation's own: `history_record.text` (VoiceX output) and
//! `history_record.edited_text` (final version), so the two history views
//! cannot disagree and an entry lives exactly as long as its dictation.
use chrono::{Duration, SecondsFormat, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use tauri::Emitter;

use crate::storage::{self, StorageError};

// Tombstones only need to outlive observers still running for a deleted
// entry; writes older than this are rejected so expired tombstones are safe.
const TOMBSTONE_DAYS: i64 = 30;
const SETTINGS_KEY: &str = "post_edit_settings";

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    // The retired diagnostics-only mode had no user-visible result. Reading it
    // as Off keeps a hidden observer from running behind a switch shown off.
    #[serde(alias = "metrics_only")]
    Off,
    Records,
}

/// Who produced the edited text. Manual text is the user's explicit final
/// version, so a late observation never replaces it.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    #[default]
    Observed,
    Manual,
}

const DICTATION_MODE_PREFIX: &str = "assistant";

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
    /// Filled from the dictation when listed; not part of the stored payload.
    #[serde(default)]
    pub original_text: String,
    #[serde(default)]
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
    pub has_newer_unconfirmed_input: bool,
    #[serde(default)]
    pub stable: bool,
    #[serde(default)]
    pub composition: super::platform::CompositionState,
    #[serde(default)]
    pub source: Source,
}

/// The dictation's final text after a manual edit; `None` when it is the
/// unedited VoiceX output again.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FinalText {
    pub edited_text: Option<String>,
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
    .map_err(sql)?;
    // `manual` lets the upsert protect a hand-edited entry from observers.
    add_column(conn, "manual", "INTEGER NOT NULL DEFAULT 0")?;
    migrate_text_to_dictations(conn)
}

/// Entries written before the text moved to the dictation carried their own
/// copy. Keep it as the dictation's final text when it edits that dictation's
/// actual output; then drop entries without an edited dictation and strip the
/// copies. Idempotent; a no-op once every entry has been migrated.
fn migrate_text_to_dictations(conn: &Connection) -> Result<(), StorageError> {
    conn.execute_batch(
        "UPDATE history_record SET
            edited_text = json_extract(p.payload, '$.editedText'),
            edit_source = COALESCE(json_extract(p.payload, '$.source'), 'observed'),
            edited_at = json_extract(p.payload, '$.lastObservedAt')
         FROM post_edit_record p
         WHERE p.source_id = history_record.id
            AND history_record.edited_text IS NULL
            AND json_extract(p.payload, '$.originalText') = history_record.text
            AND json_extract(p.payload, '$.editedText') != history_record.text;
         DELETE FROM post_edit_record WHERE NOT EXISTS (
            SELECT 1 FROM history_record h
            WHERE h.id = post_edit_record.source_id AND h.edited_text IS NOT NULL
         );
         UPDATE post_edit_record SET payload = json_remove(payload, '$.originalText', '$.editedText')
         WHERE json_type(payload, '$.editedText') IS NOT NULL OR json_type(payload, '$.originalText') IS NOT NULL;",
    )
    .map_err(sql)
}

/// The stored payload: capture details only, never the text.
fn metadata(record: &Record) -> Result<String, StorageError> {
    let mut value = serde_json::to_value(record).map_err(json)?;
    if let Some(fields) = value.as_object_mut() {
        fields.remove("originalText");
        fields.remove("editedText");
    }
    serde_json::to_string(&value).map_err(json)
}

fn add_column(conn: &Connection, column: &str, definition: &str) -> Result<(), StorageError> {
    let exists = conn
        .prepare("SELECT 1 FROM pragma_table_info('post_edit_record') WHERE name=?1")
        .and_then(|mut stmt| stmt.exists([column]))
        .map_err(sql)?;
    if !exists {
        conn.execute(
            &format!("ALTER TABLE post_edit_record ADD COLUMN {column} {definition}"),
            [],
        )
        .map_err(sql)?;
    }
    Ok(())
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

/// Entries follow their dictation's retention (deleted with it), so only
/// tombstones expire here.
fn cleanup(conn: &Connection) -> Result<(), StorageError> {
    let cutoff = timestamp(Utc::now() - Duration::days(TOMBSTONE_DAYS));
    conn.execute(
        "DELETE FROM post_edit_deleted WHERE timestamp < ?1",
        [&cutoff],
    )
    .map_err(sql)?;
    Ok(())
}

/// Stores an observed edit and makes it the dictation's final text. Requires
/// the dictation row, with the observed text as its output, and skips
/// dictations the user already edited by hand.
pub(super) fn insert(conn: &Connection, record: &Record) -> Result<bool, StorageError> {
    if record.source != Source::Observed
        || !record.stable
        || (record.complete && record.has_newer_unconfirmed_input)
        || record.composition == super::platform::CompositionState::Active
        || record.original_text == record.edited_text
        || record.timestamp < timestamp(Utc::now() - Duration::days(TOMBSTONE_DAYS))
    {
        return Ok(false);
    }
    let payload = metadata(record)?;
    // A superseded injection of the same dictation typed different text; its
    // edit does not apply to the output the dictation kept.
    let count = conn.execute(
        "INSERT INTO post_edit_record(id,source_id,timestamp,payload)
         SELECT ?1,?2,?3,?4 WHERE NOT EXISTS (
            SELECT 1 FROM post_edit_deleted WHERE (kind='record' AND id=?1) OR (kind='source' AND id=?2)
         ) AND EXISTS (
            SELECT 1 FROM history_record WHERE id=?2 AND text=?5
                AND (edit_source IS NULL OR edit_source='observed')
         ) ON CONFLICT(source_id) DO UPDATE SET
            id=excluded.id, timestamp=excluded.timestamp, payload=excluded.payload
            WHERE excluded.timestamp > post_edit_record.timestamp AND post_edit_record.manual = 0",
        params![record.id, record.record_id, record.timestamp, payload, record.original_text],
    ).map_err(sql)?;
    if count > 0 {
        conn.execute(
            "UPDATE history_record SET edited_text=?2, edit_source='observed', edited_at=?3 WHERE id=?1",
            params![record.record_id, record.edited_text, record.last_observed_at],
        )
        .map_err(sql)?;
    }
    Ok(count > 0)
}

/// Sets a dictation's final text from either history view. Text equal to the
/// output VoiceX produced removes the edit; anything else becomes the manual
/// final version and its edit-history pair. New pairs are only created for
/// ordinary dictation while edit records are enabled.
pub fn set_final_text(source_id: &str, text: &str) -> Result<FinalText, StorageError> {
    if text.trim().is_empty() {
        return Err(StorageError::InvalidInput("text is empty".into()));
    }
    let policy = POLICY.lock().unwrap();
    let records_enabled = policy.settings.mode == Mode::Records;
    let result = storage::with_db(|conn| {
        let tx = conn.unchecked_transaction().map_err(sql)?;
        let result = apply_final_text(&tx, source_id, text, records_enabled, Utc::now())?;
        tx.commit().map_err(sql)?;
        Ok(result)
    })?;
    drop(policy);
    changed();
    history_changed(source_id);
    Ok(result)
}

fn apply_final_text(
    conn: &Connection,
    source_id: &str,
    text: &str,
    records_enabled: bool,
    now: chrono::DateTime<Utc>,
) -> Result<FinalText, StorageError> {
    let (output, mode, at): (String, String, String) = conn
        .query_row(
            "SELECT text, mode, timestamp FROM history_record WHERE id=?1",
            [source_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(sql)?
        .ok_or_else(|| StorageError::NotFound(source_id.into()))?;
    let entry: Option<Record> = conn
        .query_row(
            "SELECT payload FROM post_edit_record WHERE source_id=?1",
            [source_id],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .map_err(sql)?
        .map(|payload| serde_json::from_str(&payload).map_err(json))
        .transpose()?;
    if text == output {
        conn.execute(
            "UPDATE history_record SET edited_text=NULL, edit_source=NULL, edited_at=NULL WHERE id=?1",
            [source_id],
        )
        .map_err(sql)?;
        // Reverting is an explicit "no edit": the tombstone also stops an
        // observer still running for this dictation from recreating one.
        if let Some(entry) = &entry {
            delete_comparison(conn, &entry.id, Some(source_id))?;
        }
        return Ok(FinalText { edited_text: None });
    }
    let now_text = timestamp(now);
    conn.execute(
        "UPDATE history_record SET edited_text=?2, edit_source='manual', edited_at=?3 WHERE id=?1",
        params![source_id, text, now_text],
    )
    .map_err(sql)?;
    let edited = FinalText {
        edited_text: Some(text.to_string()),
    };
    let mut record = match entry {
        Some(record) => record,
        None if !records_enabled || !mode.starts_with(DICTATION_MODE_PREFIX) => return Ok(edited),
        None => {
            conn.execute(
                "DELETE FROM post_edit_deleted WHERE kind='source' AND id=?1",
                [source_id],
            )
            .map_err(sql)?;
            let at = chrono::DateTime::parse_from_rfc3339(&at)
                .map(|at| timestamp(at.with_timezone(&Utc)))
                .unwrap_or_else(|_| now_text.clone());
            Record {
                id: uuid::Uuid::new_v4().to_string(),
                record_id: source_id.into(),
                session_id: String::new(),
                timestamp: at,
                last_observed_at: now_text.clone(),
                app_name: None,
                original_text: String::new(),
                edited_text: String::new(),
                complete: true,
                end_reason: "none".into(),
                strategy: "manual".into(),
                range_evidence: "manual".into(),
                insertions: None,
                deletions: None,
                substitutions: None,
                had_intermediate_changes: false,
                has_newer_unconfirmed_input: false,
                stable: true,
                composition: super::platform::CompositionState::Inactive,
                source: Source::Manual,
            }
        }
    };
    let counts = super::diff::analyze(&output, text).ok();
    record.source = Source::Manual;
    record.complete = true;
    record.stable = true;
    record.has_newer_unconfirmed_input = false;
    record.last_observed_at = now_text;
    record.insertions = counts.map(|c| c.insertions);
    record.deletions = counts.map(|c| c.deletions);
    record.substitutions = counts.map(|c| c.substitutions);
    conn.execute(
        "INSERT INTO post_edit_record(id,source_id,timestamp,payload,manual)
         VALUES (?1,?2,?3,?4,1)
         ON CONFLICT(source_id) DO UPDATE SET id=excluded.id, payload=excluded.payload, manual=1",
        params![record.id, record.record_id, record.timestamp, metadata(&record)?],
    )
    .map_err(sql)?;
    Ok(edited)
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
    if matches!(result, Ok(true)) {
        history_changed(&record.record_id);
    }
    result.map(|_| ())
}

pub fn list(limit: u32, offset: u32) -> Result<RecordPage, StorageError> {
    let records = storage::with_db(|conn| {
        cleanup(conn)?;
        let mut stmt = conn
            .prepare(
                "SELECT p.payload, h.text, h.edited_text FROM post_edit_record p
                 JOIN history_record h ON h.id = p.source_id
                 WHERE h.edited_text IS NOT NULL
                 ORDER BY p.timestamp DESC, p.id DESC LIMIT ?1 OFFSET ?2",
            )
            .map_err(sql)?;
        let rows = stmt
            .query_map(params![limit.clamp(1, 100), offset], |r| {
                Ok((r.get::<_, String>(0)?, r.get(1)?, r.get(2)?))
            })
            .map_err(sql)?;
        rows.map(|row| {
            let (payload, original, edited) = row.map_err(sql)?;
            let mut record: Record = serde_json::from_str(&payload).map_err(json)?;
            record.original_text = original;
            record.edited_text = edited;
            Ok(record)
        })
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
/// Deleting an edit entry discards the edit: the dictation returns to the
/// output VoiceX produced, the same as restoring the original by hand.
pub fn delete_record(id: &str, source: Option<&str>) -> Result<(), StorageError> {
    let source = storage::with_db(|conn| {
        let tx = conn.unchecked_transaction().map_err(sql)?;
        let source = discard_edit(&tx, id, source)?;
        tx.commit().map_err(sql)?;
        Ok(source)
    })?;
    changed();
    if let Some(source) = source {
        history_changed(&source);
    }
    Ok(())
}

fn discard_edit(
    conn: &Connection,
    id: &str,
    source: Option<&str>,
) -> Result<Option<String>, StorageError> {
    let source = match source {
        Some(source) => Some(source.to_string()),
        None => conn
            .query_row(
                "SELECT source_id FROM post_edit_record WHERE id=?1",
                [id],
                |row| row.get(0),
            )
            .optional()
            .map_err(sql)?,
    };
    if let Some(source) = &source {
        conn.execute(
            "UPDATE history_record SET edited_text=NULL, edit_source=NULL, edited_at=NULL WHERE id=?1",
            [source],
        )
        .map_err(sql)?;
    }
    delete_comparison(conn, id, source.as_deref())?;
    Ok(source)
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
fn history_changed(id: &str) {
    if let Some(app) = APP.get() {
        if let Err(error) = app.emit("history:updated", serde_json::json!({ "id": id })) {
            log::warn!("Could not notify dictation history: {error}");
        }
    }
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
    use super::super::platform::CompositionState;

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
            has_newer_unconfirmed_input: false,
            stable: true,
            composition: CompositionState::Unknown,
            source: Source::Observed,
        }
    }
    /// Dictations "source" and "other-source" output 返利.
    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE history_record (
                id TEXT PRIMARY KEY, timestamp TEXT NOT NULL, text TEXT NOT NULL, mode TEXT NOT NULL,
                edited_text TEXT, edit_source TEXT, edited_at TEXT
             );",
        )
        .unwrap();
        create_schema(&conn).unwrap();
        for id in ["source", "other-source"] {
            dictation(&conn, id, "assistant_raw", Utc::now());
        }
        conn
    }
    fn dictation(conn: &Connection, id: &str, mode: &str, at: chrono::DateTime<Utc>) {
        conn.execute(
            "INSERT INTO history_record(id,timestamp,text,mode) VALUES (?1,?2,'返利',?3)",
            params![id, at.to_rfc3339(), mode],
        )
        .unwrap();
    }
    fn final_text(conn: &Connection, id: &str) -> (Option<String>, Option<String>) {
        conn.query_row(
            "SELECT edited_text, edit_source FROM history_record WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    }
    fn listed(conn: &Connection) -> Vec<Record> {
        let mut stmt = conn
            .prepare(
                "SELECT p.payload, h.text, h.edited_text FROM post_edit_record p
                 JOIN history_record h ON h.id = p.source_id WHERE h.edited_text IS NOT NULL
                 ORDER BY p.timestamp DESC",
            )
            .unwrap();
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get(1)?, r.get(2)?)))
            .unwrap();
        rows.map(|row| {
            let (payload, original, edited) = row.unwrap();
            let mut record: Record = serde_json::from_str(&payload).unwrap();
            (record.original_text, record.edited_text) = (original, edited);
            record
        })
        .collect()
    }
    fn entry(conn: &Connection, id: &str) -> Option<Record> {
        listed(conn).into_iter().find(|r| r.record_id == id)
    }

    #[test]
    fn opt_in_ticket_is_revoked_by_mode_changes() {
        let mut policy = Policy::default();
        assert!(policy.ticket().is_none());
        let legacy: Settings = serde_json::from_str(r#"{"mode":"metrics_only"}"#).unwrap();
        assert_eq!(legacy.mode, Mode::Off);
        policy.settings.mode = Mode::Records;
        let ticket = policy.ticket().unwrap();
        assert!(policy.accepts(ticket));
        policy.generation += 1;
        assert!(!policy.accepts(ticket));
        policy.settings.mode = Mode::Off;
        assert!(!policy.accepts(policy.ticket().unwrap_or(ticket)));
    }

    #[test]
    fn observed_edit_is_stored_once_as_the_dictations_final_text() {
        let conn = db();
        assert!(insert(&conn, &record()).unwrap());
        assert_eq!(final_text(&conn, "source"), (Some("反力".into()), Some("observed".into())));
        let payload: String = conn
            .query_row("SELECT payload FROM post_edit_record", [], |r| r.get(0))
            .unwrap();
        assert!(!payload.contains("反力") && !payload.contains("返利"));
        let listed = entry(&conn, "source").unwrap();
        assert_eq!((listed.original_text.as_str(), listed.edited_text.as_str()), ("返利", "反力"));
        assert!(!listed.complete);
    }

    #[test]
    fn observed_edit_needs_the_dictation_and_its_actual_output() {
        let conn = db();
        let mut missing = record();
        missing.record_id = "not-persisted".into();
        assert!(!insert(&conn, &missing).unwrap());
        // A superseded injection of this dictation typed different text.
        let mut superseded = record();
        superseded.original_text = "旧的返利".into();
        assert!(!insert(&conn, &superseded).unwrap());
        assert_eq!(final_text(&conn, "source"), (None, None));
    }

    #[test]
    fn deletions_block_in_flight_writes_and_do_not_affect_other_sources() {
        for kind in ["record", "source"] {
            let conn = db();
            let record = record();
            assert!(insert(&conn, &record).unwrap());
            let id = if kind == "record" { &record.id } else { &record.record_id };
            delete(&conn, kind, id).unwrap();
            assert!(!insert(&conn, &record).unwrap());
            let mut other = record.clone();
            other.id = "other-injection".into();
            other.record_id = "other-source".into();
            assert!(insert(&conn, &other).unwrap());
            assert_eq!(listed(&conn).len(), 1);
            // Removing the entry keeps the dictation's final text.
            assert_eq!(final_text(&conn, "source").0.as_deref(), Some("反力"));
        }
    }

    #[test]
    fn entries_follow_the_dictation_not_a_fixed_age() {
        let conn = db();
        let mut record = record();
        record.edited_text = record.original_text.clone();
        assert!(!insert(&conn, &record).unwrap());
        record.edited_text = "反力".into();
        // A write this old can only be a stale observer behind a tombstone.
        record.timestamp = timestamp(Utc::now() - Duration::days(TOMBSTONE_DAYS + 1));
        assert!(!insert(&conn, &record).unwrap());
        record.timestamp = timestamp(Utc::now());
        assert!(insert(&conn, &record).unwrap());
        conn.execute(
            "UPDATE post_edit_record SET timestamp=?1",
            [timestamp(Utc::now() - Duration::days(400))],
        )
        .unwrap();
        cleanup(&conn).unwrap();
        assert_eq!(listed(&conn).len(), 1);
        delete_source(&conn, "source").unwrap();
        assert!(listed(&conn).is_empty());
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
        let entries = listed(&conn);
        assert_eq!(entries.len(), 1);
        assert_eq!((entries[0].id.as_str(), entries[0].edited_text.as_str()), ("latest-injection", "最终反力"));
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
        assert!(listed(&conn).is_empty());
    }

    #[test]
    fn unstable_or_composing_text_is_not_saved_and_legacy_stability_is_unknown() {
        let conn = db();
        let mut record = record();
        record.stable = false;
        assert!(!insert(&conn, &record).unwrap());
        record.stable = true;
        record.composition = CompositionState::Active;
        assert!(!insert(&conn, &record).unwrap());
        let mut legacy = serde_json::to_value(&record).unwrap();
        for key in ["stable", "composition", "hasNewerUnconfirmedInput", "source"] {
            legacy.as_object_mut().unwrap().remove(key);
        }
        let loaded: Record = serde_json::from_value(legacy).unwrap();
        assert!(!loaded.stable && !loaded.has_newer_unconfirmed_input);
        assert_eq!((loaded.composition, loaded.source), (CompositionState::Unknown, Source::Observed));
    }

    #[test]
    fn newer_unconfirmed_input_is_saved_only_as_partial() {
        let conn = db();
        let mut record = record();
        record.has_newer_unconfirmed_input = true;
        record.complete = true;
        assert!(!insert(&conn, &record).unwrap());
        record.complete = false;
        assert!(insert(&conn, &record).unwrap());
        let loaded = entry(&conn, "source").unwrap();
        assert!(loaded.has_newer_unconfirmed_input && !loaded.complete);
    }

    #[test]
    fn manual_edits_win_over_later_observations() {
        let conn = db();
        assert!(insert(&conn, &record()).unwrap());
        apply_final_text(&conn, "source", "反力学", true, Utc::now()).unwrap();
        let mut late = record();
        late.id = "late".into();
        late.timestamp = timestamp(Utc::now() + Duration::seconds(5));
        late.edited_text = "观察".into();
        assert!(!insert(&conn, &late).unwrap());
        // Hand-edited before any observation finished: none is stored either.
        apply_final_text(&conn, "other-source", "反力", false, Utc::now()).unwrap();
        let mut early = record();
        early.record_id = "other-source".into();
        assert!(!insert(&conn, &early).unwrap());
        assert_eq!(final_text(&conn, "source"), (Some("反力学".into()), Some("manual".into())));
        let stored = entry(&conn, "source").unwrap();
        assert_eq!((stored.edited_text.as_str(), stored.source), ("反力学", Source::Manual));
        assert_eq!(
            (stored.app_name.as_deref(), stored.substitutions, stored.insertions),
            (Some("Fixture"), Some(2), Some(1))
        );
    }

    #[test]
    fn manual_edit_creates_an_entry_only_for_enabled_dictation() {
        let conn = db();
        dictation(&conn, "translated", "translate_en", Utc::now());
        apply_final_text(&conn, "source", "反力", true, Utc::now()).unwrap();
        apply_final_text(&conn, "translated", "rebate", true, Utc::now()).unwrap();
        apply_final_text(&conn, "other-source", "反力", false, Utc::now()).unwrap();
        let created = entry(&conn, "source").unwrap();
        assert_eq!((created.substitutions, created.complete), (Some(2), true));
        assert_eq!(listed(&conn).len(), 1);
        for id in ["source", "translated", "other-source"] {
            assert_eq!(final_text(&conn, id).1.as_deref(), Some("manual"));
        }
        assert!(apply_final_text(&conn, "missing", "x", true, Utc::now()).is_err());
    }

    #[test]
    fn reverting_to_the_output_removes_the_edit_and_blocks_observers() {
        let conn = db();
        assert!(insert(&conn, &record()).unwrap());
        let result = apply_final_text(&conn, "source", "返利", true, Utc::now()).unwrap();
        assert!(result.edited_text.is_none());
        assert_eq!(final_text(&conn, "source"), (None, None));
        assert!(listed(&conn).is_empty());
        let mut late = record();
        late.id = "late".into();
        assert!(!insert(&conn, &late).unwrap());
        // An explicit edit after deletion is the user's choice and is kept.
        apply_final_text(&conn, "source", "反力", true, Utc::now()).unwrap();
        assert_eq!(entry(&conn, "source").unwrap().edited_text, "反力");
    }

    #[test]
    fn legacy_entries_move_their_text_to_the_dictation() {
        let conn = db();
        dictation(&conn, "superseded", "assistant_raw", Utc::now());
        let legacy = |id: &str, source: &str, original: &str| {
            let mut value = serde_json::to_value(record()).unwrap();
            let fields = value.as_object_mut().unwrap();
            fields.remove("source");
            fields.insert("recordId".into(), source.into());
            fields.insert("originalText".into(), original.into());
            conn.execute(
                "INSERT INTO post_edit_record(id,source_id,timestamp,payload) VALUES (?1,?2,?3,?4)",
                params![id, source, timestamp(Utc::now()), value.to_string()],
            )
            .unwrap();
        };
        legacy("kept", "source", "返利");
        legacy("orphan", "deleted-dictation", "返利");
        legacy("mismatch", "superseded", "旧的返利");
        create_schema(&conn).unwrap();
        create_schema(&conn).unwrap();
        assert_eq!(final_text(&conn, "source"), (Some("反力".into()), Some("observed".into())));
        assert_eq!(final_text(&conn, "superseded"), (None, None));
        let count: usize = conn
            .query_row("SELECT COUNT(*) FROM post_edit_record", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
        let payload: String = conn
            .query_row("SELECT payload FROM post_edit_record", [], |r| r.get(0))
            .unwrap();
        assert!(!payload.contains("Text\""));
        assert_eq!(entry(&conn, "source").unwrap().edited_text, "反力");
    }

    #[test]
    fn deleting_an_entry_restores_the_output_and_blocks_observers() {
        let conn = db();
        assert!(insert(&conn, &record()).unwrap());
        apply_final_text(&conn, "other-source", "反力", true, Utc::now()).unwrap();
        // The view may hold an injection ID already replaced by a newer one.
        assert_eq!(discard_edit(&conn, "stale-injection", Some("source")).unwrap().as_deref(), Some("source"));
        assert_eq!(final_text(&conn, "source"), (None, None));
        let mut late = record();
        late.id = "late".into();
        assert!(!insert(&conn, &late).unwrap());
        let manual = entry(&conn, "other-source").unwrap();
        discard_edit(&conn, &manual.id, None).unwrap();
        assert_eq!(final_text(&conn, "other-source"), (None, None));
        assert!(listed(&conn).is_empty());
    }
}
