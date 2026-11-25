//! Local file-backed credential storage for Sync v2 (`sync-credentials.json`).
//!
//! Stored separately from `voicex.db` in `app_data_dir` (mode `0600` on Unix,
//! per-user AppData ACL on Windows) so copying `voicex.db` does not carry write
//! credentials or decryption keys, while avoiding macOS Keychain prompts on
//! locally-signed desktop builds.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use thiserror::Error;

const CREDENTIALS_FILE_NAME: &str = "sync-credentials.json";
const CREDENTIALS_SCHEMA_VERSION: u32 = 1;
const DEVICE_IDENTITY_FILE_NAME: &str = "device-identity.json";
const DEVICE_IDENTITY_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Error)]
pub enum CredentialError {
    #[error("credential I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("credential JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpaceCredentials {
    pub device_token: String,
    pub recovery_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CredentialFile {
    version: u32,
    spaces: BTreeMap<String, SpaceCredentials>,
}

impl Default for CredentialFile {
    fn default() -> Self {
        Self {
            version: CREDENTIALS_SCHEMA_VERSION,
            spaces: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CredentialStore {
    path: PathBuf,
}

impl CredentialStore {
    pub fn in_data_dir(app_data_dir: &Path) -> Self {
        Self {
            path: app_data_dir.join(CREDENTIALS_FILE_NAME),
        }
    }

    pub fn at_path(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn get(&self, space_id: &str) -> Result<Option<SpaceCredentials>, CredentialError> {
        let file = self.load_file()?;
        Ok(file.spaces.get(space_id).cloned())
    }

    pub fn save(
        &self,
        space_id: &str,
        creds: &SpaceCredentials,
    ) -> Result<(), CredentialError> {
        let mut file = self.load_file()?;
        file.spaces.insert(space_id.to_string(), creds.clone());
        self.write_file_atomic(&file)
    }

    pub fn remove(&self, space_id: &str) -> Result<(), CredentialError> {
        if !self.path.exists() {
            return Ok(());
        }
        let mut file = self.load_file()?;
        if file.spaces.remove(space_id).is_some() {
            self.write_file_atomic(&file)?;
        }
        Ok(())
    }

    fn load_file(&self) -> Result<CredentialFile, CredentialError> {
        if !self.path.exists() {
            return Ok(CredentialFile::default());
        }
        let raw = fs::read_to_string(&self.path)?;
        if raw.trim().is_empty() {
            return Ok(CredentialFile::default());
        }
        Ok(serde_json::from_str(&raw)?)
    }

    fn write_file_atomic(&self, file: &CredentialFile) -> Result<(), CredentialError> {
        let parent = self
            .path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;

        let json = serde_json::to_string_pretty(file)?;
        let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tmp.as_file().set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        tmp.write_all(json.as_bytes())?;
        tmp.as_file().sync_all()?;
        tmp.persist(&self.path).map_err(|e| e.error)?;
        Ok(())
    }
}

/// Out-of-database installation identity (`device-identity.json`) bound to a
/// local machine fingerprint so cloning `voicex.db` or copying the entire data
/// directory onto another machine yields a fresh `device_id` instead of
/// hijacking the original machine's active token or stats shards (S4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeviceIdentityFile {
    version: u32,
    device_id: String,
    machine_fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedDeviceIdentity {
    pub device_id: String,
    /// True when a new `device_id` was allocated because the DB or data dir was
    /// cloned onto a different machine/installation.
    pub rotated_for_new_install: bool,
}

#[derive(Debug, Clone)]
pub struct DeviceIdentityStore {
    path: PathBuf,
}

impl DeviceIdentityStore {
    pub fn in_data_dir(app_data_dir: &Path) -> Self {
        Self {
            path: app_data_dir.join(DEVICE_IDENTITY_FILE_NAME),
        }
    }

    pub fn resolve_for_machine(
        &self,
        db_device_id: Option<&str>,
        machine_fingerprint: &str,
    ) -> Result<ResolvedDeviceIdentity, CredentialError> {
        let db_id_clean = db_device_id.map(str::trim).filter(|s| !s.is_empty());
        if self.path.exists() {
            let raw = fs::read_to_string(&self.path)?;
            if let Ok(existing) = serde_json::from_str::<DeviceIdentityFile>(&raw) {
                if !existing.device_id.trim().is_empty() {
                    if existing.machine_fingerprint == machine_fingerprint {
                        let rotated = db_id_clean.is_some_and(|d| d != existing.device_id);
                        return Ok(ResolvedDeviceIdentity {
                            device_id: existing.device_id,
                            rotated_for_new_install: rotated,
                        });
                    }
                    // Whole data directory was copied to a different machine -> mint a fresh device_id.
                    let fresh_id = uuid::Uuid::new_v4().to_string();
                    self.write_identity(&DeviceIdentityFile {
                        version: DEVICE_IDENTITY_SCHEMA_VERSION,
                        device_id: fresh_id.clone(),
                        machine_fingerprint: machine_fingerprint.to_string(),
                    })?;
                    return Ok(ResolvedDeviceIdentity {
                        device_id: fresh_id,
                        rotated_for_new_install: true,
                    });
                }
            }
        }

        // First initialization on this installation: adopt existing v1 DB device_id if present,
        // otherwise mint a new UUID, and bind it to this machine's fingerprint.
        let device_id = db_id_clean
            .map(str::to_string)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        self.write_identity(&DeviceIdentityFile {
            version: DEVICE_IDENTITY_SCHEMA_VERSION,
            device_id: device_id.clone(),
            machine_fingerprint: machine_fingerprint.to_string(),
        })?;
        Ok(ResolvedDeviceIdentity {
            device_id,
            rotated_for_new_install: false,
        })
    }

    pub fn resolve(&self, db_device_id: Option<&str>) -> Result<ResolvedDeviceIdentity, CredentialError> {
        let fp = current_machine_fingerprint();
        self.resolve_for_machine(db_device_id, &fp)
    }

    fn write_identity(&self, file: &DeviceIdentityFile) -> Result<(), CredentialError> {
        let parent = self
            .path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;

        let json = serde_json::to_string_pretty(file)?;
        let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tmp.as_file().set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        tmp.write_all(json.as_bytes())?;
        tmp.as_file().sync_all()?;
        tmp.persist(&self.path).map_err(|e| e.error)?;
        Ok(())
    }
}

/// Derive a stable, privacy-preserving SHA-256 machine fingerprint (cross-platform).
pub fn current_machine_fingerprint() -> String {
    use sha2::{Digest, Sha256};

    let raw_id = read_os_machine_id().unwrap_or_else(|| {
        let host = std::env::var("COMPUTERNAME")
            .or_else(|_| std::env::var("HOSTNAME"))
            .unwrap_or_else(|_| "unknown-host".to_string());
        let user = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_default();
        format!("{host}:{user}")
    });
    let mut hasher = Sha256::new();
    hasher.update(b"voicex-install-fp-v1:");
    hasher.update(std::env::consts::OS.as_bytes());
    hasher.update(b":");
    hasher.update(std::env::consts::ARCH.as_bytes());
    hasher.update(b":");
    hasher.update(raw_id.trim().as_bytes());
    hex::encode(hasher.finalize())
}

fn read_os_machine_id() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("ioreg")
            .args(["-rd1", "-c", "IOPlatformExpertDevice"])
            .output()
            .ok()?;
        if out.status.success() {
            let text = String::from_utf8_lossy(&out.stdout);
            for line in text.lines() {
                if line.contains("IOPlatformUUID") {
                    if let Some(uuid) = line.split('"').nth(3) {
                        if !uuid.trim().is_empty() {
                            return Some(uuid.trim().to_string());
                        }
                    }
                }
            }
        }
        None
    }
    #[cfg(target_os = "windows")]
    {
        let out = std::process::Command::new("reg")
            .args([
                "query",
                r"HKLM\SOFTWARE\Microsoft\Cryptography",
                "/v",
                "MachineGuid",
            ])
            .output()
            .ok()?;
        if out.status.success() {
            let text = String::from_utf8_lossy(&out.stdout);
            for line in text.lines() {
                if line.contains("MachineGuid") {
                    if let Some(val) = line.split_whitespace().last() {
                        if !val.trim().is_empty() {
                            return Some(val.trim().to_string());
                        }
                    }
                }
            }
        }
        None
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        fs::read_to_string("/etc/machine-id")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_save_get_remove_and_unix_permissions() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = CredentialStore::in_data_dir(dir.path());
        assert_eq!(store.get("space-1").unwrap(), None);

        let creds = SpaceCredentials {
            device_token: "vxd2_secret_token".into(),
            recovery_key: "VXK2-AAAAA-BBBBB".into(),
        };
        store.save("space-1", &creds).unwrap();
        assert_eq!(store.get("space-1").unwrap(), Some(creds.clone()));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(store.path()).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }

        store.remove("space-1").unwrap();
        assert_eq!(store.get("space-1").unwrap(), None);
    }

    #[test]
    fn device_identity_store_adopts_v1_and_rotates_on_cloned_machine() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = DeviceIdentityStore::in_data_dir(dir.path());

        // First run adopts v1 DB device_id on machine A
        let r1 = store.resolve_for_machine(Some("v1-dev-a"), "fp-machine-a").unwrap();
        assert_eq!(r1.device_id, "v1-dev-a");
        assert!(!r1.rotated_for_new_install);

        // Same machine, same DB -> stable
        let r2 = store.resolve_for_machine(Some("v1-dev-a"), "fp-machine-a").unwrap();
        assert_eq!(r2.device_id, "v1-dev-a");
        assert!(!r2.rotated_for_new_install);

        // Cloned DB from another device dropped into machine A -> keeps machine A's own ID
        let r3 = store
            .resolve_for_machine(Some("cloned-other-dev"), "fp-machine-a")
            .unwrap();
        assert_eq!(r3.device_id, "v1-dev-a");
        assert!(r3.rotated_for_new_install);

        // Whole data directory copied to machine B (different fingerprint) -> mints fresh ID!
        let r4 = store.resolve_for_machine(Some("v1-dev-a"), "fp-machine-b").unwrap();
        assert_ne!(r4.device_id, "v1-dev-a");
        assert!(r4.rotated_for_new_install);
    }
}
