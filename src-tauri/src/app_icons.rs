//! Application icon extraction and local disk cache (`app_data_dir/app_icons/`).
//!
//! Icons are extracted as 64×64 PNG files and indexed in SQLite (`app_icon_index`)
//! by `(platform, app_key)`.
//! - When a foreground app is captured on hotkey press or injection, its metadata
//!   and icon are recorded asynchronously in the background without delaying
//!   recording start or HUD display.
//! - When the UI requests icons for statistics, injection settings, or history
//!   views, cached PNGs are served from disk; missing or updated apps (detected
//!   via path or modification time changes, without background timers) are
//!   extracted on demand and persisted.
//! - The disk cache is capped at 10 MiB. Startup and writes evict the least
//!   recently accessed PNGs while retaining app identities and source paths.
//! - Path guards avoid probing TCC-protected personal folders on macOS
//!   (`~/Desktop`, `~/Documents`, `~/Downloads`, iCloud Drive, removable volumes)
//!   or network shares on Windows, preventing OS permission/error popups.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::UNIX_EPOCH;

use base64::Engine as _;
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::foreground_app::ForegroundAppInfo;
use crate::storage::{self, StorageError};

pub const ICON_PIXEL_SIZE: u32 = 64;
const ICON_CACHE_MAX_BYTES: u64 = 10 * 1024 * 1024;

static ICONS_DIR: OnceLock<PathBuf> = OnceLock::new();
// Keep extraction, reads and eviction together so cleanup cannot remove a PNG
// between resolving its path and serving it to the UI.
static CACHE_IO: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppIconEntry {
    pub platform: String,
    pub app_key: String,
    pub app_name: String,
    pub bundle_id: Option<String>,
    pub executable_path: Option<String>,
    pub icon_file: Option<String>,
    pub source_path: Option<String>,
    pub source_mtime_secs: Option<i64>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppIconLookupRequest {
    pub request_key: String,
    pub platform: Option<String>,
    pub app_key: Option<String>,
    pub bundle_id: Option<String>,
    pub executable_path: Option<String>,
    pub app_name: Option<String>,
}

/// Create the `app_icon_index` table during database initialization.
pub fn create_schema(conn: &Connection) -> Result<(), StorageError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS app_icon_index (
            platform TEXT NOT NULL,
            app_key TEXT NOT NULL,
            app_name TEXT NOT NULL,
            bundle_id TEXT,
            executable_path TEXT,
            icon_file TEXT,
            source_path TEXT,
            source_mtime_secs INTEGER,
            updated_at TEXT NOT NULL,
            last_accessed_at TEXT NOT NULL DEFAULT '',
            PRIMARY KEY (platform, app_key)
        );
        CREATE INDEX IF NOT EXISTS app_icon_index_name_idx
            ON app_icon_index(app_name COLLATE NOCASE);",
    )
    .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
    let has_access_time = conn
        .prepare(
            "SELECT 1 FROM pragma_table_info('app_icon_index') WHERE name = 'last_accessed_at'",
        )
        .and_then(|mut stmt| stmt.exists([]))
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
    if !has_access_time {
        conn.execute_batch(
            "ALTER TABLE app_icon_index ADD COLUMN last_accessed_at TEXT NOT NULL DEFAULT '';",
        )
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
    }
    conn.execute(
        "UPDATE app_icon_index SET last_accessed_at = updated_at WHERE last_accessed_at = ''",
        [],
    )
    .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
    Ok(())
}

/// Initialize the on-disk icon cache directory (`<app_data_dir>/app_icons`)
/// and seed `app_icon_index` metadata from existing recent apps and overrides.
pub fn init(app_data_dir: &Path) -> Result<(), StorageError> {
    let dir = app_data_dir.join("app_icons");
    std::fs::create_dir_all(&dir).map_err(|e| {
        StorageError::ConnectionFailed(format!("Failed to create app_icons directory: {e}"))
    })?;
    let _ = ICONS_DIR.set(dir.clone());
    seed_known_apps_from_storage();
    std::thread::spawn(move || {
        let result = CACHE_IO
            .lock()
            .map_err(|e| format!("Failed to lock app icon cache: {e}"))
            .and_then(|_guard| cleanup_icon_cache_in_dir(&dir));
        if let Err(err) = result {
            log::warn!("Failed to clean app icon cache at startup: {err}");
        }
    });
    Ok(())
}

fn icons_dir() -> Option<&'static Path> {
    ICONS_DIR.get().map(PathBuf::as_path)
}

fn normalize_platform(platform: &str) -> String {
    let lower = platform.trim().to_ascii_lowercase();
    if lower.is_empty() {
        current_platform_name().to_string()
    } else {
        lower
    }
}

fn current_platform_name() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        return "macos";
    }
    #[cfg(target_os = "windows")]
    {
        return "windows";
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        return "unknown";
    }
}

