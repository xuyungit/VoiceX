//! The correction prompt and hotword dictionary the installed app is using, read from its database, and the system
//! prompt the app builds from them. With `prompt_source = "app"` the bench sends what a dictation sends.

use std::path::PathBuf;

const DICTIONARY_PLACEHOLDER: &str = "{{DICTIONARY}}";
const INPUT_HISTORY_PLACEHOLDER: &str = "{{INPUT_HISTORY}}";

/// What the app sends for a dictation, as far as the bench can reproduce it.
pub struct AppPrompt {
    pub db: PathBuf,
    pub prompt: String,
    pub dictionary: String,
    /// The app adds recent dictations to the prompt; the bench has no history to add, so it cannot match.
    pub history_context: bool,
}

/// `VOICEX_DB`, or `voicex.db` in the app's data directory (Tauri `app_data_dir` for `com.voicex.app`).
pub fn default_db() -> Result<PathBuf, String> {
    if let Ok(path) = std::env::var("VOICEX_DB") {
        return Ok(PathBuf::from(path));
    }
    let base = if cfg!(target_os = "macos") {
        std::env::var("HOME").map(|home| PathBuf::from(home).join("Library/Application Support"))
    } else if cfg!(windows) {
        std::env::var("APPDATA").map(PathBuf::from)
    } else {
        std::env::var("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|_| std::env::var("HOME").map(|home| PathBuf::from(home).join(".local/share")))
    };
    base.map(|dir| dir.join("com.voicex.app").join("voicex.db"))
        .map_err(|_| "cannot locate the app data directory; set VOICEX_DB".to_string())
}

pub fn load() -> Result<AppPrompt, String> {
    let db = default_db()?;
    let conn = rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("cannot open {}: {e}", db.display()))?;
    let json: String = conn
        .query_row("SELECT value FROM user_config WHERE key = 'app_settings' LIMIT 1", [], |row| row.get(0))
        .map_err(|e| format!("no app settings in {}: {e}", db.display()))?;
    let settings: serde_json::Value =
        serde_json::from_str(&json).map_err(|e| format!("app settings in {} are not JSON: {e}", db.display()))?;
    let text = |key: &str| -> Result<String, String> {
        settings[key].as_str().map(str::to_string).ok_or_else(|| format!("app settings have no `{key}`"))
    };
    Ok(AppPrompt {
        prompt: text("llmPromptTemplate")?,
        dictionary: text("dictionaryText")?,
        history_context: settings["enableLlmHistoryContext"].as_bool().unwrap_or(false),
        db,
    })
}

/// The app's `format_dictionary`: one trimmed term per line, duplicates dropped, `（空）` when there is none.
fn format_dictionary(raw: &str) -> String {
    let mut terms: Vec<&str> = Vec::new();
    for term in raw.lines().map(str::trim).filter(|t| !t.is_empty()) {
        if !terms.contains(&term) {
            terms.push(term);
        }
    }
    if terms.is_empty() { "（空）".to_string() } else { terms.join("\n") }
}

/// The app's `build_system_prompt` for a dictation with history context off.
pub fn system_prompt(template: &str, dictionary: &str) -> String {
    let dictionary = format_dictionary(dictionary);
    let mut prompt = template.replace(INPUT_HISTORY_PLACEHOLDER, "（已关闭）");
    if prompt.contains(DICTIONARY_PLACEHOLDER) {
        prompt = prompt.replace(DICTIONARY_PLACEHOLDER, &dictionary);
    } else {
        prompt = format!("{prompt}\n\n用户热词词典：\n{dictionary}");
    }
    prompt
}

#[cfg(test)]
mod tests {
    use super::system_prompt;

    #[test]
    fn builds_the_prompt_like_the_app() {
        assert_eq!(system_prompt("词典：\n{{DICTIONARY}}\n历史：{{INPUT_HISTORY}}", " 支座 \n\nMidas\n支座\n"), "词典：\n支座\nMidas\n历史：（已关闭）");
        assert_eq!(system_prompt("整理文本", ""), "整理文本\n\n用户热词词典：\n（空）");
    }
}
