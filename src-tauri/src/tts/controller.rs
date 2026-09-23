//! Session control for selected-text reading.
//!
//! Phase 0 keeps this deliberately small, but the cancellation contract is
//! already the real one: a single [`SessionSlot`] owns "a read-and-speak is in
//! progress", and every stage carries the [`CancelToken`] it was started with.
//! Phase 3 grows this into the full `TtsSession` state machine
//! (`Idle -> ReadingSelection -> Synthesizing -> Playing -> Idle`) — the log
//! events emitted here are already the assertion surface it will keep.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use tauri::{AppHandle, Emitter, Manager};

use super::aliyun::{self, AliyunBackend, AliyunConfig};
use super::azure::{self, AzureBackend, AzureConfig};
use super::clipboard_text;
use super::llm_stage::{self, LlmStageError, TRANSLATE_MAX_CHARS};
use super::mimo::{MimoBackend, MimoConfig};
use super::volcengine::{self, VolcengineBackend, VolcengineConfig};
use super::{
    log_event, SessionSlot, SpeechProgress, StopReason, TtsBackend, TtsError, TtsRequest,
    TtsStatus, TtsVoiceList,
};
use crate::commands::settings::AppSettings;
use crate::selection::{self, SelectionError, SelectionOutcome, SelectionRequest};
use crate::services::history_service::{
    sync_owns_counters, HistoryService, HISTORY_MODE_TRANSLATE_READ,
};
use crate::services::hud_service::{
    HudPresentation, HudService, ReadingKind, ReadingPhase, ReadingSource,
};
use crate::services::llm_service::{build_llm_config_for_key, settings_for_llm_key};
use crate::services::sync_service::SyncService;
use crate::storage::TtsCounters;

/// Longest preview we will speak. The settings page sends a short fixed
/// sentence; the cap only stops a malformed call from starting a long read.
const PREVIEW_MAX_CHARS: usize = 500;

/// How often the HUD driver samples the session and backend state.
///
/// Polling, not events: the eventual signal is the phase-3
/// `AVSpeechSynthesizerDelegate`, and building an observer channel before that
/// exists would mean designing it twice. It also has to work for the cloud
/// backend, which has no delegate at all.
const HUD_POLL: std::time::Duration = std::time::Duration::from_millis(60);

/// How long the HUD lingers after a read ends, so a short one does not blink.
const HUD_LINGER_MS: u64 = 400;

/// How long an error stays on screen. Longer, because it is the only place a
/// failed read is visible at all.
const HUD_ERROR_LINGER_MS: u64 = 2_600;

/// Longest piece a read with captions is synthesized in, in characters.
///
/// A caption is the piece being spoken, so the piece has to be about a
/// sentence: long enough that a normal sentence is not cut, short enough
/// that the HUD's four lines hold it. CosyVoice already ran at this size;
/// for the other providers it means more, smaller requests per read.
const CAPTION_PIECE_LIMIT: usize = 120;

/// Sentence-sized pieces cost extra requests, so a read is only split that
/// way when captions will show them: the setting is on, whichever hotkey
/// started the read. With it off every read keeps its backend's own piece
/// size.
fn caption_piece_limit(captions_enabled: bool) -> Option<usize> {
    captions_enabled.then_some(CAPTION_PIECE_LIMIT)
}

/// The caption to put on the HUD, or `None` while the one it shows stays.
///
/// Only a new sentence replaces a caption. A backend stops reporting progress
/// the moment its audio ends, a beat before the session does, and in the
/// text-only caption layout a cleared caption is an empty frame: the last
/// sentence stays up through the linger instead, and goes with the window.
fn next_caption(
    shown: Option<&SpeechProgress>,
    now: Option<SpeechProgress>,
) -> Option<SpeechProgress> {
    now.filter(|now| shown != Some(now))
}

/// Whether the HUD stays up for [`HUD_LINGER_MS`] after a read that ended
/// without an error.
///
/// The linger keeps a short read from blinking, and takes something on screen
/// to keep: the compact card always has it, the caption layout only once a
/// sentence has been shown. A read stopped before its first sentence would
/// linger as an empty frame, so its window goes at once.
fn lingers_after_read(captions: bool, shown: Option<&SpeechProgress>) -> bool {
    !captions || shown.is_some()
}

/// Settings values selecting a cloud backend.
const PROVIDER_VOLCENGINE: &str = "volcengine";
const PROVIDER_ALIYUN: &str = "aliyun";
const PROVIDER_MIMO: &str = "mimo";
const PROVIDER_AZURE: &str = "azure";

/// What a reading session does with its text before speaking it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadKind {
    /// Speak the text (through the preprocessor when that is on).
    Read,
    /// Translate the text with the LLM, then speak the translation.
    Translate,
}

/// Where a read's text came from. Orthogonal to [`ReadKind`]: everything
/// after the text is in hand — cleanup, translation, voice, captions,
/// counting — is the same whichever source it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadSource {
    /// The focused application's selection.
    Selection,
    /// The clipboard, read because no selection could be (see
    /// [`falls_back_to_clipboard`]).
    Clipboard,
}

impl ReadSource {
    fn as_str(self) -> &'static str {
        self.hud_source().as_str()
    }

    fn hud_source(self) -> ReadingSource {
        match self {
            ReadSource::Selection => ReadingSource::Selection,
            ReadSource::Clipboard => ReadingSource::Clipboard,
        }
    }
}

impl ReadKind {
    /// The key's name in the log (`hotkey_action`, `speak_start`, …). Named
    /// after the selection because that is what the key reads first; where
    /// the text actually came from is logged as `source`.
    fn action(self) -> &'static str {
        match self {
            ReadKind::Read => "read_selection",
            ReadKind::Translate => "translate_selection",
        }
    }

    fn hud_kind(self) -> ReadingKind {
        match self {
            ReadKind::Read => ReadingKind::Read,
            ReadKind::Translate => ReadingKind::Translate,
        }
    }
}

/// What the LLM stage handed back for speaking.
struct StagedText {
    text: String,
    llm_invoked: bool,
}

/// Whether `provider` names one of the network backends. The controller asks
/// this in several places — voice listing, request building — and a new cloud
/// provider that missed one of them would silently fall back to the system
/// voice there.
fn is_cloud_provider(provider: &str) -> bool {
    matches!(
        provider,
        PROVIDER_VOLCENGINE | PROVIDER_ALIYUN | PROVIDER_MIMO | PROVIDER_AZURE
    )
}

#[derive(Default)]
struct ControllerInner {
    app: Mutex<Option<AppHandle>>,
    /// Compact macOS voices via AVSpeechSynthesizer. Absent on other platforms.
    system: Mutex<Option<Arc<dyn TtsBackend>>>,
    /// Spoken Content / Siri voice via `/usr/bin/say`. Used when the system
    /// provider is selected and the voice id is empty. Absent on other platforms.
    say: Mutex<Option<Arc<dyn TtsBackend>>>,
    /// Kept as their concrete types so credentials can be re-applied without
    /// rebuilding them — the settings page changes them while the app runs.
    volcengine: Mutex<Option<Arc<VolcengineBackend>>>,
    aliyun: Mutex<Option<Arc<AliyunBackend>>>,
    mimo: Mutex<Option<Arc<MimoBackend>>>,
    azure: Mutex<Option<Arc<AzureBackend>>>,
    /// Whichever backend owns the current session. `stop` has to reach that
    /// one, not whichever provider the settings happen to name right now.
    active: Mutex<Option<Arc<dyn TtsBackend>>>,
    /// Owns the whole read-and-speak lifetime, from the moment a read starts
    /// until speech ends, fails, or is stopped. One flag, one owner — the
    /// previous split between a `reading` bool and the backend's own state had
    /// windows where neither said "busy" and a second hotkey press started a
    /// second read instead of stopping the first.
    session: SessionSlot,
    /// Set while dictation is recording. Reading is refused then: the speech
    /// would be picked up by the microphone and transcribed.
    recording: Mutex<Option<Arc<AtomicBool>>>,
    /// Shared with the dictation session rather than built fresh, so both
    /// drive one HUD window and cannot fight over its hide timer. Reading and
    /// dictation are mutually exclusive, so they never want it at once.
    hud: Mutex<Option<HudService>>,
    /// Set just before dictation stops a read. The HUD driver must not hide
    /// afterwards — dictation is about to use the same window, and a linger
    /// hide would take it down a few hundred milliseconds into recording.
    hud_yielded: Arc<AtomicBool>,
}

#[derive(Clone, Default)]
pub struct TtsController {
    inner: Arc<ControllerInner>,
}