fn normalize_key(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

fn non_empty_trimmed(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Deterministic, filesystem-safe filename for `(platform, app_key)`.
pub fn icon_filename_for_key(platform: &str, app_key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(normalize_platform(platform).as_bytes());
    hasher.update(b":");
    hasher.update(normalize_key(app_key).as_bytes());
    let digest = hasher.finalize();
    format!("{}.png", hex::encode(&digest[..16]))
}

/// Seed `app_icon_index` with metadata from `recent_target_apps` and
/// `text_injection_overrides` so existing known paths are preserved even after
/// `recent_target_apps` rotates past its 5-entry limit.
fn seed_known_apps_from_storage() {
    if let Ok(recent_apps) = storage::get_recent_target_apps() {
        for recent in recent_apps {
            let info = ForegroundAppInfo {
                platform: recent.platform.clone(),
                display_name: recent
                    .display_name
                    .clone()
                    .or(Some(recent.app_name.clone())),
                process_name: recent.process_name.clone(),
                bundle_id: recent.bundle_id.clone(),
                executable_path: recent.executable_path.clone(),
                process_id: 0,
                is_self: false,
            };
            if let Some(identity) = info.to_target_app_identity() {
                let _ = upsert_metadata_only(
                    &identity.platform,
                    &identity.app_key,
                    &identity.app_name,
                    recent.bundle_id.as_deref(),
                    recent.executable_path.as_deref(),
                );
            }
        }
    }

    if let Ok(settings) = storage::get_settings() {
        for item in settings.text_injection_overrides {
            let platform = normalize_platform(&item.platform);
            let bundle_id = (item.match_kind == "bundle_id").then_some(item.match_value.as_str());
            let executable_path =
                (item.match_kind == "executable_path").then_some(item.match_value.as_str());
            let app_key = if platform == "macos" {
                bundle_id
                    .map(normalize_key)
                    .unwrap_or_else(|| normalize_key(&item.match_value))
            } else if item.match_kind == "executable_path" {
                Path::new(&item.match_value)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(normalize_key)
                    .unwrap_or_else(|| normalize_key(&item.match_value))
            } else {
                normalize_key(&item.match_value)
            };
            if !app_key.is_empty() {
                let _ = upsert_metadata_only(
                    &platform,
                    &app_key,
                    &item.app_name,
                    bundle_id,
                    executable_path,
                );
            }
        }
    }
}

fn upsert_metadata_only(
    platform: &str,
    app_key: &str,
    app_name: &str,
    bundle_id: Option<&str>,
    executable_path: Option<&str>,
) -> Result<(), StorageError> {
    let platform = normalize_platform(platform);
    let app_key = normalize_key(app_key);
    if app_key.is_empty() {
        return Ok(());
    }
    let now = Utc::now().to_rfc3339();
    let clean_name = app_name.trim();
    let bundle_id = non_empty_trimmed(bundle_id);
    let executable_path = non_empty_trimmed(executable_path);

    storage::with_db(|conn| {
        conn.execute(
            "INSERT INTO app_icon_index (
                platform, app_key, app_name, bundle_id, executable_path, updated_at, last_accessed_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
            ON CONFLICT(platform, app_key) DO UPDATE SET
                app_name = CASE
                    WHEN excluded.app_name != '' THEN excluded.app_name
                    ELSE app_icon_index.app_name
                END,
                bundle_id = COALESCE(excluded.bundle_id, app_icon_index.bundle_id),
                executable_path = COALESCE(excluded.executable_path, app_icon_index.executable_path)",
            params![platform, app_key, clean_name, bundle_id, executable_path, now],
        )
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
        Ok(())
    })
}

/// Spawn a non-blocking background task to record and cache the icon for `app`.
pub fn record_foreground_app_async(app: &ForegroundAppInfo) {
    if app.is_self {
        return;
    }
    let cloned = app.clone();
    std::thread::spawn(move || {
        if let Err(err) = record_and_cache_foreground_app(&cloned) {
            log::warn!("Failed to cache foreground app icon: {err}");
        }
    });
}

/// Synchronously record `ForegroundAppInfo` metadata and extract/refresh its
/// cached PNG if missing or if the application binary/bundle changed on disk.
pub fn record_and_cache_foreground_app(app: &ForegroundAppInfo) -> Result<Option<PathBuf>, String> {
    if app.is_self {
        return Ok(None);
    }
    let Some(identity) = app.to_target_app_identity() else {
        return Ok(None);
    };
    let _guard = CACHE_IO
        .lock()
        .map_err(|e| format!("Failed to lock app icon cache: {e}"))?;

    upsert_metadata_only(
        &identity.platform,
        &identity.app_key,
        &identity.app_name,
        app.bundle_id.as_deref(),
        app.executable_path.as_deref(),
    )
    .map_err(|e| e.to_string())?;

    let Some(dir) = icons_dir() else {
        return Ok(None);
    };

    ensure_cached_icon_in_dir(
        dir,
        &identity.platform,
        &identity.app_key,
        &identity.app_name,
        app.bundle_id.as_deref(),
        app.executable_path.as_deref(),
    )
}

/// Batch-resolve icons for UI components. Returns a map of `request_key -> data:image/png;base64,...`.
pub fn resolve_app_icons(
    requests: &[AppIconLookupRequest],
) -> Result<HashMap<String, String>, String> {
    let Some(dir) = icons_dir() else {
        return Ok(HashMap::new());
    };
    resolve_app_icons_in_dir(dir, requests)
}

pub fn resolve_app_icons_in_dir(
    dir: &Path,
    requests: &[AppIconLookupRequest],
) -> Result<HashMap<String, String>, String> {
    let _guard = CACHE_IO
        .lock()
        .map_err(|e| format!("Failed to lock app icon cache: {e}"))?;
    let mut results = HashMap::with_capacity(requests.len());

    for req in requests {
        if req.request_key.trim().is_empty() {
            continue;
        }
        let png_path = match resolve_single_request_in_dir(dir, req) {
            Ok(Some(path)) => path,
            Ok(None) => continue,
            Err(err) => {
                log::warn!("Failed to resolve app icon {}: {err}", req.request_key);
                continue;
            }
        };
        match std::fs::read(&png_path) {
            Ok(bytes) if !bytes.is_empty() => {
                let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                results.insert(
                    req.request_key.clone(),
                    format!("data:image/png;base64,{b64}"),
                );
            }
            Ok(_) => {}
            Err(err) => {
                log::warn!("Failed to read cached app icon {:?}: {}", png_path, err);
            }
        }
    }

    Ok(results)
}

fn resolve_single_request_in_dir(
    dir: &Path,
    req: &AppIconLookupRequest,
) -> Result<Option<PathBuf>, String> {
    let platform = req
        .platform
        .as_deref()
        .map(normalize_platform)
        .unwrap_or_else(|| current_platform_name().to_string());

    let explicit_bundle_id = non_empty_trimmed(req.bundle_id.as_deref());
    let explicit_exe_path = non_empty_trimmed(req.executable_path.as_deref());
    let explicit_app_name = non_empty_trimmed(req.app_name.as_deref()).unwrap_or_default();

    // 1. Determine canonical app_key if possible, or look up existing index entry by name/path.
    let mut app_key = non_empty_trimmed(req.app_key.as_deref()).map(|k| normalize_key(&k));
    if app_key.is_none() {
        if platform == "macos" {
            app_key = explicit_bundle_id.as_deref().map(normalize_key);
        }
        if app_key.is_none() {
            if let Some(ref exe) = explicit_exe_path {
                app_key = Path::new(exe)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(normalize_key);
            }
        }
    }

    // Legacy records may only have an app name. New history/replay requests
    // supply the stable platform/app_key identity instead.
    if app_key.is_none() && !explicit_app_name.is_empty() {
        if let Some(entry) = storage::with_db(|conn| {
            find_entry_by_name_in(conn, &explicit_app_name, req.platform.as_deref())
        })
        .map_err(|e| e.to_string())?
        {
            return ensure_cached_icon_in_dir(
                dir,
                &entry.platform,
                &entry.app_key,
                &entry.app_name,
                entry.bundle_id.as_deref(),
                entry.executable_path.as_deref(),
            );
        }
    }

    let Some(app_key) = app_key else {
        return Ok(None);
    };
    if app_key.is_empty() || app_key == crate::storage::UNTRACKED_APP_KEY {
        return Ok(None);
    }

    ensure_cached_icon_in_dir(
        dir,
        &platform,
        &app_key,
        &explicit_app_name,
        explicit_bundle_id.as_deref(),
        explicit_exe_path.as_deref(),
    )
}

fn ensure_cached_icon_in_dir(
    dir: &Path,
    platform: &str,
    app_key: &str,
    app_name: &str,
    bundle_id_hint: Option<&str>,
    executable_path_hint: Option<&str>,
) -> Result<Option<PathBuf>, String> {
    let platform = normalize_platform(platform);
    let app_key = normalize_key(app_key);
    if app_key.is_empty() {
        return Ok(None);
    }

    let existing = get_entry(&platform, &app_key).map_err(|e| e.to_string())?;

    let merged_bundle_id = non_empty_trimmed(bundle_id_hint)
        .or_else(|| existing.as_ref().and_then(|e| e.bundle_id.clone()))
        .or_else(|| {
            // On macOS, daily_usage_stats stores bundle_id directly in app_key.
            if platform == "macos" && app_key.contains('.') && !app_key.ends_with(".app") {
                Some(app_key.clone())
            } else {
                None
            }
        });

    let merged_exe_path = non_empty_trimmed(executable_path_hint)
        .or_else(|| existing.as_ref().and_then(|e| e.executable_path.clone()));

    let merged_name = if !app_name.trim().is_empty() {
        app_name.trim().to_string()
    } else if let Some(ref e) = existing {
        e.app_name.clone()
    } else {
        app_key.clone()
    };

    // Check if the current platform can inspect the live source path/mtime.
    let current_source = if platform == current_platform_name() {
        resolve_local_source(
            &platform,
            &app_key,
            merged_bundle_id.as_deref(),
            merged_exe_path.as_deref(),
        )
    } else {
        None
    };

    // Determine whether the existing cached PNG on disk is still valid.
    if let Some(ref entry) = existing {
        if let Some(ref icon_file) = entry.icon_file {
            let cached_path = dir.join(icon_file);
            if cached_path.is_file() {
                let source_changed =
                    match (&current_source, &entry.source_path, entry.source_mtime_secs) {
                        (Some((cur_path, cur_mtime)), Some(old_path), old_mtime) => {
                            cur_path != old_path || (cur_mtime.is_some() && *cur_mtime != old_mtime)
                        }
                        _ => false,
                    };
                if !source_changed {
                    storage::with_db(|conn| touch_cached_icon_in(conn, &platform, &app_key))
                        .map_err(|e| e.to_string())?;
                    return Ok(Some(cached_path));
                }
            }
        }
    }

    // Cannot extract an icon from another OS on this machine.
    if platform != current_platform_name() {
        return Ok(None);
    }

    let extracted = extract_platform_icon_png(
        &app_key,
        merged_bundle_id.as_deref(),
        merged_exe_path.as_deref(),
        current_source.as_ref().map(|(p, _)| p.as_str()),
    )?;

    let Some((png_bytes, resolved_source_path)) = extracted else {
        return Ok(None);
    };

    let mtime_secs = resolved_source_path
        .as_deref()
        .and_then(|p| source_mtime_secs(Path::new(p)))
        .or_else(|| current_source.and_then(|(_, m)| m));

    let file_name = icon_filename_for_key(&platform, &app_key);
    let final_path = dir.join(&file_name);
    write_png_atomically(dir, &final_path, &png_bytes)?;

    let entry = AppIconEntry {
        platform,
        app_key,
        app_name: merged_name,
        bundle_id: merged_bundle_id,
        executable_path: merged_exe_path,
        icon_file: Some(file_name),
        source_path: resolved_source_path,
        source_mtime_secs: mtime_secs,
        updated_at: Utc::now().to_rfc3339(),
    };
    upsert_entry(&entry).map_err(|e| e.to_string())?;
    cleanup_icon_cache_in_dir(dir)?;

    Ok(Some(final_path))
}

fn write_png_atomically(dir: &Path, final_path: &Path, bytes: &[u8]) -> Result<(), String> {
    std::fs::create_dir_all(dir)
        .map_err(|e| format!("Failed to create app_icons directory: {e}"))?;
    let mut temp = tempfile::NamedTempFile::new_in(dir)
        .map_err(|e| format!("Failed to create temporary icon file: {e}"))?;
    temp.write_all(bytes)
        .map_err(|e| format!("Failed to write temporary icon file: {e}"))?;
    temp.persist(final_path)
        .map_err(|e| format!("Failed to persist icon file {:?}: {}", final_path, e.error))?;
    Ok(())
}

pub fn get_entry(platform: &str, app_key: &str) -> Result<Option<AppIconEntry>, StorageError> {
    let platform = normalize_platform(platform);
    let app_key = normalize_key(app_key);
    storage::with_db(|conn| {
        conn.query_row(
            "SELECT platform, app_key, app_name, bundle_id, executable_path,
                    icon_file, source_path, source_mtime_secs, updated_at
             FROM app_icon_index
             WHERE platform = ?1 AND app_key = ?2
             LIMIT 1",
            params![platform, app_key],
            row_to_entry,
        )
        .optional()
        .map_err(|e| StorageError::QueryFailed(e.to_string()))
    })
}

fn find_entry_by_name_in(
    conn: &Connection,
    app_name: &str,
    platform: Option<&str>,
) -> Result<Option<AppIconEntry>, StorageError> {
    let trimmed = app_name.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    // Legacy history/replay records may only have a process name (Code.exe),
    // while TargetAppIdentity stores its display name without the suffix (Code).
    let windows_name = crate::foreground_app::clean_app_display_name("windows", trimmed);
    let windows_key = format!("{}.exe", normalize_key(&windows_name));
    let platform = platform
        .filter(|p| !p.trim().is_empty())
        .map(normalize_platform);
    let cur_platform = current_platform_name();
    conn.query_row(
        "SELECT platform, app_key, app_name, bundle_id, executable_path,
                    icon_file, source_path, source_mtime_secs, updated_at
             FROM app_icon_index
             WHERE (?4 IS NULL OR platform = ?4)
               AND (app_name = ?1 COLLATE NOCASE
                    OR (platform = 'windows' AND
                        (app_name = ?2 COLLATE NOCASE OR app_key = ?3)))
             ORDER BY (platform = ?5) DESC, (icon_file IS NOT NULL) DESC, updated_at DESC
             LIMIT 1",
        params![trimmed, windows_name, windows_key, platform, cur_platform],
        row_to_entry,
    )
    .optional()
    .map_err(|e| StorageError::QueryFailed(e.to_string()))
}

pub fn upsert_entry(entry: &AppIconEntry) -> Result<(), StorageError> {
    storage::with_db(|conn| {
        conn.execute(
            "INSERT INTO app_icon_index (
                platform, app_key, app_name, bundle_id, executable_path,
                icon_file, source_path, source_mtime_secs, updated_at, last_accessed_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)
            ON CONFLICT(platform, app_key) DO UPDATE SET
                app_name = CASE
                    WHEN excluded.app_name != '' THEN excluded.app_name
                    ELSE app_icon_index.app_name
                END,
                bundle_id = COALESCE(excluded.bundle_id, app_icon_index.bundle_id),
                executable_path = COALESCE(excluded.executable_path, app_icon_index.executable_path),
                icon_file = COALESCE(excluded.icon_file, app_icon_index.icon_file),
                source_path = COALESCE(excluded.source_path, app_icon_index.source_path),
                source_mtime_secs = COALESCE(excluded.source_mtime_secs, app_icon_index.source_mtime_secs),
                updated_at = excluded.updated_at,
                last_accessed_at = excluded.last_accessed_at",
            params![
                normalize_platform(&entry.platform),
                normalize_key(&entry.app_key),
                entry.app_name.trim(),
                entry.bundle_id,
                entry.executable_path,
                entry.icon_file,
                entry.source_path,
                entry.source_mtime_secs,
                entry.updated_at,
            ],
        )
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
        Ok(())
    })
}

fn row_to_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<AppIconEntry> {
    Ok(AppIconEntry {
        platform: row.get(0)?,
        app_key: row.get(1)?,
        app_name: row.get(2)?,
        bundle_id: row.get(3)?,
        executable_path: row.get(4)?,
        icon_file: row.get(5)?,
        source_path: row.get(6)?,
        source_mtime_secs: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

fn touch_cached_icon_in(
    conn: &Connection,
    platform: &str,
    app_key: &str,
) -> Result<(), StorageError> {
    conn.execute(
        "UPDATE app_icon_index SET last_accessed_at = ?3 WHERE platform = ?1 AND app_key = ?2",
        params![platform, app_key, Utc::now().to_rfc3339()],
    )
    .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
    Ok(())
}

fn cached_icon_accesses(conn: &Connection) -> Result<HashMap<String, String>, StorageError> {
    let mut stmt = conn
        .prepare(
            "SELECT icon_file, last_accessed_at FROM app_icon_index WHERE icon_file IS NOT NULL",
        )
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
    let rows = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
    rows.collect::<Result<_, _>>()
        .map_err(|e| StorageError::QueryFailed(e.to_string()))
}

// Caller holds CACHE_IO. Filesystem work stays outside the database lock so a
// directory scan or deletion does not hold up recording/history database writes.
fn cleanup_icon_cache_in_dir(dir: &Path) -> Result<(), String> {
    let accesses = storage::with_db(cached_icon_accesses).map_err(|e| e.to_string())?;
    let removed = prune_icon_cache_files(dir, &accesses, ICON_CACHE_MAX_BYTES)?;
    if !removed.is_empty() {
        storage::with_db(|conn| clear_cached_icon_files(conn, &removed))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn prune_icon_cache_files(
    dir: &Path,
    accesses: &HashMap<String, String>,
    max_bytes: u64,
) -> Result<Vec<String>, String> {
    let mut files = Vec::new();
    let mut seen = HashSet::new();
    let mut total_bytes = 0u64;
    for entry in
        std::fs::read_dir(dir).map_err(|e| format!("Failed to scan app icon cache: {e}"))?
    {
        let entry = entry.map_err(|e| format!("Failed to read app icon cache entry: {e}"))?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("png")
            || !entry.file_type().map_err(|e| e.to_string())?.is_file()
        {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let size = entry.metadata().map_err(|e| e.to_string())?.len();
        total_bytes += size;
        seen.insert(name.clone());
        // Unindexed PNGs left by an interrupted write are evicted first.
        let accessed_at = accesses.get(&name).cloned().unwrap_or_default();
        files.push((accessed_at, name, size));
    }

    let mut removed: Vec<String> = accesses
        .keys()
        .filter(|name| !seen.contains(*name))
        .cloned()
        .collect();
    if total_bytes > max_bytes {
        files.sort_unstable();
        for (_, name, size) in files {
            if total_bytes <= max_bytes {
                break;
            }
            std::fs::remove_file(dir.join(&name))
                .map_err(|e| format!("Failed to evict app icon {name}: {e}"))?;
            total_bytes -= size;
            removed.push(name);
        }
    }
    Ok(removed)
}

fn clear_cached_icon_files(conn: &Connection, files: &[String]) -> Result<(), StorageError> {
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
    {
        let mut stmt = tx
            .prepare("UPDATE app_icon_index SET icon_file = NULL WHERE icon_file = ?1")
            .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
        for file in files {
            stmt.execute([file])
                .map_err(|e| StorageError::QueryFailed(e.to_string()))?;
        }
    }
    tx.commit()
        .map_err(|e| StorageError::QueryFailed(e.to_string()))
}

/// Check whether `path` is safe to probe on disk without triggering a macOS TCC
/// folder-access prompt (`~/Desktop`, `~/Documents`, `~/Downloads`, iCloud Drive,
/// removable volumes) or blocking on a Windows UNC network share (`\\server\...`).
pub fn is_safe_probe_path(path: &Path) -> bool {
    let raw = path.to_string_lossy();
    if raw.trim().is_empty() {
        return false;
    }

    // Never probe Windows UNC paths (`\\server\share\...`) or extended UNC (`\\?\UNC\...`).
    let normalized_slashes = raw.replace('\\', "/");
    if normalized_slashes.starts_with("//")
        || normalized_slashes
            .to_ascii_lowercase()
            .starts_with("//?/unc/")
    {
        return false;
    }

    let lower = normalized_slashes.to_ascii_lowercase();

    // Never probe removable/mounted volumes on macOS (`/Volumes/...`).
    if lower.starts_with("/volumes/") {
        return false;
    }

    // Never probe TCC-protected user directories under `/Users/<user>/...`.
    if let Some(after_users) = lower.strip_prefix("/users/") {
        if let Some((_user, rest)) = after_users.split_once('/') {
            const TCC_PROTECTED_PREFIXES: [&str; 8] = [
                "desktop/",
                "desktop",
                "documents/",
                "documents",
                "downloads/",
                "downloads",
                "library/mobile documents",
                "library/cloudstorage",
            ];
            if TCC_PROTECTED_PREFIXES
                .iter()
                .any(|prefix| rest.starts_with(prefix))
            {
                return false;
            }
        }
    }

    true
}

/// Extract outer `.app` bundle path from a macOS executable path
/// (e.g. `/Applications/Foo.app/Contents/MacOS/Foo` -> `/Applications/Foo.app`).
pub fn find_macos_app_bundle_path(path: &Path) -> Option<PathBuf> {
    let mut best: Option<PathBuf> = None;
    for ancestor in path.ancestors() {
        if ancestor
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("app"))
        {
            best = Some(ancestor.to_path_buf());
        }
    }
    best
}

fn file_mtime_secs(path: &Path) -> Option<i64> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?;
    let dur = modified.duration_since(UNIX_EPOCH).ok()?;
    Some(dur.as_secs() as i64)
}

fn source_mtime_secs(path: &Path) -> Option<i64> {
    if !is_safe_probe_path(path) {
        return None;
    }
    let base_mtime = file_mtime_secs(path);
    if path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("app"))
    {
        let plist_mtime = file_mtime_secs(&path.join("Contents/Info.plist"));
        return match (base_mtime, plist_mtime) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };
    }
    base_mtime
}

fn resolve_local_source(
    _platform: &str,
    app_key: &str,
    bundle_id: Option<&str>,
    executable_path: Option<&str>,
) -> Option<(String, Option<i64>)> {
    #[cfg(target_os = "macos")]
    {
        if let Some(bundle_path) =
            macos::locate_app_bundle_path(app_key, bundle_id, executable_path)
        {
            let mtime = source_mtime_secs(&bundle_path);
            return Some((bundle_path.to_string_lossy().to_string(), mtime));
        }
        return None;
    }

    #[cfg(target_os = "windows")]
    {
        let exe = executable_path?;
        let path = Path::new(exe);
        if !is_safe_probe_path(path) || !path.is_file() {
            return None;
        }
        let mtime = source_mtime_secs(path);
        return Some((exe.to_string(), mtime));
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = (app_key, bundle_id, executable_path);
        None
    }
}

fn extract_platform_icon_png(
    app_key: &str,
    bundle_id: Option<&str>,
    executable_path: Option<&str>,
    resolved_source: Option<&str>,
) -> Result<Option<(Vec<u8>, Option<String>)>, String> {
    #[cfg(target_os = "macos")]
    {
        return macos::extract_icon_png(app_key, bundle_id, executable_path, resolved_source);
    }

    #[cfg(target_os = "windows")]
    {
        let _ = (app_key, bundle_id);
        let Some(exe) = resolved_source.or(executable_path) else {
            return Ok(None);
        };
        return windows::extract_icon_png(Path::new(exe))
            .map(|opt| opt.map(|bytes| (bytes, Some(exe.to_string()))));
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = (app_key, bundle_id, executable_path, resolved_source);
        Ok(None)
    }
}

/// Encode an RGBA pixel buffer (`width * height * 4` bytes) into a PNG byte vector.
pub fn encode_rgba_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, String> {
    let expected_len = (width as usize)
        .checked_mul(height as usize)
        .and_then(|px| px.checked_mul(4))
        .ok_or_else(|| "Invalid icon dimensions".to_string())?;
    if rgba.len() != expected_len {
        return Err(format!(
            "RGBA buffer length mismatch: expected {expected_len}, got {}",
            rgba.len()
        ));
    }

    let mut out = Vec::with_capacity(4096);
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|e| format!("Failed to write PNG header: {e}"))?;
        writer
            .write_image_data(rgba)
            .map_err(|e| format!("Failed to encode PNG image data: {e}"))?;
    }
    Ok(out)
}

/// Convert top-down BGRA pixel buffer (as produced by Windows `GetDIBits`) into RGBA,
/// applying a 1-bpp mask buffer if all alpha bytes are 0 (legacy non-alpha `HICON`).
pub fn bgra_to_rgba_with_fallback_mask(
    width: u32,
    height: u32,
    bgra: &[u8],
    mask_bgra: Option<&[u8]>,
) -> Vec<u8> {
    let mut rgba = vec![0u8; bgra.len()];
    let has_alpha = bgra.chunks_exact(4).any(|px| px[3] != 0);

    for (i, src) in bgra.chunks_exact(4).enumerate() {
        let dst = &mut rgba[i * 4..i * 4 + 4];
        dst[0] = src[2]; // R
        dst[1] = src[1]; // G
        dst[2] = src[0]; // B
        if has_alpha {
            dst[3] = src[3];
        } else if let Some(mask) = mask_bgra.and_then(|m| m.get(i * 4..i * 4 + 4)) {
            // In a Windows AND mask rendered as 32-bit RGB via GetDIBits, black (0) = opaque, non-zero = transparent.
            let transparent = mask[0] != 0 || mask[1] != 0 || mask[2] != 0;
            dst[3] = if transparent { 0 } else { 255 };
        } else {
            dst[3] = 255;
        }
    }
    let _ = (width, height);
    rgba
}

#[cfg(target_os = "macos")]
mod macos {
    use std::path::{Path, PathBuf};
    use std::ptr::null_mut;

    use objc2::rc::Retained;
    use objc2::AnyThread;
    use objc2_app_kit::{
        NSBitmapImageFileType, NSBitmapImageRep, NSCalibratedRGBColorSpace, NSCompositingOperation,
        NSGraphicsContext, NSImage, NSImageInterpolation, NSWorkspace,
    };
    use objc2_foundation::{NSDictionary, NSPoint, NSRect, NSSize, NSString};

    use super::{find_macos_app_bundle_path, is_safe_probe_path, ICON_PIXEL_SIZE};

    pub fn locate_app_bundle_path(
        app_key: &str,
        bundle_id: Option<&str>,
        executable_path: Option<&str>,
    ) -> Option<PathBuf> {
        if let Some(exe) = executable_path {
            let exe_path = Path::new(exe);
            if let Some(bundle) = find_macos_app_bundle_path(exe_path) {
                if is_safe_probe_path(&bundle) && bundle.exists() {
                    return Some(bundle);
                }
            } else if is_safe_probe_path(exe_path) && exe_path.exists() {
                return Some(exe_path.to_path_buf());
            }
        }

        let candidate_bundle_id = bundle_id.or_else(|| {
            if app_key.contains('.') && !app_key.ends_with(".app") {
                Some(app_key)
            } else {
                None
            }
        })?;

        let workspace = NSWorkspace::sharedWorkspace();
        let ns_id = NSString::from_str(candidate_bundle_id);
        if let Some(url) = workspace.URLForApplicationWithBundleIdentifier(&ns_id) {
            if let Some(path_ns) = url.path() {
                let path = PathBuf::from(path_ns.to_string());
                if is_safe_probe_path(&path) && path.exists() {
                    return Some(path);
                }
            }
        }

        // Fallback: check running applications case-insensitively.
        for running in workspace.runningApplications().iter() {
            let matches_id = running
                .bundleIdentifier()
                .is_some_and(|id| id.to_string().eq_ignore_ascii_case(candidate_bundle_id));
            if matches_id {
                if let Some(url) = running.bundleURL() {
                    if let Some(path_ns) = url.path() {
                        let path = PathBuf::from(path_ns.to_string());
                        if is_safe_probe_path(&path) && path.exists() {
                            return Some(path);
                        }
                    }
                }
            }
        }

        None
    }

    pub fn extract_icon_png(
        app_key: &str,
        bundle_id: Option<&str>,
        executable_path: Option<&str>,
        resolved_source: Option<&str>,
    ) -> Result<Option<(Vec<u8>, Option<String>)>, String> {
        let workspace = NSWorkspace::sharedWorkspace();

        // 1. Prefer safe on-disk app bundle via NSWorkspace::iconForFile.
        let bundle_path = resolved_source
            .map(PathBuf::from)
            .filter(|p| is_safe_probe_path(p) && p.exists())
            .or_else(|| locate_app_bundle_path(app_key, bundle_id, executable_path));

        if let Some(path) = bundle_path {
            let path_str = path.to_string_lossy().to_string();
            let ns_path = NSString::from_str(&path_str);
            let image = workspace.iconForFile(&ns_path);
            let png = render_nsimage_to_png(&image)?;
            return Ok(Some((png, Some(path_str))));
        }

        // 2. If the app lives in a TCC-protected folder (e.g. ~/Downloads) and is
        // currently running, read its in-memory NSRunningApplication.icon() directly
        // without touching the filesystem.
        let candidate_id = bundle_id.unwrap_or(app_key);
        for running in workspace.runningApplications().iter() {
            let matches = running
                .bundleIdentifier()
                .is_some_and(|id| id.to_string().eq_ignore_ascii_case(candidate_id));
            if matches {
                if let Some(image) = running.icon() {
                    let png = render_nsimage_to_png(&image)?;
                    return Ok(Some((png, None)));
                }
            }
        }

        Ok(None)
    }

    fn render_nsimage_to_png(image: &NSImage) -> Result<Vec<u8>, String> {
        let px = ICON_PIXEL_SIZE as isize;
        let size_f = ICON_PIXEL_SIZE as f64;

        unsafe {
            let rep: Retained<NSBitmapImageRep> = NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
                NSBitmapImageRep::alloc(),
                null_mut(),
                px,
                px,
                8,
                4,
                true,
                false,
                NSCalibratedRGBColorSpace,
                px * 4,
                32,
            )
            .ok_or_else(|| "Failed to allocate 64x64 NSBitmapImageRep".to_string())?;

            rep.setSize(NSSize::new(size_f, size_f));

            let ctx = NSGraphicsContext::graphicsContextWithBitmapImageRep(&rep)
                .ok_or_else(|| "Failed to create NSGraphicsContext for icon bitmap".to_string())?;

            NSGraphicsContext::saveGraphicsState_class();
            NSGraphicsContext::setCurrentContext(Some(&ctx));
            ctx.setImageInterpolation(NSImageInterpolation::High);

            let dest_rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(size_f, size_f));
            let zero_rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0));
            image.drawInRect_fromRect_operation_fraction(
                dest_rect,
                zero_rect,
                NSCompositingOperation::Copy,
                1.0,
            );

            ctx.flushGraphics();
            NSGraphicsContext::restoreGraphicsState_class();

            let props = NSDictionary::new();
            let png_data = rep
                .representationUsingType_properties(NSBitmapImageFileType::PNG, &props)
                .ok_or_else(|| "Failed to encode NSBitmapImageRep as PNG".to_string())?;

            Ok(png_data.to_vec())
        }
    }
}

