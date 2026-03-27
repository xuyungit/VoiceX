use crate::{
    asr::AsrEvent,
    state::{ProcessingIntent, RecordingStyle},
};

#[derive(Debug, Clone)]
pub enum CancelReason {
    EscapeKey,
    TooShort {
        duration_ms: u64,
        audio_path: Option<String>,
    },
}

#[derive(Debug)]
pub enum SessionMessage {
    HotkeyPressed,
    HotkeyReleased,
    HoldThresholdReached,
    CancelSession(CancelReason),
    HandsFreeCountdownTick(u32),
    HandsFreeTimeout,
    FinalizeHideReady,
    AsrEvent(AsrEvent),
    AsrStreamFinished,
    CorrectingStart,
    CorrectingStop,
    ApplySettings {
        hold_threshold_ms: u32,
        max_recording_minutes: u32,
        text_injection_mode: String,
        input_device_uid: Option<String>,
        remove_trailing_punctuation: bool,
        short_sentence_threshold: u32,
        replacement_rules: Vec<crate::commands::settings::ReplacementRule>,
        translation_enabled: bool,
        translation_trigger_mode: String,
        double_tap_window_ms: u32,
    },
    AsrFinalTimeout,
    AudioStarted {
        sample_rate: u32,
        channels: u16,
        path: Option<String>,
    },
    AudioStopped {
        path: Option<String>,
        refinement_path: Option<String>,
        duration_ms: Option<u64>,
    },
    AudioStartFailed {
        reason: String,
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
    },
}
