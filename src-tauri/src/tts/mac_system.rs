//! macOS system voice backend (AVSpeechSynthesizer).
//!
//! Speaks directly through the OS; no audio bytes and no playback layer are
//! involved, which is why [`TtsBackend`] does not force backends to produce
//! audio.
//!
//! Completion comes from `AVSpeechSynthesizerDelegate`, which reports started,
//! finished and cancelled as distinct events. It replaced polling `isSpeaking`,
//! which could not see an utterance that began and ended between two polls: a
//! short read was reported as a start timeout and, worse, kept the session
//! claimed for the full two seconds, so pressing the hotkey again in that window
//! stopped nothing instead of starting the next read.
//!
//! A watchdog still guards the one case the delegate cannot report — the engine
//! never calling back at all — because a session that stays claimed disables the
//! hotkey until the app restarts.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::sync_channel;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, AnyThread, DefinedClass};
use objc2_avf_audio::{
    AVSpeechBoundary, AVSpeechSynthesisVoice, AVSpeechSynthesizer, AVSpeechSynthesizerDelegate,
    AVSpeechUtterance,
};
use objc2_foundation::NSString;
use tauri::AppHandle;

use super::{
    log_event, split_for_backend, CancelToken, SpeechProgress, TtsBackend, TtsError, TtsRequest,
    TtsStatus, TtsVoice,
};

/// How long a main-thread hop may take before we treat the engine as wedged.
const MAIN_THREAD_TIMEOUT_MS: u64 = 2_000;
/// How long the engine has to call back before the watchdog gives up on it.
const START_TIMEOUT_MS: u64 = 2_000;
/// `BackendState::current` while no piece is being spoken.
const NO_PIECE: usize = usize::MAX;

thread_local! {
    /// Owned by the main thread. Every access goes through [`on_main`].
    ///
    /// The delegate lives here too: `AVSpeechSynthesizer` does not retain it,
    /// so dropping ours would leave the synthesizer pointing at freed memory.
    static SYNTHESIZER: RefCell<Option<(Retained<AVSpeechSynthesizer>, Retained<SpeechDelegate>)>> =
        const { RefCell::new(None) };
}

/// The read the synthesizer is currently working on.
///
/// Carried through a shared slot rather than captured per-utterance because the
/// delegate object is created once and reused; each `start` replaces this.
struct Speaking {
    token: CancelToken,
    state: Arc<BackendState>,
    /// Whether the engine reported that it began. Distinguishes "finished
    /// normally" from "never started", which the watchdog needs.
    started: AtomicBool,
    /// Addresses of the read's utterances, one per piece and in speaking
    /// order, used only to tell callbacks apart — never dereferenced, and
    /// `AVSpeechUtterance` is not `Send`, so they cannot be held here anyway.
    /// A callback's position in this list is the piece it is about.
    ///
    /// Needed because starting a read stops the previous one, and the resulting
    /// `didCancel` is delivered *after* the new utterances are registered.
    /// Without matching, that stale callback would clear the new registration
    /// and the session would never be released.
    ///
    /// Comparing addresses is sound here: a registered utterance is retained
    /// by the synthesizer for as long as it can still be the subject of a
    /// callback, so its address cannot be handed to a later allocation while we
    /// still care about it.
    utterances: Vec<usize>,
}

/// Whether the utterance at `index` is the read's last, whose finish is the
/// read's finish. An earlier one finishing only means the engine is moving on
/// to the next piece, which its start callback reports.
fn is_last(speaking: &Speaking, index: usize) -> bool {
    index + 1 == speaking.utterances.len()
}

fn address_of(utterance: &AVSpeechUtterance) -> usize {
    utterance as *const AVSpeechUtterance as usize
}

