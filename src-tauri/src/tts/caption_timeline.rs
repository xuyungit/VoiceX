//! Captions placed by the service's own clock rather than by request seams.
//!
//! Most cloud backends caption per request: each request is a caption-sized
//! piece, and the sink knows where each one begins in the sample stream (see
//! [`cloud_playback`](super::cloud_playback)). A service that reports when it
//! speaks each character lets a read go out as one request instead — the
//! seams are then only the service's own chunk boundaries, and the client can
//! no longer cut anywhere it should not. The backend feeds this timeline as
//! the timing arrives; `progress` looks the caption up by how far into its
//! request the device has played.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use super::playback::PlaybackHandle;
use super::SpeechProgress;

struct TimedCaption {
    /// Milliseconds from the first sample of the caption's request.
    start_ms: u64,
    text: String,
}

/// Every caption of one read, per request, in speaking order.
pub struct CaptionTimeline {
    sample_rate: u32,
    requests: Vec<Vec<TimedCaption>>,
}

impl CaptionTimeline {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate,
            requests: Vec::new(),
        }
    }

    /// Start (or restart) placing captions for request `request`. A retried
    /// request sends its chunks again from the first, so whatever the failed
    /// attempt placed goes.
    pub fn begin_request(&mut self, request: usize) {
        if self.requests.len() <= request {
            self.requests.resize_with(request + 1, Vec::new);
        }
        self.requests[request].clear();
    }

    /// Place a caption. Starts never go backwards, so the lookup can take the
    /// last caption that has started.
    pub fn push(&mut self, request: usize, start_ms: u64, text: String) {
        let Some(captions) = self.requests.get_mut(request) else {
            return;
        };
        let start_ms = captions
            .last()
            .map_or(start_ms, |last| start_ms.max(last.start_ms));
        captions.push(TimedCaption { start_ms, text });
    }

    /// The caption being heard once `samples` of request `request` have
    /// played. `total` is `None`: how many captions a read has is only known
    /// once the service has sent its last chunk.
    pub fn at(&self, request: usize, samples: u64) -> Option<SpeechProgress> {
        let captions = self.requests.get(request)?;
        let ms = samples * 1000 / u64::from(self.sample_rate.max(1));
        let position = captions.iter().rposition(|c| c.start_ms <= ms)?;
        let before: usize = self.requests[..request].iter().map(Vec::len).sum();
        Some(SpeechProgress {
            index: before + position,
            total: None,
            text: captions[position].text.clone(),
        })
    }
}

/// The caption being heard, for `TtsBackend::progress`. Gated on `speaking`
/// for the reason [`cloud_playback::progress`](super::cloud_playback::progress)
/// is: the handle outlives the read it belongs to.
pub fn progress(
    speaking: &AtomicBool,
    timeline: &CaptionTimeline,
    playback: &Mutex<Option<PlaybackHandle>>,
) -> Option<SpeechProgress> {
    if !speaking.load(Ordering::SeqCst) {
        return None;
    }
    let (request, samples) = playback.lock().ok()?.as_ref()?.position()?;
    timeline.at(request, samples)
}

