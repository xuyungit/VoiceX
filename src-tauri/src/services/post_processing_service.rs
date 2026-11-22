use std::sync::Arc;

use crate::commands::settings::ReplacementRule;
use regex::{Regex, RegexBuilder};

#[derive(Debug, Clone)]
enum CompiledReplacementMatcher {
    Exact { key: String },
    Regex(Regex),
}

#[derive(Debug, Clone)]
pub struct CompiledReplacementRule {
    matcher: CompiledReplacementMatcher,
    replacement: String,
}

pub struct PostProcessingService;

impl PostProcessingService {
    /// Pre-compile enabled replacement rules so the dictation hot path never
    /// recompiles regular expressions on every utterance.
    pub fn compile_rules(rules: &[ReplacementRule]) -> Arc<[CompiledReplacementRule]> {
        let mut compiled = Vec::with_capacity(rules.len());
        for rule in rules {
            if !rule.enabled {
                continue;
            }
            match rule.match_mode.as_str() {
                "exact" => compiled.push(CompiledReplacementRule {
                    matcher: CompiledReplacementMatcher::Exact {
                        key: Self::exact_match_key(&rule.keyword),
                    },
                    replacement: rule.replacement.clone(),
                }),
                "contains" => match RegexBuilder::new(&regex::escape(&rule.keyword))
                    .case_insensitive(true)
                    .build()
                {
                    Ok(re) => compiled.push(CompiledReplacementRule {
                        matcher: CompiledReplacementMatcher::Regex(re),
                        replacement: rule.replacement.clone(),
                    }),
                    Err(err) => {
                        log::warn!(
                            "Skipping invalid contains replacement rule '{}': {}",
                            rule.id,
                            err
                        );
                    }
                },
                "regex" => match Regex::new(&rule.keyword) {
                    Ok(re) => compiled.push(CompiledReplacementRule {
                        matcher: CompiledReplacementMatcher::Regex(re),
                        replacement: rule.replacement.clone(),
                    }),
                    Err(err) => {
                        log::warn!(
                            "Skipping invalid regex replacement rule '{}': {}",
                            rule.id,
                            err
                        );
                    }
                },
                other => {
                    log::warn!(
                        "Skipping replacement rule '{}' with unknown match_mode '{}'",
                        rule.id,
                        other
                    );
                }
            }
        }
        Arc::from(compiled)
    }

    pub fn process(
        text: &str,
        remove_punctuation: bool,
        threshold: u32,
        remove_blank_lines: bool,
        rules: &[ReplacementRule],
    ) -> String {
        let compiled = Self::compile_rules(rules);
        Self::process_compiled(
            text,
            remove_punctuation,
            threshold,
            remove_blank_lines,
            &compiled,
        )
    }

    pub fn process_compiled(
        text: &str,
        remove_punctuation: bool,
        threshold: u32,
        remove_blank_lines: bool,
        rules: &[CompiledReplacementRule],
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
            match &rule.matcher {
                CompiledReplacementMatcher::Exact { key } => {
                    // Compare the spoken content only, so "keyword。" from a long
                    // sentence (not stripped above) still matches.
                    if Self::exact_match_key(&processed) == *key {
                        processed = rule.replacement.clone();
                    }
                }
                CompiledReplacementMatcher::Regex(re) => {
                    processed = re.replace_all(&processed, &rule.replacement).to_string();
                }
            }
        }

        if remove_blank_lines {
            // Keep each non-blank line and its original line ending. Run last
            // so blank lines inserted by keyword substitutions are removed too.
            processed
                .split_inclusive('\n')
                .filter(|line| !line.trim().is_empty())
                .collect()
        } else {
            processed
        }
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

        let processed = PostProcessingService::process("  HELLO  ", false, 5, false, &rules);

        assert_eq!(processed, "Hi");
    }

    #[test]
    fn contains_match_ignores_case() {
        let rules = vec![rule("world", "VoiceX", "contains")];

        let processed = PostProcessingService::process("Hello WORLD", false, 5, false, &rules);

        assert_eq!(processed, "Hello VoiceX");
    }

    #[test]
    fn threshold_counts_characters_without_trailing_punctuation() {
        let processed = PostProcessingService::process("我家的地址。", true, 5, false, &[]);
        assert_eq!(processed, "我家的地址");

        let processed = PostProcessingService::process("下一步该做什么？", true, 5, false, &[]);
        assert_eq!(processed, "下一步该做什么？");
    }

    #[test]
    fn exact_match_ignores_trailing_punctuation() {
        let rules = vec![rule("目前公司的地址", "公司地址", "exact")];

        let processed = PostProcessingService::process("目前公司的地址。", true, 5, false, &rules);

        assert_eq!(processed, "公司地址");
    }

    #[test]
    fn regex_match_remains_case_sensitive_by_default() {
        let rules = vec![rule("world", "VoiceX", "regex")];

        let processed = PostProcessingService::process("Hello WORLD", false, 5, false, &rules);

        assert_eq!(processed, "Hello WORLD");
    }

    #[test]
    fn remove_blank_lines_preserves_regular_line_breaks() {
        for (text, expected) in [
            (
                "\n第一段。\n\n \t\n第二段。\n  - 列表项",
                "第一段。\n第二段。\n  - 列表项",
            ),
            ("First\r\n\r\n \t\r\nSecond\r\n", "First\r\nSecond\r\n"),
        ] {
            assert_eq!(
                PostProcessingService::process(text, false, 5, true, &[]),
                expected
            );
            assert_eq!(
                PostProcessingService::process(text, false, 5, false, &[]),
                text
            );
        }
    }

    #[test]
    fn remove_blank_lines_runs_after_keyword_substitution() {
        let rules = vec![rule("交接", "第一项\n\n第二项", "exact")];
        assert_eq!(
            PostProcessingService::process("交接", false, 5, true, &rules),
            "第一项\n第二项"
        );
    }

    #[test]
    fn compile_rules_omits_disabled_and_invalid_regex_while_keeping_valid_rules() {
        let mut disabled = rule("foo", "DISABLED", "contains");
        disabled.enabled = false;
        let invalid_regex = rule("[unclosed", "BROKEN", "regex");
        let valid_contains = rule("foo", "bar", "contains");
        let valid_regex = rule(r"\bbar\b", "VoiceX", "regex");

        let compiled = PostProcessingService::compile_rules(&[
            disabled,
            invalid_regex,
            valid_contains,
            valid_regex,
        ]);
        assert_eq!(compiled.len(), 2);
        assert_eq!(
            PostProcessingService::process_compiled("Say FOO now", false, 5, false, &compiled),
            "Say VoiceX now"
        );
    }
}