impl TtsController {
    pub fn init_with_handle(&self, app: &AppHandle) {
        if let Ok(mut slot) = self.inner.app.lock() {
            *slot = Some(app.clone());
        }

        #[cfg(target_os = "macos")]
        {
            let backend: Arc<dyn TtsBackend> =
                Arc::new(super::mac_system::MacSystemBackend::new(app.clone()));
            if let Ok(mut slot) = self.inner.system.lock() {
                *slot = Some(backend);
            }
            let say: Arc<dyn TtsBackend> = Arc::new(super::mac_say::MacSayBackend::new());
            if let Ok(mut slot) = self.inner.say.lock() {
                *slot = Some(say);
            }
        }

        // Built unconditionally: they are network-only, so they work wherever
        // the app runs, including where there is no system voice at all.
        if let Ok(mut slot) = self.inner.volcengine.lock() {
            *slot = Some(Arc::new(VolcengineBackend::new(VolcengineConfig {
                api_key: String::new(),
                resource_id: volcengine::DEFAULT_RESOURCE_ID.to_string(),
            })));
        }
        if let Ok(mut slot) = self.inner.aliyun.lock() {
            *slot = Some(Arc::new(AliyunBackend::new(AliyunConfig {
                api_key: String::new(),
                model: aliyun::default_model().to_string(),
            })));
        }
        if let Ok(mut slot) = self.inner.mimo.lock() {
            *slot = Some(Arc::new(MimoBackend::new(MimoConfig {
                api_key: String::new(),
                instruction: String::new(),
            })));
        }
        if let Ok(mut slot) = self.inner.azure.lock() {
            *slot = Some(Arc::new(AzureBackend::new(AzureConfig {
                api_key: String::new(),
                region: azure::DEFAULT_REGION.to_string(),
            })));
        }
    }

    /// Share the dictation session's recording flag so reads can be refused
    /// while the microphone is live.
    pub fn attach_recording_flag(&self, recording: Arc<AtomicBool>) {
        if let Ok(mut slot) = self.inner.recording.lock() {
            *slot = Some(recording);
        }
    }

    /// Share the dictation session's HUD so reads can show their state there.
    pub fn attach_hud(&self, hud: HudService) {
        if let Ok(mut slot) = self.inner.hud.lock() {
            *slot = Some(hud);
        }
    }

    fn hud(&self) -> Option<HudService> {
        self.inner.hud.lock().ok().and_then(|slot| slot.clone())
    }

    /// Show the read's progress until the session ends, then get out of the way.
    ///
    /// Runs for the lifetime of one read. Phase comes from the backend's own
    /// `status`, which reports whether audio is actually coming out — the point
    /// of the HUD is the 300-700 ms before that happens on a cloud provider
    /// (plan §5.4), which is otherwise indistinguishable from a dead hotkey.
    /// `failure` is how the read reports a problem: the driver owns the HUD for
    /// the whole read, so letting anyone else write to it would race the hide it
    /// schedules on the way out. `from_clipboard` is how the worker says the
    /// text came from the clipboard, for the same reason.
    fn spawn_hud_driver(
        &self,
        kind: ReadKind,
        from_clipboard: Arc<AtomicBool>,
        backend: Arc<dyn TtsBackend>,
        failure: Arc<Mutex<Option<String>>>,
        llm_busy: Arc<AtomicBool>,
        captions: bool,
    ) {
        let Some(hud) = self.hud() else { return };
        let hud_kind = kind.hud_kind();
        let source_of = |from_clipboard: &AtomicBool| {
            if from_clipboard.load(Ordering::SeqCst) {
                ReadSource::Clipboard
            } else {
                ReadSource::Selection
            }
            .hud_source()
        };
        let session = self.inner.session.clone();
        let yielded = self.inner.hud_yielded.clone();
        // A leftover yield from a previous handoff must not suppress hide on
        // this read — that flag is only meaningful for the session that set it.
        yielded.store(false, Ordering::SeqCst);

        hud.cancel_hide();
        // A read without captions has nothing to display but its own
        // existence, so it gets the compact card rather than the layout sized
        // for text; with captions the sentence being spoken needs the room.
        hud.show(if captions {
            HudPresentation::Caption
        } else {
            HudPresentation::Batch
        });
        hud.emit_error(None);
        hud.emit_caption(None);
        hud.emit_reading(
            hud_kind,
            ReadingSource::Selection,
            Some(ReadingPhase::Preparing),
        );

        thread::Builder::new()
            .name("voicex-tts-hud".to_string())
            .spawn(move || {
                let mut shown = ReadingPhase::Preparing;
                let mut shown_source = ReadingSource::Selection;
                let mut caption: Option<SpeechProgress> = None;
                while session.is_active() {
                    let phase = match backend.status() {
                        TtsStatus::Speaking => ReadingPhase::Speaking,
                        // The engine is idle both while the selection is
                        // read and while the LLM works; only the read worker
                        // knows which, and it says so through the flag.
                        TtsStatus::Idle if llm_busy.load(Ordering::SeqCst) => {
                            ReadingPhase::Translating
                        }
                        TtsStatus::Idle => ReadingPhase::Preparing,
                    };
                    // The source changes at most once, when the selection
                    // turns out to be unreadable and the clipboard is read.
                    let source = source_of(&from_clipboard);
                    if phase != shown || source != shown_source {
                        shown = phase;
                        shown_source = source;
                        hud.emit_reading(hud_kind, source, Some(phase));
                    }
                    // Only backends that render audio themselves have a level;
                    // the system voice reports none and the HUD then shows just
                    // the animated icon instead of a waveform standing still.
                    if let Some(level) = backend.audio_level() {
                        hud.emit_audio_level(level);
                    }
                    // The same poll carries the caption: the backend says
                    // which piece is being heard, and only a change is worth
                    // an event.
                    if captions {
                        if let Some(next) = next_caption(caption.as_ref(), backend.progress()) {
                            log_event(
                                "caption",
                                &[
                                    ("index", next.index.to_string()),
                                    ("total", next.total.to_string()),
                                ],
                            );
                            hud.emit_caption(Some(&next));
                            caption = Some(next);
                        }
                    }
                    thread::sleep(HUD_POLL);
                }

                hud.emit_reading(hud_kind, source_of(&from_clipboard), None);
                // Dictation already owns the window. Hiding here would take the
                // recording HUD down a moment after it appeared, and the next
                // dictation tap would stop a session the user thought never
                // started.
                if yielded.swap(false, Ordering::SeqCst) {
                    hud.emit_caption(None);
                    return;
                }
                let reported = failure.lock().ok().and_then(|slot| slot.clone());
                let hud_for_hide = hud.clone();
                match reported {
                    // Without this a failed read is completely silent: the user
                    // pressed the hotkey and nothing whatsoever happened.
                    Some(code) => {
                        // The error takes the caption's place for its linger.
                        hud.emit_caption(None);
                        hud.emit_error(Some(&code));
                        hud.schedule_hide(HUD_ERROR_LINGER_MS, move || {
                            hud_for_hide.emit_error(None);
                            hud_for_hide.hide();
                        });
                    }
                    // The last sentence stays up for the linger: the caption
                    // layout draws nothing but its text, so clearing it first
                    // left an empty frame on screen until the hide.
                    None if lingers_after_read(captions, caption.as_ref()) => {
                        hud.schedule_hide(HUD_LINGER_MS, move || {
                            hud_for_hide.hide();
                            hud_for_hide.emit_caption(None);
                        })
                    }
                    None => hud.hide(),
                }
            })
            .expect("failed to spawn the TTS HUD driver");
    }

    /// Lock-free "is a read or speech in progress" view for the keyboard hook,
    /// which must not take locks inside the event tap callback.
    pub fn active_handle(&self) -> Arc<AtomicU64> {
        self.inner.session.active_handle()
    }

    fn app(&self) -> Option<AppHandle> {
        self.inner.app.lock().ok().and_then(|slot| slot.clone())
    }

    /// Resolve a backend by provider name, applying its credentials.
    ///
    /// `provider` is passed in rather than read from `settings` because the
    /// settings page asks about a provider the user just picked, which the
    /// debounced save has not written yet. Reading it from the store there
    /// would answer about the *previous* provider.
    ///
    /// Falls back to the system voice when the cloud provider is selected but
    /// unavailable, rather than refusing to speak — but says so in the log, so
    /// "why does it sound like the local voice" has an answer.
    fn backend_for(
        &self,
        provider: &str,
        settings: Option<&AppSettings>,
    ) -> Option<Arc<dyn TtsBackend>> {
        match provider {
            PROVIDER_VOLCENGINE => {
                let cloud = self
                    .inner
                    .volcengine
                    .lock()
                    .ok()
                    .and_then(|slot| slot.clone());
                if let (Some(cloud), Some(settings)) = (cloud, settings) {
                    cloud.apply_config(VolcengineConfig {
                        api_key: settings.volc_tts_api_key.clone(),
                        resource_id: settings.volc_tts_resource_id.clone(),
                    });
                    return Some(cloud as Arc<dyn TtsBackend>);
                }
                log_event("backend_fallback", &[("provider", provider.to_string())]);
            }
            PROVIDER_ALIYUN => {
                let cloud = self.inner.aliyun.lock().ok().and_then(|slot| slot.clone());
                if let (Some(cloud), Some(settings)) = (cloud, settings) {
                    // The model has to be applied here and not only at speak
                    // time: it decides which voice list the settings page shows
                    // and which text limit the truncation uses.
                    cloud.apply_config(AliyunConfig {
                        api_key: settings.aliyun_tts_api_key.clone(),
                        model: settings.aliyun_tts_model.clone(),
                    });
                    return Some(cloud as Arc<dyn TtsBackend>);
                }
                log_event("backend_fallback", &[("provider", provider.to_string())]);
            }
            PROVIDER_MIMO => {
                let cloud = self.inner.mimo.lock().ok().and_then(|slot| slot.clone());
                if let (Some(cloud), Some(settings)) = (cloud, settings) {
                    cloud.apply_config(MimoConfig {
                        api_key: settings.mimo_tts_api_key.clone(),
                        instruction: settings.mimo_tts_instruction.clone(),
                    });
                    return Some(cloud as Arc<dyn TtsBackend>);
                }
                log_event("backend_fallback", &[("provider", provider.to_string())]);
            }
            PROVIDER_AZURE => {
                let cloud = self.inner.azure.lock().ok().and_then(|slot| slot.clone());
                if let (Some(cloud), Some(settings)) = (cloud, settings) {
                    cloud.apply_config(AzureConfig {
                        api_key: settings.azure_tts_api_key.clone(),
                        region: settings.azure_tts_region.clone(),
                    });
                    return Some(cloud as Arc<dyn TtsBackend>);
                }
                log_event("backend_fallback", &[("provider", provider.to_string())]);
            }
            _ => {}
        }

        self.system_backend(settings)
    }

