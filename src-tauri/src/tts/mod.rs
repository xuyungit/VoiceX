//! Text-to-speech subsystem for the selected-text reading feature.
//!
//! [`TtsBackend`] is the platform- and vendor-neutral control surface. The
//! macOS system voice implements it directly (it speaks without ever producing
//! audio bytes); a cloud backend composes synthesis and playback behind the
//! same trait.
//!
//! On macOS the empty-id "system default" path goes through [`mac_say`] so it
//! can use the Spoken Content / Siri voice; a listed compact voice still goes
//! through [`mac_system`].

pub mod aliyun;
pub mod azure;
pub mod controller;
pub mod decode;
#[cfg(target_os = "macos")]
pub mod mac_say;
#[cfg(target_os = "macos")]
pub mod mac_system;
pub mod mimo;
pub mod playback;
pub mod volcengine;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub use controller::TtsController;

/// Log target for the subsystem's structured events.
///
/// The automation harness under `scripts/tts/` asserts on these lines, so the
/// `event=` vocabulary and field names are part of the test contract: change
/// them and the scripts together.
/// Kept under the crate's own module path so an ordinary `RUST_LOG=voicex_lib`
/// filter still shows them.
pub const LOG_TARGET: &str = "voicex_lib::tts";

/// Emit one structured event line: `event=<name> key=value ...`.
///
/// Values are sanitized to stay whitespace-free so the lines remain trivially
/// splittable. Never pass selected text through here — only lengths, sources
/// and error codes (plan §3.4: no full text in ordinary logs).
pub fn log_event(event: &str, fields: &[(&str, String)]) {
    let mut line = String::from("event=");
    line.push_str(event);
    for (key, value) in fields {
        line.push(' ');
        line.push_str(key);
        line.push('=');
        line.push_str(&sanitize_field(value));
    }
    log::info!(target: LOG_TARGET, "{line}");
}

fn sanitize_field(value: &str) -> String {
    if value.is_empty() {
        return "-".to_string();
    }
    value
        .chars()
        .map(|c| {
            if c.is_whitespace() || c == '=' {
                '_'
            } else {
                c
            }
        })
        .collect()
}

#[derive(Debug, Clone)]
pub struct TtsRequest {
    pub text: String,
    /// Backend-specific voice identifier; `None` uses the engine default.
    pub voice: Option<String>,
    /// Normalized 0.0..=1.0 speaking rate; `None` uses the engine default.
    /// The user-facing scale is 0.5x–2x, converted in the settings UI.
    pub rate: Option<f32>,
    /// Normalized 0.0..=1.0 volume; `None` uses the engine default.
    pub volume: Option<f32>,
    /// Pitch multiplier in the engine's own 0.5..=2.0 scale; `None` uses the
    /// engine default. Unlike rate and volume this one is not normalized —
    /// 1.0 is the neutral value and both ends are meaningful, so squashing it
    /// into 0..1 would only hide where "unchanged" sits.
    pub pitch: Option<f32>,
}

impl TtsRequest {
    /// A request that leaves every voice parameter at the engine default.
    pub fn plain(text: String) -> Self {
        Self {
            text,
            voice: None,
            rate: None,
            volume: None,
            pitch: None,
        }
    }
}

/// Ownership of the read-and-speak session.
///
/// One `AtomicU64` holds the generation that currently owns the session, or 0
/// when nothing does. Superseding is a plain store of a newer generation, which
/// instantly makes every older [`CancelToken`] report cancelled — including one
/// captured by a closure already queued on the main thread, which is the only
/// way to stop work that `run_on_main_thread` will run whether or not the
/// caller is still waiting for it.
#[derive(Clone, Default)]
pub struct SessionSlot {
    owner: Arc<AtomicU64>,
    counter: Arc<AtomicU64>,
}

impl SessionSlot {
    /// Take the session for a new request, cancelling any current owner.
    pub fn claim(&self) -> CancelToken {
        let generation = self.counter.fetch_add(1, Ordering::SeqCst) + 1;
        self.owner.store(generation, Ordering::SeqCst);
        CancelToken {
            owner: self.owner.clone(),
            generation,
        }
    }

    /// Cancel the current owner and leave the session idle.
    pub fn release(&self) {
        self.counter.fetch_add(1, Ordering::SeqCst);
        self.owner.store(0, Ordering::SeqCst);
    }

    pub fn is_active(&self) -> bool {
        self.owner.load(Ordering::SeqCst) != 0
    }

    /// Lock-free view for the keyboard hook, which must not take locks inside
    /// the event tap callback.
    pub fn active_handle(&self) -> Arc<AtomicU64> {
        self.owner.clone()
    }
}

