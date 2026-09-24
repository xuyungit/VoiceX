//! Streaming audio output for speech backends that return audio bytes.
//!
//! The macOS system voice speaks through the OS and never hands us samples, so
//! nothing here is involved in that path. Cloud backends do produce bytes, and
//! plan §5.4 measured why they must be played *while* they arrive: a 222-character
//! selection takes 5 s to synthesize in full but only 621 ms to start. Buffering
//! the whole response first would put those five seconds of silence in front of
//! every long read.
//!
//! [`Playback`] owns a `cpal` output stream, which is `!Send`, so one synthesis
//! creates it, feeds it, and drops it all on the same thread. Other threads stop
//! it through the `Send` [`PlaybackHandle`].

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream};

use super::CancelToken;

/// Sample rates the cloud providers can render at, best first.
///
/// We ask the provider for a rate the output device already runs at, which
/// keeps a resampler out of the playback path entirely — one less stage to get
/// subtly wrong. Every provider in plan §5.4 offers all of these.
///
/// Not every provider added since does: `qwen3-tts-flash` renders only
/// 8/16/24/48 kHz, so a backend whose model is fussier passes its own list to
/// [`negotiate_sample_rate_among`] instead of using this one.
const NEGOTIABLE_RATES: [u32; 5] = [48_000, 44_100, 24_000, 22_050, 16_000];

/// How long the device may go without consuming anything while samples are
/// waiting before we call the stream wedged. Generous: a healthy stream
/// consumes every few milliseconds.
const STALL_TIMEOUT: Duration = Duration::from_secs(5);

/// Grace period after the last sample reaches the device, covering the buffer
/// the driver still has in hand. Without it the stream would be dropped
/// mid-word on short utterances.
const DRAIN_TAIL: Duration = Duration::from_millis(180);

const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Weight of the newest measurement; the rest carries over from the last one.
const LEVEL_SMOOTHING: f32 = 0.35;

/// Audio to accumulate before starting, and before resuming after an underrun,
/// when the utterance plays faster than the provider synthesizes it.
///
/// Measured on CosyVoice (2026-08): the service generates a constant characters
/// per second regardless of the requested speech rate, so at 2x the audio drains
/// almost exactly as fast as it arrives. Without hysteresis every network gap
/// becomes its own crackle — machine-gun stutter. With it, a starved stream
/// pauses cleanly and resumes with two seconds in hand.
const PREBUFFER_SECONDS: f32 = 2.0;

/// Speech-rate multiplier above which the hysteresis buffer engages. Below it
/// synthesis outpaces playback comfortably and buffering would only delay the
/// first audible word — which plan §5.4 measured as the feature's whole point.
const PREBUFFER_SPEED_THRESHOLD: f32 = 1.25;

/// The hysteresis buffer for `speed`, in mono samples; 0 disables gating.
///
/// Callers hand this to [`Playback::open`]. It is a function of the speech-rate
/// multiplier, not the provider, so every cloud backend applies one policy.
pub fn prebuffer_samples(speed: f32, sample_rate: u32) -> u64 {
    if speed > PREBUFFER_SPEED_THRESHOLD {
        (PREBUFFER_SECONDS * sample_rate as f32) as u64
    } else {
        0
    }
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum PlaybackError {
    #[error("No audio output device is available")]
    NoDevice,

    #[error("The output device supports none of the sample rates we can request")]
    NoUsableSampleRate,

    #[error("Unsupported output sample format: {0}")]
    UnsupportedFormat(String),

    #[error("Audio output failed: {0}")]
    Device(String),

    #[error("The audio device stopped consuming samples")]
    Stalled,
}

impl PlaybackError {
    pub fn code(&self) -> &'static str {
        match self {
            PlaybackError::NoDevice => "no_output_device",
            PlaybackError::NoUsableSampleRate => "no_usable_sample_rate",
            PlaybackError::UnsupportedFormat(_) => "unsupported_output_format",
            PlaybackError::Device(_) => "output_device_error",
            PlaybackError::Stalled => "output_stalled",
        }
    }
}