    /// Local speak path: empty voice id uses `say` (Siri / Spoken Content);
    /// a listed id uses AVSpeech. Listing always goes through AVSpeech, so
    /// this is only for actually speaking.
    fn system_backend(&self, settings: Option<&AppSettings>) -> Option<Arc<dyn TtsBackend>> {
        // No settings at all is the same as an unset voice, which is `say`.
        let use_say = settings.map(uses_say_voice).unwrap_or(true);
        if use_say {
            if let Some(say) = self.inner.say.lock().ok().and_then(|slot| slot.clone()) {
                return Some(say);
            }
        }
        self.inner.system.lock().ok().and_then(|slot| slot.clone())
    }

    /// The backend that owns the session right now, for stopping it.
    fn active_backend(&self) -> Option<Arc<dyn TtsBackend>> {
        self.inner.active.lock().ok().and_then(|slot| slot.clone())
    }

    fn set_active_backend(&self, backend: Option<Arc<dyn TtsBackend>>) {
        if let Ok(mut slot) = self.inner.active.lock() {
            *slot = backend;
        }
    }

    fn is_recording(&self) -> bool {
        self.inner
            .recording
            .lock()
            .ok()
            .and_then(|slot| slot.as_ref().map(|flag| flag.load(Ordering::SeqCst)))
            .unwrap_or(false)
    }

    pub fn is_active(&self) -> bool {
        self.inner.session.is_active()
    }

    /// Voices offered by `provider`, or by the configured one when `None`.
    ///
    /// `model` overrides the stored one for providers that have several, for the
    /// same reason `provider` is passed in: the settings page asks about a
    /// choice the debounced save has not written yet, so reading it back from
    /// the store would list the previous model's voices.
    ///
    /// Blocking, and hops to the main thread — never call from there.
    pub fn list_voices(
        &self,
        provider: Option<&str>,
        model: Option<&str>,
    ) -> Result<TtsVoiceList, TtsError> {
        let mut settings = load_settings();
        if let (Some(settings), Some(model)) = (settings.as_mut(), model) {
            if !model.trim().is_empty() {
                settings.aliyun_tts_model = model.to_string();
            }
        }
        let provider = provider
            .or_else(|| settings.as_ref().map(|s| s.tts_provider_type.as_str()))
            .unwrap_or_default();
        // Compact ids come from AVSpeech even when the default speak path is
        // `say`. The picker needs those ids; the empty entry is added in the
        // UI and is not a listed voice.
        let backend = if !is_cloud_provider(provider) {
            self.inner.system.lock().ok().and_then(|slot| slot.clone())
        } else {
            self.backend_for(provider, settings.as_ref())
        }
        .ok_or(TtsError::Unsupported)?;
        Ok(TtsVoiceList {
            voices: backend.list_voices()?,
            custom_voice_only: backend.custom_voice_only(),
        })
    }

    /// Speak a fixed sample with the current voice settings.
    ///
    /// Deliberately skips the selection reader: the settings page needs to
    /// audition rate, pitch and voice without a foreground app or a selection.
    /// It shares the session slot with reading, so the read hotkey stops a
    /// preview and a second click supersedes the first.
    ///
    /// Allowed even when the master switch is off — auditioning a voice before
    /// turning the feature on is the point of the button.
    ///
    /// Blocking, and hops to the main thread — never call from there.
    pub fn speak_preview(&self, text: String) -> Result<(), TtsError> {
        if self.is_recording() {
            return Err(TtsError::Backend("dictation is recording".to_string()));
        }
        let settings = load_settings();
        let provider = settings
            .as_ref()
            .map(|s| s.tts_provider_type.clone())
            .unwrap_or_default();
        let backend = self
            .backend_for(&provider, settings.as_ref())
            .ok_or(TtsError::Unsupported)?;

        let text: String = text.chars().take(PREVIEW_MAX_CHARS).collect();
        if text.trim().is_empty() {
            return Err(TtsError::Backend("nothing to preview".to_string()));
        }

        let token = self.inner.session.claim();
        self.set_active_backend(Some(backend.clone()));
        log_event(
            "speak_start",
            &[
                ("backend", backend.name().to_string()),
                ("chars", text.chars().count().to_string()),
                ("origin", "preview".to_string()),
            ],
        );

        let request = match settings {
            Some(settings) => voice_request(&settings, text),
            None => TtsRequest::plain(text),
        };
        // Tell the settings page when it ends, so its button can be one toggle
        // rather than a separate Preview and Stop. Before the delegate existed
        // there was no end event to report, which is why it was two buttons.
        self.spawn_preview_watcher();

        backend.start(request, token).inspect_err(|err| {
            log_event(
                "speak_err",
                &[
                    ("error", err.code().to_string()),
                    ("detail", err.to_string()),
                ],
            );
        })
    }

    /// Emit `tts:preview_ended` once the session goes idle.
    ///
    /// The preview deliberately has no HUD — the user is looking at the settings
    /// page — so this is the only signal the page can act on.
    fn spawn_preview_watcher(&self) {
        let Some(app) = self.app() else { return };
        let session = self.inner.session.clone();

        thread::Builder::new()
            .name("voicex-tts-preview".to_string())
            .spawn(move || {
                while session.is_active() {
                    thread::sleep(HUD_POLL);
                }
                let _ = app.emit("tts:preview_ended", ());
            })
            .expect("failed to spawn the preview watcher");
    }

    /// The read-selection hotkey fired.
    ///
    /// Single-click semantics (plan §3.3): idle reads and speaks, anything else
    /// stops. Returns immediately — the read runs on a worker thread because
    /// the clipboard fallback can block for hundreds of milliseconds and this
    /// is called from the hotkey hook's worker.
    pub fn handle_read_selection_hotkey(&self) {
        self.handle_hotkey(ReadKind::Read);
    }

    pub fn handle_translate_selection_hotkey(&self) {
        self.handle_hotkey(ReadKind::Translate);
    }

    /// Either reading hotkey while a session is active stops it, whichever
    /// kind started it: the user wants silence, not a second read queued
    /// behind the first.
    fn handle_hotkey(&self, kind: ReadKind) {
        let active = self.is_active();
        // Dictation keeps the microphone. A new read would be inaudible and
        // would get transcribed, so the key is swallowed. An already-running
        // read is the stuck case takeover missed — stopping it is still right.
        if self.is_recording() && !active {
            log_event(
                "hotkey_action",
                &[
                    ("action", kind.action().to_string()),
                    ("state", "recording".to_string()),
                ],
            );
            return;
        }

        log_event(
            "hotkey_action",
            &[
                ("action", kind.action().to_string()),
                ("state", if active { "active" } else { "idle" }.to_string()),
            ],
        );

        if active {
            self.stop(StopReason::Hotkey);
        } else {
            self.start(kind);
        }
    }

    /// Stop reading because dictation is taking over. No-op when idle, so the
    /// dictation hotkey stays cheap. The HUD handoff is `stop`'s job, next to
    /// the session release it has to precede.
    ///
    /// Must return without waiting for the engine to go silent: this runs on
    /// the same worker that then starts dictation, and a blocking stop delays
    /// `HotkeyPressed` by however long the backend needs — up to two seconds
    /// for the system voice's main-thread hop.
    pub fn stop_for_dictation(&self) {
        if self.is_active() {
            self.stop(StopReason::Dictation);
        }
    }

