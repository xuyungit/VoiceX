//! Gentle synthesized audio cues for dictation readiness.
//!
//! Uses `cpal`'s default output device so the same soft tone works identically
//! on macOS and Windows with no bundled audio asset files. Failures are logged
//! and never prevent microphone capture from proceeding.

use std::f32::consts::TAU;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SampleFormat;

/// Soft peak gain (~ -17 dBFS) so the cue is clearly audible yet unobtrusive.
const CUE_PEAK_GAIN: f32 = 0.13;
/// Total cue length in milliseconds (two soft overlapping warm tones: C5 -> E5).
const CUE_DURATION_MS: u32 = 106;
/// Trailing silence fed to the output callback before marking playback done so
/// the hardware ring buffer finishes cleanly without a click.
const TRAILING_SILENCE_MS: u32 = 24;
/// Extra wall-clock grace after the callback finishes writing samples.
const HARDWARE_DRAIN_MS: u64 = 35;

/// Synthesize a gentle, warm two-note ascending ready cue at `sample_rate`.
///
/// Both ends taper smoothly to zero so playback never clicks.
pub fn render_ready_chime(sample_rate: u32) -> Vec<f32> {
    if sample_rate == 0 {
        return Vec::new();
    }
    let total_samples = ((sample_rate as u64 * CUE_DURATION_MS as u64) / 1000).max(1) as usize;
    let mut samples = vec![0.0f32; total_samples];

    // Note 1: soft C5 (523.25 Hz), 0..56ms
    add_soft_tone(&mut samples, sample_rate, 523.25, 0.0, 0.056, 0.006, 48.0, 0.85, 0.14);
    // Note 2: soft E5 (659.25 Hz), 36..106ms (gentle major-third lift)
    add_soft_tone(&mut samples, sample_rate, 659.25, 0.036, 0.070, 0.006, 42.0, 1.0, 0.10);

    // Normalize softly so overlapping notes never exceed `CUE_PEAK_GAIN`.
    let max_abs = samples
        .iter()
        .fold(0.0f32, |acc, &value| acc.max(value.abs()));
    if max_abs > 1e-6 {
        let scale = CUE_PEAK_GAIN / max_abs;
        for sample in &mut samples {
            *sample *= scale;
        }
    }

    // Guarantee clean zero endpoints.
    if let Some(first) = samples.first_mut() {
        *first = 0.0;
    }
    if let Some(last) = samples.last_mut() {
        *last = 0.0;
    }
    samples
}

#[allow(clippy::too_many_arguments)]
fn add_soft_tone(
    buffer: &mut [f32],
    sample_rate: u32,
    freq_hz: f32,
    start_sec: f32,
    duration_sec: f32,
    attack_sec: f32,
    decay_rate: f32,
    weight: f32,
    second_harmonic: f32,
) {
    let start_idx = (start_sec * sample_rate as f32).round() as usize;
    let len = (duration_sec * sample_rate as f32).round() as usize;
    let end_idx = (start_idx + len).min(buffer.len());
    if end_idx <= start_idx {
        return;
    }

    let fade_out_sec = 0.012f32.min(duration_sec * 0.35);
    for (offset, slot) in buffer[start_idx..end_idx].iter_mut().enumerate() {
        let t = offset as f32 / sample_rate as f32;
        let remaining = (duration_sec - t).max(0.0);

        let attack = if attack_sec > 0.0 && t < attack_sec {
            0.5 - 0.5 * (std::f32::consts::PI * t / attack_sec).cos()
        } else {
            1.0
        };
        let release = if fade_out_sec > 0.0 && remaining < fade_out_sec {
            0.5 - 0.5 * (std::f32::consts::PI * remaining / fade_out_sec).cos()
        } else {
            1.0
        };
        let decay = (-decay_rate * t).exp();
        let envelope = attack * release * decay * weight;

        let fundamental = (TAU * freq_hz * t).sin();
        let harmonic = (TAU * freq_hz * 2.0 * t).sin() * second_harmonic;
        *slot += (fundamental + harmonic) * envelope;
    }
}

/// Play the ready chime on a short-lived background thread, then run
/// `after_played` regardless of whether an output device was available.
pub fn play_ready_chime_async(after_played: impl FnOnce() + Send + 'static) {
    let spawned = thread::Builder::new()
        .name("voicex-audio-cue".into())
        .spawn(move || {
            if let Err(err) = play_ready_chime_blocking() {
                log::debug!("Ready audio cue skipped: {}", err);
            }
            after_played();
        });
    if let Err(err) = spawned {
        log::warn!("Failed to spawn audio cue thread: {}", err);
    }
}