/// Pick the sample rate to request from the provider.
///
/// Prefers whatever the device already runs at so nothing has to be resampled.
/// Fails loudly rather than picking a rate the device cannot play — a silent
/// mismatch would come out as chipmunk audio, which is far harder to diagnose
/// than an error code.
pub fn negotiate_sample_rate() -> Result<u32, PlaybackError> {
    negotiate_sample_rate_among(&NEGOTIABLE_RATES)
}

/// Pick a sample rate from `candidates`, best first.
///
/// For backends whose provider renders a narrower set than [`NEGOTIABLE_RATES`].
/// The device's own rate still wins when it is on the list; otherwise the first
/// candidate the device can open is taken and the device is opened at that rate,
/// which cpal handles. Asking for a rate the *provider* does not render is the
/// case this exists to prevent: the audio would come back at some other rate and
/// surface as a `sample_rate_mismatch` decode error rather than as speech.
pub fn negotiate_sample_rate_among(candidates: &[u32]) -> Result<u32, PlaybackError> {
    let device = cpal::default_host()
        .default_output_device()
        .ok_or(PlaybackError::NoDevice)?;

    if let Ok(default) = device.default_output_config() {
        let rate = default.sample_rate().0;
        if candidates.contains(&rate) {
            return Ok(rate);
        }
    }

    let supported: Vec<_> = device
        .supported_output_configs()
        .map_err(|err| PlaybackError::Device(err.to_string()))?
        .collect();

    candidates
        .iter()
        .copied()
        .find(|rate| {
            supported.iter().any(|config| {
                config.min_sample_rate().0 <= *rate && *rate <= config.max_sample_rate().0
            })
        })
        .ok_or(PlaybackError::NoUsableSampleRate)
}

#[derive(Default)]
struct PlaybackShared {
    /// Mono samples handed over by the decoder, waiting for the audio callback
    /// to pick them up.
    staging: Mutex<Vec<f32>>,
    /// Mono samples accepted from the decoder.
    pushed: AtomicU64,
    /// Mono samples actually written to the device.
    played: AtomicU64,
    /// The producer will not push again.
    ended: AtomicBool,
    /// Stop now and discard whatever is still buffered.
    stopped: AtomicBool,
    /// Linear gain, stored as `f32` bits.
    gain: AtomicU32,
    /// Set by cpal's error callback; turns an otherwise silent device failure
    /// into a reported error instead of a wait that never ends.
    failed: AtomicBool,
    /// Samples that must be waiting before a gated stream (re)opens. Zero
    /// disables gating entirely, which is the slow-speech case.
    gate_resume: AtomicU64,
    /// While set, the callback plays silence without consuming, letting the
    /// buffer refill after an underrun instead of crackling chunk by chunk.
    gated: AtomicBool,
    /// Smoothed output level, as `f32` bits, for the HUD waveform. Measured
    /// from the samples the callback is already walking, so it costs nothing
    /// extra and is a real level rather than an animation pretending to be one.
    level: AtomicU32,
    /// Where each piece of the request begins, as a `pushed` count, in the
    /// order the pieces were pushed. Captions follow the piece being heard,
    /// which only the sink can place: it is the one party that knows both
    /// where a piece's samples went and how many the device has consumed.
    piece_starts: Mutex<Vec<u64>>,
}

impl PlaybackShared {
    /// The piece being heard: the last one at least one sample of which has
    /// been written to the device. `None` until the first piece's first
    /// sample has, so a caption never appears ahead of its audio.
    fn current_piece(&self) -> Option<usize> {
        self.position().map(|(piece, _)| piece)
    }

    /// The piece being heard, as for [`current_piece`](Self::current_piece),
    /// and how many of its samples the device has been given.
    fn position(&self) -> Option<(usize, u64)> {
        let played = self.played.load(Ordering::SeqCst);
        let starts = self.piece_starts.lock().ok()?;
        let piece = starts.iter().rposition(|&start| start < played)?;
        Some((piece, played - starts[piece]))
    }

