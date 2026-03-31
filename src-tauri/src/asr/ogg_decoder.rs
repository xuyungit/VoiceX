//! OGG/Opus file decoder — decodes OGG/Opus audio files to PCM 16-bit LE 16kHz mono.

use std::convert::TryInto;
use std::fs::File;
use std::path::Path;

use audiopus::{
    coder::Decoder as OpusDecoder, packet::Packet as OpusPacket, Channels, MutSignals,
    SampleRate as OpusSampleRate,
};
use ogg::reading::PacketReader;

use super::audio_utils::resample_to_16k;

/// Maximum Opus frame size in samples per channel at 48kHz (120ms).
const MAX_FRAME_SAMPLES: usize = 5760;

/// Decode an OGG/Opus file to 16-bit LE PCM at 16kHz mono.
///
/// Reads the OpusHead header to determine the encoded sample rate and channel
/// count, then decodes all audio packets using `audiopus::coder::Decoder`.
pub fn decode_ogg_opus_to_pcm16k(path: &Path) -> Result<Vec<u8>, String> {
    let file = File::open(path)
        .map_err(|e| format!("Failed to open audio file {}: {}", path.display(), e))?;
    let mut reader = PacketReader::new(file);

    // --- Packet 0: OpusHead ---
    let head_packet = reader
        .read_packet()
        .map_err(|e| format!("Failed to read OGG OpusHead packet: {}", e))?
        .ok_or("OGG file contains no packets")?;

    let head = &head_packet.data;
    if head.len() < 19 || &head[..8] != b"OpusHead" {
        return Err("Invalid OpusHead: missing magic bytes or too short".into());
    }

    let channel_count = head[9] as u16;
    let pre_skip = u16::from_le_bytes([head[10], head[11]]) as usize;
    let input_sample_rate = u32::from_le_bytes([head[12], head[13], head[14], head[15]]);

    log::info!(
        "OGG decode: channels={}, pre_skip={}, input_sample_rate={}",
        channel_count,
        pre_skip,
        input_sample_rate,
    );

    // Map the stored input sample rate to the OpusSampleRate enum used by the
    // encoder.  Our encoder (capture.rs) picks the OpusSampleRate matching the
    // capture rate, so we mirror that mapping here for the decoder.
    let opus_rate = match input_sample_rate {
        8000 => OpusSampleRate::Hz8000,
        12000 => OpusSampleRate::Hz12000,
        16000 => OpusSampleRate::Hz16000,
        24000 => OpusSampleRate::Hz24000,
        _ => OpusSampleRate::Hz48000,
    };
    let decode_rate = opus_rate as u32;

    let channels = match channel_count {
        1 => Channels::Mono,
        2 => Channels::Stereo,
        _ => return Err(format!("Unsupported channel count: {}", channel_count)),
    };

    let mut decoder = OpusDecoder::new(opus_rate, channels)
        .map_err(|e| format!("Failed to create Opus decoder: {}", e))?;

    // --- Packet 1: OpusTags (skip) ---
    let _tags = reader
        .read_packet()
        .map_err(|e| format!("Failed to read OGG OpusTags packet: {}", e))?;

    // --- Audio packets ---
    let mut all_samples: Vec<i16> = Vec::new();
    let mut decode_buf = vec![0i16; MAX_FRAME_SAMPLES * channel_count as usize];

    loop {
        let packet = match reader.read_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            Err(e) => return Err(format!("Failed to read OGG audio packet: {}", e)),
        };

        // Skip empty packets (padding, end-of-stream markers)
        if packet.data.is_empty() {
            continue;
        }

        let input: OpusPacket<'_> = (&packet.data[..])
            .try_into()
            .map_err(|e| format!("Invalid Opus packet: {}", e))?;
        let output: MutSignals<'_, i16> = (&mut decode_buf[..])
            .try_into()
            .map_err(|e| format!("Decode buffer error: {}", e))?;

        let decoded_samples = decoder
            .decode(Some(input), output, false)
            .map_err(|e| format!("Opus decode error: {}", e))?;

        // decoded_samples is per-channel count
        let total = decoded_samples * channel_count as usize;
        all_samples.extend_from_slice(&decode_buf[..total]);
    }

    // Skip pre-skip samples (pre_skip is in units of the decode sample rate)
    let skip_samples = pre_skip * channel_count as usize;
    if skip_samples < all_samples.len() {
        all_samples = all_samples[skip_samples..].to_vec();
    }

    // Downmix stereo to mono if needed
    if channel_count == 2 {
        let mono: Vec<i16> = all_samples
            .chunks_exact(2)
            .map(|pair| ((pair[0] as i32 + pair[1] as i32) / 2) as i16)
            .collect();
        all_samples = mono;
    }

    // Convert i16 samples to bytes
    let mut pcm_bytes: Vec<u8> = Vec::with_capacity(all_samples.len() * 2);
    for sample in &all_samples {
        pcm_bytes.extend_from_slice(&sample.to_le_bytes());
    }

    // Resample to 16kHz if the decode rate differs
    if decode_rate != 16000 {
        pcm_bytes = resample_to_16k(&pcm_bytes, decode_rate);
    }

    log::info!(
        "OGG decode complete: {} PCM bytes at 16kHz mono ({:.1}s)",
        pcm_bytes.len(),
        pcm_bytes.len() as f64 / (16000.0 * 2.0),
    );

    Ok(pcm_bytes)
}
