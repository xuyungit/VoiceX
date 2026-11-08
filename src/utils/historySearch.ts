import { finalText, type HistoryRecord } from '../stores/history'

// Match SQLite LIKE's ASCII case folding; Chinese remains a literal substring.
const fold = (value: string) => value.replace(/[A-Z]/g, char => char.toLowerCase())
export const searchWords = (query: string) => [...new Set(fold(query).split(/\s+/u).filter(Boolean))]

export interface SearchPart { text: string; match: boolean }

const LEAD = 28
const LOOKBACK = 60
const SENTENCE_ENDS = ['。', '！', '？', '；', '\n', '. ', '! ', '? ', '; ']

/** Where an excerpt showing a match at `hit` should start: the sentence it is
 *  in when that begins close enough, else a short lead before it. */
function excerptStart(text: string, hit: number) {
    if (hit <= LEAD) return 0
    const from = Math.max(0, hit - LOOKBACK)
    const before = text.slice(from, hit)
    const cut = Math.max(...SENTENCE_ENDS.map(end => {
        const at = before.lastIndexOf(end)
        return at < 0 ? -1 : at + end.length
    }))
    let start = cut >= 0 ? from + cut : hit - LEAD
    while (start < hit && /\s/u.test(text[start])) start++
    return start
}

/** Plain text parts only: history content must never become HTML. */
export function searchPreview(text: string, query: string): { parts: SearchPart[]; truncated: boolean } {
    const words = searchWords(query)
    if (!words.length) return { parts: [{ text, match: false }], truncated: false }
    const folded = fold(text)
    const hits = words.map(word => ({ index: folded.indexOf(word), length: word.length }))
        .filter(hit => hit.index >= 0).sort((a, b) => a.index - b.index)
    const first = hits[0]
    let start = first ? excerptStart(text, first.index) : 0
    let end = Math.min(text.length, Math.max(start + 140, first ? first.index + first.length : 0))
    // Keep UTF-16 surrogate pairs intact at excerpt boundaries.
    if (start > 0 && /[\uDC00-\uDFFF]/.test(text[start])) start--
    if (end < text.length && /[\uDC00-\uDFFF]/.test(text[end])) end++
    const excerpt = text.slice(start, end)
    const lower = fold(excerpt)
    const ranges: [number, number][] = []
    for (const word of words) {
        let at = lower.indexOf(word)
        while (at >= 0) {
            ranges.push([at, at + word.length])
            at = lower.indexOf(word, at + 1)
        }
    }
    ranges.sort((a, b) => a[0] - b[0])
    const merged: [number, number][] = []
    for (const range of ranges) {
        const last = merged[merged.length - 1]
        if (last && range[0] <= last[1]) last[1] = Math.max(last[1], range[1])
        else merged.push([...range])
    }
    const parts: SearchPart[] = []
    if (start) parts.push({ text: '…', match: false })
    let cursor = 0
    for (const [from, to] of merged) {
        if (from > cursor) parts.push({ text: excerpt.slice(cursor, from), match: false })
        parts.push({ text: excerpt.slice(from, to), match: true })
        cursor = to
    }
    if (cursor < excerpt.length) parts.push({ text: excerpt.slice(cursor), match: false })
    if (end < text.length) parts.push({ text: '…', match: false })
    return { parts, truncated: start > 0 || end < text.length }
}

/** Explain matches that the visible final text alone cannot account for. */
export function hiddenSearchMatches(record: HistoryRecord, query: string) {
    let missing = searchWords(query).filter(word => !fold(finalText(record)).includes(word))
    const sources: { kind: 'output' | 'original'; text: string; query: string }[] = []
    for (const [kind, text] of [['output', record.text], ['original', record.originalText]] as const) {
        if (!text || !missing.some(word => fold(text).includes(word))) continue
        // Center this excerpt on the missing words, rather than an earlier word
        // that the final text already explained.
        sources.push({ kind, text, query: missing.join(' ') })
        missing = missing.filter(word => !fold(text).includes(word))
    }
    return sources
}
