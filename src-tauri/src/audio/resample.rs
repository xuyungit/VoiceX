/// Streaming linear interpolation with a sample position shared across chunks.
///
/// This interpolator has no anti-alias filter.
pub(crate) struct LinearResampler {
    src_rate: i64,
    dst_rate: i64,
    /// Position relative to the chunk, in units of 1/dst_rate source samples.
    pos: i64,
    /// Last sample of the previous chunk, addressed as index -1.
    prev: Option<f32>,
}

impl LinearResampler {
    pub(crate) fn new(src_rate: u32, dst_rate: u32) -> Self {
        Self {
            src_rate: i64::from(src_rate),
            dst_rate: i64::from(dst_rate),
            pos: 0,
            prev: None,
        }
    }

    /// Whether `process` would just copy its input through.
    pub(crate) fn is_identity(&self) -> bool {
        self.src_rate == self.dst_rate
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
            let index = self.pos.div_euclid(self.dst_rate);
            let frac = self.pos.rem_euclid(self.dst_rate) as f32 / self.dst_rate as f32;
            let at = |i: i64| -> Option<f32> {
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
            self.pos += self.src_rate;
        }

        self.pos -= input.len() as i64 * self.dst_rate;
        self.prev = input.last().copied();
    }

    /// Emit the remaining samples by extending the last input sample.
    pub(crate) fn finish(&mut self, output: &mut Vec<f32>) {
        output.clear();
        if let Some(last) = self.prev {
            self.process(&[last], output);
            self.prev = None;
        }
    }
}
