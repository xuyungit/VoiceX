//! In-app update through the Tauri updater plugin.
//!
//! Only official release builds take part. Those are release-profile builds
//! with `tauri.updater.conf.json` merged in, which supplies the updater's
//! public key and endpoint. Debug builds and local release builds
//! (`mac:build-local`, `Build-VoiceX.ps1`) carry no updater config, so the
//! plugin is never registered and nothing is checked. A build made from the
//! working tree is therefore never replaced by a published one.
//!
//! Nothing installs on its own: a check only reports a newer version, and the
//! user starts the install from the About page or the tray menu.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::{Update, UpdaterExt};

use crate::commands::settings::AppSettings;
use crate::tts::TtsController;

const STATUS_EVENT: &str = "update:status";
const FIRST_CHECK_DELAY: Duration = Duration::from_secs(15);
const CHECK_INTERVAL_SECS: i64 = 24 * 60 * 60;
/// How often the background task looks at whether a check is due. Due-ness is
/// judged by wall-clock time because a monotonic sleep stops while the
/// machine sleeps, which would stretch a plain 24-hour timer indefinitely.
const WAKE_INTERVAL: Duration = Duration::from_secs(60 * 60);
const PROGRESS_EMIT_INTERVAL: Duration = Duration::from_millis(200);

/// Error codes the UI translates; any other error text comes from the plugin
/// and is shown as is.
const ERR_DISABLED: &str = "disabled";
const ERR_IN_PROGRESS: &str = "inProgress";
const ERR_BUSY: &str = "busy";
const ERR_NO_UPDATE: &str = "noUpdate";

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum Phase {
    #[default]
    Idle,
    Checking,
    UpToDate,
    Available,
    Downloading,
    Installing,
    Error,
}

impl Phase {
    fn is_working(self) -> bool {
        matches!(self, Phase::Checking | Phase::Downloading | Phase::Installing)
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AvailableUpdate {
    pub version: String,
    pub notes: Option<String>,
    /// Unix seconds.
    pub published_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    /// False in debug and local builds, which never check.
    pub enabled: bool,
    pub phase: Phase,
    /// Kept through a failed download so the user can retry.
    pub available: Option<AvailableUpdate>,
    pub downloaded: u64,
    pub total: Option<u64>,
    pub error: Option<String>,
    /// Unix seconds of the last check that reached the server.
    pub last_checked_at: Option<i64>,
}

#[derive(Default)]
pub struct UpdateService {
    inner: Mutex<Inner>,
    recording: Mutex<Option<Arc<AtomicBool>>>,
}

#[derive(Default)]
struct Inner {
    status: UpdateStatus,
    pending: Option<Update>,
}

impl UpdateService {
    pub fn status(&self) -> UpdateStatus {
        self.lock().status.clone()
    }

    pub fn available_version(&self) -> Option<String> {
        self.lock().status.available.as_ref().map(|a| a.version.clone())
    }

    /// The session's recording flag; an install is refused while it is set.
    pub fn attach_recording_flag(&self, flag: Arc<AtomicBool>) {
        *self.recording.lock().unwrap_or_else(|e| e.into_inner()) = Some(flag);
    }

    fn is_recording(&self) -> bool {
        self.recording
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::SeqCst))
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Apply `change` and push the result to the UI.
    fn update(&self, app: &AppHandle, change: impl FnOnce(&mut Inner)) {
        let status = {
            let mut inner = self.lock();
            change(&mut inner);
            inner.status.clone()
        };
        if let Err(err) = app.emit(STATUS_EVENT, &status) {
            log::warn!("Failed to emit update status: {err}");
        }
    }
}

/// Release profile with the updater config merged in.
fn is_official_build(app: &tauri::App) -> bool {
    !cfg!(debug_assertions) && app.config().plugins.0.contains_key("updater")
}

/// Called once from setup: registers the plugin and starts the daily check in
/// official builds, and does nothing in any other build.
pub fn init(app: &tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    if !is_official_build(app) {
        log::info!("Update checks are off: not an official release build");
        return Ok(());
    }
    app.handle()
        .plugin(tauri_plugin_updater::Builder::new().build())?;
    app.state::<UpdateService>().lock().status.enabled = true;
    log::info!("Update checks are on");
    spawn_background_checks(app.handle().clone());
    Ok(())
}

fn spawn_background_checks(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST_CHECK_DELAY).await;
        loop {
            if background_check_due(&app) {
                if let Err(err) = check(&app).await {
                    log::warn!("Background update check failed: {err}");
                }
            }
            tokio::time::sleep(WAKE_INTERVAL).await;
        }
    });
}

fn background_check_due(app: &AppHandle) -> bool {
    let settings = crate::storage::get_settings().unwrap_or_else(|err| {
        log::warn!("Failed to load settings for the update check: {err}");
        AppSettings::default()
    });
    if !settings.auto_check_updates {
        return false;
    }
    let status = app.state::<UpdateService>().status();
    // Once a version is on offer there is nothing new to learn until the user
    // installs it or the app restarts.
    if status.phase.is_working() || status.available.is_some() {
        return false;
    }
    status
        .last_checked_at
        .is_none_or(|last| now_secs() - last >= CHECK_INTERVAL_SECS)
}

