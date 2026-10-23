const PUNCTUATION = [
    '。', '？', '！', '，', '、', '；', '：', '“', '”', '‘', '’', '（', '）', '《', '》',
    '.', '?', '!', ',', ';', ':', '"', '\'', '(', ')', '[', ']', '{', '}',
]

/**
 * The punctuation a spoken line can afford to lose: the marks that carry no
 * meaning of their own at the end of a subtitle, because the line ending
 * already says the thought has closed.
 *
 * Everything else stays. ？！ carry the tone of voice, … says the sentence
 * runs on, brackets and dashes carry their own meaning — and none of them is
 * what makes a caption look like the end of a paragraph.
 */
const LINE_ENDING_PUNCTUATION = new Set([
    '。', '，', '、', '；', '：',
    '.', ',', ';', ':',
])

/**
 * Chinese quotation marks, the ones a line can end in with the real
 * punctuation tucked inside the quote: “他说：“别急。”” ends in the quote, not
 * in the full stop.
 *
 * ASCII quotes are not in this set on purpose: ' is an apostrophe at least as
 * often as it is a quotation, and guessing wrong would turn "it's" into
 * "its".
 */
const QUOTE_PAIRS: Array<[string, string]> = [
    ['“', '”'],
    ['‘', '’'],
]

function isQuoteMark(char: string): boolean {
    return QUOTE_PAIRS.some(([openMark, closeMark]) => char === openMark || char === closeMark)
}

/**
 * The half of a quote pair whose partner never showed up — a pair that lost
 * one side anywhere along the line is no longer a quotation, just noise.
 */
function withoutUnmatchedQuotes(text: string): string {
    let result = text
    for (const [openMark, closeMark] of QUOTE_PAIRS) {
        const openCount = result.split(openMark).length - 1
        const closeCount = result.split(closeMark).length - 1
        if (openCount > 0 && closeCount === 0) {
            result = result.split(openMark).join('')
        } else if (closeCount > 0 && openCount === 0) {
            result = result.split(closeMark).join('')
        }
    }
    return result.trim()
}

/**
 * The text of a reading caption as it should be shown: the same line, minus
 * the punctuation that closes it.
 *
 * A caption ends by vanishing, so a "。" at its edge closes nothing — but the
 * line keeps every mark that says something. A closing quote is lifted off to
 * reach the punctuation inside it and put straight back, so a quoted sentence
 * stays quoted: “你好。” is shown as “你好”, not stripped to 你好.
 */
export function trimCaptionPunctuation(text: string): string {
    const trimmed = text.trim()

    let body = trimmed
    let closingQuotes = ''
    while (body.length > 0 && isQuoteMark(body.charAt(body.length - 1))) {
        closingQuotes = body.charAt(body.length - 1) + closingQuotes
        body = body.slice(0, -1).trimEnd()
    }

    while (
        body.length > 0 &&
        LINE_ENDING_PUNCTUATION.has(body.charAt(body.length - 1))
    ) {
        body = body.slice(0, -1).trimEnd()
    }

    return withoutUnmatchedQuotes(body + closingQuotes)
}

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
