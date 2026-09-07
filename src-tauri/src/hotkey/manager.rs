//! Hotkey manager - records and listens for global hotkeys using rdev.

use std::{
    cell::RefCell,
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        mpsc::{self, Receiver, Sender},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};

use rdev::{Event, EventType, Key};
use serde::Serialize;
use tauri::Emitter;

use super::config::HotkeyConfiguration;
use crate::session::{SessionCoordinator, SessionMessage};
use crate::tts::TtsController;

#[derive(Debug)]
enum HookEvent {
    RecordComplete(HotkeyConfiguration),
    Pressed(HotkeyConfiguration),
    Released(HotkeyConfiguration),
    EscapePressed,
    /// Selected-text reading hotkey. Phase 0 binds this to a hardcoded
    /// combination; phase 2 generalises the hook into an action map with
    /// conflict detection. There is still exactly one system-level listener.
    ReadSelectionPressed,
    /// Escape while reading aloud. Kept separate from `EscapePressed` so the
    /// dictation cancel path is untouched.
    ReadSelectionEscape,
    /// The dictation hotkey went down; reading must yield to it.
    DictationTakesOver,
    /// Diagnostic snapshot of a shortcut-shaped key press (root-cause hunt for
    /// "the first press does nothing"). Sent through the channel so the tap
    /// callback itself never does IO.
    #[cfg(target_os = "macos")]
    Diag(DiagSnapshot),
}

/// What the hook believed vs. what the session actually held, at the moment a
/// non-modifier key went down with modifiers involved. `tracked_*` comes from
/// the event-driven [`ModifierState`]; `actual_*` from
/// `CGEventSourceFlagsState`, which cannot go stale. A mismatch is the
/// modifier-state desync in person.
#[cfg(target_os = "macos")]
#[derive(Debug)]
struct DiagSnapshot {
    key_code: u32,
    tracked_mods: u32,
    actual_mods: u32,
    tracked_fn: bool,
    actual_fn: bool,
    /// Tracked and authoritative state disagree, after accounting for the
    /// pressed key's own stripped bit. With the sync in place this should
    /// never be true again; a `true` here is a regression alarm, which is why
    /// the diag line stays in.
    desynced: bool,
    dictation_match: bool,
    read_match: bool,
    read_latched: bool,
    suspended: u32,
}

/// The modifier bit a key contributes to the flags itself, which
/// [`HotkeySnapshot::from_event`] strips from its own snapshot. The diag
/// comparison has to strip the same bit from the authoritative side, or every
/// modifier press would read as a false desync.
#[cfg(target_os = "macos")]
fn own_modifier_bit(key_code: u32) -> u32 {
    match key_code {
        56 | 60 => 0x0200, // shift
        59 | 62 => 0x1000, // control
        58 | 61 => 0x0800, // option
        55 | 54 => 0x0100, // command
        _ => 0,
    }
}

/// The session's live modifier flags, as our internal modifier bits plus the
/// Fn state. Authoritative: unlike the event-tracked state it does not depend
/// on having seen every FlagsChanged event.
#[cfg(target_os = "macos")]
fn authoritative_modifier_bits() -> (u32, bool) {
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventSourceFlagsState(state_id: u32) -> u64;
    }
    const COMBINED_SESSION_STATE: u32 = 0;
    let flags = unsafe { CGEventSourceFlagsState(COMBINED_SESSION_STATE) };
    let mut bits = 0u32;
    if flags & 0x0004_0000 != 0 {
        bits |= 0x1000; // control
    }
    if flags & 0x0008_0000 != 0 {
        bits |= 0x0800; // option
    }
    if flags & 0x0002_0000 != 0 {
        bits |= 0x0200; // shift
    }
    if flags & 0x0010_0000 != 0 {
        bits |= 0x0100; // command
    }
    (bits, flags & 0x0080_0000 != 0)
}

/// State of the selected-text reading binding, as the settings page sees it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadSelectionStatus {
    /// A binding is configured at all (the feature is switched on).
    pub bound: bool,
    /// The hook will actually act on it.
    pub enabled: bool,
    /// Bound but suppressed because it duplicates the dictation hotkey.
    pub conflicts_with_dictation: bool,
    pub display: Option<String>,
}