#[derive(Clone)]
pub struct CancelToken {
    owner: Arc<AtomicU64>,
    generation: u64,
}

impl CancelToken {
    pub fn is_cancelled(&self) -> bool {
        self.owner.load(Ordering::SeqCst) != self.generation
    }

    /// Give the session back if this generation still owns it.
    ///
    /// Returns false when a newer request already took over — in which case the
    /// caller must not touch shared state, because it no longer describes this
    /// request.
    pub fn finish(&self) -> bool {
        self.owner
            .compare_exchange(self.generation, 0, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TtsStatus {
    Idle,
    Speaking,
}

#[derive(Debug, Clone)]
pub struct TtsVoice {
    pub id: String,
    pub name: String,
    pub language: String,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum TtsError {
    #[error("No text-to-speech backend is available on this platform")]
    Unsupported,

    #[error("The speech engine did not start within {0} ms")]
    StartTimeout(u64),

    #[error("Speech engine failure: {0}")]
    Backend(String),
}

impl TtsError {
    pub fn code(&self) -> &'static str {
        match self {
            TtsError::Unsupported => "unsupported",
            TtsError::StartTimeout(_) => "start_timeout",
            TtsError::Backend(_) => "backend",
        }
    }
}

/// A cloud response may be attempted three times in total. The short first
/// delay hides a one-off connection reset; the longer second delay gives a
/// briefly overloaded gateway time to recover while keeping the audible gap
/// bounded to 1.3 seconds plus connection establishment.
const CLOUD_RETRY_DELAYS_MS: [u64; 2] = [300, 1_000];

/// Do not let one unreachable host spend the operating system's much longer
/// TCP timeout budget before the retry policy gets a chance to act. This is a
/// connect timeout only: synthesis streams themselves may legitimately run for
/// minutes when a long selection is being read.
const CLOUD_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// Failure metadata shared by the HTTP TTS providers.
///
/// Retrying is safe only before the current piece has emitted audio. Once any
/// bytes have entered the playback pipeline, sending the same piece again
/// would repeat an unknown amount of speech.
#[derive(Debug)]
pub(crate) struct CloudStreamError {
    detail: String,
    retryable: bool,
    audio_emitted: bool,
    reason: &'static str,
}

impl std::fmt::Display for CloudStreamError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl CloudStreamError {
    fn new(
        detail: String,
        retryable: bool,
        audio_emitted: bool,
        reason: &'static str,
    ) -> Self {
        Self {
            detail,
            retryable,
            audio_emitted,
            reason,
        }
    }

    pub(crate) fn request(err: reqwest::Error) -> Self {
        let retryable = err.is_connect() || err.is_timeout();
        Self::new(
            format!("request failed: {err}"),
            retryable,
            false,
            "transport",
        )
    }

    pub(crate) fn stream(err: reqwest::Error, audio_emitted: bool) -> Self {
        // A response-body error is commonly an HTTP/2 reset or a connection
        // closing between headers and the first event. It is transient, but
        // only replayable when no audio from this attempt escaped.
        let retryable = err.is_connect() || err.is_timeout() || err.is_body();
        Self::new(
            format!("stream broke: {err}"),
            retryable,
            audio_emitted,
            "transport",
        )
    }

    pub(crate) fn http(status: reqwest::StatusCode, detail: String) -> Self {
        let retryable = matches!(status.as_u16(), 408 | 425 | 429) || status.is_server_error();
        Self::new(
            format!("HTTP {status}: {detail}"),
            retryable,
            false,
            "http",
        )
    }

    pub(crate) fn response(detail: String, audio_emitted: bool) -> Self {
        // Malformed data and provider-declared failures are not assumed to be
        // transient. Retrying auth, quota, voice-id or protocol problems only
        // delays the useful error and may bill the same request repeatedly.
        Self::new(detail, false, audio_emitted, "response")
    }

    pub(crate) fn retry_delay_ms(&self, retries_done: usize) -> Option<u64> {
        if !self.retryable || self.audio_emitted {
            return None;
        }
        CLOUD_RETRY_DELAYS_MS.get(retries_done).copied()
    }

    pub(crate) fn reason(&self) -> &'static str {
        self.reason
    }

    pub(crate) fn into_detail(self) -> String {
        self.detail
    }
}

pub(crate) fn cloud_http_client() -> Result<reqwest::Client, CloudStreamError> {
    reqwest::Client::builder()
        .connect_timeout(CLOUD_CONNECT_TIMEOUT)
        .build()
        .map_err(|err| {
            CloudStreamError::response(format!("failed to build HTTP client: {err}"), false)
        })
}

pub(crate) fn log_cloud_retry(
    backend: &str,
    retries_done: usize,
    delay_ms: u64,
    reason: &str,
) {
    log_event(
        "speak_retry",
        &[
            ("backend", backend.to_string()),
            // `retries_done == 0` follows failed attempt 1, so the request
            // about to be sent is attempt 2 of 3.
            ("attempt", (retries_done + 2).to_string()),
            ("max_attempts", (CLOUD_RETRY_DELAYS_MS.len() + 1).to_string()),
            ("delay_ms", delay_ms.to_string()),
            ("reason", reason.to_string()),
        ],
    );
}

/// Why speech ended, for the structured log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// User pressed the read hotkey while speech was active.
    Hotkey,
    /// User pressed Escape while speech was active.
    Escape,
    /// Stopped from the app's own UI — today, the settings-page preview.
    /// Distinct from `Hotkey` so the harness assertion on `reason=hotkey`
    /// keeps meaning "the global hotkey did it".
    Ui,
    /// Dictation started and takes priority over reading.
    Dictation,
    /// A newer read request superseded this one.
    Superseded,
}

impl StopReason {
    pub fn as_str(self) -> &'static str {
        match self {
            StopReason::Hotkey => "hotkey",
            StopReason::Escape => "escape",
            StopReason::Ui => "ui",
            StopReason::Dictation => "dictation",
            StopReason::Superseded => "superseded",
        }
    }
}

