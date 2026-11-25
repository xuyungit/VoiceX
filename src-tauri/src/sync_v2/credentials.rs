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
}