    pub fn stop(&self, reason: StopReason) {
        // Dictation is about to show the same HUD window, so the reading driver
        // must not hide it on the way out. This has to happen before the
        // release below — the driver leaves its loop the moment the slot goes
        // idle — which is why it lives here rather than in the caller: the two
        // lines sit together and cannot be reordered from outside. Only
        // dictation reuses the window; every other reason still wants the
        // driver's linger-and-hide.
        if reason == StopReason::Dictation {
            self.inner.hud_yielded.store(true, Ordering::SeqCst);
        }

        // Release the session first: this cancels every outstanding token, so
        // an in-flight read discards its result and a queued main-thread speak
        // aborts instead of starting after the user asked to stop.
        self.inner.session.release();

        log_event("speak_stop", &[("reason", reason.as_str().to_string())]);

        let Some(backend) = self.active_backend() else {
            return;
        };
        match backend.stop() {
            // Backends must not block here: this is called from the hotkey
            // worker, which still has dictation's `Pressed` to deliver. So
            // this marks the stop being *accepted*, one step past `speak_stop`
            // recording that it was asked for, and short of the audio actually
            // ending — the backends report that separately as `speak_cancelled`
            // once the engine confirms it. Anything asserting that speech
            // stopped wants that event, not this one.
            Ok(()) => log_event("speak_stopped", &[("reason", reason.as_str().to_string())]),
            Err(err) => log_event(
                "speak_err",
                &[
                    ("error", err.code().to_string()),
                    ("detail", err.to_string()),
                ],
            ),
        }
    }

    fn start(&self, kind: ReadKind) {
        if self.is_recording() {
            // Dictation wins: reading aloud during recording would feed the
            // speech straight back into the microphone.
            log_event("speak_err", &[("error", "recording_active".to_string())]);
            return;
        }

        let Some(app) = self.app() else {
            log_event("speak_err", &[("error", "not_initialized".to_string())]);
            return;
        };

        // Settings decide which backend speaks, so they have to be read before
        // the session is claimed. This runs on the hotkey hook's worker, and a
        // database read is cheap enough not to matter there.
        let mut settings = load_settings();
        if kind == ReadKind::Translate {
            // Applied before the backend is picked: for the system provider
            // the voice id decides between `say` and AVSpeech.
            if let Some(voice) = settings.as_mut().and_then(apply_translate_voice_override) {
                log_event("translate_voice", &[("voice", voice)]);
            }
        }
        let settings = settings;
        // Read before `settings` is moved into the worker: it decides whether
        // the account-wide reading counters are ours to bump or the server's.
        let sync_owns_totals = settings.as_ref().is_some_and(sync_owns_counters);
        let provider = settings
            .as_ref()
            .map(|s| s.tts_provider_type.clone())
            .unwrap_or_default();
        let Some(backend) = self.backend_for(&provider, settings.as_ref()) else {
            log_event("speak_err", &[("error", "unsupported".to_string())]);
            return;
        };

        // Captions follow the piece being spoken, so they need sentence-sized
        // pieces and a backend that can say which piece it is on; `say` has
        // no such signal and keeps the compact HUD. Decided here, before the
        // HUD is shown, because the layout is fixed for the session.
        let piece_limit =
            caption_piece_limit(settings.as_ref().is_some_and(|s| s.tts_captions_enabled));
        let captions = piece_limit.is_some() && backend.reports_progress();

        let token = self.inner.session.claim();
        // The read outlives `backend.start()`, which returns as soon as the
        // engine accepts the request. This clone is how the worker learns the
        // read is over: the token stops owning the session when the backend
        // hands it back, or when a stop or a newer read takes it away.
        let stats_token = token.clone();
        self.set_active_backend(Some(backend.clone()));
        let failure: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let llm_busy = Arc::new(AtomicBool::new(false));
        let from_clipboard = Arc::new(AtomicBool::new(false));
        self.spawn_hud_driver(
            kind,
            from_clipboard.clone(),
            backend.clone(),
            failure.clone(),
            llm_busy.clone(),
            captions,
        );
        let app_for_history = app.clone();
        let app_for_stats = app.clone();

        thread::Builder::new()
            .name("voicex-tts-read".to_string())
            .spawn(move || {
                log_event("selection_start", &[("kind", kind.action().to_string())]);
                let clipboard_when_no_selection = settings
                    .as_ref()
                    .is_some_and(|s| s.tts_clipboard_when_no_selection);
                let result = selection::read_selection(SelectionRequest {
                    app,
                    // Compatibility mode, off by choice in the reading settings.
                    // The fail-closed clipboard rules apply either way.
                    allow_clipboard_fallback: settings
                        .as_ref()
                        .map(|s| s.tts_clipboard_fallback)
                        .unwrap_or(true),
                    // Lets the read's slow waits (modifier release, copy
                    // landing) abort as soon as a stop or a superseding read
                    // cancels this session, instead of holding it for seconds.
                    cancelled: Some(std::sync::Arc::new({
                        let token = token.clone();
                        move || token.is_cancelled()
                    })),
                });

                // The session stays claimed across the handoff to the backend,
                // so there is no window where a hotkey press sees "idle" and
                // starts a second read instead of stopping this one.
                if token.is_cancelled() {
                    log_event(
                        "selection_discarded",
                        &[("reason", "superseded".to_string())],
                    );
                    return;
                }

                let acquired = match result {
                    Ok(outcome) => {
                        log_selection_ok(&outcome);
                        Ok((outcome.text, ReadSource::Selection))
                    }
                    Err(err) => {
                        log_selection_error(&err);
                        if clipboard_when_no_selection && falls_back_to_clipboard(&err) {
                            read_clipboard_instead(&err)
                                .map(|text| (text, ReadSource::Clipboard))
                        } else {
                            Err(err.code().to_string())
                        }
                    }
                };

                match acquired {
                    Ok((original, source)) => {
                        if source == ReadSource::Clipboard {
                            from_clipboard.store(true, Ordering::SeqCst);
                        }
                        let staged = match stage_text(
                            kind,
                            settings.as_ref(),
                            original.clone(),
                            &token,
                            &llm_busy,
                        ) {
                            Ok(staged) => staged,
                            Err(LlmStageError::Cancelled) => {
                                // Esc or a hotkey during the LLM wait. The
                                // session is already released; the reply,
                                // if it ever arrives, is nobody's.
                                log_event(
                                    "selection_discarded",
                                    &[("reason", "cancelled_during_llm".to_string())],
                                );
                                return;
                            }
                            Err(err) => {
                                // Nothing to speak. The HUD shows the code
                                // for the same reason a selection failure
                                // does: a hotkey that does nothing at all is
                                // the worst outcome.
                                if let Ok(mut slot) = failure.lock() {
                                    *slot = Some(err.code().to_string());
                                }
                                token.finish();
                                return;
                            }
                        };
                        if kind == ReadKind::Translate {
                            if let Some(settings) = settings.as_ref() {
                                retain_translation(
                                    settings,
                                    &original,
                                    &staged.text,
                                    &app_for_history,
                                );
                            }
                        }
                        let chars = staged.text.chars().count() as i64;
                        log_event(
                            "speak_start",
                            &[
                                ("kind", kind.action().to_string()),
                                ("source", source.as_str().to_string()),
                                ("backend", backend.name().to_string()),
                                ("chars", chars.to_string()),
                                ("llm", staged.llm_invoked.to_string()),
                            ],
                        );
                        // No length gate on the plain path: a cloud backend
                        // splits an oversized selection into per-request
                        // pieces itself, so the whole selection is read rather
                        // than a truncated prefix of it. (Translate has its
                        // own cap, applied before the LLM call.)
                        let mut request = match settings {
                            Some(settings) => voice_request(&settings, staged.text),
                            None => TtsRequest::plain(staged.text),
                        };
                        request.piece_limit = piece_limit;
                        match backend.start(request, token) {
                            // Counted only once the engine has taken the
                            // request: a start that was refused read nothing.
                            Ok(()) => record_read(
                                kind,
                                chars,
                                staged.llm_invoked,
                                sync_owns_totals,
                                &stats_token,
                                &app_for_stats,
                            ),
                            Err(err) => {
                                log_event(
                                    "speak_err",
                                    &[
                                        ("error", err.code().to_string()),
                                        ("detail", err.to_string()),
                                    ],
                                );
                                // The backend has already handed the session
                                // back (its `start` contract), so this lands a
                                // hair after the driver could see idle; the
                                // driver's own emits on the way out cover that
                                // gap. Without it a refused start — no API key,
                                // no voice id for a designed-voice model — is a
                                // HUD that flashes "preparing" and vanishes.
                                if let Ok(mut slot) = failure.lock() {
                                    *slot = Some(err.code().to_string());
                                }
                            }
                        }
                    }
                    Err(code) => {
                        // Record before releasing the session: the HUD driver
                        // reads this the moment it sees the session go idle.
                        if let Ok(mut slot) = failure.lock() {
                            *slot = Some(code);
                        }
                        // Nothing will speak, so hand the session back — but
                        // only if a newer request has not already claimed it.
                        token.finish();
                    }
                }
            })
            .expect("failed to spawn the TTS read worker");
    }
}