#[derive(Clone)]
pub struct HotkeyManager {
    config: Arc<Mutex<Option<HotkeyConfiguration>>>,
    active_key_code: Arc<AtomicU32>,
    active_modifiers: Arc<AtomicU32>,
    active_uses_fn: Arc<AtomicBool>,
    active_enabled: Arc<AtomicBool>,
    read_selection_config: Arc<Mutex<Option<HotkeyConfiguration>>>,
    read_selection_key_code: Arc<AtomicU32>,
    read_selection_modifiers: Arc<AtomicU32>,
    read_selection_uses_fn: Arc<AtomicBool>,
    read_selection_enabled: Arc<AtomicBool>,
    suspension_count: Arc<AtomicU32>,
    listener_started: Arc<AtomicBool>,
    recording_sender: Arc<Mutex<Option<Sender<HotkeyConfiguration>>>>,
    swallow_escape: Arc<AtomicBool>,
    /// Whether we are actively recording a hotkey combination
    recording_active: Arc<AtomicBool>,
    /// Accumulated hotkey configuration during recording
    recording_accumulated_config: Arc<Mutex<Option<HotkeyConfiguration>>>,
}

impl HotkeyManager {
    pub fn new() -> Self {
        Self {
            config: Arc::new(Mutex::new(None)),
            active_key_code: Arc::new(AtomicU32::new(0)),
            active_modifiers: Arc::new(AtomicU32::new(0)),
            active_uses_fn: Arc::new(AtomicBool::new(false)),
            active_enabled: Arc::new(AtomicBool::new(false)),
            read_selection_config: Arc::new(Mutex::new(None)),
            read_selection_key_code: Arc::new(AtomicU32::new(0)),
            read_selection_modifiers: Arc::new(AtomicU32::new(0)),
            read_selection_uses_fn: Arc::new(AtomicBool::new(false)),
            read_selection_enabled: Arc::new(AtomicBool::new(false)),
            suspension_count: Arc::new(AtomicU32::new(0)),
            listener_started: Arc::new(AtomicBool::new(false)),
            recording_sender: Arc::new(Mutex::new(None)),
            swallow_escape: Arc::new(AtomicBool::new(false)),
            recording_active: Arc::new(AtomicBool::new(false)),
            recording_accumulated_config: Arc::new(Mutex::new(None)),
        }
    }

