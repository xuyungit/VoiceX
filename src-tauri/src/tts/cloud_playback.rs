//! The audio sink shared by the MP3-streaming cloud backends.
//!
//! One thread per read owns the output device, decodes each piece's byte
//! stream as it arrives, and reports the outcome through the structured log.
//! Pieces arrive as a channel of channels: the producer opens one byte channel
//! per synthesis request and hands the receiving end over before streaming
//! into it, so the sink knows where each piece begins in the sample stream.
//! That boundary is what captions follow (see [`Playback::begin_piece`]); the
//! providers' own timestamp interfaces are not involved.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};

use super::decode::{decode_mp3_stream, ChunkSource, RECV_POLL};
use super::playback::{Playback, PlaybackHandle};
use super::{log_event, split_for_backend, CancelToken, SpeechProgress};

/// One synthesis request's audio bytes, streamed as they arrive. The sender
/// closing it is what tells the sink the piece is complete.
pub type PieceStream = Receiver<Vec<u8>>;

/// Decode and play every piece, on a thread of its own because both block and
/// because the output stream may not cross threads.
///
/// `network_error` lets the producer tell "the provider failed" apart from
/// "the audio ended", which otherwise both look like a closed channel.
#[allow(clippy::too_many_arguments)]
pub fn run_playback(
    pieces: Receiver<PieceStream>,
    sample_rate: u32,
    gain: f32,
    prebuffer: u64,
    token: CancelToken,
    network_error: Arc<Mutex<Option<String>>>,
    handle_slot: Arc<Mutex<Option<PlaybackHandle>>>,
    speaking: Arc<AtomicBool>,
) {
    let playback = match Playback::open(sample_rate, gain, prebuffer) {
        Ok(playback) => playback,
        Err(err) => {
            if token.finish() {
                log_event(
                    "speak_err",
                    &[
                        ("error", err.code().to_string()),
                        ("detail", err.to_string()),
                    ],
                );
            }
            return;
        }
    };

    if let Ok(mut slot) = handle_slot.lock() {
        *slot = Some(playback.handle());
    }

    let mut started = false;
    let mut decoded = Ok(0);
    loop {
        let piece = match pieces.recv_timeout(RECV_POLL) {
            Ok(piece) => piece,
            // The producer is done, whether it ran out of pieces or gave up.
            Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {
                if token.is_cancelled() {
                    break;
                }
                continue;
            }
        };
        // Marked before the piece's first byte is decoded, so the boundary is
        // exactly where the previous piece's samples ended.
        playback.begin_piece();
        decoded = decode_mp3_stream(
            ChunkSource::new(piece, token.clone()),
            sample_rate,
            |samples| {
                if !started {
                    started = true;
                    speaking.store(true, Ordering::SeqCst);
                    log_event("speak_started", &[]);
                }
                playback.push(samples)
            },
        );
        // A piece that failed to decode ends the read; so does a stop, which
        // the producer notices too and answers by closing the channel.
        if decoded.is_err() || token.is_cancelled() {
            break;
        }
    }

    let failure = network_error.lock().ok().and_then(|slot| slot.clone());

    match decoded {
        Ok(_) if failure.is_none() => {
            playback.mark_end_of_stream();
            match playback.wait_until_drained(&token) {
                Ok(true) => {
                    if token.finish() {
                        log_event("speak_finished", &[]);
                    }
                }
                // Cancelled: whoever cancelled owns the session now, but they
                // cannot report this part. `speak_stopped` fires when the stop
                // is accepted; only here is the audio actually finished, which
                // is what the smoke scripts need to assert on.
                Ok(false) => log_event("speak_cancelled", &[]),
                Err(err) => {
                    if token.finish() {
                        log_event(
                            "speak_err",
                            &[
                                ("error", err.code().to_string()),
                                ("detail", err.to_string()),
                            ],
                        );
                    } else {
                        log_event("speak_cancelled", &[]);
                    }
                }
            }
        }
        // A network failure reads as a truncated stream, so report the real
        // cause rather than the decoder's confusion about it. A stop is not a
        // failure, whatever the stream looked like from here: the request it
        // aborted may well have recorded one.
        _ => {
            let detail = failure.unwrap_or_else(|| match &decoded {
                Err(err) => err.to_string(),
                Ok(_) => "the audio stream ended early".to_string(),
            });
            if token.finish() {
                log_event(
                    "speak_err",
                    &[("error", "backend".to_string()), ("detail", detail)],
                );
            } else {
                log_event("speak_cancelled", &[]);
            }
        }
    }
}

/// The piece being heard, for `TtsBackend::progress`: the sink's sample
/// position mapped back to the text the producer split.
///
/// `speaking` gates it because the handle and the pieces outlive the read
/// they belong to — `stop` needs the handle until the next read replaces it —
/// and without the gate the last piece of the previous read would show as a
/// caption while the next one is still being translated.
pub fn progress(
    speaking: &AtomicBool,
    pieces: &Mutex<Vec<String>>,
    playback: &Mutex<Option<PlaybackHandle>>,
) -> Option<SpeechProgress> {
    if !speaking.load(Ordering::SeqCst) {
        return None;
    }
    let index = playback.lock().ok()?.as_ref()?.current_piece()?;
    let pieces = pieces.lock().ok()?;
    SpeechProgress::at(index, &pieces)
}

/// Split a request for the sink and remember the pieces for [`progress`].
pub fn split_pieces(text: &str, limit: usize, pieces: &Mutex<Vec<String>>) -> Vec<String> {
    let split = split_for_backend(text, limit);
    if split.len() > 1 {
        log_event("speak_chunked", &[("pieces", split.len().to_string())]);
    }
    if let Ok(mut slot) = pieces.lock() {
        *slot = split.clone();
    }
    split
}