/// Run the LLM stage the kind calls for, or pass the text through.
///
/// Translate needs the stage to succeed: there is nothing sensible to say in
/// the wrong language. Preprocessing is best-effort by design (requirements
/// doc §4.3): its failure is logged by the stage and the raw text is read,
/// because "read this" still has an answer without the cleanup.
fn stage_text(
    kind: ReadKind,
    settings: Option<&AppSettings>,
    text: String,
    token: &super::CancelToken,
    llm_busy: &AtomicBool,
) -> Result<StagedText, LlmStageError> {
    match kind {
        ReadKind::Translate => {
            let chars = text.chars().count();
            if chars > TRANSLATE_MAX_CHARS {
                let err = LlmStageError::TooLong { chars };
                log_event(
                    "llm_stage_err",
                    &[
                        ("stage", "translate".to_string()),
                        ("error", err.code().to_string()),
                        ("detail", err.to_string()),
                    ],
                );
                return Err(err);
            }
            let Some(settings) = settings else {
                log_event(
                    "speak_err",
                    &[("error", "settings_unavailable".to_string())],
                );
                return Err(LlmStageError::NotConfigured);
            };
            let prompt = llm_stage::fill_translate_prompt(
                &settings.tts_translate_prompt_template,
                &settings.tts_translate_source_language,
                &settings.tts_translate_target_language,
            );
            let config = build_llm_config_for_key(settings, &settings.tts_llm_provider_key);
            run_stage("translate", config, &prompt, &text, token, llm_busy).map(|translated| {
                StagedText {
                    text: translated,
                    llm_invoked: true,
                }
            })
        }
        ReadKind::Read => {
            let Some(settings) = settings.filter(|s| s.tts_preprocess_enabled) else {
                return Ok(StagedText {
                    text,
                    llm_invoked: false,
                });
            };
            let config = build_llm_config_for_key(settings, &settings.tts_llm_provider_key);
            match run_stage(
                "preprocess",
                config,
                &settings.tts_preprocess_prompt_template,
                &text,
                token,
                llm_busy,
            ) {
                Ok(cleaned) => Ok(StagedText {
                    text: cleaned,
                    llm_invoked: true,
                }),
                Err(LlmStageError::Cancelled) => Err(LlmStageError::Cancelled),
                Err(_) => Ok(StagedText {
                    text,
                    llm_invoked: false,
                }),
            }
        }
    }
}

fn run_stage(
    stage: &str,
    config: crate::llm::LLMConfig,
    prompt: &str,
    text: &str,
    token: &super::CancelToken,
    llm_busy: &AtomicBool,
) -> Result<String, LlmStageError> {
    llm_busy.store(true, Ordering::SeqCst);
    let started = std::time::Instant::now();
    let result = llm_stage::run_llm_stage(config, prompt, text, token);
    llm_busy.store(false, Ordering::SeqCst);
    llm_stage::log_stage_result(
        stage,
        text.chars().count(),
        started.elapsed().as_millis(),
        &result,
    );
    result
}

/// Key of the translate voice override for the current provider. Aliyun's
/// model families reject each other's voice ids, so the model is part of the
/// key there; `ReadingSettings.vue` builds the same string.
pub fn translate_voice_key(settings: &AppSettings) -> String {
    if settings.tts_provider_type == PROVIDER_ALIYUN {
        format!("{}:{}", PROVIDER_ALIYUN, settings.aliyun_tts_model)
    } else {
        settings.tts_provider_type.clone()
    }
}

/// Write the translate voice, if one is set, onto the provider's own voice
/// field, so backend selection and `voice_request` see it without either
/// learning about overrides. Returns the voice that was applied. An empty or
/// missing override means "same voice as reading" and changes nothing.
fn apply_translate_voice_override(settings: &mut AppSettings) -> Option<String> {
    let key = translate_voice_key(settings);
    let voice = settings
        .tts_translate_voice_overrides
        .get(&key)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())?;
    let slot = match settings.tts_provider_type.as_str() {
        PROVIDER_VOLCENGINE => &mut settings.volc_tts_speaker,
        PROVIDER_ALIYUN => {
            if settings.aliyun_tts_model == aliyun::MODEL_QWEN_AUDIO {
                &mut settings.aliyun_tts_voice_qwen_audio
            } else if settings.aliyun_tts_model == aliyun::MODEL_QWEN_AUDIO_31 {
                &mut settings.aliyun_tts_voice_qwen_audio31
            } else if settings.aliyun_tts_model == aliyun::MODEL_COSYVOICE_V35 {
                &mut settings.aliyun_tts_voice_cosy_voice_v35
            } else if settings.aliyun_tts_model == aliyun::MODEL_COSYVOICE {
                &mut settings.aliyun_tts_voice_cosy_voice
            } else {
                &mut settings.aliyun_tts_voice_qwen3
            }
        }
        PROVIDER_MIMO => &mut settings.mimo_tts_voice,
        PROVIDER_AZURE => &mut settings.azure_tts_voice,
        _ => &mut settings.system_tts_voice_id,
    };
    *slot = voice.clone();
    Some(voice)
}

/// Keep the translation where the settings say to: the clipboard, history,
/// both or neither. Runs before speech starts so the text is there even if
/// the user stops the read a second in.
fn retain_translation(settings: &AppSettings, source: &str, translated: &str, app: &AppHandle) {
    if settings.tts_translate_copy_to_clipboard {
        match arboard::Clipboard::new().and_then(|mut clipboard| clipboard.set_text(translated)) {
            Ok(()) => log_event(
                "translate_clipboard",
                &[("chars", translated.chars().count().to_string())],
            ),
            Err(err) => log_event("translate_clipboard_err", &[("detail", err.to_string())]),
        }
    }
    if settings.tts_translate_save_history {
        let llm_model_name = HistoryService::resolve_llm_model_name(&settings_for_llm_key(
            settings,
            &settings.tts_llm_provider_key,
        ));
        HistoryService::new().persist(
            translated.to_string(),
            Some(source.to_string()),
            true,
            true,
            HISTORY_MODE_TRANSLATE_READ.to_string(),
            None,
            None,
            None,
            llm_model_name,
            Some(app.clone()),
        );
        log_event("translate_history", &[]);
    }
}

/// Count one read, once it is over.
///
/// Parks the read worker for the length of the read. The thread has nothing
/// else left to do, and the duration is only knowable at the end: `start`
/// returns as soon as the engine accepts the request. The token stops owning
/// the session when the backend finishes, when the user stops the read, and
/// when a newer read takes over — all three end a read that really happened,
/// so all three are counted, for as long as they lasted.
fn record_read(
    kind: ReadKind,
    characters: i64,
    llm_invoked: bool,
    sync_owns_totals: bool,
    token: &super::CancelToken,
    app: &AppHandle,
) {
    let spoken_for = wait_for_read_to_end(token);

    let translate = kind == ReadKind::Translate;
    let delta = TtsCounters {
        read_count: i64::from(!translate),
        translate_count: i64::from(translate),
        characters,
        duration_ms: spoken_for.as_millis() as i64,
        llm_count: i64::from(llm_invoked),
    };
    log_event(
        "speak_end",
        &[
            ("kind", kind.action().to_string()),
            ("chars", characters.to_string()),
            ("ms", delta.duration_ms.to_string()),
        ],
    );

    // With sync configured the server owns this row and pushes the account
    // total back; bumping it here would only be overwritten on the next
    // refresh. Same rule the dictation counters follow.
    if !sync_owns_totals {
        if let Err(err) = crate::storage::increment_account_tts_stats(&delta) {
            log::warn!("Failed to update reading stats: {err}");
        }
    }

    let device_id = match crate::storage::get_or_create_device_id() {
        Ok(id) => id,
        Err(err) => {
            log::warn!("Failed to load device id for reading stats: {err}");
            return;
        }
    };
    match crate::storage::increment_device_tts_stats(&device_id, &delta) {
        // The upload carries this device's absolute totals, so a send that
        // fails costs nothing: the next read sends the same numbers again.
        Ok(totals) => app.state::<SyncService>().upload_tts_usage(totals),
        Err(err) => log::warn!("Failed to update device reading stats: {err}"),
    }
}

/// Block until the session this token owns is handed back, and say how long
/// that took.
fn wait_for_read_to_end(token: &super::CancelToken) -> std::time::Duration {
    let started = std::time::Instant::now();
    while !token.is_cancelled() {
        thread::sleep(HUD_POLL);
    }
    started.elapsed()
}

/// Persisted settings, or `None` when the store cannot be read.
///
/// A failure here must not stop the user from being read to, so callers fall
/// back to engine defaults — loudly, because silently speaking in the wrong
/// voice is the kind of thing that gets reported as "the setting does nothing".
fn load_settings() -> Option<AppSettings> {
    match crate::storage::get_settings() {
        Ok(settings) => Some(settings),
        Err(err) => {
            log::warn!("Falling back to default voice parameters: {err}");
            log_event("settings_err", &[("error", err.to_string())]);
            None
        }
    }
}

/// Whether the local provider speaks through `say` rather than AVSpeech.
///
/// An unset system voice means "whatever Spoken Content is set to", which only
/// `/usr/bin/say` can reach — not AVSpeech's locale compact voice. Two
/// decisions hang off this and have to agree: which backend speaks, and which
/// parameters the request is allowed to carry. Answering it in one place is
/// what keeps them from drifting into a request that names a voice the chosen
/// engine cannot use.
fn uses_say_voice(settings: &AppSettings) -> bool {
    settings.system_tts_voice_id.trim().is_empty()
}