    /// Start the global listener once for the app lifetime.
    ///
    /// All hotkey actions share this one listener; adding an action must never
    /// mean adding a second system-level keyboard hook.
    pub fn start_listener(
        &self,
        app: tauri::AppHandle,
        session: Option<SessionCoordinator>,
        tts: Option<TtsController>,
    ) {
        if self.listener_started.swap(true, Ordering::SeqCst) {
            return;
        }

        let active_key_code = self.active_key_code.clone();
        let active_modifiers = self.active_modifiers.clone();
        let active_uses_fn = self.active_uses_fn.clone();
        let active_enabled = self.active_enabled.clone();
        let read_selection_key_code = self.read_selection_key_code.clone();
        let read_selection_modifiers = self.read_selection_modifiers.clone();
        let read_selection_uses_fn = self.read_selection_uses_fn.clone();
        let read_selection_enabled = self.read_selection_enabled.clone();
        let suspension_count = self.suspension_count.clone();
        let recording_sender = self.recording_sender.clone();
        let swallow_escape = self.swallow_escape.clone();
        let session_handler = session.clone();
        let recording_active = self.recording_active.clone();
        let recording_accumulated_config = self.recording_accumulated_config.clone();
        let tts_active = tts.as_ref().map(|controller| controller.active_handle());

        thread::spawn(move || {
            let modifier_state = RefCell::new(ModifierState::default());
            let last_key_for_config: RefCell<Option<Key>> = RefCell::new(None);
            let last_active_config: RefCell<Option<HotkeyConfiguration>> = RefCell::new(None);
            let active_hotkey_pressed = RefCell::new(false);
            let read_selection_key: RefCell<Option<Key>> = RefCell::new(None);
            let (hook_tx, hook_rx) = mpsc::channel::<HookEvent>();

            // Worker thread to process hotkey actions off the hook callback.
            let worker_app = app.clone();
            let worker_session = session_handler.clone();
            let worker_recording = recording_sender.clone();
            let worker_tts = tts.clone();
            thread::spawn(move || {
                while let Ok(event) = hook_rx.recv() {
                    match event {
                        HookEvent::RecordComplete(cfg) => {
                            if let Ok(mut guard) = worker_recording.lock() {
                                if let Some(sender) = guard.take() {
                                    let _ = sender.send(cfg);
                                }
                            }
                        }
                        HookEvent::Pressed(cfg) => {
                            if let Some(handler) = worker_session.as_ref() {
                                handler.send(SessionMessage::HotkeyPressed);
                            }
                            let _ = worker_app.emit("hotkey:pressed", cfg.display_string());
                        }
                        HookEvent::Released(cfg) => {
                            if let Some(handler) = worker_session.as_ref() {
                                handler.send(SessionMessage::HotkeyReleased);
                            }
                            let _ = worker_app.emit("hotkey:released", cfg.display_string());
                        }
                        HookEvent::EscapePressed => {
                            if let Some(handler) = worker_session.as_ref() {
                                handler.send(SessionMessage::CancelSession(
                                    crate::session::CancelReason::EscapeKey,
                                ));
                            }
                        }
                        HookEvent::ReadSelectionPressed => {
                            if let Some(controller) = worker_tts.as_ref() {
                                controller.handle_read_selection_hotkey();
                            }
                        }
                        HookEvent::ReadSelectionEscape => {
                            if let Some(controller) = worker_tts.as_ref() {
                                controller.stop(crate::tts::StopReason::Escape);
                            }
                        }
                        HookEvent::DictationTakesOver => {
                            if let Some(controller) = worker_tts.as_ref() {
                                controller.stop_for_dictation();
                            }
                        }
                        #[cfg(target_os = "macos")]
                        HookEvent::Diag(diag) => {
                            // Routine presses are debug-only; a desync "should
                            // never be true again" (see DiagSnapshot) and is the
                            // regression alarm this diag exists for, so that
                            // case alone stays loud.
                            let (level, tag) = if diag.desynced {
                                (log::Level::Warn, "event=hook_press_desync")
                            } else {
                                (log::Level::Debug, "event=hook_press")
                            };
                            log::log!(
                                target: "voicex::hotkey",
                                level,
                                "{} key={} tracked_mods={:#06x} actual_mods={:#06x} \
                                 tracked_fn={} actual_fn={} desynced={} dict_match={} \
                                 read_match={} read_latched={} suspended={}",
                                tag,
                                diag.key_code,
                                diag.tracked_mods,
                                diag.actual_mods,
                                diag.tracked_fn,
                                diag.actual_fn,
                                diag.desynced,
                                diag.dictation_match,
                                diag.read_match,
                                diag.read_latched,
                                diag.suspended,
                            );
                        }
                    }
                }
            });

            // Use grab so we can optionally swallow the active hotkey from the system (e.g., IME).
            let callback = move |event: Event| -> Option<Event> {
                let mut suppress = false;
                match event.event_type {
                    EventType::KeyPress(key) => {
                        // The authoritative session flags, queried once per
                        // press. The event-driven state below is re-seeded from
                        // them every time: it can go stale whenever the tap
                        // misses a FlagsChanged event (tap disabled, secure
                        // input, misclassified press/release), and a stale
                        // modifier silently un-matches every combo until the
                        // user's own key releases repair it — the "first press
                        // does nothing, second works" bug.
                        #[cfg(target_os = "macos")]
                        let (auth_mods, auth_fn) = authoritative_modifier_bits();

                        let mut mods = modifier_state.borrow_mut();
                        #[cfg(target_os = "macos")]
                        mods.sync_from_bits(auth_mods, auth_fn);
                        mods.on_press(key);
                        if key == Key::Escape {
                            let _ = hook_tx.send(HookEvent::EscapePressed);
                            // Swallow ESC only when hotkey handling is active (not during recording suspension).
                            if active_enabled.load(Ordering::SeqCst)
                                && suspension_count.load(Ordering::SeqCst) == 0
                                && swallow_escape.load(Ordering::SeqCst)
                            {
                                suppress = true;
                            }

                            // Escape also cancels reading, but only while
                            // reading is actually happening (plan §3.3) —
                            // otherwise Escape must reach the foreground app
                            // untouched. Read lock-free: taking a lock inside
                            // the event tap callback risks the tap timing out.
                            if tts_active
                                .as_ref()
                                .is_some_and(|flag| flag.load(Ordering::SeqCst) != 0)
                                && suspension_count.load(Ordering::SeqCst) == 0
                            {
                                let _ = hook_tx.send(HookEvent::ReadSelectionEscape);
                                suppress = true;
                            }
                        }
                        if let Some(snapshot) = HotkeySnapshot::from_event(key, &mods) {
                            let cfg = snapshot.to_config();

                            // Recording mode: accumulate the configuration instead of sending immediately
                            if recording_active.load(Ordering::SeqCst) {
                                if let Ok(mut guard) = recording_accumulated_config.lock() {
                                    *guard = Some(cfg.clone());
                                }
                            }

                            let enabled = active_enabled.load(Ordering::SeqCst);
                            let active_match = snapshot.matches_active(
                                active_key_code.load(Ordering::SeqCst),
                                active_modifiers.load(Ordering::SeqCst),
                                active_uses_fn.load(Ordering::SeqCst),
                            );

                            let read_selection_match = snapshot.matches_active(
                                read_selection_key_code.load(Ordering::SeqCst),
                                read_selection_modifiers.load(Ordering::SeqCst),
                                read_selection_uses_fn.load(Ordering::SeqCst),
                            );

                            // Diagnostics for the modifier-state desync class of
                            // bug: whenever a key goes down that looks like a
                            // shortcut (Ctrl/Option/Cmd/Fn involved on either
                            // view), record what the event-tracked state and the
                            // authoritative session flags each claim. Shift-only
                            // presses are ordinary typing and stay out of the log.
                            #[cfg(target_os = "macos")]
                            {
                                let shortcut_shaped = (snapshot.modifiers | auth_mods) & !0x0200
                                    != 0
                                    || snapshot.uses_fn
                                    || auth_fn;
                                if shortcut_shaped {
                                    // The pressed key's own bit is stripped from
                                    // the snapshot but present in the session
                                    // flags; the fn state lags on the Fn key's
                                    // own press. Neither is a desync.
                                    let desynced = snapshot.modifiers
                                        != auth_mods & !own_modifier_bit(cfg.key_code)
                                        || (cfg.key_code != 63 && snapshot.uses_fn != auth_fn);
                                    let _ = hook_tx.send(HookEvent::Diag(DiagSnapshot {
                                        key_code: cfg.key_code,
                                        tracked_mods: snapshot.modifiers,
                                        actual_mods: auth_mods,
                                        tracked_fn: snapshot.uses_fn,
                                        actual_fn: auth_fn,
                                        desynced,
                                        dictation_match: active_match,
                                        read_match: read_selection_match,
                                        read_latched: read_selection_key.borrow().is_some(),
                                        suspended: suspension_count.load(Ordering::SeqCst),
                                    }));
                                }
                            }

                            if enabled
                                && active_match
                                && suspension_count.load(Ordering::SeqCst) == 0
                            {
                                *last_key_for_config.borrow_mut() = Some(key);
                                *last_active_config.borrow_mut() = Some(cfg.clone());
                                if !*active_hotkey_pressed.borrow() {
                                    *active_hotkey_pressed.borrow_mut() = true;
                                    // Dictation takes priority over reading:
                                    // speech playing into a live microphone
                                    // gets transcribed back (plan §3.3).
                                    if tts_active
                                        .as_ref()
                                        .is_some_and(|flag| flag.load(Ordering::SeqCst) != 0)
                                    {
                                        let _ = hook_tx.send(HookEvent::DictationTakesOver);
                                    }
                                    let _ = hook_tx.send(HookEvent::Pressed(cfg));
                                }
                                suppress = true;
                            } else if read_selection_enabled.load(Ordering::SeqCst)
                                && read_selection_match
                                && suspension_count.load(Ordering::SeqCst) == 0
                            {
                                // Fire once per physical press; key repeat while
                                // held must not re-trigger the action.
                                if read_selection_key.borrow().is_none() {
                                    *read_selection_key.borrow_mut() = Some(key);
                                    let _ = hook_tx.send(HookEvent::ReadSelectionPressed);
                                }
                                suppress = true;
                            }
                        }
                    }
                    EventType::KeyRelease(key) => {
                        {
                            let mut mods = modifier_state.borrow_mut();
                            // Same re-seed as on press, so the tracked state
                            // never carries a stale modifier across events even
                            // between presses.
                            #[cfg(target_os = "macos")]
                            {
                                let (auth_mods, auth_fn) = authoritative_modifier_bits();
                                mods.sync_from_bits(auth_mods, auth_fn);
                            }
                            mods.on_release(key);
                        }

                        // Recording mode: send accumulated config on key release
                        if recording_active.load(Ordering::SeqCst) {
                            if let Ok(mut guard) = recording_accumulated_config.lock() {
                                if let Some(cfg) = guard.take() {
                                    let _ = hook_tx.send(HookEvent::RecordComplete(cfg));
                                }
                            }
                        }

                        let active_key_opt = *last_key_for_config.borrow();
                        if let Some(active_key) = active_key_opt {
                            if key == active_key {
                                let cfg_opt = last_active_config.borrow().as_ref().cloned();
                                let was_pressed = *active_hotkey_pressed.borrow();
                                *active_hotkey_pressed.borrow_mut() = false;
                                if suspension_count.load(Ordering::SeqCst) == 0 {
                                    if was_pressed {
                                        if let Some(cfg) = cfg_opt {
                                            let _ = hook_tx.send(HookEvent::Released(cfg));
                                        }
                                    }
                                }
                                *last_key_for_config.borrow_mut() = None;
                                *last_active_config.borrow_mut() = None;
                                suppress = true;
                            }
                        }

                        // Swallow the matching release so the target app never
                        // sees a stray key-up for a hotkey whose press we ate.
                        // Cleared regardless of suspension to avoid a stuck latch.
                        let read_selection_key_opt = *read_selection_key.borrow();
                        if let Some(pressed_key) = read_selection_key_opt {
                            if key == pressed_key {
                                *read_selection_key.borrow_mut() = None;
                                suppress = true;
                            }
                        }
                    }
                    _ => {}
                }
                if suppress {
                    None
                } else {
                    Some(event)
                }
            };

            if let Err(err) = rdev::grab(callback) {
                log::error!("Global hotkey listener failed: {:?}", err);
            }
        });
    }