    fn take_staged(&self, local: &mut VecDeque<f32>) {
        // Never block the audio thread. If the decoder happens to hold the lock
        // we play from what we already took rather than inserting a gap; the
        // next callback picks the samples up a few milliseconds later.
        if let Ok(mut staging) = self.staging.try_lock() {
            if !staging.is_empty() {
                local.extend(staging.drain(..));
            }
        }
    }
}

/// Stop control that can cross threads, unlike [`Playback`] itself.
#[derive(Clone)]
pub struct PlaybackHandle {
    shared: Arc<PlaybackShared>,
}

impl PlaybackHandle {
    /// Recent output level in 0..=1, or `None` before anything has played.
    pub fn level(&self) -> Option<f32> {
        let bits = self.shared.level.load(Ordering::Relaxed);
        (bits != 0).then(|| f32::from_bits(bits))
    }

    pub fn stop(&self) {
        self.shared.stopped.store(true, Ordering::SeqCst);
    }

    /// Index of the piece being heard; see [`Playback::begin_piece`].
    pub fn current_piece(&self) -> Option<usize> {
        self.shared.current_piece()
    }

    /// The piece being heard and how far into it the device has played, in
    /// samples: what a caption timed within its piece is looked up by.
    pub fn position(&self) -> Option<(usize, u64)> {
        self.shared.position()
    }