/// Build a request from the selected provider's own settings.
///
/// Every synthesis parameter is per provider, not just the voice id. Engines
/// differ in baseline speed and loudness, so one shared value would force a
/// compromise between them — and keeping them separate means adding a provider
/// never reopens the question of which settings are shared.
///
/// Rate and volume are stored normalized (0..=1) on both sides and each backend
/// maps them onto its own scale. Pitch exists only for compact system voices
/// (AVSpeech). Cloud engines and the empty-id `say` path carry `None` rather
/// than a value that would be dropped.
///
/// An empty voice id means "backend default", which is not the same as a voice
/// literally named "". For the system provider that default is `say` without
/// `-v`, not AVSpeech's locale compact voice.
fn voice_request(settings: &AppSettings, text: String) -> TtsRequest {
    let (voice, rate, volume, pitch) = match settings.tts_provider_type.as_str() {
        PROVIDER_VOLCENGINE => (
            settings.volc_tts_speaker.clone(),
            Some(settings.volc_tts_rate),
            Some(settings.volc_tts_volume),
            None,
        ),
        PROVIDER_ALIYUN => (
            aliyun_voice(settings),
            Some(settings.aliyun_tts_rate),
            Some(settings.aliyun_tts_volume),
            None,
        ),
        // MiMo has no speed parameter at all — the only delivery control is the
        // instruction text, which travels in the backend's config. Volume is
        // local playback gain, so that one is real.
        PROVIDER_MIMO => (
            settings.mimo_tts_voice.clone(),
            None,
            Some(settings.mimo_tts_volume),
            None,
        ),
        PROVIDER_AZURE => (
            settings.azure_tts_voice.clone(),
            Some(settings.azure_tts_rate),
            Some(settings.azure_tts_volume),
            None,
        ),
        _ => {
            // `say` has no flag for either, so sending them would be sending a
            // value the engine drops on the floor.
            let say = uses_say_voice(settings);
            (
                settings.system_tts_voice_id.clone(),
                Some(settings.system_tts_rate),
                (!say).then_some(settings.system_tts_volume),
                (!say).then_some(settings.system_tts_pitch),
            )
        }
    };

    TtsRequest {
        text,
        voice: Some(voice).filter(|id| !id.trim().is_empty()),
        rate,
        volume,
        pitch,
        piece_limit: None,
    }
}

/// The Aliyun voice for whichever model is selected.
///
/// The families reject each other's voice ids outright, so they get a setting
/// each and switching model must not carry the old id over — one shared key
/// would make every model switch produce a guaranteed 400.
fn aliyun_voice(settings: &AppSettings) -> String {
    if settings.aliyun_tts_model == aliyun::MODEL_QWEN_AUDIO {
        settings.aliyun_tts_voice_qwen_audio.clone()
    } else if settings.aliyun_tts_model == aliyun::MODEL_QWEN_AUDIO_31 {
        settings.aliyun_tts_voice_qwen_audio31.clone()
    } else if settings.aliyun_tts_model == aliyun::MODEL_COSYVOICE_V35 {
        settings.aliyun_tts_voice_cosy_voice_v35.clone()
    } else if settings.aliyun_tts_model == aliyun::MODEL_COSYVOICE {
        settings.aliyun_tts_voice_cosy_voice.clone()
    } else {
        settings.aliyun_tts_voice_qwen3.clone()
    }
}

/// Selection failures after which the clipboard is read instead, when the
/// setting allows it. Each is "there is no selection we can read here":
/// nothing selected, a control that does not expose its selection, a copy
/// that changed nothing. The rest are left alone on purpose — a secure input
/// field is where a password is about to be pasted, a missing permission or
/// VoiceX's own focus is something the user has to fix, and a refused
/// clipboard snapshot or a changed foreground app may still have a selection
/// the user meant.
fn falls_back_to_clipboard(err: &SelectionError) -> bool {
    matches!(
        err,
        SelectionError::NoSelection
            | SelectionError::UnsupportedControl
            | SelectionError::CopyTimeout
    )
}

/// Read the clipboard in place of a selection that could not be read. The
/// error codes are the clipboard's own; the HUD words them as "no readable
/// selection, and the clipboard …", since this is the only way it is read.
fn read_clipboard_instead(err: &SelectionError) -> Result<String, String> {
    log_event("clipboard_fallback", &[("after", err.code().to_string())]);
    clipboard_text::read().map_err(|err| err.code().to_string())
}

fn log_selection_ok(outcome: &SelectionOutcome) {
    let mut fields = vec![
        ("source", outcome.source.as_str().to_string()),
        ("chars", outcome.text.chars().count().to_string()),
        ("elapsed_ms", outcome.elapsed_ms.to_string()),
        (
            "app",
            outcome
                .app_bundle_id
                .clone()
                .or_else(|| outcome.app_name.clone())
                .unwrap_or_default(),
        ),
    ];
    if let Some(restored) = outcome.clipboard_restored {
        fields.push(("clipboard_restored", restored.to_string()));
    }
    log_event("selection_ok", &fields);
}