#[derive(Default)]
struct DelegateIvars {
    current: Mutex<Option<Arc<Speaking>>>,
}

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements.
    // - SpeechDelegate does not implement Drop.
    // Deliberately not `MainThreadOnly`: the delegate protocol requires
    // `Send + Sync`, which a main-thread-only class cannot satisfy. The ivars
    // are `Send + Sync` on their own, and the callbacks only touch atomics,
    // a mutex and the log.
    #[unsafe(super(NSObject))]
    #[name = "VoiceXSpeechDelegate"]
    #[ivars = DelegateIvars]
    struct SpeechDelegate;

    unsafe impl NSObjectProtocol for SpeechDelegate {}

    unsafe impl AVSpeechSynthesizerDelegate for SpeechDelegate {
        #[unsafe(method(speechSynthesizer:didStartSpeechUtterance:))]
        fn did_start(&self, _synthesizer: &AVSpeechSynthesizer, utterance: &AVSpeechUtterance) {
            let Some((speaking, index)) = self.matching(utterance) else {
                return;
            };
            if speaking.token.is_cancelled() {
                return;
            }
            // The engine's own "now speaking this one" is the progress signal
            // captions follow; there is no sample stream to measure here.
            speaking.state.current.store(index, Ordering::SeqCst);
            if !speaking.started.swap(true, Ordering::SeqCst) {
                // Only now is it audible, which is what `status` reports and
                // what the HUD uses to tell waiting apart from speaking.
                speaking.state.speaking.store(true, Ordering::SeqCst);
                log_event("speak_started", &[]);
            }
        }

        #[unsafe(method(speechSynthesizer:didFinishSpeechUtterance:))]
        fn did_finish(&self, _synthesizer: &AVSpeechSynthesizer, utterance: &AVSpeechUtterance) {
            let Some((speaking, index)) = self.matching(utterance) else {
                return;
            };
            // Between pieces the registration stays: the next utterance is
            // already queued and its start callback moves `current` on.
            if !is_last(&speaking, index) {
                return;
            }
            self.take_matching(utterance);
            speaking.state.current.store(NO_PIECE, Ordering::SeqCst);
            if speaking.token.finish() {
                speaking.state.speaking.store(false, Ordering::SeqCst);
                log_event("speak_finished", &[]);
            }
        }

        #[unsafe(method(speechSynthesizer:didCancelSpeechUtterance:))]
        fn did_cancel(&self, _synthesizer: &AVSpeechSynthesizer, utterance: &AVSpeechUtterance) {
            // This used to stay silent, on the grounds that whoever cancelled
            // had already logged why. That reasoning stopped holding when
            // `stop` became non-blocking: `speak_stopped` now marks the request
            // being queued, not the engine going quiet, so without this line
            // nothing at all reports that the audio actually ended — and the
            // smoke scripts have no way to tell a stop that worked from one
            // that was merely accepted. It is not a duplicate of
            // `speak_stopped`; it is the other half of it.
            //
            // Supersession does not reach here: `start` cancels the previous
            // utterance before registering the new one, so the stale callback
            // fails the identity check above and returns (see `Speaking`).
            //
            // Stopping cancels every queued utterance, each with its own
            // callback; the first one takes the registration and the rest
            // find nothing, so the read is reported cancelled once.
            let Some(speaking) = self.take_matching(utterance) else {
                return;
            };
            speaking.state.current.store(NO_PIECE, Ordering::SeqCst);
            speaking.state.speaking.store(false, Ordering::SeqCst);
            log_event("speak_cancelled", &[]);
        }
    }
);

impl SpeechDelegate {
    fn new() -> Retained<Self> {
        let this = Self::alloc().set_ivars(DelegateIvars::default());
        unsafe { msg_send![super(this), init] }
    }

    /// The registration `utterance` belongs to and its piece index, or `None`
    /// when the callback is about a read we have already moved past.
    fn matching(&self, utterance: &AVSpeechUtterance) -> Option<(Arc<Speaking>, usize)> {
        self.address_matches(address_of(utterance))
    }

    fn address_matches(&self, address: usize) -> Option<(Arc<Speaking>, usize)> {
        let speaking = self.ivars().current.lock().ok()?.clone()?;
        let index = speaking.utterances.iter().position(|&a| a == address)?;
        Some((speaking, index))
    }

