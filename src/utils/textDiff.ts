/**
 * Text diff for showing how a dictation was corrected.
 *
 * The result is a list of segments: unchanged text, and changes that pair the
 * removed text with what replaced it. Pairing (rather than separate delete and
 * insert runs) is what lets a change be read as a correction "记 → 纪", which
 * is also the shape later tooling needs to mine recurring recognition errors
 * into hotwords or replacement rules. Each change keeps its offsets into both
 * texts so such tooling can recover the surrounding context.
 */

export interface EqualSegment {
  kind: 'equal'
  text: string
}

export interface ChangeSegment {
  kind: 'change'
  /** Removed text; empty for a pure insertion. */
  before: string
  /** Inserted text; empty for a pure deletion. */
  after: string
  /** UTF-16 offset of `before` in the original text. */
  beforeStart: number
  /** UTF-16 offset of `after` in the edited text. */
  afterStart: number
}

export type DiffSegment = EqualSegment | ChangeSegment

export interface TextDiff {
  segments: DiffSegment[]
  /** Share of the longer text that changed, 0..1. */
  changeRatio: number
  /**
   * False when the texts differ too much to align within the edit budget; the
   * result is then a single change covering both texts.
   */
  aligned: boolean
}

/** Above this share of changed text an inline diff stops being readable. */
export const INLINE_MAX_CHANGE_RATIO = 0.4

/** Edit budget in tokens; texts needing more edits are treated as rewritten. */
const MAX_EDIT_DISTANCE = 1500

// Han, kana: one token per character, since these scripts have no spaces and a
// recognition error is usually one or two characters. Other letters and digits
// form word tokens so "DERP" never matches letter by letter. Line breaks stand
// alone so a change never silently swallows one.
const TOKEN = /[\p{sc=Han}\p{sc=Hiragana}\p{sc=Katakana}]|(?:(?![\p{sc=Han}\p{sc=Hiragana}\p{sc=Katakana}])[\p{L}\p{M}\p{N}_])+|\n|[^\S\n]+|./gsu

export function tokenize(text: string): string[] {
  return text.match(TOKEN) ?? []
}

type Op = 0 | 1 | 2 // equal, delete, insert

/** Myers' O(ND) diff over tokens; null when more than `maxD` edits are needed. */
function myers(a: string[], b: string[], maxD: number): Op[] | null {
  const n = a.length
  const m = b.length
  const max = n + m
  const offset = max + 1
  const v = new Int32Array(2 * max + 3)
  const trace: Int32Array[] = []
  const limit = Math.min(max, maxD)
  for (let d = 0; d <= limit; d++) {
    for (let k = -d; k <= d; k += 2) {
      let x = k === -d || (k !== d && v[offset + k - 1] < v[offset + k + 1])
        ? v[offset + k + 1]
        : v[offset + k - 1] + 1
      let y = x - k
      while (x < n && y < m && a[x] === b[y]) { x++; y++ }
      v[offset + k] = x
      if (x >= n && y >= m) {
        trace.push(v.slice(offset - d, offset + d + 1))
        return backtrack(trace, n, m)
      }
    }
    trace.push(v.slice(offset - d, offset + d + 1))
  }
  return null
}

function backtrack(trace: Int32Array[], n: number, m: number): Op[] {
  const ops: Op[] = []
  let x = n
  let y = m
  for (let d = trace.length - 1; d > 0; d--) {
    const prev = trace[d - 1] // k in [-(d - 1), d - 1]
    const at = (k: number) => prev[k + d - 1]
    const k = x - y
    const prevK = k === -d || (k !== d && at(k - 1) < at(k + 1)) ? k + 1 : k - 1
    const prevX = at(prevK)
    const prevY = prevX - prevK
    while (x > prevX && y > prevY) { ops.push(0); x--; y-- }
    if (x === prevX) { ops.push(2); y-- } else { ops.push(1); x-- }
  }
  while (x > 0 && y > 0) { ops.push(0); x--; y-- }
  return ops.reverse()
}

const length = (text: string) => Array.from(text).length

/** Longest unchanged run that may be folded into its neighbouring changes. */
const MAX_FOLD = 2

/**
 * Fold a short unchanged run between two changes into them, so a rewrite
 * reads as one change instead of confetti around coincidental matches like a
 * shared 的 (diff-match-patch's semantic cleanup, capped at MAX_FOLD so two
 * separate corrections such as "cloud code" and "type script" stay apart).
 * Never folds across a line break.
 */