    /// Update active hotkey configuration used for recognition.
    pub fn set_config(&self, config: Option<HotkeyConfiguration>) {
        if let Ok(mut guard) = self.config.lock() {
            *guard = config.clone();
        }

        if let Some(cfg) = config {
            self.active_key_code.store(cfg.key_code, Ordering::SeqCst);
            self.active_modifiers
                .store(cfg.modifiers_bits(), Ordering::SeqCst);
            self.active_uses_fn.store(cfg.uses_fn, Ordering::SeqCst);
            self.active_enabled.store(true, Ordering::SeqCst);
        } else {
            self.active_enabled.store(false, Ordering::SeqCst);
        }

        // The dictation key can change at runtime, so re-evaluate the reading
        // binding against it rather than only at registration time.
        self.refresh_read_selection_binding();
    }

    /// Get current configuration.
    pub fn current_config(&self) -> Option<HotkeyConfiguration> {
        self.config.lock().ok().and_then(|c| c.clone())
    }

    /// Set the selected-text reading binding. `None` unbinds it, which is how
    /// the master switch in the reading settings turns the feature off.
    pub fn set_read_selection_config(&self, config: Option<HotkeyConfiguration>) {
        if let Ok(mut guard) = self.read_selection_config.lock() {
            *guard = config;
        }
        self.refresh_read_selection_binding();
    }

