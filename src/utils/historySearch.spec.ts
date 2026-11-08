import { hiddenSearchMatches, searchPreview } from './historySearch'
import type { HistoryRecord } from '../stores/history'

export function checkHistorySearchPreview() {
    const check = (ok: boolean, message: string) => { if (!ok) throw new Error(message) }
    const marked = (text: string, query: string) => searchPreview(text, query).parts.filter(p => p.match).map(p => p.text)
    check(marked('支座反力标定 OPENAI 50% a_b', '反力 标定 openai 50% a_b').join('|') === '反力标定|OPENAI|50%|a_b', 'Highlight literal terms and merge adjacent matches')
    check(marked('aaaa', 'aa aaa').join('') === 'aaaa', 'Overlapping matches must not duplicate text')
    const long = '说明。'.repeat(100) + '目标反力标定' + '补充。'.repeat(100)
    const preview = searchPreview(long, '反力')
    check(preview.truncated && preview.parts.some(p => p.match && p.text === '反力'), 'Long results show the matching region')
    check(preview.parts[1].text.startsWith('目标'), 'An excerpt starts at the sentence holding the match')
    const english = 'x'.repeat(100) + '. The bearing reaction was calibrated today.'
    check(searchPreview(english, 'calibrated').parts[1].text.startsWith('The bearing'), 'English sentences start after the full stop and its space')
    const run = '无标点'.repeat(40) + '反力' + '后文'.repeat(80)
    check(searchPreview(run, '反力').parts[1].text.length === 28, 'Without a nearby sentence start, keep a short lead')
    const html = '<script>alert("x")</script>'
    check(searchPreview(html, 'script').parts.map(p => p.text).join('') === html, 'HTML-like content remains plain text')
    const row = { text: '今天标定完成', editedText: '支座反力已确认', originalText: '支座返利' } as HistoryRecord
    check(hiddenSearchMatches(row, '反力').length === 0, 'Visible final-text matches need no extra snippet')
    check(hiddenSearchMatches(row, '反力 标定 返利').map(m => m.kind).join(',') === 'output,original', 'Explain terms found only in hidden versions')
    check(searchPreview(long, '').parts[0].text === long, 'Ordinary history remains unabridged')
}