fn play_ready_chime_blocking() -> Result<(), String> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| "no default output device".to_string())?;
    let default_config = device
        .default_output_config()
        .map_err(|e| format!("default output config unavailable: {e}"))?;
    let sample_format = default_config.sample_format();
    let config: cpal::StreamConfig = default_config.into();
    let sample_rate = config.sample_rate.0;
    let channels = config.channels.max(1) as usize;

    let mut samples = render_ready_chime(sample_rate);
    if samples.is_empty() {
        return Ok(());
    }
    let silence_samples = ((sample_rate as u64 * TRAILING_SILENCE_MS as u64) / 1000) as usize;
    samples.resize(samples.len() + silence_samples, 0.0);

    let finished = Arc::new(AtomicBool::new(false));
    let stream = build_output_stream(
        &device,
        &config,
        sample_format,
        channels,
        samples,
        Arc::clone(&finished),
    )?;

    stream
        .play()
        .map_err(|e| format!("failed to start audio cue stream: {e}"))?;

    let deadline = Instant::now() + Duration::from_millis(u64::from(CUE_DURATION_MS) + 220);
    while !finished.load(Ordering::Acquire) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    if finished.load(Ordering::Acquire) {
        thread::sleep(Duration::from_millis(HARDWARE_DRAIN_MS));
    }

    let _ = stream.pause();
    drop(stream);
    Ok(())
}

fn build_output_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    format: SampleFormat,
    channels: usize,
    samples: Vec<f32>,
    finished: Arc<AtomicBool>,
) -> Result<cpal::Stream, String> {
    let mut cursor = 0usize;
    let err_fn = |err| log::debug!("Audio cue output stream error: {}", err);

    match format {
        SampleFormat::F32 => device
            .build_output_stream(
                config,
                move |out: &mut [f32], _| {
                    write_cue_frames(out, channels, &samples, &mut cursor, &finished, |s| s);
                },
                err_fn,
                None,
            )
            .map_err(|e| format!("failed to build f32 cue stream: {e}")),
        SampleFormat::I16 => device
            .build_output_stream(
                config,
                move |out: &mut [i16], _| {
                    write_cue_frames(out, channels, &samples, &mut cursor, &finished, |s| {
                        (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
                    });
                },
                err_fn,
                None,
            )
            .map_err(|e| format!("failed to build i16 cue stream: {e}")),
        SampleFormat::U16 => device
            .build_output_stream(
                config,
                move |out: &mut [u16], _| {
                    write_cue_frames(out, channels, &samples, &mut cursor, &finished, |s| {
                        ((s.clamp(-1.0, 1.0) * 0.5 + 0.5) * u16::MAX as f32) as u16
                    });
                },
                err_fn,
                None,
            )
            .map_err(|e| format!("failed to build u16 cue stream: {e}")),
        other => Err(format!("unsupported output format for cue: {other:?}")),
    }
}

fn write_cue_frames<T: Copy>(
    output: &mut [T],
    channels: usize,
    samples: &[f32],
    cursor: &mut usize,
    finished: &AtomicBool,
    convert: impl Fn(f32) -> T,
) {
    let silence = convert(0.0);
    for frame in output.chunks_mut(channels) {
        let value = if *cursor < samples.len() {
            let sample = samples[*cursor];
            *cursor += 1;
            convert(sample)
        } else {
            silence
        };
        for channel in frame.iter_mut() {
            *channel = value;
        }
    }
    if *cursor >= samples.len() {
        finished.store(true, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_chime_is_soft_finite_and_click_free() {
        for rate in [16_000u32, 44_100, 48_000] {
            let samples = render_ready_chime(rate);
            assert!(!samples.is_empty());
            assert_eq!(samples.first().copied(), Some(0.0));
            assert_eq!(samples.last().copied(), Some(0.0));
            let peak = samples
                .iter()
                .fold(0.0f32, |acc, &v| acc.max(v.abs()));
            assert!(samples.iter().all(|v| v.is_finite()));
            assert!(
                (peak - CUE_PEAK_GAIN).abs() < 1e-4,
                "expected soft peak {CUE_PEAK_GAIN} at {rate}Hz, got {peak}"
            );
        }
    }

    #[test]
    fn write_cue_frames_duplicates_mono_to_stereo_and_marks_finished() {
        let samples = [0.1f32, -0.1, 0.05];
        let mut cursor = 0;
        let finished = AtomicBool::new(false);
        let mut out = [0.0f32; 8]; // 4 stereo frames for 3 mono samples + 1 silence frame

        write_cue_frames(&mut out, 2, &samples, &mut cursor, &finished, |s| s);

        assert_eq!(cursor, 3);
        assert!(finished.load(Ordering::Acquire));
        assert_eq!(out, [0.1, 0.1, -0.1, -0.1, 0.05, 0.05, 0.0, 0.0]);
    }
}
