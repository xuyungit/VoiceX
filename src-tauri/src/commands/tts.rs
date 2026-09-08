//! Selected-text reading commands.
//!
//! Every command here is `async` on purpose: Tauri runs synchronous commands
//! on the main thread, and both the voice list and the preview hop to the main
//! thread themselves — doing that from the main thread would deadlock.

use serde::Serialize;
use tauri::State;

use crate::hotkey::{HotkeyConfiguration, HotkeyManager, ReadSelectionStatus};
use crate::tts::TtsController;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TtsVoiceOption {
    pub id: String,
    pub name: String,
    pub language: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TtsVoiceList {
    pub voices: Vec<TtsVoiceOption>,
    /// The model lists no voices and every request needs a typed id (a cloned
    /// or designed voice); the page shows a text field instead of the picker.
    pub custom_voice_only: bool,
}

/// Voices a backend offers, for the voice picker.
///
/// `provider` is explicit because the settings page asks right after the user
/// picks one, before the debounced save has written it — resolving it from the
/// store would answer about the previous provider and list its voices instead.
/// `model` is explicit for the same reason: under Alibaba Cloud the model
/// families have entirely separate voice tables, so a stale read there is just
/// as wrong.
#[tauri::command]
pub async fn list_tts_voices(
    tts: State<'_, TtsController>,
    provider: Option<String>,
    model: Option<String>,
) -> Result<TtsVoiceList, String> {
    let controller = tts.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        controller.list_voices(provider.as_deref(), model.as_deref())
    })
    .await
    .map_err(|err| format!("Failed to list voices: {err}"))?
    .map(|list| TtsVoiceList {
        voices: list
            .voices
            .into_iter()
            .map(|voice| TtsVoiceOption {
                id: voice.id,
                name: voice.name,
                language: voice.language,
            })
            .collect(),
        custom_voice_only: list.custom_voice_only,
    })
    .map_err(|err| err.to_string())
}

/// Speak `text` with the saved voice parameters, bypassing the selection
/// reader. The caller supplies the sample so it can be in the UI's language.
#[tauri::command]
pub async fn preview_tts(tts: State<'_, TtsController>, text: String) -> Result<(), String> {
    let controller = tts.inner().clone();
    tauri::async_runtime::spawn_blocking(move || controller.speak_preview(text))
        .await
        .map_err(|err| format!("Failed to start the preview: {err}"))?
        .map_err(|err| err.to_string())
}

/// Stop whatever is being spoken. Used by the settings page to cancel a
/// preview; the read hotkey has its own path into the same session.
#[tauri::command]
pub async fn stop_tts(tts: State<'_, TtsController>) -> Result<(), String> {
    let controller = tts.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        controller.stop(crate::tts::StopReason::Ui);
    })
    .await
    .map_err(|err| format!("Failed to stop speech: {err}"))
}

/// Apply the reading hotkey. `enabled: false` unbinds it entirely, so the key
/// goes back to the foreground application instead of being swallowed.
///
/// Returns the resulting state, including whether the binding was refused for
/// colliding with the dictation key — the caller has no other way to find out.
#[tauri::command]
pub async fn apply_read_selection_hotkey(
    manager: State<'_, HotkeyManager>,
    config: Option<String>,
    enabled: bool,
) -> Result<ReadSelectionStatus, String> {
    // Same rule as startup (`lib.rs`): bind only where a selection reader
    // exists. Off macOS the key would be swallowed from the foreground
    // application in exchange for a `platform_unsupported` error. The settings
    // page already refuses to send this, so this is the case where a synced
    // `ttsEnabled` reaches a platform that cannot honour it.
    #[cfg(not(target_os = "macos"))]
    let (config, enabled) = {
        let _ = (config, enabled);
        (None::<String>, false)
    };

    let binding = if enabled {
        Some(
            config
                .and_then(|value| HotkeyConfiguration::from_storage(&value))
                .unwrap_or_else(HotkeyConfiguration::default_read_selection),
        )
    } else {
        None
    };

    manager.set_read_selection_config(binding);
    Ok(manager.read_selection_status())
}

/// One diagnostic read, reported in full.
///
/// Plan §4.2 calls this L2b: the same `SelectionReader` the hotkey uses, run
/// under VoiceX's own TCC identity. That identity is the point — macOS grants
/// Accessibility per process, so a standalone probe passing says nothing about
/// whether the installed app can read anything.
///
/// Deliberately carries no text, only its length. This is meant to be pasted
/// into a bug report, and what someone had selected is their business.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionDiagnostics {
    pub ok: bool,
    /// Stable error code, matching what the structured log and HUD use.
    pub error: Option<String>,
    pub detail: Option<String>,
    pub source: Option<String>,
    pub chars: Option<usize>,
    pub elapsed_ms: Option<u64>,
    pub clipboard_restored: Option<bool>,
    /// Settings state, so a report explains itself without a follow-up question.
    pub clipboard_fallback_allowed: bool,
    pub accessibility: bool,
    pub input_monitoring: bool,
    pub probe: crate::selection::SelectionProbe,
}

/// Read the selection once and report everything observed.
///
/// `delay_ms` exists because clicking the button makes VoiceX frontmost, and a
/// read then finds its own window — the answer would always be `focus_is_self`.
/// The caller counts down while the user switches back to the application they
/// actually want diagnosed.
#[tauri::command]
pub async fn diagnose_selection(
    app: tauri::AppHandle,
    delay_ms: Option<u64>,
) -> Result<SelectionDiagnostics, String> {
    let delay = delay_ms.unwrap_or(0).min(30_000);
    tokio::time::sleep(std::time::Duration::from_millis(delay)).await;

    let settings = crate::storage::get_settings().map_err(|err| err.to_string())?;
    let allow_clipboard_fallback = settings.tts_clipboard_fallback;

    let report = tauri::async_runtime::spawn_blocking(move || {
        crate::selection::read_selection_reporting(crate::selection::SelectionRequest {
            app,
            allow_clipboard_fallback,
            // The diagnostic read has no session to cancel it; it runs to
            // completion or to its own timeouts.
            cancelled: None,
        })
    })
    .await
    .map_err(|err| format!("Diagnostic read failed to run: {err}"))?;

    let permission = crate::hotkey::HotkeyPermissionStatus::detect();
    let mut diagnostics = SelectionDiagnostics {
        ok: report.outcome.is_ok(),
        error: None,
        detail: None,
        source: None,
        chars: None,
        elapsed_ms: None,
        clipboard_restored: None,
        clipboard_fallback_allowed: allow_clipboard_fallback,
        accessibility: permission.accessibility,
        input_monitoring: permission.input_monitoring,
        probe: report.probe,
    };

    match report.outcome {
        Ok(outcome) => {
            diagnostics.source = Some(outcome.source.as_str().to_string());
            diagnostics.chars = Some(outcome.text.chars().count());
            diagnostics.elapsed_ms = Some(outcome.elapsed_ms);
            diagnostics.clipboard_restored = outcome.clipboard_restored;
        }
        Err(err) => {
            diagnostics.error = Some(err.code().to_string());
            diagnostics.detail = Some(err.to_string());
        }
    }

    Ok(diagnostics)
}

/// Current state of the reading binding. The settings page asks on mount
/// because the dictation key may have changed since it last applied one.
#[tauri::command]
pub async fn read_selection_hotkey_status(
    manager: State<'_, HotkeyManager>,
) -> Result<ReadSelectionStatus, String> {
    Ok(manager.read_selection_status())
}
