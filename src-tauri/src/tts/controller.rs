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

use tauri::{AppHandle, Emitter};

use super::aliyun::{self, AliyunBackend, AliyunConfig};
use super::azure::{self, AzureBackend, AzureConfig};
use super::mimo::{MimoBackend, MimoConfig};
use super::volcengine::{self, VolcengineBackend, VolcengineConfig};
use super::{
    log_event, SessionSlot, StopReason, TtsBackend, TtsError, TtsRequest, TtsStatus, TtsVoiceList,
};
use crate::commands::settings::AppSettings;
use crate::selection::{self, SelectionError, SelectionOutcome, SelectionRequest};
use crate::services::hud_service::{HudService, ReadingPhase};

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

/// Settings values selecting a cloud backend.
const PROVIDER_VOLCENGINE: &str = "volcengine";
const PROVIDER_ALIYUN: &str = "aliyun";
const PROVIDER_MIMO: &str = "mimo";
const PROVIDER_AZURE: &str = "azure";

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
    /// schedules on the way out.
    fn spawn_hud_driver(&self, backend: Arc<dyn TtsBackend>, failure: Arc<Mutex<Option<String>>>) {
        let Some(hud) = self.hud() else { return };
        let session = self.inner.session.clone();
        let yielded = self.inner.hud_yielded.clone();
        // A leftover yield from a previous handoff must not suppress hide on
        // this read — that flag is only meaningful for the session that set it.
        yielded.store(false, Ordering::SeqCst);

        hud.cancel_hide();
        // Compact presentation: a read has nothing to display but its own
        // existence, so it gets the small HUD rather than the one sized for
        // streaming transcripts.
        hud.show(true);
        hud.emit_error(None);
        hud.emit_reading(Some(ReadingPhase::Preparing));

        thread::Builder::new()
            .name("voicex-tts-hud".to_string())
            .spawn(move || {
                let mut shown = ReadingPhase::Preparing;
                while session.is_active() {
                    let phase = match backend.status() {
                        TtsStatus::Speaking => ReadingPhase::Speaking,
                        TtsStatus::Idle => ReadingPhase::Preparing,
                    };
                    if phase != shown {
                        shown = phase;
                        hud.emit_reading(Some(phase));
                    }
                    // Only backends that render audio themselves have a level;
                    // the system voice reports none and the HUD then shows just
                    // the animated icon instead of a waveform standing still.
                    if let Some(level) = backend.audio_level() {
                        hud.emit_audio_level(level);
                    }
                    thread::sleep(HUD_POLL);
                }

                hud.emit_reading(None);
                // Dictation already owns the window. Hiding here would take the
                // recording HUD down a moment after it appeared, and the next
                // dictation tap would stop a session the user thought never
                // started.
                if yielded.swap(false, Ordering::SeqCst) {
                    return;
                }
                let reported = failure.lock().ok().and_then(|slot| slot.clone());
                let hud_for_hide = hud.clone();
                match reported {
                    // Without this a failed read is completely silent: the user
                    // pressed the hotkey and nothing whatsoever happened.
                    Some(code) => {
                        hud.emit_error(Some(&code));
                        hud.schedule_hide(HUD_ERROR_LINGER_MS, move || {
                            hud_for_hide.emit_error(None);
                            hud_for_hide.hide();
                        });
                    }
                    None => hud.schedule_hide(HUD_LINGER_MS, move || hud_for_hide.hide()),
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
            self.inner
                .system
                .lock()
                .ok()
                .and_then(|slot| slot.clone())
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
        let active = self.is_active();
        // Dictation keeps the microphone. A new read would be inaudible and
        // would get transcribed, so the key is swallowed. An already-running
        // read is the stuck case takeover missed — stopping it is still right.
        if self.is_recording() && !active {
            log_event(
                "hotkey_action",
                &[
                    ("action", "read_selection".to_string()),
                    ("state", "recording".to_string()),
                ],
            );
            return;
        }

        log_event(
            "hotkey_action",
            &[
                ("action", "read_selection".to_string()),
                ("state", if active { "active" } else { "idle" }.to_string()),
            ],
        );

        if active {
            self.stop(StopReason::Hotkey);
        } else {
            self.start();
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

    fn start(&self) {
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
        let settings = load_settings();
        let provider = settings
            .as_ref()
            .map(|s| s.tts_provider_type.clone())
            .unwrap_or_default();
        let Some(backend) = self.backend_for(&provider, settings.as_ref()) else {
            log_event("speak_err", &[("error", "unsupported".to_string())]);
            return;
        };

        let token = self.inner.session.claim();
        self.set_active_backend(Some(backend.clone()));
        let failure: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        self.spawn_hud_driver(backend.clone(), failure.clone());

        thread::Builder::new()
            .name("voicex-tts-read".to_string())
            .spawn(move || {
                log_event("selection_start", &[]);
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

                match result {
                    Ok(outcome) => {
                        log_selection_ok(&outcome);
                        log_event(
                            "speak_start",
                            &[
                                ("backend", backend.name().to_string()),
                                ("chars", outcome.text.chars().count().to_string()),
                            ],
                        );
                        // No length gate: a cloud backend splits an oversized
                        // selection into per-request pieces itself, so the
                        // whole selection is read rather than a truncated
                        // prefix of it.
                        let request = match settings {
                            Some(settings) => voice_request(&settings, outcome.text),
                            None => TtsRequest::plain(outcome.text),
                        };
                        if let Err(err) = backend.start(request, token) {
                            log_event(
                                "speak_err",
                                &[
                                    ("error", err.code().to_string()),
                                    ("detail", err.to_string()),
                                ],
                            );
                            // The backend has already handed the session back
                            // (its `start` contract), so this lands a hair
                            // after the driver could see idle; the driver's
                            // own emits on the way out cover that gap. Without
                            // it a refused start — no API key, no voice id
                            // for a designed-voice model — is a HUD that
                            // flashes "preparing" and vanishes.
                            if let Ok(mut slot) = failure.lock() {
                                *slot = Some(err.code().to_string());
                            }
                        }
                    }
                    Err(err) => {
                        log_selection_error(&err);
                        // Record before releasing the session: the HUD driver
                        // reads this the moment it sees the session go idle.
                        if let Ok(mut slot) = failure.lock() {
                            *slot = Some(err.code().to_string());
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
    } else if settings.aliyun_tts_model == aliyun::MODEL_COSYVOICE_V35 {
        settings.aliyun_tts_voice_cosy_voice_v35.clone()
    } else if settings.aliyun_tts_model == aliyun::MODEL_COSYVOICE {
        settings.aliyun_tts_voice_cosy_voice.clone()
    } else {
        settings.aliyun_tts_voice_qwen3.clone()
    }
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

    use super::super::SessionSlot;
    use super::{voice_request, StopReason, TtsController};
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
        };

        let mut settings = AppSettings::default();
        settings.tts_provider_type = "aliyun".to_string();
        settings.aliyun_tts_voice_qwen3 = "Dylan".to_string();
        settings.aliyun_tts_voice_qwen_audio = "longanfengyue".to_string();
        settings.aliyun_tts_voice_cosy_voice = "longanyang".to_string();
        settings.aliyun_tts_voice_cosy_voice_v35 =
            "cosyvoice-v3.5-flash-vd-test".to_string();

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
}
