use crate::commands::settings::ReplacementRule;
use regex::{Regex, RegexBuilder};

pub struct PostProcessingService;

impl PostProcessingService {
    pub fn process(
        text: &str,
        remove_punctuation: bool,
        threshold: u32,
        rules: &[ReplacementRule],
    ) -> String {
        let mut processed = text.to_string();

        // 1. Trailing Punctuation Removal
        // The threshold measures the sentence itself; ASR/LLM output nearly
        // always ends in a terminator, which must not push a short sentence over.
        if remove_punctuation {
            let without_punctuation = Self::trim_trailing_punctuation(&processed);
            if without_punctuation.chars().count() <= threshold as usize {
                processed = without_punctuation;
            }
        }

        // 2. Keyword Substitution
        for rule in rules {
            if !rule.enabled {
                continue;
            }

            match rule.match_mode.as_str() {
                "exact" => {
                    // Compare the spoken content only, so "keyword。" from a long
                    // sentence (not stripped above) still matches.
                    if Self::exact_match_key(&processed) == Self::exact_match_key(&rule.keyword) {
                        processed = rule.replacement.clone();
                    }
                }
                "contains" => {
                    if let Ok(re) = RegexBuilder::new(&regex::escape(&rule.keyword))
                        .case_insensitive(true)
                        .build()
                    {
                        processed = re.replace_all(&processed, &rule.replacement).to_string();
                    }
                }
                "regex" => {
                    if let Ok(re) = Regex::new(&rule.keyword) {
                        processed = re.replace_all(&processed, &rule.replacement).to_string();
                    }
                }
                _ => {}
            }
        }

        processed
    }

    fn exact_match_key(text: &str) -> String {
        Self::trim_trailing_punctuation(text.trim())
            .trim()
            .to_lowercase()
    }

    fn trim_trailing_punctuation(text: &str) -> String {
        let punctuation = [
            '。', '？', '！', '，', '、', '；', '：', '“', '”', '‘', '’', '（', '）', '《', '》',
            '.', '?', '!', ',', ';', ':', '"', '\'', '(', ')', '[', ']', '{', '}',
        ];

        let mut result = text.to_string();
        while !result.is_empty() {
            if let Some(last_char) = result.chars().last() {
                if punctuation.contains(&last_char) {
                    result.pop();
                } else {
                    break;
                }
            } else {
                break;
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::PostProcessingService;
    use crate::commands::settings::ReplacementRule;

    fn rule(keyword: &str, replacement: &str, match_mode: &str) -> ReplacementRule {
        ReplacementRule {
            id: "rule-1".to_string(),
            keyword: keyword.to_string(),
            replacement: replacement.to_string(),
            match_mode: match_mode.to_string(),
            enabled: true,
        }
    }

    #[test]
    fn exact_match_ignores_case() {
        let rules = vec![rule("hello", "Hi", "exact")];

        let processed = PostProcessingService::process("  HELLO  ", false, 5, &rules);

        assert_eq!(processed, "Hi");
    }

    #[test]
    fn contains_match_ignores_case() {
        let rules = vec![rule("world", "VoiceX", "contains")];

        let processed = PostProcessingService::process("Hello WORLD", false, 5, &rules);

        assert_eq!(processed, "Hello VoiceX");
    }

    #[test]
    fn threshold_counts_characters_without_trailing_punctuation() {
        let processed = PostProcessingService::process("我家的地址。", true, 5, &[]);
        assert_eq!(processed, "我家的地址");

        let processed = PostProcessingService::process("下一步该做什么？", true, 5, &[]);
        assert_eq!(processed, "下一步该做什么？");
    }

    #[test]
    fn exact_match_ignores_trailing_punctuation() {
        let rules = vec![rule("目前公司的地址", "公司地址", "exact")];

        let processed = PostProcessingService::process("目前公司的地址。", true, 5, &rules);

        assert_eq!(processed, "公司地址");
    }

    #[test]
    fn regex_match_remains_case_sensitive_by_default() {
        let rules = vec![rule("world", "VoiceX", "regex")];

        let processed = PostProcessingService::process("Hello WORLD", false, 5, &rules);

        assert_eq!(processed, "Hello WORLD");
    }
}
