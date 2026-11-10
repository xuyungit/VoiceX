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

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum InstallOutcome {
    Installed,
    Refreshed,
    UpToDate,
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
    downloaded_package: Option<DownloadedPackage>,
}

#[derive(Clone)]
struct DownloadedPackage {
    identity: PackageIdentity,
    bytes: Arc<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PackageIdentity {
    version: String,
    target: String,
    url: String,
    signature: String,
}

struct PreparedDownload {
    update: Update,
    cached: Option<DownloadedPackage>,
}

impl From<&Update> for PackageIdentity {
    fn from(update: &Update) -> Self {
        Self {
            version: update.version.clone(),
            target: update.target.clone(),
            url: update.download_url.to_string(),
            signature: update.signature.clone(),
        }
    }
}

impl Inner {
    fn begin_check(&mut self) -> Result<Option<String>, String> {
        if !self.status.enabled {
            return Err(ERR_DISABLED.into());
        }
        if self.status.phase.is_working() {
            return Err(ERR_IN_PROGRESS.into());
        }
        let offered = self.status.available.as_ref().map(|a| a.version.clone());
        self.status.phase = Phase::Checking;
        self.status.error = None;
        Ok(offered)
    }

    fn offer(&mut self, update: Update) {
        self.retain_package(&PackageIdentity::from(&update));
        self.status.available = Some(AvailableUpdate {
            version: update.version.clone(),
            notes: update.body.clone(),
            published_at: update.date.map(|d| d.unix_timestamp()),
        });
        self.status.phase = Phase::Available;
        self.status.last_checked_at = Some(now_secs());
        self.pending = Some(update);
    }

    fn retain_package(&mut self, identity: &PackageIdentity) {
        if !self
            .downloaded_package
            .as_ref()
            .is_some_and(|package| &package.identity == identity)
        {
            self.downloaded_package = None;
            self.status.downloaded = 0;
            self.status.total = None;
        }
    }

    fn finish_check(
        &mut self,
        result: tauri_plugin_updater::Result<Option<Update>>,
        expected_version: Option<&str>,
    ) -> Result<Option<PreparedDownload>, String> {
        match result {
            Ok(Some(update)) => {
                log::info!(
                    "Update available: {} -> {}",
                    update.current_version,
                    update.version
                );
                self.offer(update);
                // Compare against the version the user actually saw, including
                // when a background check has already replaced the old offer.
                let update = self.pending.as_ref().expect("offered update");
                if expected_version != Some(update.version.as_str()) {
                    return Ok(None);
                }
                self.status.phase = Phase::Downloading;
                Ok(Some(PreparedDownload {
                    update: update.clone(),
                    cached: self.downloaded_package.clone(),
                }))
            }
            Ok(None) | Err(tauri_plugin_updater::Error::TargetNotFound(_)) => {
                // A platform may not have its package yet while a release is
                // being published. Clear the old offer in that case too.
                self.status.phase = Phase::UpToDate;
                self.status.available = None;
                self.status.last_checked_at = Some(now_secs());
                self.status.downloaded = 0;
                self.status.total = None;
                self.pending = None;
                self.downloaded_package = None;
                Ok(None)
            }
            Err(err) => {
                let message = err.to_string();
                self.status.phase = if self.status.available.is_some() {
                    Phase::Available
                } else {
                    Phase::Error
                };
                self.status.error = Some(message.clone());
                Err(message)
            }
        }
    }

    fn install_failed(&mut self, message: &str) {
        self.status.phase = if message == ERR_BUSY {
            Phase::Available
        } else {
            Phase::Error
        };
        // A busy refusal is reported by the translated action toast, not a
        // persistent error. Keep the verified package for the next attempt.
        self.status.error = (message != ERR_BUSY).then(|| message.to_owned());
    }