/// Ask the update endpoint whether a newer version exists.
pub async fn check(app: &AppHandle) -> Result<UpdateStatus, String> {
    let service = app.state::<UpdateService>();
    {
        let inner = service.lock();
        if !inner.status.enabled {
            return Err(ERR_DISABLED.into());
        }
        if inner.status.phase.is_working() {
            return Err(ERR_IN_PROGRESS.into());
        }
    }
    let offered_before = service.available_version();
    service.update(app, |inner| {
        inner.status.phase = Phase::Checking;
        inner.status.error = None;
    });

    let result = match app.updater() {
        Ok(updater) => updater.check().await,
        Err(err) => Err(err),
    };

    let outcome = match result {
        Ok(Some(update)) => {
            log::info!(
                "Update available: {} -> {}",
                update.current_version,
                update.version
            );
            service.update(app, |inner| {
                inner.status.available = Some(AvailableUpdate {
                    version: update.version.clone(),
                    notes: update.body.clone(),
                    published_at: update.date.map(|d| d.unix_timestamp()),
                });
                inner.status.phase = Phase::Available;
                inner.status.last_checked_at = Some(now_secs());
                inner.pending = Some(update);
            });
            Ok(())
        }
        Ok(None) => {
            log::info!("No update available");
            mark_up_to_date(app, &service);
            Ok(())
        }
        // The release is published before every platform's package is
        // attached (Windows is built by CI afterwards), so for a while the
        // manifest may not list this platform yet. There is nothing to
        // install, so it reads as up to date.
        Err(tauri_plugin_updater::Error::TargetNotFound(target)) => {
            log::info!("Latest release has no package for {target} yet");
            mark_up_to_date(app, &service);
            Ok(())
        }
        Err(err) => {
            let message = err.to_string();
            service.update(app, |inner| {
                inner.status.phase = if inner.status.available.is_some() {
                    Phase::Available
                } else {
                    Phase::Error
                };
                inner.status.error = Some(message.clone());
            });
            Err(message)
        }
    };

    if service.available_version() != offered_before {
        refresh_tray(app);
    }
    outcome.map(|()| service.status())
}

fn mark_up_to_date(app: &AppHandle, service: &UpdateService) {
    service.update(app, |inner| {
        inner.status.phase = Phase::UpToDate;
        inner.status.available = None;
        inner.status.last_checked_at = Some(now_secs());
        inner.pending = None;
    });
}

/// Download, verify, and install the update found by the last check, then
/// restart. On Windows the installer closes the app itself.
pub async fn install(app: &AppHandle) -> Result<(), String> {
    let service = app.state::<UpdateService>();
    // Replacing the app mid-dictation would lose the recording; mid-reading
    // it would cut the speech off.
    if service.is_recording() || app.state::<TtsController>().is_active() {
        return Err(ERR_BUSY.into());
    }
    let update = {
        let inner = service.lock();
        if !inner.status.enabled {
            return Err(ERR_DISABLED.into());
        }
        if inner.status.phase.is_working() {
            return Err(ERR_IN_PROGRESS.into());
        }
        inner.pending.clone().ok_or_else(|| ERR_NO_UPDATE.to_string())?
    };
    log::info!("Installing update {}", update.version);
    service.update(app, |inner| {
        inner.status.phase = Phase::Downloading;
        inner.status.downloaded = 0;
        inner.status.total = None;
        inner.status.error = None;
    });

    let mut downloaded: u64 = 0;
    let mut last_emit: Option<Instant> = None;
    let result = update
        .download_and_install(
            |chunk, total| {
                downloaded += chunk as u64;
                let now = Instant::now();
                let complete = total.is_some_and(|t| downloaded >= t);
                if complete || last_emit.is_none_or(|t| now - t >= PROGRESS_EMIT_INTERVAL) {
                    last_emit = Some(now);
                    service.update(app, |inner| {
                        inner.status.downloaded = downloaded;
                        inner.status.total = total;
                    });
                }
            },
            || {
                service.update(app, |inner| inner.status.phase = Phase::Installing);
            },
        )
        .await;

    match result {
        Ok(()) => {
            log::info!("Update {} installed; restarting", update.version);
            app.restart();
        }
        Err(err) => {
            let message = err.to_string();
            log::error!("Update {} failed to install: {message}", update.version);
            service.update(app, |inner| {
                inner.status.phase = Phase::Error;
                inner.status.error = Some(message.clone());
            });
            Err(message)
        }
    }
}

/// Rebuild the tray menu so its update item matches the offered version.
fn refresh_tray(app: &AppHandle) {
    let settings = crate::storage::get_settings().unwrap_or_else(|err| {
        log::warn!("Failed to load settings for the tray menu: {err}");
        AppSettings::default()
    });
    if let Err(err) = crate::tray::apply_menu(app, &settings) {
        log::warn!("Failed to add the update item to the tray menu: {err}");
    }
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