function mergeNearbyChanges(segments: DiffSegment[]): DiffSegment[] {
  let merged = true
  while (merged) {
    merged = false
    for (let i = 1; i < segments.length - 1; i++) {
      const prev = segments[i - 1]
      const eq = segments[i]
      const next = segments[i + 1]
      if (prev.kind !== 'change' || eq.kind !== 'equal' || next.kind !== 'change') continue
      if (eq.text.includes('\n')) continue
      const size = length(eq.text)
      const left = Math.max(length(prev.before), length(prev.after))
      const right = Math.max(length(next.before), length(next.after))
      if (size > MAX_FOLD || size > left || size > right) continue
      segments.splice(i - 1, 3, {
        kind: 'change',
        before: prev.before + eq.text + next.before,
        after: prev.after + eq.text + next.after,
        beforeStart: prev.beforeStart,
        afterStart: prev.afterStart
      })
      merged = true
      i--
    }
  }
  return segments
}

export function diffText(before: string, after: string): TextDiff {
  if (before === after) return { segments: before ? [{ kind: 'equal', text: before }] : [], changeRatio: 0, aligned: true }
  const a = tokenize(before)
  const b = tokenize(after)

  // Common prefix and suffix are free; most corrections touch a few tokens.
  let head = 0
  while (head < a.length && head < b.length && a[head] === b[head]) head++
  let tail = 0
  while (tail < a.length - head && tail < b.length - head && a[a.length - 1 - tail] === b[b.length - 1 - tail]) tail++
  const midA = a.slice(head, a.length - tail)
  const midB = b.slice(head, b.length - tail)
  const midOps = myers(midA, midB, MAX_EDIT_DISTANCE)
  const longer = Math.max(length(before), length(after))

  if (!midOps) {
    return {
      segments: [{ kind: 'change', before, after, beforeStart: 0, afterStart: 0 }],
      changeRatio: 1,
      aligned: false
    }
  }
  const ops: Op[] = [...new Array<Op>(head).fill(0), ...midOps, ...new Array<Op>(tail).fill(0)]

  const segments: DiffSegment[] = []
  let i = 0
  let j = 0
  let beforeOffset = 0
  let afterOffset = 0
  for (const op of ops) {
    const last = segments[segments.length - 1]
    if (op === 0) {
      const token = a[i++]
      j++
      if (last?.kind === 'equal') last.text += token
      else segments.push({ kind: 'equal', text: token })
      beforeOffset += token.length
      afterOffset += token.length
      continue
    }
    let change: ChangeSegment
    if (last?.kind === 'change') change = last
    else {
      change = { kind: 'change', before: '', after: '', beforeStart: beforeOffset, afterStart: afterOffset }
      segments.push(change)
    }
    if (op === 1) {
      change.before += a[i]
      beforeOffset += a[i++].length
    } else {
      change.after += b[j]
      afterOffset += b[j++].length
    }
  }

  mergeNearbyChanges(segments)
  let changed = 0
  for (const segment of segments) {
    if (segment.kind === 'change') changed += Math.max(length(segment.before), length(segment.after))
  }
  return { segments, changeRatio: longer ? changed / longer : 0, aligned: true }
}

/** Whether the diff reads well as one paragraph with marked changes. */
export function prefersInline(diff: TextDiff): boolean {
  return diff.aligned && diff.changeRatio <= INLINE_MAX_CHANGE_RATIO
}

export function changes(diff: TextDiff): ChangeSegment[] {
  return diff.segments.filter((segment): segment is ChangeSegment => segment.kind === 'change')
}

export interface GapSegment {
  kind: 'gap'
}

/** A segment to render, tagged with its index in `TextDiff.segments`. */
export type DisplaySegment = (DiffSegment | GapSegment) & { index: number }

/**
 * Shorten unchanged runs to `context` characters around each change, so a
 * collapsed record shows every change instead of only its first lines.
 */
export function condense(diff: TextDiff, context: number): DisplaySegment[] {
  const result: DisplaySegment[] = []
  const last = diff.segments.length - 1
  diff.segments.forEach((segment, index) => {
    if (segment.kind !== 'equal') { result.push({ ...segment, index }); return }
    const chars = Array.from(segment.text)
    const keepHead = index > 0 // follows a change
    const keepTail = index < last // precedes a change
    const keep = (keepHead ? context : 0) + (keepTail ? context : 0)
    if (chars.length <= keep + 1) { result.push({ ...segment, index }); return }
    if (!keepHead && !keepTail) {
      // No change at all: show the opening only.
      result.push({ kind: 'equal', text: chars.slice(0, context * 2).join(''), index }, { kind: 'gap', index })
      return
    }
    if (keepHead) result.push({ kind: 'equal', text: chars.slice(0, context).join(''), index })
    result.push({ kind: 'gap', index })
    if (keepTail) result.push({ kind: 'equal', text: chars.slice(chars.length - context).join(''), index })
  })
  return result
}

/** Every segment as-is, in the same shape `condense` returns. */
export function expand(diff: TextDiff): DisplaySegment[] {
  return diff.segments.map((segment, index) => ({ ...segment, index }))
}
