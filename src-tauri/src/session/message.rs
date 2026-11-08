use crate::{
    asr::{AsrEvent, AsrFailure},
    state::{ProcessingIntent, RecordingStyle},
};

/// What the injector can tell of typing a final text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InjectionOutcome {
    /// The input calls all went out. Whether the focused control took the
    /// text is not observable, so this is as close to "typed" as it gets.
    Dispatched,
    /// Typing failed, before any input or part-way through: the user does not
    /// have the whole text where they were typing.
    Failed,
    /// Nothing was typed, and nothing went wrong: the injection was cancelled,
    /// or there was no text.
    NotAttempted,
}

impl From<&crate::injector::DispatchOutcome> for InjectionOutcome {
    fn from(outcome: &crate::injector::DispatchOutcome) -> Self {
        use crate::injector::DispatchOutcome;
        match outcome {
            DispatchOutcome::CommandsPosted => Self::Dispatched,
            DispatchOutcome::PartialOrUncertain | DispatchOutcome::Failed { .. } => Self::Failed,
            DispatchOutcome::CancelledBeforeDispatch => Self::NotAttempted,
        }
    }
}

/// Why the microphone could not be opened, as far as the user can act on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioStartFailure {
    /// No input device is connected at all.
    NoInputDevice,
    /// A device exists but could not be opened.
    Other,
}

impl AudioStartFailure {
    /// Stable code read by the HUD (`hud.ts`, error code messages).
    pub fn hud_code(self) -> &'static str {
        match self {
            Self::NoInputDevice => "no_input_device",
            Self::Other => "audio_start_failed",
        }
    }
}

#[derive(Debug, Clone)]
pub enum CancelReason {
    EscapeKey,
}

#[derive(Debug)]
pub enum SessionMessage {
    HotkeyPressed,
    HotkeyReleased,
    HoldThresholdReached,
    CancelSession(CancelReason),
    RecordingCountdownTick(u32),
    RecordingTimeout,
    FinalizeHideReady,
    ErrorDisplayDone,
    AsrEvent(AsrEvent),
    AsrStreamFinished {
        error: Option<AsrFailure>,
    },
    AsrStartupFailed {
        reason: String,
    },
    CorrectingStart,
    CorrectingStop,
    ApplySettings {
        hold_threshold_ms: u32,
        max_recording_minutes: u32,
        text_injection_mode: String,
        text_injection_overrides: Vec<crate::foreground_app::TextInjectionAppOverride>,
        input_device_uid: Option<String>,
        remove_trailing_punctuation: bool,
        short_sentence_threshold: u32,
        replacement_rules: Vec<crate::commands::settings::ReplacementRule>,
        translation_enabled: bool,
        translation_trigger_mode: String,
        double_tap_window_ms: u32,
    },
    AsrFinalTimeout,
    RetryAsrStartup,
    RetryAsrReconnect,
    AudioStarted {
        audio_epoch: u64,
        sample_rate: u32,
        channels: u16,
        path: Option<String>,
    },
    AudioStopped {
        audio_epoch: u64,
        path: Option<String>,
        refinement_path: Option<String>,
        duration_ms: Option<u64>,
    },
    AudioStartFailed {
        audio_epoch: u64,
        kind: AudioStartFailure,
        reason: String,
    },
    AudioStopFailed {
        audio_epoch: u64,
        reason: String,
    },
    BatchAsrDone {
        text: String,
        model_name: Option<String>,
        batch_epoch: u64,
    },
    BatchAsrFailed {
        reason: String,
        batch_epoch: u64,
    },
    AsrRefinementDone {
        text: Option<String>,
        model_name: Option<String>,
        refinement_epoch: u64,
    },
    AsrRefinementFailed {
        reason: String,
        refinement_epoch: u64,
    },
    InjectDone {
        text: String,
        corrected: bool,
        llm_invoked: bool,
        recording_style: Option<RecordingStyle>,
        duration_ms: Option<u64>,
        audio_path: Option<String>,
        original_for_history: Option<String>,
        injection_version: u64,
        intent: ProcessingIntent,
        outcome: InjectionOutcome,
    },
}
