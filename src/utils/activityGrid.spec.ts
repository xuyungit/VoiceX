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

  const cumulative = buildActivityGrid(sample, 'chars', 'cumulative', 'en-US')
  // Prior 1000 + week 0 (200) = 1200; week 51 adds 800 => 2000.
  check(cumulative.columns[0][0].cumulativeChars === 1200, 'Cumulative includes prior baseline')
  check(cumulative.columns[51][0].cumulativeChars === 2000, 'Final week reaches total cumulative chars')
  // Row 6 (bottom) of week 51 must be filled (level 4)
  check(cumulative.columns[51][6].level === 4, 'Cumulative bottom dot is filled')
}
