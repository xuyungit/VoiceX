/// Linear-interpolation resampler from the provider's fixed 24 kHz to the
/// device rate. Stateful across chunks — the last sample of one chunk seeds
/// the interpolation into the next, so chunk boundaries are inaudible.
///
/// This interpolator has no anti-alias filter. Callers should prefer device
/// rates at or above the source rate when negotiating playback.
pub(crate) struct LinearResampler {
    /// Source samples advanced per output sample.
    step: f64,
    /// Position of the next output sample, in source samples, relative to the
    /// start of the current chunk. May be negative fractionally into `prev`.
    pos: f64,
    /// Last sample of the previous chunk, addressed as index -1.
    prev: Option<f32>,
}

impl LinearResampler {
    pub(crate) fn new(src_rate: u32, dst_rate: u32) -> Self {
        Self {
            step: f64::from(src_rate) / f64::from(dst_rate),
            pos: 0.0,
            prev: None,
        }
    }

    /// Whether `process` would just copy its input through.
    pub(crate) fn is_identity(&self) -> bool {
        self.step == 1.0
    }

    pub(crate) fn process(&mut self, input: &[f32], output: &mut Vec<f32>) {
        output.clear();
        if input.is_empty() {
            return;
        }
        if self.is_identity() {
            output.extend_from_slice(input);
            return;
        }

        loop {
            let index = self.pos.floor();
            let frac = (self.pos - index) as f32;
            let index = index as isize;
            let at = |i: isize| -> Option<f32> {
                if i < 0 {
                    self.prev
                } else {
                    input.get(i as usize).copied()
                }
            };
            // Both neighbours must exist; the seam into the next chunk is
            // handled there via `prev`.
            let (Some(a), Some(b)) = (at(index), at(index + 1)) else {
                break;
            };
            output.push(a + (b - a) * frac);
            self.pos += self.step;
        }

        self.pos -= input.len() as f64;
        self.prev = input.last().copied();
    }
}
