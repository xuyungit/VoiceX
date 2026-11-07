import { changes, condense, diffText, prefersInline, tokenize } from './textDiff'

function assert(condition: unknown, message: string): asserts condition {
  if (!condition) throw new Error(message)
}

// Rebuilding both texts from the segments must be lossless, and every change
// offset must point at its text in the source, or mined corrections are wrong.
function assertConsistent(before: string, after: string) {
  const diff = diffText(before, after)
  let rebuiltBefore = ''
  let rebuiltAfter = ''
  for (const segment of diff.segments) {
    if (segment.kind === 'equal') {
      rebuiltBefore += segment.text
      rebuiltAfter += segment.text
      continue
    }
    assert(before.slice(segment.beforeStart, segment.beforeStart + segment.before.length) === segment.before, `beforeStart for ${JSON.stringify(segment)}`)
    assert(after.slice(segment.afterStart, segment.afterStart + segment.after.length) === segment.after, `afterStart for ${JSON.stringify(segment)}`)
    rebuiltBefore += segment.before
    rebuiltAfter += segment.after
  }
  assert(rebuiltBefore === before && rebuiltAfter === after, `Lossless rebuild: ${before} -> ${after}`)
  return diff
}

const pairs = (before: string, after: string) => changes(assertConsistent(before, after)).map(c => [c.before, c.after])

export function checkTextDiff() {
  assert(tokenize('DERP所以 ok').join('|') === 'DERP|所|以| |ok', 'Latin words stay whole, Han splits per character')

  const sub = pairs('今天开会的记要我已经发到群里了。', '今天开会的纪要我已经发到群里了。')
  assert(JSON.stringify(sub) === '[["记","纪"]]', `Single-character substitution: ${JSON.stringify(sub)}`)

  const ins = pairs('应该做替换，所以你检查一下', '应该做替换得到正确的识别：DERP，所以你检查一下')
  assert(JSON.stringify(ins) === '[["","得到正确的识别：DERP"]]', `Pure insertion: ${JSON.stringify(ins)}`)

  const del = pairs('嗯，我们明天见', '我们明天见')
  assert(JSON.stringify(del) === '[["嗯，",""]]', `Pure deletion: ${JSON.stringify(del)}`)

  const word = pairs('please ping the DIRP team', 'please ping the DERP team')
  assert(JSON.stringify(word) === '[["DIRP","DERP"]]', `Whole-word substitution: ${JSON.stringify(word)}`)

  // A coincidental shared character between two changes folds into one change.
  const rewrite = pairs('提示词写得更密一些好，还是', '提示词写得更好有可能改善，还是')
  assert(rewrite.length === 1, `Nearby changes merge: ${JSON.stringify(rewrite)}`)

  // Separate corrections stay separate so each can become its own rule.
  const terms = pairs('我们用 cloud code 来写 type script', '我们用 Claude Code 来写 TypeScript')
  assert(JSON.stringify(terms) === '[["cloud code","Claude Code"],["type script","TypeScript"]]', `Distinct corrections: ${JSON.stringify(terms)}`)

  const lines = pairs('第一行\n第二行', '第一行\n\n第二行')
  assert(JSON.stringify(lines) === '[["","\\n"]]', `Line break insertion: ${JSON.stringify(lines)}`)

  assertConsistent('', 'new text')
  assertConsistent('old text', '')
  assertConsistent('a b c 中文 d', 'x 中 y 文 z')
  assert(diffText('same', 'same').segments.length === 1 && diffText('', '').segments.length === 0, 'Identical texts')

  const small = diffText('今天开会的记要我已经发到群里了。', '今天开会的纪要我已经发到群里了。')
  assert(prefersInline(small), 'Small edits read inline')
  assert(!prefersInline(diffText('今天天气很好', '明日有雨记得带伞')), 'Rewrites fall back to stacked')

  // A rewrite too large to align is still lossless, as one change.
  const longA = Array.from({ length: 2000 }, (_, i) => String.fromCharCode(0x4e00 + (i * 7) % 500)).join('')
  const longB = Array.from({ length: 2000 }, (_, i) => String.fromCharCode(0x4e00 + (i * 13 + 3) % 500)).join('')
  const big = assertConsistent(longA, longB)
  assert(!big.aligned && !prefersInline(big), 'Unalignable texts are reported as such')

  const long = '开头'.repeat(40) + '记' + '中间'.repeat(40) + '要' + '结尾'.repeat(40)
  const shown = condense(diffText(long, long.replace('记', '纪').replace('要', '耀')), 4)
  const text = shown.map(s => s.kind === 'equal' ? s.text : s.kind === 'change' ? `[${s.before}>${s.after}]` : '…').join('')
  assert(text === '…开头开头[记>纪]中间中间…中间中间[要>耀]结尾结尾…', `Condensed view keeps context around each change: ${text}`)
}