    fn take_matching(&self, utterance: &AVSpeechUtterance) -> Option<Arc<Speaking>> {
        self.take_address(address_of(utterance))
    }

    /// Remove the registration if `address` is one of its utterances.
    fn take_address(&self, address: usize) -> Option<Arc<Speaking>> {
        let mut slot = self.ivars().current.lock().ok()?;
        if slot
            .as_ref()
            .is_some_and(|s| s.utterances.contains(&address))
        {
            slot.take()
        } else {
            None
        }
    }

    fn set(&self, speaking: Arc<Speaking>) {
        if let Ok(mut slot) = self.ivars().current.lock() {
            *slot = Some(speaking);
        }
    }
}

/// Run `body` on the main thread and wait for its result.
///
/// Must not be called from the main thread itself.
fn on_main<T, F>(app: &AppHandle, body: F) -> Result<T, TtsError>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let (tx, rx) = sync_channel(1);
    app.run_on_main_thread(move || {
        let _ = tx.send(body());
    })
    .map_err(|err| TtsError::Backend(format!("run_on_main_thread failed: {err}")))?;

    rx.recv_timeout(Duration::from_millis(MAIN_THREAD_TIMEOUT_MS))
        .map_err(|_| TtsError::Backend("the main thread did not respond".to_string()))
}

/// Main thread only: hand the shared synthesizer and its delegate to `body`,
/// creating them lazily.
fn with_synthesizer<T>(body: impl FnOnce(&AVSpeechSynthesizer, &SpeechDelegate) -> T) -> T {
    SYNTHESIZER.with(|cell| {
        let mut slot = cell.borrow_mut();
        let (synthesizer, delegate) = slot.get_or_insert_with(|| {
            let synthesizer = unsafe { AVSpeechSynthesizer::new() };
            let delegate = SpeechDelegate::new();
            unsafe {
                synthesizer.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
            }
            (synthesizer, delegate)
        });
        body(synthesizer, delegate)
    })
}

struct BackendState {
    /// Whether audio is actually coming out. Raised by the delegate when the
    /// engine reports it began, not when the utterance was accepted.
    speaking: AtomicBool,
    /// The current request's text as the pieces it was queued in, one
    /// utterance each, which is what `progress` reports on.
    pieces: Mutex<Vec<String>>,
    /// Index into `pieces` of the utterance the engine reported starting
    /// last, or [`NO_PIECE`].
    current: AtomicUsize,
}

impl Default for BackendState {
    fn default() -> Self {
        Self {
            speaking: AtomicBool::new(false),
            pieces: Mutex::new(Vec::new()),
            current: AtomicUsize::new(NO_PIECE),
        }
    }
}

pub struct MacSystemBackend {
    app: AppHandle,
    state: Arc<BackendState>,
}

impl MacSystemBackend {
    pub fn new(app: AppHandle) -> Self {
        Self {
            app,
            state: Arc::new(BackendState::default()),
        }
    }

