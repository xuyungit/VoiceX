<script setup lang="ts">
import { onMounted, computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { useHistoryStore } from '../stores/history'
import { useSettingsStore } from '../stores/settings'
import { formatDictationHotkey } from '../utils/hotkey'
import PermissionSetupCard from '../components/PermissionSetupCard.vue'

const historyStore = useHistoryStore()
const settingsStore = useSettingsStore()
const { t, locale } = useI18n()

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

onMounted(async () => {
  await historyStore.loadStats()
})
</script>

<template>
  <div class="page overview-page">
    <div class="page-header">
      <h1 class="page-title">{{ t('overview.title') }}</h1>
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
  </div>
</template>

<style scoped>
/* Sized so every stat fits the default 900 x 650 window without scrolling. */
.overview-page {
  max-width: 760px;
  gap: 20px;
}

.page-header {
  gap: 12px;
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

.stat-section .section-title {
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

/* Narrow windows: detail stats wrap to two columns. */
@container (max-width: 520px) {
  .stat-row.details {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }
}
</style>
