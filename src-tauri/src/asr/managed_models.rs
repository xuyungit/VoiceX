//! VoiceX-owned Qwen models. No remote catalog or model updater: identities,
//! revisions and checksums ship with the app.

use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter, Manager};
use tokio_util::sync::CancellationToken;

const EVENT: &str = "local-models:updated";
static GLOBAL: OnceLock<ManagedModels> = OnceLock::new();
static CATALOG: OnceLock<Catalog> = OnceLock::new();

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Catalog {
    pub engine: Engine,
    pub models: Vec<ModelSpec>,
}
#[derive(Deserialize)]
pub struct Engine {
    pub version: String,
}
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSpec {
    pub id: String,
    pub label: String,
    pub revision: String,
    pub repository: String,
    pub huggingface_revision: String,
    pub modelscope_revision: String,
    pub files: Vec<ModelFile>,
}
#[derive(Clone, Deserialize)]
pub struct ModelFile {
    pub name: String,
    pub bytes: u64,
    pub sha256: String,
}

pub fn catalog() -> &'static Catalog {
    CATALOG.get_or_init(|| {
        serde_json::from_str(include_str!("../../../src/shared/localAsrModels.json"))
            .expect("bundled model catalog")
    })
}
pub fn default_model() -> String {
    catalog()
        .models
        .first()
        .expect("bundled model catalog is not empty")
        .id
        .clone()
}
fn model(id: &str) -> Result<&'static ModelSpec, String> {
    catalog()
        .models
        .iter()
        .find(|m| m.id == id)
        .ok_or_else(|| "unknown_model".into())
}
pub fn supported() -> bool {
    cfg!(all(target_os = "macos", target_arch = "aarch64"))
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Source {
    #[default]
    Modelscope,
    Huggingface,
}
impl Source {
    fn url(self, spec: &ModelSpec, file: &ModelFile) -> String {
        match self {
            Self::Huggingface => format!(
                "https://huggingface.co/{}/resolve/{}/{}",
                spec.repository, spec.huggingface_revision, file.name
            ),
            Self::Modelscope => format!(
                "https://modelscope.cn/api/v1/models/{}/repo?Revision={}&FilePath={}",
                spec.repository, spec.modelscope_revision, file.name
            ),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Phase {
    Missing,
    Ready,
    Downloading,
    Pausing,
    Paused,
    Verifying,
    WaitingForIdle,
    Error,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelStatus {
    pub id: String,
    pub label: String,
    pub phase: Phase,
    pub installed: bool,
    pub download_bytes: u64,
    pub downloaded_bytes: u64,
    pub disk_bytes: u64,
    pub speed_bytes_per_second: u64,
    pub in_use: bool,
    pub error: Option<String>,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub supported: bool,
    pub platform: &'static str,
    pub architecture: &'static str,
    pub engine_available: bool,
    pub engine_version: String,
    pub source: Source,
    pub models: Vec<ModelStatus>,
}

#[derive(Clone)]
pub struct ManagedModels(Arc<Service>);
struct Service {
    root: PathBuf,
    engine: PathBuf,
    app: Option<AppHandle>,
    inner: Mutex<Inner>,
}
#[derive(Default)]
struct Inner {
    job: Option<Job>,
    states: HashMap<String, (Phase, Option<String>, u64)>,
    leases: HashMap<String, usize>,
    source: Source,
}
struct Job {
    id: String,
    cancel: CancellationToken,
    done: CancellationToken,
}

pub struct ModelLease {
    manager: ManagedModels,
    id: String,
    pub path: PathBuf,
}
impl Drop for ModelLease {
    fn drop(&mut self) {
        let mut state = self.manager.0.inner.lock().unwrap();
        *state.leases.get_mut(&self.id).expect("active model lease") -= 1;
        drop(state);
        // Inference may have created a large cache beside the model weights.
        self.manager.emit();
    }
}

fn metadata_len(path: &Path) -> Result<u64, String> {
    match fs::metadata(path) {
        Ok(m) => Ok(m.len()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(e.to_string()),
    }
}
fn disk_bytes(path: &Path) -> Result<u64, String> {
    if !path.exists() {
        return Ok(0);
    }
    let mut total = 0;
    for entry in fs::read_dir(path).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        if kind.is_dir() {
            total += disk_bytes(&entry.path())?;
        } else if kind.is_file() {
            total += entry.metadata().map_err(|e| e.to_string())?.len();
        }
    }
    Ok(total)
}
fn remove_directory(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => {
            Err("Managed model directory is a symbolic link".into())
        }
        Ok(_) => fs::remove_dir_all(path).map_err(|e| e.to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}
fn write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let temporary = path.with_extension("json.tmp");
    fs::write(
        &temporary,
        serde_json::to_vec(value).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    fs::rename(temporary, path).map_err(|e| e.to_string())
}
#[cfg(target_os = "macos")]
fn available_space(path: &Path) -> Result<u64, String> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    let mut info = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::statvfs(path.as_ptr(), info.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let info = unsafe { info.assume_init() };
    Ok(info.f_bavail as u64 * info.f_frsize as u64)
}
#[cfg(not(target_os = "macos"))]
fn available_space(_: &Path) -> Result<u64, String> {
    Err("unsupported_platform".into())
}

impl ManagedModels {
    pub fn open(root: PathBuf, engine: PathBuf) -> Result<Self, String> {
        fs::create_dir_all(root.join(".downloads")).map_err(|e| e.to_string())?;
        // Recover an interrupted directory replacement before admitting reads.
        for spec in &catalog().models {
            let backup = root.join(format!(".backup-{}", spec.id));
            if backup.exists() {
                if root.join(&spec.id).exists() {
                    remove_directory(&backup)?;
                } else {
                    fs::rename(&backup, root.join(&spec.id)).map_err(|e| e.to_string())?;
                }
            }
        }
        let source = match fs::read(root.join("source.json")) {
            Ok(data) => {
                serde_json::from_slice(&data).map_err(|e| format!("Download preference: {e}"))?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Source::default(),
            Err(e) => return Err(e.to_string()),
        };
        Ok(Self(Arc::new(Service {
            root,
            engine,
            app: None,
            inner: Mutex::new(Inner {
                source,
                ..Inner::default()
            }),
        })))
    }

    pub fn init(app: &AppHandle) -> Result<(), String> {
        let engine = if cfg!(debug_assertions) {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("binaries/qwen-asr-aarch64-apple-darwin")
        } else {
            std::env::current_exe()
                .map_err(|e| e.to_string())?
                .parent()
                .ok_or("No executable directory")?
                .join("qwen-asr")
        };
        let mut manager = Self::open(
            app.path()
                .app_data_dir()
                .map_err(|e| e.to_string())?
                .join("models/qwen3-asr"),
            engine,
        )?;
        Arc::get_mut(&mut manager.0).expect("new model service").app = Some(app.clone());
        GLOBAL
            .set(manager.clone())
            .map_err(|_| "Local models already initialized")?;
        app.manage(manager);
        Ok(())
    }

    pub fn global() -> Result<&'static Self, String> {
        GLOBAL
            .get()
            .ok_or_else(|| "Local model manager is not initialized".into())
    }
    pub fn install_global(&self) -> Result<(), String> {
        GLOBAL
            .set(self.clone())
            .map_err(|_| "Local models already initialized".into())
    }
    pub fn engine(&self) -> Result<PathBuf, String> {
        if !supported() {
            return Err("unsupported_platform".into());
        }
        if !self.0.engine.is_file() {
            return Err("bundled_engine_missing".into());
        }
        Ok(self.0.engine.clone())
    }
    fn directory(&self, id: &str) -> PathBuf {
        self.0.root.join(id)
    }
    fn staging(&self, id: &str) -> PathBuf {
        self.0.root.join(".downloads").join(id)
    }
    fn installed(&self, spec: &ModelSpec) -> Result<bool, String> {
        let directory = self.directory(&spec.id);
        let receipt = match fs::read(directory.join("voicex-model.json")) {
            Ok(data) => {
                serde_json::from_slice::<serde_json::Value>(&data).map_err(|e| e.to_string())?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e.to_string()),
        };
        if receipt["id"] != spec.id || receipt["revision"] != spec.revision {
            return Err("Model receipt does not match this application; download again".into());
        }
        for file in &spec.files {
            if metadata_len(&directory.join(&file.name))? != file.bytes {
                return Err("Model files are incomplete; download again".into());
            }
        }
        Ok(true)
    }
    pub fn status(&self) -> Result<Status, String> {
        let state = self.0.inner.lock().unwrap();
        let mut models = Vec::new();
        for spec in &catalog().models {
            let (installed, invalid) = match self.installed(spec) {
                Ok(value) => (value, None),
                Err(e) => (false, Some(e)),
            };
            let mut downloaded = 0;
            for file in &spec.files {
                downloaded +=
                    metadata_len(&self.staging(&spec.id).join(&file.name))?.min(file.bytes);
            }
            let default_phase = if invalid.is_some() {
                Phase::Error
            } else if downloaded > 0 {
                Phase::Paused
            } else if installed {
                Phase::Ready
            } else {
                Phase::Missing
            };
            let (phase, error, speed) =
                state
                    .states
                    .get(&spec.id)
                    .cloned()
                    .unwrap_or((default_phase, invalid, 0));
            models.push(ModelStatus {
                id: spec.id.clone(),
                label: spec.label.clone(),
                phase,
                installed,
                download_bytes: spec.files.iter().map(|f| f.bytes).sum(),
                downloaded_bytes: downloaded,
                disk_bytes: disk_bytes(&self.directory(&spec.id))?
                    + disk_bytes(&self.staging(&spec.id))?,
                speed_bytes_per_second: speed,
                in_use: state.leases.get(&spec.id).copied().unwrap_or(0) > 0,
                error,
            });
        }
        Ok(Status {
            supported: supported(),
            platform: std::env::consts::OS,
            architecture: std::env::consts::ARCH,
            engine_available: supported() && self.0.engine.is_file(),
            engine_version: catalog().engine.version.clone(),
            source: state.source,
            models,
        })
    }
    fn emit(&self) {
        if let Some(app) = &self.0.app {
            match self.status() {
                Ok(status) => {
                    if let Err(e) = app.emit(EVENT, status) {
                        log::warn!("Model status event failed: {e}");
                    }
                }
                Err(e) => log::error!("Cannot read local model status: {e}"),
            }
        }
    }
    fn progress(&self, id: &str, phase: Phase, speed: u64) {
        self.0
            .inner
            .lock()
            .unwrap()
            .states
            .insert(id.into(), (phase, None, speed));
        self.emit();
    }
    pub async fn start(&self, id: &str, source: Source, restart: bool) -> Result<(), String> {
        self.engine()?;
        let spec = model(id)?;
        let cancel = CancellationToken::new();
        let done = CancellationToken::new();
        {
            let mut state = self.0.inner.lock().unwrap();
            if state.job.is_some() {
                return Err("download_in_progress".into());
            }
            let total: u64 = spec.files.iter().map(|file| file.bytes).sum();
            let partial = if restart {
                0
            } else {
                disk_bytes(&self.staging(id))?.min(total)
            };
            // Reserve space for the engine's generated INT8 cache as well as weights.
            if available_space(&self.0.root)? < total - partial + total / 2 {
                return Err("insufficient_space".into());
            }
            if restart {
                remove_directory(&self.staging(id))?;
            }
            fs::create_dir_all(self.staging(id)).map_err(|e| e.to_string())?;
            write_json(&self.0.root.join("source.json"), &source)?;
            state.source = source;
            state
                .states
                .insert(id.into(), (Phase::Downloading, None, 0));
            state.job = Some(Job {
                id: id.into(),
                cancel: cancel.clone(),
                done: done.clone(),
            });
        }
        self.emit();
        let manager = self.clone();
        tauri::async_runtime::spawn(async move {
            let result = manager.download(spec, source, &cancel).await;
            {
                let mut state = manager.0.inner.lock().unwrap();
                state.job = None;
                let status = match result {
                    Ok(()) => (Phase::Ready, None, 0),
                    Err(_) if cancel.is_cancelled() => (Phase::Paused, None, 0),
                    Err(error) => {
                        log::warn!("Local model {} download failed: {error}", spec.id);
                        (Phase::Error, Some(error), 0)
                    }
                };
                state.states.insert(spec.id.clone(), status);
            }
            done.cancel();
            manager.emit();
        });
        Ok(())
    }
    pub async fn pause(&self, id: &str) -> Result<(), String> {
        model(id)?;
        let done = {
            let mut state = self.0.inner.lock().unwrap();
            let Some(job) = &state.job else { return Ok(()) };
            if job.id != id {
                return Err("download_in_progress".into());
            }
            job.cancel.cancel();
            let done = job.done.clone();
            state.states.insert(id.into(), (Phase::Pausing, None, 0));
            done
        };
        self.emit();
        done.cancelled().await;
        Ok(())
    }
    pub fn remove(&self, id: &str, partial_only: bool) -> Result<(), String> {
        if !supported() {
            return Err("unsupported_platform".into());
        }
        model(id)?;
        let mut state = self.0.inner.lock().unwrap();
        if state.job.as_ref().is_some_and(|job| job.id == id) {
            return Err("download_in_progress".into());
        }
        if !partial_only && state.leases.get(id).copied().unwrap_or(0) > 0 {
            return Err("model_in_use".into());
        }
        let _work = self
            .0
            .app
            .as_ref()
            .map(|app| {
                app.state::<crate::updater::UpdateService>()
                    .work_gate()
                    .try_work()
                    .ok_or("update_installing")
            })
            .transpose()?;
        if !partial_only {
            remove_directory(&self.directory(id))?;
        }
        remove_directory(&self.staging(id))?;
        state.states.remove(id);
        drop(state);
        self.emit();
        Ok(())
    }
    pub fn lease(&self, id: &str) -> Result<ModelLease, String> {
        let spec = model(id)?;
        let mut state = self.0.inner.lock().unwrap();
        if matches!(
            state.states.get(id).map(|s| s.0),
            Some(Phase::WaitingForIdle)
        ) {
            return Err("model_in_use".into());
        }
        if !self.installed(spec)? {
            return Err("model_not_installed".into());
        }
        *state.leases.entry(id.into()).or_default() += 1;
        drop(state);
        self.emit();
        Ok(ModelLease {
            manager: self.clone(),
            id: id.into(),
            path: self.directory(id),
        })
    }
    async fn download(
        &self,
        spec: &ModelSpec,
        source: Source,
        cancel: &CancellationToken,
    ) -> Result<(), String> {
        let client = reqwest_updater_proxy::Client::builder()
            .user_agent("VoiceX-Models")
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| e.to_string())?;
        for file in &spec.files {
            let mut last = Instant::now();
            let mut last_bytes = metadata_len(&self.staging(&spec.id).join(&file.name))?;
            super::model_download::download_file(
                &client,
                &source.url(spec, file),
                &self.staging(&spec.id).join(&file.name),
                file,
                cancel,
                |bytes, verifying| {
                    if last.elapsed() >= Duration::from_millis(200) {
                        let speed = if verifying {
                            0
                        } else {
                            ((bytes.saturating_sub(last_bytes)) as f64
                                / last.elapsed().as_secs_f64()) as u64
                        };
                        self.progress(
                            &spec.id,
                            if verifying {
                                Phase::Verifying
                            } else {
                                Phase::Downloading
                            },
                            speed,
                        );
                        last = Instant::now();
                        last_bytes = bytes;
                    }
                },
            )
            .await?;
        }
        self.progress(&spec.id, Phase::WaitingForIdle, 0);
        loop {
            if cancel.is_cancelled() {
                return Err("cancelled".into());
            }
            {
                let state = self.0.inner.lock().unwrap();
                if state.leases.get(&spec.id).copied().unwrap_or(0) == 0 {
                    // Keep the app updater out of the short directory transaction.
                    let _work = self
                        .0
                        .app
                        .as_ref()
                        .map(|app| {
                            app.state::<crate::updater::UpdateService>()
                                .work_gate()
                                .try_work()
                                .ok_or("update_installing")
                        })
                        .transpose()?;
                    self.publish(spec)?;
                    return Ok(());
                }
            }
            tokio::select! { _ = cancel.cancelled() => return Err("cancelled".into()), _ = tokio::time::sleep(Duration::from_millis(100)) => {} }
        }
    }
    fn publish(&self, spec: &ModelSpec) -> Result<(), String> {
        let stage = self.staging(&spec.id);
        write_json(
            &stage.join("voicex-model.json"),
            &serde_json::json!({"id":spec.id,"revision":spec.revision}),
        )?;
        let destination = self.directory(&spec.id);
        let backup = self.0.root.join(format!(".backup-{}", spec.id));
        if destination.exists() {
            fs::rename(&destination, &backup).map_err(|e| e.to_string())?;
        }
        if let Err(error) = fs::rename(&stage, &destination) {
            if backup.exists() {
                fs::rename(&backup, &destination).map_err(|restore| {
                    format!("Install failed: {error}; restoring model also failed: {restore}")
                })?;
            }
            return Err(error.to_string());
        }
        remove_directory(&backup)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalog_pins_all_files_and_both_sources_to_revisions() {
        for spec in &catalog().models {
            assert_eq!(spec.huggingface_revision.len(), 40);
            assert_eq!(spec.modelscope_revision.len(), 40);
            for file in &spec.files {
                assert_eq!(file.sha256.len(), 64);
                assert!(file.bytes > 0);
                assert!(!file.name.contains('/'));
            }
        }
    }
    #[test]
    fn partial_downloads_survive_reopening_and_unknown_paths_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let service = ManagedModels::open(root.path().into(), root.path().join("engine")).unwrap();
        let spec = &catalog().models[0];
        fs::create_dir_all(service.staging(&spec.id)).unwrap();
        fs::write(service.staging(&spec.id).join("config.json"), b"partial").unwrap();
        let reopened = ManagedModels::open(root.path().into(), root.path().join("engine")).unwrap();
        let status = reopened.status().unwrap();
        assert_eq!(status.models[0].phase, Phase::Paused);
        assert_eq!(status.models[0].downloaded_bytes, 7);
        assert!(model("../../other-data").is_err());
        assert!(reopened.lease(&spec.id).is_err());
    }

    fn staged_files(service: &ManagedModels, spec: &ModelSpec) {
        fs::create_dir_all(service.staging(&spec.id)).unwrap();
        for file in &spec.files {
            fs::File::create(service.staging(&spec.id).join(&file.name))
                .unwrap()
                .set_len(file.bytes)
                .unwrap();
        }
    }

    #[test]
    fn replacement_is_atomic_and_an_interrupted_replacement_is_recovered() {
        let root = tempfile::tempdir().unwrap();
        let service = ManagedModels::open(root.path().into(), root.path().join("engine")).unwrap();
        let spec = &catalog().models[0];
        staged_files(&service, spec);
        service.publish(spec).unwrap();
        assert!(service.installed(spec).unwrap());
        fs::write(
            service.directory(&spec.id).join("qwen-asr-int8.sidecar"),
            b"generated cache",
        )
        .unwrap();
        staged_files(&service, spec);
        assert!(service.lease(&spec.id).is_ok());
        service.publish(spec).unwrap();
        assert!(!service
            .directory(&spec.id)
            .join("qwen-asr-int8.sidecar")
            .exists());
        let backup = root.path().join(format!(".backup-{}", spec.id));
        fs::rename(service.directory(&spec.id), backup).unwrap();
        let reopened = ManagedModels::open(root.path().into(), root.path().join("engine")).unwrap();
        assert!(reopened.installed(spec).unwrap());
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn deletion_waits_for_recognition_and_removes_only_the_managed_model() {
        let root = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        fs::write(external.path().join("keep"), b"external model").unwrap();
        let service = ManagedModels::open(root.path().into(), root.path().join("engine")).unwrap();
        let spec = &catalog().models[0];
        staged_files(&service, spec);
        service.publish(spec).unwrap();
        let lease = service.lease(&spec.id).unwrap();
        assert_eq!(service.remove(&spec.id, false).unwrap_err(), "model_in_use");
        drop(lease);
        service.remove(&spec.id, false).unwrap();
        assert!(!service.directory(&spec.id).exists());
        assert!(external.path().join("keep").exists());
    }
}