#[cfg(target_os = "windows")]
mod windows {
    use std::mem::{size_of, zeroed};
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use std::ptr::null_mut;

    use windows_sys::Win32::{
        Graphics::Gdi::{
            DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO,
            BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP,
        },
        System::Diagnostics::Debug::{
            SetThreadErrorMode, SEM_FAILCRITICALERRORS, SEM_NOOPENFILEERRORBOX, THREAD_ERROR_MODE,
        },
        UI::Shell::{SHDefExtractIconW, SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON},
        UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, HICON, ICONINFO},
    };

    use super::{
        bgra_to_rgba_with_fallback_mask, encode_rgba_png, is_safe_probe_path, ICON_PIXEL_SIZE,
    };

    struct ThreadErrorModeGuard(THREAD_ERROR_MODE);

    impl ThreadErrorModeGuard {
        unsafe fn suppress_dialogs() -> Self {
            let mut old_mode: THREAD_ERROR_MODE = 0;
            SetThreadErrorMode(
                SEM_FAILCRITICALERRORS | SEM_NOOPENFILEERRORBOX,
                &mut old_mode,
            );
            Self(old_mode)
        }
    }

    impl Drop for ThreadErrorModeGuard {
        fn drop(&mut self) {
            unsafe {
                let mut _ignored: THREAD_ERROR_MODE = 0;
                SetThreadErrorMode(self.0, &mut _ignored);
            }
        }
    }

    pub fn extract_icon_png(exe_path: &Path) -> Result<Option<Vec<u8>>, String> {
        if !is_safe_probe_path(exe_path) || !exe_path.is_file() {
            return Ok(None);
        }

        let wide_path: Vec<u16> = exe_path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        unsafe {
            let _error_guard = ThreadErrorModeGuard::suppress_dialogs();

            let mut hicon: HICON = null_mut();
            let hr = SHDefExtractIconW(
                wide_path.as_ptr(),
                0,
                0,
                &mut hicon,
                null_mut(),
                ICON_PIXEL_SIZE,
            );

            if hr != 0 || hicon.is_null() {
                hicon = null_mut();
                let mut shfi: SHFILEINFOW = zeroed();
                let res = SHGetFileInfoW(
                    wide_path.as_ptr(),
                    0,
                    &mut shfi,
                    size_of::<SHFILEINFOW>() as u32,
                    SHGFI_ICON | SHGFI_LARGEICON,
                );
                if res != 0 && !shfi.hIcon.is_null() {
                    hicon = shfi.hIcon;
                }
            }

            if hicon.is_null() {
                return Ok(None);
            }

            let result = hicon_to_png(hicon);
            DestroyIcon(hicon);
            result.map(Some)
        }
    }

    unsafe fn hicon_to_png(hicon: HICON) -> Result<Vec<u8>, String> {
        let mut icon_info: ICONINFO = zeroed();
        if GetIconInfo(hicon, &mut icon_info) == 0 {
            return Err("GetIconInfo failed".to_string());
        }

        let color_bmp = icon_info.hbmColor;
        let mask_bmp = icon_info.hbmMask;
        let cleanup = || {
            if !color_bmp.is_null() {
                DeleteObject(color_bmp as _);
            }
            if !mask_bmp.is_null() {
                DeleteObject(mask_bmp as _);
            }
        };

        let target_bmp = if !color_bmp.is_null() {
            color_bmp
        } else {
            mask_bmp
        };
        if target_bmp.is_null() {
            cleanup();
            return Err("Icon has neither color nor mask bitmap".to_string());
        }

        let mut bmp: BITMAP = zeroed();
        if GetObjectW(
            target_bmp as _,
            size_of::<BITMAP>() as i32,
            &mut bmp as *mut _ as *mut _,
        ) == 0
        {
            cleanup();
            return Err("GetObjectW failed on icon bitmap".to_string());
        }

        let width = bmp.bmWidth.max(1) as u32;
        let height = (if color_bmp.is_null() {
            bmp.bmHeight / 2
        } else {
            bmp.bmHeight
        })
        .max(1) as u32;

        let color_bytes = read_bitmap_bgra(target_bmp, width, height);
        let mask_bytes = if !mask_bmp.is_null() && !color_bmp.is_null() {
            read_bitmap_bgra(mask_bmp, width, height).ok()
        } else {
            None
        };
        cleanup();

        let bgra = color_bytes?;
        let rgba = bgra_to_rgba_with_fallback_mask(width, height, &bgra, mask_bytes.as_deref());
        encode_rgba_png(width, height, &rgba)
    }

    unsafe fn read_bitmap_bgra(
        hbitmap: HBITMAP,
        width: u32,
        height: u32,
    ) -> Result<Vec<u8>, String> {
        let hdc = GetDC(null_mut());
        if hdc.is_null() {
            return Err("GetDC failed".to_string());
        }

        let mut bmi: BITMAPINFO = zeroed();
        bmi.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = width as i32;
        bmi.bmiHeader.biHeight = -(height as i32); // top-down DIB
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB;

        let mut buf = vec![0u8; (width * height * 4) as usize];
        let lines = GetDIBits(
            hdc,
            hbitmap,
            0,
            height,
            buf.as_mut_ptr() as *mut _,
            &mut bmi,
            DIB_RGB_COLORS,
        );
        ReleaseDC(null_mut(), hdc);

        if lines == 0 {
            return Err("GetDIBits failed".to_string());
        }
        Ok(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn migrates_existing_icon_index_access_times_once() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE app_icon_index (
                platform TEXT NOT NULL, app_key TEXT NOT NULL, app_name TEXT NOT NULL,
                bundle_id TEXT, executable_path TEXT, icon_file TEXT, source_path TEXT,
                source_mtime_secs INTEGER, updated_at TEXT NOT NULL,
                PRIMARY KEY (platform, app_key)
             );
             INSERT INTO app_icon_index(platform, app_key, app_name, updated_at)
             VALUES ('windows', 'code.exe', 'Code', '2025-01-01T00:00:00Z');",
        )
        .unwrap();
        create_schema(&conn).unwrap();
        let migrated: String = conn
            .query_row("SELECT last_accessed_at FROM app_icon_index", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(migrated, "2025-01-01T00:00:00Z");
        touch_cached_icon_in(&conn, "windows", "code.exe").unwrap();
        let accessed: String = conn
            .query_row("SELECT last_accessed_at FROM app_icon_index", [], |row| {
                row.get(0)
            })
            .unwrap();
        create_schema(&conn).unwrap();
        let after_restart: String = conn
            .query_row("SELECT last_accessed_at FROM app_icon_index", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_ne!(accessed, "2025-01-01T00:00:00Z");
        assert_eq!(accessed, after_restart);
    }

    #[test]
    fn legacy_windows_names_match_canonical_identity_without_crossing_platforms() {
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO app_icon_index(platform, app_key, app_name, updated_at) VALUES
                ('windows', 'code.exe', 'Code', '2025-01-01T00:00:00Z'),
                ('macos', 'com.microsoft.vscode', 'Code', '2025-01-01T00:00:00Z');",
        )
        .unwrap();
        for name in ["Code", "Code.exe", "CODE.EXE"] {
            let entry = find_entry_by_name_in(&conn, name, Some("Windows"))
                .unwrap()
                .unwrap();
            assert_eq!(entry.app_key, "code.exe");
        }
        assert_eq!(
            find_entry_by_name_in(&conn, "Code.exe", None)
                .unwrap()
                .unwrap()
                .platform,
            "windows"
        );
        assert!(find_entry_by_name_in(&conn, "Code.exe", Some("macos"))
            .unwrap()
            .is_none());
        // Older indexes may still store the uncleaned process name.
        conn.execute(
            "UPDATE app_icon_index SET app_name = 'Code.exe' WHERE platform = 'windows'",
            [],
        )
        .unwrap();
        assert_eq!(
            find_entry_by_name_in(&conn, "Code", Some("windows"))
                .unwrap()
                .unwrap()
                .app_key,
            "code.exe"
        );
    }

    #[test]
    fn cache_eviction_respects_recent_access_and_retains_source_metadata() {
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let mib = 1024 * 1024;
        for (name, size) in [
            ("old", 6 * mib),
            ("used", 4 * mib),
            ("new", 4 * mib),
            ("missing", 0),
        ] {
            if size != 0 {
                std::fs::File::create(dir.path().join(format!("{name}.png")))
                    .unwrap()
                    .set_len(size)
                    .unwrap();
            }
            conn.execute(
                "INSERT INTO app_icon_index (
                    platform, app_key, app_name, executable_path, icon_file, source_path,
                    source_mtime_secs, updated_at, last_accessed_at
                 ) VALUES ('windows', ?1, ?1, 'C:\\Apps\\app.exe', ?2, 'C:\\Apps\\app.exe', 42,
                    '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z')",
                params![name, format!("{name}.png")],
            )
            .unwrap();
        }
        touch_cached_icon_in(&conn, "windows", "used").unwrap();
        touch_cached_icon_in(&conn, "windows", "new").unwrap();
        // Also clean orphan PNGs from an interrupted file/index write.
        std::fs::File::create(dir.path().join("orphan.png"))
            .unwrap()
            .set_len(mib)
            .unwrap();
        let removed = prune_icon_cache_files(
            dir.path(),
            &cached_icon_accesses(&conn).unwrap(),
            ICON_CACHE_MAX_BYTES,
        )
        .unwrap();
        clear_cached_icon_files(&conn, &removed).unwrap();
        assert!(removed.contains(&"old.png".to_string()));
        assert!(removed.contains(&"orphan.png".to_string()));
        assert!(removed.contains(&"missing.png".to_string()));
        assert!(dir.path().join("used.png").is_file());
        assert!(dir.path().join("new.png").is_file());
        let retained: (Option<String>, String, String, i64) = conn
            .query_row(
                "SELECT icon_file, executable_path, source_path, source_mtime_secs
             FROM app_icon_index WHERE app_key = 'old'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            retained,
            (
                None,
                r"C:\Apps\app.exe".into(),
                r"C:\Apps\app.exe".into(),
                42
            )
        );
        assert!(prune_icon_cache_files(
            dir.path(),
            &cached_icon_accesses(&conn).unwrap(),
            ICON_CACHE_MAX_BYTES
        )
        .unwrap()
        .is_empty());
    }

    #[test]
    fn cache_at_ten_mebibytes_is_kept_and_replacing_an_icon_does_not_add_files() {
        let dir = tempfile::tempdir().unwrap();
        let name = icon_filename_for_key("windows", "code.exe");
        let path = dir.path().join(&name);
        write_png_atomically(dir.path(), &path, b"old").unwrap();
        write_png_atomically(dir.path(), &path, b"new").unwrap();
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(ICON_CACHE_MAX_BYTES)
            .unwrap();
        assert!(
            prune_icon_cache_files(dir.path(), &HashMap::new(), ICON_CACHE_MAX_BYTES)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn tcc_and_unc_path_guard_filters_protected_locations() {
        // Safe application paths on macOS and Windows
        assert!(is_safe_probe_path(Path::new("/Applications/VoiceX.app")));
        assert!(is_safe_probe_path(Path::new(
            "/System/Library/CoreServices/Finder.app"
        )));
        assert!(is_safe_probe_path(Path::new(
            "/Users/alice/Applications/Cursor.app"
        )));
        assert!(is_safe_probe_path(Path::new(
            r"C:\Users\alice\AppData\Local\Programs\Microsoft VS Code\Code.exe"
        )));

        // TCC-protected macOS user folders must never be probed on disk
        assert!(!is_safe_probe_path(Path::new(
            "/Users/alice/Downloads/TempApp.app"
        )));
        assert!(!is_safe_probe_path(Path::new(
            "/Users/alice/Desktop/Test.app"
        )));
        assert!(!is_safe_probe_path(Path::new(
            "/Users/alice/Documents/Work.app"
        )));
        assert!(!is_safe_probe_path(Path::new(
            "/Users/alice/Library/Mobile Documents/com~apple~CloudDocs/App.app"
        )));
        assert!(!is_safe_probe_path(Path::new(
            "/Volumes/ExternalDisk/SomeApp.app"
        )));

        // Windows UNC network shares must never be probed
        assert!(!is_safe_probe_path(Path::new(
            r"\\fileserver\share\app\tool.exe"
        )));
        assert!(!is_safe_probe_path(Path::new(
            r"\\?\UNC\fileserver\share\tool.exe"
        )));
    }

    #[test]
    fn finds_outermost_macos_app_bundle_from_executable_path() {
        let exe = Path::new(
            "/Applications/Antigravity.app/Contents/Frameworks/Helper.app/Contents/MacOS/Helper",
        );
        assert_eq!(
            find_macos_app_bundle_path(exe),
            Some(PathBuf::from("/Applications/Antigravity.app"))
        );
    }

    #[test]
    fn bgra_to_rgba_preserves_alpha_or_applies_mask() {
        // 1x1 pixel with explicit alpha = 128
        let bgra = [10u8, 20, 30, 128];
        let rgba = bgra_to_rgba_with_fallback_mask(1, 1, &bgra, None);
        assert_eq!(rgba, vec![30, 20, 10, 128]);

        // 2x1 legacy pixel with alpha = 0 and a mask (first pixel opaque=0, second transparent=255)
        let legacy_bgra = [10u8, 20, 30, 0, 40, 50, 60, 0];
        let mask_bgra = [0u8, 0, 0, 0, 255, 255, 255, 0];
        let rgba_masked = bgra_to_rgba_with_fallback_mask(2, 1, &legacy_bgra, Some(&mask_bgra));
        assert_eq!(rgba_masked, vec![30, 20, 10, 255, 60, 50, 40, 0]);

        let png = encode_rgba_png(2, 1, &rgba_masked).expect("valid PNG");
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn extracts_real_macos_finder_icon_as_64x64_png() {
        let (png_bytes, source_path) =
            macos::extract_icon_png("com.apple.finder", Some("com.apple.finder"), None, None)
                .expect("extract_icon_png should not error")
                .expect("Finder icon should be found on macOS");
        assert!(source_path.is_some());
        assert!(png_bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert!(
            png_bytes.len() > 500 && png_bytes.len() < 50_000,
            "Unexpected 64x64 PNG byte size: {}",
            png_bytes.len()
        );

        let decoder = png::Decoder::new(std::io::Cursor::new(&png_bytes));
        let reader = decoder.read_info().expect("valid PNG info");
        assert_eq!(reader.info().width, ICON_PIXEL_SIZE);
        assert_eq!(reader.info().height, ICON_PIXEL_SIZE);
    }
}