    fn download_refresh_outcome(
        &self,
        failed_update: &Update,
        download_error: &str,
    ) -> Result<InstallOutcome, String> {
        if self.pending.as_ref().is_some_and(|update| {
            PackageIdentity::from(update) == PackageIdentity::from(failed_update)
        }) {
            // The server still advertises the same broken package. Keep the
            // download failure visible instead of claiming recovery.
            Err(download_error.into())
        } else {
            Ok(refreshed_outcome(&self.status))
        }
    }
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
    fn update<T>(&self, app: &AppHandle, change: impl FnOnce(&mut Inner) -> T) -> T {
        let (result, status) = {
            let mut inner = self.lock();
            let result = change(&mut inner);
            (result, inner.status.clone())
        };
        if let Err(err) = app.emit(STATUS_EVENT, &status) {
            log::warn!("Failed to emit update status: {err}");
        }
        result
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
    check_due(&app.state::<UpdateService>().status(), now_secs())
}

fn check_due(status: &UpdateStatus, now: i64) -> bool {
    status.enabled
        && !status.phase.is_working()
        && status
            .last_checked_at
            .is_none_or(|last| now - last >= CHECK_INTERVAL_SECS)
}

/// Ask the update endpoint whether a newer version exists.
pub async fn check(app: &AppHandle) -> Result<UpdateStatus, String> {
    let service = app.state::<UpdateService>();
    let offered_before = service.update(app, Inner::begin_check)?;
    finish_refresh(app, offered_before, None).await?;
    Ok(service.status())
}

/// The caller has already entered Checking. Completing an install preflight
/// enters Downloading under the same lock, so another check cannot replace
/// the package between confirmation and download.
async fn finish_refresh(
    app: &AppHandle,
    offered_before: Option<String>,
    expected_version: Option<&str>,
) -> Result<Option<PreparedDownload>, String> {
    let service = app.state::<UpdateService>();
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

    let outcome = service.update(app, |inner| inner.finish_check(result, expected_version));

    if service.available_version() != offered_before {
        refresh_tray(app);
    }
    outcome
}

/// Recheck before downloading. If the offered version changed, leave its
/// notes visible and require another click. Windows closes the app itself
/// during installation; other platforms restart after replacement.
pub async fn install(app: &AppHandle, expected_version: &str) -> Result<InstallOutcome, String> {
    let service = app.state::<UpdateService>();
    // Replacing the app mid-dictation would lose the recording; mid-reading
    // it would cut the speech off.
    if service.session_active() || app.state::<TtsController>().is_active() {
        return Err(ERR_BUSY.into());
    }
    let offered_before = service.update(app, |inner| {
        if inner.pending.is_none() && !inner.status.phase.is_working() && inner.status.enabled {
            return Err(ERR_NO_UPDATE.into());
        }
        inner.begin_check()
    })?;
    let Some(PreparedDownload { update, cached }) =
        finish_refresh(app, offered_before, Some(expected_version)).await?
    else {
        return Ok(refreshed_outcome(&service.status()));
    };

    let result = async {
        let bytes = if let Some(bytes) = cached {
            bytes.bytes
        } else {
            log::info!("Downloading update {}", update.version);
            let mut downloaded: u64 = 0;
            let mut last_emit: Option<Instant> = None;
            let result = update
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
                .await;
            let bytes = match result {
                Ok(bytes) => bytes,
                Err(err) => {
                    return refresh_after_download_failure(app, &update, &err.to_string()).await;
                }
            };
            // download() verifies the signature before returning these bytes.
            let bytes = Arc::new(bytes);
            service.lock().downloaded_package = Some(DownloadedPackage {
                identity: PackageIdentity::from(&update),
                bytes: bytes.clone(),
            });
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
        .map_err(|err| err.to_string())??;
        Ok(InstallOutcome::Installed)
    }
    .await;

    match result {
        Ok(outcome) => Ok(outcome),
        Err(err) => {
            let message = err.to_string();
            log::warn!("Update installation did not complete: {message}");
            service.update(app, |inner| {
                inner.install_failed(&message);
            });
            Err(message)
        }
    }
}

fn refreshed_outcome(status: &UpdateStatus) -> InstallOutcome {
    if status.available.is_some() {
        InstallOutcome::Refreshed
    } else {
        InstallOutcome::UpToDate
    }
}

/// Re-read the manifest once after a failed download, including a package
/// removed by mirror retention. Do not automatically retry or install the
/// replacement. Checking all download errors avoids relying on the plugin's
/// HTTP status text (it does not expose a structured download status code).
async fn refresh_after_download_failure(
    app: &AppHandle,
    failed_update: &Update,
    download_error: &str,
) -> Result<InstallOutcome, String> {
    log::warn!("Update download failed; refreshing the manifest: {download_error}");
    let service = app.state::<UpdateService>();
    service.update(app, |inner| {
        inner.status.phase = Phase::Checking;
        inner.status.downloaded = 0;
        inner.status.total = None;
    });
    finish_refresh(app, Some(failed_update.version.clone()), None)
        .await
        .map_err(|err| format!("{download_error}\nUpdate refresh failed: {err}"))?;
    let outcome = service
        .lock()
        .download_refresh_outcome(failed_update, download_error);
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A mutable local mirror: manifests can advance while an old package
    /// disappears. Downloads never return an installable package.
    struct Mirror {
        endpoint: String,
        version: Arc<Mutex<String>>,
        package_status: Arc<Mutex<u16>>,
        task: tokio::task::JoinHandle<()>,
    }

    impl Mirror {
        async fn start() -> Self {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let endpoint = format!("{base}/latest.json");
            let version = Arc::new(Mutex::new("0.2.0".to_owned()));
            let package_status = Arc::new(Mutex::new(404));
            let current = version.clone();
            let status = package_status.clone();
            let task = tokio::spawn(async move {
                loop {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    let mut request = [0; 4096];
                    let size = stream.read(&mut request).await.unwrap();
                    let manifest = request[..size].starts_with(b"GET /latest.json ");
                    let (code, body) = if manifest {
                        let version = current.lock().unwrap().clone();
                        (200, serde_json::json!({
                            "version": version,
                            "notes": format!("Notes for {version}"),
                            "platforms": {
                                "darwin-aarch64": { "url": format!("{base}/{version}.tar.gz"), "signature": "signed" },
                                "windows-x86_64": { "url": format!("{base}/{version}.exe"), "signature": "signed" }
                            }
                        }).to_string())
                    } else {
                        (*status.lock().unwrap(), "Package removed".into())
                    };
                    let response = format!(
                        "HTTP/1.1 {code} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    stream.write_all(response.as_bytes()).await.unwrap();
                }
            });
            Self {
                endpoint,
                version,
                package_status,
                task,
            }
        }
    }

    impl Drop for Mirror {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    fn mock_app() -> tauri::App<tauri::test::MockRuntime> {
        let mut context = tauri::test::mock_context(tauri::test::noop_assets());
        context.config_mut().plugins.0.insert(
            "updater".into(),
            serde_json::json!({ "pubkey": "test", "dangerousInsecureTransportProtocol": true }),
        );
        tauri::test::mock_builder()
            .plugin(tauri_plugin_updater::Builder::new().build())
            .build(context)
            .unwrap()
    }

    async fn mirror_check(
        app: &tauri::App<tauri::test::MockRuntime>,
        mirror: &Mirror,
        target: &str,
    ) -> tauri_plugin_updater::Result<Option<Update>> {
        app.updater_builder()
            .target(target)
            .endpoints(vec![mirror.endpoint.parse().unwrap()])?
            .configure_client(|client| client.no_proxy())
            .build()?
            .check()
            .await
    }

    fn enabled_inner() -> Inner {
        Inner {
            status: UpdateStatus {
                enabled: true,
                ..UpdateStatus::default()
            },
            ..Inner::default()
        }
    }

    #[test]
    fn available_updates_remain_due_for_daily_checks() {
        let mut inner = enabled_inner();
        inner.status.phase = Phase::Available;
        inner.status.available = Some(AvailableUpdate {
            version: "0.2.0".into(),
            notes: None,
            published_at: None,
        });
        inner.status.last_checked_at = Some(100);
        assert!(!check_due(&inner.status, 100 + CHECK_INTERVAL_SECS - 1));
        assert!(check_due(&inner.status, 100 + CHECK_INTERVAL_SECS));
        for phase in [Phase::Checking, Phase::Downloading, Phase::Installing] {
            inner.status.phase = phase;
            assert!(!check_due(&inner.status, 100 + CHECK_INTERVAL_SECS));
        }
        inner.status.phase = Phase::Available;
        inner.status.enabled = false;
        assert!(!check_due(&inner.status, 100 + CHECK_INTERVAL_SECS));
    }

    #[tokio::test]
    async fn install_preflight_requires_a_new_click_for_a_new_version_on_both_platforms() {
        let mirror = Mirror::start().await;
        let app = mock_app();
        for target in ["darwin-aarch64", "windows-x86_64"] {
            let mut inner = enabled_inner();
            *mirror.version.lock().unwrap() = "0.2.0".into();
            inner.begin_check().unwrap();
            assert!(inner
                .finish_check(mirror_check(&app, &mirror, target).await, None)
                .unwrap()
                .is_none());
            *mirror.version.lock().unwrap() = "0.5.0".into();
            inner.begin_check().unwrap();
            assert!(inner
                .finish_check(mirror_check(&app, &mirror, target).await, Some("0.2.0"))
                .unwrap()
                .is_none());
            let available = inner.status.available.as_ref().unwrap();
            assert_eq!(available.version, "0.5.0");
            assert_eq!(available.notes.as_deref(), Some("Notes for 0.5.0"));
            assert_eq!(inner.status.phase, Phase::Available);
            assert_eq!(refreshed_outcome(&inner.status), InstallOutcome::Refreshed);
            // Even after the background offer changed, an old UI click must
            // never approve the new version.
            inner.begin_check().unwrap();
            assert!(inner
                .finish_check(mirror_check(&app, &mirror, target).await, Some("0.2.0"))
                .unwrap()
                .is_none());
            inner.begin_check().unwrap();
            let ready = inner
                .finish_check(mirror_check(&app, &mirror, target).await, Some("0.5.0"))
                .unwrap()
                .unwrap();
            assert_eq!(ready.update.version, "0.5.0");
            assert!(ready.cached.is_none());
            assert_eq!(inner.status.phase, Phase::Downloading);
            assert_eq!(inner.begin_check().unwrap_err(), ERR_IN_PROGRESS);
        }
    }

    #[tokio::test]
    async fn removed_packages_refresh_the_offer_and_preserve_unresolved_errors() {
        let mirror = Mirror::start().await;
        let app = mock_app();
        for code in [404, 410] {
            *mirror.package_status.lock().unwrap() = code;
            *mirror.version.lock().unwrap() = "0.2.0".into();
            let mut inner = enabled_inner();
            inner.begin_check().unwrap();
            let ready = inner
                .finish_check(
                    mirror_check(&app, &mirror, "darwin-aarch64").await,
                    Some("0.2.0"),
                )
                .unwrap()
                .unwrap();
            let error = ready
                .update
                .download(|_, _| {}, || {})
                .await
                .unwrap_err()
                .to_string();
            assert!(error.contains(&code.to_string()));
            inner.status.phase = Phase::Checking;
            inner
                .finish_check(mirror_check(&app, &mirror, "darwin-aarch64").await, None)
                .unwrap();
            assert_eq!(
                inner
                    .download_refresh_outcome(&ready.update, &error)
                    .unwrap_err(),
                error
            );
            *mirror.version.lock().unwrap() = "0.5.0".into();
            inner.begin_check().unwrap();
            inner
                .finish_check(mirror_check(&app, &mirror, "darwin-aarch64").await, None)
                .unwrap();
            assert_eq!(
                inner
                    .download_refresh_outcome(&ready.update, &error)
                    .unwrap(),
                InstallOutcome::Refreshed
            );
            assert_eq!(inner.status.phase, Phase::Available);
            assert_eq!(
                inner.status.available.as_ref().unwrap().notes.as_deref(),
                Some("Notes for 0.5.0")
            );
        }
    }

    #[tokio::test]
    async fn failed_preflight_blocks_installation_and_withdrawn_offers_clear_the_cache() {
        let mirror = Mirror::start().await;
        let app = mock_app();
        let update = mirror_check(&app, &mirror, "darwin-aarch64")
            .await
            .unwrap()
            .unwrap();
        let mut inner = enabled_inner();
        inner.offer(update.clone());
        let checked = inner.status.last_checked_at;
        inner.begin_check().unwrap();
        let failure = inner.finish_check(
            Err(tauri_plugin_updater::Error::ReleaseNotFound),
            Some("0.2.0"),
        );
        assert!(failure.is_err());
        assert_eq!(inner.status.phase, Phase::Available);
        assert!(inner.status.error.is_some());
        assert_eq!(inner.status.last_checked_at, checked);
        assert_eq!(inner.pending.as_ref().unwrap().version, "0.2.0");
        for result in [
            Ok(None),
            Err(tauri_plugin_updater::Error::TargetNotFound(
                "windows-x86_64".into(),
            )),
        ] {
            inner.offer(update.clone());
            inner.downloaded_package = Some(cached_package());
            inner.status.downloaded = 3;
            inner.status.total = Some(3);
            inner.begin_check().unwrap();
            assert!(inner.finish_check(result, Some("0.2.0")).unwrap().is_none());
            assert_eq!(refreshed_outcome(&inner.status), InstallOutcome::UpToDate);
            assert!(inner.pending.is_none() && inner.downloaded_package.is_none());
            assert_eq!(inner.status.downloaded, 0);
            assert!(inner.status.total.is_none());
        }
    }

    fn cached_package() -> DownloadedPackage {
        DownloadedPackage {
            identity: PackageIdentity {
                version: "1.0.0".into(),
                target: "darwin-aarch64".into(),
                url: "https://example.com/update".into(),
                signature: "signed".into(),
            },
            bytes: Arc::new(vec![1, 2, 3]),
        }
    }

    #[test]
    fn busy_is_a_transient_refusal_and_keeps_the_verified_package() {
        let package = cached_package();
        let bytes = package.bytes.clone();
        let mut inner = Inner {
            downloaded_package: Some(package),
            ..Inner::default()
        };
        inner.install_failed(ERR_BUSY);
        assert_eq!(inner.status.phase, Phase::Available);
        assert!(inner.status.error.is_none());
        assert!(Arc::ptr_eq(
            &bytes,
            &inner.downloaded_package.as_ref().unwrap().bytes
        ));
        inner.install_failed("installer failed");
        assert_eq!(inner.status.phase, Phase::Error);
        assert_eq!(inner.status.error.as_deref(), Some("installer failed"));
    }

    #[test]
    fn repeated_checks_keep_only_the_same_signed_package() {
        let package = cached_package();
        let identity = package.identity.clone();
        let bytes = package.bytes.clone();
        for field in ["same", "version", "target", "url", "signature"] {
            let mut next = identity.clone();
            match field {
                "version" => next.version.push('2'),
                "target" => next.target.push('2'),
                "url" => next.url.push('2'),
                "signature" => next.signature.push('2'),
                _ => {}
            }
            let mut inner = Inner {
                downloaded_package: Some(package.clone()),
                ..Inner::default()
            };
            inner.status.downloaded = 3;
            inner.status.total = Some(3);
            inner.retain_package(&next);
            if field == "same" {
                assert!(Arc::ptr_eq(
                    &bytes,
                    &inner.downloaded_package.unwrap().bytes
                ));
                assert_eq!(inner.status.downloaded, 3);
            } else {
                assert!(inner.downloaded_package.is_none());
                assert_eq!(inner.status.downloaded, 0);
                assert!(inner.status.total.is_none());
            }
        }
    }

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