    /// What actually happened to the reading binding, for the settings page.
    ///
    /// The binding can be off for two very different reasons — the user turned
    /// the feature off, or it collides with the dictation key — and only the
    /// second one needs explaining in the UI. Recomputed rather than cached so
    /// it stays right after the dictation key changes from the other page.
    pub fn read_selection_status(&self) -> ReadSelectionStatus {
        let config = self
            .read_selection_config
            .lock()
            .ok()
            .and_then(|guard| guard.clone());

        let Some(config) = config else {
            return ReadSelectionStatus {
                bound: false,
                enabled: false,
                conflicts_with_dictation: false,
                display: None,
            };
        };

        ReadSelectionStatus {
            bound: true,
            enabled: self.read_selection_enabled.load(Ordering::SeqCst),
            conflicts_with_dictation: self.conflicts_with_dictation(&config),
            display: Some(config.display_string()),
        }
    }

    /// Re-apply the reading binding, disabling it when it collides with the
    /// dictation key.
    ///
    /// The dictation branch is checked first in the hook, so an identical
    /// binding would make reading unreachable with no sign of why. Refusing the
    /// binding loudly beats swallowing the key and doing nothing.
    fn refresh_read_selection_binding(&self) {
        let Some(cfg) = self
            .read_selection_config
            .lock()
            .ok()
            .and_then(|guard| guard.clone())
        else {
            self.read_selection_enabled.store(false, Ordering::SeqCst);
            return;
        };

        if self.conflicts_with_dictation(&cfg) {
            log::warn!(
                "Selected-text reading hotkey ({}) is also the dictation hotkey; \
                 reading is disabled until one of them changes",
                cfg.display_string()
            );
            self.read_selection_enabled.store(false, Ordering::SeqCst);
            return;
        }

        self.read_selection_key_code
            .store(cfg.key_code, Ordering::SeqCst);
        self.read_selection_modifiers
            .store(cfg.modifiers_bits(), Ordering::SeqCst);
        self.read_selection_uses_fn
            .store(cfg.uses_fn, Ordering::SeqCst);
        self.read_selection_enabled.store(true, Ordering::SeqCst);
    }