    /// Release the session if the engine never called back at all.
    ///
    /// The delegate covers every outcome the engine reports; this covers it
    /// reporting nothing, which would otherwise leave the session claimed for
    /// good and make the read hotkey a permanent "stop".
    fn spawn_start_watchdog(&self, speaking: Arc<Speaking>) {
        thread::Builder::new()
            .name("voicex-tts-watchdog".to_string())
            .spawn(move || {
                thread::sleep(Duration::from_millis(START_TIMEOUT_MS));
                if speaking.started.load(Ordering::SeqCst) || speaking.token.is_cancelled() {
                    return;
                }
                if speaking.token.finish() {
                    speaking.state.speaking.store(false, Ordering::SeqCst);
                    log_event(
                        "speak_err",
                        &[(
                            "error",
                            TtsError::StartTimeout(START_TIMEOUT_MS).code().to_string(),
                        )],
                    );
                }
            })
            .expect("failed to spawn the TTS start watchdog");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_delegate_class_registers_and_conforms_to_the_protocol() {
        // `define_class!` registers the Objective-C class the first time this
        // runs, and with debug assertions on it verifies the protocol
        // conformance then — not at compile time. Without this the first
        // failure would be a panic on the user's first read.
        let delegate = SpeechDelegate::new();
        let _: &ProtocolObject<dyn AVSpeechSynthesizerDelegate> =
            ProtocolObject::from_ref(&*delegate);
    }

    #[test]
    fn callbacks_for_a_superseded_utterance_are_ignored() {
        // Starting a read stops the previous one, and that `didCancel` lands
        // after the new utterance is registered. Matching on identity is what
        // stops it from clearing the new registration and stranding the
        // session — the bug that would make the hotkey stop working entirely.
        let delegate = SpeechDelegate::new();
        let slot = crate::tts::SessionSlot::default();
        let state = Arc::new(BackendState::default());

        let current = Arc::new(Speaking {
            token: slot.claim(),
            state: state.clone(),
            started: AtomicBool::new(false),
            utterances: vec![0x1000],
        });
        delegate.set(current);

        // A callback carrying some other utterance's address finds nothing.
        assert!(delegate.address_matches(0x2000).is_none());
        assert!(delegate.take_address(0x2000).is_none());
        // And leaves the registration in place.
        assert!(delegate.address_matches(0x1000).is_some());
        assert!(slot.is_active(), "the live read must still own the session");
    }

    #[test]
    fn each_utterance_maps_to_its_piece_and_only_the_last_ends_the_read() {
        // A read with captions queues one utterance per piece. The engine's
        // callbacks carry the utterance, so the piece index has to come from
        // its position in the registration — and a middle piece finishing
        // must not be mistaken for the read finishing.
        let delegate = SpeechDelegate::new();
        let slot = crate::tts::SessionSlot::default();
        delegate.set(Arc::new(Speaking {
            token: slot.claim(),
            state: Arc::new(BackendState::default()),
            started: AtomicBool::new(false),
            utterances: vec![0x1000, 0x2000, 0x3000],
        }));

        let (speaking, index) = delegate
            .address_matches(0x2000)
            .expect("the middle utterance is registered");
        assert_eq!(index, 1);
        assert!(!is_last(&speaking, index));
        assert!(is_last(&speaking, 2));
        // A cancel for any queued utterance takes the whole read down.
        assert!(delegate.take_address(0x3000).is_some());
        assert!(delegate.address_matches(0x1000).is_none());
    }
}

impl TtsBackend for MacSystemBackend {
    fn name(&self) -> &'static str {
        "mac_system"
    }

    fn list_voices(&self) -> Result<Vec<TtsVoice>, TtsError> {
        on_main(&self.app, || unsafe {
            AVSpeechSynthesisVoice::speechVoices()
                .iter()
                .map(|voice| TtsVoice {
                    id: voice.identifier().to_string(),
                    name: voice.name().to_string(),
                    language: voice.language().to_string(),
                })
                .collect()
        })
    }

