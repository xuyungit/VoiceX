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
    /// Selected-text reading hotkey. There is still exactly one system-level
    /// listener; every reading action is a branch inside it.
    ReadSelectionPressed,
    /// Translate-and-read hotkey: same selection path, one LLM call before
    /// the engine.
    TranslateSelectionPressed,
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
    translate_match: bool,
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

/// State of a reading binding (read or translate), as the settings page sees
/// it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadSelectionStatus {
    /// A binding is configured at all (the feature is switched on).
    pub bound: bool,
    /// The hook will actually act on it.
    pub enabled: bool,
    /// Bound but suppressed because it duplicates the dictation hotkey.
    pub conflicts_with_dictation: bool,
    /// Bound but suppressed because it duplicates the plain reading hotkey.
    /// Only the translate binding can report this.
    pub conflicts_with_reading: bool,
    pub display: Option<String>,
}

impl ReadSelectionStatus {
    fn unbound() -> Self {
        Self {
            bound: false,
            enabled: false,
            conflicts_with_dictation: false,
            conflicts_with_reading: false,
            display: None,
        }
    }
}

/// One hotkey-triggered reading action: the binding the user asked for, and
/// the lock-free copy the tap callback matches against. The two differ when
/// the binding is refused for colliding with a higher-priority one — the
/// request is kept so the status can explain, the atomics stay disabled so
/// the hook never swallows the key for nothing.
#[derive(Clone)]
struct ActionBinding {
    requested: Arc<Mutex<Option<HotkeyConfiguration>>>,
    key_code: Arc<AtomicU32>,
    modifiers: Arc<AtomicU32>,
    uses_fn: Arc<AtomicBool>,
    enabled: Arc<AtomicBool>,
}

impl ActionBinding {
    fn new() -> Self {
        Self {
            requested: Arc::new(Mutex::new(None)),
            key_code: Arc::new(AtomicU32::new(0)),
            modifiers: Arc::new(AtomicU32::new(0)),
            uses_fn: Arc::new(AtomicBool::new(false)),
            enabled: Arc::new(AtomicBool::new(false)),
        }
    }

    fn requested(&self) -> Option<HotkeyConfiguration> {
        self.requested.lock().ok().and_then(|guard| guard.clone())
    }

    fn set_requested(&self, config: Option<HotkeyConfiguration>) {
        if let Ok(mut guard) = self.requested.lock() {
            *guard = config;
        }
    }

    fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }

    fn activate(&self, cfg: &HotkeyConfiguration) {
        self.key_code.store(cfg.key_code, Ordering::SeqCst);
        self.modifiers.store(cfg.modifiers_bits(), Ordering::SeqCst);
        self.uses_fn.store(cfg.uses_fn, Ordering::SeqCst);
        self.enabled.store(true, Ordering::SeqCst);
    }

    fn deactivate(&self) {
        self.enabled.store(false, Ordering::SeqCst);
    }

    /// Whether `cfg` is the combination this binding currently acts on.
    /// A disabled binding claims nothing, so a refused binding cannot in turn
    /// refuse another.
    fn is_active_binding(&self, cfg: &HotkeyConfiguration) -> bool {
        self.is_enabled()
            && cfg.key_code == self.key_code.load(Ordering::SeqCst)
            && cfg.modifiers_bits() == self.modifiers.load(Ordering::SeqCst)
            && cfg.uses_fn == self.uses_fn.load(Ordering::SeqCst)
    }

    /// Called from the tap callback: no locks, atomics only.
    fn matches(&self, snapshot: &HotkeySnapshot) -> bool {
        snapshot.matches_active(
            self.key_code.load(Ordering::SeqCst),
            self.modifiers.load(Ordering::SeqCst),
            self.uses_fn.load(Ordering::SeqCst),
        )
    }
}

