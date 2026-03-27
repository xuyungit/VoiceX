use serde::Serialize;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildInfo {
    pub version: String,
    pub profile: String,
    pub commit: String,
    pub built_at: String,
}

#[tauri::command]
pub fn get_build_info() -> BuildInfo {
    BuildInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        profile: env!("VOICEX_BUILD_PROFILE").to_string(),
        commit: env!("VOICEX_GIT_COMMIT").to_string(),
        built_at: env!("VOICEX_BUILD_TIME").to_string(),
    }
}