pub trait TtsBackend: Send + Sync {
    fn name(&self) -> &'static str;
    fn list_voices(&self) -> Result<Vec<TtsVoice>, TtsError>;
    /// Begin speaking.
    ///
    /// Returns once the engine has accepted the request; completion is reported
    /// asynchronously through the structured log. The backend owns `token` for
    /// the rest of the utterance: it must check [`CancelToken::is_cancelled`]
    /// before any irreversible step, and call [`CancelToken::finish`] on every
    /// terminal outcome — completion, failure, or refusal — so the session
    /// cannot be left stuck as active.
    fn start(&self, request: TtsRequest, token: CancelToken) -> Result<(), TtsError>;
    fn stop(&self) -> Result<(), TtsError>;
    fn status(&self) -> TtsStatus;

    /// Recent output level in 0..=1, for the HUD waveform.
    ///
    /// `None` means this backend cannot know: the macOS system voice speaks
    /// straight through the OS and never hands over audio (plan §5.3). The HUD
    /// hides the waveform in that case rather than animating a made-up one.
    fn audio_level(&self) -> Option<f32> {
        None
    }

}

/// Split `text` into pieces of at most `limit` characters, cutting at a
/// sentence boundary where one lands near the edge, or at a clause boundary
/// when a whole window goes by without one.
///
/// Cloud providers cap a single request, not a read: each backend issues one
/// request per piece into the same decode pipeline, so a split point is
/// audible only as the pause a human would take there anyway. The clause
/// fallback exists for text like a patent claim — hundreds of characters of
/// commas before the first full stop — where cutting at a 、 or ， is still a
/// pause a reader would take, unlike the mid-word hard cut it replaces. The
/// hard cut remains only for text with no punctuation at all. Nothing is
/// dropped: the pieces concatenate back to the input.
pub fn split_for_backend(text: &str, limit: usize) -> Vec<String> {
    /// How far back to look for a boundary. Everything here counts
    /// characters, never bytes — a byte-based window silently shrinks to a
    /// third of its intended size on Chinese text.
    const SENTENCE_LOOKBACK: usize = 200;

    fn is_sentence_end(ch: char) -> bool {
        matches!(ch, '。' | '！' | '？' | '.' | '!' | '?' | '\n')
    }
    fn is_clause_end(ch: char) -> bool {
        matches!(ch, '，' | '、' | '；' | '：' | ',' | ';' | ':')
    }

    let chars: Vec<char> = text.chars().collect();
    if limit == 0 || chars.len() <= limit {
        return vec![text.to_string()];
    }

    let mut pieces = Vec::new();
    let mut start = 0;
    while chars.len() - start > limit {
        let window = &chars[start..start + limit];
        // A fixed window, not a proportion of the limit: at 5000 characters a
        // proportional one would happily push hundreds into the next piece
        // just to land on a full stop.
        let lookback = limit.saturating_sub(SENTENCE_LOOKBACK);
        let cut = window[lookback..]
            .iter()
            .rposition(|ch| is_sentence_end(*ch))
            .or_else(|| window[lookback..].iter().rposition(|ch| is_clause_end(*ch)))
            .map(|offset| lookback + offset + 1)
            .unwrap_or(limit);
        pieces.push(window[..cut].iter().collect());
        start += cut;
    }
    pieces.push(chars[start..].iter().collect());
    pieces
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_within_the_limit_stays_one_piece() {
        assert_eq!(split_for_backend("你好，世界。", 100), vec!["你好，世界。"]);
        // Exactly at the limit is still within it.
        assert_eq!(split_for_backend("abcde", 5), vec!["abcde"]);
    }

    #[test]
    fn splits_prefer_a_sentence_boundary_near_the_cut() {
        // Cutting mid-sentence is audible; pausing on a full stop is not.
        let text = "第一句话。第二句话。第三句话在这里继续说下去";
        let pieces = split_for_backend(text, 12);
        assert_eq!(pieces[0], "第一句话。第二句话。");
        assert!(pieces.iter().all(|piece| piece.chars().count() <= 12));
        assert_eq!(pieces.concat(), text, "a split may pause, never drop");
    }

    #[test]
    fn a_long_clause_run_is_cut_at_clause_punctuation_not_mid_word() {
        // Patent-claim text: hundreds of characters of 、 and ， before the
        // first full stop. A hard cut there lands mid-word; a clause cut is a
        // pause a human reader would take anyway.
        let text = "第一部分甲乙丙丁、第二部分戊己庚辛，第三部分壬癸子丑继续延伸";
        let pieces = split_for_backend(text, 12);
        assert_eq!(pieces[0], "第一部分甲乙丙丁、");
        assert!(pieces.iter().all(|piece| piece.chars().count() <= 12));
        assert_eq!(pieces.concat(), text);
    }

    #[test]
    fn a_sentence_end_beats_a_later_clause_end() {
        // The bigger pause wins even when a comma sits closer to the limit.
        let text = "前一句话结束。之后是很长的、带着顿号的从句一直说下去说下去";
        let pieces = split_for_backend(text, 20);
        assert_eq!(pieces[0], "前一句话结束。");
    }

    #[test]
    fn a_wall_of_text_with_no_punctuation_is_cut_at_the_limit() {
        // Hunting further back for a boundary would leave the window nearly
        // empty and multiply the number of billed requests.
        let text = "あ".repeat(50);
        let pieces = split_for_backend(&text, 20);
        let lengths: Vec<usize> = pieces.iter().map(|piece| piece.chars().count()).collect();
        assert_eq!(lengths, vec![20, 20, 10]);
        assert_eq!(pieces.concat(), text);
    }

    #[test]
    fn the_limit_counts_characters_not_bytes() {
        // A multibyte cap measured in bytes would cut a third of the text and,
        // worse, could land inside a character.
        let text = "中".repeat(10);
        let pieces = split_for_backend(&text, 6);
        assert_eq!(pieces[0].chars().count(), 6);
        assert_eq!(pieces.concat(), text);
    }

    #[test]
    fn field_values_stay_single_token() {
        assert_eq!(sanitize_field("Google Chrome"), "Google_Chrome");
        assert_eq!(sanitize_field("a=b"), "a_b");
        assert_eq!(sanitize_field(""), "-");
    }

    #[test]
    fn transient_cloud_failures_get_two_bounded_retries() {
        for status in [
            reqwest::StatusCode::REQUEST_TIMEOUT,
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            reqwest::StatusCode::BAD_GATEWAY,
            reqwest::StatusCode::SERVICE_UNAVAILABLE,
        ] {
            let error = CloudStreamError::http(status, "temporary".to_string());
            assert_eq!(error.retry_delay_ms(0), Some(300), "{status}");
            assert_eq!(error.retry_delay_ms(1), Some(1_000), "{status}");
            assert_eq!(error.retry_delay_ms(2), None, "{status}");
        }
    }

    #[test]
    fn permanent_or_partly_played_failures_are_not_retried() {
        for status in [
            reqwest::StatusCode::BAD_REQUEST,
            reqwest::StatusCode::UNAUTHORIZED,
            reqwest::StatusCode::FORBIDDEN,
        ] {
            let error = CloudStreamError::http(status, "permanent".to_string());
            assert_eq!(error.retry_delay_ms(0), None, "{status}");
        }

        let partly_played = CloudStreamError::new(
            "connection reset after audio".to_string(),
            true,
            true,
            "transport",
        );
        assert_eq!(
            partly_played.retry_delay_ms(0),
            None,
            "replaying a partly heard piece would duplicate speech"
        );

        let malformed = CloudStreamError::response("bad response".to_string(), false);
        assert_eq!(malformed.retry_delay_ms(0), None);
    }
}