#[derive(Clone)]
pub struct HotkeyManager {
    config: Arc<Mutex<Option<HotkeyConfiguration>>>,
    active_key_code: Arc<AtomicU32>,
    active_modifiers: Arc<AtomicU32>,
    active_uses_fn: Arc<AtomicBool>,
    active_enabled: Arc<AtomicBool>,
    read_selection: ActionBinding,
    translate_selection: ActionBinding,
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
            read_selection: ActionBinding::new(),
            translate_selection: ActionBinding::new(),
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
        let read_selection = self.read_selection.clone();
        let translate_selection = self.translate_selection.clone();
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
            // Latched key of a reading action whose press we ate, so the
            // release is eaten too and key repeat does not re-fire it.
            let read_selection_key: RefCell<Option<Key>> = RefCell::new(None);
            let translate_selection_key: RefCell<Option<Key>> = RefCell::new(None);
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
                        HookEvent::TranslateSelectionPressed => {
                            if let Some(controller) = worker_tts.as_ref() {
                                controller.handle_translate_selection_hotkey();
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
                                 read_match={} read_latched={} translate_match={} suspended={}",
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
                                diag.translate_match,
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

                            let read_selection_match =
                                read_selection.is_enabled() && read_selection.matches(&snapshot);
                            let translate_selection_match = translate_selection.is_enabled()
                                && translate_selection.matches(&snapshot);

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
                                        translate_match: translate_selection_match,
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
                            } else if read_selection_match
                                && suspension_count.load(Ordering::SeqCst) == 0
                            {
                                // Fire once per physical press; key repeat while
                                // held must not re-trigger the action.
                                if read_selection_key.borrow().is_none() {
                                    *read_selection_key.borrow_mut() = Some(key);
                                    let _ = hook_tx.send(HookEvent::ReadSelectionPressed);
                                }
                                suppress = true;
                            } else if translate_selection_match
                                && suspension_count.load(Ordering::SeqCst) == 0
                            {
                                if translate_selection_key.borrow().is_none() {
                                    *translate_selection_key.borrow_mut() = Some(key);
                                    let _ = hook_tx.send(HookEvent::TranslateSelectionPressed);
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
                        for latch in [&read_selection_key, &translate_selection_key] {
                            let latched = *latch.borrow();
                            if latched == Some(key) {
                                *latch.borrow_mut() = None;
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
        // bindings against it rather than only at registration time.
        self.refresh_reading_bindings();
    }

    /// Get current configuration.
    pub fn current_config(&self) -> Option<HotkeyConfiguration> {
        self.config.lock().ok().and_then(|c| c.clone())
    }

    /// Set the selected-text reading binding. `None` unbinds it, which is how
    /// the master switch in the reading settings turns the feature off.
    pub fn set_read_selection_config(&self, config: Option<HotkeyConfiguration>) {
        self.read_selection.set_requested(config);
        self.refresh_reading_bindings();
    }

    /// Set the translate-and-read binding. Same contract as the reading one.
    pub fn set_translate_selection_config(&self, config: Option<HotkeyConfiguration>) {
        self.translate_selection.set_requested(config);
        self.refresh_reading_bindings();
    }

    /// What actually happened to the reading binding, for the settings page.
    ///
    /// The binding can be off for two very different reasons — the user turned
    /// the feature off, or it collides with the dictation key — and only the
    /// second one needs explaining in the UI. Recomputed rather than cached so
    /// it stays right after the dictation key changes from the other page.
    pub fn read_selection_status(&self) -> ReadSelectionStatus {
        let Some(config) = self.read_selection.requested() else {
            return ReadSelectionStatus::unbound();
        };
        ReadSelectionStatus {
            bound: true,
            enabled: self.read_selection.is_enabled(),
            conflicts_with_dictation: self.conflicts_with_dictation(&config),
            conflicts_with_reading: false,
            display: Some(config.display_string()),
        }
    }

    /// Same for the translate binding, which can also lose to plain reading.
    pub fn translate_selection_status(&self) -> ReadSelectionStatus {
        let Some(config) = self.translate_selection.requested() else {
            return ReadSelectionStatus::unbound();
        };
        let conflicts_with_dictation = self.conflicts_with_dictation(&config);
        ReadSelectionStatus {
            bound: true,
            enabled: self.translate_selection.is_enabled(),
            conflicts_with_dictation,
            // Reported only when dictation is not already the reason, so the
            // page shows one explanation rather than two for the same key.
            conflicts_with_reading: !conflicts_with_dictation
                && self.read_selection.is_active_binding(&config),
            display: Some(config.display_string()),
        }
    }

    /// Re-apply both reading bindings in priority order, disabling each one
    /// that collides with a binding checked before it: dictation, then read,
    /// then translate — the same order the hook tests them in.
    ///
    /// An identical binding would make the later action unreachable with no
    /// sign of why. Refusing it loudly beats swallowing the key and doing
    /// nothing.
    fn refresh_reading_bindings(&self) {
        match self.read_selection.requested() {
            Some(cfg) if self.conflicts_with_dictation(&cfg) => {
                log::warn!(
                    "Selected-text reading hotkey ({}) is also the dictation hotkey; \
                     reading is disabled until one of them changes",
                    cfg.display_string()
                );
                self.read_selection.deactivate();
            }
            Some(cfg) => self.read_selection.activate(&cfg),
            None => self.read_selection.deactivate(),
        }

        match self.translate_selection.requested() {
            Some(cfg) if self.conflicts_with_dictation(&cfg) => {
                log::warn!(
                    "Translate-and-read hotkey ({}) is also the dictation hotkey; \
                     translate-and-read is disabled until one of them changes",
                    cfg.display_string()
                );
                self.translate_selection.deactivate();
            }
            Some(cfg) if self.read_selection.is_active_binding(&cfg) => {
                log::warn!(
                    "Translate-and-read hotkey ({}) is also the reading hotkey; \
                     translate-and-read is disabled until one of them changes",
                    cfg.display_string()
                );
                self.translate_selection.deactivate();
            }
            Some(cfg) => self.translate_selection.activate(&cfg),
            None => self.translate_selection.deactivate(),
        }
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
        manager.read_selection.is_enabled()
    }

    fn translate_selection_enabled(manager: &HotkeyManager) -> bool {
        manager.translate_selection.is_enabled()
    }

    #[test]
    fn distinct_bindings_both_stay_enabled() {
        let manager = HotkeyManager::new();
        manager.set_config(Some(HotkeyConfiguration::default_primary()));
        manager.set_read_selection_config(Some(HotkeyConfiguration::default_read_selection()));

        assert!(read_selection_enabled(&manager));
        assert_eq!(
            manager.read_selection.key_code.load(Ordering::SeqCst),
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
        assert!(!clash.conflicts_with_reading, "reading never conflicts with itself");
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

    // --- translate-and-read: third binding, lowest priority ---

    fn manager_with_all_three_defaults() -> HotkeyManager {
        let manager = HotkeyManager::new();
        manager.set_config(Some(HotkeyConfiguration::default_primary()));
        manager.set_read_selection_config(Some(HotkeyConfiguration::default_read_selection()));
        manager.set_translate_selection_config(Some(
            HotkeyConfiguration::default_translate_selection(),
        ));
        manager
    }

    #[test]
    fn the_three_default_bindings_are_all_live() {
        let manager = manager_with_all_three_defaults();
        assert!(read_selection_enabled(&manager));
        assert!(translate_selection_enabled(&manager));
        assert_eq!(
            manager.translate_selection.key_code.load(Ordering::SeqCst),
            'T' as u32
        );
        let status = manager.translate_selection_status();
        assert!(status.bound && status.enabled);
        assert!(!status.conflicts_with_dictation && !status.conflicts_with_reading);
        assert_eq!(status.display.as_deref(), Some("Option + Command + T"));
    }

    #[test]
    fn translate_identical_to_reading_is_refused_and_says_why() {
        let manager = manager_with_all_three_defaults();
        manager.set_translate_selection_config(Some(HotkeyConfiguration::default_read_selection()));

        assert!(read_selection_enabled(&manager), "reading keeps its key");
        assert!(!translate_selection_enabled(&manager));
        let status = manager.translate_selection_status();
        assert!(status.bound && !status.enabled);
        assert!(status.conflicts_with_reading);
        assert!(!status.conflicts_with_dictation);
    }

    #[test]
    fn translate_identical_to_dictation_reports_dictation_only() {
        let manager = manager_with_all_three_defaults();
        manager.set_translate_selection_config(Some(HotkeyConfiguration::default_primary()));

        assert!(!translate_selection_enabled(&manager));
        let status = manager.translate_selection_status();
        assert!(status.conflicts_with_dictation);
        assert!(
            !status.conflicts_with_reading,
            "one explanation per key, and dictation is the higher-priority one"
        );
    }

    #[test]
    fn moving_reading_onto_the_translate_key_demotes_translate_not_reading() {
        // Read outranks translate, so it is translate that goes dark.
        let manager = manager_with_all_three_defaults();
        manager.set_read_selection_config(Some(HotkeyConfiguration::default_translate_selection()));

        assert!(read_selection_enabled(&manager));
        assert!(!translate_selection_enabled(&manager));
        assert!(manager.translate_selection_status().conflicts_with_reading);

        manager.set_read_selection_config(Some(HotkeyConfiguration::default_read_selection()));
        assert!(translate_selection_enabled(&manager), "re-enabled once reading moves away");
        assert!(!manager.translate_selection_status().conflicts_with_reading);
    }

    #[test]
    fn a_refused_reading_binding_does_not_refuse_translate() {
        // Reading is dead because it equals dictation; translate on the same
        // key is refused for the dictation reason, but translate on reading's
        // *default* key must not be refused by a binding that is not live.
        let manager = HotkeyManager::new();
        manager.set_config(Some(HotkeyConfiguration::default_read_selection()));
        manager.set_read_selection_config(Some(HotkeyConfiguration::default_read_selection()));
        assert!(!read_selection_enabled(&manager));

        manager.set_translate_selection_config(Some(
            HotkeyConfiguration::default_translate_selection(),
        ));
        assert!(translate_selection_enabled(&manager));

        manager.set_translate_selection_config(Some(HotkeyConfiguration::default_read_selection()));
        let status = manager.translate_selection_status();
        assert!(!status.enabled);
        assert!(status.conflicts_with_dictation);
        assert!(!status.conflicts_with_reading);
    }

    #[test]
    fn switching_reading_off_frees_its_key_for_translate() {
        let manager = manager_with_all_three_defaults();
        manager.set_translate_selection_config(Some(HotkeyConfiguration::default_read_selection()));
        assert!(!translate_selection_enabled(&manager));

        manager.set_read_selection_config(None);
        assert!(translate_selection_enabled(&manager));
        assert!(!manager.translate_selection_status().conflicts_with_reading);
    }

    #[test]
    fn clearing_the_translate_binding_disables_it_and_reports_unbound() {
        let manager = manager_with_all_three_defaults();
        manager.set_translate_selection_config(None);
        assert!(!translate_selection_enabled(&manager));
        let status = manager.translate_selection_status();
        assert!(!status.bound && !status.enabled && status.display.is_none());
        assert!(read_selection_enabled(&manager), "reading is untouched");
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
