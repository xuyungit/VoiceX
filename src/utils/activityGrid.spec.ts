import { buildActivityGrid } from './activityGrid'
import type { DailyActivityResponse } from '../stores/history'

function check(condition: boolean, message: string) {
  if (!condition) throw new Error(message)
}

export function checkActivityGrid() {
  const sample: DailyActivityResponse = {
    startDate: '2025-11-24', // Monday
    endDate: '2026-11-16',   // Monday of week 51
    priorCount: 10,
    priorCharacters: 1000,
    priorDurationMs: 5000,
    undatedCount: 90,
    undatedCharacters: 9000,
    undatedDurationMs: 45000,
    days: [
      { localDate: '2025-11-24', usageCount: 2, totalCharacters: 200, totalDurationMs: 1000, llmCount: 1 },
      { localDate: '2026-11-16', usageCount: 8, totalCharacters: 800, totalDurationMs: 4000, llmCount: 8 },
    ],
  }

  const daily = buildActivityGrid(sample, 'chars', 'daily', 'zh-CN')
  check(daily.columns.length === 52, 'Grid must have 52 week columns')
  check(daily.columns[0].length === 7, 'Each column must have 7 day rows')
  check(daily.columns[0][0].level === 1, 'Smaller day gets level 1')
  check(daily.columns[51][0].level === 4, 'Peak day gets level 4')
  check(daily.columns[51][1].isFuture === true, 'Days after endDate are marked future')
  check(daily.columns[51][1].level === 0, 'Future days stay at level 0')
  // Week 0 is 2025-11-24 and week 1 is 2025-12-01; December at col 1 must replace the 1-week November label rather than disappearing.
  check(daily.monthLabels[0]?.colIndex === 1, 'Month starting at col 1 replaces single-week col 0 label')
  check(daily.monthLabels.length === 12, 'All 12 full months in the 52-week span receive labels')

  const cumulative = buildActivityGrid(sample, 'chars', 'cumulative', 'en-US')
  // Prior 1000 + week 0 (200) = 1200; week 51 adds 800 => 2000.
  check(cumulative.columns[0][0].cumulativeChars === 1200, 'Cumulative includes prior baseline')
  check(cumulative.columns[51][0].cumulativeChars === 2000, 'Final week reaches total cumulative chars')
  const counts = buildActivityGrid(sample, 'count', 'cumulative', 'en-US')
  check(counts.columns[0][0].cumulativeCount === 12, 'Undated counts must not be added to the prior baseline')
  const undatedOnly = buildActivityGrid({ ...sample, priorCount: 0, priorCharacters: 0, priorDurationMs: 0, days: [] }, 'chars', 'cumulative', 'en-US')
  check(undatedOnly.columns.every(col => col.every(cell => cell.level === 0 && cell.cumulativeChars === 0)), 'Undated usage alone must not create historical activity')
  // Row 6 (bottom) of week 51 must be filled (level 4)
  check(cumulative.columns[51][6].level === 4, 'Cumulative bottom dot is filled')
}
