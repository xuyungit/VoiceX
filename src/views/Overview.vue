<script setup lang="ts">
import { onMounted, computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { useHistoryStore } from '../stores/history'
import { useSettingsStore } from '../stores/settings'
import { formatHotkey } from '../utils/hotkey'

const historyStore = useHistoryStore()
const settingsStore = useSettingsStore()
const { t } = useI18n()

const hotkeyDisplay = computed(() => {
  const config = settingsStore.settings.hotkeyConfig
  return formatHotkey(config) ?? t('overview.unset')
})

const ASR_DISPLAY_NAMES: Record<string, string> = {
  volcengine: 'Volcengine',
  google: 'Google',
  qwen: 'Qwen',
  gemini: 'Gemini',
  'gemini-live': 'Gemini Live',
  cohere: 'Cohere',
  openai: 'OpenAI',
  soniox: 'Soniox',
  coli: 'coli (local)'
}

const LLM_DISPLAY_NAMES: Record<string, string> = {
  volcengine: 'Volcengine',
  openai: 'OpenAI',
  qwen: 'Qwen',
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
    return t('overview.hoursMinutes', { hours, minutes: remaining })
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

const formatCharacters = (count: number) => {
  if (count >= 1000) return `${(count / 1000).toFixed(1)}K`
  return count.toString()
}

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

onMounted(async () => {
  await historyStore.loadStats()
})
</script>

<template>
  <div class="page overview-page">
    <div class="page-header">
      <h1 class="page-title">{{ t('overview.title') }}</h1>
      <div class="status-lines">
        <div class="meta-line">
          <span class="meta-label">{{ t('overview.hotkey') }}:</span>
          <span class="meta-value">{{ hotkeyDisplay }}</span>
        </div>
      </div>
    </div>

    <div class="status-bar">
      <div class="status-pill">
        <span class="pill-label">{{ t('overview.asrProvider') }}</span>
        <span class="pill-value">{{ asrDisplay }}</span>
      </div>
      <div class="status-pill">
        <span class="pill-label">{{ t('overview.llmCorrection') }}</span>
        <span class="pill-value" :class="{ 'pill-off': !settingsStore.settings.enableLlmCorrection }">{{ llmDisplay }}</span>
      </div>
      <div class="status-pill">
        <span class="pill-label">{{ t('overview.syncStatus') }}</span>
        <span class="pill-value" :class="{ 'pill-off': !settingsStore.settings.syncEnabled }">{{ syncDisplay }}</span>
      </div>
    </div>

    <div class="stats-grid">
      <!-- Row 1: Total input characters | Total dictation time -->
      <div class="surface-card stat-card">
        <div class="stat-icon">
          <span class="glyph">Aa</span>
        </div>
        <div class="stat-content">
          <div class="stat-value">{{ totalCharacters }}</div>
          <div class="stat-subvalue">{{ t('overview.thisDevice') }}: {{ localCharacters }}</div>
          <div class="stat-label">{{ t('overview.totalCharacters') }}</div>
        </div>
      </div>

      <div class="surface-card stat-card">
        <div class="stat-icon">
          <svg viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
            <path d="M12 2a10 10 0 1 0 10 10A10 10 0 0 0 12 2zm0 18a8 8 0 1 1 8-8 8 8 0 0 1-8 8zm.5-13h-1v6l5 3 .5-.84-4.5-2.66Z" />
          </svg>
        </div>
        <div class="stat-content">
          <div class="stat-value">{{ totalDuration }}</div>
          <div class="stat-subvalue">{{ t('overview.thisDevice') }}: {{ localDuration }}</div>
          <div class="stat-label">{{ t('overview.totalDictationTime') }}</div>
        </div>
      </div>

      <!-- Row 2: Average input speed | Average recording length -->
      <div class="surface-card stat-card">
        <div class="stat-icon">
          <svg viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
            <path d="M11 21h-1l2-7H7l8-10h1l-2 8h5Z" />
          </svg>
        </div>
        <div class="stat-content">
          <div class="stat-value">{{ t('overview.charactersPerMinute', { value: averageSpeed }) }}</div>
          <div class="stat-subvalue">{{ t('overview.thisDevice') }}: {{ t('overview.charactersPerMinute', { value: localAverageSpeed }) }}</div>
          <div class="stat-label">{{ t('overview.averageDictationSpeed') }}</div>
        </div>
      </div>

      <div class="surface-card stat-card">
        <div class="stat-icon">
          <svg viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
            <path d="M15 1H9v2h6V1zm-4 13h2V8h-2v6zm8.03-6.61 1.42-1.42c-.43-.51-.9-.99-1.41-1.41l-1.42 1.42A8.962 8.962 0 0 0 12 4c-4.97 0-9 4.03-9 9s4.03 9 9 9 9-4.03 9-9c0-2.12-.74-4.07-1.97-5.61zM12 20c-3.87 0-7-3.13-7-7s3.13-7 7-7 7 3.13 7 7-3.13 7-7 7z" />
          </svg>
        </div>
        <div class="stat-content">
          <div class="stat-value">{{ avgRecordingLength }}</div>
          <div class="stat-subvalue">{{ t('overview.thisDevice') }}: {{ localAvgRecordingLength }}</div>
          <div class="stat-label">{{ t('overview.averageRecordingLength') }}</div>
        </div>
      </div>

      <!-- Row 3: Recording count | AI correction count -->
      <div class="surface-card stat-card">
        <div class="stat-icon">
          <svg viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
            <path d="M12 14c1.66 0 3-1.34 3-3V5c0-1.66-1.34-3-3-3S9 3.34 9 5v6c0 1.66 1.34 3 3 3zm-1-9c0-.55.45-1 1-1s1 .45 1 1v6c0 .55-.45 1-1 1s-1-.45-1-1V5zm6 6c0 2.76-2.24 5-5 5s-5-2.24-5-5H5c0 3.53 2.61 6.43 6 6.92V21h2v-3.08c3.39-.49 6-3.39 6-6.92h-2z" />
          </svg>
        </div>
        <div class="stat-content">
          <div class="stat-value">{{ recordingCount.toLocaleString() }}</div>
          <div class="stat-subvalue">{{ t('overview.thisDevice') }}: {{ localRecordingCount.toLocaleString() }}</div>
          <div class="stat-label">{{ t('overview.recordingCount') }}</div>
        </div>
      </div>

      <div class="surface-card stat-card">
        <div class="stat-icon">
          <svg viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
            <path d="M12 2a1 1 0 0 0-.92.6l-3 7a1 1 0 0 0 1.84.8L10.38 9h3.24l.46 1.4a1 1 0 0 0 1.92-.6l-3-7A1 1 0 0 0 12 2Zm7.66 11.11-2.78-.93a1 1 0 0 0-.64 1.9l1.38.46-1.88 2.64-1.37-.46a1 1 0 1 0-.64 1.9l2.78.92a1 1 0 0 0 1.1-.38l2.5-3.5a1 1 0 0 0-.45-1.55ZM8.1 14.2l-1.38.46-1.88-2.64 1.37-.46a1 1 0 1 0-.64-1.9l-2.78.92a1 1 0 0 0-.45 1.56l2.5 3.5a1 1 0 0 0 1.1.38l2.78-.93a1 1 0 0 0-.64-1.9Z" />
          </svg>
        </div>
        <div class="stat-content">
          <div class="stat-value">{{ aiCalls }}</div>
          <div class="stat-subvalue">{{ t('overview.thisDevice') }}: {{ localAiCalls }}</div>
          <div class="stat-label">{{ t('overview.aiCalls') }}</div>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.page-header {
  gap: 4px;
}

.status-lines {
  display: flex;
  flex-direction: column;
  gap: 4px;
  font-size: var(--font-sm);
  color: var(--color-text-secondary);
}

.meta-line {
  display: inline-flex;
  align-items: center;
  gap: 6px;
}

.meta-label {
  color: var(--color-text-tertiary);
}

.status-bar {
  display: flex;
  flex-wrap: wrap;
  gap: 10px;
}

.status-pill {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 6px 14px;
  border-radius: 20px;
  background: var(--color-bg-secondary);
  border: 1px solid var(--color-border);
  font-size: var(--font-sm);
}

.pill-label {
  color: var(--color-text-tertiary);
}

.pill-value {
  color: var(--color-text-primary);
  font-weight: 600;
}

.pill-off {
  color: var(--color-text-tertiary);
}

.stats-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(260px, 1fr));
  gap: var(--spacing-lg);
}

.stat-card {
  display: flex;
  align-items: center;
  gap: var(--spacing-md);
  padding: var(--spacing-lg) var(--spacing-xl);
  background: var(--color-bg-secondary);
  border: 1px solid var(--color-border);
}

.stat-icon {
  width: 38px;
  height: 38px;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  border-radius: 12px;
  background: rgba(255, 255, 255, 0.04);
  color: var(--color-text-secondary);
  flex-shrink: 0;
}

.stat-icon svg {
  width: 22px;
  height: 22px;
}

.glyph {
  font-weight: 700;
  letter-spacing: 0.02em;
}

.stat-content {
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.stat-value {
  font-size: 22px;
  font-weight: 700;
  letter-spacing: 0.01em;
}

.stat-subvalue {
  font-size: var(--font-xs);
  color: var(--color-text-secondary);
}

.stat-label {
  color: var(--color-text-tertiary);
  font-size: var(--font-xs);
}
</style>