    pub fn set_gain(&self, gain: f32) {
        self.shared
            .gain
            .store(gain.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }
}

pub struct Playback {
    shared: Arc<PlaybackShared>,
    sample_rate: u32,
    /// Kept alive for the utterance; dropping it closes the device.
    _stream: Stream,
}

impl Playback {
    /// Open the default output device at `sample_rate`.
    ///
    /// `prebuffer` is the hysteresis threshold from [`prebuffer_samples`]:
    /// with a non-zero value the stream holds silence until that much audio is
    /// waiting — both at the start and again after any underrun — so a supply
    /// that barely keeps up produces a rare clean pause instead of a crackle
    /// per network chunk. Zero keeps the start-immediately behaviour.
    ///
    /// Must be called from the thread that will feed and drop it.
    pub fn open(sample_rate: u32, gain: f32, prebuffer: u64) -> Result<Self, PlaybackError> {
        let device = cpal::default_host()
            .default_output_device()
            .ok_or(PlaybackError::NoDevice)?;
        let default = device
            .default_output_config()
            .map_err(|err| PlaybackError::Device(err.to_string()))?;

        let channels = default.channels() as usize;
        let config = cpal::StreamConfig {
            channels: default.channels(),
            sample_rate: cpal::SampleRate(sample_rate),
            buffer_size: cpal::BufferSize::Default,
        };

        let shared = Arc::new(PlaybackShared::default());
        shared
            .gain
            .store(gain.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
        shared.gate_resume.store(prebuffer, Ordering::Relaxed);
        // Gated from the first callback, so the opening words also start with
        // a full buffer in hand rather than a syllable followed by a stall.
        shared.gated.store(prebuffer > 0, Ordering::Relaxed);

        let error_shared = shared.clone();
        let on_error = move |err| {
            log::error!("Audio output error: {err}");
            error_shared.failed.store(true, Ordering::SeqCst);
        };

        let stream = match default.sample_format() {
            SampleFormat::F32 => {
                let cb_shared = shared.clone();
                let mut local: VecDeque<f32> = VecDeque::new();
                device.build_output_stream(
                    &config,
                    move |out: &mut [f32], _: &cpal::OutputCallbackInfo| {
                        fill(&cb_shared, &mut local, out, channels, |sample, slot| {
                            *slot = sample;
                        });
                    },
                    on_error,
                    None,
                )
            }
            SampleFormat::I16 => {
                let cb_shared = shared.clone();
                let mut local: VecDeque<f32> = VecDeque::new();
                device.build_output_stream(
                    &config,
                    move |out: &mut [i16], _: &cpal::OutputCallbackInfo| {
                        fill(&cb_shared, &mut local, out, channels, |sample, slot| {
                            *slot = (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
                        });
                    },
                    on_error,
                    None,
                )
            }
            other => return Err(PlaybackError::UnsupportedFormat(format!("{other:?}"))),
        }
        .map_err(|err| PlaybackError::Device(err.to_string()))?;

        stream
            .play()
            .map_err(|err| PlaybackError::Device(err.to_string()))?;

        Ok(Self {
            shared,
            sample_rate,
            _stream: stream,
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn handle(&self) -> PlaybackHandle {
        PlaybackHandle {
            shared: self.shared.clone(),
        }
    }

    /// Hand mono samples to the device. Returns false once playback has been
    /// stopped, so the decoder can give up instead of decoding into a void.
    pub fn push(&self, samples: &[f32]) -> bool {
        if self.shared.stopped.load(Ordering::SeqCst) {
            return false;
        }
        if let Ok(mut staging) = self.shared.staging.lock() {
            staging.extend_from_slice(samples);
            self.shared
                .pushed
                .fetch_add(samples.len() as u64, Ordering::SeqCst);
        }
        true
    }

    /// Mark that the samples pushed from now on belong to the next piece of
    /// the request. Called by the producer before it decodes a piece, so the
    /// recorded start is exactly the previous piece's end.
    pub fn begin_piece(&self) {
        let start = self.shared.pushed.load(Ordering::SeqCst);
        if let Ok(mut starts) = self.shared.piece_starts.lock() {
            starts.push(start);
        }
    }

    pub fn mark_end_of_stream(&self) {
        self.shared.ended.store(true, Ordering::SeqCst);
    }

    /// Block until everything pushed has reached the device.
    ///
    /// Returns `Ok(false)` when the wait ended because the token was cancelled
    /// or playback was stopped — the caller then has nothing to report as a
    /// completion.
    pub fn wait_until_drained(&self, token: &CancelToken) -> Result<bool, PlaybackError> {
        let mut last_played = self.shared.played.load(Ordering::SeqCst);
        let mut last_progress = Instant::now();

        loop {
            if token.is_cancelled() || self.shared.stopped.load(Ordering::SeqCst) {
                return Ok(false);
            }
            if self.shared.failed.load(Ordering::SeqCst) {
                return Err(PlaybackError::Stalled);
            }

            let played = self.shared.played.load(Ordering::SeqCst);
            let pushed = self.shared.pushed.load(Ordering::SeqCst);
            if self.shared.ended.load(Ordering::SeqCst) && played >= pushed {
                // The device still holds a buffer we already wrote into.
                thread::sleep(DRAIN_TAIL);
                return Ok(true);
            }

            if played != last_played {
                last_played = played;
                last_progress = Instant::now();
            } else if self.shared.gated.load(Ordering::Relaxed) {
                // A gated stream is deliberately not consuming while the
                // buffer refills; that is a wait on the network, not a wedged
                // device, however long the provider takes.
                last_progress = Instant::now();
            } else if played < pushed && last_progress.elapsed() > STALL_TIMEOUT {
                // Samples are waiting but the device is not taking them.
                return Err(PlaybackError::Stalled);
            }

            thread::sleep(POLL_INTERVAL);
        }
    }
}

impl Drop for Playback {
    fn drop(&mut self) {
        self.shared.stopped.store(true, Ordering::SeqCst);
    }
}

/// Shared body of the audio callback for every sample format.
///
/// Runs on the realtime audio thread: no allocation beyond the local queue's
/// own growth, no blocking locks, no logging.
fn fill<T, W>(
    shared: &Arc<PlaybackShared>,
    local: &mut VecDeque<f32>,
    out: &mut [T],
    channels: usize,
    write: W,
) where
    T: Copy + Default,
    W: Fn(f32, &mut T),
{
    shared.take_staged(local);

    if shared.stopped.load(Ordering::SeqCst) {
        local.clear();
        out.fill(T::default());
        return;
    }

    let gate_resume = shared.gate_resume.load(Ordering::Relaxed);
    if gate_resume > 0 && shared.gated.load(Ordering::Relaxed) {
        // A finished stream has nothing more to wait for: drain what is left.
        if shared.ended.load(Ordering::SeqCst) || local.len() as u64 >= gate_resume {
            shared.gated.store(false, Ordering::Relaxed);
        } else {
            out.fill(T::default());
            return;
        }
    }

    let gain = f32::from_bits(shared.gain.load(Ordering::Relaxed));
    let mut written = 0u64;
    let mut squares = 0f32;

    for frame in out.chunks_mut(channels.max(1)) {
        match local.pop_front() {
            Some(sample) => {
                let value = sample * gain;
                squares += value * value;
                // Mono from the provider, duplicated across the device's
                // channels — the alternative is silence on one ear.
                for slot in frame.iter_mut() {
                    write(value, slot);
                }
                written += 1;
            }
            None => frame.fill(T::default()),
        }
    }

    shared.played.fetch_add(written, Ordering::SeqCst);

    // Ran dry mid-stream: close the gate so the buffer refills to the
    // threshold before another sample plays. Doing it only on a true underrun
    // keeps a stream that ends exactly on a callback boundary gate-free.
    if gate_resume > 0
        && local.is_empty()
        && (written as usize) < out.len() / channels.max(1)
        && !shared.ended.load(Ordering::SeqCst)
    {
        shared.gated.store(true, Ordering::Relaxed);
    }

    if written > 0 {
        // RMS of the normalized waveform, which is exactly what the microphone
        // path reports (`audio/capture.rs`) — one meaning for one event, so the
        // HUD is not decoding two different units from the same field. No gain
        // is applied here: shaping the value for display is the HUD's job, and
        // doing it twice is how the bars ended up pinned at full deflection.
        // Smoothed against the previous reading so the waveform does not strobe.
        let rms = (squares / written as f32).sqrt().clamp(0.0, 1.0);
        let previous = f32::from_bits(shared.level.load(Ordering::Relaxed));
        let smoothed = previous * (1.0 - LEVEL_SMOOTHING) + rms * LEVEL_SMOOTHING;
        // Never store exactly zero: that is the "nothing has played" sentinel.
        shared
            .level
            .store(smoothed.max(f32::MIN_POSITIVE).to_bits(), Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_current_piece_is_the_last_one_the_device_has_started_on() {
        // Strictly "started on", not "reached": with the second piece
        // starting at sample 4, having played exactly 4 samples means the
        // first piece just ended and nothing of the second has been heard,
        // so its caption would be early by one callback.
        let shared = PlaybackShared::default();
        for start in [0, 4] {
            shared.piece_starts.lock().unwrap().push(start);
        }
        for (played, expected) in [(0, None), (1, Some(0)), (4, Some(0)), (5, Some(1))] {
            shared.played.store(played, Ordering::SeqCst);
            assert_eq!(shared.current_piece(), expected, "played={played}");
        }
        // And how far into that piece: one sample past the second's start.
        assert_eq!(shared.position(), Some((1, 1)));
    }

    fn drain_into<T: Copy + Default + std::fmt::Debug>(
        shared: &Arc<PlaybackShared>,
        local: &mut VecDeque<f32>,
        frames: usize,
        channels: usize,
    ) -> Vec<f32> {
        let mut out = vec![0.0f32; frames * channels];
        fill(shared, local, &mut out, channels, |sample, slot| {
            *slot = sample;
        });
        out
    }

    #[test]
    fn mono_samples_are_duplicated_across_device_channels() {
        // A stereo device fed a mono stream must hear it on both ears.
        let shared = Arc::new(PlaybackShared::default());
        shared.gain.store(1.0f32.to_bits(), Ordering::Relaxed);
        shared
            .staging
            .lock()
            .unwrap()
            .extend_from_slice(&[0.5, -0.25]);

        let mut local = VecDeque::new();
        let out = drain_into::<f32>(&shared, &mut local, 2, 2);

        assert_eq!(out, vec![0.5, 0.5, -0.25, -0.25]);
        assert_eq!(shared.played.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn an_underrun_writes_silence_rather_than_repeating_the_last_sample() {
        // Asked for more frames than we have. The tail must be silent, and only
        // the frames actually filled may count as played — otherwise the drain
        // check would think the utterance finished early.
        let shared = Arc::new(PlaybackShared::default());
        shared.gain.store(1.0f32.to_bits(), Ordering::Relaxed);
        shared.staging.lock().unwrap().extend_from_slice(&[1.0]);

        let mut local = VecDeque::new();
        let out = drain_into::<f32>(&shared, &mut local, 3, 1);

        assert_eq!(out, vec![1.0, 0.0, 0.0]);
        assert_eq!(shared.played.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn stopping_discards_buffered_audio_instead_of_letting_it_finish() {
        // Pressing the hotkey again must cut the voice off now, not after
        // whatever is already buffered has played out.
        let shared = Arc::new(PlaybackShared::default());
        shared.gain.store(1.0f32.to_bits(), Ordering::Relaxed);
        shared
            .staging
            .lock()
            .unwrap()
            .extend_from_slice(&[1.0, 1.0, 1.0]);
        shared.stopped.store(true, Ordering::SeqCst);

        let mut local = VecDeque::new();
        let out = drain_into::<f32>(&shared, &mut local, 3, 1);

        assert_eq!(out, vec![0.0, 0.0, 0.0]);
        assert!(local.is_empty(), "buffered audio must be dropped, not held");
    }

    #[test]
    fn gain_scales_the_output() {
        let shared = Arc::new(PlaybackShared::default());
        shared.gain.store(0.5f32.to_bits(), Ordering::Relaxed);
        shared
            .staging
            .lock()
            .unwrap()
            .extend_from_slice(&[1.0, -1.0]);

        let mut local = VecDeque::new();
        let out = drain_into::<f32>(&shared, &mut local, 2, 1);

        assert_eq!(out, vec![0.5, -0.5]);
    }

    #[test]
    fn the_level_is_measured_from_real_samples_not_invented() {
        // The HUD waveform is driven by this. "Nothing has played yet" and
        // "played, and it was silent" have to stay distinguishable, because the
        // HUD hides the bars for the first and draws flat ones for the second.
        let shared = Arc::new(PlaybackShared::default());
        shared.gain.store(1.0f32.to_bits(), Ordering::Relaxed);
        let handle = PlaybackHandle {
            shared: shared.clone(),
        };
        assert_eq!(handle.level(), None, "no audio has played yet");

        let mut local = VecDeque::new();
        shared
            .staging
            .lock()
            .unwrap()
            .extend_from_slice(&[0.8, -0.8, 0.8, -0.8]);
        drain_into::<f32>(&shared, &mut local, 4, 1);

        let loud = handle.level().expect("playing must produce a level");
        assert!(loud > 0.0);

        // Silence still counts as having played, so the level exists but drops.
        for _ in 0..12 {
            shared.staging.lock().unwrap().extend_from_slice(&[0.0; 4]);
            drain_into::<f32>(&shared, &mut local, 4, 1);
        }
        let quiet = handle.level().expect("still playing, just quietly");
        assert!(
            quiet < loud,
            "silence must decay the level: {quiet} vs {loud}"
        );
    }

    fn gated_shared(threshold: u64) -> Arc<PlaybackShared> {
        let shared = Arc::new(PlaybackShared::default());
        shared.gain.store(1.0f32.to_bits(), Ordering::Relaxed);
        shared.gate_resume.store(threshold, Ordering::Relaxed);
        shared.gated.store(true, Ordering::Relaxed);
        shared
    }

    fn push(shared: &Arc<PlaybackShared>, samples: &[f32]) {
        shared.staging.lock().unwrap().extend_from_slice(samples);
        shared
            .pushed
            .fetch_add(samples.len() as u64, Ordering::SeqCst);
    }

    #[test]
    fn a_gated_stream_holds_silence_until_the_buffer_reaches_the_threshold() {
        // Fast speech drains faster than the provider synthesizes; opening the
        // stream on the first sample would play a syllable and then stall.
        let shared = gated_shared(4);
        let mut local = VecDeque::new();

        push(&shared, &[0.5, 0.5]);
        let out = drain_into::<f32>(&shared, &mut local, 2, 1);
        assert_eq!(out, vec![0.0, 0.0], "below threshold must stay silent");
        assert_eq!(shared.played.load(Ordering::SeqCst), 0);

        push(&shared, &[0.5, 0.5]);
        let out = drain_into::<f32>(&shared, &mut local, 2, 1);
        assert_eq!(out, vec![0.5, 0.5], "the threshold opens the gate");
    }

    #[test]
    fn an_underrun_recloses_the_gate_until_the_buffer_refills() {
        // The whole point of the hysteresis: one clean pause instead of a
        // crackle for every late network chunk.
        let shared = gated_shared(3);
        let mut local = VecDeque::new();

        push(&shared, &[1.0, 1.0, 1.0]);
        drain_into::<f32>(&shared, &mut local, 4, 1); // drains dry: underrun
        assert!(shared.gated.load(Ordering::Relaxed), "underrun must re-gate");

        push(&shared, &[1.0]);
        let out = drain_into::<f32>(&shared, &mut local, 1, 1);
        assert_eq!(out, vec![0.0], "a partial refill stays gated");

        push(&shared, &[1.0, 1.0]);
        let out = drain_into::<f32>(&shared, &mut local, 3, 1);
        assert_eq!(out, vec![1.0, 1.0, 1.0], "a full refill resumes");
    }

    #[test]
    fn the_end_of_the_stream_opens_the_gate_regardless_of_the_buffer() {
        // A short utterance may never reach the threshold; the tail must play.
        let shared = gated_shared(1_000);
        let mut local = VecDeque::new();

        push(&shared, &[0.7]);
        assert_eq!(
            drain_into::<f32>(&shared, &mut local, 1, 1),
            vec![0.0],
            "still gated while the stream may yet refill"
        );

        shared.ended.store(true, Ordering::SeqCst);
        assert_eq!(
            drain_into::<f32>(&shared, &mut local, 1, 1),
            vec![0.7],
            "nothing more is coming, so what is buffered plays out"
        );
        assert!(
            !shared.gated.load(Ordering::Relaxed),
            "a finished stream must not re-gate on its final underrun"
        );
    }

    #[test]
    fn slow_speech_gets_no_buffer_and_fast_speech_gets_one() {
        // At 1x the provider outpaces playback and the buffer would only delay
        // the first word; above the threshold the deficit is real.
        assert_eq!(prebuffer_samples(1.0, 48_000), 0);
        assert_eq!(prebuffer_samples(1.25, 48_000), 0, "threshold is exclusive");
        assert_eq!(prebuffer_samples(1.5, 48_000), 96_000);
        assert_eq!(prebuffer_samples(2.0, 24_000), 48_000);
    }

    #[test]
    fn negotiable_rates_are_ordered_best_first() {
        // The list is a preference order, not a set: picking 16 kHz when the
        // device can do 48 kHz would throw away quality for nothing.
        let mut sorted = NEGOTIABLE_RATES;
        sorted.sort_unstable_by(|a, b| b.cmp(a));
        assert_eq!(sorted, NEGOTIABLE_RATES);
    }
}
