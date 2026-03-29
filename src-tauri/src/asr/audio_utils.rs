//! Shared audio utilities for ASR clients.

pub fn resample_to_16k(chunk: &[u8], src_rate: u32) -> Vec<u8> {
    resample_pcm(chunk, src_rate, 16_000)
}

pub fn resample_to_24k(chunk: &[u8], src_rate: u32) -> Vec<u8> {
    resample_pcm(chunk, src_rate, 24_000)
}

pub fn downmix_to_mono(pcm: &[u8], channels: u16) -> Vec<u8> {
    let ch = channels as usize;
    if ch <= 1 {
        return pcm.to_vec();
    }

    let samples: Vec<i16> = pcm
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect();
    let frames = samples.len() / ch;
    let mut out = Vec::with_capacity(frames * 2);
    for f in 0..frames {
        let mut sum: i32 = 0;
        for c in 0..ch {
            sum += samples[f * ch + c] as i32;
        }
        let avg = (sum / ch as i32).clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        out.extend_from_slice(&avg.to_le_bytes());
    }
    out
}

fn resample_pcm(chunk: &[u8], src_rate: u32, dst_rate: u32) -> Vec<u8> {
    if src_rate == dst_rate {
        return chunk.to_vec();
    }

    if chunk.len() < 2 {
        return Vec::new();
    }

    let mut samples: Vec<i16> = Vec::with_capacity(chunk.len() / 2);
    for pair in chunk.chunks_exact(2) {
        samples.push(i16::from_le_bytes([pair[0], pair[1]]));
    }

    let ratio = src_rate as f32 / dst_rate as f32;
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