    fn conflicts_with_dictation(&self, cfg: &HotkeyConfiguration) -> bool {
        if !self.active_enabled.load(Ordering::SeqCst) {
            return false;
        }
        cfg.key_code == self.active_key_code.load(Ordering::SeqCst)
            && cfg.modifiers_bits() == self.active_modifiers.load(Ordering::SeqCst)
            && cfg.uses_fn == self.active_uses_fn.load(Ordering::SeqCst)
    }

    /// Control whether ESC should be swallowed by the global hook.
    pub fn set_escape_swallowing(&self, enabled: bool) {
        self.swallow_escape.store(enabled, Ordering::SeqCst);
    }

    /// Suspend hotkey triggers (e.g., during hotkey recording)
    pub fn begin_suspension(&self) {
        self.suspension_count.fetch_add(1, Ordering::SeqCst);
        log::debug!("Hotkey suspension started");
    }

    /// Resume hotkey triggers
    pub fn end_suspension(&self) {
        let prev = self.suspension_count.fetch_sub(1, Ordering::SeqCst);
        if prev == 0 {
            self.suspension_count.store(0, Ordering::SeqCst);
        }
        log::debug!("Hotkey suspension ended");
    }

    /// Capture the next key combination globally (with timeout).
    /// Uses accumulative recording: waits for key release to capture the full combination.
    pub fn record_once(&self, timeout_ms: u64) -> Result<HotkeyConfiguration, HotkeyError> {
        let (tx, rx): (Sender<HotkeyConfiguration>, Receiver<HotkeyConfiguration>) =
            mpsc::channel();
        if let Ok(mut guard) = self.recording_sender.lock() {
            *guard = Some(tx);
        }
        // Clear any previously accumulated config
        if let Ok(mut guard) = self.recording_accumulated_config.lock() {
            *guard = None;
        }
        self.begin_suspension();
        // Enable recording mode
        self.recording_active.store(true, Ordering::SeqCst);
        let result = rx
            .recv_timeout(Duration::from_millis(timeout_ms))
            .map_err(|_| HotkeyError::Timeout);
        // Disable recording mode
        self.recording_active.store(false, Ordering::SeqCst);
        self.end_suspension();
        result
    }
}

impl Default for HotkeyManager {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum HotkeyError {
    #[error("Failed to register hotkey: {0}")]
    RegistrationFailed(String),

    #[error("Hotkey conflict with another application")]
    Conflict,

    #[error("Permission denied - accessibility access required")]
    PermissionDenied,

    #[error("Timed out waiting for hotkey input")]
    Timeout,
}

#[derive(Default)]
struct ModifierState {
    ctrl: bool,
    alt: bool,
    shift: bool,
    meta: bool,
    fn_key: bool,
}

impl ModifierState {
    /// Re-seed from the authoritative session flags. The event-driven updates
    /// below still run afterwards for the key in hand — the session state can
    /// lag behind the very event being processed — but any modifier this state
    /// wrongly remembered from a missed or misclassified earlier event is
    /// corrected here before it can un-match a combo.
    #[cfg(target_os = "macos")]
    fn sync_from_bits(&mut self, bits: u32, fn_key: bool) {
        self.ctrl = bits & 0x1000 != 0;
        self.alt = bits & 0x0800 != 0;
        self.shift = bits & 0x0200 != 0;
        self.meta = bits & 0x0100 != 0;
        self.fn_key = fn_key;
    }

    fn on_press(&mut self, key: Key) {
        match key {
            Key::ControlLeft | Key::ControlRight => self.ctrl = true,
            Key::Alt | Key::AltGr => self.alt = true,
            Key::ShiftLeft | Key::ShiftRight => self.shift = true,
            Key::MetaLeft | Key::MetaRight => self.meta = true,
            Key::Function => self.fn_key = true,
            _ => {}
        }
    }

    fn on_release(&mut self, key: Key) {
        match key {
            Key::ControlLeft | Key::ControlRight => self.ctrl = false,
            Key::Alt | Key::AltGr => self.alt = false,
            Key::ShiftLeft | Key::ShiftRight => self.shift = false,
            Key::MetaLeft | Key::MetaRight => self.meta = false,
            Key::Function => self.fn_key = false,
            _ => {}
        }
    }