    fn start(&self, request: TtsRequest, token: CancelToken) -> Result<(), TtsError> {
        let TtsRequest {
            text,
            voice,
            rate,
            volume,
            pitch,
            piece_limit,
        } = request;

        // One utterance per piece: the engine reports each utterance's start,
        // and that is the progress signal captions follow. Without a limit the
        // whole text is one utterance, as it always was.
        let pieces = match piece_limit {
            Some(limit) => split_for_backend(&text, limit),
            None => vec![text],
        };
        if pieces.len() > 1 {
            log_event("speak_chunked", &[("pieces", pieces.len().to_string())]);
        }
        if let Ok(mut slot) = self.state.pieces.lock() {
            *slot = pieces.clone();
        }
        self.state.current.store(NO_PIECE, Ordering::SeqCst);

        // `run_on_main_thread` cannot be un-queued: if the wait below times out,
        // the closure still runs later. It therefore re-checks the token itself
        // rather than trusting that the caller is still interested — otherwise a
        // timed-out start could begin speaking after the user already cancelled.
        let speak_token = token.clone();
        let state = self.state.clone();
        let queued = on_main(&self.app, move || {
            if speak_token.is_cancelled() {
                return None;
            }

            with_synthesizer(|synthesizer, delegate| unsafe {
                // Replace whatever is queued; a new request cancels the old one.
                synthesizer.stopSpeakingAtBoundary(AVSpeechBoundary::Immediate);

                let voice = voice.as_deref().and_then(|identifier| {
                    let voice = AVSpeechSynthesisVoice::voiceWithIdentifier(&NSString::from_str(
                        identifier,
                    ));
                    if voice.is_none() {
                        log::warn!("Unknown system voice identifier: {identifier}");
                    }
                    voice
                });
                let utterances: Vec<_> = pieces
                    .iter()
                    .map(|piece| {
                        let utterance = AVSpeechUtterance::speechUtteranceWithString(
                            &NSString::from_str(piece),
                        );
                        if let Some(rate) = rate {
                            utterance.setRate(rate.clamp(0.0, 1.0));
                        }
                        if let Some(volume) = volume {
                            utterance.setVolume(volume.clamp(0.0, 1.0));
                        }
                        if let Some(pitch) = pitch {
                            utterance.setPitchMultiplier(pitch.clamp(0.5, 2.0));
                        }
                        if let Some(voice) = voice.as_deref() {
                            utterance.setVoice(Some(voice));
                        }
                        utterance
                    })
                    .collect();

                // Registered only once the utterances exist, so their
                // addresses can identify the callbacks that belong to them,
                // and before any is queued, so no callback can precede it.
                let speaking = Arc::new(Speaking {
                    token: speak_token.clone(),
                    state,
                    started: AtomicBool::new(false),
                    utterances: utterances.iter().map(|u| address_of(u)).collect(),
                });
                delegate.set(speaking.clone());

                // The synthesizer queues them and speaks them back to back.
                for utterance in &utterances {
                    synthesizer.speakUtterance(utterance);
                }
                Some(speaking)
            })
        });

        match queued {
            Ok(Some(speaking)) => {
                self.spawn_start_watchdog(speaking);
                Ok(())
            }
            Ok(None) => {
                // Superseded between the caller's check and the main thread.
                token.finish();
                log_event("speak_discarded", &[("reason", "superseded".to_string())]);
                Ok(())
            }
            Err(err) => {
                // Release the session, which also cancels the queued closure
                // above so it cannot start speaking behind our back.
                token.finish();
                Err(err)
            }
        }
    }

    fn stop(&self) -> Result<(), TtsError> {
        self.state.speaking.store(false, Ordering::SeqCst);
        self.state.current.store(NO_PIECE, Ordering::SeqCst);

        // Queue the engine stop and return. Waiting for the main thread here
        // (as `start` must) would stall the hotkey worker for up to
        // `MAIN_THREAD_TIMEOUT_MS`, and dictation's `Pressed` sits behind this
        // call on that same worker. The closure still runs; we just do not
        // wait for it. `run_on_main_thread` cannot be un-queued, which is
        // fine: stopping a synthesizer that has already gone idle is a no-op.
        self.app
            .run_on_main_thread(|| {
                with_synthesizer(|synthesizer, _| unsafe {
                    synthesizer.stopSpeakingAtBoundary(AVSpeechBoundary::Immediate);
                });
            })
            .map_err(|err| TtsError::Backend(format!("run_on_main_thread failed: {err}")))
    }

    fn status(&self) -> TtsStatus {
        if self.state.speaking.load(Ordering::SeqCst) {
            TtsStatus::Speaking
        } else {
            TtsStatus::Idle
        }
    }

    fn reports_progress(&self) -> bool {
        true
    }

    fn progress(&self) -> Option<SpeechProgress> {
        let index = self.state.current.load(Ordering::SeqCst);
        if index == NO_PIECE {
            return None;
        }
        let pieces = self.state.pieces.lock().ok()?;
        SpeechProgress::at(index, &pieces)
    }
}