fn log_selection_error(err: &SelectionError) {
    let mut fields = vec![("error", err.code().to_string())];
    if let SelectionError::ClipboardSnapshotRefused(reason) = err {
        fields.push(("detail", reason.clone()));
    }
    log_event("selection_err", &fields);
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::thread;

    use super::super::SessionSlot;
    use super::{voice_request, wait_for_read_to_end, StopReason, TtsController, HUD_POLL};
    use crate::commands::settings::AppSettings;

    #[test]
    fn an_unset_voice_means_engine_default_not_a_voice_named_empty() {
        let mut settings = AppSettings::default();
        assert!(settings.system_tts_voice_id.is_empty());
        assert_eq!(voice_request(&settings, "hi".to_string()).voice, None);

        settings.system_tts_voice_id = "com.apple.voice.compact.zh-CN.Tingting".to_string();
        assert_eq!(
            voice_request(&settings, "hi".to_string()).voice.as_deref(),
            Some("com.apple.voice.compact.zh-CN.Tingting")
        );
    }

    #[test]
    fn voice_parameters_reach_the_request_on_their_stored_scales() {
        // Empty system voice is the `say` path: rate still applies, volume and
        // pitch do not exist on that engine so they stay `None`.
        let settings = AppSettings::default();
        let request = voice_request(&settings, "hi".to_string());
        assert_eq!(request.rate, Some(0.5), "0.5 is the engine's 1x mark");
        assert_eq!(request.volume, None);
        assert_eq!(request.pitch, None);
    }

    #[test]
    fn a_listed_system_voice_keeps_pitch_and_volume() {
        // Compact AVSpeech voices honour both; sending them only then means a
        // value is never silently dropped on the `say` path.
        let mut settings = AppSettings::default();
        settings.system_tts_voice_id = "com.apple.voice.compact.zh-CN.Tingting".to_string();
        settings.system_tts_volume = 0.4;
        settings.system_tts_pitch = 1.2;
        let request = voice_request(&settings, "hi".to_string());
        assert_eq!(request.volume, Some(0.4));
        assert_eq!(request.pitch, Some(1.2));
    }

    #[test]
    fn each_provider_speaks_from_its_own_settings() {
        // The whole point of splitting them: tuning one engine must not move
        // the other. A shared value would force a compromise between engines
        // whose baseline speed and loudness differ.
        let mut settings = AppSettings::default();
        settings.system_tts_voice_id = "com.apple.voice.compact.zh-CN.Tingting".to_string();
        settings.system_tts_rate = 0.9;
        settings.system_tts_volume = 0.4;
        settings.volc_tts_speaker = "zh_male_liufei_uranus_bigtts".to_string();
        settings.volc_tts_rate = 0.3;
        settings.volc_tts_volume = 1.0;

        settings.tts_provider_type = "system".to_string();
        let local = voice_request(&settings, "hi".to_string());
        assert_eq!(
            local.voice.as_deref(),
            Some("com.apple.voice.compact.zh-CN.Tingting")
        );
        assert_eq!(local.rate, Some(0.9));
        assert_eq!(local.volume, Some(0.4));

        settings.tts_provider_type = "volcengine".to_string();
        let cloud = voice_request(&settings, "hi".to_string());
        assert_eq!(cloud.voice.as_deref(), Some("zh_male_liufei_uranus_bigtts"));
        assert_eq!(cloud.rate, Some(0.3));
        assert_eq!(cloud.volume, Some(1.0));
    }

    #[test]
    fn the_aliyun_voice_follows_the_selected_model() {
        // The families reject each other's voice ids, so switching model
        // has to switch voice with it. Carrying one over is not a wrong-sounding
        // voice, it is a guaranteed 400 on the next read.
        use crate::tts::aliyun::{
            MODEL_COSYVOICE, MODEL_COSYVOICE_V35, MODEL_QWEN3, MODEL_QWEN_AUDIO,
            MODEL_QWEN_AUDIO_31,
        };

        let mut settings = AppSettings::default();
        settings.tts_provider_type = "aliyun".to_string();
        settings.aliyun_tts_voice_qwen3 = "Dylan".to_string();
        settings.aliyun_tts_voice_qwen_audio = "longanfengyue".to_string();
        settings.aliyun_tts_voice_qwen_audio31 = "xieshurou_v3.1".to_string();
        settings.aliyun_tts_voice_cosy_voice = "longanyang".to_string();
        settings.aliyun_tts_voice_cosy_voice_v35 = "cosyvoice-v3.5-flash-vd-test".to_string();

        settings.aliyun_tts_model = MODEL_QWEN3.to_string();
        assert_eq!(
            voice_request(&settings, "hi".to_string()).voice.as_deref(),
            Some("Dylan")
        );

        settings.aliyun_tts_model = MODEL_QWEN_AUDIO.to_string();
        assert_eq!(
            voice_request(&settings, "hi".to_string()).voice.as_deref(),
            Some("longanfengyue")
        );

        settings.aliyun_tts_model = MODEL_QWEN_AUDIO_31.to_string();
        assert_eq!(
            voice_request(&settings, "hi".to_string()).voice.as_deref(),
            Some("xieshurou_v3.1")
        );

        settings.aliyun_tts_model = MODEL_COSYVOICE.to_string();
        assert_eq!(
            voice_request(&settings, "hi".to_string()).voice.as_deref(),
            Some("longanyang")
        );

        settings.aliyun_tts_model = MODEL_COSYVOICE_V35.to_string();
        assert_eq!(
            voice_request(&settings, "hi".to_string()).voice.as_deref(),
            Some("cosyvoice-v3.5-flash-vd-test")
        );
    }

    #[test]
    fn a_cloud_request_carries_no_pitch_at_all() {
        // Sending a pitch no cloud provider implements would be a value the
        // backend silently drops; `None` says so in the type instead.
        let mut settings = AppSettings::default();
        settings.tts_provider_type = "volcengine".to_string();
        assert_eq!(voice_request(&settings, "hi".to_string()).pitch, None);

        settings.tts_provider_type = "aliyun".to_string();
        assert_eq!(voice_request(&settings, "hi".to_string()).pitch, None);

        settings.tts_provider_type = "mimo".to_string();
        assert_eq!(voice_request(&settings, "hi".to_string()).pitch, None);

        settings.tts_provider_type = "azure".to_string();
        assert_eq!(voice_request(&settings, "hi".to_string()).pitch, None);
    }

    #[test]
    fn an_azure_request_carries_its_own_voice_rate_and_volume() {
        let mut settings = AppSettings::default();
        settings.tts_provider_type = "azure".to_string();
        settings.azure_tts_voice = "zh-CN-XiaoxiaoNeural".to_string();
        settings.azure_tts_rate = 0.75;
        settings.azure_tts_volume = 0.6;

        let request = voice_request(&settings, "hi".to_string());
        assert_eq!(request.voice.as_deref(), Some("zh-CN-XiaoxiaoNeural"));
        assert_eq!(request.rate, Some(0.75));
        assert_eq!(request.volume, Some(0.6));
    }

    #[test]
    fn a_mimo_request_carries_volume_but_no_rate() {
        // MiMo has no speed parameter; the settings page hides its rate slider
        // and the request says `None` rather than a value the backend would
        // silently ignore. Volume is real — it is local playback gain.
        let mut settings = AppSettings::default();
        settings.tts_provider_type = "mimo".to_string();
        settings.mimo_tts_voice = "冰糖".to_string();
        settings.mimo_tts_volume = 0.7;

        let request = voice_request(&settings, "hi".to_string());
        assert_eq!(request.voice.as_deref(), Some("冰糖"));
        assert_eq!(request.rate, None);
        assert_eq!(request.volume, Some(0.7));
    }

    #[test]
    fn a_new_claim_cancels_the_previous_one() {
        let slot = SessionSlot::default();
        let first = slot.claim();
        assert!(!first.is_cancelled());

        let second = slot.claim();
        assert!(first.is_cancelled(), "superseded request must see cancel");
        assert!(!second.is_cancelled());
    }

    #[test]
    fn release_cancels_and_leaves_the_slot_idle() {
        let slot = SessionSlot::default();
        let token = slot.claim();
        assert!(slot.is_active());

        slot.release();
        assert!(!slot.is_active());
        assert!(token.is_cancelled());
        assert!(!token.finish(), "a cancelled token must not release again");
    }

    #[test]
    fn only_the_owner_can_finish() {
        let slot = SessionSlot::default();
        let stale = slot.claim();
        let current = slot.claim();

        assert!(!stale.finish(), "stale token must not clear a live session");
        assert!(slot.is_active(), "the live session survives a stale finish");

        assert!(current.finish());
        assert!(!slot.is_active());
    }

    #[test]
    fn finishing_twice_releases_once() {
        let slot = SessionSlot::default();
        let token = slot.claim();
        assert!(token.finish());
        assert!(!token.finish());
    }

    #[test]
    fn a_finished_session_is_idle_so_the_hotkey_starts_a_new_read() {
        let slot = SessionSlot::default();
        let token = slot.claim();
        assert!(
            slot.is_active(),
            "hotkey during a read must stop, not start"
        );
        token.finish();
        assert!(!slot.is_active(), "hotkey after completion must start anew");
    }

    #[test]
    fn the_reading_hotkey_is_ignored_while_dictation_is_recording() {
        // Starting a read into a live microphone would be inaudible and would
        // get transcribed. The key is swallowed; nothing else happens.
        let controller = TtsController::default();
        controller.attach_recording_flag(Arc::new(AtomicBool::new(true)));
        assert!(!controller.is_active());

        controller.handle_read_selection_hotkey();

        assert!(!controller.is_active(), "recording must not start a read");
        assert!(
            !controller.inner.hud_yielded.load(Ordering::SeqCst),
            "a no-op must not mark the HUD as handed off"
        );
    }

    #[test]
    fn stopping_for_dictation_marks_the_hud_as_handed_off() {
        // The HUD driver exits as soon as the session goes idle, so the flag
        // has to be set before the release. That ordering is not observable
        // from out here; what this pins is that the flag is `stop`'s own doing,
        // which is what keeps the two lines adjacent and unreorderable.
        let controller = TtsController::default();
        controller.inner.session.claim();

        controller.stop(StopReason::Dictation);

        assert!(!controller.is_active());
        assert!(
            controller.inner.hud_yielded.load(Ordering::SeqCst),
            "the HUD driver must see the handoff when it leaves its loop"
        );
    }

    #[test]
    fn stopping_for_any_other_reason_leaves_the_hud_to_hide_itself() {
        // Only dictation reuses the window. Suppressing the hide for a hotkey
        // or Escape stop would strand the reading HUD on screen with nothing
        // behind it.
        for reason in [
            StopReason::Hotkey,
            StopReason::Escape,
            StopReason::Ui,
            StopReason::Superseded,
        ] {
            let controller = TtsController::default();
            controller.inner.session.claim();

            controller.stop(reason);

            assert!(
                !controller.inner.hud_yielded.load(Ordering::SeqCst),
                "{reason:?} must leave the hide to the HUD driver"
            );
        }
    }

    #[test]
    fn dictation_takeover_hands_the_hud_over() {
        let controller = TtsController::default();
        controller.inner.session.claim();

        controller.stop_for_dictation();

        assert!(!controller.is_active());
        assert!(controller.inner.hud_yielded.load(Ordering::SeqCst));
    }

    #[test]
    fn dictation_takeover_is_a_no_op_when_nothing_is_being_read() {
        let controller = TtsController::default();
        controller.stop_for_dictation();
        assert!(!controller.inner.hud_yielded.load(Ordering::SeqCst));
        assert!(!controller.is_active());
    }

    // --- translate-and-read ---

    use super::super::llm_stage::{LlmStageError, TRANSLATE_MAX_CHARS};
    use super::super::SpeechProgress;
    use super::{
        apply_translate_voice_override, caption_piece_limit, lingers_after_read, next_caption,
        falls_back_to_clipboard, stage_text, translate_voice_key, ReadKind,
    };
    use crate::selection::SelectionError;

    #[test]
    fn a_caption_is_replaced_by_the_next_sentence_and_by_nothing_else() {
        let pieces = vec!["One.".to_string(), "Two.".to_string()];
        let first = SpeechProgress::at(0, &pieces);
        let second = SpeechProgress::at(1, &pieces);

        assert_eq!(next_caption(None, None), None);
        assert_eq!(next_caption(None, first.clone()), first);
        assert_eq!(next_caption(first.as_ref(), first.clone()), None);
        assert_eq!(next_caption(first.as_ref(), second.clone()), second);
        // The backend going quiet ends the read; it does not blank the HUD.
        assert_eq!(next_caption(second.as_ref(), None), None);
    }

    #[test]
    fn a_caption_read_lingers_only_with_a_sentence_on_screen() {
        let shown = SpeechProgress::at(0, &["One.".to_string()]);

        assert!(lingers_after_read(false, None));
        assert!(lingers_after_read(true, shown.as_ref()));
        assert!(!lingers_after_read(true, None));
    }

    #[test]
    fn only_a_read_with_captions_on_is_split_into_sentences() {
        assert_eq!(caption_piece_limit(true), Some(120));
        assert_eq!(caption_piece_limit(false), None);
    }

    #[test]
    fn the_translate_hotkey_stops_a_plain_read_and_vice_versa() {
        // Any reading hotkey during a session means "stop"; the kind that
        // started it does not matter (requirements §2.2).
        let controller = TtsController::default();
        let _token = controller.inner.session.claim();
        assert!(controller.is_active());
        controller.handle_translate_selection_hotkey();
        assert!(!controller.is_active());

        let _token = controller.inner.session.claim();
        controller.handle_read_selection_hotkey();
        assert!(!controller.is_active());
    }

    #[test]
    fn the_read_clock_stops_when_the_session_is_handed_back() {
        let slot = SessionSlot::default();
        let token = slot.claim();
        let waiter = thread::spawn(move || wait_for_read_to_end(&token));
        thread::sleep(HUD_POLL * 3);
        slot.release();
        let elapsed = waiter
            .join()
            .expect("the read clock must not outlive the session");
        assert!(
            elapsed >= HUD_POLL * 3,
            "a read that lasted three polls cannot measure less"
        );
    }

    #[test]
    fn a_superseding_read_stops_the_previous_read_clock() {
        let slot = SessionSlot::default();
        let token = slot.claim();
        let waiter = thread::spawn(move || wait_for_read_to_end(&token));
        thread::sleep(HUD_POLL);
        // The next read takes the session without releasing it first; the
        // first read still ended, and still lasted as long as it lasted.
        let _next = slot.claim();
        waiter
            .join()
            .expect("being superseded must end the read, not hang it");
    }

    #[test]
    fn the_translate_hotkey_is_ignored_while_dictation_is_recording() {
        let controller = TtsController::default();
        controller.attach_recording_flag(Arc::new(AtomicBool::new(true)));
        controller.handle_translate_selection_hotkey();
        assert!(
            !controller.is_active(),
            "no session may start into a live microphone"
        );
    }

    // --- reading the clipboard when there is no selection ---

    #[test]
    fn only_an_unreadable_selection_falls_back_to_the_clipboard() {
        for err in [
            SelectionError::NoSelection,
            SelectionError::UnsupportedControl,
            SelectionError::CopyTimeout,
        ] {
            assert!(falls_back_to_clipboard(&err), "{}", err.code());
        }
        for err in [
            // A password field: the clipboard likely holds what goes into it.
            SelectionError::SecureInput,
            SelectionError::PermissionDenied,
            SelectionError::FocusIsSelf,
            SelectionError::ClipboardSnapshotRefused("promised type".to_string()),
            SelectionError::ForegroundChanged,
            SelectionError::ModifiersHeld,
            SelectionError::Cancelled,
        ] {
            assert!(!falls_back_to_clipboard(&err), "{}", err.code());
        }
    }

    #[test]
    fn the_voice_override_key_carries_the_aliyun_model() {
        let mut settings = AppSettings::default();
        settings.tts_provider_type = "volcengine".to_string();
        assert_eq!(translate_voice_key(&settings), "volcengine");
        settings.tts_provider_type = "aliyun".to_string();
        settings.aliyun_tts_model = crate::tts::aliyun::MODEL_COSYVOICE.to_string();
        assert_eq!(
            translate_voice_key(&settings),
            format!("aliyun:{}", crate::tts::aliyun::MODEL_COSYVOICE)
        );
    }

    #[test]
    fn a_missing_or_blank_override_keeps_the_reading_voice() {
        let mut settings = AppSettings::default();
        settings.tts_provider_type = "volcengine".to_string();
        settings.volc_tts_speaker = "reader".to_string();
        assert_eq!(apply_translate_voice_override(&mut settings), None);
        assert_eq!(settings.volc_tts_speaker, "reader");

        settings
            .tts_translate_voice_overrides
            .insert("volcengine".to_string(), "   ".to_string());
        assert_eq!(apply_translate_voice_override(&mut settings), None);
        assert_eq!(settings.volc_tts_speaker, "reader");
    }

    #[test]
    fn an_override_lands_on_the_providers_own_voice_field() {
        let mut settings = AppSettings::default();
        settings.tts_provider_type = "aliyun".to_string();
        settings.aliyun_tts_model = crate::tts::aliyun::MODEL_COSYVOICE_V35.to_string();
        settings.aliyun_tts_voice_cosy_voice_v35 = "reader".to_string();
        settings.aliyun_tts_voice_cosy_voice = "other-family".to_string();
        settings.tts_translate_voice_overrides.insert(
            format!("aliyun:{}", crate::tts::aliyun::MODEL_COSYVOICE_V35),
            "translator".to_string(),
        );
        // A key for a different model must not apply.
        settings.tts_translate_voice_overrides.insert(
            format!("aliyun:{}", crate::tts::aliyun::MODEL_COSYVOICE),
            "wrong".to_string(),
        );

        assert_eq!(
            apply_translate_voice_override(&mut settings).as_deref(),
            Some("translator")
        );
        assert_eq!(settings.aliyun_tts_voice_cosy_voice_v35, "translator");
        assert_eq!(settings.aliyun_tts_voice_cosy_voice, "other-family");
        assert_eq!(
            voice_request(&settings, "x".to_string()).voice.as_deref(),
            Some("translator"),
            "the request is built from the overridden field"
        );
    }

    #[test]
    fn a_system_override_switches_say_to_avspeech() {
        // The empty reading voice means `say`; an override id can only be
        // spoken by AVSpeech, and that choice is made from the same field.
        let mut settings = AppSettings::default();
        assert!(super::uses_say_voice(&settings));
        settings.tts_translate_voice_overrides.insert(
            "system".to_string(),
            "com.apple.voice.compact.en-US.Samantha".to_string(),
        );
        apply_translate_voice_override(&mut settings);
        assert!(!super::uses_say_voice(&settings));
    }

    #[test]
    fn an_oversized_selection_is_refused_before_any_llm_call() {
        let slot = SessionSlot::default();
        let token = slot.claim();
        let busy = AtomicBool::new(false);
        let text = "字".repeat(TRANSLATE_MAX_CHARS + 1);
        let result = stage_text(ReadKind::Translate, None, text, &token, &busy);
        assert!(matches!(
            result,
            Err(LlmStageError::TooLong { chars }) if chars == TRANSLATE_MAX_CHARS + 1
        ));
    }

    #[test]
    fn translate_without_an_api_key_is_not_configured() {
        let slot = SessionSlot::default();
        let token = slot.claim();
        let busy = AtomicBool::new(false);
        let settings = AppSettings::default();
        let result = stage_text(
            ReadKind::Translate,
            Some(&settings),
            "hi".to_string(),
            &token,
            &busy,
        );
        assert_eq!(result.err(), Some(LlmStageError::NotConfigured));
        assert!(
            !busy.load(Ordering::SeqCst),
            "the busy flag never leaks past the stage"
        );
    }

    #[test]
    fn plain_reading_passes_the_text_through_when_preprocessing_is_off() {
        let slot = SessionSlot::default();
        let token = slot.claim();
        let busy = AtomicBool::new(false);
        let settings = AppSettings::default();
        assert!(!settings.tts_preprocess_enabled);
        let staged = stage_text(
            ReadKind::Read,
            Some(&settings),
            "raw".to_string(),
            &token,
            &busy,
        )
        .expect("no LLM involved");
        assert_eq!(staged.text, "raw");
        assert!(!staged.llm_invoked);
    }

    #[test]
    fn a_failed_preprocess_reads_the_raw_text() {
        // Requirements §4.3: preprocessing is best-effort. No key configured
        // is the failure that needs no network to reproduce.
        let slot = SessionSlot::default();
        let token = slot.claim();
        let busy = AtomicBool::new(false);
        let mut settings = AppSettings::default();
        settings.tts_preprocess_enabled = true;
        let staged = stage_text(
            ReadKind::Read,
            Some(&settings),
            "raw".to_string(),
            &token,
            &busy,
        )
        .expect("falls back to the raw text");
        assert_eq!(staged.text, "raw");
        assert!(!staged.llm_invoked);
    }

    #[test]
    fn a_cancelled_preprocess_is_not_read_at_all() {
        let slot = SessionSlot::default();
        let token = slot.claim();
        slot.release();
        let busy = AtomicBool::new(false);
        let mut settings = AppSettings::default();
        settings.tts_preprocess_enabled = true;
        settings.llm_volcengine_api_key = "key".to_string();
        settings.llm_volcengine_base_url = "http://127.0.0.1:9".to_string();
        let result = stage_text(
            ReadKind::Read,
            Some(&settings),
            "raw".to_string(),
            &token,
            &busy,
        );
        assert_eq!(result.err(), Some(LlmStageError::Cancelled));
    }
}
