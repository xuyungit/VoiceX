//! Shared audio utilities for ASR clients.

pub fn resample_to_16k(chunk: &[u8], src_rate: u32) -> Vec<u8> {
    if src_rate == 16_000 {
        return chunk.to_vec();
    }

    if chunk.len() < 2 {
        return Vec::new();
    }

    let mut samples: Vec<i16> = Vec::with_capacity(chunk.len() / 2);
    for pair in chunk.chunks_exact(2) {
        samples.push(i16::from_le_bytes([pair[0], pair[1]]));
    }

    let ratio = src_rate as f32 / 16_000f32;
    let target_len = ((samples.len() as f32) / ratio).floor() as usize;
    if target_len == 0 || samples.is_empty() {
        return Vec::new();
    }

    let mut resampled: Vec<i16> = Vec::with_capacity(target_len);
    let last_index = samples.len() - 1;
    for i in 0..target_len {
        let src_pos = i as f32 * ratio;
        let idx = src_pos.floor() as usize;
        let frac = src_pos - idx as f32;
        let s0 = samples[idx];
        let s1 = if idx + 1 <= last_index {
            samples[idx + 1]
        } else {
            samples[last_index]
        };
        let interp = (s0 as f32) * (1.0 - frac) + (s1 as f32) * frac;
        resampled.push(interp.round() as i16);
    }

    let mut bytes = Vec::with_capacity(resampled.len() * 2);
    for s in resampled {
        bytes.extend_from_slice(&s.to_le_bytes());
    }
    bytes
}