/// When each cut in `text` is spoken, given the service's timing of what it
/// actually said: `spoken` is its characters in order, each with the start of
/// the word it belongs to.
///
/// The service times the text it speaks, not the text it was sent — digits
/// come back as 五点六六, `%` as 百分之, letters lower-cased — so positions do
/// not carry over. The characters the two share, which is nearly all of the
/// Chinese and the punctuation, are aligned instead (longest common
/// subsequence), and a cut takes the time of the character just before it.
/// That is usually the full stop closing the previous caption, which the
/// service attaches to the character after it: its time is the new caption's
/// first sound, even when that sound is a number no character matches.
/// `None` for a cut with nothing recognizable at or after it.
pub fn cut_times(text: &[char], cuts: &[usize], spoken: &[(char, u64)]) -> Vec<Option<u64>> {
    fn fold(ch: char) -> char {
        ch.to_ascii_lowercase()
    }
    let a: Vec<(usize, char)> = text
        .iter()
        .enumerate()
        .filter(|(_, ch)| !ch.is_whitespace())
        .map(|(index, &ch)| (index, fold(ch)))
        .collect();
    let b: Vec<(char, u64)> = spoken
        .iter()
        .filter(|(ch, _)| !ch.is_whitespace())
        .map(|&(ch, ms)| (fold(ch), ms))
        .collect();

    // lengths[i][j]: common subsequence of a[i..] and b[j..].
    let width = b.len() + 1;
    let mut lengths = vec![0u32; (a.len() + 1) * width];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lengths[i * width + j] = if a[i].1 == b[j].0 {
                lengths[(i + 1) * width + j + 1] + 1
            } else {
                lengths[(i + 1) * width + j].max(lengths[i * width + j + 1])
            };
        }
    }
    // Walked forward, so the pairs come out in text order.
    let mut matched: Vec<(usize, u64)> = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i].1 == b[j].0 {
            matched.push((a[i].0, b[j].1));
            i += 1;
            j += 1;
        } else if lengths[(i + 1) * width + j] >= lengths[i * width + j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }

    cuts.iter()
        .map(|&cut| {
            matched
                .iter()
                .find(|(index, _)| index + 1 >= cut)
                .map(|&(_, ms)| ms)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spoken characters as the service reports them: per word, punctuation
    /// riding on the character after it.
    fn words(list: &[(&str, u64)]) -> Vec<(char, u64)> {
        list.iter()
            .flat_map(|(text, ms)| text.chars().map(move |ch| (ch, *ms)))
            .collect()
    }

    #[test]
    fn a_cut_is_timed_through_what_the_service_rewrote() {
        let text: Vec<char> = "增加到10.05千牛。3 组对比。".chars().collect();
        let spoken = words(&[
            ("增", 0),
            ("加", 100),
            ("到", 200),
            ("十", 300),
            ("点", 400),
            ("零", 500),
            ("五", 600),
            ("千", 700),
            ("牛", 800),
            ("。三", 1_200),
            ("组", 1_400),
            ("对", 1_500),
            ("比", 1_600),
            ("。", 1_700),
        ]);
        // The second caption opens on a digit no spoken character matches;
        // the full stop before it carries the time of its first sound.
        let cut = text.iter().position(|&ch| ch == '3').unwrap();
        assert_eq!(cut_times(&text, &[cut], &spoken), vec![Some(1_200)]);
    }

    #[test]
    fn letters_align_whatever_their_case_and_spacing() {
        let text: Vec<char> = "用 HUD 显示。CAD 模型".chars().collect();
        let spoken = words(&[
            ("用", 0),
            ("h", 100),
            ("u", 150),
            ("d", 200),
            ("显", 300),
            ("示", 400),
            ("。C", 900),
            (" A", 1_000),
            (" D", 1_100),
            ("模", 1_200),
        ]);
        let cut = text.iter().position(|&ch| ch == 'C').unwrap();
        assert_eq!(cut_times(&text, &[cut], &spoken), vec![Some(900)]);
    }

    #[test]
    fn a_cut_with_nothing_recognizable_after_it_is_untimed() {
        let text: Vec<char> = "你好。123".chars().collect();
        assert_eq!(
            cut_times(&text, &[4], &words(&[("你", 0), ("好", 100)])),
            vec![None]
        );
    }

    #[test]
    fn the_caption_heard_is_the_last_one_started_across_requests() {
        let mut timeline = CaptionTimeline::new(1_000);
        timeline.begin_request(0);
        timeline.push(0, 0, "一".to_string());
        timeline.push(0, 1_000, "二".to_string());
        timeline.begin_request(1);
        timeline.push(1, 0, "三".to_string());

        let heard = |request, samples| timeline.at(request, samples).map(|p| (p.index, p.text));
        assert_eq!(heard(0, 500), Some((0, "一".to_string())));
        assert_eq!(heard(0, 1_500), Some((1, "二".to_string())));
        // Indexed across the read, not per request.
        assert_eq!(heard(1, 10), Some((2, "三".to_string())));
        assert_eq!(heard(2, 10), None);
    }

    #[test]
    fn a_retried_request_places_its_captions_afresh() {
        let mut timeline = CaptionTimeline::new(1_000);
        timeline.begin_request(0);
        timeline.push(0, 0, "attempt one".to_string());
        timeline.begin_request(0);
        timeline.push(0, 0, "attempt two".to_string());
        let heard = timeline.at(0, 10).unwrap();
        assert_eq!((heard.index, heard.text.as_str()), (0, "attempt two"));
    }

    #[test]
    fn starts_never_go_backwards() {
        let mut timeline = CaptionTimeline::new(1_000);
        timeline.begin_request(0);
        timeline.push(0, 2_000, "first".to_string());
        timeline.push(0, 1_000, "second".to_string());
        assert_eq!(timeline.at(0, 1_500), None);
        assert_eq!(timeline.at(0, 2_000).unwrap().text, "second");
    }
}
