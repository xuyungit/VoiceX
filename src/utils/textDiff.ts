export interface DiffPart {
  text: string
  changed: boolean
}

/**
 * Character-level diff (longest common subsequence). `before` marks the
 * characters that were removed, `after` the ones that were added. Meant for
 * sentence-sized text such as a provider test, not whole documents.
 */
export function diffChars(before: string, after: string): { before: DiffPart[]; after: DiffPart[] } {
  const a = Array.from(before)
  const b = Array.from(after)
  // lcs[i][j] = LCS length of a[i..] and b[j..]
  const lcs: number[][] = Array.from({ length: a.length + 1 }, () => new Array<number>(b.length + 1).fill(0))
  for (let i = a.length - 1; i >= 0; i--) {
    for (let j = b.length - 1; j >= 0; j--) {
      lcs[i][j] = a[i] === b[j] ? lcs[i + 1][j + 1] + 1 : Math.max(lcs[i + 1][j], lcs[i][j + 1])
    }
  }

  const beforeParts: DiffPart[] = []
  const afterParts: DiffPart[] = []
  const push = (parts: DiffPart[], text: string, changed: boolean) => {
    const last = parts[parts.length - 1]
    if (last && last.changed === changed) last.text += text
    else parts.push({ text, changed })
  }

  let i = 0
  let j = 0
  while (i < a.length || j < b.length) {
    if (i < a.length && j < b.length && a[i] === b[j]) {
      push(beforeParts, a[i++], false)
      push(afterParts, b[j++], false)
    } else if (j < b.length && (i === a.length || lcs[i][j + 1] >= lcs[i + 1][j])) {
      push(afterParts, b[j++], true)
    } else {
      push(beforeParts, a[i++], true)
    }
  }
  return { before: beforeParts, after: afterParts }
}