    fn modifiers_bits(&self) -> u32 {
        let mut bits = 0;
        if self.ctrl {
            bits |= 0x1000;
        }
        if self.alt {
            bits |= 0x0800;
        }
        if self.shift {
            bits |= 0x0200;
        }
        if self.meta {
            bits |= 0x0100;
        }
        bits
    }
}

#[derive(Clone, Debug)]
struct HotkeySnapshot {
    key: Key,
    modifiers: u32,
    uses_fn: bool,
}

impl HotkeySnapshot {
    fn from_event(key: Key, mods: &ModifierState) -> Option<Self> {
        let is_modifier = matches!(
            key,
            Key::ControlLeft
                | Key::ControlRight
                | Key::Alt
                | Key::AltGr
                | Key::ShiftLeft
                | Key::ShiftRight
                | Key::MetaLeft
                | Key::MetaRight
                | Key::Function
        );

        // For standard combos, only capture when a non-modifier key is pressed.
        if is_modifier
            && !HotkeyConfiguration::is_modifier_only_key_code(key_code_from_key(key))
            && key != Key::Function
        {
            return None;
        }

        let mut snapshot = Self {
            key,
            modifiers: mods.modifiers_bits(),
            uses_fn: mods.fn_key || key == Key::Function,
        };

        // If the key itself is a modifier, drop the matching modifier flag to avoid duplicate labels.
        match key {
            Key::ShiftLeft | Key::ShiftRight => snapshot.modifiers &= !0x0200,
            Key::ControlLeft | Key::ControlRight => snapshot.modifiers &= !0x1000,
            Key::Alt | Key::AltGr => snapshot.modifiers &= !0x0800,
            Key::MetaLeft | Key::MetaRight => snapshot.modifiers &= !0x0100,
            _ => {}
        }

        Some(snapshot)
    }

    fn to_config(&self) -> HotkeyConfiguration {
        HotkeyConfiguration::with_uses_fn(key_code_from_key(self.key), self.modifiers, self.uses_fn)
    }

