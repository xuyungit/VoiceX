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
use crate::work_gate::{InstallPermit, WorkGate};

const STATUS_EVENT: &str = "update:status";
const FIRST_CHECK_DELAY: Duration = Duration::from_secs(15);
const CHECK_INTERVAL_SECS: i64 = 24 * 60 * 60;
/// How often the background task looks at whether a check is due. Due-ness is
/// judged by wall-clock time because a monotonic sleep stops while the
/// machine sleeps, which would stretch a plain 24-hour timer indefinitely.
const WAKE_INTERVAL: Duration = Duration::from_secs(60 * 60);
const PROGRESS_EMIT_INTERVAL: Duration = Duration::from_millis(200);
/// The endpoints are tried in order and the first manifest that answers
/// decides where the package is downloaded from, so an unreachable endpoint
/// has to fail fast for the next one to get a turn.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// A download that stops receiving data fails instead of hanging in the
/// downloading phase; the user can retry. Not a total timeout, which would
/// cut off a slow but progressing download.
const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// Replaces the plugin's generic `tauri-plugin-updater/<version>` so update
/// requests can be told apart from browser downloads. Carries no app version
/// or other client detail.
const USER_AGENT: &str = "VoiceX-Updater";

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
        matches!(
            self,
            Phase::Checking | Phase::Downloading | Phase::Installing
        )
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
    session: Mutex<Option<Arc<AtomicBool>>>,
    work_gate: WorkGate,
}

#[derive(Default)]
struct Inner {
    status: UpdateStatus,
    pending: Option<Update>,
    downloaded_package: Option<Arc<Vec<u8>>>,
}

impl UpdateService {
    pub fn status(&self) -> UpdateStatus {
        self.lock().status.clone()
    }

    pub fn available_version(&self) -> Option<String> {
        self.lock()
            .status
            .available
            .as_ref()
            .map(|a| a.version.clone())
    }

    pub(crate) fn work_gate(&self) -> WorkGate {
        self.work_gate.clone()
    }

    /// Includes ASR finalization, correction and text injection after key-up.
    pub fn attach_session_flag(&self, flag: Arc<AtomicBool>) {
        *self.session.lock().unwrap_or_else(|e| e.into_inner()) = Some(flag);
    }

    fn session_active(&self) -> bool {
        self.session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::SeqCst))
    }

    fn begin_install(
        &self,
        reading_active: impl FnOnce() -> bool,
    ) -> Result<InstallPermit, String> {
        // Admission closes before checking the session mirrors. A new hotkey
        // or reading request cannot sneak in after this idle check.
        let permit = self.work_gate.try_install().ok_or(ERR_BUSY)?;
        if self.session_active() || reading_active() {
            return Err(ERR_BUSY.into());
        }
        Ok(permit)
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
    let offered_before = {
        let mut inner = service.lock();
        if !inner.status.enabled {
            return Err(ERR_DISABLED.into());
        }
        if inner.status.phase.is_working() {
            return Err(ERR_IN_PROGRESS.into());
        }
        let offered = inner.status.available.as_ref().map(|a| a.version.clone());
        inner.status.phase = Phase::Checking;
        inner.status.error = None;
        offered
    };
    service.update(app, |_| {});

    // The update returned by the check keeps this client setup for its download.
    let result = match app
        .updater_builder()
        .configure_client(|client| {
            client
                .user_agent(USER_AGENT)
                .connect_timeout(CONNECT_TIMEOUT)
                .read_timeout(READ_TIMEOUT)
        })
        .build()
    {
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
                inner.downloaded_package = None;
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
        inner.downloaded_package = None;
    });
}

/// Download, verify, and install the update found by the last check, then
/// restart. On Windows the installer closes the app itself.
pub async fn install(app: &AppHandle) -> Result<(), String> {
    let service = app.state::<UpdateService>();
    // Replacing the app mid-dictation would lose the recording; mid-reading
    // it would cut the speech off.
    if service.session_active() || app.state::<TtsController>().is_active() {
        return Err(ERR_BUSY.into());
    }
    let (update, cached) = {
        let mut inner = service.lock();
        if !inner.status.enabled {
            return Err(ERR_DISABLED.into());
        }
        if inner.status.phase.is_working() {
            return Err(ERR_IN_PROGRESS.into());
        }
        let update = inner
            .pending
            .clone()
            .ok_or_else(|| ERR_NO_UPDATE.to_string())?;
        inner.status.phase = Phase::Downloading;
        inner.status.error = None;
        if inner.downloaded_package.is_none() {
            inner.status.downloaded = 0;
            inner.status.total = None;
        }
        (update, inner.downloaded_package.clone())
    };
    service.update(app, |_| {});

    let result = async {
        let bytes = if let Some(bytes) = cached {
            bytes
        } else {
            log::info!("Downloading update {}", update.version);
            let mut downloaded: u64 = 0;
            let mut last_emit: Option<Instant> = None;
            let bytes = update
                .download(
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
                    || {},
                )
                .await
                .map_err(|err| err.to_string())?;
            // download() verifies the signature before returning these bytes.
            let bytes = Arc::new(bytes);
            service.lock().downloaded_package = Some(bytes.clone());
            bytes
        };

        let permit = service.begin_install(|| app.state::<TtsController>().is_active())?;
        service.update(app, |inner| inner.status.phase = Phase::Installing);
        let app = app.clone();
        tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
            // Keep admission closed through replacement AND restart, including
            // Windows where the installer exits the process itself.
            let _permit = permit;
            log::info!("Installing update {}", update.version);
            update
                .install(bytes.as_slice())
                .map_err(|err| err.to_string())?;
            app.restart()
        })
        .await
        .map_err(|err| err.to_string())?
    }
    .await;

    match result {
        Ok(()) => Ok(()),
        Err(err) => {
            let message = err.to_string();
            log::warn!("Update installation did not complete: {message}");
            service.update(app, |inner| {
                inner.status.phase = if message == ERR_BUSY {
                    Phase::Available
                } else {
                    Phase::Error
                };
                inner.status.error = Some(message.clone());
            });
            Err(message)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busy_after_download_keeps_installation_out_until_session_and_saves_finish() {
        let service = UpdateService::default();
        let active = Arc::new(AtomicBool::new(true));
        service.attach_session_flag(active.clone());
        assert!(service.begin_install(|| false).is_err());
        assert!(service.work_gate.try_work().is_some());
        active.store(false, Ordering::SeqCst);
        let save = service.work_gate.try_work().unwrap();
        assert!(service.begin_install(|| false).is_err());
        drop(save);
        assert!(service.begin_install(|| true).is_err());
        let install = service.begin_install(|| false).unwrap();
        assert!(service.work_gate.try_work().is_none());
        drop(install);
        assert!(service.work_gate.try_work().is_some());
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
