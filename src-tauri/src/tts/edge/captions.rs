//! Short captions within large requests, timed by Edge's sentence/word events.

use super::super::{caption_timeline, log_event, split_for_backend};

#[derive(Debug, PartialEq)]
pub(super) enum Boundary {
    Sentence {
        start_ms: u64,
        duration_ms: u64,
        text: String,
    },
    Word {
        start_ms: u64,
        text: String,
    },
}

struct Sentence {
    start_ms: u64,
    end_ms: u64,
    written: Vec<char>,
    parts: Vec<String>,
    cuts: Vec<usize>,
    times: Vec<Option<u64>>,
    aligned_chars: usize,
}

pub(super) struct CaptionFeed {
    limit: usize,
    sentences: Vec<Sentence>,
    words: Vec<(u64, String)>,
    new_word_chars: usize,
    new_sentence: bool,
}

impl CaptionFeed {
    pub(super) fn new(limit: usize) -> Self {
        Self {
            limit: limit.max(1),
            sentences: Vec::new(),
            words: Vec::new(),
            new_word_chars: 0,
            new_sentence: false,
        }
    }

    pub(super) fn read(&mut self, event: Boundary) {
        match event {
            Boundary::Sentence {
                start_ms,
                duration_ms,
                text,
            } => {
                let parts = split_for_backend(&text, self.limit);
                let cuts: Vec<usize> = parts
                    .iter()
                    .take(parts.len().saturating_sub(1))
                    // Edge words omit punctuation. Select the first matched
                    // character at/after the cut, rather than timing it on
                    // the previous caption's last spoken character.
                    .scan(0, |at, part| {
                        *at += part.chars().count();
                        Some(*at + 1)
                    })
                    .collect();
                self.sentences.push(Sentence {
                    start_ms,
                    end_ms: start_ms.saturating_add(duration_ms),
                    written: text.chars().collect(),
                    parts,
                    times: vec![None; cuts.len()],
                    cuts,
                    aligned_chars: 0,
                });
                self.new_sentence = true;
            }
            Boundary::Word { start_ms, text } => {
                self.new_word_chars += text.chars().count();
                self.words.push((start_ms, text));
            }
        }
    }

    /// Revisit long-sentence cuts in batches, avoiding an alignment for every
    /// word. Sentence captions are placed immediately; the final frame flushes
    /// the last word batch. No equal-duration or character-rate estimates.
    pub(super) fn schedule(&mut self, finished: bool) -> Option<Vec<(u64, String)>> {
        if !finished && !self.new_sentence && self.new_word_chars < 64 {
            return None;
        }
        self.new_sentence = false;
        self.new_word_chars = 0;
        let mut schedule = Vec::new();
        let mut untimed = 0;
        for sentence in &mut self.sentences {
            schedule.push((sentence.start_ms, sentence.parts[0].clone()));
            if sentence.cuts.is_empty() {
                continue;
            }
            if sentence.times.iter().any(Option::is_none) {
                let spoken: Vec<(char, u64)> = self
                    .words
                    .iter()
                    .filter(|(ms, _)| *ms >= sentence.start_ms && *ms < sentence.end_ms)
                    .flat_map(|(ms, word)| word.chars().map(move |ch| (ch, *ms)))
                    .collect();
                if spoken.len() > sentence.aligned_chars {
                    sentence.aligned_chars = spoken.len();
                    let times =
                        caption_timeline::cut_times(&sentence.written, &sentence.cuts, &spoken);
                    for (placed, time) in sentence.times.iter_mut().zip(times) {
                        // Resolved cuts stay fixed as later words arrive.
                        if placed.is_none() {
                            *placed = time;
                        }
                    }
                }
            }
            for (text, time) in sentence.parts.iter().skip(1).zip(&sentence.times) {
                if let Some(time) = time {
                    schedule.push((*time, text.clone()));
                } else {
                    untimed += 1;
                }
            }
        }
        if finished && untimed > 0 {
            log_event(
                "edge_caption_untimed",
                &[("boundaries", untimed.to_string())],
            );
        }
        Some(schedule)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interleaved_words_time_long_captions_and_preserve_spaces_and_numbers() {
        let text = "反力10.05千牛，English words with spaces。";
        let mut feed = CaptionFeed::new(12);
        feed.read(Boundary::Word {
            start_ms: 100,
            text: "反力".into(),
        });
        feed.read(Boundary::Sentence {
            start_ms: 100,
            duration_ms: 5000,
            text: text.into(),
        });
        let first = feed.schedule(false).unwrap();
        assert_eq!(first[0], (100, "反力10.05千牛，".into()));
        for (ms, word) in [
            (300, "10.05千"),
            (700, "牛"),
            (1100, "English"),
            (2100, "words"),
            (2800, "with"),
            (3500, "spaces"),
        ] {
            feed.read(Boundary::Word {
                start_ms: ms,
                text: word.into(),
            });
        }
        let schedule = feed.schedule(true).unwrap();
        assert_eq!(
            schedule
                .iter()
                .map(|(_, text)| text.as_str())
                .collect::<String>(),
            text
        );
        assert_eq!(schedule[1].0, 1100);
        assert!(schedule.iter().all(|(_, text)| text.chars().count() <= 12));
    }

    #[test]
    fn normalized_words_align_without_inventing_times_for_unmatched_text() {
        let mut feed = CaptionFeed::new(6);
        feed.read(Boundary::Sentence {
            start_ms: 100,
            duration_ms: 4000,
            text: "增加10千牛。3组对比。".into(),
        });
        for (ms, word) in [(100, "增加"), (500, "十千牛"), (1500, "三组对比")] {
            feed.read(Boundary::Word {
                start_ms: ms,
                text: word.into(),
            });
        }
        let schedule = feed.schedule(true).unwrap();
        assert!(schedule.len() > 1);
        assert_eq!(schedule.last().unwrap().0, 1500);
        assert!(schedule
            .iter()
            .map(|(ms, _)| *ms)
            .all(|ms| [100, 500, 1500].contains(&ms)));
        let mut missing = CaptionFeed::new(3);
        missing.read(Boundary::Sentence {
            start_ms: 0,
            duration_ms: 1000,
            text: "你好。123".into(),
        });
        missing.read(Boundary::Word {
            start_ms: 100,
            text: "你好".into(),
        });
        assert_eq!(missing.schedule(true).unwrap(), vec![(0, "你好。".into())]);
    }
}
