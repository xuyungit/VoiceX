<script setup lang="ts">
import { onMounted, computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { NEmpty } from 'naive-ui'
import { useHistoryStore, type AppUsageRange, type AppUsageStat } from '../stores/history'
import { useSettingsStore } from '../stores/settings'
import { formatDictationHotkey } from '../utils/hotkey'
import {
  buildActivityGrid,
  type ActivityCell,
  type ActivityMetric,
  type ActivityMode,
} from '../utils/activityGrid'
import PermissionSetupCard from '../components/PermissionSetupCard.vue'

type OverviewTab = 'core' | 'insights'
type AppSortBy = 'count' | 'chars' | 'duration'

const historyStore = useHistoryStore()
const settingsStore = useSettingsStore()
const { t, locale } = useI18n()

const activeTab = ref<OverviewTab>('core')
const activityMetric = ref<ActivityMetric>('chars')
const activityMode = ref<ActivityMode>('daily')
const appSortBy = ref<AppSortBy>('count')

const hoveredCell = ref<ActivityCell | null>(null)
const tooltipStyle = ref<{ left: string; top: string }>({ left: '0px', top: '0px' })
const gridPanelRef = ref<HTMLElement | null>(null)

const hotkeyDisplay = computed(() => formatDictationHotkey(settingsStore.settings.hotkeyConfig))

const ASR_DISPLAY_NAMES: Record<string, string> = {
  volcengine: 'Volcengine',
  google: 'Google',
  funasr: 'Fun-ASR',
  qwen: 'Qwen',
  gemini: 'Gemini',
  cohere: 'Cohere',
  openai: 'OpenAI',
  soniox: 'Soniox',
  stepaudio: 'StepAudio',
  mimo: 'MiMo',
  deepseek: 'DeepSeek',
  coli: 'coli (local)'
}

const LLM_DISPLAY_NAMES: Record<string, string> = {
  volcengine: 'Volcengine',
  openai: 'OpenAI',
  qwen: 'Qwen',
  gemini: 'Google Gemini',
  anthropic: 'Anthropic',
  custom: 'Custom'
}

const asrDisplay = computed(() => {
  return ASR_DISPLAY_NAMES[settingsStore.settings.asrProviderType] || settingsStore.settings.asrProviderType
})

const llmDisplay = computed(() => {
  if (!settingsStore.settings.enableLlmCorrection) return t('overview.off')
  return LLM_DISPLAY_NAMES[settingsStore.settings.llmProviderType] || settingsStore.settings.llmProviderType
})

const syncDisplay = computed(() => {
  return settingsStore.settings.syncEnabled ? t('overview.on') : t('overview.off')
})

const formatDuration = (durationMs: number) => {
  const minutes = Math.floor(durationMs / 60000)
  const hours = Math.floor(minutes / 60)
  const remaining = minutes % 60
  if (hours > 0) {
    return remaining === 0
      ? t('overview.hoursOnly', { hours })
      : t('overview.hoursMinutes', { hours, minutes: remaining })
  }
  return t('overview.minutesOnly', { minutes: remaining })
}

const formatShortDuration = (durationMs: number) => {
  const totalSeconds = Math.round(durationMs / 1000)
  const minutes = Math.floor(totalSeconds / 60)
  const seconds = totalSeconds % 60
  if (minutes > 0) {
    return t('overview.minutesSeconds', { minutes, seconds })
  }
  return t('overview.secondsOnly', { seconds })
}

const formatCompactAppDuration = (durationMs: number) => {
  if (durationMs >= 60000) return formatDuration(durationMs)
  return formatShortDuration(durationMs)
}

// Large totals in the reader's own short form: 124.4万 / 1.24M / 898.6K.
// "morePrecision" keeps one decimal or three significant digits, whichever says more.
const formatCharacters = (count: number) =>
  new Intl.NumberFormat(locale.value, {
    notation: 'compact',
    maximumFractionDigits: 1,
    maximumSignificantDigits: 3,
    roundingPriority: 'morePrecision',
  } as Intl.NumberFormatOptions).format(count)

const formatCount = (count: number) => new Intl.NumberFormat(locale.value).format(count)

const formatSpeed = (value: number) => t('overview.charactersPerMinute', { value: formatCount(value) })

const totalDuration = computed(() => formatDuration(historyStore.stats.totalDurationMs || 0))
const localDuration = computed(() => formatDuration(historyStore.localStats.totalDurationMs || 0))

const totalCharacters = computed(() => formatCharacters(historyStore.stats.totalCharacters || 0))
const localCharacters = computed(() => formatCharacters(historyStore.localStats.totalCharacters || 0))

const averageSpeed = computed(() => historyStore.formattedStats.averageSpeed || 0)
const localAverageSpeed = computed(() => historyStore.formattedLocalStats.averageSpeed || 0)

const aiCalls = computed(() => historyStore.stats.llmCorrectionCount || 0)
const localAiCalls = computed(() => historyStore.localStats.llmCorrectionCount || 0)

const recordingCount = computed(() => historyStore.stats.totalRecordingCount || 0)
const localRecordingCount = computed(() => historyStore.localStats.totalRecordingCount || 0)

const avgRecordingLength = computed(() => {
  const count = historyStore.stats.totalRecordingCount || 0
  if (count === 0) return '—'
  return formatShortDuration(historyStore.stats.totalDurationMs / count)
})
const localAvgRecordingLength = computed(() => {
  const count = historyStore.localStats.totalRecordingCount || 0
  if (count === 0) return '—'
  return formatShortDuration(historyStore.localStats.totalDurationMs / count)
})

// Reads run from seconds to minutes, so the dictation formatter -- which
// rounds down to whole minutes -- would report a 45-second read as "0 minutes".
const formatReadingDuration = (durationMs: number) => {
  if (durationMs >= 3600000) return formatDuration(durationMs)
  return formatShortDuration(durationMs)
}

const tts = computed(() => historyStore.stats.tts)
const localTts = computed(() => historyStore.localStats.tts)

const readDuration = computed(() => formatReadingDuration(tts.value.durationMs || 0))
const localReadDuration = computed(() => formatReadingDuration(localTts.value.durationMs || 0))

const readCharacters = computed(() => formatCharacters(tts.value.characters || 0))
const localReadCharacters = computed(() => formatCharacters(localTts.value.characters || 0))

interface Stat {
  label: string
  total: string
  local: string
}

const dictationHeadline = computed<Stat[]>(() => [
  { label: t('overview.totalCharacters'), total: totalCharacters.value, local: localCharacters.value },
  { label: t('overview.totalDictationTime'), total: totalDuration.value, local: localDuration.value },
])

const dictationDetails = computed<Stat[]>(() => [
  { label: t('overview.averageDictationSpeed'), total: formatSpeed(averageSpeed.value), local: formatSpeed(localAverageSpeed.value) },
  { label: t('overview.averageRecordingLength'), total: avgRecordingLength.value, local: localAvgRecordingLength.value },
  { label: t('overview.recordingCount'), total: formatCount(recordingCount.value), local: formatCount(localRecordingCount.value) },
  { label: t('overview.aiCalls'), total: formatCount(aiCalls.value), local: formatCount(localAiCalls.value) },
])

const readingHeadline = computed<Stat[]>(() => [
  { label: t('overview.readCharacters'), total: readCharacters.value, local: localReadCharacters.value },
  { label: t('overview.readDuration'), total: readDuration.value, local: localReadDuration.value },
])

const readingDetails = computed<Stat[]>(() => [
  { label: t('overview.plainReadCount'), total: formatCount(tts.value.readCount || 0), local: formatCount(localTts.value.readCount || 0) },
  { label: t('overview.translateReadCount'), total: formatCount(tts.value.translateCount || 0), local: formatCount(localTts.value.translateCount || 0) },
  { label: t('overview.readAiCalls'), total: formatCount(tts.value.llmCount || 0), local: formatCount(localTts.value.llmCount || 0) },
])

const statSections = computed(() => [
  { title: t('overview.dictationSection'), headline: dictationHeadline.value, details: dictationDetails.value },
  { title: t('overview.readingSection'), headline: readingHeadline.value, details: readingDetails.value },
])

// "103 小时 5 分钟" -> numbers at full size, units smaller.
function valueParts(value: string) {
  return value
    .split(/(\d[\d.,]*)/)
    .filter(part => part !== '')
    .map(part => ({ text: part, unit: !/^\d/.test(part) }))
}

// In the narrow detail columns "This device 194 chars/min" wraps; the unit is
// already on the line above, so drop it when both values share it.
function localWithoutSharedUnit(local: string, total: string) {
  const unit = (value: string) => value.match(/[^\d.,]+$/)?.[0] ?? ''
  const suffix = unit(total)
  return suffix && unit(local) === suffix && /^[\d.,]+\s*$/.test(local.slice(0, -suffix.length) + ' ')
    ? local.slice(0, -suffix.length).trim()
    : local
}

const weeklyTopWithShare = computed(() => {
  const items = historyStore.weeklyTopApps
  const total = historyStore.weeklyTotalCount
  return items.slice(0, 3).map(item => ({
    ...item,
    sharePct: total > 0 ? Math.round((item.usageCount / total) * 100) : 0,
  }))
})

const activityGrid = computed(() => {
  const data = historyStore.dailyActivity
  if (!data) return null
  return buildActivityGrid(data, activityMetric.value, activityMode.value, locale.value)
})

const undatedActivityNote = computed(() => {
  const data = historyStore.dailyActivity
  if (!data) return ''
  const count = activityMetric.value === 'chars' ? data.undatedCharacters : data.undatedCount
  if (count <= 0) return ''
  const value = activityMetric.value === 'chars'
    ? t('overview.charsUnit', { count: formatCharacters(count) })
    : t('overview.countUnit', { count: formatCount(count) })
  return t('overview.undatedActivity', { value })
})

function formatShortDate(isoDate: string): string {
  const [y, m, d] = isoDate.split('-').map(Number)
  return new Intl.DateTimeFormat(locale.value, { month: 'short', day: 'numeric' }).format(
    new Date(y, (m || 1) - 1, d || 1),
  )
}

function formatFullDate(isoDate: string): string {
  const [y, m, d] = isoDate.split('-').map(Number)
  return new Intl.DateTimeFormat(locale.value, { year: 'numeric', month: 'short', day: 'numeric' }).format(
    new Date(y, (m || 1) - 1, d || 1),
  )
}

const tooltipText = computed(() => {
  const cell = hoveredCell.value
  if (!cell) return ''
  if (activityMode.value === 'daily') {
    return t('overview.tooltipDay', {
      date: formatShortDate(cell.localDate),
      chars: formatCharacters(cell.chars),
      count: formatCount(cell.count),
    })
  }
  const value =
    activityMetric.value === 'chars'
      ? t('overview.charsUnit', {
          count: formatCharacters(
            activityMode.value === 'weekly' ? cell.weeklyChars : cell.cumulativeChars,
          ),
        })
      : t('overview.countUnit', {
          count: formatCount(
            activityMode.value === 'weekly' ? cell.weeklyCount : cell.cumulativeCount,
          ),
        })
  return activityMode.value === 'weekly'
    ? t('overview.tooltipWeek', { date: formatFullDate(cell.weekStartDate), value })
    : t('overview.tooltipCumulative', { date: formatFullDate(cell.weekStartDate), value })
})

function onCellEnter(event: MouseEvent, cell: ActivityCell) {
  if (cell.isFuture) {
    hoveredCell.value = null
    return
  }
  hoveredCell.value = cell
  const target = event.currentTarget as HTMLElement | null
  const panel = gridPanelRef.value
  if (!target || !panel) return
  const cellRect = target.getBoundingClientRect()
  const panelRect = panel.getBoundingClientRect()
  const left = Math.min(Math.max(10, cellRect.left - panelRect.left - 44), Math.max(10, panelRect.width - 210))
  const top = Math.max(6, cellRect.top - panelRect.top - 36)
  tooltipStyle.value = { left: `${left}px`, top: `${top}px` }
}

function onCellLeave() {
  hoveredCell.value = null
}

const timeRanges: Array<{ value: AppUsageRange; labelKey: string }> = [
  { value: 'today', labelKey: 'overview.rangeToday' },
  { value: 'week', labelKey: 'overview.rangeWeek' },
  { value: 'month', labelKey: 'overview.rangeMonth' },
  { value: 'all', labelKey: 'overview.rangeAll' },
]

const sortOptions: Array<{ value: AppSortBy; labelKey: string }> = [
  { value: 'count', labelKey: 'overview.sortByCount' },
  { value: 'chars', labelKey: 'overview.sortByChars' },
  { value: 'duration', labelKey: 'overview.sortByDuration' },
]

function metricValue(item: AppUsageStat, sortBy: AppSortBy): number {
  if (sortBy === 'chars') return item.totalCharacters
  if (sortBy === 'duration') return item.totalDurationMs
  return item.usageCount
}

const sortedApps = computed(() => {
  const items = [...historyStore.appStats]
  items.sort((a, b) => metricValue(b, appSortBy.value) - metricValue(a, appSortBy.value))
  return items
})

const periodTotals = computed(() => {
  const items = sortedApps.value
  const totalCount = items.reduce((sum, item) => sum + item.usageCount, 0)
  const totalChars = items.reduce((sum, item) => sum + item.totalCharacters, 0)
  const totalDurationMs = items.reduce((sum, item) => sum + item.totalDurationMs, 0)
  const totalMetric = items.reduce((sum, item) => sum + metricValue(item, appSortBy.value), 0)
  const maxMetric = items.reduce((max, item) => Math.max(max, metricValue(item, appSortBy.value)), 0)
  const topApp = items[0] ?? null
  const topShare =
    topApp && totalMetric > 0 ? Math.round((metricValue(topApp, appSortBy.value) / totalMetric) * 100) : 0
  return { totalCount, totalChars, totalDurationMs, totalMetric, maxMetric, topApp, topShare }
})

function appSharePct(item: AppUsageStat): number {
  const total = periodTotals.value.totalMetric
  if (total <= 0) return 0
  return Math.round((metricValue(item, appSortBy.value) / total) * 100)
}

function appBarWidthPct(item: AppUsageStat): number {
  const max = periodTotals.value.maxMetric
  if (max <= 0) return 0
  return Math.max(4, Math.round((metricValue(item, appSortBy.value) / max) * 100))
}

function primaryAppMetric(item: AppUsageStat): string {
  if (appSortBy.value === 'chars') {
    return t('overview.charsUnit', { count: formatCharacters(item.totalCharacters) })
  }
  if (appSortBy.value === 'duration') {
    return formatCompactAppDuration(item.totalDurationMs)
  }
  return t('overview.countUnit', { count: formatCount(item.usageCount) })
}

function secondaryAppMetric(item: AppUsageStat): string {
  const avg = t('overview.avgCharsPerUse', {
    count: formatCount(Math.round(item.totalCharacters / Math.max(1, item.usageCount))),
  })
  if (appSortBy.value === 'chars') {
    return `${t('overview.countUnit', { count: formatCount(item.usageCount) })} · ${formatCompactAppDuration(item.totalDurationMs)} · ${avg}`
  }
  if (appSortBy.value === 'duration') {
    return `${t('overview.countUnit', { count: formatCount(item.usageCount) })} · ${t('overview.charsUnit', { count: formatCharacters(item.totalCharacters) })} · ${avg}`
  }
  return `${t('overview.charsUnit', { count: formatCharacters(item.totalCharacters) })} · ${formatCompactAppDuration(item.totalDurationMs)} · ${avg}`
}

function appInitials(name: string): string {
  const trimmed = name.trim()
  return trimmed ? trimmed.slice(0, 2).toUpperCase() : '?'
}

function showAppSubKey(item: AppUsageStat): boolean {
  return item.appKey.trim().toLowerCase() !== item.appName.trim().toLowerCase()
}

function selectRange(range: AppUsageRange) {
  void historyStore.loadAppStats(range)
}

async function refreshInsights() {
  const tasks: Array<Promise<unknown>> = [
    historyStore.loadAppStats(historyStore.appUsageRange),
    historyStore.loadDailyActivity(),
  ]
  if (historyStore.appUsageRange !== 'week') {
    tasks.push(historyStore.loadWeeklyTopApps())
  }
  await Promise.all(tasks)
}

watch(
  () => historyStore.statsRevision,
  () => {
    void refreshInsights()
  },
)

onMounted(async () => {
  await Promise.all([historyStore.loadStats(), refreshInsights()])
})
</script>

<template>
  <div class="page overview-page">
    <div class="page-header">
      <div class="header-top">
        <h1 class="page-title">{{ t('overview.title') }}</h1>
        <div class="segmented-control" role="tablist" :aria-label="t('overview.title')">
          <button
            type="button"
            role="tab"
            class="segmented-item"
            :class="{ active: activeTab === 'core' }"
            :aria-selected="activeTab === 'core'"
            @click="activeTab = 'core'"
          >
            {{ t('overview.tabCore') }}
          </button>
          <button
            type="button"
            role="tab"
            class="segmented-item"
            :class="{ active: activeTab === 'insights' }"
            :aria-selected="activeTab === 'insights'"
            @click="activeTab = 'insights'"
          >
            {{ t('overview.tabActivityAndApps') }}
          </button>
        </div>
      </div>

      <div class="status-bar">
        <div class="info-pill">
          <span class="info-pill-label">{{ t('overview.hotkey') }}</span>
          <span class="info-pill-value">{{ hotkeyDisplay }}</span>
        </div>
        <div class="info-pill">
          <span class="info-pill-label">{{ t('overview.asrProvider') }}</span>
          <span class="info-pill-value">{{ asrDisplay }}</span>
        </div>
        <div class="info-pill">
          <span class="info-pill-label">{{ t('overview.llmCorrection') }}</span>
          <span class="info-pill-value" :class="{ off: !settingsStore.settings.enableLlmCorrection }">{{ llmDisplay }}</span>
        </div>
        <div class="info-pill">
          <span class="info-pill-label">{{ t('overview.syncStatus') }}</span>
          <span class="info-pill-value" :class="{ off: !settingsStore.settings.syncEnabled }">{{ syncDisplay }}</span>
        </div>
      </div>
    </div>

    <PermissionSetupCard />

    <template v-if="activeTab === 'core'">
      <section v-for="section in statSections" :key="section.title" class="stat-section">
        <h2 class="section-title">{{ section.title }}</h2>
        <div class="surface-card stat-panel">
          <div class="stat-row headline">
            <div v-for="stat in section.headline" :key="stat.label" class="stat">
              <div class="stat-label">{{ stat.label }}</div>
              <div class="stat-value">
                <span v-for="(part, i) in valueParts(stat.total)" :key="i" :class="{ unit: part.unit }">{{ part.text }}</span>
              </div>
              <div v-if="stat.local !== stat.total" class="stat-local">{{ t('overview.thisDevice') }} {{ stat.local }}</div>
            </div>
          </div>
          <div class="stat-row details" :style="{ '--columns': section.details.length }">
            <div v-for="stat in section.details" :key="stat.label" class="stat">
              <div class="stat-label">{{ stat.label }}</div>
              <div class="stat-value">
                <span v-for="(part, i) in valueParts(stat.total)" :key="i" :class="{ unit: part.unit }">{{ part.text }}</span>
              </div>
              <div v-if="stat.local !== stat.total" class="stat-local">{{ t('overview.thisDevice') }} {{ localWithoutSharedUnit(stat.local, stat.total) }}</div>
            </div>
          </div>
        </div>
      </section>

      <section v-if="weeklyTopWithShare.length > 0" class="surface-card teaser-card">
        <div class="teaser-left">
          <span class="teaser-label">{{ t('overview.weeklyTopApps') }}</span>
          <div class="teaser-pills">
            <span v-for="app in weeklyTopWithShare" :key="`${app.platform}:${app.appKey}`" class="teaser-pill">
              <span class="teaser-name">{{ app.appName }}</span>
              <span class="teaser-pct">{{ app.sharePct }}%</span>
            </span>
          </div>
        </div>
        <button type="button" class="teaser-link" @click="activeTab = 'insights'">
          {{ t('overview.viewActivityAndApps') }} →
        </button>
      </section>
    </template>

    <template v-else>
      <section class="insights-section">
        <div class="insights-header">
          <h2 class="section-title">{{ t('overview.activitySection') }}</h2>
          <div class="insights-controls">
            <div class="segmented-control compact">
              <button
                type="button"
                class="segmented-item"
                :class="{ active: activityMetric === 'chars' }"
                @click="activityMetric = 'chars'"
              >
                {{ t('overview.metricChars') }}
              </button>
              <button
                type="button"
                class="segmented-item"
                :class="{ active: activityMetric === 'count' }"
                @click="activityMetric = 'count'"
              >
                {{ t('overview.metricCount') }}
              </button>
            </div>
            <div class="mode-links">
              <button
                type="button"
                class="mode-link"
                :class="{ active: activityMode === 'daily' }"
                @click="activityMode = 'daily'"
              >
                {{ t('overview.modeDaily') }}
              </button>
              <button
                type="button"
                class="mode-link"
                :class="{ active: activityMode === 'weekly' }"
                @click="activityMode = 'weekly'"
              >
                {{ t('overview.modeWeekly') }}
              </button>
              <button
                type="button"
                class="mode-link"
                :class="{ active: activityMode === 'cumulative' }"
                @click="activityMode = 'cumulative'"
              >
                {{ t('overview.modeCumulative') }}
              </button>
            </div>
          </div>
        </div>

        <div ref="gridPanelRef" class="surface-card activity-panel">
          <div v-if="hoveredCell && tooltipText" class="activity-tooltip" :style="tooltipStyle">
            {{ tooltipText }}
          </div>

          <div v-if="activityGrid" class="dot-grid">
            <div v-for="(col, colIdx) in activityGrid.columns" :key="colIdx" class="dot-col">
              <div
                v-for="cell in col"
                :key="cell.localDate"
                class="dot-cell"
                :class="[`level-${cell.level}`, { future: cell.isFuture }]"
                @mouseenter="e => onCellEnter(e, cell)"
                @mouseleave="onCellLeave"
              />
            </div>
          </div>

          <div v-if="activityGrid" class="activity-footer">
            <div v-if="undatedActivityNote">{{ undatedActivityNote }}</div>
            <div class="month-track">
              <span
                v-for="m in activityGrid.monthLabels"
                :key="`${m.colIndex}-${m.label}`"
                class="month-label"
                :style="{ gridColumnStart: m.colIndex + 1 }"
              >
                {{ m.label }}
              </span>
            </div>
            <div v-if="activityMode === 'daily'" class="activity-legend">
              <span>{{ t('overview.legendLess') }}</span>
              <span class="legend-dot level-0" />
              <span class="legend-dot level-1" />
              <span class="legend-dot level-2" />
              <span class="legend-dot level-3" />
              <span class="legend-dot level-4" />
              <span>{{ t('overview.legendMore') }}</span>
            </div>
          </div>
        </div>
      </section>

      <section class="insights-section">
        <div class="insights-header">
          <h2 class="section-title">{{ t('overview.appDistributionSection') }}</h2>
          <div class="insights-controls">
            <div class="segmented-control compact">
              <button
                v-for="item in timeRanges"
                :key="item.value"
                type="button"
                class="segmented-item"
                :class="{ active: historyStore.appUsageRange === item.value }"
                @click="selectRange(item.value)"
              >
                {{ t(item.labelKey) }}
              </button>
            </div>
            <div class="segmented-control compact">
              <button
                v-for="opt in sortOptions"
                :key="opt.value"
                type="button"
                class="segmented-item"
                :class="{ active: appSortBy === opt.value }"
                @click="appSortBy = opt.value"
              >
                {{ t(opt.labelKey) }}
              </button>
            </div>
          </div>
        </div>

        <div class="surface-card app-panel">
          <div v-if="sortedApps.length === 0" class="app-empty">
            <NEmpty :description="t('overview.emptyApps')" />
          </div>
          <template v-else>
            <div class="app-summary-strip">
              <div class="summary-cell">
                <div class="summary-label">{{ t('overview.activeApps') }}</div>
                <div class="summary-value">{{ t('overview.appsCountUnit', { count: formatCount(sortedApps.length) }) }}</div>
              </div>
              <div class="summary-cell">
                <div class="summary-label">{{ t('overview.mostUsedApp') }}</div>
                <div class="summary-value truncate">
                  {{ periodTotals.topApp?.appName ?? '—' }}
                  <span v-if="periodTotals.topApp" class="summary-sub">{{ periodTotals.topShare }}%</span>
                </div>
              </div>
              <div class="summary-cell">
                <div class="summary-label">{{ t('overview.periodTotal') }}</div>
                <div class="summary-value">
                  {{ t('overview.countUnit', { count: formatCount(periodTotals.totalCount) }) }}
                  · {{ t('overview.charsUnit', { count: formatCharacters(periodTotals.totalChars) }) }}
                  · {{ formatCompactAppDuration(periodTotals.totalDurationMs) }}
                </div>
              </div>
            </div>

            <div class="app-list">
              <div v-for="(item, index) in sortedApps" :key="`${item.platform}:${item.appKey}`" class="app-row">
                <div class="app-main">
                  <div class="app-avatar">{{ appInitials(item.appName) }}</div>
                  <div class="app-info">
                    <div class="app-title-line">
                      <span class="app-rank">{{ index + 1 }}.</span>
                      <span class="app-name">{{ item.appName }}</span>
                      <span v-if="showAppSubKey(item)" class="app-key">{{ item.appKey }}</span>
                    </div>
                    <div class="app-bar-track">
                      <div class="app-bar-fill" :style="{ width: `${appBarWidthPct(item)}%` }" />
                    </div>
                  </div>
                </div>
                <div class="app-metrics">
                  <div class="app-metric-primary">
                    {{ primaryAppMetric(item) }}
                    <span class="app-share">({{ appSharePct(item) }}%)</span>
                  </div>
                  <div class="app-metric-secondary">
                    {{ secondaryAppMetric(item) }}
                  </div>
                </div>
              </div>
            </div>
          </template>
        </div>
      </section>
    </template>
  </div>
</template>

<style scoped>
/* Core tab stays sized so every stat fits the default 900 x 650 window without scrolling. */
.overview-page {
  max-width: 760px;
  gap: 20px;
  padding-bottom: var(--spacing-xl);
}

.page-header {
  gap: 12px;
}

.header-top {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  flex-wrap: wrap;
}

.segmented-control {
  display: inline-flex;
  padding: 2px;
  border-radius: var(--radius-md);
  background: var(--color-bg-secondary);
  border: 1px solid var(--color-border);
  gap: 2px;
}

.segmented-item {
  padding: 4px 12px;
  border-radius: 6px;
  font-size: var(--font-sm);
  font-weight: 500;
  color: var(--color-text-secondary);
  transition: background-color var(--transition-fast), color var(--transition-fast);
}

.segmented-control.compact .segmented-item {
  padding: 3px 9px;
  font-size: var(--font-xs);
}

.segmented-item:hover {
  color: var(--color-text-primary);
}

.segmented-item.active {
  background: var(--color-bg-tertiary);
  color: var(--color-text-primary);
}

.status-bar {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
}

.stat-section {
  display: flex;
  flex-direction: column;
  gap: 6px;
  container-type: inline-size;
}

.stat-section .section-title,
.insights-header {
  padding: 0 2px;
}

.stat-panel {
  padding: 0;
}

.stat-row {
  display: grid;
  gap: 16px;
  padding: 12px 18px;
}

.stat-row.headline {
  grid-template-columns: repeat(2, minmax(0, 1fr));
}

.stat-row.details {
  grid-template-columns: repeat(var(--columns), minmax(0, 1fr));
  border-top: 1px solid var(--color-divider);
}

.stat {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}

.stat-label {
  font-size: var(--font-sm);
  color: var(--color-text-secondary);
}

.stat-value {
  font-weight: 600;
  font-variant-numeric: tabular-nums;
  letter-spacing: -0.01em;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.headline .stat-value {
  margin-top: 2px;
  font-size: 24px;
  line-height: 1.2;
}

.details .stat-value {
  font-size: 17px;
  line-height: 1.3;
}

.stat-value .unit {
  font-weight: 500;
  color: var(--color-text-secondary);
  letter-spacing: 0;
}

.headline .stat-value .unit {
  font-size: var(--font-md);
}

.details .stat-value .unit {
  font-size: var(--font-sm);
}

.stat-local {
  font-size: var(--font-sm);
  color: var(--color-text-tertiary);
  font-variant-numeric: tabular-nums;
}

.teaser-card {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  flex-wrap: wrap;
  padding: 10px 16px;
}

.teaser-left {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
  min-width: 0;
}

.teaser-label {
  font-size: var(--font-sm);
  color: var(--color-text-secondary);
}

.teaser-pills {
  display: flex;
  align-items: center;
  gap: 6px;
  flex-wrap: wrap;
}

.teaser-pill {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  padding: 2px 8px;
  border-radius: 6px;
  background: var(--color-bg-tertiary);
  font-size: var(--font-xs);
}

.teaser-name {
  color: var(--color-text-primary);
  font-weight: 500;
}

.teaser-pct {
  color: var(--color-text-tertiary);
  font-variant-numeric: tabular-nums;
}

.teaser-link {
  font-size: var(--font-xs);
  font-weight: 500;
  color: var(--color-accent);
  white-space: nowrap;
}

.teaser-link:hover {
  color: var(--color-accent-hover);
}

.insights-section {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.insights-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  flex-wrap: wrap;
}

.insights-controls {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
}

.mode-links {
  display: inline-flex;
  align-items: center;
  gap: 10px;
}

.mode-link {
  font-size: var(--font-xs);
  color: var(--color-text-tertiary);
  transition: color var(--transition-fast);
}

.mode-link:hover {
  color: var(--color-text-primary);
}

.mode-link.active {
  color: var(--color-text-primary);
  font-weight: 600;
}

.activity-panel {
  position: relative;
  padding: 14px 16px 12px;
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.activity-tooltip {
  position: absolute;
  z-index: 10;
  pointer-events: none;
  padding: 5px 10px;
  border-radius: 6px;
  background: var(--color-text-primary);
  color: var(--color-bg-primary);
  font-size: var(--font-xs);
  font-weight: 500;
  white-space: nowrap;
  box-shadow: var(--shadow-md);
}

.dot-grid {
  display: grid;
  grid-template-columns: repeat(52, minmax(0, 1fr));
  gap: 3px;
}

.dot-col {
  display: grid;
  grid-template-rows: repeat(7, minmax(0, 1fr));
  gap: 3px;
}

.dot-cell,
.legend-dot {
  border-radius: 2.5px;
}

.dot-cell {
  aspect-ratio: 1 / 1;
  transition: transform var(--transition-fast);
}

.dot-cell:not(.future):hover {
  transform: scale(1.25);
  outline: 1.5px solid var(--color-text-primary);
  outline-offset: 1px;
}

.dot-cell.future {
  opacity: 0;
  pointer-events: none;
}

.level-0 {
  background: color-mix(in srgb, var(--color-text-primary) 7%, transparent);
}

.level-1 {
  background: color-mix(in srgb, var(--color-accent) 26%, transparent);
}

.level-2 {
  background: color-mix(in srgb, var(--color-accent) 50%, transparent);
}

.level-3 {
  background: color-mix(in srgb, var(--color-accent) 75%, transparent);
}

.level-4 {
  background: var(--color-accent);
}

.activity-footer {
  display: flex;
  flex-direction: column;
  gap: 6px;
  font-size: var(--font-xs);
  color: var(--color-text-tertiary);
}

.month-track {
  display: grid;
  grid-template-columns: repeat(52, minmax(0, 1fr));
  gap: 3px;
  width: 100%;
  min-width: 0;
}

.month-label {
  grid-row: 1;
  white-space: nowrap;
  font-size: 11px;
}

.activity-legend {
  display: inline-flex;
  align-items: center;
  align-self: flex-end;
  gap: 3px;
  font-size: 11px;
}

.legend-dot {
  width: 9px;
  height: 9px;
}

.app-panel {
  padding: 0;
  overflow: hidden;
}

.app-empty {
  padding: 32px 16px;
}

.app-summary-strip {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: 14px;
  padding: 10px 16px;
  background: var(--color-bg-tertiary);
  border-bottom: 1px solid var(--color-divider);
}

.summary-cell {
  min-width: 0;
}

.summary-label {
  font-size: var(--font-xs);
  color: var(--color-text-tertiary);
}

.summary-value {
  margin-top: 2px;
  font-size: var(--font-md);
  font-weight: 600;
  font-variant-numeric: tabular-nums;
  color: var(--color-text-primary);
}

.summary-sub {
  margin-left: 4px;
  font-size: var(--font-xs);
  font-weight: 400;
  color: var(--color-text-secondary);
}

.app-list {
  display: flex;
  flex-direction: column;
}

.app-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  padding: 11px 16px;
  border-top: 1px solid var(--color-divider);
}

.app-row:first-child {
  border-top: none;
}

.app-main {
  display: flex;
  align-items: center;
  gap: 12px;
  flex: 1;
  min-width: 0;
}

.app-avatar {
  width: 28px;
  height: 28px;
  border-radius: 7px;
  background: var(--color-accent-light);
  color: var(--color-accent);
  font-size: var(--font-xs);
  font-weight: 700;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  flex-shrink: 0;
}

.app-info {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.app-title-line {
  display: flex;
  align-items: baseline;
  gap: 6px;
  min-width: 0;
}

.app-rank {
  font-size: var(--font-xs);
  color: var(--color-text-tertiary);
  font-variant-numeric: tabular-nums;
}

.app-name {
  font-size: var(--font-md);
  font-weight: 600;
  color: var(--color-text-primary);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.app-key {
  font-size: var(--font-xs);
  color: var(--color-text-tertiary);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.app-bar-track {
  height: 5px;
  width: 100%;
  border-radius: 999px;
  background: var(--color-bg-tertiary);
  overflow: hidden;
}

.app-bar-fill {
  height: 100%;
  border-radius: 999px;
  background: var(--color-accent);
  transition: width 0.25s ease;
}

.app-metrics {
  flex-shrink: 0;
  text-align: right;
  font-variant-numeric: tabular-nums;
}

.app-metric-primary {
  font-size: var(--font-md);
  font-weight: 600;
  color: var(--color-text-primary);
}

.app-share {
  font-size: var(--font-xs);
  font-weight: 400;
  color: var(--color-text-secondary);
}

.app-metric-secondary {
  margin-top: 2px;
  font-size: var(--font-xs);
  color: var(--color-text-tertiary);
}

/* Narrow windows: detail stats wrap to two columns. */
@container (max-width: 520px) {
  .stat-row.details {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }
}
</style>
