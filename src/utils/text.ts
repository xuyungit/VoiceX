const PUNCTUATION = [
    '。', '？', '！', '，', '、', '；', '：', '“', '”', '‘', '’', '（', '）', '《', '》',
    '.', '?', '!', ',', ';', ':', '"', '\'', '(', ')', '[', ']', '{', '}',
]

/**
 * Trims trailing punctuation from a string.
 */
export function trimTrailingPunctuation(text: string): string {
    let result = text.trim()
    while (result.length > 0) {
        const lastChar = result.charAt(result.length - 1)
        if (PUNCTUATION.includes(lastChar)) {
            result = result.slice(0, -1).trim()
        } else {
            break
        }
    }
    return result
}