    fn matches_active(&self, key_code: u32, modifiers: u32, uses_fn: bool) -> bool {
        key_code_from_key(self.key) == key_code
            && self.modifiers == modifiers
            && self.uses_fn == uses_fn
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_selection_enabled(manager: &HotkeyManager) -> bool {
        manager.read_selection_enabled.load(Ordering::SeqCst)
    }

    #[test]
    fn distinct_bindings_both_stay_enabled() {
        let manager = HotkeyManager::new();
        manager.set_config(Some(HotkeyConfiguration::default_primary()));
        manager.set_read_selection_config(Some(HotkeyConfiguration::default_read_selection()));

        assert!(read_selection_enabled(&manager));
        assert_eq!(
            manager.read_selection_key_code.load(Ordering::SeqCst),
            'R' as u32
        );
    }

    #[test]
    fn a_binding_identical_to_dictation_is_refused() {
        let manager = HotkeyManager::new();
        let shared = HotkeyConfiguration::default_read_selection();
        manager.set_config(Some(shared.clone()));
        manager.set_read_selection_config(Some(shared));

        // The dictation branch wins in the hook, so leaving this enabled would
        // swallow the key and silently do nothing.
        assert!(!read_selection_enabled(&manager));
    }

    #[test]
    fn changing_dictation_onto_the_reading_key_disables_reading() {
        let manager = HotkeyManager::new();
        manager.set_config(Some(HotkeyConfiguration::default_primary()));
        manager.set_read_selection_config(Some(HotkeyConfiguration::default_read_selection()));
        assert!(read_selection_enabled(&manager));

        manager.set_config(Some(HotkeyConfiguration::default_read_selection()));
        assert!(!read_selection_enabled(&manager));
    }

    #[test]
    fn moving_dictation_away_re_enables_reading() {
        let manager = HotkeyManager::new();
        manager.set_config(Some(HotkeyConfiguration::default_read_selection()));
        manager.set_read_selection_config(Some(HotkeyConfiguration::default_read_selection()));
        assert!(!read_selection_enabled(&manager));

        manager.set_config(Some(HotkeyConfiguration::default_primary()));
        assert!(read_selection_enabled(&manager));
    }

    #[test]
    fn the_status_tells_switched_off_apart_from_conflicting() {
        // Both leave reading dead, but only one of them is worth explaining in
        // the settings page — and only one is the user's own doing.
        let manager = HotkeyManager::new();
        manager.set_config(Some(HotkeyConfiguration::default_primary()));

        manager.set_read_selection_config(None);
        let off = manager.read_selection_status();
        assert!(!off.bound);
        assert!(!off.conflicts_with_dictation);
        assert!(off.display.is_none());

        manager.set_config(Some(HotkeyConfiguration::default_read_selection()));
        manager.set_read_selection_config(Some(HotkeyConfiguration::default_read_selection()));
        let clash = manager.read_selection_status();
        assert!(clash.bound, "the user did configure a binding");
        assert!(!clash.enabled, "but the hook will not act on it");
        assert!(clash.conflicts_with_dictation);
        assert_eq!(clash.display.as_deref(), Some("Option + Command + R"));
    }

    #[test]
    fn moving_dictation_away_clears_the_reported_conflict() {
        // The dictation key changes from a different settings page, so the
        // status has to be recomputed rather than remembered.
        let manager = HotkeyManager::new();
        manager.set_config(Some(HotkeyConfiguration::default_read_selection()));
        manager.set_read_selection_config(Some(HotkeyConfiguration::default_read_selection()));
        assert!(manager.read_selection_status().conflicts_with_dictation);

        manager.set_config(Some(HotkeyConfiguration::default_primary()));
        let status = manager.read_selection_status();
        assert!(!status.conflicts_with_dictation);
        assert!(status.enabled);
    }

    #[test]
    fn clearing_the_reading_binding_disables_it() {
        let manager = HotkeyManager::new();
        manager.set_config(Some(HotkeyConfiguration::default_primary()));
        manager.set_read_selection_config(Some(HotkeyConfiguration::default_read_selection()));
        assert!(read_selection_enabled(&manager));

        manager.set_read_selection_config(None);
        assert!(!read_selection_enabled(&manager));
    }
}

fn key_code_from_key(key: Key) -> u32 {
    match key {
        Key::Space => 49,
        Key::Return | Key::KpReturn => 36,
        Key::Tab => 48,
        Key::Escape => 53,
        Key::Backspace => 51,
        Key::ShiftRight => 60,
        Key::ShiftLeft => 56,
        Key::MetaRight => 54,
        Key::MetaLeft => 55,
        Key::Alt => 58,   // Left Alt/Option
        Key::AltGr => 61, // Right Alt
        Key::ControlLeft => 59,
        Key::ControlRight => 62,
        Key::Function => 63,
        Key::KeyA => 'A' as u32,
        Key::KeyB => 'B' as u32,
        Key::KeyC => 'C' as u32,
        Key::KeyD => 'D' as u32,
        Key::KeyE => 'E' as u32,
        Key::KeyF => 'F' as u32,
        Key::KeyG => 'G' as u32,
        Key::KeyH => 'H' as u32,
        Key::KeyI => 'I' as u32,
        Key::KeyJ => 'J' as u32,
        Key::KeyK => 'K' as u32,
        Key::KeyL => 'L' as u32,
        Key::KeyM => 'M' as u32,
        Key::KeyN => 'N' as u32,
        Key::KeyO => 'O' as u32,
        Key::KeyP => 'P' as u32,
        Key::KeyQ => 'Q' as u32,
        Key::KeyR => 'R' as u32,
        Key::KeyS => 'S' as u32,
        Key::KeyT => 'T' as u32,
        Key::KeyU => 'U' as u32,
        Key::KeyV => 'V' as u32,
        Key::KeyW => 'W' as u32,
        Key::KeyX => 'X' as u32,
        Key::KeyY => 'Y' as u32,
        Key::KeyZ => 'Z' as u32,
        Key::Num0 | Key::Kp0 => '0' as u32,
        Key::Num1 | Key::Kp1 => '1' as u32,
        Key::Num2 | Key::Kp2 => '2' as u32,
        Key::Num3 | Key::Kp3 => '3' as u32,
        Key::Num4 | Key::Kp4 => '4' as u32,
        Key::Num5 | Key::Kp5 => '5' as u32,
        Key::Num6 | Key::Kp6 => '6' as u32,
        Key::Num7 | Key::Kp7 => '7' as u32,
        Key::Num8 | Key::Kp8 => '8' as u32,
        Key::Num9 | Key::Kp9 => '9' as u32,
        // Fallback to hash
        Key::Unknown(code) => code,
        _ => 0,
    }
}
