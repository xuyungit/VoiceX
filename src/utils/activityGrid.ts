import type { DailyActivityPoint, DailyActivityResponse } from '../stores/history'

export type ActivityMetric = 'chars' | 'count'
export type ActivityMode = 'daily' | 'weekly' | 'cumulative'

export interface ActivityCell {
  localDate: string
  weekIndex: number
  dayOfWeek: number
  isFuture: boolean
  level: 0 | 1 | 2 | 3 | 4
  chars: number
  count: number
  durationMs: number
  /** Week start date (Monday) for weekly & cumulative tooltips. */
  weekStartDate: string
  /** Cumulative value up to and including this week. */
  cumulativeChars: number
  cumulativeCount: number
  weeklyChars: number
  weeklyCount: number
}

export interface ActivityGridModel {
  columns: ActivityCell[][]
  monthLabels: Array<{ colIndex: number; label: string }>
}

function parseLocalDate(isoDate: string): Date {
  const [y, m, d] = isoDate.split('-').map(Number)
  return new Date(y, (m || 1) - 1, d || 1)
}

function formatIsoDate(date: Date): string {
  const y = date.getFullYear()
  const m = String(date.getMonth() + 1).padStart(2, '0')
  const d = String(date.getDate()).padStart(2, '0')
  return `${y}-${m}-${d}`
}

function addDays(date: Date, days: number): Date {
  const next = new Date(date.getFullYear(), date.getMonth(), date.getDate() + days)
  return next
}

function intensityLevel(value: number, max: number): 0 | 1 | 2 | 3 | 4 {
  if (value <= 0 || max <= 0) return 0
  const ratio = value / max
  if (ratio > 0.75) return 4
  if (ratio > 0.5) return 3
  if (ratio > 0.25) return 2
  return 1
}

/**
 * Shapes 52 weeks of daily dictation totals into a 52 x 7 grid for daily
 * heatmaps, weekly dot-bars, and cumulative growth curves.
 */
export function buildActivityGrid(
  data: DailyActivityResponse,
  metric: ActivityMetric,
  mode: ActivityMode,
  locale: string,
): ActivityGridModel {
  const start = parseLocalDate(data.startDate)
  const endStr = data.endDate
  const byDate = new Map<string, DailyActivityPoint>()
  for (const point of data.days) {
    byDate.set(point.localDate, point)
  }

  const weekMeta: Array<{
    weekStartDate: string
    weeklyChars: number
    weeklyCount: number
    cumChars: number
    cumCount: number
    hasPastDay: boolean
  }> = []

  let runningChars = data.priorCharacters || 0
  let runningCount = data.priorCount || 0

  for (let w = 0; w < 52; w++) {
    const weekStart = addDays(start, w * 7)
    const weekStartDate = formatIsoDate(weekStart)
    let weeklyChars = 0
    let weeklyCount = 0
    let hasPastDay = false

    for (let d = 0; d < 7; d++) {
      const iso = formatIsoDate(addDays(weekStart, d))
      if (iso <= endStr) hasPastDay = true
      const point = byDate.get(iso)
      if (point) {
        weeklyChars += point.totalCharacters
        weeklyCount += point.usageCount
      }
    }

    runningChars += weeklyChars
    runningCount += weeklyCount
    weekMeta.push({
      weekStartDate,
      weeklyChars,
      weeklyCount,
      cumChars: runningChars,
      cumCount: runningCount,
      hasPastDay,
    })
  }

  let maxDaily = 0
  for (const point of data.days) {
    const val = metric === 'chars' ? point.totalCharacters : point.usageCount
    if (val > maxDaily) maxDaily = val
  }

  const maxWeekly = Math.max(
    0,
    ...weekMeta.map(w => (metric === 'chars' ? w.weeklyChars : w.weeklyCount)),
  )
  const maxCum = Math.max(
    0,
    ...weekMeta.map(w => (metric === 'chars' ? w.cumChars : w.cumCount)),
  )

  const columns: ActivityCell[][] = []
  const monthLabels: Array<{ colIndex: number; label: string }> = []
  const monthFormatter = new Intl.DateTimeFormat(locale, { month: 'short' })
  let lastMonth = -1

  for (let w = 0; w < 52; w++) {
    const meta = weekMeta[w]
    const weekStart = addDays(start, w * 7)
    const colMonth = weekStart.getMonth()
    if (colMonth !== lastMonth) {
      // Avoid crowding the very first label if month switches immediately at col 1.
      if (monthLabels.length === 0 || w - monthLabels[monthLabels.length - 1].colIndex >= 2) {
        monthLabels.push({ colIndex: w, label: monthFormatter.format(weekStart) })
      }
      lastMonth = colMonth
    }

    const weekVal = metric === 'chars' ? meta.weeklyChars : meta.weeklyCount
    const cumVal = metric === 'chars' ? meta.cumChars : meta.cumCount

    const filledDots =
      mode === 'weekly'
        ? weekVal <= 0 || maxWeekly <= 0
          ? 0
          : Math.max(1, Math.round((weekVal / maxWeekly) * 7))
        : mode === 'cumulative'
          ? cumVal <= 0 || maxCum <= 0
            ? 0
            : Math.max(1, Math.round((cumVal / maxCum) * 7))
          : 0

    const col: ActivityCell[] = []
    for (let row = 0; row < 7; row++) {
      const cellDate = addDays(weekStart, row)
      const localDate = formatIsoDate(cellDate)
      const isFuture = localDate > endStr
      const point = byDate.get(localDate)
      const chars = point?.totalCharacters ?? 0
      const count = point?.usageCount ?? 0
      const durationMs = point?.totalDurationMs ?? 0

      let level: 0 | 1 | 2 | 3 | 4 = 0
      if (mode === 'daily') {
        if (!isFuture) {
          level = intensityLevel(metric === 'chars' ? chars : count, maxDaily)
        }
      } else if (meta.hasPastDay) {
        const heightFromBottom = 7 - row
        level = heightFromBottom <= filledDots ? 4 : 0
      }

      col.push({
        localDate,
        weekIndex: w,
        dayOfWeek: row,
        isFuture: mode === 'daily' ? isFuture : !meta.hasPastDay,
        level,
        chars,
        count,
        durationMs,
        weekStartDate: meta.weekStartDate,
        cumulativeChars: meta.cumChars,
        cumulativeCount: meta.cumCount,
        weeklyChars: meta.weeklyChars,
        weeklyCount: meta.weeklyCount,
      })
    }
    columns.push(col)
  }

  return { columns, monthLabels }
}
