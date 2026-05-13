use std::time::Duration;

const BASE_CORRECTION_TIMEOUT_SECS: u64 = 10;
const BASE_CORRECTION_TEXT_CHARS: usize = 120;
const MAX_CORRECTION_TIMEOUT_SECS: u64 = 60;

pub fn correction_timeout_for_text(text: &str) -> Duration {
    let text_len = text.trim().chars().count();
    let mut timeout_secs = BASE_CORRECTION_TIMEOUT_SECS;
    let mut threshold = BASE_CORRECTION_TEXT_CHARS;

    while text_len > threshold && timeout_secs < MAX_CORRECTION_TIMEOUT_SECS {
        timeout_secs = timeout_secs.saturating_mul(3).div_ceil(2);
        threshold = threshold.saturating_mul(2);
    }

    Duration::from_secs(timeout_secs.min(MAX_CORRECTION_TIMEOUT_SECS))
}

#[cfg(test)]
mod tests {
    use super::correction_timeout_for_text;

    #[test]
    fn keeps_short_text_at_base_timeout() {
        assert_eq!(correction_timeout_for_text("短文本").as_secs(), 10);
        assert_eq!(correction_timeout_for_text(&"a".repeat(120)).as_secs(), 10);
    }

    #[test]
    fn increases_by_half_for_each_length_doubling_step() {
        assert_eq!(correction_timeout_for_text(&"a".repeat(121)).as_secs(), 15);
        assert_eq!(correction_timeout_for_text(&"a".repeat(241)).as_secs(), 23);
        assert_eq!(correction_timeout_for_text(&"a".repeat(481)).as_secs(), 35);
    }

    #[test]
    fn caps_timeout_for_very_long_text() {
        assert_eq!(
            correction_timeout_for_text(&"a".repeat(10_000)).as_secs(),
            60
        );
    }
}
